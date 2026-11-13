use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[cfg(not(target_os = "macos"))]
use notify::{RecursiveMode, Watcher};
use octopus_descriptors::InterfaceId;
use octopus_interfaces::{
    ArtifactBindingPayload, ArtifactBindingPayloadSet, InterfaceArtifactFile,
    InterfaceArtifactFileError, InterfaceArtifactManifest, InterfaceInvocationEnvelope,
    InvocationMetadata,
};
use octopus_ledger::{
    ArtifactRef, ContentHash, DurableMetadataContent, Endpoint, LedgerError, LedgerResult, Movement, MovementKind,
    decode_durable_movement_route, decode_durable_movement_row,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use tokio_util::sync::CancellationToken;

use crate::{InterfaceActivationArrival, InterfaceActivationEvidence, SystemInterfaceEmission};

pub const RESOLVED_INTERFACE_DESCRIPTOR_ENV: &str = "OCTOPUS_INTERFACE_DESCRIPTOR_JSON";
pub const INTERFACE_HOST_IO_LIMITS_ENV: &str = "OCTOPUS_INTERFACE_HOST_IO_LIMITS";
const AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET: usize = 256;
const LEDGER_INDEX_SCHEMA_VERSION: u64 = 4;
const RUNTIME_INTERFACE_OUTBOUND_CANDIDATE_SQL: &str = "SELECT seq, start_offset, end_offset
     FROM movements INDEXED BY idx_mv_runtime_interface_outbound
     WHERE runtime_interface = ?1
       AND runtime_interface IS NOT NULL
       AND kind = 'InterfaceOutbound'
       AND from_kind = 'interface'
       AND seq >= ?2
       AND seq < ?3
     ORDER BY seq ASC
     LIMIT 1";
const ENDPOINT_INTERFACE_OUTBOUND_CANDIDATE_SQL: &str = "SELECT seq, start_offset, end_offset
     FROM movements INDEXED BY idx_mv_interface_outbound_to
     WHERE kind = 'InterfaceOutbound'
       AND from_kind = 'interface'
       AND to_kind = 'interface'
       AND to_id = ?1
       AND seq >= ?2
       AND seq < ?3
     ORDER BY seq ASC
     LIMIT 1";

struct DurableMovementDecoder {
    metadata_dir: PathBuf,
    last_metadata: Mutex<HashMap<DurableMetadataContent, (ContentHash, MetadataFileFingerprint, serde_json::Value)>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MetadataFileFingerprint {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity_and_change: (u64, u64, i64, i64),
}

impl MetadataFileFingerprint {
    fn of(path: &Path, hash: ContentHash) -> LedgerResult<Self> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                LedgerError::BlobMissing(hash)
            } else {
                LedgerError::Io {
                    path: path.display().to_string(),
                    message: error.to_string(),
                }
            }
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(LedgerError::Io {
                path: path.display().to_string(),
                message: "movement metadata content is not a regular file".to_string(),
            });
        }
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().map_err(|error| LedgerError::Io {
                path: path.display().to_string(),
                message: error.to_string(),
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
        })
    }
}

impl DurableMovementDecoder {
    fn for_log(movements_path: impl AsRef<Path>) -> Self {
        let metadata_dir = movements_path
            .as_ref()
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join("metadata");
        Self {
            metadata_dir,
            last_metadata: Mutex::new(HashMap::new()),
        }
    }

    fn decode(&self, row: &[u8], line: usize) -> LedgerResult<Movement> {
        decode_durable_movement_row(row, line, |kind, hash| self.metadata_value(kind, hash, line))
    }

    fn metadata_value(&self, kind: DurableMetadataContent, hash: ContentHash, line: usize) -> LedgerResult<serde_json::Value> {
        let path = self.metadata_dir.join(hash.to_hex());
        let fingerprint = MetadataFileFingerprint::of(&path, hash)?;
        let mut last_metadata =
            self.last_metadata
                .lock()
                .map_err(|_| LedgerError::MalformedMovementMetadata {
                    line,
                    message: "movement metadata value cache is poisoned".to_string(),
                })?;
        if let Some((cached_hash, cached_fingerprint, value)) = last_metadata.get(&kind)
            && *cached_hash == hash
            && *cached_fingerprint == fingerprint
        {
            return Ok(value.clone());
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LedgerError::BlobMissing(hash));
            }
            Err(error) => {
                return Err(LedgerError::Io {
                    path: path.display().to_string(),
                    message: error.to_string(),
                });
            }
        };
        let actual = ContentHash::of_bytes(&bytes);
        if actual != hash {
            return Err(LedgerError::BlobIntegrity {
                expected: hash,
                actual,
            });
        }
        if MetadataFileFingerprint::of(&path, hash)? != fingerprint {
            return Err(LedgerError::Io {
                path: path.display().to_string(),
                message: "movement metadata content changed while being read".to_string(),
            });
        }
        let value = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
            LedgerError::MalformedMovementMetadata {
                line,
                message: format!("content-addressed {} is malformed: {error}", kind.describe()),
            }
        })?;
        last_metadata.insert(kind, (hash, fingerprint, value.clone()));
        Ok(value)
    }
}

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

pub async fn drain_bounded_stderr<R>(
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
            options.create(true).truncate(true).write(true);
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

pub fn interface_descriptor_from_process_env(
    interface: &InterfaceId,
) -> Result<Option<octopus_interfaces::InterfaceDescriptor>, String> {
    let Some(bytes) = std::env::var_os(RESOLVED_INTERFACE_DESCRIPTOR_ENV) else {
        return Ok(None);
    };
    let bytes = bytes.into_string().map_err(|_| {
        format!(
            "{RESOLVED_INTERFACE_DESCRIPTOR_ENV} is not valid UTF-8 for interface '{}'",
            interface.as_str()
        )
    })?;
    let descriptor = octopus_descriptors::parse_strict::<octopus_interfaces::InterfaceDescriptor>(
        bytes.as_bytes(),
    )
    .map_err(|error| {
        format!(
            "{RESOLVED_INTERFACE_DESCRIPTOR_ENV} is malformed for interface '{}': {error}",
            interface.as_str()
        )
    })?;
    if descriptor.id != *interface || !descriptor.has_interface_driver() {
        return Err(format!(
            "{RESOLVED_INTERFACE_DESCRIPTOR_ENV} does not describe executable interface '{}'",
            interface.as_str()
        ));
    }
    Ok(Some(descriptor))
}

#[derive(Clone)]
pub struct InterfaceLiftContext {
    pub(crate) interface: InterfaceId,
    pub(crate) interface_dir: PathBuf,
    pub(crate) descriptor_snapshot: Option<octopus_interfaces::InterfaceDescriptor>,
    pub(crate) var_dir: PathBuf,
    pub(crate) movements_path: PathBuf,
    pub(crate) blobs_dir: PathBuf,
    pub(crate) ledger_index_path: PathBuf,
    pub(crate) root_slots: BTreeMap<String, PathBuf>,
    pub(crate) host_io_limits: Option<InterfaceHostIoLimits>,
    pub(crate) arrivals: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    pub(crate) emissions: tokio::sync::mpsc::Sender<SystemInterfaceEmission>,
    pub(crate) cancel: CancellationToken,
    readiness: InterfaceLifeReadiness,
    source_movement: Option<Arc<Movement>>,
    pub(crate) asleep: Arc<std::sync::Mutex<Option<u64>>>,
}

#[derive(Clone, Copy, Debug)]
pub struct InterfaceLedgerReadHandles<'a> {
    pub movements_path: &'a Path,
    pub ledger_index_path: &'a Path,
    pub blobs_dir: &'a Path,
    pub limits: InterfaceHostIoLimits,
}

#[derive(Clone, Default)]
struct InterfaceLifeReadiness {
    sender: Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Result<(), String>>>>>,
}

