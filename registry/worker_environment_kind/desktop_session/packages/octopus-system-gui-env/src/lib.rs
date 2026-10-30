use base64::Engine;
use std::collections::BTreeMap;
use std::sync::Arc;

use octopus_interfaces::{InterfaceArtifactFile, InterfaceInvocationEnvelope};
use octopus_tool_door_runtime::room::{decode_tool_call, emit_json_artifact, formed_scope, strict_object};
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo, WorkerEnvironmentLifecycleRecorder, WorkerEnvironmentLiveStateRecorder,
};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact, ToolResultError};
use octopus_worker_env::{
    WorkerEnvKindSpec, WorkerEnvironmentFormation, WorkerEnvironmentId, WorkerEnvironmentLifecycle,
};
use reqwest::header::{HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{json, Map, Value};

pub mod backend;
mod config;
mod app_authorization;
mod native_connection;
pub use native_connection::prepare_private_desktop_host;

pub fn application_settings(root: &std::path::Path, action: Value) -> Result<Value, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, tag = "operation", rename_all = "snake_case")]
    enum Action { List {}, Remove { bundle_id: String } }
    let action: Action = serde_json::from_value(action).map_err(|error| error.to_string())?;
    let bundle = match &action { Action::List {} => None, Action::Remove { bundle_id } => Some(bundle_id.as_str()) };
    let mut result = app_authorization::application_preferences(root, bundle)?;
    result["schema_version"] = json!(1);
    Ok(result)
}
pub mod native_client;
pub mod protocol;

use config::DesktopConfig;

const ROOM: &str = "desktop room";
const SUBSTRATE_FAMILY: &str = "gui";

pub(crate) trait WorkerEnvironmentRecorder:
    WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder
{
}

