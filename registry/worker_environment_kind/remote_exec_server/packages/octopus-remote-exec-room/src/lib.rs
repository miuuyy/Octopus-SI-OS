use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use octopus_exec::{
    ByteChunk, Environment, ExecEnvPolicy, ExecOutputStream, ExecParams, ExecProcess,
    ExecutorFileSystem, FsRootedOperation, FsRootedParams, FsRootedResponse, ProcessId,
    ProcessSignal, WriteStatus,
};
use octopus_interfaces::InterfaceArtifactFile;
use octopus_tool_door_runtime::room::{
    decode_tool_call, descriptor_operation, emit_json_artifact, formed_scope, strict_object,
};
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo, WorkerEnvironmentLifecycleRecorder, WorkerEnvironmentLiveStateRecorder,
    WorkerEnvironmentReleaseContext, WorkerEnvironmentReleaseMode,
};
use octopus_tool_handlers::{ToolResultArtifact, ToolResultError};
use octopus_util_pathuri::PathUri;
use octopus_worker_env::{
    WorkerEnvKindSpec, WorkerEnvironmentFormation, WorkerEnvironmentId, WorkerEnvironmentLifecycle,
};
use serde_json::{json, Map, Value};

mod config;
mod recovery;

use config::RemoteExecConfig;
use recovery::{
    DurableProcessRecord, DurableProcessStatus, DurableSessionStatus, RecoveryStore,
    RemoteRoomIdentity,
};

const ROOM: &str = "remote exec room";
const SUBSTRATE_FAMILY: &str = "remote_exec_server";
const OPERATIONS: &[&str] = &[
    "process_start", "process_read", "process_write", "process_interrupt", "process_terminate",
    "process_list", "status", "exec", "file_read", "file_write", "file_list", "file_stat",
    "file_delete", "file_copy", "directory_create",
];
const READ_CHUNK_BYTES: usize = 1024 * 1024;

struct RoomEntry {
    state: tokio::sync::Mutex<RoomState>,
    changed: tokio::sync::Notify,
}

struct RoomState {
    identity: RemoteRoomIdentity,
    store: Arc<RecoveryStore>,
    request_timeout: Duration,
    environment: Option<Arc<Environment>>,
    session_id: Option<String>,
    unavailable: Option<String>,
    initializing: bool,
    calls: u64,
    processes: BTreeMap<String, RemoteProcessState>,
}

#[derive(Clone)]
struct RemoteProcessState {
    process: Arc<dyn ExecProcess>,
    session_id: String,
    remote_process_id: String,
    generation: u64,
}

struct RemoteAccess {
    environment: Option<Arc<Environment>>,
    store: Arc<RecoveryStore>,
    session_id: Option<String>,
    call_sequence: u64,
    unavailable: Option<String>,
}

struct InitializedRoom {
    environment: Option<Arc<Environment>>,
    session_id: Option<String>,
    unavailable: Option<String>,
    processes: BTreeMap<String, RemoteProcessState>,
}

pub trait WorkerEnvironmentRecorder:
    WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder
{
}

impl<T> WorkerEnvironmentRecorder for T where
    T: WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder + ?Sized
{
}

pub struct RemoteExecRoom {
    environment: ToolProcessEnvironment,
    rooms: tokio::sync::Mutex<BTreeMap<WorkerEnvironmentId, Arc<RoomEntry>>>,
}

