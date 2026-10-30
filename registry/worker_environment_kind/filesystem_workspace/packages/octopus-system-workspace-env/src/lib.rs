use octopus_descriptors::ProcessSandboxMode;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceBoundaryError, InterfaceInvocationEnvelope,
};
use octopus_tool_door_runtime::room::{admitted_tool, decode_tool_call, emit_json_artifact, invocation_scope};
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo,
};
use octopus_tool_handlers::{
    ToolCall, ToolHandler, ToolHandlerContext, ToolHandlerError, ToolHandlerResult,
    ToolResultArtifact, ToolResultError,
};
use octopus_tool_registry::ToolDescriptor;
use octopus_worker_env::{
    AbsPath, EnvContext, ProjectedSandbox, SandboxedCommandRunner, WorkerEnvDriver,
    WorkerEnvError, WorkerEnvKindSpec, WorkerEnvironment, WorkerEnvironmentFormation,
    WorkerEnvironmentId, WorkerEnvironmentLifecycle,
};
use octopus_workspace_substrate::{
    FAMILY_PROCESS_EXEC, WORKSPACE_ROOT_SLOT, WorkspaceProcessManager, WorkspaceProcessStore,
};

mod workspace_exec;
mod workspace_fs;
#[cfg(test)]
mod tests;
pub use workspace_exec::WorkspaceExecHandler;
pub use workspace_fs::WorkspaceFsHandler;

const INTERFACE_LOCAL_IPC_ROOT_SLOT: &str = "interface_local_ipc_root";

pub trait WorkspaceLayout: Send + Sync + 'static {
    fn room(&self) -> &'static str;

    fn room_root(&self, formation: &WorkerEnvironmentFormation) -> Result<PathBuf, String>;

    fn lease_parts(
        &self,
        kind: &WorkerEnvKindSpec,
    ) -> Result<(BTreeMap<String, AbsPath>, Arc<dyn WorkerEnvDriver>), WorkerEnvError>;

    fn release_unleased(&self, formation: &WorkerEnvironmentFormation) -> Result<(), String>;

    fn operation(&self, call: LayoutCall<'_>) -> Result<Option<LayoutOperation>, String>;
}

pub struct LayoutCall<'a> {
    pub tool_id: &'a str,
    pub handler_config: &'a serde_json::Value,
    pub formation: &'a WorkerEnvironmentFormation,
    pub room_root: &'a Path,
}

pub type LayoutRun = Box<dyn FnOnce() -> Result<ToolHandlerResult, String> + Send>;

pub enum LayoutOperation {
    InRoom(LayoutRun),
    EndsRoom {
        cleanup_timeout: Duration,
        run: LayoutRun,
    },
}

pub struct DirectWorkspace {
    workspace_root: PathBuf,
}

impl DirectWorkspace {
    pub fn new(workspace_root: PathBuf) -> Result<Self, String> {
        let workspace_root = AbsPath::new(&workspace_root)
            .map_err(|error| format!("system_workspace_env root failed closed: {error}"))?
            .as_path()
            .to_path_buf();
        Ok(Self { workspace_root })
    }

    pub fn from_process_environment(env: &ToolProcessEnvironment) -> Result<Self, String> {
        Self::new(env.root_slot(WORKSPACE_ROOT_SLOT)?.to_path_buf())
    }
}

