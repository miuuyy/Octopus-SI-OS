use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_descriptors::InterfaceId;
use octopus_interface_transport::InterfaceLiftContext;
use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceDescriptor, InterfaceDriverConfig, InterfaceInvocationEnvelope,
    InterfaceTransportConfig,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::account_auth::AccountAuthSettings;
use crate::mcp_client::{resolve_base_url, McpClient, McpClientError};
use crate::oauth::{
    authorization_status, complete_authorization, start_authorization, OAuthSettings,
};
use crate::operator_approval::{
    admit_decision, approval_request_body, read_response, ApprovalPolicy, ApprovalStore, CallApproval,
    McpToolAnnotations, OPERATOR_APPROVAL_RESPONSE_LABEL,
};
use crate::tool_filter::ToolMetaFilter;

mod account_auth;
mod life;
mod mcp_client;
mod oauth;
mod operator_approval;
mod tool_filter;
pub use life::{catalog_interface_life, operation_interface_life};

const MCP_TOOL_RESULT_LABEL: &str = "mcp.tool.result";
const MCP_RESOURCE_LABEL: &str = "mcp.resource";
const MCP_RESOURCE_CATALOG_LABEL: &str = "mcp.resource_catalog";
const MCP_RESOURCE_TEMPLATE_LABEL: &str = "mcp.resource_templates";
const MCP_TOOL_CATALOG_LABEL: &str = "mcp.tool_catalog";
const MCP_SERVER_LABEL: &str = "mcp.server";
const MCP_PROMPT_CATALOG_LABEL: &str = "mcp.prompt_catalog";
const MCP_EVENT_LABEL: &str = "mcp.events";
const MCP_OAUTH_LABEL: &str = "mcp.oauth";
const ABSOLUTE_MAX_CATALOG_PAGES: usize = 1024;
const ABSOLUTE_MAX_CATALOG_ITEMS: usize = 100_000;
const ABSOLUTE_MAX_CATALOG_BYTES: usize = 256 * 1024 * 1024;
const ABSOLUTE_MAX_EVENT_JOURNAL_SEGMENT_BYTES: u64 = 512 * 1024 * 1024;
const ABSOLUTE_MAX_EVENT_JOURNAL_RETAINED_SEGMENTS: usize = 1024;
const ABSOLUTE_MAX_RECONNECT_BACKOFF_MS: u64 = 600_000;

