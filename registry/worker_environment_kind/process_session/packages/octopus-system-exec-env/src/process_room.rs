use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use octopus_exec::{
    Environment, ExecOutputStream, ExecParams, ExecProcess, ExecProcessEvent,
    ExecServerRuntimePaths, ProcessId, ProcessSignal,
};
use octopus_interfaces::InterfaceArtifactFile;
use octopus_process_session_tool_runtime::room::{
    admitted_tool, decode_tool_call, emit_json_artifact, formed_scope, strict_object,
};
use octopus_process_session_tool_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo, WorkerEnvironmentLifecycleRecorder, WorkerEnvironmentLiveStateRecorder,
};
use octopus_tool_handlers::{ToolResultArtifact, ToolResultError};
use octopus_util_pathuri::PathUri;
use octopus_worker_env::{
    WorkerEnvKindSpec, WorkerEnvironmentFormation, WorkerEnvironmentId, WorkerEnvironmentLifecycle,
};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

const ROOM: &str = "system_exec_env";
const INTERFACE_LOCAL_IPC_ROOT_SLOT: &str = "interface_local_ipc_root";

pub struct ProcessRoom {
    environment: ToolProcessEnvironment,
    rooms: tokio::sync::Mutex<BTreeMap<WorkerEnvironmentId, Arc<tokio::sync::Mutex<RoomState>>>>,
}

struct RoomState {
    formation: WorkerEnvironmentFormation,
    recorder: Arc<dyn ProcessRoomRecorder>,
    exec_environment: Environment,
    shell: String,
    generation: u64,
    process_id: Option<ProcessId>,
    process: Option<Arc<dyn ExecProcess>>,
    launch: Option<SafeLaunchSpec>,
    status: String,
    exit_code: Option<i32>,
    closed: bool,
    failure: Option<String>,
    workspace: PathBuf,
    transcript: PathBuf,
    viewer_socket: PathBuf,
    viewer_token: String,
    viewer_task: Option<tokio::task::JoinHandle<()>>,
    generation_guard: Arc<AtomicU64>,
}

#[derive(Clone)]
struct SafeLaunchSpec {
    argv: Vec<String>,
    env: HashMap<String, String>,
    tty: bool,
    label: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProcessOperation {
    Start,
    Read,
    Write,
    Signal,
    Terminate,
    List,
    Run,
    FileRead,
    FileWrite,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessHandlerConfig {
    operation: ProcessOperation,
}

trait ProcessRoomRecorder:
    WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder
{
}

impl<T> ProcessRoomRecorder for T where
    T: WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder
{
}

impl ProcessRoom {
    pub fn new(environment: ToolProcessEnvironment) -> Self {
        Self {
            environment,
            rooms: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }

    async fn start_from_spec(
        room: &Arc<tokio::sync::Mutex<RoomState>>,
        launch: SafeLaunchSpec,
    ) -> Result<(Arc<dyn ExecProcess>, ProcessId, u64), String> {
        let mut state = room.lock().await;
        if state.exit_code.is_none() && !state.closed {
            if let Some(process) = state.process.as_ref() {
                let observed = process
                    .read(None, Some(1), Some(0))
                    .await
                    .map_err(|error| {
                        format!("process room could not inspect its process: {error}")
                    })?;
                if !observed.exited && !observed.closed {
                    return Err("terminal process is still running".to_string());
                }
            }
        }
        let generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| "process room generation overflowed".to_string())?;
        let process_id = ProcessId::new(format!(
            "{}-{}",
            state.formation.worker_environment_id.as_str(),
            generation
        ));
        let started = state
            .exec_environment
            .get_exec_backend()
            .start(ExecParams {
                process_id: process_id.clone(),
                argv: launch.argv.clone(),
                cwd: PathUri::from_path(&state.workspace)
                    .map_err(|error| format!("process room workspace URI failed: {error}"))?,
                env_policy: None,
                env: launch.env.clone(),
                tty: launch.tty,
                pipe_stdin: true,
                arg0: None,
            })
            .await
            .map_err(|error| format!("process start failed: {error}"))?;
        std::fs::write(&state.transcript, [])
            .map_err(|error| format!("terminal transcript reset failed: {error}"))?;
        state.generation = generation;
        state.generation_guard.store(generation, Ordering::Release);
        state.process_id = Some(process_id.clone());
        state.process = Some(Arc::clone(&started.process));
        state.launch = Some(launch);
        state.status = "running".to_string();
        state.exit_code = None;
        state.closed = false;
        state.failure = None;
        Ok((started.process, process_id, generation))
    }

    async fn receive(
        &self,
        invocation: ToolProcessInvocation,
        recorder: Arc<dyn ProcessRoomRecorder>,
    ) -> Result<InterfaceArtifactFile, ToolProcessError> {
        let ToolProcessInvocation {
            descriptor,
            envelope,
            worker_environment,
            ..
        } = invocation;
        let formation = worker_environment.ok_or_else(|| {
            ToolProcessError::Capability(
                "persistent process invocation has no formed worker environment".to_string(),
            )
        })?;
        let call = decode_tool_call(ROOM, &envelope)?;
        let tool = admitted_tool(ROOM, &envelope, &call)
            .map_err(|error| ToolProcessError::Capability(error.to_string()))?;
        if descriptor.id.as_str() != tool {
            return Err(ToolProcessError::Capability(format!(
                "tool descriptor '{}' does not match tool.call '{}'",
                descriptor.id, tool
            )));
        }
        let operation = process_operation(&descriptor.handler_config, descriptor.id.as_str())?;
        formed_scope(ROOM, &envelope, &formation)?;
        let (_workspace, _transcript, inserted) = self
            .materialize_room(&formation, Arc::clone(&recorder))
            .await
            .map_err(|error| {
                ToolProcessError::Unavailable(format!("process room failed to open: {error}"))
            })?;
        recorder
            .record(&formation, WorkerEnvironmentLifecycle::Active)
            .await
            .map_err(ToolProcessError::Unavailable)?;
        if inserted {
            let room = self
                .room(&formation)
                .await
                .map_err(ToolProcessError::Unavailable)?;
            let initial_live_state = {
                let state = room.lock().await;
                room_live_state(&state)
            };
            recorder
                .record_live_state(&formation, &initial_live_state)
                .await
                .map_err(ToolProcessError::Unavailable)?;
        }
        let correlation_id = call.correlation_id;
        let response = match self
            .dispatch(
                &formation,
                operation,
                &call.arguments,
                Arc::clone(&recorder),
            )
            .await
        {
            Ok(body) => ToolResultArtifact::success(&tool, correlation_id, body),
            Err(error) => ToolResultArtifact::failure(
                &tool,
                correlation_id,
                ToolResultError::new("tool_execution_failed", error),
            ),
        };
        emit_json_artifact(&envelope, &response.label, &response)
            .map_err(ToolProcessError::Capability)
    }

    async fn dispatch(
        &self,
        formation: &WorkerEnvironmentFormation,
        operation: ProcessOperation,
        arguments: &Value,
        recorder: Arc<dyn ProcessRoomRecorder>,
    ) -> Result<Value, String> {
        match operation {
            ProcessOperation::Start => self.start(formation, arguments, recorder).await,
            ProcessOperation::Read => self.read(formation, arguments).await,
            ProcessOperation::Write => self.write(formation, arguments).await,
            ProcessOperation::Signal => self.signal(formation, arguments).await,
            ProcessOperation::Terminate => self.terminate(formation, arguments).await,
            ProcessOperation::List => self.list(formation, arguments).await,
            ProcessOperation::Run => self.exec_run(formation, arguments, recorder).await,
            ProcessOperation::FileRead => self.file_read(formation, arguments).await,
            ProcessOperation::FileWrite => self.file_write(formation, arguments).await,
        }
    }

    async fn start(
        &self,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
        recorder: Arc<dyn ProcessRoomRecorder>,
    ) -> Result<Value, String> {
        let object = strict_object(
            "process.start",
            arguments,
            &["command", "argv", "tty", "env"],
        )?;
        let (workspace, transcript) = self.ensure_room(formation).await?;
        let room = self
            .rooms
            .lock()
            .await
            .get(&formation.worker_environment_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "worker environment '{}' disappeared while starting its process",
                    formation.worker_environment_id
                )
            })?;
        let id = &formation.worker_environment_id;
        let (shell, room_workspace) = {
            let state = room.lock().await;
            if state.workspace != workspace || state.transcript != transcript {
                return Err(format!(
                    "worker environment '{}' changed its private room coordinates",
                    id
                ));
            }
            (state.shell.clone(), state.workspace.clone())
        };
        let mut argv = process_argv(object, &shell)?;
        let tty = optional_bool(object, "tty")?.unwrap_or(true);
        let mut env = optional_string_map(object, "env")?.unwrap_or_default();
        if let Ok(path) = std::env::var("PATH") {
            env.entry("PATH".to_string()).or_insert(path);
        }
        env.insert(
            "HOME".to_string(),
            room_workspace.to_string_lossy().to_string(),
        );
        env.entry("TERM".to_string())
            .or_insert_with(|| "xterm-256color".to_string());
        resolve_argv_program(&mut argv, &env, &room_workspace)?;
        let launch = SafeLaunchSpec {
            argv: argv.clone(),
            env: env.clone(),
            tty,
            label: if object.contains_key("command") || object.contains_key("argv") {
                "Command"
            } else {
                "Login shell"
            },
        };
        let (process, process_id, generation) = Self::start_from_spec(&room, launch).await?;
        let (projected_state, transcript_path, generation_guard, workspace_path, viewer_socket) = {
            let state = room.lock().await;
            (
                room_live_state(&state),
                state.transcript.clone(),
                Arc::clone(&state.generation_guard),
                state.workspace.clone(),
                state.viewer_socket.clone(),
            )
        };
        recorder
            .record_live_state(formation, &projected_state)
            .await?;
        spawn_transcript_writer(
            Arc::clone(&process),
            formation.clone(),
            transcript_path,
            generation,
            generation_guard,
            Arc::clone(&room),
            recorder,
        );
        Ok(json!({
            "worker_environment_id": id,
            "process_id": process_id,
            "generation": generation,
            "tty": tty,
            "workspace": workspace_path,
            "viewer": {
                "protocol": "process_session.direct.v1",
                "address": viewer_socket
            }
        }))
    }