impl WorkspaceLayout for DirectWorkspace {
    fn room(&self) -> &'static str {
        "system_workspace_env"
    }

    fn room_root(&self, _formation: &WorkerEnvironmentFormation) -> Result<PathBuf, String> {
        Ok(self.workspace_root.clone())
    }

    fn lease_parts(
        &self,
        _kind: &WorkerEnvKindSpec,
    ) -> Result<(BTreeMap<String, AbsPath>, Arc<dyn WorkerEnvDriver>), WorkerEnvError> {
        Ok((
            BTreeMap::from([(
                WORKSPACE_ROOT_SLOT.to_string(),
                AbsPath::new(&self.workspace_root)?,
            )]),
            octopus_workspace_substrate::direct_workspace_driver(),
        ))
    }

    fn release_unleased(&self, _formation: &WorkerEnvironmentFormation) -> Result<(), String> {
        Ok(())
    }

    fn operation(&self, _call: LayoutCall<'_>) -> Result<Option<LayoutOperation>, String> {
        Ok(None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOperation {
    WriteFile,
    ApplyPatch,
    ReadFile,
    ListDir,
    Grep,
    RunCommand,
    StartCommand,
    WriteStdin,
    ProcessSignal,
    ProcessTerminate,
    ProcessList,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceHandlerConfig {
    pub operation: WorkspaceOperation,
    #[serde(default)]
    pub(crate) default_timeout_ms: Option<u64>,
    #[serde(default)]
    pub(crate) max_timeout_ms: Option<u64>,
    #[serde(default)]
    pub(crate) default_max_output_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) max_output_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) default_yield_time_ms: Option<u64>,
    #[serde(default)]
    pub(crate) max_yield_time_ms: Option<u64>,
    #[serde(default)]
    pub(crate) max_buffered_output_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) max_buffered_output_events: Option<usize>,
    #[serde(default)]
    pub(crate) max_processes_per_worker_environment: Option<usize>,
    #[serde(default)]
    pub(crate) max_input_bytes: Option<usize>,
    #[serde(default)]
    pub(crate) cleanup_timeout_ms: Option<u64>,
    #[serde(default)]
    pub(crate) default_shell: Option<String>,
    #[serde(default)]
    pub(crate) allowed_shells: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) allow_login_shell: Option<bool>,
    #[serde(default)]
    pub(crate) max_scanned_files: Option<usize>,
    #[serde(default)]
    pub(crate) max_scanned_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) deadline_ms: Option<u64>,
    #[serde(default)]
    pub(crate) max_line_bytes: Option<usize>,
}

#[derive(Clone, Debug)]
struct RoomInvocationScope {
    run: String,
    ee_root_slot_allowance: Option<std::collections::BTreeSet<String>>,
}

#[derive(Clone, Debug)]
struct DecodedRoomRequest {
    scope: RoomInvocationScope,
    call: ToolCall,
    correlation_id: Option<String>,
}

pub struct WorkspaceRoom {
    layout: Box<dyn WorkspaceLayout>,
    handler: Arc<dyn ToolHandler>,
    process_manager: Arc<WorkspaceProcessManager>,
    floor_runner: Arc<FloorRunner>,
    rooms: tokio::sync::Mutex<BTreeMap<WorkerEnvironmentId, WorkerEnvironment>>,
}

enum PlannedOperation {
    Layout(LayoutRun),
    Workspace(WorkspaceHandlerConfig),
}

struct RoomToolFailure(ToolResultError);

impl From<String> for RoomToolFailure {
    fn from(message: String) -> Self {
        Self(ToolResultError::new("tool_execution_failed", message))
    }
}

impl From<ToolHandlerError> for RoomToolFailure {
    fn from(error: ToolHandlerError) -> Self {
        Self(error.into_tool_result_error())
    }
}

