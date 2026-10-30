use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use crate::process_authority::{
    ProcessAuthoritySpecExecution, resolve_process_exact_path, resolve_required_repository_file,
};
use crate::life_process::{LifeProcess, exit_result, finish_stderr};
use octopus_descriptors::{ArtifactId, InterfaceId, ProcessAuthoritySpec};
use octopus_interfaces::{
    DeliveryVerdict, InterfaceArtifactFile, InterfaceArtifactManifest, InterfaceInvocationEnvelope,
};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_file_storage::metadata::FileStamp;
use std::time::SystemTime;
use octopus_run_record::RunRecordsIndex;
use sha2::Sha256;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;

use crate::{InterfaceActivationArrival, InterfaceActivationEvidence, SystemInterfaceEmission};

pub const RESOLVED_INTERFACE_DESCRIPTOR_ENV: &str = "OCTOPUS_INTERFACE_DESCRIPTOR_JSON";
pub const INTERFACE_HOST_IO_LIMITS_ENV: &str = "OCTOPUS_INTERFACE_HOST_IO_LIMITS";
static STDERR_CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const OWNER_EXECUTABLE_PREFIX: &str = "owner://";
const INTERFACE_EXECUTABLE_PREFIX: &str = "interface://";
const IMPORTED_EXECUTABLE_BUILD_RECEIPT_FILE: &str = "build-receipt.json";
const INTERFACE_PRIVATE_ENVIRONMENT_FILE: &str = "environment.json";
const INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceHostIoLimits {
    pub max_control_frame_bytes: u64,
    pub max_stderr_bytes: u64,
    pub max_manifest_bytes: u64,
    pub max_payload_bytes: u64,
}

impl InterfaceHostIoLimits {
    pub fn validate(self) -> Result<(), String> {
        for (name, value) in [
            ("max_control_frame_bytes", self.max_control_frame_bytes),
            ("max_stderr_bytes", self.max_stderr_bytes),
            ("max_manifest_bytes", self.max_manifest_bytes),
            ("max_payload_bytes", self.max_payload_bytes),
        ] {
            if value == 0 {
                return Err(format!(
                    "interface host I/O limit '{name}' must be positive"
                ));
            }
        }
        usize::try_from(self.max_control_frame_bytes)
            .map_err(|_| "protocol frame ceiling exceeds host address space".to_string())?;
        Ok(())
    }
}

pub struct BoundedJsonLineReader<R> {
    reader: tokio::io::BufReader<R>,
    buffer: Vec<u8>,
    maximum_bytes: usize,
    failure: Option<String>,
}

impl<R> BoundedJsonLineReader<R>
where
    R: tokio::io::AsyncRead + Unpin,
{
    pub fn new(reader: R, maximum_bytes: u64) -> Result<Self, String> {
        let maximum_bytes = usize::try_from(maximum_bytes)
            .map_err(|_| "protocol frame ceiling exceeds host address space".to_string())?;
        if maximum_bytes == 0 {
            return Err("protocol frame ceiling must be positive".to_string());
        }
        Ok(Self {
            reader: tokio::io::BufReader::new(reader),
            buffer: Vec::new(),
            maximum_bytes,
            failure: None,
        })
    }

    pub async fn next_line(&mut self) -> Result<Option<String>, String> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let result = self.read_line().await;
        if let Err(error) = &result {
            self.failure = Some(error.clone());
        }
        result
    }

    async fn read_line(&mut self) -> Result<Option<String>, String> {
        use tokio::io::AsyncBufReadExt;

        loop {
            let available = self
                .reader
                .fill_buf()
                .await
                .map_err(|error| format!("protocol output read failed: {error}"))?;
            if available.is_empty() {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                return Err("protocol output ended with an unterminated frame".to_string());
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |index| index + 1);
            if take > self.maximum_bytes.saturating_sub(self.buffer.len()) {
                return Err(format!(
                    "protocol output frame exceeds declared ceiling of {} bytes",
                    self.maximum_bytes
                ));
            }
            self.buffer.extend_from_slice(&available[..take]);
            self.reader.consume(take);
            if newline.is_some() {
                if self.buffer.last() == Some(&b'\n') {
                    self.buffer.pop();
                }
                if self.buffer.last() == Some(&b'\r') {
                    self.buffer.pop();
                }
                return self.finish_line().map(Some);
            }
        }
    }

    fn finish_line(&mut self) -> Result<String, String> {
        String::from_utf8(std::mem::take(&mut self.buffer))
            .map_err(|error| format!("protocol output is not UTF-8: {error}"))
    }
}

pub fn serialize_bounded_json_line<T: serde::Serialize>(
    value: &T,
    maximum_bytes: u64,
    name: &str,
) -> Result<Vec<u8>, String> {
    let maximum_bytes = usize::try_from(maximum_bytes)
        .map_err(|_| "protocol frame ceiling exceeds host address space".to_string())?;
    if maximum_bytes == 0 {
        return Err("protocol frame ceiling must be positive".to_string());
    }
    let mut bytes =
        serde_json::to_vec(value).map_err(|error| format!("{name} did not serialize: {error}"))?;
    bytes.push(b'\n');
    if bytes.len() > maximum_bytes {
        return Err(format!(
            "{name} exceeds declared ceiling of {maximum_bytes} bytes"
        ));
    }
    Ok(bytes)
}

async fn drain_bounded_stderr_to_private_file<R>(
    mut reader: R,
    maximum_bytes: u64,
    interface: InterfaceId,
    process_name: &'static str,
    private_path: Option<PathBuf>,
) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut observed = 0_u64;
    let mut digest = blake3::Hasher::new();
    let mut buffer = [0_u8; 8192];
    let mut private_file = None;
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                return Err(format!(
                    "interface '{}' {process_name} stderr read failed: {error}",
                    interface.as_str()
                ));
            }
        };
        observed = observed.checked_add(read as u64).ok_or_else(|| {
            format!(
                "interface '{}' {process_name} stderr byte count overflowed",
                interface.as_str()
            )
        })?;
        digest.update(&buffer[..read]);
        if observed > maximum_bytes {
            return Err(format!(
                "interface '{}' {process_name} stderr exceeded declared ceiling of {maximum_bytes} bytes after {observed} bytes (digest {})",
                interface.as_str(),
                digest.finalize().to_hex()
            ));
        }
        if private_file.is_none()
            && let Some(path) = private_path.as_ref()
        {
            let mut options = std::fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            private_file = Some(options.open(path).map_err(|error| {
                format!(
                    "interface '{}' {process_name} private stderr '{}' cannot be opened: {error}",
                    interface.as_str(),
                    path.display()
                )
            })?);
        }
        if let Some(file) = private_file.as_mut() {
            use std::io::Write;
            file.write_all(&buffer[..read]).map_err(|error| {
                format!(
                    "interface '{}' {process_name} private stderr cannot be written: {error}",
                    interface.as_str()
                )
            })?;
        }
    }
    if let Some(file) = private_file.as_mut() {
        file.sync_all().map_err(|error| {
            format!(
                "interface '{}' {process_name} private stderr cannot be synced: {error}",
                interface.as_str()
            )
        })?;
    }
    if observed > 0 {
        let digest = digest.finalize().to_hex().to_string();
        tracing::warn!(
            interface = interface.as_str(),
            process = process_name,
            stderr_digest = digest,
            observed_bytes = observed,
            private_path = private_path.as_ref().map(|path| path.display().to_string()),
            "spawned process wrote private stderr diagnostics"
        );
    }
    Ok(())
}

#[derive(Clone)]
pub struct InterfaceLiftContext {
    pub interface: InterfaceId,
    pub interface_dir: PathBuf,
    pub descriptor_snapshot: Option<octopus_interfaces::InterfaceDescriptor>,
    pub var_dir: PathBuf,
    pub movements_path: PathBuf,
    pub blobs_dir: PathBuf,
    pub ledger_index_path: PathBuf,
    pub root_slots: BTreeMap<String, PathBuf>,
    pub host_io_limits: Option<InterfaceHostIoLimits>,
    pub arrivals: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    pub(crate) emissions: tokio::sync::mpsc::Sender<SystemInterfaceEmission>,
    pub cancel: CancellationToken,
    pub(crate) readiness: InterfaceLifeReadiness,
}

#[derive(Clone, Default)]
pub(crate) struct InterfaceLifeReadiness {
    sender: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Result<(), String>>>>>,
    publication: Option<tokio::sync::watch::Receiver<Option<Result<(), String>>>>,
}

pub struct InterfaceLifeReadyPublication {
    sender: tokio::sync::watch::Sender<Option<Result<(), String>>>,
}

impl InterfaceLifeReadyPublication {
    pub fn complete(self, result: Result<(), String>) -> Result<(), String> {
        self.sender.send(Some(result))
            .map_err(|_| "interface life readiness publication receiver was dropped".to_string())
    }
}

impl InterfaceLifeReadiness {
    pub(crate) fn channel() -> (Self, tokio::sync::oneshot::Receiver<Result<(), String>>) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        (
            Self {
                sender: Arc::new(std::sync::Mutex::new(Some(sender))),
                publication: None,
            },
            receiver,
        )
    }

    pub(crate) fn signal(&self, result: Result<(), String>) -> Result<(), String> {
        let Some(sender) = self
            .sender
            .lock()
            .map_err(|_| "interface life readiness lock is poisoned".to_string())?
            .take()
        else {
            return Ok(());
        };
        sender
            .send(result)
            .map_err(|_| "interface life readiness receiver was dropped".to_string())
    }
}

impl InterfaceLiftContext {
    pub fn unprivileged(
        interface: InterfaceId,
        interface_dir: PathBuf,
        descriptor_snapshot: Option<octopus_interfaces::InterfaceDescriptor>,
        var_dir: PathBuf,
        movements_path: PathBuf,
        blobs_dir: PathBuf,
        ledger_index_path: PathBuf,
        root_slots: BTreeMap<String, PathBuf>,
        arrivals: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
        cancel: CancellationToken,
    ) -> Self {
        let (emissions, receiver) = tokio::sync::mpsc::channel(1);
        drop(receiver);
        Self {
            interface,
            interface_dir,
            descriptor_snapshot,
            var_dir,
            movements_path,
            blobs_dir,
            ledger_index_path,
            root_slots,
            host_io_limits: None,
            arrivals,
            emissions,
            cancel,
            readiness: InterfaceLifeReadiness::default(),
        }
    }

    pub fn with_readiness_channel(
        mut self,
    ) -> (Self, tokio::sync::oneshot::Receiver<Result<(), String>>) {
        let (readiness, receiver) = InterfaceLifeReadiness::channel();
        self.readiness = readiness;
        (self, receiver)
    }

    pub fn with_readiness_publication_channel(
        self,
    ) -> (Self, tokio::sync::oneshot::Receiver<Result<(), String>>, InterfaceLifeReadyPublication) {
        let (mut ctx, receiver) = self.with_readiness_channel();
        let (sender, published) = tokio::sync::watch::channel(None);
        ctx.readiness.publication = Some(published);
        (ctx, receiver, InterfaceLifeReadyPublication { sender })
    }

    pub async fn signal_ready(&self) -> Result<(), String> {
        if self.cancel.is_cancelled() {
            return Err("interface life readiness publication cancelled".into());
        }
        self.readiness.signal(Ok(()))?;
        let Some(mut publication) = self.readiness.publication.clone() else {
            return Ok(());
        };
        loop {
            if self.cancel.is_cancelled() {
                return Err("interface life readiness publication cancelled".into());
            }
            if let Some(result) = publication.borrow_and_update().clone() {
                return result;
            }
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err("interface life readiness publication cancelled".into()),
                changed = publication.changed() => changed
                    .map_err(|_| "interface life readiness publisher closed without confirmation".to_string())?,
            }
        }
    }

    pub fn signal_startup_failed(&self, error: impl Into<String>) -> Result<(), String> {
        self.readiness.signal(Err(error.into()))
    }

    pub fn with_host_io_limits(mut self, limits: InterfaceHostIoLimits) -> Result<Self, String> {
        limits.validate()?;
        self.host_io_limits = Some(limits);
        Ok(self)
    }

    pub fn with_system_emission_sender(
        mut self,
        emissions: tokio::sync::mpsc::Sender<SystemInterfaceEmission>,
    ) -> Self {
        self.emissions = emissions;
        self
    }

    pub fn required_host_io_limits(&self) -> Result<InterfaceHostIoLimits, String> {
        let limits = self.host_io_limits.ok_or_else(|| {
            format!(
                "interface '{}' has no process-owned host I/O limits",
                self.interface.as_str()
            )
        })?;
        limits.validate()?;
        Ok(limits)
    }

    pub async fn hand_system_emission(
        &self,
        artifact: crate::SystemInterfaceEmissionArtifact,
        source_movement_seq: u64,
        to_interface: InterfaceId,
    ) -> Result<DeliveryVerdict, String> {
        let artifact = match artifact {
            crate::SystemInterfaceEmissionArtifact::Materialized(artifact) => {
                let limits = self.required_host_io_limits()?;
                crate::SystemInterfaceEmissionArtifact::Materialized(
                    artifact
                        .capture_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                        .map_err(|error| {
                            format!("interface output carrier capture failed: {error}")
                        })?,
                )
            }
            artifact @ crate::SystemInterfaceEmissionArtifact::Reference(_) => artifact,
        };
        let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
        self.emissions
            .send(SystemInterfaceEmission::host_attested(
                artifact,
                self.interface.clone(),
                source_movement_seq,
                to_interface,
                verdict_tx,
            ))
            .await
            .map_err(|_| "system emission intake closed".to_string())?;
        verdict_rx
            .await
            .map_err(|_| "system emission verdict channel dropped".to_string())
    }

    pub fn interface_var_dir(&self) -> PathBuf {
        self.var_dir.clone()
    }

    pub fn interface_registry_dir(&self) -> PathBuf {
        self.interface_dir.clone()
    }

    pub fn resolved_interface_descriptor(
        &self,
    ) -> Result<octopus_interfaces::InterfaceDescriptor, String> {
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
                    "interface directory '{}' has no registry root",
                    self.interface_dir.display()
                )
            })?;
        resolve_interface_descriptor(registry_root, &self.interface)?.ok_or_else(|| {
            format!(
                "interface '{}' resolves no descriptor",
                self.interface.as_str()
            )
        })
    }

    fn life_process_install_root(&self) -> Result<PathBuf, String> {
        self.root_slot(crate::INTERFACE_LIFE_INSTALL_ROOT_SLOT)
            .map(Path::to_path_buf)
    }

    fn registry_install_root(&self) -> Result<PathBuf, String> {
        self.interface_dir
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                format!(
                    "interface registry directory '{}' has no install root",
                    self.interface_dir.display()
                )
            })
    }

    pub fn root_slot(&self, name: &str) -> Result<&Path, String> {
        self.root_slots
            .get(name)
            .map(PathBuf::as_path)
            .ok_or_else(|| {
                format!(
                    "interface '{}' requires unbound root slot '{}'",
                    self.interface.as_str(),
                    name
                )
            })
    }

    pub async fn hand_arrival(
        &self,
        artifact: InterfaceArtifactFile,
        evidence: Vec<InterfaceActivationEvidence>,
    ) -> Result<DeliveryVerdict, String> {
        let limits = self.required_host_io_limits()?;
        let artifact = artifact
            .capture_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .map_err(|error| format!("interface output carrier capture failed: {error}"))?;
        let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
        self.arrivals
            .send(InterfaceActivationArrival {
                artifact,
                interface: self.interface.clone(),
                evidence,
                verdict_tx: Some(verdict_tx),
                retained: None,
            })
            .await
            .map_err(|_| "arrival intake closed; the spine is shutting down".to_string())?;
        verdict_rx
            .await
            .map_err(|_| "arrival verdict channel dropped before admission".to_string())
    }

    pub async fn hand_arrival_retained(
        &self,
        artifact: InterfaceArtifactFile,
        evidence: Vec<InterfaceActivationEvidence>,
        delivery_id: &str,
    ) -> Result<octopus_ledger::ArtifactDelivery, String> {
        validate_retained_delivery_id(delivery_id)?;
        let limits = self.required_host_io_limits()?;
        let artifact = artifact
            .capture_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .map_err(|error| format!("interface output carrier capture failed: {error}"))?;
        let (reply, result) = tokio::sync::oneshot::channel();
        self.arrivals
            .send(InterfaceActivationArrival {
                artifact,
                interface: self.interface.clone(),
                evidence,
                verdict_tx: None,
                retained: Some(crate::RetainedInterfaceArrival {
                    id: delivery_id.to_string(),
                    reply,
                }),
            })
            .await
            .map_err(|_| "arrival intake closed; the spine is shutting down".to_string())?;
        result
            .await
            .map_err(|_| "retained arrival reply channel dropped before completion".to_string())?
    }

    pub async fn hand_admitted_arrival(
        &self,
        artifact: InterfaceArtifactFile,
        evidence: Vec<InterfaceActivationEvidence>,
    ) -> Result<u64, String> {
        match self.hand_arrival(artifact, evidence).await? {
            DeliveryVerdict::Admitted { seq } => Ok(seq),
            DeliveryVerdict::Refused { reason, detail } => Err(format!(
                "runtime refused interface '{}' output as '{}': {}",
                self.interface.as_str(),
                reason.as_str(),
                detail
            )),
        }
    }

    pub fn failure_artifact(
        &self,
        source_movement_seq: u64,
        failure_class: &str,
        details: serde_json::Value,
    ) -> Result<InterfaceArtifactFile, String> {
        let payload = serde_json::to_vec(&serde_json::json!({
            "source_movement_seq": source_movement_seq,
            "failure_class": failure_class,
            "details": details,
        }))
        .map_err(|error| format!("interface failure artifact did not encode: {error}"))?;
        InterfaceArtifactFile::write(
            self.var_dir.join("failure-artifacts"),
            &format!("{}.failure", self.interface.as_str()),
            &payload,
        )
        .map_err(|error| format!("interface failure artifact did not materialize: {error}"))
    }

    pub async fn hand_admitted_failure(
        &self,
        source_movement_seq: u64,
        failure_class: &str,
        details: serde_json::Value,
    ) -> Result<u64, String> {
        let artifact = self.failure_artifact(source_movement_seq, failure_class, details)?;
        self.hand_admitted_arrival(artifact, Vec::new()).await
    }

    pub fn pinned_run_record_bytes(
        &self,
        envelope: &InterfaceInvocationEnvelope,
    ) -> Result<Vec<u8>, String> {
        if !self.blobs_dir.is_absolute() {
            return Err(format!(
                "interface '{}' requires an explicit blob read handle for the pinned run record",
                self.interface.as_str()
            ));
        }
        let runtime = envelope
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.runtime.as_ref())
            .ok_or_else(|| {
                format!(
                    "interface '{}' invocation carries no runtime metadata",
                    self.interface.as_str()
                )
            })?;
        let expected = runtime
            .run_record_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .ok_or_else(|| {
                format!(
                    "interface '{}' invocation carries no valid run_record_hash",
                    self.interface.as_str()
                )
            })?;
        let path = self.blobs_dir.join(expected.to_hex());
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "interface '{}' pinned run blob '{}' cannot be read: {error}",
                self.interface.as_str(),
                path.display()
            )
        })?;
        let observed = ContentHash::of_bytes(&bytes);
        if observed != expected {
            return Err(format!(
                "interface '{}' pinned run blob '{}' hashes to {}, not {}",
                self.interface.as_str(),
                path.display(),
                observed.to_hex(),
                expected.to_hex()
            ));
        }
        Ok(bytes)
    }
}