impl InterfaceLifeReadiness {
    fn channel() -> (Self, tokio::sync::oneshot::Receiver<Result<(), String>>) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        (
            Self {
                sender: Arc::new(std::sync::Mutex::new(Some(sender))),
            },
            receiver,
        )
    }

    fn signal(&self, result: Result<(), String>) -> Result<(), String> {
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
    #[cfg(any(test, feature = "owner-contract-tests"))]
    pub(crate) fn unprivileged(
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
            source_movement: None,
            asleep: Default::default(),
        }
    }

    pub(crate) fn with_readiness_channel(
        mut self,
    ) -> (Self, tokio::sync::oneshot::Receiver<Result<(), String>>) {
        let (readiness, receiver) = InterfaceLifeReadiness::channel();
        self.readiness = readiness;
        (self, receiver)
    }

    pub(crate) fn signal_ready(&self) -> Result<(), String> {
        self.readiness.signal(Ok(()))
    }

    pub(crate) fn fall_asleep(&self, through_seq: u64) -> Result<(), String> {
        let mut asleep = self
            .asleep
            .lock()
            .map_err(|_| "interface life sleep lock is poisoned".to_string())?;
        if asleep.is_some() {
            return Err("interface life declared sleep twice".to_string());
        }
        *asleep = Some(through_seq);
        Ok(())
    }

    pub(crate) fn required_host_io_limits(&self) -> Result<InterfaceHostIoLimits, String> {
        let limits = self.host_io_limits.ok_or_else(|| {
            format!(
                "interface '{}' has no process-owned host I/O limits",
                self.interface.as_str()
            )
        })?;
        limits.validate()?;
        Ok(limits)
    }

    pub(crate) async fn hand_system_emission(
        &self,
        artifact: crate::SystemInterfaceEmissionArtifact,
        source_movement_seq: u64,
        to_interface: InterfaceId,
    ) -> Result<crate::DeliveryVerdict, String> {
        let artifact = match artifact {
            crate::SystemInterfaceEmissionArtifact::Materialized(artifact) => {
                let limits = self.required_host_io_limits()?;
                let mut roots = vec![self.interface_var_dir()];
                if let Some(root) = self.root_slots.get("interface_hand_in_root") {
                    roots.push(root.join(self.interface.as_str()));
                }
                let private = capture_spawned_artifact(artifact.manifest_path(), &roots, limits)?;
                let original = artifact
                    .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                    .map_err(|error| format!("interface output carrier capture failed: {error}"))?;
                let captured = private.read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                    .map_err(|error| error.to_string())?;
                if original.manifest_bytes != captured.manifest_bytes || original.payload_bytes != captured.payload_bytes {
                    return Err("interface emission differs from its exact private output files".into());
                }
                crate::SystemInterfaceEmissionArtifact::Materialized(private)
            }
            artifact @ crate::SystemInterfaceEmissionArtifact::Reference(_) => artifact,
        };
        let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
        self.emissions
            .send(SystemInterfaceEmission::host_attested(
                artifact,
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

    pub(crate) fn with_source_movement(&self, movement: Movement) -> Self {
        let mut context = self.clone();
        context.source_movement = Some(Arc::new(movement));
        context
    }

    pub fn source_movement(
        &self,
        envelope: &InterfaceInvocationEnvelope,
    ) -> Result<&Movement, String> {
        let movement = self.source_movement.as_deref()
            .ok_or("owner artifact decoder has no original ledger movement")?;
        let metadata = envelope.metadata().map_err(|error| error.to_string())?;
        if movement.kind != MovementKind::InterfaceOutbound
            || movement.to != Endpoint::Interface(self.interface.clone())
            || movement.artifact.as_ref() != Some(&envelope.artifact)
            || movement.invocation_metadata.as_ref().and_then(|value| value.invocation()) != Some(metadata)
        {
            return Err("owner artifact decoder source movement does not match its exact envelope".into());
        }
        Ok(movement)
    }

    pub fn ledger_read_handles(&self) -> Result<InterfaceLedgerReadHandles<'_>, String> {
        let limits = self.required_host_io_limits()?;
        if [&self.movements_path, &self.ledger_index_path, &self.blobs_dir]
            .into_iter().any(|path| !path.is_absolute()) {
            return Err(format!(
                "interface '{}' requires explicit ledger, index and blob read handles",
                self.interface.as_str()
            ));
        }
        Ok(InterfaceLedgerReadHandles {
            movements_path: &self.movements_path,
            ledger_index_path: &self.ledger_index_path,
            blobs_dir: &self.blobs_dir,
            limits,
        })
    }

    pub fn owner_interface(&self) -> &InterfaceId {
        &self.interface
    }

    pub fn interface_var_dir(&self) -> PathBuf {
        self.var_dir.clone()
    }

    pub fn interface_registry_dir(&self) -> PathBuf {
        self.interface_dir.clone()
    }

    pub(crate) fn resolved_interface_descriptor(
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

    pub(crate) fn installation_root(&self) -> Result<PathBuf, String> {
        self.root_slot("interface_life_install_root")
            .map(Path::to_path_buf)
    }

    pub(crate) fn root_slot(&self, name: &str) -> Result<&Path, String> {
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

    pub(crate) async fn hand_arrival(
        &self,
        artifact: InterfaceArtifactFile,
        evidence: Vec<InterfaceActivationEvidence>,
    ) -> Result<crate::DeliveryVerdict, String> {
        let limits = self.required_host_io_limits()?;
        let artifact = artifact
            .capture_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
            .map_err(|error| format!("interface output carrier capture failed: {error}"))?;
        let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
        self.arrivals
            .send(InterfaceActivationArrival {
                artifact,
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

    pub(crate) async fn hand_arrival_retained(
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
                evidence,
                verdict_tx: None,
                retained: Some(crate::RetainedInterfaceArrival { id: delivery_id.to_string(), reply }),
            })
            .await
            .map_err(|_| "arrival intake closed; the spine is shutting down".to_string())?;
        result.await.map_err(|_| "retained arrival reply channel dropped before completion".to_string())?
    }

    pub(crate) fn failure_artifact(
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

    pub(crate) fn tool_result_failure_artifact(
        &self,
        tool: &str,
        correlation_id: Option<&str>,
        error: octopus_tool_handlers::ToolResultError,
    ) -> Result<InterfaceArtifactFile, String> {
        let result = octopus_tool_handlers::ToolResultArtifact::failure(
            tool,
            correlation_id.map(str::to_string),
            error,
        );
        let payload = serde_json::to_vec(&result).map_err(|encode_error| {
            format!("tool result failure artifact did not encode: {encode_error}")
        })?;
        InterfaceArtifactFile::write(
            self.var_dir.join("failure-artifacts"),
            &format!("{tool}.result"),
            &payload,
        )
        .map_err(|write_error| {
            format!("tool result failure artifact did not materialize: {write_error}")
        })
    }
}

pub type InterfaceLifeFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

pub type InterfaceLiftFn = Arc<dyn Fn(InterfaceLiftContext) -> InterfaceLifeFuture + Send + Sync>;

struct ResolvedInterfaceDescriptorFiles {
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
    Ok(Some(ResolvedInterfaceDescriptorFiles { descriptor }))
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
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "life private VAR root '{}' cannot be made owner-only: {error}",
            root.display()
        )
    })?;
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnedEvidenceLine {
    interface_id: String,
    artifact_id: String,
    label: String,
    bytes: Vec<u8>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "carrier", rename_all = "snake_case", deny_unknown_fields)]
enum SpawnedLifeEmissionArtifact {
    Materialized { artifact_manifest_path: PathBuf },
    Reference { artifact: ArtifactRef },
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
    Asleep {
        through_seq: u64,
    },
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SpawnedLifeInput {
    DeliveryVerdict {
        request_id: u64,
        verdict: crate::DeliveryVerdict,
    },
    RetainedDelivery {
        request_id: u64,
        delivery: Result<octopus_ledger::ArtifactDelivery, String>,
    },
}

fn validate_retained_delivery_id(delivery_id: &str) -> Result<(), String> {
    if delivery_id.is_empty() || delivery_id.len() > 256 {
        return Err("retained delivery identity must contain 1 to 256 bytes".into());
    }
    Ok(())
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

pub async fn run_spawned_interface_life(life: InterfaceLiftFn) -> Result<(), String> {
    let (arrivals, arrival_rx) = tokio::sync::mpsc::channel(64);
    let (emissions, emission_rx) = tokio::sync::mpsc::channel(1);
    let ctx = spawned_context_from_env(arrivals, emissions)?;
    let host_io_limits = ctx.required_host_io_limits()?;
    let (ctx, readiness) = ctx.with_readiness_channel();

    let pending = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let output = pump_spawned_life_outputs(
        arrival_rx,
        emission_rx,
        readiness,
        Arc::clone(&pending),
        host_io_limits.max_control_frame_bytes,
    );
    let input =
        pump_spawned_life_inputs(Arc::clone(&pending), host_io_limits.max_control_frame_bytes);
    let asleep = Arc::clone(&ctx.asleep);
    let life = life(ctx);
    tokio::pin!(output);
    tokio::pin!(input);
    tokio::pin!(life);
    let result = tokio::select! {
        result = &mut life => result,
        result = &mut output => return result,
        result = &mut input => return result,
    };
    result?;
    let through_seq = *asleep
        .lock()
        .map_err(|_| "interface life sleep lock is poisoned".to_string())?;
    if let Some(through_seq) = through_seq {
        use std::io::Write;
        let bytes = serialize_bounded_json_line(
            &SpawnedLifeOutput::Asleep { through_seq },
            host_io_limits.max_control_frame_bytes,
            "spawned-life output frame",
        )?;
        let mut stdout = std::io::stdout();
        stdout
            .write_all(&bytes)
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("spawned life sleep frame write failed: {error}"))?;
    }
    Ok(())
}

fn required_spawned_env_path(name: &str) -> Result<PathBuf, String> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} is required by the spawned-life env contract"))
}

fn optional_spawned_env_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn spawned_context_from_env(
    arrivals: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    emissions: tokio::sync::mpsc::Sender<crate::SystemInterfaceEmission>,
) -> Result<InterfaceLiftContext, String> {
    let interface = std::env::var("OCTOPUS_INTERFACE_ID").map_err(|_| {
        "OCTOPUS_INTERFACE_ID is required by the spawned-life env contract".to_string()
    })?;
    let interface = InterfaceId::new(interface);
    let descriptor_snapshot =
        interface_descriptor_from_process_env(&interface)?.ok_or_else(|| {
            format!(
                "{RESOLVED_INTERFACE_DESCRIPTOR_ENV} is required by the spawned-life env contract"
            )
        })?;
    let interface_dir = required_spawned_env_path("OCTOPUS_INTERFACE_DIR")?;
    let var_dir = required_spawned_env_path("OCTOPUS_VAR_DIR")?;
    let host_io_limits: InterfaceHostIoLimits =
        serde_json::from_str(&std::env::var(INTERFACE_HOST_IO_LIMITS_ENV).map_err(|_| {
            format!("{INTERFACE_HOST_IO_LIMITS_ENV} is required by the spawned-life env contract")
        })?)
        .map_err(|error| format!("{INTERFACE_HOST_IO_LIMITS_ENV} is malformed: {error}"))?;
    host_io_limits.validate()?;
    Ok(InterfaceLiftContext {
        interface,
        interface_dir,
        descriptor_snapshot: Some(descriptor_snapshot),
        var_dir,
        movements_path: optional_spawned_env_path("OCTOPUS_MOVEMENTS_JSONL"),
        blobs_dir: optional_spawned_env_path("OCTOPUS_BLOBS_DIR"),
        ledger_index_path: optional_spawned_env_path("OCTOPUS_LEDGER_INDEX_SQLITE"),
        root_slots: serde_json::from_str(
            &std::env::var("OCTOPUS_ROOT_SLOTS_JSON").map_err(|_| "OCTOPUS_ROOT_SLOTS_JSON is required by the process env contract".to_string())?,
        )
        .map_err(|error| format!("OCTOPUS_ROOT_SLOTS_JSON is malformed: {error}"))?,
        host_io_limits: Some(host_io_limits),
        arrivals,
        emissions,
        cancel: CancellationToken::new(),
        readiness: InterfaceLifeReadiness::default(),
        source_movement: None,
        asleep: Default::default(),
    })
}

enum SpawnedReply {
    Ordinary(tokio::sync::oneshot::Sender<crate::DeliveryVerdict>),
    Retained(tokio::sync::oneshot::Sender<Result<octopus_ledger::ArtifactDelivery, String>>),
}

type PendingVerdicts = Arc<tokio::sync::Mutex<BTreeMap<u64, SpawnedReply>>>;

async fn pump_spawned_life_outputs(
    mut arrivals: tokio::sync::mpsc::Receiver<InterfaceActivationArrival>,
    mut emissions: tokio::sync::mpsc::Receiver<crate::SystemInterfaceEmission>,
    readiness: tokio::sync::oneshot::Receiver<Result<(), String>>,
    pending: PendingVerdicts,
    maximum_frame_bytes: u64,
) -> Result<(), String> {
    use std::io::Write;
    let mut stdout = std::io::stdout();
    let mut next_request_id = 1_u64;
    let mut readiness = Some(readiness);
    loop {
        let output = tokio::select! {
            biased;
            ready = async {
                readiness
                    .as_mut()
                    .expect("guarded readiness receiver")
                    .await
            }, if readiness.is_some() => {
                readiness = None;
                ready
                    .map_err(|_| "spawned life readiness channel closed".to_string())??;
                SpawnedLifeOutput::Ready
            }
            arrival = arrivals.recv() => {
                let arrival = arrival
                    .ok_or_else(|| "spawned life arrival channel closed".to_string())?;
                spawned_arrival_output(arrival, &pending, &mut next_request_id).await?
            }
            emission = emissions.recv() => {
                let emission = emission
                    .ok_or_else(|| "spawned life system-emission channel closed".to_string())?;
                spawned_system_emission_output(emission, &pending, &mut next_request_id).await?
            }
        };
        let bytes =
            serialize_bounded_json_line(&output, maximum_frame_bytes, "spawned-life output frame")?;
        stdout
            .write_all(&bytes)
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("spawned life output write failed: {error}"))?;
    }
}