impl RemoteExecRoom {
    pub fn new(environment: ToolProcessEnvironment) -> Self {
        Self {
            environment,
            rooms: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn receive(
        &self,
        invocation: ToolProcessInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
        let ToolProcessInvocation {
            descriptor,
            envelope,
            source_movement_seq,
            worker_environment,
            worker_environment_kind,
            ..
        } = invocation;
        let formation = worker_environment.ok_or_else(|| {
            ToolProcessError::Capability(
                "remote exec invocation has no formed worker environment".to_string(),
            )
        })?;
        let kind = worker_environment_kind.ok_or_else(|| {
            ToolProcessError::Capability(
                "remote exec invocation has no formed kind descriptor".to_string(),
            )
        })?;
        let config = RemoteExecConfig::load_formed(&self.environment, &formation, &kind)
            .map_err(ToolProcessError::Capability)?;
        let call = decode_tool_call(ROOM, &envelope)?;
        if descriptor.id.as_str() != call.tool {
            return Err(ToolProcessError::Capability(format!(
                "tool descriptor '{}' does not match tool.call '{}'",
                descriptor.id, call.tool
            )));
        }
        let operation = descriptor_operation(ROOM, descriptor.id.as_str(), &descriptor.handler_config, OPERATIONS)
            .map_err(ToolProcessError::Capability)?;
        formed_scope(ROOM, &envelope, &formation)?;
        let access = self
            .remote_environment(&formation, &config)
            .await
            .map_err(ToolProcessError::Unavailable)?;
        recorder
            .record(
                &formation,
                if access.environment.is_some() {
                    WorkerEnvironmentLifecycle::Active
                } else {
                    WorkerEnvironmentLifecycle::Failed
                },
            )
            .await
            .map_err(ToolProcessError::Unavailable)?;
        let body = self
            .dispatch(
                access.environment.as_deref(),
                &access.store,
                &formation,
                &config,
                source_movement_seq,
                access.unavailable.as_deref(),
                operation,
                &call.arguments,
            )
            .await;
        if body.is_err() {
            if let Some(remote) = access.environment.as_deref() {
                let session_probe = tokio::time::timeout(config.timeout(), remote.info()).await;
                if !matches!(session_probe, Ok(Ok(_))) {
                    self.mark_session_lost(
                        &formation,
                        &access.store,
                        access.session_id.as_deref().ok_or_else(|| {
                            ToolProcessError::Unavailable(
                                "active remote exec environment has no session identity"
                                    .to_string(),
                            )
                        })?,
                        "remote_session_lost",
                        "remote exec server session could not be authenticated after an operation failure",
                    )
                    .await
                    .map_err(ToolProcessError::Unavailable)?;
                }
            }
        }
        let status = if body.is_ok() { "ready" } else { "failed" };
        let detail = body.as_ref().err().cloned();
        let live_state = self
            .write_live_state(
                recorder,
                &formation,
                &config,
                access.call_sequence,
                status,
                detail,
            )
            .await;
        let response = match (body, live_state) {
            (Ok(body), Ok(())) => {
                ToolResultArtifact::success(&call.tool, call.correlation_id, body)
            }
            (Ok(body), Err(error)) => {
                let mut details = Map::new();
                details.insert("operation_body".to_string(), body);
                ToolResultArtifact::failure(
                    &call.tool,
                    call.correlation_id,
                    ToolResultError::new("remote_exec_evidence_failed", error)
                        .with_details(details),
                )
            }
            (Err(error), Ok(())) => ToolResultArtifact::failure(
                &call.tool,
                call.correlation_id,
                ToolResultError::new("tool_execution_failed", error),
            ),
            (Err(error), Err(evidence_error)) => {
                let mut details = Map::new();
                details.insert("evidence_error".to_string(), Value::String(evidence_error));
                ToolResultArtifact::failure(
                    &call.tool,
                    call.correlation_id,
                    ToolResultError::new("tool_execution_failed", error).with_details(details),
                )
            }
        };
        Ok(vec![emit_json_artifact(&envelope, &response.label, &response)
            .map_err(ToolProcessError::Capability)?])
    }

    async fn remote_environment(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &RemoteExecConfig,
    ) -> Result<RemoteAccess, String> {
        let durable_root = self.environment.durable_var_dir.as_ref().ok_or_else(|| {
            "remote exec room requires a formation-private durable state directory".to_string()
        })?;
        let auth = config.load_auth(&self.environment)?;
        let auth_identity = blake3::Hash::from_bytes(auth.identity())
            .to_hex()
            .to_string();
        let identity = RemoteRoomIdentity::new(
            formation,
            config.websocket_url.clone(),
            config.workspace()?.to_string(),
            auth_identity,
            config.identity()?,
        );
        let store = Arc::new(RecoveryStore::open(durable_root, identity.clone())?);
        let entry = {
            let mut rooms = self.rooms.lock().await;
            Arc::clone(
                rooms
                    .entry(formation.worker_environment_id.clone())
                    .or_insert_with(|| {
                        Arc::new(RoomEntry {
                            state: tokio::sync::Mutex::new(RoomState {
                                identity: identity.clone(),
                                store: Arc::clone(&store),
                                request_timeout: config.timeout(),
                                environment: None,
                                session_id: None,
                                unavailable: None,
                                initializing: false,
                                calls: 0,
                                processes: BTreeMap::new(),
                            }),
                            changed: tokio::sync::Notify::new(),
                        })
                    }),
            )
        };

        loop {
            let notified = entry.changed.notified();
            let initialize = {
                let mut state = entry.state.lock().await;
                if state.identity != identity || state.store.identity() != &identity {
                    return Err(
                        "remote exec connection, authentication, configuration, or formation identity changed while the formed room is active"
                            .to_string(),
                    );
                }
                if state.initializing {
                    false
                } else if state.environment.is_some()
                    || (state.unavailable.is_some() && state.store.has_unresolved_processes()?)
                {
                    state.calls = state
                        .calls
                        .checked_add(1)
                        .ok_or_else(|| "remote exec call sequence overflowed".to_string())?;
                    return Ok(RemoteAccess {
                        environment: state.environment.clone(),
                        store: Arc::clone(&state.store),
                        session_id: state.session_id.clone(),
                        call_sequence: state.calls,
                        unavailable: state.unavailable.clone(),
                    });
                } else {
                    state.initializing = true;
                    true
                }
            };
            if !initialize {
                notified.await;
                continue;
            }

            let initialized = self
                .initialize_remote_state(formation, config, auth.clone(), Arc::clone(&store))
                .await;
            let mut state = entry.state.lock().await;
            state.initializing = false;
            match initialized {
                Ok(initialized) => {
                    state.environment = initialized.environment;
                    state.session_id = initialized.session_id;
                    state.unavailable = initialized.unavailable;
                    state.processes = initialized.processes;
                    state.calls = state
                        .calls
                        .checked_add(1)
                        .ok_or_else(|| "remote exec call sequence overflowed".to_string())?;
                    let access = RemoteAccess {
                        environment: state.environment.clone(),
                        store: Arc::clone(&state.store),
                        session_id: state.session_id.clone(),
                        call_sequence: state.calls,
                        unavailable: state.unavailable.clone(),
                    };
                    drop(state);
                    entry.changed.notify_waiters();
                    return Ok(access);
                }
                Err(error) => {
                    drop(state);
                    entry.changed.notify_waiters();
                    return Err(error);
                }
            }
        }
    }

    async fn initialize_remote_state(
        &self,
        _formation: &WorkerEnvironmentFormation,
        config: &RemoteExecConfig,
        auth: octopus_exec::DirectExecServerAuth,
        store: Arc<RecoveryStore>,
    ) -> Result<InitializedRoom, String> {
        let session = store.load_session()?;
        if let Some(session) = session.as_ref() {
            if session.status == DurableSessionStatus::Claiming {
                return self
                    .finish_remote_session_claim(
                        config,
                        auth,
                        Arc::clone(&store),
                        session.session_id.clone(),
                    )
                    .await;
            }
            if session.status == DurableSessionStatus::Released {
                return Err("released remote exec session cannot be materialized again".to_string());
            }
            if session.status == DurableSessionStatus::Active {
                let environment = Arc::new(
                    Environment::remote_authenticated_resume(
                        config.websocket_url.clone(),
                        auth.clone(),
                        session.session_id.clone(),
                    )
                    .map_err(|error| format!("remote exec resume environment refused: {error}"))?,
                );
                environment.start_connecting();
                match ensure_ready(&environment, config).await {
                    Ok(()) => {
                        let confirmed = environment.remote_session_id().await.map_err(|error| {
                            format!("remote exec session identity failed: {error}")
                        })?;
                        if confirmed != session.session_id {
                            return Err(
                                "remote exec resumed with an unexpected server session identity"
                                    .to_string(),
                            );
                        }
                        let processes = self
                            .attach_durable_processes(&environment, config, &store)
                            .await?;
                        return Ok(InitializedRoom {
                            environment: Some(environment),
                            session_id: Some(confirmed),
                            unavailable: None,
                            processes,
                        });
                    }
                    Err(error) => {
                        let now = unix_ms()?;
                        store.mark_session_lost(
                            &session.session_id,
                            "remote_session_lost",
                            &error,
                            now,
                        )?;
                        for process in store.load_processes()? {
                            if matches!(
                                process.status,
                                DurableProcessStatus::Starting | DurableProcessStatus::Active
                            ) {
                                store.mark_process_lost(
                                    &process.process_id,
                                    &process.session_id,
                                    &process.remote_process_id,
                                    process.generation,
                                    "remote_session_lost",
                                    &error,
                                    now,
                                )?;
                            }
                        }
                        if store.has_unresolved_processes()? {
                            return Ok(InitializedRoom {
                                environment: None,
                                session_id: Some(session.session_id.clone()),
                                unavailable: Some(
                                    "remote exec server session is lost; lost process records require explicit remote.process_terminate resolution"
                                        .to_string(),
                                ),
                                processes: BTreeMap::new(),
                            });
                        }
                    }
                }
            }
        }

        if store.has_unresolved_processes()? {
            return Ok(InitializedRoom {
                environment: None,
                session_id: session.map(|record| record.session_id),
                unavailable: Some(
                    "remote exec durable process state is unresolved; call remote.process_terminate for each lost process before forming a new server session"
                        .to_string(),
                ),
                processes: BTreeMap::new(),
            });
        }

        let session_id = store.identity().session_claim_id()?;
        store.begin_session_claim(session_id.clone(), unix_ms()?)?;
        self.finish_remote_session_claim(config, auth, store, session_id)
            .await
    }

    async fn finish_remote_session_claim(
        &self,
        config: &RemoteExecConfig,
        auth: octopus_exec::DirectExecServerAuth,
        store: Arc<RecoveryStore>,
        session_id: String,
    ) -> Result<InitializedRoom, String> {
        let environment = Arc::new(
            Environment::remote_authenticated_claim(
                config.websocket_url.clone(),
                auth,
                session_id.clone(),
            )
            .map_err(|error| format!("remote exec claim environment refused: {error}"))?,
        );
        environment.start_connecting();
        ensure_ready(&environment, config).await?;
        let confirmed = environment
            .remote_session_id()
            .await
            .map_err(|error| format!("remote exec session identity failed: {error}"))?;
        if confirmed != session_id {
            return Err(
                "remote exec claim returned an unexpected server session identity".to_string(),
            );
        }
        store.activate_session(confirmed.clone(), unix_ms()?)?;
        Ok(InitializedRoom {
            environment: Some(environment),
            session_id: Some(confirmed),
            unavailable: None,
            processes: BTreeMap::new(),
        })
    }

    async fn attach_durable_processes(
        &self,
        environment: &Environment,
        config: &RemoteExecConfig,
        store: &RecoveryStore,
    ) -> Result<BTreeMap<String, RemoteProcessState>, String> {
        let mut attached = BTreeMap::new();
        for record in store.load_processes()? {
            if !matches!(
                record.status,
                DurableProcessStatus::Starting | DurableProcessStatus::Active
            ) {
                continue;
            }
            let remote_process_id = ProcessId::new(record.remote_process_id.clone());
            let recovered = async {
                let started = environment.attach_exec_process(&remote_process_id).await?;
                let response = started.process.read(None, Some(1), Some(0)).await?;
                Ok::<_, octopus_exec::ExecServerError>((started.process, response))
            };
            match tokio::time::timeout(config.timeout(), recovered).await {
                Ok(Ok((process, response))) if !response.exited && !response.closed => {
                    store.mark_process_active(
                        &record.process_id,
                        &record.session_id,
                        &record.remote_process_id,
                        record.generation,
                        unix_ms()?,
                    )?;
                    attached.insert(
                        record.process_id.clone(),
                        RemoteProcessState {
                            process,
                            session_id: record.session_id.clone(),
                            remote_process_id: record.remote_process_id.clone(),
                            generation: record.generation,
                        },
                    );
                }
                Ok(Ok((_process, response))) => {
                    store.mark_process_terminal(
                        &record.process_id,
                        &record.session_id,
                        &record.remote_process_id,
                        record.generation,
                        response.exit_code,
                        unix_ms()?,
                    )?;
                }
                Ok(Err(error)) => {
                    store.mark_process_lost(
                        &record.process_id,
                        &record.session_id,
                        &record.remote_process_id,
                        record.generation,
                        "remote_process_missing",
                        &error.to_string(),
                        unix_ms()?,
                    )?;
                }
                Err(_) => {
                    store.mark_process_lost(
                        &record.process_id,
                        &record.session_id,
                        &record.remote_process_id,
                        record.generation,
                        "remote_process_attach_timeout",
                        "exact remote process attachment timed out",
                        unix_ms()?,
                    )?;
                }
            }
        }
        Ok(attached)
    }

    async fn mark_session_lost(
        &self,
        formation: &WorkerEnvironmentFormation,
        store: &RecoveryStore,
        expected_session_id: &str,
        code: &str,
        detail: &str,
    ) -> Result<(), String> {
        let now = unix_ms()?;
        store.mark_session_lost(expected_session_id, code, detail, now)?;
        for process in store.load_processes()? {
            if matches!(
                process.status,
                DurableProcessStatus::Starting | DurableProcessStatus::Active
            ) {
                store.mark_process_lost(
                    &process.process_id,
                    &process.session_id,
                    &process.remote_process_id,
                    process.generation,
                    code,
                    detail,
                    now,
                )?;
            }
        }
        if let Ok(entry) = self.room_entry(formation).await {
            let mut state = entry.state.lock().await;
            state.environment = None;
            state.unavailable = Some(
                "remote exec server session is lost; lost process records require explicit remote.process_terminate resolution"
                    .to_string(),
            );
            state.processes.clear();
        }
        Ok(())
    }

    async fn write_live_state(
        &self,
        recorder: &dyn WorkerEnvironmentRecorder,
        formation: &WorkerEnvironmentFormation,
        config: &RemoteExecConfig,
        calls: u64,
        status: &str,
        detail: Option<String>,
    ) -> Result<(), String> {
        let entry = self
            .rooms
            .lock()
            .await
            .get(&formation.worker_environment_id)
            .cloned();
        let (process_count, session_id, unavailable, lost_process_count) =
            if let Some(entry) = entry {
                let state = entry.state.lock().await;
                let lost_process_count = state
                    .store
                    .load_processes()?
                    .iter()
                    .filter(|record| record.status == DurableProcessStatus::Lost)
                    .count();
                (
                    state.processes.len(),
                    state.session_id.clone(),
                    state.unavailable.clone(),
                    lost_process_count,
                )
            } else {
                (0, None, None, 0)
            };
        let body = json!({
            "schema_version": 1,
            "worker_environment_id": formation.worker_environment_id,
            "worker_environment_kind": formation.worker_environment_kind,
            "substrate_family": SUBSTRATE_FAMILY,
            "status": status,
            "calls": calls,
            "process_count": process_count,
            "lost_process_count": lost_process_count,
            "remote_session_id": session_id,
            "remote_session_unavailable": unavailable,
            "workspace": config.workspace()?.to_string(),
            "viewer": {
                "protocol": "remote_exec_server",
                "address": config.websocket_url,
            },
            "detail": detail,
        });
        recorder.record_live_state(formation, &body).await
    }

    async fn dispatch(
        &self,
        remote: Option<&Environment>,
        store: &RecoveryStore,
        formation: &WorkerEnvironmentFormation,
        config: &RemoteExecConfig,
        source_movement_seq: u64,
        unavailable: Option<&str>,
        tool: &str,
        arguments: &Value,
    ) -> Result<Value, String> {
        match tool {
            "process_start" => {
                let remote = require_remote(remote, unavailable)?;
                self.process_start(
                    remote,
                    store,
                    formation,
                    config,
                    source_movement_seq,
                    arguments,
                )
                .await
            }
            "process_read" => self.process_read(store, formation, config, arguments).await,
            "process_write" => {
                self.process_write(store, formation, config, arguments)
                    .await
            }
            "process_interrupt" => self.process_interrupt(store, formation, arguments).await,
            "process_terminate" => {
                self.process_terminate(remote, store, formation, config, arguments)
                    .await
            }
            "process_list" => self.process_list(store, arguments).await,
            _ => {
                let remote = require_remote(remote, unavailable)?;
                dispatch(
                    remote,
                    store,
                    formation,
                    config,
                    source_movement_seq,
                    tool,
                    arguments,
                )
                .await
            }
        }
    }

    async fn process_start(
        &self,
        remote: &Environment,
        store: &RecoveryStore,
        formation: &WorkerEnvironmentFormation,
        config: &RemoteExecConfig,
        generation: u64,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object(
            "remote.process_start",
            arguments,
            &["process_id", "argv", "cwd", "env", "tty", "pipe_stdin"],
        )?;
        let handle = valid_process_handle(required_string(object, "process_id")?)?;
        let argv = required_string_array(object, "argv")?;
        if argv.is_empty() {
            return Err("remote.process_start argv must not be empty".to_string());
        }
        let cwd = resolve_cwd(remote, config, optional_string(object, "cwd")?).await?;
        let env = parse_env_overrides(config, object.get("env"))?;
        let tty = optional_bool(object, "tty")?.unwrap_or(false);
        let pipe_stdin = optional_bool(object, "pipe_stdin")?.unwrap_or(!tty);
        let started_unix_ms = unix_ms()?;
        let process_id = ProcessId::new(format!(
            "{}-{handle}-{generation}",
            formation.worker_environment_id.as_str(),
        ));
        let room_entry = self.room_entry(formation).await?;
        let durable_process = {
            let state = room_entry.state.lock().await;
            if state.processes.contains_key(handle) {
                return Err(format!("remote process '{handle}' already exists"));
            }
            store.begin_process(
                handle,
                process_id.as_str(),
                generation,
                config.max_processes,
                argv.clone(),
                cwd.to_string(),
                tty,
                pipe_stdin,
                started_unix_ms,
            )?
        };
        let started = tokio::time::timeout(
            config.timeout(),
            remote.get_exec_backend().start(ExecParams {
                process_id: process_id.clone(),
                argv: argv.clone(),
                cwd: cwd.clone(),
                env_policy: Some(exec_env_policy(config)),
                env,
                tty,
                pipe_stdin,
                arg0: None,
            }),
        )
        .await
        .map_err(|_| "remote.process_start timed out".to_string())
        .and_then(|result| result.map_err(|error| format!("remote.process_start failed: {error}")));
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                store.mark_process_lost(
                    handle,
                    &durable_process.session_id,
                    &durable_process.remote_process_id,
                    durable_process.generation,
                    "remote_process_start_outcome_unknown",
                    &error,
                    unix_ms()?,
                )?;
                return Err(format!(
                    "{error}; durable process state is lost and requires explicit remote.process_terminate resolution before reuse"
                ));
            }
        };
        let process_entry = RemoteProcessState {
            process: Arc::clone(&started.process),
            session_id: durable_process.session_id.clone(),
            remote_process_id: durable_process.remote_process_id.clone(),
            generation: durable_process.generation,
        };
        let inserted = {
            let mut state = room_entry.state.lock().await;
            match state.processes.get(handle) {
                Some(_) => Err(format!("remote process '{handle}' lost its formation race")),
                None => {
                    state.processes.insert(handle.to_string(), process_entry);
                    Ok(())
                }
            }
        };
        if let Err(error) = inserted {
            let cleanup = match started.process.terminate().await {
                Ok(_) => store
                    .mark_process_terminal(
                        handle,
                        &durable_process.session_id,
                        &durable_process.remote_process_id,
                        durable_process.generation,
                        None,
                        unix_ms()?,
                    )
                    .map(|_| "the raced remote process was terminated and retired".to_string())
                    .unwrap_or_else(|evidence_error| {
                        format!(
                            "the raced remote process was terminated but terminal evidence failed: {evidence_error}"
                        )
                    }),
                Err(termination_error) => store
                    .mark_process_lost(
                        handle,
                        &durable_process.session_id,
                        &durable_process.remote_process_id,
                        durable_process.generation,
                        "remote_process_start_formation_race",
                        &termination_error.to_string(),
                        unix_ms()?,
                    )
                    .map(|_| {
                        "the raced remote process could not be terminated and is durably lost"
                            .to_string()
                    })
                    .unwrap_or_else(|evidence_error| {
                        format!(
                            "the raced remote process could not be terminated and loss evidence failed: {evidence_error}"
                        )
                    }),
            };
            return Err(format!("{error}; {cleanup}"));
        }
        if let Err(error) = store.mark_process_active(
            handle,
            &durable_process.session_id,
            &durable_process.remote_process_id,
            durable_process.generation,
            unix_ms()?,
        ) {
            return Err(format!(
                "remote process started but its durable active record failed: {error}"
            ));
        }
        Ok(json!({
            "process_id": handle,
            "remote_process_id": process_id,
            "generation": generation,
            "argv": argv,
            "cwd": cwd,
            "tty": tty,
            "pipe_stdin": pipe_stdin,
            "started_unix_ms": started_unix_ms,
        }))
    }