impl WorkspaceRoom {
    pub fn new(
        layout: Box<dyn WorkspaceLayout>,
        handler: Arc<dyn ToolHandler>,
        process_store: Option<WorkspaceProcessStore>,
    ) -> Self {
        Self {
            layout,
            handler,
            process_manager: Arc::new(WorkspaceProcessManager::new(process_store)),
            floor_runner: Arc::new(FloorRunner::default()),
            rooms: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }

    async fn receive(
        &self,
        descriptor: ToolDescriptor,
        input: InterfaceInvocationEnvelope,
        source_movement_seq: u64,
        formation: WorkerEnvironmentFormation,
        kind: WorkerEnvKindSpec,
        io: &ToolProcessIo,
    ) -> Result<InterfaceArtifactFile, InterfaceBoundaryError> {
        let decoded = decode_room_request(self.layout.room(), &input)?;
        if descriptor.id.as_str() != decoded.call.tool_id {
            return Err(InterfaceBoundaryError::Protocol(format!(
                "tool descriptor '{}' does not match tool.call '{}'",
                descriptor.id, decoded.call.tool_id
            )));
        }
        let tool_id = decoded.call.tool_id.clone();
        let correlation_id = decoded.correlation_id;
        let response = match self
            .execute_tool_call(
                decoded.scope,
                decoded.call,
                &format!("movement:{source_movement_seq}"),
                &descriptor,
                &formation,
                &kind,
                io,
            )
            .await
        {
            Ok(result) => ToolResultArtifact::success(&tool_id, correlation_id, result.body),
            Err(error) => ToolResultArtifact::failure(&tool_id, correlation_id, error.0),
        };
        emit_json_artifact(&input, &response.label, &response).map_err(InterfaceBoundaryError::Protocol)
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_call(
        &self,
        scope: RoomInvocationScope,
        mut call: ToolCall,
        call_identity: &str,
        descriptor: &ToolDescriptor,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
        io: &ToolProcessIo,
    ) -> Result<ToolHandlerResult, RoomToolFailure> {
        call.handler_config = descriptor.handler_config.clone();
        let room_root = self.layout.room_root(formation)?;
        let planned = match self.layout.operation(LayoutCall {
            tool_id: &call.tool_id,
            handler_config: &call.handler_config,
            formation,
            room_root: &room_root,
        })? {
            Some(LayoutOperation::EndsRoom {
                cleanup_timeout,
                run,
            }) => {
                self.process_manager
                    .release(&formation.worker_environment_id, Some(cleanup_timeout))
                    .await?;
                let result = run()?;
                self.rooms
                    .lock()
                    .await
                    .remove(&formation.worker_environment_id);
                io.record_worker_environment_lifecycle(
                    formation,
                    WorkerEnvironmentLifecycle::Released,
                )
                .await?;
                return Ok(result);
            }
            Some(LayoutOperation::InRoom(run)) => PlannedOperation::Layout(run),
            None => PlannedOperation::Workspace(workspace_handler_config(&call)?),
        };
        if !self
            .rooms
            .lock()
            .await
            .contains_key(&formation.worker_environment_id)
        {
            let env = match self.lease(&scope, formation, kind).await {
                Ok(env) => env,
                Err(error) => {
                    io.record_worker_environment_lifecycle(
                        formation,
                        WorkerEnvironmentLifecycle::Failed,
                    )
                    .await?;
                    return Err(error.to_string().into());
                }
            };
            self.rooms
                .lock()
                .await
                .insert(formation.worker_environment_id.clone(), env);
        }
        io.record_worker_environment_lifecycle(formation, WorkerEnvironmentLifecycle::Active)
            .await?;
        let (leased_kind, sandbox, fs, exec) = {
            let rooms = self.rooms.lock().await;
            let env = rooms.get(&formation.worker_environment_id).ok_or_else(|| {
                format!(
                    "worker environment '{}' disappeared after lease",
                    formation.worker_environment_id
                )
            })?;
            (
                env.kind_id().clone(),
                env.sandbox().clone(),
                env.fs(),
                env.exec_with(Arc::new(FloorExecution {
                    owner: self.floor_runner.clone(),
                    sandbox_mode: descriptor.process.authority.sandbox_mode,
                })),
            )
        };
        validate_tool_code_world(descriptor, &leased_kind)?;
        let handler_config = match planned {
            PlannedOperation::Layout(run) => return Ok(run()?),
            PlannedOperation::Workspace(config) => config,
        };
        let ctx = ToolHandlerContext {
            run: scope.run.clone(),
            room_root: room_root.clone(),
            sandbox: sandbox.clone(),
            fs,
            exec,
            artifacts: None,
        };
        let result = match handler_config.operation {
            WorkspaceOperation::StartCommand => workspace_exec::start_command(
                &ctx,
                &call,
                call_identity,
                &descriptor.process.inherit_env,
                &handler_config,
                &self.process_manager,
                &formation.worker_environment_id,
                &sandbox,
                descriptor.process.authority.sandbox_mode,
            )
            .await
            .map_err(RoomToolFailure::from)?,
            WorkspaceOperation::ProcessSignal => workspace_exec::process_signal(
                &ctx,
                &call,
                call_identity,
                &handler_config,
                &self.process_manager,
                &formation.worker_environment_id,
            )
            .await
            .map_err(RoomToolFailure::from)?,
            WorkspaceOperation::ProcessTerminate => workspace_exec::process_terminate(
                &ctx,
                &call,
                call_identity,
                &handler_config,
                &self.process_manager,
                &formation.worker_environment_id,
            )
            .await
            .map_err(RoomToolFailure::from)?,
            WorkspaceOperation::ProcessList => workspace_exec::process_list(
                &ctx,
                &call,
                &self.process_manager,
                &formation.worker_environment_id,
            )
            .await
            .map_err(RoomToolFailure::from)?,
            WorkspaceOperation::WriteStdin => workspace_exec::write_stdin(
                &ctx,
                &call,
                call_identity,
                &handler_config,
                &self.process_manager,
                &formation.worker_environment_id,
            )
            .await
            .map_err(RoomToolFailure::from)?,
            _ => self
                .handler
                .execute(&ctx, &call)
                .await
                .map_err(RoomToolFailure::from)?,
        };
        Ok(result)
    }

    async fn lease(
        &self,
        scope: &RoomInvocationScope,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Result<WorkerEnvironment, WorkerEnvError> {
        let (mut root_slots, driver) = self.layout.lease_parts(kind)?;
        for (slot, path) in &formation.door_root_bindings {
            if root_slots.contains_key(slot) {
                continue;
            }
            root_slots.insert(slot.clone(), AbsPath::new(path)?);
        }
        let ctx = EnvContext {
            additional_root_slots: formation.door_additional_root_bindings.iter().map(|(slot, paths)| Ok((slot.clone(), paths.iter().map(|path| AbsPath::new(path)).collect::<Result<Vec<_>, _>>()?))).collect::<Result<_, WorkerEnvError>>()?,
            root_slots,
            env: BTreeMap::new(),
        };
        octopus_worker_env::lease(
            kind,
            formation,
            driver,
            ctx,
            scope.ee_root_slot_allowance.as_ref(),
        )
        .await
    }
}

fn validate_tool_code_world(
    descriptor: &ToolDescriptor,
    leased_kind: &octopus_descriptors::WorkerEnvironmentKindId,
) -> Result<(), String> {
    match descriptor.supported_worker_environment_kinds.as_slice() {
        [kind] if kind == leased_kind => Ok(()),
        _ => Err(format!(
            "tool '{}' cannot execute in worker_environment_kind '{}' because its code-world ownership is not exact",
            descriptor.id,
            leased_kind.as_str()
        )),
    }
}

impl ToolProcessHandler for WorkspaceRoom {
    fn substrate_family(&self) -> &str {
        FAMILY_PROCESS_EXEC
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
            let room = self.layout.room();
            let formation = invocation.worker_environment.ok_or_else(|| {
                ToolProcessError::Capability(format!(
                    "{room} invocation has no formed worker environment"
                ))
            })?;
            let kind = invocation.worker_environment_kind.ok_or_else(|| {
                ToolProcessError::Capability(format!(
                    "{room} invocation has no formed kind descriptor"
                ))
            })?;
            self.receive(
                invocation.descriptor,
                invocation.envelope,
                invocation.source_movement_seq,
                formation,
                kind,
                &io,
            )
            .await
            .map(|artifact| vec![artifact])
            .map_err(|error| ToolProcessError::Capability(error.to_string()))
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
            self.process_manager
                .release(&formation.worker_environment_id, None)
                .await
                .map_err(ToolProcessError::Unavailable)?;
            let env = self
                .rooms
                .lock()
                .await
                .remove(&formation.worker_environment_id);
            match env {
                Some(env) => env
                    .release()
                    .await
                    .map_err(|error| ToolProcessError::Unavailable(error.to_string())),
                None => self
                    .layout
                    .release_unleased(formation)
                    .map_err(ToolProcessError::Unavailable),
            }
        })
    }

    fn quiesce<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.floor_runner
                .quiesce()
                .await
                .map_err(ToolProcessError::Unavailable)
        })
    }
}