    async fn read(
        &self,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object(
            "process.read",
            arguments,
            &["after_seq", "max_bytes", "wait_ms"],
        )?;
        let after_seq = optional_u64(object, "after_seq")?;
        let max_bytes = bounded_optional_u64(object, "max_bytes", 1, 1_048_576)?
            .map(|value| usize::try_from(value).map_err(|_| "max_bytes exceeds usize".to_string()))
            .transpose()?
            .or(Some(65_536));
        let wait_ms = bounded_optional_u64(object, "wait_ms", 0, 30_000)?.or(Some(0));
        let process = self.process(formation).await?;
        let response = process
            .read(after_seq, max_bytes, wait_ms)
            .await
            .map_err(|error| format!("process read failed: {error}"))?;
        let chunks = response
            .chunks
            .iter()
            .map(|chunk| {
                json!({
                    "seq": chunk.seq,
                    "stream": chunk.stream,
                    "text": String::from_utf8_lossy(&chunk.chunk.0),
                    "bytes": chunk.chunk,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "process_id": process.process_id(),
            "chunks": chunks,
            "next_seq": response.next_seq,
            "exited": response.exited,
            "exit_code": response.exit_code,
            "closed": response.closed,
            "failure": response.failure,
        }))
    }

    async fn write(
        &self,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object("process.write", arguments, &["text"])?;
        let text = required_string(object, "text")?;
        if text.len() > 1_048_576 {
            return Err("process.write text exceeds 1048576 bytes".to_string());
        }
        let process = self.process(formation).await?;
        let response = process
            .write(text.as_bytes().to_vec())
            .await
            .map_err(|error| format!("process write failed: {error}"))?;
        Ok(json!({
            "process_id": process.process_id(),
            "status": response.status,
            "bytes_written": text.len(),
        }))
    }

    async fn signal(
        &self,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object("process.signal", arguments, &["signal"])?;
        let signal = required_string(object, "signal")?;
        if signal != "interrupt" {
            return Err(format!("process.signal does not support signal '{signal}'"));
        }
        let process = self.process(formation).await?;
        let response = process
            .signal(ProcessSignal::Interrupt)
            .await
            .map_err(|error| format!("process signal failed: {error}"))?;
        if !response.delivered {
            return Err("process signal was not delivered to a running process".to_string());
        }
        Ok(json!({
            "process_id": process.process_id(),
            "signal": signal,
            "accepted": true,
        }))
    }

    async fn terminate(
        &self,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
    ) -> Result<Value, String> {
        strict_object("process.terminate", arguments, &[])?;
        let process = self.process(formation).await?;
        let response = process
            .terminate()
            .await
            .map_err(|error| format!("process terminate failed: {error}"))?;
        if !response.running {
            return Err("process termination was not delivered to a running process".to_string());
        }
        Ok(json!({
            "process_id": process.process_id(),
            "termination_requested": true,
        }))
    }

    async fn list(
        &self,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
    ) -> Result<Value, String> {
        strict_object("process.list", arguments, &[])?;
        self.ensure_room(formation).await?;
        let room = self.room(formation).await?;
        let state = room.lock().await;
        Ok(json!({
            "worker_environment_id": formation.worker_environment_id,
            "generation": state.generation,
            "process_id": state.process_id,
            "status": state.status,
            "exit_code": state.exit_code,
            "closed": state.closed,
            "workspace": state.workspace,
            "viewer": {
                "protocol": "process_session.direct.v1",
                "address": state.viewer_socket
            }
        }))
    }

    async fn exec_run(
        &self,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
        recorder: Arc<dyn ProcessRoomRecorder>,
    ) -> Result<Value, String> {
        let object = strict_object(
            "exec.run",
            arguments,
            &["command", "env", "timeout_ms", "max_output_bytes"],
        )?;
        let command = object
            .get("command")
            .ok_or_else(|| "exec.run command is required".to_string())?;
        let mut start = Map::new();
        match command {
            Value::String(command) if !command.is_empty() => {
                start.insert("command".to_string(), Value::String(command.clone()));
            }
            Value::Array(_) => {
                let argv = optional_string_array(object, "command")?
                    .ok_or_else(|| "exec.run command is required".to_string())?;
                if argv.is_empty() {
                    return Err("exec.run command array must not be empty".to_string());
                }
                start.insert(
                    "argv".to_string(),
                    Value::Array(argv.into_iter().map(Value::String).collect()),
                );
            }
            _ => {
                return Err(
                    "exec.run command must be a non-empty string or string array".to_string(),
                );
            }
        }
        if let Some(env) = object.get("env") {
            optional_string_map(object, "env")?;
            start.insert("env".to_string(), env.clone());
        }
        start.insert("tty".to_string(), Value::Bool(false));
        let timeout_ms = bounded_optional_u64(object, "timeout_ms", 1, 600_000)?.unwrap_or(120_000);
        let max_output_bytes =
            bounded_optional_u64(object, "max_output_bytes", 1, 16 * 1024 * 1024)?
                .unwrap_or(4 * 1024 * 1024);
        let max_output_bytes = usize::try_from(max_output_bytes)
            .map_err(|_| "exec.run max_output_bytes exceeds usize".to_string())?;
        self.start(formation, &Value::Object(start), recorder)
            .await?;
        let process = self.process(formation).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let mut after_seq = Some(0u64);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut truncated = false;
        loop {
            let remaining_time = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining_time.is_zero() {
                let _ = process.terminate().await;
                return Ok(exec_run_result(
                    process.process_id(),
                    false,
                    None,
                    Some("timeout"),
                    &stdout,
                    &stderr,
                    truncated,
                ));
            }
            let captured = stdout.len().saturating_add(stderr.len());
            if captured >= max_output_bytes {
                let _ = process.terminate().await;
                return Ok(exec_run_result(
                    process.process_id(),
                    false,
                    None,
                    Some("output_limit"),
                    &stdout,
                    &stderr,
                    true,
                ));
            }
            let wait_ms = remaining_time
                .min(Duration::from_millis(1_000))
                .as_millis()
                .try_into()
                .unwrap_or(1_000);
            let response = process
                .read(after_seq, Some(max_output_bytes - captured), Some(wait_ms))
                .await
                .map_err(|error| format!("exec.run output read failed: {error}"))?;
            if let Some(failure) = response.failure.as_deref() {
                let _ = process.terminate().await;
                return Err(format!("exec.run process failed: {failure}"));
            }
            let mut expected_seq = after_seq.unwrap_or(0).saturating_add(1);
            for chunk in response.chunks {
                if chunk.seq != expected_seq {
                    let _ = process.terminate().await;
                    return Ok(exec_run_result(
                        process.process_id(),
                        false,
                        response.exit_code,
                        Some("output_history_gap"),
                        &stdout,
                        &stderr,
                        true,
                    ));
                }
                expected_seq = expected_seq.saturating_add(1);
                after_seq = Some(chunk.seq);
                let remaining =
                    max_output_bytes.saturating_sub(stdout.len().saturating_add(stderr.len()));
                let take = remaining.min(chunk.chunk.0.len());
                let target = match chunk.stream {
                    ExecOutputStream::Stdout | ExecOutputStream::Pty => &mut stdout,
                    ExecOutputStream::Stderr => &mut stderr,
                };
                target.extend_from_slice(&chunk.chunk.0[..take]);
                if take < chunk.chunk.0.len() {
                    truncated = true;
                    break;
                }
            }
            if truncated {
                let _ = process.terminate().await;
                return Ok(exec_run_result(
                    process.process_id(),
                    false,
                    response.exit_code,
                    Some("output_limit"),
                    &stdout,
                    &stderr,
                    true,
                ));
            }
            if response.closed {
                let Some(exit_code) = response.exit_code else {
                    return Ok(exec_run_result(
                        process.process_id(),
                        false,
                        None,
                        Some("closed_without_exit_status"),
                        &stdout,
                        &stderr,
                        false,
                    ));
                };
                return Ok(exec_run_result(
                    process.process_id(),
                    true,
                    Some(exit_code),
                    None,
                    &stdout,
                    &stderr,
                    false,
                ));
            }
            after_seq = response.next_seq.checked_sub(1).or(after_seq);
        }
    }

    async fn file_read(
        &self,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object("exec.file_read", arguments, &["path", "max_bytes"])?;
        let path = required_string(object, "path")?;
        let max_bytes =
            bounded_optional_u64(object, "max_bytes", 1, 16 * 1024 * 1024)?.unwrap_or(1024 * 1024);
        let max_bytes = usize::try_from(max_bytes)
            .map_err(|_| "exec.file_read max_bytes exceeds usize".to_string())?;
        let (workspace, _) = self.ensure_room(formation).await?;
        let relative = normalized_relative_path("exec.file_read", path)?;
        let absolute = resolve_existing_workspace_file(&workspace, &relative)?;
        let metadata = tokio::fs::metadata(&absolute)
            .await
            .map_err(|error| format!("exec.file_read cannot inspect '{}': {error}", path))?;
        let file = tokio::fs::File::open(&absolute)
            .await
            .map_err(|error| format!("exec.file_read cannot open '{}': {error}", path))?;
        let limit = u64::try_from(max_bytes)
            .map_err(|_| "exec.file_read max_bytes exceeds u64".to_string())?;
        let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
        file.take(limit)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| format!("exec.file_read cannot read '{}': {error}", path))?;
        let text = std::str::from_utf8(&bytes).ok();
        Ok(json!({
            "result": "file_read",
            "path": relative,
            "file_size": metadata.len(),
            "bytes_read": bytes.len(),
            "truncated": metadata.len() > bytes.len() as u64,
            "content": text,
            "content_base64": BASE64_STANDARD.encode(&bytes),
        }))
    }

