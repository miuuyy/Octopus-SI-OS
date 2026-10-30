use octopus_descriptors::ProcessSandboxMode;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use octopus_exec::{
    Environment, ExecOutputStream, ExecParams, ExecProcess, ExecProcessEvent,
    ExecProcessEventReceiver, ExecServerRuntimePaths, ProcessId, ProcessSignal, WriteStatus,
};
use octopus_util_pathuri::PathUri;
use octopus_worker_env::{AbsPath, ProjectedSandbox, WorkerEnvironmentId};

pub const PERSISTENT_PROCESS_KEEPER_FLAG: &str = "--octopus-persistent-process-keeper";
const KEEPER_SPEC_SCHEMA_VERSION: u32 = 1;
const KEEPER_FRAME_BYTES: u64 = 8 * 1024 * 1024;
const KEEPER_READY_TIMEOUT: Duration = Duration::from_secs(10);
const KEEPER_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
const DELIVERED_PROCESS_RETENTION: Duration = Duration::from_secs(300);
const UNDELIVERED_PROCESS_RETENTION: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceProcessLimits {
    pub max_processes_per_worker_environment: usize,
    pub max_buffered_output_bytes: usize,
    pub max_buffered_output_events: usize,
    pub max_input_bytes: usize,
    pub process_timeout: Duration,
    pub cleanup_timeout: Duration,
}

impl WorkspaceProcessLimits {
    fn validate(&self) -> Result<(), String> {
        if self.max_processes_per_worker_environment == 0 {
            return Err("persistent coder process count ceiling must be positive".to_string());
        }
        if self.max_buffered_output_bytes == 0 {
            return Err("persistent coder buffered output ceiling must be positive".to_string());
        }
        if self.max_buffered_output_events == 0 {
            return Err("persistent coder buffered event ceiling must be positive".to_string());
        }
        if self.max_input_bytes == 0 {
            return Err("persistent coder input ceiling must be positive".to_string());
        }
        if self.process_timeout.is_zero() {
            return Err("persistent coder process timeout must be positive".to_string());
        }
        if self.cleanup_timeout.is_zero() {
            return Err("persistent coder cleanup timeout must be positive".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceProcessChunk {
    pub seq: u64,
    pub stream: ExecOutputStream,
    pub byte_offset: usize,
    pub end_of_event: bool,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceProcessPoll {
    pub process_id: String,
    pub chunks: Vec<WorkspaceProcessChunk>,
    pub next_seq: u64,
    pub running: bool,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub exit_seq: Option<u64>,
    pub closed: bool,
    pub close_seq: Option<u64>,
    pub continuation_available: bool,
    pub timed_out: bool,
    pub failure: Option<String>,
    pub tty: bool,
    pub cwd: AbsPath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceProcessInfo {
    pub process_id: String,
    pub running: bool,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub closed: bool,
    pub timed_out: bool,
    pub failure: Option<String>,
    pub continuation_available: bool,
    pub pending_output_bytes: usize,
    pub next_source_seq: u64,
    pub tty: bool,
    pub cwd: AbsPath,
}

#[derive(Clone, Debug)]
pub struct WorkspaceProcessStore {
    registry: PathBuf,
    sockets: PathBuf,
    keeper: PathBuf,
}

impl WorkspaceProcessStore {
    pub fn new(durable_state: &Path, sockets: &Path, keeper: PathBuf) -> Result<Self, String> {
        let registry = durable_state.join("persistent-processes");
        private_directory(&registry)?;
        if !sockets.is_absolute() || !keeper.is_absolute() {
            return Err("persistent process sockets and keeper must be absolute paths".to_string());
        }
        Ok(Self {
            registry,
            sockets: sockets.to_path_buf(),
            keeper,
        })
    }

    fn spec_path(&self, process_id: &str) -> PathBuf {
        self.registry.join(format!("{process_id}.json"))
    }

    fn start_path(&self, room: &WorkerEnvironmentId, call: &str) -> PathBuf {
        let mut identity = blake3::Hasher::new();
        identity.update(&(room.as_str().len() as u64).to_le_bytes());
        identity.update(room.as_str().as_bytes());
        identity.update(call.as_bytes());
        self.registry.join("starts").join(format!("{}.json", identity.finalize().to_hex()))
    }

    fn retain_start(&self, spec: &KeeperSpec) -> Result<(), String> {
        if let Some(call) = &spec.start_call {
            private_directory(&self.registry.join("starts"))?;
            let room = WorkerEnvironmentId::parse(spec.worker_environment_id.clone()).map_err(|error| error.to_string())?;
            spec.write_new(&self.start_path(&room, call))?;
        }
        Ok(())
    }

    fn specs(&self, worker_environment_id: &WorkerEnvironmentId) -> Result<Vec<KeeperSpec>, String> {
        let entries = std::fs::read_dir(&self.registry).map_err(|error| {
            format!(
                "persistent process registry '{}' cannot be read: {error}",
                self.registry.display()
            )
        })?;
        let mut specs = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| format!("persistent process registry entry is unreadable: {error}"))?
                .path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let spec = match KeeperSpec::read(&path) {
                Ok(spec) => spec,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(format!(
                        "persistent process record '{}' is unreadable: {error}",
                        path.display()
                    ));
                }
            };
            if spec.worker_environment_id == worker_environment_id.as_str() {
                specs.push(spec);
            }
        }
        specs.sort_by(|left, right| left.process_id.cmp(&right.process_id));
        Ok(specs)
    }

    fn spec(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
    ) -> Result<KeeperSpec, String> {
        let missing = || {
            format!(
                "worker environment '{}' has no persistent process '{}'",
                worker_environment_id, process_id
            )
        };
        if !valid_process_id(process_id) {
            return Err(missing());
        }
        let path = self.spec_path(process_id);
        let spec = match KeeperSpec::read(&path).or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                KeeperSpec::read(&retained_spec_path(&path))
            } else { Err(error) }
        }) {
            Ok(spec) => spec,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(missing()),
            Err(error) => {
                return Err(format!(
                    "persistent process record '{}' is unreadable: {error}",
                    process_id
                ));
            }
        };
        if spec.worker_environment_id != worker_environment_id.as_str() {
            return Err(missing());
        }
        Ok(spec)
    }

    fn retire(&self, spec: &KeeperSpec) -> Result<(), String> {
        let path = self.spec_path(&spec.process_id);
        retire_spec(&path, spec)?;
        for path in [spec.socket.clone(), path.with_extension("log")] {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    async fn lock_starts(&self) -> Result<std::fs::File, String> {
        use fs2::FileExt;
        let file = private_options().write(true).create(true).truncate(false)
            .open(self.registry.join("starts.lock")).map_err(|error| error.to_string())?;
        let deadline = tokio::time::Instant::now() + KEEPER_READY_TIMEOUT;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(file),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err("persistent process start is still owned by another invocation".into());
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => return Err(format!("persistent process start cannot be locked: {error}")),
            }
        }
    }
}