pub type InterfaceLifeFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

pub type InterfaceLiftFn = Arc<dyn Fn(InterfaceLiftContext) -> InterfaceLifeFuture + Send + Sync>;

pub fn interfaces_mentioned_in_live_runs(
    run_records_dir: &Path,
) -> Result<BTreeSet<InterfaceId>, String> {
    let mut index = RunRecordsIndex::new();
    index.catch_up(run_records_dir).map_err(|error| {
        format!(
            "run records directory '{}' cannot be indexed: {error}",
            run_records_dir.display()
        )
    })?;
    Ok(index.interfaces_requiring_life())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnLifeConfig {
    pub processes: Vec<SpawnLifeCommand>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnLifeCommand {
    pub bin: String,
    pub io_limits: InterfaceHostIoLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doctor: Option<SpawnLifeDoctorCommand>,
    #[serde(default)]
    pub private_var_directories: BTreeSet<String>,
    #[serde(default)]
    pub args: Vec<SpawnLifeArgument>,
    #[serde(default)]
    pub required_files: BTreeSet<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub inherit_env: BTreeSet<String>,
    #[serde(default)]
    pub required_env: BTreeSet<String>,
    #[serde(default)]
    pub inherit_env_from_interface: BTreeSet<String>,
    pub authority: ProcessAuthoritySpec,
    #[serde(default)]
    pub coordinate_root_slots: BTreeSet<String>,
    pub read_handles: BTreeSet<InterfaceProcessReadHandle>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnLifeDoctorCommand {
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug)]
pub struct ResolvedSpawnLifeExecutable {
    pub executable: PathBuf,
    pub working_directory: PathBuf,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum SpawnLifeArgument {
    Literal(String),
    RepositoryFile(SpawnLifeRepositoryFileArgument),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnLifeRepositoryFileArgument {
    pub repository_file: String,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceProcessReadHandle {
    Ledger,
    Blobs,
    LedgerIndex,
}

#[derive(Clone, Debug)]
pub struct ResolvedSpawnLifeConfig {
    pub config: SpawnLifeConfig,
    pub fingerprint: ContentHash,
    pub interface_descriptor: octopus_interfaces::InterfaceDescriptor,
}

#[derive(Clone, Debug)]
pub struct ValidatedSpawnLifeConfig {
    pub config: SpawnLifeConfig,
    pub interface_descriptor: octopus_interfaces::InterfaceDescriptor,
}

struct LoadedSpawnLifeConfig {
    own_files: BTreeMap<String, Vec<u8>>,
    config: SpawnLifeConfig,
    interface_descriptor: octopus_interfaces::InterfaceDescriptor,
}

struct ResolvedInterfaceDescriptorFiles {
    interface_dir: PathBuf,
    own_files: BTreeMap<String, Vec<u8>>,
    descriptor: octopus_interfaces::InterfaceDescriptor,
}

fn resolve_interface_descriptor_files(
    registry_root: &Path,
    interface: &InterfaceId,
) -> Result<Option<ResolvedInterfaceDescriptorFiles>, String> {
    let interface_dir = registry_root.join("interface").join(interface.as_str());
    match std::fs::symlink_metadata(&interface_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "interface directory '{}' is not a regular directory",
                interface_dir.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "interface directory '{}' cannot be inspected: {error}",
                interface_dir.display()
            ));
        }
    }
    let own_files = read_interface_json_files(&interface_dir)?;
    let descriptor_path = interface_dir.join("interface.json");
    let descriptor_bytes = own_files.get("interface.json").ok_or_else(|| {
        format!(
            "interface descriptor '{}' is absent",
            descriptor_path.display()
        )
    })?;
    let descriptor: octopus_interfaces::InterfaceDescriptor =
        octopus_descriptors::parse_strict(descriptor_bytes).map_err(|error| {
            format!(
                "interface descriptor '{}' is malformed: {error}",
                descriptor_path.display()
            )
        })?;
    if descriptor.id != *interface {
        return Err(format!(
            "interface descriptor '{}' has id '{}' instead of folder id '{}'",
            descriptor_path.display(),
            descriptor.id,
            interface
        ));
    }
    Ok(Some(ResolvedInterfaceDescriptorFiles {
        interface_dir,
        own_files,
        descriptor,
    }))
}

pub fn resolve_interface_descriptor(
    registry_root: &Path,
    interface: &InterfaceId,
) -> Result<Option<octopus_interfaces::InterfaceDescriptor>, String> {
    Ok(
        resolve_interface_descriptor_files(registry_root, interface)?
            .map(|resolved| resolved.descriptor),
    )
}

pub fn spawn_life_config_of(
    registry_root: &Path,
    interface: &InterfaceId,
) -> Result<Option<SpawnLifeConfig>, String> {
    Ok(resolve_spawn_life_config(registry_root, interface)?.map(|resolved| resolved.config))
}

pub fn resolve_spawn_life_config(
    registry_root: &Path,
    interface: &InterfaceId,
) -> Result<Option<ResolvedSpawnLifeConfig>, String> {
    resolve_spawn_life_config_with_verification(
        registry_root,
        interface,
        &mut ImportedExecutableVerification::default(),
    )
}

pub(crate) fn resolve_spawn_life_config_with_verification(
    registry_root: &Path,
    interface: &InterfaceId,
    verification: &mut ImportedExecutableVerification,
) -> Result<Option<ResolvedSpawnLifeConfig>, String> {
    let Some(loaded) = load_spawn_life_config(registry_root, interface)? else {
        return Ok(None);
    };
    let mut fingerprint_material = Vec::new();
    append_json_fingerprint(
        &mut fingerprint_material,
        interface.as_str(),
        &loaded.own_files,
    );
    append_life_repository_file_fingerprints(
        &mut fingerprint_material,
        registry_root,
        interface,
        &loaded.config,
    )?;
    append_owner_life_dependencies(
        &mut fingerprint_material,
        registry_root,
        interface,
        &loaded.config,
        true,
    )?;
    append_imported_life_dependency_fingerprints(
        &mut fingerprint_material,
        registry_root,
        &loaded.config,
        ImportedDependencyFingerprint::Content,
        Some(verification),
    )?;
    Ok(Some(ResolvedSpawnLifeConfig {
        config: loaded.config,
        fingerprint: ContentHash::of_bytes(&fingerprint_material),
        interface_descriptor: loaded.interface_descriptor,
    }))
}

pub fn validate_spawn_life_config(
    registry_root: &Path,
    interface: &InterfaceId,
) -> Result<Option<ValidatedSpawnLifeConfig>, String> {
    Ok(
        load_spawn_life_config(registry_root, interface)?.map(|loaded| ValidatedSpawnLifeConfig {
            config: loaded.config,
            interface_descriptor: loaded.interface_descriptor,
        }),
    )
}

fn load_spawn_life_config(
    registry_root: &Path,
    interface: &InterfaceId,
) -> Result<Option<LoadedSpawnLifeConfig>, String> {
    let Some(resolved) = resolve_interface_descriptor_files(registry_root, interface)? else {
        return Ok(None);
    };
    let ResolvedInterfaceDescriptorFiles {
        interface_dir,
        own_files,
        descriptor: resolved_descriptor,
    } = resolved;
    let (life_path, life_bytes) = match own_files.get("life.json") {
        Some(bytes) => (interface_dir.join("life.json"), bytes),
        None => return Ok(None),
    };
    if !resolved_descriptor.has_interface_driver() {
        return Err(format!(
            "interface '{}' has executable life '{}' but resolves no interface_driver_ref",
            interface.as_str(),
            life_path.display()
        ));
    }
    let config = parse_spawn_life_config(&life_path, life_bytes)?;
    for process in &config.processes {
        interface_config_env_names(&resolved_descriptor, &process.inherit_env_from_interface)?;
    }
    Ok(Some(LoadedSpawnLifeConfig {
        own_files,
        config,
        interface_descriptor: resolved_descriptor,
    }))
}

pub fn resolve_spawn_life_executable(
    registry_root: &Path,
    interface: &InterfaceId,
    installation_root: &Path,
    command: &SpawnLifeCommand,
) -> Result<ResolvedSpawnLifeExecutable, String> {
    let owner_root = registry_root.join("interface").join(interface.as_str());
    let (declared_program, working_directory) =
        resolve_executable_base(&command.bin, &owner_root, installation_root)?;
    let search_path = command
        .inherit_env
        .contains("PATH")
        .then(|| std::env::var_os("PATH"))
        .flatten();
    let executable = crate::process_authority::resolve_executable(
        &declared_program,
        &working_directory,
        search_path.as_deref(),
    )?;
    if (command.bin.starts_with(OWNER_EXECUTABLE_PREFIX)
        || command.bin.starts_with(INTERFACE_EXECUTABLE_PREFIX))
        && !executable.starts_with(&working_directory)
    {
        return Err(format!(
            "interface-owned executable '{}' escapes owner directory '{}'",
            executable.display(),
            working_directory.display()
        ));
    }
    Ok(ResolvedSpawnLifeExecutable {
        executable,
        working_directory,
    })
}

pub(crate) fn resolved_spawn_life_source_fingerprint(
    registry_root: &Path,
    interface: &InterfaceId,
    resolved: &ResolvedSpawnLifeConfig,
    verification: &mut ImportedExecutableVerification,
) -> Result<ContentHash, String> {
    let mut material = Vec::new();
    append_json_metadata_fingerprint(
        &mut material,
        &registry_root.join("interface").join(interface.as_str()),
        None,
    )?;
    let repository_root = registry_root.parent().ok_or_else(|| {
        format!(
            "registry root '{}' has no repository parent",
            registry_root.display()
        )
    })?;
    for declared in life_repository_files(&resolved.config) {
        let path = resolve_life_file(&declared, registry_root, interface, repository_root)?;
        append_path_metadata_fingerprint(
            &mut material,
            &format!("repository-file/{declared}"),
            &path,
            true,
        )?;
    }
    append_imported_life_dependency_fingerprints(
        &mut material,
        registry_root,
        &resolved.config,
        ImportedDependencyFingerprint::Source,
        Some(verification),
    )?;
    append_owner_life_dependencies(
        &mut material,
        registry_root,
        interface,
        &resolved.config,
        false,
    )?;
    Ok(ContentHash::of_bytes(&material))
}

pub(crate) fn interface_private_environment_source_fingerprint(
    var_dir: &Path,
) -> Result<ContentHash, String> {
    let path = var_dir.join(INTERFACE_PRIVATE_ENVIRONMENT_FILE);
    let mut material = Vec::new();
    append_fingerprint_part(
        &mut material,
        "interface-private-environment-path",
        path.as_os_str().as_encoded_bytes(),
    );
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            append_fingerprint_part(&mut material, "state", b"absent");
            return Ok(ContentHash::of_bytes(&material));
        }
        Err(error) => {
            return Err(format!(
                "interface private environment '{}' cannot be inspected: {error}",
                path.display()
            ));
        }
    };
    let var_dir_metadata = std::fs::symlink_metadata(var_dir).map_err(|error| {
        format!(
            "interface private directory '{}' cannot be inspected: {error}",
            var_dir.display()
        )
    })?;
    validate_private_environment_directory_metadata(var_dir, &var_dir_metadata)?;
    validate_private_environment_file_metadata(&path, &metadata)?;
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "interface private environment '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    append_fingerprint_part(&mut material, "state", b"present");
    append_fingerprint_part(&mut material, "content", &bytes);
    Ok(ContentHash::of_bytes(&material))
}

fn append_json_metadata_fingerprint(
    material: &mut Vec<u8>,
    dir: &Path,
    selected: Option<&[&str]>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        format!(
            "interface directory '{}' cannot be read for freshness: {error}",
            dir.display()
        )
    })?;
    let selected = selected.map(|names| names.iter().copied().collect::<BTreeSet<_>>());
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "interface directory '{}' contains an unreadable entry during freshness: {error}",
                dir.display()
            )
        })?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.extension().and_then(|extension| extension.to_str()) != Some("json")
            || selected
                .as_ref()
                .is_some_and(|selected| !selected.contains(name))
        {
            continue;
        }
        paths.push(path);
    }
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("interface JSON '{}' has a non-UTF-8 name", path.display()))?;
        append_path_metadata_fingerprint(material, name, &path, true)?;
    }
    Ok(())
}

fn append_path_metadata_fingerprint(
    material: &mut Vec<u8>,
    name: &str,
    path: &Path,
    read_unsettled: bool,
) -> Result<(), String> {
    let observed = SystemTime::now();
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        format!(
            "interface life dependency '{}' cannot be inspected: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "interface life dependency '{}' is not a regular file",
            path.display()
        ));
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    append_fingerprint_part(material, name, path.as_os_str().as_encoded_bytes());
    append_fingerprint_part(material, "length", &metadata.len().to_le_bytes());
    append_fingerprint_part(material, "modified", &modified.to_le_bytes());
    #[cfg(unix)]
    for value in executable_file_stamp(&metadata) {
        append_fingerprint_part(material, "file-state", &value.to_le_bytes());
    }
    if read_unsettled && !FileStamp::of(&metadata).is_settled(observed) {
        let digest = ImportedExecutableVerification::default().digest(path, &metadata)?;
        append_fingerprint_part(material, "unsettled-content", digest.sha256.as_bytes());
    }
    Ok(())
}

fn append_life_repository_file_fingerprints(
    material: &mut Vec<u8>,
    registry_root: &Path,
    interface: &InterfaceId,
    config: &SpawnLifeConfig,
) -> Result<(), String> {
    let repository_root = registry_root.parent().ok_or_else(|| {
        format!(
            "registry root '{}' has no repository parent",
            registry_root.display()
        )
    })?;
    for declared in life_repository_files(config) {
        let path = resolve_life_file(&declared, registry_root, interface, repository_root)?;
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        let digest = ImportedExecutableVerification::default().digest(&path, &metadata)?;
        append_fingerprint_part(
            material,
            &format!("repository-file/{declared}"),
            digest.sha256.as_bytes(),
        );
    }
    Ok(())
}

fn life_repository_files(config: &SpawnLifeConfig) -> BTreeSet<String> {
    let mut declared_files = BTreeSet::new();
    for process in &config.processes {
        declared_files.extend(process.required_files.iter().cloned());
        declared_files.extend(process.args.iter().filter_map(|argument| match argument {
            SpawnLifeArgument::RepositoryFile(argument) => Some(argument.repository_file.clone()),
            SpawnLifeArgument::Literal(value) if value.starts_with(OWNER_EXECUTABLE_PREFIX) => {
                Some(value.clone())
            }
            SpawnLifeArgument::Literal(_) => None,
        }));
    }
    declared_files
}

fn resolve_life_file(
    declared: &str,
    registry_root: &Path,
    interface: &InterfaceId,
    repository_root: &Path,
) -> Result<PathBuf, String> {
    let Some(relative) = declared.strip_prefix(OWNER_EXECUTABLE_PREFIX) else {
        return resolve_required_repository_file(declared, repository_root);
    };
    let interface_root = std::fs::canonicalize(registry_root.join("interface"))
        .map_err(|error| error.to_string())?;
    let owner = std::fs::canonicalize(interface_root.join(interface.as_str()))
        .map_err(|error| error.to_string())?;
    if owner != interface_root.join(interface.as_str())
        || !Path::new(relative).starts_with("packages")
    {
        return Err(format!(
            "owner life file '{declared}' escapes its exact interface package"
        ));
    }
    let file = resolve_required_repository_file(relative, &owner)?;
    if file != owner.join(relative) {
        return Err(format!(
            "owner life file '{declared}' traverses a symbolic link"
        ));
    }
    Ok(file)
}

fn append_owner_life_dependencies(
    material: &mut Vec<u8>,
    registry_root: &Path,
    interface: &InterfaceId,
    config: &SpawnLifeConfig,
    content: bool,
) -> Result<(), String> {
    let root = registry_root
        .parent()
        .ok_or("interface registry has no installation parent")?;
    let files = config
        .processes
        .iter()
        .map(|process| &process.bin)
        .filter(|bin| bin.starts_with(OWNER_EXECUTABLE_PREFIX))
        .collect::<BTreeSet<_>>();
    for declared in files {
        let file = resolve_life_file(&declared, registry_root, interface, root)?;
        crate::process_authority::resolve_executable(
            file.to_str().ok_or("life executable path is not UTF-8")?,
            root,
            None,
        )?;
        append_path_metadata_fingerprint(material, &declared, &file, !content)?;
        if content {
            let metadata = std::fs::symlink_metadata(&file).map_err(|error| error.to_string())?;
            let digest = ImportedExecutableVerification::default().digest(&file, &metadata)?;
            append_fingerprint_part(material, declared, digest.sha256.as_bytes());
        }
    }
    Ok(())
}

struct ResolvedImportedExecutableDependency {
    source_interface: InterfaceId,
    source_owner: PathBuf,
    source_descriptor_bytes: Vec<u8>,
    executable_relative: PathBuf,
    executable_path: PathBuf,
    build_receipt_bytes: Option<Vec<u8>>,
    receipt_verified_executable_digest: Option<ExecutableDigest>,
}

#[derive(serde::Deserialize)]
struct ImportedExecutableBuildReceipt {
    schema_version: u64,
    executable: String,
    executable_sha256: String,
    executable_bytes: u64,
}

#[derive(Clone, Copy)]
enum ImportedDependencyFingerprint {
    Content,
    Source,
}

#[cfg(test)]
fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", <Sha256 as sha2::Digest>::digest(bytes))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Clone)]
struct ExecutableDigest {
    bytes: u64,
    sha256: String,
}

#[derive(Default)]
pub(crate) struct ImportedExecutableVerification {
    #[cfg(unix)]
    digests: BTreeMap<PathBuf, CachedExecutableDigest>,
}

#[cfg(unix)]
struct CachedExecutableDigest {
    stamp: [u64; 10],
    digest: ExecutableDigest,
    observed: bool,
    settled: bool,
}

#[cfg(unix)]
fn executable_file_stamp(metadata: &std::fs::Metadata) -> [u64; 10] {
    use std::os::unix::fs::MetadataExt;
    [
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
        metadata.mode() as u64,
        metadata.uid() as u64,
        metadata.gid() as u64,
    ]
}