    async fn process_read(
        &self,
        store: &RecoveryStore,
        formation: &WorkerEnvironmentFormation,
        config: &RemoteExecConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object(
            "remote.process_read",
            arguments,
            &["process_id", "after_seq", "max_bytes", "wait_ms"],
        )?;
        let handle = valid_process_handle(required_string(object, "process_id")?)?;
        let process = self.process(store, formation, handle).await?;
        let after_seq = optional_u64(object, "after_seq")?;
        let max_bytes = optional_usize(object, "max_bytes")?
            .unwrap_or(READ_CHUNK_BYTES)
            .min(config.max_output_bytes);
        if max_bytes == 0 {
            return Err("remote.process_read max_bytes must be positive".to_string());
        }
        let wait_ms = optional_u64(object, "wait_ms")?.unwrap_or(0);
        if wait_ms > 30_000 {
            return Err("remote.process_read wait_ms must be in 0..=30000".to_string());
        }
        let response = tokio::time::timeout(
            config.timeout(),
            process
                .process
                .read(after_seq, Some(max_bytes), Some(wait_ms)),
        )
        .await
        .map_err(|_| "remote.process_read timed out".to_string())
        .and_then(|result| result.map_err(|error| format!("remote.process_read failed: {error}")));
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                store.mark_process_lost(
                    handle,
                    &process.session_id,
                    &process.remote_process_id,
                    process.generation,
                    "remote_process_unreachable",
                    &error,
                    unix_ms()?,
                )?;
                self.remove_process(formation, handle, &process).await;
                return Err(format!(
                    "{error}; durable process state is lost and requires explicit remote.process_terminate resolution"
                ));
            }
        };
        let exited = response.exited;
        let closed = response.closed;
        let next_after_seq = response.next_seq.checked_sub(1).or(after_seq);
        let released = if closed {
            store.mark_process_terminal(
                handle,
                &process.session_id,
                &process.remote_process_id,
                process.generation,
                response.exit_code,
                unix_ms()?,
            )?;
            self.release_process_if(formation, handle, &process).await
        } else {
            false
        };
        let chunks = response
            .chunks
            .into_iter()
            .map(|chunk| {
                let utf8 = std::str::from_utf8(&chunk.chunk.0).ok();
                json!({
                    "seq": chunk.seq,
                    "stream": chunk.stream,
                    "data_base64": BASE64.encode(&chunk.chunk.0),
                    "utf8": utf8,
                    "bytes": chunk.chunk.0.len(),
                    "blake3": blake3::hash(&chunk.chunk.0).to_hex().to_string(),
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "process_id": handle,
            "chunks": chunks,
            "next_seq": response.next_seq,
            "next_after_seq": next_after_seq,
            "exited": exited,
            "exit_code": response.exit_code,
            "closed": closed,
            "failure": response.failure,
            "state": if closed { "released" } else if exited { "exited" } else { "running" },
            "released": released,
        }))
    }

    async fn process_write(
        &self,
        store: &RecoveryStore,
        formation: &WorkerEnvironmentFormation,
        config: &RemoteExecConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object(
            "remote.process_write",
            arguments,
            &["process_id", "data", "data_base64"],
        )?;
        let handle = valid_process_handle(required_string(object, "process_id")?)?;
        let data = decode_bytes(object, "data", "data_base64", config.max_stdin_bytes)?;
        let process = self.process(store, formation, handle).await?;
        let response = tokio::time::timeout(config.timeout(), process.process.write(data.clone()))
            .await
            .map_err(|_| "remote.process_write timed out".to_string())
            .and_then(|result| {
                result.map_err(|error| format!("remote.process_write failed: {error}"))
            });
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                store.mark_process_lost(
                    handle,
                    &process.session_id,
                    &process.remote_process_id,
                    process.generation,
                    "remote_process_unreachable",
                    &error,
                    unix_ms()?,
                )?;
                self.remove_process(formation, handle, &process).await;
                return Err(format!(
                    "{error}; durable process state is lost and requires explicit remote.process_terminate resolution"
                ));
            }
        };
        if response.status != WriteStatus::Accepted {
            return Err(format!(
                "remote.process_write was not accepted: {:?}",
                response.status
            ));
        }
        Ok(json!({
            "process_id": handle,
            "bytes": data.len(),
            "status": response.status,
        }))
    }

    async fn process_interrupt(
        &self,
        store: &RecoveryStore,
        formation: &WorkerEnvironmentFormation,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object("remote.process_interrupt", arguments, &["process_id"])?;
        let handle = valid_process_handle(required_string(object, "process_id")?)?;
        let process = self.process(store, formation, handle).await?;
        let response = match process.process.signal(ProcessSignal::Interrupt).await {
            Ok(response) => response,
            Err(error) => {
                let error = format!("remote.process_interrupt failed: {error}");
                store.mark_process_lost(
                    handle,
                    &process.session_id,
                    &process.remote_process_id,
                    process.generation,
                    "remote_process_unreachable",
                    &error,
                    unix_ms()?,
                )?;
                self.remove_process(formation, handle, &process).await;
                return Err(format!(
                    "{error}; durable process state is lost and requires explicit remote.process_terminate resolution"
                ));
            }
        };
        if !response.delivered {
            return Err(
                "remote.process_interrupt was not delivered to a running process".to_string(),
            );
        }
        Ok(json!({"process_id": handle, "interrupted": true}))
    }

    async fn process_terminate(
        &self,
        remote: Option<&Environment>,
        store: &RecoveryStore,
        formation: &WorkerEnvironmentFormation,
        config: &RemoteExecConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object("remote.process_terminate", arguments, &["process_id"])?;
        let handle = valid_process_handle(required_string(object, "process_id")?)?;
        let record = store
            .load_process(handle)?
            .ok_or_else(|| format!("remote process '{handle}' has no durable record"))?;
        let resident = match self
            .rooms
            .lock()
            .await
            .get(&formation.worker_environment_id)
            .cloned()
        {
            Some(entry) => entry.state.lock().await.processes.get(handle).cloned(),
            None => None,
        };
        if record.status == DurableProcessStatus::Lost {
            let (was_running, resolution) = match remote {
                Some(remote) => {
                    let was_running = terminate_exact_remote_process(
                        remote,
                        &record.remote_process_id,
                        config.timeout(),
                    )
                    .await?;
                    (
                        Some(was_running),
                        if was_running {
                            "remote_terminated"
                        } else {
                            "remote_absent"
                        },
                    )
                }
                None => (None, "acknowledged_unavailable_session"),
            };
            let resolved = store.resolve_lost_process(
                handle,
                &record.session_id,
                &record.remote_process_id,
                record.generation,
                resolution,
                unix_ms()?,
            )?;
            return Ok(json!({
                "process_id": handle,
                "remote_process_id": resolved.remote_process_id,
                "state": "resolved_lost",
                "was_running": was_running,
                "terminated": was_running == Some(true),
                "released": true,
                "resolution": resolution,
                "loss": resolved.loss,
            }));
        }
        let Some(process) = resident else {
            let remote = remote.ok_or_else(|| {
                format!("remote process '{handle}' is durable but its exact session is unavailable")
            })?;
            let was_running =
                terminate_exact_remote_process(remote, &record.remote_process_id, config.timeout())
                    .await?;
            store.mark_process_terminal(
                handle,
                &record.session_id,
                &record.remote_process_id,
                record.generation,
                None,
                unix_ms()?,
            )?;
            return Ok(json!({
                "process_id": handle,
                "remote_process_id": record.remote_process_id,
                "state": if was_running { "terminated" } else { "released" },
                "was_running": was_running,
                "terminated": was_running,
                "released": true,
                "resolution": if was_running { "remote_terminated" } else { "remote_absent" },
            }));
        };
        let remote_process_id = process.process.process_id().clone();
        let response = match tokio::time::timeout(config.timeout(), process.process.terminate())
            .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                let error = format!("remote.process_terminate failed: {error}");
                store.mark_process_lost(
                    handle,
                    &process.session_id,
                    &process.remote_process_id,
                    process.generation,
                    "remote_process_unreachable",
                    &error,
                    unix_ms()?,
                )?;
                self.remove_process(formation, handle, &process).await;
                return Err(format!(
                    "{error}; durable process state is lost and requires explicit remote.process_terminate resolution"
                ));
            }
            Err(_) => {
                let error = format!(
                    "remote.process_terminate timed out after {} ms",
                    config.timeout().as_millis()
                );
                store.mark_process_lost(
                    handle,
                    &process.session_id,
                    &process.remote_process_id,
                    process.generation,
                    "remote_process_termination_timeout",
                    &error,
                    unix_ms()?,
                )?;
                self.remove_process(formation, handle, &process).await;
                return Err(format!(
                    "{error}; durable process state is lost and requires explicit remote.process_terminate resolution"
                ));
            }
        };
        store.mark_process_terminal(
            handle,
            &process.session_id,
            &process.remote_process_id,
            process.generation,
            None,
            unix_ms()?,
        )?;
        let released = self.release_process_if(formation, handle, &process).await;
        Ok(json!({
            "process_id": handle,
            "remote_process_id": remote_process_id,
            "state": if response.running { "terminated" } else { "released" },
            "was_running": response.running,
            "terminated": response.running,
            "released": released,
        }))
    }

    async fn process_list(
        &self,
        store: &RecoveryStore,
        arguments: &Value,
    ) -> Result<Value, String> {
        strict_object("remote.process_list", arguments, &[])?;
        Ok(json!({
            "processes": store.load_processes()?.into_iter().map(|process| json!({
                "process_id": process.process_id,
                "remote_process_id": process.remote_process_id,
                "generation": process.generation,
                "argv": process.argv,
                "cwd": process.cwd,
                "tty": process.tty,
                "pipe_stdin": process.pipe_stdin,
                "started_unix_ms": process.started_unix_ms,
                "state": process.status,
                "loss": process.loss,
            })).collect::<Vec<_>>()
        }))
    }

    async fn process(
        &self,
        store: &RecoveryStore,
        formation: &WorkerEnvironmentFormation,
        handle: &str,
    ) -> Result<RemoteProcessState, String> {
        if let Some(record) = store.load_process(handle)? {
            if record.status == DurableProcessStatus::Lost {
                return Err(format!(
                    "remote process '{handle}' is durably lost and requires explicit remote.process_terminate resolution"
                ));
            }
        }
        let entry = self.room_entry(formation).await?;
        let state = entry.state.lock().await;
        let process = state
            .processes
            .get(handle)
            .cloned()
            .ok_or_else(|| format!("remote process '{handle}' is not active"));
        process
    }

    async fn release_process_if(
        &self,
        formation: &WorkerEnvironmentFormation,
        handle: &str,
        process: &RemoteProcessState,
    ) -> bool {
        let Some(entry) = self
            .rooms
            .lock()
            .await
            .get(&formation.worker_environment_id)
            .cloned()
        else {
            return true;
        };
        let mut state = entry.state.lock().await;
        match state.processes.get(handle) {
            Some(current)
                if Arc::ptr_eq(&current.process, &process.process)
                    && current.session_id == process.session_id
                    && current.remote_process_id == process.remote_process_id
                    && current.generation == process.generation =>
            {
                state.processes.remove(handle);
                true
            }
            None => true,
            Some(_) => false,
        }
    }

    async fn remove_process(
        &self,
        formation: &WorkerEnvironmentFormation,
        handle: &str,
        process: &RemoteProcessState,
    ) {
        let _ = self.release_process_if(formation, handle, process).await;
    }

    async fn room_entry(
        &self,
        formation: &WorkerEnvironmentFormation,
    ) -> Result<Arc<RoomEntry>, String> {
        self.rooms
            .lock()
            .await
            .get(&formation.worker_environment_id)
            .cloned()
            .ok_or_else(|| "remote process room is not materialized".to_string())
    }

    async fn release_room(&self, formation: &WorkerEnvironmentFormation) -> Result<(), String> {
        let Some(entry) = self
            .rooms
            .lock()
            .await
            .get(&formation.worker_environment_id)
            .cloned()
        else {
            return Ok(());
        };
        let (environment, store, request_timeout, session_id, processes) = {
            let state = entry.state.lock().await;
            (
                state.environment.clone(),
                Arc::clone(&state.store),
                state.request_timeout,
                state.session_id.clone(),
                state
                    .processes
                    .iter()
                    .map(|(handle, state)| (handle.clone(), state.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        let mut terminated = Vec::new();
        let mut errors = Vec::new();
        for (handle, process) in processes {
            match process.process.terminate().await {
                Ok(_) => match store.mark_process_terminal(
                    &handle,
                    &process.session_id,
                    &process.remote_process_id,
                    process.generation,
                    None,
                    unix_ms()?,
                ) {
                    Ok(_) => terminated.push((handle, process)),
                    Err(error) => errors.push(format!(
                        "remote process '{handle}' terminated but terminal evidence failed: {error}"
                    )),
                },
                Err(error) => errors.push(format!(
                    "remote process '{handle}' could not be terminated during worker-environment release: {error}"
                )),
            }
        }
        {
            let mut state = entry.state.lock().await;
            for (handle, process) in terminated {
                if state.processes.get(&handle).is_some_and(|current| {
                    Arc::ptr_eq(&current.process, &process.process)
                        && current.session_id == process.session_id
                        && current.remote_process_id == process.remote_process_id
                        && current.generation == process.generation
                }) {
                    state.processes.remove(&handle);
                }
            }
        }
        if errors.is_empty() && store.has_unresolved_processes()? {
            errors.push(
                "remote worker environment has unresolved lost processes; resolve each with remote.process_terminate before release"
                    .to_string(),
            );
        }
        if errors.is_empty() {
            let session_id = session_id.ok_or_else(|| {
                "remote worker environment has no session identity to release".to_string()
            })?;
            let environment = environment.ok_or_else(|| {
                "remote worker environment has no exact server session to release".to_string()
            })?;
            let released_session_id = tokio::time::timeout(
                request_timeout,
                environment.release_remote_session(&session_id),
            )
            .await
            .map_err(|_| "remote exec session release timed out".to_string())?
            .map_err(|error| format!("remote exec session release failed: {error}"))?;
            if released_session_id != session_id {
                return Err(
                    "remote exec server released an unexpected session identity".to_string()
                );
            }
            store.release_session(&session_id, unix_ms()?)?;
            let mut rooms = self.rooms.lock().await;
            if rooms
                .get(&formation.worker_environment_id)
                .is_some_and(|current| Arc::ptr_eq(current, &entry))
            {
                rooms.remove(&formation.worker_environment_id);
            }
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    async fn release_cold_room(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Result<(), String> {
        let config = RemoteExecConfig::load_formed(&self.environment, formation, kind)?;
        let durable_root = self.environment.durable_var_dir.as_ref().ok_or_else(|| {
            "remote exec room requires a formation-private durable state directory".to_string()
        })?;
        let auth = config.load_auth(&self.environment)?;
        let identity = RemoteRoomIdentity::new(
            formation,
            config.websocket_url.clone(),
            config.workspace()?.to_string(),
            blake3::Hash::from_bytes(auth.identity())
                .to_hex()
                .to_string(),
            config.identity()?,
        );
        let store = RecoveryStore::open(durable_root, identity)?;
        let Some(session) = store.load_session()? else {
            if store.has_unresolved_processes()? {
                return Err(
                    "remote exec durable process state exists without a releasable session"
                        .to_string(),
                );
            }
            return Ok(());
        };
        match session.status {
            DurableSessionStatus::Released => {
                if store.has_unresolved_processes()? {
                    return Err(
                        "released remote exec session retains unresolved process state".to_string(),
                    );
                }
                return Ok(());
            }
            DurableSessionStatus::Lost => {
                return Err(
                    "lost remote exec session cannot be proven released without reconnecting to its exact server session"
                        .to_string(),
                );
            }
            DurableSessionStatus::Claiming | DurableSessionStatus::Active => {}
        }
        let remote = Arc::new(
            Environment::remote_authenticated_claim(
                config.websocket_url.clone(),
                auth,
                session.session_id.clone(),
            )
            .map_err(|error| format!("remote exec release claim refused: {error}"))?,
        );
        remote.start_connecting();
        ensure_ready(&remote, &config).await?;
        let confirmed = remote
            .remote_session_id()
            .await
            .map_err(|error| format!("remote exec release session identity failed: {error}"))?;
        if confirmed != session.session_id {
            return Err(
                "remote exec release claimed an unexpected server session identity".to_string(),
            );
        }
        if session.status == DurableSessionStatus::Claiming {
            store.activate_session(confirmed.clone(), unix_ms()?)?;
        }
        let processes = self
            .attach_durable_processes(&remote, &config, &store)
            .await?;
        let mut errors = Vec::new();
        for (handle, process) in processes {
            match process.process.terminate().await {
                Ok(_) => {
                    if let Err(error) = store.mark_process_terminal(
                        &handle,
                        &process.session_id,
                        &process.remote_process_id,
                        process.generation,
                        None,
                        unix_ms()?,
                    ) {
                        errors.push(format!(
                            "remote process '{handle}' terminated but terminal evidence failed: {error}"
                        ));
                    }
                }
                Err(error) => errors.push(format!(
                    "remote process '{handle}' could not be terminated during cold worker-environment release: {error}"
                )),
            }
        }
        if store.has_unresolved_processes()? {
            errors.push(
                "remote worker environment retains unresolved processes after cold release reconciliation"
                    .to_string(),
            );
        }
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        let released_session_id =
            tokio::time::timeout(config.timeout(), remote.release_remote_session(&confirmed))
                .await
                .map_err(|_| "remote exec cold session release timed out".to_string())?
                .map_err(|error| format!("remote exec cold session release failed: {error}"))?;
        if released_session_id != confirmed {
            return Err("remote exec server released an unexpected cold session".to_string());
        }
        store.release_session(&confirmed, unix_ms()?)
    }
}

impl ToolProcessHandler for RemoteExecRoom {
    fn substrate_family(&self) -> &str {
        SUBSTRATE_FAMILY
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

    fn release_with_context<'a>(
        &'a self,
        context: WorkerEnvironmentReleaseContext<'a>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            match context.mode {
                WorkerEnvironmentReleaseMode::LiveOwner => {
                    self.release_room(context.formation).await
                }
                WorkerEnvironmentReleaseMode::ColdRecovery => {
                    self.release_cold_room(context.formation, context.kind)
                        .await
                }
            }
            .map_err(ToolProcessError::Unavailable)
        })
    }
}

pub async fn run_remote_exec_room_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    let room = Arc::new(RemoteExecRoom::new(environment));
    octopus_tool_door_runtime::run_tool_process(room).await
}

async fn dispatch(
    remote: &Environment,
    store: &RecoveryStore,
    formation: &WorkerEnvironmentFormation,
    config: &RemoteExecConfig,
    generation: u64,
    tool: &str,
    arguments: &Value,
) -> Result<Value, String> {
    match tool {
        "status" => remote_status(remote, formation, config).await,
        "exec" => remote_exec(remote, store, formation, config, generation, arguments).await,
        "file_read" => remote_file_read(remote, config, arguments).await,
        "file_write" => remote_file_write(remote, config, arguments).await,
        "file_list" => remote_file_list(remote, config, arguments).await,
        "file_stat" => remote_file_stat(remote, config, arguments).await,
        "file_delete" => remote_file_delete(remote, config, arguments).await,
        "file_copy" => remote_file_copy(remote, config, arguments).await,
        "directory_create" => remote_directory_create(remote, config, arguments).await,
        other => Err(format!(
            "remote exec room does not host operation '{other}'"
        )),
    }
}

fn require_remote<'a>(
    remote: Option<&'a Environment>,
    unavailable: Option<&str>,
) -> Result<&'a Environment, String> {
    remote.ok_or_else(|| {
        unavailable
            .unwrap_or("remote exec server session is unavailable")
            .to_string()
    })
}