#[derive(Clone)]
pub(crate) struct McpInterfaceConfig {
    transport: InterfaceTransportConfig,
    protocol_versions: Vec<String>,
    client_name: String,
    client_version: String,
    max_catalog_pages: usize,
    max_catalog_items: usize,
    max_catalog_bytes: usize,
    event_journal_segment_bytes: u64,
    event_journal_retained_segments: usize,
    reconnect_backoff: std::time::Duration,
    oauth: Option<OAuthSettings>,
    account_auth: Option<AccountAuthSettings>,
    tool_filter: Option<ToolMetaFilter>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum McpProjectionError {
    #[error(transparent)]
    Transport(#[from] McpClientError),
    #[error("mcp catalog contract failed: {0}")]
    Contract(String),
    #[error("mcp catalog artifact delivery failed: {0}")]
    Delivery(String),
}

impl McpProjectionError {
    pub(crate) fn is_reconnectable(&self) -> bool {
        matches!(self, Self::Transport(error) if error.is_reconnectable())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpOperationRequest {
    operation: String,
    #[serde(default)]
    arguments: Value,
    #[serde(default)]
    correlation_id: Option<String>,
    approval_policy: ApprovalPolicy,
}

struct McpOperationSpec {
    id: String,
    request_label: String,
    output_label: String,
    description: String,
    operation: McpOperation,
    approval: CallApproval,
    validator: jsonschema::Validator,
}

struct McpOperationCatalog {
    catalog_label: String,
    request_label: String,
    operations: BTreeMap<String, McpOperationSpec>,
    affordances: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpOperationFile {
    id: String,
    request_label: String,
    output_label: String,
    handler_config: Value,
    description: String,
    input_schema: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum McpOperation {
    CallTool {
        #[serde(default)]
        mcp_tool_name: Option<String>,
        #[serde(default)]
        annotations: Option<McpToolAnnotations>,
    },
    ListTools,
    ListResources,
    ListResourceTemplates,
    ReadResource,
    SubscribeResource,
    UnsubscribeResource,
    ListPrompts,
    GetPrompt,
    Complete,
    PollEvents,
    CancelRequest,
    SendProgress,
    Respond,
    SetLoggingLevel,
    Ping,
    Status,
    Close,
    #[serde(rename = "oauth_start")]
    OAuthStart,
    #[serde(rename = "oauth_complete")]
    OAuthComplete,
    #[serde(rename = "oauth_status")]
    OAuthStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadResourceArguments {
    uri: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenericCallArguments {
    name: String,
    #[serde(default)]
    arguments: Value,
    #[serde(default)]
    progress_token: Option<Value>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

impl McpOperation {
    fn from_handler_config(id: &str, handler_config: &Value) -> Result<Self, String> {
        let operation =
            serde_json::from_value::<Self>(handler_config.clone()).map_err(|error| {
                format!("MCP operation '{id}' handler_config is malformed: {error}")
            })?;
        match &operation {
            Self::CallTool {
                mcp_tool_name: Some(mcp_tool_name),
                ..
            } => require_trimmed("handler_config.mcp_tool_name", mcp_tool_name)?,
            Self::CallTool {
                mcp_tool_name: None,
                annotations: Some(_),
            } => {
                return Err(format!(
                    "MCP operation '{id}' declares annotations without a fixed mcp_tool_name"
                ))
            }
            _ => {}
        }
        Ok(operation)
    }

    fn server_tool(&self, arguments: &Value) -> Option<String> {
        match self {
            Self::CallTool {
                mcp_tool_name: Some(name),
                ..
            } => Some(name.clone()),
            Self::CallTool {
                mcp_tool_name: None,
                ..
            } => arguments
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        }
    }

    fn artifact_label(&self) -> &'static str {
        match self {
            Self::CallTool { .. } => MCP_TOOL_RESULT_LABEL,
            Self::ReadResource => MCP_RESOURCE_LABEL,
            Self::PollEvents => MCP_EVENT_LABEL,
            Self::OAuthStart | Self::OAuthComplete | Self::OAuthStatus => MCP_OAUTH_LABEL,
            _ => "mcp.result",
        }
    }

    async fn invoke(&self, client: &McpClient, arguments: &Value) -> Result<Value, McpClientError> {
        match self {
            Self::CallTool {
                mcp_tool_name: Some(mcp_tool_name),
                ..
            } => {
                let forwarded =
                    require_object_or_null("fixed MCP tools/call arguments", arguments.clone())?;
                client.call_tool(mcp_tool_name, forwarded, None, None).await
            }
            Self::CallTool {
                mcp_tool_name: None,
                ..
            } => {
                let call = serde_json::from_value::<GenericCallArguments>(arguments.clone())
                    .map_err(|error| {
                        McpClientError::Config(format!(
                            "MCP tools/call arguments are malformed: {error}"
                        ))
                    })?;
                require_trimmed("tools/call name", &call.name).map_err(McpClientError::Config)?;
                let forwarded = require_object_or_null("tools/call arguments", call.arguments)?;
                client
                    .call_tool(&call.name, forwarded, call.progress_token, call.timeout_ms)
                    .await
            }
            Self::ListTools => client.list_tools(optional_cursor(arguments)?).await,
            Self::ListResources => client.list_resources(optional_cursor(arguments)?).await,
            Self::ListResourceTemplates => {
                client
                    .list_resource_templates(optional_cursor(arguments)?)
                    .await
            }
            Self::ReadResource => {
                let arguments = serde_json::from_value::<ReadResourceArguments>(arguments.clone())
                    .map_err(|error| {
                        McpClientError::Config(format!(
                            "MCP resources/read arguments are malformed: {error}"
                        ))
                    })?;
                require_trimmed("resources/read uri", &arguments.uri)
                    .map_err(McpClientError::Config)?;
                client.read_resource(&arguments.uri).await
            }
            Self::SubscribeResource => {
                let uri = required_argument_string(arguments, "uri")?;
                client.subscribe_resource(uri).await
            }
            Self::UnsubscribeResource => {
                let uri = required_argument_string(arguments, "uri")?;
                client.unsubscribe_resource(uri).await
            }
            Self::ListPrompts => client.list_prompts(optional_cursor(arguments)?).await,
            Self::GetPrompt => {
                let object = require_argument_object(arguments)?;
                let name = required_map_string(object, "name")?;
                let prompt_arguments = object
                    .get("arguments")
                    .cloned()
                    .map(|value| require_object_or_null("prompts/get arguments", value))
                    .transpose()?
                    .unwrap_or_else(|| json!({}));
                client.get_prompt(name, prompt_arguments).await
            }
            Self::Complete => {
                let object = require_argument_object(arguments)?;
                let reference = object.get("ref").cloned().ok_or_else(|| {
                    McpClientError::Config("completion ref is absent".to_string())
                })?;
                let argument = object.get("argument").cloned().ok_or_else(|| {
                    McpClientError::Config("completion argument is absent".to_string())
                })?;
                client
                    .complete(reference, argument, object.get("context").cloned())
                    .await
            }
            Self::PollEvents => {
                let object = require_argument_object(arguments)?;
                let after_cursor = optional_map_u64(object, "after_cursor")?.unwrap_or(0);
                let max_events = optional_map_u64(object, "max_events")?.unwrap_or(64);
                let wait_ms = optional_map_u64(object, "wait_ms")?.unwrap_or(0);
                client
                    .poll_events(
                        after_cursor,
                        usize::try_from(max_events).map_err(|_| {
                            McpClientError::Config("max_events exceeds usize".to_string())
                        })?,
                        wait_ms,
                    )
                    .await
            }
            Self::CancelRequest => {
                let object = require_argument_object(arguments)?;
                let request_id = object.get("request_id").cloned().ok_or_else(|| {
                    McpClientError::Config("cancel request_id is absent".to_string())
                })?;
                client
                    .cancel_request(request_id, object.get("reason").and_then(Value::as_str))
                    .await?;
                Ok(json!({ "cancelled": true }))
            }
            Self::SendProgress => {
                let object = require_argument_object(arguments)?;
                let token = object.get("progress_token").cloned().ok_or_else(|| {
                    McpClientError::Config("progress_token is absent".to_string())
                })?;
                let progress = object
                    .get("progress")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| McpClientError::Config("progress is absent".to_string()))?;
                client
                    .send_progress(
                        token,
                        progress,
                        object.get("total").and_then(Value::as_f64),
                        object.get("message").and_then(Value::as_str),
                    )
                    .await?;
                Ok(json!({ "sent": true }))
            }
            Self::Respond => {
                let object = require_argument_object(arguments)?;
                let request_id = object.get("request_id").cloned().ok_or_else(|| {
                    McpClientError::Config("response request_id is absent".to_string())
                })?;
                client
                    .respond(
                        request_id,
                        object.get("result").cloned(),
                        object.get("error").cloned(),
                    )
                    .await?;
                Ok(json!({ "sent": true }))
            }
            Self::SetLoggingLevel => {
                client
                    .set_logging_level(required_argument_string(arguments, "level")?)
                    .await
            }
            Self::Ping => client.ping().await,
            Self::Status => Ok(client.status().await),
            Self::Close => client.close().await,
            Self::OAuthStart | Self::OAuthComplete | Self::OAuthStatus => {
                Err(McpClientError::Config(
                    "oauth operation was routed through an active MCP session".to_string(),
                ))
            }
        }
    }

    fn is_oauth(&self) -> bool {
        matches!(
            self,
            Self::OAuthStart | Self::OAuthComplete | Self::OAuthStatus
        )
    }

    fn replay_safe_after_reconnect(&self) -> bool {
        matches!(
            self,
            Self::ListTools
                | Self::ListResources
                | Self::ListResourceTemplates
                | Self::ReadResource
                | Self::ListPrompts
                | Self::GetPrompt
                | Self::Complete
                | Self::PollEvents
                | Self::SetLoggingLevel
                | Self::Ping
                | Self::Status
        )
    }
}

struct McpInterfaceProcess {
    interface: InterfaceId,
    config: McpInterfaceConfig,
    state_dir: std::path::PathBuf,
    client: Arc<tokio::sync::Mutex<McpClientState>>,
    client_changed: Arc<tokio::sync::Notify>,
}

struct McpClientState {
    generation: u64,
    client: Option<Arc<McpClient>>,
    transition: Option<McpClientTransition>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum McpClientTransition {
    Connecting,
    Closing,
}

struct McpTransitionGuard {
    state: Arc<tokio::sync::Mutex<McpClientState>>,
    changed: Arc<tokio::sync::Notify>,
    transition: McpClientTransition,
    armed: bool,
}

impl McpTransitionGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for McpTransitionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let state = Arc::clone(&self.state);
        let changed = Arc::clone(&self.changed);
        let transition = self.transition;
        tokio::spawn(async move {
            let mut state = state.lock().await;
            if state.transition == Some(transition) {
                state.transition = None;
            }
            drop(state);
            changed.notify_waiters();
        });
    }
}

impl McpInterfaceProcess {
    async fn receive(
        &self,
        spec: &McpOperationSpec,
        envelope: InterfaceInvocationEnvelope,
        output_root: &Path,
    ) -> Result<InterfaceArtifactFile, String> {
        let manifest = envelope
            .read_artifact_manifest()
            .map_err(|error| error.to_string())?;
        if manifest.label != spec.request_label {
            return Err(format!(
                "MCP interface '{}' expected operation request label '{}', got '{}'",
                self.interface, spec.request_label, manifest.label
            ));
        }
        let payload = envelope
            .read_artifact_payload()
            .map_err(|error| error.to_string())?;
        let call: McpOperationRequest = serde_json::from_slice(&payload)
            .map_err(|error| format!("MCP operation request is malformed: {error}"))?;
        if spec.id != call.operation {
            return Err(format!(
                "MCP operation spec '{}' does not match request '{}'",
                spec.id, call.operation
            ));
        }
        if let Some(correlation_id) = call.correlation_id.as_deref() {
            require_trimmed("correlation_id", correlation_id)?;
        }
        validate_arguments(spec, &call.arguments)?;
        let gated = match call.approval_policy {
            ApprovalPolicy::OnRequest => spec.approval.gated(&call.arguments),
            ApprovalPolicy::Never => None,
        };
        if let Some((tool, annotations)) = gated {
            let request = self.approvals().open_request(
                &self.interface,
                &envelope,
                &spec.id,
                &spec.description,
                tool,
                annotations,
                &call.arguments,
                call.correlation_id.as_deref(),
            )?;
            let body = approval_request_body(
                &spec.output_label,
                &call.operation,
                call.correlation_id.as_deref(),
                &request,
            );
            return materialize_emitted_artifact(output_root, &spec.output_label, &body);
        }
        let body = self.execute(spec, &call).await;
        materialize_emitted_artifact(output_root, &spec.output_label, &body)
    }

    async fn decide(
        &self,
        catalog: &McpOperationCatalog,
        envelope: InterfaceInvocationEnvelope,
        output_root: &Path,
    ) -> Result<InterfaceArtifactFile, String> {
        let response = read_response(&envelope)?;
        let approvals = self.approvals();
        let pending = approvals.pending(&response.request_id)?;
        let decision = admit_decision(&self.interface, &envelope, &pending, response)?;
        let request = &pending.request;
        let spec = catalog.operations.get(&request.operation);
        let label = spec
            .map(|spec| spec.output_label.as_str())
            .unwrap_or(MCP_TOOL_RESULT_LABEL);
        let correlation_id = pending.correlation_id.as_deref();
        if !approvals.record_decision(&decision)? {
            let body = match approvals.retained_result(&request.request_id)? {
                Some(body) => body,
                None => closed_body(
                    label,
                    &request.operation,
                    correlation_id,
                    json!({
                        "code": "indeterminate_external_effect",
                        "message": format!("The approved call to {} may have reached the server before Octopus stopped. Check its current state before asking again.", request.tool),
                    }),
                ),
            };
            return materialize_emitted_artifact(output_root, label, &body);
        }
        let body = match spec {
            _ if !decision.approved() => closed_body(
                label,
                &request.operation,
                correlation_id,
                json!({
                    "code": "operator_declined",
                    "message": format!("The operator did not approve {}; nothing was sent to the server.", request.tool),
                }),
            ),
            Some(spec)
                if spec.operation.server_tool(&request.arguments).as_deref()
                    == Some(request.tool.as_str())
                    && validate_arguments(spec, &request.arguments).is_ok() =>
            {
                let call = McpOperationRequest {
                    operation: request.operation.clone(),
                    arguments: request.arguments.clone(),
                    correlation_id: pending.correlation_id.clone(),
                    approval_policy: ApprovalPolicy::OnRequest,
                };
                self.execute(spec, &call).await
            }
            _ => closed_body(
                label,
                &request.operation,
                correlation_id,
                json!({
                    "code": "operation_unavailable",
                    "message": format!("{} no longer offers {} with the approved arguments; nothing was sent to the server.", self.interface, request.operation),
                }),
            ),
        };
        approvals.retain_result(&request.request_id, &body)?;
        materialize_emitted_artifact(output_root, label, &body)
    }

    fn approvals(&self) -> ApprovalStore {
        ApprovalStore::new(self.state_dir.join("operator-approvals"))
    }

    async fn execute(&self, spec: &McpOperationSpec, call: &McpOperationRequest) -> Value {
        let operation = &spec.operation;
        let artifact_label = spec.output_label.as_str();
        let correlation_id = call.correlation_id.as_deref();
        if operation.is_oauth() {
            return match self.invoke_oauth(operation, &call.arguments).await {
                Ok(result) => ok_body(artifact_label, &call.operation, correlation_id, "oauth", result),
                Err(error) => closed_body(artifact_label, &call.operation, correlation_id, error.structured()),
            };
        }
        if matches!(operation, McpOperation::Close) {
            return match self.close_current().await {
                Ok(result) => ok_body(artifact_label, &call.operation, correlation_id, "closed", result),
                Err(error) => closed_body(artifact_label, &call.operation, correlation_id, error.structured()),
            };
        }
        let (mut client, mut disposition) = match self.acquire_client().await {
            Ok(client) => client,
            Err(error) => {
                return closed_body(artifact_label, &call.operation, correlation_id, error.structured());
            }
        };
        let mut result = operation.invoke(&client, &call.arguments).await;
        if matches!(&result, Err(error) if error.is_reconnectable()) {
            self.retire_current(&client).await;
            if operation.replay_safe_after_reconnect() {
                match self.acquire_client().await {
                    Ok((connected, _)) => {
                        client = connected;
                        disposition = "reconnected".to_string();
                        result = operation.invoke(&client, &call.arguments).await;
                    }
                    Err(error) => {
                        disposition = "reconnect_failed".to_string();
                        result = Err(error);
                    }
                }
            } else {
                disposition = "replay_refused_effectful_operation".to_string();
            }
        }
        match result {
            Ok(result) => ok_body(artifact_label, &call.operation, correlation_id, &disposition, result),
            Err(error) if error.is_reconnectable() => {
                self.retire_current(&client).await;
                closed_body(artifact_label, &call.operation, correlation_id, error.structured())
            }
            Err(error) => closed_body(artifact_label, &call.operation, correlation_id, error.structured()),
        }
    }

    async fn acquire_client(&self) -> Result<(Arc<McpClient>, String), McpClientError> {
        loop {
            let notified = self.client_changed.notified();
            let connect = {
                let mut state = self.client.lock().await;
                if let Some(client) = state.client.as_ref() {
                    return Ok((Arc::clone(client), "attached".to_string()));
                }
                if state.transition.is_some() {
                    false
                } else {
                    state.generation = state.generation.checked_add(1).ok_or_else(|| {
                        McpClientError::Protocol {
                            method: "connect".to_string(),
                            reason: "MCP client generation overflowed".to_string(),
                        }
                    })?;
                    state.transition = Some(McpClientTransition::Connecting);
                    true
                }
            };
            if !connect {
                notified.await;
                continue;
            }
            let mut transition = McpTransitionGuard {
                state: Arc::clone(&self.client),
                changed: Arc::clone(&self.client_changed),
                transition: McpClientTransition::Connecting,
                armed: true,
            };
            let connected = McpClient::connect(&self.config, &self.state_dir)
                .await
                .map(Arc::new);
            let mut state = self.client.lock().await;
            if state.transition != Some(McpClientTransition::Connecting) {
                let abandoned = connected.as_ref().ok().cloned();
                drop(state);
                if let Some(client) = abandoned {
                    client.retire().await;
                }
                return Err(McpClientError::Protocol {
                    method: "connect".to_string(),
                    reason: "MCP client connection lost lifecycle ownership".to_string(),
                });
            }
            state.transition = None;
            transition.disarm();
            match connected {
                Ok(client) => {
                    let disposition = if state.generation == 1 {
                        "connected"
                    } else {
                        "reconnected"
                    };
                    state.client = Some(Arc::clone(&client));
                    self.client_changed.notify_waiters();
                    return Ok((client, disposition.to_string()));
                }
                Err(error) => {
                    self.client_changed.notify_waiters();
                    return Err(error);
                }
            }
        }
    }

    async fn retire_current(&self, expected: &Arc<McpClient>) {
        let client = {
            let mut state = self.client.lock().await;
            if state
                .client
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, expected))
                && state.transition.is_none()
            {
                state.transition = Some(McpClientTransition::Closing);
                state.client.take()
            } else {
                None
            }
        };
        if let Some(client) = client {
            let mut transition = McpTransitionGuard {
                state: Arc::clone(&self.client),
                changed: Arc::clone(&self.client_changed),
                transition: McpClientTransition::Closing,
                armed: true,
            };
            let _ = client.retire().await;
            let mut state = self.client.lock().await;
            if state.transition == Some(McpClientTransition::Closing) {
                state.transition = None;
            }
            transition.disarm();
            self.client_changed.notify_waiters();
        }
    }

    async fn close_current(&self) -> Result<Value, McpClientError> {
        loop {
            let notified = self.client_changed.notified();
            let client = {
                let mut state = self.client.lock().await;
                if state.transition.is_some() {
                    None
                } else {
                    let client = state
                        .client
                        .take()
                        .ok_or_else(|| McpClientError::Protocol {
                            method: "close".to_string(),
                            reason: "MCP client is not connected".to_string(),
                        })?;
                    state.transition = Some(McpClientTransition::Closing);
                    Some(client)
                }
            };
            let Some(client) = client else {
                notified.await;
                continue;
            };
            let mut transition = McpTransitionGuard {
                state: Arc::clone(&self.client),
                changed: Arc::clone(&self.client_changed),
                transition: McpClientTransition::Closing,
                armed: true,
            };
            let result = client.close().await;
            let mut state = self.client.lock().await;
            if state.transition == Some(McpClientTransition::Closing) {
                state.transition = None;
            }
            transition.disarm();
            self.client_changed.notify_waiters();
            return result;
        }
    }

    async fn reset_current(&self) {
        loop {
            let notified = self.client_changed.notified();
            let current = {
                let state = self.client.lock().await;
                if state.transition.is_some() {
                    None
                } else if let Some(client) = state.client.as_ref() {
                    Some(Arc::clone(client))
                } else {
                    return;
                }
            };
            let Some(current) = current else {
                notified.await;
                continue;
            };
            self.retire_current(&current).await;
            return;
        }
    }

    async fn invoke_oauth(
        &self,
        operation: &McpOperation,
        arguments: &Value,
    ) -> Result<Value, McpClientError> {
        let settings = self.config.oauth.clone().ok_or_else(|| {
            McpClientError::Config("MCP interface has no oauth config".to_string())
        })?;
        let resource_url = resolve_base_url(&self.config.transport)?;
        match operation {
            McpOperation::OAuthStart => {
                let force_new = require_argument_object(arguments)?
                    .get("force_new")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(start_authorization(settings, &resource_url, force_new).await?)
            }
            McpOperation::OAuthComplete => {
                let object = require_argument_object(arguments)?;
                let code = required_map_string(object, "code")?;
                let state = required_map_string(object, "state")?;
                let result = complete_authorization(settings, &resource_url, code, state).await?;
                self.reset_current().await;
                Ok(result)
            }
            McpOperation::OAuthStatus => Ok(authorization_status(settings, &resource_url)?),
            _ => Err(McpClientError::Config(
                "operation is not an oauth operation".to_string(),
            )),
        }
    }
}

fn mcp_interface_process(
    interface: InterfaceId,
    config: McpInterfaceConfig,
    state_dir: PathBuf,
) -> McpInterfaceProcess {
    McpInterfaceProcess {
        interface,
        config,
        state_dir,
        client: Arc::new(tokio::sync::Mutex::new(McpClientState {
            generation: 0,
            client: None,
            transition: None,
        })),
        client_changed: Arc::new(tokio::sync::Notify::new()),
    }
}

fn validate_arguments(spec: &McpOperationSpec, arguments: &Value) -> Result<(), String> {
    let errors = spec
        .validator
        .iter_errors(arguments)
        .take(8)
        .map(|error| {
            format!(
                "{} at instance '{}' against schema '{}'",
                error, error.instance_path, error.schema_path
            )
        })
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "MCP operation '{}' arguments are invalid: {}",
            spec.id,
            errors.join("; ")
        ))
    }
}

fn ok_body(
    label: &str,
    operation: &str,
    correlation_id: Option<&str>,
    session_disposition: &str,
    result: Value,
) -> Value {
    json!({
        "label": label,
        "operation": operation,
        "correlation_id": correlation_id,
        "session_disposition": session_disposition,
        "ok": true,
        "result": result,
    })
}

fn closed_body(label: &str, operation: &str, correlation_id: Option<&str>, error: Value) -> Value {
    json!({
        "label": label,
        "operation": operation,
        "correlation_id": correlation_id,
        "ok": false,
        "error": error,
    })
}

pub(crate) async fn emit_catalog_as_inbound_artifacts(
    ctx: &InterfaceLiftContext,
    config: &McpInterfaceConfig,
    client: &McpClient,
) -> Result<(), McpProjectionError> {
    emit_inbound_value(
        ctx,
        "server",
        MCP_SERVER_LABEL,
        &json!({
            "label": MCP_SERVER_LABEL,
            "initialize": client.initialize_result(),
        }),
    )
    .await?;

    if client.capabilities().tools {
        let mut tools = Vec::new();
        let mut tool_bytes = 0;
        let mut cursor = None;
        let mut seen_cursors = std::collections::BTreeSet::new();
        for _ in 0..config.max_catalog_pages {
            let page = client.list_tools(cursor.as_deref()).await?;
            extend_catalog(
                &mut tools,
                &mut tool_bytes,
                required_array(&page, "tools/list", "tools")?,
                "tools/list",
                config.max_catalog_bytes,
            )?;
            enforce_catalog_bound("tools/list", tools.len(), config.max_catalog_items)?;
            cursor = next_cursor(&page, "tools/list")?;
            let Some(next) = cursor.as_ref() else {
                break;
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(McpProjectionError::Contract(format!(
                    "tools/list repeated cursor '{next}'"
                )));
            }
        }
        if cursor.is_some() {
            return Err(McpProjectionError::Contract(format!(
                "tools/list exceeded the {}-page boundary",
                config.max_catalog_pages
            )));
        }
        emit_inbound_value(
            ctx,
            "tool-catalog",
            MCP_TOOL_CATALOG_LABEL,
            &json!({ "label": MCP_TOOL_CATALOG_LABEL, "tools": tools }),
        )
        .await?;
    }

    if client.capabilities().resources && config.tool_filter.is_none() {
        let mut templates = Vec::new();
        let mut template_bytes = 0;
        let mut cursor = None;
        let mut seen_cursors = std::collections::BTreeSet::new();
        for _ in 0..config.max_catalog_pages {
            let page = client.list_resource_templates(cursor.as_deref()).await?;
            extend_catalog(
                &mut templates,
                &mut template_bytes,
                required_array(&page, "resources/templates/list", "resourceTemplates")?,
                "resources/templates/list",
                config.max_catalog_bytes,
            )?;
            enforce_catalog_bound(
                "resources/templates/list",
                templates.len(),
                config.max_catalog_items,
            )?;
            cursor = next_cursor(&page, "resources/templates/list")?;
            let Some(next) = cursor.as_ref() else {
                break;
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(McpProjectionError::Contract(format!(
                    "resources/templates/list repeated cursor '{next}'"
                )));
            }
        }
        if cursor.is_some() {
            return Err(McpProjectionError::Contract(format!(
                "resources/templates/list exceeded the {}-page boundary",
                config.max_catalog_pages
            )));
        }
        emit_inbound_value(
            ctx,
            "resource-templates",
            MCP_RESOURCE_TEMPLATE_LABEL,
            &json!({
                "label": MCP_RESOURCE_TEMPLATE_LABEL,
                "resource_templates": templates
            }),
        )
        .await?;

        let mut resources = Vec::new();
        let mut resource_bytes = 0;
        let mut cursor = None;
        let mut seen_cursors = std::collections::BTreeSet::new();
        for _ in 0..config.max_catalog_pages {
            let page = client.list_resources(cursor.as_deref()).await?;
            extend_catalog(
                &mut resources,
                &mut resource_bytes,
                required_array(&page, "resources/list", "resources")?,
                "resources/list",
                config.max_catalog_bytes,
            )?;
            enforce_catalog_bound("resources/list", resources.len(), config.max_catalog_items)?;
            cursor = next_cursor(&page, "resources/list")?;
            let Some(next) = cursor.as_ref() else {
                break;
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(McpProjectionError::Contract(format!(
                    "resources/list repeated cursor '{next}'"
                )));
            }
        }
        if cursor.is_some() {
            return Err(McpProjectionError::Contract(format!(
                "resources/list exceeded the {}-page boundary",
                config.max_catalog_pages
            )));
        }
        for resource in &resources {
            resource
                .get("uri")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|uri| !uri.is_empty())
                .ok_or_else(|| {
                    McpProjectionError::Contract(
                        "resources/list returned a resource without a non-empty uri".to_string(),
                    )
                })?;
        }
        emit_inbound_value(
            ctx,
            "resource-catalog",
            MCP_RESOURCE_CATALOG_LABEL,
            &json!({
                "label": MCP_RESOURCE_CATALOG_LABEL,
                "resources": resources,
            }),
        )
        .await?;
    }
    if client.capabilities().prompts && config.tool_filter.is_none() {
        let mut prompts = Vec::new();
        let mut prompt_bytes = 0;
        let mut cursor = None;
        let mut seen_cursors = std::collections::BTreeSet::new();
        for _ in 0..config.max_catalog_pages {
            let page = client.list_prompts(cursor.as_deref()).await?;
            extend_catalog(
                &mut prompts,
                &mut prompt_bytes,
                required_array(&page, "prompts/list", "prompts")?,
                "prompts/list",
                config.max_catalog_bytes,
            )?;
            enforce_catalog_bound("prompts/list", prompts.len(), config.max_catalog_items)?;
            cursor = next_cursor(&page, "prompts/list")?;
            let Some(next) = cursor.as_ref() else {
                break;
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(McpProjectionError::Contract(format!(
                    "prompts/list repeated cursor '{next}'"
                )));
            }
        }
        if cursor.is_some() {
            return Err(McpProjectionError::Contract(format!(
                "prompts/list exceeded the {}-page boundary",
                config.max_catalog_pages
            )));
        }
        emit_inbound_value(
            ctx,
            "prompt-catalog",
            MCP_PROMPT_CATALOG_LABEL,
            &json!({
                "label": MCP_PROMPT_CATALOG_LABEL,
                "prompts": prompts,
            }),
        )
        .await?;
    }
    Ok(())
}

