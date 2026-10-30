use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

mod door_life;
mod execution_inputs;
mod completed_output;
mod contract_update;
pub mod room;
pub use contract_update::{ToolContractUpdateRequest, ToolContractUpdateProof, ToolRetirementRequest, tool_contract_update_quiescent, tool_retirement_quiescent};
pub use completed_output::{PreparedToolOutputRecovery, prepare_tool_output_recovery};

#[cfg(test)]
#[path = "../tests/support/execution_loading.rs"]
mod execution_loading_tests;
#[cfg(test)]
#[path = "../tests/support/invocation_context.rs"]
mod invocation_context_tests;
mod process_authority;

#[cfg(unix)]
use std::ffi::{CString, OsString};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use octopus_descriptors::{ArtifactId, InterfaceId};
use octopus_interfaces::{
    ArtifactBindingPayload, InterfaceArtifactFile, InterfaceArtifactManifest, InterfaceDescriptor,
    InterfaceInvocationEnvelope, InvocationMetadata,
};
pub use octopus_interfaces::{DeliveryVerdict, RefusedReason};
use octopus_ledger::{
    ArtifactRef, ContentHash, Endpoint, LedgerError, Movement, MovementKind,
    decode_durable_movement_route, decode_durable_movement_row,
};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
use octopus_tool_registry::{
    ProcessArgumentPathSpec, ProcessAuthoritySpec, ToolArgumentValidator, ToolDescriptor,
    ToolInterruptedEffectRecovery, ToolProcessDeclaration, ToolProcessPrivateReadSubpath,
    load_tool_descriptor_from_dir,
};
use octopus_worker_env::{
    NetworkCeiling, WorkerEnvKindRegistry, WorkerEnvKindSpec, WorkerEnvironmentFormation,
    WorkerEnvironmentFormationRequest, WorkerEnvironmentId, WorkerEnvironmentKindId,
    WorkerEnvironmentLifecycle, WorkerEnvironmentRecord, WorkerEnvironmentRecordStore,
};
use tokio::io::AsyncWriteExt;

use door_life::{
    BoundedJsonLineReader, INTERFACE_HOST_IO_LIMITS_ENV, RESOLVED_INTERFACE_DESCRIPTOR_ENV,
    capture_spawned_artifact, drain_bounded_stderr, interface_descriptor_from_process_env,
    resolve_interface_descriptor, serialize_bounded_json_line,
};
pub use door_life::{
    InterfaceHostIoLimits, InterfaceLedgerReadHandles, InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn,
    LedgerTailReader, OwnOutboundMovement, run_spawned_interface_life,
};
pub use notify;
#[cfg(test)]
use process_authority::ProcessAuthority;
use process_authority::{ProcessAuthoritySpecExecution, ProcessInvocationAuthorityError};

const TOOL_PROCESS_DURABLE_VAR_DIR_ENV: &str = "OCTOPUS_TOOL_PROCESS_DURABLE_VAR_DIR";
const TOOL_PROCESS_OWNER_FILE: &str = "owner.json";
const TOOL_PROCESS_OWNER_LOCK_FILE: &str = "owner.lock";
const TOOL_PROCESS_OWNER_TERM_GRACE_MS: u64 = 2_000;
const TOOL_PROCESS_OWNER_KILL_GRACE_MS: u64 = 2_000;
const TOOL_PROCESS_OWNER_POLL_MS: u64 = 25;
const INTERFACE_LOCAL_IPC_ROOT_SLOT: &str = "interface_local_ipc_root";

#[derive(Clone, Debug)]
pub struct ToolDoorOwner {
    pub worker_environment_kind: WorkerEnvironmentKindId,
    pub door_interface: InterfaceId,
    pub max_in_flight: usize,
    pub private_read_max_bytes: u64,
    pub root_slot_transform: Option<
        fn(
            &std::collections::BTreeMap<String, std::path::PathBuf>,
        ) -> Result<std::collections::BTreeMap<String, std::path::PathBuf>, String>,
    >,
}

#[derive(Clone, Debug)]
pub struct ToolDoorOutputSource {
    pub movement: Movement,
    pub envelope: InterfaceInvocationEnvelope,
    pub formation: Option<WorkerEnvironmentFormation>,
}

pub type ToolDoorOutputFuture =
    Pin<Box<dyn Future<Output = Result<DeliveryVerdict, String>> + Send>>;
pub type ToolDoorOutputDispatch = Arc<
    dyn Fn(InterfaceLiftContext, InterfaceArtifactFile) -> ToolDoorOutputFuture
        + Send
        + Sync,
>;

pub trait ToolDoorOutputHandler: Send + Sync {
    fn output(
        &self,
        ctx: InterfaceLiftContext,
        source: ToolDoorOutputSource,
        artifact: InterfaceArtifactFile,
        dispatch: ToolDoorOutputDispatch,
    ) -> ToolDoorOutputFuture;

    fn can_resume_unsubmitted(
        &self,
        _ctx: &InterfaceLiftContext,
        _source: &ToolDoorOutputSource,
        _artifact: &InterfaceArtifactFile,
    ) -> Result<bool, String> {
        Ok(false)
    }
}

pub type ToolDoorOutputCallback = Arc<dyn ToolDoorOutputHandler>;

impl InterfaceLiftContext {
    pub fn owner_root_slot(&self, name: &str) -> Result<&Path, String> {
        self.root_slot(name)
    }
    pub fn owner_descriptor(&self) -> Result<InterfaceDescriptor, String> {
        self.resolved_interface_descriptor()
    }
    pub fn owner_cancel(&self) -> tokio_util::sync::CancellationToken {
        self.cancel.clone()
    }
    pub fn owner_io_limits(&self) -> Result<InterfaceHostIoLimits, String> {
        self.required_host_io_limits()
    }
    pub fn owner_movements_path(&self) -> &Path { &self.movements_path }
    pub fn owner_ledger_index_path(&self) -> &Path { &self.ledger_index_path }
    pub fn owner_blobs_dir(&self) -> &Path { &self.blobs_dir }
    pub async fn owner_hand_arrival(
        &self,
        artifact: InterfaceArtifactFile,
    ) -> Result<DeliveryVerdict, String> {
        self.hand_arrival(artifact, Vec::new()).await
    }
    pub async fn owner_hand_system_emission(
        &self,
        artifact: InterfaceArtifactFile,
        source: &Movement,
        target: InterfaceId,
    ) -> Result<DeliveryVerdict, String> {
        if source.kind != MovementKind::InterfaceOutbound
            || source.to != Endpoint::Interface(self.interface.clone())
        {
            return Err("owner output source is not an admitted movement to this door".into());
        }
        self.hand_system_emission(
            SystemInterfaceEmissionArtifact::Materialized(artifact),
            source.seq.0,
            target,
        )
        .await
    }
}

async fn hand_tool_process_output(
    ctx: &InterfaceLiftContext,
    source: Option<&ToolDoorOutputSource>,
    callback: Option<&ToolDoorOutputCallback>,
    artifact: InterfaceArtifactFile,
) -> Result<DeliveryVerdict, String> {
    let dispatch: ToolDoorOutputDispatch = Arc::new(|ctx, artifact| {
        Box::pin(async move { ctx.hand_arrival(artifact, Vec::new()).await })
    });
    hand_tool_process_output_with_dispatch(ctx, source, callback, artifact, dispatch).await
}

async fn hand_tool_process_output_with_dispatch(
    ctx: &InterfaceLiftContext,
    source: Option<&ToolDoorOutputSource>,
    callback: Option<&ToolDoorOutputCallback>,
    artifact: InterfaceArtifactFile,
    dispatch: ToolDoorOutputDispatch,
) -> Result<DeliveryVerdict, String> {
    match callback {
        Some(callback) => {
            callback.output(
                ctx.clone(),
                source
                    .cloned()
                    .ok_or("owner output has no immutable admitted source")?,
                artifact,
                dispatch,
            )
            .await
        }
        None => dispatch(ctx.clone(), artifact).await,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InterfaceActivationEvidence {
    pub interface_id: InterfaceId,
    pub artifact_id: ArtifactId,
    pub label: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct InterfaceActivationArrival {
    pub artifact: InterfaceArtifactFile,
    pub evidence: Vec<InterfaceActivationEvidence>,
    pub verdict_tx: Option<tokio::sync::oneshot::Sender<DeliveryVerdict>>,
    pub retained: Option<RetainedInterfaceArrival>,
}

#[derive(Debug)]
pub(crate) struct RetainedInterfaceArrival {
    pub id: String,
    pub reply: tokio::sync::oneshot::Sender<Result<octopus_ledger::ArtifactDelivery, String>>,
}

#[derive(Debug)]
pub enum SystemInterfaceEmissionArtifact {
    Materialized(InterfaceArtifactFile),
    Reference(ArtifactRef),
}

#[derive(Debug)]
pub(crate) struct SystemInterfaceEmission {
    artifact: SystemInterfaceEmissionArtifact,
    source_movement_seq: u64,
    to_interface: InterfaceId,
    verdict_tx: tokio::sync::oneshot::Sender<DeliveryVerdict>,
}

impl SystemInterfaceEmission {
    pub(crate) fn host_attested(
        artifact: SystemInterfaceEmissionArtifact,
        source_movement_seq: u64,
        to_interface: InterfaceId,
        verdict_tx: tokio::sync::oneshot::Sender<DeliveryVerdict>,
    ) -> Self {
        Self {
            artifact,
            source_movement_seq,
            to_interface,
            verdict_tx,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        SystemInterfaceEmissionArtifact,
        u64,
        InterfaceId,
        tokio::sync::oneshot::Sender<DeliveryVerdict>,
    ) {
        (
            self.artifact,
            self.source_movement_seq,
            self.to_interface,
            self.verdict_tx,
        )
    }
}

impl ToolDoorOwner {
    fn effective_context(
        &self,
        ctx: &InterfaceLiftContext,
    ) -> Result<Option<InterfaceLiftContext>, String> {
        let Some(transform) = self.root_slot_transform else {
            return Ok(None);
        };
        let mut effective = ctx.clone();
        effective.root_slots = transform(&ctx.root_slots)?;
        Ok(Some(effective))
    }

    fn validate(&self) -> Result<(), String> {
        if self.worker_environment_kind.as_str().trim().is_empty() {
            return Err("tool owner kind must be non-empty".to_string());
        }
        if self.door_interface.as_str().trim().is_empty() {
            return Err("tool-door owner interface must be non-empty".to_string());
        }
        if self.max_in_flight == 0 {
            return Err("tool-door max_in_flight must be positive".to_string());
        }
        if self.private_read_max_bytes == 0 || self.private_read_max_bytes == u64::MAX {
            return Err("tool-door private-read ceiling must be finite and positive".to_string());
        }
        usize::try_from(self.private_read_max_bytes)
            .map_err(|_| "tool-door private-read ceiling exceeds host address space".to_string())?;
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolProcessBrokerResult {
    Completed,
    OutputPending {
        error: String,
    },
    Refused {
        reason: RefusedReason,
        detail: serde_json::Value,
    },
    CapabilityFailed {
        error: String,
    },
    Unavailable {
        error: String,
    },
}

enum ToolProcessBrokerInvocation {
    Outcome(ToolProcessBrokerResult),
    Completed(Option<ToolProcessPrimaryInput>),
    InterruptedEffectUnknown,
}

fn is_reserved_tool_process_host_env(name: &str) -> bool {
    matches!(
        name,
        "OCTOPUS_INTERFACE_ID"
            | "OCTOPUS_INTERFACE_DIR"
            | RESOLVED_INTERFACE_DESCRIPTOR_ENV
            | INTERFACE_HOST_IO_LIMITS_ENV
            | "OCTOPUS_VAR_DIR"
            | TOOL_PROCESS_DURABLE_VAR_DIR_ENV
            | "OCTOPUS_INTERFACE_VAR_DIR"
            | "OCTOPUS_PRIVATE_READS_JSON"
            | "OCTOPUS_ROOT_SLOTS_JSON"
            | "OCTOPUS_MOVEMENTS_JSONL"
            | "OCTOPUS_BLOBS_DIR"
            | "OCTOPUS_LEDGER_INDEX_SQLITE"
            | "TMPDIR"
    )
}

#[derive(Clone, Debug)]
pub struct ToolProcessEnvironment {
    pub interface: InterfaceId,
    pub interface_dir: PathBuf,
    pub descriptor_snapshot: Option<InterfaceDescriptor>,
    pub var_dir: PathBuf,
    pub durable_var_dir: Option<PathBuf>,
    pub private_reads: BTreeMap<String, PathBuf>,
    pub root_slots: BTreeMap<String, PathBuf>,
    pub host_io_limits: InterfaceHostIoLimits,
}

impl ToolProcessEnvironment {
    pub fn from_env() -> Result<Self, String> {
        let interface = std::env::var("OCTOPUS_INTERFACE_ID").map_err(|_| {
            "OCTOPUS_INTERFACE_ID is required by the tool-process contract".to_string()
        })?;
        let interface = InterfaceId::new(interface);
        let descriptor_snapshot =
            interface_descriptor_from_process_env(&interface)?.ok_or_else(|| {
                format!(
                    "{} is required by the tool-process env contract",
                    RESOLVED_INTERFACE_DESCRIPTOR_ENV
                )
            })?;
        let root_slots = serde_json::from_str(
            &std::env::var("OCTOPUS_ROOT_SLOTS_JSON").unwrap_or_else(|_| "{}".to_string()),
        )
        .map_err(|error| format!("OCTOPUS_ROOT_SLOTS_JSON is malformed: {error}"))?;
        let var_dir = required_env_path("OCTOPUS_VAR_DIR")?;
        let durable_var_dir = std::env::var_os(TOOL_PROCESS_DURABLE_VAR_DIR_ENV)
            .map(PathBuf::from)
            .map(validate_tool_process_durable_var_dir)
            .transpose()?;
        let host_io_limits: InterfaceHostIoLimits =
            serde_json::from_str(&std::env::var(INTERFACE_HOST_IO_LIMITS_ENV).map_err(|_| {
                format!(
                    "{} is required by the tool-process contract",
                    INTERFACE_HOST_IO_LIMITS_ENV
                )
            })?)
            .map_err(|error| format!("{} is malformed: {error}", INTERFACE_HOST_IO_LIMITS_ENV))?;
        host_io_limits.validate()?;
        let private_reads = serde_json::from_str::<BTreeMap<String, PathBuf>>(
            &std::env::var("OCTOPUS_PRIVATE_READS_JSON").map_err(|_| {
                "OCTOPUS_PRIVATE_READS_JSON is required by the tool-process contract".to_string()
            })?,
        )
        .map_err(|error| format!("OCTOPUS_PRIVATE_READS_JSON is malformed: {error}"))?;
        for (handle, path) in &private_reads {
            validate_private_read_handle(handle)?;
            if !path.is_absolute() {
                return Err(format!(
                    "OCTOPUS_PRIVATE_READS_JSON handle {:?} does not resolve to an absolute path",
                    handle
                ));
            }
        }
        Ok(Self {
            interface,
            interface_dir: required_env_path("OCTOPUS_INTERFACE_DIR")?,
            descriptor_snapshot: Some(descriptor_snapshot),
            var_dir,
            durable_var_dir,
            private_reads,
            root_slots,
            host_io_limits,
        })
    }

    pub fn root_slot(&self, name: &str) -> Result<&Path, String> {
        self.root_slots
            .get(name)
            .map(PathBuf::as_path)
            .ok_or_else(|| {
                format!(
                    "tool process for interface '{}' requires undeclared root slot '{}'",
                    self.interface.as_str(),
                    name
                )
            })
    }

    pub fn resolve_package_file(
        &self,
        declared: &str,
        kind: &WorkerEnvKindSpec,
    ) -> Result<PathBuf, String> {
        if declared.starts_with("owner://") {
            let owner =
                execution_inputs::kind_owner_root_from_interface(&self.interface_dir, kind)?;
            execution_inputs::owner_file(declared, &owner)
        } else {
            process_authority::resolve_required_repository_file(
                declared,
                self.root_slot("interface_life_install_root")?,
            )
        }
    }

    pub fn resolve_program(
        &self,
        declared: &str,
        kind: &WorkerEnvKindSpec,
        search_path: Option<&std::ffi::OsStr>,
    ) -> Result<PathBuf, String> {
        let program = if declared.starts_with("owner://") {
            self.resolve_package_file(declared, kind)?.into_os_string()
        } else {
            std::ffi::OsString::from(declared)
        };
        let base = if Path::new(&program).is_absolute() {
            self.interface_dir.as_path()
        } else {
            self.root_slot("interface_life_install_root")?
        };
        process_authority::resolve_executable(
            program.to_str().ok_or("tool program path is not UTF-8")?,
            base,
            search_path,
        )
    }

    pub fn private_read_path(&self, handle: &str) -> Result<&Path, String> {
        validate_private_read_handle(handle)?;
        self.private_reads
            .get(handle)
            .map(PathBuf::as_path)
            .ok_or_else(|| {
                format!(
                    "tool process for interface '{}' requires undeclared private read handle {:?}",
                    self.interface.as_str(),
                    handle
                )
            })
    }

    pub fn resolved_interface_descriptor(&self) -> Result<InterfaceDescriptor, String> {
        if let Some(descriptor) = &self.descriptor_snapshot {
            if descriptor.id != self.interface || !descriptor.has_interface_driver() {
                return Err(format!(
                    "descriptor snapshot does not describe executable interface '{}'",
                    self.interface.as_str()
                ));
            }
            return Ok(descriptor.clone());
        }
        let registry_root = self
            .interface_dir
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                format!(
                    "tool process interface directory '{}' has no registry root",
                    self.interface_dir.display()
                )
            })?;
        resolve_interface_descriptor(registry_root, &self.interface)?.ok_or_else(|| {
            format!(
                "tool process interface '{}' resolves no descriptor",
                self.interface.as_str()
            )
        })
    }

    pub fn load_formed_json_sidecar<T: serde::de::DeserializeOwned>(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        name: &str,
    ) -> Result<T, String> {
        if formation.door_interface != self.interface
            || formation.worker_environment_kind != kind.worker_environment_kind_id
        {
            return Err(format!(
                "tool process interface '{}' cannot load config for formation door '{}' kind '{}'",
                self.interface, formation.door_interface, formation.worker_environment_kind
            ));
        }
        if !kind.instance_config_files.iter().any(|file| file == name) {
            return Err(format!(
                "worker_environment_kind '{}' does not declare instance config '{}'",
                kind.worker_environment_kind_id, name
            ));
        }
        let expected = formation.instance_config_hashes.get(name).ok_or_else(|| {
            format!(
                "formed worker environment '{}' has no hash for declared instance config '{}'",
                formation.worker_environment_id, name
            )
        })?;
        let path = self.interface_dir.join(name);
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "formed instance config '{}' cannot be read: {error}",
                path.display()
            )
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "formed instance config '{}' is malformed JSON: {error}",
                path.display()
            )
        })?;
        if !value.is_object() {
            return Err(format!(
                "formed instance config '{}' must be a JSON object",
                path.display()
            ));
        }
        let observed = blake3::hash(&octopus_descriptors::canonical_bytes(&value))
            .to_hex()
            .to_string();
        if &observed != expected {
            return Err(format!(
                "formed instance config '{}' changed after worker-environment formation: expected {}, observed {}",
                path.display(),
                expected,
                observed
            ));
        }
        serde_json::from_value(value).map_err(|error| {
            format!(
                "formed instance config '{}' violates its package contract: {error}",
                path.display()
            )
        })
    }
}

type ToolProcessBoundArtifact = ArtifactBindingPayload;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolProcessBoundArtifactCarrier {
    artifact: ArtifactRef,
    manifest_path: PathBuf,
    payload_hash: ContentHash,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "carrier", rename_all = "snake_case", deny_unknown_fields)]
enum ToolProcessEmissionArtifact {
    Materialized { manifest_path: PathBuf },
    Reference { artifact: ArtifactRef },
}

#[derive(Clone, Debug)]
pub(crate) struct ToolProcessEnvelope {
    artifact: ArtifactRef,
    label: String,
    payload: Vec<u8>,
    artifact_bindings: Vec<ToolProcessBoundArtifact>,
    metadata: Option<InvocationMetadata>,
    source_movement_seq: u64,
    worker_environment: Option<WorkerEnvironmentFormation>,
    worker_environment_kind: Option<WorkerEnvKindSpec>,
    output_source: Option<ToolDoorOutputSource>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolProcessWireEnvelope {
    artifact: ArtifactRef,
    label: String,
    manifest_path: PathBuf,
    context: ToolProcessContextCarrier,
    source_movement_seq: u64,
    worker_environment: Option<WorkerEnvironmentFormation>,
    worker_environment_kind: Option<WorkerEnvKindSpec>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolProcessInvocationContext {
    // Private request transport inside this code-world, never a runtime artifact.
    artifact_bindings: Vec<ToolProcessBoundArtifactCarrier>,
    metadata: Option<InvocationMetadata>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolProcessContextCarrier {
    path: PathBuf,
    content_hash: ContentHash,
    byte_len: u64,
}

impl ToolProcessContextCarrier {
    fn read_bytes(
        &self,
        root: &Path,
        request_id: u64,
        limits: &InterfaceHostIoLimits,
    ) -> Result<Vec<u8>, String> {
        let expected = root.canonicalize().map_err(|error| error.to_string())?
            .join(request_id.to_string()).join("artifact")
            .join(format!("invocation-context-{}.json", self.content_hash.to_hex()));
        if self.path != expected {
            return Err("tool-process context carrier left its exact generation and request".into());
        }
        if self.byte_len > limits.max_payload_bytes {
            return Err("tool-process context carrier exceeds its declared payload byte ceiling".into());
        }
        let bytes = read_tool_process_binding_bytes(root, &self.path, self.byte_len)?;
        if bytes.len() as u64 != self.byte_len || ContentHash::of_bytes(&bytes) != self.content_hash {
            return Err("tool-process context carrier changed its pinned length or hash".into());
        }
        Ok(bytes)
    }

    fn read(
        &self,
        root: &Path,
        request_id: u64,
        limits: &InterfaceHostIoLimits,
    ) -> Result<ToolProcessInvocationContext, String> {
        serde_json::from_slice(&self.read_bytes(root, request_id, limits)?)
            .map_err(|error| format!("tool-process context carrier is malformed: {error}"))
    }
}

impl ToolProcessEnvelope {
    pub(crate) fn capture(
        envelope: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        host_io_limits: &InterfaceHostIoLimits,
    ) -> Result<Self, String> {
        let manifest = envelope
            .read_artifact_manifest_bounded(host_io_limits.max_manifest_bytes)
            .map_err(|error| error.to_string())?;
        let payload = envelope
            .read_artifact_payload_bounded(host_io_limits.max_payload_bytes)
            .map_err(|error| error.to_string())?;
        let retained = envelope.artifact_binding_payloads.iter().try_fold(0u64, |size, binding| {
            size.checked_add(binding.retained_byte_len()).filter(|size| *size <= host_io_limits.max_payload_bytes)
                .ok_or("tool-process inline bindings exceed their retained byte ceiling")
        })?;
        for binding in &envelope.artifact_binding_payloads {
            let maximum = if binding.retained_byte_len() == 0 { host_io_limits.max_payload_bytes - retained } else { host_io_limits.max_payload_bytes };
            binding.read_bounded(maximum)
                .map_err(|error| format!("tool-process bound payload preparation failed: {error}"))?;
        }
        Ok(Self {
            artifact: envelope.artifact.clone(),
            label: manifest.manifest.label,
            payload,
            artifact_bindings: envelope.artifact_binding_payloads.clone(),
            metadata: envelope.metadata.clone(),
            source_movement_seq,
            worker_environment: None,
            worker_environment_kind: None,
            output_source: None,
        })
    }

    fn materialize_for_process(
        self,
        root: &Path,
        request_id: u64,
        host_io_limits: &InterfaceHostIoLimits,
    ) -> Result<ToolProcessWireEnvelope, String> {
        std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
        let directory = materialize_owner_private_descendant(root, &[request_id.to_string(), "artifact".into()])?;
        let artifact_file =
            InterfaceArtifactFile::write(&directory, &self.label, &self.payload)
                .map_err(|error| {
                    format!("tool-process invocation artifact failed to materialize: {error}")
                })?;
        let mut artifact_bindings = Vec::with_capacity(self.artifact_bindings.len());
        for binding in self.artifact_bindings {
            artifact_bindings.push(materialize_tool_process_binding(root, binding)?);
        }
        let bytes = serde_json::to_vec(&ToolProcessInvocationContext {
            artifact_bindings, metadata: self.metadata,
        }).map_err(|error| format!("tool-process context carrier cannot encode: {error}"))?;
        if bytes.len() as u64 > host_io_limits.max_payload_bytes {
            return Err("tool-process context carrier exceeds its declared payload byte ceiling".into());
        }
        let content_hash = ContentHash::of_bytes(&bytes);
        let context = ToolProcessContextCarrier {
            path: directory.join(format!("invocation-context-{}.json", content_hash.to_hex())),
            content_hash,
            byte_len: bytes.len() as u64,
        };
        match octopus_file_storage::write_new(&context.path, &bytes) {
            Ok(file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|error| error.to_string())?;
                }
                file.sync_all().map_err(|error| error.to_string())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                context.read_bytes(root, request_id, host_io_limits)?;
            }
            Err(error) => return Err(format!("tool-process context carrier cannot be published: {error}")),
        }
        sync_tool_process_owner_directory(&directory)?;
        Ok(ToolProcessWireEnvelope {
            artifact: self.artifact,
            label: self.label,
            manifest_path: artifact_file.manifest_path().to_path_buf(),
            context,
            source_movement_seq: self.source_movement_seq,
            worker_environment: self.worker_environment,
            worker_environment_kind: self.worker_environment_kind,
        })
    }
}

impl ToolProcessWireEnvelope {
    fn materialize(
        self,
        root: &Path,
        request_id: u64,
        host_io_limits: &InterfaceHostIoLimits,
    ) -> Result<InterfaceInvocationEnvelope, String> {
        let context = self.context.read(root, request_id, host_io_limits)?;
        let request_root = root.join(request_id.to_string());
        let artifact_file =
            read_tool_process_input_artifact(&self.manifest_path, &request_root, host_io_limits)?;
        let manifest = artifact_file
            .read_manifest_bounded(host_io_limits.max_manifest_bytes)
            .map_err(|error| error.to_string())?;
        if manifest.manifest.label != self.label {
            return Err(format!(
                "tool-process invocation artifact label changed across the control boundary: expected {:?}, observed {:?}",
                self.label, manifest.manifest.label
            ));
        }
        artifact_file.read_payload_bytes_bounded(host_io_limits.max_payload_bytes)
            .map_err(|error| error.to_string())?;
        let mut artifact_binding_payloads = Vec::with_capacity(context.artifact_bindings.len());
        for binding in context.artifact_bindings {
            artifact_binding_payloads.push(read_tool_process_bound_artifact(&binding, root, host_io_limits)?);
        }
        Ok(InterfaceInvocationEnvelope {
            artifact: self.artifact,
            artifact_file,
            artifact_binding_payloads,
            metadata: context.metadata,
        })
    }
}

fn tool_process_binding_root(
    root: &Path,
    artifact: &ArtifactRef,
    payload_hash: &ContentHash,
) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("tool-process invocation root cannot be resolved: {error}"))?;
    let reference = serde_json::to_vec(artifact)
        .map_err(|error| format!("tool-process bound reference cannot encode: {error}"))?;
    Ok(root.join("bound-artifacts").join(format!(
        "{}-{}",
        ContentHash::of_bytes(&reference).to_hex(),
        payload_hash.to_hex(),
    )))
}

fn read_tool_process_bound_artifact(
    binding: &ToolProcessBoundArtifactCarrier,
    root: &Path,
    limits: &InterfaceHostIoLimits,
) -> Result<ArtifactBindingPayload, String> {
    let expected = tool_process_binding_root(root, &binding.artifact, &binding.payload_hash)?;
    if binding.manifest_path != expected.join("artifact.json")
        || expected.canonicalize().map_err(|error| {
            format!("tool-process bound artifact directory cannot be resolved: {error}")
        })? != expected
    {
        return Err(
            "tool-process bound artifact escaped its exact generation and reference".into(),
        );
    }
    let manifest_bytes =
        read_tool_process_binding_bytes(root, &binding.manifest_path, limits.max_manifest_bytes)?;
    let manifest: InterfaceArtifactManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| error.to_string())?;
    if manifest.label != "tool-process.bound-artifact"
        || manifest.payload_path != expected.join("payload")
        || manifest.payload_content_hash.as_deref() != Some(binding.payload_hash.to_hex().as_str())
    {
        return Err("tool-process bound artifact has a different payload contract".into());
    }
    ArtifactBindingPayload::from_pinned_file(
        binding.artifact.clone(), expected.join("payload"), binding.payload_hash, limits.max_payload_bytes,
    )
        .map_err(|error| error.to_string())
}

fn read_tool_process_binding_bytes(
    root: &Path,
    path: &Path,
    maximum: u64,
) -> Result<Vec<u8>, String> {
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let relative = path
        .strip_prefix(&canonical_root)
        .map_err(|_| "tool-process bound carrier escaped its generation")?;
    let file = open_private_read_source(
        &canonical_root,
        relative
            .to_str()
            .ok_or("tool-process bound carrier path is not UTF-8")?,
    )?;
    if file.metadata().map_err(|error| error.to_string())?.len() > maximum {
        return Err("tool-process bound carrier exceeds its byte limit".into());
    }
    let limit = maximum
        .checked_add(1)
        .ok_or("tool-process bound carrier byte limit overflowed")?;
    let mut bytes = Vec::new();
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > maximum {
        return Err("tool-process bound carrier exceeds its byte limit".into());
    }
    Ok(bytes)
}

fn materialize_tool_process_binding(
    root: &Path,
    binding: ToolProcessBoundArtifact,
) -> Result<ToolProcessBoundArtifactCarrier, String> {
    let payload_hash = binding.payload_hash();
    let destination = tool_process_binding_root(root, &binding.artifact, &payload_hash)?;
    let parent = destination
        .parent()
        .ok_or("tool-process bound artifact has no parent")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("tool-process bound artifact root cannot be created: {error}"))?;
    if parent.canonicalize().map_err(|error| error.to_string())? != parent {
        return Err("tool-process bound artifact root is a symbolic link".into());
    }
    let carrier = ToolProcessBoundArtifactCarrier {
        artifact: binding.artifact.clone(),
        manifest_path: destination.join("artifact.json"),
        payload_hash,
    };
    let manifest = InterfaceArtifactManifest {
        label: "tool-process.bound-artifact".into(),
        payload_path: destination.join("payload"),
        payload_content_hash: Some(carrier.payload_hash.to_hex()),
    };
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|error| format!("tool-process bound artifact manifest cannot encode: {error}"))?;
    let verify = || {
        if destination
            .canonicalize()
            .map_err(|error| error.to_string())?
            != destination
        {
            return Err("tool-process bound artifact directory is a symbolic link".into());
        }
        if read_tool_process_binding_bytes(
            root,
            &carrier.manifest_path,
            manifest_bytes.len() as u64,
        )? != manifest_bytes
        {
            return Err("tool-process bound artifact changed its immutable manifest".into());
        }
        let bytes = read_tool_process_binding_bytes(root, &destination.join("payload"), binding.payload_len())?;
        if bytes.len() as u64 != binding.payload_len() || ContentHash::of_bytes(&bytes) != payload_hash {
            return Err("tool-process bound artifact changed its immutable bytes".into());
        }
        Ok(())
    };
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => {
            verify()?;
            return Ok(carrier);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "tool-process bound artifact cannot be inspected: {error}"
            ));
        }
    }
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| "tool-process bound artifact publication sequence overflowed")?;
    let temporary = parent.join(format!(".binding-{}-{sequence}", std::process::id()));
    std::fs::create_dir(&temporary)
        .map_err(|error| format!("tool-process bound artifact stage cannot be created: {error}"))?;
    let result = (|| -> Result<(), String> {
        #[cfg(unix)]
        let directory_permissions = {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::metadata(&temporary)
                .map_err(|error| error.to_string())?
                .permissions();
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
            permissions
        };
        let payload = binding.read_bytes().map_err(|error| error.to_string())?;
        for (name, bytes) in [
            ("payload", payload.as_ref()),
            ("artifact.json", manifest_bytes.as_slice()),
        ] {
            let file = octopus_file_storage::write_new(&temporary.join(name), bytes)
                .map_err(|error| error.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|error| error.to_string())?;
            }
            file.sync_all().map_err(|error| error.to_string())?;
        }
        drop(payload);
        #[cfg(unix)]
        std::fs::set_permissions(&temporary, directory_permissions)
            .map_err(|error| error.to_string())?;
        sync_tool_process_owner_directory(&temporary)?;
        match std::fs::rename(&temporary, &destination) {
            Ok(()) => sync_tool_process_owner_directory(parent)?,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                std::fs::remove_dir_all(&temporary).map_err(|error| error.to_string())?;
            }
            Err(error) => {
                return Err(format!(
                    "tool-process bound artifact cannot be published: {error}"
                ));
            }
        }
        verify()
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temporary);
    }
    result?;
    Ok(carrier)
}

fn read_tool_process_input_artifact(
    manifest_path: &Path,
    request_root: &Path,
    host_io_limits: &InterfaceHostIoLimits,
) -> Result<InterfaceArtifactFile, String> {
    let request_root = request_root.canonicalize().map_err(|error| {
        format!(
            "tool-process request root '{}' is unavailable: {error}",
            request_root.display()
        )
    })?;
    let manifest_path = manifest_path.canonicalize().map_err(|error| {
        format!(
            "tool-process input manifest '{}' is unavailable: {error}",
            manifest_path.display()
        )
    })?;
    if !manifest_path.starts_with(&request_root) {
        return Err(format!(
            "tool-process input manifest '{}' escapes request root '{}'",
            manifest_path.display(),
            request_root.display()
        ));
    }
    let artifact_file =
        InterfaceArtifactFile::new(&manifest_path).map_err(|error| error.to_string())?;
    let manifest = artifact_file
        .read_manifest_bounded(host_io_limits.max_manifest_bytes)
        .map_err(|error| error.to_string())?;
    let payload_path = manifest
        .manifest
        .payload_path
        .canonicalize()
        .map_err(|error| {
            format!(
                "tool-process input payload '{}' is unavailable: {error}",
                manifest.manifest.payload_path.display()
            )
        })?;
    if !payload_path.starts_with(&request_root) {
        return Err(format!(
            "tool-process input payload '{}' escapes request root '{}'",
            payload_path.display(),
            request_root.display()
        ));
    }
    artifact_file
        .capture_bounded(
            host_io_limits.max_manifest_bytes,
            host_io_limits.max_payload_bytes,
        )
        .map_err(|error| error.to_string())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ToolProcessInput {
    Invoke {
        request_id: u64,
        descriptor: ToolDescriptor,
        envelope: ToolProcessWireEnvelope,
    },
    ReleaseWorkerEnvironment {
        request_id: u64,
        formation: WorkerEnvironmentFormation,
        worker_environment_kind: WorkerEnvKindSpec,
        mode: WorkerEnvironmentReleaseMode,
    },
    DeliveryVerdict {
        request_id: u64,
        event_id: u64,
        verdict: DeliveryVerdict,
    },
    DeliveryFailure {
        request_id: u64,
        event_id: u64,
        error: String,
    },
    WorkerEnvironmentLifecycleRecorded {
        request_id: u64,
        event_id: u64,
        error: Option<String>,
    },
    WorkerEnvironmentLiveStateRecorded {
        request_id: u64,
        event_id: u64,
        error: Option<String>,
    },
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ToolProcessOutput {
    ArtifactArrival {
        request_id: u64,
        event_id: u64,
        manifest_path: PathBuf,
    },
    SystemEmission {
        request_id: u64,
        event_id: u64,
        artifact: ToolProcessEmissionArtifact,
        to_interface: String,
    },
    WorkerEnvironmentLifecycle {
        request_id: u64,
        event_id: u64,
        worker_environment_id: String,
        lifecycle: WorkerEnvironmentLifecycle,
    },
    WorkerEnvironmentLiveState {
        request_id: u64,
        event_id: u64,
        worker_environment_id: String,
        state: serde_json::Value,
    },
    Complete {
        request_id: u64,
        artifact_manifest_paths: Vec<PathBuf>,
    },
    WorkerEnvironmentReleased {
        request_id: u64,
    },
    Failed {
        request_id: u64,
        error: String,
    },
    Unavailable {
        request_id: u64,
        error: String,
    },
}

pub struct ToolProcessInvocation {
    pub request_id: u64,
    pub descriptor: ToolDescriptor,
    pub envelope: InterfaceInvocationEnvelope,
    pub source_movement_seq: u64,
    pub worker_environment: Option<WorkerEnvironmentFormation>,
    pub worker_environment_kind: Option<WorkerEnvKindSpec>,
}

pub enum ToolProcessError {
    Capability(String),
    Unavailable(String),
}

impl std::fmt::Display for ToolProcessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capability(message) | Self::Unavailable(message) => formatter.write_str(message),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerEnvironmentReleaseMode {
    LiveOwner,
    ColdRecovery,
}

#[derive(Clone, Copy)]
pub struct WorkerEnvironmentReleaseContext<'a> {
    pub formation: &'a WorkerEnvironmentFormation,
    pub kind: &'a WorkerEnvKindSpec,
    pub mode: WorkerEnvironmentReleaseMode,
}

pub trait ToolProcessHandler: Send + Sync {
    fn substrate_family(&self) -> &str;

    fn releases_cold(&self) -> bool {
        false
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolProcessInvocation,
        io: ToolProcessIo,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<InterfaceArtifactFile>, ToolProcessError>> + Send + 'a>,
    >;

    fn release<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        _kind: &'a WorkerEnvKindSpec,
    ) -> Pin<Box<dyn Future<Output = Result<(), ToolProcessError>> + Send + 'a>> {
        Box::pin(async move {
            if formation.requires_release {
                Err(ToolProcessError::Capability(format!(
                    "worker environment '{}' requires release but its package implements no release hook",
                    formation.worker_environment_id
                )))
            } else {
                Ok(())
            }
        })
    }

    fn release_with_context<'a>(
        &'a self,
        context: WorkerEnvironmentReleaseContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), ToolProcessError>> + Send + 'a>> {
        Box::pin(async move {
            match context.mode {
                WorkerEnvironmentReleaseMode::ColdRecovery if !self.releases_cold() => {
                    Err(ToolProcessError::Capability(format!(
                        "worker environment '{}' requires cold release but its package implements no cold recovery hook",
                        context.formation.worker_environment_id
                    )))
                }
                WorkerEnvironmentReleaseMode::LiveOwner
                | WorkerEnvironmentReleaseMode::ColdRecovery => {
                    self.release(context.formation, context.kind).await
                }
            }
        })
    }

    fn quiesce<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<(), ToolProcessError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

enum ToolProcessCommand {
    Invoke {
        request_id: u64,
        descriptor: ToolDescriptor,
        envelope: ToolProcessWireEnvelope,
    },
    Release {
        request_id: u64,
        formation: WorkerEnvironmentFormation,
        kind: WorkerEnvKindSpec,
        mode: WorkerEnvironmentReleaseMode,
    },
}

type PendingToolVerdicts = Arc<
    tokio::sync::Mutex<
        BTreeMap<(u64, u64), tokio::sync::oneshot::Sender<Result<DeliveryVerdict, String>>>,
    >,
>;

type PendingWorkerEnvironmentRecords =
    Arc<tokio::sync::Mutex<BTreeMap<(u64, u64), tokio::sync::oneshot::Sender<Result<(), String>>>>>;

#[derive(Clone)]
pub struct ToolProcessIo {
    request_id: u64,
    next_event_id: Arc<AtomicU64>,
    output: tokio::sync::mpsc::Sender<ToolProcessOutput>,
    pending: PendingToolVerdicts,
    pending_worker_environment_records: PendingWorkerEnvironmentRecords,
    input_open: Arc<std::sync::atomic::AtomicBool>,
}

pub trait WorkerEnvironmentLifecycleRecorder: Send + Sync {
    fn record<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        lifecycle: WorkerEnvironmentLifecycle,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

pub trait WorkerEnvironmentLiveStateRecorder: Send + Sync {
    fn record_live_state<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        state: &'a serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

impl WorkerEnvironmentLifecycleRecorder for ToolProcessIo {
    fn record<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        lifecycle: WorkerEnvironmentLifecycle,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.record_worker_environment_lifecycle(formation, lifecycle)
                .await
        })
    }
}

impl WorkerEnvironmentLiveStateRecorder for ToolProcessIo {
    fn record_live_state<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        state: &'a serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            self.record_worker_environment_live_state(formation, state)
                .await
        })
    }
}

impl ToolProcessIo {
    fn next_delivery_event_id(&self) -> Result<u64, String> {
        self.next_event_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| "tool-process delivery event id overflowed".to_string())
    }

    async fn await_delivery_verdict(
        &self,
        event_id: u64,
        output: ToolProcessOutput,
        send_error: &str,
        receive_error: &str,
    ) -> Result<DeliveryVerdict, String> {
        if !self.input_open.load(Ordering::Acquire) {
            return Err(receive_error.to_string());
        }
        let key = (self.request_id, event_id);
        let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.insert(key, verdict_tx);
        if !self.input_open.load(Ordering::Acquire) {
            self.pending.lock().await.remove(&key);
            return Err(receive_error.to_string());
        }
        if self.output.send(output).await.is_err() {
            self.pending.lock().await.remove(&key);
            return Err(send_error.to_string());
        }
        match verdict_rx.await {
            Ok(verdict) => verdict,
            Err(_) => {
                self.pending.lock().await.remove(&key);
                Err(receive_error.to_string())
            }
        }
    }

    pub async fn hand_output_artifact(
        &self,
        artifact: &InterfaceArtifactFile,
    ) -> Result<DeliveryVerdict, String> {
        let event_id = self.next_delivery_event_id()?;
        self.await_delivery_verdict(
            event_id,
            ToolProcessOutput::ArtifactArrival {
                request_id: self.request_id,
                event_id,
                manifest_path: artifact.manifest_path().to_path_buf(),
            },
            "tool-process output channel closed during artifact arrival",
            "tool-process artifact-arrival verdict channel dropped",
        )
        .await
    }

    pub async fn hand_system_emission(
        &self,
        artifact: &SystemInterfaceEmissionArtifact,
        to_interface: &InterfaceId,
    ) -> Result<DeliveryVerdict, String> {
        let event_id = self.next_delivery_event_id()?;
        self.await_delivery_verdict(
            event_id,
            ToolProcessOutput::SystemEmission {
                request_id: self.request_id,
                event_id,
                artifact: match artifact {
                    SystemInterfaceEmissionArtifact::Materialized(artifact) => {
                        ToolProcessEmissionArtifact::Materialized {
                            manifest_path: artifact.manifest_path().to_path_buf(),
                        }
                    }
                    SystemInterfaceEmissionArtifact::Reference(artifact) => {
                        ToolProcessEmissionArtifact::Reference {
                            artifact: artifact.clone(),
                        }
                    }
                },
                to_interface: to_interface.as_str().to_string(),
            },
            "tool-process output channel closed during system emission",
            "tool-process system-emission verdict channel dropped",
        )
        .await
    }

    pub async fn record_worker_environment_lifecycle(
        &self,
        formation: &WorkerEnvironmentFormation,
        lifecycle: WorkerEnvironmentLifecycle,
    ) -> Result<(), String> {
        if !self.input_open.load(Ordering::Acquire) {
            return Err(
                "tool-process input closed before worker-environment lifecycle acknowledgement"
                    .to_string(),
            );
        }
        let event_id = self.next_delivery_event_id()?;
        let (recorded_tx, recorded_rx) = tokio::sync::oneshot::channel();
        self.pending_worker_environment_records
            .lock()
            .await
            .insert((self.request_id, event_id), recorded_tx);
        if !self.input_open.load(Ordering::Acquire) {
            self.pending_worker_environment_records
                .lock()
                .await
                .remove(&(self.request_id, event_id));
            return Err(
                "tool-process input closed before worker-environment lifecycle acknowledgement"
                    .to_string(),
            );
        }
        if self
            .output
            .send(ToolProcessOutput::WorkerEnvironmentLifecycle {
                request_id: self.request_id,
                event_id,
                worker_environment_id: formation.worker_environment_id.as_str().to_string(),
                lifecycle,
            })
            .await
            .is_err()
        {
            self.pending_worker_environment_records
                .lock()
                .await
                .remove(&(self.request_id, event_id));
            return Err(
                "tool-process output channel closed during worker-environment lifecycle"
                    .to_string(),
            );
        }
        match recorded_rx.await {
            Ok(result) => result,
            Err(_) => {
                self.pending_worker_environment_records
                    .lock()
                    .await
                    .remove(&(self.request_id, event_id));
                Err("tool-process worker-environment lifecycle acknowledgement dropped".to_string())
            }
        }
    }

    pub async fn record_worker_environment_live_state(
        &self,
        formation: &WorkerEnvironmentFormation,
        state: &serde_json::Value,
    ) -> Result<(), String> {
        if !state.is_object() {
            return Err(
                "tool-process worker-environment live state must be a JSON object".to_string(),
            );
        }
        if !self.input_open.load(Ordering::Acquire) {
            return Err(
                "tool-process input closed before worker-environment live-state acknowledgement"
                    .to_string(),
            );
        }
        let event_id = self.next_delivery_event_id()?;
        let (recorded_tx, recorded_rx) = tokio::sync::oneshot::channel();
        self.pending_worker_environment_records
            .lock()
            .await
            .insert((self.request_id, event_id), recorded_tx);
        if !self.input_open.load(Ordering::Acquire) {
            self.pending_worker_environment_records
                .lock()
                .await
                .remove(&(self.request_id, event_id));
            return Err(
                "tool-process input closed before worker-environment live-state acknowledgement"
                    .to_string(),
            );
        }
        if self
            .output
            .send(ToolProcessOutput::WorkerEnvironmentLiveState {
                request_id: self.request_id,
                event_id,
                worker_environment_id: formation.worker_environment_id.as_str().to_string(),
                state: state.clone(),
            })
            .await
            .is_err()
        {
            self.pending_worker_environment_records
                .lock()
                .await
                .remove(&(self.request_id, event_id));
            return Err(
                "tool-process output channel closed during worker-environment live state"
                    .to_string(),
            );
        }
        match recorded_rx.await {
            Ok(result) => result,
            Err(_) => {
                self.pending_worker_environment_records
                    .lock()
                    .await
                    .remove(&(self.request_id, event_id));
                Err(
                    "tool-process worker-environment live-state acknowledgement dropped"
                        .to_string(),
                )
            }
        }
    }
}

pub async fn run_tool_process(handler: Arc<dyn ToolProcessHandler>) -> Result<(), String> {
    seal_inherited_descriptors();
    let environment = ToolProcessEnvironment::from_env()?;
    let host_io_limits = environment.host_io_limits;
    let input_root = environment.var_dir.join("invocations");
    std::fs::create_dir_all(&input_root).map_err(|error| {
        format!(
            "tool-process invocation root '{}' cannot be created: {error}",
            input_root.display()
        )
    })?;
    let pending: PendingToolVerdicts = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let pending_worker_environment_records: PendingWorkerEnvironmentRecords =
        Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let input_closed = tokio_util::sync::CancellationToken::new();
    let input_close_guard = input_closed.clone().drop_guard();
    let input_open = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (command_tx, mut command_rx) = tokio::sync::mpsc::channel(8);
    let (output_tx, output_rx) = tokio::sync::mpsc::channel(8);
    let input_future = read_tool_process_inputs(
        command_tx,
        Arc::clone(&pending),
        Arc::clone(&pending_worker_environment_records),
        Arc::clone(&input_open),
        host_io_limits.max_control_frame_bytes,
        input_closed.clone(),
    );
    let input_task = tokio::spawn(async move {
        let _input_close_guard = input_close_guard;
        input_future.await
    });
    let mut output_task = Some(tokio::spawn(write_tool_process_outputs(
        output_rx,
        host_io_limits.max_control_frame_bytes,
    )));
    let next_event_id = Arc::new(AtomicU64::new(1));
    let command_result = loop {
        let command =
            match await_with_tool_process_io(&mut output_task, &input_closed, command_rx.recv())
                .await
            {
                Ok(Some(command)) => command,
                Ok(None) => break Ok(()),
                Err(_) if input_closed.is_cancelled() => break Ok(()),
                Err(error) => break Err(error),
            };
        let output = match await_with_tool_process_io(
            &mut output_task,
            &input_closed,
            execute_tool_process_command(
                handler.as_ref(),
                &environment,
                &input_root,
                command,
                &output_tx,
                &pending,
                &pending_worker_environment_records,
                &next_event_id,
                &input_open,
            ),
        )
        .await
        {
            Ok(output) => output,
            Err(error) => break Err(format!("{error}; active command outcome is unknown")),
        };
        match await_with_tool_process_io(&mut output_task, &input_closed, output_tx.send(output))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => break Err("tool-process output channel closed".to_string()),
            Err(error) => {
                break Err(format!(
                    "{error}; command completion was not delivered; outcome is unknown"
                ));
            }
        }
    };

    let command_result = match handler.quiesce().await {
        Ok(()) => command_result,
        Err(ToolProcessError::Capability(quiesce_error))
        | Err(ToolProcessError::Unavailable(quiesce_error)) => match command_result {
            Ok(()) => Err(format!("tool-process quiesce failed: {quiesce_error}")),
            Err(command_error) => Err(format!(
                "{command_error}; tool-process quiesce failed: {quiesce_error}"
            )),
        },
    };

    if let Err(mut error) = command_result {
        if !input_closed.is_cancelled() {
            input_task.abort();
        }
        drop(output_tx);
        if let Err(output_error) = finish_tool_process_output_writer(&mut output_task, true).await {
            error.push_str(&format!("; {output_error}"));
        }
        match input_task.await {
            Ok(Err(input_error)) => error.push_str(&format!("; {input_error}")),
            Err(input_error) if !input_error.is_cancelled() => {
                error.push_str(&format!("; tool-process input task failed: {input_error}"))
            }
            _ => {}
        }
        fail_pending_delivery_verdicts(&pending, &error).await;
        fail_pending_worker_environment_records(&pending_worker_environment_records, &error).await;
        return Err(error);
    }

    let input_result = input_task
        .await
        .map_err(|error| format!("tool-process input task failed: {error}"))
        .and_then(|result| result);
    drop(output_tx);
    let output_result = finish_tool_process_output_writer(&mut output_task, false).await;
    match (input_result, output_result) {
        (Err(input), Err(output)) => Err(format!("{input}; {output}")),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(unix)]
fn seal_inherited_descriptors() {
    let ceiling = unsafe { libc::getdtablesize() }.clamp(3, 65_536);
    for descriptor in 3..ceiling {
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
            unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        }
    }
}

#[cfg(not(unix))]
fn seal_inherited_descriptors() {}

async fn finish_tool_process_output_writer(
    output_task: &mut Option<tokio::task::JoinHandle<Result<(), String>>>,
    abort: bool,
) -> Result<(), String> {
    let Some(task) = output_task.take() else {
        return Ok(());
    };
    if abort {
        task.abort();
    }
    match task.await {
        Ok(Ok(())) => Ok(()),
        Err(error) if abort && error.is_cancelled() => Ok(()),
        Ok(Err(error)) => Err(format!("tool-process output task failed: {error}")),
        Err(error) => Err(format!("tool-process output task failed: {error}")),
    }
}

async fn await_with_tool_process_io<T>(
    output_task: &mut Option<tokio::task::JoinHandle<Result<(), String>>>,
    input_closed: &tokio_util::sync::CancellationToken,
    future: impl Future<Output = T>,
) -> Result<T, String> {
    tokio::select! {
        biased;
        _ = input_closed.cancelled() => Err("tool-process input closed".to_string()),
        output = await_with_tool_process_output_writer(output_task, future) => output,
    }
}

async fn await_with_tool_process_output_writer<T>(
    output_task: &mut Option<tokio::task::JoinHandle<Result<(), String>>>,
    future: impl Future<Output = T>,
) -> Result<T, String> {
    let task = output_task
        .as_mut()
        .ok_or_else(|| "tool-process output task already joined".to_string())?;
    tokio::select! {
        output = task => {
            output_task.take();
            Err(match output {
                Ok(Ok(())) => "tool-process output task closed before the command loop".to_string(),
                Ok(Err(error)) => format!("tool-process output task failed: {error}"),
                Err(error) => format!("tool-process output task failed: {error}"),
            })
        },
        output = future => Ok(output),
    }
}

async fn execute_tool_process_command(
    handler: &dyn ToolProcessHandler,
    environment: &ToolProcessEnvironment,
    input_root: &Path,
    command: ToolProcessCommand,
    output_tx: &tokio::sync::mpsc::Sender<ToolProcessOutput>,
    pending: &PendingToolVerdicts,
    pending_worker_environment_records: &PendingWorkerEnvironmentRecords,
    next_event_id: &Arc<AtomicU64>,
    input_open: &Arc<std::sync::atomic::AtomicBool>,
) -> ToolProcessOutput {
    match command {
        ToolProcessCommand::Invoke {
            request_id,
            descriptor,
            envelope,
        } => {
            let source_movement_seq = envelope.source_movement_seq;
            let worker_environment = envelope.worker_environment.clone();
            let worker_environment_kind = envelope.worker_environment_kind.clone();
            if let Err(error) = validate_tool_process_formation(
                &environment.interface,
                handler.substrate_family(),
                &descriptor,
                worker_environment.as_ref(),
                worker_environment_kind.as_ref(),
            ) {
                return ToolProcessOutput::Failed { request_id, error };
            }
            let envelope =
                match envelope.materialize(input_root, request_id, &environment.host_io_limits) {
                    Ok(envelope) => envelope,
                    Err(error) => return ToolProcessOutput::Failed { request_id, error },
                };
            let invocation = ToolProcessInvocation {
                request_id,
                descriptor,
                envelope,
                source_movement_seq,
                worker_environment,
                worker_environment_kind,
            };
            let io = ToolProcessIo {
                request_id,
                next_event_id: Arc::clone(next_event_id),
                output: output_tx.clone(),
                pending: Arc::clone(pending),
                pending_worker_environment_records: Arc::clone(pending_worker_environment_records),
                input_open: Arc::clone(input_open),
            };
            match handler.execute(invocation, io).await {
                Ok(artifacts) => ToolProcessOutput::Complete {
                    request_id,
                    artifact_manifest_paths: artifacts
                        .into_iter()
                        .map(|artifact| artifact.manifest_path().to_path_buf())
                        .collect(),
                },
                Err(ToolProcessError::Capability(error)) => {
                    ToolProcessOutput::Failed { request_id, error }
                }
                Err(ToolProcessError::Unavailable(error)) => {
                    ToolProcessOutput::Unavailable { request_id, error }
                }
            }
        }
        ToolProcessCommand::Release {
            request_id,
            formation,
            kind,
            mode,
        } => {
            let release = formation
                .validate_against_kind(&kind)
                .map_err(|error| ToolProcessError::Capability(error.to_string()))
                .and_then(|_| {
                    if formation.door_interface != environment.interface {
                        Err(ToolProcessError::Capability(format!(
                            "tool process interface '{}' received release for door '{}'",
                            environment.interface, formation.door_interface
                        )))
                    } else {
                        validate_substrate_family(handler.substrate_family(), &kind)
                            .map_err(ToolProcessError::Capability)
                    }
                });
            match release {
                Ok(()) => match handler
                    .release_with_context(WorkerEnvironmentReleaseContext {
                        formation: &formation,
                        kind: &kind,
                        mode,
                    })
                    .await
                {
                    Ok(()) => ToolProcessOutput::WorkerEnvironmentReleased { request_id },
                    Err(ToolProcessError::Capability(error)) => {
                        ToolProcessOutput::Failed { request_id, error }
                    }
                    Err(ToolProcessError::Unavailable(error)) => {
                        ToolProcessOutput::Unavailable { request_id, error }
                    }
                },
                Err(ToolProcessError::Capability(error)) => {
                    ToolProcessOutput::Failed { request_id, error }
                }
                Err(ToolProcessError::Unavailable(error)) => {
                    ToolProcessOutput::Unavailable { request_id, error }
                }
            }
        }
    }
}

fn validate_substrate_family(family: &str, kind: &WorkerEnvKindSpec) -> Result<(), String> {
    if kind.substrate_family != family {
        return Err(format!(
            "worker_environment_kind '{}' declares substrate family '{}', but this package serves '{family}'",
            kind.worker_environment_kind_id, kind.substrate_family
        ));
    }
    Ok(())
}

fn validate_tool_process_formation(
    interface: &InterfaceId,
    family: &str,
    descriptor: &ToolDescriptor,
    formation: Option<&WorkerEnvironmentFormation>,
    kind: Option<&WorkerEnvKindSpec>,
) -> Result<(), String> {
    if descriptor.interface != *interface {
        return Err(format!(
            "tool descriptor '{}' belongs to interface '{}', not tool process interface '{interface}'",
            descriptor.id, descriptor.interface
        ));
    }
    match (formation, kind) {
        (None, None) => Err(format!(
            "tool '{}' invocation carries no worker-environment formation",
            descriptor.id
        )),
        (Some(_), None) | (None, Some(_)) => Err(format!(
            "tool '{}' invocation carries an incomplete worker-environment formation",
            descriptor.id
        )),
        (Some(formation), Some(kind)) => {
            formation
                .validate_against_kind(kind)
                .map_err(|error| error.to_string())?;
            if formation.door_interface != *interface {
                return Err(format!(
                    "tool process interface '{}' received formation for door '{}'",
                    interface, formation.door_interface
                ));
            }
            validate_substrate_family(family, kind)?;
            if !descriptor
                .supported_worker_environment_kinds
                .contains(&formation.worker_environment_kind)
            {
                return Err(format!(
                    "tool '{}' does not declare formed kind '{}'",
                    descriptor.id, formation.worker_environment_kind
                ));
            }
            if !formation
                .granted_tools
                .iter()
                .any(|tool| tool == descriptor.id.as_str())
            {
                return Err(format!(
                    "tool '{}' is absent from the formed worker-environment grant",
                    descriptor.id
                ));
            }
            let expected_hash = formation
                .tool_descriptor_hashes
                .get(descriptor.id.as_str())
                .ok_or_else(|| {
                    format!(
                        "tool '{}' has no descriptor hash in the formed worker environment",
                        descriptor.id
                    )
                })?;
            let observed_hash = canonical_hash(descriptor)?;
            if expected_hash != &observed_hash {
                return Err(format!(
                    "tool '{}' descriptor changed after worker-environment formation",
                    descriptor.id
                ));
            }
            Ok(())
        }
    }
}

async fn read_tool_process_inputs(
    command_tx: tokio::sync::mpsc::Sender<ToolProcessCommand>,
    pending: PendingToolVerdicts,
    pending_worker_environment_records: PendingWorkerEnvironmentRecords,
    input_open: Arc<std::sync::atomic::AtomicBool>,
    maximum_frame_bytes: u64,
    input_closed: tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    let result = async {
        let mut lines = BoundedJsonLineReader::new(tokio::io::stdin(), maximum_frame_bytes)?;
        while let Some(line) = lines.next_line().await? {
            let input: ToolProcessInput = serde_json::from_str(&line)
                .map_err(|error| format!("tool-process stdin line is malformed: {error}"))?;
            match input {
            ToolProcessInput::Invoke {
                request_id,
                descriptor,
                envelope,
            } => command_tx
                .send(ToolProcessCommand::Invoke {
                    request_id,
                    descriptor,
                    envelope,
                })
                .await
                .map_err(|_| "tool-process invocation channel closed".to_string())?,
            ToolProcessInput::ReleaseWorkerEnvironment {
                request_id,
                formation,
                worker_environment_kind,
                mode,
            } => command_tx
                .send(ToolProcessCommand::Release {
                    request_id,
                    formation,
                    kind: worker_environment_kind,
                    mode,
                })
                .await
                .map_err(|_| "tool-process release channel closed".to_string())?,
            ToolProcessInput::DeliveryVerdict {
                request_id,
                event_id,
                verdict,
            } => {
                let sender = pending
                    .lock()
                    .await
                    .remove(&(request_id, event_id))
                    .ok_or_else(|| {
                        format!(
                            "tool-process received unknown delivery verdict {request_id}/{event_id}"
                        )
                    })?;
                let _ = sender.send(Ok(verdict));
            }
            ToolProcessInput::DeliveryFailure {
                request_id,
                event_id,
                error,
            } => {
                let sender = pending
                    .lock()
                    .await
                    .remove(&(request_id, event_id))
                    .ok_or_else(|| {
                        format!(
                            "tool-process received unknown delivery failure {request_id}/{event_id}"
                        )
                    })?;
                let _ = sender.send(Err(error));
            }
            ToolProcessInput::WorkerEnvironmentLifecycleRecorded {
                request_id,
                event_id,
                error,
            } => {
                let sender = pending_worker_environment_records
                    .lock()
                    .await
                    .remove(&(request_id, event_id))
                    .ok_or_else(|| {
                        format!(
                            "tool-process received unknown worker-environment lifecycle acknowledgement {request_id}/{event_id}"
                        )
                    })?;
                let _ = sender.send(match error {
                    Some(error) => Err(error),
                    None => Ok(()),
                });
            }
            ToolProcessInput::WorkerEnvironmentLiveStateRecorded {
                request_id,
                event_id,
                error,
            } => {
                let sender = pending_worker_environment_records
                    .lock()
                    .await
                    .remove(&(request_id, event_id))
                    .ok_or_else(|| {
                        format!(
                            "tool-process received unknown worker-environment live-state acknowledgement {request_id}/{event_id}"
                        )
                    })?;
                let _ = sender.send(match error {
                    Some(error) => Err(error),
                    None => Ok(()),
                });
            }
            }
        }
        Ok(())
    }
    .await;
    input_open.store(false, Ordering::Release);
    input_closed.cancel();
    fail_pending_delivery_verdicts(
        &pending,
        "tool-process stdin closed before delivery verdict",
    )
    .await;
    let mut pending_worker_environment_records = pending_worker_environment_records.lock().await;
    for (_, sender) in std::mem::take(&mut *pending_worker_environment_records) {
        let _ = sender.send(Err(
            "tool-process stdin closed before worker-environment record acknowledgement"
                .to_string(),
        ));
    }
    result
}

async fn fail_pending_delivery_verdicts(pending: &PendingToolVerdicts, error: &str) {
    let mut pending = pending.lock().await;
    for (_, sender) in std::mem::take(&mut *pending) {
        let _ = sender.send(Err(error.to_string()));
    }
}

async fn fail_pending_worker_environment_records(
    pending: &PendingWorkerEnvironmentRecords,
    error: &str,
) {
    let mut pending = pending.lock().await;
    for (_, sender) in std::mem::take(&mut *pending) {
        let _ = sender.send(Err(error.to_string()));
    }
}

async fn write_tool_process_outputs(
    mut output_rx: tokio::sync::mpsc::Receiver<ToolProcessOutput>,
    maximum_frame_bytes: u64,
) -> Result<(), String> {
    let mut stdout = tokio::io::BufWriter::new(tokio::io::stdout());
    while let Some(output) = output_rx.recv().await {
        let bytes =
            serialize_bounded_json_line(&output, maximum_frame_bytes, "tool-process output frame")?;
        stdout
            .write_all(&bytes)
            .await
            .map_err(|error| format!("tool-process stdout write failed: {error}"))?;
        stdout
            .flush()
            .await
            .map_err(|error| format!("tool-process stdout flush failed: {error}"))?;
    }
    Ok(())
}

struct ToolProcessClient {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: BoundedJsonLineReader<tokio::process::ChildStdout>,
    stderr_failure: tokio::sync::oneshot::Receiver<String>,
    stderr_open: bool,
    stderr_path: PathBuf,
    next_request_id: u64,
    input_root: PathBuf,
    output_roots: Vec<PathBuf>,
    generation_root: PathBuf,
    generation_dir: PathBuf,
    host_io_limits: InterfaceHostIoLimits,
    formation: Option<WorkerEnvironmentFormation>,
    worker_environment_kind: Option<WorkerEnvKindSpec>,
    last_source_movement_seq: u64,
    _sandbox_profile: Option<Arc<octopus_verifiers::process_authority::SandboxProfileFile>>,
}

struct ToolProcessCompletion {
    artifacts: Vec<InterfaceArtifactFile>,
    streamed_output_admitted: bool,
    primary_input: Option<ToolProcessPrimaryInput>,
    invocation_manifest: PathBuf,
    generation: PathBuf,
}

struct ToolProcessPrimaryInput {
    directory: std::fs::File,
    files: Vec<(std::ffi::OsString, ContentHash, std::fs::Metadata)>,
}

async fn hand_completed_tool_outputs(
    ctx: &InterfaceLiftContext,
    input: &ToolProcessEnvelope,
    completion: ToolProcessCompletion,
    callback: Option<&ToolDoorOutputCallback>,
    primary_input: &mut Option<ToolProcessPrimaryInput>,
) -> ToolProcessBrokerResult {
    if completion.artifacts.is_empty() {
        *primary_input = completion.primary_input;
        return ToolProcessBrokerResult::Completed;
    }
    let mut retained = match completed_output::CompletedOutputs::retain(ctx, input, &completion) {
        Ok(retained) => retained,
        Err(error) => return ToolProcessBrokerResult::OutputPending { error },
    };
    match retained.deliver(ctx, input, callback, false).await {
        Ok(ToolProcessBrokerResult::Completed) => {
            *primary_input = completion.primary_input;
            ToolProcessBrokerResult::Completed
        }
        Ok(result) => result,
        Err(error) => ToolProcessBrokerResult::OutputPending { error },
    }
}

#[cfg(unix)]
impl ToolProcessPrimaryInput {
    fn capture_invocation(root: &Path, request_id: u64, envelope: &ToolProcessWireEnvelope, limits: &InterfaceHostIoLimits) -> Result<Self, String> {
        let mut owned = Self::capture(root, request_id, &envelope.manifest_path, limits)?;
        let expected = envelope.context.read_bytes(root, request_id, limits)?;
        let name = envelope.context.path.file_name().ok_or("tool-process context carrier has no filename")?;
        let (bytes, metadata) = owned.read_file(name, envelope.context.byte_len)?;
        if bytes != expected { return Err("tool-process context carrier changed before dispatch".into()); }
        owned.files.push((name.to_os_string(), envelope.context.content_hash, metadata));
        Ok(owned)
    }

    fn capture(root: &Path, request_id: u64, manifest_path: &Path, limits: &InterfaceHostIoLimits) -> Result<Self, String> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::OpenOptionsExt;
        let root = root.canonicalize().map_err(|error| error.to_string())?;
        let request_root = root.join(request_id.to_string());
        let directory_path = request_root.join("artifact");
        if manifest_path.parent() != Some(directory_path.as_path()) {
            return Err("tool-process primary manifest left its exact request directory".into());
        }
        let artifact = read_tool_process_input_artifact(manifest_path, &request_root, limits)?;
        let snapshot = artifact.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .map_err(|error| error.to_string())?;
        if snapshot.manifest.payload_path.parent() != Some(directory_path.as_path()) {
            return Err("tool-process primary payload left its exact request directory".into());
        }
        let mut directory = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY).open(&root)
            .map_err(|error| error.to_string())?;
        for component in [request_id.to_string(), "artifact".to_string()] {
            let component = std::ffi::CString::new(component).map_err(|error| error.to_string())?;
            let descriptor = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
            if descriptor < 0 {
                return Err(format!("tool-process primary input directory cannot be opened: {}", std::io::Error::last_os_error()));
            }
            directory = unsafe { std::fs::File::from_raw_fd(descriptor) };
        }
        let mut owned = Self { directory, files: Vec::with_capacity(2) };
        for (path, expected) in [(manifest_path, snapshot.manifest_bytes.as_slice()),
            (snapshot.manifest.payload_path.as_path(), snapshot.payload_bytes.as_slice())]
        {
            let name = path.file_name().ok_or("tool-process primary input has no filename")?;
            let (bytes, metadata) = owned.read_file(name, expected.len() as u64)?;
            if bytes != expected { return Err("tool-process primary input changed before dispatch".into()); }
            owned.files.push((name.to_os_string(), ContentHash::of_bytes(&bytes), metadata));
        }
        Ok(owned)
    }

    fn read_file(&self, name: &std::ffi::OsStr, maximum: u64) -> Result<(Vec<u8>, std::fs::Metadata), String> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|error| error.to_string())?;
        let descriptor = unsafe { libc::openat(self.directory.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if descriptor < 0 { return Err(format!("tool-process primary input cannot be opened: {}", std::io::Error::last_os_error())); }
        let mut file = unsafe { std::fs::File::from_raw_fd(descriptor) };
        let before = file.metadata().map_err(|error| error.to_string())?;
        if !before.is_file() || before.nlink() != 1 || before.uid() != unsafe { libc::geteuid() } || before.len() > maximum {
            return Err("tool-process primary input changed its private regular-file boundary".into());
        }
        let mut bytes = Vec::new();
        (&mut file).take(maximum.saturating_add(1)).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
        let after = file.metadata().map_err(|error| error.to_string())?;
        if bytes.len() as u64 != before.len() || !Self::same_file(&before, &after) {
            return Err("tool-process primary input changed during verification".into());
        }
        Ok((bytes, after))
    }

    fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino() && left.mode() == right.mode()
            && left.uid() == right.uid() && left.nlink() == right.nlink() && left.len() == right.len()
            && left.mtime() == right.mtime() && left.mtime_nsec() == right.mtime_nsec()
            && left.ctime() == right.ctime() && left.ctime_nsec() == right.ctime_nsec()
    }

    fn retire(self) -> Result<(), String> {
        use std::os::{fd::AsRawFd, unix::ffi::OsStrExt};
        for (name, hash, expected) in &self.files {
            let (bytes, observed) = self.read_file(name, expected.len())?;
            if !Self::same_file(expected, &observed) || ContentHash::of_bytes(&bytes) != *hash {
                return Err("tool-process primary input changed before retirement".into());
            }
        }
        for (name, _, _) in &self.files {
            let name = std::ffi::CString::new(name.as_bytes()).map_err(|error| error.to_string())?;
            if unsafe { libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) } < 0 {
                return Err(format!("tool-process primary input retirement failed: {}", std::io::Error::last_os_error()));
            }
        }
        self.directory.sync_all().map_err(|error| {
            format!("tool-process primary input retirement directory sync failed: {error}")
        })
    }
}

#[cfg(not(unix))]
impl ToolProcessPrimaryInput {
    fn capture_invocation(_root: &Path, _request_id: u64, _envelope: &ToolProcessWireEnvelope, _limits: &InterfaceHostIoLimits) -> Result<Self, String> {
        Err("tool-process primary input retirement requires descriptor-relative file operations".into())
    }
    fn capture(_root: &Path, _request_id: u64, _manifest_path: &Path, _limits: &InterfaceHostIoLimits) -> Result<Self, String> {
        Err("tool-process primary input retirement requires descriptor-relative file operations".into())
    }
    fn retire(self) -> Result<(), String> {
        Err("tool-process primary input retirement requires descriptor-relative file operations".into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ToolProcessKey {
    declaration: ToolProcessDeclaration,
    effective_declaration_hash: String,
    formation_id: Option<WorkerEnvironmentId>,
    formation_scope: Option<String>,
    private_read_snapshot_hash: String,
    effective_environment_hash: String,
    invocation_authority_hash: String,
    execution_inputs_hash: String,
}

fn attest_tool_process_source_interface(
    interface: &InterfaceId,
    metadata: Option<&InvocationMetadata>,
) -> Result<(), ToolProcessInvokeError> {
    let metadata = metadata.ok_or_else(|| {
        ToolProcessInvokeError::Capability(
            "tool process invocation carries no host-attested metadata".to_string(),
        )
    })?;
    let runtime = metadata.runtime.as_ref().ok_or_else(|| {
        ToolProcessInvokeError::Capability(
            "tool process invocation carries no host-attested runtime coordinates".to_string(),
        )
    })?;
    if runtime.interface != *interface {
        return Err(ToolProcessInvokeError::Capability(format!(
            "tool process invocation is attested for interface '{}', not host interface '{}'",
            runtime.interface.as_str(),
            interface.as_str()
        )));
    }
    Ok(())
}

impl ToolProcessClient {
    fn spawn(
        ctx: &InterfaceLiftContext,
        declaration: &ToolProcessDeclaration,
        arguments: Option<&serde_json::Value>,
        formation: Option<&WorkerEnvironmentFormation>,
        worker_environment_kind: Option<&WorkerEnvKindSpec>,
        private_read_max_bytes_ceiling: u64,
        expected_effective_declaration_hash: &str,
        expected_private_read_snapshot_hash: &str,
        expected_effective_environment_hash: &str,
        expected_invocation_authority_hash: &str,
        expected_execution_inputs_hash: &str,
    ) -> Result<Self, ToolProcessInvokeError> {
        if formation.is_some() != worker_environment_kind.is_some() {
            return Err(ToolProcessInvokeError::Unavailable(
                "tool process spawn carries an incomplete worker-environment formation".to_string(),
            ));
        }
        let effective_declaration_hash =
            canonical_hash(declaration).map_err(ToolProcessInvokeError::Unavailable)?;
        if effective_declaration_hash != expected_effective_declaration_hash {
            return Err(ToolProcessInvokeError::Unavailable(
                "tool process declaration changed during spawn".to_string(),
            ));
        }
        let environment = effective_tool_process_environment(ctx, declaration)
            .map_err(ToolProcessInvokeError::Unavailable)?;
        let execution = execution_inputs::ExecutionInputCache::default()
            .resolve(ctx, declaration, worker_environment_kind, &environment)
            .map_err(ToolProcessInvokeError::Unavailable)?;
        if execution.hash != expected_execution_inputs_hash {
            return Err(ToolProcessInvokeError::Unavailable(
                "tool executable or support files changed during spawn".into(),
            ));
        }
        let executable_base = execution.base.clone();
        let effective_environment_hash = tool_process_environment_hash(&environment);
        if effective_environment_hash != expected_effective_environment_hash {
            return Err(ToolProcessInvokeError::Unavailable(
                "tool process inherited environment changed during spawn".to_string(),
            ));
        }
        let invocation_authority_hash = match arguments {
            Some(arguments) => {
                invocation_authority_hash(&declaration.authority, &ctx.root_slots, arguments)
            }
            None if declaration.authority.has_argument_paths() => {
                return Err(ToolProcessInvokeError::Unavailable(
                    "tool process declares invocation authority without invocation arguments"
                        .to_string(),
                ));
            }
            None => Ok(empty_invocation_authority_hash()),
        }
        .map_err(|error| match error {
            ProcessInvocationAuthorityError::Unavailable(error) => {
                ToolProcessInvokeError::Unavailable(error)
            }
            ProcessInvocationAuthorityError::Refused(error) => {
                ToolProcessInvokeError::Capability(error)
            }
        })?;
        if invocation_authority_hash != expected_invocation_authority_hash {
            return Err(ToolProcessInvokeError::Unavailable(
                "tool process invocation authority changed during spawn".to_string(),
            ));
        }
        let process_key = tool_process_generation_key(
            expected_effective_declaration_hash,
            formation,
            expected_private_read_snapshot_hash,
            expected_effective_environment_hash,
            expected_invocation_authority_hash,
            expected_execution_inputs_hash,
        )
        .map_err(ToolProcessInvokeError::Unavailable)?;
        let process_root =
            tool_process_generation_root(ctx).map_err(ToolProcessInvokeError::Unavailable)?;
        let stderr_root = materialize_owner_private_descendant(
            &tool_process_host_private_root(ctx).map_err(ToolProcessInvokeError::Unavailable)?,
            &["stderr".to_string()],
        )
        .map_err(ToolProcessInvokeError::Unavailable)?;
        let stderr_path = stderr_root.join(format!("{process_key}.log"));
        match std::fs::remove_file(&stderr_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ToolProcessInvokeError::Unavailable(format!(
                    "tool process private stderr '{}' cannot be reset: {error}",
                    stderr_path.display()
                )));
            }
        }
        let private_read_max_bytes = private_read_max_bytes(
            &declaration.private_read_subpaths,
            private_read_max_bytes_ceiling,
        )
        .map_err(ToolProcessInvokeError::Unavailable)?;
        let materialized = materialize_private_read_generation(
            &ctx.interface_var_dir(),
            &process_root,
            &process_key,
            &declaration.private_read_subpaths,
            private_read_max_bytes,
            expected_private_read_snapshot_hash,
        )
        .map_err(ToolProcessInvokeError::Unavailable)?;
        let process_var = materialized.state_dir;
        let generation_dir = process_var
            .parent()
            .ok_or_else(|| {
                ToolProcessInvokeError::Unavailable(
                    "tool process state directory has no generation parent".to_string(),
                )
            })?
            .to_path_buf();
        let owner_lease = prepare_tool_process_owner_lease(&generation_dir)
            .map_err(ToolProcessInvokeError::Unavailable)?;
        let durable_var = materialize_tool_process_durable_dir(ctx, formation)
            .map_err(ToolProcessInvokeError::Unavailable)?;
        let private_read_exact = materialized.exact_paths;
        let private_reads_json =
            serde_json::to_string(&materialized.private_reads).map_err(|error| {
                ToolProcessInvokeError::Unavailable(format!(
                    "tool process private read handles did not encode: {error}"
                ))
            })?;
        let mut package_files = execution.readable_files.clone();
        package_files.push(ctx.interface_registry_dir());
        let authority = declaration.authority.clone();
        let interface_local_ipc_root = tool_process_interface_local_ipc_root(ctx)
            .map_err(ToolProcessInvokeError::Unavailable)?;
        let mut private_read_write = vec![process_var.clone()];
        if let Some(path) = &durable_var {
            private_read_write.push(path.clone());
        }
        if let Some(path) = &interface_local_ipc_root {
            private_read_write.push(path.clone());
        }
        let mut process_readable = package_files.clone();
        process_readable.extend(private_read_write.iter().cloned());
        let additional = octopus_interfaces::additional_host_root_paths(
            &ctx.resolved_interface_descriptor().map_err(ToolProcessInvokeError::Unavailable)?, &ctx.root_slots,
        ).map_err(ToolProcessInvokeError::Unavailable)?;
        let (authority, authority_roots) = octopus_interfaces::expand_host_root_authority(&authority, &ctx.root_slots, &additional)
            .map_err(ToolProcessInvokeError::Unavailable)?;
        let resolved = match arguments {
            Some(arguments) => {
                authority
                    .preflight_invocation_paths(&authority_roots, arguments)
                    .map_err(|error| match error {
                        ProcessInvocationAuthorityError::Unavailable(error) => {
                            ToolProcessInvokeError::Unavailable(error)
                        }
                        ProcessInvocationAuthorityError::Refused(error) => {
                            ToolProcessInvokeError::Capability(error)
                        }
                    })?;
                authority.resolve_for_invocation(
                    &executable_base,
                    &authority_roots,
                    &environment,
                    arguments,
                    process_readable.clone(),
                    private_read_write.clone(),
                    std::iter::empty(),
                    private_read_exact.clone(),
                )
            }
            None if authority.has_argument_paths() => {
                return Err(ToolProcessInvokeError::Unavailable(
                    "tool process declares invocation authority without invocation arguments"
                        .to_string(),
                ));
            }
            None => authority.resolve(
                &executable_base,
                &authority_roots,
                &environment,
                process_readable,
                private_read_write,
                std::iter::empty(),
                private_read_exact,
            ),
        }
        .map_err(ToolProcessInvokeError::Unavailable)?;
        let mut exposed_root_slots = resolved.root_slots;
        exposed_root_slots.retain(|slot, _| ctx.root_slots.contains_key(slot));
        exposed_root_slots.insert(
            "interface_life_install_root".to_string(),
            ctx.installation_root()
                .map_err(ToolProcessInvokeError::Unavailable)?,
        );
        if let Some(path) = interface_local_ipc_root {
            exposed_root_slots.insert(INTERFACE_LOCAL_IPC_ROOT_SLOT.to_string(), path);
        }
        for slot in &declaration.coordinate_root_slots {
            let root = ctx.root_slots.get(slot).ok_or_else(|| {
                ToolProcessInvokeError::Unavailable(format!(
                    "tool process requests unbound coordinate root slot '{}'",
                    slot
                ))
            })?;
            if !root.is_absolute() {
                return Err(ToolProcessInvokeError::Unavailable(format!(
                    "tool process coordinate root slot '{}' is not absolute: {}",
                    slot,
                    root.display()
                )));
            }
            exposed_root_slots.insert(slot.clone(), root.clone());
        }
        let root_slots_json = serde_json::to_string(&exposed_root_slots).map_err(|error| {
            ToolProcessInvokeError::Unavailable(format!(
                "tool process root slots did not encode: {error}"
            ))
        })?;
        let interface_descriptor = ctx
            .resolved_interface_descriptor()
            .map_err(ToolProcessInvokeError::Unavailable)?;
        let host_io_limits = ctx
            .required_host_io_limits()
            .map_err(ToolProcessInvokeError::Unavailable)?;
        let interface_descriptor_json =
            serde_json::to_string(&interface_descriptor).map_err(|error| {
                ToolProcessInvokeError::Unavailable(format!(
                    "resolved interface descriptor failed to encode: {error}"
                ))
            })?;
        let host_io_limits_json = serde_json::to_string(&host_io_limits).map_err(|error| {
            ToolProcessInvokeError::Unavailable(format!(
                "tool process host I/O limits failed to encode: {error}"
            ))
        })?;
        let prepared = resolved
            .authority
            .prepare(
                &execution.program,
                &execution.arguments,
                &executable_base,
                environment.get("PATH").map(std::ffi::OsString::as_os_str),
                &generation_dir,
            )
            .map_err(ToolProcessInvokeError::Unavailable)?;
        if prepared.executable != execution.executable
            || execution_inputs::ExecutionInputCache::default()
                .resolve(ctx, declaration, worker_environment_kind, &environment)
                .map_err(ToolProcessInvokeError::Unavailable)?
                .hash
                != expected_execution_inputs_hash
        {
            return Err(ToolProcessInvokeError::Unavailable(
                "tool execution inputs changed after process authority formation".into(),
            ));
        }
        let executable = prepared.executable.clone();
        let sandbox_profile = prepared.sandbox_profile.clone();
        let mut command = tokio::process::Command::new(&prepared.program);
        #[cfg(unix)]
        command.process_group(0);
        configure_tool_process_owner_lease(&mut command, &owner_lease);
        command
            .env_clear()
            .envs(prepared.environment)
            .envs(environment)
            .args(prepared.args)
            .current_dir(&executable_base)
            .env("OCTOPUS_INTERFACE_ID", ctx.interface.as_str())
            .env("OCTOPUS_INTERFACE_DIR", ctx.interface_registry_dir())
            .env(RESOLVED_INTERFACE_DESCRIPTOR_ENV, interface_descriptor_json)
            .env(INTERFACE_HOST_IO_LIMITS_ENV, host_io_limits_json)
            .env("OCTOPUS_VAR_DIR", &process_var)
            .env("OCTOPUS_PRIVATE_READS_JSON", private_reads_json)
            .env("OCTOPUS_ROOT_SLOTS_JSON", root_slots_json)
            .env("TMPDIR", &process_var)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = &durable_var {
            command.env(TOOL_PROCESS_DURABLE_VAR_DIR_ENV, path);
        }
        let mut child = command.spawn().map_err(|error| {
            ToolProcessInvokeError::Unavailable(format!(
                "tool process '{}' failed to spawn: {error}",
                executable.display()
            ))
        })?;
        let process_id = child.id().ok_or_else(|| {
            let _ = child.start_kill();
            ToolProcessInvokeError::Unavailable(format!(
                "tool process '{}' spawned without a process id",
                executable.display()
            ))
        })?;
        if let Err(error) = publish_tool_process_owner(
            &generation_dir,
            process_id,
            formation.map(|formation| formation.worker_environment_id.as_str()),
        ) {
            let _ = child.start_kill();
            return Err(ToolProcessInvokeError::Unavailable(format!(
                "tool process '{}' owner proof could not be published: {error}",
                executable.display()
            )));
        }
        drop(owner_lease);
        let stderr = child.stderr.take().ok_or_else(|| {
            ToolProcessInvokeError::Unavailable("tool process stderr unavailable".to_string())
        })?;
        let interface = ctx.interface.clone();
        let maximum_bytes = host_io_limits.max_stderr_bytes;
        let stderr_capture_path = stderr_path.clone();
        let (stderr_failure_tx, stderr_failure) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            if let Err(error) = drain_bounded_stderr(
                stderr,
                maximum_bytes,
                interface,
                "tool_process",
                Some(stderr_capture_path),
            )
            .await
            {
                let _ = stderr_failure_tx.send(error);
            }
        });
        let stdin = child.stdin.take().ok_or_else(|| {
            ToolProcessInvokeError::Unavailable("tool process stdin unavailable".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ToolProcessInvokeError::Unavailable("tool process stdout unavailable".to_string())
        })?;
        let mut output_roots = vec![process_var.clone()];
        // Durable evidence belongs to this exact formation, just like its writable state.
        // Keep its stable paths valid across process generations for admitted-output replay.
        if let Some(path) = durable_var {
            output_roots.push(path);
        }
        let input_root = process_var.join("invocations");
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BoundedJsonLineReader::new(stdout, host_io_limits.max_control_frame_bytes)
                .map_err(ToolProcessInvokeError::Unavailable)?,
            stderr_failure,
            stderr_open: true,
            stderr_path,
            next_request_id: 1,
            input_root,
            output_roots,
            generation_root: process_root,
            generation_dir,
            host_io_limits,
            formation: formation.cloned(),
            worker_environment_kind: worker_environment_kind.cloned(),
            last_source_movement_seq: 0,
            _sandbox_profile: sandbox_profile,
        })
    }

    async fn invoke(
        &mut self,
        ctx: &InterfaceLiftContext,
        descriptor: ToolDescriptor,
        envelope: ToolProcessEnvelope,
        terminalize_on_failure: bool,
        interrupted_recovery: bool,
        output_callback: Option<&ToolDoorOutputCallback>,
    ) -> Result<ToolProcessCompletion, ToolProcessInvokeError> {
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| ToolProcessInvokeError::Unavailable(error.to_string()))?
        {
            return Err(ToolProcessInvokeError::Unavailable(format!(
                "tool process exited before invocation with {status}"
            )));
        }
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1).ok_or_else(|| {
            ToolProcessInvokeError::Unavailable("tool request id overflow".to_string())
        })?;
        let formation = envelope.worker_environment.clone();
        let output_source = envelope.output_source.clone();
        let source_movement_seq = envelope.source_movement_seq;
        let kind_changed = match (
            envelope.worker_environment_kind.as_ref(),
            self.worker_environment_kind.as_ref(),
        ) {
            (Some(observed), Some(expected)) => {
                canonical_hash(observed).map_err(ToolProcessInvokeError::Unavailable)?
                    != canonical_hash(expected).map_err(ToolProcessInvokeError::Unavailable)?
            }
            (None, None) => false,
            _ => true,
        };
        if formation != self.formation || kind_changed {
            return Err(ToolProcessInvokeError::Unavailable(
                "tool process invocation formation changed after process spawn".to_string(),
            ));
        }
        attest_tool_process_source_interface(&ctx.interface, envelope.metadata.as_ref())?;
        self.last_source_movement_seq = source_movement_seq;
        let mut lifecycle_recorded = false;
        let mut lifecycle_recording_error = None;
        let preparation_error = |error| if interrupted_recovery {
            ToolProcessInvokeError::Unavailable(error)
        } else {
            ToolProcessInvokeError::Capability(error)
        };
        let envelope = envelope
            .materialize_for_process(&self.input_root, request_id, &self.host_io_limits)
            .map_err(preparation_error)?;
        let primary_input = ToolProcessPrimaryInput::capture_invocation(
            &self.input_root, request_id, &envelope, &self.host_io_limits,
        ).map_err(preparation_error)?;
        let invocation_manifest = envelope.manifest_path.clone();
        let input = ToolProcessInput::Invoke {
            request_id,
            descriptor,
            envelope,
        };
        let frame = serialize_bounded_json_line(&input, self.host_io_limits.max_control_frame_bytes, "tool-process input frame")
            .map_err(preparation_error)?;
        self.write_input_bytes(&frame)
            .await
            .map_err(ToolProcessInvokeError::Unavailable)?;
        let mut streamed_output_admitted = false;
        let mut all_streamed_outputs_admitted = true;
        loop {
            let output = self
                .read_protocol_output("tool process closed stdout before completion")
                .await?;
            match output {
                ToolProcessOutput::ArtifactArrival {
                    request_id: output_request,
                    event_id,
                    manifest_path,
                } if output_request == request_id => {
                    let delivery = match self.capture_output_artifact(&manifest_path) {
                        Ok(artifact) => {
                            hand_tool_process_output(
                                ctx,
                                output_source.as_ref(),
                                output_callback,
                                artifact,
                            )
                            .await
                        }
                        Err(error) => Err(tool_process_error_message(error)),
                    };
                    let admitted = matches!(&delivery, Ok(DeliveryVerdict::Admitted { .. }));
                    all_streamed_outputs_admitted &= admitted;
                    self.acknowledge_delivery(request_id, event_id, delivery)
                        .await?;
                    streamed_output_admitted |= admitted;
                }
                ToolProcessOutput::SystemEmission {
                    request_id: output_request,
                    event_id,
                    artifact,
                    to_interface,
                } if output_request == request_id => {
                    let artifact = match artifact {
                        ToolProcessEmissionArtifact::Materialized { manifest_path } => self
                            .capture_output_artifact(&manifest_path)
                            .map(SystemInterfaceEmissionArtifact::Materialized)
                            .map_err(tool_process_error_message),
                        ToolProcessEmissionArtifact::Reference { artifact } => {
                            Ok(SystemInterfaceEmissionArtifact::Reference(artifact))
                        }
                    };
                    let delivery = match artifact {
                        Ok(artifact) => {
                            ctx.hand_system_emission(
                                artifact,
                                source_movement_seq,
                                InterfaceId::new(to_interface),
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    };
                    all_streamed_outputs_admitted &= matches!(&delivery, Ok(DeliveryVerdict::Admitted { .. }));
                    self.acknowledge_delivery(request_id, event_id, delivery)
                        .await?;
                }
                ToolProcessOutput::WorkerEnvironmentLifecycle {
                    request_id: output_request,
                    event_id,
                    worker_environment_id,
                    lifecycle,
                } if output_request == request_id => {
                    let recorded = match formation.as_ref() {
                        Some(formation)
                            if formation.worker_environment_id.as_str()
                                == worker_environment_id =>
                        {
                            record_worker_environment_lifecycle(
                                ctx,
                                formation,
                                source_movement_seq,
                                lifecycle,
                            )
                        }
                        Some(formation) => Err(format!(
                            "tool process reported lifecycle for worker environment '{}' while invocation was formed for '{}'",
                            worker_environment_id, formation.worker_environment_id
                        )),
                        None => Err(format!(
                            "tool process reported worker-environment lifecycle '{}' for an unformed invocation",
                            worker_environment_id
                        )),
                    };
                    let error = recorded.err();
                    if error.is_none() {
                        lifecycle_recorded = true;
                    } else {
                        lifecycle_recording_error = error.clone();
                    }
                    self.write_input(&ToolProcessInput::WorkerEnvironmentLifecycleRecorded {
                        request_id,
                        event_id,
                        error,
                    })
                    .await
                    .map_err(ToolProcessInvokeError::Unavailable)?;
                }
                ToolProcessOutput::WorkerEnvironmentLiveState {
                    request_id: output_request,
                    event_id,
                    worker_environment_id,
                    state,
                } if output_request == request_id => {
                    self.acknowledge_worker_environment_live_state(
                        ctx,
                        formation.as_ref(),
                        output_request,
                        event_id,
                        worker_environment_id,
                        state,
                    )
                    .await?;
                }
                ToolProcessOutput::WorkerEnvironmentLiveState {
                    request_id: output_request,
                    event_id,
                    worker_environment_id,
                    state,
                } if output_request < request_id => {
                    self.acknowledge_worker_environment_live_state(
                        ctx,
                        formation.as_ref(),
                        output_request,
                        event_id,
                        worker_environment_id,
                        state,
                    )
                    .await?;
                }
                ToolProcessOutput::Complete {
                    request_id: output_request,
                    artifact_manifest_paths,
                } if output_request == request_id => {
                    if let Some(error) = lifecycle_recording_error {
                        return Err(ToolProcessInvokeError::Unavailable(error));
                    }
                    if terminalize_on_failure && formation.is_some() && !lifecycle_recorded {
                        if let Some(formation) = formation.as_ref() {
                            let _ = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir())
                                .fail(formation, source_movement_seq);
                        }
                        return Err(ToolProcessInvokeError::Unavailable(
                            "formed tool process completed without recording real substrate lifecycle"
                                .to_string(),
                        ));
                    }
                    let artifacts = artifact_manifest_paths
                        .into_iter()
                        .map(|path| self.capture_output_artifact(&path))
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(ToolProcessCompletion {
                        artifacts,
                        streamed_output_admitted,
                        primary_input: all_streamed_outputs_admitted.then_some(primary_input),
                        invocation_manifest,
                        generation: self.generation_dir.clone(),
                    });
                }
                ToolProcessOutput::Failed {
                    request_id: output_request,
                    error,
                } if output_request == request_id => {
                    if terminalize_on_failure && !lifecycle_recorded {
                        if let Some(formation) = formation.as_ref() {
                            let _ = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir())
                                .fail(formation, source_movement_seq);
                        }
                    }
                    if let Some(lifecycle_error) = lifecycle_recording_error {
                        return Err(ToolProcessInvokeError::Unavailable(format!(
                            "{error}; worker-environment lifecycle could not be recorded: {lifecycle_error}"
                        )));
                    }
                    return Err(ToolProcessInvokeError::Capability(error));
                }
                ToolProcessOutput::Unavailable {
                    request_id: output_request,
                    error,
                } if output_request == request_id => {
                    if terminalize_on_failure && !lifecycle_recorded {
                        if let Some(formation) = formation.as_ref() {
                            let _ = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir())
                                .fail(formation, source_movement_seq);
                        }
                    }
                    if let Some(lifecycle_error) = lifecycle_recording_error {
                        return Err(ToolProcessInvokeError::Unavailable(format!(
                            "{error}; worker-environment lifecycle could not be recorded: {lifecycle_error}"
                        )));
                    }
                    return Err(ToolProcessInvokeError::Unavailable(error));
                }
                _ => {
                    return Err(ToolProcessInvokeError::Unavailable(format!(
                        "tool process returned an event for another request while waiting for {request_id}"
                    )));
                }
            }
        }
    }

    async fn acknowledge_worker_environment_live_state(
        &mut self,
        ctx: &InterfaceLiftContext,
        formation: Option<&WorkerEnvironmentFormation>,
        request_id: u64,
        event_id: u64,
        worker_environment_id: String,
        state: serde_json::Value,
    ) -> Result<(), ToolProcessInvokeError> {
        let recorded = match formation {
            Some(formation)
                if formation.worker_environment_id.as_str() == worker_environment_id =>
            {
                record_worker_environment_live_state(ctx, formation, &state)
            }
            Some(formation) => Err(format!(
                "tool process reported live state for worker environment '{}' while invocation was formed for '{}'",
                worker_environment_id, formation.worker_environment_id
            )),
            None => Err(format!(
                "tool process reported worker-environment live state '{}' for an unformed invocation",
                worker_environment_id
            )),
        };
        self.write_input(&ToolProcessInput::WorkerEnvironmentLiveStateRecorded {
            request_id,
            event_id,
            error: recorded.err(),
        })
        .await
        .map_err(ToolProcessInvokeError::Unavailable)
    }

    async fn acknowledge_delivery(
        &mut self,
        request_id: u64,
        event_id: u64,
        delivery: Result<DeliveryVerdict, String>,
    ) -> Result<(), ToolProcessInvokeError> {
        let input = match delivery {
            Ok(verdict) => ToolProcessInput::DeliveryVerdict {
                request_id,
                event_id,
                verdict,
            },
            Err(error) => ToolProcessInput::DeliveryFailure {
                request_id,
                event_id,
                error,
            },
        };
        self.write_input(&input)
            .await
            .map_err(ToolProcessInvokeError::Unavailable)
    }

    async fn release(
        &mut self,
        ctx: &InterfaceLiftContext,
        mode: WorkerEnvironmentReleaseMode,
    ) -> Result<(), ToolProcessInvokeError> {
        let (formation, worker_environment_kind) =
            match (self.formation.clone(), self.worker_environment_kind.clone()) {
                (Some(formation), Some(kind)) => (formation, kind),
                (None, None) => return Ok(()),
                _ => {
                    return Err(ToolProcessInvokeError::Unavailable(
                        "tool process holds an incomplete worker-environment formation".to_string(),
                    ));
                }
            };
        if let Some(status) = self
            .child
            .try_wait()
            .map_err(|error| ToolProcessInvokeError::Unavailable(error.to_string()))?
        {
            return Err(ToolProcessInvokeError::Unavailable(format!(
                "tool process exited before worker-environment release with {status}"
            )));
        }
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.checked_add(1).ok_or_else(|| {
            ToolProcessInvokeError::Unavailable("tool request id overflow".to_string())
        })?;
        self.write_input(&ToolProcessInput::ReleaseWorkerEnvironment {
            request_id,
            formation: formation.clone(),
            worker_environment_kind,
            mode,
        })
        .await
        .map_err(ToolProcessInvokeError::Unavailable)?;
        loop {
            let output = self
                .read_protocol_output(
                    "tool process closed stdout before worker-environment release completed",
                )
                .await?;
            match output {
                ToolProcessOutput::WorkerEnvironmentLiveState {
                    request_id: output_request,
                    event_id,
                    worker_environment_id,
                    state,
                } if output_request < request_id => {
                    self.acknowledge_worker_environment_live_state(
                        ctx,
                        Some(&formation),
                        output_request,
                        event_id,
                        worker_environment_id,
                        state,
                    )
                    .await?;
                }
                ToolProcessOutput::WorkerEnvironmentReleased {
                    request_id: output_request,
                } if output_request == request_id => return Ok(()),
                ToolProcessOutput::Failed {
                    request_id: output_request,
                    error,
                } if output_request == request_id => {
                    return Err(ToolProcessInvokeError::Capability(error));
                }
                ToolProcessOutput::Unavailable {
                    request_id: output_request,
                    error,
                } if output_request == request_id => {
                    return Err(ToolProcessInvokeError::Unavailable(error));
                }
                _ => {
                    return Err(ToolProcessInvokeError::Unavailable(format!(
                        "tool process returned an event for another request while releasing {request_id}"
                    )));
                }
            }
        }
    }

    async fn read_protocol_output(
        &mut self,
        closed_message: &str,
    ) -> Result<ToolProcessOutput, ToolProcessInvokeError> {
        let line_result = loop {
            if self.stderr_open {
                tokio::select! {
                    line = self.stdout.next_line() => break line,
                    stderr = &mut self.stderr_failure => {
                        match stderr {
                            Ok(error) => {
                                return Err(self.terminate_for_protocol_failure(error).await);
                            }
                            Err(_) => self.stderr_open = false,
                        }
                    }
                }
            } else {
                break self.stdout.next_line().await;
            }
        };
        let line = match line_result {
            Ok(Some(line)) => line,
            Ok(None) => {
                return Err(ToolProcessInvokeError::Unavailable(
                    closed_message.to_string(),
                ));
            }
            Err(error) => {
                return Err(self
                    .terminate_for_protocol_failure(format!(
                        "tool process protocol output was refused: {error}"
                    ))
                    .await);
            }
        };
        match serde_json::from_str(&line) {
            Ok(output) => Ok(output),
            Err(error) => Err(self
                .terminate_for_protocol_failure(format!(
                    "tool process output is not protocol JSON: {error}"
                ))
                .await),
        }
    }

    async fn terminate_for_protocol_failure(&mut self, message: String) -> ToolProcessInvokeError {
        let termination = match self.child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => self
                .child
                .kill()
                .await
                .map_err(|error| format!("tool process termination failed: {error}")),
            Err(error) => Err(format!("tool process status failed: {error}")),
        };
        match termination {
            Ok(()) => ToolProcessInvokeError::Unavailable(message),
            Err(error) => ToolProcessInvokeError::Unavailable(format!("{message}; {error}")),
        }
    }

    async fn terminate_and_remove_generation(&mut self) -> Result<(), String> {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => {
                drop(self.stdin.take());
                self.child
                    .wait()
                    .await
                    .map_err(|error| format!("tool process exit wait failed: {error}"))?
            }
            Err(error) => return Err(format!("tool process status failed: {error}")),
        };
        if !status.success() {
            return Err(format!(
                "tool process exited unsuccessfully during protocol shutdown: {status}"
            ));
        }
        remove_tool_process_generation(&self.generation_root, &self.generation_dir)
    }

    async fn write_input(&mut self, input: &ToolProcessInput) -> Result<(), String> {
        let bytes = serialize_bounded_json_line(
            input,
            self.host_io_limits.max_control_frame_bytes,
            "tool-process input frame",
        )?;
        self.write_input_bytes(&bytes).await
    }

    async fn write_input_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "tool process stdin is closed".to_string())?;
        if let Err(error) = stdin.write_all(bytes).await {
            return Err(self
                .process_input_error("tool process stdin write failed", error)
                .await);
        }
        if let Err(error) = stdin.flush().await {
            return Err(self
                .process_input_error("tool process stdin flush failed", error)
                .await);
        }
        Ok(())
    }

    async fn process_input_error(&mut self, message: &str, error: std::io::Error) -> String {
        let mut detail = format!("{message}: {error}");
        match self.child.try_wait() {
            Ok(Some(status)) => {
                detail.push_str(&format!("; tool process exited with {status}"));
                for _ in 0..10 {
                    if self.stderr_path.is_file() {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
            Ok(None) => {}
            Err(status_error) => {
                detail.push_str(&format!(
                    "; tool process exit status unavailable: {status_error}"
                ));
            }
        }
        if self.stderr_path.is_file() {
            detail.push_str(&format!(
                "; private stderr captured at '{}'",
                self.stderr_path.display()
            ));
        }
        detail
    }

    fn capture_output_artifact(
        &self,
        path: &Path,
    ) -> Result<InterfaceArtifactFile, ToolProcessInvokeError> {
        capture_spawned_artifact(path, &self.output_roots, self.host_io_limits)
            .map_err(ToolProcessInvokeError::Capability)
    }
}

fn record_worker_environment_lifecycle(
    ctx: &InterfaceLiftContext,
    formation: &WorkerEnvironmentFormation,
    source_movement_seq: u64,
    lifecycle: WorkerEnvironmentLifecycle,
) -> Result<(), String> {
    let lock =
        worker_environment_record_lock(&ctx.interface_var_dir(), &formation.worker_environment_id)?;
    let _guard = lock
        .lock()
        .map_err(|_| "worker-environment record lock is poisoned".to_string())?;
    let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
    match lifecycle {
        WorkerEnvironmentLifecycle::Active => {
            records.activate(formation, source_movement_seq).map(|_| ())
        }
        WorkerEnvironmentLifecycle::Released => {
            records.release(formation, source_movement_seq).map(|_| ())
        }
        WorkerEnvironmentLifecycle::Failed => {
            records.fail(formation, source_movement_seq).map(|_| ())
        }
    }
    .map_err(|error| {
        format!(
            "worker environment '{}' could not record {:?}: {error}",
            formation.worker_environment_id, lifecycle
        )
    })
}

fn refuse_released_worker_environment(
    ctx: &InterfaceLiftContext,
    formation: &WorkerEnvironmentFormation,
) -> Result<(), String> {
    let lock =
        worker_environment_record_lock(&ctx.interface_var_dir(), &formation.worker_environment_id)?;
    let _guard = lock
        .lock()
        .map_err(|_| "worker-environment record lock is poisoned".to_string())?;
    let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
    let path = records.record_path(&formation.worker_environment_id);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "worker environment '{}' record cannot be inspected before invocation: {error}",
            formation.worker_environment_id
        )),
        Ok(_) => {
            let record = records
                .read(&formation.worker_environment_id)
                .map_err(|error| {
                    format!(
                        "worker environment '{}' record cannot be read before invocation: {error}",
                        formation.worker_environment_id
                    )
                })?;
            if record.formation != *formation {
                return Err(format!(
                    "worker environment '{}' record formation changed before invocation",
                    formation.worker_environment_id
                ));
            }
            if record.lifecycle == WorkerEnvironmentLifecycle::Released
                && formation.requires_release
            {
                return Err(format!(
                    "worker environment '{}' was released and cannot execute another tool",
                    formation.worker_environment_id
                ));
            }
            Ok(())
        }
    }
}

fn record_worker_environment_live_state(
    ctx: &InterfaceLiftContext,
    formation: &WorkerEnvironmentFormation,
    state: &serde_json::Value,
) -> Result<(), String> {
    let lock =
        worker_environment_record_lock(&ctx.interface_var_dir(), &formation.worker_environment_id)?;
    let _guard = lock
        .lock()
        .map_err(|_| "worker-environment record lock is poisoned".to_string())?;
    WorkerEnvironmentRecordStore::new(ctx.interface_var_dir())
        .write_live_state(&formation.worker_environment_id, state)
        .map(|_| ())
        .map_err(|error| {
            format!(
                "worker environment '{}' could not record live state: {error}",
                formation.worker_environment_id
            )
        })
}

type WorkerEnvironmentRecordLockKey = (PathBuf, WorkerEnvironmentId);
type WorkerEnvironmentRecordLockMap = std::sync::Mutex<
    BTreeMap<WorkerEnvironmentRecordLockKey, std::sync::Weak<std::sync::Mutex<()>>>,
>;

fn worker_environment_record_lock(
    root: &Path,
    id: &WorkerEnvironmentId,
) -> Result<Arc<std::sync::Mutex<()>>, String> {
    static LOCKS: std::sync::OnceLock<WorkerEnvironmentRecordLockMap> = std::sync::OnceLock::new();
    let locks = LOCKS.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()));
    let mut locks = locks
        .lock()
        .map_err(|_| "worker-environment record lock registry is poisoned".to_string())?;
    locks.retain(|_, lock| lock.strong_count() > 0);
    let key = (root.to_path_buf(), id.clone());
    if let Some(lock) = locks.get(&key).and_then(std::sync::Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(std::sync::Mutex::new(()));
    locks.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}

pub(crate) struct ToolProcessBrokerHost {
    owner: ToolDoorOwner,
    output_callback: Option<ToolDoorOutputCallback>,
    routing: tokio::sync::Mutex<ToolProcessBrokerState>,
    shards: tokio::sync::Mutex<ToolProcessBrokerShards>,
    release_gates:
        std::sync::Mutex<BTreeMap<WorkerEnvironmentId, std::sync::Weak<tokio::sync::RwLock<()>>>>,
}

#[derive(Default)]
struct ToolProcessBrokerShards {
    by_key: BTreeMap<ToolProcessKey, Arc<tokio::sync::Mutex<ToolProcessBrokerState>>>,
    by_tool: BTreeMap<String, BTreeSet<ToolProcessKey>>,
}

impl ToolProcessBrokerShards {
    fn route(
        &mut self,
        tool: String,
        process_key: ToolProcessKey,
    ) -> (
        Arc<tokio::sync::Mutex<ToolProcessBrokerState>>,
        Vec<(Arc<tokio::sync::Mutex<ToolProcessBrokerState>>, bool)>,
        bool,
    ) {
        let mut stale_keys = BTreeSet::new();
        let prior_for_tool = self.by_tool.entry(tool).or_default();
        let replaced = prior_for_tool
            .iter()
            .filter(|key| {
                key.formation_scope == process_key.formation_scope && **key != process_key
            })
            .cloned()
            .collect::<Vec<_>>();
        for key in replaced {
            prior_for_tool.remove(&key);
            stale_keys.insert(key);
        }
        prior_for_tool.insert(process_key.clone());
        if let Some(scope) = process_key.formation_scope.as_deref() {
            stale_keys.extend(
                self.by_key
                    .keys()
                    .filter(|key| {
                        key.formation_scope.as_deref() == Some(scope)
                            && key.formation_id != process_key.formation_id
                    })
                    .cloned(),
            );
        }
        for key in &stale_keys {
            for keys in self.by_tool.values_mut() {
                keys.remove(key);
            }
        }
        self.by_tool.retain(|_, keys| !keys.is_empty());
        let referenced = self
            .by_tool
            .values()
            .flat_map(|keys| keys.iter().cloned())
            .collect::<BTreeSet<_>>();
        let stale = stale_keys
            .into_iter()
            .filter(|key| !referenced.contains(key))
            .filter_map(|key| self.by_key.remove(&key).map(|state| (key, state)))
            .collect::<Vec<_>>();
        let shard = Arc::clone(self.by_key.entry(process_key.clone()).or_insert_with(|| {
            Arc::new(tokio::sync::Mutex::new(ToolProcessBrokerState::default()))
        }));
        let remaining_formations = self
            .by_key
            .keys()
            .filter_map(|key| key.formation_id.clone())
            .collect::<BTreeSet<_>>();
        let mut terminal_assigned = BTreeSet::new();
        let mut stale = stale
            .into_iter()
            .map(|(key, state)| {
                let terminal = key.formation_id.as_ref().is_some_and(|id| {
                    !remaining_formations.contains(id) && terminal_assigned.insert(id.clone())
                });
                (state, terminal)
            })
            .collect::<Vec<_>>();
        stale.sort_by_key(|(_, terminal)| *terminal);
        let sole_formation_key = process_key.formation_id.as_ref().is_some_and(|id| {
            self.by_key
                .keys()
                .filter(|key| key.formation_id.as_ref() == Some(id))
                .count()
                == 1
        });
        (shard, stale, sole_formation_key)
    }
}

#[derive(Default)]
struct ToolProcessBrokerState {
    processes: BTreeMap<ToolProcessKey, ToolProcessClient>,
    declarations_by_tool: BTreeMap<String, (ToolProcessDeclaration, ToolProcessKey)>,
    argument_validators: BTreeMap<String, ToolArgumentValidator>,
    tool_descriptors: BTreeMap<String, CachedToolDescriptor>,
    tool_owners: BTreeMap<String, CachedToolOwner>,
    worker_environment_registry: Option<CachedWorkerEnvironmentRegistry>,
    execution_inputs: execution_inputs::ExecutionInputCache,
}

impl ToolProcessBrokerHost {
    fn new(owner: ToolDoorOwner) -> Result<Self, String> {
        owner.validate()?;
        Ok(Self {
            owner,
            output_callback: None,
            routing: tokio::sync::Mutex::new(ToolProcessBrokerState::default()),
            shards: tokio::sync::Mutex::new(ToolProcessBrokerShards::default()),
            release_gates: std::sync::Mutex::new(BTreeMap::new()),
        })
    }
}

#[cfg(test)]
impl Default for ToolProcessBrokerHost {
    fn default() -> Self {
        Self::new(ToolDoorOwner {
            worker_environment_kind: WorkerEnvironmentKindId::new("test.room"),
            door_interface: InterfaceId::new("system_test_room"),
            max_in_flight: 4,
            private_read_max_bytes: 4096,
            root_slot_transform: None,
        })
        .expect("test tool-door owner")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct RegistryFileFingerprint {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity_and_change: (u64, u64, i64, i64),
}

#[derive(Clone)]
struct CachedToolDescriptor {
    fingerprint: RegistryFileFingerprint,
    descriptor: ToolDescriptor,
    canonical_hash: String,
}

#[derive(Clone)]
struct CachedToolOwner {
    fingerprint: RegistryFileFingerprint,
    interface: InterfaceId,
}

struct CachedWorkerEnvironmentRegistry {
    fingerprint: RegistryFileFingerprint,
    kind: WorkerEnvironmentKindId,
    registry: WorkerEnvKindRegistry,
}

struct WorkerEnvironmentReleaseContract {
    formation: WorkerEnvironmentFormation,
    kind: WorkerEnvKindSpec,
    process: ToolProcessDeclaration,
}

impl ToolProcessBrokerHost {
    pub(crate) async fn invoke(
        &self,
        ctx: &InterfaceLiftContext,
        tool: String,
        arguments: serde_json::Value,
        envelope: ToolProcessEnvelope,
        interrupted_recovery: bool,
    ) -> ToolProcessBrokerInvocation {
        if ctx.interface != self.owner.door_interface {
            return ToolProcessBrokerInvocation::Outcome(
                ToolProcessBrokerResult::CapabilityFailed {
                    error: format!(
                        "tool-door owns exact interface '{}', not '{}'",
                        self.owner.door_interface, ctx.interface
                    ),
                },
            );
        }
        let effective_ctx = match self.owner.effective_context(ctx) {
            Ok(effective) => effective,
            Err(error) => {
                return ToolProcessBrokerInvocation::Outcome(
                    ToolProcessBrokerResult::CapabilityFailed { error },
                );
            }
        };
        let ctx = effective_ctx.as_ref().unwrap_or(ctx);
        match completed_output::CompletedOutputs::load(ctx, envelope.source_movement_seq) {
            Ok(Some(mut retained)) => {
                return match retained.deliver(ctx, &envelope, self.output_callback.as_ref(), true).await {
                    Ok(ToolProcessBrokerResult::Completed) => ToolProcessBrokerInvocation::Completed(None),
                    Ok(result) => ToolProcessBrokerInvocation::Outcome(result),
                    Err(error) => ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::OutputPending { error }),
                };
            }
            Ok(None) => {}
            Err(error) => return ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::OutputPending { error }),
        }
        let initial_process_key = {
            let mut routing = self.routing.lock().await;
            match routing.resolve_process_key(ctx, &tool, &arguments, &envelope, &self.owner) {
                Ok(key) => key,
                Err(result) => return ToolProcessBrokerInvocation::Outcome(result),
            }
        };
        let (process_key, _release_guard) = if let Some(id) =
            initial_process_key.formation_id.as_ref()
        {
            let gate = match self.release_gate(id) {
                Ok(gate) => gate,
                Err(error) => {
                    return ToolProcessBrokerInvocation::Outcome(
                        ToolProcessBrokerResult::Unavailable { error },
                    );
                }
            };
            let guard = gate.read_owned().await;
            let current = {
                let mut routing = self.routing.lock().await;
                match routing.resolve_process_key(ctx, &tool, &arguments, &envelope, &self.owner) {
                    Ok(key) => key,
                    Err(result) => return ToolProcessBrokerInvocation::Outcome(result),
                }
            };
            if current.formation_id.as_ref() != Some(id) {
                return ToolProcessBrokerInvocation::Outcome(
                    ToolProcessBrokerResult::Unavailable {
                        error: format!(
                            "worker environment '{}' changed while acquiring its lifecycle gate",
                            id.as_str()
                        ),
                    },
                );
            }
            (current, Some(guard))
        } else {
            (initial_process_key, None)
        };
        if interrupted_recovery
            && process_key.declaration.interrupted_effect_recovery
                != ToolInterruptedEffectRecovery::ReinvokeExactArtifact
        {
            return ToolProcessBrokerInvocation::InterruptedEffectUnknown;
        }
        let (shard, stale, terminalize_on_failure) = {
            let mut shards = self.shards.lock().await;
            shards.route(tool.clone(), process_key.clone())
        };
        for (stale, terminalize_formation) in stale {
            let mut state = stale.lock().await;
            if let Err(error) = state.shutdown(ctx, terminalize_formation).await {
                return ToolProcessBrokerInvocation::Outcome(
                    ToolProcessBrokerResult::Unavailable {
                        error: format!(
                            "stale tool-process formation could not be released: {error}"
                        ),
                    },
                );
            }
        }
        let active_generations = {
            let shards = self.shards.lock().await;
            shards
                .by_key
                .keys()
                .map(tool_process_generation_key_from_process_key)
                .collect::<Result<BTreeSet<_>, _>>()
        };
        let active_generations = match active_generations {
            Ok(active) => active,
            Err(error) => {
                return ToolProcessBrokerInvocation::Outcome(
                    ToolProcessBrokerResult::Unavailable { error },
                );
            }
        };
        let generation_root = match tool_process_generation_root(ctx) {
            Ok(root) => root,
            Err(error) => {
                return ToolProcessBrokerInvocation::Outcome(
                    ToolProcessBrokerResult::Unavailable { error },
                );
            }
        };
        if let Err(error) =
            quiesce_orphaned_generation_owners(&generation_root, &active_generations).await
        {
            return ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Unavailable {
                error,
            });
        }
        if let Err(error) =
            reconcile_tool_process_generations(&generation_root, &active_generations)
        {
            return ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Unavailable {
                error,
            });
        }
        let mut primary_input = None;
        let result = shard.lock().await.invoke(
            ctx, tool, arguments, envelope, process_key, terminalize_on_failure, interrupted_recovery,
            &self.owner, self.output_callback.as_ref(), &mut primary_input,
        ).await;
        match result {
            ToolProcessBrokerResult::Completed => {
                ToolProcessBrokerInvocation::Completed(primary_input)
            }
            result => ToolProcessBrokerInvocation::Outcome(result),
        }
    }

    pub(crate) async fn release_worker_environment(
        &self,
        ctx: &InterfaceLiftContext,
        worker_environment_id: &WorkerEnvironmentId,
        envelope: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
    ) -> Result<WorkerEnvironmentFormation, String> {
        if ctx.interface != self.owner.door_interface {
            return Err(format!(
                "tool-door owns exact interface '{}', not '{}'",
                self.owner.door_interface, ctx.interface
            ));
        }
        let effective_ctx = self.owner.effective_context(ctx)?;
        let ctx = effective_ctx.as_ref().unwrap_or(ctx);
        let gate = self.release_gate(worker_environment_id)?;
        let _guard = gate.write().await;
        self.release_worker_environment_inner(
            ctx,
            worker_environment_id,
            envelope,
            source_movement_seq,
        )
        .await
    }

    fn release_gate(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
    ) -> Result<Arc<tokio::sync::RwLock<()>>, String> {
        let mut gates = self
            .release_gates
            .lock()
            .map_err(|_| "worker-environment release gate registry is poisoned".to_string())?;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates
            .get(worker_environment_id)
            .and_then(std::sync::Weak::upgrade)
        {
            return Ok(gate);
        }
        let gate = Arc::new(tokio::sync::RwLock::new(()));
        gates.insert(worker_environment_id.clone(), Arc::downgrade(&gate));
        Ok(gate)
    }

    async fn release_worker_environment_inner(
        &self,
        ctx: &InterfaceLiftContext,
        worker_environment_id: &WorkerEnvironmentId,
        envelope: &InterfaceInvocationEnvelope,
        source_movement_seq: u64,
    ) -> Result<WorkerEnvironmentFormation, String> {
        let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
        let record = read_retained_worker_environment_record(&records, worker_environment_id)?;
        validate_worker_environment_release_runtime(envelope, &record.formation, &self.owner)?;
        let contract = self
            .routing
            .lock()
            .await
            .load_worker_environment_release_contract(ctx, record.formation.clone(), &self.owner)?;
        if record.lifecycle == WorkerEnvironmentLifecycle::Released {
            let generation_root = tool_process_generation_root(ctx)?;
            quiesce_worker_environment_owner_generations(
                &generation_root,
                &contract.formation.worker_environment_id,
            )
            .await?;
            let active = {
                let shards = self.shards.lock().await;
                shards
                    .by_key
                    .keys()
                    .map(tool_process_generation_key_from_process_key)
                    .collect::<Result<BTreeSet<_>, _>>()?
            };
            reconcile_tool_process_generations(&generation_root, &active)?;
            return Ok(contract.formation);
        }

        let mut processes = self
            .take_worker_environment_processes(worker_environment_id)
            .await;
        let release_mode = if processes.is_empty() {
            quiesce_worker_environment_owner_generations(
                &tool_process_generation_root(ctx)?,
                &contract.formation.worker_environment_id,
            )
            .await?;
            processes.push(spawn_worker_environment_release_process(
                ctx,
                &contract,
                self.owner.private_read_max_bytes,
            )?);
            WorkerEnvironmentReleaseMode::ColdRecovery
        } else {
            WorkerEnvironmentReleaseMode::LiveOwner
        };
        let mut release_errors = Vec::new();
        let mut cleanup_errors = Vec::new();
        let terminal_process = processes.pop().ok_or_else(|| {
            format!(
                "worker environment '{}' has no process capable of release",
                contract.formation.worker_environment_id
            )
        })?;
        for mut process in processes {
            if let Err(error) = process.terminate_and_remove_generation().await {
                cleanup_errors.push(error);
            }
        }
        let mut terminal_process = terminal_process;
        if cleanup_errors.is_empty() {
            if let Err(error) = terminal_process.release(ctx, release_mode).await {
                release_errors.push(tool_process_error_message(error));
            }
        } else {
            release_errors.push(format!(
                "worker environment '{}' kept its substrate because another tool process did not quiesce",
                contract.formation.worker_environment_id
            ));
        }
        if let Err(error) = terminal_process.terminate_and_remove_generation().await {
            cleanup_errors.push(error);
        }
        if let Err(error) = quiesce_worker_environment_owner_generations(
            &tool_process_generation_root(ctx)?,
            &contract.formation.worker_environment_id,
        )
        .await
        {
            cleanup_errors.push(error);
        }

        let release = if release_errors.is_empty() && cleanup_errors.is_empty() {
            let lock = worker_environment_record_lock(
                &ctx.interface_var_dir(),
                &contract.formation.worker_environment_id,
            )?;
            let _guard = lock
                .lock()
                .map_err(|_| "worker-environment record lock is poisoned".to_string())?;
            records
                    .release(&contract.formation, source_movement_seq)
                    .map_err(|error| {
                        format!(
                            "worker environment '{}' released its substrate but could not record release: {error}",
                            contract.formation.worker_environment_id
                        )
                    })
                    .map(|_| ())
        } else {
            let release_error = release_errors.join("; ");
            let lock = worker_environment_record_lock(
                &ctx.interface_var_dir(),
                &contract.formation.worker_environment_id,
            )?;
            let _guard = lock
                .lock()
                .map_err(|_| "worker-environment record lock is poisoned".to_string())?;
            match records.fail(&contract.formation, source_movement_seq) {
                Ok(_) => Err(format!(
                    "worker environment '{}' could not release its substrate: {release_error}",
                    contract.formation.worker_environment_id
                )),
                Err(record_error) => Err(format!(
                    "worker environment '{}' could not release its substrate: {release_error}; failure could not be recorded: {record_error}",
                    contract.formation.worker_environment_id
                )),
            }
        };

        let active_generations = {
            let shards = self.shards.lock().await;
            shards
                .by_key
                .keys()
                .map(tool_process_generation_key_from_process_key)
                .collect::<Result<BTreeSet<_>, _>>()
        };
        let reconcile = active_generations.and_then(|active| {
            let root = tool_process_generation_root(ctx)?;
            reconcile_tool_process_generations(&root, &active)
        });
        let mut errors = release.err().into_iter().collect::<Vec<_>>();
        errors.append(&mut cleanup_errors);
        errors.extend(reconcile.err());
        if errors.is_empty() {
            Ok(contract.formation)
        } else {
            Err(errors.join("; "))
        }
    }

    async fn take_worker_environment_processes(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
    ) -> Vec<ToolProcessClient> {
        let states = {
            let mut shards = self.shards.lock().await;
            let keys = shards
                .by_key
                .keys()
                .filter(|key| key.formation_id.as_ref() == Some(worker_environment_id))
                .cloned()
                .collect::<Vec<_>>();
            for key in &keys {
                for tool_keys in shards.by_tool.values_mut() {
                    tool_keys.remove(key);
                }
            }
            shards.by_tool.retain(|_, keys| !keys.is_empty());
            keys.into_iter()
                .filter_map(|key| shards.by_key.remove(&key))
                .collect::<Vec<_>>()
        };
        let mut processes = Vec::new();
        for state in states {
            let mut state = state.lock().await;
            let keys = state
                .processes
                .keys()
                .filter(|key| key.formation_id.as_ref() == Some(worker_environment_id))
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(process) = state.processes.remove(&key) {
                    processes.push(process);
                }
            }
        }
        processes
    }

    pub(crate) async fn detach(&self, ctx: &InterfaceLiftContext) -> Result<(), String> {
        let states = {
            let mut shards = self.shards.lock().await;
            shards.by_tool.clear();
            std::mem::take(&mut shards.by_key)
                .into_iter()
                .collect::<Vec<_>>()
        };
        let (effective_ctx, root_error) = match self.owner.effective_context(ctx) {
            Ok(effective) => (effective, None),
            Err(error) => (None, (!states.is_empty()).then_some(error)),
        };
        let ctx = effective_ctx.as_ref().unwrap_or(ctx);
        let mut errors = root_error.into_iter().collect::<Vec<_>>();
        for (_, state) in states {
            if let Err(error) = state.lock().await.shutdown(ctx, false).await {
                errors.push(error);
            }
        }
        match tool_process_generation_root(ctx) {
            Ok(root) => {
                if let Err(error) = reconcile_tool_process_generations(&root, &BTreeSet::new()) {
                    errors.push(error);
                }
            }
            Err(error) => errors.push(error),
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

fn read_retained_worker_environment_record(
    records: &WorkerEnvironmentRecordStore,
    worker_environment_id: &WorkerEnvironmentId,
) -> Result<WorkerEnvironmentRecord, String> {
    let path = records.record_path(worker_environment_id);
    let directory = path.parent().ok_or_else(|| {
        format!(
            "worker-environment record '{}' has no owner directory",
            path.display()
        )
    })?;
    let root = directory.parent().ok_or_else(|| {
        format!(
            "worker-environment record directory '{}' has no store root",
            directory.display()
        )
    })?;
    let root_metadata = std::fs::symlink_metadata(root).map_err(|error| {
        format!(
            "worker-environment record root '{}' is unavailable: {error}",
            root.display()
        )
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!(
            "worker-environment record root '{}' is not a regular directory",
            root.display()
        ));
    }
    let directory_metadata = std::fs::symlink_metadata(directory).map_err(|error| {
        format!(
            "worker-environment record directory '{}' is unavailable: {error}",
            directory.display()
        )
    })?;
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return Err(format!(
            "worker-environment record directory '{}' is not a regular directory",
            directory.display()
        ));
    }
    let record_metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "worker-environment record '{}' is unavailable: {error}",
            path.display()
        )
    })?;
    if record_metadata.file_type().is_symlink() || !record_metadata.is_file() {
        return Err(format!(
            "worker-environment record '{}' is not a regular file",
            path.display()
        ));
    }
    records.read(worker_environment_id).map_err(|error| {
        format!(
            "worker environment '{}' retained formation is invalid: {error}",
            worker_environment_id
        )
    })
}

fn validate_worker_environment_release_runtime(
    envelope: &InterfaceInvocationEnvelope,
    formation: &WorkerEnvironmentFormation,
    owner: &ToolDoorOwner,
) -> Result<(), String> {
    let runtime = envelope.runtime().map_err(|error| error.to_string())?;
    let matches = runtime.run.as_str() == formation.run
        && runtime.ee.as_str() == formation.ee
        && runtime.worker == formation.worker
        && runtime.interface == formation.door_interface
        && runtime.interface == owner.door_interface;
    if !matches {
        return Err(format!(
            "worker environment '{}' release coordinates do not match its retained formation",
            formation.worker_environment_id
        ));
    }
    if owner.worker_environment_kind != formation.worker_environment_kind {
        return Err(format!(
            "worker environment '{}' release owner kind does not match its retained formation",
            formation.worker_environment_id
        ));
    }
    Ok(())
}

fn spawn_worker_environment_release_process(
    ctx: &InterfaceLiftContext,
    contract: &WorkerEnvironmentReleaseContract,
    private_read_max_bytes_ceiling: u64,
) -> Result<ToolProcessClient, String> {
    let mut release_ctx = ctx.clone();
    for (slot, path) in &contract.formation.door_root_bindings {
        release_ctx.root_slots.insert(slot.clone(), path.clone());
    }
    contract
        .formation
        .attest_door_root_bindings(&release_ctx.root_slots)
        .map_err(|error| error.to_string())?;
    let declaration_hash = canonical_hash(&contract.process)?;
    let environment = effective_tool_process_environment(&release_ctx, &contract.process)?;
    let environment_hash = tool_process_environment_hash(&environment);
    let maximum_bytes = private_read_max_bytes(
        &contract.process.private_read_subpaths,
        private_read_max_bytes_ceiling,
    )?;
    let private_read_hash = private_read_snapshot_hash(
        &release_ctx.interface_var_dir(),
        &contract.process.private_read_subpaths,
        maximum_bytes,
    )?;
    let invocation_authority_hash = empty_invocation_authority_hash();
    let execution_inputs_hash = execution_inputs::ExecutionInputCache::default()
        .resolve(
            &release_ctx,
            &contract.process,
            Some(&contract.kind),
            &environment,
        )?
        .hash;
    ToolProcessClient::spawn(
        &release_ctx,
        &contract.process,
        None,
        Some(&contract.formation),
        Some(&contract.kind),
        private_read_max_bytes_ceiling,
        &declaration_hash,
        &private_read_hash,
        &environment_hash,
        &invocation_authority_hash,
        &execution_inputs_hash,
    )
    .map_err(tool_process_error_message)
}

impl ToolProcessBrokerState {
    fn resolve_process_key(
        &mut self,
        ctx: &InterfaceLiftContext,
        tool: &str,
        arguments: &serde_json::Value,
        envelope: &ToolProcessEnvelope,
        owner: &ToolDoorOwner,
    ) -> Result<ToolProcessKey, ToolProcessBrokerResult> {
        require_tool_in_invocation_grant(envelope, tool)
            .map_err(|error| ToolProcessBrokerResult::CapabilityFailed { error })?;
        let tool_root = ctx
            .root_slot("registry_tool_root")
            .map(Path::to_path_buf)
            .map_err(|error| ToolProcessBrokerResult::Unavailable { error })?;
        let cached = self
            .load_effective_tool_descriptor(ctx, &tool_root, tool)
            .map_err(|error| ToolProcessBrokerResult::CapabilityFailed {
                error: format!("tool descriptor '{tool}' is malformed: {error}"),
            })?
            .ok_or_else(|| ToolProcessBrokerResult::CapabilityFailed {
                error: format!("tool descriptor '{tool}' is unavailable"),
            })?;
        let descriptor = cached.descriptor;
        require_owned_worker_environment_kind(&descriptor, owner)
            .map_err(|error| ToolProcessBrokerResult::CapabilityFailed { error })?;
        self.validate_arguments(&descriptor, arguments)
            .map_err(|error| ToolProcessBrokerResult::CapabilityFailed { error })?;
        let (hashes, process_bins) = self
            .granted_tool_descriptor_pins(ctx, &tool_root, envelope, &owner.worker_environment_kind)
            .map_err(|error| ToolProcessBrokerResult::Unavailable { error })?;
        let root = ctx
            .root_slot("registry_worker_environment_kind_root")
            .map_err(|error| ToolProcessBrokerResult::Unavailable { error })?;
        self.refresh_worker_environment_registry(root, &owner.worker_environment_kind)
            .map_err(|error| ToolProcessBrokerResult::Unavailable { error })?;
        let formed = form_worker_environment(
            ctx,
            &descriptor,
            envelope,
            &hashes,
            &process_bins,
            owner,
            self.worker_environment_registry
                .as_ref()
                .map(|cached| &cached.registry),
        )
        .map_err(|error| match error {
            ToolProcessInvokeError::Capability(error) => {
                ToolProcessBrokerResult::CapabilityFailed { error }
            }
            ToolProcessInvokeError::Unavailable(error) => {
                ToolProcessBrokerResult::Unavailable { error }
            }
        })?;
        require_owned_formation(&formed, owner)
            .map_err(|error| ToolProcessBrokerResult::CapabilityFailed { error })?;
        let formation = &formed.formation;
        let ee_root_slot_allowance = envelope
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.grant().ee_root_slot_allowance_set());
        let effective_declaration = narrow_process_declaration(
            &descriptor.process,
            Some(formation),
            ee_root_slot_allowance.as_ref(),
        )
        .map_err(|error| ToolProcessBrokerResult::CapabilityFailed { error })?;
        let effective_environment = effective_tool_process_environment(ctx, &effective_declaration)
            .map_err(|error| ToolProcessBrokerResult::Unavailable { error })?;
        let effective_declaration_hash = canonical_hash(&effective_declaration)
            .map_err(|error| ToolProcessBrokerResult::Unavailable { error })?;
        let effective_environment_hash = tool_process_environment_hash(&effective_environment);
        let invocation_authority_hash = if effective_declaration.authority.has_argument_paths() {
            invocation_authority_hash(&effective_declaration.authority, &ctx.root_slots, arguments)
                .map_err(|error| match error {
                    ProcessInvocationAuthorityError::Unavailable(error) => {
                        ToolProcessBrokerResult::Unavailable { error }
                    }
                    ProcessInvocationAuthorityError::Refused(error) => {
                        ToolProcessBrokerResult::CapabilityFailed { error }
                    }
                })?
        } else {
            empty_invocation_authority_hash()
        };
        let private_read_max_bytes = private_read_max_bytes(
            &descriptor.process.private_read_subpaths,
            owner.private_read_max_bytes,
        )
        .map_err(|error| ToolProcessBrokerResult::Unavailable { error })?;
        let private_read_snapshot_hash = private_read_snapshot_hash(
            &ctx.interface_var_dir(),
            &descriptor.process.private_read_subpaths,
            private_read_max_bytes,
        )
        .map_err(|error| ToolProcessBrokerResult::Unavailable { error })?;
        let execution_inputs_hash = self
            .execution_inputs
            .resolve(
                ctx,
                &effective_declaration,
                Some(&formed.kind),
                &effective_environment,
            )
            .map_err(|error| ToolProcessBrokerResult::Unavailable { error })?
            .hash;
        Ok(ToolProcessKey {
            declaration: effective_declaration,
            effective_declaration_hash,
            formation_id: Some(formation.worker_environment_id.clone()),
            formation_scope: Some(formation.scope_key()),
            private_read_snapshot_hash,
            effective_environment_hash,
            invocation_authority_hash,
            execution_inputs_hash,
        })
    }

    pub(crate) async fn invoke(
        &mut self,
        ctx: &InterfaceLiftContext,
        tool: String,
        arguments: serde_json::Value,
        mut envelope: ToolProcessEnvelope,
        routed_process_key: ToolProcessKey,
        terminalize_on_failure: bool,
        interrupted_recovery: bool,
        owner: &ToolDoorOwner,
        output_callback: Option<&ToolDoorOutputCallback>,
        primary_input: &mut Option<ToolProcessPrimaryInput>,
    ) -> ToolProcessBrokerResult {
        let source_movement_seq = envelope.source_movement_seq;
        if let Err(error) = require_tool_in_invocation_grant(&envelope, &tool) {
            return ToolProcessBrokerResult::CapabilityFailed { error };
        }
        let tool_root = match ctx.root_slot("registry_tool_root") {
            Ok(root) => root.to_path_buf(),
            Err(error) => return ToolProcessBrokerResult::Unavailable { error },
        };
        if let Err(error) = self
            .reconcile(ctx, &tool_root, Some(source_movement_seq))
            .await
        {
            return ToolProcessBrokerResult::Unavailable { error };
        }
        let cached_descriptor = match self.load_effective_tool_descriptor(ctx, &tool_root, &tool) {
            Ok(Some(cached)) => cached,
            Ok(None) => {
                return ToolProcessBrokerResult::CapabilityFailed {
                    error: format!("tool descriptor '{tool}' is unavailable"),
                };
            }
            Err(error) => {
                return ToolProcessBrokerResult::CapabilityFailed {
                    error: format!("tool descriptor '{tool}' is malformed: {error}"),
                };
            }
        };
        let descriptor = cached_descriptor.descriptor;
        if let Err(error) = require_owned_worker_environment_kind(&descriptor, owner) {
            return ToolProcessBrokerResult::CapabilityFailed { error };
        }
        if let Err(error) = self.validate_arguments(&descriptor, &arguments) {
            return ToolProcessBrokerResult::CapabilityFailed { error };
        }
        let ee_root_slot_allowance = envelope
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.grant().ee_root_slot_allowance_set());
        let (tool_descriptor_hashes, tool_process_bins) = match self.granted_tool_descriptor_pins(
            ctx,
            &tool_root,
            &envelope,
            &owner.worker_environment_kind,
        ) {
            Ok(pins) => pins,
            Err(error) => return ToolProcessBrokerResult::Unavailable { error },
        };
        let wek_root = match ctx.root_slot("registry_worker_environment_kind_root") {
            Ok(root) => root,
            Err(error) => return ToolProcessBrokerResult::Unavailable { error },
        };
        if let Err(error) =
            self.refresh_worker_environment_registry(wek_root, &owner.worker_environment_kind)
        {
            return ToolProcessBrokerResult::Unavailable { error };
        }
        let registry = self
            .worker_environment_registry
            .as_ref()
            .map(|cached| &cached.registry);
        let formed = match form_worker_environment(
            ctx,
            &descriptor,
            &envelope,
            &tool_descriptor_hashes,
            &tool_process_bins,
            owner,
            registry,
        ) {
            Ok(formed) => formed,
            Err(ToolProcessInvokeError::Capability(error)) => {
                return ToolProcessBrokerResult::CapabilityFailed { error };
            }
            Err(ToolProcessInvokeError::Unavailable(error)) => {
                return ToolProcessBrokerResult::Unavailable { error };
            }
        };
        if let Err(error) = require_owned_formation(&formed, owner) {
            return ToolProcessBrokerResult::CapabilityFailed { error };
        }
        let formation = formed.formation;
        let worker_environment_kind = formed.kind;
        if let Err(error) = refuse_released_worker_environment(ctx, &formation) {
            return ToolProcessBrokerResult::CapabilityFailed { error };
        }
        envelope.worker_environment = Some(formation.clone());
        envelope.worker_environment_kind = Some(worker_environment_kind.clone());
        let base_declaration = descriptor.process.clone();
        let effective_declaration = match narrow_process_declaration(
            &base_declaration,
            Some(&formation),
            ee_root_slot_allowance.as_ref(),
        ) {
            Ok(declaration) => declaration,
            Err(error) => return ToolProcessBrokerResult::CapabilityFailed { error },
        };
        let effective_environment =
            match effective_tool_process_environment(ctx, &effective_declaration) {
                Ok(environment) => environment,
                Err(error) => return ToolProcessBrokerResult::Unavailable { error },
            };
        let effective_declaration_hash = match canonical_hash(&effective_declaration) {
            Ok(hash) => hash,
            Err(error) => return ToolProcessBrokerResult::Unavailable { error },
        };
        let effective_environment_hash = tool_process_environment_hash(&effective_environment);
        let invocation_authority_hash = if effective_declaration.authority.has_argument_paths() {
            match invocation_authority_hash(
                &effective_declaration.authority,
                &ctx.root_slots,
                &arguments,
            ) {
                Ok(hash) => hash,
                Err(ProcessInvocationAuthorityError::Unavailable(error)) => {
                    return ToolProcessBrokerResult::Unavailable { error };
                }
                Err(ProcessInvocationAuthorityError::Refused(error)) => {
                    return ToolProcessBrokerResult::CapabilityFailed { error };
                }
            }
        } else {
            empty_invocation_authority_hash()
        };
        let private_read_max_bytes = match private_read_max_bytes(
            &base_declaration.private_read_subpaths,
            owner.private_read_max_bytes,
        ) {
            Ok(maximum_bytes) => maximum_bytes,
            Err(error) => return ToolProcessBrokerResult::Unavailable { error },
        };
        let private_read_snapshot_hash = match private_read_snapshot_hash(
            &ctx.interface_var_dir(),
            &base_declaration.private_read_subpaths,
            private_read_max_bytes,
        ) {
            Ok(hash) => hash,
            Err(error) => return ToolProcessBrokerResult::Unavailable { error },
        };
        let process_key = ToolProcessKey {
            declaration: effective_declaration.clone(),
            effective_declaration_hash,
            formation_id: Some(formation.worker_environment_id.clone()),
            formation_scope: Some(formation.scope_key()),
            private_read_snapshot_hash,
            effective_environment_hash,
            invocation_authority_hash,
            execution_inputs_hash: match self.execution_inputs.resolve(
                ctx,
                &effective_declaration,
                Some(&worker_environment_kind),
                &effective_environment,
            ) {
                Ok(inputs) => inputs.hash,
                Err(error) => return ToolProcessBrokerResult::Unavailable { error },
            },
        };
        if process_key != routed_process_key {
            return ToolProcessBrokerResult::Unavailable {
                error: "tool process authority changed during route formation".to_string(),
            };
        }
        self.declarations_by_tool
            .insert(tool.clone(), (base_declaration, process_key.clone()));
        if let Some(scope) = process_key.formation_scope.as_deref() {
            let stale = self
                .processes
                .keys()
                .filter(|key| {
                    key.formation_scope.as_deref() == Some(scope)
                        && key.formation_id != process_key.formation_id
                })
                .cloned()
                .collect::<Vec<_>>();
            if let Err(error) = self
                .release_processes(ctx, stale, Some(source_movement_seq), false)
                .await
            {
                return ToolProcessBrokerResult::Unavailable { error };
            }
        }
        if !self.processes.contains_key(&process_key) {
            if let Err(error) = quiesce_orphaned_generation_owner(ctx, &process_key).await {
                return ToolProcessBrokerResult::Unavailable { error };
            }
            let arguments_for_spawn = effective_declaration
                .authority
                .has_argument_paths()
                .then_some(&arguments);
            let process = match ToolProcessClient::spawn(
                ctx,
                &effective_declaration,
                arguments_for_spawn,
                Some(&formation),
                Some(&worker_environment_kind),
                owner.private_read_max_bytes,
                &process_key.effective_declaration_hash,
                &process_key.private_read_snapshot_hash,
                &process_key.effective_environment_hash,
                &process_key.invocation_authority_hash,
                &process_key.execution_inputs_hash,
            ) {
                Ok(process) => process,
                Err(ToolProcessInvokeError::Capability(error)) => {
                    if terminalize_on_failure {
                        if let Err(record_error) = self.finish_formation_if_unreferenced(
                            ctx,
                            &formation,
                            envelope.source_movement_seq,
                            WorkerEnvironmentLifecycle::Failed,
                        ) {
                            return ToolProcessBrokerResult::Unavailable {
                                error: format!(
                                    "{error}; worker-environment failure could not be recorded: {record_error}"
                                ),
                            };
                        }
                    }
                    return ToolProcessBrokerResult::CapabilityFailed { error };
                }
                Err(ToolProcessInvokeError::Unavailable(error)) => {
                    if terminalize_on_failure {
                        if let Err(record_error) = self.finish_formation_if_unreferenced(
                            ctx,
                            &formation,
                            envelope.source_movement_seq,
                            WorkerEnvironmentLifecycle::Failed,
                        ) {
                            return ToolProcessBrokerResult::Unavailable {
                                error: format!(
                                    "{error}; worker-environment failure could not be recorded: {record_error}"
                                ),
                            };
                        }
                    }
                    return ToolProcessBrokerResult::Unavailable { error };
                }
            };
            self.processes.insert(process_key.clone(), process);
        }
        if let Some(source) = envelope.output_source.as_mut() {
            source.formation = Some(formation.clone());
        }
        let completed_input = envelope.clone();
        let result = {
            let Some(process) = self.processes.get_mut(&process_key) else {
                return ToolProcessBrokerResult::Unavailable {
                    error: "tool process pool lost a just-inserted process".to_string(),
                };
            };
            process
                .invoke(
                    ctx,
                    descriptor,
                    envelope,
                    terminalize_on_failure,
                    interrupted_recovery,
                    output_callback,
                )
                .await
        };
        if let Err(ToolProcessInvokeError::Unavailable(invocation_error)) = &result {
            if let Some(mut process) = self.processes.remove(&process_key) {
                if let Err(error) = self
                    .release_client(ctx, &mut process, Some(source_movement_seq), false)
                    .await
                {
                    return ToolProcessBrokerResult::Unavailable {
                        error: format!(
                            "tool process became unavailable: {invocation_error}; worker-environment release failed: {error}"
                        ),
                    };
                }
            }
        }
        match result {
            Ok(completion)
                if completion.artifacts.is_empty() && !completion.streamed_output_admitted =>
            {
                ToolProcessBrokerResult::CapabilityFailed {
                    error: format!("tool process for '{tool}' emitted no artifacts"),
                }
            }
            Ok(completion) => {
                hand_completed_tool_outputs(ctx, &completed_input, completion, output_callback, primary_input).await
            }
            Err(ToolProcessInvokeError::Capability(error)) => {
                ToolProcessBrokerResult::CapabilityFailed { error }
            }
            Err(ToolProcessInvokeError::Unavailable(error)) => {
                ToolProcessBrokerResult::Unavailable { error }
            }
        }
    }

    async fn reconcile(
        &mut self,
        ctx: &InterfaceLiftContext,
        tool_root: &Path,
        source_movement_seq: Option<u64>,
    ) -> Result<(), String> {
        let mut retained = BTreeMap::new();
        let declarations = self
            .declarations_by_tool
            .iter()
            .map(|(tool, declaration)| (tool.clone(), declaration.clone()))
            .collect::<Vec<_>>();
        for (tool, (declaration, process_key)) in declarations {
            let Some(descriptor) = self
                .load_effective_tool_descriptor(ctx, tool_root, &tool)
                .map_err(|error| {
                    format!(
                        "tool descriptor '{tool}' became malformed during reconciliation: {error}"
                    )
                })?
            else {
                continue;
            };
            if descriptor.descriptor.process == declaration {
                retained.insert(tool, (declaration, process_key));
            }
        }
        self.declarations_by_tool = retained;
        let live = self
            .declarations_by_tool
            .values()
            .map(|(_, key)| key.clone())
            .collect::<BTreeSet<_>>();
        let stale = self
            .processes
            .keys()
            .filter(|key| !live.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        self.release_processes(ctx, stale, source_movement_seq, false)
            .await
    }

    fn load_effective_tool_descriptor(
        &mut self,
        ctx: &InterfaceLiftContext,
        tool_root: &Path,
        tool: &str,
    ) -> Result<Option<CachedToolDescriptor>, String> {
        let path = registry_entry_file(tool_root, tool, "tool.json")?;
        let Some(fingerprint) = registry_file_fingerprint(&path)? else {
            self.tool_descriptors.remove(tool);
            self.argument_validators.remove(tool);
            return Ok(None);
        };
        if let Some(cached) = self.tool_descriptors.get(tool) {
            if cached.fingerprint == fingerprint {
                return Ok(Some(cached.clone()));
            }
        }
        let descriptor = load_tool_descriptor_from_dir(tool_root, tool)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!(
                    "tool descriptor '{}' disappeared after stat",
                    path.display()
                )
            })?;
        let descriptor = effective_tool_descriptor(ctx, descriptor)?;
        if registry_file_fingerprint(&path)? != Some(fingerprint) {
            return Err(format!(
                "tool descriptor '{}' changed while being loaded",
                path.display()
            ));
        }
        let cached = CachedToolDescriptor {
            fingerprint,
            canonical_hash: canonical_hash(&descriptor)?,
            descriptor,
        };
        self.tool_descriptors
            .insert(tool.to_string(), cached.clone());
        Ok(Some(cached))
    }

    fn load_tool_owner(
        &mut self,
        tool_root: &Path,
        tool: &str,
    ) -> Result<Option<InterfaceId>, String> {
        let path = registry_entry_file(tool_root, tool, "tool.json")?;
        let Some(fingerprint) = registry_file_fingerprint(&path)? else {
            self.tool_owners.remove(tool);
            return Ok(None);
        };
        if let Some(cached) = self.tool_owners.get(tool)
            && cached.fingerprint == fingerprint
        {
            return Ok(Some(cached.interface.clone()));
        }
        let descriptor = load_tool_descriptor_from_dir(tool_root, tool)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!(
                    "tool descriptor '{}' disappeared after stat",
                    path.display()
                )
            })?;
        let interface = descriptor.interface;
        if registry_file_fingerprint(&path)? != Some(fingerprint) {
            return Err(format!(
                "tool descriptor '{}' changed while resolving its owner",
                path.display()
            ));
        }
        self.tool_owners.insert(
            tool.to_string(),
            CachedToolOwner {
                fingerprint,
                interface: interface.clone(),
            },
        );
        Ok(Some(interface))
    }

    fn granted_tool_descriptor_pins(
        &mut self,
        ctx: &InterfaceLiftContext,
        tool_root: &Path,
        envelope: &ToolProcessEnvelope,
        owner_kind: &WorkerEnvironmentKindId,
    ) -> Result<(BTreeMap<String, String>, BTreeMap<String, String>), String> {
        let metadata = envelope
            .metadata
            .as_ref()
            .ok_or_else(|| "worker-environment tool invocation carries no metadata".to_string())?;
        let interface_root = ctx.root_slot("registry_interface_root")?;
        load_interface_descriptor(interface_root, &ctx.interface)?;
        let mut hashes = BTreeMap::new();
        let mut process_bins = BTreeMap::new();
        for tool in &metadata.grant().tools {
            let owner = self
                .load_tool_owner(tool_root, tool)?
                .ok_or_else(|| format!("granted tool descriptor '{tool}' is unavailable"))?;
            if owner != ctx.interface {
                continue;
            }
            let descriptor = self
                .load_effective_tool_descriptor(ctx, tool_root, tool)?
                .ok_or_else(|| format!("granted tool descriptor '{tool}' is unavailable"))?;
            if !descriptor
                .descriptor
                .supported_worker_environment_kinds
                .iter()
                .any(|kind| kind == owner_kind)
            {
                continue;
            }
            hashes.insert(tool.clone(), descriptor.canonical_hash);
            process_bins.insert(tool.clone(), descriptor.descriptor.process.bin.clone());
        }
        Ok((hashes, process_bins))
    }

    fn refresh_worker_environment_registry(
        &mut self,
        root: &Path,
        kind: &WorkerEnvironmentKindId,
    ) -> Result<(), String> {
        let path = registry_entry_file(root, kind.as_str(), "worker_environment_kind.json")?;
        let canonical_root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
        let canonical_path = std::fs::canonicalize(&path).map_err(|error| error.to_string())?;
        if canonical_path != canonical_root.join(kind.as_str()).join("worker_environment_kind.json") {
            self.worker_environment_registry = None;
            return Err("worker-environment descriptor escapes its exact kind owner".into());
        }
        let fingerprint = registry_file_fingerprint(&path)?
            .ok_or_else(|| format!("worker-environment kind '{kind}' is unavailable"))?;
        if self
            .worker_environment_registry
            .as_ref()
            .is_some_and(|cached| cached.fingerprint == fingerprint && cached.kind == *kind)
        {
            return Ok(());
        }
        self.worker_environment_registry = None;
        let registry = WorkerEnvKindRegistry::load_kind_from_dir(root, kind)
            .map_err(|error| error.to_string())?;
        if registry_file_fingerprint(&path)? != Some(fingerprint.clone()) {
            return Err("worker-environment descriptors changed while being loaded".to_string());
        }
        self.worker_environment_registry = Some(CachedWorkerEnvironmentRegistry {
            fingerprint,
            kind: kind.clone(),
            registry,
        });
        Ok(())
    }

    fn load_worker_environment_release_contract(
        &mut self,
        ctx: &InterfaceLiftContext,
        formation: WorkerEnvironmentFormation,
        owner: &ToolDoorOwner,
    ) -> Result<WorkerEnvironmentReleaseContract, String> {
        formation.validate().map_err(|error| error.to_string())?;
        let owner_kind = &owner.worker_environment_kind;
        if formation.worker_environment_kind != *owner_kind {
            return Err(format!(
                "tool-door kind '{}' refuses release for kind '{}'",
                owner_kind, formation.worker_environment_kind
            ));
        }
        if formation.door_interface != owner.door_interface
            || formation.door_interface != ctx.interface
        {
            return Err(format!(
                "tool-door interface '{}' refuses release for door '{}'",
                ctx.interface, formation.door_interface
            ));
        }

        let interface_root = ctx.root_slot("registry_interface_root")?;
        let door = load_interface_descriptor(interface_root, &ctx.interface)?;
        let observed_door_hash = canonical_hash(&door)?;
        if observed_door_hash != formation.door_descriptor_hash {
            return Err(format!(
                "worker environment '{}' door descriptor changed after formation",
                formation.worker_environment_id
            ));
        }

        let kind_root = ctx.root_slot("registry_worker_environment_kind_root")?;
        self.refresh_worker_environment_registry(kind_root, &owner.worker_environment_kind)?;
        let registry = self
            .worker_environment_registry
            .as_ref()
            .ok_or_else(|| "worker-environment registry projection is unavailable".to_string())?;
        let (kind, _) = registry
            .registry
            .get(owner_kind)
            .map_err(|error| error.to_string())?;
        let kind = kind.as_ref().clone();
        formation
            .validate_against_kind(&kind)
            .map_err(|error| error.to_string())?;

        let tool_root = ctx.root_slot("registry_tool_root")?.to_path_buf();
        let mut selected = None;
        for tool in &formation.granted_tools {
            let descriptor = self
                .load_effective_tool_descriptor(ctx, &tool_root, tool)?
                .ok_or_else(|| format!("formed tool descriptor '{tool}' is unavailable"))?;
            if descriptor.descriptor.interface != ctx.interface {
                return Err(format!(
                    "formed tool '{}' belongs to interface '{}', not release door '{}'",
                    tool, descriptor.descriptor.interface, ctx.interface
                ));
            }
            require_owned_worker_environment_kind(&descriptor.descriptor, owner)?;
            let expected_hash = formation
                .tool_descriptor_hashes
                .get(tool)
                .ok_or_else(|| format!("formed tool '{tool}' has no pinned descriptor hash"))?;
            if &descriptor.canonical_hash != expected_hash {
                return Err(format!(
                    "formed tool '{}' descriptor changed after worker-environment formation",
                    tool
                ));
            }
            if selected.is_none() && !descriptor.descriptor.process.authority.has_argument_paths() {
                selected = Some(descriptor.descriptor.process);
            }
        }
        let process = selected.ok_or_else(|| {
            format!(
                "worker environment '{}' has no pinned granted tool process without invocation argument authority for cold release",
                formation.worker_environment_id
            )
        })?;
        let process = narrow_process_declaration(&process, Some(&formation), None)?;

        Ok(WorkerEnvironmentReleaseContract {
            formation,
            kind,
            process,
        })
    }

    async fn release_processes(
        &mut self,
        ctx: &InterfaceLiftContext,
        keys: Vec<ToolProcessKey>,
        source_movement_seq: Option<u64>,
        terminalize_formations: bool,
    ) -> Result<(), String> {
        let mut remaining = BTreeMap::<WorkerEnvironmentId, usize>::new();
        if terminalize_formations {
            for key in &keys {
                if self.processes.contains_key(key)
                    && let Some(id) = &key.formation_id
                {
                    *remaining.entry(id.clone()).or_default() += 1;
                }
            }
        }
        let mut errors = Vec::new();
        let mut failed_formations = BTreeSet::new();
        for key in keys {
            let Some(mut process) = self.processes.remove(&key) else {
                continue;
            };
            let last_formation_process = terminalize_formations
                && key.formation_id.as_ref().is_some_and(|id| {
                    let count = remaining.get_mut(id).expect("formation count exists");
                    *count -= 1;
                    *count == 0
                });
            let closure_failed_before_release = last_formation_process
                && key
                    .formation_id
                    .as_ref()
                    .is_some_and(|id| failed_formations.contains(id));
            let terminalize_formation = last_formation_process && !closure_failed_before_release;
            let formation = process.formation.clone();
            let failure_movement_seq =
                source_movement_seq.unwrap_or(process.last_source_movement_seq);
            if let Err(error) = self
                .release_client(
                    ctx,
                    &mut process,
                    source_movement_seq,
                    terminalize_formation,
                )
                .await
            {
                if let Some(id) = key.formation_id.clone() {
                    failed_formations.insert(id);
                }
                errors.push(error);
            }
            if closure_failed_before_release
                && let Some(formation) = formation
                && let Err(error) = self.finish_formation_if_unreferenced(
                    ctx,
                    &formation,
                    failure_movement_seq,
                    WorkerEnvironmentLifecycle::Failed,
                )
            {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    async fn release_client(
        &self,
        ctx: &InterfaceLiftContext,
        process: &mut ToolProcessClient,
        source_movement_seq: Option<u64>,
        terminalize_formation: bool,
    ) -> Result<(), String> {
        let release_result = match process.formation.clone() {
            None => process
                .release(ctx, WorkerEnvironmentReleaseMode::LiveOwner)
                .await
                .map_err(tool_process_error_message),
            Some(_) if !terminalize_formation => Ok(()),
            Some(formation) => {
                let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
                let lock = worker_environment_record_lock(
                    &ctx.interface_var_dir(),
                    &formation.worker_environment_id,
                )?;
                let source_movement_seq = source_movement_seq
                    .or_else(|| {
                        let _guard = lock.lock().ok()?;
                        records
                            .read(&formation.worker_environment_id)
                            .ok()
                            .map(|record| record.last_activity_movement_seq)
                    })
                    .unwrap_or(process.last_source_movement_seq);
                let release = process
                    .release(ctx, WorkerEnvironmentReleaseMode::LiveOwner)
                    .await;
                let _guard = lock
                    .lock()
                    .map_err(|_| "worker-environment record lock is poisoned".to_string())?;
                match release {
                    Ok(()) => records
                        .release(&formation, source_movement_seq)
                        .map(|_| ())
                        .map_err(|error| {
                            format!(
                                "worker environment '{}' released its substrate but could not record release: {error}",
                                formation.worker_environment_id
                            )
                        }),
                    Err(error) => {
                        let release_error = tool_process_error_message(error);
                        match records.fail(&formation, source_movement_seq) {
                            Ok(_) => Err(format!(
                                "worker environment '{}' could not release its substrate: {release_error}",
                                formation.worker_environment_id
                            )),
                            Err(record_error) => Err(format!(
                                "worker environment '{}' could not release its substrate: {release_error}; failure could not be recorded: {record_error}",
                                formation.worker_environment_id
                            )),
                        }
                    }
                }
            }
        };
        let cleanup_result = process.terminate_and_remove_generation().await;
        match (release_result, cleanup_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(release_error), Err(cleanup_error)) => {
                Err(format!("{release_error}; {cleanup_error}"))
            }
        }
    }

    pub(crate) async fn shutdown(
        &mut self,
        ctx: &InterfaceLiftContext,
        terminalize_formations: bool,
    ) -> Result<(), String> {
        self.declarations_by_tool.clear();
        let keys = self.processes.keys().cloned().collect::<Vec<_>>();
        self.release_processes(ctx, keys, None, terminalize_formations)
            .await
    }

    fn finish_formation_if_unreferenced(
        &mut self,
        ctx: &InterfaceLiftContext,
        formation: &WorkerEnvironmentFormation,
        source_movement_seq: u64,
        lifecycle: WorkerEnvironmentLifecycle,
    ) -> Result<(), String> {
        let id = &formation.worker_environment_id;
        if self
            .processes
            .keys()
            .any(|key| key.formation_id.as_ref() == Some(id))
        {
            return Ok(());
        }
        let lock = worker_environment_record_lock(&ctx.interface_var_dir(), id)?;
        let _guard = lock
            .lock()
            .map_err(|_| "worker-environment record lock is poisoned".to_string())?;
        let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
        match lifecycle {
            WorkerEnvironmentLifecycle::Failed => records
                .fail(formation, source_movement_seq)
                .map(|_| ())
                .map_err(|error| {
                    format!("worker environment '{id}' could not record failure: {error}")
                }),
            WorkerEnvironmentLifecycle::Released => records
                .read(id)
                .map_err(|error| {
                    format!("worker environment '{id}' could not be read before release: {error}")
                })
                .and_then(|record| {
                    if record.lifecycle == WorkerEnvironmentLifecycle::Active {
                        records
                            .release(formation, source_movement_seq)
                            .map(|_| ())
                            .map_err(|error| {
                                format!(
                                    "worker environment '{id}' could not record release: {error}"
                                )
                            })
                    } else {
                        Ok(())
                    }
                }),
            WorkerEnvironmentLifecycle::Active => {
                Err(format!("worker environment '{id}' cannot finish as active"))
            }
        }
    }

    fn validate_arguments(
        &mut self,
        descriptor: &ToolDescriptor,
        arguments: &serde_json::Value,
    ) -> Result<(), String> {
        let tool = descriptor.id.as_str();
        let schema_hash = ToolArgumentValidator::hash(&descriptor.args_schema);
        let replace = self
            .argument_validators
            .get(tool)
            .is_none_or(|validator| validator.schema_hash() != schema_hash);
        if replace {
            let validator =
                ToolArgumentValidator::compile(&descriptor.args_schema).map_err(|error| {
                    format!("tool '{tool}' descriptor cannot form its args_schema: {error}")
                })?;
            self.argument_validators.insert(tool.to_string(), validator);
        }
        self.argument_validators
            .get(tool)
            .ok_or_else(|| format!("tool '{tool}' argument validator disappeared after formation"))?
            .validate(arguments)
            .map_err(|error| format!("tool '{tool}' arguments violate args_schema: {error}"))
    }
}

fn tool_process_error_message(error: ToolProcessInvokeError) -> String {
    match error {
        ToolProcessInvokeError::Unavailable(error) | ToolProcessInvokeError::Capability(error) => {
            error
        }
    }
}

struct FormedToolWorkerEnvironment {
    formation: WorkerEnvironmentFormation,
    kind: WorkerEnvKindSpec,
}

fn require_owned_worker_environment_kind(
    descriptor: &ToolDescriptor,
    owner: &ToolDoorOwner,
) -> Result<(), String> {
    if descriptor
        .supported_worker_environment_kinds
        .iter()
        .any(|kind| kind == &owner.worker_environment_kind)
    {
        Ok(())
    } else {
        Err(format!(
            "tool '{}' does not belong to worker-environment kind '{}'",
            descriptor.id, owner.worker_environment_kind
        ))
    }
}

fn require_owned_formation(
    formed: &FormedToolWorkerEnvironment,
    owner: &ToolDoorOwner,
) -> Result<(), String> {
    if formed.formation.worker_environment_kind != owner.worker_environment_kind {
        return Err(format!(
            "tool-door kind '{}' refuses formation for '{}'",
            owner.worker_environment_kind, formed.formation.worker_environment_kind
        ));
    }
    if formed.formation.door_interface != owner.door_interface {
        return Err(format!(
            "tool-door interface '{}' refuses formation for '{}'",
            owner.door_interface, formed.formation.door_interface
        ));
    }
    Ok(())
}

fn form_worker_environment(
    ctx: &InterfaceLiftContext,
    descriptor: &ToolDescriptor,
    envelope: &ToolProcessEnvelope,
    tool_descriptor_hashes: &BTreeMap<String, String>,
    tool_process_bins: &BTreeMap<String, String>,
    owner: &ToolDoorOwner,
    registry: Option<&WorkerEnvKindRegistry>,
) -> Result<FormedToolWorkerEnvironment, ToolProcessInvokeError> {
    let registry = registry.ok_or_else(|| {
        ToolProcessInvokeError::Unavailable(
            "worker-environment registry projection is unavailable".to_string(),
        )
    })?;
    let metadata = envelope.metadata.as_ref().ok_or_else(|| {
        ToolProcessInvokeError::Capability(
            "worker-environment tool invocation carries no metadata".to_string(),
        )
    })?;
    let runtime = metadata.runtime.as_ref().ok_or_else(|| {
        ToolProcessInvokeError::Capability(
            "worker-environment tool invocation carries no runtime coordinates".to_string(),
        )
    })?;
    let interface_root = ctx
        .root_slot("registry_interface_root")
        .map_err(ToolProcessInvokeError::Unavailable)?;
    let door = load_interface_descriptor(interface_root, &ctx.interface)
        .map_err(ToolProcessInvokeError::Unavailable)?;
    let door_descriptor_hash =
        canonical_hash(&door).map_err(ToolProcessInvokeError::Unavailable)?;
    load_interface_descriptor(interface_root, &runtime.worker)
        .map_err(ToolProcessInvokeError::Unavailable)?;
    let granted_worker_environment_kinds = metadata
        .grant()
        .worker_environment_kinds
        .iter()
        .map(|id| WorkerEnvironmentKindId::new(id.as_str()))
        .collect::<Vec<_>>();
    let owner_kind = &owner.worker_environment_kind;
    if !descriptor
        .supported_worker_environment_kinds
        .iter()
        .any(|kind| kind == owner_kind)
    {
        return Err(ToolProcessInvokeError::Capability(format!(
            "tool '{}' does not declare tool-door code kind '{}'",
            descriptor.id, owner_kind
        )));
    }
    let owned_kind = [owner_kind.clone()];
    let door_root_bindings = ctx.root_slots.clone();
    let door_additional_root_bindings = octopus_interfaces::additional_host_root_paths(&door, &door_root_bindings).map_err(ToolProcessInvokeError::Unavailable)?;
    let ee_root_slot_allowance = metadata.grant().ee_root_slot_allowance_set();
    let door_instance_dir = interface_root.join(ctx.interface.as_str());
    let request = WorkerEnvironmentFormationRequest {
        door_additional_root_bindings: &door_additional_root_bindings,
        door_interface: &ctx.interface,
        door_descriptor_hash: &door_descriptor_hash,
        door_driver_ref: &door.interface_driver_ref,
        tool: descriptor.id.as_str(),
        tool_descriptor_hashes,
        tool_process_bins,
        tool_worker_environment_kinds: &owned_kind,
        worker: &runtime.worker,
        ee: runtime.ee.as_str(),
        run: runtime.run.as_str(),
        granted_worker_environment_kinds: &granted_worker_environment_kinds,
        granted_tools: &metadata.grant().tools,
        door_root_bindings: &door_root_bindings,
        door_instance_dir: &door_instance_dir,
        ee_root_slot_allowance: ee_root_slot_allowance.as_ref(),
    };
    let formation = registry
        .form(&request)
        .map_err(|error| ToolProcessInvokeError::Capability(error.to_string()))?;
    let (kind, hash) = registry
        .get(&formation.worker_environment_kind)
        .map_err(|error| ToolProcessInvokeError::Unavailable(error.to_string()))?;
    if hash.to_hex() != formation.worker_environment_kind_descriptor_hash {
        return Err(ToolProcessInvokeError::Unavailable(
            "worker-environment descriptor changed during formation".to_string(),
        ));
    }
    Ok(FormedToolWorkerEnvironment {
        formation,
        kind: (*kind).clone(),
    })
}

fn require_tool_in_invocation_grant(
    envelope: &ToolProcessEnvelope,
    tool: &str,
) -> Result<(), String> {
    let metadata = envelope
        .metadata
        .as_ref()
        .ok_or_else(|| "tool-process invocation carries no formed grant metadata".to_string())?;
    if metadata
        .grant()
        .hard_blocked_tools
        .iter()
        .any(|blocked| blocked == tool)
    {
        return Err(format!(
            "tool-process invocation requests hard-blocked tool '{tool}'"
        ));
    }
    if !metadata.grant().tools.iter().any(|granted| granted == tool) {
        return Err(format!(
            "tool-process invocation requests tool '{tool}' outside the formed grant"
        ));
    }
    Ok(())
}

fn canonical_hash(value: &impl serde::Serialize) -> Result<String, String> {
    let value = serde_json::to_value(value)
        .map_err(|error| format!("descriptor failed to encode for hashing: {error}"))?;
    Ok(blake3::hash(&octopus_descriptors::canonical_bytes(&value))
        .to_hex()
        .to_string())
}

fn registry_entry_file(root: &Path, id: &str, file: &str) -> Result<PathBuf, String> {
    let mut components = Path::new(id).components();
    let safe = matches!(
        components.next(),
        Some(std::path::Component::Normal(name)) if name.to_str() == Some(id)
    ) && components.next().is_none();
    if !safe || id.trim().is_empty() {
        return Err(format!(
            "registry descriptor id '{id}' is not a directory name"
        ));
    }
    Ok(root.join(id).join(file))
}

fn registry_file_fingerprint(path: &Path) -> Result<Option<RegistryFileFingerprint>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "registry descriptor '{}' cannot be inspected: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "registry descriptor '{}' is not a regular file",
            path.display()
        ));
    }
    Ok(Some(RegistryFileFingerprint {
        len: metadata.len(),
        modified: metadata.modified().map_err(|error| {
            format!(
                "registry descriptor '{}' has no modification time: {error}",
                path.display()
            )
        })?,
        #[cfg(unix)]
        identity_and_change: {
            use std::os::unix::fs::MetadataExt;
            (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        },
    }))
}

fn narrow_process_declaration(
    declaration: &ToolProcessDeclaration,
    formation: Option<&WorkerEnvironmentFormation>,
    ee_root_slot_allowance: Option<&BTreeSet<String>>,
) -> Result<ToolProcessDeclaration, String> {
    let Some(formation) = formation else {
        let mut declaration = declaration.clone();
        if let Some(allowed) = ee_root_slot_allowance {
            declaration.authority =
                narrow_process_authority_to_root_slots(&declaration.authority, allowed)?;
            declaration
                .coordinate_root_slots
                .retain(|slot| allowed.contains(slot));
        }
        return Ok(declaration);
    };

    let mut declaration = declaration.clone();
    let readable = formation
        .readable_root_slots
        .iter()
        .chain(formation.writable_root_slots.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let writable = formation
        .writable_root_slots
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    declaration
        .authority
        .read_root_slots
        .retain(|slot| readable.contains(slot));
    declaration
        .authority
        .write_root_slots
        .retain(|slot| writable.contains(slot));
    declaration
        .authority
        .executable_root_slots
        .retain(|slot| readable.contains(slot));
    declaration
        .authority
        .optional_read_root_slots
        .retain(|slot| readable.contains(slot));
    declaration
        .authority
        .optional_write_root_slots
        .retain(|slot| writable.contains(slot));
    declaration
        .authority
        .optional_executable_root_slots
        .retain(|slot| readable.contains(slot));
    declaration.authority.read_argument_paths = narrow_argument_paths(
        "read_argument_paths",
        &declaration.authority.read_argument_paths,
        &readable,
    )?;
    declaration.authority.write_argument_paths = narrow_argument_paths(
        "write_argument_paths",
        &declaration.authority.write_argument_paths,
        &writable,
    )?;
    declaration.authority.executable_argument_paths = narrow_argument_paths(
        "executable_argument_paths",
        &declaration.authority.executable_argument_paths,
        &readable,
    )?;
    declaration.authority.allow_network =
        declaration.authority.allow_network && formation.network_ceiling != NetworkCeiling::Denied;
    declaration
        .coordinate_root_slots
        .retain(|slot| readable.contains(slot));
    Ok(declaration)
}

fn narrow_process_authority_to_root_slots(
    authority: &ProcessAuthoritySpec,
    allowed: &BTreeSet<String>,
) -> Result<ProcessAuthoritySpec, String> {
    let keep = |set: &BTreeSet<String>| set.intersection(allowed).cloned().collect();
    Ok(ProcessAuthoritySpec {
        read_root_slots: keep(&authority.read_root_slots),
        write_root_slots: keep(&authority.write_root_slots),
        executable_root_slots: keep(&authority.executable_root_slots),
        optional_read_root_slots: keep(&authority.optional_read_root_slots),
        optional_write_root_slots: keep(&authority.optional_write_root_slots),
        optional_executable_root_slots: keep(&authority.optional_executable_root_slots),
        read_argument_paths: narrow_argument_paths(
            "read_argument_paths",
            &authority.read_argument_paths,
            allowed,
        )?,
        write_argument_paths: narrow_argument_paths(
            "write_argument_paths",
            &authority.write_argument_paths,
            allowed,
        )?,
        executable_argument_paths: narrow_argument_paths(
            "executable_argument_paths",
            &authority.executable_argument_paths,
            allowed,
        )?,
        ..authority.clone()
    })
}

fn narrow_argument_paths(
    field: &str,
    declarations: &[ProcessArgumentPathSpec],
    allowed: &BTreeSet<String>,
) -> Result<Vec<ProcessArgumentPathSpec>, String> {
    declarations
        .iter()
        .map(|declaration| {
            let within_root_slots = declaration
                .within_root_slots
                .intersection(allowed)
                .cloned()
                .collect::<BTreeSet<_>>();
            if within_root_slots.is_empty() {
                return Err(format!(
                    "worker-environment formation admits no root for {field} pointer {:?}",
                    declaration.pointer
                ));
            }
            Ok(ProcessArgumentPathSpec {
                pointer: declaration.pointer.clone(),
                within_root_slots,
                include_parent: declaration.include_parent,
            })
        })
        .collect()
}

fn effective_tool_descriptor(
    ctx: &InterfaceLiftContext,
    descriptor: ToolDescriptor,
) -> Result<ToolDescriptor, String> {
    if descriptor.interface == ctx.interface {
        return Ok(descriptor);
    }
    Err(format!(
        "tool '{}' belongs to exact interface '{}', not broker interface '{}'",
        descriptor.id, descriptor.interface, ctx.interface
    ))
}

fn load_interface_descriptor(
    interface_root: &Path,
    interface: &InterfaceId,
) -> Result<InterfaceDescriptor, String> {
    let path = interface_root
        .join(interface.as_str())
        .join("interface.json");
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "interface descriptor '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    let descriptor =
        octopus_descriptors::parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| {
            format!(
                "interface descriptor '{}' is malformed: {error}",
                path.display()
            )
        })?;
    if descriptor.id != *interface {
        return Err(format!(
            "interface descriptor '{}' declares '{}' instead of folder id '{}'",
            path.display(),
            descriptor.id,
            interface
        ));
    }
    Ok(descriptor)
}

fn interface_declared_env(
    ctx: &InterfaceLiftContext,
    pointers: &std::collections::BTreeSet<String>,
) -> Result<Vec<(String, std::ffi::OsString)>, String> {
    if pointers.is_empty() {
        return Ok(Vec::new());
    }
    let descriptor = serde_json::to_value(ctx.resolved_interface_descriptor()?)
        .map_err(|error| format!("resolved interface descriptor did not encode: {error}"))?;
    let mut names = BTreeSet::new();
    for pointer in pointers {
        let value = descriptor.pointer(pointer).ok_or_else(|| {
            format!(
                "tool process interface env pointer '{}' is absent from resolved interface '{}'",
                pointer,
                ctx.interface.as_str()
            )
        })?;
        let selected = match value {
            serde_json::Value::String(name) => vec![name.as_str()],
            serde_json::Value::Array(values) => values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    value.as_str().ok_or_else(|| {
                        format!(
                            "tool process interface env pointer '{}[{}]' does not select a string",
                            pointer, index
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => {
                return Err(format!(
                    "tool process interface env pointer '{}' must select a string or string array",
                    pointer
                ));
            }
        };
        for name in selected {
            let name = name.trim();
            if name.is_empty() {
                return Err(format!(
                    "tool process interface env pointer '{}' selects an empty name",
                    pointer
                ));
            }
            if is_reserved_tool_process_host_env(name) {
                return Err(format!(
                    "tool process interface env pointer '{}' selects reserved name '{}'",
                    pointer, name
                ));
            }
            names.insert(name.to_string());
        }
    }
    let mut selected = Vec::new();
    for name in names {
        if let Some(value) = std::env::var_os(&name) {
            selected.push((name, value));
        }
    }
    Ok(selected)
}

fn effective_tool_process_environment(
    ctx: &InterfaceLiftContext,
    declaration: &ToolProcessDeclaration,
) -> Result<BTreeMap<String, std::ffi::OsString>, String> {
    for name in declaration.inherit_env.iter().chain(declaration.env.keys()) {
        if is_reserved_tool_process_host_env(name) {
            return Err(format!(
                "tool process declaration selects reserved host env '{}'",
                name
            ));
        }
    }
    let mut environment = declaration
        .inherit_env
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (name.clone(), value)))
        .collect::<BTreeMap<_, _>>();
    for (name, value) in interface_declared_env(ctx, &declaration.inherit_env_from_interface)? {
        if declaration.env.contains_key(&name) {
            return Err(format!(
                "tool process env '{}' is both literal and selected from interface config",
                name
            ));
        }
        environment.insert(name, value);
    }
    for (name, value) in &declaration.env {
        environment.insert(name.clone(), std::ffi::OsString::from(value));
    }
    Ok(environment)
}

fn append_tool_process_identity_part(hasher: &mut blake3::Hasher, name: &[u8], value: &[u8]) {
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name);
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn tool_process_environment_hash(environment: &BTreeMap<String, std::ffi::OsString>) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"octopus.tool-process.effective-environment.v1\0");
    for (name, value) in environment {
        append_tool_process_identity_part(
            &mut hasher,
            name.as_bytes(),
            value.as_os_str().as_encoded_bytes(),
        );
    }
    hasher.finalize().to_hex().to_string()
}

fn invocation_authority_hash(
    authority: &ProcessAuthoritySpec,
    root_slots: &BTreeMap<String, PathBuf>,
    arguments: &serde_json::Value,
) -> Result<String, ProcessInvocationAuthorityError> {
    let paths = authority.resolve_invocation_paths(root_slots, arguments)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"octopus.tool-process.invocation-authority.v1\0");
    for (class, values) in [
        (b"read".as_slice(), &paths.readable),
        (b"write".as_slice(), &paths.writable),
        (b"execute".as_slice(), &paths.executable),
    ] {
        for path in values {
            append_tool_process_identity_part(
                &mut hasher,
                class,
                path.as_os_str().as_encoded_bytes(),
            );
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn empty_invocation_authority_hash() -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"octopus.tool-process.invocation-authority.v1\0");
    hasher.finalize().to_hex().to_string()
}

#[cfg(unix)]
fn materialize_owner_private_descendant(
    root: &Path,
    components: &[String],
) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;

    let canonical_root = std::fs::canonicalize(root).map_err(|error| {
        format!(
            "tool process host-private root '{}' cannot be canonicalized: {error}",
            root.display()
        )
    })?;
    let root_name = CString::new(canonical_root.as_os_str().as_bytes()).map_err(|_| {
        format!(
            "tool process host-private root '{}' contains NUL",
            canonical_root.display()
        )
    })?;
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(format!(
            "tool process host-private root '{}' cannot be opened: {}",
            canonical_root.display(),
            std::io::Error::last_os_error()
        ));
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    let mut path = canonical_root;
    for component in components {
        if component.is_empty()
            || Path::new(component).components().count() != 1
            || !matches!(
                Path::new(component).components().next(),
                Some(std::path::Component::Normal(_))
            )
        {
            return Err(format!(
                "tool process host-private component {component:?} is invalid"
            ));
        }
        let component_name = CString::new(component.as_bytes()).map_err(|_| {
            format!("tool process host-private component {component:?} contains NUL")
        })?;
        let created =
            unsafe { libc::mkdirat(directory.as_raw_fd(), component_name.as_ptr(), 0o700) };
        if created < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(format!(
                    "tool process host-private directory component {component:?} cannot be created: {error}"
                ));
            }
        }
        let opened = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if opened < 0 {
            return Err(format!(
                "tool process host-private directory component {component:?} cannot be opened: {}",
                std::io::Error::last_os_error()
            ));
        }
        let opened = unsafe { OwnedFd::from_raw_fd(opened) };
        let metadata = std::fs::File::from(opened.try_clone().map_err(|error| {
            format!(
                "tool process host-private directory component {component:?} descriptor cannot be cloned: {error}"
            )
        })?)
        .metadata()
        .map_err(|error| {
            format!(
                "tool process host-private directory component {component:?} cannot be inspected: {error}"
            )
        })?;
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "tool process host-private directory component {component:?} is not owner-controlled"
            ));
        }
        if unsafe { libc::fchmod(opened.as_raw_fd(), 0o700) } < 0 {
            return Err(format!(
                "tool process host-private directory component {component:?} cannot be made owner-only: {}",
                std::io::Error::last_os_error()
            ));
        }
        path.push(component);
        directory = opened;
    }
    Ok(path)
}

#[cfg(not(unix))]
fn materialize_owner_private_descendant(
    root: &Path,
    components: &[String],
) -> Result<PathBuf, String> {
    let canonical_root = std::fs::canonicalize(root).map_err(|error| {
        format!(
            "tool process host-private root '{}' cannot be canonicalized: {error}",
            root.display()
        )
    })?;
    let mut path = canonical_root.clone();
    for component in components {
        if component.is_empty()
            || Path::new(component).components().count() != 1
            || !matches!(
                Path::new(component).components().next(),
                Some(std::path::Component::Normal(_))
            )
        {
            return Err(format!(
                "tool process host-private component {component:?} is invalid"
            ));
        }
        path.push(component);
        ensure_private_directory(&path)?;
        let canonical = std::fs::canonicalize(&path).map_err(|error| {
            format!(
                "tool process host-private directory '{}' cannot be canonicalized: {error}",
                path.display()
            )
        })?;
        if !canonical.starts_with(&canonical_root) || canonical == canonical_root {
            return Err(format!(
                "tool process host-private directory '{}' escaped its root",
                path.display()
            ));
        }
        path = canonical;
    }
    Ok(path)
}

fn tool_process_host_private_root(ctx: &InterfaceLiftContext) -> Result<PathBuf, String> {
    let interface_var = ctx.interface_var_dir();
    materialize_owner_private_descendant(&interface_var, &[".tool-process-host-state".to_string()])
}

fn tool_process_interface_local_ipc_root(
    ctx: &InterfaceLiftContext,
) -> Result<Option<PathBuf>, String> {
    let Some(root) = ctx.root_slots.get(INTERFACE_LOCAL_IPC_ROOT_SLOT) else {
        return Ok(None);
    };
    if !root.is_absolute() {
        return Err(format!(
            "host-formed interface-local IPC root is not absolute: {}",
            root.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(root).map_err(|error| {
        format!(
            "host-formed interface-local IPC root '{}' cannot be inspected: {error}",
            root.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "host-formed interface-local IPC root '{}' is not a real directory",
            root.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(format!(
                "host-formed interface-local IPC root '{}' is not owner-only",
                root.display()
            ));
        }
    }
    let canonical = std::fs::canonicalize(root).map_err(|error| {
        format!(
            "host-formed interface-local IPC root '{}' cannot be canonicalized: {error}",
            root.display()
        )
    })?;
    if canonical != root.as_path() {
        return Err(format!(
            "host-formed interface-local IPC root '{}' is not canonical",
            root.display()
        ));
    }
    Ok(Some(canonical))
}

fn tool_process_generation_root(ctx: &InterfaceLiftContext) -> Result<PathBuf, String> {
    let root = tool_process_host_private_root(ctx)?;
    materialize_owner_private_descendant(&root, &["generations".to_string()])
}

pub fn tool_door_durable_state_dir(
    ctx: &InterfaceLiftContext,
    worker_environment_id: &WorkerEnvironmentId,
) -> Result<PathBuf, String> {
    let mut path = ctx.interface_var_dir();
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || std::fs::canonicalize(&path).map_err(|error| error.to_string())? != path
    {
        return Err("tool-door private state parent is not a canonical real directory".into());
    }
    for component in [
        ".tool-process-host-state".to_string(),
        "worker-environment-state".to_string(),
        blake3::hash(worker_environment_id.as_str().as_bytes())
            .to_hex()
            .to_string(),
    ] {
        path = validate_tool_process_durable_var_dir(path.join(component))?;
    }
    Ok(path)
}

fn materialize_tool_process_durable_dir(
    ctx: &InterfaceLiftContext,
    formation: Option<&WorkerEnvironmentFormation>,
) -> Result<Option<PathBuf>, String> {
    let Some(formation) = formation else {
        return Ok(None);
    };
    formation.validate().map_err(|error| error.to_string())?;
    let root = tool_process_host_private_root(ctx)?;
    let identity = blake3::hash(formation.worker_environment_id.as_str().as_bytes())
        .to_hex()
        .to_string();
    let path = materialize_owner_private_descendant(
        &root,
        &["worker-environment-state".to_string(), identity],
    )?;
    Ok(Some(validate_tool_process_durable_var_dir(path)?))
}

fn validate_tool_process_durable_var_dir(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "{TOOL_PROCESS_DURABLE_VAR_DIR_ENV} must be an absolute path"
        ));
    }
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "{TOOL_PROCESS_DURABLE_VAR_DIR_ENV} '{}' cannot be inspected: {error}",
            path.display()
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "{TOOL_PROCESS_DURABLE_VAR_DIR_ENV} '{}' is not a real directory",
            path.display()
        ));
    }
    let canonical = std::fs::canonicalize(&path).map_err(|error| {
        format!(
            "{TOOL_PROCESS_DURABLE_VAR_DIR_ENV} '{}' cannot be canonicalized: {error}",
            path.display()
        )
    })?;
    if canonical != path {
        return Err(format!(
            "{TOOL_PROCESS_DURABLE_VAR_DIR_ENV} '{}' is not canonical",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(format!(
                "{TOOL_PROCESS_DURABLE_VAR_DIR_ENV} '{}' is not owner-only",
                path.display()
            ));
        }
    }
    Ok(canonical)
}

#[cfg(not(unix))]
fn ensure_private_directory(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "tool process private directory '{}' is not a real directory",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(|error| {
                format!(
                    "tool process private directory '{}' cannot be created: {error}",
                    path.display()
                )
            })?;
        }
        Err(error) => {
            return Err(format!(
                "tool process private directory '{}' cannot be inspected: {error}",
                path.display()
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                format!(
                    "tool process private directory '{}' permissions cannot be set: {error}",
                    path.display()
                )
            },
        )?;
    }
    Ok(())
}

static PRIVATE_READ_SNAPSHOT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static TOOL_PROCESS_OWNER_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct MaterializedPrivateGeneration {
    state_dir: PathBuf,
    private_reads: BTreeMap<String, PathBuf>,
    exact_paths: Vec<PathBuf>,
}

#[derive(Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ToolProcessOwnerRecord {
    schema_version: u32,
    generation: String,
    formation_id: Option<String>,
    process_id: u32,
    process_group_id: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolProcessOwnerState {
    Locked,
    Quiesced,
    UnlockedProcessGroup,
}

#[cfg(unix)]
struct ToolProcessOwnerLease {
    file: std::fs::File,
}

#[cfg(not(unix))]
struct ToolProcessOwnerLease;

fn tool_process_generation_name(generation: &Path) -> Result<&str, String> {
    let name = generation
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "tool process generation has no UTF-8 identity".to_string())?;
    if name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "tool process generation '{}' has an invalid identity",
            generation.display()
        ));
    }
    Ok(name)
}

#[cfg(unix)]
fn try_acquire_tool_process_owner_lease(
    generation: &Path,
) -> Result<Option<ToolProcessOwnerLease>, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let lock_path = generation.join(TOOL_PROCESS_OWNER_LOCK_FILE);
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW);
    let file = options.open(&lock_path).map_err(|error| {
        format!(
            "tool process owner lock '{}' cannot be opened: {error}",
            lock_path.display()
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "tool process owner lock '{}' cannot be inspected: {error}",
            lock_path.display()
        )
    })?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(format!(
            "tool process owner lock '{}' is not an owner-controlled regular file",
            lock_path.display()
        ));
    }
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } < 0 {
        return Err(format!(
            "tool process owner lock '{}' permissions cannot be set: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if locked < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock
            || error
                .raw_os_error()
                .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
        {
            return Ok(None);
        }
        return Err(format!(
            "tool process owner lock '{}' cannot be acquired: {error}",
            lock_path.display()
        ));
    }
    file.sync_all().map_err(|error| {
        format!(
            "tool process owner lock '{}' cannot be synced: {error}",
            lock_path.display()
        )
    })?;
    sync_tool_process_owner_directory(generation)?;
    Ok(Some(ToolProcessOwnerLease { file }))
}

#[cfg(not(unix))]
fn try_acquire_tool_process_owner_lease(
    generation: &Path,
) -> Result<Option<ToolProcessOwnerLease>, String> {
    Err(format!(
        "tool process generation '{}' cannot prove process-group ownership on this platform",
        generation.display()
    ))
}

fn acquire_tool_process_owner_lease(generation: &Path) -> Result<ToolProcessOwnerLease, String> {
    let lease = try_acquire_tool_process_owner_lease(generation)?.ok_or_else(|| {
        format!(
            "tool process generation '{}' still has a live owner process group",
            generation.display()
        )
    })?;
    if let Some(record) = read_tool_process_owner_record(generation)?
        && process_group_exists(record.process_group_id)?
    {
        return Err(format!(
            "tool process generation '{}' has an unlocked but extant process group {}; refusing possible process-group reuse",
            generation.display(),
            record.process_group_id
        ));
    }
    Ok(lease)
}

#[cfg(unix)]
fn process_group_exists(process_group_id: i32) -> Result<bool, String> {
    if process_group_id <= 0 {
        return Err("tool process owner process_group_id must be positive".to_string());
    }
    let result = unsafe { libc::kill(-process_group_id, 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(format!(
            "tool process group {process_group_id} cannot be inspected: {error}"
        )),
    }
}

#[cfg(not(unix))]
fn process_group_exists(process_group_id: i32) -> Result<bool, String> {
    Err(format!(
        "tool process group {process_group_id} cannot be inspected on this platform"
    ))
}

#[cfg(unix)]
async fn quiesce_worker_environment_owner_generations(
    root: &Path,
    worker_environment_id: &WorkerEnvironmentId,
) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "tool process generation root '{}' cannot be inspected before cold release: {error}",
                root.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "tool process generation root '{}' is not a regular directory",
            root.display()
        ));
    }
    let root = std::fs::canonicalize(root).map_err(|error| {
        format!(
            "tool process generation root '{}' cannot be canonicalized: {error}",
            root.display()
        )
    })?;
    let entries = std::fs::read_dir(&root).map_err(|error| {
        format!(
            "tool process generation root '{}' cannot be read: {error}",
            root.display()
        )
    })?;
    let mut owned = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "tool process generation root '{}' contains an unreadable entry: {error}",
                root.display()
            )
        })?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "tool process generation '{}' cannot be inspected: {error}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "tool process generation '{}' is not a regular directory",
                path.display()
            ));
        }
        let Some(record) = read_tool_process_owner_record(&path)? else {
            continue;
        };
        if record.formation_id.as_deref() == Some(worker_environment_id.as_str()) {
            owned.push((path, record));
        }
    }
    owned.sort_by(|left, right| left.0.cmp(&right.0));
    for (generation, record) in owned {
        quiesce_tool_process_owner_generation(&generation, &record).await?;
        remove_tool_process_generation(&root, &generation)?;
    }
    Ok(())
}

#[cfg(not(unix))]
async fn quiesce_worker_environment_owner_generations(
    root: &Path,
    _worker_environment_id: &WorkerEnvironmentId,
) -> Result<(), String> {
    Err(format!(
        "tool process generation root '{}' cannot reconcile process groups on this platform",
        root.display()
    ))
}

#[cfg(unix)]
async fn quiesce_tool_process_owner_generation(
    generation: &Path,
    expected: &ToolProcessOwnerRecord,
) -> Result<(), String> {
    match tool_process_owner_state(generation, expected)? {
        ToolProcessOwnerState::Quiesced => return Ok(()),
        ToolProcessOwnerState::UnlockedProcessGroup => {
            if wait_for_owner_generation_quiescence(
                generation,
                expected,
                std::time::Duration::from_millis(TOOL_PROCESS_OWNER_TERM_GRACE_MS),
            )
            .await?
            {
                return Ok(());
            }
            return Err(format!(
                "tool process generation '{}' has an unlocked but extant process group {}; refusing possible process-group reuse",
                generation.display(),
                expected.process_group_id
            ));
        }
        ToolProcessOwnerState::Locked => {}
    }
    signal_tool_process_group(expected.process_group_id, libc::SIGTERM)?;
    if wait_for_owner_generation_quiescence(
        generation,
        expected,
        std::time::Duration::from_millis(TOOL_PROCESS_OWNER_TERM_GRACE_MS),
    )
    .await?
    {
        return Ok(());
    }
    match tool_process_owner_state(generation, expected)? {
        ToolProcessOwnerState::Quiesced => return Ok(()),
        ToolProcessOwnerState::UnlockedProcessGroup => {
            return Err(format!(
                "tool process generation '{}' has an unlocked but extant process group {}; refusing possible process-group reuse",
                generation.display(),
                expected.process_group_id
            ));
        }
        ToolProcessOwnerState::Locked => {}
    }
    signal_tool_process_group(expected.process_group_id, libc::SIGKILL)?;
    if wait_for_owner_generation_quiescence(
        generation,
        expected,
        std::time::Duration::from_millis(TOOL_PROCESS_OWNER_KILL_GRACE_MS),
    )
    .await?
    {
        Ok(())
    } else {
        Err(format!(
            "tool process generation '{}' retained process group {} after forced quiescence",
            generation.display(),
            expected.process_group_id
        ))
    }
}

#[cfg(unix)]
fn tool_process_owner_state(
    generation: &Path,
    expected: &ToolProcessOwnerRecord,
) -> Result<ToolProcessOwnerState, String> {
    let Some(lease) = try_acquire_tool_process_owner_lease(generation)? else {
        return Ok(ToolProcessOwnerState::Locked);
    };
    let observed = read_tool_process_owner_record(generation)?.ok_or_else(|| {
        format!(
            "tool process generation '{}' lost its owner record during quiescence",
            generation.display()
        )
    })?;
    if &observed != expected {
        return Err(format!(
            "tool process generation '{}' changed owner identity during quiescence",
            generation.display()
        ));
    }
    if process_group_exists(expected.process_group_id)? {
        drop(lease);
        return Ok(ToolProcessOwnerState::UnlockedProcessGroup);
    }
    drop(lease);
    Ok(ToolProcessOwnerState::Quiesced)
}

#[cfg(unix)]
async fn wait_for_owner_generation_quiescence(
    generation: &Path,
    expected: &ToolProcessOwnerRecord,
    timeout: std::time::Duration,
) -> Result<bool, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tool_process_owner_state(generation, expected)? == ToolProcessOwnerState::Quiesced {
            return Ok(true);
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(
            std::time::Duration::from_millis(TOOL_PROCESS_OWNER_POLL_MS)
                .min(deadline.saturating_duration_since(now)),
        )
        .await;
    }
}

#[cfg(unix)]
fn signal_tool_process_group(process_group_id: i32, signal: i32) -> Result<(), String> {
    if process_group_id <= 0 {
        return Err("tool process owner process_group_id must be positive".to_string());
    }
    let result = unsafe { libc::kill(-process_group_id, signal) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!(
            "tool process group {process_group_id} cannot receive signal {signal}: {error}"
        ))
    }
}

#[cfg(not(unix))]
fn signal_tool_process_group(process_group_id: i32, _signal: i32) -> Result<(), String> {
    Err(format!(
        "tool process group {process_group_id} cannot be signalled on this platform"
    ))
}

async fn quiesce_orphaned_generation_owner(
    ctx: &InterfaceLiftContext,
    process_key: &ToolProcessKey,
) -> Result<(), String> {
    let generation = tool_process_generation_root(ctx)?
        .join(tool_process_generation_key_from_process_key(process_key)?);
    quiesce_orphaned_generation(&generation).await
}

async fn quiesce_orphaned_generation_owners(
    root: &Path,
    active: &BTreeSet<String>,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "tool process generation root '{}' cannot be read: {error}",
                root.display()
            ));
        }
    };
    let mut orphaned = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "tool process generation root '{}' contains an unreadable entry: {error}",
                root.display()
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit()) && !active.contains(&name) {
            orphaned.push(entry.path());
        }
    }
    orphaned.sort();
    for generation in orphaned {
        quiesce_orphaned_generation(&generation).await?;
    }
    Ok(())
}

#[cfg(unix)]
async fn quiesce_orphaned_generation(generation: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(generation) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "tool process generation '{}' is not a regular directory",
                generation.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "tool process generation '{}' cannot be inspected: {error}",
                generation.display()
            ));
        }
    }
    let Some(record) = read_tool_process_owner_record(generation)? else {
        return Ok(());
    };
    quiesce_tool_process_owner_generation(generation, &record).await
}

#[cfg(not(unix))]
async fn quiesce_orphaned_generation(_generation: &Path) -> Result<(), String> {
    Ok(())
}

fn read_tool_process_owner_record(
    generation: &Path,
) -> Result<Option<ToolProcessOwnerRecord>, String> {
    let generation_name = tool_process_generation_name(generation)?;
    let path = generation.join(TOOL_PROCESS_OWNER_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "tool process owner record '{}' cannot be inspected: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "tool process owner record '{}' is not a regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(format!(
                "tool process owner record '{}' is not owner-only",
                path.display()
            ));
        }
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "tool process owner record '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    let record: ToolProcessOwnerRecord = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "tool process owner record '{}' is malformed: {error}",
            path.display()
        )
    })?;
    if record.schema_version != 1
        || record.generation != generation_name
        || record
            .formation_id
            .as_deref()
            .is_some_and(|id| WorkerEnvironmentId::parse(id).is_err())
        || record.process_id == 0
        || record.process_group_id <= 0
        || record.process_id != record.process_group_id as u32
    {
        return Err(format!(
            "tool process owner record '{}' violates its generation identity",
            path.display()
        ));
    }
    Ok(Some(record))
}

fn prepare_tool_process_owner_lease(generation: &Path) -> Result<ToolProcessOwnerLease, String> {
    let lease = acquire_tool_process_owner_lease(generation)?;
    let owner_path = generation.join(TOOL_PROCESS_OWNER_FILE);
    match std::fs::remove_file(&owner_path) {
        Ok(()) => sync_tool_process_owner_directory(generation)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "tool process owner record '{}' cannot be reset: {error}",
                owner_path.display()
            ));
        }
    }
    Ok(lease)
}

#[cfg(unix)]
fn configure_tool_process_owner_lease(
    command: &mut tokio::process::Command,
    lease: &ToolProcessOwnerLease,
) {
    let owner_fd = lease.file.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(owner_fd, libc::F_GETFD);
            if flags < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(owner_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_tool_process_owner_lease(
    _command: &mut tokio::process::Command,
    _lease: &ToolProcessOwnerLease,
) {
}

#[cfg(unix)]
fn publish_tool_process_owner(
    generation: &Path,
    process_id: u32,
    formation_id: Option<&str>,
) -> Result<(), String> {
    let process_group_id = i32::try_from(process_id)
        .map_err(|_| "tool process id exceeds Unix process-group range".to_string())?;
    let observed_group = unsafe { libc::getpgid(process_group_id) };
    if observed_group < 0 {
        return Err(format!(
            "tool process {process_id} process group cannot be inspected: {}",
            std::io::Error::last_os_error()
        ));
    }
    if observed_group != process_group_id {
        return Err(format!(
            "tool process {process_id} joined unexpected process group {observed_group}"
        ));
    }
    let generation_name = tool_process_generation_name(generation)?.to_string();
    let record = ToolProcessOwnerRecord {
        schema_version: 1,
        generation: generation_name,
        formation_id: formation_id.map(str::to_string),
        process_id,
        process_group_id,
    };
    write_tool_process_owner_record(generation, &record)
}

#[cfg(not(unix))]
fn publish_tool_process_owner(
    generation: &Path,
    _process_id: u32,
    _formation_id: Option<&str>,
) -> Result<(), String> {
    Err(format!(
        "tool process generation '{}' cannot publish process-group ownership on this platform",
        generation.display()
    ))
}

fn write_tool_process_owner_record(
    generation: &Path,
    record: &ToolProcessOwnerRecord,
) -> Result<(), String> {
    let path = generation.join(TOOL_PROCESS_OWNER_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(format!(
                "tool process owner record '{}' already exists",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "tool process owner record '{}' cannot be inspected before publication: {error}",
                path.display()
            ));
        }
    }
    let bytes = serde_json::to_vec(record)
        .map_err(|error| format!("tool process owner record cannot encode: {error}"))?;
    let sequence = TOOL_PROCESS_OWNER_TEMP_SEQUENCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| "tool process owner record sequence overflowed".to_string())?;
    let temporary = generation.join(format!(
        ".{TOOL_PROCESS_OWNER_FILE}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| -> Result<(), String> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temporary).map_err(|error| {
            format!(
                "tool process owner record temporary '{}' cannot be created: {error}",
                temporary.display()
            )
        })?;
        file.write_all(&bytes).map_err(|error| {
            format!(
                "tool process owner record temporary '{}' cannot be written: {error}",
                temporary.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "tool process owner record temporary '{}' cannot be synced: {error}",
                temporary.display()
            )
        })?;
        std::fs::rename(&temporary, &path).map_err(|error| {
            format!(
                "tool process owner record '{}' cannot be published: {error}",
                path.display()
            )
        })?;
        sync_tool_process_owner_directory(generation)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn sync_tool_process_owner_directory(generation: &Path) -> Result<(), String> {
    let directory = std::fs::File::open(generation).map_err(|error| {
        format!(
            "tool process generation '{}' cannot be opened for sync: {error}",
            generation.display()
        )
    })?;
    directory.sync_all().map_err(|error| {
        format!(
            "tool process generation '{}' cannot be synced: {error}",
            generation.display()
        )
    })
}

fn remove_tool_process_generation(root: &Path, generation: &Path) -> Result<(), String> {
    tool_process_generation_name(generation)?;
    if generation.parent() != Some(root) {
        return Err(format!(
            "tool process generation '{}' is outside its owned root '{}'",
            generation.display(),
            root.display()
        ));
    }
    match std::fs::symlink_metadata(generation) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let _owner_quiescence = acquire_tool_process_owner_lease(generation)?;
            if completed_output::retains_generation(generation)? { return Ok(()); }
            std::fs::remove_dir_all(generation).map_err(|error| {
                format!(
                    "tool process generation '{}' cannot be removed: {error}",
                    generation.display()
                )
            })?;
            let root_handle = std::fs::File::open(root).map_err(|error| {
                format!(
                    "tool process generation root '{}' cannot be opened for sync: {error}",
                    root.display()
                )
            })?;
            root_handle.sync_all().map_err(|error| {
                format!(
                    "tool process generation root '{}' cannot be synced: {error}",
                    root.display()
                )
            })
        }
        Ok(_) => Err(format!(
            "tool process generation '{}' is not a regular directory",
            generation.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "tool process generation '{}' cannot be inspected: {error}",
            generation.display()
        )),
    }
}

fn tool_process_generation_key(
    effective_declaration_hash: &str,
    formation: Option<&WorkerEnvironmentFormation>,
    private_read_snapshot_hash: &str,
    effective_environment_hash: &str,
    invocation_authority_hash: &str,
    execution_inputs_hash: &str,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(
        effective_declaration_hash,
        formation.map(|formed| formed.worker_environment_id.as_str()),
        private_read_snapshot_hash,
        effective_environment_hash,
        invocation_authority_hash,
        execution_inputs_hash,
    ))
    .map_err(|error| format!("tool process declaration did not encode: {error}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn tool_process_generation_key_from_process_key(key: &ToolProcessKey) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(
        &key.effective_declaration_hash,
        key.formation_id.as_ref().map(WorkerEnvironmentId::as_str),
        &key.private_read_snapshot_hash,
        &key.effective_environment_hash,
        &key.invocation_authority_hash,
        &key.execution_inputs_hash,
    ))
    .map_err(|error| format!("tool process key did not encode: {error}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn reconcile_tool_process_generations(
    root: &Path,
    active: &BTreeSet<String>,
) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "tool process generation root '{}' cannot be inspected: {error}",
                root.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "tool process generation root '{}' is not a regular directory",
            root.display()
        ));
    }
    let root = std::fs::canonicalize(root).map_err(|error| {
        format!(
            "tool process generation root '{}' cannot be canonicalized: {error}",
            root.display()
        )
    })?;
    let entries = std::fs::read_dir(&root).map_err(|error| {
        format!(
            "tool process generation root '{}' cannot be read: {error}",
            root.display()
        )
    })?;
    let mut stale = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "tool process generation root '{}' contains an unreadable entry: {error}",
                root.display()
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(|error| {
            format!(
                "tool process generation '{}' cannot be inspected: {error}",
                entry.path().display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "tool process generation '{}' is not a regular directory",
                entry.path().display()
            ));
        }
        if !active.contains(&name) {
            stale.push(entry.path());
        }
    }
    stale.sort();
    for generation in stale {
        remove_tool_process_generation(&root, &generation)?;
    }
    Ok(())
}

fn materialize_private_read_generation(
    interface_var_dir: &Path,
    process_root: &Path,
    generation: &str,
    declarations: &BTreeSet<ToolProcessPrivateReadSubpath>,
    max_bytes: u64,
    expected_snapshot_hash: &str,
) -> Result<MaterializedPrivateGeneration, String> {
    if generation.len() != 64
        || !generation.bytes().all(|byte| byte.is_ascii_hexdigit())
        || expected_snapshot_hash.len() != 64
        || !expected_snapshot_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("tool process private generation identity is malformed".to_string());
    }
    let process_root = std::fs::canonicalize(process_root).map_err(|error| {
        format!(
            "tool process generation root '{}' cannot be canonicalized: {error}",
            process_root.display()
        )
    })?;
    let published = process_root.join(generation);
    match std::fs::symlink_metadata(&published) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!(
                    "tool process generation '{}' is not a regular directory",
                    published.display()
                ));
            }
            return inspect_materialized_private_generation(
                &published,
                declarations,
                max_bytes,
                expected_snapshot_hash,
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "tool process generation '{}' cannot be inspected: {error}",
                published.display()
            ));
        }
    }
    let sequence = PRIVATE_READ_SNAPSHOT_TEMP_SEQUENCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| "tool process private generation sequence overflowed".to_string())?;
    let temporary = process_root.join(format!(
        ".{generation}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&temporary).map_err(|error| {
        format!(
            "tool process private generation '{}' cannot be created: {error}",
            temporary.display()
        )
    })?;
    let result = (|| -> Result<MaterializedPrivateGeneration, String> {
        let private_root = create_private_generation_subdir(&temporary, "private")?;
        create_private_generation_subdir(&temporary, "state")?;
        let (_, observed) = materialize_private_read_subpaths(
            interface_var_dir,
            &private_root,
            declarations,
            max_bytes,
        )?;
        if observed != expected_snapshot_hash {
            return Err("tool process private inputs changed during spawn".to_string());
        }
        match std::fs::rename(&temporary, &published) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    std::fs::symlink_metadata(&published),
                    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink()
                ) =>
            {
                std::fs::remove_dir_all(&temporary).map_err(|cleanup_error| {
                    format!(
                        "tool process private generation raced publication ({error}) and temp cleanup failed: {cleanup_error}"
                    )
                })?;
                return inspect_materialized_private_generation(
                    &published,
                    declarations,
                    max_bytes,
                    expected_snapshot_hash,
                );
            }
            Err(error) => {
                return Err(format!(
                    "tool process private generation '{}' cannot be published atomically: {error}",
                    published.display()
                ));
            }
        }
        let directory = std::fs::File::open(&process_root).map_err(|error| {
            format!(
                "tool process generation root '{}' cannot be opened for sync: {error}",
                process_root.display()
            )
        })?;
        directory.sync_all().map_err(|error| {
            format!(
                "tool process generation root '{}' cannot be synced: {error}",
                process_root.display()
            )
        })?;
        inspect_materialized_private_generation(
            &published,
            declarations,
            max_bytes,
            expected_snapshot_hash,
        )
    })();
    if result.is_err() && temporary.exists() {
        let _ = std::fs::remove_dir_all(&temporary);
    }
    result
}

fn create_private_generation_subdir(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = root.join(name);
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path).map_err(|error| {
        format!(
            "tool process private generation directory '{}' cannot be created: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

fn inspect_materialized_private_generation(
    published: &Path,
    declarations: &BTreeSet<ToolProcessPrivateReadSubpath>,
    max_bytes: u64,
    expected_snapshot_hash: &str,
) -> Result<MaterializedPrivateGeneration, String> {
    let private_root = inspect_private_generation_subdir(published, "private")?;
    let state_dir = inspect_private_generation_subdir(published, "state")?;
    let observed = private_read_snapshot_hash(&private_root, declarations, max_bytes)?;
    if observed != expected_snapshot_hash {
        return Err(format!(
            "tool process generation '{}' private snapshot does not match its identity",
            published.display()
        ));
    }
    let private_reads = declarations
        .iter()
        .map(|declaration| {
            (
                declaration.path.clone(),
                private_root.join(&declaration.path),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let exact_paths = private_reads.values().cloned().collect();
    Ok(MaterializedPrivateGeneration {
        state_dir,
        private_reads,
        exact_paths,
    })
}

fn inspect_private_generation_subdir(root: &Path, name: &str) -> Result<PathBuf, String> {
    let path = root.join(name);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "tool process private generation directory '{}' cannot be inspected: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "tool process private generation directory '{}' is not a regular directory",
            path.display()
        ));
    }
    std::fs::canonicalize(&path).map_err(|error| {
        format!(
            "tool process private generation directory '{}' cannot be canonicalized: {error}",
            path.display()
        )
    })
}

fn private_read_max_bytes(
    declarations: &BTreeSet<ToolProcessPrivateReadSubpath>,
    configured_max_bytes: u64,
) -> Result<u64, String> {
    if declarations.is_empty() {
        return Ok(0);
    }
    if configured_max_bytes == 0 || configured_max_bytes == u64::MAX {
        return Err("tool-door private-read ceiling must be finite and positive".to_string());
    }
    usize::try_from(configured_max_bytes)
        .map_err(|_| "tool-door private-read ceiling exceeds host address space".to_string())?;
    Ok(configured_max_bytes)
}

fn private_read_snapshot_hash(
    interface_var_dir: &Path,
    declarations: &BTreeSet<ToolProcessPrivateReadSubpath>,
    max_bytes: u64,
) -> Result<String, String> {
    let mut snapshot = blake3::Hasher::new();
    for declaration in declarations {
        usize::try_from(max_bytes).map_err(|_| {
            format!(
                "tool process private read path {:?} ceiling exceeds host address space",
                declaration.path
            )
        })?;
        let (digest, size) =
            hash_private_read_source(interface_var_dir, &declaration.path, max_bytes)?;
        append_private_read_snapshot_entry(&mut snapshot, declaration, max_bytes, &digest, size);
    }
    Ok(snapshot.finalize().to_hex().to_string())
}

fn materialize_private_read_subpaths(
    interface_var_dir: &Path,
    process_var_dir: &Path,
    declarations: &BTreeSet<ToolProcessPrivateReadSubpath>,
    max_bytes: u64,
) -> Result<(Vec<PathBuf>, String), String> {
    let mut paths = Vec::with_capacity(declarations.len());
    let mut snapshot = blake3::Hasher::new();
    for declaration in declarations {
        let mut source = open_private_read_source(interface_var_dir, &declaration.path)?;
        let (path, digest, size) = materialize_private_read_source(
            process_var_dir,
            &declaration.path,
            max_bytes,
            &mut source,
        )?;
        append_private_read_snapshot_entry(&mut snapshot, declaration, max_bytes, &digest, size);
        paths.push(path);
    }
    Ok((paths, snapshot.finalize().to_hex().to_string()))
}

fn append_private_read_snapshot_entry(
    snapshot: &mut blake3::Hasher,
    declaration: &ToolProcessPrivateReadSubpath,
    max_bytes: u64,
    digest: &blake3::Hash,
    size: u64,
) {
    snapshot.update(&(declaration.path.len() as u64).to_le_bytes());
    snapshot.update(declaration.path.as_bytes());
    snapshot.update(&max_bytes.to_le_bytes());
    snapshot.update(&size.to_le_bytes());
    snapshot.update(digest.as_bytes());
}

fn hash_private_read_source(
    interface_var_dir: &Path,
    declaration: &str,
    max_bytes: u64,
) -> Result<(blake3::Hash, u64), String> {
    let mut source = open_private_read_source(interface_var_dir, declaration)?;
    let before = source.metadata().map_err(|error| {
        format!(
            "tool process private read path {:?} cannot be inspected: {error}",
            declaration
        )
    })?;
    if !before.is_file() {
        return Err(format!(
            "tool process private read path {:?} is not a regular file",
            declaration
        ));
    }
    if before.len() > max_bytes {
        return Err(format!(
            "tool process private read path {:?} exceeds owning life-process ceiling of {} bytes",
            declaration, max_bytes
        ));
    }
    let mut digest = blake3::Hasher::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let remaining = max_bytes.saturating_sub(size);
        let read_limit = usize::try_from(remaining.saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = source.read(&mut buffer[..read_limit]).map_err(|error| {
            format!(
                "tool process private read path {:?} cannot be read: {error}",
                declaration
            )
        })?;
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            return Err(format!(
                "tool process private read path {:?} exceeds owning life-process ceiling of {} bytes",
                declaration, max_bytes
            ));
        }
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| "tool process private read size overflowed".to_string())?;
        digest.update(&buffer[..read]);
    }
    let after = source.metadata().map_err(|error| {
        format!(
            "tool process private read path {:?} cannot be reinspected: {error}",
            declaration
        )
    })?;
    if before.len() != size || after.len() != size {
        return Err(format!(
            "tool process private read path {:?} changed while being hashed",
            declaration
        ));
    }
    Ok((digest.finalize(), size))
}

#[cfg(unix)]
fn private_read_components(declaration: &str) -> Result<Vec<OsString>, String> {
    let relative = Path::new(declaration);
    if relative.is_absolute() || relative.as_os_str().is_empty() {
        return Err(format!(
            "tool process private read path {:?} is not a relative path of normal components",
            declaration
        ));
    }
    relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(value) if !value.is_empty() => Ok(value.to_os_string()),
            _ => Err(format!(
                "tool process private read path {:?} is not a relative path of normal components",
                declaration
            )),
        })
        .collect()
}

#[cfg(unix)]
fn open_private_read_root(path: &Path, purpose: &str) -> Result<OwnedFd, String> {
    let root = std::fs::canonicalize(path).map_err(|error| {
        format!(
            "tool process {purpose} root '{}' cannot be canonicalized: {error}",
            path.display()
        )
    })?;
    let encoded = CString::new(root.as_os_str().as_bytes()).map_err(|_| {
        format!(
            "tool process {purpose} root '{}' contains NUL",
            root.display()
        )
    })?;
    let fd = unsafe {
        libc::open(
            encoded.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(format!(
            "tool process {purpose} root '{}' cannot be opened without following links: {}",
            root.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_private_read_source(
    interface_var_dir: &Path,
    declaration: &str,
) -> Result<std::fs::File, String> {
    let components = private_read_components(declaration)?;
    let mut directory = open_private_read_root(interface_var_dir, "private source")?;
    for (index, component) in components.iter().enumerate() {
        let component = CString::new(component.as_bytes()).map_err(|_| {
            format!(
                "tool process private read path {:?} contains NUL",
                declaration
            )
        })?;
        let leaf = index + 1 == components.len();
        let flags = if leaf {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
        };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 {
            return Err(format!(
                "tool process private read path {:?} cannot be opened without following links: {}",
                declaration,
                std::io::Error::last_os_error()
            ));
        }
        let opened = unsafe { OwnedFd::from_raw_fd(fd) };
        if leaf {
            let file = std::fs::File::from(opened);
            if !file
                .metadata()
                .map_err(|error| {
                    format!(
                        "tool process private read path {:?} cannot be inspected: {error}",
                        declaration
                    )
                })?
                .is_file()
            {
                return Err(format!(
                    "tool process private read path {:?} is not a regular file",
                    declaration
                ));
            }
            return Ok(file);
        }
        directory = opened;
    }
    Err(format!(
        "tool process private read path {:?} is empty",
        declaration
    ))
}

#[cfg(not(unix))]
fn open_private_read_source(
    _interface_var_dir: &Path,
    declaration: &str,
) -> Result<std::fs::File, String> {
    Err(format!(
        "tool process private read path {:?} has no no-follow host implementation on this platform",
        declaration
    ))
}

#[cfg(unix)]
fn materialize_private_read_source(
    process_var_dir: &Path,
    declaration: &str,
    max_bytes: u64,
    source: &mut std::fs::File,
) -> Result<(PathBuf, blake3::Hash, u64), String> {
    let components = private_read_components(declaration)?;
    let root = std::fs::canonicalize(process_var_dir).map_err(|error| {
        format!(
            "tool process private destination root '{}' cannot be canonicalized: {error}",
            process_var_dir.display()
        )
    })?;
    let mut directory = open_private_read_root(&root, "private destination")?;
    for component in &components[..components.len() - 1] {
        let component = CString::new(component.as_bytes()).map_err(|_| {
            format!(
                "tool process private read path {:?} contains NUL",
                declaration
            )
        })?;
        let created = unsafe { libc::mkdirat(directory.as_raw_fd(), component.as_ptr(), 0o700) };
        if created < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(format!(
                "tool process private destination for {:?} cannot create a directory: {}",
                declaration,
                std::io::Error::last_os_error()
            ));
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(format!(
                "tool process private destination for {:?} crosses an invalid directory: {}",
                declaration,
                std::io::Error::last_os_error()
            ));
        }
        let opened = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::fchmod(opened.as_raw_fd(), 0o700) } < 0 {
            return Err(format!(
                "tool process private destination for {:?} cannot constrain a directory: {}",
                declaration,
                std::io::Error::last_os_error()
            ));
        }
        directory = opened;
    }
    let leaf = CString::new(components.last().unwrap().as_bytes()).map_err(|_| {
        format!(
            "tool process private read path {:?} contains NUL",
            declaration
        )
    })?;
    let sequence = PRIVATE_READ_SNAPSHOT_TEMP_SEQUENCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| "tool process private snapshot sequence overflowed".to_string())?;
    let temporary_name = format!(
        ".octopus-private-read.{}.{}.tmp",
        std::process::id(),
        sequence
    );
    let temporary = CString::new(temporary_name.as_bytes())
        .map_err(|_| "tool process private snapshot temp name contains NUL".to_string())?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(format!(
            "tool process private snapshot for {:?} cannot be created: {}",
            declaration,
            std::io::Error::last_os_error()
        ));
    }
    let opened = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut destination = std::fs::File::from(opened);
    let result = (|| -> Result<(blake3::Hash, u64), String> {
        if unsafe { libc::fchmod(destination.as_raw_fd(), 0o600) } < 0 {
            return Err(format!(
                "tool process private snapshot for {:?} cannot set mode 0600: {}",
                declaration,
                std::io::Error::last_os_error()
            ));
        }
        let before = source.metadata().map_err(|error| {
            format!(
                "tool process private read path {:?} cannot be inspected: {error}",
                declaration
            )
        })?;
        if !before.is_file() {
            return Err(format!(
                "tool process private read path {:?} is not a regular file",
                declaration
            ));
        }
        if before.len() > max_bytes {
            return Err(format!(
                "tool process private read path {:?} exceeds owning life-process ceiling of {} bytes",
                declaration, max_bytes
            ));
        }
        let mut digest = blake3::Hasher::new();
        let mut size = 0_u64;
        let mut buffer = [0_u8; 8192];
        loop {
            let remaining = max_bytes.saturating_sub(size);
            let read_limit = usize::try_from(remaining.saturating_add(1))
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let read = source.read(&mut buffer[..read_limit]).map_err(|error| {
                format!(
                    "tool process private read path {:?} cannot be read: {error}",
                    declaration
                )
            })?;
            if read == 0 {
                break;
            }
            if read as u64 > remaining {
                return Err(format!(
                    "tool process private read path {:?} exceeds owning life-process ceiling of {} bytes",
                    declaration, max_bytes
                ));
            }
            size = size
                .checked_add(read as u64)
                .ok_or_else(|| "tool process private read size overflowed".to_string())?;
            digest.update(&buffer[..read]);
            std::io::Write::write_all(&mut destination, &buffer[..read]).map_err(|error| {
                format!(
                    "tool process private snapshot for {:?} cannot be written: {error}",
                    declaration
                )
            })?;
        }
        let after = source.metadata().map_err(|error| {
            format!(
                "tool process private read path {:?} cannot be reinspected: {error}",
                declaration
            )
        })?;
        if before.len() != size || after.len() != size {
            return Err(format!(
                "tool process private read path {:?} changed while being materialized",
                declaration
            ));
        }
        destination.sync_all().map_err(|error| {
            format!(
                "tool process private snapshot for {:?} cannot be synced: {error}",
                declaration
            )
        })?;
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                directory.as_raw_fd(),
                leaf.as_ptr(),
            )
        } < 0
        {
            return Err(format!(
                "tool process private snapshot for {:?} cannot be installed atomically: {}",
                declaration,
                std::io::Error::last_os_error()
            ));
        }
        if unsafe { libc::fsync(directory.as_raw_fd()) } < 0 {
            return Err(format!(
                "tool process private snapshot directory for {:?} cannot be synced: {}",
                declaration,
                std::io::Error::last_os_error()
            ));
        }
        Ok((digest.finalize(), size))
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0);
        }
    }
    let (digest, size) = result?;
    Ok((root.join(declaration), digest, size))
}

#[cfg(not(unix))]
fn materialize_private_read_source(
    _process_var_dir: &Path,
    declaration: &str,
    _max_bytes: u64,
    _source: &mut std::fs::File,
) -> Result<(PathBuf, blake3::Hash, u64), String> {
    Err(format!(
        "tool process private read path {:?} has no atomic host materialization on this platform",
        declaration
    ))
}

#[derive(Debug)]
pub(crate) enum ToolProcessInvokeError {
    Unavailable(String),
    Capability(String),
}

#[derive(Clone)]
struct ToolResultTarget {
    tool: String,
    correlation_id: Option<String>,
}

impl From<&ToolCallArtifact> for ToolResultTarget {
    fn from(call: &ToolCallArtifact) -> Self {
        Self {
            tool: call.tool.clone(),
            correlation_id: call.correlation_id.clone(),
        }
    }
}

const TOOL_DOOR_EFFECT_JOURNAL_FILE: &str = "tool-door-effect-frontier.jsonl";
const TOOL_EFFECT_INDETERMINATE: &str = "tool_effect_indeterminate";
static TOOL_DOOR_COMPLETION_JOURNAL_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolDoorJournalState {
    Prepared,
    InFlight,
    Completed,
    Refused,
    Indeterminate,
}

impl ToolDoorJournalState {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Refused | Self::Indeterminate)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolDoorJournalReceipt {
    schema_version: u32,
    seq: u64,
    movement_digest: String,
    state: ToolDoorJournalState,
}

#[derive(Clone)]
struct ToolDoorJournalEntry {
    schema_version: u32,
    movement_digest: String,
    state: ToolDoorJournalState,
}

struct ToolDoorEffectJournal {
    path: PathBuf,
    entries: BTreeMap<u64, ToolDoorJournalEntry>,
    maximum_receipt_bytes: u64,
    source_sha256: Option<String>,
}

fn read_bounded_tool_door_receipt<R: std::io::BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    maximum_bytes: usize,
) -> Result<usize, String> {
    buffer.clear();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("tool-door completion journal read failed: {error}"))?;
        if available.is_empty() {
            return Ok(buffer.len());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if take > maximum_bytes.saturating_sub(buffer.len()) {
            return Err(format!(
                "tool-door completion journal receipt exceeds declared ceiling of {maximum_bytes} bytes"
            ));
        }
        buffer.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(buffer.len());
        }
    }
}

impl ToolDoorEffectJournal {
    fn open(path: PathBuf, committed_seq: u64, maximum_receipt_bytes: u64) -> Result<Self, String> {
        Self::load(path, Some(committed_seq), maximum_receipt_bytes)
    }

    fn read_only(path: PathBuf, maximum_receipt_bytes: u64) -> Result<Self, String> {
        Self::load(path, None, maximum_receipt_bytes)
    }

    fn load(path: PathBuf, committed_seq: Option<u64>, maximum_receipt_bytes: u64) -> Result<Self, String> {
        if maximum_receipt_bytes == 0 {
            return Err("tool-door receipt ceiling must be positive".to_string());
        }
        let maximum_receipt_bytes = usize::try_from(maximum_receipt_bytes)
            .map_err(|_| "tool-door receipt ceiling exceeds host address space".to_string())?;
        let mut entries = BTreeMap::new();
        let mut source_sha256 = None;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                validate_tool_door_journal_metadata(&path, &metadata)?;
                let mut options = std::fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW);
                }
                let file = options.open(&path).map_err(|error| {
                    format!(
                        "tool-door completion journal '{}' cannot be opened: {error}",
                        path.display()
                    )
                })?;
                validate_tool_door_journal_metadata(
                    &path,
                    &file.metadata().map_err(|error| {
                        format!(
                            "tool-door completion journal '{}' metadata cannot be read: {error}",
                            path.display()
                        )
                    })?,
                )?;
                use sha2::Digest;
                let mut hasher = committed_seq.is_none().then(sha2::Sha256::new);
                let mut reader = std::io::BufReader::new(file);
                let mut line = Vec::new();
                let mut index = 0_u64;
                loop {
                    line.clear();
                    let read = read_bounded_tool_door_receipt(
                        &mut reader,
                        &mut line,
                        maximum_receipt_bytes,
                    )?;
                    if read == 0 {
                        break;
                    }
                    if let Some(hasher) = &mut hasher { hasher.update(&line); }
                    index = index
                        .checked_add(1)
                        .ok_or_else(|| "tool-door receipt line count overflowed".to_string())?;
                    let Some(line) = line.strip_suffix(b"\n") else {
                        return Err(format!(
                            "tool-door completion journal '{}' ends with an incomplete receipt",
                            path.display()
                        ));
                    };
                    let line = line.strip_suffix(b"\r").unwrap_or(line);
                    if line.is_empty() {
                        return Err(format!(
                            "tool-door completion journal '{}' line {} is empty",
                            path.display(),
                            index
                        ));
                    }
                    let receipt: ToolDoorJournalReceipt =
                        serde_json::from_slice(line).map_err(|error| {
                            format!(
                                "tool-door completion journal '{}' line {} is malformed: {error}",
                                path.display(),
                                index
                            )
                        })?;
                    validate_tool_door_journal_receipt(&receipt)?;
                    apply_tool_door_journal_receipt(&mut entries, &receipt)?;
                }
                source_sha256 = hasher.map(|hasher| format!("{:x}", hasher.finalize()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "tool-door completion journal '{}' cannot be inspected: {error}",
                    path.display()
                ));
            }
        }
        let journal = Self {
            path,
            entries,
            maximum_receipt_bytes: maximum_receipt_bytes as u64,
            source_sha256,
        };
        Ok(journal)
    }

    fn state(
        &self,
        seq: u64,
        movement_digest: &str,
    ) -> Result<Option<ToolDoorJournalState>, String> {
        match self.entries.get(&seq) {
            Some(entry) if entry.movement_digest == movement_digest => Ok(Some(entry.state)),
            Some(_) => Err(format!(
                "tool-door completion journal seq {seq} does not match the current movement"
            )),
            None => Ok(None),
        }
    }

    fn prepare(&mut self, seq: u64, movement_digest: &str) -> Result<(), String> {
        self.transition(seq, movement_digest, ToolDoorJournalState::Prepared)
    }

    fn begin(&mut self, seq: u64, movement_digest: &str) -> Result<(), String> {
        self.transition(seq, movement_digest, ToolDoorJournalState::InFlight)
    }

    fn finish(
        &mut self,
        seq: u64,
        movement_digest: &str,
        state: ToolDoorJournalState,
    ) -> Result<(), String> {
        if !state.is_terminal() {
            return Err("tool-door journal finish requires a terminal state".to_string());
        }
        self.transition(seq, movement_digest, state)
    }

    fn transition(
        &mut self,
        seq: u64,
        movement_digest: &str,
        state: ToolDoorJournalState,
    ) -> Result<(), String> {
        if self.state(seq, movement_digest)? == Some(state) {
            return Ok(());
        }
        let receipt = ToolDoorJournalReceipt {
            schema_version: 3,
            seq,
            movement_digest: movement_digest.to_string(),
            state,
        };
        validate_tool_door_journal_receipt(&receipt)?;
        validate_tool_door_journal_transition(self.entries.get(&seq), &receipt)?;
        let mut bytes = serde_json::to_vec(&receipt)
            .map_err(|error| format!("tool-door journal receipt cannot serialize: {error}"))?;
        bytes.push(b'\n');
        self.append(&bytes)?;
        self.entries.insert(seq, ToolDoorJournalEntry { schema_version: receipt.schema_version, movement_digest: movement_digest.to_string(), state });
        Ok(())
    }

    fn prune_before(&mut self, committed_seq: u64) -> Result<(), String> {
        completed_output::prune_before(self.path.parent().ok_or("tool-door journal has no owner")?, committed_seq, self.maximum_receipt_bytes)?;
        Ok(())
    }

    fn append(&self, bytes: &[u8]) -> Result<(), String> {
        let parent = self.path.parent().ok_or_else(|| {
            "tool-door completion journal path has no parent directory".to_string()
        })?;
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "tool-door completion journal directory '{}' cannot be created: {error}",
                parent.display()
            )
        })?;
        if bytes.len() as u64 > self.maximum_receipt_bytes {
            return Err(format!(
                "tool-door completion journal receipt for '{}' exceeds declared ceiling of {} bytes",
                self.path.display(),
                self.maximum_receipt_bytes
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&self.path).map_err(|error| {
            format!(
                "tool-door completion journal '{}' cannot be opened for append: {error}",
                self.path.display()
            )
        })?;
        validate_tool_door_journal_metadata(
            &self.path,
            &file.metadata().map_err(|error| {
                format!(
                    "tool-door completion journal '{}' metadata cannot be read: {error}",
                    self.path.display()
                )
            })?,
        )?;
        use std::io::Write;
        file.write_all(bytes).map_err(|error| {
            format!(
                "tool-door completion journal '{}' cannot append receipt: {error}",
                self.path.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "tool-door completion journal '{}' cannot sync receipt: {error}",
                self.path.display()
            )
        })?;
        sync_tool_door_journal_parent(parent)
    }

}

fn validate_tool_door_journal_receipt(receipt: &ToolDoorJournalReceipt) -> Result<(), String> {
    if !matches!(receipt.schema_version, 2 | 3) {
        return Err(format!(
            "tool-door journal receipt has unsupported schema_version {}",
            receipt.schema_version
        ));
    }
    if receipt.movement_digest.len() != 64
        || !receipt
            .movement_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!(
            "tool-door journal receipt seq {} has an invalid movement digest",
            receipt.seq
        ));
    }
    Ok(())
}

fn apply_tool_door_journal_receipt(
    entries: &mut BTreeMap<u64, ToolDoorJournalEntry>,
    receipt: &ToolDoorJournalReceipt,
) -> Result<(), String> {
    validate_tool_door_journal_transition(entries.get(&receipt.seq), receipt)?;
    entries.insert(receipt.seq, ToolDoorJournalEntry {
        schema_version: receipt.schema_version,
        movement_digest: receipt.movement_digest.clone(), state: receipt.state,
    });
    Ok(())
}

fn validate_tool_door_journal_transition(
    current: Option<&ToolDoorJournalEntry>,
    receipt: &ToolDoorJournalReceipt,
) -> Result<(), String> {
    if current.is_some_and(|entry| entry.schema_version > receipt.schema_version) {
        return Err(format!("tool-door completion journal seq {} regresses its evidence schema", receipt.seq));
    }
    if let Some(current) = current
        && current.movement_digest != receipt.movement_digest
    {
        return Err(format!(
            "tool-door completion journal seq {} carries conflicting movement digests",
            receipt.seq
        ));
    }
    let previous = current.map(|entry| entry.state);
    if previous == Some(receipt.state) {
        return Ok(());
    }
    let valid = matches!(
        (previous, receipt.state),
        (None, ToolDoorJournalState::Prepared)
            | (
                Some(ToolDoorJournalState::Prepared),
                ToolDoorJournalState::InFlight
            )
            | (
                Some(ToolDoorJournalState::Prepared),
                ToolDoorJournalState::Completed
            )
            | (
                Some(ToolDoorJournalState::Prepared),
                ToolDoorJournalState::Refused
            )
            | (
                Some(ToolDoorJournalState::InFlight),
                ToolDoorJournalState::Completed
            )
            | (
                Some(ToolDoorJournalState::InFlight),
                ToolDoorJournalState::Refused
            )
            | (
                Some(ToolDoorJournalState::InFlight),
                ToolDoorJournalState::Indeterminate
            )
    );
    if !valid {
        return Err(format!(
            "tool-door completion journal seq {} has invalid transition {:?} -> {:?}",
            receipt.seq, previous, receipt.state
        ));
    }
    Ok(())
}

fn sync_tool_door_journal_parent(parent: &Path) -> Result<(), String> {
    let directory = std::fs::File::open(parent).map_err(|error| {
        format!(
            "tool-door completion journal directory '{}' cannot be opened: {error}",
            parent.display()
        )
    })?;
    directory.sync_all().map_err(|error| {
        format!(
            "tool-door completion journal directory '{}' cannot be synced: {error}",
            parent.display()
        )
    })
}

fn validate_tool_door_journal_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!(
            "tool-door completion journal '{}' must be a regular non-symlink file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(format!(
                "tool-door completion journal '{}' must be owner-only with mode 0600",
                path.display()
            ));
        }
    }
    Ok(())
}



fn create_tool_door_journal_temp(parent: &Path) -> Result<(PathBuf, std::fs::File), String> {
    for _ in 0..1024 {
        let sequence = TOOL_DOOR_COMPLETION_JOURNAL_TEMP_SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| "tool-door completion journal temp sequence overflowed".to_string())?;
        let temp = parent.join(format!(
            ".{TOOL_DOOR_EFFECT_JOURNAL_FILE}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        match options.open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "tool-door completion journal temp '{}' cannot be created: {error}",
                    temp.display()
                ));
            }
        }
    }
    Err("tool-door completion journal cannot allocate an atomic temp file".to_string())
}

enum ToolDoorAction {
    Complete(ToolDoorJournalState),
    AwaitingResults,
    RecoverIndeterminate,
    Recover {
        call: ToolCallArtifact,
        envelope: InterfaceInvocationEnvelope,
        captured: ToolProcessEnvelope,
    },
    Failure {
        class: &'static str,
        details: serde_json::Value,
        target: Option<ToolResultTarget>,
    },
    Invoke {
        call: ToolCallArtifact,
        envelope: InterfaceInvocationEnvelope,
        captured: ToolProcessEnvelope,
    },
    Release {
        request: WorkerEnvironmentReleaseRequest,
        envelope: InterfaceInvocationEnvelope,
    },
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerEnvironmentReleaseRequest {
    worker_environment_id: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolDoorRowState {
    AwaitingResults,
    InFlight,
    Completed,
}

struct ToolDoorPendingRow {
    own: OwnOutboundMovement,
    movement_digest: String,
    state: ToolDoorRowState,
}

struct ToolDoorCompletion {
    seq: u64,
    movement_digest: String,
    result: Result<ToolDoorJournalState, String>,
    primary_input: Option<ToolProcessPrimaryInput>,
}

pub type ToolDoorArtifactDecoder = fn(
    &InterfaceLiftContext,
    &InterfaceInvocationEnvelope,
    ToolDoorArtifactDecodePurpose,
) -> Result<Option<ToolCallArtifact>, ToolDoorArtifactDecodeError>;

#[derive(Debug)]
pub enum ToolDoorArtifactDecodeError {
    Invalid(String),
    AwaitingResults { call_sequences: Vec<u64> },
}

impl From<String> for ToolDoorArtifactDecodeError {
    fn from(error: String) -> Self { Self::Invalid(error) }
}

impl From<&str> for ToolDoorArtifactDecodeError {
    fn from(error: &str) -> Self { Self::Invalid(error.to_string()) }
}

impl std::fmt::Display for ToolDoorArtifactDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => formatter.write_str(error),
            Self::AwaitingResults { call_sequences } =>
                write!(formatter, "tool artifact awaits checked results for calls {call_sequences:?}"),
        }
    }
}

impl ToolDoorArtifactDecodeError {
    fn into_action(self, purpose: ToolDoorArtifactDecodePurpose) -> ToolDoorAction {
        let error = match self {
            Self::AwaitingResults { call_sequences }
                if purpose == ToolDoorArtifactDecodePurpose::Dispatch
                    && !call_sequences.is_empty()
                    && call_sequences.windows(2).all(|pair| pair[0] < pair[1]) =>
                return ToolDoorAction::AwaitingResults,
            Self::AwaitingResults { call_sequences } => format!(
                "tool artifact cannot await results with purpose {purpose:?} and call sequences {call_sequences:?}"),
            Self::Invalid(error) => error,
        };
        ToolDoorAction::Failure {
            class: "invalid_tool_artifact",
            details: serde_json::json!({ "error": error }),
            target: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolDoorArtifactDecodePurpose {
    Dispatch,
    HistoricalIdentity,
}

pub fn default_tool_door_artifact_decoder(
    _ctx: &InterfaceLiftContext,
    envelope: &InterfaceInvocationEnvelope,
    _purpose: ToolDoorArtifactDecodePurpose,
) -> Result<Option<ToolCallArtifact>, ToolDoorArtifactDecodeError> {
    decode_tool_call(envelope).map_err(Into::into)
}

pub fn owned_tool_door_interface_life(owner: ToolDoorOwner) -> Result<InterfaceLiftFn, String> {
    owned_tool_door_interface_life_with_decoder(owner, default_tool_door_artifact_decoder)
}

pub fn owned_tool_door_interface_life_with_decoder(
    owner: ToolDoorOwner,
    decoder: ToolDoorArtifactDecoder,
) -> Result<InterfaceLiftFn, String> {
    owned_tool_door_interface_life_with_output(owner, decoder, None)
}

pub fn owned_tool_door_interface_life_with_output(
    owner: ToolDoorOwner,
    decoder: ToolDoorArtifactDecoder,
    output_callback: Option<ToolDoorOutputCallback>,
) -> Result<InterfaceLiftFn, String> {
    owner.validate()?;
    Ok(Arc::new(move |ctx| {
        Box::pin(tool_door_life(
            ctx,
            owner.clone(),
            decoder,
            output_callback.clone(),
        )) as InterfaceLifeFuture
    }))
}

async fn tool_door_life(
    ctx: InterfaceLiftContext,
    owner: ToolDoorOwner,
    decoder: ToolDoorArtifactDecoder,
    output_callback: Option<ToolDoorOutputCallback>,
) -> Result<(), String> {
    ctx.installation_root()?;
    let mut host = ToolProcessBrokerHost::new(owner.clone())?;
    host.output_callback = output_callback;
    let host = Arc::new(host);
    let result = tool_door_loop(&ctx, &owner, Arc::clone(&host), decoder).await;
    let detached = host.detach(&ctx).await;
    match (result, detached) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(format!("{first}; {second}")),
    }
}

async fn tool_door_loop(
    ctx: &InterfaceLiftContext,
    owner: &ToolDoorOwner,
    host: Arc<ToolProcessBrokerHost>,
    decoder: ToolDoorArtifactDecoder,
) -> Result<(), String> {
    let maximum_in_flight = owner.max_in_flight;
    let host_io_limits = ctx.required_host_io_limits()?;
    let mut reader = LedgerTailReader::open_named(&ctx, "tool-door")?;
    let mut journal = ToolDoorEffectJournal::open(
        ctx.interface_var_dir().join(TOOL_DOOR_EFFECT_JOURNAL_FILE),
        reader.next_seq(),
        host_io_limits.max_control_frame_bytes,
    )?;
    let mut rows = BTreeMap::<u64, ToolDoorPendingRow>::new();
    let mut tasks = tokio::task::JoinSet::<ToolDoorCompletion>::new();
    ctx.signal_ready()?;
    completed_output::recover_consumed_sources(ctx, host.output_callback.as_ref(), reader.next_seq()).await?;
    loop {
        if ctx.cancel.is_cancelled() {
            retire_tool_door_tasks(&mut tasks, &mut journal, &mut rows).await?;
            return Ok(());
        }
        advance_tool_door_frontier(&mut reader, &mut journal, &mut rows)?;
        let available = maximum_in_flight.checked_sub(rows.len()).ok_or_else(|| {
            format!(
                "tool-door in-flight window contains {} rows above its declared maximum {maximum_in_flight}",
                rows.len()
            )
        })?;
        let incoming = reader.read_new_uncommitted_limit(available)?;
        let filled_window = available > 0 && incoming.len() == available;
        let deferred = rows.iter().filter_map(|(&seq, row)|
            (row.state == ToolDoorRowState::AwaitingResults).then_some(seq)).collect::<Vec<_>>();
        let mut batch = Vec::with_capacity(deferred.len() + incoming.len());
        for seq in deferred {
            let row = rows.remove(&seq).ok_or_else(|| format!("deferred tool-door row {seq} disappeared"))?;
            if journal.state(seq, &row.movement_digest)? != Some(ToolDoorJournalState::Prepared) {
                return Err(format!("deferred tool-door row {seq} is not prepared"));
            }
            batch.push(row.own);
        }
        batch.extend(incoming);
        for own in batch {
            let seq = own.movement.seq.0;
            let movement_digest = tool_door_movement_digest(&own.movement)?;
            if rows.contains_key(&seq) {
                return Err(format!(
                    "tool-door reader produced duplicate uncommitted movement seq {seq}"
                ));
            }
            let recorded = journal.state(seq, &movement_digest)?;
            if recorded.is_none() {
                journal.prepare(seq, &movement_digest)?;
            }
            if recorded.is_some_and(ToolDoorJournalState::is_terminal) {
                rows.insert(
                    seq,
                    ToolDoorPendingRow {
                        own,
                        movement_digest,
                        state: ToolDoorRowState::Completed,
                    },
                );
                continue;
            }
            let action = match recorded {
                Some(ToolDoorJournalState::InFlight) => {
                    match prepare_tool_door_action(
                        ctx,
                        &reader,
                        &own,
                        decoder,
                        ToolDoorArtifactDecodePurpose::HistoricalIdentity,
                    ) {
                        ToolDoorAction::Invoke { call, envelope, captured } => {
                            if completed_output::CompletedOutputs::load(ctx, seq)?.is_some() {
                                ToolDoorAction::Recover { call, envelope, captured }
                            } else if let Some(state) = tool_door_admitted_result_state(
                                ctx,
                                &own.movement,
                                &call,
                                &envelope,
                                decoder,
                            )? {
                                ToolDoorAction::Complete(state)
                            } else {
                                ToolDoorAction::Recover { call, envelope, captured }
                            }
                        }
                        _ => ToolDoorAction::RecoverIndeterminate,
                    }
                }
                Some(ToolDoorJournalState::Prepared) | None => prepare_tool_door_action(
                    ctx,
                    &reader,
                    &own,
                    decoder,
                    ToolDoorArtifactDecodePurpose::Dispatch,
                ),
                Some(
                    ToolDoorJournalState::Completed
                    | ToolDoorJournalState::Refused
                    | ToolDoorJournalState::Indeterminate,
                ) => {
                    unreachable!("terminal journal states were handled above")
                }
            };
            let should_spawn = !matches!(&action, ToolDoorAction::Complete(_) | ToolDoorAction::AwaitingResults);
            if matches!(
                &action,
                ToolDoorAction::Invoke { .. } | ToolDoorAction::Release { .. }
            ) {
                journal.begin(seq, &movement_digest)?;
            }
            let state = match &action {
                ToolDoorAction::AwaitingResults => ToolDoorRowState::AwaitingResults,
                ToolDoorAction::Complete(state) => {
                    journal.finish(seq, &movement_digest, *state)?;
                    ToolDoorRowState::Completed
                }
                _ => ToolDoorRowState::InFlight,
            };
            let invocation_ctx = ctx.with_source_movement(own.movement.clone());
            rows.insert(
                seq,
                ToolDoorPendingRow {
                    own,
                    movement_digest: movement_digest.clone(),
                    state,
                },
            );
            if should_spawn {
                let ctx = invocation_ctx;
                let host = Arc::clone(&host);
                tasks.spawn(async move {
                    let mut primary_input = None;
                    let result = execute_tool_door_action(&ctx, &host, seq, action, &mut primary_input).await;
                    ToolDoorCompletion {
                        seq,
                        movement_digest,
                        result,
                        primary_input,
                    }
                });
            }
        }
        advance_tool_door_frontier(&mut reader, &mut journal, &mut rows)?;
        if filled_window && rows.len() < maximum_in_flight {
            tokio::task::yield_now().await;
            continue;
        }
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => {
                retire_tool_door_tasks(&mut tasks, &mut journal, &mut rows).await?;
                return Ok(());
            }
            completed = tasks.join_next(), if !tasks.is_empty() => {
                let completion = match completed {
                    Some(Ok(completion)) => completion,
                    Some(Err(error)) => {
                        retire_tool_door_tasks(&mut tasks, &mut journal, &mut rows).await?;
                        return Err(format!("tool-door invocation task failed: {error}"));
                    }
                    None => continue,
                };
                if let Some(error) = completion.result.as_ref().err().cloned() {
                    retire_tool_door_tasks(&mut tasks, &mut journal, &mut rows).await?;
                    return Err(error);
                }
                record_tool_door_completion(completion, &mut journal, &mut rows)?;
            }
            changed = reader.wait_for_change(&ctx.cancel) => {
                changed?;
            }
        }
    }
}

fn prepare_tool_door_action(
    ctx: &InterfaceLiftContext,
    reader: &LedgerTailReader,
    own: &OwnOutboundMovement,
    decoder: ToolDoorArtifactDecoder,
    purpose: ToolDoorArtifactDecodePurpose,
) -> ToolDoorAction {
    let envelope = match reader.envelope(own) {
        Ok(envelope) => envelope,
        Err(error) => {
            return ToolDoorAction::Failure {
                class: "poisoned_tool_delivery",
                details: serde_json::json!({ "error": error }),
                target: None,
            };
        }
    };
    match decode_worker_environment_release(&envelope) {
        Ok(Some(request)) => return ToolDoorAction::Release { request, envelope },
        Ok(None) => {}
        Err(error) => {
            return ToolDoorAction::Failure {
                class: "invalid_worker_environment_release_artifact",
                details: serde_json::json!({ "error": error }),
                target: None,
            };
        }
    }
    let decoder_context = ctx.with_source_movement(own.movement.clone());
    let call = match decoder(&decoder_context, &envelope, purpose) {
        Ok(Some(call)) => call,
        Ok(None) => return ToolDoorAction::Complete(ToolDoorJournalState::Completed),
        Err(error) => return error.into_action(purpose),
    };
    let target = ToolResultTarget::from(&call);
    let granted_tools = match envelope.granted_tool_names() {
        Ok(granted) => granted,
        Err(error) => {
            return ToolDoorAction::Failure {
                class: "malformed_tool_grant_metadata",
                details: serde_json::json!({
                    "field": "tools",
                    "error": error.to_string(),
                }),
                target: Some(target),
            };
        }
    };
    if !granted_tools.iter().any(|granted| granted == &call.tool) {
        return ToolDoorAction::Failure {
            class: "tool_not_granted",
            details: serde_json::json!({ "tool": call.tool }),
            target: Some(target),
        };
    }
    let hard_blocked_tools = match envelope.hard_blocked_tool_names() {
        Ok(blocked) => blocked,
        Err(error) => {
            return ToolDoorAction::Failure {
                class: "malformed_tool_grant_metadata",
                details: serde_json::json!({
                    "field": "hard_blocked_tools",
                    "error": error.to_string(),
                }),
                target: Some(target),
            };
        }
    };
    if hard_blocked_tools
        .iter()
        .any(|blocked| blocked == &call.tool)
    {
        return ToolDoorAction::Failure {
            class: "tool_hard_blocked",
            details: serde_json::json!({ "tool": call.tool }),
            target: Some(target),
        };
    }
    let captured = match ctx.required_host_io_limits().and_then(|limits| {
        ToolProcessEnvelope::capture(&envelope, own.movement.seq.0, &limits)
    }) {
        Ok(captured) => captured,
        Err(error) => return ToolDoorAction::Failure {
            class: "invalid_tool_process_input",
            details: serde_json::json!({ "error": error }),
            target: Some(target),
        },
    };
    ToolDoorAction::Invoke { call, envelope, captured }
}

async fn execute_tool_door_action(
    ctx: &InterfaceLiftContext,
    host: &ToolProcessBrokerHost,
    source_movement_seq: u64,
    action: ToolDoorAction,
    primary_input: &mut Option<ToolProcessPrimaryInput>,
) -> Result<ToolDoorJournalState, String> {
    match action {
        ToolDoorAction::Complete(state) => Ok(state),
        ToolDoorAction::AwaitingResults => Err("deferred tool artifact cannot execute before its checked results".into()),
        ToolDoorAction::RecoverIndeterminate => {
            hand_tool_effect_indeterminate(ctx, source_movement_seq, None, None, None).await
        }
        ToolDoorAction::Failure {
            class,
            details,
            target,
        } => {
            hand_tool_failure(
                ctx,
                source_movement_seq,
                class,
                details,
                target.as_ref(),
                ToolDoorJournalState::Completed,
                None,
            )
            .await
        }
        ToolDoorAction::Invoke { call, envelope, captured } => {
            execute_tool_invocation(ctx, host, source_movement_seq, call, envelope, captured, false, primary_input).await
        }
        ToolDoorAction::Recover { call, envelope, captured } => {
            execute_tool_invocation(ctx, host, source_movement_seq, call, envelope, captured, true, primary_input).await
        }
        ToolDoorAction::Release { request, envelope } => {
            execute_worker_environment_release(ctx, host, source_movement_seq, request, envelope)
                .await
        }
    }
}

async fn execute_worker_environment_release(
    ctx: &InterfaceLiftContext,
    host: &ToolProcessBrokerHost,
    source_movement_seq: u64,
    request: WorkerEnvironmentReleaseRequest,
    envelope: InterfaceInvocationEnvelope,
) -> Result<ToolDoorJournalState, String> {
    let worker_environment_id = match WorkerEnvironmentId::parse(&request.worker_environment_id) {
        Ok(id) => id,
        Err(error) => {
            return hand_tool_failure(
                ctx,
                source_movement_seq,
                "invalid_worker_environment_release_artifact",
                serde_json::json!({ "error": error.to_string() }),
                None,
                ToolDoorJournalState::Completed,
                None,
            )
            .await;
        }
    };
    match host
        .release_worker_environment(ctx, &worker_environment_id, &envelope, source_movement_seq)
        .await
    {
        Ok(formation) => {
            let artifact = serde_json::to_vec(&serde_json::json!({
                "worker_environment_id": formation.worker_environment_id.as_str(),
            }))
            .map_err(|error| format!("worker-environment release result did not encode: {error}"))
            .and_then(|payload| {
                InterfaceArtifactFile::write(
                    ctx.interface_var_dir()
                        .join("worker-environment-release-artifacts"),
                    "worker_environment.released",
                    &payload,
                )
                .map_err(|error| {
                    format!("worker-environment release result did not materialize: {error}")
                })
            });
            let artifact = match artifact {
                Ok(artifact) => artifact,
                Err(error) => {
                    return hand_tool_failure(
                        ctx,
                        source_movement_seq,
                        "worker_environment_release_result_failed",
                        serde_json::json!({
                            "worker_environment_id": formation.worker_environment_id.as_str(),
                            "error": error,
                        }),
                        None,
                        ToolDoorJournalState::Completed,
                        None,
                    )
                    .await;
                }
            };
            match ctx.hand_arrival(artifact, Vec::new()).await? {
                DeliveryVerdict::Admitted { .. } => Ok(ToolDoorJournalState::Completed),
                DeliveryVerdict::Refused { .. } => Ok(ToolDoorJournalState::Refused),
            }
        }
        Err(error) => {
            hand_tool_failure(
                ctx,
                source_movement_seq,
                "worker_environment_release_failed",
                serde_json::json!({
                    "worker_environment_id": worker_environment_id.as_str(),
                    "error": error,
                }),
                None,
                ToolDoorJournalState::Completed,
                None,
            )
            .await
        }
    }
}

async fn execute_tool_invocation(
    ctx: &InterfaceLiftContext,
    host: &ToolProcessBrokerHost,
    source_movement_seq: u64,
    call: ToolCallArtifact,
    envelope: InterfaceInvocationEnvelope,
    mut captured: ToolProcessEnvelope,
    interrupted_recovery: bool,
    primary_input: &mut Option<ToolProcessPrimaryInput>,
) -> Result<ToolDoorJournalState, String> {
    let target = ToolResultTarget::from(&call);
    let failure_output = match host.output_callback.as_ref() {
        Some(callback) => Some((callback, ToolDoorOutputSource {
            movement: ctx.source_movement(&envelope)?.clone(),
            envelope: envelope.clone(),
            formation: None,
        })),
        None => None,
    };
    captured.output_source = Some(ToolDoorOutputSource {
        movement: ctx.source_movement(&envelope)?.clone(),
        envelope: envelope.clone(),
        formation: None,
    });
    match host
        .invoke(
            ctx,
            target.tool.clone(),
            call.arguments,
            captured,
            interrupted_recovery,
        )
        .await
    {
        ToolProcessBrokerInvocation::Completed(retirement) => {
            *primary_input = retirement;
            Ok(ToolDoorJournalState::Completed)
        }
        ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Completed) => {
            Ok(ToolDoorJournalState::Completed)
        }
        ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Refused { .. }) => {
            Ok(ToolDoorJournalState::Refused)
        }
        ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::OutputPending { error }) => Err(error),
        ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::CapabilityFailed {
            error,
        }) => {
            hand_tool_failure(
                ctx,
                source_movement_seq,
                "tool_process_failure",
                serde_json::json!({ "tool": target.tool.clone(), "error": error }),
                Some(&target),
                ToolDoorJournalState::Completed,
                failure_output,
            )
            .await
        }
        ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Unavailable { error }) => {
            hand_tool_effect_indeterminate(
                ctx,
                source_movement_seq,
                Some(&target),
                Some((&target.tool, &error)),
                failure_output,
            )
            .await
        }
        ToolProcessBrokerInvocation::InterruptedEffectUnknown => {
            hand_tool_effect_indeterminate(ctx, source_movement_seq, Some(&target), None, failure_output).await
        }
    }
}

async fn hand_tool_effect_indeterminate(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
    target: Option<&ToolResultTarget>,
    failure: Option<(&str, &str)>,
    output: Option<(&ToolDoorOutputCallback, ToolDoorOutputSource)>,
) -> Result<ToolDoorJournalState, String> {
    let mut details = serde_json::json!({
        "source_movement_seq": source_movement_seq,
        "effect_state": "in_flight_outcome_unknown"
    });
    if let Some((tool, error)) = failure {
        let object = details
            .as_object_mut()
            .ok_or_else(|| "tool effect failure details are not an object".to_string())?;
        object.insert("tool".to_string(), serde_json::json!(tool));
        object.insert("error".to_string(), serde_json::json!(error));
        object.insert(
            "error_blake3".to_string(),
            serde_json::json!(blake3::hash(error.as_bytes()).to_hex().to_string()),
        );
        object.insert(
            "error_truncated".to_string(),
            serde_json::json!(false),
        );
    }
    hand_tool_failure(
        ctx,
        source_movement_seq,
        TOOL_EFFECT_INDETERMINATE,
        details,
        target,
        ToolDoorJournalState::Indeterminate,
        output,
    )
    .await
}

async fn hand_tool_failure(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
    failure_class: &str,
    details: serde_json::Value,
    target: Option<&ToolResultTarget>,
    admitted_state: ToolDoorJournalState,
    output: Option<(&ToolDoorOutputCallback, ToolDoorOutputSource)>,
) -> Result<ToolDoorJournalState, String> {
    let artifact = match target {
        Some(target) => {
            let error = bounded_tool_failure_error(source_movement_seq, failure_class, &details)?;
            ctx.tool_result_failure_artifact(&target.tool, target.correlation_id.as_deref(), error)?
        }
        None => ctx.failure_artifact(source_movement_seq, failure_class, details)?,
    };
    let verdict = match output {
        Some((callback, source)) => {
            let dispatch: ToolDoorOutputDispatch = Arc::new(|ctx, artifact| {
                Box::pin(async move { hand_arrival_at_current_source(&ctx, artifact).await })
            });
            hand_tool_process_output_with_dispatch(ctx, Some(&source), Some(callback), artifact, dispatch).await?
        }
        None => hand_arrival_at_current_source(ctx, artifact).await?,
    };
    match verdict {
        DeliveryVerdict::Admitted { .. } => Ok(admitted_state),
        DeliveryVerdict::Refused { .. } if admitted_state == ToolDoorJournalState::Indeterminate => Ok(admitted_state),
        DeliveryVerdict::Refused { .. } => Ok(ToolDoorJournalState::Refused),
    }
}

async fn hand_arrival_at_current_source(
    ctx: &InterfaceLiftContext,
    artifact: InterfaceArtifactFile,
) -> Result<DeliveryVerdict, String> {
    loop {
        let verdict = ctx.hand_arrival(artifact.clone(), Vec::new()).await?;
        if !verdict.awaits_current_source() {
            return Ok(verdict);
        }
        tokio::select! {
            _ = ctx.cancel.cancelled() => {
                return Err("tool failure result remains undelivered while admission is cancelled".into());
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
    }
}

fn bounded_tool_failure_error(
    source_movement_seq: u64,
    failure_class: &str,
    details: &serde_json::Value,
) -> Result<octopus_tool_handlers::ToolResultError, String> {
    const MAX_CHARS: usize = 4096;
    let mut error_details = serde_json::Map::new();
    error_details.insert(
        "source_movement_seq".to_string(),
        serde_json::json!(source_movement_seq),
    );
    error_details.insert("failure_details".to_string(), details.clone());
    let rendered = serde_json::to_string(&error_details)
        .map_err(|error| format!("tool failure details did not encode: {error}"))?;
    if rendered.chars().count() > MAX_CHARS {
        let mut bounded = serde_json::json!({
            "truncated": true,
            "blake3": blake3::hash(rendered.as_bytes()).to_hex().to_string(),
            "encoded_chars": rendered.chars().count(),
        });
        if let Some(fields) = details.as_object() {
            for (key, value) in fields {
                if key == "error" || value.is_array() || value.is_object()
                    || bounded.get(key).is_some()
                {
                    continue;
                }
                bounded[key] = value.clone();
                if bounded.to_string().chars().count() > MAX_CHARS / 4 {
                    bounded.as_object_mut().unwrap().remove(key);
                }
            }
        }
        let diagnostic = details.get("error").and_then(serde_json::Value::as_str)
            .unwrap_or(&rendered);
        let diagnostic_chars = diagnostic.chars().count();
        let mut retained_chars = diagnostic_chars.min(MAX_CHARS);
        bounded["error_blake3"] =
            serde_json::json!(blake3::hash(diagnostic.as_bytes()).to_hex().to_string());
        loop {
            let preview = if retained_chars == diagnostic_chars {
                diagnostic.to_string()
            } else {
                let head = diagnostic.chars().take(retained_chars.div_ceil(2)).collect::<String>();
                let tail = diagnostic.chars().rev().take(retained_chars / 2)
                    .collect::<Vec<_>>().into_iter().rev().collect::<String>();
                format!("{head}\n… [middle omitted] …\n{tail}")
            };
            bounded["error"] = serde_json::json!(preview);
            bounded["error_truncated"] = serde_json::json!(retained_chars < diagnostic_chars);
            bounded["error_omitted_chars"] =
                serde_json::json!(diagnostic_chars - retained_chars);
            error_details.insert("failure_details".to_string(), bounded.clone());
            if serde_json::to_string(&error_details)
                .map_err(|error| format!("tool failure preview did not encode: {error}"))?
                .chars().count() <= MAX_CHARS
            {
                break;
            }
            retained_chars /= 2;
        }
    }
    Ok(octopus_tool_handlers::ToolResultError::new(
        failure_class,
        format!("tool process failed with class '{failure_class}'"),
    )
    .with_details(error_details))
}


#[test]
fn oversized_tool_failures_preserve_recovery_evidence() {
    use octopus_tool_handlers::ToolResultArtifact;
    for failure_class in ["tool_process_failure", TOOL_EFFECT_INDETERMINATE] {
        let details = serde_json::json!({
            "tool": "test.write",
            "effect_state": "in_flight_outcome_unknown",
            "error": format!("write outcome unknown: {}disk is full", "\n\0\"\\🦑".repeat(3000)),
        });
        let error = bounded_tool_failure_error(42, failure_class, &details).unwrap();
        let result = ToolResultArtifact::failure("test.write", Some("call-42".into()), error);
        let result: ToolResultArtifact = serde_json::from_slice(&serde_json::to_vec(&result).unwrap()).unwrap();
        assert!(!result.ok);
        assert_eq!(result.correlation_id.as_deref(), Some("call-42"));
        let error = result.error.unwrap();
        assert_eq!(error.code, failure_class);
        let bounded = error.details.unwrap();
        assert!(serde_json::to_string(&bounded).unwrap().chars().count() <= 4096);
        assert_eq!(bounded["source_movement_seq"], 42);
        assert_eq!(bounded["failure_details"]["effect_state"], "in_flight_outcome_unknown");
        let diagnostic = bounded["failure_details"]["error"].as_str().unwrap();
        assert!(diagnostic.starts_with("write outcome unknown: "));
        assert!(diagnostic.ends_with("disk is full"));
        assert_eq!(bounded["failure_details"]["truncated"], true);
    }
    let details = serde_json::json!({"error": "permission denied", "tool": "test.write"});
    let error = bounded_tool_failure_error(43, "tool_process_failure", &details).unwrap();
    assert_eq!(error.details.unwrap()["failure_details"], details);
}

fn read_bounded_tool_door_ledger_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    maximum_bytes: u64,
) -> Result<usize, String> {
    let maximum_bytes = usize::try_from(maximum_bytes).map_err(|_| {
        "tool-door recovery ledger row ceiling exceeds host address space".to_string()
    })?;
    buffer.clear();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("tool-door recovery ledger read failed: {error}"))?;
        if available.is_empty() {
            return Ok(buffer.len());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if take > maximum_bytes.saturating_sub(buffer.len()) {
            return Err(format!(
                "tool-door recovery ledger row exceeds declared ceiling of {maximum_bytes} bytes"
            ));
        }
        buffer.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(buffer.len());
        }
    }
}

fn decode_tool_door_recovery_movement(
    movements_path: &Path,
    row: &[u8],
    line: usize,
    maximum_metadata_bytes: u64,
) -> Result<Movement, String> {
    let metadata_dir = movements_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("metadata");
    decode_durable_movement_row(row, line, |kind, hash| {
        let path = metadata_dir.join(hash.to_hex());
        let bytes =
            octopus_interfaces::read_artifact_carrier_file_bounded(&path, maximum_metadata_bytes)
                .map_err(|error| LedgerError::MalformedMovementMetadata {
                line,
                message: format!(
                    "content-addressed {} '{}' cannot be read: {error}",
                    kind.describe(),
                    path.display()
                ),
            })?;
        let actual = ContentHash::of_bytes(&bytes);
        if actual != hash {
            return Err(LedgerError::BlobIntegrity {
                expected: hash,
                actual,
            });
        }
        serde_json::from_slice(&bytes).map_err(|error| LedgerError::MalformedMovementMetadata {
            line,
            message: format!("content-addressed {} is malformed: {error}", kind.describe()),
        })
    })
    .map_err(|error| format!("tool-door recovery ledger row {line} is malformed: {error}"))
}

fn tool_door_recovery_artifact(
    ctx: &InterfaceLiftContext,
    artifact: &ArtifactRef,
) -> Result<(InterfaceArtifactFile, InterfaceArtifactManifest), String> {
    tool_door_recovery_artifact_at(&ctx.ledger_read_handles()?, artifact)
}

fn tool_door_recovery_artifact_at(
    handles: &InterfaceLedgerReadHandles<'_>,
    artifact: &ArtifactRef,
) -> Result<(InterfaceArtifactFile, InterfaceArtifactManifest), String> {
    let limits = handles.limits;
    let file = InterfaceArtifactFile::new(handles.blobs_dir.join(artifact.content.to_hex()))
        .map_err(|error| format!("tool-door recovery artifact cannot be opened: {error}"))?
        .with_payload_cache_root(&handles.blobs_dir);
    let bounded = file
        .read_manifest_bounded(limits.max_manifest_bytes)
        .map_err(|error| format!("tool-door recovery artifact manifest is invalid: {error}"))?;
    let actual = ContentHash::of_bytes(&bounded.manifest_bytes);
    if actual != artifact.content {
        return Err(format!(
            "tool-door recovery artifact '{}' manifest hash changed",
            artifact.id.as_str()
        ));
    }
    let expected_id = format!("{}:{}", bounded.manifest.label, actual.to_hex());
    if artifact.id.as_str() != expected_id {
        return Err(format!(
            "tool-door recovery artifact id '{}' conflicts with manifest label '{}'",
            artifact.id.as_str(),
            bounded.manifest.label
        ));
    }
    Ok((file, bounded.manifest))
}

pub fn tool_door_has_exact_admitted_result(
    ctx: &InterfaceLiftContext,
    source: &Movement,
    call: &ToolCallArtifact,
    envelope: &InterfaceInvocationEnvelope,
    decoder: ToolDoorArtifactDecoder,
) -> Result<bool, String> {
    tool_door_admitted_result_state(ctx, source, call, envelope, decoder).map(|state| state.is_some())
}

fn tool_door_admitted_result_state(
    ctx: &InterfaceLiftContext,
    source: &Movement,
    call: &ToolCallArtifact,
    envelope: &InterfaceInvocationEnvelope,
    decoder: ToolDoorArtifactDecoder,
) -> Result<Option<ToolDoorJournalState>, String> {
    tool_door_admitted_result_state_at(
        &ctx.ledger_read_handles()?, &ctx.interface, source, call, envelope, None,
        |movement, candidate| decoder(&ctx.with_source_movement(movement.clone()), candidate,
            ToolDoorArtifactDecodePurpose::HistoricalIdentity).map_err(|error| error.to_string()),
    )
}

fn tool_door_admitted_result_state_at(
    handles: &InterfaceLedgerReadHandles<'_>,
    door: &InterfaceId,
    source: &Movement,
    call: &ToolCallArtifact,
    envelope: &InterfaceInvocationEnvelope,
    ledger_snapshot: Option<&[u8]>,
    decoder: impl Fn(&Movement, &InterfaceInvocationEnvelope) -> Result<Option<ToolCallArtifact>, String>,
) -> Result<Option<ToolDoorJournalState>, String> {
    let source_runtime = envelope
        .runtime()
        .map_err(|error| format!("tool-door recovery source coordinates are invalid: {error}"))?;
    if source.kind != MovementKind::InterfaceOutbound
        || !matches!(&source.from, Endpoint::Interface(_))
        || !matches!(&source.to, Endpoint::Interface(interface) if interface == &*door)
        || source_runtime.interface != *door
        || source.artifact.as_ref() != Some(&envelope.artifact)
    {
        return Err(format!(
            "tool-door recovery source movement {} does not name this exact door artifact",
            source.seq.0
        ));
    }
    let limits = handles.limits;
    let (mut reader, ledger_len): (Box<dyn BufRead + '_>, u64) = if let Some(bytes) = ledger_snapshot {
        (Box::new(std::io::Cursor::new(bytes)), bytes.len() as u64)
    } else {
        let metadata = std::fs::symlink_metadata(&handles.movements_path).map_err(|error| {
            format!(
                "tool-door recovery ledger '{}' cannot be inspected: {error}",
                handles.movements_path.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(format!(
                "tool-door recovery ledger '{}' must be a regular non-symlink file",
                handles.movements_path.display()
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&handles.movements_path).map_err(|error| {
            format!(
                "tool-door recovery ledger '{}' cannot be opened: {error}",
                handles.movements_path.display()
            )
        })?;
        let ledger_len = file
            .metadata()
            .map_err(|error| format!("tool-door recovery ledger metadata cannot be read: {error}"))?
            .len();
        (Box::new(std::io::BufReader::new(file.take(ledger_len))), ledger_len)
    };
    let expected_result_label = format!("{}.result", call.tool);
    let mut line = Vec::new();
    let mut offset = 0_u64;
    let mut expected_seq = 0_u64;
    let mut source_seen = false;
    let mut admitted_result = None;
    while offset < ledger_len {
        let read = read_bounded_tool_door_ledger_line(
            &mut reader,
            &mut line,
            limits.max_control_frame_bytes,
        )?;
        if read == 0 {
            return Err(format!(
                "tool-door recovery ledger '{}' ended before its declared length",
                handles.movements_path.display()
            ));
        }
        let end = offset
            .checked_add(read as u64)
            .ok_or_else(|| "tool-door recovery ledger offset overflowed".to_string())?;
        if end > ledger_len {
            return Err(format!(
                "tool-door recovery ledger '{}' changed across the inspected boundary",
                handles.movements_path.display()
            ));
        }
        let Some(json_line) = line.strip_suffix(b"\n") else {
            return Err(format!(
                "tool-door recovery ledger '{}' ends with an incomplete movement",
                handles.movements_path.display()
            ));
        };
        let json_line = json_line.strip_suffix(b"\r").unwrap_or(json_line);
        if json_line.is_empty() {
            return Err(format!(
                "tool-door recovery ledger '{}' contains an empty movement at seq {expected_seq}",
                handles.movements_path.display()
            ));
        }
        let line_index = usize::try_from(expected_seq)
            .map_err(|_| "tool-door recovery movement sequence exceeds host range".to_string())?;
        let route = decode_durable_movement_route(json_line, line_index).map_err(|error| {
            format!("tool-door recovery ledger row {expected_seq} is malformed: {error}")
        })?;
        if route.seq.0 != expected_seq {
            return Err(format!(
                "tool-door recovery ledger row {expected_seq} carries seq {}",
                route.seq.0
            ));
        }
        offset = end;
        expected_seq = expected_seq
            .checked_add(1)
            .ok_or_else(|| "tool-door recovery movement sequence overflowed".to_string())?;
        if route.seq == source.seq {
            let observed = decode_tool_door_recovery_movement(
                &handles.movements_path,
                json_line,
                line_index,
                limits.max_control_frame_bytes,
            )?;
            if observed != *source {
                return Err(format!(
                    "tool-door recovery source movement {} changed in the ledger",
                    source.seq.0
                ));
            }
            source_seen = true;
        } else if route.kind == MovementKind::InterfaceOutbound
            && matches!(&route.from, Endpoint::Interface(_))
            && matches!(&route.to, Endpoint::Interface(interface) if interface == &*door)
        {
            let movement = decode_tool_door_recovery_movement(
                &handles.movements_path,
                json_line,
                line_index,
                limits.max_control_frame_bytes,
            )?;
            let invocation = movement
                .invocation_metadata
                .as_ref()
                .and_then(|value| value.invocation())
                .ok_or_else(|| {
                    format!(
                        "tool-door recovery source candidate movement {} has no invocation metadata",
                        movement.seq.0
                    )
                })?;
            let runtime = invocation.runtime().ok_or_else(|| {
                format!(
                    "tool-door recovery source candidate movement {} has no runtime coordinates",
                    movement.seq.0
                )
            })?;
            if runtime.interface != *door {
                return Err(format!(
                    "tool-door recovery source candidate movement {} names a different door coordinate",
                    movement.seq.0
                ));
            }
            if runtime.run == source_runtime.run {
                if runtime.ee != source_runtime.ee || runtime.worker != source_runtime.worker {
                    return Err(format!(
                        "tool-door recovery source candidate movement {} conflicts with the source run coordinates",
                        movement.seq.0
                    ));
                }
                let artifact = movement.artifact.as_ref().ok_or_else(|| {
                    format!(
                        "tool-door recovery source candidate movement {} carries no artifact",
                        movement.seq.0
                    )
                })?;
                let (file, _) = tool_door_recovery_artifact_at(handles, artifact)?;
                let captured = file
                    .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                    .map_err(|error| {
                        format!("tool-door recovery source payload cannot be captured: {error}")
                    })?;
                let file = InterfaceArtifactFile::from_captured_snapshot(
                    file.manifest_path().to_path_buf(),
                    captured.manifest_bytes,
                    captured.payload_bytes,
                )
                .map_err(|error| format!("tool-door recovery source cannot be pinned: {error}"))?;
                let mut remaining = limits.max_payload_bytes;
                let mut artifact_binding_payloads = Vec::new();
                for binding in &invocation.artifact_binding_refs {
                    let path = handles.blobs_dir.join(binding.content.to_hex());
                    artifact_binding_payloads.push(door_life::resolve_artifact_binding_payload(
                        &path, binding, limits, &mut remaining,
                    )?);
                }
                let candidate_envelope = InterfaceInvocationEnvelope {
                    artifact: artifact.clone(),
                    artifact_file: file,
                    artifact_binding_payloads,
                    metadata: Some(invocation.clone()),
                };
                if let Some(candidate) = decoder(&movement, &candidate_envelope)? {
                    if candidate.tool == call.tool
                        && candidate.correlation_id == call.correlation_id
                    {
                        return Err(format!(
                            "tool-door recovery found duplicate source call identity at movements {} and {}",
                            source.seq.0, movement.seq.0
                        ));
                    }
                }
            }
        } else if route.kind == MovementKind::InterfaceArrival
            && matches!(&route.from, Endpoint::Interface(interface) if interface == &*door)
            && matches!(&route.to, Endpoint::Interface(worker) if worker == &source_runtime.worker)
        {
            let movement = decode_tool_door_recovery_movement(
                &handles.movements_path,
                json_line,
                line_index,
                limits.max_control_frame_bytes,
            )?;
            let invocation = movement
                .invocation_metadata
                .as_ref()
                .and_then(|value| value.invocation())
                .ok_or_else(|| {
                    format!(
                        "tool-door recovery result candidate movement {} has no invocation metadata",
                        movement.seq.0
                    )
                })?;
            let runtime = invocation.runtime().ok_or_else(|| {
                format!(
                    "tool-door recovery result candidate movement {} has no runtime coordinates",
                    movement.seq.0
                )
            })?;
            if runtime.run != source_runtime.run {
                continue;
            }
            if runtime.ee != source_runtime.ee
                || runtime.worker != source_runtime.worker
                || runtime.interface != *door
            {
                return Err(format!(
                    "tool-door recovery result candidate movement {} conflicts with the source run/interface coordinates",
                    movement.seq.0
                ));
            }
            let artifact = movement.artifact.as_ref().ok_or_else(|| {
                format!(
                    "tool-door recovery result candidate movement {} carries no artifact",
                    movement.seq.0
                )
            })?;
            let (file, manifest) = tool_door_recovery_artifact_at(handles, artifact)?;
            if manifest.label == expected_result_label {
                let payload = file
                    .read_payload_bytes_bounded(limits.max_payload_bytes)
                    .map_err(|error| {
                        format!(
                            "tool-door recovery result candidate movement {} payload is invalid: {error}",
                            movement.seq.0
                        )
                    })?;
                let result: ToolResultArtifact =
                    serde_json::from_slice(&payload).map_err(|error| {
                        format!(
                            "tool-door recovery result candidate movement {} is malformed: {error}",
                            movement.seq.0
                        )
                    })?;
                if result.label != manifest.label || result.tool != call.tool {
                    return Err(format!(
                        "tool-door recovery result candidate movement {} conflicts with its manifest/tool identity",
                        movement.seq.0
                    ));
                }
                if result.correlation_id == call.correlation_id
                    && invocation.verification_failure.is_none()
                {
                    if movement.seq.0 <= source.seq.0 {
                        return Err(format!(
                            "tool-door recovery exact result movement {} does not follow source movement {}",
                            movement.seq.0, source.seq.0
                        ));
                    }
                    let state = if result.error.as_ref().is_some_and(|error| error.code == TOOL_EFFECT_INDETERMINATE) {
                        ToolDoorJournalState::Indeterminate
                    } else {
                        ToolDoorJournalState::Completed
                    };
                    if let Some((previous, _)) = admitted_result.replace((movement.seq.0, state)) {
                        return Err(format!(
                            "tool-door recovery found duplicate exact admitted results at movements {previous} and {}",
                            movement.seq.0
                        ));
                    }
                }
            }
        }
    }
    if !source_seen {
        return Err(format!(
            "tool-door recovery source movement {} is absent from the ledger",
            source.seq.0
        ));
    }
    Ok(admitted_result.map(|(_, state)| state))
}

fn tool_door_movement_digest(movement: &octopus_ledger::Movement) -> Result<String, String> {
    let bytes = serde_json::to_vec(movement)
        .map_err(|error| format!("tool-door movement cannot serialize: {error}"))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn record_tool_door_completion(
    completion: ToolDoorCompletion,
    journal: &mut ToolDoorEffectJournal,
    rows: &mut BTreeMap<u64, ToolDoorPendingRow>,
) -> Result<(), String> {
    let row = rows.get_mut(&completion.seq).ok_or_else(|| {
        format!(
            "tool-door completion references unknown movement seq {}",
            completion.seq
        )
    })?;
    if row.movement_digest != completion.movement_digest {
        return Err(format!(
            "tool-door completion movement digest changed for seq {}",
            completion.seq
        ));
    }
    let state = completion.result?;
    journal.finish(completion.seq, &completion.movement_digest, state)?;
    row.state = ToolDoorRowState::Completed;
    if let Some(primary_input) = completion.primary_input {
        if state != ToolDoorJournalState::Completed {
            return Err("tool-process input retirement requires durable completion".into());
        }
        primary_input.retire().map_err(|error| {
            format!(
                "tool input retirement failed after durable completion of movement {}: {error}",
                completion.seq,
            )
        })?;
    }
    Ok(())
}

fn advance_tool_door_frontier(
    reader: &mut LedgerTailReader,
    journal: &mut ToolDoorEffectJournal,
    rows: &mut BTreeMap<u64, ToolDoorPendingRow>,
) -> Result<(), String> {
    let mut advanced = false;
    loop {
        let Some((&seq, row)) = rows.first_key_value() else {
            reader.advance_scanned()?;
            break;
        };
        if row.state != ToolDoorRowState::Completed {
            break;
        }
        let row = rows
            .remove(&seq)
            .ok_or_else(|| format!("tool-door completed row {seq} disappeared"))?;
        reader.commit_through(&row.own)?;
        advanced = true;
    }
    if advanced {
        journal.prune_before(reader.next_seq())?;
    }
    Ok(())
}

async fn retire_tool_door_tasks(
    tasks: &mut tokio::task::JoinSet<ToolDoorCompletion>,
    journal: &mut ToolDoorEffectJournal,
    rows: &mut BTreeMap<u64, ToolDoorPendingRow>,
) -> Result<(), String> {
    tasks.abort_all();
    let mut errors = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(completion) if completion.result.is_ok() => {
                if let Err(error) = record_tool_door_completion(completion, journal, rows) {
                    errors.push(error);
                }
            }
            Ok(_) => {}
            Err(error) if error.is_cancelled() => {}
            Err(error) => errors.push(format!("tool-door invocation task failed: {error}")),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn decode_tool_call(
    envelope: &InterfaceInvocationEnvelope,
) -> Result<Option<ToolCallArtifact>, String> {
    let manifest: InterfaceArtifactManifest = envelope
        .read_artifact_manifest()
        .map_err(|error| error.to_string())?;
    if manifest.label != "tool.call" {
        return Ok(None);
    }
    let bytes = envelope
        .read_artifact_payload()
        .map_err(|error| error.to_string())?;
    let call: ToolCallArtifact = serde_json::from_slice(&bytes)
        .map_err(|error| format!("tool.call payload is malformed: {error}"))?;
    Ok(Some(call))
}

fn decode_worker_environment_release(
    envelope: &InterfaceInvocationEnvelope,
) -> Result<Option<WorkerEnvironmentReleaseRequest>, String> {
    let manifest: InterfaceArtifactManifest = envelope
        .read_artifact_manifest()
        .map_err(|error| error.to_string())?;
    if manifest.label != "worker_environment.release" {
        return Ok(None);
    }
    let bytes = envelope
        .read_artifact_payload()
        .map_err(|error| error.to_string())?;
    let request: WorkerEnvironmentReleaseRequest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("worker_environment.release payload is malformed: {error}"))?;
    WorkerEnvironmentId::parse(&request.worker_environment_id).map_err(|error| {
        format!("worker_environment.release payload has invalid worker_environment_id: {error}")
    })?;
    Ok(Some(request))
}

fn required_env_path(name: &str) -> Result<PathBuf, String> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} is required by the tool-process contract"))
}

fn validate_private_read_handle(handle: &str) -> Result<(), String> {
    let path = Path::new(handle);
    if handle.trim() != handle
        || handle.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            !matches!(component, std::path::Component::Normal(value) if !value.is_empty())
        })
    {
        return Err(format!(
            "tool process private read handle {:?} is not a relative path of normal components",
            handle
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tool_process_private_boundary_tests {
    mod contract_update_tests {
        include!("../tests/support/contract_update.rs");
    }
    mod completed_output_tests {
        include!("../tests/support/completed_output.rs");

        #[tokio::test]
        async fn tool_failure_result_is_handed_again_while_its_current_source_settles() {
            let root = tempfile::tempdir().unwrap(); let root = root.path().canonicalize().unwrap();
            let (ctx, _, _, mut arrivals, _) = completed_fixture(&root, 1).await;
            let target = ToolResultTarget::from(&ToolCallArtifact {
                tool:"test.tool".into(), arguments:serde_json::json!({}), correlation_id:Some("failed-0".into()),
                label:Some("tool.call".into()), source_interface_evidence:BTreeMap::new(),
            });
            let failure = hand_tool_failure(&ctx, 0, "tool_effect_indeterminate",
                serde_json::json!({"effect_state":"in_flight_outcome_unknown"}), Some(&target), ToolDoorJournalState::Indeterminate, None);
            tokio::pin!(failure);
            let mut handed = Vec::new();
            for verdict in [
                DeliveryVerdict::refused(RefusedReason::StaleEpoch, serde_json::json!({"reason_detail":"arrival_target_changed"})),
                DeliveryVerdict::refused(RefusedReason::RegistryUnstable, serde_json::json!({})),
                DeliveryVerdict::Admitted { seq: 3 },
            ] {
                let arrival = tokio::select! { arrival = arrivals.recv() => arrival.unwrap(), result = &mut failure => panic!("failure settled before its current source: {result:?}") };
                handed.push(arrival.artifact.read_payload_bytes().unwrap());
                arrival.verdict_tx.unwrap().send(verdict).unwrap();
            }
            assert_eq!(failure.await.unwrap(), ToolDoorJournalState::Indeterminate);
            assert!(handed.windows(2).all(|pair| pair[0] == pair[1]));
        }

        #[tokio::test]
        async fn retained_pending_rehands_selected_output_before_resuming_owner_callback() {
            let root = tempfile::tempdir().unwrap(); let root = root.path().canonicalize().unwrap();
            let (ctx, input, completion, mut arrivals, _) = completed_fixture(&root, 1).await;
            let original = completion.artifacts[0].manifest_path().to_path_buf();
            let calls = Arc::new(AtomicU64::new(0));
            let callback: ToolDoorOutputCallback = Arc::new(CompletedOutputOwner {
                mode: "selected", calls: Arc::clone(&calls), proofs: Arc::new(AtomicU64::new(0)), unsubmitted: false,
            });
            let mut receipt = completed_output::CompletedOutputs::retain(&ctx, &input, &completion).unwrap();
            let mut pinned = None;
            for attempt in 0..5 {
                {
                let delivery = receipt.deliver(&ctx, &input, Some(&callback), attempt != 0); tokio::pin!(delivery);
                let arrival = tokio::select! { arrival = arrivals.recv() => arrival.unwrap(), result = &mut delivery => panic!("missing retained handoff: {result:?}") };
                assert!(arrival.verdict_tx.is_none());
                let retained = arrival.retained.unwrap();
                let identity = (retained.id.clone(), arrival.artifact.manifest_path().to_path_buf());
                assert_ne!(identity.1, original);
                assert_eq!(arrival.artifact.read_payload_bytes().unwrap(), b"transformed result");
                if let Some(prior) = &pinned { assert_eq!(prior, &identity); } else { pinned = Some(identity); }
                retained.reply.send(match attempt {
                    1 => Err("scope corruption after partial admission".into()),
                    2 => Ok(octopus_ledger::ArtifactDelivery::Complete { verdict: DeliveryVerdict::refused(RefusedReason::RegistryUnstable, serde_json::json!({})) }),
                    4 => Ok(octopus_ledger::ArtifactDelivery::Complete { verdict: DeliveryVerdict::Admitted { seq: 9 } }),
                    _ => Ok(octopus_ledger::ArtifactDelivery::Pending { scope_seq: 7 }),
                }).unwrap();
                if attempt < 4 {
                    let error = delivery.await.unwrap_err();
                    assert!(error.contains(match attempt { 1 => "scope corruption after partial admission", 2 => "current source is unresolved", _ => "unresolved edges" }));
                    assert_eq!(calls.load(Ordering::SeqCst), 1);
                    let saved = serde_json::to_value(completed_output::CompletedOutputs::load(&ctx, 0).unwrap().unwrap()).unwrap();
                    assert_eq!(saved["schema_version"], 4);
                    assert_eq!(saved["outputs"][0]["admission"], serde_json::json!({"state":"retained", "scope_seq":7}));
                    assert_eq!(saved["outputs"][0]["callback_completed"], false);
                } else { assert!(matches!(delivery.await.unwrap(), ToolProcessBrokerResult::Completed)); }
                }
                receipt = completed_output::CompletedOutputs::load(&ctx, 0).unwrap().unwrap();
            }
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert!(arrivals.try_recv().is_err());
            assert_eq!(std::fs::read(root.join("effects")).unwrap(), b"effect");
        }

        #[tokio::test]
        async fn deferred_decode_preserves_prepared_sources_across_wake_cancel_and_refusal() {
            use std::io::Write;
            use ToolDoorArtifactDecodeError as DecodeError;
            async fn until(running: std::pin::Pin<&mut impl std::future::Future<Output=Result<(),String>>>, done: impl Fn() -> bool) {
                tokio::select! {
                    result = running => panic!("door stopped: {result:?}"),
                    result = tokio::time::timeout(std::time::Duration::from_secs(5), async { while !done() { tokio::task::yield_now().await; } }) => result.unwrap(),
                }
            }
            fn receipt(ctx: &InterfaceLiftContext, source: &Movement) {
                let mut row = source.clone(); row.seq = Seq(2); row.lamport = Lamport(3); row.to = Endpoint::Interface(InterfaceId::new("foreign"));
                writeln!(std::fs::OpenOptions::new().append(true).open(&ctx.movements_path).unwrap(), "{}", serde_json::to_string(&row).unwrap()).unwrap();
            }
            fn decode(ctx: &InterfaceLiftContext, envelope: &InterfaceInvocationEnvelope, purpose: ToolDoorArtifactDecodePurpose) -> Result<Option<ToolCallArtifact>, DecodeError> {
                assert_eq!(purpose, ToolDoorArtifactDecodePurpose::Dispatch);
                let source = ctx.source_movement(envelope)?;
                let name = if source.seq.0 == 0 { "attempts" } else { "unrelated" };
                std::fs::OpenOptions::new().create(true).append(true).open(ctx.interface_var_dir().join(name)).unwrap().write_all(b"x").unwrap();
                if source.seq.0 != 0 { return Ok(None); }
                if std::fs::read_to_string(&ctx.movements_path).unwrap().lines().count() < 3 {
                    if ctx.interface_var_dir().join("early").exists() { receipt(ctx, source); }
                    return Err(DecodeError::AwaitingResults { call_sequences: vec![0] });
                }
                if ctx.interface_var_dir().join("invalid").exists() { return Err("run change awaits admitted checked file.write results for calls [0]".into()); }
                Ok(None)
            }
            for (early, refused) in [(true, false), (false, false), (false, true)] {
                let root = tempfile::tempdir().unwrap(); let root = root.path().canonicalize().unwrap();
                let (mut ctx, _, _, mut outputs, source) = completed_fixture(&root, 1).await;
                for (flag, enabled) in [("early", early), ("invalid", refused)] { if enabled { std::fs::write(ctx.interface_var_dir().join(flag), []).unwrap(); } }
                let mut unrelated = source.clone(); unrelated.seq = Seq(1); unrelated.lamport = Lamport(2);
                write_recovery_ledger(&root, &[source.clone(), unrelated]);
                let owner = ToolDoorOwner { worker_environment_kind:WorkerEnvironmentKindId::new("test.room"), door_interface:ctx.interface.clone(), max_in_flight:2, private_read_max_bytes:4096, root_slot_transform:None };
                let path = ctx.interface_var_dir().join(TOOL_DOOR_EFFECT_JOURNAL_FILE);
                let state = |seq| ToolDoorEffectJournal::read_only(path.clone(),65536).unwrap().entries.get(&seq).map(|entry| entry.state);
                {
                    let hot = tool_door_loop(&ctx, &owner, Arc::new(ToolProcessBrokerHost::new(owner.clone()).unwrap()), decode); tokio::pin!(hot);
                    until(hot.as_mut(), || state(1) == Some(ToolDoorJournalState::Completed) && state(0) == Some(if early { ToolDoorJournalState::Completed } else { ToolDoorJournalState::Prepared })).await;
                    assert!(std::fs::read(ctx.interface_var_dir().join("attempts")).unwrap().len() <= 3);
                    ctx.cancel.cancel(); hot.await.unwrap();
                }
                if !early {
                    assert_eq!(LedgerTailReader::open_named(&ctx, "tool-door").unwrap().next_seq(), 0);
                    ctx.cancel = tokio_util::sync::CancellationToken::new();
                    let attempts = std::fs::read(ctx.interface_var_dir().join("attempts")).unwrap().len();
                    let cold = tool_door_loop(&ctx, &owner, Arc::new(ToolProcessBrokerHost::new(owner.clone()).unwrap()), decode); tokio::pin!(cold);
                    until(cold.as_mut(), || std::fs::read(ctx.interface_var_dir().join("attempts")).unwrap().len() > attempts).await; receipt(&ctx, &source);
                    if refused {
                        let output = tokio::select! { output = tokio::time::timeout(std::time::Duration::from_secs(5), outputs.recv()) => output.unwrap().unwrap(), result = &mut cold => panic!("door stopped: {result:?}") };
                        output.verdict_tx.unwrap().send(DeliveryVerdict::refused(RefusedReason::CheckpointSpent, serde_json::json!({}))).unwrap();
                    }
                    until(cold.as_mut(), || state(0) == Some(if refused { ToolDoorJournalState::Refused } else { ToolDoorJournalState::Completed })).await;
                    ctx.cancel.cancel(); cold.await.unwrap();
                }
                assert_eq!(std::fs::read(ctx.interface_var_dir().join("unrelated")).unwrap(), b"x");
            }
        }
    }
    use super::{
        BoundedJsonLineReader, TOOL_DOOR_EFFECT_JOURNAL_FILE, ToolDoorEffectJournal,
        ToolDoorJournalState, ToolProcessBrokerShards, ToolProcessBrokerState, ToolProcessClient,
        ToolProcessEnvelope, ToolProcessKey, WorkerEnvironmentReleaseRequest, canonical_hash,
        configure_tool_process_owner_lease, default_tool_door_artifact_decoder,
        effective_tool_process_environment, empty_invocation_authority_hash,
        invocation_authority_hash, materialize_private_read_generation,
        materialize_tool_process_durable_dir, narrow_process_declaration,
        prepare_tool_process_owner_lease, private_read_snapshot_hash, publish_tool_process_owner,
        quiesce_worker_environment_owner_generations, reconcile_tool_process_generations,
        refuse_released_worker_environment, remove_tool_process_generation,
        signal_tool_process_group, tool_door_has_exact_admitted_result,
        tool_process_environment_hash, tool_process_generation_key, tool_process_host_private_root,
        tool_process_interface_local_ipc_root, validate_tool_process_durable_var_dir,
    };
    use super::{
        INTERFACE_LOCAL_IPC_ROOT_SLOT, InterfaceActivationArrival, InterfaceHostIoLimits,
        InterfaceLiftContext,
    };
    use octopus_descriptors::{
        ArtifactId, CheckpointId, EeId, InterfaceId, RunId, WorkerEnvironmentKindId,
    };
    use octopus_interfaces::{
        GrantProjection, InterfaceArtifactFile, InterfaceArtifactManifest,
        InterfaceInvocationEnvelope, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{
        ArtifactRef, ContentHash, Endpoint, Lamport, Movement, MovementKind, Seq, TraceId,
        encode_durable_movement_row,
    };
    use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
    use octopus_tool_registry::{
        ProcessArgumentPathSpec, ProcessAuthoritySpec, ToolProcessDeclaration,
        ToolProcessPrivateReadSubpath, load_tool_descriptor_from_dir,
    };
    use octopus_worker_env::{
        WorkerEnvKindRegistry, WorkerEnvKindSpec, WorkerEnvironmentFormationRequest,
        WorkerEnvironmentId, WorkerEnvironmentLifecycle, WorkerEnvironmentRecordStore,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    fn process_declaration(
        private_read_subpaths: BTreeSet<ToolProcessPrivateReadSubpath>,
    ) -> ToolProcessDeclaration {
        ToolProcessDeclaration {
            bin: "/usr/bin/true".to_string(),
            args: Vec::new(),
            required_files: BTreeSet::new(),
            env: BTreeMap::new(),
            inherit_env: BTreeSet::new(),
            inherit_env_from_interface: BTreeSet::new(),
            authority: ProcessAuthoritySpec::default(),
            coordinate_root_slots: BTreeSet::new(),
            private_read_subpaths,
            interrupted_effect_recovery: Default::default(),
        }
    }

    #[test]
    fn worker_environment_release_payload_is_closed_and_typed() {
        let valid: WorkerEnvironmentReleaseRequest = serde_json::from_value(serde_json::json!({
            "worker_environment_id": "we_0123456789abcdef0123456789abcdef"
        }))
        .unwrap();
        assert!(
            octopus_worker_env::WorkerEnvironmentId::parse(valid.worker_environment_id).is_ok()
        );
        assert!(
            serde_json::from_value::<WorkerEnvironmentReleaseRequest>(serde_json::json!({
                "worker_environment_id": "we_0123456789abcdef0123456789abcdef",
                "extra": true
            }))
            .is_err()
        );
        assert!(
            octopus_worker_env::WorkerEnvironmentId::parse("not-a-worker-environment").is_err()
        );
    }

    fn test_formation(run: &str) -> (InterfaceId, octopus_worker_env::WorkerEnvironmentFormation) {
        let mut registry = WorkerEnvKindRegistry::default();
        registry
            .upsert(
                &serde_json::to_vec(&serde_json::json!({
                    "worker_environment_kind_id": "test.room",
                    "substrate_family": "test",
                    "door_driver_refs": ["test_door"],
                    "tool_process_bins": ["target/debug/test-tool"],
                    "worker_consent": "explicit_ceiling",
                    "supported_executor_kinds": ["exec"],
                    "capabilities": ["process_exec"],
                    "boundary_defaults": {
                        "read_root_refs": [],
                        "write_root_refs": [],
                        "protected_read_only_subpaths": [],
                        "network_ceiling": "denied"
                    },
                    "lifecycle": {"requires_release": true}
                }))
                .unwrap(),
            )
            .unwrap();
        let door = InterfaceId::new("system_test_door");
        let worker = InterfaceId::new("system_worker_test");
        let kind = WorkerEnvironmentKindId::from("test.room");
        let kinds = vec![kind];
        let tools = vec!["test.tool".to_string()];
        let tool_hashes = BTreeMap::from([("test.tool".to_string(), "hash-tool".to_string())]);
        let tool_process_bins = BTreeMap::from([(
            "test.tool".to_string(),
            "target/debug/test-tool".to_string(),
        )]);
        let roots = BTreeMap::new();
        let formation = registry
            .form(&WorkerEnvironmentFormationRequest {
                door_additional_root_bindings: &Default::default(),
                door_interface: &door,
                door_descriptor_hash: "hash-door",
                door_driver_ref: "test_door",
                tool: "test.tool",
                tool_descriptor_hashes: &tool_hashes,
                tool_process_bins: &tool_process_bins,
                tool_worker_environment_kinds: &kinds,
                worker: &worker,
                ee: "ee_test",
                run,
                granted_worker_environment_kinds: &kinds,
                granted_tools: &tools,
                door_root_bindings: &roots,
                door_instance_dir: std::path::Path::new("."),
                ee_root_slot_allowance: None,
            })
            .unwrap();
        (door, formation)
    }

    fn test_lift_context(root: &std::path::Path, door: InterfaceId) -> InterfaceLiftContext {
        let (arrivals, _receiver) = tokio::sync::mpsc::channel::<InterfaceActivationArrival>(1);
        InterfaceLiftContext::unprivileged(
            door,
            root.join("registry/interface/system_test_door_instance"),
            None,
            root.join("var"),
            root.join("movements.jsonl"),
            root.join("blobs"),
            root.join("ledger-index.sqlite"),
            Default::default(),
            arrivals,
            tokio_util::sync::CancellationToken::new(),
        )
    }

    fn store_recovery_artifact(root: &std::path::Path, label: &str, payload: &[u8]) -> ArtifactRef {
        let blobs = root.join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        let payload_hash = ContentHash::of_bytes(payload);
        std::fs::write(blobs.join(payload_hash.to_hex()), payload).unwrap();
        let manifest = InterfaceArtifactManifest {
            label: label.to_string(),
            payload_path: root.join("unused-payload"),
            payload_content_hash: Some(payload_hash.to_hex()),
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_hash = ContentHash::of_bytes(&manifest_bytes);
        std::fs::write(blobs.join(manifest_hash.to_hex()), manifest_bytes).unwrap();
        ArtifactRef {
            id: ArtifactId::new(format!("{label}:{}", manifest_hash.to_hex())),
            content: manifest_hash,
        }
    }

    fn write_recovery_ledger(root: &std::path::Path, movements: &[Movement]) {
        let metadata = root.join("metadata");
        std::fs::create_dir_all(&metadata).unwrap();
        let mut bytes = Vec::new();
        for (line, movement) in movements.iter().enumerate() {
            let row = encode_durable_movement_row(movement, line, |_, grant| {
                let hash = ContentHash::of_bytes(grant);
                std::fs::write(metadata.join(hash.to_hex()), grant).unwrap();
                Ok(hash)
            })
            .unwrap();
            bytes.extend_from_slice(row.as_bytes());
            bytes.push(b'\n');
        }
        std::fs::write(root.join("movements.jsonl"), bytes).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn primary_input_retirement_preserves_outputs_and_durable_completion_on_failure() {
        for corrupt in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let limits = InterfaceHostIoLimits { max_control_frame_bytes: 65536, max_stderr_bytes: 65536,
                max_manifest_bytes: 65536, max_payload_bytes: 1024 * 1024 };
            let mut ctx = test_lift_context(root.path(), InterfaceId::new("system_test_door"));
            ctx.host_io_limits = Some(limits);
            let payload = vec![b'p'; 256 * 1024];
            let artifact = store_recovery_artifact(root.path(), "tool.call", &payload);
            let input_root = ctx.interface_var_dir().join("invocations");
            let wire = ToolProcessEnvelope {
                artifact: artifact.clone(), label: "tool.call".into(), payload: payload.clone(),
                artifact_bindings: vec![super::ToolProcessBoundArtifact::new(artifact.clone(), b"bound bytes".to_vec())],
                metadata: None, source_movement_seq: 0, worker_environment: None,
                worker_environment_kind: None, output_source: None,
            }.materialize_for_process(&input_root, 1, &limits).unwrap();
            std::os::unix::fs::symlink(input_root.join("1"), input_root.join("2")).unwrap();
            assert!(super::ToolProcessPrimaryInput::capture_invocation(&input_root, 2, &wire, &limits).is_err());
            let input = super::ToolProcessPrimaryInput::capture_invocation(&input_root, 1, &wire, &limits).unwrap();
            let captured = wire.clone().materialize(&input_root, 1, &limits).unwrap();
            let input_payload = captured.artifact_file.read_manifest().unwrap().payload_path;
            let output = InterfaceArtifactFile::write(wire.manifest_path.parent().unwrap().join("emitted"), "result", b"retained output").unwrap();
            let bound = wire.context.read(&input_root, 1, &limits).unwrap().artifact_bindings[0].manifest_path.clone();
            let source = Movement { seq: Seq(0), lamport: Lamport(1), recorded_at_unix_ms: 1,
                trace: TraceId("retirement".into()), kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(InterfaceId::new("worker")), to: Endpoint::Interface(ctx.interface.clone()),
                artifact: Some(artifact), invocation_metadata: None };
            write_recovery_ledger(root.path(), std::slice::from_ref(&source));
            let digest = super::tool_door_movement_digest(&source).unwrap();
            let mut reader = super::LedgerTailReader::open_named(&ctx, "retirement-test").unwrap();
            let own = reader.read_new_uncommitted_limit(1).unwrap().remove(0);
            let mut rows = BTreeMap::from([(0, super::ToolDoorPendingRow {
                own, movement_digest: digest.clone(), state: super::ToolDoorRowState::InFlight,
            })]);
            let journal_path = ctx.interface_var_dir().join(TOOL_DOOR_EFFECT_JOURNAL_FILE);
            let mut journal = ToolDoorEffectJournal::open(journal_path.clone(), 0, 65536).unwrap();
            journal.prepare(0, &digest).unwrap(); journal.begin(0, &digest).unwrap();
            if corrupt { std::fs::write(&input_payload, vec![b'x'; payload.len()]).unwrap(); }
            let result = super::record_tool_door_completion(super::ToolDoorCompletion {
                seq: 0, movement_digest: digest.clone(), result: Ok(ToolDoorJournalState::Completed), primary_input: Some(input),
            }, &mut journal, &mut rows);
            assert_eq!(result.is_err(), corrupt);
            assert_eq!(wire.manifest_path.exists(), corrupt);
            assert_eq!(input_payload.exists(), corrupt);
            assert_eq!(wire.context.path.exists(), corrupt);
            assert_eq!(output.read_payload_bytes().unwrap(), b"retained output");
            assert!(bound.exists());
            assert_eq!(captured.read_artifact_payload_bounded(limits.max_payload_bytes).unwrap(), payload);
            drop(journal);
            let recovered = ToolDoorEffectJournal::open(journal_path, 0, 65536).unwrap();
            assert_eq!(recovered.state(0, &digest).unwrap(), Some(ToolDoorJournalState::Completed));
        }
    }

    fn test_worker_environment_kind() -> WorkerEnvKindSpec {
        serde_json::from_value(serde_json::json!({
            "worker_environment_kind_id": "test.room",
            "substrate_family": "test",
            "door_driver_refs": ["test_door"],
            "tool_process_bins": ["target/debug/test-tool"],
            "worker_consent": "explicit_ceiling",
            "supported_executor_kinds": ["exec"],
            "capabilities": ["process_exec"],
            "boundary_defaults": {
                "read_root_refs": [],
                "write_root_refs": [],
                "protected_read_only_subpaths": [],
                "network_ceiling": "denied"
            },
            "lifecycle": {"requires_release": true}
        }))
        .unwrap()
    }

    fn test_process_client(
        root: &std::path::Path,
        formation: &octopus_worker_env::WorkerEnvironmentFormation,
        acknowledge_release: bool,
    ) -> ToolProcessClient {
        let script = if acknowledge_release {
            "IFS= read -r line; printf '%s\\n' '{\"kind\":\"worker_environment_released\",\"request_id\":1}'"
        } else {
            "while IFS= read -r line; do :; done"
        };
        test_process_client_with_script(root, formation, script)
    }

    fn test_process_client_with_script(
        root: &std::path::Path,
        formation: &octopus_worker_env::WorkerEnvironmentFormation,
        script: &str,
    ) -> ToolProcessClient {
        let generation_root = root.join("tool-processes");
        let generation_dir = generation_root.join("a".repeat(64));
        let stderr_path = generation_root.join("stderr.log");
        std::fs::create_dir_all(&generation_dir).unwrap();
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (_stderr_failure_tx, stderr_failure) = tokio::sync::oneshot::channel();
        ToolProcessClient {
            child,
            stdin: Some(stdin),
            stdout: BoundedJsonLineReader::new(stdout, 64 * 1024).unwrap(),
            stderr_failure,
            stderr_open: true,
            stderr_path,
            next_request_id: 1,
            input_root: generation_dir.join("invocations"),
            output_roots: vec![generation_dir.clone()],
            generation_root,
            generation_dir,
            host_io_limits: InterfaceHostIoLimits {
                max_control_frame_bytes: 64 * 1024,
                max_stderr_bytes: 64 * 1024,
                max_manifest_bytes: 64 * 1024,
                max_payload_bytes: 64 * 1024,
            },
            formation: Some(formation.clone()),
            worker_environment_kind: Some(test_worker_environment_kind()),
            last_source_movement_seq: 7,
            _sandbox_profile: None,
        }
    }

    fn test_process_envelope(
        door: &InterfaceId,
        formation: &octopus_worker_env::WorkerEnvironmentFormation,
        source_movement_seq: u64,
    ) -> ToolProcessEnvelope {
        let payload = b"{}".to_vec();
        ToolProcessEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new(format!("tool.call:{source_movement_seq}")),
                content: ContentHash::of_bytes(&payload),
            },
            label: "tool.call".to_string(),
            payload,
            artifact_bindings: Vec::new(),
            metadata: Some(
                InvocationMetadata::new(GrantProjection::default()).with_runtime(
                    RuntimeInvocationMetadata {
                        run: RunId::new("run_protocol_test"),
                        ee: EeId::new("ee_test"),
                        worker: InterfaceId::new("system_worker_test"),
                        checkpoint: CheckpointId::new("checkpoint_protocol_test"),
                        interface: door.clone(),
                        epoch: 0,
                        run_record_hash: None,
                    },
                ),
            ),
            source_movement_seq,
            worker_environment: Some(formation.clone()),
            worker_environment_kind: Some(test_worker_environment_kind()),
            output_source: None,
        }
    }

    fn test_process_key(
        formation: &octopus_worker_env::WorkerEnvironmentFormation,
    ) -> ToolProcessKey {
        ToolProcessKey {
            declaration: process_declaration(BTreeSet::new()),
            effective_declaration_hash: "b".repeat(64),
            formation_id: Some(formation.worker_environment_id.clone()),
            formation_scope: Some(formation.scope_key()),
            private_read_snapshot_hash: "c".repeat(64),
            effective_environment_hash: "d".repeat(64),
            invocation_authority_hash: "e".repeat(64),
            execution_inputs_hash: "f".repeat(64),
        }
    }

    #[tokio::test]
    async fn input_preparation_refusal_sends_no_command_and_preserves_recovery_uncertainty() {
        for recovery in [false, true] {
            for frame_limit in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let (door, formation) = test_formation("run_preparation");
                let ctx = test_lift_context(root.path(), door.clone());
                let mut client = test_process_client_with_script(root.path(), &formation, "if IFS= read -r command; then exit 23; fi");
                let mut envelope = test_process_envelope(&door, &formation, 7);
                if frame_limit {
                    client.host_io_limits.max_control_frame_bytes = 1;
                } else {
                    let path = root.path().canonicalize().unwrap().join("bad-binding");
                    std::fs::write(&path, b"corrupt").unwrap();
                    envelope.artifact_bindings.push(super::ArtifactBindingPayload::from_pinned_file(
                        envelope.artifact.clone(), path, ContentHash::of_bytes(b"expected"), 64,
                    ).unwrap());
                }
                let descriptor = load_tool_descriptor_from_dir(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../tool"), "process.list").unwrap().unwrap();
                let result = client.invoke(&ctx, descriptor, envelope, false, recovery, None).await;
                assert!(matches!(&result, Err(super::ToolProcessInvokeError::Unavailable(_))) == recovery);
                assert!(matches!(&result, Err(super::ToolProcessInvokeError::Capability(_))) != recovery);
                drop(client.stdin.take());
                assert_eq!(client.child.wait().await.unwrap().code(), Some(0));
            }
        }
    }

    #[tokio::test]
    async fn completion_counts_only_admitted_streamed_artifacts() {
        for admitted in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (door, formation) = test_formation("run_streamed_output");
            let mut ctx = test_lift_context(root.path(), door.clone());
            let (tx, mut rx) = tokio::sync::mpsc::channel::<InterfaceActivationArrival>(1);
            ctx.arrivals = tx;
            let generation = root.path().join("tool-processes").join("a".repeat(64));
            let artifact = InterfaceArtifactFile::write(generation.join("output"), "test.result", b"{}").unwrap();
            let arrival = serde_json::json!({"kind":"artifact_arrival", "request_id":1, "event_id":1, "manifest_path":artifact.manifest_path()});
            let complete = serde_json::json!({"kind":"complete", "request_id":1, "artifact_manifest_paths":[]});
            let script = format!("IFS= read -r request; printf '%s\\n' '{arrival}'; IFS= read -r verdict; printf '%s\\n' '{complete}'");
            let mut client = test_process_client_with_script(root.path(), &formation, &script);
            ctx.host_io_limits = Some(client.host_io_limits);
            let responder = tokio::spawn(async move {
                let output = rx.recv().await.unwrap();
                let verdict = if admitted { super::DeliveryVerdict::Admitted { seq: 99 } }
                    else { super::DeliveryVerdict::Refused { reason: super::RefusedReason::Policy, detail: serde_json::json!({"message":"test refusal"}) } };
                output.verdict_tx.unwrap().send(verdict).unwrap();
                rx
            });
            let tool_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../tool");
            let descriptor = load_tool_descriptor_from_dir(&tool_root, "process.list").unwrap().unwrap();
            let completed = tokio::time::timeout(std::time::Duration::from_secs(2),
                client.invoke(&ctx, descriptor, test_process_envelope(&door, &formation, 7), false, false, None))
                .await.expect("streamed result must complete after its verdict").unwrap();
            assert!(completed.artifacts.is_empty(), "an admitted output must not be handed twice");
            assert_eq!(completed.streamed_output_admitted, admitted);
            assert_eq!(completed.primary_input.is_some(), admitted);
            assert!(responder.await.unwrap().try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn deferred_room_live_state_cannot_poison_the_next_tool_invocation() {
        let root = tempfile::tempdir().unwrap();
        let (door, formation) = test_formation("run_deferred_live_state");
        let ctx = test_lift_context(root.path(), door.clone());
        std::fs::create_dir_all(ctx.interface_var_dir()).unwrap();
        let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
        records.activate(&formation, 7).unwrap();
        let first_complete = serde_json::json!({
            "kind": "complete",
            "request_id": 1,
            "artifact_manifest_paths": []
        });
        let deferred_live_state = serde_json::json!({
            "kind": "worker_environment_live_state",
            "request_id": 1,
            "event_id": 91,
            "worker_environment_id": formation.worker_environment_id.as_str(),
            "state": {"status": "exited"}
        });
        let second_complete = serde_json::json!({
            "kind": "complete",
            "request_id": 2,
            "artifact_manifest_paths": []
        });
        let script = format!(
            "IFS= read -r first; printf '%s\\n' '{}'; printf '%s\\n' '{}'; IFS= read -r second; IFS= read -r acknowledgement; printf '%s\\n' '{}'",
            first_complete, deferred_live_state, second_complete
        );
        let mut client = test_process_client_with_script(root.path(), &formation, &script);
        let tool_root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../tool");
        let descriptor = load_tool_descriptor_from_dir(&tool_root, "process.list")
            .unwrap()
            .unwrap();

        let first = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.invoke(
                &ctx,
                descriptor.clone(),
                test_process_envelope(&door, &formation, 7),
                false,
                false,
                None,
            ),
        )
        .await
        .expect("first invocation must complete")
        .unwrap();
        assert!(first.artifacts.is_empty() && !first.streamed_output_admitted);
        let second = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.invoke(
                &ctx,
                descriptor,
                test_process_envelope(&door, &formation, 8),
                false,
                false,
                None,
            ),
        )
        .await
        .expect("second invocation must not hang behind deferred room state")
        .unwrap();
        assert!(second.artifacts.is_empty() && !second.streamed_output_admitted);
        assert_eq!(
            records
                .read_live_state(&formation.worker_environment_id)
                .unwrap()
                .unwrap(),
            serde_json::json!({"status": "exited"})
        );
    }

    #[tokio::test]
    async fn host_shutdown_detaches_and_same_formation_can_restart() {
        let root = tempfile::tempdir().unwrap();
        let (door, formation) = test_formation("run_coma");
        let ctx = test_lift_context(root.path(), door);
        std::fs::create_dir_all(ctx.interface_var_dir()).unwrap();
        let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
        records.activate(&formation, 7).unwrap();
        let mut state = ToolProcessBrokerState::default();
        state.processes.insert(
            test_process_key(&formation),
            test_process_client(root.path(), &formation, false),
        );

        state.shutdown(&ctx, false).await.unwrap();

        assert_eq!(
            records
                .read(&formation.worker_environment_id)
                .unwrap()
                .lifecycle,
            WorkerEnvironmentLifecycle::Active
        );
        assert!(refuse_released_worker_environment(&ctx, &formation).is_ok());
        records.activate(&formation, 8).unwrap();
    }

    #[tokio::test]
    async fn abrupt_host_drop_preserves_the_formed_environment_for_recovery() {
        let root = tempfile::tempdir().unwrap();
        let (door, formation) = test_formation("run_crash");
        let ctx = test_lift_context(root.path(), door);
        std::fs::create_dir_all(ctx.interface_var_dir()).unwrap();
        let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
        records.activate(&formation, 7).unwrap();
        let mut state = ToolProcessBrokerState::default();
        state.processes.insert(
            test_process_key(&formation),
            test_process_client(root.path(), &formation, false),
        );

        drop(state);

        assert_eq!(
            records
                .read(&formation.worker_environment_id)
                .unwrap()
                .lifecycle,
            WorkerEnvironmentLifecycle::Active
        );
        assert!(refuse_released_worker_environment(&ctx, &formation).is_ok());
    }

    #[tokio::test]
    async fn explicit_terminal_release_remains_terminal() {
        let root = tempfile::tempdir().unwrap();
        let (door, formation) = test_formation("run_release");
        let ctx = test_lift_context(root.path(), door);
        std::fs::create_dir_all(ctx.interface_var_dir()).unwrap();
        let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
        records.activate(&formation, 7).unwrap();
        let mut state = ToolProcessBrokerState::default();
        state.processes.insert(
            test_process_key(&formation),
            test_process_client(root.path(), &formation, true),
        );

        state.shutdown(&ctx, true).await.unwrap();

        assert_eq!(
            records
                .read(&formation.worker_environment_id)
                .unwrap()
                .lifecycle,
            WorkerEnvironmentLifecycle::Released
        );
        assert!(refuse_released_worker_environment(&ctx, &formation).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cold_owner_quiescence_is_exact_to_one_formation() {
        let root = tempfile::tempdir().unwrap();
        let generation_root = root.path().join("generations");
        std::fs::create_dir(&generation_root).unwrap();
        let first_generation = generation_root.join("a".repeat(64));
        let second_generation = generation_root.join("b".repeat(64));
        std::fs::create_dir(&first_generation).unwrap();
        std::fs::create_dir(&second_generation).unwrap();
        let first_id = WorkerEnvironmentId::parse("we_11111111111111111111111111111111").unwrap();
        let second_id = WorkerEnvironmentId::parse("we_22222222222222222222222222222222").unwrap();

        let spawn_owner = |generation: &std::path::Path,
                           formation: &WorkerEnvironmentId|
         -> tokio::process::Child {
            let lease = prepare_tool_process_owner_lease(generation).unwrap();
            let mut command = tokio::process::Command::new("/bin/sleep");
            command
                .arg("60")
                .process_group(0)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            configure_tool_process_owner_lease(&mut command, &lease);
            let child = command.spawn().unwrap();
            publish_tool_process_owner(generation, child.id().unwrap(), Some(formation.as_str()))
                .unwrap();
            drop(lease);
            child
        };

        let mut first = spawn_owner(&first_generation, &first_id);
        let mut second = spawn_owner(&second_generation, &second_id);
        assert!(remove_tool_process_generation(&generation_root, &first_generation).is_err());

        let first_wait = tokio::spawn(async move { first.wait().await });
        quiesce_worker_environment_owner_generations(&generation_root, &first_id)
            .await
            .unwrap();
        first_wait.await.unwrap().unwrap();
        assert!(!first_generation.exists());
        assert!(second_generation.exists());
        assert!(second.try_wait().unwrap().is_none());

        let second_group = second.id().unwrap() as i32;
        let second_wait = tokio::spawn(async move { second.wait().await });
        signal_tool_process_group(second_group, libc::SIGKILL).unwrap();
        second_wait.await.unwrap().unwrap();
        quiesce_worker_environment_owner_generations(&generation_root, &second_id)
            .await
            .unwrap();
        assert!(!second_generation.exists());
    }

    #[test]
    fn durable_state_is_stable_private_and_formation_scoped() {
        let root = tempfile::tempdir().unwrap();
        let (door, formation) = test_formation("run_test");
        let ctx = test_lift_context(root.path(), door);
        std::fs::create_dir_all(ctx.interface_var_dir()).unwrap();
        let first = materialize_tool_process_durable_dir(&ctx, Some(&formation))
            .unwrap()
            .unwrap();
        std::fs::write(first.join("state"), b"durable").unwrap();
        let second = materialize_tool_process_durable_dir(&ctx, Some(&formation))
            .unwrap()
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(std::fs::read(second.join("state")).unwrap(), b"durable");
        let host_private_root = tool_process_host_private_root(&ctx).unwrap();
        let durable_parent = host_private_root.join("worker-environment-state");
        assert_eq!(second.parent(), Some(durable_parent.as_path()));
        assert_eq!(
            validate_tool_process_durable_var_dir(second.clone()).unwrap(),
            second
        );

        let (_, other_formation) = test_formation("run_other");
        let other = materialize_tool_process_durable_dir(&ctx, Some(&other_formation))
            .unwrap()
            .unwrap();
        assert_ne!(first, other);
    }

    #[test]
    fn interface_local_ipc_root_is_broker_private_not_worker_environment_authority() {
        let root = tempfile::tempdir().unwrap();
        let ipc_root = root.path().join("interface-ipc");
        std::fs::create_dir(&ipc_root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ipc_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let ipc_root = std::fs::canonicalize(ipc_root).unwrap();
        let (door, formation) = test_formation("run_private_ipc");
        let mut ctx = test_lift_context(root.path(), door);
        ctx.root_slots
            .insert(INTERFACE_LOCAL_IPC_ROOT_SLOT.to_string(), ipc_root.clone());

        assert_eq!(
            tool_process_interface_local_ipc_root(&ctx).unwrap(),
            Some(ipc_root)
        );

        let mut declared = process_declaration(BTreeSet::new());
        declared.authority.write_root_slots =
            BTreeSet::from([INTERFACE_LOCAL_IPC_ROOT_SLOT.to_string()]);
        declared.authority.optional_write_root_slots =
            BTreeSet::from([INTERFACE_LOCAL_IPC_ROOT_SLOT.to_string()]);
        declared.authority.allow_local_ipc = true;
        declared.coordinate_root_slots =
            BTreeSet::from([INTERFACE_LOCAL_IPC_ROOT_SLOT.to_string()]);
        let narrowed = narrow_process_declaration(&declared, Some(&formation), None).unwrap();

        assert!(narrowed.authority.write_root_slots.is_empty());
        assert!(narrowed.authority.optional_write_root_slots.is_empty());
        assert!(narrowed.coordinate_root_slots.is_empty());
        assert!(narrowed.authority.allow_local_ipc);
    }

    #[cfg(unix)]
    #[test]
    fn durable_state_refuses_a_symlinked_private_root() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let escape = tempfile::tempdir().unwrap();
        let (door, formation) = test_formation("run_test");
        let ctx = test_lift_context(root.path(), door);
        std::fs::create_dir_all(ctx.interface_var_dir()).unwrap();
        let host_private_root = tool_process_host_private_root(&ctx).unwrap();
        symlink(
            escape.path(),
            host_private_root.join("worker-environment-state"),
        )
        .unwrap();
        let error = materialize_tool_process_durable_dir(&ctx, Some(&formation)).unwrap_err();
        assert!(error.contains("cannot be opened"));
        assert_eq!(std::fs::read_dir(escape.path()).unwrap().count(), 0);
    }

    #[test]
    fn interface_config_cannot_select_the_host_durable_state_env() {
        let root = tempfile::tempdir().unwrap();
        let descriptor = serde_json::from_value(serde_json::json!({
            "id": "system_test_door_instance",
            "interface_type": "system",
            "role": "system",
            "direction": "bidirectional",
            "source_names": ["worker_environment"],
            "interface_driver_ref": "test",
            "interface_driver_config": {
                "private_config": {
                    "process_env_keys": ["OCTOPUS_TOOL_PROCESS_DURABLE_VAR_DIR"]
                }
            }
        }))
        .unwrap();
        let (arrivals, _receiver) = tokio::sync::mpsc::channel::<InterfaceActivationArrival>(1);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("system_test_door_instance"),
            root.path()
                .join("registry/interface/system_test_door_instance"),
            Some(descriptor),
            root.path().join("var"),
            root.path().join("movements.jsonl"),
            root.path().join("blobs"),
            root.path().join("ledger-index.sqlite"),
            Default::default(),
            arrivals,
            tokio_util::sync::CancellationToken::new(),
        );
        let mut declaration = process_declaration(BTreeSet::new());
        declaration
            .inherit_env_from_interface
            .insert("/interface_driver_config/private_config/process_env_keys".to_string());
        let error = effective_tool_process_environment(&ctx, &declaration).unwrap_err();
        assert!(error.contains("selects reserved name"));
    }

    #[cfg(unix)]
    #[test]
    fn private_read_declaration_materializes_only_a_regular_nofollow_leaf() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().unwrap();
        let generations = tempfile::tempdir().unwrap();
        let secrets = source.path().join("secrets");
        std::fs::create_dir(&secrets).unwrap();
        let auth = secrets.join("room.auth");
        std::fs::write(&auth, b"opaque").unwrap();
        let declared = BTreeSet::from([ToolProcessPrivateReadSubpath {
            path: "secrets/room.auth".to_string(),
        }]);
        let process = process_declaration(declared.clone());
        let expected_hash = private_read_snapshot_hash(source.path(), &declared, 64).unwrap();
        let wider_hash = private_read_snapshot_hash(source.path(), &declared, 128).unwrap();
        assert_ne!(expected_hash, wider_hash);
        let environment_hash = tool_process_environment_hash(&BTreeMap::new());
        let authority_hash = empty_invocation_authority_hash();
        let old_generation = tool_process_generation_key(
            &canonical_hash(&process).unwrap(),
            None,
            &expected_hash,
            &environment_hash,
            &authority_hash,
            "execution-inputs",
        )
        .unwrap();
        let old = materialize_private_read_generation(
            source.path(),
            generations.path(),
            &old_generation,
            &declared,
            64,
            &expected_hash,
        )
        .unwrap();
        let snapshot = old.private_reads["secrets/room.auth"].clone();
        assert_eq!(old.exact_paths, vec![snapshot.clone()]);
        assert!(!snapshot.starts_with(&old.state_dir));
        assert_eq!(std::fs::read(&snapshot).unwrap(), b"opaque");
        assert_eq!(
            std::fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::write(&auth, b"rotated").unwrap();
        let rotated_hash = private_read_snapshot_hash(source.path(), &declared, 64).unwrap();
        let new_generation = tool_process_generation_key(
            &canonical_hash(&process).unwrap(),
            None,
            &rotated_hash,
            &environment_hash,
            &authority_hash,
            "execution-inputs",
        )
        .unwrap();
        let new = materialize_private_read_generation(
            source.path(),
            generations.path(),
            &new_generation,
            &declared,
            64,
            &rotated_hash,
        )
        .unwrap();
        assert_ne!(old.state_dir, new.state_dir);
        assert_eq!(std::fs::read(&snapshot).unwrap(), b"opaque");
        assert_eq!(
            std::fs::read(&new.private_reads["secrets/room.auth"]).unwrap(),
            b"rotated"
        );

        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), secrets.join("linked.auth")).unwrap();
        let linked = BTreeSet::from([ToolProcessPrivateReadSubpath {
            path: "secrets/linked.auth".to_string(),
        }]);
        assert!(private_read_snapshot_hash(source.path(), &linked, 64).is_err());

        let generation_root = std::fs::canonicalize(generations.path()).unwrap();
        let old_generation_dir = old.state_dir.parent().unwrap().to_path_buf();
        let new_generation_dir = new.state_dir.parent().unwrap().to_path_buf();
        reconcile_tool_process_generations(
            &generation_root,
            &BTreeSet::from([new_generation.clone()]),
        )
        .unwrap();
        assert!(!old_generation_dir.exists());
        assert!(new_generation_dir.exists());
        reconcile_tool_process_generations(&generation_root, &BTreeSet::new()).unwrap();
        assert!(!new_generation_dir.exists());
    }

    #[test]
    fn private_read_declaration_refuses_bytes_above_its_ceiling() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("oversized"), b"1234567").unwrap();
        let declared = BTreeSet::from([ToolProcessPrivateReadSubpath {
            path: "oversized".to_string(),
        }]);
        let error = private_read_snapshot_hash(source.path(), &declared, 6).unwrap_err();
        assert!(error.contains("exceeds owning life-process ceiling of 6 bytes"));
    }

    #[test]
    fn changed_private_inputs_never_publish_a_partial_generation() {
        let source = tempfile::tempdir().unwrap();
        let generations = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("secret"), b"before").unwrap();
        let declared = BTreeSet::from([ToolProcessPrivateReadSubpath {
            path: "secret".to_string(),
        }]);
        let process = process_declaration(declared.clone());
        let expected = private_read_snapshot_hash(source.path(), &declared, 64).unwrap();
        let generation = tool_process_generation_key(
            &canonical_hash(&process).unwrap(),
            None,
            &expected,
            &tool_process_environment_hash(&BTreeMap::new()),
            &empty_invocation_authority_hash(),
            "execution-inputs",
        )
        .unwrap();
        std::fs::write(source.path().join("secret"), b"after").unwrap();

        let error = materialize_private_read_generation(
            source.path(),
            generations.path(),
            &generation,
            &declared,
            64,
            &expected,
        )
        .err()
        .unwrap();

        assert!(error.contains("changed during spawn"));
        assert!(!generations.path().join(generation).exists());
    }

    #[test]
    fn superseded_process_key_retires_every_tool_reference() {
        let declaration = process_declaration(BTreeSet::new());
        let formation_id = octopus_worker_env::WorkerEnvironmentId::derive(b"room");
        let old = ToolProcessKey {
            declaration: declaration.clone(),
            effective_declaration_hash: canonical_hash(&declaration).unwrap(),
            formation_id: Some(formation_id.clone()),
            formation_scope: Some("scope".to_string()),
            private_read_snapshot_hash: "a".repeat(64),
            effective_environment_hash: "c".repeat(64),
            invocation_authority_hash: "d".repeat(64),
            execution_inputs_hash: "f".repeat(64),
        };
        let new = ToolProcessKey {
            effective_declaration_hash: canonical_hash(&declaration).unwrap(),
            declaration,
            formation_id: Some(formation_id),
            formation_scope: Some("scope".to_string()),
            private_read_snapshot_hash: "b".repeat(64),
            effective_environment_hash: "c".repeat(64),
            invocation_authority_hash: "d".repeat(64),
            execution_inputs_hash: "f".repeat(64),
        };
        let state = Arc::new(tokio::sync::Mutex::new(ToolProcessBrokerState::default()));
        let mut shards = ToolProcessBrokerShards::default();
        shards.by_key.insert(old.clone(), state);
        shards
            .by_tool
            .insert("remote.exec".to_string(), BTreeSet::from([old.clone()]));
        shards
            .by_tool
            .insert("remote.status".to_string(), BTreeSet::from([old.clone()]));

        let (_, stale, sole_formation_key) = shards.route("remote.exec".to_string(), new.clone());

        assert_eq!(stale.len(), 1);
        assert!(!stale[0].1);
        assert!(sole_formation_key);
        assert!(!shards.by_key.contains_key(&old));
        assert!(shards.by_key.contains_key(&new));
        assert_eq!(
            shards.by_tool.get("remote.exec"),
            Some(&BTreeSet::from([new]))
        );
        assert!(!shards.by_tool.contains_key("remote.status"));
    }

    #[test]
    fn equal_invocation_authority_reuses_a_shard_and_changed_authority_retires_it() {
        let declaration = process_declaration(BTreeSet::new());
        let key = ToolProcessKey {
            effective_declaration_hash: canonical_hash(&declaration).unwrap(),
            declaration,
            formation_id: None,
            formation_scope: Some("scope".to_string()),
            private_read_snapshot_hash: "a".repeat(64),
            effective_environment_hash: "b".repeat(64),
            invocation_authority_hash: "c".repeat(64),
            execution_inputs_hash: "f".repeat(64),
        };
        let mut shards = ToolProcessBrokerShards::default();
        let (first, stale, _) = shards.route("file.read".to_string(), key.clone());
        assert!(stale.is_empty());
        let (same, stale, _) = shards.route("file.read".to_string(), key.clone());
        assert!(Arc::ptr_eq(&first, &same));
        assert!(stale.is_empty());
        let changed = ToolProcessKey {
            invocation_authority_hash: "d".repeat(64),
            ..key
        };
        let (replacement, stale, _) = shards.route("file.read".to_string(), changed);
        assert!(!Arc::ptr_eq(&same, &replacement));
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn effective_environment_values_participate_in_process_identity() {
        let first = BTreeMap::from([(
            "DRIVER_SECRET".to_string(),
            std::ffi::OsString::from("first-secret"),
        )]);
        let second = BTreeMap::from([(
            "DRIVER_SECRET".to_string(),
            std::ffi::OsString::from("rotated-secret"),
        )]);
        let first_hash = tool_process_environment_hash(&first);
        let second_hash = tool_process_environment_hash(&second);
        assert_ne!(first_hash, second_hash);
        assert!(!first_hash.contains("first-secret"));
        assert!(!second_hash.contains("rotated-secret"));
    }

    #[test]
    fn resolved_invocation_paths_participate_in_process_identity() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let authority = ProcessAuthoritySpec {
            read_argument_paths: vec![ProcessArgumentPathSpec {
                pointer: "/path".to_string(),
                within_root_slots: BTreeSet::from(["room".to_string()]),
                include_parent: false,
            }],
            ..ProcessAuthoritySpec::default()
        };
        let root_slots = BTreeMap::from([(
            "room".to_string(),
            std::fs::canonicalize(root.path()).unwrap(),
        )]);
        let first_hash = invocation_authority_hash(
            &authority,
            &root_slots,
            &serde_json::json!({"path": first, "unrelated": 1}),
        )
        .unwrap();
        let same_hash = invocation_authority_hash(
            &authority,
            &root_slots,
            &serde_json::json!({"path": first, "unrelated": 2}),
        )
        .unwrap();
        let second_hash = invocation_authority_hash(
            &authority,
            &root_slots,
            &serde_json::json!({"path": second}),
        )
        .unwrap();
        assert_eq!(first_hash, same_hash);
        assert_ne!(first_hash, second_hash);
    }

    #[test]
    fn released_formation_respects_the_declared_release_contract() {
        for requires_release in [true, false] {
            let mut registry = WorkerEnvKindRegistry::default();
            registry
                .upsert(
                    &serde_json::to_vec(&serde_json::json!({
                        "worker_environment_kind_id": "test.room",
                        "substrate_family": "test",
                        "door_driver_refs": ["test_door"],
                        "tool_process_bins": ["target/debug/test-tool"],
                        "worker_consent": "explicit_ceiling",
                        "supported_executor_kinds": ["exec"],
                        "capabilities": ["process_exec"],
                        "boundary_defaults": {
                            "read_root_refs": [],
                            "write_root_refs": [],
                            "protected_read_only_subpaths": [],
                            "network_ceiling": "denied"
                        },
                        "lifecycle": {"requires_release": requires_release}
                    }))
                    .unwrap(),
                )
                .unwrap();
            let door = InterfaceId::new("system_test_door");
            let worker = InterfaceId::new("system_worker_test");
            let kind = WorkerEnvironmentKindId::from("test.room");
            let kinds = vec![kind.clone()];
            let tools = vec!["test.tool".to_string()];
            let tool_hashes = BTreeMap::from([("test.tool".to_string(), "hash-tool".to_string())]);
            let tool_process_bins = BTreeMap::from([(
                "test.tool".to_string(),
                "target/debug/test-tool".to_string(),
            )]);
            let roots = BTreeMap::new();
            let formation = registry
                .form(&WorkerEnvironmentFormationRequest {
                    door_additional_root_bindings: &Default::default(),
                    door_interface: &door,
                    door_descriptor_hash: "hash-door",
                    door_driver_ref: "test_door",
                    tool: "test.tool",
                    tool_descriptor_hashes: &tool_hashes,
                    tool_process_bins: &tool_process_bins,
                    tool_worker_environment_kinds: &kinds,
                    worker: &worker,
                    ee: "ee_test",
                    run: "run_test",
                    granted_worker_environment_kinds: &kinds,
                    granted_tools: &tools,
                    door_root_bindings: &roots,
                    door_instance_dir: std::path::Path::new("."),
                    ee_root_slot_allowance: None,
                })
                .unwrap();
            let root = tempfile::tempdir().unwrap();
            let (arrivals, _receiver) = tokio::sync::mpsc::channel::<InterfaceActivationArrival>(1);
            let ctx = InterfaceLiftContext::unprivileged(
                door,
                root.path()
                    .join("registry/interface/system_test_door_instance"),
                None,
                root.path().join("var"),
                root.path().join("movements.jsonl"),
                root.path().join("blobs"),
                root.path().join("ledger-index.sqlite"),
                Default::default(),
                arrivals,
                tokio_util::sync::CancellationToken::new(),
            );
            let records = WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
            records.activate(&formation, 1).unwrap();
            assert!(refuse_released_worker_environment(&ctx, &formation).is_ok());
            records.release(&formation, 2).unwrap();
            if requires_release {
                assert!(refuse_released_worker_environment(&ctx, &formation).is_err());
                assert!(records.activate(&formation, 3).is_err());
            } else {
                assert!(refuse_released_worker_environment(&ctx, &formation).is_ok());
                records.activate(&formation, 3).unwrap();
                assert_eq!(
                    records
                        .read(&formation.worker_environment_id)
                        .unwrap()
                        .lifecycle,
                    WorkerEnvironmentLifecycle::Active
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn writable_process_state_cannot_overwrite_private_snapshot() {
        use std::ffi::OsString;

        let source = tempfile::tempdir().unwrap();
        let generations = tempfile::tempdir().unwrap();
        let profiles = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("secret"), b"opaque").unwrap();
        let declared = BTreeSet::from([ToolProcessPrivateReadSubpath {
            path: "secret".to_string(),
        }]);
        let process = process_declaration(declared.clone());
        let hash = private_read_snapshot_hash(source.path(), &declared, 64).unwrap();
        let generation = tool_process_generation_key(
            &canonical_hash(&process).unwrap(),
            None,
            &hash,
            &tool_process_environment_hash(&BTreeMap::new()),
            &empty_invocation_authority_hash(),
            "execution-inputs",
        )
        .unwrap();
        let materialized = materialize_private_read_generation(
            source.path(),
            generations.path(),
            &generation,
            &declared,
            64,
            &hash,
        )
        .unwrap();
        let snapshot = materialized.private_reads["secret"].clone();
        let writable = materialized.state_dir.join("writable");
        let shell = std::path::PathBuf::from("/bin/sh");
        let authority = super::ProcessAuthority::resolve(
            std::iter::empty(),
            [materialized.state_dir.clone()],
            [shell.clone()],
            materialized.exact_paths,
            std::iter::empty(),
            std::iter::empty(),
            false,
            false,
            false,
            false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        )
        .unwrap();
        let args = vec![
            OsString::from("-c"),
            OsString::from("printf tampered > \"$1\"; printf writable > \"$2\""),
            OsString::from("octopus-private-boundary"),
            snapshot.clone().into_os_string(),
            writable.clone().into_os_string(),
        ];
        let prepared = authority
            .prepare("/bin/sh", &args, generations.path(), None, profiles.path())
            .unwrap();
        let _ = std::process::Command::new(&prepared.program)
            .args(&prepared.args)
            .env_clear()
            .envs(&prepared.environment)
            .current_dir(generations.path())
            .status()
            .unwrap();
        assert_eq!(std::fs::read(snapshot).unwrap(), b"opaque");
        assert_eq!(std::fs::read(writable).unwrap(), b"writable");
    }

    #[test]
    fn effect_journal_survives_restart_and_binds_every_phase_to_movement_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(TOOL_DOOR_EFFECT_JOURNAL_FILE);
        let digest = "a".repeat(64);
        let mut journal = ToolDoorEffectJournal::open(path.clone(), 0, 64 * 1024).unwrap();
        journal.prepare(9, &digest).unwrap();
        journal.begin(9, &digest).unwrap();

        let mut reopened = ToolDoorEffectJournal::open(path.clone(), 0, 64 * 1024).unwrap();
        assert_eq!(
            reopened.state(9, &digest).unwrap(),
            Some(ToolDoorJournalState::InFlight)
        );
        assert!(reopened.state(9, &"b".repeat(64)).is_err());
        reopened
            .finish(9, &digest, ToolDoorJournalState::Indeterminate)
            .unwrap();
        assert_eq!(
            reopened.state(9, &digest).unwrap(),
            Some(ToolDoorJournalState::Indeterminate)
        );
        reopened.prepare(10, &digest).unwrap();
        reopened.begin(10, &digest).unwrap();
        reopened
            .finish(10, &digest, ToolDoorJournalState::Indeterminate)
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        reopened.prune_before(10).unwrap();
        let compacted = ToolDoorEffectJournal::open(path.clone(), 10, 64 * 1024).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert_eq!(compacted.state(9, &digest).unwrap(), Some(ToolDoorJournalState::Indeterminate));
        assert_eq!(
            compacted.state(10, &digest).unwrap(),
            Some(ToolDoorJournalState::Indeterminate)
        );
        use super::{ToolDoorAction, ToolDoorArtifactDecodeError as DecodeError, ToolDoorArtifactDecodePurpose as Purpose};
        for (purpose, calls) in [(Purpose::Dispatch, vec![]), (Purpose::Dispatch, vec![2, 2]), (Purpose::Dispatch, vec![2, 1]), (Purpose::HistoricalIdentity, vec![1])] {
            assert!(matches!(DecodeError::AwaitingResults { call_sequences: calls }.into_action(purpose), ToolDoorAction::Failure { class: "invalid_tool_artifact", .. }));
        }
    }

    #[test]
    fn in_flight_recovery_accepts_one_exact_admitted_result_and_refuses_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let door = InterfaceId::new("system_test_door");
        let mut ctx = test_lift_context(root.path(), door.clone());
        ctx.host_io_limits = Some(InterfaceHostIoLimits {
            max_control_frame_bytes: 64 * 1024,
            max_stderr_bytes: 64 * 1024,
            max_manifest_bytes: 64 * 1024,
            max_payload_bytes: 64 * 1024,
        });
        let call = ToolCallArtifact {
            tool: "test.tool".to_string(),
            arguments: serde_json::json!({}),
            correlation_id: Some("call-1".to_string()),
            label: Some("tool.call".to_string()),
            source_interface_evidence: BTreeMap::new(),
        };
        let call_payload = serde_json::to_vec(&call).unwrap();
        let call_artifact = store_recovery_artifact(root.path(), "tool.call", &call_payload);
        let result_payload = serde_json::to_vec(&ToolResultArtifact::success(
            "test.tool",
            Some("call-1".to_string()),
            serde_json::json!({"changed": true}),
        ))
        .unwrap();
        let result_artifact =
            store_recovery_artifact(root.path(), "test.tool.result", &result_payload);
        let source_metadata = InvocationMetadata::new(GrantProjection::default()).with_runtime(
            RuntimeInvocationMetadata {
                run: RunId::new("run_recovery"),
                ee: EeId::new("ee_test"),
                worker: InterfaceId::new("system_worker_test"),
                checkpoint: CheckpointId::new("checkpoint_call"),
                interface: door.clone(),
                epoch: 0,
                run_record_hash: None,
            },
        );
        let result_metadata = InvocationMetadata::new(GrantProjection::default()).with_runtime(
            RuntimeInvocationMetadata {
                checkpoint: CheckpointId::new("checkpoint_result"),
                ..source_metadata.runtime().unwrap().clone()
            },
        );
        let source = Movement {
            seq: Seq(0),
            lamport: Lamport(1),
            recorded_at_unix_ms: 1,
            trace: TraceId("trace:recovery".to_string()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_test")),
            to: Endpoint::Interface(door.clone()),
            artifact: Some(call_artifact.clone()),
            invocation_metadata: Some(source_metadata.clone().into()),
        };
        let result = Movement {
            seq: Seq(1),
            lamport: Lamport(2),
            recorded_at_unix_ms: 2,
            trace: TraceId("trace:recovery".to_string()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(door),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(result_artifact),
            invocation_metadata: Some(result_metadata.into()),
        };
        let envelope = InterfaceInvocationEnvelope {
            artifact: call_artifact,
            artifact_file: InterfaceArtifactFile::new(root.path().join("unused-manifest")).unwrap(),
            artifact_binding_payloads: Vec::new(),
            metadata: Some(source_metadata),
        };
        write_recovery_ledger(root.path(), &[source.clone(), result.clone()]);
        assert!(
            tool_door_has_exact_admitted_result(
                &ctx,
                &source,
                &call,
                &envelope,
                default_tool_door_artifact_decoder
            )
            .unwrap()
        );
        let mut foreign = result.clone();
        foreign.seq = Seq(0);
        let mut foreign_metadata = result
            .invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        foreign_metadata.runtime.as_mut().unwrap().run = RunId::new("another_run");
        foreign.invocation_metadata = Some(foreign_metadata.into());
        let mut later_source = source.clone();
        later_source.seq = Seq(1);
        for include_result in [false, true] {
            let mut movements = vec![foreign.clone(), later_source.clone()];
            if include_result {
                let mut exact = result.clone();
                exact.seq = Seq(2);
                movements.push(exact);
                let mut foreign_tail = foreign.clone();
                foreign_tail.seq = Seq(3);
                movements.push(foreign_tail);
            }
            write_recovery_ledger(root.path(), &movements);
            assert_eq!(tool_door_has_exact_admitted_result(
                &ctx, &later_source, &call, &envelope, default_tool_door_artifact_decoder,
            ).unwrap(), include_result);
        }
        let mut duplicate = result.clone();
        duplicate.seq = Seq(2);
        duplicate.lamport = Lamport(3);
        write_recovery_ledger(root.path(), &[source.clone(), result, duplicate]);
        assert!(
            tool_door_has_exact_admitted_result(
                &ctx,
                &source,
                &call,
                &envelope,
                default_tool_door_artifact_decoder
            )
            .is_err()
        );
        fn owner_decoder(
            ctx: &InterfaceLiftContext,
            envelope: &InterfaceInvocationEnvelope,
            purpose: super::ToolDoorArtifactDecodePurpose,
        ) -> Result<Option<ToolCallArtifact>, super::ToolDoorArtifactDecodeError> {
            assert_eq!(
                purpose,
                super::ToolDoorArtifactDecodePurpose::HistoricalIdentity,
            );
            if envelope
                .artifact_file
                .read_manifest()
                .map_err(|error| error.to_string())?
                .label
                != "operator.approval.response"
            {
                return default_tool_door_artifact_decoder(ctx, envelope, purpose);
            }
            let metadata = envelope.metadata().map_err(|error| error.to_string())?;
            if metadata
                .checkpoint_admission
                .as_ref()
                .is_none_or(|admission| !admission.complete)
                || metadata.verification_failure.is_some()
                || metadata.required_verified_artifacts.len() != 1
                || metadata.required_verified_artifacts[0].params
                    != serde_json::json!({"request_id":"request-1","call_id":"call-1"})
            {
                return Err("owner decoder requires exact admitted evidence".into());
            }
            let [bound] = envelope.artifact_binding_payloads.as_slice() else {
                return Err("owner decoder lost its pinned original call".into());
            };
            serde_json::from_slice(&bound.read_bytes().expect("bound bytes"))
                .map(Some)
                .map_err(|error| super::ToolDoorArtifactDecodeError::Invalid(error.to_string()))
        }
        let approval_artifact = store_recovery_artifact(root.path(), "operator.approval.response", br#"{"operator_approval_response":{"request_id":"request-1","call_id":"call-1","decision":"accept"}}"#);
        let mut approval = source.clone();
        approval.seq = Seq(1);
        approval.lamport = Lamport(2);
        approval.artifact = Some(approval_artifact);
        let mut metadata = envelope.metadata().unwrap().clone();
        metadata.runtime.as_mut().unwrap().checkpoint =
            CheckpointId::new("removed_approval_checkpoint");
        metadata.artifact_binding_refs = vec![envelope.artifact.clone()];
        metadata.required_verified_artifacts = vec![serde_json::from_value(serde_json::json!({"mark":"operator.approval","artifact_label":"operator.approval.response","verifier":"operator_approval","params":{"request_id":"request-1","call_id":"call-1"}})).unwrap()];
        metadata.checkpoint_admission = Some(octopus_interfaces::CheckpointAdmissionMetadata {
            artifact_label: "operator.approval.response".into(),
            required_artifact_labels: vec!["operator.approval.response".into()],
            complete: true,
        });
        approval.invocation_metadata = Some(metadata.into());
        write_recovery_ledger(root.path(), &[source.clone(), approval]);
        let error =
            tool_door_has_exact_admitted_result(&ctx, &source, &call, &envelope, owner_decoder)
                .unwrap_err();
        assert!(error.contains("duplicate source call identity"), "{error}");
    }
}

#[cfg(test)]
mod operator_chat_file_write_root_formation_tests {
    use super::narrow_process_authority_to_root_slots;
    use octopus_tool_registry::load_tool_descriptor_from_dir;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn registry_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../..")
    }

    fn file_write_authority() -> octopus_tool_registry::ProcessAuthoritySpec {
        load_tool_descriptor_from_dir(&registry_root().join("tool"), "file.write")
            .expect("file.write descriptor loads")
            .expect("file.write descriptor is present")
            .process
            .authority
    }

    fn operator_interaction_root_allowance() -> BTreeSet<String> {
        let template = registry_root()
            .join("ee_template")
            .join("operator_interaction")
            .join("ee_template.json");
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&template).expect("operator_interaction template on disk"),
        )
        .expect("operator_interaction template is JSON");
        value
            .get("allowed_root_slots")
            .and_then(serde_json::Value::as_array)
            .expect("operator_interaction declares allowed_root_slots")
            .iter()
            .map(|slot| slot.as_str().expect("root slot is a string").to_string())
            .collect()
    }

    #[test]
    fn operator_chat_file_write_narrows_to_the_ee_authoring_roots() {
        let mut authority = file_write_authority();
        authority.sandbox_mode = octopus_descriptors::ProcessSandboxMode::Disabled;
        let allowance = operator_interaction_root_allowance();
        let formed = narrow_process_authority_to_root_slots(&authority, &allowance)
            .expect("the operator-chat EE admits its run-record authoring root");
        assert_eq!(formed.sandbox_mode, authority.sandbox_mode);
        let within = &formed.write_argument_paths[0].within_root_slots;
        let expected = authority.write_argument_paths[0]
            .within_root_slots
            .intersection(&allowance)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            within, &expected,
            "the formed file.write authority must be the descriptor and EE-chain intersection"
        );
    }

    #[test]
    fn an_ee_chain_that_forbids_every_room_root_refuses_fail_closed() {
        let allowance: BTreeSet<String> = ["worktree_root".to_string()].into_iter().collect();
        let mut authority = file_write_authority();
        authority.sandbox_mode = octopus_descriptors::ProcessSandboxMode::Disabled;
        assert!(
            narrow_process_authority_to_root_slots(&authority, &allowance).is_err(),
            "an EE chain admitting none of the file.write room roots refuses the write fail-closed"
        );
    }
}

#[cfg(test)]
mod tool_process_carrier_tests {
    use super::{
        InterfaceHostIoLimits, ToolProcessBoundArtifact, ToolProcessEnvelope,
        ToolProcessWireEnvelope,
    };
    use octopus_descriptors::ArtifactId;
    use octopus_ledger::{ArtifactRef, ContentHash};

    #[test]
    fn bound_corpus_exceeds_one_read_budget_without_eager_worker_materialization() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let bytes = vec![b'x'; 8192];
        let path = root.join("source");
        std::fs::write(&path, &bytes).unwrap();
        let bindings: Vec<_> = (0..72).map(|index| ToolProcessBoundArtifact::from_pinned_file(
            ArtifactRef { id: ArtifactId::new(format!("bound:{index}")), content: ContentHash::of_bytes(&bytes) },
            path.clone(), ContentHash::of_bytes(&bytes), 65536,
        ).unwrap()).collect();
        let expected: Vec<_> = bindings.iter().map(|binding| binding.artifact.clone()).collect();
        let file = octopus_interfaces::InterfaceArtifactFile::write(root.join("call"), "tool.call", b"{}").unwrap();
        let input = octopus_interfaces::InterfaceInvocationEnvelope {
            artifact: expected[0].clone(), artifact_file: file, artifact_binding_payloads: bindings, metadata: None,
        };
        let limits = InterfaceHostIoLimits { max_control_frame_bytes: 65536, max_stderr_bytes: 65536, max_manifest_bytes: 65536, max_payload_bytes: 65536 };
        let captured = ToolProcessEnvelope::capture(&input, 16805, &limits).unwrap();
        let wire = captured.materialize_for_process(&root, 1, &limits).unwrap();
        assert!(serde_json::to_vec(&wire).unwrap().len() < limits.max_control_frame_bytes as usize);
        let invocation = wire.materialize(&root, 1, &limits).unwrap();
        assert_eq!(invocation.artifact_binding_payloads.iter().map(|binding| binding.artifact.clone()).collect::<Vec<_>>(), expected);
        assert_eq!(invocation.artifact_binding_payloads[71].read_bytes().unwrap().as_ref(), bytes);
        std::fs::write(&path, vec![b'y'; bytes.len()]).unwrap();
        assert!(ToolProcessEnvelope::capture(&input, 16806, &limits).is_err());
        assert_eq!(invocation.artifact_binding_payloads[0].read_bytes().unwrap().as_ref(), bytes);
    }

    #[test]
    fn payload_bytes_remain_artifact_carriers_outside_the_control_frame() {
        let root = tempfile::tempdir().unwrap();
        let limits = InterfaceHostIoLimits {
            max_control_frame_bytes: 16 * 1024,
            max_stderr_bytes: 16 * 1024,
            max_manifest_bytes: 16 * 1024,
            max_payload_bytes: 1024 * 1024,
        };
        let payload = vec![b'a'; 400 * 1024];
        let binding_payload = vec![b'b'; 200 * 1024];
        let envelope = ToolProcessEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new("tool.call:large"),
                content: ContentHash::of_bytes(&payload),
            },
            label: "tool.call".to_string(),
            payload: payload.clone(),
            artifact_bindings: vec![ToolProcessBoundArtifact::new(ArtifactRef {
                    id: ArtifactId::new("context:large"),
                    content: ContentHash::of_bytes(&binding_payload),
                },
                binding_payload.clone(),
            )],
            metadata: None,
            source_movement_seq: 41,
            worker_environment: None,
            worker_environment_kind: None,
            output_source: None,
        };
        let wire = envelope
            .clone()
            .materialize_for_process(root.path(), 7, &limits)
            .unwrap();
        let binding = wire.context.read(root.path(), 7, &limits).unwrap().artifact_bindings.remove(0);
        let shared = binding.manifest_path.clone();
        let payload_path = shared.parent().unwrap().join("payload");
        #[cfg(unix)]
        let published_metadata = std::fs::metadata(&payload_path).unwrap();
        for request_id in 8..40 {
            let repeated = envelope
                .clone()
                .materialize_for_process(root.path(), request_id, &limits)
                .unwrap();
            assert_eq!(repeated.context.read(root.path(), request_id, &limits).unwrap().artifact_bindings[0].manifest_path, shared);
        }
        assert_eq!(
            std::fs::read_dir(root.path().join("bound-artifacts"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_dir(shared.parent().unwrap()).unwrap().count(),
            2
        );
        assert_eq!(
            std::fs::metadata(&payload_path).unwrap().len(),
            binding_payload.len() as u64
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(&payload_path).unwrap();
            assert_eq!(metadata.nlink(), 1);
            assert_eq!(metadata.ino(), published_metadata.ino());
            assert_eq!(metadata.mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&shared).unwrap().mode() & 0o777, 0o600);
        }
        let control = serde_json::to_vec(&wire).unwrap();
        assert!(control.len() < 16 * 1024);
        let decoded: ToolProcessWireEnvelope = serde_json::from_slice(&control).unwrap();
        let invocation = decoded
            .materialize(
                root.path(),
                7,
                &InterfaceHostIoLimits {
                    max_control_frame_bytes: 16 * 1024,
                    max_stderr_bytes: 16 * 1024,
                    max_manifest_bytes: 16 * 1024,
                    max_payload_bytes: 1024 * 1024,
                },
            )
            .unwrap();
        assert_eq!(
            invocation
                .read_artifact_payload_bounded(1024 * 1024)
                .unwrap(),
            payload
        );
        assert_eq!(
            invocation.artifact_binding_payloads[0].read_bytes().unwrap().as_ref(),
            binding_payload
        );
        #[cfg(target_vendor = "apple")]
        {
            use std::os::macos::fs::MetadataExt as _;
            use std::os::unix::fs::MetadataExt as _;
            let metadata = std::fs::metadata(&payload_path).unwrap();
            assert_ne!(metadata.st_flags() & 0x20, 0);
            assert_eq!(metadata.blocks(), published_metadata.blocks());
            let raw = root.path().join("raw-allocation-control");
            std::fs::write(&raw, &binding_payload).unwrap();
            let raw_metadata = std::fs::metadata(&raw).unwrap();
            assert!(metadata.blocks() < raw_metadata.blocks());
            println!(
                "bound-carrier logical={} raw_allocated={} compressed_allocated={} after_replay_allocated={} mode={:o} nlink={}",
                metadata.len(), raw_metadata.blocks() * 512, published_metadata.blocks() * 512,
                metadata.blocks() * 512, metadata.mode() & 0o777, metadata.nlink(),
            );
        }
        let other = tempfile::tempdir().unwrap();
        let foreign = envelope
            .clone()
            .materialize_for_process(other.path(), 7, &limits)
            .unwrap();
        let foreign_binding = foreign.context.read(other.path(), 7, &limits).unwrap().artifact_bindings.remove(0);
        assert_ne!(foreign_binding.manifest_path, shared);
        assert!(super::read_tool_process_bound_artifact(&foreign_binding, root.path(), &limits).is_err());
        let mut wrong_reference = binding;
        wrong_reference.artifact.id = ArtifactId::new("context:other");
        assert!(super::read_tool_process_bound_artifact(&wrong_reference, root.path(), &limits).is_err());
        let concurrent = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let publishers: Vec<_> = (0..4)
            .map(|_| {
                let root = concurrent.path().to_path_buf();
                let binding = envelope.artifact_bindings[0].clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    super::materialize_tool_process_binding(&root, binding)
                        .unwrap()
                        .manifest_path
                })
            })
            .collect();
        let paths: Vec<_> = publishers
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(paths.iter().all(|path| path == &paths[0]));
        assert_eq!(
            std::fs::read_dir(concurrent.path().join("bound-artifacts"))
                .unwrap()
                .count(),
            1
        );
        let mut changed = envelope.clone();
        let mut changed_bytes = binding_payload.clone();
        changed_bytes[0] = b'c';
        changed.artifact_bindings[0] = ToolProcessBoundArtifact::new(changed.artifact_bindings[0].artifact.clone(), changed_bytes);
        let changed_wire = changed.materialize_for_process(root.path(), 40, &limits).unwrap();
        assert_ne!(changed_wire.context.read(root.path(), 40, &limits).unwrap().artifact_bindings[0].manifest_path, shared);
        assert_eq!(
            changed_wire
                .materialize(root.path(), 40, &limits)
                .unwrap()
                .artifact_binding_payloads[0]
                .read_bytes().unwrap()[0],
            b'c'
        );
        let mut too_small = limits;
        too_small.max_payload_bytes = payload.len() as u64 - 1;
        assert!(
            wire.clone()
                .materialize(root.path(), 7, &too_small)
                .is_err()
        );
        let manifest_bytes = std::fs::read(&shared).unwrap();
        let mut redirected: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
        redirected["payload_path"] = serde_json::json!(other.path().join("not-admitted"));
        std::fs::write(&shared, serde_json::to_vec(&redirected).unwrap()).unwrap();
        assert!(
            wire.clone()
                .materialize(root.path(), 7, &limits)
                .unwrap_err()
                .contains("different payload contract")
        );
        assert!(
            envelope
                .clone()
                .materialize_for_process(root.path(), 41, &limits)
                .is_err()
        );
        std::fs::write(&shared, &manifest_bytes).unwrap();
        std::fs::write(&payload_path, vec![b'x'; binding_payload.len()]).unwrap();
        assert!(wire.materialize(root.path(), 7, &limits).unwrap().artifact_binding_payloads[0].read_bytes().is_err());
        assert!(envelope.materialize_for_process(root.path(), 41, &limits).is_err());
        assert_eq!(std::fs::read(&payload_path).unwrap()[0], b'x');
    }
}

#[cfg(test)]
mod tool_argument_gate_tests {
    use super::{ToolProcessBrokerState, ToolProcessEnvelope, require_tool_in_invocation_grant};
    use octopus_descriptors::ArtifactId;
    use octopus_interfaces::{GrantProjection, InvocationMetadata};
    use octopus_ledger::{ArtifactRef, ContentHash};
    use octopus_tool_registry::load_tool_descriptor_from_dir;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn artifact_send_payload_and_reference_modes_are_disjoint_at_the_process_gate() {
        let tool_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../tool");
        let descriptor = load_tool_descriptor_from_dir(&tool_root, "artifact.send")
            .unwrap()
            .unwrap();
        let mut broker = ToolProcessBrokerState::default();
        broker
            .validate_arguments(
                &descriptor,
                &json!({
                    "interface": "system_worker_planner",
                    "label": "plan.input",
                    "payload": {"task": "continue"}
                }),
            )
            .unwrap();
        broker
            .validate_arguments(
                &descriptor,
                &json!({
                    "interface": "system_worker_planner",
                    "ref": "memory.fact:0123456789abcdef"
                }),
            )
            .unwrap();
        assert!(
            broker
                .validate_arguments(
                    &descriptor,
                    &json!({
                        "interface": "system_worker_planner",
                        "label": "memory.fact",
                        "ref": "memory.fact:0123456789abcdef"
                    }),
                )
                .is_err()
        );
        assert!(
            broker
                .validate_arguments(
                    &descriptor,
                    &json!({
                        "interface": "system_worker_planner",
                        "payload": {"task": "continue"}
                    }),
                )
                .is_err()
        );
    }

    #[test]
    fn formed_grant_gates_tools_before_any_room_branch() {
        let envelope = ToolProcessEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new("tool.call:grant-gate"),
                content: ContentHash::of_bytes(b"grant-gate"),
            },
            label: "tool.call".to_string(),
            payload: Vec::new(),
            artifact_bindings: Vec::new(),
            metadata: Some(InvocationMetadata::new(GrantProjection {
                tools: vec!["test.allowed".to_string(), "test.blocked".to_string()],
                hard_blocked_tools: vec!["test.blocked".to_string()],
                ..GrantProjection::default()
            })),
            source_movement_seq: 1,
            worker_environment: None,
            worker_environment_kind: None,
            output_source: None,
        };
        assert!(require_tool_in_invocation_grant(&envelope, "test.allowed").is_ok());
        assert!(require_tool_in_invocation_grant(&envelope, "test.blocked").is_err());
        assert!(require_tool_in_invocation_grant(&envelope, "test.absent").is_err());
    }
}

#[cfg(test)]
mod tool_process_output_hand_in_tests {
    use super::{
        PendingToolVerdicts, PendingWorkerEnvironmentRecords, ToolProcessInvokeError,
        ToolProcessIo, ToolProcessOutput, attest_tool_process_source_interface,
        await_with_tool_process_output_writer, fail_pending_delivery_verdicts,
    };
    use crate::DeliveryVerdict;
    use octopus_descriptors::{CheckpointId, EeId, InterfaceId, RunId};
    use octopus_interfaces::{
        GrantProjection, InterfaceArtifactFile, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    fn test_io(
        request_id: u64,
    ) -> (
        ToolProcessIo,
        tokio::sync::mpsc::Receiver<ToolProcessOutput>,
        PendingToolVerdicts,
    ) {
        let pending = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
        let records: PendingWorkerEnvironmentRecords =
            Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
        let (output, receiver) = tokio::sync::mpsc::channel(2);
        (
            ToolProcessIo {
                request_id,
                next_event_id: Arc::new(AtomicU64::new(1)),
                output,
                pending: Arc::clone(&pending),
                pending_worker_environment_records: records,
                input_open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            },
            receiver,
            pending,
        )
    }

    #[tokio::test]
    async fn materialized_output_waits_for_the_correlated_delivery_verdict() {
        let root = tempfile::tempdir().unwrap();
        let artifact =
            InterfaceArtifactFile::write(root.path(), "test.output", b"payload").unwrap();
        let expected_path = artifact.manifest_path().to_path_buf();
        let (io, mut output, pending) = test_io(73);
        let hand_in = tokio::spawn(async move { io.hand_output_artifact(&artifact).await });
        let event_id = match output.recv().await.unwrap() {
            ToolProcessOutput::ArtifactArrival {
                request_id,
                event_id,
                manifest_path,
            } => {
                assert_eq!(request_id, 73);
                assert_eq!(manifest_path, expected_path);
                event_id
            }
            _ => panic!("unexpected tool-process output"),
        };
        let sender = pending
            .lock()
            .await
            .remove(&(73, event_id))
            .expect("correlated delivery waiter");
        sender
            .send(Ok(DeliveryVerdict::Admitted { seq: 991 }))
            .unwrap();
        assert!(matches!(
            hand_in.await.unwrap().unwrap(),
            DeliveryVerdict::Admitted { seq: 991 }
        ));
        assert!(pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn pipe_close_fails_and_removes_the_pending_output_waiter() {
        let root = tempfile::tempdir().unwrap();
        let artifact =
            InterfaceArtifactFile::write(root.path(), "test.output", b"payload").unwrap();
        let (io, mut output, pending) = test_io(91);
        let hand_in = tokio::spawn(async move { io.hand_output_artifact(&artifact).await });
        assert!(matches!(
            output.recv().await.unwrap(),
            ToolProcessOutput::ArtifactArrival { request_id: 91, .. }
        ));
        fail_pending_delivery_verdicts(&pending, "transport closed").await;
        assert_eq!(hand_in.await.unwrap().unwrap_err(), "transport closed");
        assert!(pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn output_writer_failure_preempts_a_stalled_command() {
        let mut output_task = Some(tokio::spawn(async {
            Err::<(), String>("control frame exceeds declared ceiling".to_string())
        }));
        let error = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            await_with_tool_process_output_writer(&mut output_task, std::future::pending::<()>()),
        )
        .await
        .expect("writer failure preempts the stalled command")
        .unwrap_err();
        assert!(error.contains("control frame exceeds declared ceiling"));
    }

    #[test]
    fn output_protocol_rejects_unknown_artifact_arrival_fields() {
        let parsed = serde_json::from_value::<ToolProcessOutput>(serde_json::json!({
            "kind": "artifact_arrival",
            "request_id": 1,
            "event_id": 2,
            "manifest_path": "/tmp/output.artifact.json",
            "unknown": true
        }));
        assert!(parsed.is_err());
    }

    #[test]
    fn system_emission_protocol_rejects_child_asserted_emitter_identity() {
        let accepted = serde_json::from_value::<ToolProcessOutput>(serde_json::json!({
            "kind": "system_emission",
            "request_id": 1,
            "event_id": 2,
            "artifact": {
                "carrier": "materialized",
                "manifest_path": "/tmp/output.artifact.json"
            },
            "to_interface": "system_worker_planner"
        }));
        assert!(accepted.is_ok());
        let spoofed = serde_json::from_value::<ToolProcessOutput>(serde_json::json!({
            "kind": "system_emission",
            "request_id": 1,
            "event_id": 2,
            "artifact": {
                "carrier": "materialized",
                "manifest_path": "/tmp/output.artifact.json"
            },
            "from_interface": "system_interface_spoofed",
            "emitter_run": "run_spoofed",
            "emitter_checkpoint": "checkpoint_spoofed",
            "to_interface": "system_worker_planner"
        }));
        assert!(spoofed.is_err());
    }

    #[test]
    fn system_emission_identity_is_attested_by_the_host_interface() {
        let metadata = InvocationMetadata::new(GrantProjection::default()).with_runtime(
            RuntimeInvocationMetadata {
                run: RunId::new("run_attested"),
                ee: EeId::new("ee_attested"),
                worker: InterfaceId::new("system_worker_attested"),
                checkpoint: CheckpointId::new("checkpoint_attested"),
                interface: InterfaceId::new("system_context"),
                epoch: 0,
                run_record_hash: None,
            },
        );
        attest_tool_process_source_interface(&InterfaceId::new("system_context"), Some(&metadata))
            .unwrap();
        assert!(matches!(
            attest_tool_process_source_interface(
                &InterfaceId::new("system_context_other"),
                Some(&metadata)
            ),
            Err(ToolProcessInvokeError::Capability(_))
        ));
    }
}

#[cfg(feature = "owner-contract-tests")]
pub mod owner_contract_tests {
    use super::*;

    pub struct DoorFixture {
        pub context: InterfaceLiftContext,
        reader: LedgerTailReader,
        next_seq: u64,
        sources: BTreeMap<u64, Movement>,
    }

    impl octopus_ledger::MovementLog for DoorFixture {
        fn append(&mut self, draft: octopus_ledger::MovementDraft) -> Movement {
            draft.validate().expect("fixture movement trace must be non-empty");
            let clock = self.sources.last_key_value().map(|(_, row)| row.lamport.0).unwrap_or(0);
            let movement = Movement {
                seq: octopus_ledger::Seq(self.next_seq),
                lamport: octopus_ledger::Lamport(clock.max(draft.causal_floor.map(|floor| floor.0).unwrap_or(0)) + 1),
                recorded_at_unix_ms: self.next_seq + 1,
                trace: draft.trace,
                kind: draft.kind,
                from: draft.from,
                to: draft.to,
                artifact: draft.artifact,
                invocation_metadata: draft.invocation_metadata,
            };
            self.append_movement(movement.clone()).expect("fixture movement append failed");
            movement
        }

        fn read_from(&self, from: octopus_ledger::Seq) -> Vec<Movement> {
            self.sources.range(from.0..).map(|(_, row)| row.clone()).collect()
        }

        fn read_at(&self, seq: octopus_ledger::Seq) -> Option<Movement> {
            self.sources.get(&seq.0).cloned()
        }

        fn len(&self) -> usize {
            self.sources.len()
        }
    }

    impl DoorFixture {
        pub fn new(
            root: &Path,
            interface: InterfaceId,
            interface_dir: PathBuf,
            limits: InterfaceHostIoLimits,
        ) -> Result<Self, String> {
            std::fs::create_dir_all(root.join("blobs")).map_err(|e| e.to_string())?;
            std::fs::create_dir_all(root.join("metadata")).map_err(|e| e.to_string())?;
            let (arrivals, _) = tokio::sync::mpsc::channel(1);
            let mut context = InterfaceLiftContext::unprivileged(
                interface,
                interface_dir,
                None,
                root.join("var"),
                root.join("movements.jsonl"),
                root.join("blobs"),
                root.join("ledger-index.sqlite"),
                BTreeMap::new(),
                arrivals,
                tokio_util::sync::CancellationToken::new(),
            );
            context.host_io_limits = Some(limits);
            let reader = LedgerTailReader::open_named(&context, "owner-contract")?;
            Ok(Self {
                context,
                reader,
                next_seq: 0,
                sources: BTreeMap::new(),
            })
        }

        pub fn decode_record(
            &self,
            seq: u64,
            envelope: &InterfaceInvocationEnvelope,
            decoder: ToolDoorArtifactDecoder,
            purpose: ToolDoorArtifactDecodePurpose,
        ) -> Result<Option<ToolCallArtifact>, ToolDoorArtifactDecodeError> {
            let movement = self
                .sources
                .get(&seq)
                .ok_or("fixture source movement is absent")?;
            let context = self.context.with_source_movement(movement.clone());
            decoder(&context, envelope, purpose)
        }

        pub fn durable_dir(
            &self,
            formation: &WorkerEnvironmentFormation,
        ) -> Result<PathBuf, String> {
            materialize_tool_process_durable_dir(&self.context, Some(formation))?
                .ok_or_else(|| "fixture has no formation".into())
        }

        pub fn cache(&self, file: &InterfaceArtifactFile) -> Result<ArtifactRef, String> {
            let limits = self.context.required_host_io_limits()?;
            let captured = file
                .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                .map_err(|e| e.to_string())?;
            let payload = ContentHash::of_bytes(&captured.payload_bytes);
            std::fs::write(
                self.context.blobs_dir.join(payload.to_hex()),
                captured.payload_bytes,
            )
            .map_err(|e| e.to_string())?;
            let content = ContentHash::of_bytes(&captured.manifest_bytes);
            std::fs::write(
                self.context.blobs_dir.join(content.to_hex()),
                captured.manifest_bytes,
            )
            .map_err(|e| e.to_string())?;
            Ok(ArtifactRef {
                id: octopus_descriptors::ArtifactId::new(format!(
                    "{}:{}",
                    captured.manifest.label,
                    content.to_hex()
                )),
                content,
            })
        }

        pub fn record_delivery(
            &mut self,
            file: &InterfaceArtifactFile,
            to: InterfaceId,
            metadata: InvocationMetadata,
            kind: MovementKind,
        ) -> Result<u64, String> {
            self.record_transfer(file, self.context.interface.clone(), to, metadata, kind)
        }

        pub fn configure_owner_transport(
            &mut self,
            roots: BTreeMap<String, PathBuf>,
            descriptor: InterfaceDescriptor,
        ) -> OwnerTransportPeer {
            let (arrivals, arrival_rx) = tokio::sync::mpsc::channel(8);
            let (emissions, emission_rx) = tokio::sync::mpsc::channel(8);
            self.context.root_slots = roots;
            self.context.descriptor_snapshot = Some(descriptor);
            self.context.arrivals = arrivals;
            self.context.emissions = emissions;
            OwnerTransportPeer {
                arrivals: arrival_rx,
                emissions: emission_rx,
            }
        }

        pub fn output_source(
            &self,
            seq: u64,
            envelope: &InterfaceInvocationEnvelope,
            formation: &WorkerEnvironmentFormation,
        ) -> Result<ToolDoorOutputSource, String> {
            let movement = self
                .sources
                .get(&seq)
                .ok_or("fixture source movement is absent")?;
            self.context
                .with_source_movement(movement.clone())
                .source_movement(envelope)?;
            Ok(ToolDoorOutputSource {
                movement: movement.clone(),
                envelope: envelope.clone(),
                formation: Some(formation.clone()),
            })
        }

        pub fn next_formed(
            &mut self,
            decoder: ToolDoorArtifactDecoder,
        ) -> Result<Option<(u64, ToolCallArtifact, InterfaceInvocationEnvelope)>, String> {
            loop {
                let rows = self.reader.read_new_uncommitted_limit(1)?;
                let Some(own) = rows.first() else {
                    return Ok(None);
                };
                let action = prepare_tool_door_action(
                    &self.context,
                    &self.reader,
                    own,
                    decoder,
                    ToolDoorArtifactDecodePurpose::Dispatch,
                );
                self.reader.commit_through(own)?;
                match action {
                    ToolDoorAction::Invoke { call, envelope, .. } => {
                        return Ok(Some((own.movement.seq.0, call, envelope)));
                    }
                    ToolDoorAction::Complete(_) => {}
                    ToolDoorAction::Failure { class, details, .. } => {
                        return Err(format!("{class}: {details}"));
                    }
                    _ => {
                        return Err(
                            "fixture received an unexpected lifecycle/recovery command".into()
                        );
                    }
                }
            }
        }

        pub fn record_transfer(
            &mut self,
            file: &InterfaceArtifactFile,
            from: InterfaceId,
            to: InterfaceId,
            mut metadata: InvocationMetadata,
            kind: MovementKind,
        ) -> Result<u64, String> {
            let artifact = self.cache(file)?;
            metadata
                .runtime
                .as_mut()
                .ok_or("fixture delivery has no coordinates")?
                .interface = to.clone();
            let seq = self.next_seq;
            let movement = Movement {
                seq: octopus_ledger::Seq(seq),
                lamport: octopus_ledger::Lamport(seq + 1),
                recorded_at_unix_ms: seq + 1,
                trace: octopus_ledger::TraceId("trace:code-mode-owner-contract".into()),
                kind,
                from: Endpoint::Interface(from),
                to: Endpoint::Interface(to),
                artifact: Some(artifact),
                invocation_metadata: Some(metadata.into()),
            };
            self.append_movement(movement)
        }

        pub fn record_admission(
            &mut self,
            artifact: &ArtifactRef,
            metadata: &InvocationMetadata,
            admitted_mark_outcomes: &[octopus_ledger::Mark],
        ) -> Result<u64,String> {
            if metadata.verification_failure.is_some() { return Err("cannot record a refused checkpoint as admitted".into()); }
            let metadata = metadata.clone();
            let runtime = metadata.runtime.clone().ok_or("actual admission has no runtime coordinate")?;
            let seq = self.next_seq;
            let first = self.append_movement(Movement {
                seq:octopus_ledger::Seq(seq),lamport:octopus_ledger::Lamport(seq+1),recorded_at_unix_ms:seq+1,
                trace:octopus_ledger::TraceId("trace:code-mode-owner-contract".into()),kind:MovementKind::InterfaceArrival,
                from:Endpoint::Interface(runtime.interface.clone()),to:Endpoint::Interface(runtime.worker.clone()),artifact:Some(artifact.clone()),invocation_metadata:Some(metadata.into()),
            })?;
            for mark in admitted_mark_outcomes {
                let seq = self.next_seq;
                self.append_movement(Movement {
                    seq:octopus_ledger::Seq(seq),lamport:octopus_ledger::Lamport(seq+1),recorded_at_unix_ms:seq+1,
                    trace:octopus_ledger::TraceId("trace:code-mode-owner-contract".into()),kind:MovementKind::MarkOutcomeAdmitted(mark.name.clone()),
                    from:Endpoint::Interface(runtime.interface.clone()),to:Endpoint::Interface(runtime.worker.clone()),artifact:mark.evidence.clone(),invocation_metadata:Some(mark.clone().into()),
                })?;
            }
            Ok(first)
        }

        pub fn record_refusal(&mut self, file: &InterfaceArtifactFile, runtime: octopus_ledger::RuntimeInvocationMetadata, verdict: DeliveryVerdict) -> Result<u64, String> {
            let artifact = self.cache(file)?;
            let seq = self.next_seq;
            self.append_movement(Movement {
                seq:octopus_ledger::Seq(seq),lamport:octopus_ledger::Lamport(seq+1),recorded_at_unix_ms:seq+1,
                trace:octopus_ledger::TraceId("trace:code-mode-owner-contract".into()),kind:MovementKind::DeliveryRefused,
                from:Endpoint::Interface(runtime.interface.clone()),to:Endpoint::Interface(runtime.worker.clone()),artifact:Some(artifact),
                invocation_metadata:Some(octopus_ledger::MovementMetadata::Delivery(octopus_ledger::DeliveryMetadata {
                    delivery_verdict:verdict,runtime:Some(runtime),source_movement_seq:None,delivery_scope_seq:None,
                })),
            })
        }

        fn append_movement(&mut self, movement: Movement) -> Result<u64,String> {
            let seq = movement.seq.0;
            if seq != self.next_seq { return Err("fixture attempted a non-contiguous movement".into()); }
            let bytes =
                octopus_ledger::encode_durable_movement_row(&movement, seq as usize, |_, grant| {
                    let content = ContentHash::of_bytes(grant);
                    std::fs::write(
                        self.context
                            .movements_path
                            .parent()
                            .unwrap()
                            .join("metadata")
                            .join(content.to_hex()),
                        grant,
                    )
                    .unwrap();
                    Ok(content)
                })
                .map_err(|e| e.to_string())?;
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.context.movements_path)
                .map_err(|e| e.to_string())?;
            writeln!(file, "{bytes}").map_err(|e| e.to_string())?;
            self.next_seq += 1;
            self.sources.insert(seq, movement);
            Ok(seq)
        }

        pub fn admit(
            &mut self,
            envelope: &InterfaceInvocationEnvelope,
            from: InterfaceId,
            decoder: ToolDoorArtifactDecoder,
        ) -> Result<(u64, ToolCallArtifact, InterfaceInvocationEnvelope), String> {
            if self.cache(&envelope.artifact_file)? != envelope.artifact {
                return Err("fixture source artifact is not exact".into());
            }
            let seq = self.next_seq;
            let movement = Movement {
                seq: octopus_ledger::Seq(seq),
                lamport: octopus_ledger::Lamport(seq + 1),
                recorded_at_unix_ms: seq + 1,
                trace: octopus_ledger::TraceId("trace:code-mode-owner-contract".into()),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface(from),
                to: Endpoint::Interface(self.context.interface.clone()),
                artifact: Some(envelope.artifact.clone()),
                invocation_metadata: Some(
                    envelope
                        .metadata()
                        .map_err(|e| e.to_string())?
                        .clone()
                        .into(),
                ),
            };
            let bytes =
                octopus_ledger::encode_durable_movement_row(&movement, seq as usize, |_, grant| {
                    let content = ContentHash::of_bytes(grant);
                    std::fs::write(
                        self.context
                            .movements_path
                            .parent()
                            .unwrap()
                            .join("metadata")
                            .join(content.to_hex()),
                        grant,
                    )
                    .unwrap();
                    Ok(content)
                })
                .map_err(|e| e.to_string())?;
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.context.movements_path)
                .map_err(|e| e.to_string())?;
            writeln!(file, "{bytes}").map_err(|e| e.to_string())?;
            self.next_seq += 1;
            self.sources.insert(seq, movement);
            let rows = self.reader.read_new_uncommitted_limit(1)?;
            let own = rows
                .first()
                .ok_or("fixture reader did not admit its exact movement")?;
            let action = prepare_tool_door_action(
                &self.context,
                &self.reader,
                own,
                decoder,
                ToolDoorArtifactDecodePurpose::Dispatch,
            );
            self.reader.commit_through(own)?;
            match action {
                ToolDoorAction::Invoke { call, envelope, .. } => Ok((seq, call, envelope)),
                ToolDoorAction::Failure { class, details, .. } => {
                    Err(format!("{class}: {details}"))
                }
                _ => Err("fixture source was not an admitted tool invocation".into()),
            }
        }
    }

    pub struct OwnerTransportPeer {
        arrivals: tokio::sync::mpsc::Receiver<InterfaceActivationArrival>,
        emissions: tokio::sync::mpsc::Receiver<SystemInterfaceEmission>,
    }

    pub struct OwnerTransportOutput {
        pub artifact: InterfaceArtifactFile,
        pub directed: Option<(u64, InterfaceId)>,
        pub retained_delivery_id: Option<String>,
        reply: OwnerTransportReply,
    }

    enum OwnerTransportReply {
        Ordinary(tokio::sync::oneshot::Sender<DeliveryVerdict>),
        Retained(tokio::sync::oneshot::Sender<Result<octopus_ledger::ArtifactDelivery, String>>),
    }

    impl OwnerTransportOutput {
        pub fn acknowledge(self, verdict: DeliveryVerdict) -> Result<(), String> {
            match self.reply {
                OwnerTransportReply::Ordinary(reply) => reply.send(verdict).map_err(|_| "fixture output acknowledgement receiver closed".into()),
                OwnerTransportReply::Retained(reply) => reply.send(Ok(octopus_ledger::ArtifactDelivery::Complete { verdict }))
                    .map_err(|_| "fixture retained completion receiver closed".into()),
            }
        }

        pub fn acknowledge_retained(self, delivery: Result<octopus_ledger::ArtifactDelivery, String>) -> Result<(), String> {
            match self.reply {
                OwnerTransportReply::Retained(reply) => reply.send(delivery).map_err(|_| "fixture retained completion receiver closed".into()),
                OwnerTransportReply::Ordinary(_) => Err("ordinary fixture output has no retained scope".into()),
            }
        }
    }

    impl OwnerTransportPeer {
        pub async fn next(&mut self) -> Result<OwnerTransportOutput, String> {
            tokio::select! {
                arrival = self.arrivals.recv() => {
                    let arrival = arrival.ok_or("fixture arrivals closed")?;
                    let (retained_delivery_id, reply) = match (arrival.verdict_tx, arrival.retained) {
                        (None, Some(retained)) => (Some(retained.id), OwnerTransportReply::Retained(retained.reply)),
                        (Some(verdict), None) => (None, OwnerTransportReply::Ordinary(verdict)),
                        _ => return Err("fixture arrival must select one acknowledgement contract".into()),
                    };
                    Ok(OwnerTransportOutput {artifact:arrival.artifact,directed:None,retained_delivery_id,reply})
                },
                emission = self.emissions.recv() => {
                    let emission = emission.ok_or("fixture emissions closed")?;
                    let (artifact,seq,to,verdict) = emission.into_parts();
                    let SystemInterfaceEmissionArtifact::Materialized(artifact) = artifact else {return Err("fixture received an unmaterialized artifact reference".into());};
                    Ok(OwnerTransportOutput {artifact,directed:Some((seq,to)),retained_delivery_id:None,reply:OwnerTransportReply::Ordinary(verdict)})
                },
            }
        }
    }

    pub async fn hand_owned_output(
        ctx: &InterfaceLiftContext,
        source: &ToolDoorOutputSource,
        callback: &ToolDoorOutputCallback,
        artifact: InterfaceArtifactFile,
    ) -> Result<DeliveryVerdict, String> {
        hand_tool_process_output(ctx, Some(source), Some(callback), artifact).await
    }

    pub async fn hand_owned_completed_output(
        ctx: &InterfaceLiftContext,
        source: &ToolDoorOutputSource,
        callback: &ToolDoorOutputCallback,
        artifact: InterfaceArtifactFile,
        generation: PathBuf,
        invocation_manifest: PathBuf,
    ) -> Result<ToolProcessBrokerResult, String> {
        let mut input = ToolProcessEnvelope::capture(&source.envelope, source.movement.seq.0, &ctx.required_host_io_limits()?)?;
        input.worker_environment = source.formation.clone();
        input.output_source = Some(source.clone());
        let (mut retained, recover) = match completed_output::CompletedOutputs::load(ctx, source.movement.seq.0)? {
            Some(retained) => (retained, true),
            None => (completed_output::CompletedOutputs::retain(ctx, &input, &ToolProcessCompletion {
                artifacts: vec![artifact], streamed_output_admitted: false, primary_input: None, generation, invocation_manifest,
            })?, false),
        };
        retained.deliver(ctx, &input, Some(callback), recover).await
    }

    pub async fn hand_owned_invocation_failure(
        ctx: &InterfaceLiftContext,
        source: &ToolDoorOutputSource,
        callback: &ToolDoorOutputCallback,
    ) -> Result<bool, String> {
        let call: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(
            &source.envelope.read_artifact_payload_bounded(ctx.required_host_io_limits()?.max_payload_bytes)
                .map_err(|e| e.to_string())?,
        ).map_err(|e| e.to_string())?;
        let target = ToolResultTarget::from(&call);
        let state = hand_tool_failure(ctx, source.movement.seq.0, "tool_process_failure",
            serde_json::json!({"error":"fixture process stopped"}), Some(&target), ToolDoorJournalState::Completed,
            Some((callback, source.clone()))).await?;
        Ok(matches!(state, ToolDoorJournalState::Refused))
    }

    pub async fn execute_formed(
        handler: &dyn ToolProcessHandler,
        environment: &ToolProcessEnvironment,
        invocation: ToolProcessInvocation,
        io: ToolProcessIo,
    ) -> Result<Vec<InterfaceArtifactFile>, String> {
        let input_root = environment.var_dir.join("owner-contract-invocations");
        let mut envelope = ToolProcessEnvelope::capture(
            &invocation.envelope,
            invocation.source_movement_seq,
            &environment.host_io_limits,
        )?;
        envelope.worker_environment = invocation.worker_environment;
        envelope.worker_environment_kind = invocation.worker_environment_kind;
        let envelope = envelope.materialize_for_process(&input_root, invocation.request_id, &environment.host_io_limits)?;
        let command = ToolProcessCommand::Invoke {
            request_id: invocation.request_id,
            descriptor: invocation.descriptor,
            envelope,
        };
        match execute_tool_process_command(
            handler,
            environment,
            &input_root,
            command,
            &io.output,
            &io.pending,
            &io.pending_worker_environment_records,
            &io.next_event_id,
            &io.input_open,
        )
        .await
        {
            ToolProcessOutput::Complete {
                artifact_manifest_paths,
                ..
            } => artifact_manifest_paths
                .into_iter()
                .map(|path| InterfaceArtifactFile::new(path).map_err(|e| e.to_string()))
                .collect(),
            ToolProcessOutput::Failed { error, .. }
            | ToolProcessOutput::Unavailable { error, .. } => Err(error),
            _ => Err("fixture invocation did not produce Complete".into()),
        }
    }

    pub struct IoPeer {
        output: tokio::sync::mpsc::Receiver<ToolProcessOutput>,
        pending: PendingToolVerdicts,
        records: PendingWorkerEnvironmentRecords,
    }

    pub fn io_pair(request_id: u64) -> (ToolProcessIo, IoPeer) {
        let (output, receiver) = tokio::sync::mpsc::channel(8);
        let pending = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
        let records = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
        (
            ToolProcessIo {
                request_id,
                next_event_id: Arc::new(AtomicU64::new(1)),
                output,
                pending: pending.clone(),
                pending_worker_environment_records: records.clone(),
                input_open: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            },
            IoPeer {
                output: receiver,
                pending,
                records,
            },
        )
    }

    impl IoPeer {
        pub async fn next_artifact(&mut self) -> Result<(u64, u64, InterfaceArtifactFile), String> {
            loop {
                match self
                    .output
                    .recv()
                    .await
                    .ok_or("fixture output channel closed")?
                {
                    ToolProcessOutput::WorkerEnvironmentLifecycle {
                        request_id,
                        event_id,
                        ..
                    }
                    | ToolProcessOutput::WorkerEnvironmentLiveState {
                        request_id,
                        event_id,
                        ..
                    } => {
                        self.records
                            .lock()
                            .await
                            .remove(&(request_id, event_id))
                            .ok_or("missing correlated evidence acknowledgement")?
                            .send(Ok(()))
                            .map_err(|_| "evidence acknowledgement dropped")?;
                    }
                    ToolProcessOutput::ArtifactArrival {
                        request_id,
                        event_id,
                        manifest_path,
                    } => {
                        return Ok((
                            request_id,
                            event_id,
                            InterfaceArtifactFile::new(manifest_path).map_err(|e| e.to_string())?,
                        ));
                    }
                    _ => {
                        return Err(
                            "fixture expected ordinary ArtifactArrival under the active invocation"
                                .into(),
                        );
                    }
                }
            }
        }

        pub async fn acknowledge(
            &self,
            request_id: u64,
            event_id: u64,
            verdict: DeliveryVerdict,
        ) -> Result<(), String> {
            self.pending
                .lock()
                .await
                .remove(&(request_id, event_id))
                .ok_or("missing exact artifact admission waiter")?
                .send(Ok(verdict))
                .map_err(|_| "artifact admission waiter dropped".into())
        }
    }
}
