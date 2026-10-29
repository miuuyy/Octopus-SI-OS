use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use octopus_interfaces::InterfaceTransportConfig;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::oneshot;
use tokio::sync::{mpsc, Mutex};

use crate::account_auth::AccountAuthSettings;
use crate::oauth::{OAuthClient, OAuthError};
use crate::tool_filter::ToolMetaFilter;
use crate::McpInterfaceConfig;

pub const MCP_STREAMABLE_HTTP_WIRE: &str = "mcp_streamable_http";
pub const MCP_SSE_WIRE: &str = "mcp_sse";

const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 120_000;
const MAX_REQUEST_TIMEOUT_MS: u64 = 600_000;
const DEFAULT_MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const ABSOLUTE_MAX_RESPONSE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_AUTH_HEADER: &str = "Authorization";
const DEFAULT_AUTH_VALUE_PREFIX: &str = "Bearer ";
const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const LAST_EVENT_ID_HEADER: &str = "last-event-id";
const MAX_EVENT_POLL: usize = 256;
const MAX_EVENT_WAIT_MS: u64 = 30_000;
const LEGACY_SSE_FRAME_BUFFER: usize = 512;
const LEGACY_SSE_EVENT_BUFFER: usize = 512;

#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    #[error("mcp transport is unsupported by the remote mcp client: {0}")]
    UnsupportedTransport(String),
    #[error("mcp transport config invalid: {0}")]
    Config(String),
    #[error("mcp wire transport error to {url}: {source}")]
    Http { url: String, source: reqwest::Error },
    #[error("mcp server returned HTTP {status} for {method}: {evidence}")]
    HttpStatus {
        status: u16,
        method: String,
        evidence: HttpFailureEvidence,
    },
    #[error("mcp request '{method}' timed out after {timeout_ms}ms")]
    Timeout { method: String, timeout_ms: u64 },
    #[error("mcp response for {method} exceeded {limit} bytes")]
    ResponseTooLarge { method: String, limit: u64 },
    #[error("mcp response for {method} is not valid JSON-RPC: {reason}")]
    Protocol { method: String, reason: String },
    #[error("mcp server error for {method}: code={code} message={message}")]
    ServerError {
        method: String,
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("mcp environment variable '{0}' is unset or empty")]
    MissingEnvironmentValue(String),
    #[error(transparent)]
    OAuth(#[from] OAuthError),
    #[error("mcp event journal failed: {0}")]
    Journal(String),
    #[error(
        "mcp event cursor {requested} fell behind retained history; earliest available event is {earliest_available}, latest is {latest}"
    )]
    EventCursorGap {
        requested: u64,
        earliest_available: u64,
        latest: u64,
    },
    #[error("ChatGPT sign-in is required: {0}; sign in to ChatGPT in Octopus")]
    AccountSignInRequired(String),
    #[error("ChatGPT sign-in expired — sign in again in Octopus")]
    AccountSignInExpired,
    #[error("ChatGPT sign-in cannot be used: {0}")]
    AccountCredential(String),
    #[error("MCP tool '{0}' is outside this interface's declared tool filter")]
    ToolNotAdmitted(String),
    #[error("MCP {0} is outside this interface's declared tool filter")]
    SurfaceNotAdmitted(String),
}

#[derive(Clone, Debug)]
pub(crate) struct HttpFailureEvidence {
    pub(crate) body_blake3: String,
    pub(crate) body_bytes: usize,
    pub(crate) safe_error_code: Option<String>,
}

impl std::fmt::Display for HttpFailureEvidence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "body_blake3={} body_bytes={}",
            self.body_blake3, self.body_bytes
        )?;
        if let Some(code) = &self.safe_error_code {
            write!(formatter, " safe_error_code={code}")?;
        }
        Ok(())
    }
}

impl McpClientError {
    pub fn is_reconnectable(&self) -> bool {
        match self {
            Self::Http { .. } | Self::Timeout { .. } => true,
            Self::HttpStatus { status, .. } => {
                matches!(
                    status,
                    404 | 408 | 409 | 410 | 425 | 429 | 500 | 502 | 503 | 504
                )
            }
            Self::Protocol { reason, .. } => {
                let reason = reason.to_ascii_lowercase();
                reason.contains("closed")
                    || reason.contains("session")
                    || reason.contains("end of stream")
            }
            Self::ServerError { message, .. } => {
                let message = message.to_ascii_lowercase();
                [
                    "invalid or expired session",
                    "session expired",
                    "session not found",
                    "unknown session",
                    "session terminated",
                    "transport is closed",
                    "closed resource",
                    "broken pipe",
                    "end of file",
                ]
                .iter()
                .any(|marker| message.contains(marker))
            }
            _ => false,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedTransport(_) => "unsupported_transport",
            Self::Config(_) => "config",
            Self::Http { .. } => "transport",
            Self::HttpStatus {
                status: 401 | 403, ..
            } => "unauthorized",
            Self::HttpStatus {
                status: 404 | 409 | 410,
                ..
            } => "session_expired",
            Self::HttpStatus { .. } => "http_status",
            Self::Timeout { .. } => "timeout",
            Self::ResponseTooLarge { .. } => "response_too_large",
            Self::Protocol { .. } => "protocol",
            Self::ServerError { .. } if self.is_reconnectable() => "session_expired",
            Self::ServerError { .. } => "server_error",
            Self::MissingEnvironmentValue(_) => "missing_environment_value",
            Self::OAuth(error) => error.code(),
            Self::Journal(_) => "event_journal",
            Self::EventCursorGap { .. } => "event_cursor_gap",
            Self::AccountSignInRequired(_) => "account_sign_in_required",
            Self::AccountSignInExpired => "account_sign_in_expired",
            Self::AccountCredential(_) => "account_credential",
            Self::ToolNotAdmitted(_) => "tool_not_admitted",
            Self::SurfaceNotAdmitted(_) => "surface_not_admitted",
        }
    }

    pub fn structured(&self) -> Value {
        let mut body = json!({
            "code": self.code(),
            "message": sanitize_error(&self.to_string()),
            "retryable": self.is_reconnectable(),
        });
        if let Self::ServerError { code, data, .. } = self {
            body["jsonrpc_code"] = json!(code);
            if let Some(data) = data {
                body["data"] = sanitize_value(data);
            }
        }
        if let Self::HttpStatus {
            status,
            method,
            evidence,
        } = self
        {
            body["http_status"] = json!(status);
            body["method"] = json!(method);
            body["body_blake3"] = json!(&evidence.body_blake3);
            body["body_bytes"] = json!(evidence.body_bytes);
            body["safe_error_code"] = json!(&evidence.safe_error_code);
        }
        if let Self::OAuth(error) = self {
            if let Some(authorization) = error.authorization() {
                body["authorization"] = authorization.clone();
            }
            if let Some((endpoint, status, evidence)) = error.http_failure() {
                body["endpoint"] = json!(endpoint);
                body["http_status"] = json!(status);
                body["body_blake3"] = json!(&evidence.body_blake3);
                body["body_bytes"] = json!(evidence.body_bytes);
                body["safe_error_code"] = json!(&evidence.safe_error_code);
            }
        }
        if let Self::EventCursorGap {
            requested,
            earliest_available,
            latest,
        } = self
        {
            body["requested_cursor"] = json!(requested);
            body["earliest_available_cursor"] = json!(earliest_available);
            body["latest_cursor"] = json!(latest);
        }
        body
    }
}

pub(crate) fn http_failure_evidence(bytes: &[u8]) -> HttpFailureEvidence {
    HttpFailureEvidence {
        body_blake3: blake3::hash(bytes).to_hex().to_string(),
        body_bytes: bytes.len(),
        safe_error_code: safe_error_code(bytes),
    }
}

fn safe_error_code(bytes: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let code = [
        value.pointer("/error/code"),
        value.get("error"),
        value.get("code"),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| match value {
        Value::String(value) if safe_error_atom(value) => Some(value.clone()),
        Value::Number(value) => {
            let value = value.to_string();
            safe_error_atom(&value).then_some(value)
        }
        _ => None,
    });
    code
}

fn safe_error_atom(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

#[derive(Clone, Debug, Default)]
pub struct McpCapabilities {
    pub tools: bool,
    pub tools_list_changed: bool,
    pub resources: bool,
    pub resources_subscribe: bool,
    pub resources_list_changed: bool,
    pub prompts: bool,
    pub prompts_list_changed: bool,
    pub logging: bool,
    pub completions: bool,
}

impl McpCapabilities {
    fn parse(result: &Value) -> Result<Self, McpClientError> {
        let capabilities = result
            .get("capabilities")
            .and_then(Value::as_object)
            .ok_or_else(|| McpClientError::Protocol {
                method: "initialize".to_string(),
                reason: "initialize result has no capabilities object".to_string(),
            })?;
        Ok(Self {
            tools: capability_object(capabilities, "tools")?.is_some(),
            tools_list_changed: capability_flag(capabilities, "tools", "listChanged")?,
            resources: capability_object(capabilities, "resources")?.is_some(),
            resources_subscribe: capability_flag(capabilities, "resources", "subscribe")?,
            resources_list_changed: capability_flag(capabilities, "resources", "listChanged")?,
            prompts: capability_object(capabilities, "prompts")?.is_some(),
            prompts_list_changed: capability_flag(capabilities, "prompts", "listChanged")?,
            logging: capability_object(capabilities, "logging")?.is_some(),
            completions: capability_object(capabilities, "completions")?.is_some(),
        })
    }

    pub fn as_value(&self) -> Value {
        json!({
            "tools": { "supported": self.tools, "list_changed": self.tools_list_changed },
            "resources": {
                "supported": self.resources,
                "subscribe": self.resources_subscribe,
                "list_changed": self.resources_list_changed,
            },
            "prompts": { "supported": self.prompts, "list_changed": self.prompts_list_changed },
            "logging": self.logging,
            "completions": self.completions,
        })
    }
}

fn capability_object<'a>(
    capabilities: &'a Map<String, Value>,
    name: &str,
) -> Result<Option<&'a Map<String, Value>>, McpClientError> {
    match capabilities.get(name) {
        None => Ok(None),
        Some(Value::Object(value)) => Ok(Some(value)),
        Some(_) => Err(McpClientError::Protocol {
            method: "initialize".to_string(),
            reason: format!("initialize capability '{name}' is not an object"),
        }),
    }
}

fn capability_flag(
    capabilities: &Map<String, Value>,
    name: &str,
    flag: &str,
) -> Result<bool, McpClientError> {
    let Some(object) = capability_object(capabilities, name)? else {
        return Ok(false);
    };
    match object.get(flag) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(McpClientError::Protocol {
            method: "initialize".to_string(),
            reason: format!("initialize capability '{name}.{flag}' is not boolean"),
        }),
    }
}

pub struct McpClient {
    transport: RemoteTransport,
    initialize_result: Value,
    capabilities: McpCapabilities,
    next_id: AtomicI64,
    request_timeout: Duration,
    max_request_timeout: Duration,
    journal: Mutex<EventJournal>,
    closed: AtomicBool,
    in_flight: std::sync::Mutex<BTreeSet<i64>>,
    cancelled: std::sync::Mutex<BTreeSet<i64>>,
    tool_filter: Option<ToolMetaFilter>,
    admitted_tools: std::sync::Mutex<BTreeSet<String>>,
    max_catalog_pages: usize,
}

struct InFlightRequest<'a> {
    id: i64,
    requests: &'a std::sync::Mutex<BTreeSet<i64>>,
    cancelled: &'a std::sync::Mutex<BTreeSet<i64>>,
}

impl Drop for InFlightRequest<'_> {
    fn drop(&mut self) {
        if let Ok(mut requests) = self.requests.lock() {
            requests.remove(&self.id);
        }
        if let Ok(mut cancelled) = self.cancelled.lock() {
            cancelled.remove(&self.id);
        }
    }
}

