use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use octopus_interfaces::{InterfaceArtifactFile, InvocationMetadata};
use octopus_tool_door_runtime::room::{
    admitted_tool, decode_tool_call, descriptor_operation, emission_root, emit_json_artifact,
    formed_scope, strict_object,
};
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo, WorkerEnvironmentLifecycleRecorder, WorkerEnvironmentLiveStateRecorder,
};
use octopus_tool_handlers::{ToolResultArtifact, ToolResultError};
use octopus_worker_env::{
    AbsPath, NetworkCeiling, WorkerEnvKindSpec, WorkerEnvironmentFormation,
    WorkerEnvironmentLifecycle,
};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

mod config;
mod rooted_file_system;

use config::{
    container_path_within, normalize_container_path, validate_env_name, ContainerConfig,
    ContainerNetwork, MountAccess, MountSourceType, PullPolicy, ReadyCondition, RecoveryPolicy,
};

const ROOM: &str = "container door";
const SUBSTRATE_FAMILY: &str = "container";
const OPERATIONS: &[&str] = &[
    "start", "attach", "status", "exec", "file_read", "file_write", "file_list", "logs", "stop",
    "remove",
];
const INSPECT_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const CONTAINER_INSTANCE_ROOT_SLOTS: &[&str] = &["worktree_root"];

fn project_container_instance_root_bindings(
    formation: &WorkerEnvironmentFormation,
) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut projected = formation.door_root_bindings.clone();
    for slot in CONTAINER_INSTANCE_ROOT_SLOTS {
        let base = formation
            .door_root_bindings
            .get(*slot)
            .ok_or_else(|| format!("container instance root slot '{slot}' is not formed"))?;
        let base = AbsPath::new(base).map_err(|error| error.to_string())?;
        let instance = AbsPath::new(
            &base
                .as_path()
                .join(formation.worker_environment_id.as_str()),
        )
        .map_err(|error| error.to_string())?;
        if !instance.is_within(&base) || instance == base {
            return Err(format!(
                "container worker environment '{}' does not resolve below '{}' base '{}'",
                formation.worker_environment_id,
                slot,
                base.as_str()
            ));
        }
        projected.insert((*slot).to_string(), instance.as_path().to_path_buf());
    }
    Ok(projected)
}

#[derive(Clone, Debug)]
struct ContainerInspection {
    id: String,
    name: String,
    image: String,
    image_id: String,
    created_at: String,
    status: String,
    running: bool,
    paused: bool,
    restarting: bool,
    oom_killed: bool,
    dead: bool,
    exit_code: Option<i64>,
    state_error: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    restart_count: u64,
    health: Option<String>,
    labels: BTreeMap<String, String>,
    mounts: Vec<ContainerMountInspection>,
}

#[derive(Clone, Debug)]
struct ContainerMountInspection {
    mount_type: String,
    source: String,
    destination: String,
    read_write: bool,
}

#[derive(Debug)]
struct BoundedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

#[derive(Debug)]
struct CommandOutcome {
    status: Option<ExitStatus>,
    stdout: BoundedBytes,
    stderr: BoundedBytes,
    timed_out: bool,
    duration_ms: u64,
    timeout_ms: u64,
}

#[derive(Clone, Debug)]
struct RecoveryEvidence {
    action: &'static str,
    previous_status: Option<String>,
}

#[derive(Clone, Debug)]
struct OperationFailure {
    code: &'static str,
    message: String,
}

struct LiveStateUpdate<'a> {
    calls: u64,
    operation: &'a str,
    operation_ok: bool,
    status: &'a str,
    inspection: Option<&'a ContainerInspection>,
    detail: Option<String>,
}

trait WorkerEnvironmentRecorder:
    WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder
{
}

impl<T> WorkerEnvironmentRecorder for T where
    T: WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder + ?Sized
{
}

fn lifecycle_has_live_projection(lifecycle: WorkerEnvironmentLifecycle) -> bool {
    matches!(
        lifecycle,
        WorkerEnvironmentLifecycle::Active | WorkerEnvironmentLifecycle::Failed
    )
}

impl OperationFailure {
    fn new(code: &'static str, message: String) -> Self {
        Self { code, message }
    }
}

impl RecoveryEvidence {
    fn none() -> Self {
        Self {
            action: "none",
            previous_status: None,
        }
    }

    fn body(&self) -> Value {
        json!({
            "action": self.action,
            "previous_status": self.previous_status,
        })
    }
}

pub struct ContainerRoom {
    environment: ToolProcessEnvironment,
    calls: AtomicU64,
    verified_helpers: std::sync::Mutex<BTreeMap<String, String>>,
}

