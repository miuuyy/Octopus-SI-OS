use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use octopus_interfaces::InterfaceArtifactFile;
use octopus_tool_door_runtime::room::{decode_tool_call, descriptor_operation, formed_scope};
use octopus_tool_door_runtime::{
    DeliveryVerdict, ToolProcessEnvironment, ToolProcessError, ToolProcessHandler,
    ToolProcessInvocation, ToolProcessIo, WorkerEnvironmentLifecycleRecorder,
    WorkerEnvironmentLiveStateRecorder,
};
use octopus_tool_handlers::{ToolResultArtifact, ToolResultError};
use octopus_worker_env::{
    WorkerEnvKindSpec, WorkerEnvironmentFormation, WorkerEnvironmentId, WorkerEnvironmentLifecycle,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

mod client;
mod config;
mod event_journal;
pub mod example_server;

use client::StdioMcpClient;
use config::McpStdioConfig;

const ROOM: &str = "MCP stdio room";
const SUBSTRATE_FAMILY: &str = "mcp_stdio";
const OPERATIONS: &[&str] = &[
    "call_tool", "list_tools", "list_resources", "list_resource_templates", "read_resource",
    "subscribe_resource", "unsubscribe_resource", "list_prompts", "get_prompt", "complete",
    "set_logging_level", "ping", "cancel_request", "send_progress", "poll_events", "status",
    "close",
];
const MAX_SERVER_EVENTS_PER_CALL: usize = 256;
const PROJECTED_EVENT_CURSOR_FILE: &str = "projected_server_events.json";
const MAX_PROJECTED_EVENT_CURSOR_BYTES: u64 = 4096;
static PROJECTED_EVENT_CURSOR_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct RoomState {
    generation: u64,
    client: Option<Arc<StdioMcpClient>>,
    room_root: PathBuf,
    had_client: bool,
    connecting: bool,
    closing: bool,
}

struct RoomEntry {
    released: AtomicBool,
    state: tokio::sync::Mutex<RoomState>,
    projected_event_cursor: Arc<tokio::sync::Mutex<RoomProjectionCursor>>,
    changed: tokio::sync::Notify,
}

#[derive(Clone, Copy)]
enum RoomTransition {
    Connecting,
    Closing,
}

struct RoomTransitionGuard {
    room: Arc<RoomEntry>,
    transition: RoomTransition,
    armed: bool,
}

impl RoomTransitionGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RoomTransitionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let room = Arc::clone(&self.room);
        let transition = self.transition;
        tokio::spawn(async move {
            let mut state = room.state.lock().await;
            match transition {
                RoomTransition::Connecting => state.connecting = false,
                RoomTransition::Closing => state.closing = false,
            }
            drop(state);
            room.changed.notify_waiters();
        });
    }
}

pub struct McpStdioRoom {
    environment: ToolProcessEnvironment,
    rooms: tokio::sync::Mutex<BTreeMap<WorkerEnvironmentId, Arc<RoomEntry>>>,
}

struct RoomDispatch {
    body: Result<Value, String>,
    events: Vec<Value>,
    event_projection: Option<RoomEventProjection>,
    disposition: String,
    evidence_write_errors: Vec<Value>,
}

struct RoomEventProjection {
    cursor: tokio::sync::OwnedMutexGuard<RoomProjectionCursor>,
    after_cursor: u64,
    next_cursor: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RoomEventEvidence {
    invocation: Value,
    worker_environment_id: WorkerEnvironmentId,
    after_cursor: Option<u64>,
    next_cursor: Option<u64>,
    events: Vec<Value>,
}

impl RoomEventProjection {
    fn commit(mut self) -> Result<(), String> {
        self.cursor.commit(self.next_cursor)
    }
}

pub trait McpStdioRoomIo:
    WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder
{
    fn hand_output_artifact<'a>(
        &'a self,
        artifact: &'a InterfaceArtifactFile,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>,
    >;
}

impl McpStdioRoomIo for ToolProcessIo {
    fn hand_output_artifact<'a>(
        &'a self,
        artifact: &'a InterfaceArtifactFile,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>,
    > {
        Box::pin(async move { ToolProcessIo::hand_output_artifact(self, artifact).await })
    }
}

struct RoomProjectionCursor {
    path: PathBuf,
    value: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RoomProjectionCursorFile {
    schema_version: u32,
    cursor: u64,
}

impl RoomProjectionCursor {
    fn new(path: PathBuf) -> Self {
        Self { path, value: None }
    }