pub struct WorkspaceProcessManager {
    store: Option<WorkspaceProcessStore>,
    starts: tokio::sync::Mutex<()>,
}

impl WorkspaceProcessManager {
    pub fn new(store: Option<WorkspaceProcessStore>) -> Self {
        Self {
            store,
            starts: tokio::sync::Mutex::new(()),
        }
    }

    fn store(&self) -> Result<&WorkspaceProcessStore, String> {
        self.store.as_ref().ok_or_else(|| {
            "persistent processes require formation-scoped durable state and an interface-local IPC root"
                .to_string()
        })
    }

    pub async fn start(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        sandbox: &ProjectedSandbox,
        sandbox_mode: ProcessSandboxMode,
        mut argv: Vec<String>,
        cwd: PathBuf,
        mut env: BTreeMap<String, String>,
        tty: bool,
        wait_ms: u64,
        max_output_bytes: usize,
        limits: WorkspaceProcessLimits,
        call: Option<&str>,
    ) -> Result<WorkspaceProcessPoll, String> {
        limits.validate()?;
        validate_poll_output_ceiling(max_output_bytes)?;
        let store = self.store()?;
        let _start = self.starts.lock().await;
        let _durable_start = store.lock_starts().await?;
        if let Some(call) = call {
            match KeeperSpec::read(&store.start_path(worker_environment_id, call)) {
                Ok(spec) => {
                    if spec.worker_environment_id != worker_environment_id.as_str() || spec.start_call.as_deref() != Some(call) {
                        return Err("persistent process start receipt differs from its invocation".into());
                    }
                    drop(_durable_start);
                    drop(_start);
                    return poll_keeper(store, &spec, KeeperRequest::Poll {
                        call: Some(call.into()), input: String::new(), wait_ms, max_output_bytes,
                    }).await;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("persistent process start receipt is unreadable: {error}")),
            }
        }
        let specs = store.specs(worker_environment_id)?;
        if let Some(call) = call
            && let Some(spec) = specs
                .iter()
                .find(|spec| spec.start_call.as_deref() == Some(call))
        {
            store.retain_start(spec)?;
            drop(_durable_start);
            drop(_start);
            return poll_keeper(
                store,
                spec,
                KeeperRequest::Poll {
                    call: Some(call.to_string()),
                    input: String::new(),
                    wait_ms,
                    max_output_bytes,
                },
            )
            .await;
        }
        if argv.is_empty() || argv.iter().any(String::is_empty) {
            return Err("persistent coder process argv must contain non-empty strings".to_string());
        }
        let cwd = AbsPath::new(&cwd)
            .map_err(|error| format!("persistent coder process cwd is invalid: {error}"))?;
        sandbox
            .authorize_read(&cwd)
            .map_err(|error| format!("persistent coder process cwd is denied: {error}"))?;
        resolve_persistent_process_program(&mut argv, &env, cwd.as_path())?;
        let argv = crate::sandboxed_room_process_argv(sandbox, sandbox_mode, &cwd, argv)
            .map_err(|error| format!("persistent coder sandbox floor failed: {error}"))?;
        let owned = specs
            .iter()
            .filter(|spec| {
                let path = store.spec_path(&spec.process_id);
                !delivered_marker(&path).exists() && !retired_marker(&path).exists()
            })
            .count();
        if owned >= limits.max_processes_per_worker_environment {
            return Err(format!(
                "worker environment '{}' already owns the descriptor maximum of {} persistent processes",
                worker_environment_id, limits.max_processes_per_worker_environment
            ));
        }
        env.insert("TMPDIR".to_string(), cwd.as_str().to_string());
        env.insert("TMP".to_string(), cwd.as_str().to_string());
        env.insert("TEMP".to_string(), cwd.as_str().to_string());
        let identity = new_process_identity(worker_environment_id)?;
        let spec = KeeperSpec {
            schema_version: KEEPER_SPEC_SCHEMA_VERSION,
            worker_environment_id: worker_environment_id.as_str().to_string(),
            process_id: format!("coder-process-{identity}"),
            start_call: call.map(str::to_string),
            argv,
            cwd: cwd.as_path().to_path_buf(),
            env,
            tty,
            socket: store.sockets.join(format!("p-{identity}.sock")),
            max_buffered_output_bytes: limits.max_buffered_output_bytes,
            max_buffered_output_events: limits.max_buffered_output_events,
            max_input_bytes: limits.max_input_bytes,
            process_timeout_ms: duration_ms(limits.process_timeout)?,
            cleanup_timeout_ms: duration_ms(limits.cleanup_timeout)?,
        };
        let spec_path = store.spec_path(&spec.process_id);
        spec.write_new(&spec_path)?;
        store.retain_start(&spec)?;
        if let Err(error) = spawn_keeper(store, &spec, &spec_path).await {
            store.retire(&spec)?;
            return Err(error);
        }
        drop(_durable_start);
        drop(_start);
        poll_keeper(
            store,
            &spec,
            KeeperRequest::Poll {
                call: call.map(str::to_string),
                input: String::new(),
                wait_ms,
                max_output_bytes,
            },
        )
        .await
    }

    pub async fn write_and_poll(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        input: &[u8],
        wait_ms: u64,
        max_output_bytes: usize,
        call: Option<&str>,
    ) -> Result<WorkspaceProcessPoll, String> {
        validate_poll_output_ceiling(max_output_bytes)?;
        let store = self.store()?;
        let spec = store.spec(worker_environment_id, process_id)?;
        if input.len() > spec.max_input_bytes {
            return Err(format!(
                "persistent coder stdin contains {} bytes, exceeding the descriptor-owned {} byte ceiling",
                input.len(),
                spec.max_input_bytes
            ));
        }
        poll_keeper(
            store,
            &spec,
            KeeperRequest::Poll {
                call: call.map(str::to_string),
                input: BASE64.encode(input),
                wait_ms,
                max_output_bytes,
            },
        )
        .await
    }

    pub async fn interrupt_and_poll(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        wait_ms: u64,
        max_output_bytes: usize,
        call: Option<&str>,
    ) -> Result<WorkspaceProcessPoll, String> {
        validate_poll_output_ceiling(max_output_bytes)?;
        let store = self.store()?;
        let spec = store.spec(worker_environment_id, process_id)?;
        poll_keeper(
            store,
            &spec,
            KeeperRequest::Interrupt {
                call: call.map(str::to_string),
                wait_ms,
                max_output_bytes,
            },
        )
        .await
    }