impl ContainerRoom {
    pub fn new(environment: ToolProcessEnvironment) -> Self {
        Self {
            environment,
            calls: AtomicU64::new(0),
            verified_helpers: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) async fn receive(
        &self,
        invocation: ToolProcessInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
        let ToolProcessInvocation {
            descriptor,
            envelope,
            worker_environment,
            worker_environment_kind,
            ..
        } = invocation;
        let formation = worker_environment.ok_or_else(|| {
            ToolProcessError::Capability(
                "container invocation has no formed worker environment".to_string(),
            )
        })?;
        let kind = worker_environment_kind.ok_or_else(|| {
            ToolProcessError::Capability(
                "container invocation has no formed kind descriptor".to_string(),
            )
        })?;
        let config = ContainerConfig::load_formed(&self.environment, &formation, &kind)
            .map_err(ToolProcessError::Capability)?;
        validate_network_grant(&formation, &config)?;
        validate_mount_grants(&formation, &config)?;
        let root_bindings = project_container_instance_root_bindings(&formation)
            .map_err(ToolProcessError::Capability)?;
        let call = decode_tool_call(ROOM, &envelope)?;
        if !call.arguments.is_object() {
            return Err(ToolProcessError::Capability(
                "container tool.call arguments must be a JSON object".to_string(),
            ));
        }
        if descriptor.id.as_str() != call.tool {
            return Err(ToolProcessError::Capability(format!(
                "tool descriptor '{}' does not match tool.call '{}'",
                descriptor.id, call.tool
            )));
        }
        let operation = descriptor_operation(ROOM, descriptor.id.as_str(), &descriptor.handler_config, OPERATIONS)
            .map_err(ToolProcessError::Capability)?;
        formed_scope(ROOM, &envelope, &formation)?;
        validate_runtime_owner(
            envelope
                .metadata()
                .map_err(|error| ToolProcessError::Capability(error.to_string()))?,
            &formation,
        )?;
        admitted_tool(ROOM, &envelope, &call)
            .map_err(|error| ToolProcessError::Capability(error.to_string()))?;
        let sequence = self.calls.fetch_add(1, Ordering::AcqRel) + 1;
        let emission_root = emission_root(&envelope).map_err(ToolProcessError::Capability)?;
        let body = self
            .dispatch(
                &formation,
                &kind,
                &root_bindings,
                &config,
                operation,
                &call.arguments,
                &emission_root,
            )
            .await;
        let inspection = self.inspect_owned(&formation, &config).await;
        let lifecycle = post_operation_lifecycle(
            operation,
            body.is_ok(),
            inspection
                .as_ref()
                .map(|item| item.is_some())
                .map_err(|_| ()),
        );
        let mut evidence_write_errors = Vec::new();
        if let Err(error) = recorder.record(&formation, lifecycle).await {
            evidence_write_errors.push(evidence_write_error("lifecycle", error));
        }
        let inspection_error = inspection.as_ref().err().map(|error| {
            let message = format!("post-operation inspection failed: {error}");
            evidence_write_errors.push(evidence_write_error("inspection", message.clone()));
            message
        });
        let live_detail = body
            .as_ref()
            .err()
            .map(|error| error.message.clone())
            .or_else(|| inspection_error.clone());
        let live_inspection = inspection.as_ref().ok().and_then(Option::as_ref);
        let live_status =
            live_inspection
                .map(|item| item.status.as_str())
                .unwrap_or(if inspection.is_err() {
                    "unknown"
                } else {
                    "absent"
                });
        if lifecycle_has_live_projection(lifecycle) {
            if let Err(error) = self
                .write_live_state(
                    recorder,
                    &formation,
                    &config,
                    LiveStateUpdate {
                        calls: sequence,
                        operation: &call.tool,
                        operation_ok: body.is_ok(),
                        status: live_status,
                        inspection: live_inspection,
                        detail: live_detail,
                    },
                )
                .await
            {
                evidence_write_errors.push(evidence_write_error("live_state", error));
            }
        }
        let response = match body {
            Ok(body) if evidence_write_errors.is_empty() => {
                ToolResultArtifact::success(&call.tool, call.correlation_id, body)
            }
            Ok(body) => {
                let mut details = Map::new();
                details.insert("operation".to_string(), Value::String(call.tool.clone()));
                details.insert("operation_body".to_string(), body);
                details.insert(
                    "evidence_write_errors".to_string(),
                    Value::Array(evidence_write_errors),
                );
                ToolResultArtifact::failure(
                    &call.tool,
                    call.correlation_id,
                    ToolResultError::new(
                        "container_evidence_failed",
                        "container operation completed but its worker-environment evidence could not be fully written",
                    )
                    .with_details(details),
                )
            }
            Err(error) => {
                let mut details = Map::new();
                details.insert("operation".to_string(), Value::String(call.tool.clone()));
                if !evidence_write_errors.is_empty() {
                    details.insert(
                        "evidence_write_errors".to_string(),
                        Value::Array(evidence_write_errors),
                    );
                }
                ToolResultArtifact::failure(
                    &call.tool,
                    call.correlation_id,
                    ToolResultError::new(error.code, error.message).with_details(details),
                )
            }
        };
        Ok(vec![emit_json_artifact(&envelope, &response.label, &response)
            .map_err(ToolProcessError::Capability)?])
    }

    async fn dispatch(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
        tool: &str,
        arguments: &Value,
        emission_root: &Path,
    ) -> Result<Value, OperationFailure> {
        match tool {
            "start" => self
                .start(formation, kind, root_bindings, config, arguments)
                .await
                .map_err(|message| OperationFailure::new("container_start_failed", message)),
            "attach" => self
                .attach(formation, kind, root_bindings, config, arguments)
                .await
                .map_err(|message| OperationFailure::new("container_attach_failed", message)),
            "status" => self
                .status(formation, config, arguments)
                .await
                .map_err(|message| OperationFailure::new("container_status_failed", message)),
            "exec" => self
                .exec(
                    formation,
                    kind,
                    root_bindings,
                    config,
                    arguments,
                    emission_root,
                )
                .await
                .map_err(|message| OperationFailure::new("container_exec_failed", message)),
            "file_read" => self
                .file_read(
                    formation,
                    kind,
                    root_bindings,
                    config,
                    arguments,
                    emission_root,
                )
                .await
                .map_err(|message| OperationFailure::new("container_file_read_failed", message)),
            "file_write" => self
                .file_write(formation, kind, root_bindings, config, arguments)
                .await
                .map_err(|message| OperationFailure::new("container_file_write_failed", message)),
            "file_list" => self
                .file_list(formation, kind, root_bindings, config, arguments)
                .await
                .map_err(|message| OperationFailure::new("container_file_list_failed", message)),
            "logs" => self
                .logs(formation, config, arguments, emission_root)
                .await
                .map_err(|message| OperationFailure::new("container_logs_failed", message)),
            "stop" => self
                .stop(formation, config, arguments)
                .await
                .map_err(|message| OperationFailure::new("container_stop_failed", message)),
            "remove" => self
                .remove(formation, config, arguments)
                .await
                .map_err(|message| OperationFailure::new("container_remove_failed", message)),
            other => Err(OperationFailure::new(
                "container_tool_unsupported",
                format!("container room does not implement operation '{other}'"),
            )),
        }
    }

    async fn start(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        strict_object("container.start", arguments, &[])?;
        self.require_engine(config).await?;
        if let Some(existing) = self.inspect_owned(formation, config).await? {
            if existing.running {
                let ready = self.await_ready(formation, config).await?;
                return Ok(status_body(
                    formation,
                    config,
                    Some(&ready),
                    "attached_running",
                ));
            }
            if !config.reuse_stopped {
                return Err(format!(
                    "formed container '{}' exists in '{}' state and reuse_stopped is false",
                    existing.name, existing.status
                ));
            }
            let outcome = self
                .run_engine(
                    config,
                    vec!["start".to_string(), existing.id.clone()],
                    None,
                    config.startup_timeout_ms,
                    config.max_output_bytes,
                )
                .await?;
            require_success("container start", &outcome)?;
            let started = self.await_ready(formation, config).await?;
            return Ok(status_body(formation, config, Some(&started), "restarted"));
        }
        let attempt = creation_attempt(formation, self.calls.load(Ordering::Acquire));
        let create = self
            .create_container(formation, kind, root_bindings, config, &attempt)
            .await;
        if let Err(error) = create {
            if let Some(concurrent) = self.inspect_owned(formation, config).await? {
                if concurrent.running {
                    let ready = self.await_ready(formation, config).await?;
                    return Ok(status_body(
                        formation,
                        config,
                        Some(&ready),
                        "attached_concurrent_create",
                    ));
                }
            }
            self.remove_after_failed_create(formation, config, &attempt)
                .await;
            return Err(error);
        }
        let started = self.await_ready(formation, config).await?;
        Ok(status_body(formation, config, Some(&started), "created"))
    }

    async fn attach(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object(
            "container.attach",
            arguments,
            &["start_if_stopped", "recover_if_missing"],
        )?;
        self.require_engine(config).await?;
        let start_if_stopped = optional_bool(object, "start_if_stopped")?.unwrap_or(false);
        let recover_if_missing = optional_bool(object, "recover_if_missing")?.unwrap_or(false);
        match self.inspect_owned(formation, config).await? {
            Some(existing) if existing.running => {
                let ready = self.await_ready(formation, config).await?;
                Ok(status_body(
                    formation,
                    config,
                    Some(&ready),
                    "attached_running",
                ))
            }
            Some(existing) if start_if_stopped => {
                let outcome = self
                    .run_engine(
                        config,
                        vec!["start".to_string(), existing.id],
                        None,
                        config.startup_timeout_ms,
                        config.max_output_bytes,
                    )
                    .await?;
                require_success("container attach start", &outcome)?;
                let ready = self.await_ready(formation, config).await?;
                Ok(status_body(
                    formation,
                    config,
                    Some(&ready),
                    "attached_and_restarted",
                ))
            }
            Some(existing) => Ok(status_body(
                formation,
                config,
                Some(&existing),
                "attached_stopped",
            )),
            None if recover_if_missing => {
                let attempt = creation_attempt(formation, self.calls.load(Ordering::Acquire));
                self.create_container(formation, kind, root_bindings, config, &attempt)
                    .await?;
                let ready = self.await_ready(formation, config).await?;
                Ok(status_body(
                    formation,
                    config,
                    Some(&ready),
                    "attached_recreated",
                ))
            }
            None => Err("formed container does not exist".to_string()),
        }
    }

    async fn create_container(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
        attempt: &str,
    ) -> Result<(), String> {
        self.prepare_image(config).await?;
        let name = container_name(formation, config);
        let args = self.run_arguments(formation, kind, root_bindings, config, &name, attempt)?;
        let outcome = self
            .run_engine(
                config,
                args,
                None,
                config.startup_timeout_ms,
                config.max_output_bytes,
            )
            .await?;
        require_success("container run", &outcome)
    }

    async fn status(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &ContainerConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        strict_object("container.status", arguments, &[])?;
        self.require_engine(config).await?;
        let inspection = self.inspect_owned(formation, config).await?;
        Ok(status_body(
            formation,
            config,
            inspection.as_ref(),
            "inspected",
        ))
    }

    async fn exec(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
        arguments: &Value,
        emission_root: &Path,
    ) -> Result<Value, String> {
        let object = strict_object(
            "container.exec",
            arguments,
            &[
                "argv",
                "cwd",
                "env",
                "stdin_text",
                "stdin_base64",
                "timeout_ms",
                "max_output_bytes",
            ],
        )?;
        let (container, recovery) = self
            .require_running(formation, kind, root_bindings, config)
            .await?;
        let argv = required_string_array(object, "argv", 1, 256)?;
        let requested_cwd = object
            .get("cwd")
            .map(|value| required_string_value(value, "cwd"))
            .transpose()?
            .map(|value| config.path_for_read(value))
            .transpose()?
            .unwrap_or(config.normalized_workdir()?);
        let env = optional_string_map(object, "env")?.unwrap_or_default();
        let stdin = exclusive_bytes(object, "stdin_text", "stdin_base64", config.max_file_bytes)?;
        let timeout_ms = optional_u64(object, "timeout_ms")?.unwrap_or(config.exec_timeout_ms);
        if timeout_ms == 0 || timeout_ms > config.exec_timeout_ms {
            return Err(format!(
                "container.exec timeout_ms must be between 1 and {}",
                config.exec_timeout_ms
            ));
        }
        let max_output_bytes =
            optional_usize(object, "max_output_bytes")?.unwrap_or(config.max_output_bytes);
        if max_output_bytes == 0 || max_output_bytes > config.max_output_bytes {
            return Err(format!(
                "container.exec max_output_bytes must be between 1 and {}",
                config.max_output_bytes
            ));
        }
        let helper = self.ensure_rooted_helper(config, &container).await?;
        let mut args = vec!["exec".to_string(), "-i".to_string()];
        for (name, value) in env {
            args.push("--env".to_string());
            args.push(format!("{name}={value}"));
        }
        args.push(container.id.clone());
        args.push(helper);
        args.push("exec-cwd".to_string());
        args.push(requested_cwd);
        args.push(config.readable_paths.len().to_string());
        for root in &config.readable_paths {
            args.push(normalize_container_path(root)?);
        }
        args.extend(argv);
        let outcome = self
            .run_engine(config, args, stdin, timeout_ms, max_output_bytes)
            .await?;
        let mut body = command_body(
            emission_root,
            "container.exec",
            &outcome,
            config.inline_text_preview_bytes,
        )?;
        body.as_object_mut()
            .ok_or_else(|| "container.exec result was not an object".to_string())?
            .insert("recovery".to_string(), recovery.body());
        Ok(body)
    }

    async fn file_read(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
        arguments: &Value,
        emission_root: &Path,
    ) -> Result<Value, String> {
        let object = strict_object(
            "container.file_read",
            arguments,
            &["path", "max_bytes", "offset_bytes", "allow_partial"],
        )?;
        let requested_path = config.path_for_read(required_string(object, "path")?)?;
        let max_bytes = optional_usize(object, "max_bytes")?.unwrap_or(config.max_file_bytes);
        if max_bytes == 0 || max_bytes > config.max_file_bytes {
            return Err(format!(
                "container.file_read max_bytes must be between 1 and {}",
                config.max_file_bytes
            ));
        }
        let offset_bytes = optional_u64(object, "offset_bytes")?.unwrap_or(0);
        let allow_partial = optional_bool(object, "allow_partial")?.unwrap_or(false);
        let (container, recovery) = self
            .require_running(formation, kind, root_bindings, config)
            .await?;
        let outcome = self
            .run_rooted_file_operation(
                config,
                &container,
                "read",
                vec![
                    requested_path.clone(),
                    offset_bytes.to_string(),
                    max_bytes.to_string(),
                    if allow_partial { "1" } else { "0" }.to_string(),
                ],
                &config.readable_paths,
                None,
                max_bytes.saturating_add(256),
            )
            .await?;
        if outcome.timed_out {
            return Err("container.file_read timed out".to_string());
        }
        if outcome.stdout.truncated {
            return Err("container.file_read violated its bounded read contract".to_string());
        }
        require_success("container.file_read", &outcome)?;
        let read = rooted_file_system::parse_read_result(&outcome.stdout.bytes)?;
        let carrier =
            materialize_binary_carrier(emission_root, "container.file_read.content", read.content)?;
        let preview = text_preview(read.content, config.inline_text_preview_bytes);
        Ok(json!({
            "path": requested_path,
            "file_size": read.file_size,
            "offset_bytes": offset_bytes,
            "bytes_read": read.declared_bytes,
            "truncated": read.truncated,
            "text_preview": preview,
            "carrier": carrier,
            "content_hash": blake3::hash(read.content).to_hex().to_string(),
            "recovery": recovery.body(),
        }))
    }

    async fn file_write(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object(
            "container.file_write",
            arguments,
            &["path", "text", "base64", "create_parents"],
        )?;
        let requested_path = config.path_for_write(required_string(object, "path")?)?;
        if config
            .writable_paths
            .iter()
            .map(|root| normalize_container_path(root))
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|root| root == &requested_path)
        {
            return Err(
                "container.file_write path must name a file below a writable root".to_string(),
            );
        }
        let bytes = exclusive_required_bytes(object, "text", "base64", config.max_file_bytes)?;
        let create_parents = optional_bool(object, "create_parents")?.unwrap_or(false);
        let (container, recovery) = self
            .require_running(formation, kind, root_bindings, config)
            .await?;
        let outcome = self
            .run_rooted_file_operation(
                config,
                &container,
                "write",
                vec![
                    requested_path.clone(),
                    config.max_file_bytes.to_string(),
                    if create_parents { "1" } else { "0" }.to_string(),
                ],
                &config.writable_paths,
                Some(bytes.clone()),
                config.max_output_bytes,
            )
            .await?;
        require_success("container.file_write", &outcome)?;
        Ok(json!({
            "path": requested_path,
            "bytes_written": bytes.len(),
            "content_hash": blake3::hash(&bytes).to_hex().to_string(),
            "recovery": recovery.body(),
        }))
    }