    fn load(&mut self) -> Result<u64, String> {
        if let Some(value) = self.value {
            return Ok(value);
        }
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.value = Some(0);
                return Ok(0);
            }
            Err(error) => {
                return Err(format!(
                    "MCP stdio projected event cursor '{}' cannot be inspected: {error}",
                    self.path.display()
                ));
            }
        };
        validate_projection_cursor_metadata(&self.path, &metadata)?;
        if metadata.len() > MAX_PROJECTED_EVENT_CURSOR_BYTES {
            return Err(format!(
                "MCP stdio projected event cursor '{}' exceeds {MAX_PROJECTED_EVENT_CURSOR_BYTES} bytes",
                self.path.display()
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&self.path).map_err(|error| {
            format!(
                "MCP stdio projected event cursor '{}' cannot be opened: {error}",
                self.path.display()
            )
        })?;
        validate_projection_cursor_metadata(
            &self.path,
            &file.metadata().map_err(|error| {
                format!(
                    "MCP stdio projected event cursor '{}' metadata cannot be read: {error}",
                    self.path.display()
                )
            })?,
        )?;
        use std::io::Read;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(MAX_PROJECTED_EVENT_CURSOR_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                format!(
                    "MCP stdio projected event cursor '{}' cannot be read: {error}",
                    self.path.display()
                )
            })?;
        if bytes.len() as u64 > MAX_PROJECTED_EVENT_CURSOR_BYTES {
            return Err(format!(
                "MCP stdio projected event cursor '{}' exceeds {MAX_PROJECTED_EVENT_CURSOR_BYTES} bytes",
                self.path.display()
            ));
        }
        let stored: RoomProjectionCursorFile = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "MCP stdio projected event cursor '{}' is malformed: {error}",
                self.path.display()
            )
        })?;
        if stored.schema_version != 1 {
            return Err(format!(
                "MCP stdio projected event cursor '{}' has unsupported schema_version {}",
                self.path.display(),
                stored.schema_version
            ));
        }
        self.value = Some(stored.cursor);
        Ok(stored.cursor)
    }

    fn commit(&mut self, next_cursor: u64) -> Result<(), String> {
        let current = self.load()?;
        if next_cursor < current {
            return Err(format!(
                "MCP stdio projected event cursor cannot move backward from {current} to {next_cursor}"
            ));
        }
        if next_cursor == current {
            return Ok(());
        }
        let parent = self.path.parent().ok_or_else(|| {
            "MCP stdio projected event cursor path has no parent directory".to_string()
        })?;
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "MCP stdio projected event cursor directory '{}' cannot be created: {error}",
                parent.display()
            )
        })?;
        let bytes = serde_json::to_vec(&RoomProjectionCursorFile {
            schema_version: 1,
            cursor: next_cursor,
        })
        .map_err(|error| format!("MCP stdio projected event cursor cannot serialize: {error}"))?;
        let (temp, mut file) = create_projection_cursor_temp(parent)?;
        let result = (|| -> Result<(), String> {
            use std::io::Write;
            file.write_all(&bytes).map_err(|error| {
                format!(
                    "MCP stdio projected event cursor temp '{}' cannot be written: {error}",
                    temp.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600)).map_err(
                    |error| {
                    format!(
                        "MCP stdio projected event cursor temp '{}' permissions cannot be set: {error}",
                        temp.display()
                    )
                    },
                )?;
            }
            file.sync_all().map_err(|error| {
                format!(
                    "MCP stdio projected event cursor temp '{}' cannot be synced: {error}",
                    temp.display()
                )
            })?;
            std::fs::rename(&temp, &self.path).map_err(|error| {
                format!(
                    "MCP stdio projected event cursor '{}' cannot be replaced atomically: {error}",
                    self.path.display()
                )
            })?;
            let directory = std::fs::File::open(parent).map_err(|error| {
                format!(
                    "MCP stdio projected event cursor directory '{}' cannot be opened: {error}",
                    parent.display()
                )
            })?;
            directory.sync_all().map_err(|error| {
                format!(
                    "MCP stdio projected event cursor directory '{}' cannot be synced: {error}",
                    parent.display()
                )
            })?;
            validate_projection_cursor_metadata(
                &self.path,
                &std::fs::symlink_metadata(&self.path).map_err(|error| {
                    format!(
                        "MCP stdio projected event cursor '{}' cannot be verified: {error}",
                        self.path.display()
                    )
                })?,
            )?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
            return result;
        }
        self.value = Some(next_cursor);
        Ok(())
    }
}

fn validate_projection_cursor_metadata(
    path: &std::path::Path,
    metadata: &std::fs::Metadata,
) -> Result<(), String> {
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!(
            "MCP stdio projected event cursor '{}' must be a regular non-symlink file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(format!(
                "MCP stdio projected event cursor '{}' must have mode 0600",
                path.display()
            ));
        }
    }
    Ok(())
}

fn create_projection_cursor_temp(
    parent: &std::path::Path,
) -> Result<(PathBuf, std::fs::File), String> {
    for _ in 0..1024 {
        let sequence = PROJECTED_EVENT_CURSOR_TEMP_SEQUENCE
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| "MCP stdio projected event cursor temp sequence overflowed".to_string())?;
        let temp = parent.join(format!(
            ".{PROJECTED_EVENT_CURSOR_FILE}.{}.{}.tmp",
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
                    "MCP stdio projected event cursor temp '{}' cannot be created: {error}",
                    temp.display()
                ));
            }
        }
    }
    Err("MCP stdio projected event cursor cannot allocate an atomic temp file".to_string())
}