impl McpClient {
    pub async fn connect(
        config: &McpInterfaceConfig,
        state_dir: &Path,
    ) -> Result<Self, McpClientError> {
        let journal = EventJournal::open(
            state_dir.join("events.jsonl"),
            config.event_journal_segment_bytes,
            config.event_journal_retained_segments,
        )?;
        let request_timeout = request_timeout(&config.transport)?;
        let http = build_http_client(&config.transport, request_timeout)?;
        let auth = HttpAuth::resolve(config, http.clone()).await?;
        let transport = match config.transport.wire.as_deref().map(str::trim) {
            Some(MCP_STREAMABLE_HTTP_WIRE) => RemoteTransport::Streamable(
                StreamableHttpClient::connect(
                    &config.transport,
                    config.protocol_versions[0].clone(),
                    http,
                    auth,
                )
                .await?,
            ),
            Some(MCP_SSE_WIRE) => RemoteTransport::Sse(
                tokio::time::timeout(
                    request_timeout,
                    LegacySseClient::connect(
                        &config.transport,
                        config.protocol_versions[0].clone(),
                        http,
                        auth,
                    ),
                )
                .await
                .map_err(|_| McpClientError::Timeout {
                    method: "sse/connect".to_string(),
                    timeout_ms: request_timeout.as_millis() as u64,
                })??,
            ),
            Some(other) => {
                return Err(McpClientError::UnsupportedTransport(format!(
                    "'{other}'; supported wires are '{MCP_STREAMABLE_HTTP_WIRE}' and '{MCP_SSE_WIRE}'"
                )));
            }
            None => {
                return Err(McpClientError::UnsupportedTransport(
                    "wire is absent; transport families never default implicitly".to_string(),
                ));
            }
        };
        let mut client = Self {
            transport,
            initialize_result: Value::Null,
            capabilities: McpCapabilities::default(),
            next_id: AtomicI64::new(1),
            request_timeout,
            max_request_timeout: Duration::from_millis(MAX_REQUEST_TIMEOUT_MS),
            journal: Mutex::new(journal),
            closed: AtomicBool::new(false),
            in_flight: std::sync::Mutex::new(BTreeSet::new()),
            cancelled: std::sync::Mutex::new(BTreeSet::new()),
            tool_filter: config.tool_filter.clone(),
            admitted_tools: std::sync::Mutex::new(BTreeSet::new()),
            max_catalog_pages: config.max_catalog_pages,
        };
        let preferred = config
            .protocol_versions
            .first()
            .ok_or_else(|| McpClientError::Config("protocol version list is empty".to_string()))?;
        let result = client
            .request_with_timeout(
                "initialize",
                initialize_params(preferred, &config.client_name, &config.client_version),
                request_timeout,
                false,
            )
            .await?;
        validate_initialize(&result, &config.protocol_versions)?;
        let protocol =
            result["protocolVersion"]
                .as_str()
                .ok_or_else(|| McpClientError::Protocol {
                    method: "initialize".to_string(),
                    reason: "validated protocolVersion disappeared".to_string(),
                })?;
        client.transport.set_protocol(protocol).await?;
        client.capabilities = McpCapabilities::parse(&result)?;
        client.initialize_result = result;
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        progress_token: Option<Value>,
        timeout_ms: Option<u64>,
    ) -> Result<Value, McpClientError> {
        self.require(self.capabilities.tools, "tools/call", "tools")?;
        self.admit_tool(name).await?;
        let mut params = json!({ "name": name, "arguments": arguments });
        insert_progress_token(&mut params, progress_token)?;
        self.request_with_optional_timeout("tools/call", params, timeout_ms)
            .await
    }

    pub async fn list_tools(&self, cursor: Option<&str>) -> Result<Value, McpClientError> {
        self.require(self.capabilities.tools, "tools/list", "tools")?;
        let page = self.request("tools/list", cursor_params(cursor)).await?;
        match &self.tool_filter {
            Some(filter) => self.filter_tool_page(filter, page),
            None => Ok(page),
        }
    }

    fn filter_tool_page(
        &self,
        filter: &ToolMetaFilter,
        mut page: Value,
    ) -> Result<Value, McpClientError> {
        let tools = page
            .get_mut("tools")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| McpClientError::Protocol {
                method: "tools/list".to_string(),
                reason: "tools/list result has no tools array".to_string(),
            })?;
        tools.retain(|tool| filter.admits(tool));
        let mut admitted = self
            .admitted_tools
            .lock()
            .map_err(|_| McpClientError::Protocol {
                method: "tools/list".to_string(),
                reason: "admitted tool set is poisoned".to_string(),
            })?;
        for tool in tools.iter() {
            let name = tool.get("name").and_then(Value::as_str).ok_or_else(|| {
                McpClientError::Protocol {
                    method: "tools/list".to_string(),
                    reason: "an admitted tool has no name".to_string(),
                }
            })?;
            admitted.insert(name.to_string());
        }
        Ok(page)
    }

    fn tool_is_admitted(&self, name: &str) -> Result<bool, McpClientError> {
        Ok(self
            .admitted_tools
            .lock()
            .map_err(|_| McpClientError::Protocol {
                method: "tools/call".to_string(),
                reason: "admitted tool set is poisoned".to_string(),
            })?
            .contains(name))
    }

    async fn admit_tool(&self, name: &str) -> Result<(), McpClientError> {
        if self.tool_filter.is_none() || self.tool_is_admitted(name)? {
            return Ok(());
        }
        let mut cursor: Option<String> = None;
        let mut seen = BTreeSet::new();
        for _ in 0..self.max_catalog_pages {
            let page = self.list_tools(cursor.as_deref()).await?;
            if self.tool_is_admitted(name)? {
                return Ok(());
            }
            cursor = match page.get("nextCursor") {
                None | Some(Value::Null) => {
                    return Err(McpClientError::ToolNotAdmitted(name.into()))
                }
                Some(Value::String(next)) if !next.trim().is_empty() => Some(next.trim().into()),
                Some(_) => {
                    return Err(McpClientError::Protocol {
                        method: "tools/list".to_string(),
                        reason: "nextCursor is not a non-empty string or null".to_string(),
                    });
                }
            };
            if !seen.insert(cursor.clone()) {
                return Err(McpClientError::Protocol {
                    method: "tools/list".to_string(),
                    reason: "tools/list repeated a cursor while admitting a tool".to_string(),
                });
            }
        }
        Err(McpClientError::Protocol {
            method: "tools/list".to_string(),
            reason: format!(
                "tools/list exceeded the {}-page boundary while admitting tool '{name}'",
                self.max_catalog_pages
            ),
        })
    }

    fn require_unfiltered(&self, method: &str) -> Result<(), McpClientError> {
        if self.tool_filter.is_some() {
            Err(McpClientError::SurfaceNotAdmitted(method.to_string()))
        } else {
            Ok(())
        }
    }

    pub async fn list_resources(&self, cursor: Option<&str>) -> Result<Value, McpClientError> {
        self.require_unfiltered("resources/list")?;
        self.require(self.capabilities.resources, "resources/list", "resources")?;
        self.request("resources/list", cursor_params(cursor)).await
    }

    pub async fn list_resource_templates(
        &self,
        cursor: Option<&str>,
    ) -> Result<Value, McpClientError> {
        self.require_unfiltered("resources/templates/list")?;
        self.require(
            self.capabilities.resources,
            "resources/templates/list",
            "resources",
        )?;
        self.request("resources/templates/list", cursor_params(cursor))
            .await
    }

    pub async fn read_resource(&self, uri: &str) -> Result<Value, McpClientError> {
        self.require_unfiltered("resources/read")?;
        self.require(self.capabilities.resources, "resources/read", "resources")?;
        self.request("resources/read", json!({ "uri": uri })).await
    }

    pub async fn subscribe_resource(&self, uri: &str) -> Result<Value, McpClientError> {
        self.require_unfiltered("resources/subscribe")?;
        self.require(
            self.capabilities.resources_subscribe,
            "resources/subscribe",
            "resources.subscribe",
        )?;
        self.request("resources/subscribe", json!({ "uri": uri }))
            .await
    }

    pub async fn unsubscribe_resource(&self, uri: &str) -> Result<Value, McpClientError> {
        self.require_unfiltered("resources/unsubscribe")?;
        self.require(
            self.capabilities.resources_subscribe,
            "resources/unsubscribe",
            "resources.subscribe",
        )?;
        self.request("resources/unsubscribe", json!({ "uri": uri }))
            .await
    }

    pub async fn list_prompts(&self, cursor: Option<&str>) -> Result<Value, McpClientError> {
        self.require_unfiltered("prompts/list")?;
        self.require(self.capabilities.prompts, "prompts/list", "prompts")?;
        self.request("prompts/list", cursor_params(cursor)).await
    }

    pub async fn get_prompt(&self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        self.require_unfiltered("prompts/get")?;
        self.require(self.capabilities.prompts, "prompts/get", "prompts")?;
        self.request(
            "prompts/get",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    pub async fn complete(
        &self,
        reference: Value,
        argument: Value,
        context: Option<Value>,
    ) -> Result<Value, McpClientError> {
        self.require_unfiltered("completion/complete")?;
        self.require(
            self.capabilities.completions,
            "completion/complete",
            "completions",
        )?;
        let mut params = json!({ "ref": reference, "argument": argument });
        if let Some(context) = context {
            params["context"] = context;
        }
        self.request("completion/complete", params).await
    }

    pub async fn set_logging_level(&self, level: &str) -> Result<Value, McpClientError> {
        self.require(self.capabilities.logging, "logging/setLevel", "logging")?;
        self.request("logging/setLevel", json!({ "level": level }))
            .await
    }

    pub async fn ping(&self) -> Result<Value, McpClientError> {
        self.request("ping", json!({})).await
    }

    pub async fn cancel_request(
        &self,
        request_id: Value,
        reason: Option<&str>,
    ) -> Result<(), McpClientError> {
        let numeric_id = request_id.as_i64().ok_or_else(|| {
            McpClientError::Config("cancel request_id must be an integer".to_string())
        })?;
        if self
            .in_flight
            .lock()
            .map_err(|_| McpClientError::Protocol {
                method: "notifications/cancelled".to_string(),
                reason: "in-flight request set is poisoned".to_string(),
            })?
            .contains(&numeric_id)
        {
            self.cancelled
                .lock()
                .map_err(|_| McpClientError::Protocol {
                    method: "notifications/cancelled".to_string(),
                    reason: "cancelled request set is poisoned".to_string(),
                })?
                .insert(numeric_id);
            self.transport.cancel_pending(numeric_id);
        }
        let mut params = json!({ "requestId": request_id });
        if let Some(reason) = reason {
            params["reason"] = json!(reason);
        }
        self.notify("notifications/cancelled", params).await
    }

    pub async fn send_progress(
        &self,
        progress_token: Value,
        progress: f64,
        total: Option<f64>,
        message: Option<&str>,
    ) -> Result<(), McpClientError> {
        if !progress.is_finite() || progress < 0.0 {
            return Err(McpClientError::Config(
                "progress must be a finite non-negative number".to_string(),
            ));
        }
        let mut params = json!({ "progressToken": progress_token, "progress": progress });
        if let Some(total) = total {
            if !total.is_finite() || total < progress {
                return Err(McpClientError::Config(
                    "progress total must be finite and greater than or equal to progress"
                        .to_string(),
                ));
            }
            params["total"] = json!(total);
        }
        if let Some(message) = message {
            params["message"] = json!(message);
        }
        self.notify("notifications/progress", params).await
    }

    pub async fn respond(
        &self,
        request_id: Value,
        result: Option<Value>,
        error: Option<Value>,
    ) -> Result<(), McpClientError> {
        self.ensure_open("response")?;
        if result.is_some() == error.is_some() {
            return Err(McpClientError::Config(
                "mcp response requires exactly one of result or error".to_string(),
            ));
        }
        let body = match (result, error) {
            (Some(result), None) => json!({ "jsonrpc": "2.0", "id": request_id, "result": result }),
            (None, Some(error)) => json!({ "jsonrpc": "2.0", "id": request_id, "error": error }),
            _ => unreachable!(),
        };
        let exchange = self.transport.exchange(&body, None).await?;
        self.ensure_open("response")?;
        self.record_wire_events(exchange.events).await
    }

    pub async fn poll_events(
        &self,
        after_cursor: u64,
        max_events: usize,
        wait_ms: u64,
    ) -> Result<Value, McpClientError> {
        self.ensure_open("events/poll")?;
        if max_events == 0 || max_events > MAX_EVENT_POLL {
            return Err(McpClientError::Config(format!(
                "max_events must be in 1..={MAX_EVENT_POLL}"
            )));
        }
        if wait_ms > MAX_EVENT_WAIT_MS {
            return Err(McpClientError::Config(format!(
                "wait_ms must be in 0..={MAX_EVENT_WAIT_MS}"
            )));
        }
        let existing = self
            .journal
            .lock()
            .await
            .read_after(after_cursor, max_events)?;
        let events = if existing.is_empty() && wait_ms > 0 {
            let transport_cursor = self.journal.lock().await.last_transport_cursor();
            let incoming = self
                .transport
                .poll(Duration::from_millis(wait_ms), transport_cursor.as_deref())
                .await?;
            self.ensure_open("events/poll")?;
            self.record_wire_events(incoming).await?;
            self.journal
                .lock()
                .await
                .read_after(after_cursor, max_events)?
        } else {
            existing
        };
        let next_cursor = events.last().map(|row| row.cursor).unwrap_or(after_cursor);
        Ok(json!({
            "events": events,
            "next_cursor": next_cursor,
            "durable": true,
        }))
    }

    pub async fn close(&self) -> Result<Value, McpClientError> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Err(McpClientError::Protocol {
                method: "close".to_string(),
                reason: "MCP client is already closed".to_string(),
            });
        }
        self.transport.close().await?;
        Ok(json!({ "closed": true }))
    }

    pub async fn retire(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ = self.transport.close().await;
        }
    }

    pub fn initialize_result(&self) -> &Value {
        &self.initialize_result
    }

    pub fn capabilities(&self) -> &McpCapabilities {
        &self.capabilities
    }

    pub async fn status(&self) -> Value {
        let journal = self.journal.lock().await;
        json!({
            "initialize": self.initialize_result,
            "capabilities": self.capabilities.as_value(),
            "transport": self.transport.status().await,
            "event_cursor": journal.last_cursor(),
            "event_earliest_cursor": journal.earliest_cursor(),
            "event_journal": journal.path(),
            "closed": self.closed.load(Ordering::Acquire),
            "in_flight_request_ids": self.in_flight.lock().map(|ids| ids.iter().copied().collect::<Vec<_>>()).unwrap_or_default(),
            "tool_filter": self.tool_filter.as_ref().map(ToolMetaFilter::as_value),
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpClientError> {
        self.request_with_timeout(method, params, self.request_timeout, true)
            .await
    }

    async fn request_with_optional_timeout(
        &self,
        method: &str,
        params: Value,
        timeout_ms: Option<u64>,
    ) -> Result<Value, McpClientError> {
        let timeout = match timeout_ms {
            None => self.request_timeout,
            Some(0) => {
                return Err(McpClientError::Config(
                    "timeout_ms must be greater than zero".to_string(),
                ));
            }
            Some(value) => {
                let timeout = Duration::from_millis(value);
                if timeout > self.max_request_timeout {
                    return Err(McpClientError::Config(format!(
                        "timeout_ms exceeds {}",
                        self.max_request_timeout.as_millis()
                    )));
                }
                timeout
            }
        };
        self.request_with_timeout(method, params, timeout, true)
            .await
    }

    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel_on_timeout: bool,
    ) -> Result<Value, McpClientError> {
        self.ensure_open(method)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if id <= 0 {
            return Err(McpClientError::Protocol {
                method: method.to_string(),
                reason: "request id overflowed".to_string(),
            });
        }
        {
            let mut in_flight = self
                .in_flight
                .lock()
                .map_err(|_| McpClientError::Protocol {
                    method: method.to_string(),
                    reason: "in-flight request set is poisoned".to_string(),
                })?;
            if !in_flight.insert(id) {
                return Err(McpClientError::Protocol {
                    method: method.to_string(),
                    reason: format!("request id {id} is already in flight"),
                });
            }
        }
        let _in_flight = InFlightRequest {
            id,
            requests: &self.in_flight,
            cancelled: &self.cancelled,
        };
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let exchange =
            match tokio::time::timeout(timeout, self.transport.exchange(&body, Some(id))).await {
                Ok(result) => result?,
                Err(_) => {
                    if cancel_on_timeout {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(2),
                            self.cancel_request(json!(id), Some("client request timeout")),
                        )
                        .await;
                    }
                    return Err(McpClientError::Timeout {
                        method: method.to_string(),
                        timeout_ms: timeout.as_millis() as u64,
                    });
                }
            };
        self.ensure_open(method)?;
        if self
            .cancelled
            .lock()
            .map_err(|_| McpClientError::Protocol {
                method: method.to_string(),
                reason: "cancelled request set is poisoned".to_string(),
            })?
            .remove(&id)
        {
            return Err(McpClientError::Protocol {
                method: method.to_string(),
                reason: format!("request id {id} was cancelled"),
            });
        }
        self.record_wire_events(exchange.events).await?;
        extract_result(method, id, exchange.response)
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpClientError> {
        self.ensure_open(method)?;
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let exchange = self.transport.exchange(&body, None).await?;
        self.ensure_open(method)?;
        self.record_wire_events(exchange.events).await
    }

    async fn record_wire_events(&self, events: Vec<WireEvent>) -> Result<(), McpClientError> {
        if events.is_empty() {
            return Ok(());
        }
        let mut journal = self.journal.lock().await;
        for event in events {
            journal.append(event)?;
        }
        Ok(())
    }

    fn require(&self, allowed: bool, method: &str, capability: &str) -> Result<(), McpClientError> {
        if allowed {
            Ok(())
        } else {
            Err(McpClientError::Protocol {
                method: method.to_string(),
                reason: format!(
                    "server did not declare the '{capability}' capability during initialize"
                ),
            })
        }
    }

    fn ensure_open(&self, method: &str) -> Result<(), McpClientError> {
        if self.closed.load(Ordering::Acquire) {
            Err(McpClientError::Protocol {
                method: method.to_string(),
                reason: "MCP client is closed".to_string(),
            })
        } else {
            Ok(())
        }
    }
}

fn initialize_params(protocol_version: &str, client_name: &str, client_version: &str) -> Value {
    json!({
        "protocolVersion": protocol_version,
        "capabilities": {},
        "clientInfo": { "name": client_name, "version": client_version }
    })
}

fn validate_initialize(result: &Value, supported: &[String]) -> Result<(), McpClientError> {
    let object = result.as_object().ok_or_else(|| McpClientError::Protocol {
        method: "initialize".to_string(),
        reason: "initialize result is not an object".to_string(),
    })?;
    let protocol = object
        .get("protocolVersion")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| McpClientError::Protocol {
            method: "initialize".to_string(),
            reason: "initialize result has no non-empty protocolVersion".to_string(),
        })?;
    if !supported.iter().any(|value| value == protocol) {
        return Err(McpClientError::Protocol {
            method: "initialize".to_string(),
            reason: format!(
                "server selected unsupported protocolVersion '{protocol}'; supported versions are {supported:?}"
            ),
        });
    }
    if !object.get("serverInfo").is_some_and(Value::is_object) {
        return Err(McpClientError::Protocol {
            method: "initialize".to_string(),
            reason: "initialize result has no serverInfo object".to_string(),
        });
    }
    McpCapabilities::parse(result)?;
    Ok(())
}