    async fn file_list(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object("container.file_list", arguments, &["path", "max_entries"])?;
        let requested_path = config.path_for_read(required_string(object, "path")?)?;
        let max_entries =
            optional_usize(object, "max_entries")?.unwrap_or(config.max_directory_entries);
        if max_entries == 0 || max_entries > config.max_directory_entries {
            return Err(format!(
                "container.file_list max_entries must be between 1 and {}",
                config.max_directory_entries
            ));
        }
        let (container, recovery) = self
            .require_running(formation, kind, root_bindings, config)
            .await?;
        let output_limit = max_entries
            .saturating_add(1)
            .saturating_mul(8192)
            .min(config.max_output_bytes);
        let outcome = self
            .run_rooted_file_operation(
                config,
                &container,
                "list",
                vec![requested_path.clone(), max_entries.to_string()],
                &config.readable_paths,
                None,
                output_limit,
            )
            .await?;
        if outcome.stdout.truncated {
            return Err("container.file_list output exceeded its admitted bound".to_string());
        }
        require_success("container.file_list", &outcome)?;
        let entries = parse_directory_entries(&outcome.stdout.bytes, max_entries)?;
        Ok(json!({
            "path": requested_path,
            "entries": entries,
            "count": entries.len(),
            "recovery": recovery.body(),
        }))
    }

    async fn logs(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &ContainerConfig,
        arguments: &Value,
        emission_root: &Path,
    ) -> Result<Value, String> {
        let object = strict_object(
            "container.logs",
            arguments,
            &["tail", "since", "timestamps", "max_output_bytes"],
        )?;
        let tail = optional_u64(object, "tail")?.unwrap_or(200);
        if tail == 0 || tail > 10_000 {
            return Err("container.logs tail must be between 1 and 10000".to_string());
        }
        let container = self
            .inspect_owned(formation, config)
            .await?
            .ok_or_else(|| "formed container does not exist".to_string())?;
        let max_output_bytes =
            optional_usize(object, "max_output_bytes")?.unwrap_or(config.max_output_bytes);
        if max_output_bytes == 0 || max_output_bytes > config.max_output_bytes {
            return Err(format!(
                "container.logs max_output_bytes must be between 1 and {}",
                config.max_output_bytes
            ));
        }
        let timestamps = optional_bool(object, "timestamps")?.unwrap_or(false);
        let since = object
            .get("since")
            .map(|value| required_string_value(value, "since"))
            .transpose()?;
        if since.is_some_and(|value| value.len() > 128) {
            return Err("container.logs since exceeds 128 bytes".to_string());
        }
        let mut args = vec!["logs".to_string(), "--tail".to_string(), tail.to_string()];
        if timestamps {
            args.push("--timestamps".to_string());
        }
        if let Some(since) = since {
            args.push("--since".to_string());
            args.push(since.to_string());
        }
        args.push(container.id);
        let outcome = self
            .run_engine(config, args, None, config.exec_timeout_ms, max_output_bytes)
            .await?;
        require_exit_success("container.logs", &outcome)?;
        let stdout = stream_body(
            emission_root,
            "container.logs.stdout",
            &outcome.stdout,
            config.inline_text_preview_bytes,
        )?;
        let stderr = stream_body(
            emission_root,
            "container.logs.stderr",
            &outcome.stderr,
            config.inline_text_preview_bytes,
        )?;
        Ok(json!({
            "tail": tail,
            "since": since,
            "timestamps": timestamps,
            "duration_ms": outcome.duration_ms,
            "stdout": stdout,
            "stderr": stderr,
        }))
    }

    async fn stop(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &ContainerConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object("container.stop", arguments, &["timeout_seconds"])?;
        let timeout = optional_u64(object, "timeout_seconds")?.unwrap_or(10);
        if timeout > 120 {
            return Err("container.stop timeout_seconds must be at most 120".to_string());
        }
        let existing = self
            .inspect_owned(formation, config)
            .await?
            .ok_or_else(|| "formed container does not exist".to_string())?;
        if existing.running {
            let outcome = self
                .run_engine(
                    config,
                    vec![
                        "stop".to_string(),
                        "--time".to_string(),
                        timeout.to_string(),
                        existing.id,
                    ],
                    None,
                    config.startup_timeout_ms,
                    config.max_output_bytes,
                )
                .await?;
            require_success("container.stop", &outcome)?;
        }
        let stopped = self
            .inspect_owned(formation, config)
            .await?
            .ok_or_else(|| "container disappeared after stop".to_string())?;
        Ok(status_body(formation, config, Some(&stopped), "stopped"))
    }

    async fn remove(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &ContainerConfig,
        arguments: &Value,
    ) -> Result<Value, String> {
        let object = strict_object("container.remove", arguments, &["remove_volumes"])?;
        let remove_volumes = optional_bool(object, "remove_volumes")?.unwrap_or(false);
        let existing = self
            .inspect_owned(formation, config)
            .await?
            .ok_or_else(|| "formed container does not exist".to_string())?;
        let container_id = existing.id.clone();
        let mut args = vec!["rm".to_string(), "-f".to_string()];
        if remove_volumes {
            args.push("--volumes".to_string());
        }
        args.push(existing.id);
        let outcome = self
            .run_engine(
                config,
                args,
                None,
                config.startup_timeout_ms,
                config.max_output_bytes,
            )
            .await?;
        require_success("container.remove", &outcome)?;
        if self.inspect_owned(formation, config).await?.is_some() {
            return Err("container still exists after remove".to_string());
        }
        self.verified_helpers
            .lock()
            .map_err(|_| "container rooted helper cache is poisoned".to_string())?
            .remove(&container_id);
        Ok(status_body(formation, config, None, "removed"))
    }

    async fn require_running(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
    ) -> Result<(ContainerInspection, RecoveryEvidence), String> {
        match self.inspect_owned(formation, config).await? {
            Some(inspection) if inspection.running => Ok((inspection, RecoveryEvidence::none())),
            Some(inspection) if config.recovery_policy == RecoveryPolicy::Refuse => Err(format!(
                "formed container '{}' is '{}'; recovery_policy refuses restart",
                inspection.name, inspection.status
            )),
            Some(inspection) => {
                let previous_status = inspection.status.clone();
                let start = self
                    .run_engine(
                        config,
                        vec!["start".to_string(), inspection.id.clone()],
                        None,
                        config.startup_timeout_ms,
                        config.max_output_bytes,
                    )
                    .await?;
                if require_success("container recovery start", &start).is_ok() {
                    let ready = self.await_ready(formation, config).await?;
                    return Ok((
                        ready,
                        RecoveryEvidence {
                            action: "restarted",
                            previous_status: Some(previous_status),
                        },
                    ));
                }
                if config.recovery_policy != RecoveryPolicy::RestartOrRecreate {
                    return Err(format!(
                        "formed container '{}' could not be restarted: {}",
                        inspection.name,
                        outcome_error(&start)
                    ));
                }
                let remove = self
                    .run_engine(
                        config,
                        vec!["rm".to_string(), "-f".to_string(), inspection.id],
                        None,
                        config.startup_timeout_ms,
                        config.max_output_bytes,
                    )
                    .await?;
                require_success("container recovery remove", &remove)?;
                let attempt = creation_attempt(formation, self.calls.load(Ordering::Acquire));
                self.create_container(formation, kind, root_bindings, config, &attempt)
                    .await?;
                let ready = self.await_ready(formation, config).await?;
                Ok((
                    ready,
                    RecoveryEvidence {
                        action: "recreated_after_restart_failure",
                        previous_status: Some(previous_status),
                    },
                ))
            }
            None if config.recovery_policy == RecoveryPolicy::RestartOrRecreate => {
                let attempt = creation_attempt(formation, self.calls.load(Ordering::Acquire));
                self.create_container(formation, kind, root_bindings, config, &attempt)
                    .await?;
                let ready = self.await_ready(formation, config).await?;
                Ok((
                    ready,
                    RecoveryEvidence {
                        action: "recreated_missing",
                        previous_status: None,
                    },
                ))
            }
            None => Err(
                "formed container does not exist and configured recovery cannot recreate it"
                    .to_string(),
            ),
        }
    }