impl McpStdioRoom {
    pub fn new(environment: ToolProcessEnvironment) -> Self {
        Self {
            environment,
            rooms: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn receive<I: McpStdioRoomIo + ?Sized>(
        &self,
        invocation: ToolProcessInvocation,
        io: &I,
    ) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
        let ToolProcessInvocation {
            descriptor,
            envelope,
            worker_environment,
            worker_environment_kind,
            source_movement_seq,
            ..
        } = invocation;
        let formation = worker_environment.ok_or_else(|| {
            ToolProcessError::Capability(
                "MCP stdio invocation has no formed worker environment".to_string(),
            )
        })?;
        let kind = worker_environment_kind.ok_or_else(|| {
            ToolProcessError::Capability(
                "MCP stdio invocation has no formed kind descriptor".to_string(),
            )
        })?;
        let config = McpStdioConfig::load_formed(&self.environment, &formation, &kind)
            .map_err(ToolProcessError::Capability)?;
        let call = decode_tool_call(ROOM, &envelope)?;
        if call.tool != descriptor.id.as_str() {
            return Err(ToolProcessError::Capability(format!(
                "tool descriptor '{}' does not match tool.call '{}'",
                descriptor.id, call.tool
            )));
        }
        let operation = descriptor_operation(ROOM, descriptor.id.as_str(), &descriptor.handler_config, OPERATIONS)
            .map_err(ToolProcessError::Capability)?;
        formed_scope(ROOM, &envelope, &formation)?;
        let metadata = envelope
            .metadata()
            .map_err(|error| ToolProcessError::Capability(error.to_string()))?;
        let invocation_evidence = json!({
            "tool": call.tool,
            "correlation_id": call.correlation_id,
            "source_movement_seq": source_movement_seq,
            "source_artifact": envelope.artifact,
            "formation": formation,
            "operation": operation,
            "runtime": metadata.runtime,
        });
        let room = self.room_entry(&formation).await?;
        let result_root = room.state.lock().await.room_root.join("tool-results");
        std::fs::create_dir_all(&result_root)
            .map_err(|error| ToolProcessError::Unavailable(error.to_string()))?;
        let call_root = result_root.join(format!(
            "{}-{}",
            source_movement_seq,
            envelope.artifact.content.to_hex()
        ));
        let completed = InterfaceArtifactFile::new(call_root.join("result.artifact.json"))
            .map_err(|error| ToolProcessError::Unavailable(error.to_string()))?;
        match std::fs::create_dir(&call_root) {
            Ok(()) => {
                std::fs::File::open(&result_root)
                    .and_then(|file| file.sync_all())
                    .map_err(|error| ToolProcessError::Unavailable(error.to_string()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let snapshot = completed.read_bounded(
                    self.environment.host_io_limits.max_manifest_bytes as u64,
                    self.environment.host_io_limits.max_payload_bytes as u64,
                ).map_err(|error| ToolProcessError::Unavailable(format!(
                    "MCP stdio invocation already started; its completed result cannot be read: {error}; execution will not be repeated"
                )))?;
                let response: ToolResultArtifact = serde_json::from_slice(&snapshot.payload_bytes)
                    .map_err(|error| ToolProcessError::Unavailable(error.to_string()))?;
                if response.tool != call.tool
                    || response.correlation_id != call.correlation_id
                    || snapshot.manifest.label != response.label
                {
                    return Err(ToolProcessError::Unavailable(
                        "MCP stdio completed result does not match the invocation; execution will not be repeated".to_string()
                    ));
                }
                let evidence = response
                    .body
                    .as_ref()
                    .and_then(|body| body.get("server_events"))
                    .or_else(|| {
                        response
                            .error
                            .as_ref()?
                            .details
                            .as_ref()?
                            .get("server_events")
                    })
                    .ok_or_else(|| {
                        ToolProcessError::Unavailable(
                            "MCP stdio completed result has no server-event evidence".to_string(),
                        )
                    })?;
                let evidence: RoomEventEvidence = serde_json::from_value(evidence.clone())
                    .map_err(|error| ToolProcessError::Unavailable(error.to_string()))?;
                if evidence.invocation != invocation_evidence {
                    return Err(ToolProcessError::Unavailable(
                        "MCP stdio completed result source or formation does not match the invocation; execution will not be repeated".to_string()
                    ));
                }
                let projection = reclaim_room_events(&room, &formation, &evidence)
                    .await
                    .map_err(ToolProcessError::Unavailable)?;
                return hand_completed_result(io, &completed, projection).await;
            }
            Err(error) => return Err(ToolProcessError::Unavailable(error.to_string())),
        }
        let dispatch = self
            .dispatch(&formation, &config, operation, &call.arguments, io)
            .await?;
        let RoomDispatch {
            body,
            events,
            event_projection,
            disposition,
            mut evidence_write_errors,
        } = dispatch;
        if event_projection.is_none() && !events.is_empty() {
            evidence_write_errors.push(evidence_write_error(
                "server_event_cursor",
                "MCP stdio server events have no projection cursor claim",
            ));
        }
        let server_events = serde_json::to_value(RoomEventEvidence {
            invocation: invocation_evidence,
            worker_environment_id: formation.worker_environment_id.clone(),
            after_cursor: event_projection.as_ref().map(|claim| claim.after_cursor),
            next_cursor: event_projection.as_ref().map(|claim| claim.next_cursor),
            events,
        })
        .map_err(|error| ToolProcessError::Unavailable(error.to_string()))?;
        let response = match body {
            Ok(body) if evidence_write_errors.is_empty() => ToolResultArtifact::success(
                &call.tool,
                call.correlation_id,
                with_session_disposition(body, disposition, server_events),
            ),
            Ok(body) => {
                let mut details = Map::new();
                details.insert(
                    "session_disposition".to_string(),
                    Value::String(disposition),
                );
                details.insert("operation_body".to_string(), body);
                details.insert("server_events".to_string(), server_events);
                details.insert(
                    "evidence_write_errors".to_string(),
                    Value::Array(evidence_write_errors),
                );
                ToolResultArtifact::failure(
                    &call.tool,
                    call.correlation_id,
                    ToolResultError::new(
                        "mcp_evidence_failed",
                        format!(
                            "{} completed but its worker-environment evidence could not be fully written",
                            call.tool
                        ),
                    )
                    .with_details(details),
                )
            }
            Err(error) => {
                let mut failure = room_error(&error);
                let details = failure.details.get_or_insert_with(Map::new);
                details.insert("server_events".to_string(), server_events);
                details.insert(
                    "session_disposition".to_string(),
                    Value::String(disposition),
                );
                if !evidence_write_errors.is_empty() {
                    details.insert(
                        "evidence_write_errors".to_string(),
                        Value::Array(evidence_write_errors),
                    );
                }
                ToolResultArtifact::failure(&call.tool, call.correlation_id, failure)
            }
        };
        materialize_completed_result(&call_root, &completed, &response).map_err(|error| {
            ToolProcessError::Unavailable(format!(
                "MCP stdio operation finished but its result could not be retained: {}; execution will not be repeated",
                error
            ))
        })?;
        hand_completed_result(io, &completed, event_projection).await
    }

    async fn room_entry(
        &self,
        formation: &WorkerEnvironmentFormation,
    ) -> Result<Arc<RoomEntry>, ToolProcessError> {
        let id = &formation.worker_environment_id;
        let durable_room_root = self
            .environment
            .durable_var_dir
            .as_ref()
            .ok_or_else(|| {
                ToolProcessError::Capability(
                    "MCP stdio room requires a formation-private durable state directory"
                        .to_string(),
                )
            })?
            .join("mcp-stdio");
        let room = {
            let mut rooms = self.rooms.lock().await;
            Arc::clone(rooms.entry(id.clone()).or_insert_with(|| {
                let room_root = durable_room_root.clone();
                Arc::new(RoomEntry {
                    released: AtomicBool::new(false),
                    state: tokio::sync::Mutex::new(RoomState {
                        generation: 0,
                        client: None,
                        room_root: room_root.clone(),
                        had_client: false,
                        connecting: false,
                        closing: false,
                    }),
                    projected_event_cursor: Arc::new(tokio::sync::Mutex::new(
                        RoomProjectionCursor::new(room_root.join(PROJECTED_EVENT_CURSOR_FILE)),
                    )),
                    changed: tokio::sync::Notify::new(),
                })
            }))
        };
        Ok(room)
    }

    async fn dispatch<R: McpStdioRoomIo + ?Sized>(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &McpStdioConfig,
        tool: &str,
        arguments: &Value,
        io: &R,
    ) -> Result<RoomDispatch, ToolProcessError> {
        let id = &formation.worker_environment_id;
        let room = self.room_entry(formation).await?;
        if room.released.load(Ordering::Acquire) {
            return Err(ToolProcessError::Unavailable(format!(
                "MCP stdio worker environment '{id}' is being released"
            )));
        }
        if tool == "close" {
            let result = self.close_room_entry(&room).await;
            let mut evidence_write_errors = Vec::new();
            if let Err(error) = io
                .record(
                    formation,
                    if result.is_ok() {
                        WorkerEnvironmentLifecycle::Active
                    } else {
                        WorkerEnvironmentLifecycle::Failed
                    },
                )
                .await
            {
                evidence_write_errors.push(evidence_write_error("lifecycle", error));
            }
            if result.is_ok() {
                if let Err(error) = self
                    .write_live_state(io, formation, &room, "closed", None)
                    .await
                {
                    evidence_write_errors.push(evidence_write_error("live_state", error));
                }
            }
            return Ok(RoomDispatch {
                body: result.map(|()| {
                    json!({
                        "worker_environment_id": id,
                        "closed": true,
                    })
                }),
                events: Vec::new(),
                event_projection: None,
                disposition: "closed".to_string(),
                evidence_write_errors,
            });
        }
        let status_state = room.state.lock().await;
        if tool == "status" && status_state.client.is_none() && !status_state.connecting {
            let generation = status_state.generation;
            let had_client = status_state.had_client;
            drop(status_state);
            if had_client {
                io.record(formation, WorkerEnvironmentLifecycle::Active)
                    .await
                    .map_err(ToolProcessError::Unavailable)?;
                self.write_live_state(io, formation, &room, "closed", None)
                    .await
                    .map_err(ToolProcessError::Unavailable)?;
            } else {
                io.record(formation, WorkerEnvironmentLifecycle::Failed)
                    .await
                    .map_err(ToolProcessError::Unavailable)?;
            }
            let status = if had_client { "closed" } else { "not_started" };
            return Ok(RoomDispatch {
                body: Ok(json!({
                    "worker_environment_id": id,
                    "status": status,
                    "generation": generation,
                })),
                events: Vec::new(),
                event_projection: None,
                disposition: status.to_string(),
                evidence_write_errors: Vec::new(),
            });
        }
        drop(status_state);
        let (client, disposition) = match self.acquire_room_client(&room, config).await {
            Ok(client) => client,
            Err(error) => {
                io.record(formation, WorkerEnvironmentLifecycle::Failed)
                    .await
                    .map_err(ToolProcessError::Unavailable)?;
                return Ok(RoomDispatch {
                    body: Err(error),
                    events: Vec::new(),
                    event_projection: None,
                    disposition: "failed".to_string(),
                    evidence_write_errors: Vec::new(),
                });
            }
        };
        io.record(formation, WorkerEnvironmentLifecycle::Active)
            .await
            .map_err(ToolProcessError::Unavailable)?;
        let body = execute_operation(&client, tool, arguments, id).await;
        let live_error = body.as_ref().err();
        let session_failed = live_error.is_some_and(|error| is_transport_failure(error));
        let mut evidence_write_errors = Vec::new();
        if session_failed {
            self.retire_room_client(&room, &client).await;
            if let Err(error) = io
                .record(formation, WorkerEnvironmentLifecycle::Failed)
                .await
            {
                evidence_write_errors.push(evidence_write_error("lifecycle", error));
            }
        } else {
            if let Err(error) = self
                .write_live_state(io, formation, &room, "running", live_error)
                .await
            {
                evidence_write_errors.push(evidence_write_error("live_state", error));
            }
        }
        let (events, event_projection) = match claim_room_events(&room, &client).await {
            Ok(events) => events,
            Err(error) => {
                evidence_write_errors.push(evidence_write_error("server_event_claim", error));
                (Vec::new(), None)
            }
        };
        Ok(RoomDispatch {
            body,
            events,
            event_projection,
            disposition,
            evidence_write_errors,
        })
    }

    async fn acquire_room_client(
        &self,
        room: &Arc<RoomEntry>,
        config: &McpStdioConfig,
    ) -> Result<(Arc<StdioMcpClient>, String), String> {
        loop {
            if room.released.load(Ordering::Acquire) {
                return Err("MCP stdio worker environment is released".to_string());
            }
            let notified = room.changed.notified();
            let spawn = {
                let mut state = room.state.lock().await;
                if let Some(client) = state.client.as_ref() {
                    let client = Arc::clone(client);
                    drop(state);
                    match client.ensure_alive().await {
                        Ok(()) => return Ok((client, "attached".to_string())),
                        Err(_) => {
                            self.retire_room_client(room, &client).await;
                            continue;
                        }
                    }
                }
                if state.connecting || state.closing {
                    None
                } else {
                    state.generation = state
                        .generation
                        .checked_add(1)
                        .ok_or_else(|| "MCP stdio room generation overflowed".to_string())?;
                    let cwd = config.cwd(&self.environment, &state.room_root)?;
                    state.connecting = true;
                    Some((cwd, state.room_root.clone(), state.had_client))
                }
            };
            let Some((cwd, room_root, had_client)) = spawn else {
                notified.await;
                continue;
            };
            let mut transition = RoomTransitionGuard {
                room: Arc::clone(room),
                transition: RoomTransition::Connecting,
                armed: true,
            };
            let spawned = StdioMcpClient::spawn(config, &cwd, &room_root)
                .await
                .map(Arc::new);
            let mut state = room.state.lock().await;
            state.connecting = false;
            transition.disarm();
            if room.released.load(Ordering::Acquire) {
                drop(state);
                if let Ok(client) = spawned.as_ref() {
                    let _ = client.terminate().await;
                }
                room.changed.notify_waiters();
                return Err("MCP stdio worker environment was released during connect".to_string());
            }
            match spawned {
                Ok(client) => {
                    state.had_client = true;
                    state.client = Some(Arc::clone(&client));
                    drop(state);
                    room.changed.notify_waiters();
                    let disposition = if had_client { "restarted" } else { "connected" };
                    return Ok((client, disposition.to_string()));
                }
                Err(error) => {
                    drop(state);
                    room.changed.notify_waiters();
                    return Err(error);
                }
            }
        }
    }

    async fn retire_room_client(&self, room: &Arc<RoomEntry>, expected: &Arc<StdioMcpClient>) {
        let client = {
            let mut state = room.state.lock().await;
            if state
                .client
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected))
                && !state.closing
            {
                state.closing = true;
                state.client.take()
            } else {
                None
            }
        };
        if let Some(client) = client {
            let mut transition = RoomTransitionGuard {
                room: Arc::clone(room),
                transition: RoomTransition::Closing,
                armed: true,
            };
            let _ = client.terminate().await;
            let mut state = room.state.lock().await;
            state.closing = false;
            transition.disarm();
            drop(state);
            room.changed.notify_waiters();
        }
    }

    async fn close_room_entry(&self, room: &Arc<RoomEntry>) -> Result<(), String> {
        loop {
            let notified = room.changed.notified();
            let client = {
                let mut state = room.state.lock().await;
                if state.connecting || state.closing {
                    None
                } else {
                    let Some(client) = state.client.take() else {
                        return if state.had_client {
                            Ok(())
                        } else {
                            Err("MCP stdio room is not running".to_string())
                        };
                    };
                    state.closing = true;
                    Some(client)
                }
            };
            let Some(client) = client else {
                notified.await;
                continue;
            };
            let mut transition = RoomTransitionGuard {
                room: Arc::clone(room),
                transition: RoomTransition::Closing,
                armed: true,
            };
            let result = client.terminate().await;
            let mut state = room.state.lock().await;
            state.closing = false;
            transition.disarm();
            drop(state);
            room.changed.notify_waiters();
            return result;
        }
    }

    async fn write_live_state<R: WorkerEnvironmentLiveStateRecorder + ?Sized>(
        &self,
        recorder: &R,
        formation: &WorkerEnvironmentFormation,
        room: &Arc<RoomEntry>,
        status: &str,
        error: Option<&String>,
    ) -> Result<(), String> {
        let (generation, client) = {
            let state = room.state.lock().await;
            (state.generation, state.client.clone())
        };
        let sanitized_error = error.map(|error| sanitize_error(error));
        let server = match client.as_ref() {
            Some(client) => Some(json!({
                "pid": client.pid().await,
                "command": client.command(),
                "args": client.args(),
                "initialize": client.initialize_result().await,
                "capabilities": client.capabilities().await.as_value(),
                "protocol_version": client.protocol_version().await,
                "event_cursor": client.event_cursor().await,
                "in_flight_request_ids": client.in_flight_request_ids()?,
            })),
            None => None,
        };
        let body = json!({
            "schema_version": 1,
            "worker_environment_id": formation.worker_environment_id,
            "substrate_family": SUBSTRATE_FAMILY,
            "status": status,
            "generation": generation,
            "server": server,
            "viewer": client.as_ref().map(|client| json!({
                "protocol": "mcp_stdio_room",
                "event_log": client.event_log(),
                "stderr_log": client.stderr_log(),
            })),
            "error": sanitized_error,
        });
        recorder.record_live_state(formation, &body).await
    }

    async fn release_room(&self, formation: &WorkerEnvironmentFormation) -> Result<(), String> {
        let room = {
            let rooms = self.rooms.lock().await;
            rooms.get(&formation.worker_environment_id).map(Arc::clone)
        };
        let Some(room) = room else {
            return Ok(());
        };
        room.released.store(true, Ordering::Release);
        room.changed.notify_waiters();
        let result = loop {
            let notified = room.changed.notified();
            let client = {
                let mut state = room.state.lock().await;
                if state.connecting || state.closing {
                    None
                } else {
                    state.closing = true;
                    Some(state.client.take())
                }
            };
            let Some(client) = client else {
                notified.await;
                continue;
            };
            let mut transition = RoomTransitionGuard {
                room: Arc::clone(&room),
                transition: RoomTransition::Closing,
                armed: true,
            };
            let result = match client {
                Some(client) => client.terminate().await,
                None => Ok(()),
            };
            let mut state = room.state.lock().await;
            state.closing = false;
            transition.disarm();
            drop(state);
            room.changed.notify_waiters();
            break result;
        };
        let mut rooms = self.rooms.lock().await;
        if rooms
            .get(&formation.worker_environment_id)
            .is_some_and(|current| Arc::ptr_eq(current, &room))
        {
            rooms.remove(&formation.worker_environment_id);
        }
        result
    }
}

impl ToolProcessHandler for McpStdioRoom {
    fn substrate_family(&self) -> &str {
        SUBSTRATE_FAMILY
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
        Box::pin(async move { self.receive(invocation, &io).await })
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

}

pub async fn run_mcp_stdio_room_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    let room = Arc::new(McpStdioRoom::new(environment));
    octopus_tool_door_runtime::run_tool_process(room).await
}

async fn claim_room_events(
    room: &Arc<RoomEntry>,
    client: &StdioMcpClient,
) -> Result<(Vec<Value>, Option<RoomEventProjection>), String> {
    let mut cursor = Arc::clone(&room.projected_event_cursor).lock_owned().await;
    let after_cursor = cursor.load()?;
    let events = client
        .read_events(after_cursor, MAX_SERVER_EVENTS_PER_CALL)
        .await?;
    if events.is_empty() {
        return Ok((events, None));
    }
    for (offset, event) in events.iter().enumerate() {
        let expected = after_cursor
            .checked_add(offset as u64)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "MCP stdio projected event cursor overflowed".to_string())?;
        if event.get("seq").and_then(Value::as_u64) != Some(expected) {
            return Err(format!(
                "MCP stdio projected event sequence is not contiguous at cursor {expected}"
            ));
        }
    }
    let next_cursor = events
        .last()
        .and_then(|event| event.get("seq"))
        .and_then(Value::as_u64)
        .ok_or_else(|| "MCP stdio projected event has no sequence".to_string())?;
    Ok((
        events,
        Some(RoomEventProjection {
            cursor,
            after_cursor,
            next_cursor,
        }),
    ))
}