pub fn process_store(env: &ToolProcessEnvironment) -> Result<Option<WorkspaceProcessStore>, String> {
    let (Some(durable), Some(sockets)) = (
        env.durable_var_dir.as_deref(),
        env.root_slots.get(INTERFACE_LOCAL_IPC_ROOT_SLOT),
    ) else {
        return Ok(None);
    };
    let keeper = std::env::current_exe()
        .map_err(|error| format!("persistent process keeper executable is unavailable: {error}"))?;
    WorkspaceProcessStore::new(durable, sockets, keeper).map(Some)
}

pub async fn run_workspace_room_tool_process(
    layout: impl FnOnce(&ToolProcessEnvironment) -> Result<Box<dyn WorkspaceLayout>, String>,
    handler: Arc<dyn ToolHandler>,
) -> Result<(), String> {
    if let Some(spec) = octopus_workspace_substrate::persistent_process_keeper_spec(std::env::args_os()) {
        return octopus_workspace_substrate::run_persistent_process_keeper(spec).await;
    }
    let env = ToolProcessEnvironment::from_env()?;
    let room = WorkspaceRoom::new(layout(&env)?, handler, process_store(&env)?);
    octopus_tool_door_runtime::run_tool_process(Arc::new(room)).await
}

fn direct_layout(env: &ToolProcessEnvironment) -> Result<Box<dyn WorkspaceLayout>, String> {
    Ok(Box::new(DirectWorkspace::from_process_environment(env)?))
}