    async fn await_ready(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &ContainerConfig,
    ) -> Result<ContainerInspection, String> {
        let deadline = Instant::now() + Duration::from_millis(config.ready_timeout_ms);
        loop {
            let inspection = self
                .inspect_owned(formation, config)
                .await?
                .ok_or_else(|| "container disappeared while awaiting readiness".to_string())?;
            if !inspection.running {
                return Err(format!(
                    "container '{}' entered '{}' while awaiting readiness",
                    inspection.name, inspection.status
                ));
            }
            match config.ready_condition {
                ReadyCondition::Running => return Ok(inspection),
                ReadyCondition::Healthy if inspection.health.as_deref() == Some("healthy") => {
                    return Ok(inspection);
                }
                ReadyCondition::Healthy if inspection.health.as_deref() == Some("unhealthy") => {
                    return Err(format!(
                        "container '{}' reported unhealthy during readiness",
                        inspection.name
                    ));
                }
                ReadyCondition::Healthy if inspection.health.is_none() => {
                    return Err(format!(
                        "container '{}' has no healthcheck but ready_condition is healthy",
                        inspection.name
                    ));
                }
                ReadyCondition::Healthy => {}
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "container '{}' did not become healthy within {} ms",
                    inspection.name, config.ready_timeout_ms
                ));
            }
            tokio::time::sleep(Duration::from_millis(config.ready_poll_ms)).await;
        }
    }

    async fn ensure_rooted_helper(
        &self,
        config: &ContainerConfig,
        container: &ContainerInspection,
    ) -> Result<String, String> {
        if let Some(path) = self
            .verified_helpers
            .lock()
            .map_err(|_| "container rooted helper cache is poisoned".to_string())?
            .get(&container.id)
            .cloned()
        {
            return Ok(path);
        }
        let architecture = self
            .run_engine(
                config,
                vec![
                    "exec".to_string(),
                    container.id.clone(),
                    "/bin/uname".to_string(),
                    "-m".to_string(),
                ],
                None,
                config.exec_timeout_ms,
                256,
            )
            .await?;
        require_success("container rooted helper architecture", &architecture)?;
        if architecture.stdout.truncated {
            return Err("container rooted helper architecture exceeded its bound".to_string());
        }
        let architecture = std::str::from_utf8(&architecture.stdout.bytes)
            .map_err(|_| "container architecture is not UTF-8".to_string())?;
        let asset = rooted_file_system::asset_for_architecture(architecture)?;
        let local = rooted_file_system::materialize(&self.environment.var_dir, &asset)?;
        let local = local.canonicalize().map_err(|error| {
            format!(
                "container rooted helper '{}' cannot be canonicalized: {error}",
                local.display()
            )
        })?;
        let remote = rooted_file_system::remote_path(&asset);
        let source = local.to_str().ok_or_else(|| {
            format!(
                "container rooted helper '{}' cannot be represented as UTF-8",
                local.display()
            )
        })?;
        let mount = container
            .mounts
            .iter()
            .find(|mount| mount.destination == remote)
            .ok_or_else(|| {
                format!(
                    "container '{}' lacks immutable rooted helper mount '{}' and must be recreated",
                    container.id, remote
                )
            })?;
        if mount.mount_type != "bind" || mount.read_write || mount.source != source {
            return Err(format!(
                "container '{}' rooted helper mount '{}' is not the expected read-only host asset",
                container.id, remote
            ));
        }
        let version = self
            .run_engine(
                config,
                vec![
                    "exec".to_string(),
                    container.id.clone(),
                    remote.clone(),
                    "version".to_string(),
                ],
                None,
                config.exec_timeout_ms,
                4096,
            )
            .await?;
        require_success("container rooted helper version", &version)?;
        rooted_file_system::validate_version(&version.stdout.bytes, &asset)?;
        self.verified_helpers
            .lock()
            .map_err(|_| "container rooted helper cache is poisoned".to_string())?
            .insert(container.id.clone(), remote.clone());
        Ok(remote)
    }

    async fn run_rooted_file_operation(
        &self,
        config: &ContainerConfig,
        container: &ContainerInspection,
        operation: &str,
        mut arguments: Vec<String>,
        roots: &[String],
        stdin: Option<Vec<u8>>,
        output_limit: usize,
    ) -> Result<CommandOutcome, String> {
        let helper = self.ensure_rooted_helper(config, container).await?;
        let mut engine_arguments = vec!["exec".to_string()];
        if stdin.is_some() {
            engine_arguments.push("-i".to_string());
        }
        engine_arguments.push(container.id.clone());
        engine_arguments.push(helper);
        engine_arguments.push(operation.to_string());
        engine_arguments.append(&mut arguments);
        for root in roots {
            engine_arguments.push(normalize_container_path(root)?);
        }
        self.run_engine(
            config,
            engine_arguments,
            stdin,
            config.exec_timeout_ms,
            output_limit,
        )
        .await
    }

    async fn require_engine(&self, config: &ContainerConfig) -> Result<(), String> {
        let outcome = self
            .run_engine(
                config,
                vec![
                    "version".to_string(),
                    "--format".to_string(),
                    "{{.Server.Version}}".to_string(),
                ],
                None,
                30_000,
                1024 * 1024,
            )
            .await?;
        require_success("container engine version", &outcome)
    }

    async fn prepare_image(&self, config: &ContainerConfig) -> Result<(), String> {
        if config.pull_policy == PullPolicy::Always {
            return self.pull_image(config).await;
        }
        let outcome = self
            .run_engine(
                config,
                vec![
                    "image".to_string(),
                    "inspect".to_string(),
                    config.image.clone(),
                ],
                None,
                30_000,
                INSPECT_OUTPUT_LIMIT,
            )
            .await?;
        if outcome.status.is_some_and(|status| status.success()) {
            return Ok(());
        }
        if config.pull_policy == PullPolicy::Never {
            return Err(format!(
                "container image '{}' is unavailable and pull_policy is never: {}",
                config.image,
                outcome_error(&outcome)
            ));
        }
        if !missing_image(&outcome) {
            return Err(format!(
                "container image inspection for '{}' failed without proving absence: {}",
                config.image,
                outcome_error(&outcome)
            ));
        }
        self.pull_image(config).await
    }

    async fn pull_image(&self, config: &ContainerConfig) -> Result<(), String> {
        let outcome = self
            .run_engine(
                config,
                vec!["pull".to_string(), config.image.clone()],
                None,
                config.startup_timeout_ms,
                config.max_output_bytes,
            )
            .await?;
        require_success("container image pull", &outcome)
    }

    fn run_arguments(
        &self,
        formation: &WorkerEnvironmentFormation,
        _kind: &WorkerEnvKindSpec,
        root_bindings: &BTreeMap<String, PathBuf>,
        config: &ContainerConfig,
        name: &str,
        creation_attempt: &str,
    ) -> Result<Vec<String>, String> {
        let mut args = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            name.to_string(),
            "--label".to_string(),
            "octopus.managed=1".to_string(),
            "--label".to_string(),
            format!(
                "octopus.worker_environment_id={}",
                formation.worker_environment_id
            ),
            "--label".to_string(),
            format!("octopus.formation_pin={}", formation.formation_pin),
            "--label".to_string(),
            format!("octopus.interface={}", formation.door_interface),
            "--label".to_string(),
            format!("octopus.creation_attempt={creation_attempt}"),
            "--workdir".to_string(),
            config.normalized_workdir()?,
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--pids-limit".to_string(),
            config.pids_limit.to_string(),
            "--cpus".to_string(),
            config.cpu_limit.to_string(),
            "--memory".to_string(),
            config.memory_bytes.to_string(),
            "--shm-size".to_string(),
            config.shm_bytes.to_string(),
            "--network".to_string(),
            config.network_engine_value()?.to_string(),
            "--tmpfs".to_string(),
            "/tmp:rw,exec,nosuid,nodev,size=536870912".to_string(),
            "--tmpfs".to_string(),
            "/var/tmp:rw,noexec,nosuid,nodev,size=268435456".to_string(),
            "--tmpfs".to_string(),
            "/run/octopus:rw,exec,nosuid,nodev,size=16777216,mode=0755".to_string(),
        ];
        let mut helper_targets = BTreeSet::new();
        for architecture in ["aarch64", "x86_64"] {
            let asset = rooted_file_system::asset_for_architecture(architecture)?;
            let source = rooted_file_system::materialize(&self.environment.var_dir, &asset)?
                .canonicalize()
                .map_err(|error| {
                    format!("container rooted helper cannot be canonicalized: {error}")
                })?;
            let source = source.to_str().ok_or_else(|| {
                "container rooted helper path cannot be represented as UTF-8".to_string()
            })?;
            if !source.starts_with('/')
                || source.contains(',')
                || source.contains('\n')
                || source.contains('\0')
            {
                return Err("container rooted helper path cannot be encoded safely".to_string());
            }
            let target = rooted_file_system::remote_path(&asset);
            helper_targets.insert(target.clone());
            args.push("--mount".to_string());
            args.push(format!(
                "type=bind,src={source},dst={target},bind-propagation=rprivate,readonly"
            ));
        }
        if let Some(storage_bytes) = config.storage_bytes {
            args.push("--storage-opt".to_string());
            args.push(format!("size={storage_bytes}"));
        }
        if config.init {
            args.insert(2, "--init".to_string());
        }
        if config.read_only_root {
            args.push("--read-only".to_string());
            let writable_mounts = config
                .mounts
                .iter()
                .filter(|mount| mount.access == MountAccess::ReadWrite)
                .map(|mount| normalize_container_path(&mount.target))
                .collect::<Result<Vec<_>, _>>()?;
            for root in &config.writable_paths {
                let root = normalize_container_path(root)?;
                if root == "/tmp"
                    || root == "/var/tmp"
                    || root == "/run/octopus"
                    || writable_mounts.iter().any(|target| {
                        root == *target
                            || root
                                .strip_prefix(target)
                                .is_some_and(|tail| tail.starts_with('/'))
                    })
                {
                    continue;
                }
                args.push("--tmpfs".to_string());
                args.push(format!("{root}:rw,exec,nosuid,nodev,size=1073741824"));
            }
        }
        if let Some(user) = config.user.as_ref() {
            args.push("--user".to_string());
            args.push(user.clone());
        }
        if let Some(hostname) = config.hostname.as_ref() {
            args.push("--hostname".to_string());
            args.push(hostname.clone());
        }
        for server in &config.dns {
            args.push("--dns".to_string());
            args.push(server.clone());
        }
        for binding in &config.publish_ports {
            let host_ip = if binding.host_ip.contains(':') {
                format!("[{}]", binding.host_ip)
            } else {
                binding.host_ip.clone()
            };
            args.push("--publish".to_string());
            args.push(format!(
                "{}:{}:{}/{}",
                host_ip,
                binding.host_port,
                binding.container_port,
                binding.protocol.as_engine_value()
            ));
        }
        for (name, value) in resolved_environment(config)? {
            args.push("--env".to_string());
            args.push(format!("{name}={value}"));
        }
        for (key, value) in &config.labels {
            args.push("--label".to_string());
            args.push(format!("{key}={value}"));
        }
        for mount in &config.mounts {
            let source = root_bindings.get(&mount.root_slot).ok_or_else(|| {
                format!(
                    "container mount references absent formed root slot '{}'",
                    mount.root_slot
                )
            })?;
            if CONTAINER_INSTANCE_ROOT_SLOTS.contains(&mount.root_slot.as_str())
                && mount.source_type == MountSourceType::Directory
            {
                std::fs::create_dir_all(source).map_err(|error| {
                    format!(
                        "container instance root for slot '{}' cannot be created: {error}",
                        mount.root_slot
                    )
                })?;
            }
            let source = source.canonicalize().map_err(|error| {
                format!(
                    "container mount source for root slot '{}' cannot be canonicalized: {error}",
                    mount.root_slot
                )
            })?;
            let metadata = std::fs::metadata(&source).map_err(|error| {
                format!(
                    "container mount source for root slot '{}' cannot be inspected: {error}",
                    mount.root_slot
                )
            })?;
            let expected_type_matches = match mount.source_type {
                MountSourceType::File => metadata.is_file(),
                MountSourceType::Directory => metadata.is_dir(),
            };
            if !expected_type_matches {
                return Err(format!(
                    "container mount source for root slot '{}' is not a {:?}",
                    mount.root_slot, mount.source_type
                ));
            }
            let source = source.to_string_lossy();
            if !source.starts_with('/')
                || source.contains(',')
                || source.contains('\n')
                || source.contains('\0')
            {
                return Err(format!(
                    "container mount source for root slot '{}' cannot be encoded safely",
                    mount.root_slot
                ));
            }
            let target = normalize_container_path(&mount.target)?;
            if helper_targets.iter().any(|helper| {
                container_path_within(helper, &target) || container_path_within(&target, helper)
            }) {
                return Err(format!(
                    "container mount target '{target}' overlaps the immutable rooted helper mounts"
                ));
            }
            let mut value =
                format!("type=bind,src={source},dst={target},bind-propagation=rprivate");
            if mount.access == MountAccess::ReadOnly {
                value.push_str(",readonly");
            }
            args.push("--mount".to_string());
            args.push(value);
        }
        args.push(config.image.clone());
        args.extend(config.start_argv.clone());
        Ok(args)
    }

    async fn inspect_owned(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &ContainerConfig,
    ) -> Result<Option<ContainerInspection>, String> {
        let listing = self
            .run_engine(
                config,
                vec![
                    "ps".to_string(),
                    "-a".to_string(),
                    "--filter".to_string(),
                    "label=octopus.managed=1".to_string(),
                    "--filter".to_string(),
                    format!(
                        "label=octopus.worker_environment_id={}",
                        formation.worker_environment_id
                    ),
                    "--filter".to_string(),
                    format!("label=octopus.formation_pin={}", formation.formation_pin),
                    "--filter".to_string(),
                    format!("label=octopus.interface={}", formation.door_interface),
                    "--format".to_string(),
                    "{{.ID}}".to_string(),
                ],
                None,
                30_000,
                INSPECT_OUTPUT_LIMIT,
            )
            .await?;
        require_success("container identity listing", &listing)?;
        let candidates = parse_identifier_lines(&listing.stdout.bytes)?;
        if candidates.len() > 1 {
            return Err(format!(
                "formed worker environment has {} attested containers; identity is ambiguous",
                candidates.len()
            ));
        }
        if let Some(identifier) = candidates.first() {
            let inspection = self.inspect_identifier(config, identifier).await?;
            validate_ownership(formation, config, &inspection)?;
            return Ok(Some(inspection));
        }
        let name = container_name(formation, config);
        let inspection = self.inspect_identifier_optional(config, &name).await?;
        let Some(inspection) = inspection else {
            return Ok(None);
        };
        validate_ownership(formation, config, &inspection)?;
        Ok(Some(inspection))
    }

    async fn inspect_identifier(
        &self,
        config: &ContainerConfig,
        identifier: &str,
    ) -> Result<ContainerInspection, String> {
        self.inspect_identifier_optional(config, identifier)
            .await?
            .ok_or_else(|| format!("container '{identifier}' disappeared during inspection"))
    }

    async fn inspect_identifier_optional(
        &self,
        config: &ContainerConfig,
        identifier: &str,
    ) -> Result<Option<ContainerInspection>, String> {
        let outcome = self
            .run_engine(
                config,
                vec!["inspect".to_string(), identifier.to_string()],
                None,
                30_000,
                INSPECT_OUTPUT_LIMIT,
            )
            .await?;
        if !outcome.status.is_some_and(|status| status.success()) {
            if missing_container(&outcome) {
                return Ok(None);
            }
            return Err(format!(
                "container inspect for '{identifier}' failed: {}",
                outcome_error(&outcome)
            ));
        }
        if outcome.stdout.truncated {
            return Err(format!(
                "container inspect for '{identifier}' exceeded its output bound"
            ));
        }
        let inspection = parse_inspection(&outcome.stdout.bytes)?;
        Ok(Some(inspection))
    }

    async fn remove_after_failed_create(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &ContainerConfig,
        attempt: &str,
    ) {
        if let Ok(Some(inspection)) = self.inspect_owned(formation, config).await {
            if inspection
                .labels
                .get("octopus.creation_attempt")
                .map(String::as_str)
                != Some(attempt)
            {
                return;
            }
            let _ = self
                .run_engine(
                    config,
                    vec!["rm".to_string(), "-f".to_string(), inspection.id],
                    None,
                    30_000,
                    1024 * 1024,
                )
                .await;
        }
    }

    async fn run_engine(
        &self,
        config: &ContainerConfig,
        args: Vec<String>,
        stdin: Option<Vec<u8>>,
        timeout_ms: u64,
        max_output_bytes: usize,
    ) -> Result<CommandOutcome, String> {
        run_command(
            &config.engine,
            &args,
            stdin,
            Duration::from_millis(timeout_ms),
            max_output_bytes,
        )
        .await
    }

    async fn write_live_state(
        &self,
        recorder: &dyn WorkerEnvironmentRecorder,
        formation: &WorkerEnvironmentFormation,
        config: &ContainerConfig,
        update: LiveStateUpdate<'_>,
    ) -> Result<(), String> {
        let name = update
            .inspection
            .map(|inspection| inspection.name.clone())
            .unwrap_or_else(|| container_name(formation, config));
        let body = json!({
            "schema_version": 1,
            "worker_environment_id": formation.worker_environment_id,
            "worker_environment_kind": formation.worker_environment_kind,
            "substrate_family": SUBSTRATE_FAMILY,
            "status": update.status,
            "calls": update.calls,
            "last_operation": update.operation,
            "last_operation_ok": update.operation_ok,
            "container": update.inspection.map(inspection_body),
            "viewer": {
                "protocol": "container_runtime",
                "address": viewer_address(config, &name),
                "container_name": name,
            },
            "detail": update.detail,
        });
        recorder.record_live_state(formation, &body).await
    }

    async fn release_container(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &ContainerConfig,
    ) -> Result<(), String> {
        let Some(existing) = self.inspect_owned(formation, config).await? else {
            return Ok(());
        };
        let container_id = existing.id.clone();
        let outcome = self
            .run_engine(
                config,
                vec!["rm".to_string(), "-f".to_string(), existing.id],
                None,
                config.startup_timeout_ms,
                config.max_output_bytes,
            )
            .await?;
        require_success("container release", &outcome)?;
        if self.inspect_owned(formation, config).await?.is_some() {
            return Err("container still exists after worker-environment release".to_string());
        }
        self.verified_helpers
            .lock()
            .map_err(|_| "container rooted helper cache is poisoned".to_string())?
            .remove(&container_id);
        Ok(())
    }
}