fn insert_progress_token(params: &mut Value, token: Option<Value>) -> Result<(), McpClientError> {
    let Some(token) = token else {
        return Ok(());
    };
    if !token.is_string() && !token.is_number() {
        return Err(McpClientError::Config(
            "progress_token must be a string or number".to_string(),
        ));
    }
    params["_meta"] = json!({ "progressToken": token });
    Ok(())
}

fn cursor_params(cursor: Option<&str>) -> Value {
    match cursor.map(str::trim).filter(|cursor| !cursor.is_empty()) {
        Some(cursor) => json!({ "cursor": cursor }),
        None => json!({}),
    }
}

struct RpcExchange {
    response: Option<Value>,
    events: Vec<WireEvent>,
}

#[derive(Clone, Debug)]
struct WireEvent {
    transport_cursor: Option<String>,
    body: Value,
}

enum RemoteTransport {
    Streamable(StreamableHttpClient),
    Sse(LegacySseClient),
}

impl RemoteTransport {
    async fn exchange(
        &self,
        body: &Value,
        expected_id: Option<i64>,
    ) -> Result<RpcExchange, McpClientError> {
        match self {
            Self::Streamable(client) => client.exchange(body, expected_id).await,
            Self::Sse(client) => client.exchange(body, expected_id).await,
        }
    }

    async fn poll(
        &self,
        wait: Duration,
        last_event_id: Option<&str>,
    ) -> Result<Vec<WireEvent>, McpClientError> {
        match self {
            Self::Streamable(client) => client.poll(wait, last_event_id).await,
            Self::Sse(client) => client.poll(wait).await,
        }
    }

    async fn set_protocol(&self, protocol: &str) -> Result<(), McpClientError> {
        match self {
            Self::Streamable(client) => client.set_protocol(protocol).await,
            Self::Sse(client) => client.set_protocol(protocol).await,
        }
    }

    async fn close(&self) -> Result<(), McpClientError> {
        match self {
            Self::Streamable(client) => client.close().await,
            Self::Sse(client) => client.close().await,
        }
    }

    async fn status(&self) -> Value {
        match self {
            Self::Streamable(client) => client.status().await,
            Self::Sse(client) => client.status().await,
        }
    }

    fn cancel_pending(&self, request_id: i64) {
        if let Self::Sse(client) = self {
            client.cancel_pending(request_id);
        }
    }
}

struct HttpAuth {
    static_header: Option<(HeaderName, HeaderValue)>,
    oauth: Option<Arc<Mutex<OAuthClient>>>,
    account: Option<AccountAuth>,
}

struct AccountAuth {
    settings: AccountAuthSettings,
    applied: std::sync::Mutex<Option<blake3::Hash>>,
}

impl HttpAuth {
    async fn resolve(
        config: &McpInterfaceConfig,
        http: reqwest::Client,
    ) -> Result<Self, McpClientError> {
        if config.oauth.is_some() && config.transport.auth_env_key.is_some() {
            return Err(McpClientError::Config(
                "oauth and transport.auth_env_key are mutually exclusive".to_string(),
            ));
        }
        if config.account_auth.is_some()
            && (config.oauth.is_some() || config.transport.auth_env_key.is_some())
        {
            return Err(McpClientError::Config(
                "private_config.auth excludes oauth and transport.auth_env_key".to_string(),
            ));
        }
        let static_header = resolve_auth_header(&config.transport)?;
        let oauth = match config.oauth.clone() {
            Some(settings) => {
                let url = resolve_base_url(&config.transport)?;
                Some(Arc::new(Mutex::new(OAuthClient::open(
                    settings, &url, http,
                )?)))
            }
            None => None,
        };
        let account = config.account_auth.clone().map(|settings| AccountAuth {
            settings,
            applied: std::sync::Mutex::new(None),
        });
        Ok(Self {
            static_header,
            oauth,
            account,
        })
    }

    fn owned_header_names(&self) -> Result<Vec<HeaderName>, McpClientError> {
        let mut names = self
            .static_header
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        if let Some(account) = &self.account {
            names.extend(account.settings.owned_header_names()?);
        }
        Ok(names)
    }

    async fn apply(&self, headers: &mut HeaderMap) -> Result<(), McpClientError> {
        if let Some((name, value)) = &self.static_header {
            headers.insert(name.clone(), value.clone());
        }
        if let Some(oauth) = &self.oauth {
            let token = oauth.lock().await.access_token().await?;
            let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|error| {
                McpClientError::Config(format!("OAuth token is invalid: {error}"))
            })?;
            headers.insert(HeaderName::from_static("authorization"), value);
        }
        if let Some(account) = &self.account {
            let credential = account.settings.read_credential()?;
            for (name, value) in credential.headers {
                headers.insert(name, value);
            }
            *account.applied.lock().map_err(|_| {
                McpClientError::AccountCredential("applied credential state is poisoned".into())
            })? = Some(credential.fingerprint);
        }
        Ok(())
    }

    async fn recover_unauthorized(&self) -> Result<bool, McpClientError> {
        if let Some(account) = &self.account {
            let current = account.settings.read_credential()?;
            let applied = *account.applied.lock().map_err(|_| {
                McpClientError::AccountCredential("applied credential state is poisoned".into())
            })?;
            return if applied == Some(current.fingerprint) {
                Err(McpClientError::AccountSignInExpired)
            } else {
                Ok(true)
            };
        }
        match &self.oauth {
            Some(oauth) => Ok(oauth.lock().await.refresh_after_unauthorized().await?),
            None => Ok(false),
        }
    }

    async fn status(&self) -> Value {
        if let Some(account) = &self.account {
            return account.settings.status();
        }
        match &self.oauth {
            Some(oauth) => oauth.lock().await.status(),
            None => json!({ "configured": false, "static_bearer": self.static_header.is_some() }),
        }
    }
}

struct StreamableHttpClient {
    http: reqwest::Client,
    url: String,
    auth: HttpAuth,
    extra_headers: HeaderMap,
    session_id: Mutex<Option<HeaderValue>>,
    protocol_version: Mutex<String>,
    max_response_bytes: u64,
}

impl StreamableHttpClient {
    async fn connect(
        transport: &InterfaceTransportConfig,
        protocol_version: String,
        http: reqwest::Client,
        auth: HttpAuth,
    ) -> Result<Self, McpClientError> {
        let url = resolve_base_url(transport)?;
        validate_remote_url(&url)?;
        let max_response_bytes = max_response_bytes(transport)?;
        let extra_headers = resolve_extra_headers(transport, &auth.owned_header_names()?)?;
        Ok(Self {
            http,
            url,
            auth,
            extra_headers,
            session_id: Mutex::new(None),
            protocol_version: Mutex::new(protocol_version),
            max_response_bytes,
        })
    }

    async fn exchange(
        &self,
        body: &Value,
        expected_id: Option<i64>,
    ) -> Result<RpcExchange, McpClientError> {
        let method = body
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("response");
        let mut retried_auth = false;
        loop {
            let headers = self.headers().await?;
            let response = self
                .http
                .post(&self.url)
                .headers(headers)
                .json(body)
                .send()
                .await
                .map_err(|source| McpClientError::Http {
                    url: self.url.clone(),
                    source,
                })?;
            if response.status().as_u16() == 401
                && !retried_auth
                && self.auth.recover_unauthorized().await?
            {
                retried_auth = true;
                continue;
            }
            if response.status().as_u16() == 401 && self.auth.account.is_some() {
                return Err(McpClientError::AccountSignInExpired);
            }
            return self.decode_response(method, response, expected_id).await;
        }
    }

