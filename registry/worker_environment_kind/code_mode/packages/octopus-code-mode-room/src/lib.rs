#[cfg(test)]
extern crate self as octopus_code_mode_room;

use base64::Engine;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use octopus_code_mode_engine::{
    CellId, CodeModeNestedToolCall, CodeModeService, CodeModeSession, CodeModeSessionDelegate,
    CodeModeToolKind, ExecuteRequest, FunctionCallOutputContentItem, NotificationFuture,
    RuntimeResponse, ToolDefinition, ToolInvocationFuture, ToolName, WaitOutcome, WaitRequest,
    normalize_code_mode_identifier,
};
use octopus_descriptors::InterfaceId;
use octopus_interfaces::{InterfaceArtifactFile, InterfaceInvocationEnvelope};
use octopus_tool_door_runtime::room::{
    TOOL_CALL_LABEL, decode_tool_call, emit_artifact, formed_scope, strict_object,
};
use octopus_tool_door_runtime::{
    DeliveryVerdict, SystemInterfaceEmissionArtifact, ToolProcessEnvironment, ToolProcessError,
    ToolProcessHandler, ToolProcessInvocation, ToolProcessIo, WorkerEnvironmentLifecycleRecorder,
    WorkerEnvironmentLiveStateRecorder,
};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact, ToolResultError};
use octopus_worker_env::{
    WorkerEnvKindSpec, WorkerEnvironmentFormation, WorkerEnvironmentId, WorkerEnvironmentLifecycle,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

pub mod config;
pub mod continuation;

use continuation::{ContinuationContext, OriginalRun};

use config::CodeModeConfig;
use config::ResolvedContext;

const ROOM: &str = "code mode room";
const SUBSTRATE_FAMILY: &str = "code_mode";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CodeModeOperation {
    Run,
    Resume,
    Wait,
    Cancel,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeModeHandlerConfig {
    operation: CodeModeOperation,
}

struct RoomState {
    service: Arc<CodeModeService>,
    delegate: Arc<RoomDelegate>,
    formation: WorkerEnvironmentFormation,
    context: ResolvedContext,
    config: CodeModeConfig,
    nested_tools: Vec<ToolDefinition>,
    continuation: Option<Arc<ContinuationContext>>,
    frontiers: Arc<Mutex<RetainedFrontiers>>,
}

#[derive(Default)]
struct RetainedFrontiers {
    cells: BTreeMap<String, VecDeque<Value>>,
    bytes: usize,
}

pub struct CodeModeRoom {
    environment: ToolProcessEnvironment,
    rooms: tokio::sync::Mutex<BTreeMap<WorkerEnvironmentId, RoomState>>,
}

impl CodeModeRoom {
    pub fn new(environment: ToolProcessEnvironment) -> Self {
        Self {
            environment,
            rooms: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn receive(
        &self,
        invocation: ToolProcessInvocation,
        io: ToolProcessIo,
    ) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
        self.receive_with_sink(invocation, &io).await
    }

    async fn receive_with_sink(
        &self,
        invocation: ToolProcessInvocation,
        sink: &dyn EmissionSink,
    ) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
        let ToolProcessInvocation {
            request_id,
            descriptor,
            envelope,
            worker_environment,
            worker_environment_kind,
            source_movement_seq,
        } = invocation;
        let formation = worker_environment.ok_or_else(|| {
            ToolProcessError::Capability(
                "code mode invocation has no formed worker environment".to_string(),
            )
        })?;
        let kind = worker_environment_kind.ok_or_else(|| {
            ToolProcessError::Capability(
                "code mode invocation has no formed kind descriptor".to_string(),
            )
        })?;
        let config = CodeModeConfig::load_formed(&self.environment, &formation, &kind)
            .map_err(ToolProcessError::Capability)?;
        let readiness = envelope
            .read_artifact_manifest()
            .map_err(|e| ToolProcessError::Capability(e.to_string()))?
            .label
            == "code_mode.frontier_ready";
        let call = if readiness {
            let root = self.environment.durable_var_dir.as_deref().ok_or_else(|| {
                ToolProcessError::Capability("readiness has no owner directory".into())
            })?;
            continuation::formed_ready_call(root, &envelope, &config, source_movement_seq)
                .map_err(ToolProcessError::Capability)?
        } else if let Some(worker) =
            continuation::result_locator(&envelope).map_err(ToolProcessError::Capability)?
        {
            if worker != formation.worker_environment_id {
                return Err(ToolProcessError::Capability(
                    "code mode result locates another formed worker environment".into(),
                ));
            }
            let root = self.environment.durable_var_dir.as_deref().ok_or_else(|| {
                ToolProcessError::Capability(
                    "code mode continuation has no durable owner directory".into(),
                )
            })?;
            continuation::decode_formed_result(
                root,
                &self.environment.interface,
                &envelope,
                &config,
                source_movement_seq,
            )
            .map_err(ToolProcessError::Capability)?
        } else {
            let call = decode_tool_call(ROOM, &envelope)?;
            if config
                .continuation
                .as_ref()
                .is_some_and(|c| c.resume_tool() == call.tool)
            {
                return Err(ToolProcessError::Capability(
                    "persistent continuation requires the original admitted ToolResult artifact"
                        .into(),
                ));
            }
            call
        };
        if descriptor.id.as_str() != call.tool {
            return Err(ToolProcessError::Capability(format!(
                "tool descriptor '{}' does not match tool.call '{}'",
                descriptor.id, call.tool
            )));
        }
        let operation = code_mode_operation(&descriptor.handler_config, descriptor.id.as_str())?;
        if config
            .continuation
            .as_ref()
            .is_some_and(|continuation| continuation.resume_tool() == call.tool)
            && operation != CodeModeOperation::Resume
        {
            return Err(ToolProcessError::Capability(
                "code mode continuation descriptor does not declare the resume operation".into(),
            ));
        }
        formed_scope(ROOM, &envelope, &formation)?;
        let state = self
            .room_state(
                &formation,
                &config,
                &envelope,
                operation,
                source_movement_seq,
            )
            .await
            .map_err(ToolProcessError::Capability)?;
        sink.record(&formation, WorkerEnvironmentLifecycle::Active)
            .await
            .map_err(ToolProcessError::Unavailable)?;
        let result = dispatch(
            &state,
            &config,
            request_id,
            source_movement_seq,
            operation,
            &call,
            &envelope,
            sink,
        )
        .await;
        let owner_failed =
            result.is_err() && (state.continuation.is_none() || !state.service.is_alive());
        let status = if owner_failed { "failed" } else { "ready" };
        let detail = result.as_ref().err().cloned();
        let mut evidence_write_errors = Vec::new();
        if owner_failed {
            if let Err(error) = sink
                .record(&formation, WorkerEnvironmentLifecycle::Failed)
                .await
            {
                evidence_write_errors.push(evidence_write_error("lifecycle", error));
            }
        }
        if let Err(error) = self
            .write_live_state(sink, &formation, &state, status, detail)
            .await
        {
            evidence_write_errors.push(evidence_write_error("live_state", error));
        }
        let result = result.and_then(|mut body| {
            materialize_output_media(&envelope, &config, &mut body)?;
            Ok(body)
        });
        let response = match result {
            Ok(body) if evidence_write_errors.is_empty() => {
                ToolResultArtifact::success(&call.tool, call.correlation_id.clone(), body)
            }
            Ok(body) => {
                let mut details = Map::new();
                details.insert("operation_body".to_string(), body);
                details.insert(
                    "evidence_write_errors".to_string(),
                    Value::Array(evidence_write_errors),
                );
                ToolResultArtifact::failure(
                    &call.tool,
                    call.correlation_id.clone(),
                    ToolResultError::new(
                        "worker_environment_evidence_failed",
                        format!(
                            "{} completed but its worker-environment evidence could not be fully written",
                            call.tool
                        ),
                    )
                    .with_details(details),
                )
            }
            Err(error) => {
                let mut failure = ToolResultError::new("tool_execution_failed", error);
                if !evidence_write_errors.is_empty() {
                    let mut details = Map::new();
                    details.insert(
                        "evidence_write_errors".to_string(),
                        Value::Array(evidence_write_errors),
                    );
                    failure = failure.with_details(details);
                }
                ToolResultArtifact::failure(&call.tool, call.correlation_id.clone(), failure)
            }
        };
        let mut bytes = serde_json::to_vec(&response).map_err(|error| {
            ToolProcessError::Capability(format!(
                "code mode emitted artifact did not serialize: {error}"
            ))
        })?;
        if bytes.len() > config.max_output_bytes {
            let encoded_bytes = bytes.len();
            let mut cleanup_error = None;
            if let Some(cell_id) = response
                .body
                .as_ref()
                .or_else(|| {
                    response
                        .error
                        .as_ref()
                        .and_then(|error| error.details.as_ref())
                        .and_then(|details| details.get("operation_body"))
                })
                .and_then(|body| body.get("cell_id"))
                .and_then(Value::as_str)
            {
                cleanup_error = state
                    .service
                    .terminate(CellId::new(cell_id.to_string()))
                    .await
                    .err()
                    .map(|error| error.to_string());
            }
            let mut details = Map::new();
            details.insert(
                "effect".to_string(),
                json!({
                    "operation": call.tool,
                    "attempted": true,
                    "encoded_bytes": encoded_bytes,
                }),
            );
            if let Some(cleanup_error) = cleanup_error {
                details.insert("cleanup_error".to_string(), Value::String(cleanup_error));
            }
            details.insert(
                "evidence_write_errors".to_string(),
                response
                    .error
                    .as_ref()
                    .and_then(|error| error.details.as_ref())
                    .and_then(|details| details.get("evidence_write_errors"))
                    .cloned()
                    .unwrap_or_else(|| json!([])),
            );
            let bounded = ToolResultArtifact::failure(
                &call.tool,
                call.correlation_id.clone(),
                ToolResultError::new(
                    "code_mode_result_too_large",
                    format!(
                        "code mode result is {encoded_bytes} bytes, above configured limit {}",
                        config.max_output_bytes
                    ),
                )
                .with_details(details),
            );
            bytes = serde_json::to_vec(&bounded).map_err(|error| {
                ToolProcessError::Capability(format!(
                    "code mode bounded failure artifact did not serialize: {error}"
                ))
            })?;
            if bytes.len() > config.max_output_bytes {
                return Err(ToolProcessError::Capability(format!(
                    "code mode configured output limit {} cannot carry its bounded failure artifact of {} bytes",
                    config.max_output_bytes,
                    bytes.len()
                )));
            }
        }
        Ok(vec![emit_artifact(&envelope, &response.label, &bytes)
            .map_err(ToolProcessError::Capability)?])
    }

    async fn room_state(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &CodeModeConfig,
        envelope: &InterfaceInvocationEnvelope,
        operation: CodeModeOperation,
        source_movement_seq: u64,
    ) -> Result<RoomState, String> {
        let context = match config.resolve_context(envelope) {
            Ok(context) => context,
            Err(error) => {
                let retired = self
                    .rooms
                    .lock()
                    .await
                    .get(&formation.worker_environment_id)
                    .filter(|state| matches!(&state.context, ResolvedContext::Persistent { .. }))
                    .map(|state| Arc::clone(&state.service));
                if let Some(service) = retired {
                    service.shutdown().await?;
                }
                return Err(error);
            }
        };
        let owners = config.handoff.as_ref().map(|_| {
            config.resolve_owner_topology(envelope)?.routes.into_iter().map(|route| {
                Ok((route.tool, config::DeclaredToolOwner::from_descriptor(&route.process_descriptor)?))
            }).collect::<Result<BTreeMap<_, _>, String>>()
        }).transpose()?;
        let nested_tools = if matches!(&context, ResolvedContext::Persistent { .. }) {
            enabled_nested_tools(config, envelope)?
        } else {
            Vec::new()
        };
        let mut rooms = self.rooms.lock().await;
        if let Some(state) = rooms.get(&formation.worker_environment_id) {
            if state.context != context
                || (matches!(&context, ResolvedContext::Persistent { .. })
                    && (state.formation != *formation
                        || state.config != *config
                        || state.nested_tools != nested_tools))
            {
                let retired = Arc::clone(&state.service);
                drop(rooms);
                retired.shutdown().await?;
                return Err("code mode context formation or bound prelude changed; a new formation is required".to_string());
            }
        } else {
            if matches!(&context, ResolvedContext::Persistent { .. })
                && operation != CodeModeOperation::Run
            {
                return Err("persistent code mode has no live context to continue".to_string());
            }
            let continuation = if matches!(&context, ResolvedContext::Persistent { .. }) {
                let root =
                    self.environment.durable_var_dir.as_deref().ok_or(
                        "persistent code mode requires its formed durable owner directory",
                    )?;
                Some(Arc::new(ContinuationContext::create(
                    root,
                    formation,
                    config,
                    &context,
                    envelope,
                    OriginalRun::from_envelope(envelope, source_movement_seq)?,
                )?))
            } else {
                None
            };
            let delegate = Arc::new(RoomDelegate {
                admitted_notifications: continuation.is_some(),
                readiness: continuation.clone(),
                owners,
                registration_byte_limit: config.max_output_bytes,
                ..RoomDelegate::default()
            });
            let service = Arc::new(match &context {
                ResolvedContext::Isolated => CodeModeService::with_delegate(delegate.clone()),
                ResolvedContext::Persistent { engine, .. } => {
                    CodeModeService::with_persistent_context(delegate.clone(), engine.clone())?
                }
            });
            rooms.insert(
                formation.worker_environment_id.clone(),
                RoomState {
                    service,
                    delegate,
                    formation: formation.clone(),
                    context,
                    config: config.clone(),
                    nested_tools,
                    continuation,
                    frontiers: Arc::new(Mutex::new(RetainedFrontiers::default())),
                },
            );
        }
        let state = rooms
            .get(&formation.worker_environment_id)
            .ok_or_else(|| "code mode context was not retained".to_string())?;
        Ok(RoomState {
            service: state.service.clone(),
            delegate: state.delegate.clone(),
            formation: state.formation.clone(),
            context: state.context.clone(),
            config: state.config.clone(),
            nested_tools: state.nested_tools.clone(),
            continuation: state.continuation.clone(),
            frontiers: state.frontiers.clone(),
        })
    }

    async fn write_live_state(
        &self,
        sink: &dyn EmissionSink,
        formation: &WorkerEnvironmentFormation,
        state: &RoomState,
        status: &str,
        detail: Option<String>,
    ) -> Result<(), String> {
        let body = json!({
            "schema_version": 1,
            "worker_environment_id": formation.worker_environment_id,
            "worker_environment_kind": formation.worker_environment_kind,
            "substrate_family": SUBSTRATE_FAMILY,
            "status": status,
            "active_cells": state.delegate.active_cell_ids(),
            "pending_tool_calls": state.delegate.pending_snapshot(),
            "detail": detail,
            "context_mode": match &state.context { ResolvedContext::Isolated => "isolated", ResolvedContext::Persistent { .. } => "persistent" },
            "prelude_artifact": match &state.context { ResolvedContext::Isolated => None, ResolvedContext::Persistent { artifact, .. } => Some(artifact) },
        });
        sink.record_live_state(formation, &body).await
    }

    async fn release_room(&self, formation: &WorkerEnvironmentFormation) -> Result<(), String> {
        let Some(state) = self
            .rooms
            .lock()
            .await
            .remove(&formation.worker_environment_id)
        else {
            return Ok(());
        };
        let mut errors = Vec::new();
        if let Some(continuation) = &state.continuation {
            if let Err(error) = continuation.retire() {
                errors.push(error);
            }
        }
        for cell_id in state.delegate.active_cell_ids() {
            if let Err(error) = state.service.terminate(CellId::new(cell_id)).await {
                errors.push(error.to_string());
            }
        }
        if let Err(error) = state.service.shutdown().await {
            errors.push(error);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "code mode room release failed: {}",
                errors.join("; ")
            ))
        }
    }
}