async fn ensure_ready(remote: &Environment, config: &RemoteExecConfig) -> Result<(), String> {
    tokio::time::timeout(config.timeout(), remote.wait_until_ready())
        .await
        .map_err(|_| {
            format!(
                "remote exec connection timed out after {} ms",
                config.request_timeout_ms
            )
        })?
        .map_err(|error| format!("remote exec connection failed: {error}"))
}

async fn remote_status(
    remote: &Environment,
    formation: &WorkerEnvironmentFormation,
    config: &RemoteExecConfig,
) -> Result<Value, String> {
    let info = tokio::time::timeout(config.timeout(), remote.info())
        .await
        .map_err(|_| "remote exec environment info timed out".to_string())?
        .map_err(|error| format!("remote exec environment info failed: {error}"))?;
    Ok(json!({
        "worker_environment_id": formation.worker_environment_id,
        "connected": true,
        "workspace": config.workspace()?.to_string(),
        "shell": info.shell,
    }))
}

async fn remote_exec(
    remote: &Environment,
    store: &RecoveryStore,
    formation: &WorkerEnvironmentFormation,
    config: &RemoteExecConfig,
    generation: u64,
    arguments: &Value,
) -> Result<Value, String> {
    let object = strict_object(
        "remote.exec",
        arguments,
        &["argv", "cwd", "env", "tty", "timeout_ms"],
    )?;
    let argv = required_string_array(object, "argv")?;
    if argv.is_empty() {
        return Err("remote.exec argv must not be empty".to_string());
    }
    let logical_process_id = format!("once-{generation}");
    let process_id = ProcessId::new(format!(
        "{}-{generation}",
        formation.worker_environment_id.as_str()
    ));
    let cwd = resolve_cwd(remote, config, optional_string(object, "cwd")?).await?;
    let env = parse_env_overrides(config, object.get("env"))?;
    let tty = optional_bool(object, "tty")?.unwrap_or(false);
    let timeout = config.bounded_timeout(optional_u64(object, "timeout_ms")?)?;
    let started_unix_ms = unix_ms()?;
    let durable_process = store.begin_process(
        &logical_process_id,
        process_id.as_str(),
        generation,
        config.max_processes,
        argv.clone(),
        cwd.to_string(),
        tty,
        false,
        started_unix_ms,
    )?;
    let start_result = tokio::time::timeout(
        timeout,
        remote.get_exec_backend().start(ExecParams {
            process_id: process_id.clone(),
            argv: argv.clone(),
            cwd: cwd.clone(),
            env_policy: Some(exec_env_policy(config)),
            env,
            tty,
            pipe_stdin: false,
            arg0: None,
        }),
    )
    .await;
    let started = match start_result {
        Ok(Ok(started)) => started,
        Ok(Err(error)) => {
            let failure = format!("remote.exec process start failed: {error}");
            let resolution = resolve_unknown_remote_process(
                remote,
                store,
                &durable_process,
                config.timeout(),
                "remote_exec_start_outcome_unknown",
                &failure,
            )
            .await;
            return Err(format!("{failure}; {resolution}"));
        }
        Err(_) => {
            let failure = "remote.exec process start timed out".to_string();
            let resolution = resolve_unknown_remote_process(
                remote,
                store,
                &durable_process,
                config.timeout(),
                "remote_exec_start_outcome_unknown",
                &failure,
            )
            .await;
            return Err(format!("{failure}; {resolution}"));
        }
    };
    if let Err(evidence_error) = store.mark_process_active(
        &durable_process.process_id,
        &durable_process.session_id,
        &durable_process.remote_process_id,
        durable_process.generation,
        unix_ms()?,
    ) {
        let failure =
            format!("remote.exec process started but active evidence failed: {evidence_error}");
        let resolution = retire_remote_process_after_failure(
            store,
            &durable_process,
            &started.process,
            config.timeout(),
            "remote_exec_active_evidence_failed",
            &failure,
        )
        .await;
        return Err(format!("{failure}; {resolution}"));
    }
    collect_process(
        store,
        &durable_process,
        started.process,
        process_id,
        argv,
        cwd,
        timeout,
        config,
    )
    .await
}