    async fn file_write(
        &self,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object(
            "exec.file_write",
            arguments,
            &["path", "content", "content_base64", "create_parents"],
        )?;
        let path = required_string(object, "path")?;
        let content = object.get("content");
        let content_base64 = object.get("content_base64");
        let bytes = match (content, content_base64) {
            (Some(Value::String(content)), None) => content.as_bytes().to_vec(),
            (None, Some(Value::String(content))) => BASE64_STANDARD
                .decode(content)
                .map_err(|error| format!("exec.file_write content_base64 is invalid: {error}"))?,
            (Some(_), None) => {
                return Err("exec.file_write content must be a string".to_string());
            }
            (None, Some(_)) => {
                return Err("exec.file_write content_base64 must be a string".to_string());
            }
            (Some(_), Some(_)) => {
                return Err(
                    "exec.file_write accepts exactly one of content or content_base64".to_string(),
                );
            }
            (None, None) => {
                return Err(
                    "exec.file_write requires exactly one of content or content_base64".to_string(),
                );
            }
        };
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("exec.file_write content exceeds 16777216 bytes".to_string());
        }
        let create_parents = optional_bool(object, "create_parents")?.unwrap_or(false);
        let (workspace, _) = self.ensure_room(formation).await?;
        let relative = normalized_relative_path("exec.file_write", path)?;
        let absolute = resolve_workspace_write_path(&workspace, &relative, create_parents)?;
        tokio::fs::write(&absolute, &bytes)
            .await
            .map_err(|error| format!("exec.file_write cannot write '{}': {error}", path))?;
        Ok(json!({
            "result": "file_written",
            "path": relative,
            "bytes_written": bytes.len(),
        }))
    }

    async fn ensure_room(
        &self,
        formation: &WorkerEnvironmentFormation,
    ) -> Result<(PathBuf, PathBuf), String> {
        let room = self.room(formation).await?;
        let state = room.lock().await;
        Ok((state.workspace.clone(), state.transcript.clone()))
    }

    async fn materialize_room(
        &self,
        formation: &WorkerEnvironmentFormation,
        recorder: Arc<dyn ProcessRoomRecorder>,
    ) -> Result<(PathBuf, PathBuf, bool), String> {
        let id = &formation.worker_environment_id;
        let root = self.environment.var_dir.join("rooms").join(id.as_str());
        let workspace = root.join("workspace");
        let transcript = root.join("transcript.bin");
        let viewer_root = self
            .environment
            .root_slot(INTERFACE_LOCAL_IPC_ROOT_SLOT)?
            .to_path_buf();
        let viewer_digest = blake3::hash(id.as_str().as_bytes()).to_hex().to_string();
        let viewer_key = &viewer_digest[..20];
        let viewer_socket = viewer_root.join(format!("p-{viewer_key}.sock"));
        let viewer_auth = viewer_socket.with_extension("auth");
        std::fs::create_dir_all(&workspace).map_err(|error| {
            format!(
                "process room workspace '{}' cannot be created: {error}",
                workspace.display()
            )
        })?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript)
            .map_err(|error| {
                format!(
                    "process transcript '{}' cannot be created: {error}",
                    transcript.display()
                )
            })?;
        std::fs::create_dir_all(&viewer_root).map_err(|error| {
            format!(
                "process viewer root '{}' cannot be created: {error}",
                viewer_root.display()
            )
        })?;
        let mut rooms = self.rooms.lock().await;
        let inserted = !rooms.contains_key(id);
        if inserted {
            let runtime_paths = ExecServerRuntimePaths::new(
                std::env::current_exe().map_err(|error| {
                    format!("process room current executable is unavailable: {error}")
                })?,
                None,
            )
            .map_err(|error| format!("process room runtime paths are invalid: {error}"))?;
            let exec_environment = Environment::create(None, runtime_paths)
                .map_err(|error| format!("process room environment failed to form: {error}"))?;
            let shell = exec_environment
                .info()
                .await
                .map_err(|error| format!("process room shell discovery failed: {error}"))?
                .shell
                .path;
            let viewer_token = viewer_token()?;
            remove_private_file(&viewer_auth)?;
            write_private_text(&viewer_auth, &viewer_token)?;
            let room = Arc::new(tokio::sync::Mutex::new(RoomState {
                formation: formation.clone(),
                recorder,
                exec_environment,
                shell,
                generation: 0,
                process_id: None,
                process: None,
                launch: None,
                status: "ready".to_string(),
                exit_code: None,
                closed: false,
                failure: None,
                workspace: workspace.clone(),
                transcript: transcript.clone(),
                viewer_socket: viewer_socket.clone(),
                viewer_token,
                viewer_task: None,
                generation_guard: Arc::new(AtomicU64::new(0)),
            }));
            let task = match spawn_viewer_listener(Arc::clone(&room)).await {
                Ok(task) => task,
                Err(error) => {
                    let _ = remove_private_file(&viewer_auth);
                    return Err(error);
                }
            };
            room.lock().await.viewer_task = Some(task);
            rooms.insert(id.clone(), room);
        } else if let Some(room) = rooms.get(id) {
            let mut state = room.lock().await;
            if state.formation.worker_environment_id != formation.worker_environment_id
                || state.formation.run != formation.run
                || state.formation.ee != formation.ee
                || state.formation.worker != formation.worker
                || state.formation.door_interface != formation.door_interface
            {
                return Err(format!(
                    "worker environment '{}' formation changed while its process room was live",
                    id
                ));
            }
            state.recorder = recorder;
        }
        drop(rooms);
        Ok((workspace, transcript, inserted))
    }

    async fn room(
        &self,
        formation: &WorkerEnvironmentFormation,
    ) -> Result<Arc<tokio::sync::Mutex<RoomState>>, String> {
        self.rooms
            .lock()
            .await
            .get(&formation.worker_environment_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "worker environment '{}' has no process room",
                    formation.worker_environment_id
                )
            })
    }

    async fn process(
        &self,
        formation: &WorkerEnvironmentFormation,
    ) -> Result<Arc<dyn ExecProcess>, String> {
        let room = self.room(formation).await?;
        let process = {
            let state = room.lock().await;
            state.process.as_ref().map(Arc::clone)
        };
        process.ok_or_else(|| {
            format!(
                "worker environment '{}' has no process; call process.start first",
                formation.worker_environment_id
            )
        })
    }

    async fn release_room(&self, formation: &WorkerEnvironmentFormation) -> Result<(), String> {
        let Some(room) = self
            .rooms
            .lock()
            .await
            .remove(&formation.worker_environment_id)
        else {
            return Ok(());
        };
        let mut state = room.lock().await;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| "process room generation overflowed during release".to_string())?;
        state
            .generation_guard
            .store(state.generation, Ordering::Release);
        if let Some(task) = state.viewer_task.take() {
            task.abort();
        }
        remove_private_socket(&state.viewer_socket)?;
        remove_private_file(&state.viewer_socket.with_extension("auth"))?;
        let Some(process) = state.process.take() else {
            return Ok(());
        };
        let running = process
            .read(None, Some(1), Some(0))
            .await
            .map(|status| !status.exited && !status.closed)
            .unwrap_or(true);
        if running {
            let response = process
                .terminate()
                .await
                .map_err(|error| format!("process room release failed: {error}"))?;
            if !response.running {
                return Err("process room release could not prove termination".to_string());
            }
        }
        Ok(())
    }

    async fn quiesce_all(&self) -> Result<(), String> {
        let rooms = {
            let mut rooms = self.rooms.lock().await;
            std::mem::take(&mut *rooms)
                .into_values()
                .collect::<Vec<_>>()
        };
        for room in rooms {
            let mut state = room.lock().await;
            if let Some(task) = state.viewer_task.take() {
                task.abort();
            }
            remove_private_socket(&state.viewer_socket)?;
            remove_private_file(&state.viewer_socket.with_extension("auth"))?;
            if let Some(process) = state.process.take() {
                let running = process
                    .read(None, Some(1), Some(0))
                    .await
                    .map(|status| !status.exited && !status.closed)
                    .unwrap_or(true);
                if running {
                    let response = process
                        .terminate()
                        .await
                        .map_err(|error| format!("process room quiesce failed: {error}"))?;
                    if !response.running {
                        return Err("process room quiesce could not prove termination".to_string());
                    }
                }
            }
        }
        Ok(())
    }
}