fn evidence_write_error(stage: &str, message: impl Into<String>) -> Value {
    json!({
        "stage": stage,
        "message": message.into(),
    })
}

impl ToolProcessHandler for ContainerRoom {
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
        kind: &'a WorkerEnvKindSpec,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let config = ContainerConfig::load_formed(&self.environment, formation, kind)
                .map_err(ToolProcessError::Capability)?;
            self.release_container(formation, &config)
                .await
                .map_err(ToolProcessError::Unavailable)
        })
    }

}

pub async fn run_container_room_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    let room = Arc::new(ContainerRoom::new(environment));
    octopus_tool_door_runtime::run_tool_process(room).await
}

fn validate_network_grant(
    formation: &WorkerEnvironmentFormation,
    config: &ContainerConfig,
) -> Result<(), ToolProcessError> {
    match config.network {
        ContainerNetwork::None => {}
        ContainerNetwork::Bridge | ContainerNetwork::Custom
            if formation.network_ceiling == NetworkCeiling::Denied =>
        {
            return Err(ToolProcessError::Capability(
                "container config requests network access but formed policy denies network"
                    .to_string(),
            ));
        }
        ContainerNetwork::Host if formation.network_ceiling != NetworkCeiling::Full => {
            return Err(ToolProcessError::Capability(
                "container host networking requires a full formed network grant".to_string(),
            ));
        }
        _ => {}
    }
    if !config.publish_ports.is_empty() && formation.network_ceiling != NetworkCeiling::Full {
        return Err(ToolProcessError::Capability(
            "container published ports require a full formed network grant".to_string(),
        ));
    }
    Ok(())
}

fn validate_mount_grants(
    formation: &WorkerEnvironmentFormation,
    config: &ContainerConfig,
) -> Result<(), ToolProcessError> {
    let readable = formation
        .readable_root_slots
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let writable = formation
        .writable_root_slots
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for mount in &config.mounts {
        let allowed = match mount.access {
            MountAccess::ReadOnly => {
                readable.contains(mount.root_slot.as_str())
                    || writable.contains(mount.root_slot.as_str())
            }
            MountAccess::ReadWrite => writable.contains(mount.root_slot.as_str()),
        };
        if !allowed {
            return Err(ToolProcessError::Capability(format!(
                "container mount root slot '{}' is outside the formed {:?} grant",
                mount.root_slot, mount.access
            )));
        }
        if !formation.door_root_bindings.contains_key(&mount.root_slot) {
            return Err(ToolProcessError::Capability(format!(
                "container mount root slot '{}' has no physical formed binding",
                mount.root_slot
            )));
        }
    }
    Ok(())
}

fn validate_runtime_owner(
    metadata: &InvocationMetadata,
    formation: &WorkerEnvironmentFormation,
) -> Result<(), ToolProcessError> {
    let runtime = metadata.runtime.as_ref().ok_or_else(|| {
        ToolProcessError::Capability(
            "container invocation metadata has no runtime coordinates".to_string(),
        )
    })?;
    if runtime.worker != formation.worker || runtime.interface != formation.door_interface {
        return Err(ToolProcessError::Capability(
            "container formation does not match invocation worker and door".to_string(),
        ));
    }
    Ok(())
}