    async fn decode_response(
        &self,
        method: &str,
        response: reqwest::Response,
        expected_id: Option<i64>,
    ) -> Result<RpcExchange, McpClientError> {
        self.capture_session(response.headers()).await?;
        let status = response.status();
        if expected_id.is_none() && status.as_u16() == 202 {
            return Ok(RpcExchange {
                response: None,
                events: Vec::new(),
            });
        }
        let content_type = response_content_type(&response);
        let bytes = read_bounded(response, method, self.max_response_bytes).await?;
        if !status.is_success() {
            if matches!(status.as_u16(), 404 | 410) {
                *self.session_id.lock().await = None;
            }
            return Err(McpClientError::HttpStatus {
                status: status.as_u16(),
                method: method.to_string(),
                evidence: http_failure_evidence(&bytes),
            });
        }
        if expected_id.is_none() && bytes.is_empty() {
            return Ok(RpcExchange {
                response: None,
                events: Vec::new(),
            });
        }
        if content_type == "application/json" {
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|error| McpClientError::Protocol {
                    method: method.to_string(),
                    reason: format!("application/json body is malformed: {error}"),
                })?;
            classify_messages(method, expected_id, vec![(None, value)])
        } else if content_type == "text/event-stream" {
            let frames = SseParser::parse_complete(&bytes, self.max_response_bytes as usize)?;
            let messages = frames
                .into_iter()
                .filter(|frame| frame.event.as_deref() != Some("endpoint"))
                .map(|frame| frame.into_json(method))
                .collect::<Result<Vec<_>, _>>()?;
            classify_messages(method, expected_id, messages)
        } else {
            Err(McpClientError::Protocol {
                method: method.to_string(),
                reason: format!(
                    "successful response has unsupported content-type '{content_type}'"
                ),
            })
        }
    }

    async fn poll(
        &self,
        wait: Duration,
        last_event_id: Option<&str>,
    ) -> Result<Vec<WireEvent>, McpClientError> {
        let mut headers = self.headers().await?;
        headers.insert("accept", HeaderValue::from_static("text/event-stream"));
        if let Some(cursor) = last_event_id {
            headers.insert(
                HeaderName::from_static(LAST_EVENT_ID_HEADER),
                HeaderValue::from_str(cursor).map_err(|error| {
                    McpClientError::Config(format!("event cursor cannot form a header: {error}"))
                })?,
            );
        }
        let response = self
            .http
            .get(&self.url)
            .headers(headers)
            .send()
            .await
            .map_err(|source| McpClientError::Http {
                url: self.url.clone(),
                source,
            })?;
        self.capture_session(response.headers()).await?;
        if !response.status().is_success() {
            let status = response.status();
            let bytes = read_bounded(response, "events/get", self.max_response_bytes).await?;
            return Err(McpClientError::HttpStatus {
                status: status.as_u16(),
                method: "events/get".to_string(),
                evidence: http_failure_evidence(&bytes),
            });
        }
        if response_content_type(&response) != "text/event-stream" {
            return Err(McpClientError::Protocol {
                method: "events/get".to_string(),
                reason: "event GET did not return text/event-stream".to_string(),
            });
        }
        collect_sse_events(response, wait, self.max_response_bytes, MAX_EVENT_POLL).await
    }

    async fn headers(&self) -> Result<HeaderMap, McpClientError> {
        let mut headers = self.extra_headers.clone();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        headers.insert(
            "accept",
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        let protocol = self.protocol_version.lock().await.clone();
        headers.insert(
            HeaderName::from_static(PROTOCOL_HEADER),
            HeaderValue::from_str(&protocol).map_err(|error| {
                McpClientError::Config(format!(
                    "negotiated protocol version cannot form an HTTP header: {error}"
                ))
            })?,
        );
        if let Some(session) = self.session_id.lock().await.clone() {
            headers.insert(HeaderName::from_static(SESSION_HEADER), session);
        }
        self.auth.apply(&mut headers).await?;
        Ok(headers)
    }

    async fn capture_session(&self, headers: &HeaderMap) -> Result<(), McpClientError> {
        if let Some(value) = headers.get(SESSION_HEADER) {
            if value.as_bytes().is_empty() {
                return Err(McpClientError::Protocol {
                    method: "session".to_string(),
                    reason: "Mcp-Session-Id response header is empty".to_string(),
                });
            }
            let mut session = self.session_id.lock().await;
            if session.as_ref().is_some_and(|existing| existing != value) {
                return Err(McpClientError::Protocol {
                    method: "session".to_string(),
                    reason: "MCP server changed the session id on an active client".to_string(),
                });
            }
            *session = Some(value.clone());
        }
        Ok(())
    }

    async fn set_protocol(&self, protocol: &str) -> Result<(), McpClientError> {
        *self.protocol_version.lock().await = protocol.to_string();
        Ok(())
    }

    async fn close(&self) -> Result<(), McpClientError> {
        let Some(_) = self.session_id.lock().await.as_ref() else {
            return Ok(());
        };
        let response = self
            .http
            .delete(&self.url)
            .headers(self.headers().await?)
            .send()
            .await
            .map_err(|source| McpClientError::Http {
                url: self.url.clone(),
                source,
            })?;
        if !response.status().is_success() && response.status().as_u16() != 405 {
            let status = response.status();
            let bytes = read_bounded(response, "session/delete", self.max_response_bytes).await?;
            return Err(McpClientError::HttpStatus {
                status: status.as_u16(),
                method: "session/delete".to_string(),
                evidence: http_failure_evidence(&bytes),
            });
        }
        *self.session_id.lock().await = None;
        Ok(())
    }

    async fn status(&self) -> Value {
        json!({
            "wire": MCP_STREAMABLE_HTTP_WIRE,
            "endpoint": public_endpoint(&self.url),
            "has_session": self.session_id.lock().await.is_some(),
            "protocol_version": self.protocol_version.lock().await.clone(),
            "auth": self.auth.status().await,
        })
    }
}

struct LegacySseClient {
    http: reqwest::Client,
    source_url: String,
    endpoint_url: String,
    auth: HttpAuth,
    extra_headers: HeaderMap,
    protocol_version: Mutex<String>,
    pending: Arc<std::sync::Mutex<BTreeMap<i64, oneshot::Sender<Result<Value, String>>>>>,
    retired: Arc<std::sync::Mutex<BTreeSet<i64>>>,
    events: Mutex<mpsc::Receiver<Result<WireEvent, String>>>,
    reader: tokio::task::JoinHandle<()>,
    dispatcher: tokio::task::JoinHandle<()>,
    max_response_bytes: u64,
}

impl LegacySseClient {
    async fn connect(
        transport: &InterfaceTransportConfig,
        protocol_version: String,
        http: reqwest::Client,
        auth: HttpAuth,
    ) -> Result<Self, McpClientError> {
        let source_url = resolve_base_url(transport)?;
        validate_remote_url(&source_url)?;
        let max_response_bytes = max_response_bytes(transport)?;
        let extra_headers = resolve_extra_headers(transport, &auth.owned_header_names()?)?;
        let mut retried_auth = false;
        let response = loop {
            let mut headers = extra_headers.clone();
            headers.insert("accept", HeaderValue::from_static("text/event-stream"));
            headers.insert(
                HeaderName::from_static(PROTOCOL_HEADER),
                HeaderValue::from_str(&protocol_version).map_err(|error| {
                    McpClientError::Config(format!("protocol header is invalid: {error}"))
                })?,
            );
            auth.apply(&mut headers).await?;
            let response = http
                .get(&source_url)
                .headers(headers)
                .send()
                .await
                .map_err(|source| McpClientError::Http {
                    url: source_url.clone(),
                    source,
                })?;
            if response.status().as_u16() == 401
                && !retried_auth
                && auth.recover_unauthorized().await?
            {
                retried_auth = true;
                continue;
            }
            break response;
        };
        let status = response.status();
        if !status.is_success() {
            let bytes = read_bounded(response, "sse/connect", max_response_bytes).await?;
            return Err(McpClientError::HttpStatus {
                status: status.as_u16(),
                method: "sse/connect".to_string(),
                evidence: http_failure_evidence(&bytes),
            });
        }
        if response_content_type(&response) != "text/event-stream" {
            return Err(McpClientError::Protocol {
                method: "sse/connect".to_string(),
                reason: "legacy SSE endpoint did not return text/event-stream".to_string(),
            });
        }
        let (tx, mut rx) = mpsc::channel::<Result<SseFrame, String>>(LEGACY_SSE_FRAME_BUFFER);
        let reader = tokio::spawn(read_sse_stream(response, tx, max_response_bytes));
        let frame = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .map_err(|_| McpClientError::Timeout {
                method: "sse/endpoint".to_string(),
                timeout_ms: 30_000,
            })?
            .ok_or_else(|| McpClientError::Protocol {
                method: "sse/endpoint".to_string(),
                reason: "legacy SSE stream closed before endpoint event".to_string(),
            })?
            .map_err(|reason| McpClientError::Protocol {
                method: "sse/endpoint".to_string(),
                reason,
            })?;
        if frame.event.as_deref() != Some("endpoint") {
            return Err(McpClientError::Protocol {
                method: "sse/endpoint".to_string(),
                reason: "first legacy SSE frame is not an endpoint event".to_string(),
            });
        }
        let endpoint_url = resolve_legacy_endpoint(&source_url, &frame.data)?;
        let pending = Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let retired = Arc::new(std::sync::Mutex::new(BTreeSet::new()));
        let (event_tx, events) = mpsc::channel(LEGACY_SSE_EVENT_BUFFER);
        let dispatcher = tokio::spawn(dispatch_legacy_sse(
            rx,
            Arc::clone(&pending),
            Arc::clone(&retired),
            event_tx,
        ));
        Ok(Self {
            http,
            source_url,
            endpoint_url,
            auth,
            extra_headers,
            protocol_version: Mutex::new(protocol_version),
            pending,
            retired,
            events: Mutex::new(events),
            reader,
            dispatcher,
            max_response_bytes,
        })
    }

    async fn exchange(
        &self,
        body: &Value,
        expected_id: Option<i64>,
    ) -> Result<RpcExchange, McpClientError> {
        let method = body
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("response");
        let mut response_receiver = None;
        let mut registration = None;
        if let Some(id) = expected_id {
            let (sender, receiver) = oneshot::channel();
            let mut pending = self.pending.lock().map_err(|_| McpClientError::Protocol {
                method: method.to_string(),
                reason: "legacy SSE pending map is poisoned".to_string(),
            })?;
            if pending.insert(id, sender).is_some() {
                return Err(McpClientError::Protocol {
                    method: method.to_string(),
                    reason: format!("legacy SSE request id {id} is already pending"),
                });
            }
            drop(pending);
            response_receiver = Some(receiver);
            registration = Some(LegacyPendingRegistration {
                id,
                pending: Arc::clone(&self.pending),
                retired: Arc::clone(&self.retired),
            });
        }
        let mut retried_auth = false;
        let response = loop {
            let mut headers = self.extra_headers.clone();
            headers.insert("content-type", HeaderValue::from_static("application/json"));
            headers.insert(
                HeaderName::from_static(PROTOCOL_HEADER),
                HeaderValue::from_str(&self.protocol_version.lock().await).map_err(|error| {
                    McpClientError::Config(format!("protocol header is invalid: {error}"))
                })?,
            );
            self.auth.apply(&mut headers).await?;
            let response = self
                .http
                .post(&self.endpoint_url)
                .headers(headers)
                .json(body)
                .send()
                .await
                .map_err(|source| McpClientError::Http {
                    url: self.endpoint_url.clone(),
                    source,
                })?;
            if response.status().as_u16() == 401
                && !retried_auth
                && self.auth.recover_unauthorized().await?
            {
                retried_auth = true;
                continue;
            }
            break response;
        };
        let status = response.status();
        let bytes = read_bounded(response, method, self.max_response_bytes).await?;
        if !status.is_success() {
            return Err(McpClientError::HttpStatus {
                status: status.as_u16(),
                method: method.to_string(),
                evidence: http_failure_evidence(&bytes),
            });
        }
        let Some(expected_id) = expected_id else {
            return Ok(RpcExchange {
                response: None,
                events: Vec::new(),
            });
        };
        let receiver = response_receiver.ok_or_else(|| McpClientError::Protocol {
            method: method.to_string(),
            reason: format!("legacy SSE request id {expected_id} has no response receiver"),
        })?;
        let response = receiver
            .await
            .map_err(|_| McpClientError::Protocol {
                method: method.to_string(),
                reason: "legacy SSE response dispatcher stopped".to_string(),
            })?
            .map_err(|reason| McpClientError::Protocol {
                method: method.to_string(),
                reason,
            })?;
        drop(registration);
        Ok(RpcExchange {
            response: Some(response),
            events: Vec::new(),
        })
    }

    async fn poll(&self, wait: Duration) -> Result<Vec<WireEvent>, McpClientError> {
        let mut messages = self.events.lock().await;
        let mut events = Vec::new();
        let first = tokio::time::timeout(wait, messages.recv()).await;
        let Ok(Some(first)) = first else {
            return Ok(events);
        };
        let first = first.map_err(|reason| McpClientError::Protocol {
            method: "events/poll".to_string(),
            reason,
        })?;
        events.push(first);
        while events.len() < MAX_EVENT_POLL {
            match messages.try_recv() {
                Ok(Ok(event)) => events.push(event),
                Ok(Err(reason)) => {
                    return Err(McpClientError::Protocol {
                        method: "events/poll".to_string(),
                        reason,
                    });
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    return Err(McpClientError::Protocol {
                        method: "events/poll".to_string(),
                        reason: "legacy SSE reader stopped".to_string(),
                    });
                }
            }
        }
        Ok(events)
    }