async fn collect_process(
    store: &RecoveryStore,
    durable_process: &DurableProcessRecord,
    process: Arc<dyn ExecProcess>,
    process_id: ProcessId,
    argv: Vec<String>,
    cwd: PathUri,
    timeout: Duration,
    config: &RemoteExecConfig,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut after_seq = None;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut pty = Vec::new();
    let mut exit_code = None;
    loop {
        let Some(captured) = stdout
            .len()
            .checked_add(stderr.len())
            .and_then(|size| size.checked_add(pty.len()))
        else {
            let failure = "remote.exec output accounting overflowed";
            let resolution = retire_remote_process_after_failure(
                store,
                durable_process,
                &process,
                config.timeout(),
                "remote_exec_output_accounting_failed",
                failure,
            )
            .await;
            return Err(format!("{failure}; {resolution}"));
        };
        let Some(remaining) = config.max_output_bytes.checked_sub(captured) else {
            let failure = "remote.exec output accounting exceeded its configured ceiling";
            let resolution = retire_remote_process_after_failure(
                store,
                durable_process,
                &process,
                config.timeout(),
                "remote_exec_output_accounting_failed",
                failure,
            )
            .await;
            return Err(format!("{failure}; {resolution}"));
        };
        if remaining == 0 {
            let failure = format!(
                "remote.exec output exceeded {} bytes",
                config.max_output_bytes
            );
            let resolution = retire_remote_process_after_failure(
                store,
                durable_process,
                &process,
                config.timeout(),
                "remote_exec_output_ceiling_exceeded",
                &failure,
            )
            .await;
            return Err(format!("{failure}; {resolution}"));
        }
        let response = match tokio::time::timeout_at(
            deadline,
            process.read(after_seq, Some(remaining.min(READ_CHUNK_BYTES)), Some(250)),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                let failure = format!("remote.exec output read failed: {error}");
                let resolution = retire_remote_process_after_failure(
                    store,
                    durable_process,
                    &process,
                    config.timeout(),
                    "remote_exec_output_read_failed",
                    &failure,
                )
                .await;
                return Err(format!("{failure}; {resolution}"));
            }
            Err(_) => {
                let failure = format!("remote.exec timed out after {} ms", timeout.as_millis());
                let resolution = retire_remote_process_after_failure(
                    store,
                    durable_process,
                    &process,
                    config.timeout(),
                    "remote_exec_timed_out",
                    &failure,
                )
                .await;
                return Err(format!("{failure}; {resolution}"));
            }
        };
        for chunk in response.chunks {
            let Some(captured) = stdout
                .len()
                .checked_add(stderr.len())
                .and_then(|size| size.checked_add(pty.len()))
                .and_then(|size| size.checked_add(chunk.chunk.0.len()))
            else {
                let failure = "remote.exec output accounting overflowed";
                let resolution = retire_remote_process_after_failure(
                    store,
                    durable_process,
                    &process,
                    config.timeout(),
                    "remote_exec_output_accounting_failed",
                    failure,
                )
                .await;
                return Err(format!("{failure}; {resolution}"));
            };
            if captured > config.max_output_bytes {
                let failure = format!(
                    "remote.exec output exceeded {} bytes",
                    config.max_output_bytes
                );
                let resolution = retire_remote_process_after_failure(
                    store,
                    durable_process,
                    &process,
                    config.timeout(),
                    "remote_exec_output_ceiling_exceeded",
                    &failure,
                )
                .await;
                return Err(format!("{failure}; {resolution}"));
            }
            let target = match chunk.stream {
                ExecOutputStream::Stdout => &mut stdout,
                ExecOutputStream::Stderr => &mut stderr,
                ExecOutputStream::Pty => &mut pty,
            };
            target.extend_from_slice(&chunk.chunk.0);
        }
        after_seq = response.next_seq.checked_sub(1).or(after_seq);
        if let Some(failure) = response.failure {
            let failure = format!("remote.exec process failed: {failure}");
            let resolution = retire_remote_process_after_failure(
                store,
                durable_process,
                &process,
                config.timeout(),
                "remote_exec_process_failed",
                &failure,
            )
            .await;
            return Err(format!("{failure}; {resolution}"));
        }
        if response.exit_code.is_some() {
            exit_code = response.exit_code;
        }
        if response.closed {
            break;
        }
    }
    let Some(exit_code) = exit_code else {
        let failure = "remote.exec process closed without a confirmed exit status";
        let terminal = store.mark_process_terminal(
            &durable_process.process_id,
            &durable_process.session_id,
            &durable_process.remote_process_id,
            durable_process.generation,
            None,
            unix_ms()?,
        );
        return match terminal {
            Ok(_) => Err(failure.to_string()),
            Err(evidence_error) => Err(format!(
                "{failure}; terminal evidence failed: {evidence_error}"
            )),
        };
    };
    let Some(captured_bytes) = stdout
        .len()
        .checked_add(stderr.len())
        .and_then(|size| size.checked_add(pty.len()))
    else {
        let failure = "remote.exec output accounting overflowed after process closure";
        let terminal = store.mark_process_terminal(
            &durable_process.process_id,
            &durable_process.session_id,
            &durable_process.remote_process_id,
            durable_process.generation,
            Some(exit_code),
            unix_ms()?,
        );
        return match terminal {
            Ok(_) => Err(failure.to_string()),
            Err(evidence_error) => Err(format!(
                "{failure}; terminal evidence failed: {evidence_error}"
            )),
        };
    };
    let body = json!({
        "process_id": process_id,
        "argv": argv,
        "cwd": cwd,
        "exit_code": exit_code,
        "stdout": std::str::from_utf8(&stdout).ok(),
        "stdout_base64": BASE64.encode(&stdout),
        "stdout_blake3": blake3::hash(&stdout).to_hex().to_string(),
        "stderr": std::str::from_utf8(&stderr).ok(),
        "stderr_base64": BASE64.encode(&stderr),
        "stderr_blake3": blake3::hash(&stderr).to_hex().to_string(),
        "pty": std::str::from_utf8(&pty).ok(),
        "pty_base64": BASE64.encode(&pty),
        "pty_blake3": blake3::hash(&pty).to_hex().to_string(),
        "captured_bytes": captured_bytes,
    });
    store.mark_process_terminal(
        &durable_process.process_id,
        &durable_process.session_id,
        &durable_process.remote_process_id,
        durable_process.generation,
        Some(exit_code),
        unix_ms()?,
    )?;
    Ok(body)
}