impl ImportedExecutableVerification {
    pub(crate) fn finish_pass(&mut self) {
        #[cfg(unix)]
        self.digests
            .retain(|_, cached| std::mem::take(&mut cached.observed));
    }

    fn digest(
        &mut self,
        path: &Path,
        expected: &std::fs::Metadata,
    ) -> Result<ExecutableDigest, String> {
        let inspect = |error| {
            format!(
                "imported interface executable '{}' cannot be read for receipt verification: {error}",
                path.display()
            )
        };
        let changed = || {
            format!(
                "imported interface executable '{}' changed during receipt verification",
                path.display()
            )
        };
        let observed_before_read = SystemTime::now();
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(path).map_err(inspect)?;
        let before = file.metadata().map_err(inspect)?;
        if !before.is_file() || before.len() != expected.len() {
            return Err(changed());
        }
        #[cfg(unix)]
        {
            let stamp = executable_file_stamp(&before);
            if stamp != executable_file_stamp(expected) {
                self.digests.remove(path);
                return Err(changed());
            }
            if let Some(cached) = self.digests.get_mut(path)
                && cached.stamp == stamp
                && cached.settled
                && FileStamp::of(&before).is_settled(observed_before_read)
            {
                cached.observed = true;
                return Ok(cached.digest.clone());
            }
            self.digests.remove(path);
        }
        let mut hash = <Sha256 as sha2::Digest>::new();
        let mut bytes = 0;
        let mut buffer = [0; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(inspect)?;
            if read == 0 {
                break;
            }
            bytes += read as u64;
            if bytes > before.len() {
                return Err(changed());
            }
            sha2::Digest::update(&mut hash, &buffer[..read]);
        }
        let after = file.metadata().map_err(inspect)?;
        let current = std::fs::symlink_metadata(path).map_err(inspect)?;
        if bytes != before.len() || !current.is_file() || after.len() != current.len() {
            return Err(changed());
        }
        #[cfg(unix)]
        if executable_file_stamp(&before) != executable_file_stamp(&after)
            || executable_file_stamp(&before) != executable_file_stamp(&current)
        {
            return Err(changed());
        }
        let digest = ExecutableDigest {
            bytes,
            sha256: format!("{:x}", sha2::Digest::finalize(hash)),
        };
        #[cfg(unix)]
        self.digests.insert(
            path.to_path_buf(),
            CachedExecutableDigest {
                stamp: executable_file_stamp(&before),
                digest: digest.clone(),
                observed: true,
                settled: FileStamp::of(&before).is_settled(observed_before_read),
            },
        );
        Ok(digest)
    }
}

fn parse_imported_executable_reference(
    declared_program: &str,
) -> Result<Option<(InterfaceId, PathBuf)>, String> {
    let Some(relative) = declared_program.strip_prefix(INTERFACE_EXECUTABLE_PREFIX) else {
        return Ok(None);
    };
    let relative_path = Path::new(relative);
    let mut components = relative_path.components();
    let source_interface = match components.next() {
        Some(Component::Normal(interface)) => interface
            .to_str()
            .ok_or_else(|| format!("interface-owned executable is invalid: {declared_program}"))?,
        _ => {
            return Err(format!(
                "interface-owned executable is invalid: {declared_program}"
            ));
        }
    };
    let executable_relative = components.collect::<PathBuf>();
    if executable_relative.as_os_str().is_empty()
        || executable_relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "interface-owned executable is invalid: {declared_program}"
        ));
    }
    Ok(Some((
        InterfaceId::new(source_interface),
        executable_relative,
    )))
}

fn resolve_imported_executable_dependency(
    registry_root: &Path,
    declared_program: &str,
    verification: Option<&mut ImportedExecutableVerification>,
) -> Result<Option<ResolvedImportedExecutableDependency>, String> {
    let Some((source_interface, executable_relative)) =
        parse_imported_executable_reference(declared_program)?
    else {
        return Ok(None);
    };
    let resolved_source = resolve_interface_descriptor_files(registry_root, &source_interface)?
        .ok_or_else(|| format!("interface executable owner '{source_interface}' is absent"))?;
    let source_descriptor_path = resolved_source.interface_dir.join("interface.json");
    let source_descriptor_bytes = resolved_source
        .own_files
        .get("interface.json")
        .cloned()
        .ok_or_else(|| {
            format!(
                "interface descriptor '{}' is absent",
                source_descriptor_path.display()
            )
        })?;
    let interface_root =
        std::fs::canonicalize(registry_root.join("interface")).map_err(|error| {
            format!(
                "interface registry root '{}' cannot be canonicalized: {error}",
                registry_root.join("interface").display()
            )
        })?;
    let source_owner = std::fs::canonicalize(&resolved_source.interface_dir).map_err(|error| {
        format!(
            "interface executable owner '{}' cannot be canonicalized: {error}",
            resolved_source.interface_dir.display()
        )
    })?;
    if source_owner != interface_root.join(source_interface.as_str()) {
        return Err(format!(
            "interface executable owner '{}' escapes the interface registry root",
            source_owner.display()
        ));
    }
    let executable_path = source_owner.join(&executable_relative);
    let executable_metadata = std::fs::symlink_metadata(&executable_path).map_err(|error| {
        format!(
            "imported interface executable '{}' cannot be inspected: {error}",
            executable_path.display()
        )
    })?;
    if executable_metadata.file_type().is_symlink() || !executable_metadata.is_file() {
        return Err(format!(
            "imported interface executable '{}' is not a regular file",
            executable_path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if executable_metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "imported interface executable '{}' is not executable",
                executable_path.display()
            ));
        }
    }
    let canonical_executable = std::fs::canonicalize(&executable_path).map_err(|error| {
        format!(
            "imported interface executable '{}' cannot be canonicalized: {error}",
            executable_path.display()
        )
    })?;
    if canonical_executable != executable_path || !canonical_executable.starts_with(&source_owner) {
        return Err(format!(
            "imported interface executable '{}' escapes its exact source owner",
            executable_path.display()
        ));
    }
    let executable_directory = canonical_executable.parent().ok_or_else(|| {
        format!(
            "imported interface executable '{}' has no parent directory",
            canonical_executable.display()
        )
    })?;
    let build_receipt_root = executable_directory.parent().ok_or_else(|| {
        format!(
            "imported interface executable '{}' has no package root",
            canonical_executable.display()
        )
    })?;
    if !build_receipt_root.starts_with(&source_owner) {
        return Err(format!(
            "imported interface executable '{}' has no package root inside its exact source owner",
            canonical_executable.display()
        ));
    }
    let build_receipt_path = build_receipt_root.join(IMPORTED_EXECUTABLE_BUILD_RECEIPT_FILE);
    let (build_receipt_bytes, receipt_verified_executable_digest) = match std::fs::symlink_metadata(
        &build_receipt_path,
    ) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let bytes = std::fs::read(&build_receipt_path).map_err(|error| {
                format!(
                    "imported executable build receipt '{}' cannot be read: {error}",
                    build_receipt_path.display()
                )
            })?;
            let receipt: ImportedExecutableBuildReceipt =
                serde_json::from_slice(&bytes).map_err(|error| {
                    format!(
                        "imported executable build receipt '{}' is malformed: {error}",
                        build_receipt_path.display()
                    )
                })?;
            let receipt_executable = Path::new(&receipt.executable);
            if receipt.schema_version != 1
                || receipt_executable.is_absolute()
                || receipt_executable
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
                || build_receipt_root.join(receipt_executable) != canonical_executable
            {
                return Err(format!(
                    "imported executable build receipt '{}' does not match executable '{}'",
                    build_receipt_path.display(),
                    canonical_executable.display()
                ));
            }
            if !is_sha256_hex(&receipt.executable_sha256) {
                return Err(format!(
                    "imported executable build receipt '{}' has malformed executable_sha256",
                    build_receipt_path.display()
                ));
            }
            let mut local_verification = ImportedExecutableVerification::default();
            let digest = verification.unwrap_or(&mut local_verification)
                .digest(&canonical_executable, &executable_metadata)?;
            if receipt.executable_bytes != executable_metadata.len()
                || receipt.executable_bytes != digest.bytes
            {
                return Err(format!(
                    "imported executable build receipt '{}' does not match executable '{}'",
                    build_receipt_path.display(),
                    canonical_executable.display()
                ));
            }
            if receipt.executable_sha256 != digest.sha256 {
                return Err(format!(
                    "imported executable build receipt '{}' SHA-256 does not match executable '{}'",
                    build_receipt_path.display(),
                    canonical_executable.display()
                ));
            }
            (Some(bytes), Some(digest))
        }
        Ok(_) => {
            return Err(format!(
                "imported executable build receipt '{}' is not a regular file",
                build_receipt_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(error) => {
            return Err(format!(
                "imported executable build receipt '{}' cannot be inspected: {error}",
                build_receipt_path.display()
            ));
        }
    };
    Ok(Some(ResolvedImportedExecutableDependency {
        source_interface,
        source_owner,
        source_descriptor_bytes,
        executable_relative,
        executable_path: canonical_executable,
        build_receipt_bytes,
        receipt_verified_executable_digest,
    }))
}

fn imported_executable_declarations(config: &SpawnLifeConfig) -> BTreeSet<String> {
    config
        .processes
        .iter()
        .map(|process| process.bin.clone())
        .filter(|bin| bin.starts_with(INTERFACE_EXECUTABLE_PREFIX))
        .collect()
}

fn append_imported_life_dependency_fingerprints(
    material: &mut Vec<u8>,
    registry_root: &Path,
    config: &SpawnLifeConfig,
    fingerprint: ImportedDependencyFingerprint,
    verification: Option<&mut ImportedExecutableVerification>,
) -> Result<(), String> {
    let mut local_verification = ImportedExecutableVerification::default();
    let verification = verification.unwrap_or(&mut local_verification);
    for declared in imported_executable_declarations(config) {
        let dependency = resolve_imported_executable_dependency(
            registry_root,
            &declared,
            Some(&mut *verification),
        )?
        .ok_or_else(|| format!("imported executable declaration '{declared}' is invalid"))?;
        let prefix = format!("interface-import/{}", dependency.source_interface);
        append_fingerprint_part(
            material,
            &format!("{prefix}/interface.json"),
            &dependency.source_descriptor_bytes,
        );
        let executable_name = format!(
            "{prefix}/{}",
            dependency.executable_relative.to_string_lossy()
        );
        match fingerprint {
            ImportedDependencyFingerprint::Content => {
                let digest = match dependency.receipt_verified_executable_digest {
                    Some(digest) => digest,
                    None => {
                        let metadata = std::fs::symlink_metadata(&dependency.executable_path)
                            .map_err(|error| error.to_string())?;
                        verification.digest(&dependency.executable_path, &metadata)?
                    }
                };
                append_fingerprint_part(material, &executable_name, digest.sha256.as_bytes());
            }
            ImportedDependencyFingerprint::Source => append_path_metadata_fingerprint(
                material,
                &executable_name,
                &dependency.executable_path,
                true,
            )?,
        }
        append_fingerprint_part(
            material,
            &format!("{prefix}/build-receipt-state"),
            if dependency.build_receipt_bytes.is_some() {
                b"present"
            } else {
                b"absent"
            },
        );
        if let Some(bytes) = dependency.build_receipt_bytes {
            append_fingerprint_part(
                material,
                &format!("{prefix}/{IMPORTED_EXECUTABLE_BUILD_RECEIPT_FILE}"),
                &bytes,
            );
        }
    }
    Ok(())
}

fn parse_spawn_life_config(path: &Path, bytes: &[u8]) -> Result<SpawnLifeConfig, String> {
    let config: SpawnLifeConfig = serde_json::from_slice(bytes)
        .map_err(|e| format!("life sidecar '{}' is malformed: {e}", path.display()))?;
    if config.processes.is_empty() {
        return Err(format!(
            "life sidecar '{}' declares no processes",
            path.display()
        ));
    }
    for process in &config.processes {
        if process.bin.trim().is_empty() {
            return Err(format!(
                "life sidecar '{}' has empty process bin",
                path.display()
            ));
        }
        process.io_limits.validate().map_err(|error| {
            format!(
                "life sidecar '{}' process io_limits are invalid: {error}",
                path.display()
            )
        })?;
        for relative in &process.private_var_directories {
            validate_private_var_directory(relative).map_err(|error| {
                format!(
                    "life sidecar '{}' has invalid private_var_directories entry: {error}",
                    path.display()
                )
            })?;
        }
        process.authority.validate().map_err(|error| {
            format!(
                "life sidecar '{}' has invalid process authority: {error}",
                path.display()
            )
        })?;
        if let Some(doctor) = &process.doctor {
            if doctor.timeout_ms == 0 || doctor.timeout_ms > 60_000 {
                return Err(format!(
                    "life sidecar '{}' process doctor timeout_ms must be between 1 and 60000",
                    path.display()
                ));
            }
            if doctor.args.len() > 32 {
                return Err(format!(
                    "life sidecar '{}' process doctor declares more than 32 arguments",
                    path.display()
                ));
            }
            for (index, argument) in doctor.args.iter().enumerate() {
                if argument.len() > 4096 || argument.contains('\0') {
                    return Err(format!(
                        "life sidecar '{}' process doctor args[{index}] is invalid",
                        path.display()
                    ));
                }
            }
        }
        for slot in &process.coordinate_root_slots {
            if slot.is_empty() || slot.trim() != slot {
                return Err(format!(
                    "life sidecar '{}' contains invalid coordinate root slot {:?}",
                    path.display(),
                    slot
                ));
            }
        }
        for name in &process.inherit_env {
            if !is_valid_life_env_name(name) || is_reserved_life_env(name) {
                return Err(format!(
                    "life sidecar '{}' contains forbidden inherited env '{}'",
                    path.display(),
                    name
                ));
            }
            if process.env.contains_key(name) {
                return Err(format!(
                    "life sidecar '{}' declares env '{}' as literal and inherited",
                    path.display(),
                    name
                ));
            }
        }
        for name in &process.required_env {
            if !is_valid_life_env_name(name)
                || is_reserved_life_env(name)
                || !process.inherit_env.contains(name)
            {
                return Err(format!(
                    "life sidecar '{}' contains required env '{}' that is not a valid inherited env",
                    path.display(),
                    name
                ));
            }
        }
        if process
            .inherit_env_from_interface
            .iter()
            .any(|pointer| !pointer.starts_with('/') || pointer.trim() == "/")
        {
            return Err(format!(
                "life sidecar '{}' contains an invalid interface env JSON pointer",
                path.display()
            ));
        }
        if process
            .env
            .keys()
            .any(|name| !is_valid_life_env_name(name) || is_reserved_life_env(name))
        {
            return Err(format!(
                "life sidecar '{}' contains an empty or reserved literal env name",
                path.display()
            ));
        }
        for name in process
            .authority
            .read_env_paths
            .iter()
            .chain(&process.authority.write_env_paths)
            .chain(&process.authority.executable_env_paths)
            .chain(&process.authority.optional_read_env_paths)
            .chain(&process.authority.optional_write_env_paths)
        {
            if !is_valid_life_env_name(name) || is_reserved_life_env(name) {
                return Err(format!(
                    "life sidecar '{}' process authority names reserved env '{}'",
                    path.display(),
                    name
                ));
            }
        }
        for declaration in process
            .authority
            .read_env_subpaths
            .iter()
            .chain(&process.authority.write_env_subpaths)
            .chain(&process.authority.executable_env_subpaths)
        {
            if (!is_valid_life_env_name(&declaration.env) || is_reserved_life_env(&declaration.env))
                && declaration.env != "OCTOPUS_VAR_DIR"
            {
                return Err(format!(
                    "life sidecar '{}' process authority names reserved env '{}'",
                    path.display(),
                    declaration.env
                ));
            }
        }
    }
    Ok(config)
}

fn validate_private_var_directory(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "private VAR directory must be a non-empty normal relative path, got {value:?}"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn materialize_private_var_directories(
    root: &Path,
    directories: &BTreeSet<String>,
) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root_metadata = std::fs::symlink_metadata(root).map_err(|error| {
        format!(
            "life private VAR root '{}' cannot be inspected: {error}",
            root.display()
        )
    })?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!(
            "life private VAR root '{}' is not a real directory",
            root.display()
        ));
    }
    if root_metadata.uid() != unsafe { libc::geteuid() } {
        return Err(format!(
            "life private VAR root '{}' is not owned by the current user",
            root.display()
        ));
    }
    if root_metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "life private VAR root '{}' is not owner-only",
            root.display()
        ));
    }
    let root_name = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| format!("life private VAR root '{}' contains NUL", root.display()))?;
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(format!(
            "life private VAR root '{}' cannot be opened: {}",
            root.display(),
            std::io::Error::last_os_error()
        ));
    }
    let root_directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    for relative in directories {
        validate_private_var_directory(relative)?;
        let mut directory = root_directory.try_clone().map_err(|error| {
            format!(
                "life private VAR root '{}' descriptor cannot be cloned: {error}",
                root.display()
            )
        })?;
        for component in Path::new(relative).components() {
            let std::path::Component::Normal(component) = component else {
                return Err(format!(
                    "life private VAR directory {relative:?} contains a non-normal component"
                ));
            };
            let component = CString::new(component.as_bytes())
                .map_err(|_| format!("life private VAR directory {relative:?} contains NUL"))?;
            let created =
                unsafe { libc::mkdirat(directory.as_raw_fd(), component.as_ptr(), 0o700) };
            if created < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(format!(
                        "life private VAR directory {relative:?} cannot be created: {error}"
                    ));
                }
            }
            let opened = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    component.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if opened < 0 {
                return Err(format!(
                    "life private VAR directory {relative:?} cannot be opened without following links: {}",
                    std::io::Error::last_os_error()
                ));
            }
            let opened = unsafe { OwnedFd::from_raw_fd(opened) };
            let metadata = std::fs::File::from(opened.try_clone().map_err(|error| {
                format!(
                    "life private VAR directory {relative:?} descriptor cannot be cloned: {error}"
                )
            })?)
            .metadata()
            .map_err(|error| {
                format!("life private VAR directory {relative:?} cannot be inspected: {error}")
            })?;
            if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
                return Err(format!(
                    "life private VAR directory {relative:?} is not an owner-controlled directory"
                ));
            }
            if unsafe { libc::fchmod(opened.as_raw_fd(), 0o700) } < 0 {
                return Err(format!(
                    "life private VAR directory {relative:?} cannot be made owner-only: {}",
                    std::io::Error::last_os_error()
                ));
            }
            directory = opened;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn materialize_private_var_directories(
    root: &Path,
    directories: &BTreeSet<String>,
) -> Result<(), String> {
    let canonical_root = std::fs::canonicalize(root).map_err(|error| {
        format!(
            "life private VAR root '{}' cannot be canonicalized: {error}",
            root.display()
        )
    })?;
    for relative in directories {
        validate_private_var_directory(relative)?;
        let mut current = canonical_root.clone();
        for component in Path::new(relative).components() {
            let std::path::Component::Normal(component) = component else {
                return Err(format!(
                    "life private VAR directory {relative:?} contains a non-normal component"
                ));
            };
            current.push(component);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    return Err(format!(
                        "life private VAR directory '{}' is not a real directory",
                        current.display()
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&current).map_err(|error| {
                        format!(
                            "life private VAR directory '{}' cannot be created: {error}",
                            current.display()
                        )
                    })?;
                }
                Err(error) => {
                    return Err(format!(
                        "life private VAR directory '{}' cannot be inspected: {error}",
                        current.display()
                    ));
                }
            }
            let canonical = std::fs::canonicalize(&current).map_err(|error| {
                format!(
                    "life private VAR directory '{}' cannot be canonicalized: {error}",
                    current.display()
                )
            })?;
            if !canonical.starts_with(&canonical_root) || canonical == canonical_root {
                return Err(format!(
                    "life private VAR directory '{}' escaped its root",
                    current.display()
                ));
            }
            current = canonical;
        }
    }
    Ok(())
}