    async fn set_protocol(&self, protocol: &str) -> Result<(), McpClientError> {
        *self.protocol_version.lock().await = protocol.to_string();
        Ok(())
    }

    async fn close(&self) -> Result<(), McpClientError> {
        self.reader.abort();
        self.dispatcher.abort();
        fail_legacy_pending(&self.pending, "legacy SSE transport closed");
        Ok(())
    }

    fn cancel_pending(&self, request_id: i64) {
        let sender = self
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&request_id));
        if let Some(sender) = sender {
            if let Ok(mut retired) = self.retired.lock() {
                retired.insert(request_id);
            }
            let _ = sender.send(Err(format!(
                "legacy SSE request {request_id} was cancelled"
            )));
        }
    }

    async fn status(&self) -> Value {
        json!({
            "wire": MCP_SSE_WIRE,
            "source_endpoint": public_endpoint(&self.source_url),
            "message_endpoint": public_endpoint(&self.endpoint_url),
            "protocol_version": self.protocol_version.lock().await.clone(),
            "reader_finished": self.reader.is_finished(),
            "dispatcher_finished": self.dispatcher.is_finished(),
            "pending_requests": self.pending.lock().map(|value| value.len()).unwrap_or(0),
            "auth": self.auth.status().await,
        })
    }
}

struct LegacyPendingRegistration {
    id: i64,
    pending: Arc<std::sync::Mutex<BTreeMap<i64, oneshot::Sender<Result<Value, String>>>>>,
    retired: Arc<std::sync::Mutex<BTreeSet<i64>>>,
}

impl Drop for LegacyPendingRegistration {
    fn drop(&mut self) {
        let removed = self
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&self.id))
            .is_some();
        if removed {
            if let Ok(mut retired) = self.retired.lock() {
                if retired.len() >= 4096 {
                    if let Some(first) = retired.first().copied() {
                        retired.remove(&first);
                    }
                }
                retired.insert(self.id);
            }
        }
    }
}

async fn dispatch_legacy_sse(
    mut frames: mpsc::Receiver<Result<SseFrame, String>>,
    pending: Arc<std::sync::Mutex<BTreeMap<i64, oneshot::Sender<Result<Value, String>>>>>,
    retired: Arc<std::sync::Mutex<BTreeSet<i64>>>,
    events: mpsc::Sender<Result<WireEvent, String>>,
) {
    while let Some(frame) = frames.recv().await {
        let frame = match frame {
            Ok(frame) => frame,
            Err(reason) => {
                fail_legacy_pending(&pending, &reason);
                let _ = try_send_legacy_event(&events, Err(reason));
                return;
            }
        };
        let (cursor, body) = match frame.into_json("sse/stream") {
            Ok(message) => message,
            Err(error) => {
                let reason = error.to_string();
                fail_legacy_pending(&pending, &reason);
                let _ = try_send_legacy_event(&events, Err(reason));
                return;
            }
        };
        if body.get("method").is_none() {
            let Some(id) = body.get("id").and_then(Value::as_i64) else {
                let reason = "legacy SSE response id is absent or non-integer".to_string();
                fail_legacy_pending(&pending, &reason);
                let _ = try_send_legacy_event(&events, Err(reason));
                return;
            };
            let sender = pending
                .lock()
                .ok()
                .and_then(|mut pending| pending.remove(&id));
            if let Some(sender) = sender {
                let _ = sender.send(Ok(body));
                continue;
            }
            let was_retired = retired.lock().is_ok_and(|mut retired| retired.remove(&id));
            if was_retired {
                let late = WireEvent {
                    transport_cursor: cursor,
                    body: json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/octopus_late_response",
                        "params": { "requestId": id, "response": body },
                    }),
                };
                if let Err(reason) = try_send_legacy_event(&events, Ok(late)) {
                    fail_legacy_pending(&pending, &reason);
                    return;
                }
                continue;
            }
            let reason = format!("legacy SSE response id {id} has no pending request");
            fail_legacy_pending(&pending, &reason);
            let _ = try_send_legacy_event(&events, Err(reason));
            return;
        }
        if let Err(error) = validate_event_message(&body, "sse/stream") {
            let reason = error.to_string();
            fail_legacy_pending(&pending, &reason);
            let _ = try_send_legacy_event(&events, Err(reason));
            return;
        }
        if let Err(reason) = try_send_legacy_event(
            &events,
            Ok(WireEvent {
                transport_cursor: cursor,
                body,
            }),
        ) {
            fail_legacy_pending(&pending, &reason);
            return;
        }
    }
    fail_legacy_pending(&pending, "legacy SSE response dispatcher stopped");
}

fn try_send_legacy_event(
    events: &mpsc::Sender<Result<WireEvent, String>>,
    event: Result<WireEvent, String>,
) -> Result<(), String> {
    events.try_send(event).map_err(|error| match error {
        mpsc::error::TrySendError::Full(_) => {
            format!("legacy SSE event buffer exceeded {LEGACY_SSE_EVENT_BUFFER} entries")
        }
        mpsc::error::TrySendError::Closed(_) => "legacy SSE event receiver is closed".to_string(),
    })
}

fn fail_legacy_pending(
    pending: &std::sync::Mutex<BTreeMap<i64, oneshot::Sender<Result<Value, String>>>>,
    reason: &str,
) {
    if let Ok(mut pending) = pending.lock() {
        let requests = std::mem::take(&mut *pending);
        drop(pending);
        for (_, sender) in requests {
            let _ = sender.send(Err(reason.to_string()));
        }
    }
}

fn classify_messages(
    method: &str,
    expected_id: Option<i64>,
    messages: Vec<(Option<String>, Value)>,
) -> Result<RpcExchange, McpClientError> {
    let mut response = None;
    let mut events = Vec::new();
    for (cursor, value) in messages {
        if expected_id.is_some()
            && value.get("id") == expected_id.map(|id| json!(id)).as_ref()
            && value.get("method").is_none()
        {
            if response.replace(value).is_some() {
                return Err(McpClientError::Protocol {
                    method: method.to_string(),
                    reason: "transport returned duplicate JSON-RPC responses".to_string(),
                });
            }
        } else {
            validate_event_message(&value, method)?;
            events.push(WireEvent {
                transport_cursor: cursor,
                body: value,
            });
        }
    }
    if expected_id.is_some() && response.is_none() {
        return Err(McpClientError::Protocol {
            method: method.to_string(),
            reason: "transport carried no matching JSON-RPC response".to_string(),
        });
    }
    Ok(RpcExchange { response, events })
}

fn validate_event_message(value: &Value, method: &str) -> Result<(), McpClientError> {
    let object = value.as_object().ok_or_else(|| McpClientError::Protocol {
        method: method.to_string(),
        reason: "JSON-RPC event is not an object".to_string(),
    })?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpClientError::Protocol {
            method: method.to_string(),
            reason: "JSON-RPC event version is not '2.0'".to_string(),
        });
    }
    if object.get("method").and_then(Value::as_str).is_none() {
        return Err(McpClientError::Protocol {
            method: method.to_string(),
            reason: "uncorrelated JSON-RPC response arrived as an event".to_string(),
        });
    }
    Ok(())
}

fn extract_result(
    method: &str,
    expected_id: i64,
    response: Option<Value>,
) -> Result<Value, McpClientError> {
    let response = response.ok_or_else(|| McpClientError::Protocol {
        method: method.to_string(),
        reason: "response is absent".to_string(),
    })?;
    let object = response
        .as_object()
        .ok_or_else(|| McpClientError::Protocol {
            method: method.to_string(),
            reason: "response is not an object".to_string(),
        })?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpClientError::Protocol {
            method: method.to_string(),
            reason: "response jsonrpc is not '2.0'".to_string(),
        });
    }
    if object.get("id") != Some(&json!(expected_id)) {
        return Err(McpClientError::Protocol {
            method: method.to_string(),
            reason: format!(
                "response id {:?} does not match request id {expected_id}",
                object.get("id")
            ),
        });
    }
    match (object.get("result"), object.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => {
            let error = error.as_object().ok_or_else(|| McpClientError::Protocol {
                method: method.to_string(),
                reason: "JSON-RPC error is not an object".to_string(),
            })?;
            let code = error.get("code").and_then(Value::as_i64).ok_or_else(|| {
                McpClientError::Protocol {
                    method: method.to_string(),
                    reason: "JSON-RPC error code is not an integer".to_string(),
                }
            })?;
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| McpClientError::Protocol {
                    method: method.to_string(),
                    reason: "JSON-RPC error message is not a string".to_string(),
                })?;
            Err(McpClientError::ServerError {
                method: method.to_string(),
                code,
                message: sanitize_error(message),
                data: error.get("data").cloned(),
            })
        }
        _ => Err(McpClientError::Protocol {
            method: method.to_string(),
            reason: "JSON-RPC response must carry exactly one of result or error".to_string(),
        }),
    }
}

#[derive(Debug, Default)]
struct SseParser {
    buffer: Vec<u8>,
    event: Option<String>,
    id: Option<String>,
    data: String,
    max_bytes: usize,
}

impl SseParser {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            ..Self::default()
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, McpClientError> {
        let buffered = self.buffer.len().checked_add(chunk.len()).ok_or_else(|| {
            McpClientError::ResponseTooLarge {
                method: "sse".to_string(),
                limit: self.max_bytes as u64,
            }
        })?;
        if buffered > self.max_bytes {
            return Err(McpClientError::ResponseTooLarge {
                method: "sse".to_string(),
                limit: self.max_bytes as u64,
            });
        }
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=position).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8(line).map_err(|error| McpClientError::Protocol {
                method: "sse".to_string(),
                reason: format!("SSE line is not UTF-8: {error}"),
            })?;
            if line.is_empty() {
                if !self.data.is_empty() {
                    frames.push(SseFrame {
                        event: self.event.take(),
                        id: self.id.take(),
                        data: std::mem::take(&mut self.data),
                    });
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line
                .split_once(':')
                .map(|(field, value)| (field, value.strip_prefix(' ').unwrap_or(value)))
                .unwrap_or((&line, ""));
            match field {
                "event" => self.event = Some(value.to_string()),
                "id" if !value.contains('\0') => self.id = Some(value.to_string()),
                "data" => {
                    if self
                        .data
                        .len()
                        .saturating_add(value.len())
                        .saturating_add(1)
                        > self.max_bytes
                    {
                        return Err(McpClientError::ResponseTooLarge {
                            method: "sse".to_string(),
                            limit: self.max_bytes as u64,
                        });
                    }
                    if !self.data.is_empty() {
                        self.data.push('\n');
                    }
                    self.data.push_str(value);
                }
                _ => {}
            }
        }
        Ok(frames)
    }

    fn finish(mut self) -> Result<Vec<SseFrame>, McpClientError> {
        let mut frames = self.push(b"\n\n")?;
        if !self.buffer.is_empty() {
            return Err(McpClientError::Protocol {
                method: "sse".to_string(),
                reason: "SSE stream ended with an incomplete frame".to_string(),
            });
        }
        Ok(std::mem::take(&mut frames))
    }

    fn parse_complete(bytes: &[u8], max_bytes: usize) -> Result<Vec<SseFrame>, McpClientError> {
        let mut parser = Self::new(max_bytes);
        let mut frames = parser.push(bytes)?;
        frames.extend(parser.finish()?);
        Ok(frames)
    }
}

#[derive(Debug)]
struct SseFrame {
    event: Option<String>,
    id: Option<String>,
    data: String,
}

impl SseFrame {
    fn into_json(self, method: &str) -> Result<(Option<String>, Value), McpClientError> {
        let value = serde_json::from_str(&self.data).map_err(|error| McpClientError::Protocol {
            method: method.to_string(),
            reason: format!("SSE data frame is not JSON: {error}"),
        })?;
        Ok((self.id, value))
    }
}