    pub async fn terminate_and_poll(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        process_id: &str,
        wait_ms: u64,
        max_output_bytes: usize,
        call: Option<&str>,
    ) -> Result<WorkspaceProcessPoll, String> {
        validate_poll_output_ceiling(max_output_bytes)?;
        let store = self.store()?;
        let spec = store.spec(worker_environment_id, process_id)?;
        poll_keeper(
            store,
            &spec,
            KeeperRequest::Terminate {
                call: call.map(str::to_string),
                wait_ms,
                max_output_bytes,
            },
        )
        .await
    }

    pub async fn list(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
    ) -> Result<Vec<WorkspaceProcessInfo>, String> {
        let store = self.store()?;
        let mut listed = Vec::new();
        for spec in store.specs(worker_environment_id)? {
            let path = store.spec_path(&spec.process_id);
            if delivered_marker(&path).exists() || retired_marker(&path).exists() {
                continue;
            }
            let cwd = AbsPath::new(&spec.cwd)
                .map_err(|error| format!("persistent process cwd is invalid: {error}"))?;
            let info = match ask_keeper(&spec, &KeeperRequest::Info).await {
                Ok(KeeperResponse::Info(info)) => info,
                Ok(KeeperResponse::Refused { error }) => InfoWire::failed(error),
                Ok(_) => InfoWire::failed("persistent process keeper answered Info with another frame".to_string()),
                Err(KeeperFailure::Gone(error)) => {
                    store.retire(&spec)?;
                    InfoWire::failed(error)
                }
                Err(KeeperFailure::Failed(error)) => InfoWire::failed(error),
            };
            listed.push(WorkspaceProcessInfo {
                process_id: spec.process_id.clone(),
                running: info.running,
                exited: info.exited,
                exit_code: info.exit_code,
                closed: info.closed,
                timed_out: info.timed_out,
                failure: info.failure,
                continuation_available: info.continuation_available,
                pending_output_bytes: info.pending_output_bytes,
                next_source_seq: info.next_source_seq,
                tty: spec.tty,
                cwd,
            });
        }
        Ok(listed)
    }