impl<T> WorkerEnvironmentRecorder for T where
    T: WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder + ?Sized
{
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DesktopOperation {
    AppState,
    AppResume,
    Apps,
    AppClick,
    AppMove,
    AppDrag,
    AppScroll,
    AppKey,
    AppType,
    AppSetValue,
    AppSelectText,
    AppSecondaryAction,
    AppPaste,
    Status,
    Observe,
    Screenshot,
    Click,
    Move,
    Type,
    Key,
    Scroll,
    Drag,
    Control,
    Cancel,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopHandlerConfig {
    operation: DesktopOperation,
}

struct RoomState {
    client: reqwest::Client,
    calls: u64,
}

pub struct SystemGuiEnv {
    environment: ToolProcessEnvironment,
    rooms: tokio::sync::Mutex<BTreeMap<WorkerEnvironmentId, RoomState>>,
}

impl SystemGuiEnv {
    pub fn new(environment: ToolProcessEnvironment) -> Self {
        Self {
            environment,
            rooms: tokio::sync::Mutex::new(BTreeMap::new()),
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
            source_movement_seq,
            ..
        } = invocation;
        let formation = worker_environment.ok_or_else(|| {
            ToolProcessError::Capability(
                "desktop invocation has no formed worker environment".to_string(),
            )
        })?;
        let kind = worker_environment_kind.ok_or_else(|| {
            ToolProcessError::Capability(
                "desktop invocation has no formed kind descriptor".to_string(),
            )
        })?;
        let config = DesktopConfig::load_formed(&self.environment, &formation, &kind)
            .map_err(ToolProcessError::Capability)?;
        config.validate_process_inputs(&descriptor).map_err(ToolProcessError::Capability)?;
        let operation = desktop_operation(&descriptor.handler_config, descriptor.id.as_str())?;
        let call = if operation == DesktopOperation::AppResume {
            app_authorization::decode_resume(&envelope, descriptor.id.as_str()).map_err(ToolProcessError::Capability)?
        } else { decode_tool_call(ROOM, &envelope)? };
        if descriptor.id.as_str() != call.tool {
            return Err(ToolProcessError::Capability(format!(
                "tool descriptor '{}' does not match tool.call '{}'",
                descriptor.id, call.tool
            )));
        }
        formed_scope(ROOM, &envelope, &formation)?;
        let metadata = envelope
            .metadata()
            .map_err(|error| ToolProcessError::Capability(error.to_string()))?;
        let (client, call_sequence) = self
            .client(&formation, &config)
            .await
            .map_err(ToolProcessError::Unavailable)?;
        let action = desktop_action(operation, &call.tool, &call.arguments).and_then(|mut action| {
            if is_application_operation(operation) {
                let approval = config.app_approval.as_ref().ok_or("application tools have no declared operator/resume configuration")?;
                let connection = config.native_connection.as_ref().ok_or("application tools require their native host")?;
                if !formation.granted_tools.contains(&approval.resume_tool)
                    || !formation.tool_descriptor_hashes.contains_key(&approval.resume_tool)
                    || (operation == DesktopOperation::AppResume && call.tool != approval.resume_tool) {
                    return Err("application resume descriptor is not the exact configured formed tool".into());
                }
                let bindings = metadata.checkpoint_binding_policy.as_ref().ok_or("application invocation has no admitted binding snapshot")?;
                let context = app_authorization::CallContext {
                    scope: protocol::application::Scope { host_generation: connection.host_generation.clone(),
                        worker_environment_id: formation.worker_environment_id.to_string(), formation_pin: formation.formation_pin.clone() },
                    call: app_authorization::OriginalCall { run: formation.run.clone(), ee: formation.ee.clone(),
                        worker: formation.worker.clone(), door_interface: formation.door_interface.clone(), tool: call.tool.clone(),
                        correlation_id: call.correlation_id.clone().ok_or("application call requires its exact correlation")?,
                        arguments: call.arguments.clone(), source_movement_seq, source_artifact: envelope.artifact.clone() },
                    inbound_interface: bindings.inbound.interface.clone(),
                };
                action["invocation"] = serde_json::to_value(context).map_err(|error| error.to_string())?;
                action["resume_tool"] = json!(approval.resume_tool);
                action["operator_interface"] = json!(approval.operator_interface);
            }
            Ok(action)
        });
        let body = match action {
            Ok(action) => {
                dispatch(
                    &client,
                    &formation,
                    &config,
                    call_sequence,
                    &call.tool,
                    &format!(
                        "{}:{}",
                        source_movement_seq,
                        envelope.artifact.content.to_hex()
                    ),
                    action,
                )
                .await
            }
            Err(error) => Err(error),
        };
        let body = body.and_then(|mut body| {
            materialize_image(&envelope, &mut body)?;
            Ok(body)
        });
        let status = body
            .as_ref()
            .ok()
            .and_then(|body| body.pointer("/evidence/status"))
            .and_then(Value::as_str)
            .unwrap_or("failed");
        let detail = body.as_ref().err().cloned();
        let mut evidence_write_errors = Vec::new();
        if let Err(error) = recorder
            .record(
                &formation,
                if status == "ready" || (operation == DesktopOperation::AppState
                    && body.as_ref().is_ok_and(|body| body.get("operator_request").is_some())) {
                    WorkerEnvironmentLifecycle::Active
                } else {
                    WorkerEnvironmentLifecycle::Failed
                },
            )
            .await
        {
            evidence_write_errors.push(evidence_write_error("lifecycle", error));
        }
        let mut live_state = self.live_state(&formation, &config, call_sequence, status, detail);
        if let Ok(body) = &body {
            live_state["desktop"] = desktop_live_evidence(body);
        }
        if let Err(error) = recorder.record_live_state(&formation, &live_state).await {
            evidence_write_errors.push(evidence_write_error("live_state", error));
        }
        let response = match body {
            Ok(body) if evidence_write_errors.is_empty() => {
                if let Some(error) = body.pointer("/application_action/error").filter(|error| !error.is_null()) {
                    ToolResultArtifact::failure(&call.tool, call.correlation_id,
                        ToolResultError::new(error["code"].as_str().ok_or_else(|| ToolProcessError::Capability("validated application failure lost its code".into()))?,
                            error["message"].as_str().ok_or_else(|| ToolProcessError::Capability("validated application failure lost its message".into()))?)
                        .with_details(Map::from_iter([("operation_body".into(), body)])))
                } else { ToolResultArtifact::success(&call.tool, call.correlation_id, body) }
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
                    call.correlation_id,
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
                ToolResultArtifact::failure(&call.tool, call.correlation_id, failure)
            }
        };
        Ok(vec![emit_json_artifact(&envelope, &response.label, &response)
            .map_err(ToolProcessError::Capability)?])
    }

    async fn client(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &DesktopConfig,
    ) -> Result<(reqwest::Client, u64), String> {
        let mut rooms = self.rooms.lock().await;
        let state = match rooms.get_mut(&formation.worker_environment_id) {
            Some(state) => state,
            None => {
                let client = reqwest::Client::builder()
                    .connect_timeout(config.timeout())
                    .timeout(config.timeout())
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|error| format!("desktop HTTP client build failed: {error}"))?;
                rooms.insert(
                    formation.worker_environment_id.clone(),
                    RoomState { client, calls: 0 },
                );
                rooms
                    .get_mut(&formation.worker_environment_id)
                    .ok_or_else(|| "desktop room lost a just-inserted state".to_string())?
            }
        };
        state.calls = state
            .calls
            .checked_add(1)
            .ok_or_else(|| "desktop call sequence overflowed".to_string())?;
        Ok((state.client.clone(), state.calls))
    }

    fn live_state(
        &self,
        formation: &WorkerEnvironmentFormation,
        config: &DesktopConfig,
        calls: u64,
        status: &str,
        detail: Option<String>,
    ) -> Value {
        json!({
            "schema_version": 1,
            "worker_environment_id": formation.worker_environment_id,
            "worker_environment_kind": formation.worker_environment_kind,
            "substrate_family": SUBSTRATE_FAMILY,
            "status": status,
            "calls": calls,
            "viewer": {
                "protocol": "desktop_session",
                "address": config.viewer_url,
                "protocol_version": protocol::VERSION,
                "host_generation": config.native_connection.as_ref().map(|connection| &connection.host_generation),
            },
            "detail": detail,
        })
    }

    async fn release_room(
        &self,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Result<(), String> {
        let config = DesktopConfig::load_formed(&self.environment, formation, kind)?;
        let (client, call_sequence) = self.client(formation, &config).await?;
        let result = dispatch(
            &client,
            formation,
            &config,
            call_sequence,
            "worker_environment.release",
            &format!(
                "release:{}:{}",
                formation.worker_environment_id, call_sequence
            ),
            json!({ "type": "release", "formation_pin": formation.formation_pin }),
        )
        .await;
        result?;
        self.rooms
            .lock()
            .await
            .remove(&formation.worker_environment_id);
        Ok(())
    }
}

fn evidence_write_error(stage: &str, error: impl Into<String>) -> Value {
    json!({
        "stage": stage,
        "message": error.into(),
    })
}

fn desktop_live_evidence(body: &Value) -> Value {
    match body.get("evidence").and_then(Value::as_object) {
        Some(evidence) => Value::Object(evidence.iter().filter(|(key, _)|
            !matches!(key.as_str(), "observation" | "effect" | "application"))
            .map(|(key, value)| (key.clone(), value.clone())).collect()),
        None => Value::Null,
    }
}

impl ToolProcessHandler for SystemGuiEnv {
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
            self.release_room(formation, kind)
                .await
                .map_err(ToolProcessError::Unavailable)
        })
    }

}

