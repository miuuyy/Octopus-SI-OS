use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[cfg(not(target_os = "macos"))]
use notify::{RecursiveMode, Watcher};
use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{
    ActivationStart, ActivationStartFn, InterfaceActivationArrival, InterfaceHostIoLimits,
    InterfaceLiftContext, InterfaceLiftFn, SystemInterfaceEmission,
    SystemInterfaceEmissionArtifact, INTERFACE_HOST_IO_LIMITS_ENV,
    RESOLVED_INTERFACE_DESCRIPTOR_ENV,
};
use octopus_interfaces::{
    ArtifactBindingPayload, ArtifactBindingPayloadSet, DeliveryVerdict, InterfaceArtifactFile,
    InterfaceArtifactFileError, InterfaceArtifactManifest, InterfaceInvocationEnvelope,
    InvocationMetadata,
};
use octopus_ledger::{
    decode_durable_movement_route, ArtifactRef, Endpoint, Movement, MovementKind,
};
use octopus_ledger_index::{LedgerSqliteReader, AUTHORITATIVE_LEDGER_SCAN_MOVEMENT_BUDGET};
use octopus_ledger_store::DurableMovementDecoder;
use tokio_util::sync::CancellationToken;

pub async fn run_descriptor_activation_life(
    ctx: InterfaceLiftContext,
    activation_ref: &str,
    start: ActivationStartFn,
) -> Result<(), String> {
    let host_io_limits = ctx.required_host_io_limits()?;
    let loaded = octopus_interface_projection::load_interface_activation_file_in_dir(
        &ctx.interface_dir,
        &ctx.interface,
    )?;
    let artifact_root = ctx.interface_var_dir().join("activation-artifacts");
    std::fs::create_dir_all(&artifact_root).map_err(|error| {
        format!(
            "activation artifact root '{}' cannot be created: {error}",
            artifact_root.display()
        )
    })?;
    let mut tasks = Vec::new();
    for activation in loaded
        .descriptor
        .activations
        .into_iter()
        .filter(|activation| {
            activation.enabled && activation.activation_driver_ref == activation_ref
        })
    {
        let started = start(ActivationStart {
            interface_id: ctx.interface.clone(),
            config: activation.config,
            host_io_limits,
            artifact_root: artifact_root.clone(),
            tx: ctx.arrivals.clone(),
            cancel: ctx.cancel.clone(),
            interface_var_dir: ctx.var_dir.clone(),
            root_slots: ctx.root_slots.clone(),
        })
        .await
        .map_err(|error| {
            format!(
                "interface '{}' activation '{}' failed: {error}",
                ctx.interface.as_str(),
                activation_ref
            )
        })?;
        tasks.extend(started);
    }
    if tasks.is_empty() {
        return Err(format!(
            "interface '{}' declares no enabled '{}' activation",
            ctx.interface.as_str(),
            activation_ref
        ));
    }
    ctx.signal_ready().await?;
    ctx.cancel.cancelled().await;
    for task in tasks {
        task.abort();
    }
    Ok(())
}

fn validate_host_io_limits(limits: InterfaceHostIoLimits) -> Result<(), String> {
    limits.validate()
}