fn required_array<'a>(
    value: &'a Value,
    method: &str,
    field: &str,
) -> Result<&'a Vec<Value>, McpProjectionError> {
    value.get(field).and_then(Value::as_array).ok_or_else(|| {
        McpProjectionError::Contract(format!("{method} result has no '{field}' array"))
    })
}

fn next_cursor(value: &Value, method: &str) -> Result<Option<String>, McpProjectionError> {
    match value.get("nextCursor") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(cursor)) if !cursor.trim().is_empty() => {
            Ok(Some(cursor.trim().to_string()))
        }
        Some(_) => Err(McpProjectionError::Contract(format!(
            "{method} result nextCursor is not a non-empty string or null"
        ))),
    }
}

fn enforce_catalog_bound(
    method: &str,
    len: usize,
    max_items: usize,
) -> Result<(), McpProjectionError> {
    if len <= max_items {
        Ok(())
    } else {
        Err(McpProjectionError::Contract(format!(
            "{method} returned more than {max_items} entries"
        )))
    }
}

fn extend_catalog(
    target: &mut Vec<Value>,
    current_bytes: &mut usize,
    incoming: &[Value],
    method: &str,
    max_bytes: usize,
) -> Result<(), McpProjectionError> {
    for item in incoming {
        let bytes = serde_json::to_vec(item).map_err(|error| {
            McpProjectionError::Contract(format!(
                "{method} returned an item that cannot be encoded: {error}"
            ))
        })?;
        *current_bytes = current_bytes.checked_add(bytes.len()).ok_or_else(|| {
            McpProjectionError::Contract(format!("{method} catalog byte count overflowed"))
        })?;
        if *current_bytes > max_bytes {
            return Err(McpProjectionError::Contract(format!(
                "{method} catalog exceeds {max_bytes} bytes"
            )));
        }
        target.push(item.clone());
    }
    Ok(())
}