impl ToolProcessHandler for ProcessRoom {
    fn substrate_family(&self) -> &str {
        "persistent_process"
    }

    fn releases_cold(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolProcessInvocation,
        io: ToolProcessIo,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<InterfaceArtifactFile>, ToolProcessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.receive(invocation, Arc::new(io))
                .await
                .map(|artifact| vec![artifact])
        })
    }

    fn release<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        _kind: &'a WorkerEnvKindSpec,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.release_room(formation)
                .await
                .map_err(ToolProcessError::Unavailable)
        })
    }

    fn quiesce<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.quiesce_all()
                .await
                .map_err(ToolProcessError::Unavailable)
        })
    }
}

fn exec_run_result(
    process_id: &ProcessId,
    completed: bool,
    exit_code: Option<i32>,
    termination_reason: Option<&str>,
    stdout: &[u8],
    stderr: &[u8],
    truncated: bool,
) -> Value {
    json!({
        "result": if completed { "command_finished" } else { "command_stopped" },
        "process_id": process_id,
        "completed": completed,
        "termination_reason": termination_reason,
        "output": {
            "exit_code": exit_code,
            "stdout": String::from_utf8_lossy(stdout),
            "stderr": String::from_utf8_lossy(stderr),
            "stdout_base64": BASE64_STANDARD.encode(stdout),
            "stderr_base64": BASE64_STANDARD.encode(stderr),
            "byte_count": stdout.len().saturating_add(stderr.len()),
            "truncated": truncated,
        }
    })
}

fn normalized_relative_path(tool: &str, raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.contains('\0') {
        return Err(format!("{tool} path must be a non-empty relative path"));
    }
    let mut relative = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "{tool} path '{}' escapes the private process workspace",
                    raw
                ));
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err(format!("{tool} path must name a file"));
    }
    Ok(relative)
}

fn canonical_workspace(workspace: &Path) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(workspace).map_err(|error| {
        format!(
            "private process workspace '{}' cannot be canonicalized: {error}",
            workspace.display()
        )
    })?;
    let metadata = std::fs::symlink_metadata(workspace).map_err(|error| {
        format!(
            "private process workspace '{}' cannot be inspected: {error}",
            workspace.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "private process workspace '{}' is not a real directory",
            workspace.display()
        ));
    }
    Ok(canonical)
}

fn reject_symlink_components(workspace: &Path, relative: &Path) -> Result<(), String> {
    let mut current = workspace.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err("private process path contains a non-normal component".to_string());
        };
        current.push(part);
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            format!(
                "private process path '{}' cannot be inspected: {error}",
                current.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "private process path '{}' traverses a symbolic link",
                current.display()
            ));
        }
    }
    Ok(())
}

fn resolve_existing_workspace_file(workspace: &Path, relative: &Path) -> Result<PathBuf, String> {
    reject_symlink_components(workspace, relative)?;
    let root = canonical_workspace(workspace)?;
    let candidate = std::fs::canonicalize(workspace.join(relative)).map_err(|error| {
        format!(
            "private process file '{}' cannot be resolved: {error}",
            relative.display()
        )
    })?;
    if !candidate.starts_with(&root) {
        return Err(format!(
            "private process file '{}' resolves outside its workspace",
            relative.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(&candidate).map_err(|error| {
        format!(
            "private process file '{}' cannot be inspected: {error}",
            relative.display()
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "private process path '{}' is not a regular file",
            relative.display()
        ));
    }
    Ok(candidate)
}

fn resolve_workspace_write_path(
    workspace: &Path,
    relative: &Path,
    create_parents: bool,
) -> Result<PathBuf, String> {
    let root = canonical_workspace(workspace)?;
    let parent_relative = relative.parent().unwrap_or_else(|| Path::new(""));
    let mut parent = root.clone();
    for component in parent_relative.components() {
        let Component::Normal(part) = component else {
            return Err("private process path contains a non-normal component".to_string());
        };
        parent.push(part);
        match std::fs::symlink_metadata(&parent) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "private process write parent '{}' is not a real directory",
                        parent.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_parents => {
                std::fs::create_dir(&parent).map_err(|error| {
                    format!(
                        "private process write parent '{}' cannot be created: {error}",
                        parent.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "private process write parent '{}' is unavailable: {error}",
                    parent.display()
                ));
            }
        }
        let canonical_parent = std::fs::canonicalize(&parent).map_err(|error| {
            format!(
                "private process write parent '{}' cannot be resolved: {error}",
                parent.display()
            )
        })?;
        if !canonical_parent.starts_with(&root) {
            return Err(format!(
                "private process write parent '{}' resolves outside its workspace",
                parent.display()
            ));
        }
    }
    let target = root.join(relative);
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "private process write target '{}' is not a regular file",
                    relative.display()
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "private process write target '{}' cannot be inspected: {error}",
                relative.display()
            ));
        }
    }
    Ok(target)
}

fn process_argv(object: &Map<String, Value>, shell: &str) -> Result<Vec<String>, String> {
    let command = match object.get("command") {
        None => None,
        Some(Value::String(command)) if !command.trim().is_empty() => Some(vec![
            shell.to_string(),
            "-lc".to_string(),
            command.to_string(),
        ]),
        Some(Value::Array(items)) => {
            let argv = items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    item.as_str()
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| {
                            format!("process.start command[{index}] must be a non-empty string")
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if argv.is_empty() {
                return Err("process.start command array must not be empty".to_string());
            }
            Some(argv)
        }
        Some(Value::String(_)) => {
            return Err("process.start command must not be empty".to_string());
        }
        Some(_) => {
            return Err("process.start command must be a string or string array".to_string());
        }
    };
    let argv = optional_string_array(object, "argv")?;
    match (command, argv) {
        (Some(_), Some(_)) => Err("process.start accepts command or argv, not both".to_string()),
        (Some(command), None) => Ok(command),
        (None, Some(argv)) if argv.is_empty() => {
            Err("process.start argv must not be empty".to_string())
        }
        (None, Some(argv)) => Ok(argv),
        (None, None) => Ok(vec![shell.to_string(), "-l".to_string()]),
    }
}

fn resolve_argv_program(
    argv: &mut [String],
    env: &HashMap<String, String>,
    workspace: &Path,
) -> Result<(), String> {
    let Some(program) = argv.first_mut() else {
        return Err("process argv must not be empty".to_string());
    };
    if program.contains('/') || program.contains('\\') {
        return Ok(());
    }
    let path = env
        .get("PATH")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("process executable '{program}' requires a non-empty PATH"))?;
    let resolved = std::env::split_paths(path)
        .map(|entry| {
            if entry.as_os_str().is_empty() {
                workspace.join(program.as_str())
            } else if entry.is_absolute() {
                entry.join(program.as_str())
            } else {
                workspace.join(entry).join(program.as_str())
            }
        })
        .find(|candidate| is_executable_file(candidate))
        .ok_or_else(|| format!("process executable '{program}' was not found in PATH"))?;
    *program = resolved.to_string_lossy().into_owned();
    Ok(())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

fn spawn_transcript_writer(
    process: Arc<dyn ExecProcess>,
    formation: WorkerEnvironmentFormation,
    transcript: PathBuf,
    generation: u64,
    generation_guard: Arc<AtomicU64>,
    room: Arc<tokio::sync::Mutex<RoomState>>,
    recorder: Arc<dyn ProcessRoomRecorder>,
) {
    let mut events = process.subscribe_events();
    let replay_truncated = events.replay_truncated();
    tokio::spawn(async move {
        if replay_truncated {
            update_room_lifecycle(
                &room,
                generation,
                "failed",
                None,
                false,
                Some(
                    "terminal transcript began after retained process output was truncated"
                        .to_string(),
                ),
            )
            .await;
            record_background_live_state(
                recorder.as_ref(),
                &formation,
                project_room_state(&room, room_live_state).await,
            )
            .await;
        }
        let mut file = if replay_truncated {
            None
        } else {
            match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&transcript)
                .await
            {
                Ok(file) => Some(file),
                Err(error) => {
                    update_room_lifecycle(
                        &room,
                        generation,
                        "failed",
                        None,
                        false,
                        Some(error.to_string()),
                    )
                    .await;
                    record_background_live_state(
                        recorder.as_ref(),
                        &formation,
                        project_room_state(&room, room_live_state).await,
                    )
                    .await;
                    None
                }
            }
        };
        loop {
            if generation_guard.load(Ordering::Acquire) != generation {
                return;
            }
            let event = events.recv().await;
            if generation_guard.load(Ordering::Acquire) != generation {
                return;
            }
            match event {
                Ok(ExecProcessEvent::Output(chunk)) => {
                    let Some(transcript_file) = file.as_mut() else {
                        continue;
                    };
                    if transcript_file.write_all(&chunk.chunk.0).await.is_err()
                        || transcript_file.flush().await.is_err()
                    {
                        file = None;
                        update_room_lifecycle(
                            &room,
                            generation,
                            "failed",
                            None,
                            false,
                            Some("terminal transcript write failed".to_string()),
                        )
                        .await;
                        record_background_live_state(
                            recorder.as_ref(),
                            &formation,
                            project_room_state(&room, room_live_state).await,
                        )
                        .await;
                    }
                }
                Ok(ExecProcessEvent::Exited { exit_code, .. }) => {
                    if !apply_process_lifecycle_event(
                        &room,
                        generation,
                        &ExecProcessEvent::Exited { seq: 0, exit_code },
                    )
                    .await
                    {
                        return;
                    }
                    if !record_background_live_state(
                        recorder.as_ref(),
                        &formation,
                        project_room_state(&room, room_live_state).await,
                    )
                    .await
                    {
                        return;
                    }
                }
                Ok(ExecProcessEvent::Closed { .. }) => {
                    if !apply_process_lifecycle_event(
                        &room,
                        generation,
                        &ExecProcessEvent::Closed { seq: 0 },
                    )
                    .await
                    {
                        return;
                    }
                    record_background_live_state(
                        recorder.as_ref(),
                        &formation,
                        project_room_state(&room, room_live_state).await,
                    )
                    .await;
                    return;
                }
                Ok(ExecProcessEvent::Failed(error)) => {
                    if !apply_process_lifecycle_event(
                        &room,
                        generation,
                        &ExecProcessEvent::Failed(error),
                    )
                    .await
                    {
                        return;
                    }
                    record_background_live_state(
                        recorder.as_ref(),
                        &formation,
                        project_room_state(&room, room_live_state).await,
                    )
                    .await;
                    return;
                }
                Err(error) => {
                    update_room_lifecycle(
                        &room,
                        generation,
                        "failed",
                        None,
                        false,
                        Some(format!("terminal event stream failed: {error}")),
                    )
                    .await;
                    record_background_live_state(
                        recorder.as_ref(),
                        &formation,
                        project_room_state(&room, room_live_state).await,
                    )
                    .await;
                    return;
                }
            }
        }
    });
}