fn materialize_binary_carrier(
    emission_root: &Path,
    label: &str,
    bytes: &[u8],
) -> Result<Value, String> {
    let artifact = InterfaceArtifactFile::write(emission_root, label, bytes)
        .map_err(|error| format!("container binary carrier failed to materialize: {error}"))?;
    let manifest = artifact
        .read_manifest()
        .map_err(|error| format!("container binary carrier manifest failed to read: {error}"))?;
    Ok(json!({
        "label": manifest.label,
        "manifest_path": artifact.manifest_path(),
        "payload_path": manifest.payload_path,
        "payload_content_hash": manifest.payload_content_hash,
        "bytes": bytes.len(),
    }))
}

fn text_preview(bytes: &[u8], maximum: usize) -> Value {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Value::Null;
    };
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    json!({
        "text": &text[..end],
        "truncated": end < text.len(),
    })
}

fn container_name(formation: &WorkerEnvironmentFormation, config: &ContainerConfig) -> String {
    let digest = blake3::hash(formation.worker_environment_id.as_str().as_bytes())
        .to_hex()
        .to_string();
    format!("{}-{}", config.name_prefix, &digest[..24])
}

fn creation_attempt(formation: &WorkerEnvironmentFormation, sequence: u64) -> String {
    let input = format!(
        "{}\0{}\0{}\0{}",
        formation.worker_environment_id,
        formation.formation_pin,
        std::process::id(),
        sequence
    );
    blake3::hash(input.as_bytes()).to_hex()[..24].to_string()
}

fn viewer_address(config: &ContainerConfig, name: &str) -> String {
    let engine = Path::new(&config.engine)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("container");
    format!("container://{engine}/{name}")
}

fn resolved_environment(config: &ContainerConfig) -> Result<BTreeMap<String, String>, String> {
    let mut values = config.env.clone();
    for name in &config.forward_env {
        let value = std::env::var(name)
            .map_err(|_| format!("container forwarded environment '{name}' is unavailable"))?;
        values.insert(name.clone(), value);
    }
    Ok(values)
}

fn parse_inspection(bytes: &[u8]) -> Result<ContainerInspection, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("container inspect output is malformed JSON: {error}"))?;
    let items = value
        .as_array()
        .ok_or_else(|| "container inspect output must be an array".to_string())?;
    if items.len() != 1 {
        return Err(format!(
            "container inspect output must contain exactly one item, got {}",
            items.len()
        ));
    }
    let item = items[0]
        .as_object()
        .ok_or_else(|| "container inspect item must be an object".to_string())?;
    let id = json_string(item, "Id")?.to_string();
    let image_id = json_string(item, "Image")?.to_string();
    let created_at = json_string(item, "Created")?.to_string();
    let name = json_string(item, "Name")?
        .trim_start_matches('/')
        .to_string();
    let config = item
        .get("Config")
        .and_then(Value::as_object)
        .ok_or_else(|| "container inspect item has no Config object".to_string())?;
    let image = json_string(config, "Image")?.to_string();
    let labels = match config.get("Labels") {
        Some(Value::Object(values)) => values
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_string()))
                    .ok_or_else(|| format!("container inspect label '{key}' is not a string"))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?,
        Some(Value::Null) | None => BTreeMap::new(),
        Some(_) => return Err("container inspect Config.Labels must be an object".to_string()),
    };
    let mounts = item
        .get("Mounts")
        .and_then(Value::as_array)
        .ok_or_else(|| "container inspect item has no Mounts array".to_string())?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let mount = value
                .as_object()
                .ok_or_else(|| format!("container inspect Mounts[{index}] must be an object"))?;
            Ok(ContainerMountInspection {
                mount_type: json_string(mount, "Type")?.to_string(),
                source: json_string(mount, "Source")?.to_string(),
                destination: json_string(mount, "Destination")?.to_string(),
                read_write: mount.get("RW").and_then(Value::as_bool).ok_or_else(|| {
                    format!("container inspect Mounts[{index}].RW must be a boolean")
                })?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let state = item
        .get("State")
        .and_then(Value::as_object)
        .ok_or_else(|| "container inspect item has no State object".to_string())?;
    let status = json_string(state, "Status")?.to_string();
    let running = state
        .get("Running")
        .and_then(Value::as_bool)
        .ok_or_else(|| "container inspect State.Running must be a boolean".to_string())?;
    let paused = state
        .get("Paused")
        .and_then(Value::as_bool)
        .ok_or_else(|| "container inspect State.Paused must be a boolean".to_string())?;
    let restarting = state
        .get("Restarting")
        .and_then(Value::as_bool)
        .ok_or_else(|| "container inspect State.Restarting must be a boolean".to_string())?;
    let oom_killed = state
        .get("OOMKilled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "container inspect State.OOMKilled must be a boolean".to_string())?;
    let dead = state
        .get("Dead")
        .and_then(Value::as_bool)
        .ok_or_else(|| "container inspect State.Dead must be a boolean".to_string())?;
    let exit_code = state.get("ExitCode").and_then(Value::as_i64);
    let state_error = state
        .get("Error")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let started_at = state
        .get("StartedAt")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && !value.starts_with("0001-01-01"))
        .map(str::to_string);
    let finished_at = state
        .get("FinishedAt")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && !value.starts_with("0001-01-01"))
        .map(str::to_string);
    let restart_count = item
        .get("RestartCount")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            "container inspect RestartCount must be a non-negative integer".to_string()
        })?;
    let health = state
        .get("Health")
        .and_then(Value::as_object)
        .and_then(|health| health.get("Status"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(ContainerInspection {
        id,
        name,
        image,
        image_id,
        created_at,
        status,
        running,
        paused,
        restarting,
        oom_killed,
        dead,
        exit_code,
        state_error,
        started_at,
        finished_at,
        restart_count,
        health,
        labels,
        mounts,
    })
}

fn validate_ownership(
    formation: &WorkerEnvironmentFormation,
    config: &ContainerConfig,
    inspection: &ContainerInspection,
) -> Result<(), String> {
    let required = [
        ("octopus.managed", "1"),
        (
            "octopus.worker_environment_id",
            formation.worker_environment_id.as_str(),
        ),
        ("octopus.formation_pin", formation.formation_pin.as_str()),
        ("octopus.interface", formation.door_interface.as_str()),
    ];
    for (key, expected) in required {
        let observed = inspection.labels.get(key).map(String::as_str);
        if observed != Some(expected) {
            return Err(format!(
                "container name '{}' is occupied by an unattested object: label '{}' expected '{}', observed {:?}",
                inspection.name, key, expected, observed
            ));
        }
    }
    if inspection.image != config.image {
        return Err(format!(
            "container '{}' image drifted: expected '{}', observed '{}'",
            inspection.name, config.image, inspection.image
        ));
    }
    Ok(())
}

fn status_body(
    formation: &WorkerEnvironmentFormation,
    config: &ContainerConfig,
    inspection: Option<&ContainerInspection>,
    lifecycle_action: &str,
) -> Value {
    let expected_name = container_name(formation, config);
    let viewer_name = inspection
        .map(|item| item.name.clone())
        .unwrap_or_else(|| expected_name.clone());
    json!({
        "worker_environment_id": formation.worker_environment_id,
        "worker_environment_kind": formation.worker_environment_kind,
        "exists": inspection.is_some(),
        "lifecycle_action": lifecycle_action,
        "expected_container_name": expected_name,
        "name_matches_expected": inspection.map(|item| item.name == expected_name),
        "container": inspection.map(inspection_body),
        "image": config.image,
        "workdir": config.workdir,
        "viewer": {
            "protocol": "container_runtime",
            "address": viewer_address(config, &viewer_name),
            "container_name": viewer_name,
        }
    })
}

fn inspection_body(inspection: &ContainerInspection) -> Value {
    json!({
        "id": inspection.id,
        "name": inspection.name,
        "image": inspection.image,
        "image_id": inspection.image_id,
        "created_at": inspection.created_at,
        "status": inspection.status,
        "running": inspection.running,
        "paused": inspection.paused,
        "restarting": inspection.restarting,
        "oom_killed": inspection.oom_killed,
        "dead": inspection.dead,
        "exit_code": inspection.exit_code,
        "state_error": inspection.state_error,
        "started_at": inspection.started_at,
        "finished_at": inspection.finished_at,
        "restart_count": inspection.restart_count,
        "health": inspection.health,
    })
}

fn command_body(
    emission_root: &Path,
    operation: &str,
    outcome: &CommandOutcome,
    preview_limit: usize,
) -> Result<Value, String> {
    let stdout = stream_body(
        emission_root,
        &format!("{operation}.stdout"),
        &outcome.stdout,
        preview_limit,
    )?;
    let stderr = stream_body(
        emission_root,
        &format!("{operation}.stderr"),
        &outcome.stderr,
        preview_limit,
    )?;
    Ok(json!({
        "exit_code": outcome.status.and_then(|status| status.code()),
        "exit_signal": exit_signal(outcome.status),
        "success": outcome.status.is_some_and(|status| status.success()) && !outcome.timed_out,
        "timed_out": outcome.timed_out,
        "timeout_ms": outcome.timeout_ms,
        "duration_ms": outcome.duration_ms,
        "stdout": stdout,
        "stderr": stderr,
    }))
}

fn stream_body(
    emission_root: &Path,
    label: &str,
    stream: &BoundedBytes,
    preview_limit: usize,
) -> Result<Value, String> {
    let carrier = materialize_binary_carrier(emission_root, label, &stream.bytes)?;
    Ok(json!({
        "bytes": stream.bytes.len(),
        "truncated": stream.truncated,
        "text_preview": text_preview(&stream.bytes, preview_limit),
        "carrier": carrier,
        "content_hash": blake3::hash(&stream.bytes).to_hex().to_string(),
    }))
}

fn parse_directory_entries(bytes: &[u8], maximum: usize) -> Result<Vec<Value>, String> {
    let fields = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let fields = if fields.last().is_some_and(|field| field.is_empty()) {
        &fields[..fields.len().saturating_sub(1)]
    } else {
        fields.as_slice()
    };
    if fields.len() % 2 != 0 {
        return Err("container.file_list returned a torn entry record".to_string());
    }
    let mut entries = Vec::new();
    for pair in fields.chunks_exact(2) {
        let kind = std::str::from_utf8(pair[0])
            .map_err(|_| "container.file_list returned a non-UTF8 entry kind".to_string())?;
        if kind == "overflow" {
            return Err(format!(
                "container directory exceeds the admitted {maximum} entry limit"
            ));
        }
        if !matches!(kind, "file" | "directory" | "symlink" | "other") {
            return Err(format!(
                "container.file_list returned unknown kind '{kind}'"
            ));
        }
        if entries.len() >= maximum {
            return Err(format!(
                "container directory exceeds the admitted {maximum} entry limit"
            ));
        }
        let name_utf8 = std::str::from_utf8(pair[1]).ok().map(str::to_string);
        entries.push(json!({
            "name": name_utf8,
            "name_base64": BASE64_STANDARD.encode(pair[1]),
            "entry_type": kind,
        }));
    }
    Ok(entries)
}

fn parse_identifier_lines(bytes: &[u8]) -> Result<Vec<String>, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "container identity listing returned non-UTF8 output".to_string())?;
    let mut identifiers = Vec::new();
    let mut unique = BTreeSet::new();
    for value in text
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err("container identity listing returned an invalid identifier".to_string());
        }
        if unique.insert(value.to_string()) {
            identifiers.push(value.to_string());
        }
    }
    Ok(identifiers)
}