async fn execute_operation(
    client: &StdioMcpClient,
    tool: &str,
    arguments: &Value,
    id: &WorkerEnvironmentId,
) -> Result<Value, String> {
    let object = arguments
        .as_object()
        .ok_or_else(|| format!("{tool} arguments are not an object"))?;
    let result = match tool {
        "call_tool" => {
            require_capability(client.capabilities().await.tools, "tools")?;
            let name = required_string(object, "name")?;
            let arguments = object
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !arguments.is_object() {
                return Err("MCP call_tool arguments must be an object".to_string());
            }
            let mut params = json!({ "name": name, "arguments": arguments });
            if let Some(progress_token) = object.get("progress_token") {
                if !progress_token.is_string() && !progress_token.is_number() {
                    return Err(
                        "MCP call_tool progress_token must be a string or number".to_string()
                    );
                }
                params["_meta"] = json!({ "progressToken": progress_token });
            }
            match optional_u64(object, "timeout_ms")? {
                Some(timeout_ms) if (1..=600_000).contains(&timeout_ms) => {
                    client
                        .request_with_timeout(
                            "tools/call",
                            params,
                            std::time::Duration::from_millis(timeout_ms),
                        )
                        .await?
                }
                Some(_) => return Err("MCP call_tool timeout_ms must be in 1..=600000".to_string()),
                None => client.request("tools/call", params).await?,
            }
        }
        "list_tools" => {
            require_capability(client.capabilities().await.tools, "tools")?;
            client.request("tools/list", cursor_params(object)?).await?
        }
        "list_resources" => {
            require_capability(client.capabilities().await.resources, "resources")?;
            client
                .request("resources/list", cursor_params(object)?)
                .await?
        }
        "list_resource_templates" => {
            require_capability(client.capabilities().await.resources, "resources")?;
            client
                .request("resources/templates/list", cursor_params(object)?)
                .await?
        }
        "read_resource" => {
            require_capability(client.capabilities().await.resources, "resources")?;
            let uri = required_string(object, "uri")?;
            client
                .request("resources/read", json!({ "uri": uri }))
                .await?
        }
        "subscribe_resource" => {
            require_capability(
                client.capabilities().await.resources_subscribe,
                "resources.subscribe",
            )?;
            let uri = required_string(object, "uri")?;
            client
                .request("resources/subscribe", json!({ "uri": uri }))
                .await?
        }
        "unsubscribe_resource" => {
            require_capability(
                client.capabilities().await.resources_subscribe,
                "resources.subscribe",
            )?;
            let uri = required_string(object, "uri")?;
            client
                .request("resources/unsubscribe", json!({ "uri": uri }))
                .await?
        }
        "list_prompts" => {
            require_capability(client.capabilities().await.prompts, "prompts")?;
            client
                .request("prompts/list", cursor_params(object)?)
                .await?
        }
        "get_prompt" => {
            require_capability(client.capabilities().await.prompts, "prompts")?;
            let name = required_string(object, "name")?;
            let arguments = object
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            client
                .request(
                    "prompts/get",
                    json!({ "name": name, "arguments": arguments }),
                )
                .await?
        }
        "complete" => {
            require_capability(client.capabilities().await.completions, "completions")?;
            let reference = object
                .get("ref")
                .cloned()
                .ok_or_else(|| "MCP complete ref is absent".to_string())?;
            let argument = object
                .get("argument")
                .cloned()
                .ok_or_else(|| "MCP complete argument is absent".to_string())?;
            let mut params = json!({ "ref": reference, "argument": argument });
            if let Some(context) = object.get("context") {
                params["context"] = context.clone();
            }
            client.request("completion/complete", params).await?
        }
        "set_logging_level" => {
            require_capability(client.capabilities().await.logging, "logging")?;
            let level = required_string(object, "level")?;
            client
                .request("logging/setLevel", json!({ "level": level }))
                .await?
        }
        "ping" => client.request("ping", json!({})).await?,
        "cancel_request" => {
            let request_id = object
                .get("request_id")
                .cloned()
                .ok_or_else(|| "MCP cancel_request request_id is absent".to_string())?;
            client
                .cancel_request(request_id, object.get("reason").and_then(Value::as_str))
                .await?;
            return Ok(json!({
                "worker_environment_id": id,
                "cancelled": true,
                "event_cursor": client.event_cursor().await,
            }));
        }
        "send_progress" => {
            let progress_token = object
                .get("progress_token")
                .cloned()
                .ok_or_else(|| "MCP send_progress progress_token is absent".to_string())?;
            if !progress_token.is_string() && !progress_token.is_number() {
                return Err("MCP progress_token must be a string or number".to_string());
            }
            let progress = object
                .get("progress")
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or_else(|| "MCP progress must be finite and non-negative".to_string())?;
            let mut params = json!({
                "progressToken": progress_token,
                "progress": progress,
            });
            if let Some(total) = object.get("total").and_then(Value::as_f64) {
                if !total.is_finite() || total < progress {
                    return Err(
                        "MCP progress total must be finite and not below progress".to_string()
                    );
                }
                params["total"] = json!(total);
            }
            if let Some(message) = object.get("message") {
                params["message"] = message.clone();
            }
            client.notify("notifications/progress", params).await?;
            return Ok(json!({
                "worker_environment_id": id,
                "sent": true,
                "event_cursor": client.event_cursor().await,
            }));
        }
        "poll_events" => {
            let after_cursor = optional_u64(object, "after_cursor")?.unwrap_or(0);
            let max_events = optional_u64(object, "max_events")?.unwrap_or(64);
            let wait_ms = optional_u64(object, "wait_ms")?.unwrap_or(0);
            if !(1..=256).contains(&max_events) {
                return Err("mcp.poll_events max_events must be in 1..=256".to_string());
            }
            if wait_ms > 30_000 {
                return Err("mcp.poll_events wait_ms must be in 0..=30000".to_string());
            }
            let events = client
                .poll_events(
                    after_cursor,
                    usize::try_from(max_events)
                        .map_err(|_| "mcp.poll_events max_events exceeds usize".to_string())?,
                    wait_ms,
                )
                .await?;
            let next_cursor = events
                .last()
                .and_then(|event| event.get("seq"))
                .and_then(Value::as_u64)
                .unwrap_or(after_cursor);
            return Ok(json!({
                "worker_environment_id": id,
                "events": events,
                "next_cursor": next_cursor,
                "durable": true,
            }));
        }
        "status" => {
            return Ok(json!({
                "worker_environment_id": id,
                "status": "running",
                "pid": client.pid().await,
                "initialize": client.initialize_result().await,
                "capabilities": client.capabilities().await.as_value(),
                "protocol_version": client.protocol_version().await,
                "event_cursor": client.event_cursor().await,
                "in_flight_request_ids": client.in_flight_request_ids()?,
                "viewer": {
                    "protocol": "mcp_stdio_room",
                    "event_log": client.event_log(),
                    "stderr_log": client.stderr_log(),
                }
            }));
        }
        other => return Err(format!("MCP stdio room does not host operation '{other}'")),
    };
    Ok(json!({
        "worker_environment_id": id,
        "result": result,
        "event_cursor": client.event_cursor().await,
    }))
}