pub async fn run_gui_session_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    let process = Arc::new(SystemGuiEnv::new(environment));
    octopus_tool_door_runtime::run_tool_process(process).await
}

pub fn decode_gui_door_artifact(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    envelope: &InterfaceInvocationEnvelope,
    purpose: octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose,
) -> Result<Option<ToolCallArtifact>, octopus_tool_door_runtime::ToolDoorArtifactDecodeError> {
    if envelope.read_artifact_manifest().map_err(|error| error.to_string())?.label != "operator.approval.response" {
        let call = octopus_tool_door_runtime::default_tool_door_artifact_decoder(ctx, envelope, purpose)?;
        if purpose == octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch {
            if let Some(call) = &call {
                let directory = std::path::PathBuf::from(std::env::var("OCTOPUS_INTERFACE_DIR")
                    .map_err(|_| "GUI owner decoder has no declared interface directory")?);
                if !directory.is_absolute() { return Err("GUI owner decoder requires its absolute declared interface directory".into()); }
                let config = DesktopConfig::read(&directory.join(config::CONFIG_FILE))?;
                if config.app_approval.as_ref().is_some_and(|approval| approval.resume_tool == call.tool) {
                    return Err("application resume requires the original operator.approval.response artifact at the GUI door".into());
                }
            }
        }
        return Ok(call);
    }
    let private = std::path::PathBuf::from(std::env::var("OCTOPUS_VAR_DIR")
        .map_err(|_| "GUI owner decoder has no declared private directory")?);
    let door = std::env::var("OCTOPUS_INTERFACE_ID").map_err(|_| "GUI owner decoder has no exact interface identity")?;
    native_connection::validate_directory(&private)?;
    if private.file_name().and_then(|value| value.to_str()) != Some(door.as_str()) {
        return Err("GUI owner decoder private directory does not identify its exact door".into());
    }
    let door = octopus_descriptors::InterfaceId::new(door);
    match purpose {
        octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch => {
            let directory = std::path::PathBuf::from(std::env::var("OCTOPUS_INTERFACE_DIR")
                .map_err(|_| "GUI owner decoder has no declared interface directory")?);
            if !directory.is_absolute() { return Err("GUI owner decoder requires its absolute declared interface directory".into()); }
            let config = DesktopConfig::read(&directory.join(config::CONFIG_FILE))?;
            app_authorization::admit_response(ctx, &private.join("desktop-host"), &door, envelope, &config)
        },
        octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::HistoricalIdentity =>
            app_authorization::historical_identity(ctx, &private.join("desktop-host"), &door, envelope),
    }.map(Some).map_err(Into::into)
}