#[cfg(unix)]
fn exit_signal(status: Option<ExitStatus>) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.and_then(|value| value.signal())
}

#[cfg(not(unix))]
fn exit_signal(_status: Option<ExitStatus>) -> Option<i32> {
    None
}

async fn run_command(
    program: &str,
    args: &[String],
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<CommandOutcome, String> {
    let started = Instant::now();
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let deadline = started + timeout;
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        format!(
            "container engine '{}' failed to spawn for {:?}: {error}",
            program, args
        )
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "container engine stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "container engine stderr is unavailable".to_string())?;
    let stdout_task = tokio::spawn(read_bounded(stdout, max_output_bytes));
    let stderr_task = tokio::spawn(read_bounded(stderr, max_output_bytes));
    if let Some(bytes) = stdin {
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| "container engine stdin is unavailable".to_string())?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("container engine stdin write timed out".to_string());
        }
        let write = tokio::time::timeout(remaining, async {
            input.write_all(&bytes).await?;
            input.shutdown().await
        })
        .await;
        match write {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(format!("container engine stdin write failed: {error}"));
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err("container engine stdin write timed out".to_string());
            }
        }
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let waited = tokio::time::timeout(remaining, child.wait()).await;
    let (status, timed_out) = match waited {
        Ok(result) => (
            Some(result.map_err(|error| format!("container engine wait failed: {error}"))?),
            false,
        ),
        Err(_) => {
            child.kill().await.map_err(|error| {
                format!("timed-out container engine process could not be killed: {error}")
            })?;
            let status = child.wait().await.map_err(|error| {
                format!("killed container engine process could not be reaped: {error}")
            })?;
            (Some(status), true)
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|error| format!("container stdout reader failed to join: {error}"))??;
    let stderr = stderr_task
        .await
        .map_err(|error| format!("container stderr reader failed to join: {error}"))??;
    Ok(CommandOutcome {
        status,
        stdout,
        stderr,
        timed_out,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        timeout_ms,
    })
}

async fn read_bounded<R>(mut reader: R, maximum: usize) -> Result<BoundedBytes, String>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(maximum.min(64 * 1024));
    let mut truncated = false;
    let mut chunk = vec![0_u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .await
            .map_err(|error| format!("container output read failed: {error}"))?;
        if read == 0 {
            break;
        }
        let remaining = maximum.saturating_sub(bytes.len());
        let take = read.min(remaining);
        bytes.extend_from_slice(&chunk[..take]);
        if take < read {
            truncated = true;
        }
    }
    Ok(BoundedBytes { bytes, truncated })
}

fn require_success(operation: &str, outcome: &CommandOutcome) -> Result<(), String> {
    if outcome.timed_out {
        return Err(format!("{operation} timed out"));
    }
    if outcome.stdout.truncated || outcome.stderr.truncated {
        return Err(format!("{operation} output exceeded its configured bound"));
    }
    if outcome.status.is_some_and(|status| status.success()) {
        Ok(())
    } else {
        Err(format!("{operation} failed: {}", outcome_error(outcome)))
    }
}

fn require_exit_success(operation: &str, outcome: &CommandOutcome) -> Result<(), String> {
    if outcome.timed_out {
        return Err(format!("{operation} timed out"));
    }
    if outcome.status.is_some_and(|status| status.success()) {
        Ok(())
    } else {
        Err(format!("{operation} failed: {}", outcome_error(outcome)))
    }
}

fn outcome_error(outcome: &CommandOutcome) -> String {
    let exit = outcome
        .status
        .and_then(|status| status.code())
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    let stderr = String::from_utf8_lossy(&outcome.stderr.bytes);
    let stdout = String::from_utf8_lossy(&outcome.stdout.bytes);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    format!("exit={exit}; {}", truncate_detail(detail, 4096))
}

fn missing_container(outcome: &CommandOutcome) -> bool {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&outcome.stdout.bytes),
        String::from_utf8_lossy(&outcome.stderr.bytes)
    )
    .to_ascii_lowercase();
    text.contains("no such object")
        || text.contains("no such container")
        || text.contains("does not exist")
}

fn missing_image(outcome: &CommandOutcome) -> bool {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&outcome.stdout.bytes),
        String::from_utf8_lossy(&outcome.stderr.bytes)
    )
    .to_ascii_lowercase();
    text.contains("no such image") || text.contains("image not known")
}

fn truncate_detail(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.to_string();
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    object
        .get(field)
        .ok_or_else(|| format!("{field} is required"))
        .and_then(|value| required_string_value(value, field))
}

fn required_string_value<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .ok_or_else(|| format!("{field} must be a non-empty string without null"))
}

fn required_string_array(
    object: &Map<String, Value>,
    field: &str,
    minimum: usize,
    maximum: usize,
) -> Result<Vec<String>, String> {
    let array = object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{field} must be an array"))?;
    if !(minimum..=maximum).contains(&array.len()) {
        return Err(format!(
            "{field} must contain between {minimum} and {maximum} entries"
        ));
    }
    array
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let value = required_string_value(value, &format!("{field}[{index}]"))?;
            if value.len() > 65_536 {
                return Err(format!("{field}[{index}] exceeds 65536 bytes"));
            }
            Ok(value.to_string())
        })
        .collect()
}