async fn emit_inbound_value(
    ctx: &InterfaceLiftContext,
    directory: &str,
    label: &str,
    body: &Value,
) -> Result<(), McpProjectionError> {
    let bytes = serde_json::to_vec(body).map_err(|error| {
        McpProjectionError::Contract(format!("{label} artifact did not serialize: {error}"))
    })?;
    let artifact =
        InterfaceArtifactFile::write(ctx.interface_var_dir().join(directory), label, &bytes)
            .map_err(|error| {
                McpProjectionError::Delivery(format!("{label} artifact write failed: {error}"))
            })?;
    match ctx
        .hand_arrival(artifact, Vec::new())
        .await
        .map_err(McpProjectionError::Delivery)?
    {
        octopus_interfaces::DeliveryVerdict::Admitted { .. }
        | octopus_interfaces::DeliveryVerdict::Refused {
            reason: octopus_interfaces::RefusedReason::NoBinding | octopus_interfaces::RefusedReason::Policy,
            ..
        } => Ok(()),
        octopus_interfaces::DeliveryVerdict::Refused { reason, detail } => Err(McpProjectionError::Delivery(
            format!("runtime refused {label} projection as '{}': {detail}", reason.as_str()),
        )),
    }
}

fn materialize_emitted_artifact(
    output_root: &Path,
    label: &str,
    body: &Value,
) -> Result<InterfaceArtifactFile, String> {
    let bytes = serde_json::to_vec(body)
        .map_err(|error| format!("MCP emitted artifact did not serialize: {error}"))?;
    InterfaceArtifactFile::write(output_root, label, &bytes).map_err(|error| error.to_string())
}