pub async fn run_workspace_fs_tool_process() -> Result<(), String> {
    run_workspace_room_tool_process(direct_layout, Arc::new(WorkspaceFsHandler)).await
}

pub async fn run_workspace_exec_tool_process() -> Result<(), String> {
    run_workspace_room_tool_process(direct_layout, Arc::new(WorkspaceExecHandler)).await
}

fn decode_room_request(
    room: &str,
    input: &InterfaceInvocationEnvelope,
) -> Result<DecodedRoomRequest, InterfaceBoundaryError> {
    let artifact = decode_tool_call(room, input)
        .map_err(|error| InterfaceBoundaryError::Protocol(error.to_string()))?;
    let tool_id = admitted_tool(room, input, &artifact)?;
    let scope = invocation_scope(room, input)?;
    let ee_root_slot_allowance = input
        .metadata()
        .map_err(|error| InterfaceBoundaryError::Protocol(error.to_string()))?
        .grant()
        .ee_root_slot_allowance_set();
    Ok(DecodedRoomRequest {
        scope: RoomInvocationScope {
            run: scope.run,
            ee_root_slot_allowance,
        },
        correlation_id: artifact.correlation_id,
        call: ToolCall {
            tool_id,
            arguments: artifact.arguments,
            handler_config: serde_json::Value::Null,
        },
    })
}