pub struct BoundedJsonLineReader<R> {
    reader: tokio::io::BufReader<R>,
    buffer: Vec<u8>,
    maximum_bytes: usize,
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
        })
    }

    pub async fn next_line(&mut self) -> Result<Option<String>, String> {
        use tokio::io::AsyncBufReadExt;

        self.buffer.clear();
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
) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    let mut observed = 0_u64;
    let mut digest = blake3::Hasher::new();
    let mut buffer = [0_u8; 8192];
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
    }
    if observed > 0 {
        let digest = digest.finalize().to_hex().to_string();
        tracing::warn!(
            interface = interface.as_str(),
            process = process_name,
            stderr_digest = digest,
            observed_bytes = observed,
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
    Asleep {
        through_seq: u64,
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

pub async fn run_spawned_interface_life(life: InterfaceLiftFn) -> Result<(), String> {
    let (arrivals, arrival_rx) = tokio::sync::mpsc::channel(64);
    let (emissions, emission_rx) = tokio::sync::mpsc::channel(64);
    let ctx = spawned_context_from_env(arrivals, emissions)?;
    let host_io_limits = ctx.required_host_io_limits()?;
    let (ctx, readiness, publication) = ctx.with_readiness_publication_channel();

    let pending = Arc::new(tokio::sync::Mutex::new(BTreeMap::new()));
    let output = pump_spawned_life_outputs(
        arrival_rx,
        emission_rx,
        readiness,
        publication,
        Arc::clone(&pending),
        host_io_limits.max_control_frame_bytes,
    );
    let input =
        pump_spawned_life_inputs(Arc::clone(&pending), host_io_limits.max_control_frame_bytes);
    let observed = ctx.clone();
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
    if let Some(through_seq) = observed.asleep_through() {
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
    emissions: tokio::sync::mpsc::Sender<SystemInterfaceEmission>,
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
    InterfaceLiftContext::unprivileged(
        interface,
        interface_dir,
        Some(descriptor_snapshot),
        var_dir,
        optional_spawned_env_path("OCTOPUS_MOVEMENTS_JSONL"),
        optional_spawned_env_path("OCTOPUS_BLOBS_DIR"),
        optional_spawned_env_path("OCTOPUS_LEDGER_INDEX_SQLITE"),
        serde_json::from_str(
            &std::env::var("OCTOPUS_ROOT_SLOTS_JSON").map_err(|_| "OCTOPUS_ROOT_SLOTS_JSON is required by the process env contract".to_string())?,
        )
        .map_err(|error| format!("OCTOPUS_ROOT_SLOTS_JSON is malformed: {error}"))?,
        arrivals,
        CancellationToken::new(),
    )
    .with_system_emission_sender(emissions)
    .with_host_io_limits(host_io_limits)
}

enum SpawnedReply {
    Ordinary(tokio::sync::oneshot::Sender<DeliveryVerdict>),
    Retained(tokio::sync::oneshot::Sender<Result<octopus_ledger::ArtifactDelivery, String>>),
}

type PendingVerdicts = Arc<tokio::sync::Mutex<BTreeMap<u64, SpawnedReply>>>;

#[cfg(test)]
#[path = "../tests/support/spawned.rs"]
mod spawned_tests;

async fn pump_spawned_life_outputs(
    arrivals: tokio::sync::mpsc::Receiver<InterfaceActivationArrival>,
    emissions: tokio::sync::mpsc::Receiver<SystemInterfaceEmission>,
    readiness: tokio::sync::oneshot::Receiver<Result<(), String>>,
    publication: octopus_interface_transport::InterfaceLifeReadyPublication,
    pending: PendingVerdicts,
    maximum_frame_bytes: u64,
) -> Result<(), String> {
    pump_spawned_life_outputs_to(arrivals, emissions, readiness, publication, pending, maximum_frame_bytes, &mut std::io::stdout()).await
}

async fn pump_spawned_life_outputs_to(
    mut arrivals: tokio::sync::mpsc::Receiver<InterfaceActivationArrival>,
    mut emissions: tokio::sync::mpsc::Receiver<SystemInterfaceEmission>,
    readiness: tokio::sync::oneshot::Receiver<Result<(), String>>,
    publication: octopus_interface_transport::InterfaceLifeReadyPublication,
    pending: PendingVerdicts,
    maximum_frame_bytes: u64,
    stdout: &mut impl std::io::Write,
) -> Result<(), String> {
    let mut next_request_id = 1_u64;
    let mut readiness = Some(readiness);
    let mut publication = Some(publication);
    loop {
        let output = tokio::select! {
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
                let (artifact, _from_interface, source_movement_seq, to_interface, verdict_tx) =
                    emission.into_parts();
                let request_id = register_required_spawned_verdict(
                    &pending,
                    &mut next_request_id,
                    verdict_tx,
                )
                .await?;
                SpawnedLifeOutput::SystemEmission {
                    request_id,
                    artifact: match artifact {
                        SystemInterfaceEmissionArtifact::Materialized(artifact) => {
                            SpawnedLifeEmissionArtifact::Materialized {
                                artifact_manifest_path: artifact.manifest_path().to_path_buf(),
                            }
                        }
                        SystemInterfaceEmissionArtifact::Reference(artifact) => {
                            SpawnedLifeEmissionArtifact::Reference { artifact }
                        }
                    },
                    source_movement_seq,
                    to_interface: to_interface.as_str().to_string(),
                }
            }
        };
        let bytes =
            serialize_bounded_json_line(&output, maximum_frame_bytes, "spawned-life output frame")?;
        let written = stdout.write_all(&bytes).and_then(|()| stdout.flush())
            .map_err(|error| format!("spawned life output write failed: {error}"));
        if matches!(output, SpawnedLifeOutput::Ready) {
            publication.take().ok_or("spawned life repeated readiness publication")?.complete(written.clone())?;
        }
        written?;
    }
}

async fn spawned_arrival_output(
    arrival: InterfaceActivationArrival,
    pending: &PendingVerdicts,
    next_request_id: &mut u64,
) -> Result<SpawnedLifeOutput, String> {
    let InterfaceActivationArrival { artifact, interface: _, evidence, verdict_tx, retained } = arrival;
    let artifact_manifest_path = artifact.manifest_path().to_path_buf();
    let evidence = evidence.into_iter().map(|item| SpawnedEvidenceLine {
        interface_id: item.interface_id.as_str().to_string(),
        artifact_id: item.artifact_id.as_str().to_string(),
        label: item.label,
        bytes: item.bytes,
    }).collect();
    match (verdict_tx, retained) {
        (None, Some(retained)) => {
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

async fn register_spawned_verdict(
    pending: &PendingVerdicts,
    next_request_id: &mut u64,
    verdict_tx: Option<tokio::sync::oneshot::Sender<DeliveryVerdict>>,
) -> Result<Option<u64>, String> {
    let Some(verdict_tx) = verdict_tx else {
        return Ok(None);
    };
    register_required_spawned_verdict(pending, next_request_id, verdict_tx).await.map(Some)
}

async fn register_required_spawned_verdict(
    pending: &PendingVerdicts,
    next_request_id: &mut u64,
    verdict_tx: tokio::sync::oneshot::Sender<DeliveryVerdict>,
) -> Result<u64, String> {
    register_spawned_reply(pending, next_request_id, SpawnedReply::Ordinary(verdict_tx)).await
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
    returned_own_through: TailCursor,
    change_watch: Option<LedgerChangeWatch>,
}

pub struct LedgerReplayReader {
    reader: LedgerTailReader,
    end_offset: u64,
    exhausted: bool,
}

impl LedgerReplayReader {
    pub fn open(ctx: &InterfaceLiftContext) -> Result<Self, String> {
        let limits = ctx.required_host_io_limits()?;
        let end_offset = std::fs::metadata(&ctx.movements_path)
            .map_err(|error| format!("ledger replay boundary cannot be read: {error}"))?
            .len();
        Ok(Self {
            reader: LedgerTailReader::from_cursor(
                ctx,
                limits,
                TailCursor::default(),
                PathBuf::new(),
            )?,
            end_offset,
            exhausted: false,
        })
    }

    pub fn read_batch(
        &mut self,
        maximum_movements: usize,
    ) -> Result<Option<Vec<OwnOutboundMovement>>, String> {
        if maximum_movements == 0 {
            return Err("ledger replay batch must allow at least one movement".into());
        }
        if self.exhausted {
            return Ok(None);
        }
        let batch = self.reader.read_from(
            self.reader.pending_cursor,
            false,
            maximum_movements,
            Some(self.end_offset),
        )?;
        self.exhausted = !self.reader.resume_without_wait;
        Ok(Some(batch))
    }

    pub fn envelope(
        &self,
        own: &OwnOutboundMovement,
    ) -> Result<InterfaceInvocationEnvelope, String> {
        self.reader.envelope(own)
    }
}

#[cfg(not(target_os = "macos"))]
struct LedgerChangeWatch {
    _watcher: notify::RecommendedWatcher,
    events: tokio::sync::mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
}

#[cfg(target_os = "macos")]
struct LedgerChangeWatch {
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
    pub fn open(ctx: &InterfaceLiftContext) -> Result<Self, String> {
        Self::open_named(ctx, "default")
    }

    pub fn open_named(ctx: &InterfaceLiftContext, reader: &str) -> Result<Self, String> {
        let host_io_limits = ctx.required_host_io_limits()?;
        Self::open_named_with_limits(ctx, reader, host_io_limits)
    }

    pub fn open_with_limits(
        ctx: &InterfaceLiftContext,
        host_io_limits: InterfaceHostIoLimits,
    ) -> Result<Self, String> {
        Self::open_named_with_limits(ctx, "default", host_io_limits)
    }

    pub fn open_named_with_limits(
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
        Self::from_cursor(ctx, host_io_limits, cursor, cursor_path)
    }

    fn from_cursor(
        ctx: &InterfaceLiftContext,
        host_io_limits: InterfaceHostIoLimits,
        cursor: TailCursor,
        cursor_path: PathBuf,
    ) -> Result<Self, String> {
        if !ctx.movements_path.is_absolute() || !ctx.blobs_dir.is_absolute() {
            return Err("ledger reader requires absolute ledger and blob read handles".into());
        }
        validate_host_io_limits(host_io_limits)?;
        let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);
        Ok(Self {
            interface: ctx.interface.clone(),
            movements_path: ctx.movements_path.clone(),
            decoder,
            blobs_dir: ctx.blobs_dir.clone(),
            host_io_limits,
            cursor_path,
            cursor,
            pending_cursor: cursor,
            spotlight: LedgerSqliteReader::open(&ctx.ledger_index_path).ok(),
            resume_without_wait: false,
            foreign_catch_up_pending: false,
            returned_own_through: cursor,
            change_watch: None,
        })
    }

    pub fn next_seq(&self) -> u64 {
        self.cursor.seq
    }

    pub fn read_new(&mut self) -> Result<Vec<OwnOutboundMovement>, String> {
        self.read_from(self.cursor, true, usize::MAX, None)
    }

    pub fn read_new_uncommitted_limit(
        &mut self,
        maximum_movements: usize,
    ) -> Result<Vec<OwnOutboundMovement>, String> {
        if maximum_movements == 0 {
            return Ok(Vec::new());
        }
        self.read_from(self.pending_cursor, false, maximum_movements, None)
    }

    fn read_from(
        &mut self,
        start: TailCursor,
        advance_empty_scan: bool,
        maximum_movements: usize,
        end_offset: Option<u64>,
    ) -> Result<Vec<OwnOutboundMovement>, String> {
        let file = match std::fs::File::open(&self.movements_path) {
            Ok(file) => file,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && end_offset.is_none() =>
            {
                return Ok(Vec::new());
            }
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
        let ledger_len = match end_offset {
            Some(end) if end > ledger_len => {
                return Err(format!("ledger was truncated below replay boundary {end}"));
            }
            Some(end) => end,
            None => ledger_len,
        };
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
            if !addressed_here {
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
            let metadata = movement
                .invocation_metadata
                .as_ref()
                .ok_or_else(|| {
                    format!(
                        "outbound movement seq {} for interface '{}' carries no invocation metadata",
                        movement.seq.0,
                        self.interface.as_str()
                    )
                })
                .and_then(|value| {
                    value.invocation().cloned().ok_or_else(|| {
                        format!(
                            "outbound movement seq {} for interface '{}' carries non-invocation metadata",
                            movement.seq.0,
                            self.interface.as_str()
                        )
                    })
                })
                .and_then(|metadata| {
                    let matches_interface = metadata
                        .runtime()
                        .is_some_and(|runtime| runtime.interface == self.interface);
                    if !matches_interface {
                        return Err(format!(
                            "outbound movement seq {} for interface '{}' carries mismatched runtime coordinates",
                            movement.seq.0,
                            self.interface.as_str()
                        ));
                    }
                    Ok(metadata)
                });
            let movement_seq = movement.seq.0;
            own.push(OwnOutboundMovement {
                movement,
                metadata,
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
        if let Some(last) = own.last() {
            self.returned_own_through = last.cursor;
        }
        if advance_empty_scan && own.is_empty() {
            self.advance_scanned()?;
        }
        Ok(own)
    }

    pub fn advance_settled_scan(&mut self) -> Result<(), String> {
        if self.returned_own_through.seq > self.cursor.seq {
            return Err(format!(
                "interface '{}' cannot advance its ledger cursor past own movement {} before committing it",
                self.interface.as_str(),
                self.returned_own_through.seq.saturating_sub(1)
            ));
        }
        self.advance_scanned()
    }

    fn advance_scanned(&mut self) -> Result<(), String> {
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

    pub fn commit(&mut self) -> Result<(), String> {
        self.persist_cursor(&self.pending_cursor)?;
        self.cursor = self.pending_cursor;
        self.foreign_catch_up_pending = false;
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
        let metadata = own.metadata()?;
        let artifact = own.movement.artifact.clone().ok_or_else(|| {
            format!(
                "outbound movement seq {} for interface '{}' carries no artifact",
                own.movement.seq.0,
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
        let mut artifact_binding_payloads: ArtifactBindingPayloadSet = Vec::new();
        for binding_ref in &metadata.artifact_binding_refs {
            let binding_blob_path = self.blobs_dir.join(binding_ref.content.to_hex());
            let bytes = resolve_artifact_binding_payload(&binding_blob_path, self.host_io_limits)?;
            artifact_binding_payloads.push(ArtifactBindingPayload::new(binding_ref.clone(), bytes));
        }
        Ok(InterfaceInvocationEnvelope {
            artifact,
            artifact_file,
            artifact_binding_payloads,
            metadata: Some(metadata.clone()),
        })
    }
}

#[cfg(not(target_os = "macos"))]
impl LedgerChangeWatch {
    async fn wait(&mut self, cancel: &CancellationToken, path: &Path) -> Result<(), String> {
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
    async fn wait(&mut self, cancel: &CancellationToken, path: &Path) -> Result<(), String> {
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
fn ledger_change_watch(path: &Path) -> Result<LedgerChangeWatch, String> {
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
fn ledger_change_watch(path: &Path) -> Result<LedgerChangeWatch, String> {
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

fn resolve_artifact_binding_payload(
    blob_path: &Path,
    limits: InterfaceHostIoLimits,
) -> Result<Vec<u8>, String> {
    let carrier_maximum = limits.max_manifest_bytes.max(limits.max_payload_bytes);
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(blob_path, carrier_maximum)
        .map_err(|error| {
            format!(
                "artifact-binding blob '{}' cannot be captured: {error}",
                blob_path.display()
            )
        })?;
    match serde_json::from_slice::<InterfaceArtifactManifest>(&bytes) {
        Ok(manifest) if !manifest.payload_path.as_os_str().is_empty() => {
            let manifest_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            if manifest_size > limits.max_manifest_bytes {
                return Err(format!(
                    "artifact-binding manifest blob '{}' is {manifest_size} bytes, above the declared ceiling {}",
                    blob_path.display(),
                    limits.max_manifest_bytes
                ));
            }
            if manifest.label.trim().is_empty() || !manifest.payload_path.is_absolute() {
                return Err(format!(
                    "artifact-binding manifest blob '{}' has an empty label or non-absolute payload path",
                    blob_path.display()
                ));
            }
            let cache_root = blob_path
                .parent()
                .ok_or_else(|| "artifact-binding blob has no cache root".to_string())?;
            let payload_path = match manifest.payload_content_hash.as_deref() {
                Some(hash) => {
                    if octopus_ledger::ContentHash::from_hex(hash).is_none() {
                        return Err(format!(
                            "artifact-binding manifest blob '{}' has an invalid payload content hash",
                            blob_path.display()
                        ));
                    }
                    cache_root.join(hash)
                }
                None => manifest.payload_path.clone(),
            };
            let payload_bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
                &payload_path,
                limits.max_payload_bytes,
            )
            .map_err(|error| {
                format!(
                    "artifact-binding payload for manifest blob '{}' cannot be captured: {error}",
                    blob_path.display()
                )
            })?;
            if let Some(expected) = manifest.payload_content_hash.as_deref() {
                let actual = octopus_ledger::ContentHash::of_bytes(&payload_bytes).to_hex();
                if expected != actual {
                    return Err(format!(
                        "artifact-binding payload for manifest blob '{}' has content hash {actual}, expected {expected}",
                        blob_path.display()
                    ));
                }
            }
            Ok(payload_bytes)
        }
        _ => {
            let payload_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            if payload_size > limits.max_payload_bytes {
                return Err(format!(
                    "artifact-binding blob '{}' is {payload_size} bytes, above the declared payload ceiling {}",
                    blob_path.display(),
                    limits.max_payload_bytes
                ));
            }
            Ok(bytes)
        }
    }
}