fn load_operation_catalog(
    interface_dir: &Path,
    descriptor: &InterfaceDescriptor,
) -> Result<McpOperationCatalog, String> {
    let driver_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        format!(
            "MCP interface '{}' resolves no interface_driver_config",
            descriptor.id
        )
    })?;
    let catalog_dir = driver_config
        .private_config
        .get("operation_catalog_dir")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "MCP interface '{}' declares no operation_catalog_dir",
                descriptor.id
            )
        })?;
    if catalog_dir.is_empty()
        || catalog_dir.trim() != catalog_dir
        || Path::new(catalog_dir).is_absolute()
        || Path::new(catalog_dir)
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "MCP interface '{}' operation_catalog_dir must be a normalized relative path",
            descriptor.id
        ));
    }
    let catalog_label = driver_config
        .private_config
        .get("operation_catalog_label")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "MCP interface '{}' declares no operation_catalog_label",
                descriptor.id
            )
        })?;
    require_trimmed("operation_catalog_label", catalog_label)?;
    let request_label = driver_config
        .private_config
        .get("operation_request_label")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "MCP interface '{}' declares no operation_request_label",
                descriptor.id
            )
        })?;
    require_trimmed("operation_request_label", request_label)?;
    let root = interface_dir.join(catalog_dir);
    let metadata = std::fs::symlink_metadata(&root).map_err(|error| {
        format!(
            "MCP interface '{}' operation catalog '{}' cannot be inspected: {error}",
            descriptor.id,
            root.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "MCP interface '{}' operation catalog '{}' is not a directory",
            descriptor.id,
            root.display()
        ));
    }
    let mut entries = std::fs::read_dir(&root)
        .map_err(|error| {
            format!(
                "MCP operation catalog '{}' cannot be read: {error}",
                root.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "MCP operation catalog '{}' cannot be enumerated: {error}",
                root.display()
            )
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut operations = BTreeMap::new();
    let mut affordances = Vec::new();
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            format!(
                "MCP operation file '{}' cannot be inspected: {error}",
                path.display()
            )
        })?;
        if file_type.is_symlink()
            || !file_type.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            return Err(format!(
                "MCP operation catalog entry '{}' is not a regular JSON file",
                path.display()
            ));
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "MCP operation file '{}' cannot be read: {error}",
                path.display()
            )
        })?;
        let file: McpOperationFile = serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "MCP operation file '{}' is malformed: {error}",
                path.display()
            )
        })?;
        require_trimmed("operation id", &file.id)?;
        require_trimmed("operation request_label", &file.request_label)?;
        require_trimmed("operation output_label", &file.output_label)?;
        require_trimmed("operation description", &file.description)?;
        if file.request_label != request_label {
            return Err(format!(
                "MCP operation '{}' request_label '{}' does not match interface label '{}'",
                file.id, file.request_label, request_label
            ));
        }
        let expected_file = format!("{}.json", file.id);
        if path.file_name().and_then(|value| value.to_str()) != Some(expected_file.as_str()) {
            return Err(format!(
                "MCP operation '{}' must live in '{}', found '{}'",
                file.id,
                expected_file,
                path.display()
            ));
        }
        let operation = McpOperation::from_handler_config(&file.id, &file.handler_config)?;
        if operation.artifact_label() != file.output_label {
            return Err(format!(
                "MCP operation '{}' output_label '{}' does not match operation result label '{}'",
                file.id,
                file.output_label,
                operation.artifact_label()
            ));
        }
        jsonschema::meta::try_validate(&file.input_schema)
            .map_err(|error| {
                format!(
                    "MCP operation '{}' input_schema draft cannot resolve: {error}",
                    file.id
                )
            })?
            .map_err(|error| {
                format!(
                    "MCP operation '{}' input_schema is malformed: {error}",
                    file.id
                )
            })?;
        let validator = jsonschema::options()
            .should_validate_formats(true)
            .should_ignore_unknown_formats(false)
            .build(&file.input_schema)
            .map_err(|error| {
                format!(
                    "MCP operation '{}' input_schema cannot compile: {error}",
                    file.id
                )
            })?;
        affordances.push(json!({
            "name": file.id.clone(),
            "description": file.description.clone(),
            "input_schema": file.input_schema.clone(),
            "request_label": file.request_label.clone(),
            "output_label": file.output_label.clone(),
        }));
        let approval = match &operation {
            McpOperation::CallTool {
                mcp_tool_name: Some(tool),
                annotations,
            } => CallApproval::fixed(tool, annotations.as_ref()),
            _ => CallApproval::Free,
        };
        let spec = McpOperationSpec {
            id: file.id.clone(),
            request_label: file.request_label,
            output_label: file.output_label,
            description: file.description,
            operation,
            approval,
            validator,
        };
        if operations.insert(file.id.clone(), spec).is_some() {
            return Err(format!("MCP operation catalog repeats '{}'", file.id));
        }
    }
    if operations.is_empty() {
        return Err(format!(
            "MCP interface '{}' operation catalog is empty",
            descriptor.id
        ));
    }
    let declared_tools = CallApproval::declared_tools(operations.values().filter_map(|spec| {
        match &spec.operation {
            McpOperation::CallTool {
                mcp_tool_name: Some(tool),
                annotations,
            } => Some((tool.as_str(), annotations.as_ref())),
            _ => None,
        }
    }));
    for spec in operations.values_mut() {
        if matches!(
            spec.operation,
            McpOperation::CallTool {
                mcp_tool_name: None,
                ..
            }
        ) {
            spec.approval = CallApproval::Named(Arc::clone(&declared_tools));
        }
    }
    Ok(McpOperationCatalog {
        catalog_label: catalog_label.to_string(),
        request_label: request_label.to_string(),
        operations,
        affordances,
    })
}