fn read_interface_json_files(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        format!(
            "interface directory '{}' cannot be read: {error}",
            dir.display()
        )
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "interface directory '{}' contains an unreadable entry: {error}",
                dir.display()
            )
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "interface JSON '{}' cannot be inspected: {error}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "interface JSON '{}' is not a regular file",
                path.display()
            ));
        }
        paths.push(path);
    }
    paths.sort();
    let mut files = BTreeMap::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("interface JSON '{}' has a non-UTF-8 name", path.display()))?
            .to_string();
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "interface JSON '{}' cannot be read: {error}",
                path.display()
            )
        })?;
        files.insert(name, bytes);
    }
    Ok(files)
}

fn append_json_fingerprint(
    material: &mut Vec<u8>,
    interface: &str,
    files: &BTreeMap<String, Vec<u8>>,
) {
    for (name, bytes) in files {
        append_fingerprint_part(material, &format!("interface/{interface}/{name}"), bytes);
    }
}

fn append_fingerprint_part(material: &mut Vec<u8>, name: &str, bytes: &[u8]) {
    material.extend_from_slice(&(name.len() as u64).to_le_bytes());
    material.extend_from_slice(name.as_bytes());
    material.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    material.extend_from_slice(bytes);
}

fn is_reserved_life_env(name: &str) -> bool {
    matches!(
        name,
        "OCTOPUS_INTERFACE_ID"
            | "OCTOPUS_INTERFACE_DIR"
            | RESOLVED_INTERFACE_DESCRIPTOR_ENV
            | INTERFACE_HOST_IO_LIMITS_ENV
            | "OCTOPUS_VAR_DIR"
            | "OCTOPUS_ROOT_SLOTS_JSON"
            | "OCTOPUS_MOVEMENTS_JSONL"
            | "OCTOPUS_BLOBS_DIR"
            | "OCTOPUS_LEDGER_INDEX_SQLITE"
            | "TMPDIR"
    )
}

fn is_valid_life_env_name(name: &str) -> bool {
    !name.is_empty() && name.trim() == name && !name.contains('=') && !name.contains('\0')
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InterfacePrivateEnvironmentDocument {
    schema_version: u64,
    interface: String,
    values: BTreeMap<String, String>,
}

fn validate_private_environment_file_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "interface private environment '{}' is not a regular file",
            path.display()
        ));
    }
    if metadata.len() > INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES {
        return Err(format!(
            "interface private environment '{}' exceeds {} bytes",
            path.display(),
            INTERFACE_PRIVATE_ENVIRONMENT_MAX_BYTES
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "interface private environment '{}' is not owned by the current user",
                path.display()
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "interface private environment '{}' is not owner-only",
                path.display()
            ));
        }
    }
    Ok(())
}

fn validate_private_environment_directory_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "interface private directory '{}' is not a real directory",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "interface private directory '{}' is not owned by the current user",
                path.display()
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "interface private directory '{}' is not owner-only",
                path.display()
            ));
        }
    }
    Ok(())
}

fn read_interface_private_environment(
    ctx: &InterfaceLiftContext,
    declared_names: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, String> {
    let var_dir = ctx.interface_var_dir();
    let path = var_dir.join(INTERFACE_PRIVATE_ENVIRONMENT_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => {
            return Err(format!(
                "interface '{}' private environment cannot be inspected: {error}",
                ctx.interface.as_str()
            ));
        }
    };
    let var_dir_metadata = std::fs::symlink_metadata(&var_dir).map_err(|error| {
        format!(
            "interface '{}' private directory cannot be inspected: {error}",
            ctx.interface.as_str()
        )
    })?;
    validate_private_environment_directory_metadata(&var_dir, &var_dir_metadata)?;
    validate_private_environment_file_metadata(&path, &metadata)?;
    let bytes = std::fs::read(&path).map_err(|error| {
        format!(
            "interface '{}' private environment cannot be read: {error}",
            ctx.interface.as_str()
        )
    })?;
    let document: InterfacePrivateEnvironmentDocument =
        serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "interface '{}' private environment is malformed: {error}",
                ctx.interface.as_str()
            )
        })?;
    if document.schema_version != 1 || document.interface != ctx.interface.as_str() {
        return Err(format!(
            "interface '{}' private environment has invalid identity or schema",
            ctx.interface.as_str()
        ));
    }
    for (name, value) in &document.values {
        if !declared_names.contains(name) {
            return Err(format!(
                "interface '{}' private environment contains undeclared name '{}'",
                ctx.interface.as_str(),
                name
            ));
        }
        if value.is_empty() || value.contains('\0') {
            return Err(format!(
                "interface '{}' private environment contains an invalid value for '{}'",
                ctx.interface.as_str(),
                name
            ));
        }
    }
    Ok(document.values)
}

fn set_life_host_environment(
    environment: &mut BTreeMap<String, std::ffi::OsString>,
    var_dir: &Path,
) {
    environment.insert(
        "OCTOPUS_VAR_DIR".to_string(),
        var_dir.as_os_str().to_os_string(),
    );
}

fn interface_config_env(
    ctx: &InterfaceLiftContext,
    pointers: &BTreeSet<String>,
    private_environment: &BTreeMap<String, String>,
) -> Result<Vec<(String, std::ffi::OsString)>, String> {
    if pointers.is_empty() {
        return Ok(Vec::new());
    }
    let descriptor = ctx.resolved_interface_descriptor()?;
    let names = interface_config_env_names(&descriptor, pointers)?;
    let mut selected = Vec::new();
    for name in names {
        let ambient = std::env::var_os(&name);
        let private = private_environment.get(&name);
        match (ambient, private) {
            (Some(ambient), Some(private)) => {
                let private = std::ffi::OsString::from(private);
                if ambient != private {
                    return Err(format!(
                        "interface '{}' environment name '{}' differs between ambient and private sources",
                        ctx.interface.as_str(),
                        name
                    ));
                }
                selected.push((name, ambient));
            }
            (Some(value), None) => selected.push((name, value)),
            (None, Some(value)) => {
                selected.push((name, std::ffi::OsString::from(value)));
            }
            (None, None) => {}
        }
    }
    Ok(selected)
}

fn interface_direct_private_env_names(inherited: &BTreeSet<String>) -> BTreeSet<String> {
    inherited
        .iter()
        .filter(|name| name.as_str() != "PATH")
        .cloned()
        .collect()
}

fn interface_config_env_names(
    descriptor: &octopus_interfaces::InterfaceDescriptor,
    pointers: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let value = serde_json::to_value(descriptor)
        .map_err(|error| format!("resolved interface descriptor did not encode: {error}"))?;
    let mut names = BTreeSet::new();
    for pointer in pointers {
        let selected = value.pointer(pointer).ok_or_else(|| {
            format!(
                "life process interface env pointer '{}' is absent from resolved interface '{}'",
                pointer,
                descriptor.id.as_str()
            )
        })?;
        let values = match selected {
            serde_json::Value::String(value) => vec![(None, value.as_str())],
            serde_json::Value::Array(values) => values
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    value
                        .as_str()
                        .map(|value| (Some(index), value))
                        .ok_or_else(|| {
                            format!(
                            "life process interface env pointer '{}' array item {} is not a string",
                            pointer, index
                        )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => {
                return Err(format!(
                    "life process interface env pointer '{}' does not select a string or string array",
                    pointer
                ));
            }
        };
        for (index, value) in values {
            let name = value.trim();
            if !is_valid_life_env_name(value) {
                return Err(match index {
                    Some(index) => format!(
                        "life process interface env pointer '{}' array item {} is empty",
                        pointer, index
                    ),
                    None => format!(
                        "life process interface env pointer '{}' selects an empty string",
                        pointer
                    ),
                });
            }
            if is_reserved_life_env(name) {
                return Err(format!(
                    "life process interface env pointer '{}' selects reserved name '{}'",
                    pointer, name
                ));
            }
            names.insert(name.to_string());
        }
    }
    Ok(names)
}

fn interface_private_pointer_env_names(
    descriptor: &octopus_interfaces::InterfaceDescriptor,
    pointers: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    Ok(interface_config_env_names(descriptor, pointers)?
        .into_iter()
        .filter(|name| name != "PATH")
        .collect())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnedEvidenceLine {
    interface_id: String,
    artifact_id: String,
    label: String,
    bytes: Vec<u8>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SpawnedLifeOutput {
    Ready,
    Arrival {
        request_id: Option<u64>,
        artifact_manifest_path: PathBuf,
        evidence: Vec<SpawnedEvidenceLine>,
    },
    RetainedArrival {
        request_id: u64,
        delivery_id: String,
        artifact_manifest_path: PathBuf,
        evidence: Vec<SpawnedEvidenceLine>,
    },
    SystemEmission {
        request_id: u64,
        artifact: SpawnedLifeEmissionArtifact,
        source_movement_seq: u64,
        to_interface: String,
    },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "carrier", rename_all = "snake_case", deny_unknown_fields)]
enum SpawnedLifeEmissionArtifact {
    Materialized { artifact_manifest_path: PathBuf },
    Reference { artifact: ArtifactRef },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SpawnedLifeInput {
    DeliveryVerdict {
        request_id: u64,
        verdict: DeliveryVerdict,
    },
    RetainedDelivery {
        request_id: u64,
        delivery: Result<octopus_ledger::ArtifactDelivery, String>,
    },
}

fn validate_retained_delivery_id(delivery_id: &str) -> Result<(), String> {
    if delivery_id.is_empty() || delivery_id.len() > 256 {
        return Err("retained delivery identity must contain 1 to 256 bytes".to_string());
    }
    Ok(())
}

pub fn spawn_process_life(
    ctx: InterfaceLiftContext,
    config: SpawnLifeConfig,
) -> InterfaceLifeFuture {
    Box::pin(async move {
        let private_root = ctx.var_dir.parent().ok_or("interface life has no private root")?;
        let ownership = loop {
            if ctx.cancel.is_cancelled() { return Ok(()); }
            if let Some(guard) = crate::InterfaceLifeOwnership::try_acquire_life(private_root, &ctx.interface)? {
                break Arc::new(guard);
            }
            tokio::select! {
                _ = ctx.cancel.cancelled() => return Ok(()),
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {},
            }
        };
        if config.processes.is_empty() {
            return Err(format!(
                "interface '{}' life declares no processes",
                ctx.interface.as_str()
            ));
        }
        let descriptor = ctx.resolved_interface_descriptor()?;
        let declared_private_environment_names = config
            .processes
            .iter()
            .map(|process| {
                let mut names = interface_private_pointer_env_names(
                    &descriptor,
                    &process.inherit_env_from_interface,
                )?;
                names.extend(interface_direct_private_env_names(&process.inherit_env));
                Ok::<BTreeSet<String>, String>(names)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<BTreeSet<_>>();
        let private_environment = Arc::new(read_interface_private_environment(
            &ctx,
            &declared_private_environment_names,
        )?);
        let missing = config
            .processes
            .iter()
            .filter_map(|process| {
                let names = process
                    .required_env
                    .iter()
                    .filter(|name| {
                        !std::env::var_os(name).is_some_and(|value| !value.is_empty())
                            && !private_environment
                                .get(*name)
                                .is_some_and(|value| !value.is_empty())
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                (!names.is_empty()).then(|| (process.bin.as_str(), names))
            })
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            let failures = missing
                .into_iter()
                .map(|(bin, names)| format!("'{bin}': {}", names.join(", ")))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "interface '{}' life is missing required environment names for process {}",
                ctx.interface.as_str(),
                failures
            ));
        }
        let mut processes = tokio::task::JoinSet::new();
        let mut readiness = tokio::task::JoinSet::new();
        let process_cancel = ctx.cancel.child_token();
        for command in config.processes {
            let mut process_ctx = ctx.clone();
            process_ctx.cancel = process_cancel.clone();
            let (process_ctx, receiver) = process_ctx.with_readiness_channel();
            readiness.spawn(async move {
                receiver
                    .await
                    .map_err(|_| "life process readiness channel closed".to_string())?
            });
            let ownership = ownership.clone();
            let environment = private_environment.clone();
            processes.spawn(async move {
                let _ownership = ownership;
                spawn_process_command_life(process_ctx, command, environment).await
            });
        }
        if processes.is_empty() {
            ctx.cancel.cancelled().await;
            return Ok(());
        }
        let life_result = async {
          while !readiness.is_empty() {
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    return Ok(());
                }
                ready = readiness.join_next() => {
                    let ready = ready
                        .ok_or_else(|| "life process readiness set ended early".to_string())?
                        .map_err(|error| format!("life process readiness task failed: {error}"))?;
                    if let Err(readiness_error) = ready {
                        return Err(readiness_error);
                    }
                }
                result = processes.join_next() => {
                    let result = result
                        .ok_or_else(|| "life process set ended before readiness".to_string())?;
                    return Err(spawned_life_task_error(result).unwrap_or_else(|| {
                        "life process exited before interface readiness".to_string()
                    }));
                }
            }
          }
          ctx.signal_ready().await?;
          tokio::select! {
              _ = ctx.cancel.cancelled() => Ok(()),
              result = processes.join_next() => {
                  let result = result.ok_or_else(|| "life process set ended without a process".to_string())?;
                  match spawned_life_task_error(result) {
                      Some(error) => Err(error),
                      None => Ok(()),
                  }
              },
          }
        }.await;
        process_cancel.cancel();
        let mut errors = Vec::new();
        if let Err(error) = life_result {
            errors.push(error);
        }
        while let Some(result) = processes.join_next().await {
            if let Some(error) = spawned_life_task_error(result) {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }) as InterfaceLifeFuture
}

fn spawned_life_task_error(
    result: Result<Result<(), String>, tokio::task::JoinError>,
) -> Option<String> {
    match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(error) => Some(format!("life process task failed: {error}")),
    }
}

fn spawn_process_command_life(
    ctx: InterfaceLiftContext,
    command_config: SpawnLifeCommand,
    private_environment: Arc<BTreeMap<String, String>>,
) -> InterfaceLifeFuture {
    Box::pin(async move {
        let var_dir = ctx.interface_var_dir();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(&var_dir)
                .map_err(|e| format!("var dir '{}' cannot be created: {e}", var_dir.display()))?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(&var_dir)
            .map_err(|e| format!("var dir '{}' cannot be created: {e}", var_dir.display()))?;
        let var_dir = std::fs::canonicalize(&var_dir).map_err(|error| {
            format!(
                "var dir '{}' cannot be canonicalized: {error}",
                var_dir.display()
            )
        })?;
        materialize_private_var_directories(&var_dir, &command_config.private_var_directories)?;
        let life_process_install_root = ctx.life_process_install_root()?;
        let (declared_program, executable_base) = resolve_executable_base(
            &command_config.bin,
            &ctx.interface_registry_dir(),
            &life_process_install_root,
        )?;
        let registry_install_root = ctx.registry_install_root()?;
        let mut environment = command_config
            .inherit_env
            .iter()
            .filter(|name| name.as_str() == "PATH")
            .filter_map(|name| std::env::var_os(name).map(|value| (name.clone(), value)))
            .collect::<BTreeMap<_, _>>();
        for name in interface_direct_private_env_names(&command_config.inherit_env) {
            match (std::env::var_os(&name), private_environment.get(&name)) {
                (Some(ambient), Some(private)) => {
                    if ambient != std::ffi::OsString::from(private) {
                        return Err(format!(
                            "interface '{}' environment name '{}' differs between ambient and private sources",
                            ctx.interface.as_str(),
                            name
                        ));
                    }
                    environment.insert(name, ambient);
                }
                (Some(ambient), None) => {
                    environment.insert(name, ambient);
                }
                (None, Some(private)) => {
                    environment.insert(name, std::ffi::OsString::from(private));
                }
                (None, None) => {}
            }
        }
        for (name, value) in interface_config_env(
            &ctx,
            &command_config.inherit_env_from_interface,
            &private_environment,
        )? {
            if command_config.env.contains_key(&name) {
                return Err(format!(
                    "life process env '{}' is both literal and selected from interface config",
                    name
                ));
            }
            environment.insert(name, value);
        }
        for (name, value) in &command_config.env {
            environment.insert(name.clone(), std::ffi::OsString::from(value));
        }
        set_life_host_environment(&mut environment, &var_dir);
        let mut readable = vec![ctx.interface_registry_dir()];
        let mut arguments = Vec::with_capacity(command_config.args.len());
        for argument in &command_config.args {
            match argument {
                SpawnLifeArgument::Literal(value) if value.starts_with(OWNER_EXECUTABLE_PREFIX) => {
                    let registry = ctx.interface_registry_dir();
                    let registry = registry
                        .parent()
                        .and_then(Path::parent)
                        .ok_or("interface life has no registry root")?;
                    let file =
                        resolve_life_file(value, registry, &ctx.interface, &registry_install_root)?;
                    readable.push(file.clone());
                    arguments.push(file.into_os_string());
                }
                SpawnLifeArgument::Literal(value) => {
                    arguments.push(std::ffi::OsString::from(value));
                }
                SpawnLifeArgument::RepositoryFile(argument) => {
                    let registry = ctx.interface_registry_dir();
                    let registry = registry
                        .parent()
                        .and_then(Path::parent)
                        .ok_or("interface life has no registry root")?;
                    let path = resolve_life_file(
                        &argument.repository_file,
                        registry,
                        &ctx.interface,
                        &registry_install_root,
                    )?;
                    readable.push(path.clone());
                    arguments.push(path.into_os_string());
                }
            }
        }
        for required_file in &command_config.required_files {
            let registry = ctx.interface_registry_dir();
            let registry = registry
                .parent()
                .and_then(Path::parent)
                .ok_or("interface life has no registry root")?;
            readable.push(resolve_life_file(
                required_file,
                registry,
                &ctx.interface,
                &registry_install_root,
            )?);
        }
        let movements_path = command_config
            .read_handles
            .contains(&InterfaceProcessReadHandle::Ledger)
            .then(|| resolve_process_read_handle("ledger", &ctx.movements_path, true))
            .transpose()?;
        let movement_metadata_dir = command_config
            .read_handles
            .contains(&InterfaceProcessReadHandle::Ledger)
            .then(|| {
                let ledger_root = ctx.movements_path.parent().ok_or_else(|| {
                    format!(
                        "life process ledger path '{}' has no parent directory",
                        ctx.movements_path.display()
                    )
                })?;
                resolve_process_read_handle("ledger_metadata", &ledger_root.join("metadata"), false)
            })
            .transpose()?;
        let blobs_dir = command_config
            .read_handles
            .contains(&InterfaceProcessReadHandle::Blobs)
            .then(|| resolve_process_read_handle("blobs", &ctx.blobs_dir, false))
            .transpose()?;
        let ledger_index_path = command_config
            .read_handles
            .contains(&InterfaceProcessReadHandle::LedgerIndex)
            .then(|| resolve_process_read_handle("ledger_index", &ctx.ledger_index_path, true))
            .transpose()?;
        readable.extend(blobs_dir.iter().cloned());
        readable.extend(movement_metadata_dir.iter().cloned());
        let mut readable_exact = movements_path.iter().cloned().collect::<Vec<_>>();
        if let Some(path) = &ledger_index_path {
            readable_exact.push(path.clone());
            readable_exact.push(path_with_suffix(path, "-wal"));
            readable_exact.push(path_with_suffix(path, "-shm"));
        }
        let resolved_interface_descriptor = ctx.resolved_interface_descriptor()?;
        let additional = octopus_interfaces::additional_host_root_paths(&resolved_interface_descriptor, &ctx.root_slots)?;
        let (authority, authority_roots) = octopus_interfaces::expand_host_root_authority(&command_config.authority, &ctx.root_slots, &additional)?;
        let resolved = authority.resolve(
            &executable_base,
            &authority_roots,
            &environment,
            readable,
            [var_dir.clone()],
            std::iter::empty(),
            readable_exact,
        )?;
        let host_io_limits = command_config.io_limits;
        host_io_limits.validate()?;
        let interface_host_slots = resolved_interface_descriptor
            .interface_driver_config
            .as_ref()
            .into_iter()
            .flat_map(|config| config.host_slots.keys());
        let mut exposed_root_slots = resolved.root_slots;
        exposed_root_slots.retain(|slot, _| ctx.root_slots.contains_key(slot));
        for slot in command_config
            .coordinate_root_slots
            .iter()
            .chain(interface_host_slots)
        {
            let root = ctx.root_slots.get(slot).ok_or_else(|| {
                format!(
                    "life process requests unbound coordinate root slot '{}'",
                    slot
                )
            })?;
            if !root.is_absolute() {
                return Err(format!(
                    "life process coordinate root slot '{}' is not absolute: {}",
                    slot,
                    root.display()
                ));
            }
            exposed_root_slots.insert(slot.clone(), root.clone());
        }
        let root_slots_json = serde_json::to_string(&exposed_root_slots)
            .map_err(|error| format!("life root slots failed to encode: {error}"))?;
        let interface_descriptor_json = serde_json::to_string(&resolved_interface_descriptor)
            .map_err(|error| format!("resolved interface descriptor failed to encode: {error}"))?;
        let host_io_limits_json = serde_json::to_string(&host_io_limits)
            .map_err(|error| format!("interface host I/O limits failed to encode: {error}"))?;
        let profile_root = life_profile_root(ctx.var_dir.parent().ok_or("interface life has no private root")?)?;
        let prepared = resolved.authority.prepare(
            &declared_program,
            &arguments,
            &executable_base,
            environment.get("PATH").map(std::ffi::OsString::as_os_str),
            &profile_root,
        )?;
        let executable = prepared.executable.clone();
        if (command_config.bin.starts_with(OWNER_EXECUTABLE_PREFIX)
            || command_config.bin.starts_with(INTERFACE_EXECUTABLE_PREFIX))
            && !executable.starts_with(&executable_base)
        {
            return Err("interface life executable escapes its exact owner before spawn".into());
        }
        let mut command = tokio::process::Command::new(&prepared.program);
        let child_ownership = crate::InterfaceLifeOwnership::try_acquire_life(
            ctx.var_dir.parent().ok_or("interface life has no private root")?, &ctx.interface,
        )?.ok_or("interface process lost its lifetime ownership before spawn")?;
        #[cfg(unix)]
        {
            command.process_group(0);
            child_ownership.inherit_into(&mut command);
        }
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
            .env("OCTOPUS_VAR_DIR", &var_dir)
            .env("OCTOPUS_ROOT_SLOTS_JSON", root_slots_json)
            .env("TMPDIR", &var_dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(movements_path) = movements_path {
            command.env("OCTOPUS_MOVEMENTS_JSONL", movements_path);
        }
        if let Some(blobs_dir) = blobs_dir {
            command.env("OCTOPUS_BLOBS_DIR", blobs_dir);
        }
        if let Some(ledger_index_path) = ledger_index_path {
            command.env("OCTOPUS_LEDGER_INDEX_SQLITE", ledger_index_path);
        }
        let started_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("life process start time is before the Unix epoch: {error}"))?
            .as_nanos();
        let mut child = LifeProcess::spawn(&mut command)
            .await
            .map_err(|e| format!("life spawn '{}' failed: {e}", executable.display()))?;
        let child_pid = child.id();
        let capture_sequence = STDERR_CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let stderr_path = var_dir.join(format!(
            "interface-life.stderr.{started_at}.{child_pid}.{capture_sequence}.log"
        ));
        let mut stderr_task = None;
        let life_result = async {
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "life stderr unavailable".to_string())?;
        let interface = ctx.interface.clone();
        let maximum_bytes = host_io_limits.max_stderr_bytes;
        let stderr_capture_path = stderr_path.clone();
        let (stderr_failure_tx, mut stderr_failure_rx) = tokio::sync::oneshot::channel();
        stderr_task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
            let result = drain_bounded_stderr_to_private_file(
                stderr,
                maximum_bytes,
                interface,
                "interface_life",
                Some(stderr_capture_path),
            )
            .await;
            if let Err(error) = &result {
                let _ = stderr_failure_tx.send(error.clone());
            }
            result
        })));
        let mut stderr_open = true;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "life stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "life stdout unavailable".to_string())?;
        let mut lines = BoundedJsonLineReader::new(stdout, host_io_limits.max_control_frame_bytes)?;
        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    break Ok(());
                }
                status = child.wait() => {
                    break exit_result(status);
                }
                stderr = &mut stderr_failure_rx, if stderr_open => {
                    match stderr {
                        Ok(error) => break Err(error),
                        Err(_) => stderr_open = false,
                    }
                }
                line = lines.next_line() => {
                    let line = match line {
                        Ok(Some(line)) => line,
                        Ok(None) => break tokio::select! {
                            _ = ctx.cancel.cancelled() => Ok(()),
                            result = child.protocol_closed() => result,
                        },
                        Err(error) => break Err(format!("life stdout read failed: {error}")),
                    };
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let output: SpawnedLifeOutput = match serde_json::from_str(trimmed) {
                        Ok(output) => output,
                        Err(error) => break Err(format!("life stdout is not a protocol emission: {error}")),
                    };
                    if let Err(error) = handle_spawned_life_output(
                        &ctx,
                        &var_dir,
                        host_io_limits,
                        &mut stdin,
                        output,
                    )
                    .await
                    {
                        break Err(error);
                    }
                }
            }
        }
        }.await;
        let termination_result = child.terminate().await;
        let mut errors = Vec::new();
        if let Err(error) = life_result {
            errors.push(error);
        }
        if let Err(error) = termination_result {
            errors.push(error);
        }
        if let Some(stderr_task) = stderr_task {
            if let Err(error) = finish_stderr(stderr_task).await { errors.push(error); }
        }
        if !errors.is_empty() && stderr_path.is_file() {
            errors.push(format!(
                "private stderr captured at '{}'",
                stderr_path.display()
            ));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }) as InterfaceLifeFuture
}