pub fn workspace_handler_config(call: &ToolCall) -> Result<WorkspaceHandlerConfig, String> {
    let config: WorkspaceHandlerConfig =
        serde_json::from_value(call.handler_config.clone()).map_err(|error| {
            format!(
                "tool '{}' has malformed workspace handler_config: {error}",
                call.tool_id
            )
        })?;
    let timeout_limits = [
        config.default_timeout_ms.is_some(),
        config.max_timeout_ms.is_some(),
    ];
    let output_limits = [
        config.default_max_output_bytes.is_some(),
        config.max_output_bytes.is_some(),
    ];
    let yield_limits = [
        config.default_yield_time_ms.is_some(),
        config.max_yield_time_ms.is_some(),
    ];
    let start_limits = [
        config.max_buffered_output_bytes.is_some(),
        config.max_buffered_output_events.is_some(),
        config.max_processes_per_worker_environment.is_some(),
        config.cleanup_timeout_ms.is_some(),
    ];
    let has_input_limit = config.max_input_bytes.is_some();
    let grep_limits = [
        config.max_scanned_files.is_some(),
        config.max_scanned_bytes.is_some(),
        config.deadline_ms.is_some(),
        config.max_line_bytes.is_some(),
    ];
    let shell_config = [
        config.default_shell.is_some(),
        config.allowed_shells.is_some(),
        config.allow_login_shell.is_some(),
    ];
    match config.operation {
        WorkspaceOperation::RunCommand => {
            if !timeout_limits.iter().all(|present| *present)
                || !output_limits.iter().all(|present| *present)
            {
                return Err(format!(
                    "tool '{}' command operation requires complete timeout and output limits",
                    call.tool_id
                ));
            }
            if grep_limits.iter().any(|present| *present) {
                return Err(format!(
                    "tool '{}' command operation cannot declare grep limits",
                    call.tool_id
                ));
            }
            if yield_limits
                .iter()
                .chain(start_limits.iter())
                .any(|present| *present)
                || has_input_limit
            {
                return Err(format!(
                    "tool '{}' one-shot command cannot declare persistent process limits",
                    call.tool_id
                ));
            }
            validate_timeout_limits(&call.tool_id, &config)?;
            validate_output_limits(&call.tool_id, &config)?;
            validate_shell_config(&call.tool_id, &config)?;
        }
        WorkspaceOperation::StartCommand => {
            if grep_limits.iter().any(|present| *present) {
                return Err(format!(
                    "tool '{}' persistent command cannot declare grep limits",
                    call.tool_id
                ));
            }
            if !timeout_limits.iter().all(|present| *present)
                || !output_limits.iter().all(|present| *present)
                || !yield_limits.iter().all(|present| *present)
                || !start_limits.iter().all(|present| *present)
                || !has_input_limit
            {
                return Err(format!(
                    "tool '{}' persistent command requires complete descriptor-owned limits",
                    call.tool_id
                ));
            }
            validate_timeout_limits(&call.tool_id, &config)?;
            validate_output_limits(&call.tool_id, &config)?;
            validate_yield_limits(&call.tool_id, &config)?;
            validate_persistent_start_limits(&call.tool_id, &config)?;
            validate_shell_config(&call.tool_id, &config)?;
        }
        WorkspaceOperation::WriteStdin => {
            if timeout_limits.iter().any(|present| *present)
                || start_limits.iter().any(|present| *present)
                || grep_limits.iter().any(|present| *present)
                || shell_config.iter().any(|present| *present)
            {
                return Err(format!(
                    "tool '{}' stdin continuation can declare only input, yield, and output limits",
                    call.tool_id
                ));
            }
            if !output_limits.iter().all(|present| *present)
                || !yield_limits.iter().all(|present| *present)
                || !has_input_limit
            {
                return Err(format!(
                    "tool '{}' stdin continuation requires complete input, yield, and output limits",
                    call.tool_id
                ));
            }
            validate_output_limits(&call.tool_id, &config)?;
            validate_yield_limits(&call.tool_id, &config)?;
            let max_input = config.max_input_bytes.unwrap_or(0);
            if max_input == 0 || max_input > 1024 * 1024 {
                return Err(format!(
                    "tool '{}' stdin continuation input limit is invalid",
                    call.tool_id
                ));
            }
        }
        WorkspaceOperation::ProcessSignal | WorkspaceOperation::ProcessTerminate => {
            if timeout_limits.iter().any(|present| *present)
                || start_limits.iter().any(|present| *present)
                || grep_limits.iter().any(|present| *present)
                || has_input_limit
                || shell_config.iter().any(|present| *present)
            {
                return Err(format!(
                    "tool '{}' process control can declare only yield and output limits",
                    call.tool_id
                ));
            }
            if !output_limits.iter().all(|present| *present)
                || !yield_limits.iter().all(|present| *present)
            {
                return Err(format!(
                    "tool '{}' process control requires complete yield and output limits",
                    call.tool_id
                ));
            }
            validate_output_limits(&call.tool_id, &config)?;
            validate_yield_limits(&call.tool_id, &config)?;
        }
        WorkspaceOperation::ProcessList => {
            if timeout_limits
                .iter()
                .chain(output_limits.iter())
                .chain(yield_limits.iter())
                .chain(start_limits.iter())
                .chain(grep_limits.iter())
                .chain(shell_config.iter())
                .any(|present| *present)
                || has_input_limit
            {
                return Err(format!(
                    "tool '{}' process list cannot declare execution limits",
                    call.tool_id
                ));
            }
        }
        WorkspaceOperation::Grep => {
            if timeout_limits
                .iter()
                .chain(output_limits.iter())
                .chain(yield_limits.iter())
                .chain(start_limits.iter())
                .any(|present| *present)
                || has_input_limit
                || shell_config.iter().any(|present| *present)
            {
                return Err(format!(
                    "tool '{}' grep operation cannot declare command limits",
                    call.tool_id
                ));
            }
            if !grep_limits.iter().all(|present| *present) {
                return Err(format!(
                    "tool '{}' grep operation requires complete scan limits",
                    call.tool_id
                ));
            }
            let max_scanned_files = config.max_scanned_files.unwrap_or(0);
            let max_scanned_bytes = config.max_scanned_bytes.unwrap_or(0);
            let deadline_ms = config.deadline_ms.unwrap_or(0);
            let max_line_bytes = config.max_line_bytes.unwrap_or(0);
            if max_scanned_files == 0
                || max_scanned_files > 1_000_000
                || max_scanned_bytes == 0
                || max_scanned_bytes > 4 * 1024 * 1024 * 1024
                || deadline_ms == 0
                || deadline_ms > 120_000
                || max_line_bytes == 0
                || max_line_bytes > 16 * 1024 * 1024
            {
                return Err(format!(
                    "tool '{}' grep scan limits are invalid",
                    call.tool_id
                ));
            }
        }
        _ if timeout_limits
            .iter()
            .chain(output_limits.iter())
            .chain(yield_limits.iter())
            .chain(start_limits.iter())
            .any(|present| *present)
            || has_input_limit =>
        {
            return Err(format!(
                "tool '{}' non-process operation cannot declare process limits",
                call.tool_id
            ));
        }
        _ if grep_limits.iter().any(|present| *present) => {
            return Err(format!(
                "tool '{}' non-grep operation cannot declare grep limits",
                call.tool_id
            ));
        }
        _ if shell_config.iter().any(|present| *present) => {
            return Err(format!(
                "tool '{}' non-command operation cannot declare shell config",
                call.tool_id
            ));
        }
        _ => {}
    }
    Ok(config)
}