fn require_capability(supported: bool, capability: &str) -> Result<(), String> {
    if supported {
        Ok(())
    } else {
        Err(format!(
            "MCP server did not declare the '{capability}' capability during initialize"
        ))
    }
}

fn room_error(message: &str) -> ToolResultError {
    let lower = message.to_ascii_lowercase();
    let code = if lower.contains("timed out") {
        "timeout"
    } else if lower.contains("capability") || lower.contains("did not declare") {
        "unsupported_capability"
    } else if lower.contains("exited") || lower.contains("closed") || lower.contains("broken pipe")
    {
        "transport_closed"
    } else if lower.contains("server error code=") {
        "server_error"
    } else {
        "protocol_or_config"
    };
    let mut details = Map::new();
    details.insert(
        "retryable".to_string(),
        Value::Bool(matches!(code, "timeout" | "transport_closed")),
    );
    ToolResultError::new(code, sanitize_error(message)).with_details(details)
}

fn with_session_disposition(mut body: Value, disposition: String, server_events: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.insert("server_events".to_string(), server_events);
        object.insert(
            "session_disposition".to_string(),
            Value::String(disposition),
        );
        body
    } else {
        json!({
            "result": body,
            "session_disposition": disposition,
            "server_events": server_events,
        })
    }
}

fn evidence_write_error(stage: &str, error: impl AsRef<str>) -> Value {
    json!({
        "stage": stage,
        "message": sanitize_error(error.as_ref()),
    })
}