fn evidence_write_error(stage: &str, error: impl Into<String>) -> Value {
    json!({
        "stage": stage,
        "message": error.into(),
    })
}

impl ToolProcessHandler for CodeModeRoom {
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
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<InterfaceArtifactFile>, ToolProcessError>> + Send + 'a>,
    > {
        Box::pin(async move { self.receive(invocation, io).await })
    }

    fn release<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        _kind: &'a WorkerEnvKindSpec,
    ) -> Pin<Box<dyn Future<Output = Result<(), ToolProcessError>> + Send + 'a>> {
        Box::pin(async move {
            self.release_room(formation)
                .await
                .map_err(ToolProcessError::Unavailable)
        })
    }

}

pub async fn run_code_mode_room_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    let room = Arc::new(CodeModeRoom::new(environment));
    octopus_tool_door_runtime::run_tool_process(room).await
}

async fn dispatch(
    state: &RoomState,
    config: &CodeModeConfig,
    request_id: u64,
    source_movement_seq: u64,
    operation: CodeModeOperation,
    call: &ToolCallArtifact,
    envelope: &InterfaceInvocationEnvelope,
    sink: &dyn EmissionSink,
) -> Result<Value, String> {
    match operation {
        CodeModeOperation::Run => {
            run_script(
                state,
                config,
                request_id,
                source_movement_seq,
                call,
                envelope,
                sink,
            )
            .await
        }
        CodeModeOperation::Resume => {
            resume_script(state, config, source_movement_seq, call, envelope, sink).await
        }
        CodeModeOperation::Wait => {
            wait_script(state, config, source_movement_seq, call, envelope, sink).await
        }
        CodeModeOperation::Cancel => cancel_script(state, config, call).await,
    }
}