async fn apply_process_lifecycle_event(
    room: &Arc<tokio::sync::Mutex<RoomState>>,
    generation: u64,
    event: &ExecProcessEvent,
) -> bool {
    let mut state = room.lock().await;
    if state.generation != generation {
        return false;
    }
    match event {
        ExecProcessEvent::Output(_) => {}
        ExecProcessEvent::Exited { exit_code, .. } => {
            state.exit_code = Some(*exit_code);
            if state.failure.is_none() {
                state.status = "exited".to_string();
            }
        }
        ExecProcessEvent::Closed { .. } => {
            state.closed = true;
            if state.status == "running" {
                state.status = "failed".to_string();
                state.failure =
                    Some("process stream closed before an exit status was observed".to_string());
            }
        }
        ExecProcessEvent::Failed(error) => {
            state.status = "failed".to_string();
            state.failure = Some(error.clone());
        }
    }
    true
}

async fn update_room_lifecycle(
    room: &Arc<tokio::sync::Mutex<RoomState>>,
    generation: u64,
    status: &str,
    exit_code: Option<i32>,
    closed: bool,
    failure: Option<String>,
) {
    let mut state = room.lock().await;
    if state.generation != generation {
        return;
    }
    state.status = status.to_string();
    if exit_code.is_some() {
        state.exit_code = exit_code;
    }
    state.closed |= closed;
    if failure.is_some() {
        state.failure = failure;
    }
}

async fn record_background_live_state(
    recorder: &dyn ProcessRoomRecorder,
    formation: &WorkerEnvironmentFormation,
    state: Value,
) -> bool {
    match recorder.record_live_state(formation, &state).await {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(
                worker_environment_id = formation.worker_environment_id.as_str(),
                "process room live-state acknowledgement failed: {error}"
            );
            false
        }
    }
}

async fn project_room_state(
    room: &Arc<tokio::sync::Mutex<RoomState>>,
    projection: fn(&RoomState) -> Value,
) -> Value {
    let state = room.lock().await;
    projection(&state)
}

fn room_live_state(state: &RoomState) -> Value {
    let can_start = state.process.is_none() || state.exit_code.is_some() || state.closed;
    let tty = state.launch.as_ref().is_some_and(|launch| launch.tty);
    let restart_available = tty && state.launch.is_some() && can_start;
    let detail = state.failure.clone().or_else(|| {
        state
            .exit_code
            .map(|exit_code| format!("Exited with code {exit_code}"))
    });
    json!({
        "schema_version": 1,
        "worker_environment_id": state.formation.worker_environment_id,
        "worker_environment_kind": state.formation.worker_environment_kind,
        "status": state.status,
        "process_id": state.process_id,
        "generation": state.generation,
        "tty": tty,
        "workspace": state.workspace,
        "exit_code": state.exit_code,
        "closed": state.closed,
        "failure": state.failure,
        "viewer": {
            "protocol": "process_session.direct.v1",
            "address": state.viewer_socket,
            "auth_file": state.viewer_socket.with_extension("auth"),
            "warning": "Manual terminal control is direct room I/O and is not Octopus artifact evidence.",
            "restart_available": restart_available,
            "start_new_shell_available": can_start && (tty || state.process.is_none()),
            "launch_label": state.launch.as_ref().map(|launch| launch.label),
        },
        "detail": detail,
    })
}

#[cfg(unix)]
async fn spawn_viewer_listener(
    room: Arc<tokio::sync::Mutex<RoomState>>,
) -> Result<tokio::task::JoinHandle<()>, String> {
    let socket = room.lock().await.viewer_socket.clone();
    remove_private_socket(&socket)?;
    let listener = UnixListener::bind(&socket).map_err(|error| {
        format!(
            "process viewer socket '{}' cannot bind: {error}",
            socket.display()
        )
    })?;
    if let Err(error) = set_owner_only(&socket) {
        let _ = remove_private_socket(&socket);
        return Err(error);
    }
    Ok(tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(serve_viewer_connection(Arc::clone(&room), stream));
        }
    }))
}

#[cfg(not(unix))]
async fn spawn_viewer_listener(
    _room: Arc<tokio::sync::Mutex<RoomState>>,
) -> Result<tokio::task::JoinHandle<()>, String> {
    Err("process_session direct viewer requires a local IPC transport on this platform".to_string())
}

#[cfg(unix)]
async fn serve_viewer_connection(room: Arc<tokio::sync::Mutex<RoomState>>, stream: UnixStream) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let Ok(Some(auth_line)) = lines.next_line().await else {
        return;
    };
    let Ok(auth) = serde_json::from_str::<ViewerAuth>(&auth_line) else {
        return;
    };
    if !viewer_identity_matches(&room, &auth).await {
        return;
    }
    let mut bound_generation = auth.generation;
    let mut bound_process_id = auth.process_id.clone();
    let (initial, replay_notice, mut events) = {
        let state = room.lock().await;
        let events = state
            .process
            .as_ref()
            .map(|process| process.subscribe_events());
        let replay_notice = events.as_ref().and_then(viewer_replay_notice);
        (viewer_state_message(&state), replay_notice, events)
    };
    if write_viewer_message(&mut writer, &initial).await.is_err() {
        return;
    }
    if let Some(replay_notice) = replay_notice {
        if write_viewer_message(&mut writer, &replay_notice)
            .await
            .is_err()
        {
            return;
        }
    }
    let mut active_auth = auth;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return; };
                let response = match serde_json::from_str::<ViewerCommand>(&line) {
                    Ok(command) => handle_viewer_command(&room, &active_auth, command).await,
                    Err(error) => viewer_error(None, format!("invalid viewer command: {error}")),
                };
                if write_viewer_message(&mut writer, &response).await.is_err() {
                    return;
                }
                if response["refresh_stream"] == true {
                    let state = room.lock().await;
                    bound_generation = state.generation;
                    bound_process_id = state.process_id.as_ref().map(|value| value.as_str().to_string());
                    active_auth.generation = bound_generation;
                    active_auth.process_id.clone_from(&bound_process_id);
                    events = state.process.as_ref().map(|process| process.subscribe_events());
                    let replay_notice = events.as_ref().and_then(viewer_replay_notice);
                    drop(state);
                    if let Some(replay_notice) = replay_notice {
                        if write_viewer_message(&mut writer, &replay_notice).await.is_err() {
                            return;
                        }
                    }
                }
            }
            event = recv_viewer_event(&mut events), if events.is_some() => {
                let identity_current = {
                    let state = room.lock().await;
                    state.generation == bound_generation
                        && state.process_id.as_ref().map(ProcessId::as_str) == bound_process_id.as_deref()
                };
                if !identity_current {
                    events = None;
                    continue;
                }
                let event = match event {
                    Ok(event) => event,
                    Err(error) => {
                        let _ = write_viewer_message(&mut writer, &json!({
                            "type": "failure",
                            "message": error,
                        })).await;
                        return;
                    }
                };
                let outgoing = match event {
                    ExecProcessEvent::Output(chunk) => json!({
                        "type": "output",
                        "seq": chunk.seq,
                        "stream": chunk.stream,
                        "data_base64": BASE64_STANDARD.encode(&chunk.chunk.0),
                    }),
                    lifecycle => {
                        if !apply_process_lifecycle_event(&room, bound_generation, &lifecycle).await {
                            events = None;
                            continue;
                        }
                        project_room_state(&room, viewer_state_message).await
                    }
                };
                if write_viewer_message(&mut writer, &outgoing).await.is_err() {
                    return;
                }
            }
        }
    }
}