async fn emit_operation_catalog(
    ctx: &InterfaceLiftContext,
    catalog: &McpOperationCatalog,
) -> Result<(), McpProjectionError> {
    emit_inbound_value(
        ctx,
        "operation-catalog",
        &catalog.catalog_label,
        &json!({
            "label": catalog.catalog_label,
            "interface": ctx.interface,
            "request_label": catalog.request_label,
            "operations": catalog.affordances,
        }),
    )
    .await
}

#[cfg(test)]
pub(crate) fn load_interface_config(
    interface_dir: &std::path::Path,
    interface: &InterfaceId,
) -> Result<McpInterfaceConfig, String> {
    let descriptor = load_interface_descriptor(interface_dir, interface)?;
    load_interface_config_from_descriptor(descriptor, interface)
}

pub(crate) fn load_interface_config_from_descriptor(
    descriptor: InterfaceDescriptor,
    interface: &InterfaceId,
) -> Result<McpInterfaceConfig, String> {
    if descriptor.id != *interface {
        return Err(format!(
            "resolved MCP interface '{}' does not match process interface '{}'",
            descriptor.id, interface
        ));
    }
    let config = descriptor.interface_driver_config.ok_or_else(|| {
        format!(
            "MCP interface '{}' resolves no interface_driver_config",
            interface
        )
    })?;
    let transport = config
        .transport
        .clone()
        .ok_or_else(|| format!("MCP interface '{}' resolves no transport", interface))?;
    validate_transport_reference(&transport, interface)?;
    let protocol_versions = metadata_string_array(&config, interface, "protocol_versions")?;
    let client_name = metadata_string(&config, interface, "client_name")?;
    let client_version = metadata_string(&config, interface, "client_version")?;
    let max_catalog_pages = metadata_usize(
        &config,
        interface,
        "max_catalog_pages",
        ABSOLUTE_MAX_CATALOG_PAGES,
    )?;
    let max_catalog_items = metadata_usize(
        &config,
        interface,
        "max_catalog_items",
        ABSOLUTE_MAX_CATALOG_ITEMS,
    )?;
    let max_catalog_bytes = metadata_usize(
        &config,
        interface,
        "max_catalog_bytes",
        ABSOLUTE_MAX_CATALOG_BYTES,
    )?;
    let event_journal_segment_bytes = metadata_u64(
        &config,
        interface,
        "event_journal_segment_bytes",
        ABSOLUTE_MAX_EVENT_JOURNAL_SEGMENT_BYTES,
    )?;
    let event_journal_retained_segments = metadata_usize(
        &config,
        interface,
        "event_journal_retained_segments",
        ABSOLUTE_MAX_EVENT_JOURNAL_RETAINED_SEGMENTS,
    )?;
    if event_journal_retained_segments < 2 {
        return Err(format!(
            "MCP interface '{}' private_config.event_journal_retained_segments must be within 2..={ABSOLUTE_MAX_EVENT_JOURNAL_RETAINED_SEGMENTS}",
            interface
        ));
    }
    let reconnect_backoff_ms = metadata_u64(
        &config,
        interface,
        "reconnect_backoff_ms",
        ABSOLUTE_MAX_RECONNECT_BACKOFF_MS,
    )?;
    let oauth = match config.private_config.get("oauth") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<OAuthSettings>(value.clone()).map_err(|error| {
                format!(
                    "MCP interface '{}' private_config.oauth is malformed: {error}",
                    interface
                )
            })?,
        ),
    };
    if let Some(oauth) = oauth.as_ref() {
        oauth.validate().map_err(|error| error.to_string())?;
    }
    let account_auth = match config.private_config.get("auth") {
        None => None,
        Some(value) => {
            let settings =
                serde_json::from_value::<AccountAuthSettings>(value.clone()).map_err(|error| {
                    format!(
                        "MCP interface '{}' private_config.auth is malformed: {error}",
                        interface
                    )
                })?;
            settings
                .validate()
                .map_err(|error| format!("MCP interface '{}' {error}", interface))?;
            if oauth.is_some() || transport.auth_env_key.is_some() {
                return Err(format!(
                    "MCP interface '{}' private_config.auth excludes private_config.oauth and transport.auth_env_key",
                    interface
                ));
            }
            Some(settings)
        }
    };
    let tool_filter = config
        .private_config
        .get("tool_filter")
        .map(ToolMetaFilter::parse)
        .transpose()
        .map_err(|error| format!("MCP interface '{}' {error}", interface))?;
    Ok(McpInterfaceConfig {
        transport,
        protocol_versions,
        client_name,
        client_version,
        max_catalog_pages,
        max_catalog_items,
        max_catalog_bytes,
        event_journal_segment_bytes,
        event_journal_retained_segments,
        reconnect_backoff: std::time::Duration::from_millis(reconnect_backoff_ms),
        oauth,
        account_auth,
        tool_filter,
    })
}