async fn dispatch(
    client: &reqwest::Client,
    formation: &WorkerEnvironmentFormation,
    config: &DesktopConfig,
    call_sequence: u64,
    tool: &str,
    request_id: &str,
    action: Value,
) -> Result<Value, String> {
    let body = protocol::Request {
        schema_version: protocol::VERSION,
        worker_environment_id: formation.worker_environment_id.to_string(),
        request_id: request_id.to_string(),
        tool: tool.to_string(),
        action,
    };
    let mut request = client
        .post(&config.action_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header(
            "X-Octopus-Worker-Environment",
            formation.worker_environment_id.as_str(),
        )
        .header("X-Octopus-Call-Sequence", call_sequence.to_string())
        .json(&body);
    for (name, value) in &config.headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("desktop header name '{name}' is invalid: {error}"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|error| format!("desktop header value is invalid: {error}"))?;
        request = request.header(name, value);
    }
    let auth_secret = if let Some(connection) = &config.native_connection {
        request = request.header("Authorization", format!("Bearer {}", connection.token));
        Some(connection.token.clone())
    } else if let Some(env_key) = config.auth_env_key.as_deref() {
        let secret = std::env::var(env_key)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "desktop auth env var '{env_key}' is unset/empty; refusing authenticated action"
                )
            })?;
        let header = HeaderName::from_bytes(config.auth_header.as_bytes()).map_err(|error| {
            format!(
                "desktop auth header '{}' is invalid: {error}",
                config.auth_header
            )
        })?;
        let value = HeaderValue::from_str(&format!("{}{}", config.auth_value_prefix, secret))
            .map_err(|error| format!("desktop auth header value is invalid: {error}"))?;
        request = request.header(header, value);
        Some(secret)
    } else {
        None
    };
    let mut response = tokio::time::timeout(config.timeout(), request.send())
        .await
        .map_err(|_| {
            format!(
                "desktop action timed out after {} ms",
                config.request_timeout_ms
            )
        })?
        .map_err(|error| format!("desktop action request failed: {error}"))?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > config.max_response_bytes as u64)
    {
        return Err(format!(
            "desktop response Content-Length exceeds {} bytes",
            config.max_response_bytes
        ));
    }
    let deadline = tokio::time::Instant::now() + config.timeout();
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::time::timeout_at(deadline, response.chunk())
            .await
            .map_err(|_| "desktop response body timed out".to_string())?
            .map_err(|error| format!("desktop response body read failed: {error}"))?;
        let Some(chunk) = chunk else {
            break;
        };
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| "desktop response size overflowed".to_string())?;
        if next > config.max_response_bytes {
            return Err(format!(
                "desktop response exceeded {} bytes",
                config.max_response_bytes
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(format!(
            "desktop backend returned HTTP {}; {}",
            status.as_u16(),
            safe_body_evidence(&bytes)
        ));
    }
    let response: protocol::Response = serde_json::from_slice(&bytes)
        .map_err(|error| format!("desktop backend response is malformed: {error}"))?;
    let response_value = serde_json::to_value(&response).map_err(|e| e.to_string())?;
    if auth_secret
        .as_deref()
        .is_some_and(|secret| json_contains_text(&response_value, secret))
    {
        return Err(format!(
            "desktop backend reflected authentication material; {}",
            safe_body_evidence(&bytes)
        ));
    }
    protocol::validate_response(&response, &body)?;
    if let Some(evidence) = &response.evidence {
        for source in &evidence.applications {
            if source.scope.formation_pin != formation.formation_pin
                || config.native_connection.as_ref().map(|connection| connection.host_generation.as_str()) != Some(source.scope.host_generation.as_str()) {
                return Err("desktop application source belongs to a different native formation".into());
            }
        }
    }
    let viewer = json!({
        "protocol": "desktop_session", "address": config.viewer_url,
        "host_generation": config.native_connection.as_ref().map(|connection| &connection.host_generation),
    });
    if let Some(application) = response.application_result {
        return app_authorization::project_result(application,
            serde_json::to_value(response.evidence).map_err(|error| error.to_string())?, viewer, &body);
    }
    Ok(json!({
        "accepted": true,
        "evidence": response.evidence,
        "viewer": viewer
    }))
}

fn desktop_operation(
    handler_config: &Value,
    tool: &str,
) -> Result<DesktopOperation, ToolProcessError> {
    serde_json::from_value::<DesktopHandlerConfig>(handler_config.clone())
        .map(|config| config.operation)
        .map_err(|error| {
            ToolProcessError::Capability(format!(
                "tool '{tool}' has malformed desktop handler_config: {error}"
            ))
        })
}

fn desktop_action(
    operation: DesktopOperation,
    tool: &str,
    arguments: &Value,
) -> Result<Value, String> {
    let mut payload = arguments.clone();
    let target = if matches!(
        operation,
        DesktopOperation::Click
            | DesktopOperation::Move
            | DesktopOperation::Type
            | DesktopOperation::Key
            | DesktopOperation::Scroll
            | DesktopOperation::Drag
    ) {
        let (screen, observation, generation) = protocol::action_target(arguments)?;
        let target = json!({"screen_id": screen, "observation_id": observation, "control_generation": generation});
        let object = payload
            .as_object_mut()
            .ok_or("desktop arguments must be an object")?;
        for key in ["screen_id", "observation_id", "control_generation"] {
            object.remove(key);
        }
        Some(target)
    } else {
        None
    };
    let mut action = desktop_action_payload(operation, tool, &payload)?;
    if let Some(target) = target {
        action
            .as_object_mut()
            .ok_or("desktop action is not an object")?
            .extend(target.as_object().unwrap().clone());
    }
    Ok(action)
}

fn is_application_operation(operation: DesktopOperation) -> bool {
    matches!(operation, DesktopOperation::AppState | DesktopOperation::AppResume | DesktopOperation::Apps
        | DesktopOperation::AppClick | DesktopOperation::AppMove | DesktopOperation::AppDrag | DesktopOperation::AppScroll
        | DesktopOperation::AppKey | DesktopOperation::AppType | DesktopOperation::AppSetValue | DesktopOperation::AppSelectText
        | DesktopOperation::AppSecondaryAction | DesktopOperation::AppPaste)
}

fn desktop_action_payload(
    operation: DesktopOperation,
    tool: &str,
    arguments: &Value,
) -> Result<Value, String> {
    match operation {
        DesktopOperation::AppState => {
            strict_object(tool, arguments, &["app", "include_screenshots", "presentation_base", "capture_selection"])?;
            let selector: protocol::application::Selector = serde_json::from_value(arguments["app"].clone())
                .map_err(|error| format!("application selector is malformed: {error}"))?;
            let include_screenshots = arguments.get("include_screenshots").map(|value| value.as_bool()
                .ok_or("include_screenshots must be a boolean")).transpose()?.unwrap_or(true);
            let presentation_base = arguments.get("presentation_base").map(|value| {
                let base: protocol::application::PresentationBase = serde_json::from_value(value.clone())
                    .map_err(|error| format!("presentation base is malformed: {error}"))?;
                base.validate()?;
                Ok::<_, String>(base)
            }).transpose()?;
            let capture_selection = protocol::application::CaptureSelection::from_arguments(arguments)?;
            Ok(json!({"type":"app_state","app":selector,"include_screenshots":include_screenshots,"presentation_base":presentation_base,"capture_selection":capture_selection}))
        }
        DesktopOperation::AppResume => {
            let object = strict_object(tool, arguments, &["request_id"])?;
            let request_id = required_string(object, "request_id")?;
            Ok(json!({"type":"app_resume","request_id":request_id}))
        }
        DesktopOperation::Apps => {
            strict_object(tool, arguments, &[])?;
            Ok(json!({"type":"apps"}))
        }
        DesktopOperation::AppClick | DesktopOperation::AppMove | DesktopOperation::AppDrag | DesktopOperation::AppScroll
        | DesktopOperation::AppKey | DesktopOperation::AppType | DesktopOperation::AppSetValue | DesktopOperation::AppSelectText
        | DesktopOperation::AppSecondaryAction | DesktopOperation::AppPaste => {
            let name = match operation {
                DesktopOperation::AppClick => "app_click", DesktopOperation::AppMove => "app_move", DesktopOperation::AppDrag => "app_drag",
                DesktopOperation::AppScroll => "app_scroll", DesktopOperation::AppKey => "app_key", DesktopOperation::AppType => "app_type",
                DesktopOperation::AppSetValue => "app_set_value", DesktopOperation::AppSelectText => "app_select_text",
                DesktopOperation::AppSecondaryAction => "app_secondary_action", DesktopOperation::AppPaste => "app_paste",
                _ => unreachable!(),
            };
            let call = protocol::application::ActionCall::parse(name, arguments)?;
            Ok(json!({"type":name,"app_call":call}))
        }
        DesktopOperation::Status => {
            strict_object(tool, arguments, &[])?;
            Ok(json!({ "type": "status" }))
        }
        DesktopOperation::Control => {
            let object = strict_object(tool, arguments, &["mode"])?;
            let mode = required_string(object, "mode")?;
            if !matches!(mode, "acquire" | "release") {
                return Err("desktop control mode must be acquire or release".into());
            }
            Ok(json!({"type":"control", "mode":mode}))
        }
        DesktopOperation::Cancel => {
            strict_object(tool, arguments, &[])?;
            Ok(json!({"type":"cancel"}))
        }
        DesktopOperation::Observe => observe_action(tool, arguments),
        DesktopOperation::Screenshot => screenshot_action(tool, arguments),
        DesktopOperation::Click => {
            let object = strict_object(tool, arguments, &["x", "y", "button", "count"])?;
            let button = optional_string(object, "button")?.unwrap_or("left");
            if !matches!(button, "left" | "right" | "middle") {
                return Err("click button must be left, right or middle".into());
            }
            let count = match object.get("count") {
                None => 1,
                Some(value) => value
                    .as_u64()
                    .filter(|v| matches!(v, 1 | 2))
                    .ok_or("click count must be 1 or 2")?,
            };
            Ok(json!({
                "type": "click",
                "button": button,
                "count": count,
                "x": required_finite_number(object, "x")?,
                "y": required_finite_number(object, "y")?,
            }))
        }
        DesktopOperation::Move => {
            let object = strict_object(tool, arguments, &["x", "y"])?;
            Ok(
                json!({"type":"move", "x":required_finite_number(object, "x")?, "y":required_finite_number(object, "y")?}),
            )
        }
        DesktopOperation::Type => {
            let object = strict_object(tool, arguments, &["text"])?;
            Ok(json!({
                "type": "type",
                "text": required_string(object, "text")?,
            }))
        }
        DesktopOperation::Key => {
            let object = strict_object(tool, arguments, &["key"])?;
            Ok(json!({
                "type": "key",
                "key": required_string(object, "key")?,
            }))
        }
        DesktopOperation::Scroll => {
            let object = strict_object(tool, arguments, &["delta_x", "delta_y"])?;
            Ok(json!({
                "type": "scroll",
                "delta_x": optional_finite_number(object, "delta_x")?.unwrap_or(0.0),
                "delta_y": optional_finite_number(object, "delta_y")?.unwrap_or(0.0),
            }))
        }
        DesktopOperation::Drag => {
            let object = strict_object(tool, arguments, &["from_x", "from_y", "to_x", "to_y"])?;
            Ok(json!({
                "type": "drag",
                "from_x": required_finite_number(object, "from_x")?,
                "from_y": required_finite_number(object, "from_y")?,
                "to_x": required_finite_number(object, "to_x")?,
                "to_y": required_finite_number(object, "to_y")?,
            }))
        }
    }
}

fn observe_action(tool: &str, arguments: &Value) -> Result<Value, String> {
    let object = strict_object(
        tool,
        arguments,
        &["screen_id", "detail", "include_screenshot"],
    )?;
    let detail = optional_string(object, "detail")?.unwrap_or("compact");
    if !matches!(detail, "compact" | "full") {
        return Err("gui.observe detail must be 'compact' or 'full'".to_string());
    }
    Ok(json!({
        "type": "observe",
        "screen_id": required_string(object, "screen_id")?,
        "detail": detail,
        "include_screenshot": optional_bool(object, "include_screenshot")?.unwrap_or(false),
    }))
}

fn screenshot_action(tool: &str, arguments: &Value) -> Result<Value, String> {
    let object = strict_object(tool, arguments, &["screen_id", "format", "quality"])?;
    let format = optional_string(object, "format")?.unwrap_or("png");
    if !matches!(format, "png" | "jpeg") {
        return Err("gui.screenshot format must be 'png' or 'jpeg'".to_string());
    }
    let quality = optional_u64(object, "quality")?;
    if quality.is_some_and(|quality| !(1..=100).contains(&quality)) {
        return Err("gui.screenshot quality must be in 1..=100".to_string());
    }
    if format == "png" && quality.is_some() {
        return Err("gui.screenshot quality is only valid for jpeg".to_string());
    }
    Ok(json!({
        "type": "screenshot",
        "screen_id": required_string(object, "screen_id")?,
        "format": format,
        "quality": quality,
    }))
}

fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && !value.contains('\0'))
        .ok_or_else(|| format!("{field} must be a non-blank string without NUL"))
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a str>, String> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() && !value.contains('\0') => {
            Ok(Some(value))
        }
        Some(_) => Err(format!("{field} must be a non-blank string or null")),
    }
}