fn optional_string_map(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<BTreeMap<String, String>>, String> {
    object
        .get(field)
        .map(|value| {
            let values = value
                .as_object()
                .ok_or_else(|| format!("{field} must be an object"))?;
            if values.len() > 256 {
                return Err(format!("{field} exceeds 256 entries"));
            }
            values
                .iter()
                .map(|(name, value)| {
                    validate_env_name(name).map_err(|error| format!("{field}.{name} {error}"))?;
                    let value = value
                        .as_str()
                        .ok_or_else(|| format!("{field}.{name} must be a string"))?;
                    if value.len() > 1024 * 1024 || value.contains('\0') {
                        return Err(format!("{field}.{name} is too large or contains null"));
                    }
                    Ok((name.clone(), value.to_string()))
                })
                .collect()
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

fn optional_usize(object: &Map<String, Value>, field: &str) -> Result<Option<usize>, String> {
    optional_u64(object, field)?
        .map(|value| usize::try_from(value).map_err(|_| format!("{field} exceeds usize")))
        .transpose()
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

fn exclusive_bytes(
    object: &Map<String, Value>,
    text_field: &str,
    base64_field: &str,
    maximum: usize,
) -> Result<Option<Vec<u8>>, String> {
    match (object.get(text_field), object.get(base64_field)) {
        (Some(_), Some(_)) => Err(format!(
            "{text_field} and {base64_field} are mutually exclusive"
        )),
        (Some(value), None) => {
            let text = value
                .as_str()
                .ok_or_else(|| format!("{text_field} must be a string"))?;
            if text.len() > maximum {
                return Err(format!("{text_field} exceeds {maximum} bytes"));
            }
            Ok(Some(text.as_bytes().to_vec()))
        }
        (None, Some(value)) => {
            let encoded = value
                .as_str()
                .ok_or_else(|| format!("{base64_field} must be a string"))?;
            let bytes = BASE64_STANDARD
                .decode(encoded)
                .map_err(|error| format!("{base64_field} is invalid base64: {error}"))?;
            if bytes.len() > maximum {
                return Err(format!("{base64_field} decodes beyond {maximum} bytes"));
            }
            Ok(Some(bytes))
        }
        (None, None) => Ok(None),
    }
}

fn exclusive_required_bytes(
    object: &Map<String, Value>,
    text_field: &str,
    base64_field: &str,
    maximum: usize,
) -> Result<Vec<u8>, String> {
    exclusive_bytes(object, text_field, base64_field, maximum)?
        .ok_or_else(|| format!("exactly one of {text_field} or {base64_field} is required"))
}

fn post_operation_lifecycle(
    tool: &str,
    operation_succeeded: bool,
    substrate_exists: Result<bool, ()>,
) -> WorkerEnvironmentLifecycle {
    match substrate_exists {
        Ok(true) => WorkerEnvironmentLifecycle::Active,
        Ok(false) if operation_succeeded && tool == "remove" => {
            WorkerEnvironmentLifecycle::Released
        }
        Ok(false) | Err(()) => WorkerEnvironmentLifecycle::Failed,
    }
}

fn json_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("container inspect field '{field}' must be a non-empty string"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    use octopus_descriptors::{InterfaceId, WorkerEnvironmentKindId};

    #[cfg(unix)]
    use octopus_worker_env::WorkerEnvironmentId;

    #[test]
    fn released_environment_has_no_live_projection() {
        assert!(lifecycle_has_live_projection(
            WorkerEnvironmentLifecycle::Active
        ));
        assert!(lifecycle_has_live_projection(
            WorkerEnvironmentLifecycle::Failed
        ));
        assert!(!lifecycle_has_live_projection(
            WorkerEnvironmentLifecycle::Released
        ));
    }

    #[test]
    fn stopped_container_remains_a_live_formed_environment() {
        assert_eq!(
            post_operation_lifecycle("stop", true, Ok(true)),
            WorkerEnvironmentLifecycle::Active
        );
        assert_eq!(
            post_operation_lifecycle("attach", true, Ok(true)),
            WorkerEnvironmentLifecycle::Active
        );
        assert_eq!(
            post_operation_lifecycle("remove", true, Ok(false)),
            WorkerEnvironmentLifecycle::Released
        );
        assert_eq!(
            post_operation_lifecycle("status", false, Ok(false)),
            WorkerEnvironmentLifecycle::Failed
        );
    }

    #[test]
    fn parses_binary_directory_records_without_loss() {
        let entries = parse_directory_entries(b"file\0alpha\0directory\0beta\0", 2).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"], "alpha");
        assert!(parse_directory_entries(b"overflow\0\0", 2).is_err());
    }

    #[test]
    fn binary_carrier_keeps_bytes_out_of_json() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"\0\xffcontainer-bytes";
        let carrier =
            materialize_binary_carrier(root.path(), "container.test.bytes", bytes).unwrap();
        assert_eq!(carrier["bytes"], bytes.len());
        assert_eq!(
            std::fs::read(carrier["payload_path"].as_str().unwrap()).unwrap(),
            bytes
        );
        assert!(text_preview(bytes, 1024).is_null());
    }

    #[test]
    fn inspect_requires_one_typed_container() {
        let parsed = parse_inspection(
            br#"[{"Id":"abc","Image":"sha256:image","Created":"2026-01-01T00:00:00Z","Name":"/octopus-x","RestartCount":0,"Config":{"Image":"ubuntu:24.04","Labels":{"octopus.managed":"1"}},"Mounts":[],"State":{"Status":"running","Running":true,"Paused":false,"Restarting":false,"OOMKilled":false,"Dead":false,"ExitCode":0,"Error":"","StartedAt":"2026-01-01T00:00:01Z","FinishedAt":"0001-01-01T00:00:00Z"}}]"#,
        )
        .unwrap();
        assert_eq!(parsed.name, "octopus-x");
        assert!(parsed.running);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exact_formation_owns_start_exec_stop_and_remove() {
        let root = tempfile::tempdir().unwrap();
        let engine = root.path().join("fake-container-engine.py");
        std::fs::write(
            &engine,
            r#"#!/usr/bin/env python3
import json
import pathlib
import sys

state_path = pathlib.Path(__file__).with_suffix(".state.json")
args = sys.argv[1:]

def load():
    if not state_path.exists():
        return None
    return json.loads(state_path.read_text())

def save(value):
    state_path.write_text(json.dumps(value))

if args[0] == "version":
    print("1.0")
elif args[0] == "ps":
    state = load()
    if state is not None:
        print(state["Id"])
elif args[0:2] == ["image", "inspect"]:
    sys.stderr.write("No such image\n")
    sys.exit(1)
elif args[0] == "pull":
    print(args[1])
elif args[0] == "inspect":
    state = load()
    if state is None:
        sys.stderr.write("No such object\n")
        sys.exit(1)
    print(json.dumps([state]))
elif args[0] == "run":
    name = args[args.index("--name") + 1]
    labels = {}
    mounts = []
    for index, value in enumerate(args):
        if value == "--label":
            key, item = args[index + 1].split("=", 1)
            labels[key] = item
        if value == "--mount":
            fields = args[index + 1].split(",")
            values = dict(field.split("=", 1) for field in fields if "=" in field)
            mounts.append({"Type": values["type"], "Source": values["src"], "Destination": values["dst"], "RW": "readonly" not in fields})
    state = {
        "Id": "fake-container-id",
        "Image": "sha256:fake-image",
        "Created": "2026-01-01T00:00:00Z",
        "Name": "/" + name,
        "RestartCount": 0,
        "Config": {"Image": "ubuntu:24.04", "Labels": labels},
        "Mounts": mounts,
        "State": {"Status": "running", "Running": True, "Paused": False, "Restarting": False, "OOMKilled": False, "Dead": False, "ExitCode": 0, "Error": "", "StartedAt": "2026-01-01T00:00:01Z", "FinishedAt": "0001-01-01T00:00:00Z"},
    }
    save(state)
    print(state["Id"])
elif args[0] == "start":
    state = load()
    state["State"] = {"Status": "running", "Running": True, "Paused": False, "Restarting": False, "OOMKilled": False, "Dead": False, "ExitCode": 0, "Error": "", "StartedAt": "2026-01-01T00:00:01Z", "FinishedAt": "0001-01-01T00:00:00Z"}
    save(state)
    print(state["Id"])
elif args[0] == "exec":
    sys.stdin.buffer.read()
    if "/bin/uname" in args:
        print("aarch64")
    elif "version" in args:
        print("octopus.container.rooted-fs.v1 aarch64")
    else:
        sys.stdout.buffer.write(b"fake-output")
elif args[0] == "logs":
    print("fake-log")
elif args[0] == "stop":
    state = load()
    state["State"] = {"Status": "exited", "Running": False, "Paused": False, "Restarting": False, "OOMKilled": False, "Dead": False, "ExitCode": 0, "Error": "", "StartedAt": "2026-01-01T00:00:01Z", "FinishedAt": "2026-01-01T00:01:00Z"}
    save(state)
    print(state["Id"])
elif args[0] == "rm":
    state = load()
    if state_path.exists():
        state_path.unlink()
    if state is not None:
        print(state["Id"])
else:
    sys.stderr.write("unsupported " + repr(args) + "\n")
    sys.exit(2)
"#,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&engine).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&engine, permissions).unwrap();
        let config: ContainerConfig = serde_json::from_value(json!({
            "schema_version": 2,
            "engine": engine,
            "image": "ubuntu:24.04",
            "pull_policy": "if_missing",
            "name_prefix": "octopus",
            "workdir": "/workspace",
            "start_argv": ["sleep", "infinity"],
            "network": "bridge",
            "network_name": null,
            "publish_ports": [],
            "dns": [],
            "hostname": null,
            "init": true,
            "read_only_root": false,
            "user": null,
            "cpu_limit": 2.0,
            "memory_bytes": 4294967296_u64,
            "storage_bytes": null,
            "shm_bytes": 67108864,
            "pids_limit": 512,
            "startup_timeout_ms": 30000,
            "ready_condition": "running",
            "ready_timeout_ms": 30000,
            "ready_poll_ms": 50,
            "exec_timeout_ms": 30000,
            "max_output_bytes": 1048576,
            "max_file_bytes": 1048576,
            "max_directory_entries": 128,
            "inline_text_preview_bytes": 65536,
            "reuse_stopped": true,
            "recovery_policy": "restart_or_recreate",
            "env": {},
            "forward_env": [],
            "labels": {},
            "mounts": [{
                "root_slot": "worktree_root",
                "target": "/workspace",
                "access": "read_write",
                "source_type": "directory"
            }],
            "readable_paths": ["/workspace", "/tmp"],
            "writable_paths": ["/workspace", "/tmp"]
        }))
        .unwrap();
        config.validate().unwrap();
        let instance_root_base = root.path().join("rooms");
        std::fs::create_dir_all(&instance_root_base).unwrap();
        let formation = WorkerEnvironmentFormation {
            door_additional_root_bindings: Default::default(),
            worker_environment_id: WorkerEnvironmentId::derive(b"container-test"),
            run: "run_container".to_string(),
            ee: "ee_container".to_string(),
            worker: InterfaceId::new("system_worker_coder_max"),
            door_interface: InterfaceId::new("system_container"),
            door_descriptor_hash: "door-hash".to_string(),
            worker_environment_kind: WorkerEnvironmentKindId::new("container_session"),
            worker_environment_kind_descriptor_hash: "11".repeat(32),
            tool_descriptor_hashes: BTreeMap::new(),
            instance_config_hashes: BTreeMap::new(),
            formation_pin: "formation-pin-test".to_string(),
            granted_tools: vec!["container.start".to_string()],
            door_root_bindings: BTreeMap::from([(
                "worktree_root".to_string(),
                instance_root_base.clone(),
            )]),
            readable_root_slots: vec!["worktree_root".to_string()],
            writable_root_slots: vec!["worktree_root".to_string()],
            network_ceiling: NetworkCeiling::Restricted,
            requires_release: true,
        };
        let var_dir = root.path().join("var");
        std::fs::create_dir_all(&var_dir).unwrap();
        let room = ContainerRoom::new(ToolProcessEnvironment {
            interface: formation.door_interface.clone(),
            interface_dir: root.path().to_path_buf(),
            descriptor_snapshot: None,
            var_dir,
            durable_var_dir: None,
            private_reads: BTreeMap::new(),
            root_slots: BTreeMap::new(),
            host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
                max_control_frame_bytes: 1024 * 1024,
                max_stderr_bytes: 256 * 1024,
                max_manifest_bytes: 1024 * 1024,
                max_payload_bytes: 64 * 1024 * 1024,
            },
        });
        let kind: WorkerEnvKindSpec =
            serde_json::from_slice(include_bytes!("../../../worker_environment_kind.json"))
                .unwrap();
        let root_bindings = project_container_instance_root_bindings(&formation).unwrap();

        let started = room
            .start(&formation, &kind, &root_bindings, &config, &json!({}))
            .await
            .unwrap();
        assert_eq!(started["exists"], true);
        assert_eq!(started["container"]["running"], true);
        let expected_instance_root =
            instance_root_base.join(formation.worker_environment_id.as_str());
        let inspection = room
            .inspect_owned(&formation, &config)
            .await
            .unwrap()
            .unwrap();
        let worktree_mount = inspection
            .mounts
            .iter()
            .find(|mount| mount.destination == "/workspace")
            .unwrap();
        assert_eq!(
            worktree_mount.source,
            expected_instance_root
                .canonicalize()
                .unwrap()
                .to_string_lossy()
        );

        let executed = room
            .exec(
                &formation,
                &kind,
                &root_bindings,
                &config,
                &json!({"argv": ["printf", "fake-output"]}),
                &root.path().join("emitted"),
            )
            .await
            .unwrap();
        assert_eq!(executed["success"], true);
        assert_eq!(executed["stdout"]["text_preview"]["text"], "fake-output");
        assert!(executed["stdout"]["carrier"]["manifest_path"].is_string());

        let stopped = room
            .stop(&formation, &config, &json!({"timeout_seconds": 0}))
            .await
            .unwrap();
        assert_eq!(stopped["container"]["status"], "exited");

        let attached_stopped = room
            .attach(&formation, &kind, &root_bindings, &config, &json!({}))
            .await
            .unwrap();
        assert_eq!(attached_stopped["lifecycle_action"], "attached_stopped");
        let restarted = room
            .attach(
                &formation,
                &kind,
                &root_bindings,
                &config,
                &json!({"start_if_stopped": true}),
            )
            .await
            .unwrap();
        assert_eq!(restarted["lifecycle_action"], "attached_and_restarted");
        let removed = room.remove(&formation, &config, &json!({})).await.unwrap();
        assert_eq!(removed["exists"], false);
    }
}