async fn read_sse_stream(
    mut response: reqwest::Response,
    sender: mpsc::Sender<Result<SseFrame, String>>,
    max_bytes: u64,
) {
    let mut parser = SseParser::new(max_bytes as usize);
    loop {
        let frames = match response.chunk().await {
            Ok(Some(chunk)) => parser.push(&chunk).map_err(|error| error.to_string()),
            Ok(None) => break,
            Err(error) => Err(error.to_string()),
        };
        match frames {
            Ok(frames) => {
                for frame in frames {
                    if sender.send(Ok(frame)).await.is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                return;
            }
        }
    }
    let frames = parser.finish().map_err(|error| error.to_string());
    match frames {
        Ok(frames) => {
            for frame in frames {
                if sender.send(Ok(frame)).await.is_err() {
                    return;
                }
            }
            let _ = sender
                .send(Err("legacy SSE stream closed".to_string()))
                .await;
        }
        Err(error) => {
            let _ = sender.send(Err(error)).await;
        }
    }
}

async fn collect_sse_events(
    mut response: reqwest::Response,
    wait: Duration,
    max_bytes: u64,
    max_events: usize,
) -> Result<Vec<WireEvent>, McpClientError> {
    let mut parser = SseParser::new(max_bytes as usize);
    let deadline = tokio::time::Instant::now() + wait;
    let mut events = Vec::new();
    while events.len() < max_events {
        let chunk = match tokio::time::timeout_at(deadline, response.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Err(source)) => {
                return Err(McpClientError::Http {
                    url: "event-stream".to_string(),
                    source,
                });
            }
            Ok(Ok(None)) | Err(_) => break,
        };
        for frame in parser.push(&chunk)? {
            if frame.event.as_deref() == Some("endpoint") {
                continue;
            }
            let (cursor, body) = frame.into_json("events/get")?;
            validate_event_message(&body, "events/get")?;
            events.push(WireEvent {
                transport_cursor: cursor,
                body,
            });
            if events.len() == max_events {
                break;
            }
        }
    }
    Ok(events)
}

fn resolve_legacy_endpoint(source: &str, endpoint: &str) -> Result<String, McpClientError> {
    let source = reqwest::Url::parse(source)
        .map_err(|error| McpClientError::Config(format!("legacy SSE URL is invalid: {error}")))?;
    let endpoint = source
        .join(endpoint.trim())
        .map_err(|error| McpClientError::Protocol {
            method: "sse/endpoint".to_string(),
            reason: format!("legacy SSE endpoint is invalid: {error}"),
        })?;
    if source.scheme() != endpoint.scheme()
        || source.host_str() != endpoint.host_str()
        || source.port_or_known_default() != endpoint.port_or_known_default()
    {
        return Err(McpClientError::Protocol {
            method: "sse/endpoint".to_string(),
            reason: "legacy SSE endpoint crossed origin".to_string(),
        });
    }
    Ok(endpoint.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventRow {
    pub cursor: u64,
    #[serde(default)]
    pub transport_cursor: Option<String>,
    pub body: Value,
}

struct EventJournal {
    path: PathBuf,
    segments: Vec<EventJournalSegment>,
    seen_transport_cursors: BTreeSet<String>,
    segment_max_bytes: u64,
    retained_segments: usize,
    last_cursor: u64,
}

struct EventJournalSegment {
    path: PathBuf,
    start_cursor: u64,
    end_cursor: u64,
    byte_len: u64,
    row_offsets: Vec<u64>,
    transport_cursors: BTreeSet<String>,
    last_transport_cursor: Option<String>,
}

impl EventJournalSegment {
    fn empty(path: PathBuf, start_cursor: u64) -> Self {
        Self {
            path,
            start_cursor,
            end_cursor: start_cursor.saturating_sub(1),
            byte_len: 0,
            row_offsets: Vec::new(),
            transport_cursors: BTreeSet::new(),
            last_transport_cursor: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.row_offsets.is_empty()
    }
}

impl EventJournal {
    fn open(
        path: PathBuf,
        segment_max_bytes: u64,
        retained_segments: usize,
    ) -> Result<Self, McpClientError> {
        if segment_max_bytes == 0 {
            return Err(McpClientError::Journal(
                "event journal segment byte ceiling is zero".to_string(),
            ));
        }
        if retained_segments < 2 {
            return Err(McpClientError::Journal(
                "event journal must retain at least two segments".to_string(),
            ));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| McpClientError::Journal(error.to_string()))?;
        }
        let mut segments = Vec::new();
        let mut seen_transport_cursors = BTreeSet::new();
        let mut archived = archived_event_segments(&path)?;
        archived.sort_by_key(|(start, end, _)| (*start, *end));
        let mut expected_next = None;
        for (declared_start, declared_end, segment_path) in archived {
            let segment = scan_event_segment(segment_path, segment_max_bytes)?;
            if segment.is_empty() {
                return Err(McpClientError::Journal(format!(
                    "archived event segment '{}' is empty",
                    segment.path.display()
                )));
            }
            if segment.start_cursor != declared_start || segment.end_cursor != declared_end {
                return Err(McpClientError::Journal(format!(
                    "archived event segment '{}' declares {declared_start}..={declared_end} but contains {}..={}",
                    segment.path.display(),
                    segment.start_cursor,
                    segment.end_cursor
                )));
            }
            admit_event_segment(&segment, &mut expected_next, &mut seen_transport_cursors)?;
            segments.push(segment);
        }
        let active = if path.exists() {
            let segment = scan_event_segment(path.clone(), segment_max_bytes)?;
            if segment.is_empty() {
                EventJournalSegment::empty(path.clone(), expected_next.unwrap_or(1))
            } else {
                admit_event_segment(&segment, &mut expected_next, &mut seen_transport_cursors)?;
                segment
            }
        } else {
            create_event_segment(&path)?;
            EventJournalSegment::empty(path.clone(), expected_next.unwrap_or(1))
        };
        segments.push(active);
        let last_cursor = segments
            .iter()
            .rev()
            .find(|segment| !segment.is_empty())
            .map(|segment| segment.end_cursor)
            .unwrap_or(0);
        let mut journal = Self {
            path,
            segments,
            seen_transport_cursors,
            segment_max_bytes,
            retained_segments,
            last_cursor,
        };
        journal.prune_retained_segments()?;
        Ok(journal)
    }

    fn append(&mut self, event: WireEvent) -> Result<(), McpClientError> {
        if event
            .transport_cursor
            .as_ref()
            .is_some_and(|cursor| self.seen_transport_cursors.contains(cursor))
        {
            return Ok(());
        }
        let cursor = self
            .last_cursor
            .checked_add(1)
            .ok_or_else(|| McpClientError::Journal("event cursor overflowed".to_string()))?;
        let row = EventRow {
            cursor,
            transport_cursor: event.transport_cursor,
            body: event.body,
        };
        let mut bytes =
            serde_json::to_vec(&row).map_err(|error| McpClientError::Journal(error.to_string()))?;
        bytes.push(b'\n');
        let row_bytes = u64::try_from(bytes.len())
            .map_err(|_| McpClientError::Journal("event row length exceeds u64".to_string()))?;
        if row_bytes > self.segment_max_bytes {
            return Err(McpClientError::Journal(format!(
                "event row requires {row_bytes} bytes, exceeding segment ceiling {}",
                self.segment_max_bytes
            )));
        }
        let active = self.segments.last().ok_or_else(|| {
            McpClientError::Journal("event journal has no active segment".to_string())
        })?;
        let next_bytes = active.byte_len.checked_add(row_bytes).ok_or_else(|| {
            McpClientError::Journal("event segment byte length overflowed".to_string())
        })?;
        if !active.is_empty() && next_bytes > self.segment_max_bytes {
            self.rotate_active_segment()?;
            self.prune_retained_segments()?;
        }
        let active = self.segments.last_mut().ok_or_else(|| {
            McpClientError::Journal("event journal has no active segment".to_string())
        })?;
        let was_empty = active.is_empty();
        let row_offset = active.byte_len;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|error| McpClientError::Journal(error.to_string()))?;
        file.write_all(&bytes)
            .map_err(|error| McpClientError::Journal(error.to_string()))?;
        file.sync_data()
            .map_err(|error| McpClientError::Journal(error.to_string()))?;
        if let Some(cursor) = row.transport_cursor.as_ref() {
            self.seen_transport_cursors.insert(cursor.clone());
            active.transport_cursors.insert(cursor.clone());
            active.last_transport_cursor = Some(cursor.clone());
        }
        active.row_offsets.push(row_offset);
        active.byte_len = active.byte_len.checked_add(row_bytes).ok_or_else(|| {
            McpClientError::Journal("event segment byte length overflowed".to_string())
        })?;
        if was_empty {
            active.start_cursor = row.cursor;
        }
        active.end_cursor = row.cursor;
        self.last_cursor = row.cursor;
        Ok(())
    }

    fn read_after(&self, cursor: u64, limit: usize) -> Result<Vec<EventRow>, McpClientError> {
        if cursor > self.last_cursor() {
            return Err(McpClientError::Journal(format!(
                "requested cursor {cursor} is ahead of durable cursor {}",
                self.last_cursor()
            )));
        }
        if limit == 0 || cursor == self.last_cursor() {
            return Ok(Vec::new());
        }
        let earliest = self.earliest_cursor().ok_or_else(|| {
            McpClientError::Journal("event journal cursor exists without retained rows".to_string())
        })?;
        let requested_next = cursor
            .checked_add(1)
            .ok_or_else(|| McpClientError::Journal("event cursor overflowed".to_string()))?;
        if requested_next < earliest {
            return Err(McpClientError::EventCursorGap {
                requested: cursor,
                earliest_available: earliest,
                latest: self.last_cursor,
            });
        }
        let mut rows = Vec::with_capacity(limit.min(MAX_EVENT_POLL));
        for segment in &self.segments {
            if segment.is_empty() || segment.end_cursor < requested_next {
                continue;
            }
            read_event_segment_after(
                segment,
                requested_next,
                limit,
                self.segment_max_bytes,
                &mut rows,
            )?;
            if rows.len() == limit {
                break;
            }
        }
        Ok(rows)
    }

    fn last_cursor(&self) -> u64 {
        self.last_cursor
    }

    fn earliest_cursor(&self) -> Option<u64> {
        self.segments
            .iter()
            .find(|segment| !segment.is_empty())
            .map(|segment| segment.start_cursor)
    }

    fn last_transport_cursor(&self) -> Option<String> {
        self.segments
            .iter()
            .rev()
            .find_map(|segment| segment.last_transport_cursor.clone())
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn rotate_active_segment(&mut self) -> Result<(), McpClientError> {
        let active = self.segments.last().ok_or_else(|| {
            McpClientError::Journal("event journal has no active segment".to_string())
        })?;
        if active.is_empty() {
            return Ok(());
        }
        let archive =
            archived_event_segment_path(&self.path, active.start_cursor, active.end_cursor)?;
        if archive.exists() {
            return Err(McpClientError::Journal(format!(
                "event segment archive '{}' already exists",
                archive.display()
            )));
        }
        std::fs::rename(&self.path, &archive)
            .map_err(|error| McpClientError::Journal(error.to_string()))?;
        if let Err(error) = create_event_segment(&self.path) {
            let _ = std::fs::rename(&archive, &self.path);
            return Err(error);
        }
        self.segments
            .last_mut()
            .ok_or_else(|| {
                McpClientError::Journal("event journal has no active segment".to_string())
            })?
            .path = archive;
        self.segments.push(EventJournalSegment::empty(
            self.path.clone(),
            self.last_cursor
                .checked_add(1)
                .ok_or_else(|| McpClientError::Journal("event cursor overflowed".to_string()))?,
        ));
        sync_event_parent(&self.path)?;
        Ok(())
    }

    fn prune_retained_segments(&mut self) -> Result<(), McpClientError> {
        while self.segments.len() > self.retained_segments {
            if self.segments[0].path == self.path {
                return Err(McpClientError::Journal(
                    "event retention attempted to remove the active segment".to_string(),
                ));
            }
            let removed_path = self.segments[0].path.clone();
            std::fs::remove_file(&removed_path)
                .map_err(|error| McpClientError::Journal(error.to_string()))?;
            let removed = self.segments.remove(0);
            for cursor in removed.transport_cursors {
                self.seen_transport_cursors.remove(&cursor);
            }
            sync_event_parent(&self.path)?;
        }
        Ok(())
    }
}

fn archived_event_segments(path: &Path) -> Result<Vec<(u64, u64, PathBuf)>, McpClientError> {
    let parent = path.parent().ok_or_else(|| {
        McpClientError::Journal(format!("event journal '{}' has no parent", path.display()))
    })?;
    let active_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            McpClientError::Journal(format!(
                "event journal '{}' has a non-UTF-8 name",
                path.display()
            ))
        })?;
    let stem = active_name.strip_suffix(".jsonl").ok_or_else(|| {
        McpClientError::Journal(format!(
            "event journal '{}' is not a .jsonl file",
            path.display()
        ))
    })?;
    let prefix = format!("{stem}.");
    let mut segments = Vec::new();
    for entry in
        std::fs::read_dir(parent).map_err(|error| McpClientError::Journal(error.to_string()))?
    {
        let entry = entry.map_err(|error| McpClientError::Journal(error.to_string()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(range) = name
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let (start, end) = range.split_once('-').ok_or_else(|| {
            McpClientError::Journal(format!("event segment '{name}' has no cursor range"))
        })?;
        let start = start.parse::<u64>().map_err(|error| {
            McpClientError::Journal(format!(
                "event segment '{name}' start is malformed: {error}"
            ))
        })?;
        let end = end.parse::<u64>().map_err(|error| {
            McpClientError::Journal(format!("event segment '{name}' end is malformed: {error}"))
        })?;
        if start == 0 || end < start {
            return Err(McpClientError::Journal(format!(
                "event segment '{name}' has invalid cursor range {start}..={end}"
            )));
        }
        segments.push((start, end, entry.path()));
    }
    Ok(segments)
}

fn archived_event_segment_path(
    path: &Path,
    start: u64,
    end: u64,
) -> Result<PathBuf, McpClientError> {
    let parent = path.parent().ok_or_else(|| {
        McpClientError::Journal(format!("event journal '{}' has no parent", path.display()))
    })?;
    let active_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            McpClientError::Journal(format!(
                "event journal '{}' has a non-UTF-8 name",
                path.display()
            ))
        })?;
    let stem = active_name.strip_suffix(".jsonl").ok_or_else(|| {
        McpClientError::Journal(format!(
            "event journal '{}' is not a .jsonl file",
            path.display()
        ))
    })?;
    Ok(parent.join(format!("{stem}.{start:020}-{end:020}.jsonl")))
}