async fn resolve_unknown_remote_process(
    remote: &Environment,
    store: &RecoveryStore,
    durable_process: &DurableProcessRecord,
    timeout: Duration,
    code: &str,
    detail: &str,
) -> String {
    let process_id = ProcessId::new(durable_process.remote_process_id.clone());
    let attached = remote.attach_exec_process(&process_id).await;
    match attached {
        Ok(started) => {
            retire_remote_process_after_failure(
                store,
                durable_process,
                &started.process,
                timeout,
                code,
                detail,
            )
            .await
        }
        Err(attach_error) => record_lost_process(
            store,
            durable_process,
            code,
            &format!("{detail}; exact process attachment failed: {attach_error}"),
        ),
    }
}

async fn retire_remote_process_after_failure(
    store: &RecoveryStore,
    durable_process: &DurableProcessRecord,
    process: &Arc<dyn ExecProcess>,
    timeout: Duration,
    code: &str,
    detail: &str,
) -> String {
    match tokio::time::timeout(timeout, process.terminate()).await {
        Ok(Ok(_)) => match unix_ms().and_then(|now| {
            store
                .mark_process_terminal(
                    &durable_process.process_id,
                    &durable_process.session_id,
                    &durable_process.remote_process_id,
                    durable_process.generation,
                    None,
                    now,
                )
                .map(|_| ())
        }) {
            Ok(()) => "the exact remote process was terminated or already absent and its durable record was retired".to_string(),
            Err(evidence_error) => format!(
                "the exact remote process was terminated or already absent but terminal evidence failed: {evidence_error}"
            ),
        },
        Ok(Err(termination_error)) => record_lost_process(
            store,
            durable_process,
            code,
            &format!("{detail}; exact process termination failed: {termination_error}"),
        ),
        Err(_) => record_lost_process(
            store,
            durable_process,
            code,
            &format!(
                "{detail}; exact process termination timed out after {} ms",
                timeout.as_millis()
            ),
        ),
    }
}

fn record_lost_process(
    store: &RecoveryStore,
    durable_process: &DurableProcessRecord,
    code: &str,
    detail: &str,
) -> String {
    match unix_ms().and_then(|now| {
        store
            .mark_process_lost(
                &durable_process.process_id,
                &durable_process.session_id,
                &durable_process.remote_process_id,
                durable_process.generation,
                code,
                detail,
                now,
            )
            .map(|_| ())
    }) {
        Ok(()) => format!(
            "the exact remote outcome remains durably lost as process_id '{}' and requires remote.process_terminate resolution",
            durable_process.process_id
        ),
        Err(evidence_error) => format!(
            "the exact remote outcome is unresolved and loss evidence failed: {evidence_error}"
        ),
    }
}

async fn terminate_exact_remote_process(
    remote: &Environment,
    remote_process_id: &str,
    timeout: Duration,
) -> Result<bool, String> {
    let process_id = ProcessId::new(remote_process_id.to_string());
    let started = remote
        .attach_exec_process(&process_id)
        .await
        .map_err(|error| format!("exact remote process attachment failed: {error}"))?;
    tokio::time::timeout(timeout, started.process.terminate())
        .await
        .map_err(|_| {
            format!(
                "exact remote process termination timed out after {} ms",
                timeout.as_millis()
            )
        })?
        .map(|response| response.running)
        .map_err(|error| format!("exact remote process termination failed: {error}"))
}