fn validate_timeout_limits(tool_id: &str, config: &WorkspaceHandlerConfig) -> Result<(), String> {
    let default_timeout = config.default_timeout_ms.unwrap_or(0);
    let max_timeout = config.max_timeout_ms.unwrap_or(0);
    if default_timeout == 0
        || max_timeout == 0
        || default_timeout > max_timeout
        || max_timeout > 1_800_000
    {
        return Err(format!(
            "tool '{tool_id}' command timeout limits are invalid"
        ));
    }
    Ok(())
}

fn validate_output_limits(tool_id: &str, config: &WorkspaceHandlerConfig) -> Result<(), String> {
    let default_output = config.default_max_output_bytes.unwrap_or(0);
    let max_output = config.max_output_bytes.unwrap_or(0);
    if default_output == 0
        || max_output == 0
        || default_output > max_output
        || max_output > 16 * 1024 * 1024
    {
        return Err(format!(
            "tool '{tool_id}' command output limits are invalid"
        ));
    }
    Ok(())
}

fn validate_yield_limits(tool_id: &str, config: &WorkspaceHandlerConfig) -> Result<(), String> {
    let default_yield = config.default_yield_time_ms.unwrap_or(u64::MAX);
    let max_yield = config.max_yield_time_ms.unwrap_or(0);
    if default_yield > max_yield {
        return Err(format!("tool '{tool_id}' command yield limits are invalid"));
    }
    Ok(())
}

fn validate_cleanup_timeout(tool_id: &str, config: &WorkspaceHandlerConfig) -> Result<(), String> {
    let cleanup_timeout = config.cleanup_timeout_ms.unwrap_or(0);
    if cleanup_timeout == 0 || cleanup_timeout > 60_000 {
        return Err(format!(
            "tool '{tool_id}' persistent cleanup timeout is invalid"
        ));
    }
    Ok(())
}

fn validate_persistent_start_limits(
    tool_id: &str,
    config: &WorkspaceHandlerConfig,
) -> Result<(), String> {
    let max_buffered_output = config.max_buffered_output_bytes.unwrap_or(0);
    let max_buffered_events = config.max_buffered_output_events.unwrap_or(0);
    let max_processes = config.max_processes_per_worker_environment.unwrap_or(0);
    let max_input = config.max_input_bytes.unwrap_or(0);
    if max_buffered_output == 0 || max_buffered_output > 64 * 1024 * 1024 {
        return Err(format!(
            "tool '{tool_id}' persistent unread output limit is invalid"
        ));
    }
    if max_buffered_events == 0 || max_buffered_events > 1_000_000 {
        return Err(format!(
            "tool '{tool_id}' persistent unread event limit is invalid"
        ));
    }
    if max_processes == 0 || max_processes > 1024 {
        return Err(format!(
            "tool '{tool_id}' persistent process count limit is invalid"
        ));
    }
    if max_input == 0 || max_input > 1024 * 1024 {
        return Err(format!(
            "tool '{tool_id}' persistent input limit is invalid"
        ));
    }
    validate_cleanup_timeout(tool_id, config)
}

fn validate_shell_config(tool_id: &str, config: &WorkspaceHandlerConfig) -> Result<(), String> {
    let default_shell = config
        .default_shell
        .as_deref()
        .filter(|shell| !shell.trim().is_empty())
        .ok_or_else(|| format!("tool '{tool_id}' command requires a non-empty default_shell"))?;
    let allowed_shells = config
        .allowed_shells
        .as_ref()
        .filter(|shells| !shells.is_empty())
        .ok_or_else(|| format!("tool '{tool_id}' command requires allowed_shells"))?;
    if allowed_shells.iter().any(|shell| shell.trim().is_empty()) {
        return Err(format!(
            "tool '{tool_id}' command allowed_shells contains an empty shell"
        ));
    }
    let unique = allowed_shells
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    if unique.len() != allowed_shells.len() {
        return Err(format!(
            "tool '{tool_id}' command allowed_shells contains duplicates"
        ));
    }
    if !allowed_shells.iter().any(|shell| shell == default_shell) {
        return Err(format!(
            "tool '{tool_id}' command default_shell is absent from allowed_shells"
        ));
    }
    if config.allow_login_shell.is_none() {
        return Err(format!(
            "tool '{tool_id}' command requires allow_login_shell"
        ));
    }
    Ok(())
}