fn scan_event_segment(
    path: PathBuf,
    segment_max_bytes: u64,
) -> Result<EventJournalSegment, McpClientError> {
    let file =
        std::fs::File::open(&path).map_err(|error| McpClientError::Journal(error.to_string()))?;
    let byte_len = file
        .metadata()
        .map_err(|error| McpClientError::Journal(error.to_string()))?
        .len();
    if byte_len > segment_max_bytes {
        return Err(McpClientError::Journal(format!(
            "event segment '{}' has {byte_len} bytes, exceeding segment ceiling {segment_max_bytes}",
            path.display()
        )));
    }
    let mut reader = BufReader::new(file);
    let mut row_offsets = Vec::new();
    let mut transport_cursors = BTreeSet::new();
    let mut last_transport_cursor = None;
    let mut position = 0u64;
    let mut first_cursor = None;
    let mut last_cursor: Option<u64> = None;
    loop {
        let Some(mut line) = read_bounded_event_row(&mut reader, segment_max_bytes, &path)? else {
            break;
        };
        let read = line.len();
        if line.last() != Some(&b'\n') {
            return Err(McpClientError::Journal(format!(
                "event segment '{}' ends with a partial row",
                path.display()
            )));
        }
        line.pop();
        if line.is_empty() {
            return Err(McpClientError::Journal(format!(
                "event segment '{}' contains an empty row",
                path.display()
            )));
        }
        let row: EventRow = serde_json::from_slice(&line).map_err(|error| {
            McpClientError::Journal(format!(
                "event segment '{}' row {} is malformed: {error}",
                path.display(),
                row_offsets.len() + 1
            ))
        })?;
        if row.cursor == 0 {
            return Err(McpClientError::Journal(format!(
                "event segment '{}' contains zero cursor",
                path.display()
            )));
        }
        if let Some(previous) = last_cursor {
            let expected = previous
                .checked_add(1)
                .ok_or_else(|| McpClientError::Journal("event cursor overflowed".to_string()))?;
            if row.cursor != expected {
                return Err(McpClientError::Journal(format!(
                    "event segment '{}' cursor {} is non-contiguous; expected {expected}",
                    path.display(),
                    row.cursor
                )));
            }
        }
        if let Some(cursor) = row.transport_cursor {
            if !transport_cursors.insert(cursor.clone()) {
                return Err(McpClientError::Journal(format!(
                    "event segment '{}' repeats transport cursor '{cursor}'",
                    path.display()
                )));
            }
            last_transport_cursor = Some(cursor);
        }
        row_offsets.push(position);
        position = position
            .checked_add(u64::try_from(read).map_err(|_| {
                McpClientError::Journal("event segment row length exceeds u64".to_string())
            })?)
            .ok_or_else(|| {
                McpClientError::Journal("event segment length overflowed".to_string())
            })?;
        first_cursor.get_or_insert(row.cursor);
        last_cursor = Some(row.cursor);
    }
    if position != byte_len {
        return Err(McpClientError::Journal(format!(
            "event segment '{}' changed while it was indexed",
            path.display()
        )));
    }
    let start_cursor = first_cursor.unwrap_or(1);
    Ok(EventJournalSegment {
        path,
        start_cursor,
        end_cursor: last_cursor.unwrap_or(start_cursor.saturating_sub(1)),
        byte_len,
        row_offsets,
        transport_cursors,
        last_transport_cursor,
    })
}

fn admit_event_segment(
    segment: &EventJournalSegment,
    expected_next: &mut Option<u64>,
    seen_transport_cursors: &mut BTreeSet<String>,
) -> Result<(), McpClientError> {
    if let Some(expected) = expected_next {
        if segment.start_cursor != *expected {
            return Err(McpClientError::Journal(format!(
                "event segment '{}' starts at {}, expected {}",
                segment.path.display(),
                segment.start_cursor,
                expected
            )));
        }
    }
    for cursor in &segment.transport_cursors {
        if !seen_transport_cursors.insert(cursor.clone()) {
            return Err(McpClientError::Journal(format!(
                "event segment '{}' repeats retained transport cursor '{cursor}'",
                segment.path.display()
            )));
        }
    }
    *expected_next = Some(
        segment
            .end_cursor
            .checked_add(1)
            .ok_or_else(|| McpClientError::Journal("event cursor overflowed".to_string()))?,
    );
    Ok(())
}

fn read_event_segment_after(
    segment: &EventJournalSegment,
    cursor: u64,
    limit: usize,
    row_max_bytes: u64,
    rows: &mut Vec<EventRow>,
) -> Result<(), McpClientError> {
    let metadata = std::fs::metadata(&segment.path)
        .map_err(|error| McpClientError::Journal(error.to_string()))?;
    if metadata.len() != segment.byte_len {
        return Err(McpClientError::Journal(format!(
            "event segment '{}' changed after it was indexed",
            segment.path.display()
        )));
    }
    let first = cursor.max(segment.start_cursor);
    let index = usize::try_from(first - segment.start_cursor)
        .map_err(|_| McpClientError::Journal("event segment index exceeds usize".to_string()))?;
    let Some(offset) = segment.row_offsets.get(index).copied() else {
        return Ok(());
    };
    let mut file = std::fs::File::open(&segment.path)
        .map_err(|error| McpClientError::Journal(error.to_string()))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| McpClientError::Journal(error.to_string()))?;
    let mut reader = BufReader::new(file);
    let mut expected = first;
    while rows.len() < limit && expected <= segment.end_cursor {
        let Some(mut line) = read_bounded_event_row(&mut reader, row_max_bytes, &segment.path)?
        else {
            return Err(McpClientError::Journal(format!(
                "event segment '{}' changed while it was read",
                segment.path.display()
            )));
        };
        if line.last() != Some(&b'\n') {
            return Err(McpClientError::Journal(format!(
                "event segment '{}' changed while it was read",
                segment.path.display()
            )));
        }
        line.pop();
        let row: EventRow = serde_json::from_slice(&line).map_err(|error| {
            McpClientError::Journal(format!(
                "event segment '{}' contains malformed indexed row: {error}",
                segment.path.display()
            ))
        })?;
        if row.cursor != expected {
            return Err(McpClientError::Journal(format!(
                "event segment '{}' indexed cursor changed from {expected} to {}",
                segment.path.display(),
                row.cursor
            )));
        }
        rows.push(row);
        expected = expected
            .checked_add(1)
            .ok_or_else(|| McpClientError::Journal("event cursor overflowed".to_string()))?;
    }
    Ok(())
}

fn read_bounded_event_row<R: BufRead>(
    reader: &mut R,
    max_bytes: u64,
    path: &Path,
) -> Result<Option<Vec<u8>>, McpClientError> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| McpClientError::Journal(error.to_string()))?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let current = u64::try_from(line.len())
            .map_err(|_| McpClientError::Journal("event row length exceeds u64".to_string()))?;
        let consumed_u64 = u64::try_from(consumed)
            .map_err(|_| McpClientError::Journal("event row length exceeds u64".to_string()))?;
        let next = current
            .checked_add(consumed_u64)
            .ok_or_else(|| McpClientError::Journal("event row length overflowed".to_string()))?;
        if next > max_bytes {
            return Err(McpClientError::Journal(format!(
                "event segment '{}' row exceeds segment ceiling {max_bytes}",
                path.display()
            )));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

fn create_event_segment(path: &Path) -> Result<(), McpClientError> {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_data())
        .map_err(|error| McpClientError::Journal(error.to_string()))
}

fn sync_event_parent(path: &Path) -> Result<(), McpClientError> {
    let parent = path.parent().ok_or_else(|| {
        McpClientError::Journal(format!("event journal '{}' has no parent", path.display()))
    })?;
    std::fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|error| McpClientError::Journal(error.to_string()))
}

fn build_http_client(
    transport: &InterfaceTransportConfig,
    timeout: Duration,
) -> Result<reqwest::Client, McpClientError> {
    resolve_base_url(transport)?;
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(timeout)
        .timeout(Duration::from_millis(MAX_REQUEST_TIMEOUT_MS))
        .redirect(same_origin_redirect_policy(5))
        .build()
        .map_err(|error| McpClientError::Config(format!("HTTP client build failed: {error}")))
}

fn request_timeout(transport: &InterfaceTransportConfig) -> Result<Duration, McpClientError> {
    let timeout_ms = transport
        .request_timeout_ms
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > MAX_REQUEST_TIMEOUT_MS {
        return Err(McpClientError::Config(format!(
            "request_timeout_ms must be in 1..={MAX_REQUEST_TIMEOUT_MS}, got {timeout_ms}"
        )));
    }
    Ok(Duration::from_millis(timeout_ms))
}

fn max_response_bytes(transport: &InterfaceTransportConfig) -> Result<u64, McpClientError> {
    let value = transport
        .max_response_bytes
        .unwrap_or(DEFAULT_MAX_RESPONSE_BYTES);
    if value == 0 || value > ABSOLUTE_MAX_RESPONSE_BYTES {
        return Err(McpClientError::Config(format!(
            "max_response_bytes must be in 1..={ABSOLUTE_MAX_RESPONSE_BYTES}"
        )));
    }
    Ok(value)
}

pub(crate) fn resolve_base_url(
    transport: &InterfaceTransportConfig,
) -> Result<String, McpClientError> {
    let direct = transport.base_url.trim();
    let env_name = transport
        .base_url_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let resolved = match (direct.is_empty(), env_name) {
        (false, None) => Ok(direct.to_string()),
        (true, Some(name)) => std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| McpClientError::MissingEnvironmentValue(name.to_string())),
        (false, Some(name)) => Err(McpClientError::Config(format!(
            "base_url and base_url_env '{name}' are both set"
        ))),
        (true, None) => Err(McpClientError::Config(
            "transport requires exactly one of base_url or base_url_env".to_string(),
        )),
    }?;
    validate_remote_url(&resolved)?;
    Ok(resolved)
}

fn validate_remote_url(raw: &str) -> Result<(), McpClientError> {
    let url = reqwest::Url::parse(raw)
        .map_err(|error| McpClientError::Config(format!("base URL is invalid: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(McpClientError::Config(
            "base URL must be an absolute http(s) URL with a host".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(McpClientError::Config(
            "base URL must not embed credentials".to_string(),
        ));
    }
    if url.fragment().is_some() {
        return Err(McpClientError::Config(
            "base URL must not contain a fragment".to_string(),
        ));
    }
    if url.scheme() == "http" && !url_host_is_loopback(&url) {
        return Err(McpClientError::Config(
            "plaintext MCP transport is allowed only for loopback hosts".to_string(),
        ));
    }
    Ok(())
}

fn url_host_is_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

pub(crate) fn same_origin_redirect_policy(max_redirects: usize) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= max_redirects {
            return attempt.error("redirect limit exceeded");
        }
        let target = attempt.url();
        let Some(origin) = attempt.previous().first() else {
            return attempt.error("redirect has no origin request");
        };
        let same_origin = target.scheme() == origin.scheme()
            && target.host_str() == origin.host_str()
            && target.port_or_known_default() == origin.port_or_known_default();
        if !same_origin {
            return attempt.error("redirect crossed the configured MCP origin");
        }
        if target.scheme() == "https" || (target.scheme() == "http" && url_host_is_loopback(target))
        {
            attempt.follow()
        } else {
            attempt.error("redirect attempted an insecure MCP transport")
        }
    })
}

fn resolve_auth_header(
    transport: &InterfaceTransportConfig,
) -> Result<Option<(HeaderName, HeaderValue)>, McpClientError> {
    let Some(env_key) = transport.auth_env_key.as_deref() else {
        return Ok(None);
    };
    let env_key = env_key.trim();
    if env_key.is_empty() {
        return Err(McpClientError::Config(
            "auth_env_key must not be empty".to_string(),
        ));
    }
    let secret = std::env::var(env_key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| McpClientError::MissingEnvironmentValue(env_key.to_string()))?;
    let name = transport
        .auth_header
        .as_deref()
        .unwrap_or(DEFAULT_AUTH_HEADER)
        .parse::<HeaderName>()
        .map_err(|error| McpClientError::Config(format!("auth header name is invalid: {error}")))?;
    let prefix = transport
        .auth_value_prefix
        .as_deref()
        .unwrap_or(DEFAULT_AUTH_VALUE_PREFIX);
    let value = HeaderValue::from_str(&format!("{prefix}{secret}")).map_err(|error| {
        McpClientError::Config(format!("auth header value is invalid: {error}"))
    })?;
    Ok(Some((name, value)))
}