fn require_argument_object(
    arguments: &Value,
) -> Result<&serde_json::Map<String, Value>, McpClientError> {
    arguments.as_object().ok_or_else(|| {
        McpClientError::Config("MCP operation arguments must be an object".to_string())
    })
}

fn required_map_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, McpClientError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| McpClientError::Config(format!("MCP argument '{key}' is absent or empty")))
}

fn required_argument_string<'a>(
    arguments: &'a Value,
    key: &str,
) -> Result<&'a str, McpClientError> {
    required_map_string(require_argument_object(arguments)?, key)
}

fn optional_map_u64(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, McpClientError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            McpClientError::Config(format!("MCP argument '{key}' is not an unsigned integer"))
        }),
    }
}

fn optional_cursor(arguments: &Value) -> Result<Option<&str>, McpClientError> {
    let object = require_argument_object(arguments)?;
    match object.get("cursor") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.trim())),
        Some(_) => Err(McpClientError::Config(
            "MCP cursor is not a non-empty string or null".to_string(),
        )),
    }
}

fn require_object_or_null(surface: &str, value: Value) -> Result<Value, McpClientError> {
    match value {
        Value::Null => Ok(json!({})),
        Value::Object(_) => Ok(value),
        _ => Err(McpClientError::Config(format!(
            "{surface} must be an object or null"
        ))),
    }
}