fn required_finite_number(object: &Map<String, Value>, field: &str) -> Result<f64, String> {
    optional_finite_number(object, field)?.ok_or_else(|| format!("{field} must be a finite number"))
}

fn optional_finite_number(object: &Map<String, Value>, field: &str) -> Result<Option<f64>, String> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .map(Some)
            .ok_or_else(|| format!("{field} must be a finite number")),
    }
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

fn materialize_image(input: &InterfaceInvocationEnvelope, body: &mut Value) -> Result<(), String> {
    let body = if body.get("original_result").is_some() {
        if body.get("media").is_some() || body.pointer("/evidence/observation").is_some_and(|value| !value.is_null())
            || body.pointer("/evidence/application").is_some() {
            return Err("application resume must carry media only in its original result".into());
        }
        let original: ToolResultArtifact = serde_json::from_value(body["original_result"].clone()).map_err(|error| error.to_string())?;
        if !original.ok { return Ok(()); }
        body.pointer_mut("/original_result/body")
            .filter(|value| value.is_object())
            .ok_or("application resume has no original result body")?
    } else { body };
    let mut paths = Vec::new();
    if body.pointer("/evidence/observation/image").is_some_and(|value| !value.is_null()) {
        paths.push("/evidence/observation/image".to_string());
    }
    if let Some(application) = body.pointer("/evidence/application") {
        let state: protocol::application::State = serde_json::from_value(application.clone())
            .map_err(|error| format!("application image evidence is malformed: {error}"))?;
        state.validate(&state.scope, &state.application.app)?;
        for index in 0..state.captures.len() {
            paths.push(format!("/evidence/application/captures/{index}/image"));
        }
    }
    if paths.is_empty() { return Ok(()); }
    if paths.len() > protocol::MAX_MEDIA_ITEMS || body.get("media").is_some() {
        return Err("desktop image evidence exceeds its item bound or already has media".into());
    }
    let mut total_bytes = 0usize;
    let mut prepared = Vec::with_capacity(paths.len());
    for path in paths {
        let mut value = body.pointer(&path).ok_or("desktop image evidence disappeared")?.clone();
        value.as_object_mut().ok_or("desktop image must be an object")?.remove("image_id");
        let image: protocol::Image = serde_json::from_value(value).map_err(|error| error.to_string())?;
        if image.byte_length > protocol::MAX_SOURCE_MEDIA_BYTES - total_bytes {
            return Err("desktop images exceed their aggregate source byte limit".into());
        }
        protocol::validate_image(&image, &protocol::Screen {
            id: path.clone(), x: 0.0, y: 0.0, width: f64::from(image.width), height: f64::from(image.height),
            pixel_width: image.width, pixel_height: image.height,
        })?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(&image.data_base64)
            .map_err(|error| error.to_string())?;
        total_bytes += bytes.len();
        prepared.push((path, image, bytes));
    }
    let parent = input
        .artifact_file
        .manifest_path()
        .parent()
        .ok_or("desktop input manifest has no parent")?;
    let mut media = Vec::with_capacity(prepared.len());
    for (path, image, bytes) in prepared {
        let file = InterfaceArtifactFile::write(parent.join("emitted/captures"), "screen.captured", &bytes)
            .map_err(|error| error.to_string())?;
        let manifest = file.read_manifest().map_err(|error| error.to_string())?;
        let stored = body.pointer_mut(&path).and_then(Value::as_object_mut)
            .ok_or("desktop image evidence disappeared during materialization")?;
        stored.remove("data_base64");
        stored.insert("artifact".into(), serde_json::to_value(&manifest).map_err(|error| error.to_string())?);
        media.push(json!({"mime_type":image.media_type,"artifact":manifest,"payload_base64":image.data_base64}));
    }
    body["media"] = Value::Array(media);
    Ok(())
}

fn safe_body_evidence(bytes: &[u8]) -> String {
    format!(
        "body_bytes={}, body_blake3={}",
        bytes.len(),
        blake3::hash(bytes).to_hex()
    )
}

fn json_contains_text(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.contains(needle),
        Value::Array(values) => values.iter().any(|value| json_contains_text(value, needle)),
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| key.contains(needle) || json_contains_text(value, needle)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

#[cfg(test)]
mod tests;