    pub async fn release(
        &self,
        worker_environment_id: &WorkerEnvironmentId,
        cleanup_timeout: Option<Duration>,
    ) -> Result<(), String> {
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        let mut failures = Vec::new();
        for spec in store.specs(worker_environment_id)? {
            if let Some(declared) = cleanup_timeout
                && duration_ms(declared)? != spec.cleanup_timeout_ms
            {
                failures.push(format!(
                    "worker environment '{}' cleanup timeout differs from persistent process '{}'",
                    worker_environment_id, spec.process_id
                ));
                continue;
            }
            match ask_keeper(&spec, &KeeperRequest::Release).await {
                Ok(KeeperResponse::Released) | Err(KeeperFailure::Gone(_)) => {
                    if let Err(error) = store.retire(&spec) { failures.push(error); }
                }
                Ok(KeeperResponse::Refused { error }) | Err(KeeperFailure::Failed(error)) => {
                    failures.push(error)
                }
                Ok(_) => failures.push(format!(
                    "persistent process '{}' keeper answered release with another frame",
                    spec.process_id
                )),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeeperSpec {
    schema_version: u32,
    worker_environment_id: String,
    process_id: String,
    start_call: Option<String>,
    argv: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    tty: bool,
    socket: PathBuf,
    max_buffered_output_bytes: usize,
    max_buffered_output_events: usize,
    max_input_bytes: usize,
    process_timeout_ms: u64,
    cleanup_timeout_ms: u64,
}

impl KeeperSpec {
    fn read(path: &Path) -> std::io::Result<Self> {
        let spec: Self = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if spec.schema_version != KEEPER_SPEC_SCHEMA_VERSION || !valid_process_id(&spec.process_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "persistent process record has an unsupported schema or identity",
            ));
        }
        Ok(spec)
    }

    fn write_new(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        write_private_new(path, &bytes)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum KeeperRequest {
    Poll {
        call: Option<String>,
        input: String,
        wait_ms: u64,
        max_output_bytes: usize,
    },
    Interrupt {
        call: Option<String>,
        wait_ms: u64,
        max_output_bytes: usize,
    },
    Terminate {
        call: Option<String>,
        wait_ms: u64,
        max_output_bytes: usize,
    },
    Info,
    Release,
}

impl KeeperRequest {
    fn identity(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::Poll { call, .. } => call.as_deref().map(|call| ("poll", call)),
            Self::Interrupt { call, .. } => call.as_deref().map(|call| ("interrupt", call)),
            Self::Terminate { call, .. } => call.as_deref().map(|call| ("terminate", call)),
            Self::Info | Self::Release => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum KeeperResponse {
    Poll(PollWire),
    Info(InfoWire),
    Released,
    Refused { error: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunkWire {
    seq: u64,
    stream: ExecOutputStream,
    byte_offset: usize,
    end_of_event: bool,
    bytes: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PollWire {
    chunks: Vec<ChunkWire>,
    next_seq: u64,
    running: bool,
    exited: bool,
    exit_code: Option<i32>,
    exit_seq: Option<u64>,
    closed: bool,
    close_seq: Option<u64>,
    continuation_available: bool,
    timed_out: bool,
    failure: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InfoWire {
    running: bool,
    exited: bool,
    exit_code: Option<i32>,
    closed: bool,
    timed_out: bool,
    failure: Option<String>,
    continuation_available: bool,
    pending_output_bytes: usize,
    next_source_seq: u64,
}

impl InfoWire {
    fn failed(error: String) -> Self {
        Self {
            running: false,
            exited: false,
            exit_code: None,
            closed: false,
            timed_out: false,
            failure: Some(error),
            continuation_available: false,
            pending_output_bytes: 0,
            next_source_seq: 0,
        }
    }
}

enum KeeperFailure {
    Gone(String),
    Failed(String),
}

async fn ask_keeper(
    spec: &KeeperSpec,
    request: &KeeperRequest,
) -> Result<KeeperResponse, KeeperFailure> {
    let mut stream = match tokio::net::UnixStream::connect(&spec.socket).await {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(KeeperFailure::Gone(format!(
                "persistent process '{}' lost its keeper before its outcome was delivered; its outcome cannot be established",
                spec.process_id
            )));
        }
        Err(error) => {
            return Err(KeeperFailure::Failed(format!(
                "persistent process '{}' keeper cannot be reached: {error}",
                spec.process_id
            )));
        }
    };
    let exchange = async {
        let mut frame = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        frame.push(b'\n');
        stream
            .write_all(&frame)
            .await
            .map_err(|error| format!("keeper request cannot be sent: {error}"))?;
        let mut line = String::new();
        tokio::io::BufReader::new((&mut stream).take(KEEPER_FRAME_BYTES))
            .read_line(&mut line)
            .await
            .map_err(|error| format!("keeper response cannot be read: {error}"))?;
        serde_json::from_str::<KeeperResponse>(&line)
            .map_err(|error| format!("keeper response is malformed: {error}"))
    };
    tokio::time::timeout(KEEPER_CONNECTION_TIMEOUT, exchange)
        .await
        .map_err(|_| {
            KeeperFailure::Failed(format!(
                "persistent process '{}' keeper did not answer in time",
                spec.process_id
            ))
        })?
        .map_err(|error| {
            KeeperFailure::Failed(format!("persistent process '{}': {error}", spec.process_id))
        })
}

async fn poll_keeper(
    store: &WorkspaceProcessStore,
    spec: &KeeperSpec,
    request: KeeperRequest,
) -> Result<WorkspaceProcessPoll, String> {
    let path = store.spec_path(&spec.process_id);
    let response = match retained_response(&path, &request)? {
        Some(response) => response,
        None if retired_marker(&path).exists() => return Err(format!(
            "persistent process '{}' is retired and has no retained outcome for this call; it cannot be re-executed", spec.process_id
        )),
        None => match ask_keeper(spec, &request).await {
            Ok(response) => response,
            Err(KeeperFailure::Gone(error)) => {
                store.retire(spec)?;
                return Err(error);
            }
            Err(KeeperFailure::Failed(error)) => return Err(error),
        },
    };
    let poll = match response {
        KeeperResponse::Poll(poll) => poll,
        KeeperResponse::Refused { error } => return Err(error),
        KeeperResponse::Info(_) | KeeperResponse::Released => {
            return Err(format!(
                "persistent process '{}' keeper answered a poll with another frame",
                spec.process_id
            ));
        }
    };
    let chunks = poll
        .chunks
        .into_iter()
        .map(|chunk| {
            Ok(WorkspaceProcessChunk {
                seq: chunk.seq,
                stream: chunk.stream,
                byte_offset: chunk.byte_offset,
                end_of_event: chunk.end_of_event,
                bytes: BASE64
                    .decode(chunk.bytes)
                    .map_err(|error| format!("keeper output chunk is not base64: {error}"))?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(WorkspaceProcessPoll {
        process_id: spec.process_id.clone(),
        chunks,
        next_seq: poll.next_seq,
        running: poll.running,
        exited: poll.exited,
        exit_code: poll.exit_code,
        exit_seq: poll.exit_seq,
        closed: poll.closed,
        close_seq: poll.close_seq,
        continuation_available: poll.continuation_available,
        timed_out: poll.timed_out,
        failure: poll.failure,
        tty: spec.tty,
        cwd: AbsPath::new(&spec.cwd)
            .map_err(|error| format!("persistent process cwd is invalid: {error}"))?,
    })
}

async fn spawn_keeper(
    store: &WorkspaceProcessStore,
    spec: &KeeperSpec,
    spec_path: &Path,
) -> Result<(), String> {
    let log_path = spec_path.with_extension("log");
    let log = {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&log_path)
            .map_err(|error| format!("persistent process keeper log cannot be opened: {error}"))?
    };
    let mut command = tokio::process::Command::new(&store.keeper);
    command
        .arg(PERSISTENT_PROCESS_KEEPER_FLAG)
        .arg(spec_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .kill_on_drop(false);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| {
        format!(
            "persistent process keeper '{}' failed to start: {error}",
            store.keeper.display()
        )
    })?;
    let deadline = tokio::time::Instant::now() + KEEPER_READY_TIMEOUT;
    loop {
        if tokio::net::UnixStream::connect(&spec.socket).await.is_ok() {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("persistent process keeper cannot be observed: {error}"))?
        {
            let detail = std::fs::read_to_string(&log_path).unwrap_or_default();
            return Err(format!(
                "persistent process keeper exited with {status} before it was ready: {}",
                detail.trim().chars().rev().take(2048).collect::<String>().chars().rev().collect::<String>()
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = child.start_kill();
            return Err("persistent process keeper did not become ready in time".to_string());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn delivered_marker(spec_path: &Path) -> PathBuf {
    spec_path.with_extension("delivered")
}

fn retired_marker(spec_path: &Path) -> PathBuf {
    spec_path.with_extension("retired")
}

fn retained_spec_path(spec_path: &Path) -> PathBuf {
    spec_path.parent().expect("process registry parent").join("retired").join(spec_path.file_name().expect("process record name"))
}

fn retire_spec(path: &Path, spec: &KeeperSpec) -> Result<(), String> {
    let retained = retained_spec_path(path);
    private_directory(retained.parent().expect("retained process parent"))?;
    match std::fs::hard_link(path, &retained) {
        Ok(()) => {},
        Err(error) if matches!(error.kind(), std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotFound) => {
            let prior = KeeperSpec::read(&retained).map_err(|error| error.to_string())?;
            if prior != *spec {
                return Err("retained persistent process differs from its original spec".into());
            }
        }
        Err(error) => return Err(format!("persistent process record cannot be retained: {error}")),
    }
    std::fs::File::open(retained.parent().expect("retained process parent"))
        .and_then(|directory| directory.sync_all()).map_err(|error| error.to_string())?;
    private_options().write(true).create(true).truncate(false).open(retired_marker(path))
        .and_then(|file| file.sync_all()).map_err(|error| error.to_string())?;
    match std::fs::remove_file(path) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(format!("persistent process live record cannot be retired: {error}")),
    }
    std::fs::File::open(path.parent().expect("process registry parent"))
        .and_then(|directory| directory.sync_all()).map_err(|error| error.to_string())
}

fn private_options() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = private_options().write(true).create_new(true).open(path)
        .map_err(|error| format!("persistent process record '{}' cannot be created: {error}", path.display()))?;
    file.write_all(bytes).and_then(|()| file.sync_all())
        .map_err(|error| format!("persistent process record cannot be written: {error}"))?;
    std::fs::File::open(path.parent().ok_or("persistent process record has no parent")?)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("persistent process registry cannot be synced: {error}"))
}

fn call_record_path(spec_path: &Path, request: &KeeperRequest) -> Option<PathBuf> {
    request.identity().map(|(operation, call)| {
        spec_path.with_extension("calls").join(format!("{operation}-{}", blake3::hash(call.as_bytes()).to_hex()))
    })
}

fn retained_response(spec_path: &Path, request: &KeeperRequest) -> Result<Option<KeeperResponse>, String> {
    let Some(path) = call_record_path(spec_path, request) else { return Ok(None); };
    match std::fs::read(path.with_extension("response")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some)
            .map_err(|error| format!("persistent process retained outcome is malformed: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("persistent process retained outcome cannot be read: {error}")),
    }
}

fn retain_response(path: &Path, response: &KeeperResponse) -> Result<(), String> {
    let staged = path.with_extension("response-staged");
    let bytes = serde_json::to_vec(response).map_err(|error| error.to_string())?;
    write_private_new(&staged, &bytes)?;
    let published = path.with_extension("response");
    std::fs::hard_link(&staged, &published)
        .map_err(|error| format!("persistent process outcome cannot be published: {error}"))?;
    std::fs::File::open(path.parent().ok_or("persistent process outcome has no parent")?)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("persistent process outcome cannot be synced: {error}"))?;
    std::fs::remove_file(staged).map_err(|error| error.to_string())
}

fn valid_process_id(process_id: &str) -> bool {
    process_id
        .strip_prefix("coder-process-")
        .is_some_and(|identity| identity.len() == 16 && identity.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn new_process_identity(worker_environment_id: &WorkerEnvironmentId) -> Result<String, String> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let mut hasher = blake3::Hasher::new();
    hasher.update(worker_environment_id.as_str().as_bytes());
    hasher.update(&now.to_le_bytes());
    hasher.update(&std::process::id().to_le_bytes());
    hasher.update(&SEQUENCE.fetch_add(1, Ordering::SeqCst).to_le_bytes());
    Ok(hasher.finalize().to_hex()[..16].to_string())
}

fn duration_ms(duration: Duration) -> Result<u64, String> {
    u64::try_from(duration.as_millis()).map_err(|_| "persistent process duration overflowed".to_string())
}

fn private_directory(path: &Path) -> Result<(), String> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        format!(
            "persistent process registry '{}' cannot be created: {error}",
            path.display()
        )
    })?;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "persistent process registry '{}' is not a real directory",
            path.display()
        ));
    }
    Ok(())
}

pub fn persistent_process_keeper_spec(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Option<PathBuf> {
    let _program = arguments.next();
    if arguments.next()?.to_str()? != PERSISTENT_PROCESS_KEEPER_FLAG {
        return None;
    }
    let spec = PathBuf::from(arguments.next()?);
    arguments.next().is_none().then_some(spec)
}

struct ManagedProcess {
    process: Arc<dyn ExecProcess>,
    state: tokio::sync::Mutex<ManagedProcessState>,
    notify: tokio::sync::Notify,
}

struct ManagedProcessState {
    pending: VecDeque<PendingChunk>,
    pending_bytes: usize,
    max_buffered_output_bytes: usize,
    max_buffered_output_events: usize,
    next_expected_seq: u64,
    exited: bool,
    exit_code: Option<i32>,
    exit_seq: Option<u64>,
    closed: bool,
    close_seq: Option<u64>,
    timed_out: bool,
    releasing: bool,
    failure: Option<String>,
    termination_failure: Option<String>,
}

struct PendingChunk {
    seq: u64,
    stream: ExecOutputStream,
    bytes: Vec<u8>,
    source_offset: usize,
    source_complete: bool,
}

struct Keeper {
    spec: KeeperSpec,
    spec_path: PathBuf,
    managed: Arc<ManagedProcess>,
    activity: Arc<tokio::sync::Notify>,
    requests: tokio::sync::Mutex<()>,
    delivered: AtomicBool,
    released: AtomicBool,
    closed_at: std::sync::Mutex<Option<tokio::time::Instant>>,
    stop: tokio::sync::Notify,
}

pub async fn run_persistent_process_keeper(spec_path: PathBuf) -> Result<(), String> {
    let spec = KeeperSpec::read(&spec_path).map_err(|error| {
        format!(
            "persistent process record '{}' is unreadable: {error}",
            spec_path.display()
        )
    })?;
    write_private_new(&spec_path.with_extension("started"), b"")?;
    match std::fs::symlink_metadata(&spec.socket) {
        Ok(_) => std::fs::remove_file(&spec.socket)
            .map_err(|error| format!("stale keeper socket cannot be removed: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("keeper socket cannot be inspected: {error}")),
    }
    let listener = tokio::net::UnixListener::bind(&spec.socket).map_err(|error| {
        format!(
            "keeper socket '{}' cannot be bound: {error}",
            spec.socket.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&spec.socket, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("keeper socket cannot be made private: {error}"))?;
    }
    let cwd = AbsPath::new(&spec.cwd).map_err(|error| format!("keeper cwd is invalid: {error}"))?;
    let process = match start_exec_process(
        &spec.process_id,
        spec.argv.clone(),
        &cwd,
        spec.env.clone(),
        spec.tty,
    )
    .await
    {
        Ok(process) => process,
        Err(error) => {
            let _ = std::fs::remove_file(&spec.socket);
            return Err(error);
        }
    };
    let events = process.subscribe_events();
    let managed = Arc::new(ManagedProcess {
        process,
        state: tokio::sync::Mutex::new(ManagedProcessState::new(
            spec.max_buffered_output_bytes,
            spec.max_buffered_output_events,
        )),
        notify: tokio::sync::Notify::new(),
    });
    let activity = Arc::new(tokio::sync::Notify::new());
    spawn_event_collector(Arc::clone(&managed), events, Arc::clone(&activity));
    spawn_process_timeout(
        Arc::clone(&managed),
        Duration::from_millis(spec.process_timeout_ms),
        Duration::from_millis(spec.cleanup_timeout_ms),
        Arc::clone(&activity),
    );
    let keeper = Arc::new(Keeper {
        spec,
        spec_path,
        managed,
        activity,
        requests: tokio::sync::Mutex::new(()),
        delivered: AtomicBool::new(false),
        released: AtomicBool::new(false),
        closed_at: std::sync::Mutex::new(None),
        stop: tokio::sync::Notify::new(),
    });
    let stop = keeper.stop.notified();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if keeper.retention_expired().await {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|error| format!("keeper socket stopped accepting: {error}"))?;
                let keeper = Arc::clone(&keeper);
                tokio::spawn(async move {
                    if let Err(error) = keeper.answer(stream).await {
                        eprintln!("persistent process keeper request failed: {error}");
                    }
                });
            }
        }
    }
    let _ = std::fs::remove_file(&keeper.spec.socket);
    let _requests = keeper.requests.lock().await;
    retire_spec(&keeper.spec_path, &keeper.spec)?;
    let _ = std::fs::remove_file(keeper.spec_path.with_extension("log"));
    Ok(())
}

impl Keeper {
    async fn retention_expired(&self) -> bool {
        let closed = self.managed.state.lock().await.closed;
        if !closed {
            return false;
        }
        let now = tokio::time::Instant::now();
        let closed_at = {
            let mut closed_at = self
                .closed_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *closed_at.get_or_insert(now)
        };
        let retention = if self.delivered.load(Ordering::SeqCst) {
            DELIVERED_PROCESS_RETENTION
        } else {
            UNDELIVERED_PROCESS_RETENTION
        };
        now.duration_since(closed_at) >= retention
    }

    async fn answer(&self, stream: tokio::net::UnixStream) -> Result<(), String> {
        let (reader, mut writer) = stream.into_split();
        let mut line = String::new();
        tokio::io::BufReader::new(reader.take(KEEPER_FRAME_BYTES))
            .read_line(&mut line)
            .await
            .map_err(|error| format!("keeper request cannot be read: {error}"))?;
        let response = match serde_json::from_str::<KeeperRequest>(&line) {
            Ok(request) => self.respond(request).await,
            Err(error) => KeeperResponse::Refused {
                error: format!("keeper request is not a strict frame: {error}"),
            },
        };
        let mut bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        writer
            .write_all(&bytes)
            .await
            .map_err(|error| format!("keeper response cannot be written: {error}"))?;
        let _ = writer.shutdown().await;
        if self.released.load(Ordering::SeqCst) {
            self.stop.notify_one();
        }
        Ok(())
    }

    async fn respond(&self, request: KeeperRequest) -> KeeperResponse {
        match request {
            KeeperRequest::Info => return KeeperResponse::Info(self.info().await),
            _ => {}
        }
        let _requests = self.requests.lock().await;
        if matches!(request, KeeperRequest::Release) { return self.release().await; }
        match retained_response(&self.spec_path, &request) {
            Ok(Some(response)) => return response,
            Err(error) => return KeeperResponse::Refused { error },
            Ok(None) => {}
        }
        if self.released.load(Ordering::SeqCst) || retired_marker(&self.spec_path).exists() {
            return KeeperResponse::Refused { error: "persistent process is retired".into() };
        }
        let call_path = call_record_path(&self.spec_path, &request);
        if let Some(path) = &call_path {
            let prepared = private_directory(path.parent().expect("call record parent"))
                .and_then(|()| serde_json::to_vec(&request).map_err(|error| error.to_string()))
                .and_then(|bytes| write_private_new(path, &bytes));
            if let Err(error) = prepared {
                return KeeperResponse::Refused { error: format!("persistent process call cannot start or has an uncertain prior outcome: {error}") };
            }
        }
        let response = match self.perform(request).await {
            Ok(poll) => KeeperResponse::Poll(poll),
            Err(error) => KeeperResponse::Refused { error },
        };
        if let Some(path) = call_path {
            if let Err(error) = retain_response(&path, &response) { return KeeperResponse::Refused { error }; }
        }
        if let KeeperResponse::Poll(poll) = &response
            && poll.closed
            && !poll.continuation_available
            && !self.delivered.swap(true, Ordering::SeqCst)
        {
            if let Err(error) = write_private_new(&delivered_marker(&self.spec_path), b"") {
                return KeeperResponse::Refused { error };
            }
        }
        response
    }

    async fn perform(&self, request: KeeperRequest) -> Result<PollWire, String> {
        let managed = &self.managed;
        let (wait_ms, max_output_bytes) = match request {
            KeeperRequest::Poll {
                input,
                wait_ms,
                max_output_bytes,
                ..
            } => {
                let input = BASE64
                    .decode(input)
                    .map_err(|error| format!("keeper stdin frame is not base64: {error}"))?;
                if input.len() > self.spec.max_input_bytes {
                    return Err(format!(
                        "persistent coder stdin contains {} bytes, exceeding the descriptor-owned {} byte ceiling",
                        input.len(),
                        self.spec.max_input_bytes
                    ));
                }
                if !input.is_empty() {
                    if managed.state.lock().await.releasing {
                        return Err(format!(
                            "persistent process '{}' is releasing and cannot accept input",
                            self.spec.process_id
                        ));
                    }
                    let response = managed
                        .process
                        .write(input)
                        .await
                        .map_err(|error| format!("persistent coder stdin write failed: {error}"))?;
                    if response.status != WriteStatus::Accepted {
                        return Err(format!(
                            "persistent coder stdin write was not accepted: {:?}",
                            response.status
                        ));
                    }
                }
                (wait_ms, max_output_bytes)
            }
            KeeperRequest::Interrupt {
                wait_ms,
                max_output_bytes,
                ..
            } => {
                let response = managed
                    .process
                    .signal(ProcessSignal::Interrupt)
                    .await
                    .map_err(|error| format!("persistent coder interrupt failed: {error}"))?;
                if !response.delivered {
                    return Err(format!(
                        "persistent coder process '{}' is not running",
                        self.spec.process_id
                    ));
                }
                (wait_ms, max_output_bytes)
            }
            KeeperRequest::Terminate {
                wait_ms,
                max_output_bytes,
                ..
            } => {
                {
                    let state = managed.state.lock().await;
                    if state.closed {
                        return Err(format!(
                            "persistent coder process '{}' is not running",
                            self.spec.process_id
                        ));
                    }
                }
                let _ = terminate_and_wait(
                    managed,
                    Duration::from_millis(self.spec.cleanup_timeout_ms),
                    &self.activity,
                ).await;
                (wait_ms, max_output_bytes)
            }
            KeeperRequest::Info | KeeperRequest::Release => {
                return Err("keeper control frame is not a poll".to_string());
            }
        };
        validate_poll_output_ceiling(max_output_bytes)?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        loop {
            let terminal = {
                let state = managed.state.lock().await;
                state.closed || state.failure.is_some() || state.termination_failure.is_some() || state.timed_out
            };
            if terminal || wait_ms == 0 || tokio::time::Instant::now() >= deadline {
                break;
            }
            if tokio::time::timeout_at(deadline, managed.notify.notified())
                .await
                .is_err()
            {
                break;
            }
        }
        let mut state = managed.state.lock().await;
        let chunks = state
            .drain(max_output_bytes)
            .into_iter()
            .map(|chunk| ChunkWire {
                seq: chunk.seq,
                stream: chunk.stream,
                byte_offset: chunk.byte_offset,
                end_of_event: chunk.end_of_event,
                bytes: BASE64.encode(chunk.bytes),
            })
            .collect();
        Ok(PollWire {
            chunks,
            next_seq: state.next_expected_seq,
            running: !state.exited && !state.closed,
            exited: state.exited,
            exit_code: state.exit_code,
            exit_seq: state.exit_seq,
            closed: state.closed,
            close_seq: state.close_seq,
            continuation_available: !state.closed || state.pending_bytes > 0,
            timed_out: state.timed_out,
            failure: state.failure(),
        })
    }

    async fn info(&self) -> InfoWire {
        let state = self.managed.state.lock().await;
        InfoWire {
            running: !state.exited && !state.closed,
            exited: state.exited,
            exit_code: state.exit_code,
            closed: state.closed,
            timed_out: state.timed_out,
            failure: state.failure(),
            continuation_available: !state.closed || state.pending_bytes > 0,
            pending_output_bytes: state.pending_bytes,
            next_source_seq: state.next_expected_seq,
        }
    }

    async fn release(&self) -> KeeperResponse {
        self.managed.state.lock().await.releasing = true;
        self.managed.notify.notify_one();
        let closed = terminate_and_wait(
            &self.managed,
            Duration::from_millis(self.spec.cleanup_timeout_ms),
            &self.activity,
        ).await;
        match closed {
            Ok(()) => {
                self.released.store(true, Ordering::SeqCst);
                KeeperResponse::Released
            }
            Err(error) => KeeperResponse::Refused { error },
        }
    }
}

impl ManagedProcessState {
    fn new(max_buffered_output_bytes: usize, max_buffered_output_events: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            pending_bytes: 0,
            max_buffered_output_bytes,
            max_buffered_output_events,
            next_expected_seq: 1,
            exited: false,
            exit_code: None,
            exit_seq: None,
            closed: false,
            close_seq: None,
            timed_out: false,
            releasing: false,
            failure: None,
            termination_failure: None,
        }
    }

    fn failure(&self) -> Option<String> {
        match (&self.failure, &self.termination_failure) {
            (Some(failure), Some(termination)) => Some(format!("{failure}; {termination}")),
            (failure, termination) => failure.clone().or_else(|| termination.clone()),
        }
    }

    fn record(&mut self, event: ExecProcessEvent) -> bool {
        let Some(seq) = event_seq(&event) else {
            if let ExecProcessEvent::Failed(message) = event {
                self.fail(format!(
                    "persistent coder process event stream failed: {message}"
                ));
            }
            return true;
        };
        if seq != self.next_expected_seq {
            self.fail(format!(
                "persistent coder process event history has a gap: expected seq {}, received {}",
                self.next_expected_seq, seq
            ));
        }
        self.next_expected_seq = match seq.checked_add(1) {
            Some(next) => next,
            None => {
                self.fail("persistent coder process event sequence exhausted".to_string());
                u64::MAX
            }
        };
        let mut terminate = self.failure.is_some();
        match event {
            ExecProcessEvent::Output(chunk) => {
                let event_available = self.pending.len() < self.max_buffered_output_events;
                let available = match self
                    .max_buffered_output_bytes
                    .checked_sub(self.pending_bytes)
                {
                    Some(available) => available,
                    None => {
                        self.fail(
                            "persistent coder unread output accounting exceeded its descriptor ceiling"
                                .to_string(),
                        );
                        terminate = true;
                        0
                    }
                };
                let retained = if event_available {
                    chunk.chunk.0.len().min(available)
                } else {
                    0
                };
                if retained > 0 {
                    self.pending.push_back(PendingChunk {
                        seq,
                        stream: chunk.stream,
                        bytes: chunk.chunk.0[..retained].to_vec(),
                        source_offset: 0,
                        source_complete: retained == chunk.chunk.0.len(),
                    });
                    self.pending_bytes += retained;
                }
                if !event_available && !chunk.chunk.0.is_empty() {
                    self.fail(format!(
                        "persistent coder process output exceeded the descriptor-owned {} unread event ceiling",
                        self.max_buffered_output_events
                    ));
                    terminate = true;
                } else if retained != chunk.chunk.0.len() {
                    self.fail(format!(
                        "persistent coder process output exceeded the descriptor-owned {} byte unread buffer ceiling",
                        self.max_buffered_output_bytes
                    ));
                    terminate = true;
                }
            }
            ExecProcessEvent::Exited { exit_code, .. } => {
                self.exited = true;
                self.exit_code = Some(exit_code);
                self.exit_seq = Some(seq);
            }
            ExecProcessEvent::Closed { .. } => {
                self.closed = true;
                self.close_seq = Some(seq);
            }
            ExecProcessEvent::Failed(_) => unreachable!(),
        }
        terminate
    }

    fn fail(&mut self, message: String) {
        if self.failure.is_none() {
            self.failure = Some(message);
        }
    }

    fn drain(&mut self, max_output_bytes: usize) -> Vec<WorkspaceProcessChunk> {
        let mut remaining_budget = max_output_bytes;
        let mut drained = Vec::new();
        while remaining_budget > 0 {
            let Some(front) = self.pending.front_mut() else {
                break;
            };
            let take = front.bytes.len().min(remaining_budget);
            let start = front.source_offset;
            let mut bytes = if take == front.bytes.len() {
                std::mem::take(&mut front.bytes)
            } else {
                let unread = front.bytes.split_off(take);
                std::mem::replace(&mut front.bytes, unread)
            };
            bytes.shrink_to_fit();
            front.source_offset += take;
            let event_drained = front.bytes.is_empty();
            drained.push(WorkspaceProcessChunk {
                seq: front.seq,
                stream: front.stream,
                byte_offset: start,
                end_of_event: event_drained && front.source_complete,
                bytes,
            });
            self.pending_bytes -= take;
            remaining_budget -= take;
            if event_drained {
                self.pending.pop_front();
            }
        }
        drained
    }
}

async fn start_exec_process(
    process_id: &str,
    argv: Vec<String>,
    cwd: &AbsPath,
    env: BTreeMap<String, String>,
    tty: bool,
) -> Result<Arc<dyn ExecProcess>, String> {
    let runtime_paths = ExecServerRuntimePaths::new(
        std::env::current_exe().map_err(|error| {
            format!("persistent coder process executable is unavailable: {error}")
        })?,
        None,
    )
    .map_err(|error| format!("persistent coder process runtime paths are invalid: {error}"))?;
    let environment = Environment::create(None, runtime_paths)
        .map_err(|error| format!("persistent coder process environment failed: {error}"))?;
    environment
        .get_exec_backend()
        .start(ExecParams {
            process_id: ProcessId::new(process_id),
            argv,
            cwd: PathUri::from_path(cwd.as_path())
                .map_err(|error| format!("persistent coder process cwd URI failed: {error}"))?,
            env_policy: None,
            env: env.into_iter().collect(),
            tty,
            pipe_stdin: true,
            arg0: None,
        })
        .await
        .map(|started| started.process)
        .map_err(|error| format!("persistent coder process failed to start: {error}"))
}

fn spawn_event_collector(
    managed: Arc<ManagedProcess>,
    mut events: ExecProcessEventReceiver,
    activity: Arc<tokio::sync::Notify>,
) {
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    managed.state.lock().await.fail(format!(
                        "persistent coder process event collector lagged by {skipped} events"
                    ));
                    managed.notify.notify_one();
                    activity.notify_waiters();
                    let _ = request_termination(&managed, &activity).await;
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    managed.state.lock().await.fail(
                        "persistent coder process event stream closed before closure evidence"
                            .to_string(),
                    );
                    managed.notify.notify_one();
                    activity.notify_waiters();
                    break;
                }
            };
            let is_closed = matches!(event, ExecProcessEvent::Closed { .. });
            let terminate = managed.state.lock().await.record(event);
            managed.notify.notify_one();
            activity.notify_waiters();
            if terminate && !is_closed {
                let _ = request_termination(&managed, &activity).await;
            }
            if is_closed {
                break;
            }
        }
    });
}

fn spawn_process_timeout(
    managed: Arc<ManagedProcess>,
    process_timeout: Duration,
    cleanup_timeout: Duration,
    activity: Arc<tokio::sync::Notify>,
) {
    let managed = Arc::downgrade(&managed);
    tokio::spawn(async move {
        tokio::time::sleep(process_timeout).await;
        let Some(managed) = managed.upgrade() else {
            return;
        };
        let terminate = {
            let mut state = managed.state.lock().await;
            if state.closed {
                false
            } else {
                state.timed_out = true;
                true
            }
        };
        if terminate {
            managed.notify.notify_one();
            activity.notify_waiters();
            let _ = terminate_and_wait(&managed, cleanup_timeout, &activity).await;
        }
    });
}

async fn record_termination_failure(
    managed: &ManagedProcess,
    activity: &tokio::sync::Notify,
    error: String,
) -> String {
    managed.state.lock().await.termination_failure = Some(error.clone());
    managed.notify.notify_one();
    activity.notify_waiters();
    error
}

async fn request_termination(
    managed: &ManagedProcess,
    activity: &tokio::sync::Notify,
) -> Result<(), String> {
    match managed.process.terminate().await {
        Ok(_) => Ok(()),
        Err(error) => Err(record_termination_failure(managed, activity, format!(
            "persistent process '{}' termination failed: {error}", managed.process.process_id()
        )).await),
    }
}

async fn terminate_and_wait(
    managed: &ManagedProcess,
    cleanup_timeout: Duration,
    activity: &tokio::sync::Notify,
) -> Result<(), String> {
    let result = tokio::time::timeout(cleanup_timeout, async {
        if managed.state.lock().await.closed {
            return Ok(());
        }
        request_termination(managed, activity).await?;
        loop {
            let notified = activity.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if managed.state.lock().await.closed {
                return Ok(());
            }
            notified.await;
        }
    }).await;
    match result {
        Ok(Ok(())) => {
            managed.state.lock().await.termination_failure = None;
            Ok(())
        }
        Ok(Err(error)) => Err(error),
        Err(_) => Err(record_termination_failure(managed, activity, format!(
            "persistent process '{}' closure was not proven within {} ms",
            managed.process.process_id(), cleanup_timeout.as_millis()
        )).await),
    }
}

fn event_seq(event: &ExecProcessEvent) -> Option<u64> {
    match event {
        ExecProcessEvent::Output(chunk) => Some(chunk.seq),
        ExecProcessEvent::Exited { seq, .. } | ExecProcessEvent::Closed { seq } => Some(*seq),
        ExecProcessEvent::Failed(_) => None,
    }
}

fn validate_poll_output_ceiling(max_output_bytes: usize) -> Result<(), String> {
    if max_output_bytes == 0 {
        Err("persistent coder process output ceiling must be positive".to_string())
    } else {
        Ok(())
    }
}

pub fn resolve_persistent_process_program(
    argv: &mut [String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    let Some(program) = argv.first_mut() else {
        return Err("persistent coder process argv must not be empty".to_string());
    };
    if program.contains('/') || program.contains('\\') {
        return Ok(());
    }
    let path = env
        .get("PATH")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("persistent coder executable '{program}' requires PATH"))?;
    let resolved = std::env::split_paths(path)
        .map(|entry| {
            if entry.as_os_str().is_empty() {
                cwd.join(program.as_str())
            } else if entry.is_absolute() {
                entry.join(program.as_str())
            } else {
                cwd.join(entry).join(program.as_str())
            }
        })
        .find(|candidate| executable_file(candidate))
        .ok_or_else(|| format!("persistent coder executable '{program}' was not found in PATH"))?;
    *program = resolved.to_string_lossy().into_owned();
    Ok(())
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "persistent_termination_tests.rs"]
mod termination_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_exec::{ByteChunk, ProcessOutputChunk};

    #[test]
    fn collector_orders_output_exit_late_output_and_close_in_one_sequence() {
        let mut state = ManagedProcessState::new(16, 16);
        assert!(!state.record(ExecProcessEvent::Output(ProcessOutputChunk {
            seq: 1,
            stream: ExecOutputStream::Stdout,
            chunk: ByteChunk(b"ab".to_vec()),
        })));
        assert!(!state.record(ExecProcessEvent::Exited {
            seq: 2,
            exit_code: 7,
        }));
        assert!(!state.record(ExecProcessEvent::Output(ProcessOutputChunk {
            seq: 3,
            stream: ExecOutputStream::Stderr,
            chunk: ByteChunk(b"cd".to_vec()),
        })));
        assert!(!state.record(ExecProcessEvent::Closed { seq: 4 }));
        assert_eq!(state.failure, None);
        assert_eq!(state.next_expected_seq, 5);
        assert_eq!(state.exit_code, Some(7));
        assert!(state.closed);
    }

    #[test]
    fn closed_process_keeps_unread_tail_until_bounded_polls_drain_it() {
        let mut state = ManagedProcessState::new(16, 16);
        state.record(ExecProcessEvent::Output(ProcessOutputChunk {
            seq: 1,
            stream: ExecOutputStream::Stdout,
            chunk: ByteChunk(b"abcdef".to_vec()),
        }));
        state.record(ExecProcessEvent::Exited {
            seq: 2,
            exit_code: 0,
        });
        state.record(ExecProcessEvent::Closed { seq: 3 });
        let first = state.drain(2);
        assert_eq!(first[0].bytes, b"ab");
        assert!(!first[0].end_of_event);
        assert_eq!(state.pending_bytes, 4);
        let second = state.drain(4);
        assert_eq!(second[0].byte_offset, 2);
        assert_eq!(second[0].bytes, b"cdef");
        assert!(second[0].end_of_event);
        assert_eq!(state.pending_bytes, 0);
    }

    #[test]
    fn gap_and_unread_buffer_overflow_are_explicit_failures() {
        let mut gap = ManagedProcessState::new(8, 8);
        assert!(gap.record(ExecProcessEvent::Exited {
            seq: 2,
            exit_code: 0,
        }));
        assert!(gap.failure.unwrap().contains("expected seq 1"));

        let mut overflow = ManagedProcessState::new(3, 8);
        assert!(
            overflow.record(ExecProcessEvent::Output(ProcessOutputChunk {
                seq: 1,
                stream: ExecOutputStream::Stdout,
                chunk: ByteChunk(b"abcde".to_vec()),
            }))
        );
        assert_eq!(overflow.pending_bytes, 3);
        assert!(overflow.failure.unwrap().contains("unread buffer ceiling"));

        let mut event_overflow = ManagedProcessState::new(8, 1);
        event_overflow.record(ExecProcessEvent::Output(ProcessOutputChunk {
            seq: 1,
            stream: ExecOutputStream::Stdout,
            chunk: ByteChunk(b"a".to_vec()),
        }));
        assert!(
            event_overflow.record(ExecProcessEvent::Output(ProcessOutputChunk {
                seq: 2,
                stream: ExecOutputStream::Stdout,
                chunk: ByteChunk(b"b".to_vec()),
            }))
        );
        assert!(event_overflow.failure.unwrap().contains("event ceiling"));
    }
}