fn code_mode_operation(
    handler_config: &Value,
    tool: &str,
) -> Result<CodeModeOperation, ToolProcessError> {
    serde_json::from_value::<CodeModeHandlerConfig>(handler_config.clone())
        .map(|config| config.operation)
        .map_err(|error| {
            ToolProcessError::Capability(format!(
                "tool '{tool}' has malformed code mode handler_config: {error}"
            ))
        })
}

async fn run_script(
    state: &RoomState,
    config: &CodeModeConfig,
    request_id: u64,
    source_movement_seq: u64,
    call: &ToolCallArtifact,
    envelope: &InterfaceInvocationEnvelope,
    sink: &dyn EmissionSink,
) -> Result<Value, String> {
    let object = strict_object(
        "code_mode.run",
        &call.arguments,
        &["script", "yield_time_ms"],
    )?;
    let source = required_string(object, "script")?;
    if source.len() > config.max_source_bytes {
        return Err(format!(
            "code mode source is {} bytes, above configured limit {}",
            source.len(),
            config.max_source_bytes
        ));
    }
    let retained_count = if state.continuation.is_some() {
        let mut cells: BTreeSet<_> = state.delegate.active_cell_ids().into_iter().collect();
        cells.extend(
            state
                .frontiers
                .lock()
                .map_err(|_| "code mode frontier buffer is poisoned")?
                .cells
                .keys()
                .cloned(),
        );
        cells.len()
    } else {
        state.delegate.active_count()
    };
    if retained_count >= config.max_cells {
        return Err(format!(
            "code mode room already has its configured maximum of {} active cells",
            config.max_cells
        ));
    }
    let yield_time_ms = config.yield_time(optional_u64(object, "yield_time_ms")?)?;
    let enabled_tools = enabled_nested_tools(config, envelope)?;
    let original = state
        .continuation
        .as_ref()
        .map(|_| OriginalRun::from_envelope(envelope, source_movement_seq))
        .transpose()?;
    let started = state
        .service
        .execute(ExecuteRequest {
            tool_call_id: call
                .correlation_id
                .clone()
                .unwrap_or_else(|| format!("request-{request_id}")),
            enabled_tools,
            source: source.to_string(),
            yield_time_ms: Some(yield_time_ms),
            max_output_tokens: None,
        })
        .await
        .map_err(|error| format!("code mode script refused: {error}"))?;
    let cell_id = started.cell_id.clone();
    if let Some(original) = original {
        state.delegate.register_origin(&cell_id, original)?;
    }
    let response =
        observe_with_notifications(state, config, started.initial_response(), envelope, sink)
            .await
            .map_err(|error| {
                format!("code mode script failed before its first frontier: {error}")
            })?;
    if matches!(response, RuntimeResponse::Yielded { .. }) {
        state.delegate.register_active(&cell_id);
    }
    finish_frontier(state, config, cell_id, response, envelope, sink).await
}