async fn spawned_arrival_output(
    arrival: InterfaceActivationArrival,
    pending: &PendingVerdicts,
    next_request_id: &mut u64,
) -> Result<SpawnedLifeOutput, String> {
    let artifact_manifest_path = arrival.artifact.manifest_path().to_path_buf();
    let evidence = arrival.evidence.into_iter().map(|item| SpawnedEvidenceLine {
        interface_id: item.interface_id.as_str().to_string(),
        artifact_id: item.artifact_id.as_str().to_string(),
        label: item.label,
        bytes: item.bytes,
    }).collect();
    match (arrival.verdict_tx, arrival.retained) {
        (None, Some(retained)) => {
            validate_retained_delivery_id(&retained.id)?;
            let request_id = register_spawned_reply(pending, next_request_id, SpawnedReply::Retained(retained.reply)).await?;
            Ok(SpawnedLifeOutput::RetainedArrival { request_id, delivery_id: retained.id, artifact_manifest_path, evidence })
        }
        (verdict_tx, None) => {
            let request_id = register_spawned_verdict(pending, next_request_id, verdict_tx).await?;
            Ok(SpawnedLifeOutput::Arrival { request_id, artifact_manifest_path, evidence })
        }
        _ => Err("spawned arrival must select one acknowledgement contract".into()),
    }
}

async fn spawned_system_emission_output(
    emission: crate::SystemInterfaceEmission,
    pending: &PendingVerdicts,
    next_request_id: &mut u64,
) -> Result<SpawnedLifeOutput, String> {
    let (artifact, source_movement_seq, to_interface, verdict_tx) = emission.into_parts();
    let request_id = register_spawned_verdict(pending, next_request_id, Some(verdict_tx))
        .await?
        .ok_or_else(|| "system emission requires a delivery verdict".to_string())?;
    Ok(SpawnedLifeOutput::SystemEmission {
        request_id,
        artifact: match artifact {
            crate::SystemInterfaceEmissionArtifact::Materialized(artifact) => {
                SpawnedLifeEmissionArtifact::Materialized {
                    artifact_manifest_path: artifact.manifest_path().to_path_buf(),
                }
            }
            crate::SystemInterfaceEmissionArtifact::Reference(artifact) => {
                SpawnedLifeEmissionArtifact::Reference { artifact }
            }
        },
        source_movement_seq,
        to_interface: to_interface.as_str().to_string(),
    })
}

async fn register_spawned_verdict(
    pending: &PendingVerdicts,
    next_request_id: &mut u64,
    verdict_tx: Option<tokio::sync::oneshot::Sender<crate::DeliveryVerdict>>,
) -> Result<Option<u64>, String> {
    let Some(verdict_tx) = verdict_tx else {
        return Ok(None);
    };
    register_spawned_reply(pending, next_request_id, SpawnedReply::Ordinary(verdict_tx)).await.map(Some)
}

async fn register_spawned_reply(
    pending: &PendingVerdicts,
    next_request_id: &mut u64,
    reply: SpawnedReply,
) -> Result<u64, String> {
    let request_id = *next_request_id;
    *next_request_id = next_request_id
        .checked_add(1)
        .ok_or_else(|| "spawned life request id overflow".to_string())?;
    match pending.lock().await.entry(request_id) {
        std::collections::btree_map::Entry::Vacant(entry) => { entry.insert(reply); }
        std::collections::btree_map::Entry::Occupied(_) => return Err("spawned life request id already has a reply contract".into()),
    }
    Ok(request_id)
}

async fn pump_spawned_life_inputs(
    pending: PendingVerdicts,
    maximum_frame_bytes: u64,
) -> Result<(), String> {
    let mut lines = BoundedJsonLineReader::new(tokio::io::stdin(), maximum_frame_bytes)?;
    while let Some(line) = lines.next_line().await? {
        let input: SpawnedLifeInput = serde_json::from_str(&line)
            .map_err(|error| format!("spawned life input is malformed: {error}"))?;
        accept_spawned_life_input(&pending, input).await?;
    }
    Err("spawned life input closed".to_string())
}

async fn accept_spawned_life_input(pending: &PendingVerdicts, input: SpawnedLifeInput) -> Result<(), String> {
    let request_id = match &input {
        SpawnedLifeInput::DeliveryVerdict { request_id, .. } | SpawnedLifeInput::RetainedDelivery { request_id, .. } => *request_id,
    };
    let reply = pending.lock().await.remove(&request_id)
        .ok_or_else(|| format!("spawned life received unknown request id {request_id}"))?;
    match (reply, input) {
        (SpawnedReply::Ordinary(sender), SpawnedLifeInput::DeliveryVerdict { verdict, .. }) => { let _ = sender.send(verdict); }
        (SpawnedReply::Retained(sender), SpawnedLifeInput::RetainedDelivery { delivery, .. }) => { let _ = sender.send(delivery); }
        _ => return Err("spawned life reply changed its request acknowledgement contract".into()),
    }
    Ok(())
}

pub struct OwnOutboundMovement {
    pub movement: Movement,
    metadata: Result<InvocationMetadata, String>,
    cursor: TailCursor,
}

impl OwnOutboundMovement {
    pub fn metadata(&self) -> Result<&InvocationMetadata, String> {
        self.metadata.as_ref().map_err(Clone::clone)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TailCursor {
    seq: u64,
    offset: u64,
}

fn read_bounded_ledger_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    maximum_bytes: u64,
) -> Result<usize, String> {
    let maximum_bytes = usize::try_from(maximum_bytes)
        .map_err(|_| "ledger row ceiling exceeds host address space".to_string())?;
    buffer.clear();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("ledger read failed: {error}"))?;
        if available.is_empty() {
            return Ok(buffer.len());
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if take > maximum_bytes.saturating_sub(buffer.len()) {
            return Err(format!(
                "ledger row exceeds declared interface ceiling of {maximum_bytes} bytes"
            ));
        }
        buffer.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            return Ok(buffer.len());
        }
    }
}

fn complete_ledger_json_line(line: &[u8]) -> Option<&[u8]> {
    line.strip_suffix(b"\n")
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
}