async fn remote_file_read(
    remote: &Environment,
    config: &RemoteExecConfig,
    arguments: &Value,
) -> Result<Value, String> {
    let object = strict_object("remote.file_read", arguments, &["path", "encoding"])?;
    let raw = required_string(object, "path")?;
    let candidate = config.resolve_path(raw)?;
    let response = rooted_operation(
        remote,
        config,
        "remote.file_read",
        FsRootedOperation::ReadFile {
            path: candidate.clone(),
            max_bytes: config.max_file_bytes,
        },
    )
    .await?;
    let FsRootedResponse::ReadFile { data } = response else {
        return Err("remote.file_read received an unrelated rooted response".to_string());
    };
    let bytes = data.into_inner();
    let encoding = optional_string(object, "encoding")?.unwrap_or("utf8");
    let content = match encoding {
        "utf8" => Value::String(String::from_utf8(bytes.clone()).map_err(|_| {
            "remote.file_read content is not valid UTF-8; request base64".to_string()
        })?),
        "base64" => Value::String(BASE64.encode(&bytes)),
        _ => return Err("remote.file_read encoding must be utf8 or base64".to_string()),
    };
    Ok(json!({
        "path": candidate.to_string(),
        "encoding": encoding,
        "content": content,
        "bytes": bytes.len(),
        "blake3": blake3::hash(&bytes).to_hex().to_string(),
    }))
}

async fn remote_file_write(
    remote: &Environment,
    config: &RemoteExecConfig,
    arguments: &Value,
) -> Result<Value, String> {
    let object = strict_object(
        "remote.file_write",
        arguments,
        &["path", "content", "content_base64", "create_parents"],
    )?;
    let raw = required_string(object, "path")?;
    let content = decode_bytes(object, "content", "content_base64", config.max_file_bytes)?;
    let candidate = config.resolve_path(raw)?;
    let response = rooted_operation(
        remote,
        config,
        "remote.file_write",
        FsRootedOperation::WriteFile {
            path: candidate.clone(),
            data: ByteChunk::from(content.clone()),
            create_parents: optional_bool(object, "create_parents")?.unwrap_or(false),
        },
    )
    .await?;
    let FsRootedResponse::WriteFile { bytes_written } = response else {
        return Err("remote.file_write received an unrelated rooted response".to_string());
    };
    if bytes_written != content.len() {
        return Err(format!(
            "remote.file_write reported {bytes_written} bytes for a {} byte input",
            content.len()
        ));
    }
    Ok(json!({
        "path": candidate.to_string(),
        "bytes_written": bytes_written,
        "blake3": blake3::hash(&content).to_hex().to_string(),
    }))
}

async fn remote_file_list(
    remote: &Environment,
    config: &RemoteExecConfig,
    arguments: &Value,
) -> Result<Value, String> {
    let object = strict_object("remote.file_list", arguments, &["path"])?;
    let raw = optional_string(object, "path")?.unwrap_or(".");
    let candidate = config.resolve_path(raw)?;
    let response = rooted_operation(
        remote,
        config,
        "remote.file_list",
        FsRootedOperation::ReadDirectory {
            path: candidate.clone(),
            max_entries: config.max_list_entries,
        },
    )
    .await?;
    let FsRootedResponse::ReadDirectory { entries } = response else {
        return Err("remote.file_list received an unrelated rooted response".to_string());
    };
    Ok(json!({
        "path": candidate.to_string(),
        "entries": entries.into_iter().map(|entry| json!({
            "name": entry.file_name,
            "is_directory": entry.is_directory,
            "is_file": entry.is_file,
            "is_symlink": entry.is_symlink,
        })).collect::<Vec<_>>(),
    }))
}

async fn remote_file_stat(
    remote: &Environment,
    config: &RemoteExecConfig,
    arguments: &Value,
) -> Result<Value, String> {
    let object = strict_object("remote.file_stat", arguments, &["path"])?;
    let candidate = config.resolve_path(required_string(object, "path")?)?;
    let response = rooted_operation(
        remote,
        config,
        "remote.file_stat",
        FsRootedOperation::GetMetadata {
            path: candidate.clone(),
        },
    )
    .await?;
    let FsRootedResponse::GetMetadata { metadata } = response else {
        return Err("remote.file_stat received an unrelated rooted response".to_string());
    };
    Ok(json!({
        "path": candidate,
        "is_directory": metadata.is_directory,
        "is_file": metadata.is_file,
        "is_symlink": metadata.is_symlink,
        "size": metadata.size,
        "created_at_ms": metadata.created_at_ms,
        "modified_at_ms": metadata.modified_at_ms,
    }))
}

async fn remote_file_delete(
    remote: &Environment,
    config: &RemoteExecConfig,
    arguments: &Value,
) -> Result<Value, String> {
    let object = strict_object(
        "remote.file_delete",
        arguments,
        &["path", "recursive", "force"],
    )?;
    let candidate = config.resolve_path(required_string(object, "path")?)?;
    let recursive = optional_bool(object, "recursive")?.unwrap_or(false);
    let force = optional_bool(object, "force")?.unwrap_or(false);
    let response = rooted_operation(
        remote,
        config,
        "remote.file_delete",
        FsRootedOperation::Remove {
            path: candidate.clone(),
            recursive,
            force,
            max_entries: config.max_list_entries,
        },
    )
    .await?;
    if response != FsRootedResponse::Remove {
        return Err("remote.file_delete received an unrelated rooted response".to_string());
    }
    Ok(json!({"path": candidate, "recursive": recursive, "force": force}))
}

async fn remote_file_copy(
    remote: &Environment,
    config: &RemoteExecConfig,
    arguments: &Value,
) -> Result<Value, String> {
    let object = strict_object(
        "remote.file_copy",
        arguments,
        &["source", "destination", "recursive", "create_parents"],
    )?;
    let source_candidate = config.resolve_path(required_string(object, "source")?)?;
    let destination_candidate = config.resolve_path(required_string(object, "destination")?)?;
    let recursive = optional_bool(object, "recursive")?.unwrap_or(false);
    let response = rooted_operation(
        remote,
        config,
        "remote.file_copy",
        FsRootedOperation::Copy {
            source_path: source_candidate.clone(),
            destination_path: destination_candidate.clone(),
            recursive,
            create_parents: optional_bool(object, "create_parents")?.unwrap_or(false),
            max_entries: config.max_list_entries,
        },
    )
    .await?;
    if response != FsRootedResponse::Copy {
        return Err("remote.file_copy received an unrelated rooted response".to_string());
    }
    Ok(json!({
        "source": source_candidate,
        "destination": destination_candidate,
        "recursive": recursive,
    }))
}

async fn remote_directory_create(
    remote: &Environment,
    config: &RemoteExecConfig,
    arguments: &Value,
) -> Result<Value, String> {
    let object = strict_object("remote.directory_create", arguments, &["path", "recursive"])?;
    let candidate = config.resolve_path(required_string(object, "path")?)?;
    let recursive = optional_bool(object, "recursive")?.unwrap_or(true);
    let response = rooted_operation(
        remote,
        config,
        "remote.directory_create",
        FsRootedOperation::CreateDirectory {
            path: candidate.clone(),
            recursive,
        },
    )
    .await?;
    if response != FsRootedResponse::CreateDirectory {
        return Err("remote.directory_create received an unrelated rooted response".to_string());
    }
    Ok(json!({"path": candidate, "recursive": recursive}))
}

async fn rooted_operation(
    remote: &Environment,
    config: &RemoteExecConfig,
    tool: &str,
    operation: FsRootedOperation,
) -> Result<FsRootedResponse, String> {
    let params = FsRootedParams {
        root: config.workspace()?,
        operation,
    };
    tokio::time::timeout(config.timeout(), remote.rooted_file_system(params))
        .await
        .map_err(|_| format!("{tool} timed out"))?
        .map_err(|error| format!("{tool} failed: {error}"))
}

async fn canonical_existing_path(
    filesystem: &dyn ExecutorFileSystem,
    config: &RemoteExecConfig,
    candidate: &PathUri,
) -> Result<PathUri, String> {
    let workspace = canonical_workspace(filesystem, config).await?;
    let canonical =
        tokio::time::timeout(config.timeout(), filesystem.canonicalize(candidate, None))
            .await
            .map_err(|_| format!("remote path canonicalization timed out for '{candidate}'"))?
            .map_err(|error| {
                format!("remote path canonicalization failed for '{candidate}': {error}")
            })?;
    ensure_canonical_within(&canonical, &workspace)?;
    Ok(canonical)
}

async fn canonical_workspace(
    filesystem: &dyn ExecutorFileSystem,
    config: &RemoteExecConfig,
) -> Result<PathUri, String> {
    let workspace = config.workspace()?;
    tokio::time::timeout(config.timeout(), filesystem.canonicalize(&workspace, None))
        .await
        .map_err(|_| "remote workspace canonicalization timed out".to_string())?
        .map_err(|error| format!("remote workspace canonicalization failed: {error}"))
}

fn ensure_canonical_within(candidate: &PathUri, workspace: &PathUri) -> Result<(), String> {
    if candidate.ancestors().any(|ancestor| ancestor == *workspace) {
        Ok(())
    } else {
        Err(format!(
            "remote path '{}' resolves outside configured workspace '{}'",
            candidate, workspace
        ))
    }
}

async fn resolve_cwd(
    remote: &Environment,
    config: &RemoteExecConfig,
    requested: Option<&str>,
) -> Result<PathUri, String> {
    let candidate = config.resolve_path(requested.unwrap_or("."))?;
    let filesystem = remote.get_filesystem();
    let path = canonical_existing_path(filesystem.as_ref(), config, &candidate).await?;
    let metadata = tokio::time::timeout(config.timeout(), filesystem.get_metadata(&path, None))
        .await
        .map_err(|_| "remote cwd metadata timed out".to_string())?
        .map_err(|error| format!("remote cwd metadata failed: {error}"))?;
    if !metadata.is_directory {
        return Err(format!("remote cwd '{path}' is not a directory"));
    }
    Ok(path)
}