#[derive(Default)]
struct FloorRunner {
    jobs: tokio::sync::Mutex<Vec<FloorJob>>,
    closed: std::sync::atomic::AtomicBool,
}

struct CancelCommandOnDrop(Arc<std::sync::atomic::AtomicBool>);

impl Drop for CancelCommandOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

struct FloorJob {
    cancel: CancelCommandOnDrop,
    task: Option<tokio::task::JoinHandle<Result<(), String>>>,
}

impl FloorJob {
    async fn join(mut self) -> Result<(), String> {
        self.task
            .take()
            .expect("owned command job exists")
            .await
            .map_err(|error| format!("sandboxed exec task failed: {error}"))?
    }
}

impl FloorRunner {
    async fn quiesce(&self) -> Result<(), String> {
        let jobs = {
            let mut jobs = self.jobs.lock().await;
            self.closed
                .store(true, std::sync::atomic::Ordering::Release);
            for job in jobs.iter() {
                job.cancel
                    .0
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            std::mem::take(&mut *jobs)
        };
        let mut errors = Vec::new();
        for job in jobs {
            if let Err(error) = job.join().await {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

struct FloorExecution {
    owner: Arc<FloorRunner>,
    sandbox_mode: ProcessSandboxMode,
}

impl SandboxedCommandRunner for FloorExecution {
    fn run_sandboxed(
        &self,
        sandbox: &ProjectedSandbox,
        cwd: &AbsPath,
        argv: Vec<String>,
        timeout: std::time::Duration,
        max_output_bytes: usize,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<octopus_worker_env::ExecOutput, WorkerEnvError>>
                + Send
                + '_,
        >,
    > {
        let sandbox = sandbox.clone();
        let cwd = cwd.clone();
        let sandbox_mode = self.sandbox_mode;
        Box::pin(async move {
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let _cancel_on_drop = CancelCommandOnDrop(Arc::clone(&cancel));
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            let mut jobs = self.owner.jobs.lock().await;
            if self.owner.closed.load(std::sync::atomic::Ordering::Acquire) {
                return Err(WorkerEnvError::ExecFailed(
                    "command owner is quiesced".to_string(),
                ));
            }
            let mut errors = Vec::new();
            for index in (0..jobs.len()).rev() {
                if jobs[index]
                    .task
                    .as_ref()
                    .is_some_and(tokio::task::JoinHandle::is_finished)
                {
                    if let Err(error) = jobs.swap_remove(index).join().await {
                        errors.push(error);
                    }
                }
            }
            if !errors.is_empty() {
                return Err(WorkerEnvError::ExecFailed(errors.join("; ")));
            }
            let job_cancel = Arc::clone(&cancel);
            let task = tokio::task::spawn_blocking(move || {
                let result = octopus_workspace_substrate::run_sandboxed_in_room(
                    &sandbox,
                    sandbox_mode,
                    &cwd,
                    argv,
                    timeout,
                    max_output_bytes,
                    &job_cancel,
                );
                match result_tx.send(result) {
                    Err(Err(WorkerEnvError::ExecFailedWithOutput { reason, captured }))
                        if captured.cancelled && captured.cleanup_errors.is_empty() =>
                    {
                        tracing::debug!(%reason, exit_observed = captured.exit_observed, "cancelled command owner job settled");
                        Ok(())
                    }
                    Err(Err(error)) => Err(format!("command result was not delivered: {error}")),
                    _ => Ok(()),
                }
            });
            jobs.push(FloorJob {
                cancel: CancelCommandOnDrop(cancel),
                task: Some(task),
            });
            drop(jobs);
            result_rx.await.map_err(|error| {
                WorkerEnvError::ExecFailed(format!("sandboxed exec result unavailable: {error}"))
            })?
        })
    }
}