fn sanitize_error(value: &str) -> String {
    let mut words = Vec::new();
    let mut redact_next = false;
    for word in value.split_whitespace() {
        if redact_next {
            words.push("[REDACTED]");
            redact_next = false;
            continue;
        }
        let lower = word.to_ascii_lowercase();
        if lower == "bearer" || lower.ends_with(":bearer") {
            words.push(word);
            redact_next = true;
        } else if lower.contains("token=")
            || lower.contains("key=")
            || lower.contains("password=")
            || lower.contains("secret=")
            || word.starts_with("sk-")
            || word.starts_with("ghp_")
        {
            words.push("[REDACTED]");
        } else {
            words.push(word);
        }
    }
    words.join(" ")
}

fn is_transport_failure(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("server exited")
        || lower.contains("closed stdout")
        || lower.contains("message reader stopped")
        || lower.contains("stdin write failed")
        || lower.contains("stdin flush failed")
        || lower.contains("stdout framing failed")
        || lower.contains("broken pipe")
        || lower.contains("invalid or expired session")
        || lower.contains("session expired")
        || lower.contains("session not found")
        || lower.contains("unknown session")
        || lower.contains("session terminated")
}

fn materialize_completed_result(
    root: &Path,
    completed: &InterfaceArtifactFile,
    response: &ToolResultArtifact,
) -> Result<(), ToolProcessError> {
    let bytes = serde_json::to_vec(response)
        .map_err(|error| ToolProcessError::Unavailable(error.to_string()))?;
    let artifact = InterfaceArtifactFile::write(root, &response.label, &bytes)
        .map_err(|error| ToolProcessError::Unavailable(error.to_string()))?;
    let persist = || -> Result<(), String> {
        let manifest = artifact
            .read_manifest()
            .map_err(|error| error.to_string())?;
        std::fs::File::open(&manifest.payload_path)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::File::open(artifact.manifest_path())
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::hard_link(artifact.manifest_path(), completed.manifest_path())
            .map_err(|error| error.to_string())?;
        std::fs::File::open(root)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        std::fs::File::open(root.parent().ok_or("MCP result root has no parent")?)
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())?;
        Ok(())
    };
    persist().map_err(ToolProcessError::Unavailable)
}