fn exec_env_policy(config: &RemoteExecConfig) -> ExecEnvPolicy {
    ExecEnvPolicy {
        inherit: config.env_inherit(),
        ignore_default_excludes: false,
        exclude: Vec::new(),
        r#set: HashMap::new(),
        include_only: Vec::new(),
    }
}

fn parse_env_overrides(
    config: &RemoteExecConfig,
    value: Option<&Value>,
) -> Result<HashMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(HashMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "remote env must be a JSON object".to_string())?;
    if object.len() > 256 {
        return Err("remote env contains more than 256 entries".to_string());
    }
    let mut result = HashMap::new();
    for (key, value) in object {
        if !config.admits_env_override(key) {
            return Err(format!("remote env override '{key}' is not configured"));
        }
        let value = value
            .as_str()
            .ok_or_else(|| format!("remote env override '{key}' must be a string"))?;
        if value.len() > 65_536 || value.contains('\0') {
            return Err(format!(
                "remote env override '{key}' exceeds its boundary or contains NUL"
            ));
        }
        result.insert(key.clone(), value.to_string());
    }
    Ok(result)
}

fn decode_bytes(
    object: &Map<String, Value>,
    text_field: &str,
    base64_field: &str,
    maximum: usize,
) -> Result<Vec<u8>, String> {
    let text = object.get(text_field);
    let encoded = object.get(base64_field);
    if text.is_some() == encoded.is_some() {
        return Err(format!(
            "exactly one of {text_field} or {base64_field} must be present"
        ));
    }
    let bytes = if let Some(value) = text {
        value
            .as_str()
            .ok_or_else(|| format!("{text_field} must be a string"))?
            .as_bytes()
            .to_vec()
    } else {
        let encoded = encoded
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{base64_field} must be a base64 string"))?;
        BASE64
            .decode(encoded)
            .map_err(|error| format!("{base64_field} is malformed: {error}"))?
    };
    if bytes.len() > maximum {
        return Err(format!(
            "decoded payload is {} bytes, above configured limit {maximum}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

fn valid_process_handle(value: &str) -> Result<&str, String> {
    if value.len() > 128
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        return Err("remote process_id must match [A-Za-z0-9._-]{1,128}".to_string());
    }
    Ok(value)
}

fn unix_ms() -> Result<u64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| format!("system clock precedes UNIX epoch: {error}"))?;
    u64::try_from(millis).map_err(|_| "system clock does not fit u64 milliseconds".to_string())
}

fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a str>, String> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        Some(_) => Err(format!("{field} must be a non-empty string or null")),
    }
}

fn required_string_array(object: &Map<String, Value>, field: &str) -> Result<Vec<String>, String> {
    let values = object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{field} must be an array"))?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && !value.contains('\0'))
                .map(str::to_string)
                .ok_or_else(|| format!("{field}[{index}] must be a non-empty string without NUL"))
        })
        .collect()
}

fn optional_bool(object: &Map<String, Value>, field: &str) -> Result<Option<bool>, String> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{field} must be a boolean or null")),
    }
}

fn optional_u64(object: &Map<String, Value>, field: &str) -> Result<Option<u64>, String> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{field} must be an unsigned integer or null")),
    }
}

fn optional_usize(object: &Map<String, Value>, field: &str) -> Result<Option<usize>, String> {
    optional_u64(object, field)?
        .map(|value| {
            usize::try_from(value).map_err(|_| format!("{field} does not fit this platform"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use octopus_exec::{
        ExecProcessEventReceiver, ExecProcessFuture, ExecServerError, ReadResponse, SignalResponse,
        TerminateResponse, WriteResponse,
    };
    use octopus_interfaces::InterfaceId;
    use octopus_tool_door_runtime::InterfaceHostIoLimits;

    use super::*;

    struct FailOnceProcess {
        process_id: ProcessId,
        reject_next_terminate: AtomicBool,
        terminate_calls: AtomicUsize,
    }

    impl ExecProcess for FailOnceProcess {
        fn process_id(&self) -> &ProcessId {
            &self.process_id
        }

        fn subscribe_wake(&self) -> tokio::sync::watch::Receiver<u64> {
            unreachable!()
        }

        fn subscribe_events(&self) -> ExecProcessEventReceiver {
            unreachable!()
        }

        fn read(
            &self,
            _after_seq: Option<u64>,
            _max_bytes: Option<usize>,
            _wait_ms: Option<u64>,
        ) -> ExecProcessFuture<'_, ReadResponse> {
            unreachable!()
        }

        fn write(&self, _chunk: Vec<u8>) -> ExecProcessFuture<'_, WriteResponse> {
            unreachable!()
        }

        fn signal(&self, _signal: ProcessSignal) -> ExecProcessFuture<'_, SignalResponse> {
            unreachable!()
        }

        fn terminate(&self) -> ExecProcessFuture<'_, TerminateResponse> {
            self.terminate_calls.fetch_add(1, Ordering::SeqCst);
            let reject = self.reject_next_terminate.swap(false, Ordering::SeqCst);
            Box::pin(async move {
                if reject {
                    Err(ExecServerError::Disconnected(
                        "test termination refusal".to_string(),
                    ))
                } else {
                    Ok(TerminateResponse { running: true })
                }
            })
        }
    }

    #[tokio::test]
    async fn failed_release_keeps_the_same_process_handle_for_retry() {
        let root = tempfile::tempdir().unwrap();
        let formation: WorkerEnvironmentFormation = serde_json::from_value(json!({
            "worker_environment_id": "we_00000000000000000000000000000000",
            "run": "run-test",
            "ee": "ee-test",
            "worker": "system_worker_test",
            "door_interface": "system_remote_test",
            "door_descriptor_hash": "door-hash",
            "worker_environment_kind": "remote_exec_server",
            "worker_environment_kind_descriptor_hash": "00".repeat(32),
            "tool_descriptor_hashes": {},
            "instance_config_hashes": {},
            "formation_pin": "formation-pin",
            "granted_tools": [],
            "door_root_bindings": {},
            "readable_root_slots": [],
            "writable_root_slots": [],
            "network_ceiling": "restricted",
            "requires_release": true
        }))
        .unwrap();
        let durable = root.path().join("durable");
        std::fs::create_dir(&durable).unwrap();
        let mut durable_permissions = std::fs::metadata(&durable).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut durable_permissions, 0o700);
        std::fs::set_permissions(&durable, durable_permissions).unwrap();
        let room = RemoteExecRoom::new(ToolProcessEnvironment {
            interface: InterfaceId::new("system_remote_test"),
            interface_dir: root.path().join("interface"),
            descriptor_snapshot: None,
            var_dir: root.path().join("var"),
            durable_var_dir: Some(durable.clone()),
            private_reads: BTreeMap::new(),
            root_slots: BTreeMap::new(),
            host_io_limits: InterfaceHostIoLimits {
                max_control_frame_bytes: 1,
                max_stderr_bytes: 1,
                max_manifest_bytes: 1,
                max_payload_bytes: 1,
            },
        });
        let process = Arc::new(FailOnceProcess {
            process_id: ProcessId::new("remote-process"),
            reject_next_terminate: AtomicBool::new(true),
            terminate_calls: AtomicUsize::new(0),
        });
        let process_handle: Arc<dyn ExecProcess> = process.clone();
        let workspace = PathUri::from_path(root.path()).unwrap();
        let identity = RemoteRoomIdentity::new(
            &formation,
            "ws://test".to_string(),
            workspace.to_string(),
            "auth-identity".to_string(),
            "config-identity".to_string(),
        );
        let store = Arc::new(RecoveryStore::open(&durable, identity.clone()).unwrap());
        store
            .activate_session("session-test".to_string(), 1)
            .unwrap();
        let durable_process = store
            .begin_process(
                "process",
                "remote-process",
                1,
                8,
                vec!["test".to_string()],
                workspace.to_string(),
                false,
                false,
                1,
            )
            .unwrap();
        store
            .mark_process_active(
                "process",
                &durable_process.session_id,
                &durable_process.remote_process_id,
                durable_process.generation,
                1,
            )
            .unwrap();
        room.rooms.lock().await.insert(
            formation.worker_environment_id.clone(),
            Arc::new(RoomEntry {
                state: tokio::sync::Mutex::new(RoomState {
                    identity,
                    store,
                    request_timeout: Duration::from_secs(1),
                    environment: Some(Arc::new(Environment::default_for_tests())),
                    session_id: Some("session-test".to_string()),
                    unavailable: None,
                    initializing: false,
                    calls: 0,
                    processes: BTreeMap::from([(
                        "process".to_string(),
                        RemoteProcessState {
                            process: Arc::clone(&process_handle),
                            session_id: durable_process.session_id,
                            remote_process_id: durable_process.remote_process_id,
                            generation: durable_process.generation,
                        },
                    )]),
                }),
                changed: tokio::sync::Notify::new(),
            }),
        );

        assert!(room.release_room(&formation).await.is_err());
        let entry = room.rooms.lock().await[&formation.worker_environment_id].clone();
        let state = entry.state.lock().await;
        let retained = &state.processes["process"].process;
        assert!(Arc::ptr_eq(retained, &process_handle));
        drop(state);
        assert_eq!(process.terminate_calls.load(Ordering::SeqCst), 1);

        assert!(room.release_room(&formation).await.is_err());
        assert_eq!(process.terminate_calls.load(Ordering::SeqCst), 2);
        assert!(room
            .rooms
            .lock()
            .await
            .contains_key(&formation.worker_environment_id));
    }
}