fn resolve_extra_headers(
    transport: &InterfaceTransportConfig,
    auth_owned: &[HeaderName],
) -> Result<HeaderMap, McpClientError> {
    let mut headers = HeaderMap::new();
    for (raw_name, raw_value) in &transport.headers {
        let name = raw_name.parse::<HeaderName>().map_err(|error| {
            McpClientError::Config(format!("header name '{raw_name}' is invalid: {error}"))
        })?;
        if matches!(
            name.as_str(),
            "content-type" | "accept" | PROTOCOL_HEADER | SESSION_HEADER | LAST_EVENT_ID_HEADER
        ) || auth_owned.contains(&name)
        {
            return Err(McpClientError::Config(format!(
                "header '{}' is owned by the transport contract",
                name.as_str()
            )));
        }
        let value = HeaderValue::from_str(raw_value).map_err(|error| {
            McpClientError::Config(format!(
                "header '{}' has invalid value: {error}",
                name.as_str()
            ))
        })?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn response_content_type(response: &reqwest::Response) -> String {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

async fn read_bounded(
    mut response: reqwest::Response,
    method: &str,
    limit: u64,
) -> Result<Vec<u8>, McpClientError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(McpClientError::ResponseTooLarge {
            method: method.to_string(),
            limit,
        });
    }
    let url = response.url().to_string();
    let mut raw = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|source| McpClientError::Http {
            url: url.clone(),
            source,
        })?
    {
        let next = raw.len().saturating_add(chunk.len()) as u64;
        if next > limit {
            return Err(McpClientError::ResponseTooLarge {
                method: method.to_string(),
                limit,
            });
        }
        raw.extend_from_slice(&chunk);
    }
    Ok(raw)
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

fn sanitize_value(value: &Value) -> Value {
    match value {
        Value::String(value) => Value::String(sanitize_error(value)),
        Value::Array(values) => Value::Array(values.iter().map(sanitize_value).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    let normalized = key.to_ascii_lowercase();
                    let value = if normalized.contains("token")
                        || normalized.contains("secret")
                        || normalized.contains("password")
                        || normalized.ends_with("key")
                    {
                        Value::String("[REDACTED]".to_string())
                    } else {
                        sanitize_value(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn public_endpoint(raw: &str) -> String {
    match reqwest::Url::parse(raw) {
        Ok(mut url) => {
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => "invalid-endpoint".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn spawn_concurrent_streamable_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        let read = stream.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..read]);
                        if let Some(index) =
                            bytes.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            break index + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&bytes[..header_end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    while bytes.len() - header_end < length {
                        let read = stream.read(&mut chunk).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..read]);
                    }
                    let request: Value =
                        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                    let id = request.get("id").cloned();
                    if id.is_none() {
                        stream
                            .write_all(b"HTTP/1.1 202 Accepted\r\nMcp-Session-Id: test-session\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await
                            .unwrap();
                        return;
                    }
                    let method = request.get("method").and_then(Value::as_str).unwrap();
                    let result = if method == "initialize" {
                        json!({
                            "protocolVersion": "2025-06-18",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "concurrent-test", "version": "1" }
                        })
                    } else {
                        let delay = request
                            .pointer("/params/arguments/delay_ms")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        if delay > 0 {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                        json!({
                            "content": [{ "type": "text", "text": request.pointer("/params/arguments/text").and_then(Value::as_str).unwrap_or("") }],
                            "isError": false
                        })
                    };
                    let body = serde_json::to_vec(
                        &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    )
                    .unwrap();
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: test-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(head.as_bytes()).await.unwrap();
                    stream.write_all(&body).await.unwrap();
                });
            }
        });
        format!("http://{address}/mcp")
    }

    fn test_config(base_url: String) -> McpInterfaceConfig {
        McpInterfaceConfig {
            transport: serde_json::from_value(json!({
                "base_url": base_url,
                "wire": MCP_STREAMABLE_HTTP_WIRE,
                "request_timeout_ms": 5000,
                "max_response_bytes": 1048576
            }))
            .unwrap(),
            protocol_versions: vec!["2025-06-18".to_string()],
            client_name: "concurrency-test".to_string(),
            client_version: "1".to_string(),
            max_catalog_pages: 8,
            max_catalog_items: 64,
            max_catalog_bytes: 65536,
            event_journal_segment_bytes: 2 * 1024 * 1024,
            event_journal_retained_segments: 8,
            reconnect_backoff: Duration::from_millis(10),
            oauth: None,
            account_auth: None,
            tool_filter: None,
        }
    }

    #[tokio::test]
    async fn streamable_requests_are_not_serialized_by_client_state() {
        let base_url = spawn_concurrent_streamable_server().await;
        let root = tempdir().unwrap();
        let client = Arc::new(
            McpClient::connect(&test_config(base_url), root.path())
                .await
                .unwrap(),
        );
        let slow_client = Arc::clone(&client);
        let slow = async move {
            slow_client
                .call_tool(
                    "echo",
                    json!({ "text": "slow", "delay_ms": 400 }),
                    None,
                    None,
                )
                .await
                .unwrap()
        };
        let fast_client = Arc::clone(&client);
        let fast = async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            let started = std::time::Instant::now();
            let result = fast_client
                .call_tool("echo", json!({ "text": "fast" }), None, None)
                .await
                .unwrap();
            (result, started.elapsed())
        };
        let (slow, (fast, elapsed)) = tokio::join!(slow, fast);
        assert_eq!(slow["content"][0]["text"], "slow");
        assert_eq!(fast["content"][0]["text"], "fast");
        assert!(
            elapsed < Duration::from_millis(250),
            "fast call took {elapsed:?}"
        );
    }

    #[test]
    fn plaintext_transport_is_loopback_only() {
        assert!(validate_remote_url("http://localhost:1234/mcp").is_ok());
        assert!(validate_remote_url("http://127.99.1.2:1234/mcp").is_ok());
        assert!(validate_remote_url("http://[::1]:1234/mcp").is_ok());
        assert!(validate_remote_url("http://192.0.2.10/mcp").is_err());
        assert!(validate_remote_url("http://mcp.example/mcp").is_err());
        assert!(validate_remote_url("https://mcp.example/mcp").is_ok());
    }

    #[tokio::test]
    async fn redirect_cannot_cross_the_configured_origin() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(b"HTTP/1.1 302 Found\r\nLocation: https://attacker.example/mcp\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let client = reqwest::Client::builder()
            .redirect(same_origin_redirect_policy(5))
            .build()
            .unwrap();
        let error = client
            .get(format!("http://{address}/mcp"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_redirect());
    }

    #[test]
    fn sse_selects_the_matching_json_rpc_response_and_preserves_progress() {
        let raw = concat!(
            "id: event-1\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
            "id: event-2\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}\n\n"
        );
        let frames = SseParser::parse_complete(raw.as_bytes(), 4096).unwrap();
        let messages = frames
            .into_iter()
            .map(|frame| frame.into_json("tools/call"))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let exchange = classify_messages("tools/call", Some(7), messages).unwrap();
        assert_eq!(exchange.events.len(), 1);
        assert_eq!(
            exchange.events[0].transport_cursor.as_deref(),
            Some("event-1")
        );
        assert_eq!(
            extract_result("tools/call", 7, exchange.response).unwrap()["ok"],
            true
        );
    }

    #[tokio::test]
    async fn legacy_sse_event_overflow_fails_pending_without_blocking_correlation() {
        let (frame_tx, frame_rx) = mpsc::channel(LEGACY_SSE_EVENT_BUFFER + 2);
        let pending = Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let retired = Arc::new(std::sync::Mutex::new(BTreeSet::new()));
        let (response_tx, response_rx) = oneshot::channel();
        pending.lock().unwrap().insert(7, response_tx);
        let (event_tx, _event_rx) = mpsc::channel(LEGACY_SSE_EVENT_BUFFER);
        let dispatcher = tokio::spawn(dispatch_legacy_sse(
            frame_rx,
            Arc::clone(&pending),
            retired,
            event_tx,
        ));
        for sequence in 0..=LEGACY_SSE_EVENT_BUFFER {
            frame_tx
                .send(Ok(SseFrame {
                    event: Some("message".to_string()),
                    id: Some(sequence.to_string()),
                    data: json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": { "progress": sequence }
                    })
                    .to_string(),
                }))
                .await
                .unwrap();
        }
        let failure = tokio::time::timeout(Duration::from_secs(1), response_rx)
            .await
            .expect("pending request stayed blocked behind an unpolled event buffer")
            .unwrap()
            .unwrap_err();
        assert!(failure.contains("event buffer exceeded"), "{failure}");
        tokio::time::timeout(Duration::from_secs(1), dispatcher)
            .await
            .expect("legacy SSE dispatcher did not fail closed")
            .unwrap();
    }

    #[test]
    fn event_journal_replays_after_a_durable_cursor() {
        let root = tempdir().unwrap();
        let path = root.path().join("events.jsonl");
        let mut journal = EventJournal::open(path.clone(), 4096, 8).unwrap();
        journal
            .append(WireEvent {
                transport_cursor: Some("a".to_string()),
                body: json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}),
            })
            .unwrap();
        journal
            .append(WireEvent {
                transport_cursor: Some("b".to_string()),
                body: json!({"jsonrpc":"2.0","method":"notifications/progress"}),
            })
            .unwrap();
        drop(journal);
        let journal = EventJournal::open(path, 4096, 8).unwrap();
        let rows = journal.read_after(1, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cursor, 2);
        assert_eq!(journal.last_transport_cursor().as_deref(), Some("b"));
    }

    #[test]
    fn event_journal_rotates_with_an_explicit_retention_gap() {
        let root = tempdir().unwrap();
        let path = root.path().join("events.jsonl");
        let mut journal = EventJournal::open(path.clone(), 256, 2).unwrap();
        for index in 0..6 {
            journal
                .append(WireEvent {
                    transport_cursor: Some(format!("event-{index}")),
                    body: json!({"payload": "x".repeat(120)}),
                })
                .unwrap();
        }
        assert!(journal.earliest_cursor().unwrap() > 1);
        let error = journal.read_after(0, 10).unwrap_err();
        assert!(matches!(error, McpClientError::EventCursorGap { .. }));
        let latest = journal.last_cursor();
        drop(journal);
        let journal = EventJournal::open(path, 256, 2).unwrap();
        assert_eq!(journal.last_cursor(), latest);
        assert!(journal.earliest_cursor().unwrap() > 1);
    }

    #[test]
    fn event_journal_rejects_segment_metadata_over_the_declared_ceiling() {
        let root = tempdir().unwrap();
        let path = root.path().join("events.jsonl");
        std::fs::write(&path, vec![b'x'; 257]).unwrap();
        let error = match EventJournal::open(path, 256, 2) {
            Ok(_) => panic!("oversized segment was admitted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("exceeding segment ceiling 256"));
    }

    #[test]
    fn event_journal_row_reader_stops_at_the_declared_ceiling() {
        let path = Path::new("oversized-event-row.jsonl");
        let mut reader = BufReader::new(std::io::Cursor::new(vec![b'x'; 257]));
        let error = read_bounded_event_row(&mut reader, 256, path).unwrap_err();
        assert!(error
            .to_string()
            .contains("row exceeds segment ceiling 256"));
    }

    #[test]
    fn response_id_mismatch_fails_closed() {
        let body = json!({"jsonrpc":"2.0","id":2,"result":{}});
        assert!(matches!(
            extract_result("tools/call", 1, Some(body)),
            Err(McpClientError::Protocol { .. })
        ));
    }

    #[test]
    fn transport_errors_redact_bearer_and_assignment_credentials() {
        let sanitized = sanitize_error(
            "Authorization: Bearer secret-value token=abc password=hunter2 sk-private",
        );
        assert!(!sanitized.contains("secret-value"));
        assert!(!sanitized.contains("token=abc"));
        assert!(!sanitized.contains("hunter2"));
        assert!(!sanitized.contains("sk-private"));
    }

    #[test]
    fn credential_bearing_http_failure_keeps_only_safe_evidence() {
        let secret = "refresh-secret-echo";
        let bytes = format!("{{\"error\":\"invalid_grant\",\"echo\":\"{secret}\"}}");
        let error = McpClientError::HttpStatus {
            status: 401,
            method: "tools/call".to_string(),
            evidence: http_failure_evidence(bytes.as_bytes()),
        };
        let rendered = format!("{error:?} {}", error.structured());
        assert!(!rendered.contains(secret));
        assert!(rendered.contains("invalid_grant"));
        assert!(rendered.contains("body_blake3"));
        assert!(rendered.contains("body_bytes"));
    }

    #[test]
    fn legacy_sse_message_endpoint_is_same_origin_only() {
        assert_eq!(
            resolve_legacy_endpoint("https://mcp.example/events", "/messages?id=7").unwrap(),
            "https://mcp.example/messages?id=7"
        );
        assert!(resolve_legacy_endpoint(
            "https://mcp.example/events",
            "https://attacker.example/messages"
        )
        .is_err());
    }
}