#[cfg(test)]
fn load_interface_descriptor(
    interface_dir: &std::path::Path,
    expected: &InterfaceId,
) -> Result<InterfaceDescriptor, String> {
    let descriptor_path = interface_dir.join("interface.json");
    let bytes = std::fs::read(&descriptor_path).map_err(|error| {
        format!(
            "MCP interface descriptor '{}' cannot be read: {error}",
            descriptor_path.display()
        )
    })?;
    let descriptor: InterfaceDescriptor =
        octopus_descriptors::parse_strict(&bytes).map_err(|error| {
            format!(
                "MCP interface descriptor '{}' is malformed: {error}",
                descriptor_path.display()
            )
        })?;
    if descriptor.id != *expected {
        return Err(format!(
            "MCP interface folder '{}' contains descriptor id '{}'",
            expected, descriptor.id
        ));
    }
    Ok(descriptor)
}

fn validate_transport_reference(
    transport: &InterfaceTransportConfig,
    interface: &InterfaceId,
) -> Result<(), String> {
    let direct = !transport.base_url.trim().is_empty();
    let from_env = transport
        .base_url_env
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if direct == from_env {
        return Err(format!(
            "MCP interface '{}' must declare exactly one of base_url or base_url_env",
            interface
        ));
    }
    Ok(())
}

fn metadata_string(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
    key: &str,
) -> Result<String, String> {
    let value = config
        .private_config
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "MCP interface '{}' private_config.{key} must be a string",
                interface
            )
        })?;
    require_trimmed(&format!("private_config.{key}"), value)?;
    Ok(value.to_string())
}

fn metadata_string_array(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
    key: &str,
) -> Result<Vec<String>, String> {
    let values = config
        .private_config
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "MCP interface '{}' private_config.{key} must be an array",
                interface
            )
        })?;
    if values.is_empty() || values.len() > 16 {
        return Err(format!(
            "MCP interface '{}' private_config.{key} must contain 1..=16 versions",
            interface
        ));
    }
    let mut unique = std::collections::BTreeSet::new();
    let mut parsed = Vec::with_capacity(values.len());
    for value in values {
        let value = value.as_str().ok_or_else(|| {
            format!(
                "MCP interface '{}' private_config.{key} entries must be strings",
                interface
            )
        })?;
        require_protocol_version(value)?;
        if !unique.insert(value) {
            return Err(format!(
                "MCP interface '{}' private_config.{key} repeats '{value}'",
                interface
            ));
        }
        parsed.push(value.to_string());
    }
    Ok(parsed)
}

fn metadata_usize(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
    key: &str,
    absolute_max: usize,
) -> Result<usize, String> {
    let value = metadata_u64(config, interface, key, absolute_max as u64)?;
    usize::try_from(value).map_err(|_| format!("private_config.{key} exceeds usize"))
}

fn metadata_u64(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
    key: &str,
    absolute_max: u64,
) -> Result<u64, String> {
    let value = config
        .private_config
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "MCP interface '{}' private_config.{key} must be an unsigned integer",
                interface
            )
        })?;
    if value == 0 || value > absolute_max {
        return Err(format!(
            "MCP interface '{}' private_config.{key} must be within 1..={absolute_max}",
            interface
        ));
    }
    Ok(value)
}

fn require_protocol_version(value: &str) -> Result<(), String> {
    require_trimmed("MCP protocol version", value)?;
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7) && !byte.is_ascii_digit())
    {
        return Err(format!(
            "MCP protocol version '{value}' must use YYYY-MM-DD"
        ));
    }
    Ok(())
}

fn require_trimmed(surface: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!("{surface} must be non-empty and trimmed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