fn resolve_executable_base(
    declared_program: &str,
    owner_root: &Path,
    installation_root: &Path,
) -> Result<(String, PathBuf), String> {
    if declared_program.starts_with(INTERFACE_EXECUTABLE_PREFIX) {
        let interface_root = owner_root.parent().ok_or_else(|| {
            format!(
                "interface executable owner '{}' has no interface registry root",
                owner_root.display()
            )
        })?;
        let registry_root = interface_root.parent().ok_or_else(|| {
            format!(
                "interface registry root '{}' has no registry parent",
                interface_root.display()
            )
        })?;
        let dependency = resolve_imported_executable_dependency(registry_root, declared_program, None)?
            .ok_or_else(|| {
            format!("imported executable declaration '{declared_program}' is invalid")
        })?;
        return Ok((
            dependency
                .executable_relative
                .to_string_lossy()
                .into_owned(),
            dependency.source_owner,
        ));
    }
    if !declared_program.starts_with(OWNER_EXECUTABLE_PREFIX) {
        return Ok((
            declared_program.to_string(),
            installation_root.to_path_buf(),
        ));
    }
    let registry = owner_root
        .parent()
        .and_then(Path::parent)
        .ok_or("interface executable owner has no registry root")?;
    let interface = InterfaceId::new(
        owner_root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("interface executable owner has no valid folder name")?,
    );
    let executable = resolve_life_file(declared_program, registry, &interface, installation_root)?;
    let owner_root = std::fs::canonicalize(owner_root).map_err(|error| {
        format!(
            "interface executable owner '{}' cannot be canonicalized: {error}",
            owner_root.display()
        )
    })?;
    Ok((
        executable
            .to_str()
            .ok_or("interface executable path is not UTF-8")?
            .to_string(),
        owner_root,
    ))
}

fn spawned_private_output_roots(ctx: &InterfaceLiftContext, var_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![var_dir.to_path_buf()];
    if let Ok(hand_in_root) = ctx.root_slot("interface_hand_in_root") {
        roots.push(hand_in_root.join(ctx.interface.as_str()));
    }
    roots
}

async fn handle_spawned_life_output(
    ctx: &InterfaceLiftContext,
    var_dir: &Path,
    host_io_limits: InterfaceHostIoLimits,
    stdin: &mut tokio::process::ChildStdin,
    output: SpawnedLifeOutput,
) -> Result<(), String> {
    match output {
        SpawnedLifeOutput::Ready => ctx.signal_ready().await?,
        SpawnedLifeOutput::Arrival {
            request_id,
            artifact_manifest_path,
            evidence,
        } => {
            let artifact = capture_spawned_artifact(
                &artifact_manifest_path,
                &spawned_private_output_roots(ctx, var_dir),
                host_io_limits,
            )?;
            let evidence = evidence
                .into_iter()
                .map(|item| crate::InterfaceActivationEvidence {
                    interface_id: InterfaceId::new(item.interface_id),
                    artifact_id: ArtifactId::new(item.artifact_id),
                    label: item.label,
                    bytes: item.bytes,
                })
                .collect();
            let verdict_rx = if request_id.is_some() {
                let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
                ctx.arrivals
                    .send(InterfaceActivationArrival {
                        artifact,
                        interface: ctx.interface.clone(),
                        evidence,
                        verdict_tx: Some(verdict_tx),
                        retained: None,
                    })
                    .await
                    .map_err(|_| "arrival intake closed".to_string())?;
                Some(verdict_rx)
            } else {
                ctx.arrivals
                    .send(InterfaceActivationArrival {
                        artifact,
                        interface: ctx.interface.clone(),
                        evidence,
                        verdict_tx: None,
                        retained: None,
                    })
                    .await
                    .map_err(|_| "arrival intake closed".to_string())?;
                None
            };
            if let (Some(request_id), Some(verdict_rx)) = (request_id, verdict_rx) {
                let verdict = tokio::select! {
                    _ = ctx.cancel.cancelled() => return Ok(()),
                    verdict = verdict_rx => verdict
                        .map_err(|_| "arrival verdict channel dropped".to_string())?,
                };
                write_spawned_life_input(
                    stdin,
                    request_id,
                    verdict,
                    host_io_limits.max_control_frame_bytes,
                )
                .await?;
            }
        }
        SpawnedLifeOutput::RetainedArrival {
            request_id,
            delivery_id,
            artifact_manifest_path,
            evidence,
        } => {
            if request_id == 0 {
                return Err("life retained-arrival request_id must be positive".to_string());
            }
            validate_retained_delivery_id(&delivery_id)?;
            let artifact = capture_spawned_artifact(
                &artifact_manifest_path,
                &spawned_private_output_roots(ctx, var_dir),
                host_io_limits,
            )?;
            let evidence = evidence
                .into_iter()
                .map(|item| InterfaceActivationEvidence {
                    interface_id: InterfaceId::new(item.interface_id),
                    artifact_id: ArtifactId::new(item.artifact_id),
                    label: item.label,
                    bytes: item.bytes,
                })
                .collect();
            let (reply, result) = tokio::sync::oneshot::channel();
            ctx.arrivals
                .send(InterfaceActivationArrival {
                    artifact,
                    interface: ctx.interface.clone(),
                    evidence,
                    verdict_tx: None,
                    retained: Some(crate::RetainedInterfaceArrival {
                        id: delivery_id,
                        reply,
                    }),
                })
                .await
                .map_err(|_| "arrival intake closed".to_string())?;
            let delivery = tokio::select! {
                _ = ctx.cancel.cancelled() => return Ok(()),
                result = result => match result {
                    Ok(delivery) => delivery,
                    Err(_) => Err("retained arrival reply channel dropped".to_string()),
                },
            };
            write_spawned_life_message(
                stdin,
                &SpawnedLifeInput::RetainedDelivery { request_id, delivery },
                host_io_limits.max_control_frame_bytes,
            )
            .await?;
        }
        SpawnedLifeOutput::SystemEmission {
            request_id,
            artifact,
            source_movement_seq,
            to_interface,
        } => {
            if request_id == 0 {
                return Err("life system-emission request_id must be positive".to_string());
            }
            let to_interface = spawned_protocol_identity("to_interface", to_interface)?;
            let artifact = match artifact {
                SpawnedLifeEmissionArtifact::Materialized {
                    artifact_manifest_path,
                } => {
                    crate::SystemInterfaceEmissionArtifact::Materialized(capture_spawned_artifact(
                        &artifact_manifest_path,
                        &spawned_private_output_roots(ctx, var_dir),
                        host_io_limits,
                    )?)
                }
                SpawnedLifeEmissionArtifact::Reference { artifact } => {
                    if artifact.id.as_str().trim().is_empty() {
                        return Err(
                            "life system-emission artifact reference has an empty id".to_string()
                        );
                    }
                    crate::SystemInterfaceEmissionArtifact::Reference(artifact)
                }
            };
            let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
            ctx.emissions
                .send(SystemInterfaceEmission::host_attested(
                    artifact,
                    ctx.interface.clone(),
                    source_movement_seq,
                    InterfaceId::new(to_interface),
                    verdict_tx,
                ))
                .await
                .map_err(|_| "system emission intake closed".to_string())?;
            let verdict = tokio::select! {
                _ = ctx.cancel.cancelled() => return Ok(()),
                verdict = verdict_rx => verdict
                    .map_err(|_| "system emission verdict channel dropped".to_string())?,
            };
            write_spawned_life_input(
                stdin,
                request_id,
                verdict,
                host_io_limits.max_control_frame_bytes,
            )
            .await?;
        }
    }
    Ok(())
}

fn spawned_protocol_identity(field: &str, value: String) -> Result<String, String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!(
            "life system-emission {field} must be a non-empty canonical identity"
        ));
    }
    Ok(value)
}

fn resolve_process_read_handle(name: &str, path: &Path, exact: bool) -> Result<PathBuf, String> {
    let resolved = if exact {
        resolve_process_exact_path(path)
    } else {
        std::fs::canonicalize(path).map_err(|error| {
            format!(
                "life process {name} read handle '{}' cannot be canonicalized: {error}",
                path.display()
            )
        })
    }?;
    Ok(resolved)
}

fn life_profile_root(private_root: &Path) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(private_root)
        .map_err(|error| format!("interface life private root '{}' cannot be resolved: {error}", private_root.display()))?;
    let profiles = root.join(".life-profiles");
    match std::fs::create_dir(&profiles) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("interface life profile root '{}' cannot be created: {error}", profiles.display())),
    }
    let metadata = std::fs::symlink_metadata(&profiles).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("interface life profile root must be a regular directory".into());
    }
    Ok(profiles)
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

pub fn capture_spawned_artifact(
    path: &Path,
    output_roots: &[PathBuf],
    limits: InterfaceHostIoLimits,
) -> Result<InterfaceArtifactFile, String> {
    validate_host_io_limits(limits)?;
    capture_spawned_artifact_with(path, output_roots, limits, read_regular_file_beneath)
}

fn capture_spawned_artifact_with<F>(
    path: &Path,
    output_roots: &[PathBuf],
    limits: InterfaceHostIoLimits,
    mut read: F,
) -> Result<InterfaceArtifactFile, String>
where
    F: FnMut(&Path, &[PathBuf], u64) -> Result<Vec<u8>, String>,
{
    let manifest_bytes = read(path, output_roots, limits.max_manifest_bytes)?;
    let manifest: InterfaceArtifactManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| {
            format!(
                "spawned interface output manifest '{}' is malformed: {error}",
                path.display()
            )
        })?;
    if manifest.label.trim().is_empty() || manifest.payload_path.as_os_str().is_empty() {
        return Err(format!(
            "spawned interface output manifest '{}' has an empty label or payload path",
            path.display()
        ));
    }
    if !manifest.payload_path.is_absolute() {
        return Err(format!(
            "spawned interface output manifest '{}' has a relative payload path '{}'",
            path.display(),
            manifest.payload_path.display()
        ));
    }
    let payload_bytes = read(
        &manifest.payload_path,
        output_roots,
        limits.max_payload_bytes,
    )?;
    InterfaceArtifactFile::from_captured_snapshot(path.to_path_buf(), manifest_bytes, payload_bytes)
        .map_err(|error| {
            format!(
                "spawned interface output manifest '{}' is invalid: {error}",
                path.display()
            )
        })
}