fn viewer_replay_notice(events: &octopus_exec::ExecProcessEventReceiver) -> Option<Value> {
    events.replay_truncated().then(|| {
        json!({
            "type": "history_truncated",
            "retained_from_seq": events.replay_first_seq(),
        })
    })
}

#[cfg(unix)]
async fn recv_viewer_event(
    events: &mut Option<octopus_exec::ExecProcessEventReceiver>,
) -> Result<ExecProcessEvent, String> {
    let receiver = events
        .as_mut()
        .ok_or_else(|| "process viewer output stream is unavailable".to_string())?;
    let event = match receiver.recv().await {
        Ok(event) => event,
        Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
            return Err(format!(
                "Terminal output gap detected after {skipped} skipped events. Reopen Output to replay the retained process stream."
            ));
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            return Err("Terminal output stream closed. Reopen Output to recover the retained process stream.".to_string());
        }
    };
    Ok(event)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewerAuth {
    schema_version: u64,
    worker_environment_id: String,
    generation: u64,
    process_id: Option<String>,
    token: String,
}

#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum ViewerCommand {
    Write {
        request_id: String,
        data_base64: String,
    },
    Resize {
        request_id: String,
        cols: u16,
        rows: u16,
    },
    Interrupt {
        request_id: String,
    },
    Terminate {
        request_id: String,
    },
    Restart {
        request_id: String,
    },
    StartNewShell {
        request_id: String,
    },
}

async fn viewer_identity_matches(
    room: &Arc<tokio::sync::Mutex<RoomState>>,
    auth: &ViewerAuth,
) -> bool {
    let state = room.lock().await;
    auth.schema_version == 1
        && auth.worker_environment_id == state.formation.worker_environment_id.as_str()
        && auth.generation == state.generation
        && auth.process_id.as_deref() == state.process_id.as_ref().map(ProcessId::as_str)
        && auth.token.as_bytes() == state.viewer_token.as_bytes()
}

async fn handle_viewer_command(
    room: &Arc<tokio::sync::Mutex<RoomState>>,
    auth: &ViewerAuth,
    command: ViewerCommand,
) -> Value {
    if !viewer_identity_matches(room, auth).await {
        return viewer_error(
            command.request_id(),
            "The projected process identity changed.".to_string(),
        );
    }
    let request_id = command.request_id().map(str::to_string);
    let may_start_first_shell = matches!(&command, ViewerCommand::StartNewShell { .. })
        && room.lock().await.process.is_none();
    if !may_start_first_shell {
        if let Err(error) = require_interactive_tty(room).await {
            return viewer_error(request_id.as_deref(), error);
        }
    }
    let result = match command {
        ViewerCommand::Write { data_base64, .. } => {
            direct_write(room, &data_base64).await.map(|_| false)
        }
        ViewerCommand::Resize { cols, rows, .. } => {
            direct_resize(room, cols, rows).await.map(|_| false)
        }
        ViewerCommand::Interrupt { .. } => direct_interrupt(room).await.map(|_| false),
        ViewerCommand::Terminate { .. } => direct_terminate(room).await.map(|_| false),
        ViewerCommand::Restart { .. } => direct_restart(room, false).await.map(|_| true),
        ViewerCommand::StartNewShell { .. } => direct_restart(room, true).await.map(|_| true),
    };
    match result {
        Ok(refresh_stream) => {
            let state = project_room_state(room, viewer_state_snapshot).await;
            json!({
                "type": "response",
                "request_id": request_id,
                "ok": true,
                "refresh_stream": refresh_stream,
                "state": state,
            })
        }
        Err(error) => viewer_error(request_id.as_deref(), error),
    }
}

impl ViewerCommand {
    fn request_id(&self) -> Option<&str> {
        Some(match self {
            Self::Write { request_id, .. }
            | Self::Resize { request_id, .. }
            | Self::Interrupt { request_id }
            | Self::Terminate { request_id }
            | Self::Restart { request_id }
            | Self::StartNewShell { request_id } => request_id,
        })
    }
}

async fn direct_write(room: &Arc<tokio::sync::Mutex<RoomState>>, data: &str) -> Result<(), String> {
    require_interactive_tty(room).await?;
    let bytes = BASE64_STANDARD
        .decode(data)
        .map_err(|error| format!("terminal input is not valid base64: {error}"))?;
    if bytes.len() > 65_536 {
        return Err("terminal input exceeds 65536 bytes".to_string());
    }
    if bytes.is_empty() {
        return Ok(());
    }
    let process = running_room_process(room).await?;
    process
        .write(bytes)
        .await
        .map_err(|error| format!("terminal input failed: {error}"))?;
    Ok(())
}

async fn direct_resize(
    room: &Arc<tokio::sync::Mutex<RoomState>>,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    require_interactive_tty(room).await?;
    if !(2..=500).contains(&cols) || !(1..=300).contains(&rows) {
        return Err("terminal size is outside the supported bounds".to_string());
    }
    running_room_process(room)
        .await?
        .resize(cols, rows)
        .await
        .map_err(|error| format!("terminal resize failed: {error}"))
}

async fn direct_interrupt(room: &Arc<tokio::sync::Mutex<RoomState>>) -> Result<(), String> {
    require_interactive_tty(room).await?;
    let response = running_room_process(room)
        .await?
        .signal(ProcessSignal::Interrupt)
        .await
        .map_err(|error| format!("terminal interrupt failed: {error}"))?;
    if response.delivered {
        Ok(())
    } else {
        Err("terminal interrupt was not delivered".to_string())
    }
}

async fn require_interactive_tty(room: &Arc<tokio::sync::Mutex<RoomState>>) -> Result<(), String> {
    if room
        .lock()
        .await
        .launch
        .as_ref()
        .is_some_and(|launch| launch.tty)
    {
        Ok(())
    } else {
        Err("This process has no interactive PTY; its projected Output is read-only.".to_string())
    }
}

async fn direct_terminate(room: &Arc<tokio::sync::Mutex<RoomState>>) -> Result<(), String> {
    let response = running_room_process(room)
        .await?
        .terminate()
        .await
        .map_err(|error| format!("terminal termination failed: {error}"))?;
    if response.running {
        Ok(())
    } else {
        Err("terminal process is not running".to_string())
    }
}

async fn running_room_process(
    room: &Arc<tokio::sync::Mutex<RoomState>>,
) -> Result<Arc<dyn ExecProcess>, String> {
    let state = room.lock().await;
    let process = state
        .process
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| "terminal process is unavailable".to_string())?;
    let expected_generation = state.generation;
    let expected_process_id = state.process_id.clone();
    drop(state);
    let observed = process
        .read(None, Some(1), Some(0))
        .await
        .map_err(|error| format!("terminal process status failed: {error}"))?;
    if observed.exited || observed.closed {
        return Err("terminal process already exited".to_string());
    }
    let state = room.lock().await;
    if state.generation != expected_generation || state.process_id != expected_process_id {
        return Err("terminal process identity changed before control was delivered".to_string());
    }
    Ok(process)
}

async fn direct_restart(
    room: &Arc<tokio::sync::Mutex<RoomState>>,
    new_shell: bool,
) -> Result<(), String> {
    let launch = {
        let state = room.lock().await;
        if new_shell {
            let mut env = HashMap::new();
            if let Ok(path) = std::env::var("PATH") {
                env.insert("PATH".to_string(), path);
            }
            env.insert(
                "HOME".to_string(),
                state.workspace.to_string_lossy().to_string(),
            );
            env.insert("TERM".to_string(), "xterm-256color".to_string());
            SafeLaunchSpec {
                argv: vec![state.shell.clone(), "-l".to_string()],
                env,
                tty: true,
                label: "Login shell",
            }
        } else {
            state
                .launch
                .clone()
                .ok_or_else(|| "No previous launch is available to restart.".to_string())?
        }
    };
    let (process, _process_id, generation) =
        ProcessRoom::start_from_spec(room, launch.clone()).await?;
    let state = room.lock().await;
    let formation = state.formation.clone();
    let transcript = state.transcript.clone();
    let generation_guard = Arc::clone(&state.generation_guard);
    let recorder = Arc::clone(&state.recorder);
    let projected = room_live_state(&state);
    drop(state);
    recorder.record_live_state(&formation, &projected).await?;
    spawn_transcript_writer(
        process,
        formation,
        transcript,
        generation,
        generation_guard,
        Arc::clone(room),
        recorder,
    );
    Ok(())
}

fn viewer_state_message(state: &RoomState) -> Value {
    json!({
        "type": "state",
        "state": viewer_state_snapshot(state),
    })
}