async fn resume_script(
    state: &RoomState,
    config: &CodeModeConfig,
    source_movement_seq: u64,
    call: &ToolCallArtifact,
    envelope: &InterfaceInvocationEnvelope,
    sink: &dyn EmissionSink,
) -> Result<Value, String> {
    if let Some(continuation) = &state.continuation {
        if !state.service.is_alive() {
            return Err("code mode context_lost: original heap is not alive".into());
        }
        let object = strict_object(
            "code_mode.resume",
            &call.arguments,
            &["cell_id", "tool_call_id", "result"],
        )?;
        let cell_id = CellId::new(required_string(object, "cell_id")?.to_string());
        let runtime_id = required_string(object, "tool_call_id")?;
        let admitted = if let Some((application, decision)) =
            continuation.application_input(envelope)?
        {
            if decision {
                let boundary = emit_artifact(envelope,"code_mode.application_boundary",&serde_json::to_vec(&json!({"phase":"retire","pending_digest":application.pending_digest,"request_id":application.request.request_id,"worker_environment_id":state.formation.worker_environment_id})).map_err(|e| e.to_string())?)?;
                match sink.arrive(&boundary).await? {
                    DeliveryVerdict::Admitted { .. } => {}
                    refusal => return Err(format!("application retirement refused: {refusal:?}")),
                }
            }
            let Some(admitted) = continuation.finish_application(&application)? else {
                return Ok(
                    json!({"state":"application_continuation_pending","cell_id":cell_id.as_str(),"request_id":application.request.request_id,"output_retained":true}),
                );
            };
            admitted
        } else if let Some(application) =
            continuation.suspend_application(envelope, source_movement_seq)?
        {
            state.delegate.require_pending(&cell_id, runtime_id)?;
            let boundary = emit_artifact(envelope,"code_mode.application_boundary",&serde_json::to_vec(&json!({"phase":"request","pending_digest":application.pending_digest,"request_id":application.request.request_id,"worker_environment_id":state.formation.worker_environment_id})).map_err(|e| e.to_string())?)?;
            match sink.arrive(&boundary).await? {
                DeliveryVerdict::Admitted { .. } => {}
                refusal => return Err(format!("application request refused: {refusal:?}")),
            }
            return Ok(
                json!({"state":"application_approval_pending","cell_id":cell_id.as_str(),"request_id":application.request.request_id,"output_retained":true}),
            );
        } else {
            state.delegate.require_pending(&cell_id, runtime_id)?;
            let result: ToolResultArtifact = serde_json::from_slice(&envelope.read_artifact_payload_bounded(config.max_output_bytes as u64).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
            if result.body.as_ref().is_some_and(|body| body.get("generated_artifacts").is_some()) {
                let boundary = emit_artifact(envelope,"code_mode.artifact_boundary",&serde_json::to_vec(&json!({"correlation_id":format!("code-mode-artifact:{}:{}",state.formation.worker_environment_id,envelope.artifact.content.to_hex()),"source_artifact":envelope.artifact,"source_movement_seq":source_movement_seq})).map_err(|e|e.to_string())?)?;
                match sink.arrive(&boundary).await? {
                    DeliveryVerdict::Admitted { .. } => {}
                    refusal => return Err(format!("generated artifact read binding refused: {refusal:?}")),
                }
            }
            continuation.accept(envelope, &state.formation, source_movement_seq)?
        };
        if admitted.pending.cell_id() != cell_id.as_str()
            || admitted.pending.runtime_tool_call_id() != runtime_id
        {
            return Err(
                "code mode authenticated pending identity changed before settlement".into(),
            );
        }
        let result = serde_json::to_value(&admitted.result).map_err(|error| error.to_string())?;
        state.delegate.resolve(&cell_id, runtime_id, Ok(result))?;
        let response = observe_with_notifications(
            state,
            config,
            state.service.wait(WaitRequest {
                cell_id: cell_id.clone(),
                yield_time_ms: config.default_yield_time_ms,
            }),
            envelope,
            sink,
        )
        .await?;
        let response = require_live_wait("admitted resume", response)?;
        let body =
            finish_frontier(state, config, cell_id.clone(), response, envelope, sink).await?;
        if let Err(error) = retain_frontier(state, config, &cell_id, body) {
            continuation.retire()?;
            state.service.shutdown().await?;
            return Err(error);
        }
        return Ok(
            json!({"state":"continuation_admitted","cell_id":cell_id.as_str(),"tool_call_id":runtime_id,"result_artifact":envelope.artifact,"output_retained":true}),
        );
    }
    let object = strict_object(
        "code_mode.resume",
        &call.arguments,
        &[
            "cell_id",
            "tool_call_id",
            "result",
            "error",
            "yield_time_ms",
        ],
    )?;
    let cell_id = CellId::new(required_string(object, "cell_id")?.to_string());
    let tool_call_id = required_string(object, "tool_call_id")?;
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");
    if has_result == has_error {
        return Err("code_mode.resume requires exactly one of result or error".to_string());
    }
    let resolution = if has_result {
        Ok(object.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(required_string(object, "error")?.to_string())
    };
    state.delegate.resolve(&cell_id, tool_call_id, resolution)?;
    let yield_time_ms = config.yield_time(optional_u64(object, "yield_time_ms")?)?;
    let response = state
        .service
        .wait(WaitRequest {
            cell_id: cell_id.clone(),
            yield_time_ms,
        })
        .await
        .map_err(|error| format!("code mode resume failed: {error}"))?;
    let response = require_live_wait("resume", response)?;
    if matches!(response, RuntimeResponse::Yielded { .. }) {
        state.delegate.register_active(&cell_id);
    }
    finish_frontier(state, config, cell_id, response, envelope, sink).await
}

async fn wait_script(
    state: &RoomState,
    config: &CodeModeConfig,
    source_movement_seq: u64,
    call: &ToolCallArtifact,
    envelope: &InterfaceInvocationEnvelope,
    sink: &dyn EmissionSink,
) -> Result<Value, String> {
    let object = strict_object(
        "code_mode.wait",
        &call.arguments,
        &["cell_id", "yield_time_ms"],
    )?;
    let cell_id = CellId::new(required_string(object, "cell_id")?.to_string());
    let yield_time_ms = config.yield_time(optional_u64(object, "yield_time_ms")?)?;
    let is_ready = envelope
        .read_artifact_manifest()
        .map_err(|e| e.to_string())?
        .label
        == "code_mode.frontier_ready";
    let ready = if is_ready {
        let context = state
            .continuation
            .as_ref()
            .ok_or("readiness has no persistent context")?;
        if !state.service.is_alive() {
            return Err("code mode context_lost: readiness cannot restore a lost heap".into());
        }
        let ready = context.accept_ready(envelope, source_movement_seq)?;
        if ready.cell_id != cell_id.as_str() {
            return Err("readiness changed its live cell".into());
        }
        if context.ready_observed(&ready.identity()?)? {
            return Ok(
                json!({"state":"readiness_observed","cell_id":cell_id.as_str(),"output_retained":true,"readiness":ready.identity()?}),
            );
        }
        Some(ready)
    } else {
        None
    };
    if ready.is_none() {
        if let Some(body) = take_retained_frontier(state, &cell_id)? {
            return Ok(body);
        }
    }
    let response = observe_with_notifications(
        state,
        config,
        state.service.wait(WaitRequest {
            cell_id: cell_id.clone(),
            yield_time_ms,
        }),
        envelope,
        sink,
    )
    .await
    .map_err(|error| format!("code mode wait failed: {error}"))?;
    let response = require_live_wait("wait", response)?;
    if matches!(response, RuntimeResponse::Yielded { .. }) {
        state.delegate.register_active(&cell_id);
    }
    let body = finish_frontier(state, config, cell_id.clone(), response, envelope, sink).await?;
    if let Some(ready) = ready {
        retain_frontier(state, config, &cell_id, body)?;
        state
            .continuation
            .as_ref()
            .ok_or("readiness context lost")?
            .observe_ready(&ready.identity()?)?;
        Ok(
            json!({"state":"readiness_observed","cell_id":cell_id.as_str(),"output_retained":true,"readiness":ready.identity()?}),
        )
    } else {
        Ok(body)
    }
}

async fn cancel_script(
    state: &RoomState,
    config: &CodeModeConfig,
    call: &ToolCallArtifact,
) -> Result<Value, String> {
    let object = strict_object("code_mode.cancel", &call.arguments, &["cell_id"])?;
    let cell_id = CellId::new(required_string(object, "cell_id")?.to_string());
    let response = state
        .service
        .terminate(cell_id.clone())
        .await
        .map_err(|error| format!("code mode cancel failed: {error}"))?;
    let response = require_live_wait("cancel", response)?;
    let body = frontier_body(
        &cell_id,
        response,
        Vec::new(),
        state.delegate.pending_ids(&cell_id),
    );
    enforce_output(config, &body)?;
    Ok(body)
}

async fn finish_frontier(
    state: &RoomState,
    config: &CodeModeConfig,
    cell_id: CellId,
    response: RuntimeResponse,
    envelope: &InterfaceInvocationEnvelope,
    sink: &dyn EmissionSink,
) -> Result<Value, String> {
    let result = materialize_frontier(state, config, cell_id, response, envelope, sink).await;
    if result.is_err() {
        if let Some(context) = &state.continuation {
            context.retire()?;
            state.service.shutdown().await?;
        }
    }
    result
}

async fn materialize_frontier(
    state: &RoomState,
    config: &CodeModeConfig,
    cell_id: CellId,
    response: RuntimeResponse,
    envelope: &InterfaceInvocationEnvelope,
    sink: &dyn EmissionSink,
) -> Result<Value, String> {
    if state.continuation.is_none() {
        if let RuntimeResponse::Result {
            error_text: Some(error),
            ..
        } = &response
        {
            return Err(format!("code mode script failed: {error}"));
        }
    }
    let routes = config.route_map();
    let mut emissions = Vec::new();
    for invocation in state.delegate.take_unemitted(&cell_id, &routes)? {
        let route = routes
            .get(&invocation.tool_name.to_string())
            .ok_or_else(|| {
                format!(
                    "code mode nested tool '{}' has no configured route",
                    invocation.tool_name
                )
            })?;
        let pending = if let Some(continuation) = &state.continuation {
            Some(
                continuation.prepare(
                    state.delegate.origin(&cell_id)?,
                    cell_id.as_str(),
                    &invocation.runtime_tool_call_id,
                    invocation.enqueue_ordinal,
                    &invocation.tool_name.to_string(),
                    invocation
                        .input
                        .clone()
                        .ok_or("code mode nested tool omitted its required argument object")?,
                    route.clone(),
                )?,
            )
        } else {
            None
        };
        let artifact = if let Some(pending) = &pending {
            emit_artifact(
                envelope,
                TOOL_CALL_LABEL,
                &serde_json::to_vec(&pending.tool_call()).map_err(|e| e.to_string())?,
            )?
        } else {
            nested_tool_artifact(envelope, &invocation)?
        };
        let emitted_ref = continuation::artifact_ref(&artifact)?;
        let artifact = SystemInterfaceEmissionArtifact::Materialized(artifact);
        let to_interface = InterfaceId::new(&route.interface);
        let delivery = if pending.is_some() {
            let SystemInterfaceEmissionArtifact::Materialized(file) = &artifact else {
                return Err("code mode pending emission lost its artifact".into());
            };
            sink.arrive(file).await
        } else {
            sink.hand(&artifact, &to_interface).await
        };
        if let (Some(context), Some(pending)) = (&state.continuation, &pending) {
            context.record_emission(pending, emitted_ref, &delivery)?;
        }
        let verdict = delivery?;
        let verdict_json = serde_json::to_value(&verdict)
            .map_err(|error| format!("code mode delivery verdict did not serialize: {error}"))?;
        emissions.push(json!({
            "tool": invocation.tool_name.to_string(),
            "tool_call_id": invocation.runtime_tool_call_id,
            "enqueue_ordinal": invocation.enqueue_ordinal,
            "correlation_id": pending.as_ref().map(|pending| pending.correlation()),
            "interface": route.interface,
            "checkpoint": route.checkpoint,
            "delivery": verdict_json,
        }));
        if let DeliveryVerdict::Refused { reason, detail } = verdict {
            let _ = state.delegate.resolve(
                &cell_id,
                &invocation.runtime_tool_call_id,
                Err(format!(
                    "nested tool delivery refused ({}): {}",
                    reason.as_str(),
                    detail
                )),
            );
        }
    }
    state.delegate.observe_readiness(
        &cell_id,
        !matches!(response, RuntimeResponse::Yielded { .. }),
    )?;
    let pending = state.delegate.pending_ids(&cell_id);
    let body = frontier_body(&cell_id, response, emissions, pending);
    if let Err(error) = enforce_output(config, &body) {
        let _ = state.service.terminate(cell_id).await;
        return Err(error);
    }
    Ok(body)
}

fn retain_frontier(
    state: &RoomState,
    config: &CodeModeConfig,
    cell_id: &CellId,
    body: Value,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?.len();
    let mut retained = state
        .frontiers
        .lock()
        .map_err(|_| "code mode frontier buffer is poisoned")?;
    let next = retained
        .bytes
        .checked_add(bytes)
        .ok_or("code mode retained frontier size overflow")?;
    if next > config.max_output_bytes {
        return Err("code mode retained model output exceeds its configured byte limit".into());
    }
    if !retained.cells.contains_key(cell_id.as_str()) && retained.cells.len() >= config.max_cells {
        return Err("code mode retained frontier count exceeds its configured cell limit".into());
    }
    retained
        .cells
        .entry(cell_id.as_str().to_string())
        .or_default()
        .push_back(body);
    retained.bytes = next;
    Ok(())
}

fn take_retained_frontier(state: &RoomState, cell_id: &CellId) -> Result<Option<Value>, String> {
    let mut retained = state
        .frontiers
        .lock()
        .map_err(|_| "code mode frontier buffer is poisoned")?;
    let body = retained
        .cells
        .get_mut(cell_id.as_str())
        .and_then(VecDeque::pop_front);
    if let Some(body) = &body {
        let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?.len();
        retained.bytes = retained
            .bytes
            .checked_sub(bytes)
            .ok_or("code mode retained frontier accounting changed")?;
    }
    if retained
        .cells
        .get(cell_id.as_str())
        .is_some_and(VecDeque::is_empty)
    {
        retained.cells.remove(cell_id.as_str());
    }
    Ok(body)
}

async fn observe_with_notifications<T>(
    state: &RoomState,
    config: &CodeModeConfig,
    observation: impl Future<Output = Result<T, String>> + Send,
    envelope: &InterfaceInvocationEnvelope,
    sink: &dyn EmissionSink,
) -> Result<T, String> {
    tokio::pin!(observation);
    loop {
        let ready = state.delegate.notification_ready.notified();
        tokio::pin!(ready);
        ready.as_mut().enable();
        if let Some(notification) = state.delegate.take_notification()? {
            if notification.responder.is_closed() {
                continue;
            }
            let outcome = admit_notification(state, config, &notification, envelope, sink).await;
            let failure = outcome.as_ref().err().cloned();
            let _ = notification.responder.send(outcome);
            if let Some(error) = failure {
                if let Some(context) = &state.continuation {
                    context.retire()?;
                }
                state.service.shutdown().await?;
                return Err(error);
            }
            continue;
        }
        tokio::select! {
            result = &mut observation => return result,
            _ = &mut ready => {}
        }
    }
}

async fn admit_notification(
    state: &RoomState,
    config: &CodeModeConfig,
    notification: &PendingNotification,
    envelope: &InterfaceInvocationEnvelope,
    sink: &dyn EmissionSink,
) -> Result<(), String> {
    let context = state
        .continuation
        .as_ref()
        .ok_or("code mode notification has no persistent context")?;
    let target = config
        .continuation
        .as_ref()
        .and_then(|c| c.notification_interface())
        .ok_or("code mode notification has no declared recipient interface")?;
    let metadata = envelope.metadata().map_err(|e| e.to_string())?;
    if !metadata
        .grant
        .interfaces
        .iter()
        .any(|interface| interface == target)
    {
        return Err("code mode notification recipient is not in this invocation's grant".into());
    }
    let original = state.delegate.origin(&notification.cell_id)?;
    if original.correlation_id != notification.call_id {
        return Err("code mode notification has a different original call identity".into());
    }
    let bytes = serde_json::to_vec(&json!({
        "schema_version":1,"context_generation":context.generation(),
        "cell_id":notification.cell_id.as_str(),"call_id":notification.call_id,
        "notification_id":notification.sequence,"text":notification.text,
    }))
    .map_err(|e| e.to_string())?;
    if bytes.len() > config.max_output_bytes {
        return Err("code mode notification exceeds its output byte limit".into());
    }
    let artifact = emit_artifact(envelope, "code_mode.notification", &bytes)?;
    match sink.arrive(&artifact).await? {
        DeliveryVerdict::Admitted { .. } => Ok(()),
        DeliveryVerdict::Refused { reason, detail } => Err(format!(
            "code mode notification refused ({}): {detail}",
            reason.as_str()
        )),
    }
}

pub fn decode_code_mode_door_artifact(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    envelope: &InterfaceInvocationEnvelope,
    purpose: octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose,
) -> Result<Option<ToolCallArtifact>, octopus_tool_door_runtime::ToolDoorArtifactDecodeError> {
    let checked_error = |error| match error {
        octopus_run_authoring::CheckedRunError::AwaitingResults { call_sequences } =>
            octopus_tool_door_runtime::ToolDoorArtifactDecodeError::AwaitingResults { call_sequences },
        error => octopus_tool_door_runtime::ToolDoorArtifactDecodeError::Invalid(error.to_string()),
    };
    let load_config = || -> Result<CodeModeConfig, String> {
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(
            &ctx.interface_registry_dir().join(config::CONFIG_FILE),
            1024 * 1024,
        )
        .map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes)
            .map_err(|e| format!("code mode owner config is malformed: {e}"))
    };
    if envelope
        .read_artifact_manifest()
        .map_err(|e| e.to_string())?
        .label
        == "code_mode.frontier_ready"
    {
        let ready = continuation::read_ready(envelope)?;
        let root = octopus_tool_door_runtime::tool_door_durable_state_dir(
            ctx,
            &ready.worker_environment_id,
        )?;
        let current =
            if purpose == octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch {
                Some(load_config()?)
            } else {
                None
            };
        if let Some(config) = current.as_ref() {
            continuation::reconcile_checked_provider_advance(ctx,&root,config,Some(envelope)).map_err(checked_error)?;
        }
        return continuation::decode_ready(
            &root,
            envelope,
            ctx.source_movement(envelope)?,
            purpose,
            current.as_ref(),
        )
        .map(Some).map_err(Into::into);
    }
    if continuation::consume_authoring_result(ctx, envelope)? {
        return Ok(None);
    }
    if continuation::is_internal_receipt(envelope)? {
        continuation::validate_internal_receipt(ctx, envelope)?;
        return Ok(None);
    }
    if let Some(worker) = continuation::result_locator(envelope)? {
        let root = octopus_tool_door_runtime::tool_door_durable_state_dir(ctx, &worker)?;
        let current =
            if purpose == octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch {
                Some(load_config()?)
            } else {
                None
            };
        if let Some(config) = current.as_ref() {
            continuation::reconcile_checked_provider_advance(ctx,&root,config,Some(envelope)).map_err(checked_error)?;
        }
        return continuation::decode_result(
            &root,
            ctx.owner_interface(),
            envelope,
            purpose,
            current.as_ref(),
            ctx.source_movement(envelope)?,
        )
        .map(Some).map_err(Into::into);
    }
    let call =
        octopus_tool_door_runtime::default_tool_door_artifact_decoder(ctx, envelope, purpose)?;
    if purpose == octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch {
        if let Some(call) = &call {
            let config = load_config()?;
            config.validate(ctx.owner_interface().as_str())?;
            if config.handoff.is_some() {
                let topology = config.resolve_owner_topology(envelope)?;
                continuation::validate_admitted_coauthor(ctx, envelope, &topology)?;
            }
            if config
                .continuation
                .as_ref()
                .is_some_and(|continuation| continuation.resume_tool() == call.tool)
            {
                return Err("code mode continuation accepts only the original admitted nested ToolResult artifact".into());
            }
        }
    }
    Ok(call)
}

fn require_live_wait(operation: &str, outcome: WaitOutcome) -> Result<RuntimeResponse, String> {
    match outcome {
        WaitOutcome::LiveCell(response) => Ok(response),
        WaitOutcome::MissingCell(_) => Err(format!(
            "code mode {operation} refers to a cell that is not live"
        )),
    }
}

fn frontier_body(
    cell_id: &CellId,
    response: RuntimeResponse,
    emissions: Vec<Value>,
    pending_tool_calls: Vec<String>,
) -> Value {
    match response {
        RuntimeResponse::Result {
            content_items,
            error_text,
            ..
        } => json!({
            "state": "completed",
            "cell_id": cell_id.as_str(),
            "output": content_items_json(content_items),
            "error_text": error_text,
            "pending_tool_calls": pending_tool_calls,
            "emissions": emissions,
        }),
        RuntimeResponse::Yielded { content_items, .. } => json!({
            "state": "yielded",
            "cell_id": cell_id.as_str(),
            "output": content_items_json(content_items),
            "pending_tool_calls": pending_tool_calls,
            "emissions": emissions,
        }),
        RuntimeResponse::Terminated { content_items, .. } => json!({
            "state": "terminated",
            "cell_id": cell_id.as_str(),
            "output": content_items_json(content_items),
            "pending_tool_calls": pending_tool_calls,
            "emissions": emissions,
        }),
    }
}

fn materialize_output_media(
    envelope: &InterfaceInvocationEnvelope,
    config: &CodeModeConfig,
    body: &mut Value,
) -> Result<(), String> {
    let Some(output) = body.get("output").and_then(Value::as_array) else {
        return Ok(());
    };
    if body.get("media").is_some() {
        return Err("code mode output already contains a media carrier".into());
    }
    let mut prepared = Vec::new();
    let mut total = 0usize;
    for (index, item) in output.iter().enumerate() {
        if item.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        if prepared.len() == 16 {
            return Err("code mode output exceeds 16 images".into());
        }
        let url = item
            .get("image_url")
            .and_then(Value::as_str)
            .ok_or("code mode image has no data URL")?;
        let (header, payload) = url
            .split_once(',')
            .ok_or("code mode image data URL is malformed")?;
        let (mime, format) = match header {
            "data:image/png;base64" => ("image/png", image::ImageFormat::Png),
            "data:image/jpeg;base64" => ("image/jpeg", image::ImageFormat::Jpeg),
            "data:image/webp;base64" => ("image/webp", image::ImageFormat::WebP),
            _ => return Err("code mode images require inline PNG, JPEG or WebP bytes".into()),
        };
        let remaining = config
            .max_output_bytes
            .checked_sub(total)
            .ok_or("code mode media exceeds its output byte limit")?;
        if payload.len() > remaining.div_ceil(3) * 4 {
            return Err("code mode image exceeds its bounded transport".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|e| format!("invalid code mode image base64: {e}"))?;
        if bytes.is_empty()
            || bytes.len() > remaining
            || image::guess_format(&bytes).map_err(|e| e.to_string())? != format
        {
            return Err("code mode image bytes differ from their declared format or size".into());
        }
        let dimensions = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
            .into_dimensions()
            .map_err(|e| format!("invalid code mode image dimensions: {e}"))?;
        if dimensions.0 == 0
            || dimensions.1 == 0
            || u64::from(dimensions.0) * u64::from(dimensions.1) * 4 > 64 * 1024 * 1024
        {
            return Err("code mode image exceeds its decoded pixel budget".into());
        }
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format);
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        reader
            .decode()
            .map_err(|e| format!("code mode image cannot be decoded within its byte limit: {e}"))?;
        total += bytes.len();
        prepared.push((index, mime, payload.to_string(), bytes, dimensions));
    }
    if prepared.is_empty() {
        return Ok(());
    }
    let parent = envelope
        .artifact_file
        .manifest_path()
        .parent()
        .ok_or("code mode input artifact has no parent")?;
    let mut media = Vec::with_capacity(prepared.len());
    for (index, mime, payload, bytes, (width, height)) in prepared {
        let artifact =
            InterfaceArtifactFile::write(parent.join("emitted/images"), "code_mode.image", &bytes)
                .map_err(|e| e.to_string())?;
        let manifest = artifact.read_manifest().map_err(|e| e.to_string())?;
        let item = body
            .get_mut("output")
            .and_then(Value::as_array_mut)
            .and_then(|v| v.get_mut(index))
            .and_then(Value::as_object_mut)
            .ok_or("code mode image output changed during materialization")?;
        item.remove("image_url");
        item.insert(
            "artifact".into(),
            serde_json::to_value(&manifest).map_err(|e| e.to_string())?,
        );
        item.insert("mime_type".into(), json!(mime));
        item.insert("width".into(), json!(width));
        item.insert("height".into(), json!(height));
        item.insert("media_index".into(), json!(media.len()));
        media.push(json!({"mime_type":mime,"artifact":manifest,"payload_base64":payload}));
    }
    body["media"] = Value::Array(media);
    enforce_output(config, body)
}

fn content_items_json(items: Vec<FunctionCallOutputContentItem>) -> Vec<Value> {
    items
        .into_iter()
        .map(|item| match item {
            FunctionCallOutputContentItem::InputText { text } => json!({
                "type": "text",
                "text": text,
            }),
            FunctionCallOutputContentItem::InputImage { image_url, detail } => json!({
                "type": "image",
                "image_url": image_url,
                "detail": detail,
            }),
        })
        .collect()
}

fn enabled_nested_tools(
    config: &CodeModeConfig,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<Vec<ToolDefinition>, String> {
    let metadata = envelope.metadata().map_err(|error| error.to_string())?;
    let grant = metadata.grant();
    let mut specs = BTreeMap::new();
    for (index, spec) in grant.interface_affordance_specs.iter().enumerate() {
        let name = spec.name.as_str();
        if name.is_empty() {
            return Err(format!("code mode affordance spec[{index}] has no name"));
        }
        if specs.insert(name.to_string(), spec).is_some() {
            return Err(format!("code mode metadata repeats affordance '{name}'"));
        }
    }
    let hard_blocked = grant
        .hard_blocked_tools
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let granted = grant
        .tools
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut normalized = BTreeSet::new();
    let mut tools = Vec::new();
    for route in &config.routes {
        if !granted.contains(route.tool.as_str()) || hard_blocked.contains(route.tool.as_str()) {
            continue;
        }
        let spec = specs.get(&route.tool).ok_or_else(|| {
            format!(
                "code mode route '{}' is granted but has no projected affordance spec",
                route.tool
            )
        })?;
        let description = spec.description.clone();
        let input_schema = if spec.args_schema.is_object() {
            spec.args_schema.clone()
        } else {
            return Err(format!(
                "code mode affordance '{}' has no object args_schema",
                route.tool
            ));
        };
        let name = normalize_code_mode_identifier(&route.tool);
        if !normalized.insert(name.clone()) {
            return Err(format!(
                "code mode routes collide after identifier normalization at '{name}'"
            ));
        }
        tools.push(ToolDefinition {
            name,
            tool_name: ToolName::plain(&route.tool),
            description,
            kind: CodeModeToolKind::Function,
            input_schema: Some(input_schema),
            output_schema: None,
        });
    }
    Ok(tools)
}

fn nested_tool_artifact(
    input: &InterfaceInvocationEnvelope,
    invocation: &CodeModeNestedToolCall,
) -> Result<InterfaceArtifactFile, String> {
    let parent = input
        .artifact_file
        .manifest_path()
        .parent()
        .ok_or_else(|| "code mode input manifest has no parent".to_string())?;
    let payload = serde_json::to_vec(&json!({
        "tool": invocation.tool_name.to_string(),
        "arguments": invocation.input.clone().unwrap_or(Value::Null),
        "correlation_id": invocation.runtime_tool_call_id,
    }))
    .map_err(|error| format!("code mode nested tool call did not serialize: {error}"))?;
    InterfaceArtifactFile::write(parent.join("nested"), TOOL_CALL_LABEL, &payload)
        .map_err(|error| format!("code mode nested tool artifact failed: {error}"))
}

fn enforce_output(config: &CodeModeConfig, body: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(body)
        .map_err(|error| format!("code mode frontier did not serialize: {error}"))?;
    if bytes.len() > config.max_output_bytes {
        return Err(format!(
            "code mode frontier is {} bytes, above configured limit {}",
            bytes.len(),
            config.max_output_bytes
        ));
    }
    Ok(())
}

#[derive(Default)]
struct RoomDelegate {
    state: Mutex<DelegateState>,
    admitted_notifications: bool,
    readiness: Option<Arc<ContinuationContext>>,
    notification_ready: tokio::sync::Notify,
    owners: Option<BTreeMap<String, config::DeclaredToolOwner>>,
    registration_byte_limit: usize,
    #[cfg(test)]
    registration_hold: Mutex<Option<(u64, tokio::sync::oneshot::Receiver<()>)>>,
    #[cfg(test)]
    registration_changed: tokio::sync::Notify,
}

#[derive(Default)]
struct DelegateState {
    active_cells: BTreeSet<String>,
    pending: BTreeMap<(String, String), PendingCall>,
    origins: BTreeMap<String, OriginalRun>,
    notifications: VecDeque<PendingNotification>,
    next_notification: u64,
    next_readiness: u64,
    completed_cells: BTreeSet<String>,
    ready_keys: BTreeMap<String, Vec<String>>,
    registrations: BTreeMap<String, CellRegistrations>,
    failed_cells: BTreeSet<String>,
}

#[derive(Default)]
struct CellRegistrations {
    prefix: u64,
    waiting: BTreeSet<u64>,
    issued: BTreeSet<String>,
    bytes: usize,
}

impl CellRegistrations {
    fn register(&mut self, invocation: &CodeModeNestedToolCall, maximum_bytes: usize) -> Result<(), String> {
        let ordinal = invocation.enqueue_ordinal;
        let bytes = self.bytes.checked_add(invocation.runtime_tool_call_id.len())
            .and_then(|n| n.checked_add(std::mem::size_of::<u64>()))
            .ok_or("nested registration evidence overflow")?;
        if ordinal == 0 || ordinal <= self.prefix || self.waiting.contains(&ordinal)
            || self.issued.contains(&invocation.runtime_tool_call_id)
            || (maximum_bytes != 0 && bytes > maximum_bytes)
        {
            return Err("nested registration repeats an issued identity or exceeds its evidence bound".into());
        }
        self.bytes = bytes;
        self.issued.insert(invocation.runtime_tool_call_id.clone());
        self.waiting.insert(ordinal);
        while let Some(next) = self.prefix.checked_add(1) {
            if !self.waiting.remove(&next) { break; }
            self.prefix = next;
        }
        Ok(())
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum BusyOwner {
    Declared(String, config::DeclaredToolOwner),
    Interface(String),
}

struct PendingNotification {
    cell_id: CellId,
    call_id: String,
    sequence: u64,
    text: String,
    responder: tokio::sync::oneshot::Sender<Result<(), String>>,
}

struct PendingCall {
    invocation: CodeModeNestedToolCall,
    responder: Option<tokio::sync::oneshot::Sender<Result<Value, String>>>,
    emitted: bool,
}

impl RoomDelegate {
    fn busy_owner(&self, route: &config::CodeModeRoute) -> Result<BusyOwner, String> {
        match &self.owners {
            Some(owners) => Ok(BusyOwner::Declared(
                self.readiness.as_ref().ok_or("declared nested owner lost its persistent context")?.generation().into(),
                owners.get(&route.tool).cloned().ok_or("nested tool lost its pinned process owner")?,
            )),
            None => Ok(BusyOwner::Interface(route.interface.clone())),
        }
    }
    fn register_origin(&self, cell_id: &CellId, original: OriginalRun) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "code mode state is poisoned")?;
        if state
            .origins
            .insert(cell_id.as_str().to_string(), original)
            .is_some()
        {
            return Err("code mode cell origin was already registered".into());
        }
        let events = state.pending.values().filter(|p| p.invocation.cell_id == *cell_id).map(|p| json!({"tool_call_id":p.invocation.runtime_tool_call_id,"enqueue_ordinal":p.invocation.enqueue_ordinal,"tool":p.invocation.tool_name.to_string(),"arguments":p.invocation.input})).collect::<Vec<_>>();
        for event in events {
            self.publish_readiness(&mut state, cell_id, event)?;
        }
        let notifications = state
            .notifications
            .iter()
            .filter(|n| n.cell_id == *cell_id)
            .map(|n| json!({"notification_id":n.sequence,"text":n.text}))
            .collect::<Vec<_>>();
        for event in notifications {
            self.publish_readiness(&mut state, cell_id, event)?;
        }
        if state.completed_cells.contains(cell_id.as_str()) {
            self.publish_readiness(&mut state, cell_id, json!({"completion_ready":true}))?;
        }
        Ok(())
    }

    fn publish_readiness(
        &self,
        state: &mut DelegateState,
        cell: &CellId,
        event: Value,
    ) -> Result<(), String> {
        let Some(context) = &self.readiness else {
            return Ok(());
        };
        let Some(original) = state.origins.get(cell.as_str()).cloned() else {
            return Ok(());
        };
        state.next_readiness = state
            .next_readiness
            .checked_add(1)
            .ok_or("readiness sequence exhausted")?;
        if let Some(key) =
            context.publish_ready(cell.as_str(), original, state.next_readiness, event)?
        {
            state
                .ready_keys
                .entry(cell.to_string())
                .or_default()
                .push(key);
        }
        Ok(())
    }

    fn observe_readiness(&self, cell: &CellId, completed: bool) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "readiness state is poisoned")?;
        if let Some(context) = &self.readiness {
            let mut remaining = Vec::new();
            for key in state.ready_keys.get(cell.as_str()).into_iter().flatten() {
                let ready = context.ready_record(key)?;
                let pending = ready
                    .event
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| {
                        state
                            .pending
                            .get(&(cell.to_string(), id.to_string()))
                            .is_some_and(|p| !p.emitted)
                    });
                let notification = ready
                    .event
                    .get("notification_id")
                    .and_then(Value::as_u64)
                    .is_some_and(|id| {
                        state
                            .notifications
                            .iter()
                            .any(|n| n.cell_id == *cell && n.sequence == id)
                    });
                let completion =
                    ready.event.get("completion_ready") == Some(&Value::Bool(true)) && !completed;
                if pending || notification || completion {
                    remaining.push(key.clone());
                } else {
                    context.observe_ready(key)?;
                }
            }
            state.ready_keys.insert(cell.to_string(), remaining);
        }
        Ok(())
    }

    fn origin(&self, cell_id: &CellId) -> Result<OriginalRun, String> {
        self.state
            .lock()
            .map_err(|_| "code mode state is poisoned")?
            .origins
            .get(cell_id.as_str())
            .cloned()
            .ok_or_else(|| "code mode cell has no original admitted Run".into())
    }

    fn require_pending(&self, cell_id: &CellId, runtime_id: &str) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "code mode state is poisoned")?;
        if !state
            .pending
            .get(&(cell_id.as_str().to_string(), runtime_id.to_string()))
            .is_some_and(|pending| {
                pending.emitted
                    && pending
                        .responder
                        .as_ref()
                        .is_some_and(|sender| !sender.is_closed())
            })
        {
            return Err("code mode context_lost or no exact pending Promise".into());
        }
        Ok(())
    }

    fn take_notification(&self) -> Result<Option<PendingNotification>, String> {
        Ok(self
            .state
            .lock()
            .map_err(|_| "code mode state is poisoned")?
            .notifications
            .pop_front())
    }

    fn register_active(&self, cell_id: &CellId) {
        if let Ok(mut state) = self.state.lock() {
            state.active_cells.insert(cell_id.as_str().to_string());
        }
    }

    fn active_count(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.active_cells.len())
            .unwrap_or(usize::MAX)
    }

    fn active_cell_ids(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.active_cells.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn pending_ids(&self, cell_id: &CellId) -> Vec<String> {
        self.state
            .lock()
            .map(|state| {
                state
                    .pending
                    .keys()
                    .filter(|(cell, _)| cell == cell_id.as_str())
                    .map(|(_, call)| call.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn pending_snapshot(&self) -> Vec<Value> {
        self.state
            .lock()
            .map(|state| {
                state
                    .pending
                    .iter()
                    .map(|((cell, call), pending)| {
                        json!({
                            "cell_id": cell,
                            "tool_call_id": call,
                            "enqueue_ordinal": pending.invocation.enqueue_ordinal,
                            "tool": pending.invocation.tool_name.to_string(),
                            "emitted": pending.emitted,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn take_unemitted(
        &self,
        cell_id: &CellId,
        routes: &BTreeMap<String, config::CodeModeRoute>,
    ) -> Result<Vec<CodeModeNestedToolCall>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "code mode delegate state is poisoned")?;
        let mut busy = BTreeSet::new();
        if state.failed_cells.contains(cell_id.as_str()) {
            return Err("code mode cell lost its issued registration or pending Promise".into());
        }
        let prefix = state.registrations.get(cell_id.as_str()).map(|r| r.prefix).unwrap_or(0);
        for pending in state.pending.values().filter(|p| p.emitted) {
            busy.insert(self.busy_owner(
                routes
                    .get(&pending.invocation.tool_name.to_string())
                    .ok_or("pending call lost its exact route")?
            )?);
        }
        let mut result = Vec::new();
        let mut eligible = state.pending.iter().filter(|((cell, _), pending)|
            cell == cell_id.as_str() && !pending.emitted && pending.invocation.enqueue_ordinal <= prefix)
            .map(|(key, pending)| (pending.invocation.enqueue_ordinal, key.clone())).collect::<Vec<_>>();
        eligible.sort_by_key(|(ordinal, _)| *ordinal);
        for (_, key) in eligible {
            let pending = state.pending.get_mut(&key).ok_or("eligible pending call disappeared")?;
            let route = routes
                .get(&pending.invocation.tool_name.to_string())
                .ok_or("pending call has no exact route")?;
            if self.readiness.is_some() && !busy.insert(self.busy_owner(route)?) {
                continue;
            }
            pending.emitted = true;
            result.push(pending.invocation.clone());
        }
        Ok(result)
    }

    fn resolve(
        &self,
        cell_id: &CellId,
        tool_call_id: &str,
        result: Result<Value, String>,
    ) -> Result<(), String> {
        let key = (cell_id.as_str().to_string(), tool_call_id.to_string());
        let pending = self
            .state
            .lock()
            .map_err(|_| "code mode delegate state is poisoned".to_string())?
            .pending
            .remove(&key)
            .ok_or_else(|| {
                format!(
                    "code mode has no pending tool call '{}' in cell '{}'",
                    tool_call_id,
                    cell_id.as_str()
                )
            })?;
        let responder = pending
            .responder
            .ok_or_else(|| "code mode pending call lost its responder".to_string())?;
        responder
            .send(result)
            .map_err(|_| "code mode cell stopped before accepting tool result".to_string())
    }
}

impl CodeModeSessionDelegate for RoomDelegate {
    fn invoke_tool<'a>(
        &'a self,
        invocation: CodeModeNestedToolCall,
        cancellation_token: CancellationToken,
    ) -> ToolInvocationFuture<'a> {
        Box::pin(async move {
            #[cfg(test)]
            {
                let hold = {
                    let mut slot = self.registration_hold.lock().unwrap();
                    if slot.as_ref().is_some_and(|(ordinal, _)| *ordinal == invocation.enqueue_ordinal) {
                        slot.take().map(|(_, receiver)| receiver)
                    } else { None }
                };
                if let Some(receiver) = hold { receiver.await.map_err(|_| "registration fixture released its source")?; }
            }
            let key = (
                invocation.cell_id.as_str().to_string(),
                invocation.runtime_tool_call_id.clone(),
            );
            let (tx, rx) = tokio::sync::oneshot::channel();
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "code mode delegate state is poisoned".to_string())?;
                if state.pending.contains_key(&key) {
                    return Err(format!(
                        "code mode repeated pending tool call '{}' in cell '{}'",
                        key.1, key.0
                    ));
                }
                if let Err(error) = state.registrations.entry(key.0.clone()).or_default()
                    .register(&invocation, self.registration_byte_limit) {
                    state.failed_cells.insert(key.0.clone());
                    return Err(error);
                }
                let event = json!({"tool_call_id":invocation.runtime_tool_call_id,"enqueue_ordinal":invocation.enqueue_ordinal,"tool":invocation.tool_name.to_string(),"arguments":invocation.input});
                let cell = invocation.cell_id.clone();
                state.pending.insert(
                    key.clone(),
                    PendingCall {
                        invocation,
                        responder: Some(tx),
                        emitted: false,
                    },
                );
                if let Err(error) = self.publish_readiness(&mut state, &cell, event) {
                    state.pending.remove(&key);
                    state.failed_cells.insert(key.0.clone());
                    return Err(error);
                }
                #[cfg(test)]
                self.registration_changed.notify_one();
            }
            tokio::select! {
                result = rx => result.map_err(|_| "code mode tool-result channel closed".to_string())?,
                _ = cancellation_token.cancelled() => {
                    if let Ok(mut state) = self.state.lock() {
                        state.pending.remove(&key);
                        state.failed_cells.insert(key.0.clone());
                    }
                    Err("code mode nested tool call was cancelled".to_string())
                }
            }
        })
    }

    fn notify<'a>(
        &'a self,
        call_id: String,
        cell_id: CellId,
        text: String,
        cancellation_token: CancellationToken,
    ) -> NotificationFuture<'a> {
        Box::pin(async move {
            if !self.admitted_notifications {
                return Ok(());
            }
            let (responder, receiver) = tokio::sync::oneshot::channel();
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "code mode state is poisoned")?;
                state.next_notification = state
                    .next_notification
                    .checked_add(1)
                    .ok_or("code mode notification sequence exhausted")?;
                let sequence = state.next_notification;
                self.publish_readiness(
                    &mut state,
                    &cell_id,
                    json!({"notification_id":sequence,"text":text}),
                )?;
                state.notifications.push_back(PendingNotification {
                    cell_id,
                    call_id,
                    sequence,
                    text,
                    responder,
                });
            }
            self.notification_ready.notify_one();
            tokio::select! {
                result = receiver => result.map_err(|_| "code mode notification admission channel closed".to_string())?,
                _ = cancellation_token.cancelled() => Err("code mode notification cancelled before admission".into()),
            }
        })
    }

    fn completion_ready(&self, cell_id: &CellId) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "code mode state is poisoned")?;
        if !state.completed_cells.insert(cell_id.to_string()) {
            return Err("code mode cell completion was published twice".into());
        }
        if let Err(error) =
            self.publish_readiness(&mut state, cell_id, json!({"completion_ready":true}))
        {
            if let Some(context) = &self.readiness {
                context.retire()?;
            }
            return Err(error);
        }
        Ok(())
    }

    fn cell_closed(&self, cell_id: &CellId) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.active_cells.remove(cell_id.as_str());
        state.completed_cells.remove(cell_id.as_str());
        state.origins.remove(cell_id.as_str());
        state.registrations.remove(cell_id.as_str());
        state.failed_cells.remove(cell_id.as_str());
        state.notifications.retain(|item| item.cell_id != *cell_id);
        let keys = state
            .pending
            .keys()
            .filter(|(cell, _)| cell == cell_id.as_str())
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(mut pending) = state.pending.remove(&key) {
                if let Some(responder) = pending.responder.take() {
                    let _ = responder.send(Err("code mode cell closed".to_string()));
                }
            }
        }
    }
}

trait EmissionSink: WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder {
    fn arrive<'a>(
        &'a self,
        artifact: &'a InterfaceArtifactFile,
    ) -> Pin<Box<dyn Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>>;
    fn hand<'a>(
        &'a self,
        artifact: &'a SystemInterfaceEmissionArtifact,
        to_interface: &'a InterfaceId,
    ) -> Pin<Box<dyn Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>>;
}

impl EmissionSink for ToolProcessIo {
    fn arrive<'a>(
        &'a self,
        artifact: &'a InterfaceArtifactFile,
    ) -> Pin<Box<dyn Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>> {
        Box::pin(async move { self.hand_output_artifact(artifact).await })
    }
    fn hand<'a>(
        &'a self,
        artifact: &'a SystemInterfaceEmissionArtifact,
        to_interface: &'a InterfaceId,
    ) -> Pin<Box<dyn Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>> {
        Box::pin(async move { self.hand_system_emission(artifact, to_interface).await })
    }
}

fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && !value.contains('\0'))
        .ok_or_else(|| format!("{field} must be a non-blank string without NUL"))
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

#[cfg(test)]
mod tests;