#[cfg(unix)]
fn read_regular_file_beneath(
    path: &Path,
    output_roots: &[PathBuf],
    maximum_bytes: u64,
) -> Result<Vec<u8>, String> {
    if !path.is_absolute() {
        return Err(format!(
            "spawned interface output '{}' is not absolute",
            path.display()
        ));
    }
    let leaf = path.file_name().ok_or_else(|| {
        format!(
            "spawned interface output '{}' has no regular-file leaf",
            path.display()
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        format!(
            "spawned interface output '{}' has no parent directory",
            path.display()
        )
    })?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|error| {
        format!(
            "spawned interface output parent '{}' cannot be canonicalized: {error}",
            parent.display()
        )
    })?;
    let normalized_path = canonical_parent.join(leaf);
    let mut roots = output_roots
        .iter()
        .filter_map(|root| {
            let root = std::fs::canonicalize(root).ok()?;
            let relative = normalized_path.strip_prefix(&root).ok()?.to_path_buf();
            Some((root, relative))
        })
        .collect::<Vec<_>>();
    roots.sort_by_key(|(root, _)| std::cmp::Reverse(root.components().count()));
    let Some((root, relative)) = roots.into_iter().next() else {
        return Err(format!(
            "spawned interface output '{}' is outside private output roots {:?}",
            path.display(),
            output_roots
        ));
    };
    let components = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(name) => Ok(name),
            _ => Err(format!(
                "spawned interface output '{}' has a non-normal relative component",
                path.display()
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.is_empty() {
        return Err(format!(
            "spawned interface output '{}' names its output root, not a regular file",
            path.display()
        ));
    }
    let root_bytes = CString::new(root.as_os_str().as_bytes()).map_err(|_| {
        format!(
            "spawned interface output root '{}' contains NUL",
            root.display()
        )
    })?;
    let root_fd = unsafe {
        libc::open(
            root_bytes.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(format!(
            "spawned interface output root '{}' cannot be opened without following links: {}",
            root.display(),
            std::io::Error::last_os_error()
        ));
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(root_fd) };
    for (index, component) in components.iter().enumerate() {
        let component = CString::new(component.as_bytes())
            .map_err(|_| format!("spawned interface output '{}' contains NUL", path.display()))?;
        let is_leaf = index + 1 == components.len();
        let flags = if is_leaf {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
        };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 {
            return Err(format!(
                "spawned interface output '{}' cannot be opened beneath '{}': {}",
                path.display(),
                root.display(),
                std::io::Error::last_os_error()
            ));
        }
        let opened = unsafe { OwnedFd::from_raw_fd(fd) };
        if !is_leaf {
            directory = opened;
            continue;
        }
        let mut file = std::fs::File::from(opened);
        let metadata = file.metadata().map_err(|error| {
            format!(
                "spawned interface output '{}' descriptor cannot be inspected: {error}",
                path.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "spawned interface output '{}' is not a regular file",
                path.display()
            ));
        }
        if metadata.len() > maximum_bytes {
            return Err(format!(
                "spawned interface output '{}' is {} bytes, above its declared ceiling {}",
                path.display(),
                metadata.len(),
                maximum_bytes
            ));
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take(maximum_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                format!(
                    "spawned interface output '{}' descriptor cannot be read: {error}",
                    path.display()
                )
            })?;
        if bytes.len() as u64 > maximum_bytes {
            return Err(format!(
                "spawned interface output '{}' grew above its declared ceiling {} while being read",
                path.display(),
                maximum_bytes
            ));
        }
        return Ok(bytes);
    }
    Err(format!(
        "spawned interface output '{}' could not be opened",
        path.display()
    ))
}

#[cfg(not(unix))]
fn read_regular_file_beneath(
    path: &Path,
    _output_roots: &[PathBuf],
    _maximum_bytes: u64,
) -> Result<Vec<u8>, String> {
    Err(format!(
        "spawned interface output '{}' cannot be captured safely on this platform",
        path.display()
    ))
}

fn validate_host_io_limits(limits: InterfaceHostIoLimits) -> Result<(), String> {
    limits.validate()
}

async fn write_spawned_life_input(
    stdin: &mut tokio::process::ChildStdin,
    request_id: u64,
    verdict: DeliveryVerdict,
    maximum_frame_bytes: u64,
) -> Result<(), String> {
    write_spawned_life_message(
        stdin,
        &SpawnedLifeInput::DeliveryVerdict {
            request_id,
            verdict,
        },
        maximum_frame_bytes,
    )
    .await
}

async fn write_spawned_life_message(
    stdin: &mut tokio::process::ChildStdin,
    input: &SpawnedLifeInput,
    maximum_frame_bytes: u64,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let bytes =
        serialize_bounded_json_line(input, maximum_frame_bytes, "spawned-life input frame")?;
    stdin
        .write_all(&bytes)
        .await
        .map_err(|error| format!("life verdict write failed: {error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("life verdict flush failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_ledger::{ContentHash, InMemoryBlobs};

    #[cfg(unix)]
    #[tokio::test]
    async fn life_cleanup_releases_descendant_stderr_after_cancel_or_leader_exit() {
        use tokio::io::AsyncReadExt;
        for (leader_exits, drop_owner) in [
            (false, false), (true, false),
            #[cfg(target_os = "macos")]
            (true, true),
        ] {
            let root = tempfile::tempdir().unwrap();
            let interface = InterfaceId::new("world_cleanup_test");
            let ownership = crate::InterfaceLifeOwnership::try_acquire_life(root.path(), &interface).unwrap().unwrap();
            let mut command = tokio::process::Command::new("/bin/sh");
            ownership.inherit_into(&mut command);
            command
                .args([
                    "-c",
                    if leader_exits {
                        "/bin/sleep 60 & printf ready; exit 0"
                    } else {
                        "/bin/sleep 60 & printf ready; wait"
                    },
                ])
                .process_group(0)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            let mut child = LifeProcess::spawn(&mut command).await.unwrap();
            let mut ready = [0; 5];
            child
                .stdout
                .take()
                .unwrap()
                .read_exact(&mut ready)
                .await
                .unwrap();
            assert_eq!(&ready, b"ready");
            drop(ownership);
            if leader_exits {
                assert!(child.wait().await.unwrap().success());
            }
            assert!(crate::InterfaceLifeOwnership::try_acquire(root.path(), &interface).unwrap().is_none());
            let mut stderr = child.stderr.take().unwrap();
            if drop_owner { drop(child); } else {
                child.terminate().await.unwrap();
                child.terminate().await.unwrap();
            }
            let mut bytes = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                stderr.read_to_end(&mut bytes),
            )
            .await
            .expect("descendants must not retain the stopped interface's stderr")
            .unwrap();
            assert!(crate::InterfaceLifeOwnership::try_acquire(root.path(), &interface).unwrap().is_some());
        }
    }

    fn test_host_io_limits() -> InterfaceHostIoLimits {
        InterfaceHostIoLimits {
            max_control_frame_bytes: 1_048_576,
            max_stderr_bytes: 262_144,
            max_manifest_bytes: 1_048_576,
            max_payload_bytes: 268_435_456,
        }
    }

    fn ctx(root: &Path, interface: &str) -> InterfaceLiftContext {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let (emissions_tx, _emissions_rx) = tokio::sync::mpsc::channel(4);
        InterfaceLiftContext {
            interface: InterfaceId::new(interface),
            interface_dir: root.join("registry").join("interface").join(interface),
            descriptor_snapshot: None,
            var_dir: root.join("home").join("interfaces").join(interface),
            movements_path: root.join("durable").join("movements.jsonl"),
            blobs_dir: root.join("durable").join("blobs"),
            ledger_index_path: root.join("durable").join("ledger-index.sqlite"),
            root_slots: Default::default(),
            host_io_limits: None,
            arrivals: tx,
            emissions: emissions_tx,
            cancel: CancellationToken::new(),
            readiness: InterfaceLifeReadiness::default(),
        }
    }

    #[test]
    fn life_host_var_is_available_only_through_declared_subpaths() {
        let sidecar = serde_json::json!({
            "processes": [{
                "bin": "/usr/bin/true",
                "doctor": {
                    "args": ["--doctor"],
                    "timeout_ms": 5000
                },
                "private_var_directories": ["runtime/bin"],
                "io_limits": {
                    "max_control_frame_bytes": 1048576,
                    "max_stderr_bytes": 262144,
                    "max_manifest_bytes": 1048576,
                    "max_payload_bytes": 268435456
                },
                "authority": {
                    "read_root_slots": [],
                    "write_root_slots": [],
                    "executable_root_slots": [],
                    "read_paths": [],
                    "write_paths": [],
                    "executable_paths": ["/usr/bin/true"],
                    "executable_commands": [],
                    "read_env_paths": [],
                    "write_env_paths": [],
                    "executable_env_paths": [],
                    "read_env_subpaths": [{
                        "env": "OCTOPUS_VAR_DIR",
                        "relative_path": "runtime"
                    }],
                    "executable_env_subpaths": [{
                        "env": "OCTOPUS_VAR_DIR",
                        "relative_path": "runtime/bin"
                    }],
                    "allow_network": false,
                    "allow_local_ipc": false
                },
                "read_handles": []
            }]
        });
        let bytes = serde_json::to_vec(&sidecar).unwrap();
        let config = parse_spawn_life_config(Path::new("life.json"), &bytes).unwrap();
        assert_eq!(
            config.processes[0].doctor.as_ref().unwrap().args.as_slice(),
            ["--doctor"]
        );
        let mut invalid = sidecar.clone();
        invalid["processes"][0]["doctor"]["timeout_ms"] = serde_json::json!(0);
        let error = parse_spawn_life_config(
            Path::new("life.json"),
            &serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("doctor timeout_ms"));
        let root = tempfile::tempdir().unwrap();
        let var_dir = root.path().join("interface-var");
        std::fs::create_dir(&var_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&var_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        materialize_private_var_directories(&var_dir, &config.processes[0].private_var_directories)
            .unwrap();
        let mut environment = BTreeMap::new();
        set_life_host_environment(&mut environment, &var_dir);
        config.processes[0]
            .authority
            .resolve(
                root.path(),
                &BTreeMap::new(),
                &environment,
                std::iter::empty::<PathBuf>(),
                [var_dir],
                std::iter::empty::<PathBuf>(),
                std::iter::empty::<PathBuf>(),
            )
            .unwrap();

        let mut full_root = sidecar.clone();
        full_root["processes"][0]["authority"]["read_env_subpaths"] = serde_json::json!([]);
        full_root["processes"][0]["authority"]["executable_env_subpaths"] = serde_json::json!([]);
        full_root["processes"][0]["authority"]["executable_env_paths"] =
            serde_json::json!(["OCTOPUS_VAR_DIR"]);
        let error = parse_spawn_life_config(
            Path::new("life.json"),
            &serde_json::to_vec(&full_root).unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("reserved env 'OCTOPUS_VAR_DIR'"));

        let mut foreign_reserved = sidecar;
        foreign_reserved["processes"][0]["authority"]["read_env_subpaths"][0]["env"] =
            serde_json::json!("OCTOPUS_INTERFACE_DIR");
        let error = parse_spawn_life_config(
            Path::new("life.json"),
            &serde_json::to_vec(&foreign_reserved).unwrap(),
        )
        .unwrap_err();
        assert!(error.contains("reserved env 'OCTOPUS_INTERFACE_DIR'"));
    }

    #[cfg(unix)]
    fn write_manifest(path: &Path, label: &str, payload_path: &Path, payload: &[u8]) {
        std::fs::write(payload_path, payload).unwrap();
        std::fs::write(
            path,
            serde_json::to_vec(&InterfaceArtifactManifest {
                label: label.to_string(),
                payload_path: payload_path.to_path_buf(),
                payload_content_hash: Some(ContentHash::of_bytes(payload).to_hex()),
            })
            .unwrap(),
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn output_limits() -> InterfaceHostIoLimits {
        InterfaceHostIoLimits {
            max_control_frame_bytes: 64 * 1024,
            max_stderr_bytes: 64 * 1024,
            max_manifest_bytes: 64 * 1024,
            max_payload_bytes: 1024 * 1024,
        }
    }

    #[tokio::test]
    async fn bounded_json_line_reader_refuses_oversized_unterminated_frame() {
        use tokio::io::AsyncWriteExt;

        let (mut writer, reader) = tokio::io::duplex(64);
        writer.write_all(&[b'x'; 33]).await.unwrap();
        let mut lines = BoundedJsonLineReader::new(reader, 32).unwrap();
        let error = lines.next_line().await.unwrap_err();
        assert!(error.contains("exceeds declared ceiling of 32 bytes"));
        assert_eq!(lines.next_line().await.unwrap_err(), error);
    }

    #[tokio::test]
    async fn interrupted_protocol_read_preserves_the_partial_frame() {
        use std::future::Future;
        use std::task::Poll;
        use tokio::io::AsyncWriteExt;

        let (mut writer, reader) = tokio::io::duplex(32);
        let mut lines = BoundedJsonLineReader::new(reader, 8).unwrap();
        writer.write_all(b"{\"a\":").await.unwrap();
        {
            let pending = lines.next_line();
            tokio::pin!(pending);
            std::future::poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        writer.write_all(b"1}\n").await.unwrap();
        assert_eq!(
            lines.next_line().await.unwrap().as_deref(),
            Some("{\"a\":1}")
        );
    }

    #[test]
    fn bounded_json_line_writer_counts_the_frame_terminator() {
        let value = serde_json::json!({"a": 1});
        let bytes = serialize_bounded_json_line(&value, 8, "test frame").unwrap();
        assert_eq!(bytes, b"{\"a\":1}\n");
        let error = serialize_bounded_json_line(&value, 7, "test frame").unwrap_err();
        assert!(error.contains("exceeds declared ceiling of 7 bytes"));
    }

    #[test]
    fn interface_owned_executable_resolves_through_the_declared_source_owner() {
        let registry = tempfile::tempdir().unwrap();
        let interface_root = registry.path().join("interface");
        let current_owner = interface_root.join("world_task");
        let source_owner = interface_root.join("world_operator_chat");
        std::fs::create_dir_all(&current_owner).unwrap();
        let executable = source_owner.join("packages/bin/octopus-app-server-activation-life");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"life").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            source_owner.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_wrong_owner",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["operator"],
                "interface_driver_ref": "operator_driver"
            }))
            .unwrap(),
        )
        .unwrap();

        let error = resolve_executable_base(
            "interface://world_operator_chat/packages/bin/octopus-app-server-activation-life",
            &current_owner,
            registry.path(),
        )
        .unwrap_err();
        assert!(error.contains("instead of folder id 'world_operator_chat'"));
        std::fs::write(
            source_owner.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_operator_chat",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["operator"],
                "interface_driver_ref": "operator_driver"
            }))
            .unwrap(),
        )
        .unwrap();

        let (program, base) = resolve_executable_base(
            "interface://world_operator_chat/packages/bin/octopus-app-server-activation-life",
            &current_owner,
            registry.path(),
        )
        .unwrap();

        assert_eq!(program, "packages/bin/octopus-app-server-activation-life");
        assert_eq!(base, std::fs::canonicalize(source_owner).unwrap());
        assert!(
            resolve_executable_base(
                "interface://../world_operator_chat/packages/bin/life",
                &current_owner,
                registry.path(),
            )
            .unwrap_err()
            .contains("interface-owned executable is invalid")
        );
        std::fs::remove_file(&executable).unwrap();
        let error = resolve_executable_base(
            "interface://world_operator_chat/packages/bin/octopus-app-server-activation-life",
            &current_owner,
            registry.path(),
        )
        .unwrap_err();
        assert!(error.contains("imported interface executable"));
    }

    #[tokio::test]
    async fn imported_life_fingerprint_tracks_owner_descriptor_executable_and_build_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry");
        let importer = registry.join("interface/world_importer");
        let source = registry.join("interface/world_source");
        let source_bin = source.join("packages/imported-life/bin/life");
        let receipt = source
            .join("packages/imported-life")
            .join(IMPORTED_EXECUTABLE_BUILD_RECEIPT_FILE);
        std::fs::create_dir_all(&importer).unwrap();
        std::fs::create_dir_all(source_bin.parent().unwrap()).unwrap();
        let write_source = |names: &[&str]| {
            std::fs::write(
                source.join("interface.json"),
                serde_json::to_vec(&serde_json::json!({
                    "id": "world_source",
                    "interface_type": "world",
                    "role": "service",
                    "direction": "bidirectional",
                    "source_names": names,
                    "interface_driver_ref": "source_driver"
                }))
                .unwrap(),
            )
            .unwrap();
        };
        write_source(&["source"]);
        std::fs::write(&source_bin, b"alpha").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&source_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let write_receipt = |digest: &str| {
            let executable_bytes = std::fs::read(&source_bin).unwrap();
            std::fs::write(
                &receipt,
                serde_json::to_vec(&serde_json::json!({
                    "schema_version": 1,
                    "executable": "bin/life",
                    "executable_sha256": sha256_hex(&executable_bytes),
                    "executable_bytes": executable_bytes.len(),
                    "inputs": {"revision": digest}
                }))
                .unwrap(),
            )
            .unwrap();
        };
        std::fs::write(
            importer.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_importer",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["importer"],
                "interface_driver_ref": "importer_driver"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            importer.join("life.json"),
            serde_json::to_vec(&serde_json::json!({
                "processes": [{
                    "bin": "interface://world_source/packages/imported-life/bin/life",
                    "io_limits": test_host_io_limits(),
                    "authority": ProcessAuthoritySpec::default(),
                    "read_handles": []
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        {
            std::fs::write(&source_bin, vec![0x5a; 8 * 1024 * 1024]).unwrap();
            write_receipt("shared-import");
            let descriptor: serde_json::Value = serde_json::from_slice(
                &std::fs::read(importer.join("interface.json")).unwrap(),
            ).unwrap();
            let life = std::fs::read(importer.join("life.json")).unwrap();
            let ids = (0..32).map(|index| InterfaceId::new(format!("world_importer_{index}")))
                .collect::<Vec<_>>();
            for id in &ids {
                let folder = registry.join("interface").join(id.as_str());
                std::fs::create_dir_all(&folder).unwrap();
                let mut own_descriptor = descriptor.clone();
                own_descriptor["id"] = serde_json::json!(id.as_str());
                std::fs::write(folder.join("interface.json"), serde_json::to_vec(&own_descriptor).unwrap()).unwrap();
                std::fs::write(folder.join("life.json"), &life).unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(1100));
            let (arrivals, _) = tokio::sync::mpsc::channel(1);
            let (emissions, _) = tokio::sync::mpsc::channel(1);
            let mut reconciler = crate::InterfaceActivationReconciler::new(
                registry.clone(), dir.path().into(), arrivals, emissions,
                CancellationToken::new(), dir.path().into(), None,
            );
            let mut verification = ImportedExecutableVerification::default();
            let started = std::time::Instant::now();
            let resolved = ids.iter().map(|id| {
                reconciler.resolve_interface_life(id, &mut verification).unwrap().unwrap()
            }).collect::<Vec<_>>();
            eprintln!("cold shared import: {} lives, 8 MiB executable, {:?}", ids.len(), started.elapsed());
            for (id, cached) in ids.iter().zip(&resolved) {
                let uncached = resolve_spawn_life_config(&registry, id).unwrap().unwrap();
                assert_eq!(cached.fingerprint, uncached.fingerprint);
            }
            let modified = std::fs::metadata(&source_bin).unwrap().modified().unwrap();
            std::fs::write(&source_bin, vec![0xa5; 8 * 1024 * 1024]).unwrap();
            std::fs::File::options().write(true).open(&source_bin).unwrap().set_modified(modified).unwrap();
            let error = reconciler.resolve_interface_life(&ids[0], &mut verification).unwrap_err();
            assert!(error.contains("SHA-256 does not match executable"));
            std::fs::write(&source_bin, b"alpha").unwrap();
            std::fs::remove_file(&receipt).unwrap();
        }

        let snapshot = || {
            let resolved =
                resolve_spawn_life_config(&registry, &InterfaceId::new("world_importer"))
                    .unwrap()
                    .unwrap();
            let source = resolved_spawn_life_source_fingerprint(
                &registry,
                &InterfaceId::new("world_importer"),
                &resolved,
                &mut ImportedExecutableVerification::default(),
            )
            .unwrap();
            (resolved.fingerprint, source)
        };
        let first = snapshot();

        write_source(&["source", "changed"]);
        let descriptor_changed = snapshot();
        assert_ne!(first, descriptor_changed);

        std::fs::write(&source_bin, b"beta-longer").unwrap();
        let executable_changed = snapshot();
        assert_ne!(descriptor_changed, executable_changed);

        write_receipt("beta");
        let receipt_added = snapshot();
        assert_ne!(executable_changed, receipt_added);

        write_receipt("gamma");
        let receipt_changed = snapshot();
        assert_ne!(receipt_added, receipt_changed);

        let (arrivals, _) = tokio::sync::mpsc::channel(1);
        let (emissions, _) = tokio::sync::mpsc::channel(1);
        let mut reconciler = crate::InterfaceActivationReconciler::new(
            registry.clone(),
            dir.path().into(),
            arrivals,
            emissions,
            CancellationToken::new(),
            dir.path().into(),
            None,
        );
        let mut verification = ImportedExecutableVerification::default();
        let id = InterfaceId::new("world_importer");
        assert!(
            reconciler
                .resolve_interface_life(&id, &mut verification)
                .unwrap()
                .is_some()
        );
        verification.finish_pass();
        #[cfg(unix)]
        assert_eq!(verification.digests.len(), 1);
        verification.finish_pass();
        #[cfg(unix)]
        assert!(verification.digests.is_empty());
        std::fs::write(&receipt, b"[]").unwrap();
        let error =
            resolve_spawn_life_config(&registry, &InterfaceId::new("world_importer")).unwrap_err();
        assert!(error.contains("build receipt") && error.contains("malformed"));
        let life = importer.join("life.json");
        let mut replacement: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&life).unwrap()).unwrap();
        let replacement_bin = source.join("packages/other/bin/life");
        std::fs::create_dir_all(replacement_bin.parent().unwrap()).unwrap();
        std::fs::copy(&source_bin, &replacement_bin).unwrap();
        replacement["processes"][0]["bin"] =
            serde_json::json!("interface://world_source/packages/other/bin/life");
        std::fs::write(&life, serde_json::to_vec(&replacement).unwrap()).unwrap();
        let repaired = reconciler
            .resolve_interface_life(&id, &mut verification)
            .unwrap()
            .unwrap();
        assert_eq!(
            repaired.config.processes[0].bin,
            "interface://world_source/packages/other/bin/life"
        );
        std::fs::write(&life, b"[]").unwrap();
        assert!(
            reconciler
                .resolve_interface_life(&id, &mut verification)
                .is_err()
        );
        assert!(!reconciler.resolved_lives.contains_key(&id));
        reconciler.durable_root = Some(dir.path().join("ledger"));
        reconciler.root_cancel.cancel();
        assert_eq!(
            reconciler.reconcile_interface_lives().await.unwrap_err(),
            "interface lifecycle reconciliation cancelled"
        );
        assert!(reconciler.lifted.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn imported_executable_receipt_rechecks_unsettled_observations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("program");
        std::fs::write(&path, b"alpha").unwrap();
        let mut verification = ImportedExecutableVerification::default();
        let before = std::fs::metadata(&path).unwrap();
        let first = verification.digest(&path, &before).unwrap();
        assert!(!verification.digests[&path].settled);
        std::fs::write(&path, b"omega").unwrap();
        let after = std::fs::metadata(&path).unwrap();
        verification.digests.get_mut(&path).unwrap().stamp = executable_file_stamp(&after);
        let second = verification.digest(&path, &after).unwrap();
        assert_ne!(first.sha256, second.sha256);
        assert_eq!(second.sha256, sha256_hex(b"omega"));
        assert!(!FileStamp::of(&after).is_settled(SystemTime::UNIX_EPOCH));
        assert!(FileStamp::of(&after).is_settled(SystemTime::now() + std::time::Duration::from_secs(2)));
        assert!(!verification.digests[&path].settled);
    }

    #[test]
    fn imported_executable_receipt_rejects_same_size_binary_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry");
        let source = registry.join("interface/world_source");
        let executable = source.join("packages/imported-life/bin/life");
        let receipt = source
            .join("packages/imported-life")
            .join(IMPORTED_EXECUTABLE_BUILD_RECEIPT_FILE);
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(
            source.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_source",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["source"],
                "interface_driver_ref": "source_driver"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(&executable, b"alpha").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            &receipt,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "executable": "bin/life",
                "executable_sha256": "8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8",
                "executable_bytes": 5
            }))
            .unwrap(),
        )
        .unwrap();
        let mut verification = ImportedExecutableVerification::default();
        let mut resolve = || {
            let result = resolve_imported_executable_dependency(
                &registry,
                "interface://world_source/packages/imported-life/bin/life",
                Some(&mut verification),
            );
            verification.finish_pass();
            result
        };
        for _ in 0..2 {
            assert!(matches!(resolve(), Ok(Some(_))));
        }
        let owner = source.join("interface.json");
        let owner_bytes = std::fs::read(&owner).unwrap();
        std::fs::write(&owner, b"[]").unwrap();
        assert!(resolve().is_err());
        std::fs::write(&owner, owner_bytes).unwrap();
        assert!(matches!(resolve(), Ok(Some(_))));
        let modified = std::fs::metadata(&executable).unwrap().modified().unwrap();
        std::fs::write(&executable, b"omega").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&executable)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert_eq!(std::fs::metadata(&executable).unwrap().len(), 5);
        let error = match resolve() {
            Err(error) => error,
            Ok(_) => panic!("stale receipt admitted a same-size executable replacement"),
        };
        assert!(error.contains("SHA-256 does not match executable"));
        std::fs::write(&executable, b"alpha").unwrap();
        assert!(matches!(resolve(), Ok(Some(_))));
        let receipt_bytes = std::fs::read(&receipt).unwrap();
        let mut wrong_receipt: serde_json::Value = serde_json::from_slice(&receipt_bytes).unwrap();
        wrong_receipt["executable_sha256"] = serde_json::json!("0".repeat(64));
        std::fs::write(&receipt, serde_json::to_vec(&wrong_receipt).unwrap()).unwrap();
        assert!(resolve().err().unwrap().contains("SHA-256 does not match"));
        std::fs::write(&receipt, b"[]").unwrap();
        assert!(resolve().err().unwrap().contains("malformed"));
        std::fs::remove_file(&receipt).unwrap();
        assert!(resolve().unwrap().unwrap().build_receipt_bytes.is_none());
        std::fs::write(&receipt, receipt_bytes).unwrap();
        assert!(matches!(resolve(), Ok(Some(_))));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(resolve().err().unwrap().contains("not executable"));
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
            let replacement = executable.with_extension("replacement");
            std::fs::write(&replacement, b"omega").unwrap();
            std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::rename(&replacement, &executable).unwrap();
            assert!(resolve().err().unwrap().contains("SHA-256 does not match"));
            std::fs::remove_file(&executable).unwrap();
            std::os::unix::fs::symlink("absent", &executable).unwrap();
            assert!(resolve().err().unwrap().contains("not a regular file"));
        }
        std::fs::remove_file(&executable).unwrap();
        assert!(resolve().err().unwrap().contains("cannot be inspected"));
    }

    #[test]
    fn spawned_life_protocol_accepts_host_attested_system_emission_shape_only() {
        let retained = serde_json::json!({
            "kind": "retained_arrival",
            "request_id": 2,
            "delivery_id": "opaque original delivery",
            "artifact_manifest_path": "/tmp/emitted.artifact.json",
            "evidence": []
        });
        assert!(matches!(
            serde_json::from_value::<SpawnedLifeOutput>(retained.clone()).unwrap(),
            SpawnedLifeOutput::RetainedArrival { .. }
        ));
        let mut spoofed_scope = retained;
        spoofed_scope["delivery_scope_seq"] = serde_json::json!(7);
        assert!(serde_json::from_value::<SpawnedLifeOutput>(spoofed_scope).is_err());
        for identity in ["".to_string(), "é".repeat(129)] {
            assert!(validate_retained_delivery_id(&identity).is_err());
        }
        assert!(validate_retained_delivery_id(&"é".repeat(128)).is_ok());
        let reply = SpawnedLifeInput::RetainedDelivery {
            request_id: 2,
            delivery: Ok(octopus_ledger::ArtifactDelivery::Pending { scope_seq: 7 }),
        };
        let bytes = serialize_bounded_json_line(&reply, 1024, "retained response").unwrap();
        assert!(matches!(
            serde_json::from_slice::<SpawnedLifeInput>(&bytes).unwrap(),
            SpawnedLifeInput::RetainedDelivery {
                request_id: 2,
                delivery: Ok(octopus_ledger::ArtifactDelivery::Pending { scope_seq: 7 })
            }
        ));
        let reply = SpawnedLifeInput::RetainedDelivery { request_id: 2, delivery: Err("scope corruption".into()) };
        let bytes = serialize_bounded_json_line(&reply, 1024, "retained response").unwrap();
        assert!(matches!(serde_json::from_slice::<SpawnedLifeInput>(&bytes).unwrap(),
            SpawnedLifeInput::RetainedDelivery { delivery: Err(error), .. } if error == "scope corruption"));

        let emission = serde_json::from_value::<SpawnedLifeOutput>(serde_json::json!({
            "kind": "system_emission",
            "request_id": 1,
            "artifact": {
                "carrier": "materialized",
                "artifact_manifest_path": "/tmp/emitted.artifact.json"
            },
            "source_movement_seq": 17,
            "to_interface": "system_worker_coder_max"
        }))
        .unwrap();
        assert!(matches!(emission, SpawnedLifeOutput::SystemEmission { .. }));

        let spoofed_source = serde_json::from_value::<SpawnedLifeOutput>(serde_json::json!({
            "kind": "system_emission",
            "request_id": 1,
            "artifact": {
                "carrier": "materialized",
                "artifact_manifest_path": "/tmp/emitted.artifact.json"
            },
            "from_interface": "system_spoofed",
            "source_movement_seq": 17,
            "to_interface": "system_worker_coder_max"
        }));
        assert!(spoofed_source.is_err());

        let spoofed_coordinates = serde_json::from_value::<SpawnedLifeOutput>(serde_json::json!({
            "kind": "system_emission",
            "request_id": 1,
            "artifact": {
                "carrier": "materialized",
                "artifact_manifest_path": "/tmp/emitted.artifact.json"
            },
            "source_movement_seq": 17,
            "emitter_run": "run_spoofed",
            "emitter_checkpoint": "checkpoint_spoofed",
            "to_interface": "system_worker_coder_max"
        }));
        assert!(spoofed_coordinates.is_err());

        let raw_envelope = serde_json::from_value::<SpawnedLifeOutput>(serde_json::json!({
            "kind": "tool_process_invocation",
            "request_id": 2,
            "tool": "file.write",
            "arguments": {"path": "/tmp/spoofed"},
            "source_movement_seq": 9,
            "envelope": {"metadata": {"runtime": {"run": "run_spoofed"}}}
        }));
        assert!(raw_envelope.is_err());
    }

    #[tokio::test]
    async fn stderr_ceiling_is_an_enforced_failure() {
        use tokio::io::AsyncWriteExt;

        let (mut writer, reader) = tokio::io::duplex(64);
        writer.write_all(&[b'x'; 33]).await.unwrap();
        writer.shutdown().await.unwrap();
        let error = drain_bounded_stderr_to_private_file(
            reader,
            32,
            InterfaceId::new("system_test"),
            "test_process",
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("exceeded declared ceiling of 32 bytes"));
        assert!(!error.contains("xxx"));
    }

    #[tokio::test]
    async fn private_stderr_capture_never_overwrites_prior_incarnation() {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interface-life.stderr.existing.log");
        std::fs::write(&path, b"prior crash").unwrap();
        let (mut writer, reader) = tokio::io::duplex(64);
        writer.write_all(b"new crash").await.unwrap();
        writer.shutdown().await.unwrap();

        let error = drain_bounded_stderr_to_private_file(
            reader,
            64,
            InterfaceId::new("system_test"),
            "test_process",
            Some(path.clone()),
        )
        .await
        .unwrap_err();

        assert!(error.contains("cannot be opened"));
        assert_eq!(std::fs::read(path).unwrap(), b"prior crash");
    }

    #[tokio::test]
    async fn missing_required_env_fails_single_and_multi_process_life_before_spawn() {
        let missing_name = format!(
            "OCTOPUS_REQUIRED_ENV_TEST_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        assert!(std::env::var_os(&missing_name).is_none());
        let missing = SpawnLifeCommand {
            bin: "/usr/bin/true".to_string(),
            io_limits: test_host_io_limits(),
            doctor: None,
            private_var_directories: BTreeSet::new(),
            args: Vec::new(),
            required_files: BTreeSet::new(),
            env: BTreeMap::new(),
            inherit_env: BTreeSet::new(),
            required_env: BTreeSet::from([missing_name.clone()]),
            inherit_env_from_interface: BTreeSet::new(),
            authority: ProcessAuthoritySpec::default(),
            coordinate_root_slots: BTreeSet::new(),
            read_handles: BTreeSet::new(),
        };
        let available = SpawnLifeCommand {
            required_env: BTreeSet::new(),
            ..missing.clone()
        };
        let root = tempfile::tempdir().unwrap();
        for interface in ["world_required_env_single", "world_required_env_multi"] {
            let interface_dir = root.path().join("registry/interface").join(interface);
            std::fs::create_dir_all(&interface_dir).unwrap();
            std::fs::write(
                interface_dir.join("interface.json"),
                serde_json::json!({
                    "id": interface,
                    "interface_type": "world",
                    "role": "service",
                    "direction": "bidirectional",
                    "source_names": [interface],
                    "interface_driver_ref": "test_driver"
                })
                .to_string(),
            )
            .unwrap();
        }
        let single = spawn_process_life(
            ctx(root.path(), "world_required_env_single"),
            SpawnLifeConfig {
                processes: vec![missing.clone()],
            },
        )
        .await
        .unwrap_err();
        assert!(single.contains(&missing_name));
        let multi = spawn_process_life(
            ctx(root.path(), "world_required_env_multi"),
            SpawnLifeConfig {
                processes: vec![available, missing],
            },
        )
        .await
        .unwrap_err();
        assert!(multi.contains(&missing_name));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn life_ownership_covers_process_readiness_cancellation_and_failed_readiness() {
        for (lose_readiness, sandbox_mode) in [
            (false, "disabled"), (true, "disabled"),
            #[cfg(target_os = "macos")]
            (false, "enforced"),
            #[cfg(target_os = "macos")]
            (true, "enforced"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let interface = InterfaceId::new("world_owned_process");
            let mut context = ctx(root.path(), interface.as_str());
            std::fs::create_dir_all(&context.interface_dir).unwrap();
            std::fs::write(context.interface_dir.join("interface.json"), serde_json::json!({
                "id": interface, "interface_type": "world", "role": "service",
                "direction": "bidirectional", "source_names": ["test"],
                "interface_driver_ref": "test_driver"
            }).to_string()).unwrap();
            context.root_slots.insert(crate::INTERFACE_LIFE_INSTALL_ROOT_SLOT.into(), root.path().to_path_buf());
            let private_root = context.var_dir.parent().unwrap().to_path_buf();
            let cancel = context.cancel.clone();
            let (context, ready) = context.with_readiness_channel();
            let mut ready = Some(ready);
            let guard = crate::InterfaceLifeOwnership::try_acquire(&private_root, &interface).unwrap().unwrap();
            let config: SpawnLifeConfig = serde_json::from_value(serde_json::json!({
                "processes": [{
                    "bin": "/bin/sh", "args": ["-c", "/bin/sleep 60 & printf '{\"kind\":\"ready\"}\\n'; wait"],
                    "io_limits": test_host_io_limits(),
                    "read_handles": [],
                    "authority": {
                        "sandbox_mode": sandbox_mode, "read_root_slots": [], "write_root_slots": [],
                        "executable_root_slots": [], "read_paths": [], "write_paths": [],
                        "executable_paths": ["/bin/sh", "/bin/sleep"], "executable_commands": [],
                        "read_env_paths": [], "write_env_paths": [], "executable_env_paths": [],
                        "allow_network": false, "allow_local_ipc": false
                    }
                }]
            })).unwrap();
            let handle = tokio::spawn(spawn_process_life(context, config));
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            assert!(!handle.is_finished());
            assert!(matches!(ready.as_mut().unwrap().try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
            if lose_readiness { drop(ready.take()); }
            drop(guard);
            if !lose_readiness {
                tokio::time::timeout(std::time::Duration::from_secs(3), ready.take().unwrap()).await.unwrap().unwrap().unwrap();
                assert!(crate::InterfaceLifeOwnership::try_acquire(&private_root, &interface).unwrap().is_none());
                cancel.cancel();
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(3), handle).await.unwrap().unwrap();
            assert_eq!(result.is_err(), lose_readiness);
            assert!(crate::InterfaceLifeOwnership::try_acquire(&private_root, &interface).unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn bounded_json_line_reader_refuses_short_unterminated_frame() {
        use tokio::io::AsyncWriteExt;

        let (mut writer, reader) = tokio::io::duplex(64);
        writer.write_all(br#"{"ok":true}"#).await.unwrap();
        writer.shutdown().await.unwrap();
        let mut lines = BoundedJsonLineReader::new(reader, 64).unwrap();
        let error = lines.next_line().await.unwrap_err();
        assert!(error.contains("unterminated frame"));
    }

    #[cfg(unix)]
    #[test]
    fn spawned_artifact_capture_refuses_absolute_and_symlink_payload_escape() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("output");
        std::fs::create_dir_all(&root).unwrap();
        let outside = dir.path().join("outside.payload");
        std::fs::write(&outside, b"outside").unwrap();
        let absolute_manifest = root.join("absolute.artifact.json");
        std::fs::write(
            &absolute_manifest,
            serde_json::to_vec(&InterfaceArtifactManifest {
                label: "interface.output".to_string(),
                payload_path: outside.clone(),
                payload_content_hash: None,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(
            capture_spawned_artifact(
                &absolute_manifest,
                std::slice::from_ref(&root),
                output_limits()
            )
            .is_err()
        );

        let opaque_payload = serde_json::to_vec(&serde_json::json!({
            "payload_path": outside,
            "arbitrary_interface_field": {"value": 7}
        }))
        .unwrap();
        let opaque_payload_path = root.join("opaque.payload");
        let opaque_manifest = root.join("opaque.artifact.json");
        write_manifest(
            &opaque_manifest,
            "interface.output",
            &opaque_payload_path,
            &opaque_payload,
        );
        let opaque = capture_spawned_artifact(
            &opaque_manifest,
            std::slice::from_ref(&root),
            output_limits(),
        )
        .unwrap();
        assert_eq!(opaque.read_payload_bytes().unwrap(), opaque_payload);

        let valid_payload = root.join("valid.payload");
        let valid_manifest = root.join("valid.artifact.json");
        write_manifest(
            &valid_manifest,
            "interface.output",
            &valid_payload,
            b"inside",
        );
        let manifest_link = root.join("manifest-link.artifact.json");
        symlink(&valid_manifest, &manifest_link).unwrap();
        assert!(
            capture_spawned_artifact(&manifest_link, std::slice::from_ref(&root), output_limits())
                .is_err()
        );

        let linked = root.join("linked.payload");
        symlink(&outside, &linked).unwrap();
        let linked_manifest = root.join("linked.artifact.json");
        std::fs::write(
            &linked_manifest,
            serde_json::to_vec(&InterfaceArtifactManifest {
                label: "interface.output".to_string(),
                payload_path: linked,
                payload_content_hash: None,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(capture_spawned_artifact(&linked_manifest, &[root], output_limits()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn spawned_artifact_capture_pins_bytes_before_foreign_paths_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("output");
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("result.payload");
        let manifest = root.join("result.artifact.json");
        write_manifest(&manifest, "interface.output", &payload, b"first");

        let captured =
            capture_spawned_artifact(&manifest, std::slice::from_ref(&root), output_limits())
                .unwrap();
        write_manifest(&manifest, "interface.changed", &payload, b"second");

        assert_eq!(captured.read_manifest().unwrap().label, "interface.output");
        assert_eq!(captured.read_payload_bytes().unwrap(), b"first");
        let loaded = captured.load_into_blob(&mut InMemoryBlobs::new()).unwrap();
        assert_eq!(loaded.manifest.label, "interface.output");
        assert_eq!(loaded.payload_bytes, b"first");
    }

    #[cfg(unix)]
    #[test]
    fn spawned_artifact_capture_refuses_payload_swapped_after_manifest_read() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("output");
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("result.payload");
        let manifest = root.join("result.artifact.json");
        let outside = dir.path().join("outside.payload");
        std::fs::write(&outside, b"outside").unwrap();
        write_manifest(&manifest, "interface.output", &payload, b"inside");
        let mut reads = 0;
        let result = capture_spawned_artifact_with(
            &manifest,
            std::slice::from_ref(&root),
            output_limits(),
            |path, roots, maximum_bytes| {
                let bytes = read_regular_file_beneath(path, roots, maximum_bytes)?;
                reads += 1;
                if reads == 1 {
                    std::fs::remove_file(&payload).unwrap();
                    symlink(&outside, &payload).unwrap();
                }
                Ok(bytes)
            },
        );
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn spawned_artifact_capture_enforces_declared_manifest_and_payload_ceilings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("output");
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("result.payload");
        let manifest = root.join("result.artifact.json");
        write_manifest(&manifest, "interface.output", &payload, b"payload");
        assert!(
            capture_spawned_artifact(
                &manifest,
                std::slice::from_ref(&root),
                InterfaceHostIoLimits {
                    max_control_frame_bytes: 64 * 1024,
                    max_stderr_bytes: 64 * 1024,
                    max_manifest_bytes: 1,
                    max_payload_bytes: 1024,
                }
            )
            .is_err()
        );
        assert!(
            capture_spawned_artifact(
                &manifest,
                std::slice::from_ref(&root),
                InterfaceHostIoLimits {
                    max_control_frame_bytes: 64 * 1024,
                    max_stderr_bytes: 64 * 1024,
                    max_manifest_bytes: 64 * 1024,
                    max_payload_bytes: 1,
                }
            )
            .is_err()
        );
    }

    #[test]
    fn mentioned_interfaces_fold_from_live_run_records() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "id": "run_a",
            "ee": "ee_test",
            "worker": "system_worker_test",
            "trace": "trace:run_a",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "c1",
                "run": "run_a",
                "trace": "trace:run_a:c1",
                "epoch": 0,
                "interface_binding": octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new("world_operator_chat")),
                "outbound_interface_bindings": [octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new("world_provider_codex"))]
            }],
            "active_checkpoints": ["c1"]
        }))
        .unwrap();
        std::fs::write(runs.join("run_a.json"), bytes).unwrap();
        std::fs::write(runs.join("run_broken.json"), b"{ this is not json").unwrap();

        let mentioned = interfaces_mentioned_in_live_runs(&runs).unwrap();
        assert!(mentioned.contains(&InterfaceId::new("world_operator_chat")));
        assert!(mentioned.contains(&InterfaceId::new("world_provider_codex")));
        assert_eq!(
            mentioned.len(),
            2,
            "a malformed sibling run record is skipped, never fail-closing the live scan"
        );
    }

    fn write_test_life(path: &Path) {
        std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({
                "processes": [{
                    "bin": "/bin/true",
                "io_limits": {
                    "max_control_frame_bytes": 1048576,
                    "max_stderr_bytes": 262144,
                    "max_manifest_bytes": 1048576,
                    "max_payload_bytes": 268435456
                },
                    "args": [],
                    "authority": {
                        "read_root_slots": [],
                        "write_root_slots": [],
                        "executable_root_slots": [],
                        "read_paths": [],
                        "write_paths": [],
                        "executable_paths": ["/bin/true"],
                        "executable_commands": [],
                        "read_env_paths": [],
                        "write_env_paths": [],
                        "executable_env_paths": [],
                        "allow_network": false,
                        "allow_local_ipc": false
                    },
                    "read_handles": []
                }]
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn executable_life_requires_a_declared_driver_identity() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry");
        let interface = registry.join("interface").join("world_test");
        std::fs::create_dir_all(&interface).unwrap();
        std::fs::write(
            interface.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_test",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["test"]
            }))
            .unwrap(),
        )
        .unwrap();
        write_test_life(&interface.join("life.json"));

        let error =
            resolve_spawn_life_config(&registry, &InterfaceId::new("world_test")).unwrap_err();
        assert!(error.contains("resolves no interface_driver_ref"));
    }

    #[test]
    fn exact_interface_life_fingerprint_tracks_its_own_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry");
        let instance = registry.join("interface").join("world_instance");
        std::fs::create_dir_all(&instance).unwrap();
        let write_instance = |profile: &str| {
            std::fs::write(
                instance.join("interface.json"),
                serde_json::to_vec(&serde_json::json!({
                    "id": "world_instance",
                    "interface_type": "world",
                    "role": "service",
                    "direction": "bidirectional",
                    "source_names": ["instance"],
                    "interface_driver_ref": "exact_driver",
                    "interface_driver_config": {
                        "private_config": {"profile": profile}
                    }
                }))
                .unwrap(),
            )
            .unwrap();
        };
        write_instance("alpha");
        write_test_life(&instance.join("life.json"));

        let first = resolve_spawn_life_config(&registry, &InterfaceId::new("world_instance"))
            .unwrap()
            .unwrap();
        assert_eq!(first.interface_descriptor.id.as_str(), "world_instance");
        assert_eq!(
            first.interface_descriptor.interface_driver_ref,
            "exact_driver"
        );
        assert_eq!(
            first
                .interface_descriptor
                .interface_driver_config
                .as_ref()
                .unwrap()
                .private_config["profile"],
            "alpha"
        );

        write_instance("beta");
        let second = resolve_spawn_life_config(&registry, &InterfaceId::new("world_instance"))
            .unwrap()
            .unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
        assert_eq!(
            second
                .interface_descriptor
                .interface_driver_config
                .as_ref()
                .unwrap()
                .private_config["profile"],
            "beta"
        );
    }

    #[cfg(unix)]
    #[test]
    fn own_life_fingerprint_tracks_executable_and_readable_owner_script_revisions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let registry = root.path().join("registry");
        let owner = registry.join("interface/world_authored");
        let executable = owner.join("packages/life");
        let script = owner.join("packages/script.py");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(owner.join("interface.json"), serde_json::to_vec(&serde_json::json!({
            "id":"world_authored", "interface_type":"world", "role":"service", "direction":"bidirectional",
            "source_names":["authored"], "interface_driver_ref":"authored"
        })).unwrap()).unwrap();
        write_test_life(&owner.join("life.json"));
        let mut life: serde_json::Value = serde_json::from_slice(&std::fs::read(owner.join("life.json")).unwrap()).unwrap();
        life["processes"][0]["bin"] = serde_json::json!("owner://packages/life");
        life["processes"][0]["args"] = serde_json::json!(["owner://packages/script.py"]);
        life["processes"][0]["required_files"] = serde_json::json!(["owner://packages/script.py"]);
        std::fs::write(owner.join("life.json"), serde_json::to_vec(&life).unwrap()).unwrap();
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&script, "print('alpha')").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
        let interface = InterfaceId::new("world_authored");
        let first = resolve_spawn_life_config(&registry, &interface).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let settled = resolve_spawn_life_config(&registry, &interface).unwrap().unwrap();
        assert_eq!(first.fingerprint, settled.fingerprint,
            "an unchanged executable must not restart when its cache observation settles");
        let source = |resolved: &ResolvedSpawnLifeConfig| resolved_spawn_life_source_fingerprint(&registry, &interface, resolved,
            &mut ImportedExecutableVerification::default()).unwrap();
        let first_source = source(&first);
        std::fs::write(&executable, "#!/bin/sh\nexit 1\n").unwrap();
        let second = resolve_spawn_life_config(&registry, &interface).unwrap().unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
        assert_ne!(first_source, source(&second));
        std::fs::write(&script, "print('omega')").unwrap();
        let third = resolve_spawn_life_config(&registry, &interface).unwrap().unwrap();
        assert_ne!(second.fingerprint, third.fingerprint);
        std::fs::remove_file(&executable).unwrap();
        assert!(resolved_spawn_life_source_fingerprint(&registry, &interface, &third, &mut ImportedExecutableVerification::default()).is_err());
        symlink("/bin/true", &executable).unwrap();
        assert!(resolve_spawn_life_executable(&registry, &interface, root.path(), &third.config.processes[0]).is_err());
        std::fs::remove_file(&executable).unwrap();
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let sibling = registry.join("interface/world_sibling");
        std::fs::rename(&owner, &sibling).unwrap();
        symlink(&sibling, &owner).unwrap();
        assert!(resolve_spawn_life_executable(&registry, &interface, root.path(), &third.config.processes[0]).is_err());
    }

    #[test]
    fn exact_interface_life_is_independent_of_unrelated_interfaces() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry");
        let unrelated = registry.join("interface").join("world_unrelated");
        let instance = registry.join("interface").join("world_instance");
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::create_dir_all(&instance).unwrap();
        std::fs::write(
            unrelated.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_unrelated",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["unrelated"],
                "interface_driver_ref": "unrelated_driver"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            instance.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_instance",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["instance"],
                "interface_driver_ref": "exact_driver"
            }))
            .unwrap(),
        )
        .unwrap();
        let write_life = |path: &Path, allow_network: bool| {
            std::fs::write(
                path,
                serde_json::to_vec(&serde_json::json!({
                    "processes": [{
                        "bin": "/bin/true",
                        "io_limits": test_host_io_limits(),
                        "args": [],
                        "authority": {
                            "read_root_slots": [],
                            "write_root_slots": [],
                            "executable_root_slots": [],
                            "read_paths": [],
                            "write_paths": [],
                            "executable_paths": ["/bin/true"],
                            "executable_commands": [],
                            "read_env_paths": [],
                            "write_env_paths": [],
                            "executable_env_paths": [],
                            "allow_network": allow_network,
                            "allow_local_ipc": false
                        },
                        "read_handles": []
                    }]
                }))
                .unwrap(),
            )
            .unwrap();
        };
        write_life(&unrelated.join("life.json"), true);
        write_life(&instance.join("life.json"), false);

        let first = resolve_spawn_life_config(&registry, &InterfaceId::new("world_instance"))
            .unwrap()
            .unwrap();
        let first_source = resolved_spawn_life_source_fingerprint(
            &registry,
            &InterfaceId::new("world_instance"),
            &first,
            &mut ImportedExecutableVerification::default(),
        )
        .unwrap();
        assert!(!first.config.processes[0].authority.allow_network);

        std::fs::write(
            unrelated.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_unrelated",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["unrelated", "changed"],
                "interface_driver_ref": "unrelated_driver"
            }))
            .unwrap(),
        )
        .unwrap();
        write_life(&unrelated.join("life.json"), true);
        let second = resolve_spawn_life_config(&registry, &InterfaceId::new("world_instance"))
            .unwrap()
            .unwrap();
        let second_source = resolved_spawn_life_source_fingerprint(
            &registry,
            &InterfaceId::new("world_instance"),
            &second,
            &mut ImportedExecutableVerification::default(),
        )
        .unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first_source, second_source);
    }

    #[test]
    fn exact_driver_config_is_used_for_life_environment_projection() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let registry = root.join("registry");
        let instance = registry.join("interface").join("world_instance");
        std::fs::create_dir_all(&instance).unwrap();
        std::fs::write(
            instance.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_instance",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["instance"],
                "interface_driver_ref": "exact_driver",
                "interface_driver_config": {
                    "private_config": {
                        "inherited_env_name": "PATH",
                        "inherited_env_names": ["PATH", "OCTOPUS_TEST_ENV_ABSENT"],
                        "invalid_env_names": ["PATH", 7]
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        write_test_life(&instance.join("life.json"));

        let context = ctx(root, "world_instance");
        let selected = interface_config_env(
            &context,
            &BTreeSet::from([
                "/interface_driver_config/private_config/inherited_env_name".to_string(),
                "/interface_driver_config/private_config/inherited_env_names".to_string(),
            ]),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, "PATH");
        assert_eq!(selected[0].1, std::env::var_os("PATH").unwrap());
        let error = interface_config_env(
            &context,
            &BTreeSet::from([
                "/interface_driver_config/private_config/invalid_env_names".to_string()
            ]),
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(error.contains("array item 1 is not a string"));
    }

    #[test]
    fn life_fingerprint_tracks_declared_repository_file_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let registry = root.join("registry");
        let interface = registry.join("interface").join("world_scripted");
        std::fs::create_dir_all(&interface).unwrap();
        std::fs::write(
            interface.join("interface.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "world_scripted",
                "interface_type": "world",
                "role": "service",
                "direction": "bidirectional",
                "source_names": ["scripted"],
                "interface_driver_ref": "scripted_driver"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("driver.py"), "print('alpha')\n").unwrap();
        std::fs::write(
            interface.join("life.json"),
            serde_json::to_vec(&serde_json::json!({
                "processes": [{
                    "bin": "/bin/true",
                "io_limits": {
                    "max_control_frame_bytes": 1048576,
                    "max_stderr_bytes": 262144,
                    "max_manifest_bytes": 1048576,
                    "max_payload_bytes": 268435456
                },
                    "args": [{"repository_file": "driver.py"}],
                    "authority": {
                        "read_root_slots": [],
                        "write_root_slots": [],
                        "executable_root_slots": [],
                        "read_paths": [],
                        "write_paths": [],
                        "executable_paths": ["/bin/true"],
                        "executable_commands": [],
                        "read_env_paths": [],
                        "write_env_paths": [],
                        "executable_env_paths": [],
                        "allow_network": false,
                        "allow_local_ipc": false
                    },
                    "read_handles": []
                }]
            }))
            .unwrap(),
        )
        .unwrap();

        let first = resolve_spawn_life_config(&registry, &InterfaceId::new("world_scripted"))
            .unwrap()
            .unwrap();
        std::fs::write(root.join("driver.py"), "print('beta')\n").unwrap();
        let second = resolve_spawn_life_config(&registry, &InterfaceId::new("world_scripted"))
            .unwrap()
            .unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
    }

    #[test]
    fn every_repository_life_resolves_to_one_executable_driver_identity() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let registry = repository.join("registry");
        let interface_root = registry.join("interface");
        for entry in std::fs::read_dir(&interface_root).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_dir()
                || !entry.path().join("interface.json").is_file()
            {
                continue;
            }
            let id = entry.file_name().into_string().unwrap();
            let resolved = validate_spawn_life_config(&registry, &InterfaceId::new(&id))
                .unwrap_or_else(|error| panic!("interface '{id}' life failed to resolve: {error}"));
            if let Some(resolved) = resolved {
                assert_eq!(resolved.interface_descriptor.id.as_str(), id);
                assert!(resolved.interface_descriptor.has_interface_driver());
                assert!(!resolved.config.processes.is_empty());
            }
        }
    }

    #[test]
    fn optional_env_roots_grant_zero_authority_when_absent() {
        let root = tempfile::tempdir().unwrap();
        let optional = ProcessAuthoritySpec {
            optional_read_env_paths: BTreeSet::from(["OPTIONAL_READ_ROOT".to_string()]),
            optional_write_env_paths: BTreeSet::from(["OPTIONAL_WRITE_ROOT".to_string()]),
            ..ProcessAuthoritySpec::default()
        };
        optional
            .resolve(
                root.path(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                std::iter::empty::<PathBuf>(),
                std::iter::empty::<PathBuf>(),
                std::iter::empty::<PathBuf>(),
                std::iter::empty::<PathBuf>(),
            )
            .unwrap();
        let required = ProcessAuthoritySpec {
            read_env_paths: BTreeSet::from(["REQUIRED_READ_ROOT".to_string()]),
            ..ProcessAuthoritySpec::default()
        };
        let error = required
            .resolve(
                root.path(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                std::iter::empty::<PathBuf>(),
                std::iter::empty::<PathBuf>(),
                std::iter::empty::<PathBuf>(),
                std::iter::empty::<PathBuf>(),
            )
            .unwrap_err();
        assert!(error.contains("REQUIRED_READ_ROOT"));
    }
}