fn viewer_state_snapshot(state: &RoomState) -> Value {
    let can_start = state.process.is_none() || state.exit_code.is_some() || state.closed;
    let tty = state.launch.as_ref().is_some_and(|launch| launch.tty);
    json!({
        "worker_environment_id": state.formation.worker_environment_id,
        "generation": state.generation,
        "process_id": state.process_id,
        "status": state.status,
        "tty": tty,
        "exit_code": state.exit_code,
        "closed": state.closed,
        "failure": state.failure,
        "restart_available": tty && state.launch.is_some() && can_start,
        "start_new_shell_available": can_start && (tty || state.process.is_none()),
        "launch_label": state.launch.as_ref().map(|launch| launch.label),
    })
}

fn viewer_error(request_id: Option<&str>, error: String) -> Value {
    json!({
        "type": "response",
        "request_id": request_id,
        "ok": false,
        "error": error,
    })
}

#[cfg(unix)]
async fn write_viewer_message(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    value: &Value,
) -> Result<(), String> {
    let mut encoded = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if encoded.len() > 1_048_576 {
        return Err("process viewer message exceeds 1048576 bytes".to_string());
    }
    encoded.push(b'\n');
    writer
        .write_all(&encoded)
        .await
        .map_err(|error| error.to_string())
}

fn write_private_text(path: &Path, value: &str) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        format!(
            "private process viewer credential '{}' cannot be created: {error}",
            path.display()
        )
    })?;
    std::io::Write::write_all(&mut file, value.as_bytes()).map_err(|error| {
        format!(
            "private process viewer credential '{}' cannot be written: {error}",
            path.display()
        )
    })?;
    set_owner_only(path)
}

fn viewer_token() -> Result<String, String> {
    #[cfg(not(unix))]
    {
        return Err(
            "process_session direct viewer has no secure entropy source on this platform"
                .to_string(),
        );
    }
    #[cfg(unix)]
    {
        let mut entropy = [0u8; 32];
        let mut source = std::fs::File::open("/dev/urandom")
            .map_err(|error| format!("process viewer entropy source is unavailable: {error}"))?;
        std::io::Read::read_exact(&mut source, &mut entropy)
            .map_err(|error| format!("process viewer entropy could not be read: {error}"))?;
        Ok(BASE64_STANDARD.encode(entropy))
    }
}

fn set_owner_only(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |error| {
                format!(
                    "private process viewer path '{}' cannot be protected: {error}",
                    path.display()
                )
            },
        )?;
    }
    Ok(())
}

fn remove_private_socket(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if is_socket_metadata(&metadata) => {
            std::fs::remove_file(path).map_err(|error| {
                format!(
                    "private process viewer socket '{}' cannot be removed: {error}",
                    path.display()
                )
            })
        }
        Ok(_) => Err(format!(
            "private process viewer path '{}' is not a socket",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "private process viewer path '{}' cannot be inspected: {error}",
            path.display()
        )),
    }
}

fn remove_private_file(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(path).map_err(|error| {
                format!(
                    "private process viewer file '{}' cannot be removed: {error}",
                    path.display()
                )
            })
        }
        Ok(_) => Err(format!(
            "private process viewer path '{}' is not a regular file",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "private process viewer path '{}' cannot be inspected: {error}",
            path.display()
        )),
    }
}

#[cfg(unix)]
fn is_socket_metadata(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}

#[cfg(not(unix))]
fn is_socket_metadata(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn process_operation(
    handler_config: &Value,
    tool: &str,
) -> Result<ProcessOperation, ToolProcessError> {
    serde_json::from_value::<ProcessHandlerConfig>(handler_config.clone())
        .map(|config| config.operation)
        .map_err(|error| {
            ToolProcessError::Capability(format!(
                "tool '{tool}' has malformed process handler_config: {error}"
            ))
        })
}

fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}

fn optional_bool(object: &Map<String, Value>, field: &str) -> Result<Option<bool>, String> {
    object
        .get(field)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| format!("{field} must be a boolean"))
        })
        .transpose()
}

fn optional_u64(object: &Map<String, Value>, field: &str) -> Result<Option<u64>, String> {
    object
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| format!("{field} must be a non-negative integer"))
        })
        .transpose()
}

fn bounded_optional_u64(
    object: &Map<String, Value>,
    field: &str,
    minimum: u64,
    maximum: u64,
) -> Result<Option<u64>, String> {
    let value = optional_u64(object, field)?;
    if let Some(value) = value {
        if !(minimum..=maximum).contains(&value) {
            return Err(format!("{field} must be between {minimum} and {maximum}"));
        }
    }
    Ok(value)
}

fn optional_string_array(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<Vec<String>>, String> {
    object
        .get(field)
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| format!("{field} must be an array"))?
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    item.as_str()
                        .filter(|item| !item.is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| format!("{field}[{index}] must be a non-empty string"))
                })
                .collect()
        })
        .transpose()
}