async fn hand_completed_result<I: McpStdioRoomIo + ?Sized>(
    io: &I,
    artifact: &InterfaceArtifactFile,
    projection: Option<RoomEventProjection>,
) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
    let delivery_error = match io.hand_output_artifact(artifact).await {
        Ok(DeliveryVerdict::Admitted { .. }) => None,
        Ok(DeliveryVerdict::Refused { reason, detail }) => {
            Some(format!("refused as '{}': {detail}", reason.as_str()))
        }
        Err(error) => Some(format!("delivery acknowledgement unavailable: {error}")),
    };
    if let Some(error) = delivery_error {
        return Err(ToolProcessError::Unavailable(format!(
            "MCP stdio completed result '{}' hand-in {}; operation outcome is retained, projection cursor unchanged, execution will not be repeated",
            artifact.manifest_path().display(), sanitize_error(&error)
        )));
    }
    if let Some(projection) = projection {
        projection.commit().map_err(|error| ToolProcessError::Unavailable(format!(
            "MCP stdio result was admitted but its projection cursor could not commit: {error}; execution will not be repeated"
        )))?;
    }
    Ok(Vec::new())
}

async fn reclaim_room_events(
    room: &Arc<RoomEntry>,
    formation: &WorkerEnvironmentFormation,
    evidence: &RoomEventEvidence,
) -> Result<Option<RoomEventProjection>, String> {
    if evidence.worker_environment_id != formation.worker_environment_id
        || evidence.events.len() > MAX_SERVER_EVENTS_PER_CALL
    {
        return Err("MCP stdio completed result has invalid server-event evidence".to_string());
    }
    let (after_cursor, next_cursor) = match (evidence.after_cursor, evidence.next_cursor) {
        (None, None) if evidence.events.is_empty() => return Ok(None),
        (Some(after), Some(next)) if !evidence.events.is_empty() => (after, next),
        _ => return Err("MCP stdio completed result has invalid projection cursors".to_string()),
    };
    if after_cursor.checked_add(evidence.events.len() as u64) != Some(next_cursor)
        || evidence.events.iter().enumerate().any(|(offset, event)| {
            event.get("seq").and_then(Value::as_u64) != after_cursor.checked_add(offset as u64 + 1)
        })
    {
        return Err("MCP stdio completed result has noncontiguous server events".to_string());
    }
    let mut cursor = Arc::clone(&room.projected_event_cursor).lock_owned().await;
    let current = cursor.load()?;
    if current >= next_cursor {
        return Ok(None);
    }
    if current != after_cursor {
        return Err(format!(
            "MCP stdio completed result claimed cursor {after_cursor}, current cursor is {current}"
        ));
    }
    Ok(Some(RoomEventProjection {
        cursor,
        after_cursor,
        next_cursor,
    }))
}

fn cursor_params(object: &Map<String, Value>) -> Result<Value, String> {
    match object.get("cursor") {
        None | Some(Value::Null) => Ok(json!({})),
        Some(Value::String(cursor)) if !cursor.trim().is_empty() => {
            Ok(json!({ "cursor": cursor.trim() }))
        }
        Some(_) => Err("MCP cursor is not a non-empty string or null".to_string()),
    }
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("MCP stdio argument '{key}' is missing or empty"))
}

fn optional_u64(object: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("MCP stdio argument '{key}' is not an unsigned integer")),
    }
}