static LEDGER_CURSOR_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn validate_private_cursor_path(path: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "ledger cursor '{}' cannot be inspected: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "ledger cursor '{}' is not a regular non-symlink file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "ledger cursor '{}' is owned by another user",
                path.display()
            ));
        }
        if metadata.permissions().mode() & 0o777 != 0o600 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
                |error| {
                    format!(
                        "ledger cursor '{}' cannot be made owner-only: {error}",
                        path.display()
                    )
                },
            )?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LedgerIndexProgress {
    watermark: u64,
    durable_end_offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexedMovementRowRange {
    seq: u64,
    start_offset: u64,
    end_offset: u64,
}

struct LedgerSqliteReader {
    connection: Connection,
}

impl LedgerSqliteReader {
    fn open(path: &Path) -> Result<Self, String> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| format!("ledger spotlight cannot open: {error}"))?;
        let schema_version = required_index_meta_u64(&connection, "schema_version")?;
        if schema_version != LEDGER_INDEX_SCHEMA_VERSION {
            return Err(format!(
                "ledger spotlight schema generation {schema_version} does not match expected {LEDGER_INDEX_SCHEMA_VERSION}"
            ));
        }
        Ok(Self { connection })
    }

    fn progress(&self) -> Result<LedgerIndexProgress, String> {
        let (watermark, durable_end_offset, first_seq, last_seq, last_end_offset): (
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = self
            .connection
            .query_row(
                "SELECT
                    (SELECT value FROM index_meta WHERE key = 'watermark'),
                    (SELECT value FROM index_meta WHERE key = 'durable_end_offset'),
                    (SELECT seq FROM movements ORDER BY seq ASC LIMIT 1),
                    (SELECT seq FROM movements ORDER BY seq DESC LIMIT 1),
                    (SELECT end_offset FROM movements ORDER BY seq DESC LIMIT 1)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(|error| format!("ledger spotlight progress cannot be read: {error}"))?;
        let progress = LedgerIndexProgress {
            watermark: parse_index_meta_u64("watermark", watermark)?,
            durable_end_offset: parse_index_meta_u64("durable_end_offset", durable_end_offset)?,
        };
        let expected_first = (progress.watermark > 0).then_some(0_i64);
        let expected_last = progress
            .watermark
            .checked_sub(1)
            .map(|value| {
                i64::try_from(value).map_err(|_| {
                    format!(
                        "ledger spotlight watermark {} exceeds SQLite range",
                        progress.watermark
                    )
                })
            })
            .transpose()?;
        let expected_end = if progress.watermark == 0 {
            if progress.durable_end_offset != 0 {
                return Err(format!(
                    "empty ledger spotlight has durable end offset {}",
                    progress.durable_end_offset
                ));
            }
            None
        } else {
            Some(i64::try_from(progress.durable_end_offset).map_err(|_| {
                format!(
                    "ledger spotlight durable end offset {} exceeds SQLite range",
                    progress.durable_end_offset
                )
            })?)
        };
        if first_seq != expected_first
            || last_seq != expected_last
            || last_end_offset != expected_end
        {
            return Err(format!(
                "ledger spotlight watermark {} and durable end offset {} do not name one contiguous indexed boundary",
                progress.watermark, progress.durable_end_offset
            ));
        }
        Ok(progress)
    }

    fn bounded_interface_delivery_candidate(
        &self,
        interface: &str,
        after_seq: u64,
        before_seq: u64,
    ) -> Result<Option<IndexedMovementRowRange>, String> {
        if after_seq >= before_seq {
            return Ok(None);
        }
        let after_seq = sqlite_index_i64("after sequence", after_seq)?;
        let before_seq = sqlite_index_i64("before sequence", before_seq)?;
        let runtime = query_indexed_candidate(
            &self.connection,
            RUNTIME_INTERFACE_OUTBOUND_CANDIDATE_SQL,
            interface,
            after_seq,
            before_seq,
        )?;
        let endpoint = query_indexed_candidate(
            &self.connection,
            ENDPOINT_INTERFACE_OUTBOUND_CANDIDATE_SQL,
            interface,
            after_seq,
            before_seq,
        )?;
        match (runtime, endpoint) {
            (Some(left), Some(right)) if left.seq == right.seq => {
                if left != right {
                    return Err(format!(
                        "ledger spotlight movement {} has conflicting indexed byte ranges",
                        left.seq
                    ));
                }
                Ok(Some(left))
            }
            (Some(left), Some(right)) => Ok(Some(if left.seq < right.seq { left } else { right })),
            (Some(candidate), None) | (None, Some(candidate)) => Ok(Some(candidate)),
            (None, None) => Ok(None),
        }
    }
}

fn query_indexed_candidate(
    connection: &Connection,
    query: &str,
    interface: &str,
    after_seq: i64,
    before_seq: i64,
) -> Result<Option<IndexedMovementRowRange>, String> {
    let row = connection
        .query_row(query, params![interface, after_seq, before_seq], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .optional()
        .map_err(|error| format!("ledger spotlight candidate cannot be read: {error}"))?;
    let Some((seq, start_offset, end_offset)) = row else {
        return Ok(None);
    };
    let (Some(start_offset), Some(end_offset)) = (start_offset, end_offset) else {
        return Err("ledger spotlight candidate has no durable byte range".to_string());
    };
    if seq < 0 || start_offset < 0 || end_offset <= start_offset {
        return Err(format!(
            "ledger spotlight row {seq} has invalid durable byte range [{start_offset}, {end_offset})"
        ));
    }
    Ok(Some(IndexedMovementRowRange {
        seq: seq as u64,
        start_offset: start_offset as u64,
        end_offset: end_offset as u64,
    }))
}

fn sqlite_index_i64(label: &str, value: u64) -> Result<i64, String> {
    i64::try_from(value)
        .map_err(|_| format!("ledger spotlight {label} {value} exceeds SQLite range"))
}

fn required_index_meta_u64(connection: &Connection, key: &str) -> Result<u64, String> {
    let value: String = connection
        .query_row(
            "SELECT value FROM index_meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .map_err(|error| format!("ledger spotlight metadata '{key}' cannot be read: {error}"))?;
    parse_index_meta_u64(key, value)
}

fn parse_index_meta_u64(key: &str, value: String) -> Result<u64, String> {
    value.parse().map_err(|_| {
        format!("ledger spotlight metadata '{key}' is not an unsigned integer: '{value}'")
    })
}

pub struct LedgerTailReader {
    interface: InterfaceId,
    movements_path: PathBuf,
    decoder: DurableMovementDecoder,
    blobs_dir: PathBuf,
    host_io_limits: InterfaceHostIoLimits,
    cursor_path: PathBuf,
    cursor: TailCursor,
    pending_cursor: TailCursor,
    spotlight: Option<LedgerSqliteReader>,
    resume_without_wait: bool,
    foreign_catch_up_pending: bool,
    change_watch: Option<LedgerChangeWatch>,
}

#[cfg(not(target_os = "macos"))]
pub(super) struct LedgerChangeWatch {
    _watcher: notify::RecommendedWatcher,
    events: tokio::sync::mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
}

#[cfg(target_os = "macos")]
pub(super) struct LedgerChangeWatch {
    events: tokio::sync::mpsc::UnboundedReceiver<Result<(), String>>,
    queue: std::os::fd::RawFd,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
const KQUEUE_STOP_IDENT: libc::uintptr_t = 1;

#[cfg(target_os = "macos")]
impl Drop for LedgerChangeWatch {
    fn drop(&mut self) {
        let trigger = libc::kevent {
            ident: KQUEUE_STOP_IDENT,
            filter: libc::EVFILT_USER,
            flags: 0,
            fflags: libc::NOTE_TRIGGER,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        unsafe {
            libc::kevent(
                self.queue,
                &trigger,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            );
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl LedgerTailReader {
    pub fn open_named(ctx: &InterfaceLiftContext, reader: &str) -> Result<Self, String> {
        let host_io_limits = ctx.required_host_io_limits()?;
        Self::open_named_with_limits(ctx, reader, host_io_limits)
    }

    fn open_named_with_limits(
        ctx: &InterfaceLiftContext,
        reader: &str,
        host_io_limits: InterfaceHostIoLimits,
    ) -> Result<Self, String> {
        if !ctx.movements_path.is_absolute() || !ctx.blobs_dir.is_absolute() {
            return Err(format!(
                "interface '{}' ledger reader requires explicit ledger and blob read handles",
                ctx.interface.as_str()
            ));
        }
        validate_host_io_limits(host_io_limits)?;
        let var_dir = ctx.interface_var_dir();
        std::fs::create_dir_all(&var_dir).map_err(|e| {
            format!(
                "interface '{}' var dir '{}' cannot be created: {e}",
                ctx.interface.as_str(),
                var_dir.display()
            )
        })?;
        materialize_private_var_directories(&var_dir, &BTreeSet::new())?;
        let cursor_name = reader
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        if cursor_name.trim_matches('_').is_empty() {
            return Err("ledger reader id must contain an alphanumeric character".to_string());
        }
        let cursor_path = if reader == "default" {
            var_dir.join("ledger-cursor.json")
        } else {
            var_dir.join(format!("ledger-cursor.{cursor_name}.json"))
        };
        validate_private_cursor_path(&cursor_path)?;
        let cursor = match octopus_interfaces::read_artifact_carrier_file_bounded(
            &cursor_path,
            host_io_limits.max_control_frame_bytes,
        ) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                format!(
                    "interface '{}' ledger cursor '{}' is malformed: {error}",
                    ctx.interface.as_str(),
                    cursor_path.display()
                )
            })?,
            Err(InterfaceArtifactFileError::ReadPayload { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                TailCursor::default()
            }
            Err(error) => {
                return Err(format!(
                    "interface '{}' ledger cursor '{}' cannot be read: {e}",
                    ctx.interface.as_str(),
                    cursor_path.display(),
                    e = error
                ));
            }
        };
        Ok(Self {
            interface: ctx.interface.clone(),
            movements_path: ctx.movements_path.clone(),
            decoder: DurableMovementDecoder::for_log(&ctx.movements_path),
            blobs_dir: ctx.blobs_dir.clone(),
            host_io_limits,
            cursor_path,
            cursor,
            pending_cursor: cursor,
            spotlight: LedgerSqliteReader::open(&ctx.ledger_index_path).ok(),
            resume_without_wait: false,
            foreign_catch_up_pending: false,
            change_watch: None,
        })
    }

    pub fn next_seq(&self) -> u64 {
        self.cursor.seq
    }

    pub fn read_new_uncommitted_limit(
        &mut self,
        maximum_movements: usize,
    ) -> Result<Vec<OwnOutboundMovement>, String> {
        if maximum_movements == 0 {
            return Ok(Vec::new());
        }
        self.read_from(self.pending_cursor, maximum_movements, false)
    }

    pub fn read_new_uncommitted_with_refusals_limit(
        &mut self,
        maximum_movements: usize,
    ) -> Result<Vec<OwnOutboundMovement>, String> {
        if maximum_movements == 0 {
            return Ok(Vec::new());
        }
        self.read_from(self.pending_cursor, maximum_movements, true)
    }

    fn read_from(
        &mut self,
        start: TailCursor,
        maximum_movements: usize,
        include_refusals: bool,
    ) -> Result<Vec<OwnOutboundMovement>, String> {
        let file = match std::fs::File::open(&self.movements_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(format!(
                    "ledger '{}' cannot be opened: {error}",
                    self.movements_path.display()
                ));
            }
        };
        let mut reader = std::io::BufReader::new(file);
        let ledger_len = reader
            .get_ref()
            .metadata()
            .map_err(|error| format!("ledger metadata cannot be read: {error}"))?
            .len();
        if start.offset > ledger_len {
            return Err(format!(
                "ledger '{}' was truncated below committed cursor offset {}",
                self.movements_path.display(),
                start.offset
            ));
        }
        let interval_end = start
            .seq
            .saturating_add(AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET as u64);
        let indexed_candidate_seq = self.spotlight.as_ref().and_then(|spotlight| {
            let progress = spotlight.progress().ok()?;
            if progress.watermark <= start.seq
                || progress.durable_end_offset < start.offset
                || progress.durable_end_offset > ledger_len
            {
                return None;
            }
            spotlight
                .bounded_interface_delivery_candidate(
                    self.interface.as_str(),
                    start.seq,
                    interval_end.min(progress.watermark),
                )
                .ok()
                .flatten()
                .filter(|candidate| {
                    candidate.start_offset >= start.offset
                        && candidate.end_offset <= progress.durable_end_offset
                })
                .map(|candidate| candidate.seq)
        });
        reader
            .seek(std::io::SeekFrom::Start(start.offset))
            .map_err(|error| format!("ledger seek to {} failed: {error}", start.offset))?;

        let mut scanned = start;
        let mut scanned_rows = 0_usize;
        let mut incomplete_tail = false;
        let mut own = Vec::new();
        let mut line = Vec::new();
        while scanned_rows < AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET
            && scanned.offset < ledger_len
        {
            let row_start = scanned.offset;
            let read = read_bounded_ledger_line(
                &mut reader,
                &mut line,
                self.host_io_limits.max_control_frame_bytes,
            )?;
            if read == 0 {
                break;
            }
            let Some(json_line) = complete_ledger_json_line(&line) else {
                incomplete_tail = true;
                break;
            };
            if row_start.saturating_add(read as u64) > ledger_len {
                incomplete_tail = true;
                break;
            }
            let route = decode_durable_movement_route(json_line, scanned.seq as usize).map_err(
                |error| {
                    format!(
                        "ledger row at offset {} is not a movement (fail-closed): {error}",
                        scanned.offset
                    )
                },
            )?;
            if route.seq.0 != scanned.seq {
                return Err(format!(
                    "ledger row at offset {} carries seq {}, expected {}",
                    scanned.offset, route.seq.0, scanned.seq
                ));
            }
            scanned.offset = scanned
                .offset
                .checked_add(read as u64)
                .ok_or_else(|| "ledger cursor offset overflowed".to_string())?;
            scanned.seq = scanned
                .seq
                .checked_add(1)
                .ok_or_else(|| "ledger cursor sequence overflowed".to_string())?;
            scanned_rows += 1;
            let addressed_here = route.kind == MovementKind::InterfaceOutbound
                && matches!(&route.from, Endpoint::Interface(_))
                && matches!(&route.to, Endpoint::Interface(target) if target == &self.interface);
            let refused_here = include_refusals
                && route.kind == MovementKind::DeliveryRefused
                && matches!(&route.from, Endpoint::Interface(source) if source == &self.interface);
            if !addressed_here && !refused_here {
                continue;
            }
            let movement = self
                .decoder
                .decode(json_line, route.seq.0 as usize)
                .map_err(|error| {
                    format!(
                        "ledger row at offset {row_start} addressed to interface '{}' is invalid: {error}",
                        self.interface.as_str()
                    )
                })?;
            let movement_seq = movement.seq.0;
            own.push(OwnOutboundMovement {
                metadata: strict_interface_outbound_metadata(&movement, &self.interface),
                movement,
                cursor: scanned,
            });
            if own.len() == maximum_movements
                || indexed_candidate_seq.is_some_and(|candidate| movement_seq <= candidate)
            {
                break;
            }
        }
        self.resume_without_wait = !incomplete_tail && scanned.offset < ledger_len;
        self.pending_cursor = scanned;
        Ok(own)
    }

    pub async fn wait_for_change(&mut self, cancel: &CancellationToken) -> Result<(), String> {
        if std::mem::take(&mut self.resume_without_wait) {
            tokio::task::yield_now().await;
            return Ok(());
        }
        if self.change_watch.is_none() {
            self.change_watch = Some(ledger_change_watch(&self.movements_path)?);
            return Ok(());
        }
        self.change_watch
            .as_mut()
            .expect("ledger change watch installed above")
            .wait(cancel, &self.movements_path)
            .await
    }

    pub(crate) fn advance_scanned(&mut self) -> Result<(), String> {
        if self.resume_without_wait {
            if self.cursor != self.pending_cursor {
                self.foreign_catch_up_pending = true;
            }
        } else if self.foreign_catch_up_pending {
            self.persist_cursor(&self.pending_cursor)?;
            self.foreign_catch_up_pending = false;
        }
        self.cursor = self.pending_cursor;
        Ok(())
    }

    pub fn commit_through(&mut self, own: &OwnOutboundMovement) -> Result<(), String> {
        if own.cursor.seq < self.cursor.seq || own.cursor.offset < self.cursor.offset {
            return Err(format!(
                "cannot move interface '{}' cursor backward from seq {} offset {} to seq {} offset {}",
                self.interface.as_str(),
                self.cursor.seq,
                self.cursor.offset,
                own.cursor.seq,
                own.cursor.offset
            ));
        }
        self.persist_cursor(&own.cursor)?;
        self.cursor = own.cursor;
        self.foreign_catch_up_pending = false;
        Ok(())
    }

    fn persist_cursor(&self, cursor: &TailCursor) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(cursor).expect("tail cursor is always serializable");
        if bytes.len() as u64 > self.host_io_limits.max_control_frame_bytes {
            return Err(format!(
                "cursor '{}' exceeds declared interface ceiling of {} bytes",
                self.cursor_path.display(),
                self.host_io_limits.max_control_frame_bytes
            ));
        }
        let parent = self.cursor_path.parent().ok_or_else(|| {
            format!(
                "cursor '{}' has no parent directory",
                self.cursor_path.display()
            )
        })?;
        let sequence = LEDGER_CURSOR_TEMP_SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| "ledger cursor temp sequence overflowed".to_string())?;
        let name = self
            .cursor_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                format!(
                    "cursor '{}' has no UTF-8 file name",
                    self.cursor_path.display()
                )
            })?;
        let tmp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&tmp).map_err(|error| {
            format!("cursor temp '{}' cannot be created: {error}", tmp.display())
        })?;
        let result = (|| -> Result<(), String> {
            use std::io::Write;
            file.write_all(&bytes).map_err(|error| {
                format!("cursor temp '{}' cannot be written: {error}", tmp.display())
            })?;
            file.sync_all().map_err(|error| {
                format!("cursor temp '{}' cannot be synced: {error}", tmp.display())
            })?;
            std::fs::rename(&tmp, &self.cursor_path).map_err(|error| {
                format!(
                    "cursor rename '{}' failed: {error}",
                    self.cursor_path.display()
                )
            })?;
            let directory = std::fs::File::open(parent).map_err(|error| {
                format!(
                    "cursor directory '{}' cannot be opened: {error}",
                    parent.display()
                )
            })?;
            directory.sync_all().map_err(|error| {
                format!(
                    "cursor directory '{}' cannot be synced: {error}",
                    parent.display()
                )
            })?;
            validate_private_cursor_path(&self.cursor_path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    pub fn envelope(
        &self,
        own: &OwnOutboundMovement,
    ) -> Result<InterfaceInvocationEnvelope, String> {
        own.metadata()?;
        self.envelope_for_movement(&own.movement)
    }

    pub fn envelope_for_movement(
        &self,
        movement: &Movement,
    ) -> Result<InterfaceInvocationEnvelope, String> {
        let metadata = strict_interface_outbound_metadata(movement, &self.interface)?;
        let artifact = movement.artifact.clone().ok_or_else(|| {
            format!(
                "outbound movement seq {} for interface '{}' carries no artifact",
                movement.seq.0,
                self.interface.as_str()
            )
        })?;
        let manifest_path = self.blobs_dir.join(artifact.content.to_hex());
        let artifact_file = InterfaceArtifactFile::new(&manifest_path)
            .map_err(|e| {
                format!(
                    "outbound artifact manifest blob '{}' cannot be opened as a manifest: {e}",
                    manifest_path.display()
                )
            })?
            .with_payload_cache_root(&self.blobs_dir);
        let captured = artifact_file
            .read_bounded(
                self.host_io_limits.max_manifest_bytes,
                self.host_io_limits.max_payload_bytes,
            )
            .map_err(|error| {
                format!(
                    "outbound artifact manifest blob '{}' cannot be captured: {error}",
                    manifest_path.display()
                )
            })?;
        let artifact_file = InterfaceArtifactFile::from_captured_snapshot(
            manifest_path.clone(),
            captured.manifest_bytes,
            captured.payload_bytes,
        )
        .map_err(|error| {
            format!(
                "outbound artifact manifest blob '{}' cannot be pinned as a captured carrier: {error}",
                manifest_path.display()
            )
        })?;
        let mut remaining = self.host_io_limits.max_payload_bytes;
        let mut artifact_binding_payloads: ArtifactBindingPayloadSet = Vec::new();
        for binding_ref in &metadata.artifact_binding_refs {
            let binding_blob_path = self.blobs_dir.join(binding_ref.content.to_hex());
            artifact_binding_payloads.push(resolve_artifact_binding_payload(
                &binding_blob_path, binding_ref, self.host_io_limits, &mut remaining,
            )?);
        }
        Ok(InterfaceInvocationEnvelope {
            artifact,
            artifact_file,
            artifact_binding_payloads,
            metadata: Some(metadata.clone()),
        })
    }
}

fn strict_interface_outbound_metadata(
    movement: &Movement,
    interface: &InterfaceId,
) -> Result<InvocationMetadata, String> {
    if movement.kind != MovementKind::InterfaceOutbound
        || !matches!(&movement.from, Endpoint::Interface(_))
        || !matches!(&movement.to, Endpoint::Interface(target) if target == interface)
    {
        return Err(format!(
            "movement seq {} is not outbound to interface '{}'",
            movement.seq.0,
            interface.as_str()
        ));
    }
    let metadata = movement
        .invocation_metadata
        .as_ref()
        .and_then(|metadata| metadata.invocation())
        .cloned()
        .ok_or_else(|| {
            format!(
                "outbound movement seq {} for interface '{}' carries no invocation metadata",
                movement.seq.0,
                interface.as_str()
            )
        })?;
    let runtime = metadata.runtime().ok_or_else(|| {
        format!(
            "outbound movement seq {} for interface '{}' carries no runtime coordinates",
            movement.seq.0,
            interface.as_str()
        )
    })?;
    if &runtime.interface != interface {
        return Err(format!(
            "outbound movement seq {} for interface '{}' carries mismatched runtime coordinates",
            movement.seq.0,
            interface.as_str()
        ));
    }
    Ok(metadata)
}

#[cfg(not(target_os = "macos"))]
impl LedgerChangeWatch {
    pub(super) async fn wait(&mut self, cancel: &CancellationToken, path: &Path) -> Result<(), String> {
        let event = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            event = self.events.recv() => event,
        };
        match event {
            Some(Ok(_)) => Ok(()),
            Some(Err(error)) => Err(format!(
                "ledger '{}' filesystem watcher failed: {error}",
                path.display()
            )),
            None => Err(format!(
                "ledger '{}' filesystem watcher channel closed",
                path.display()
            )),
        }
    }
}

#[cfg(target_os = "macos")]
impl LedgerChangeWatch {
    pub(super) async fn wait(&mut self, cancel: &CancellationToken, path: &Path) -> Result<(), String> {
        let event = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            event = self.events.recv() => event,
        };
        match event {
            Some(result) => result,
            None => Err(format!(
                "ledger '{}' kqueue watcher channel closed",
                path.display()
            )),
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn ledger_change_watch(path: &Path) -> Result<LedgerChangeWatch, String> {
    if !path.is_file() {
        return Err(format!(
            "ledger '{}' must exist before an interface reader starts",
            path.display()
        ));
    }
    let (tx, events) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    })
    .map_err(|error| {
        format!(
            "ledger '{}' filesystem watcher cannot be created: {error}",
            path.display()
        )
    })?;
    watcher
        .watch(path, RecursiveMode::NonRecursive)
        .map_err(|error| {
            format!(
                "ledger file '{}' cannot be watched: {error}",
                path.display()
            )
        })?;
    Ok(LedgerChangeWatch {
        _watcher: watcher,
        events,
    })
}

#[cfg(target_os = "macos")]
pub(super) fn ledger_change_watch(path: &Path) -> Result<LedgerChangeWatch, String> {
    use std::os::fd::AsRawFd;

    let file = std::fs::File::open(path).map_err(|error| {
        format!(
            "ledger '{}' cannot be opened for kqueue: {error}",
            path.display()
        )
    })?;
    let queue = unsafe { libc::kqueue() };
    if queue < 0 {
        return Err(format!(
            "ledger '{}' kqueue cannot be created: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let vnode_event = libc::kevent {
        ident: file.as_raw_fd() as libc::uintptr_t,
        filter: libc::EVFILT_VNODE,
        flags: libc::EV_ADD | libc::EV_CLEAR,
        fflags: libc::NOTE_WRITE
            | libc::NOTE_EXTEND
            | libc::NOTE_ATTRIB
            | libc::NOTE_RENAME
            | libc::NOTE_DELETE
            | libc::NOTE_REVOKE,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let stop_event = libc::kevent {
        ident: KQUEUE_STOP_IDENT,
        filter: libc::EVFILT_USER,
        flags: libc::EV_ADD | libc::EV_CLEAR,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let changes = [vnode_event, stop_event];
    let result = unsafe {
        libc::kevent(
            queue,
            changes.as_ptr(),
            changes.len() as libc::c_int,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if result < 0 {
        return Err(format!(
            "ledger '{}' kqueue watch cannot be registered: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let (tx, events) = tokio::sync::mpsc::unbounded_channel();
    let thread_path = path.to_path_buf();
    let thread = std::thread::Builder::new()
        .name("octopus-ledger-watch".to_string())
        .spawn(move || {
            let _file = file;
            loop {
                let mut event = std::mem::MaybeUninit::<libc::kevent>::uninit();
                let count = unsafe {
                    libc::kevent(
                        queue,
                        std::ptr::null(),
                        0,
                        event.as_mut_ptr(),
                        1,
                        std::ptr::null(),
                    )
                };
                if count < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    let _ = tx.send(Err(format!(
                        "ledger '{}' kqueue read failed: {error}",
                        thread_path.display()
                    )));
                    break;
                }
                if count == 0 {
                    continue;
                }
                let event = unsafe { event.assume_init() };
                if event.filter == libc::EVFILT_USER && event.ident == KQUEUE_STOP_IDENT {
                    break;
                }
                if event.flags & libc::EV_ERROR != 0 {
                    let _ = tx.send(Err(format!(
                        "ledger '{}' kqueue event failed: {}",
                        thread_path.display(),
                        std::io::Error::from_raw_os_error(event.data as i32)
                    )));
                    break;
                }
                if tx.send(Ok(())).is_err() {
                    break;
                }
            }
            unsafe {
                libc::close(queue);
            }
        })
        .map_err(|error| {
            unsafe {
                libc::close(queue);
            }
            format!(
                "ledger '{}' kqueue watcher thread cannot start: {error}",
                path.display()
            )
        })?;
    Ok(LedgerChangeWatch {
        events,
        queue,
        thread: Some(thread),
    })
}

pub(crate) fn resolve_artifact_binding_payload(
    blob_path: &Path,
    reference: &ArtifactRef,
    limits: InterfaceHostIoLimits,
    remaining: &mut u64,
) -> Result<ArtifactBindingPayload, String> {
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
        blob_path, limits.max_manifest_bytes.max(limits.max_payload_bytes.min(*remaining)),
    ).map_err(|error| error.to_string())?;
    if ContentHash::of_bytes(&bytes) != reference.content {
        return Err("artifact-binding carrier differs from its exact admitted reference".into());
    }
    let cache_root = blob_path.parent().ok_or("artifact-binding blob has no cache root")?
        .canonicalize().map_err(|error| error.to_string())?;
    match serde_json::from_slice::<InterfaceArtifactManifest>(&bytes) {
        Ok(manifest) if !manifest.payload_path.as_os_str().is_empty() => {
            if bytes.len() as u64 > limits.max_manifest_bytes {
                return Err("artifact-binding manifest exceeds its declared ceiling".into());
            }
            if manifest.label.trim().is_empty() || !manifest.payload_path.is_absolute() {
                return Err("artifact-binding manifest has an empty label or non-absolute payload path".into());
            }
            match manifest.payload_content_hash.as_deref() {
                Some(hash) => {
                    let hash = ContentHash::from_hex(hash)
                        .ok_or("artifact-binding manifest has an invalid payload hash")?;
                    ArtifactBindingPayload::from_pinned_file(
                        reference.clone(), cache_root.join(hash.to_hex()), hash, limits.max_payload_bytes,
                    ).map_err(|error| error.to_string())
                }
                None => {
                    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
                        &manifest.payload_path, limits.max_payload_bytes.min(*remaining),
                    ).map_err(|error| error.to_string())?;
                    *remaining -= bytes.len() as u64;
                    Ok(ArtifactBindingPayload::new(reference.clone(), bytes))
                }
            }
        }
        _ => ArtifactBindingPayload::from_pinned_file(
            reference.clone(), cache_root.join(reference.content.to_hex()),
            reference.content, limits.max_payload_bytes,
        ).map_err(|error| error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET, DurableMovementDecoder, InterfaceHostIoLimits,
        InterfaceLiftContext, LedgerSqliteReader, LedgerTailReader, PendingVerdicts, TailCursor,
        SpawnedLifeInput, SpawnedLifeOutput, accept_spawned_life_input, spawned_arrival_output,
        spawned_system_emission_output,
    };
    use crate::{DeliveryVerdict, SystemInterfaceEmission, SystemInterfaceEmissionArtifact};
    use octopus_descriptors::{CheckpointId, EeId, InterfaceId, RunId};
    use octopus_interfaces::{
        GrantProjection, InterfaceArtifactFile, InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{
        ContentHash, Endpoint, Lamport, Movement, MovementKind, Seq, TraceId,
        encode_durable_movement_row,
    };
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn outbound(seq: u64, from: Endpoint, target: &str, attested_interface: &str) -> Movement {
        Movement {
            seq: Seq(seq),
            lamport: Lamport(seq),
            recorded_at_unix_ms: 0,
            trace: TraceId("trace-outbound".to_string()),
            kind: MovementKind::InterfaceOutbound,
            from,
            to: Endpoint::Interface(InterfaceId::new(target)),
            artifact: None,
            invocation_metadata: Some(
                InvocationMetadata::new(GrantProjection::default())
                    .with_runtime(RuntimeInvocationMetadata {
                        run: RunId::new("run-outbound"),
                        ee: EeId::new("ee-outbound"),
                        worker: InterfaceId::new("system_worker_coder_max"),
                        checkpoint: CheckpointId::new("checkpoint-outbound"),
                        interface: InterfaceId::new(attested_interface),
                        epoch: 0,
                        run_record_hash: None,
                    })
                    .into(),
            ),
        }
    }

    fn filler(seq: u64) -> Movement {
        Movement {
            seq: Seq(seq),
            lamport: Lamport(seq),
            recorded_at_unix_ms: 0,
            trace: TraceId("trace-filler".to_string()),
            kind: MovementKind::VerificationFailed,
            from: Endpoint::Interface(InterfaceId::new("system_source")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_other")),
            artifact: None,
            invocation_metadata: None,
        }
    }

    fn write_ledger(root: &Path, movements: &[Movement]) -> (PathBuf, Vec<(u64, u64)>) {
        let rows = movements
            .iter()
            .map(|movement| format!("{}\n", serde_json::to_string(movement).unwrap()))
            .collect::<Vec<_>>();
        let mut offset = 0_u64;
        let ranges = rows
            .iter()
            .map(|row| {
                let start = offset;
                offset += row.len() as u64;
                (start, offset)
            })
            .collect::<Vec<_>>();
        let path = root.join("movements.jsonl");
        std::fs::write(&path, rows.concat()).unwrap();
        (path, ranges)
    }

    fn limits() -> InterfaceHostIoLimits {
        InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        }
    }

    fn test_reader(
        root: &Path,
        interface: &InterfaceId,
        movements_path: &Path,
        index_path: Option<&Path>,
        cursor_path: PathBuf,
        cursor: TailCursor,
    ) -> LedgerTailReader {
        LedgerTailReader {
            interface: interface.clone(),
            decoder: DurableMovementDecoder::for_log(movements_path),
            movements_path: movements_path.to_path_buf(),
            blobs_dir: root.join("blobs"),
            host_io_limits: limits(),
            cursor_path,
            cursor,
            pending_cursor: cursor,
            spotlight: index_path.and_then(|path| LedgerSqliteReader::open(path).ok()),
            resume_without_wait: false,
            foreign_catch_up_pending: false,
            change_watch: None,
        }
    }

    fn index_ledger(path: &Path, movements: &[Movement], ranges: &[(u64, u64)]) {
        let mut connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE index_meta (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                );
                CREATE TABLE movements (
                    seq INTEGER PRIMARY KEY NOT NULL,
                    kind TEXT NOT NULL,
                    from_kind TEXT NOT NULL,
                    to_kind TEXT NOT NULL,
                    to_id TEXT NOT NULL,
                    runtime_interface TEXT,
                    start_offset INTEGER,
                    end_offset INTEGER
                );
                CREATE INDEX idx_mv_runtime_interface_outbound
                    ON movements(runtime_interface, kind, from_kind, seq);
                CREATE INDEX idx_mv_interface_outbound_to
                    ON movements(kind, from_kind, to_kind, to_id, seq);",
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO index_meta(key, value) VALUES('schema_version', '4')",
                [],
            )
            .unwrap();
        transaction
            .execute(
                "INSERT INTO index_meta(key, value) VALUES('watermark', ?1)",
                [movements.len().to_string()],
            )
            .unwrap();
        transaction
            .execute(
                "INSERT INTO index_meta(key, value) VALUES('durable_end_offset', ?1)",
                [ranges.last().map_or(0, |range| range.1).to_string()],
            )
            .unwrap();
        for (movement, (start_offset, end_offset)) in movements.iter().zip(ranges) {
            let kind = match &movement.kind {
                MovementKind::InterfaceOutbound => "InterfaceOutbound".to_string(),
                other => match serde_json::to_value(other).unwrap() {
                    serde_json::Value::String(value) => value,
                    serde_json::Value::Object(value) => value.into_iter().next().unwrap().0,
                    value => panic!("movement kind has unsupported shape: {value}"),
                },
            };
            let (from_kind, _) = endpoint_parts(&movement.from);
            let (to_kind, to_id) = endpoint_parts(&movement.to);
            let runtime_interface = movement
                .invocation_metadata
                .as_ref()
                .and_then(|metadata| metadata.invocation())
                .and_then(|metadata| metadata.runtime())
                .filter(|runtime| {
                    movement.kind == MovementKind::InterfaceOutbound
                        && matches!(&movement.from, Endpoint::Interface(_))
                        && matches!(&movement.to, Endpoint::Interface(interface) if interface == &runtime.interface)
                })
                .map(|runtime| runtime.interface.as_str().to_string());
            transaction
                .execute(
                    "INSERT INTO movements(
                        seq, kind, from_kind, to_kind, to_id, runtime_interface,
                        start_offset, end_offset
                    ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    rusqlite::params![
                        movement.seq.0 as i64,
                        kind,
                        from_kind,
                        to_kind,
                        to_id,
                        runtime_interface,
                        *start_offset as i64,
                        *end_offset as i64
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
    }

    fn endpoint_parts(endpoint: &Endpoint) -> (&'static str, String) {
        match endpoint {
            Endpoint::Interface(id) => ("interface", id.as_str().to_string()),
        }
    }

    #[test]
    fn kind_owned_decoder_resolves_and_checks_content_addressed_metadata() {
        let root = tempfile::tempdir().unwrap();
        let movements_path = root.path().join("movements.jsonl");
        let metadata_dir = root.path().join("metadata");
        std::fs::create_dir(&metadata_dir).unwrap();
        let mut movement = outbound(
            0,
            Endpoint::Interface(InterfaceId::new("world_provider_codex")),
            "system_exec",
            "system_exec",
        );
        let Some(octopus_ledger::MovementMetadata::Invocation(metadata)) = movement.invocation_metadata.as_mut() else { panic!("invocation") };
        metadata.artifact_binding_refs = vec![
            octopus_ledger::ArtifactRef { id: octopus_descriptors::ArtifactId::new("bound"), content: ContentHash::of_bytes(b"bound") }
        ];
        let mut pinned = None;
        let row = encode_durable_movement_row(&movement, 0, |_, bytes| {
            let hash = ContentHash::of_bytes(bytes);
            std::fs::write(metadata_dir.join(hash.to_hex()), bytes).unwrap();
            pinned = Some(hash);
            Ok(hash)
        })
        .unwrap();
        let decoder = DurableMovementDecoder::for_log(&movements_path);
        let decoded = decoder.decode(row.as_bytes(), 0).unwrap();
        assert_eq!(decoded, movement);
        assert_eq!(decoder.decode(row.as_bytes(), 0).unwrap(), movement);

        let hash = pinned.unwrap();
        std::fs::write(metadata_dir.join(hash.to_hex()), b"corrupt").unwrap();
        assert!(decoder.decode(row.as_bytes(), 0).is_err());
    }

    #[tokio::test]
    async fn room_protocol_read_preserves_cancelled_fragments_and_ceiling() {
        use std::future::Future;
        use std::task::Poll;
        use tokio::io::AsyncWriteExt;

        let (mut writer, reader) = tokio::io::duplex(32);
        let mut lines = super::BoundedJsonLineReader::new(reader, 8).unwrap();
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
        writer.write_all(b"123456789\n").await.unwrap();
        let error = lines.next_line().await.unwrap_err();
        assert!(error.contains("exceeds declared ceiling"));
        assert_eq!(lines.next_line().await.unwrap_err(), error);
    }

    #[test]
    fn outbound_reader_accepts_only_exact_interface_provenance() {
        let root = tempfile::tempdir().unwrap();
        let door = InterfaceId::new("system_exec");
        let mut missing_metadata = outbound(
            2,
            Endpoint::Interface(InterfaceId::new("world_provider_codex")),
            door.as_str(),
            door.as_str(),
        );
        missing_metadata.invocation_metadata = None;
        let movements = [
            outbound(
                0,
                Endpoint::Interface(InterfaceId::new("world_provider_codex")),
                "system_other",
                door.as_str(),
            ),
            outbound(
                1,
                Endpoint::Interface(InterfaceId::new("world_provider_codex")),
                door.as_str(),
                "system_other",
            ),
            missing_metadata,
            outbound(
                3,
                Endpoint::Interface(InterfaceId::new("world_provider_codex")),
                "system_other",
                door.as_str(),
            ),
            outbound(
                4,
                Endpoint::Interface(InterfaceId::new("world_provider_codex")),
                door.as_str(),
                door.as_str(),
            ),
        ];
        let (movements_path, _) = write_ledger(root.path(), &movements);
        let cursor_path = root.path().join("cursor.json");
        let mut reader = test_reader(
            root.path(),
            &door,
            &movements_path,
            None,
            cursor_path.clone(),
            TailCursor::default(),
        );
        let accepted = reader.read_new_uncommitted_limit(10).unwrap();
        assert_eq!(
            accepted
                .iter()
                .map(|movement| movement.movement.seq.0)
                .collect::<Vec<_>>(),
            vec![1, 2, 4]
        );
        assert!(accepted[0].metadata().is_err());
        assert!(accepted[1].metadata().is_err());
        assert_eq!(
            accepted[2].metadata().unwrap().runtime().unwrap().interface,
            door
        );
        reader.advance_scanned().unwrap();
        assert_eq!(reader.next_seq(), 5);
        assert!(!cursor_path.exists());
    }

    #[test]
    fn outbound_reader_recovers_the_exact_torn_21971_boundary() {
        let root = tempfile::tempdir().unwrap();
        let door = InterfaceId::new("system_room_authoring");
        let movements = (0..=21972)
            .map(|seq| {
                if seq == 21971 {
                    outbound(
                        seq,
                        Endpoint::Interface(InterfaceId::new("system_test_authoring")),
                        door.as_str(),
                        door.as_str(),
                    )
                } else {
                    filler(seq)
                }
            })
            .collect::<Vec<_>>();
        let (movements_path, ranges) = write_ledger(root.path(), &movements);
        let index_path = root.path().join("ledger-index.sqlite");
        index_ledger(&index_path, &movements, &ranges);
        let connection = rusqlite::Connection::open(&index_path).unwrap();
        connection
            .execute(
                "UPDATE index_meta SET value = ?1 WHERE key = 'watermark'",
                ["21971"],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE index_meta SET value = ?1 WHERE key = 'durable_end_offset'",
                [ranges[21971].1.to_string()],
            )
            .unwrap();
        drop(connection);
        assert!(
            LedgerSqliteReader::open(&index_path)
                .unwrap()
                .progress()
                .is_err()
        );

        let cursor = TailCursor {
            seq: 21971,
            offset: ranges[21971].0,
        };
        let cursor_path = root.path().join("cursor.json");
        let mut reader = test_reader(
            root.path(),
            &door,
            &movements_path,
            Some(&index_path),
            cursor_path.clone(),
            cursor,
        );
        let accepted = reader.read_new_uncommitted_limit(10).unwrap();
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].movement.seq, Seq(21971));
        assert_eq!(
            accepted[0].cursor,
            TailCursor {
                seq: 21972,
                offset: ranges[21971].1,
            }
        );
        reader.commit_through(&accepted[0]).unwrap();
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cursor_path).unwrap()).unwrap();
        assert_eq!(
            persisted,
            serde_json::json!({ "seq": 21972, "offset": ranges[21971].1 })
        );
    }

    #[test]
    fn outbound_reader_bounds_double_corrupt_cache_resume_and_restart() {
        let root = tempfile::tempdir().unwrap();
        let door = InterfaceId::new("system_test_room");
        let target = AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET as u64 * 3 + 17;
        let movements = (0..=target)
            .map(|seq| {
                if seq == target {
                    outbound(
                        seq,
                        Endpoint::Interface(InterfaceId::new("system_test_authoring")),
                        door.as_str(),
                        door.as_str(),
                    )
                } else {
                    filler(seq)
                }
            })
            .collect::<Vec<_>>();
        let (movements_path, ranges) = write_ledger(root.path(), &movements);
        let index_path = root.path().join("ledger-index.sqlite");
        index_ledger(&index_path, &movements, &ranges);
        let connection = rusqlite::Connection::open(&index_path).unwrap();
        connection
            .execute(
                "UPDATE movements
                 SET to_id = 'system_other', runtime_interface = 'system_other'
                 WHERE seq = ?1",
                [i64::try_from(target).unwrap()],
            )
            .unwrap();
        drop(connection);
        assert!(
            LedgerSqliteReader::open(&index_path)
                .unwrap()
                .bounded_interface_delivery_candidate(door.as_str(), 0, target + 1)
                .unwrap()
                .is_none()
        );

        let cursor_path = root.path().join("cursor.json");
        let mut reader = test_reader(
            root.path(),
            &door,
            &movements_path,
            Some(&index_path),
            cursor_path.clone(),
            TailCursor::default(),
        );
        for _ in 0..3 {
            let before = reader.pending_cursor.seq;
            assert!(reader.read_new_uncommitted_limit(16).unwrap().is_empty());
            assert_eq!(
                reader.pending_cursor.seq - before,
                AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET as u64
            );
            assert!(reader.resume_without_wait);
            reader.advance_scanned().unwrap();
            assert!(!cursor_path.exists());
        }
        let accepted = reader.read_new_uncommitted_limit(16).unwrap();
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].movement.seq, Seq(target));
        reader.commit_through(&accepted[0]).unwrap();
        drop(reader);

        let persisted: TailCursor =
            serde_json::from_slice(&std::fs::read(&cursor_path).unwrap()).unwrap();
        let mut restarted = test_reader(
            root.path(),
            &door,
            &movements_path,
            Some(&index_path),
            cursor_path,
            persisted,
        );
        assert!(restarted.read_new_uncommitted_limit(16).unwrap().is_empty());
        assert_eq!(restarted.next_seq(), target + 1);
    }

    #[test]
    fn decoded_tool_failure_preserves_result_identity() {
        let root = tempfile::tempdir().unwrap();
        let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
        drop(receiver);
        let context = InterfaceLiftContext::unprivileged(
            InterfaceId::new("system_exec_env"),
            root.path().join("interface"),
            None,
            root.path().join("var"),
            root.path().join("movements.jsonl"),
            root.path().join("blobs"),
            root.path().join("ledger-index.sqlite"),
            BTreeMap::new(),
            arrivals,
            CancellationToken::new(),
        );
        let artifact = context
            .tool_result_failure_artifact(
                "exec.run",
                Some("call-1"),
                octopus_tool_handlers::ToolResultError::new(
                    "execution_refused",
                    "execution refused",
                ),
            )
            .unwrap();
        assert_eq!(artifact.read_manifest().unwrap().label, "exec.run.result");
        let payload: serde_json::Value =
            serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(payload["label"], "exec.run.result");
        assert_eq!(payload["tool"], "exec.run");
        assert_eq!(payload["correlation_id"], "call-1");
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["error"]["code"], "execution_refused");
        assert_eq!(payload["error"]["message"], "execution refused");
    }

    #[tokio::test]
    async fn materialized_emissions_keep_only_exact_private_carriers() {
        let root = tempfile::tempdir().unwrap();
        let var = root.path().join("var");
        let hand_in = root.path().join("hand-in");
        let (arrivals, _) = tokio::sync::mpsc::channel(1);
        let limits = InterfaceHostIoLimits {
            max_control_frame_bytes: 65536, max_stderr_bytes: 65536,
            max_manifest_bytes: 65536, max_payload_bytes: 65536,
        };
        let mut ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("door"), root.path().join("interface"), None, var.clone(),
            root.path().join("movements.jsonl"), root.path().join("blobs"),
            root.path().join("ledger-index.sqlite"),
            BTreeMap::from([("interface_hand_in_root".into(), hand_in.clone())]),
            arrivals, CancellationToken::new(),
        );
        ctx.host_io_limits = Some(limits);
        let (emissions, mut received) = tokio::sync::mpsc::channel(1);
        ctx.emissions = emissions;
        for output in [var.join(".tool-process-host-state/generations/g/state/invocations/1/artifact/emitted"),
            var.join(".tool-process-host-state/worker-environment-state/w/authoring-artifacts"),
            hand_in.join("door")]
        {
            let artifact = InterfaceArtifactFile::write(output, "test.output", b"payload").unwrap();
            let original = artifact.manifest_path().to_path_buf();
            let send = ctx.hand_system_emission(SystemInterfaceEmissionArtifact::Materialized(artifact), 17, InterfaceId::new("target"));
            let host = async {
                let emission = received.recv().await.unwrap();
                let (SystemInterfaceEmissionArtifact::Materialized(artifact), seq, target, verdict) = emission.into_parts()
                    else { panic!("materialized emission changed kind") };
                assert_eq!(seq, 17); assert_eq!(target.as_str(), "target");
                assert_eq!(artifact.manifest_path(), original);
                let recaptured = super::capture_spawned_artifact(artifact.manifest_path(), &[var.clone(), hand_in.join("door")], limits).unwrap();
                assert_eq!(recaptured.read_payload_bytes().unwrap(), b"payload");
                verdict.send(DeliveryVerdict::Admitted { seq: 18 }).unwrap();
            };
            let (verdict, ()) = tokio::join!(send, host);
            assert!(matches!(verdict.unwrap(), DeliveryVerdict::Admitted { seq: 18 }));
        }
        assert!(!var.join("emitted").exists());
        let outside = InterfaceArtifactFile::write(root.path().join("blobs"), "outside", b"payload").unwrap();
        assert!(ctx.hand_system_emission(SystemInterfaceEmissionArtifact::Materialized(outside.clone()), 17, InterfaceId::new("target")).await.is_err());
        let manifest = std::fs::read(outside.manifest_path()).unwrap();
        let alias = InterfaceArtifactFile::store_manifest_bounded(var.join("alias"), &manifest, 65536, 65536).unwrap();
        assert!(ctx.hand_system_emission(SystemInterfaceEmissionArtifact::Materialized(alias), 17, InterfaceId::new("target")).await.is_err());
        let private = InterfaceArtifactFile::write(var.join("private"), "private", b"payload").unwrap()
            .capture_bounded(65536, 65536).unwrap().rewrite_payload(b"changed capture").unwrap();
        assert!(ctx.hand_system_emission(SystemInterfaceEmissionArtifact::Materialized(private), 17, InterfaceId::new("target")).await.is_err());
    }

    #[tokio::test]
    async fn spawned_system_emission_carries_attested_coordinates_and_correlates_verdict() {
        let root = tempfile::tempdir().unwrap();
        let artifact =
            InterfaceArtifactFile::write(root.path(), "test.output", b"payload").unwrap();
        let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
        let emission = SystemInterfaceEmission::host_attested(
            SystemInterfaceEmissionArtifact::Materialized(artifact),
            17,
            InterfaceId::new("system_worker_planner"),
            verdict_tx,
        );
        let pending: PendingVerdicts = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
        let mut next_request_id = 1;
        let output = spawned_system_emission_output(emission, &pending, &mut next_request_id)
            .await
            .unwrap();
        let encoded = serde_json::to_value(output).unwrap();
        assert_eq!(encoded["kind"], "system_emission");
        assert_eq!(encoded["request_id"], 1);
        assert_eq!(encoded["source_movement_seq"], 17);
        assert_eq!(encoded["to_interface"], "system_worker_planner");
        assert!(encoded.get("from_interface").is_none());
        assert!(encoded.get("emitter_run").is_none());
        assert!(encoded.get("emitter_checkpoint").is_none());
        accept_spawned_life_input(&pending, SpawnedLifeInput::DeliveryVerdict {
            request_id: 1, verdict: DeliveryVerdict::Admitted { seq: 44 },
        }).await.unwrap();
        assert!(matches!(
            verdict_rx.await.unwrap(),
            DeliveryVerdict::Admitted { seq: 44 }
        ));
        let (arrivals, mut arrivals_rx) = tokio::sync::mpsc::channel(1);
        let mut ctx = InterfaceLiftContext::unprivileged(InterfaceId::new("door"), root.path().join("interface"), None,
            root.path().to_path_buf(), root.path().join("movements.jsonl"), root.path().join("blobs"),
            root.path().join("ledger-index.sqlite"), BTreeMap::new(), arrivals, CancellationToken::new());
        ctx.host_io_limits = Some(InterfaceHostIoLimits {
            max_control_frame_bytes: 65536, max_stderr_bytes: 65536, max_manifest_bytes: 65536, max_payload_bytes: 65536,
        });
        let artifact = InterfaceArtifactFile::write(root.path(), "retained.output", b"payload").unwrap();
        for expected in [Ok(octopus_ledger::ArtifactDelivery::Pending { scope_seq: 5 }), Err("exact scope corruption".to_string())] {
            let handoff = ctx.hand_arrival_retained(artifact.clone(), Vec::new(), "opaque retry identity");
            let host = async {
                let arrival = arrivals_rx.recv().await.unwrap();
                let output = spawned_arrival_output(arrival, &pending, &mut next_request_id).await.unwrap();
                let SpawnedLifeOutput::RetainedArrival { request_id, delivery_id, .. } = output else { panic!("missing retained frame") };
                assert_eq!(delivery_id, "opaque retry identity");
                let frame = serde_json::to_vec(&SpawnedLifeInput::RetainedDelivery { request_id, delivery: expected.clone() }).unwrap();
                accept_spawned_life_input(&pending, serde_json::from_slice(&frame).unwrap()).await.unwrap();
            };
            let (actual, ()) = tokio::join!(handoff, host);
            assert_eq!(actual, expected);
        }
    }
}