fn optional_string_map(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<HashMap<String, String>>, String> {
    object
        .get(field)
        .map(|value| {
            let map = value
                .as_object()
                .ok_or_else(|| format!("{field} must be an object"))?;
            map.iter()
                .map(|(key, value)| {
                    if key.is_empty() || key.contains('=') || key.contains('\0') {
                        return Err(format!("{field} contains invalid environment name '{key}'"));
                    }
                    let value = value
                        .as_str()
                        .ok_or_else(|| format!("{field}.{key} must be a string"))?;
                    if value.contains('\0') {
                        return Err(format!("{field}.{key} contains a null byte"));
                    }
                    Ok((key.clone(), value.to_string()))
                })
                .collect()
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use octopus_descriptors::{
        ArtifactId, CheckpointId, EeId, InterfaceId, RunId, ToolDescriptorId,
    };
    use octopus_interfaces::{
        GrantProjection, InterfaceArtifactFile, InterfaceInvocationEnvelope, InvocationMetadata,
        RuntimeInvocationMetadata,
    };
    use octopus_ledger::{ArtifactRef, ContentHash};
    use octopus_tool_registry::{load_tool_descriptor_from_dir, ToolDescriptor};
    use octopus_worker_env::{
        WorkerEnvKindRegistry, WorkerEnvKindSpec, WorkerEnvironmentFormation,
        WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId,
    };

    use super::*;

    const KIND: &str = "process_session";
    const WORKER: &str = "system_worker_coder_max";

    #[derive(Default)]
    struct TestRoomRecorder {
        live_state: std::sync::Mutex<Option<Value>>,
    }

    impl WorkerEnvironmentLifecycleRecorder for TestRoomRecorder {
        fn record<'a>(
            &'a self,
            _formation: &'a WorkerEnvironmentFormation,
            _lifecycle: WorkerEnvironmentLifecycle,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }
    }

    impl WorkerEnvironmentLiveStateRecorder for TestRoomRecorder {
        fn record_live_state<'a>(
            &'a self,
            _formation: &'a WorkerEnvironmentFormation,
            state: &'a Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>
        {
            Box::pin(async move {
                *self.live_state.lock().unwrap() = Some(state.clone());
                Ok(())
            })
        }
    }

    fn registry_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap()
    }

    fn tool_ids() -> Vec<String> {
        [
            "process.start",
            "process.read",
            "process.write",
            "process.signal",
            "process.terminate",
            "process.list",
            "exec.run",
            "exec.file_read",
            "exec.file_write",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn descriptor(id: &str) -> ToolDescriptor {
        load_tool_descriptor_from_dir(&registry_root().join("tool"), id)
            .unwrap()
            .unwrap()
    }

    fn formation() -> (WorkerEnvironmentFormation, WorkerEnvKindSpec) {
        let registry =
            WorkerEnvKindRegistry::load_from_dir(&registry_root().join("worker_environment_kind"))
                .unwrap();
        let tools = tool_ids();
        let tool_hashes = tools
            .iter()
            .map(|tool| {
                let value = serde_json::to_value(descriptor(tool)).unwrap();
                let hash = blake3::hash(&octopus_descriptors::canonical_bytes(&value))
                    .to_hex()
                    .to_string();
                (tool.clone(), hash)
            })
            .collect::<BTreeMap<_, _>>();
        let tool_process_bins = tools
            .iter()
            .map(|tool| (tool.clone(), descriptor(tool).process.bin))
            .collect::<BTreeMap<_, _>>();
        let kind_id = WorkerEnvironmentKindId::new(KIND);
        let kinds = vec![kind_id.clone()];
        let door = InterfaceId::new("system_exec_env");
        let worker = InterfaceId::new(WORKER);
        let door_root_bindings = BTreeMap::new();
        let formed = registry
            .form(&WorkerEnvironmentFormationRequest {
                door_additional_root_bindings: &Default::default(),
                door_interface: &door,
                door_descriptor_hash: "hash-door",
                door_driver_ref: "system_exec_env",
                tool: "process.start",
                tool_descriptor_hashes: &tool_hashes,
                tool_process_bins: &tool_process_bins,
                tool_worker_environment_kinds: &kinds,
                worker: &worker,
                ee: "ee_process",
                run: "run_process",
                granted_worker_environment_kinds: &kinds,
                granted_tools: &tools,
                door_root_bindings: &door_root_bindings,
                door_instance_dir: std::path::Path::new("."),
                ee_root_slot_allowance: None,
            })
            .unwrap();
        let (kind, _) = registry.get(&kind_id).unwrap();
        (formed, (*kind).clone())
    }

    fn envelope(
        root: &Path,
        sequence: u64,
        tool: &str,
        arguments: Value,
    ) -> InterfaceInvocationEnvelope {
        envelope_with_tools(root, sequence, tool, arguments, tool_ids())
    }

    fn envelope_with_tools(
        root: &Path,
        sequence: u64,
        tool: &str,
        arguments: Value,
        tools: Vec<String>,
    ) -> InterfaceInvocationEnvelope {
        let payload = serde_json::to_vec(&json!({
            "tool": tool,
            "arguments": arguments
        }))
        .unwrap();
        let artifact_file = InterfaceArtifactFile::write(
            root.join(format!("inbound-{sequence}")),
            "tool.call",
            &payload,
        )
        .unwrap();
        let content = ContentHash::of_bytes(&payload);
        let metadata = InvocationMetadata::new(GrantProjection {
            tools,
            worker_environment_kinds: vec![KIND.to_string()],
            ..GrantProjection::default()
        })
        .with_runtime(RuntimeInvocationMetadata {
            run: RunId::new("run_process"),
            ee: EeId::new("ee_process"),
            worker: InterfaceId::new(WORKER),
            checkpoint: CheckpointId::new(format!("cp_{sequence}")),
            interface: InterfaceId::new("system_exec_env"),
            epoch: 0,
            run_record_hash: None,
        });
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
                content,
            },
            artifact_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(metadata),
        }
    }

    async fn invoke(
        room: &ProcessRoom,
        recorder: &Arc<TestRoomRecorder>,
        root: &Path,
        sequence: u64,
        tool: &str,
        arguments: Value,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Value {
        let invocation = ToolProcessInvocation {
            request_id: sequence,
            descriptor: descriptor(tool),
            envelope: envelope(root, sequence, tool, arguments),
            source_movement_seq: sequence,
            worker_environment: Some(formation.clone()),
            worker_environment_kind: Some(kind.clone()),
        };
        let artifact = room
            .receive(invocation, recorder.clone())
            .await
            .unwrap_or_else(|error| match error {
                ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                    panic!("process room invocation failed: {error}")
                }
            });
        serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn private_process_room_supports_files_one_shot_exec_and_persistent_process() {
        let root = tempfile::tempdir().unwrap();
        let process_var = root.path().join("process-var");
        let environment = ToolProcessEnvironment {
            interface: InterfaceId::new("system_exec_env"),
            interface_dir: root.path().join("interface"),
            descriptor_snapshot: None,
            var_dir: process_var.clone(),
            durable_var_dir: None,
            host_io_limits: octopus_process_session_tool_runtime::InterfaceHostIoLimits {
                max_control_frame_bytes: 1024 * 1024,
                max_stderr_bytes: 256 * 1024,
                max_manifest_bytes: 1024 * 1024,
                max_payload_bytes: 64 * 1024 * 1024,
            },
            private_reads: BTreeMap::new(),
            root_slots: BTreeMap::from([(
                INTERFACE_LOCAL_IPC_ROOT_SLOT.to_string(),
                root.path().join("ipc"),
            )]),
        };
        let room = ProcessRoom::new(environment);
        let (formation, kind) = formation();
        let recorder = Arc::new(TestRoomRecorder::default());

        let file_written = invoke(
            &room,
            &recorder,
            root.path(),
            1,
            "exec.file_write",
            json!({
                "path": "state/note.txt",
                "content": "PRIVATE-WORKSPACE",
                "create_parents": true
            }),
            &formation,
            &kind,
        )
        .await;
        assert_eq!(file_written["ok"], true);
        assert_eq!(file_written["body"]["bytes_written"], 17);

        let file_read = invoke(
            &room,
            &recorder,
            root.path(),
            2,
            "exec.file_read",
            json!({ "path": "state/note.txt" }),
            &formation,
            &kind,
        )
        .await;
        assert_eq!(file_read["ok"], true);
        assert_eq!(file_read["body"]["content"], "PRIVATE-WORKSPACE");

        let one_shot = invoke(
            &room,
            &recorder,
            root.path(),
            3,
            "exec.run",
            json!({
                "command": ["/bin/sh", "-lc", "printf OCTOPUS-ONESHOT"],
                "timeout_ms": 5000
            }),
            &formation,
            &kind,
        )
        .await;
        assert_eq!(one_shot["ok"], true);
        assert_eq!(one_shot["body"]["completed"], true, "{one_shot}");
        assert_eq!(one_shot["body"]["output"]["exit_code"], 0);
        assert_eq!(one_shot["body"]["output"]["stdout"], "OCTOPUS-ONESHOT");

        let started = invoke(
            &room,
            &recorder,
            root.path(),
            4,
            "process.start",
            json!({ "argv": ["/bin/sh"], "tty": false }),
            &formation,
            &kind,
        )
        .await;
        assert_eq!(started["ok"], true);
        assert_eq!(
            started["body"]["worker_environment_id"],
            formation.worker_environment_id.as_str()
        );

        let written = invoke(
            &room,
            &recorder,
            root.path(),
            5,
            "process.write",
            json!({ "text": "printf 'OCTOPUS-PROCESS\\n'\n" }),
            &formation,
            &kind,
        )
        .await;
        assert_eq!(written["ok"], true);

        let read = invoke(
            &room,
            &recorder,
            root.path(),
            6,
            "process.read",
            json!({ "wait_ms": 2000 }),
            &formation,
            &kind,
        )
        .await;
        assert_eq!(read["ok"], true);
        let output = read["body"]["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|chunk| chunk["text"].as_str())
            .collect::<String>();
        assert!(output.contains("OCTOPUS-PROCESS"), "{read}");

        let live = recorder.live_state.lock().unwrap().clone().unwrap();
        assert_eq!(live["viewer"]["protocol"], "process_session.direct.v1");
        assert!(started["body"]["workspace"]
            .as_str()
            .unwrap()
            .starts_with(process_var.to_str().unwrap()));

        let terminated = invoke(
            &room,
            &recorder,
            root.path(),
            7,
            "process.terminate",
            json!({}),
            &formation,
            &kind,
        )
        .await;
        assert_eq!(terminated["ok"], true);
    }

    #[tokio::test]
    async fn exact_clone_id_executes_copied_file_write_operation_and_stays_exact_in_evidence() {
        let root = tempfile::tempdir().unwrap();
        let room = ProcessRoom::new(ToolProcessEnvironment {
            interface: InterfaceId::new("system_exec_env"),
            interface_dir: root.path().join("interface"),
            descriptor_snapshot: None,
            var_dir: root.path().join("process-var"),
            durable_var_dir: None,
            host_io_limits: octopus_process_session_tool_runtime::InterfaceHostIoLimits {
                max_control_frame_bytes: 1024 * 1024,
                max_stderr_bytes: 256 * 1024,
                max_manifest_bytes: 1024 * 1024,
                max_payload_bytes: 64 * 1024 * 1024,
            },
            private_reads: BTreeMap::new(),
            root_slots: BTreeMap::from([(
                INTERFACE_LOCAL_IPC_ROOT_SLOT.to_string(),
                root.path().join("ipc"),
            )]),
        });
        let exact_tool = "exec.file_write.run_42";
        let mut clone = descriptor("exec.file_write");
        clone.id = ToolDescriptorId::new(exact_tool);
        let (mut formation, kind) = formation();
        formation.granted_tools.push(exact_tool.to_string());
        formation.granted_tools.sort();
        formation.tool_descriptor_hashes.insert(
            exact_tool.to_string(),
            blake3::hash(&octopus_descriptors::canonical_bytes(
                &serde_json::to_value(&clone).unwrap(),
            ))
            .to_hex()
            .to_string(),
        );
        let mut granted_tools = tool_ids();
        granted_tools.push(exact_tool.to_string());
        granted_tools.sort();
        let invocation = ToolProcessInvocation {
            request_id: 42,
            descriptor: clone,
            envelope: envelope_with_tools(
                root.path(),
                42,
                exact_tool,
                json!({
                    "path": "clone/proof.txt",
                    "content": "descriptor-operation",
                    "create_parents": true
                }),
                granted_tools,
            ),
            source_movement_seq: 42,
            worker_environment: Some(formation),
            worker_environment_kind: Some(kind),
        };
        let recorder = Arc::new(TestRoomRecorder::default());
        let artifact =
            room.receive(invocation, recorder)
                .await
                .unwrap_or_else(|error| match error {
                    ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                        panic!("process clone invocation failed: {error}")
                    }
                });
        let response: Value =
            serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();

        assert_eq!(response["ok"], true, "{response}");
        assert_eq!(response["tool"], exact_tool);
        assert_eq!(response["label"], format!("{exact_tool}.result"));
        assert_eq!(response["body"]["result"], "file_written");
        assert_eq!(response["body"]["bytes_written"], 20);
        for handler_config in [
            json!({}),
            json!({"operation": 1}),
            json!({"operation": "unknown"}),
        ] {
            let error = process_operation(&handler_config, exact_tool).unwrap_err();
            let message = match error {
                ToolProcessError::Capability(message) | ToolProcessError::Unavailable(message) => {
                    message
                }
            };
            assert!(message.contains(exact_tool), "{message}");
            assert!(message.contains("handler_config"), "{message}");
        }
    }
}
