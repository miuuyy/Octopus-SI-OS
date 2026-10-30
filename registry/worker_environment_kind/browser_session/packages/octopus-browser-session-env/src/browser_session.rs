use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Write;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use octopus_interfaces::InterfaceArtifactFile;
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo, WorkerEnvironmentLifecycleRecorder, WorkerEnvironmentLiveStateRecorder,
};
use octopus_worker_env::{
    WorkerEnvironmentFormation, WorkerEnvironmentId, WorkerEnvironmentKindId,
    WorkerEnvironmentLifecycle,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::native_transport::{NativeHostConfig, NativeTransport, NativeConnection};

use crate::{
    browser_result, decode_room_invocation, evidence_write_error,
    BrowserOperation, DecodedRoomInvocation,
};
use octopus_tool_door_runtime::room::strict_object;

#[path = "browser_viewport.rs"]
mod viewport;

#[path = "browser_capture.rs"]
mod capture;

#[path = "browser_downloads.rs"]
mod downloads;

#[path = "browser_actions.rs"]
mod actions;

#[path = "browser_locators.rs"]
mod locators;

#[path = "browser_filechooser.rs"]
pub(crate) mod filechooser;

#[path = "browser_history.rs"]
pub(crate) mod history;
pub use filechooser::decode_filechooser_door_artifact;
pub(crate) use filechooser::decoded_response;
use actions::{BrowserRef, ObservationState, NavigationExpectation, arm_navigation, current_document, navigation_expectation, pointer_point, settle_after_action, settle_observation, type_into_ref, wait_for_document};

const CDP_WIRE: &str = "cdp";
const DEFAULT_AUTH_HEADER: &str = "Authorization";
const DEFAULT_AUTH_VALUE_PREFIX: &str = "Bearer ";

pub(crate) trait WorkerEnvironmentRecorder:
    WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder
{
}

impl<T> WorkerEnvironmentRecorder for T where
    T: WorkerEnvironmentLifecycleRecorder + WorkerEnvironmentLiveStateRecorder + ?Sized
{
}

pub struct BrowserRoom {
    environment: ToolProcessEnvironment,
    transport: BrowserTransport,
    state_dir: std::path::PathBuf,
    limits: BrowserLimits,
    page_editing_profile: PageEditingProfile,
    upload_approval: Option<filechooser::UploadApproval>,
    history_access: Option<history::HistoryAccess>,
    history_collection: Option<history::HistoryCollection>,
    source_execution: tokio::sync::Mutex<()>,
    max_upload_staged_bytes: Option<usize>,
    wait_completion: Option<crate::browser_wait_completion::WaitCompletionConfig>,
    rooms: tokio::sync::Mutex<
        BTreeMap<WorkerEnvironmentId, std::sync::Arc<tokio::sync::Mutex<RoomState>>>,
    >,
}

struct RoomState {
    generation: u64,
    browser_context_id: Option<String>,
    native_identity: Option<NativeIdentity>,
    browser: Option<CdpSession>,
    target_id: Option<String>,
    pages: BTreeMap<String, PageState>,
    execution: std::sync::Arc<tokio::sync::Mutex<()>>,
    activity: Option<Value>,
    presentation_intent: Option<BrowserPresentationIntent>,
    tabs: Vec<Value>,
    viewport: viewport::ViewportState,
}

struct PageState {
    target_id: String,
    target_url: Option<String>,
    target_title: Option<String>,
    session: Option<CdpSession>,
    refs: BTreeMap<String, BrowserRef>,
    last_capture: Option<Value>,
}

enum SessionDisposition {
    Attached,
    Reconnected,
    Created,
}

impl SessionDisposition {
    fn into_value(self) -> Value {
        match self {
            Self::Attached => json!({ "disposition": "attached" }),
            Self::Reconnected => json!({ "disposition": "reconnected" }),
            Self::Created => json!({ "disposition": "created" }),

        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserConfig {
    schema_version: u32,
    transport: BrowserTransportConfig,
    limits: BrowserLimits,
    page_editing_profile: PageEditingProfile,
    #[serde(default)]
    upload_approval: Option<filechooser::UploadApproval>,
    #[serde(default)]
    history_access: Option<history::HistoryAccess>,
    #[serde(default)]
    history_collection: Option<history::HistoryCollection>,
    #[serde(default)]
    max_upload_staged_bytes: Option<usize>,
    #[serde(default)]
    wait_completion: Option<crate::browser_wait_completion::WaitCompletionConfig>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PageEditingProfile {
    rich_text_paste: bool,
    plain_text_documents: Vec<PlainTextDocument>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PlainTextDocument {
    host: String,
    first_path_segment: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "wire", deny_unknown_fields)]
enum BrowserTransportConfig {
    #[serde(rename = "cdp")]
    Cdp(CdpConfig),
    #[serde(rename = "browser.native.v1")]
    Native(NativeHostConfig),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CdpConfig {
    discovery_url: String,
    discovery_url_env: String,
    auth_env_key: String,
    auth_required: bool,
    auth_header: String,
    auth_value_prefix: String,
    headers: BTreeMap<String, String>,
    #[serde(default)]
    allowed_websocket_hosts: Vec<String>,
    request_timeout_ms: u64,
    navigation_timeout_ms: u64,
    max_control_response_bytes: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserLimits {
    max_snapshot_nodes: usize,
    max_snapshot_chars: usize,
    max_type_chars: usize,
    max_eval_chars: usize,
    max_screenshot_bytes: usize,
    max_pdf_bytes: usize,
    max_event_count: usize,
    max_event_bytes: usize,
    max_console_entries: usize,
    max_images: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserLiveState {
    schema_version: u32,
    worker_environment_id: WorkerEnvironmentId,
    worker_environment_kind: WorkerEnvironmentKindId,
    status: String,
    generation: u64,
    browser_context_id: Option<String>,
    target_id: Option<String>,
    url: Option<String>,
    title: Option<String>,
    viewer: BrowserViewerState,
    detail: Option<String>,
    #[serde(default)]
    activity: Option<Value>,
    presentation_intent: Option<BrowserPresentationIntent>,
    #[serde(default)]
    tabs: Vec<Value>,
    #[serde(default)]
    last_capture: Option<Value>,
    #[serde(default)]
    viewport: viewport::ViewportState,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserPresentationIntent {
    id: u64,
    target_id: String,
    source: BrowserPresentationSource,
    activate: bool,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BrowserPresentationSource {
    Tool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserViewerState {
    protocol: String,
    #[serde(default)]
    address: String,
    engine_handle: Option<String>,
    host_generation: Option<String>,
    browser_context_id: Option<String>,
    target_id: Option<String>,
}

#[derive(Clone, Debug)]
struct NativeIdentity {
    engine_handle: String,
    host_generation: String,
}

enum BrowserTransport {
    Cdp(CdpTransport),
    Native(NativeTransport),
}

impl BrowserTransport {
    fn navigation_timeout(&self) -> Duration {
        match self { Self::Cdp(value) => value.navigation_timeout, Self::Native(value) => value.navigation_timeout }
    }

    async fn connect(&self, limits: &BrowserLimits, target: Option<&CdpTarget>) -> Result<CdpSession, String> {
        match self {
            Self::Cdp(transport) => {
                let websocket = match target {
                    Some(target) => target.websocket_debugger_url.clone().ok_or("CDP target has no debugger URL")?,
                    None => transport.browser_websocket().await?,
                };
                CdpSession::connect(transport, limits, target.map(|value| value.id.as_str()).unwrap_or("browser"), &websocket).await
            }
            Self::Native(transport) => {
                let connection = transport.connect(target.map(|value| value.id.as_str())).await?;
                Ok(CdpSession {
                    target_id: target.map(|value| value.id.clone()).unwrap_or_else(|| "browser".into()),
                    stream: CdpStream::Native(connection), next_id: AtomicI64::new(1),
                    usable: true,
                    request_timeout: transport.request_timeout,
                    navigation_timeout: transport.navigation_timeout,
                    observation: ObservationState::default(),
                    frames: actions::FrameState::default(),
                    events: VecDeque::new(), event_bytes: 0, dropped_events: 0, next_event_id: 1,
                    max_event_count: limits.max_event_count, max_event_bytes: limits.max_event_bytes,
                })
            }
        }
    }
}

enum CdpStream {
    WebSocket(WebSocketStream<MaybeTlsStream<TcpStream>>),
    Native(NativeConnection),
}

impl CdpStream {
    async fn send(&mut self, id: i64, session_id: Option<&str>, method: &str, params: Value) -> Result<(), String> {
        match self {
            Self::Native(connection) => connection.send(id, session_id, method, params).await,
            Self::WebSocket(stream) => {
                let mut message = json!({"id":id,"method":method,"params":params});
                if let Some(route) = session_id { message["sessionId"] = json!(route); }
                stream.send(Message::Text(message.to_string().into())).await.map_err(|error| format!("CDP send failed: {error}"))
            },
        }
    }

    async fn next(&mut self) -> Result<Value, String> {
        match self {
            Self::Native(connection) => connection.next().await,
            Self::WebSocket(stream) => loop {
                let message = stream.next().await.ok_or("CDP endpoint closed")?
                    .map_err(|error| format!("CDP receive failed: {error}"))?;
                let bytes = match message {
                    Message::Text(text) => text.as_bytes().to_vec(),
                    Message::Binary(bytes) => bytes.to_vec(),
                    Message::Close(_) => return Err("CDP endpoint closed".into()),
                    Message::Ping(payload) => { stream.send(Message::Pong(payload)).await.map_err(|error| format!("CDP pong failed: {error}"))?; continue; }
                    Message::Pong(_) | Message::Frame(_) => continue,
                };
                return serde_json::from_slice(&bytes).map_err(|_| format!("CDP response malformed; {}", safe_control_body_evidence(&bytes)));
            },
        }
    }

    async fn close(&mut self) {
        match self { Self::Native(connection) => connection.close().await, Self::WebSocket(stream) => { let _ = stream.close(None).await; } }
    }
}

#[derive(Clone)]
struct CdpTransport {
    discovery_url: reqwest::Url,
    headers: HeaderMap,
    allowed_websocket_hosts: BTreeSet<String>,
    request_timeout: Duration,
    navigation_timeout: Duration,
    max_control_response_bytes: usize,
    http: reqwest::Client,
}

#[derive(Clone, Debug, Deserialize)]
struct CdpTarget {
    id: String,
    #[serde(rename = "type")]
    target_type: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    websocket_debugger_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct CdpTargetInfo {
    #[serde(rename = "targetId")]
    target_id: String,
    #[serde(rename = "type")]
    target_type: String,
    title: String,
    url: String,
    #[serde(rename = "browserContextId")]
    browser_context_id: Option<String>,
    #[serde(rename = "openerId")]
    opener_id: Option<String>,
    lifetime: Option<String>,
    model_controlled: Option<bool>,
}

struct CdpSession {
    target_id: String,
    stream: CdpStream,
    usable: bool,
    next_id: AtomicI64,
    request_timeout: Duration,
    navigation_timeout: Duration,
    observation: ObservationState,
    frames: actions::FrameState,
    events: VecDeque<CdpEvent>,
    event_bytes: usize,
    dropped_events: u64,
    next_event_id: u64,
    max_event_count: usize,
    max_event_bytes: usize,
}

struct CdpEvent {
    id: u64,
    bytes: usize,
    value: Value,
}

impl BrowserRoom {
    pub fn load(environment: ToolProcessEnvironment) -> Result<Self, String> {
        let config = load_browser_config(&environment)?;
        let transport = match &config.transport {
            BrowserTransportConfig::Cdp(config) => BrowserTransport::Cdp(CdpTransport::form(config)?),
            BrowserTransportConfig::Native(config) => BrowserTransport::Native(NativeTransport::new(&environment, config)?),
        };
        validate_limits(&config.limits)?;
        for document in &config.page_editing_profile.plain_text_documents {
            let url = reqwest::Url::parse(&format!("https://{}", document.host)).map_err(|error| format!("Invalid page editing profile host: {error}"))?;
            if url.host_str().is_none() || url.host_str().unwrap_or_default().to_string() + &url.port().map(|port| format!(":{port}")).unwrap_or_default() != document.host
                || document.first_path_segment.is_empty() || document.first_path_segment.contains('/') {
                return Err("Page editing profile requires an exact canonical host and first path segment".into());
            }
        }
        let state_dir = environment.durable_var_dir.clone().ok_or_else(|| {
            "browser room requires host-formed durable worker-environment state".to_string()
        })?;
        Ok(Self {
            environment,
            transport,
            state_dir,
            limits: config.limits,
            page_editing_profile: config.page_editing_profile,
            upload_approval: config.upload_approval,
            history_access: config.history_access,
            history_collection: config.history_collection,
            source_execution: tokio::sync::Mutex::new(()),
            max_upload_staged_bytes: config.max_upload_staged_bytes,
            wait_completion: config.wait_completion,
            rooms: tokio::sync::Mutex::new(BTreeMap::new()),
        })
    }

    pub(crate) async fn receive(
        &self,
        invocation: ToolProcessInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<InterfaceArtifactFile, ToolProcessError> {
        let decoded = decode_room_invocation(invocation)?;
        if matches!(decoded.operation, BrowserOperation::History | BrowserOperation::HistoryResume) {
            let _execution = self.source_execution.lock().await;
            return self.receive_history(&decoded).await;
        }
        let tool = decoded.call.tool_id.clone();
        let room = self.room(&decoded.formation).await.map_err(ToolProcessError::Capability)?;
        let execution = std::sync::Arc::clone(&room.lock().await.execution);
        let _execution = execution.lock().await;
        if let BrowserTransport::Native(transport) = &self.transport {
            transport.authorize(&decoded).await.map_err(ToolProcessError::Capability)?;
        }
        {
            let mut state = room.lock().await;
            state.activity = Some(json!({
                "tool": tool,
                "source_movement_seq": decoded.source_movement_seq,
                "status": "running",
            }));
        }
        let result = Box::pin(self.dispatch(&decoded, recorder)).await;
        let mut evidence_write_errors = Vec::new();
        let mut detail = result.as_ref().err().cloned();
        let available = if result.is_ok() && decoded.operation != BrowserOperation::FilechooserResume {
            Some(true)
        } else {
            let mut state = room.lock().await;
            match self.existing_context_available(&mut state).await {
                Ok(available) => Some(available),
                Err(error) => {
                    detail = Some(format!("{}; browser availability unconfirmed: {error}", detail.as_deref().unwrap_or_default()));
                    None
                }
            }
        };
        if let Some(available) = available {
            let lifecycle = if available { WorkerEnvironmentLifecycle::Active } else { WorkerEnvironmentLifecycle::Failed };
            if let Err(error) = recorder.record(&decoded.formation, lifecycle).await {
                evidence_write_errors.push(evidence_write_error("lifecycle", error));
            }
        }
        {
            let mut state = room.lock().await;
            state.activity = Some(json!({
                "tool": tool,
                "source_movement_seq": decoded.source_movement_seq,
                "status": if result.is_ok() { "completed" } else { "failed" },
            }));
        }
        let live_state = self
            .capture_live_state(&decoded.formation, available == Some(true), detail)
            .await;
        if let Err(error) = self.persist_live_state(&decoded.formation, &live_state) {
            evidence_write_errors.push(evidence_write_error("private_live_state", error));
        }
        if let Err(error) = recorder
            .record_live_state(&decoded.formation, &live_state)
            .await
        {
            evidence_write_errors.push(evidence_write_error("host_live_state", error));
        }
        let response = browser_result(
            &tool,
            decoded.correlation_id.clone(),
            result,
            evidence_write_errors,
        );
        crate::browser_wait_completion::finish(&decoded.envelope, response, match &self.transport { BrowserTransport::Native(transport) => Some(transport), _ => None }, None).await
    }

    async fn dispatch(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        match decoded.operation {
            BrowserOperation::Navigate => self.navigate(decoded, recorder).await,
            BrowserOperation::Snapshot => self.snapshot(decoded, recorder).await,
            BrowserOperation::Click => self.click(decoded, recorder).await,
            BrowserOperation::Type => self.type_text(decoded, recorder).await,
            BrowserOperation::Key => self.key(decoded, recorder).await,
            BrowserOperation::Scroll => self.scroll(decoded, recorder).await,
            BrowserOperation::Back => self.back(decoded, recorder).await,
            BrowserOperation::Forward => self.forward(decoded, recorder).await,
            BrowserOperation::Reload => self.reload(decoded, recorder).await,
            BrowserOperation::ViewportSet | BrowserOperation::ViewportReset => self.viewport(decoded).await,
            BrowserOperation::Eval => self.evaluate(decoded, recorder).await,
            BrowserOperation::EvaluateReadonly | BrowserOperation::LocatorQuery | BrowserOperation::LocatorFill
            | BrowserOperation::LocatorPress | BrowserOperation::LocatorSelect | BrowserOperation::LocatorCheck
            | BrowserOperation::LocatorWait | BrowserOperation::PointerMove | BrowserOperation::Paste
            | BrowserOperation::MediaDownload => self.browser_family(decoded, recorder).await,
            BrowserOperation::ClipboardRead | BrowserOperation::ClipboardWrite
            | BrowserOperation::VisibilityGet | BrowserOperation::VisibilitySet => self.browser_family_control(decoded).await,
            BrowserOperation::PlaywrightDomSnapshot | BrowserOperation::DomSnapshot | BrowserOperation::DomClick | BrowserOperation::DomScroll => self.browser_family(decoded, recorder).await,
            BrowserOperation::Hover => self.hover(decoded, recorder).await,
            BrowserOperation::Select => self.select(decoded, recorder).await,
            BrowserOperation::Drag => self.drag(decoded, recorder).await,
            BrowserOperation::Wait => self.wait(decoded, recorder).await,
            BrowserOperation::WaitComplete => Err("Wait completion requires its separately declared observer executable".into()),
            BrowserOperation::History | BrowserOperation::HistoryResume => Err("History source operations cannot enter page dispatch".into()),
            BrowserOperation::Images => self.images(decoded, recorder).await,
            BrowserOperation::Console => self.console(decoded, recorder).await,
            BrowserOperation::Dialog => self.dialog(decoded, recorder).await,
            BrowserOperation::Screenshot => self.screenshot(decoded, recorder).await,
            BrowserOperation::Pdf => self.pdf(decoded, recorder).await,
            BrowserOperation::DownloadArm | BrowserOperation::DownloadWait | BrowserOperation::DownloadCancel => self.download(decoded, recorder).await,
            BrowserOperation::FilechooserArm | BrowserOperation::FilechooserWait | BrowserOperation::FilechooserSetFiles
            | BrowserOperation::FilechooserCancel | BrowserOperation::FilechooserResume => self.filechooser(decoded, recorder).await,
            BrowserOperation::Cookies => self.cookies(decoded, recorder).await,
            BrowserOperation::CookieSet => self.cookie_set(decoded, recorder).await,
            BrowserOperation::CookieDelete => self.cookie_delete(decoded, recorder).await,
            BrowserOperation::Tabs => self.tabs(decoded).await,
            BrowserOperation::TabNew => self.tab_new(decoded, recorder).await,
            BrowserOperation::TabSelect => self.tab_select(decoded, recorder).await,
            BrowserOperation::TabMark => self.tab_mark(decoded).await,
            BrowserOperation::TabClose => self.tab_close(decoded).await,
            BrowserOperation::Close => self.close(decoded).await,
        }
    }

    async fn navigate(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.navigate",
            &decoded.call.arguments,
            &["url", "wait_until", "timeout_ms", "snapshot"],
        )?;
        let url = required_string(object, "url")?;
        validate_navigation_url(url)?;
        let wait_until =
            optional_string(object, "wait_until")?.unwrap_or_else(|| "interactive".to_string());
        if !matches!(wait_until.as_str(), "none" | "interactive" | "complete") {
            return Err(
                "browser.navigate wait_until must be none, interactive, or complete".into(),
            );
        }
        let timeout_ms = bounded_optional_u64(object, "timeout_ms", 1, 300_000)?;
        let include_snapshot = optional_bool(object, "snapshot")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (session_disposition, state) = self.ensure_session(&mut room, true, decoded, recorder).await?;
        let session = state
            .session
            .as_mut()
            .ok_or_else(|| "browser.navigate could not materialize a CDP session".to_string())?;
        session
            .command("browser.navigate", "Page.enable", json!({}))
            .await?;
        state.refs.clear();
        let navigation_start = arm_navigation(session).await?;
        let navigation = session
            .command("browser.navigate", "Page.navigate", json!({ "url": url }))
            .await?;
        if navigation.get("isDownload").and_then(Value::as_bool) == Some(true) {
            return Ok(json!({"result":"browser.navigate","worker_environment_id":decoded.formation.worker_environment_id,
                "target_id":state.target_id,"is_download":true,"navigation":navigation,"snapshot":Value::Null,
                "session":session_disposition.into_value()}));
        }
        if let Some(error) = navigation.get("errorText").and_then(Value::as_str) {
            if !error.is_empty() {
                return Err(format!("browser.navigate failed: {error}"));
            }
        }
        let pending_dialogs = pending_dialog_values(session);
        if !pending_dialogs.is_empty() {
            state.target_url = Some(url.to_string());
            state.target_title = None;
            state.refs.clear();
            return Ok(json!({
                "result": "browser.navigate",
                "worker_environment_id": decoded.formation.worker_environment_id,
                "target_id": state.target_id,
                "url": state.target_url,
                "title": state.target_title,
                "frame_id": navigation.get("frameId").cloned().unwrap_or(Value::Null),
                "loader_id": navigation.get("loaderId").cloned().unwrap_or(Value::Null),
                "session": session_disposition.into_value(),
                "snapshot": Value::Null,
                "pending_dialogs": pending_dialogs,
            }));
        }
        if wait_until != "none" {
            let duration = timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(self.transport.navigation_timeout());
            wait_for_document(session, navigation_expectation(&navigation, navigation_start)?, &wait_until, duration).await?;
        }
        let (url, title) = page_identity(session).await?;
        let snapshot = if include_snapshot {
            let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
            state.refs = refs;
            Some(snapshot)
        } else {
            state.refs.clear();
            None
        };
        let (url, title) = if let Some(snapshot) = snapshot.as_ref() { snapshot_identity(snapshot)? } else { (url, title) };
        state.target_url = url;
        state.target_title = title;
        Ok(json!({
            "result": "browser.navigate",
            "worker_environment_id": decoded.formation.worker_environment_id,
            "target_id": state.target_id,
            "url": state.target_url,
            "title": state.target_title,
            "frame_id": navigation.get("frameId").cloned().unwrap_or(Value::Null),
            "loader_id": navigation.get("loaderId").cloned().unwrap_or(Value::Null),
            "session": session_disposition.into_value(),
            "snapshot": snapshot,
        }))
    }

    async fn snapshot(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.snapshot",
            &decoded.call.arguments,
            &["full", "max_nodes"],
        )?;
        let full = optional_bool(object, "full")?.unwrap_or(false);
        let max_nodes = bounded_optional_u64(
            object,
            "max_nodes",
            1,
            u64::try_from(self.limits.max_snapshot_nodes).unwrap_or(u64::MAX),
        )?
        .map(|value| usize::try_from(value).map_err(|_| "max_nodes exceeds usize".to_string()))
        .transpose()?;
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.snapshot requires browser.navigate or browser.tab_new first".to_string()
        })?;
        session.pump_events(Duration::from_millis(5)).await?;
        let pending_dialogs = pending_dialog_values(session);
        if !pending_dialogs.is_empty() {
            state.refs.clear();
            return Ok(json!({
                "result": "browser.snapshot",
                "worker_environment_id": decoded.formation.worker_environment_id,
                "target_id": state.target_id,
                "url": state.target_url,
                "title": state.target_title,
                "snapshot": Value::Null,
                "pending_dialogs": pending_dialogs,
            }));
        }
        state.refs.clear();
        let (snapshot, refs) = snapshot_page(session, &self.limits, max_nodes, full, decoded.source_movement_seq).await?;
        let (url, title) = snapshot_identity(&snapshot)?;
        state.target_url = url;
        state.target_title = title;
        state.refs = refs;
        Ok(json!({
            "result": "browser.snapshot",
            "worker_environment_id": decoded.formation.worker_environment_id,
            "target_id": state.target_id,
            "url": state.target_url,
            "title": state.target_title,
            "snapshot": snapshot,
        }))
    }

    async fn click(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        if decoded.call.arguments.get("selector").is_some()
            || (matches!(self.transport, BrowserTransport::Native(_)) && decoded.call.arguments.get("ref").is_none() && decoded.call.arguments.get("snapshot") == Some(&Value::Bool(false))) {
            return self.browser_family(decoded, recorder).await;
        }
        let object = page_object(
            "browser.click",
            &decoded.call.arguments,
            &["ref", "x", "y", "button", "click_count", "snapshot", "modifiers"],
        )?;
        let modifiers = page_modifiers(object)?;
        let reference = optional_string(object, "ref")?;
        let x = optional_f64(object, "x")?;
        let y = optional_f64(object, "y")?;
        if reference.is_some() == (x.is_some() && y.is_some()) {
            return Err("browser.click requires exactly one of ref or numeric x+y".to_string());
        }
        if x.is_some() != y.is_some() {
            return Err("browser.click x and y must be supplied together".to_string());
        }
        let button = optional_string(object, "button")?.unwrap_or_else(|| "left".to_string());
        if !matches!(button.as_str(), "left" | "right" | "middle") {
            return Err("browser.click button must be left, right, or middle".to_string());
        }
        let click_count = bounded_optional_u64(object, "click_count", 1, 3)?.unwrap_or(1);
        let include_snapshot = optional_bool(object, "snapshot")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let backend_node_id = reference
            .as_deref()
            .map(|reference| {
                state.refs.get(reference).cloned().ok_or_else(|| {
                    format!(
                        "browser.click ref '{reference}' is stale or unknown; call browser.snapshot"
                    )
                })
            })
            .transpose()?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.click requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let navigation_start = arm_navigation(session).await?;
        let point = match backend_node_id.as_ref() {
            Some(node) => pointer_point(session, node, "browser.click", true).await?,
            None => actions::PointerPoint::root(x.unwrap_or_default(),y.unwrap_or_default()),
        };
        let (x,y)=point.root;
        state.refs.clear();
        point.dispatch(session,"browser.click",json!({"type":"mouseMoved","modifiers":modifiers})).await?;
        let buttons = match button.as_str() { "right" => 2, "middle" => 4, _ => 1 };
        for count in 1..=click_count {
            point.dispatch(session,"browser.click",json!({"type":"mousePressed","button":button,"buttons":buttons,"clickCount":count,"modifiers":modifiers})).await?;
            point.dispatch(session,"browser.click",json!({"type":"mouseReleased","button":button,"buttons":0,"clickCount":count,"modifiers":modifiers})).await?;
        }
        settle_after_action(session, &navigation_start).await?;
        let pending_dialogs = pending_dialog_values(session);
        if !pending_dialogs.is_empty() {
            state.refs.clear();
            return Ok(json!({
                "result": "browser.click",
                "target_id": state.target_id,
                "ref": reference,
                "x": x,
                "y": y,
                "button": button,
                "click_count": click_count,
                "snapshot": Value::Null,
                "pending_dialogs": pending_dialogs,
            }));
        }
        let (url, title) = page_identity(session).await?;
        let snapshot = if include_snapshot {
            let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
            state.refs = refs;
            Some(snapshot)
        } else {
            state.refs.clear();
            None
        };
        let (url, title) = if let Some(snapshot) = snapshot.as_ref() { snapshot_identity(snapshot)? } else { (url, title) };
        state.target_url = url;
        state.target_title = title;
        Ok(json!({
            "result": "browser.click",
            "target_id": state.target_id,
            "ref": reference,
            "x": x,
            "y": y,
            "button": button,
            "click_count": click_count,
            "snapshot": snapshot,
        }))
    }

    async fn type_text(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.type",
            &decoded.call.arguments,
            &["ref", "text", "clear", "submit", "snapshot"],
        )?;
        let reference = required_string(object, "ref")?;
        let text = required_string_allow_empty(object, "text")?;
        if text.len() > self.limits.max_type_chars {
            return Err(format!(
                "browser.type text exceeds {} bytes",
                self.limits.max_type_chars
            ));
        }
        let clear = optional_bool(object, "clear")?.unwrap_or(true);
        let submit = optional_bool(object, "submit")?.unwrap_or(false);
        let include_snapshot = optional_bool(object, "snapshot")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let backend_node_id = state.refs.get(reference).cloned().ok_or_else(|| {
            format!("browser.type ref '{reference}' is stale or unknown; call browser.snapshot")
        })?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.type requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let navigation_start = arm_navigation(session).await?;
        state.refs.clear();
        type_into_ref(session, &backend_node_id, text, clear, submit).await?;
        settle_after_action(session, &navigation_start).await?;
        let pending_dialogs = pending_dialog_values(session);
        if !pending_dialogs.is_empty() {
            state.refs.clear();
            return Ok(json!({
                "result": "browser.type",
                "target_id": state.target_id,
                "ref": reference,
                "typed_chars": text.chars().count(),
                "cleared": clear,
                "submitted": submit,
                "snapshot": Value::Null,
                "pending_dialogs": pending_dialogs,
            }));
        }
        let (url, title) = page_identity(session).await?;
        let snapshot = if include_snapshot {
            let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
            state.refs = refs;
            Some(snapshot)
        } else {
            state.refs.clear();
            None
        };
        let (url, title) = if let Some(snapshot) = snapshot.as_ref() { snapshot_identity(snapshot)? } else { (url, title) };
        state.target_url = url;
        state.target_title = title;
        Ok(json!({
            "result": "browser.type",
            "target_id": state.target_id,
            "ref": reference,
            "typed_chars": text.chars().count(),
            "cleared": clear,
            "submitted": submit,
            "snapshot": snapshot,
        }))
    }

    async fn key(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        if decoded.call.arguments.get("keys").is_some() { return self.browser_family(decoded, recorder).await; }
        let object = page_object(
            "browser.key",
            &decoded.call.arguments,
            &["key", "modifiers", "text"],
        )?;
        let key = required_string(object, "key")?;
        if key.chars().count() > 64 {
            return Err("browser.key key exceeds 64 characters".to_string());
        }
        let modifiers = bounded_optional_u64(object, "modifiers", 0, 15)?.unwrap_or(0);
        let text = optional_string_allow_empty(object, "text")?;
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.key requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let navigation_start = arm_navigation(session).await?;
        state.refs.clear();
        dispatch_key(session, "browser.key", key, modifiers, text.as_deref()).await?;
        settle_after_action(session, &navigation_start).await?;
        let (url, title) = page_identity(session).await?;
        state.target_url = url;
        state.target_title = title;
        Ok(json!({
            "result": "browser.key",
            "target_id": state.target_id,
            "key": key,
            "modifiers": modifiers,
        }))
    }

    async fn scroll(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        if decoded.call.arguments.get("mode").is_some() { return self.browser_family(decoded, recorder).await; }
        let object = page_object(
            "browser.scroll",
            &decoded.call.arguments,
            &["delta_x", "delta_y", "x", "y", "snapshot"],
        )?;
        let delta_x = optional_f64(object, "delta_x")?.unwrap_or(0.0);
        let delta_y = optional_f64(object, "delta_y")?.unwrap_or(0.0);
        if delta_x == 0.0 && delta_y == 0.0 {
            return Err("browser.scroll requires a non-zero delta_x or delta_y".to_string());
        }
        let x = optional_f64(object, "x")?.unwrap_or(0.0);
        let y = optional_f64(object, "y")?.unwrap_or(0.0);
        let include_snapshot = optional_bool(object, "snapshot")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.scroll requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let navigation_start = arm_navigation(session).await?;
        state.refs.clear();
        session.move_cursor("browser.scroll", x, y, true).await?;
        session
            .command(
                "browser.scroll",
                "Input.dispatchMouseEvent",
                json!({
                    "type": "mouseWheel",
                    "x": x,
                    "y": y,
                    "deltaX": delta_x,
                    "deltaY": delta_y
                }),
            )
            .await?;
        settle_after_action(session, &navigation_start).await?;
        let snapshot = if include_snapshot {
            let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
            state.refs = refs;
            Some(snapshot)
        } else {
            state.refs.clear();
            None
        };
        if let Some(snapshot) = snapshot.as_ref() {
            let (url, title) = snapshot_identity(snapshot)?;
            state.target_url = url;
            state.target_title = title;
        }
        Ok(json!({
            "result": "browser.scroll",
            "target_id": state.target_id,
            "delta_x": delta_x,
            "delta_y": delta_y,
            "snapshot": snapshot,
        }))
    }

    async fn back(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        page_object("browser.back", &decoded.call.arguments, &[])?;
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.back requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let history = session
            .command("browser.back", "Page.getNavigationHistory", json!({}))
            .await?;
        let current = history
            .get("currentIndex")
            .and_then(Value::as_i64)
            .ok_or_else(|| "browser.back CDP history has no currentIndex".to_string())?;
        if current <= 0 {
            return Err("browser.back has no previous history entry".to_string());
        }
        let entries = history
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| "browser.back CDP history has no entries".to_string())?;
        let previous = entries
            .get(usize::try_from(current - 1).map_err(|_| "history index overflow".to_string())?)
            .ok_or_else(|| "browser.back previous history entry is unavailable".to_string())?;
        let entry_id = previous
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| "browser.back previous history entry has no id".to_string())?;
        state.refs.clear();
        let navigation_start = arm_navigation(session).await?;
        session
            .command(
                "browser.back",
                "Page.navigateToHistoryEntry",
                json!({ "entryId": entry_id }),
            )
            .await?;
        wait_for_document(session, NavigationExpectation::Change(navigation_start), "interactive", self.transport.navigation_timeout()).await?;
        let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
        let (url, title) = snapshot_identity(&snapshot)?;
        state.target_url = url;
        state.target_title = title;
        state.refs = refs;
        Ok(json!({
            "result": "browser.back",
            "target_id": state.target_id,
            "url": state.target_url,
            "title": state.target_title,
            "snapshot": snapshot,
        }))
    }

    async fn forward(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        page_object("browser.forward", &decoded.call.arguments, &[])?;
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.forward requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let history = session
            .command("browser.forward", "Page.getNavigationHistory", json!({}))
            .await?;
        let current = history
            .get("currentIndex")
            .and_then(Value::as_i64)
            .ok_or_else(|| "browser.forward CDP history has no currentIndex".to_string())?;
        let entries = history
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| "browser.forward CDP history has no entries".to_string())?;
        let next_index = usize::try_from(current.saturating_add(1))
            .map_err(|_| "browser.forward history index overflow".to_string())?;
        let next = entries
            .get(next_index)
            .ok_or_else(|| "browser.forward has no next history entry".to_string())?;
        let entry_id = next
            .get("id")
            .and_then(Value::as_i64)
            .ok_or_else(|| "browser.forward next history entry has no id".to_string())?;
        state.refs.clear();
        let navigation_start = arm_navigation(session).await?;
        session
            .command(
                "browser.forward",
                "Page.navigateToHistoryEntry",
                json!({ "entryId": entry_id }),
            )
            .await?;
        wait_for_document(session, NavigationExpectation::Change(navigation_start), "interactive", self.transport.navigation_timeout()).await?;
        let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
        let (url, title) = snapshot_identity(&snapshot)?;
        state.target_url = url;
        state.target_title = title;
        state.refs = refs;
        Ok(json!({
            "result": "browser.forward",
            "target_id": state.target_id,
            "url": state.target_url,
            "title": state.target_title,
            "snapshot": snapshot,
        }))
    }

    async fn reload(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.reload",
            &decoded.call.arguments,
            &["ignore_cache", "wait_until", "timeout_ms", "snapshot"],
        )?;
        let ignore_cache = optional_bool(object, "ignore_cache")?.unwrap_or(false);
        let wait_until =
            optional_string(object, "wait_until")?.unwrap_or_else(|| "interactive".into());
        if !matches!(wait_until.as_str(), "none" | "interactive" | "complete") {
            return Err("browser.reload wait_until must be none, interactive, or complete".into());
        }
        let timeout_ms = bounded_optional_u64(object, "timeout_ms", 1, 300_000)?;
        let include_snapshot = optional_bool(object, "snapshot")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.reload requires browser.navigate or browser.tab_new first".to_string()
        })?;
        state.refs.clear();
        let navigation_start = arm_navigation(session).await?;
        session
            .command(
                "browser.reload",
                "Page.reload",
                json!({ "ignoreCache": ignore_cache }),
            )
            .await?;
        if wait_until != "none" {
            wait_for_document(
                session,
                NavigationExpectation::Change(navigation_start),
                &wait_until,
                timeout_ms
                    .map(Duration::from_millis)
                    .unwrap_or(self.transport.navigation_timeout()),
            )
            .await?;
        }
        let (url, title) = page_identity(session).await?;
        let snapshot = if include_snapshot {
            let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
            state.refs = refs;
            Some(snapshot)
        } else {
            state.refs.clear();
            None
        };
        let (url, title) = if let Some(snapshot) = snapshot.as_ref() { snapshot_identity(snapshot)? } else { (url, title) };
        state.target_url = url;
        state.target_title = title;
        Ok(json!({
            "result": "browser.reload",
            "target_id": state.target_id,
            "url": state.target_url,
            "title": state.target_title,
            "ignore_cache": ignore_cache,
            "snapshot": snapshot,
        }))
    }

    async fn hover(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.hover",
            &decoded.call.arguments,
            &["ref", "snapshot"],
        )?;
        let reference = required_string(object, "ref")?;
        let include_snapshot = optional_bool(object, "snapshot")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let backend_node_id = state.refs.get(reference).cloned().ok_or_else(|| {
            format!("browser.hover ref '{reference}' is stale or unknown; call browser.snapshot")
        })?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.hover requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let navigation_start = arm_navigation(session).await?;
        let point = pointer_point(session,&backend_node_id,"browser.hover",false).await?;
        let (x,y)=point.root;
        state.refs.clear();
        point.dispatch(session,"browser.hover",json!({"type":"mouseMoved"})).await?;
        settle_after_action(session, &navigation_start).await?;
        let snapshot = if include_snapshot {
            let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
            state.refs = refs;
            Some(snapshot)
        } else {
            None
        };
        if let Some(snapshot) = snapshot.as_ref() {
            let (url, title) = snapshot_identity(snapshot)?;
            state.target_url = url;
            state.target_title = title;
        }
        Ok(json!({
            "result": "browser.hover",
            "target_id": state.target_id,
            "ref": reference,
            "x": x,
            "y": y,
            "snapshot": snapshot,
        }))
    }

    async fn select(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.select",
            &decoded.call.arguments,
            &["ref", "values", "snapshot"],
        )?;
        let reference = required_string(object, "ref")?;
        let values = required_string_array(object, "values", 1, 100)?;
        let include_snapshot = optional_bool(object, "snapshot")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let backend_node_id = state.refs.get(reference).cloned().ok_or_else(|| {
            format!("browser.select ref '{reference}' is stale or unknown; call browser.snapshot")
        })?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.select requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let navigation_start = arm_navigation(session).await?;
        state.refs.clear();
        let selected = actions::select_from_ref(session, &backend_node_id, &values).await?;
        settle_after_action(session, &navigation_start).await?;
        let snapshot = if include_snapshot {
            let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
            state.refs = refs;
            Some(snapshot)
        } else {
            None
        };
        if let Some(snapshot) = snapshot.as_ref() {
            let (url, title) = snapshot_identity(snapshot)?;
            state.target_url = url;
            state.target_title = title;
        }
        Ok(json!({
            "result": "browser.select",
            "target_id": state.target_id,
            "ref": reference,
            "selected": selected,
            "snapshot": snapshot,
        }))
    }

    async fn drag(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        if decoded.call.arguments.get("path").is_some() { return self.browser_family(decoded, recorder).await; }
        let object = page_object(
            "browser.drag",
            &decoded.call.arguments,
            &["source_ref", "target_ref", "steps", "snapshot"],
        )?;
        let source_ref = required_string(object, "source_ref")?;
        let target_ref = required_string(object, "target_ref")?;
        let steps = bounded_optional_u64(object, "steps", 1, 100)?.unwrap_or(12);
        let include_snapshot = optional_bool(object, "snapshot")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let source_node =
            state.refs.get(source_ref).cloned().ok_or_else(|| {
                format!("browser.drag source_ref '{source_ref}' is stale or unknown")
            })?;
        let target_node =
            state.refs.get(target_ref).cloned().ok_or_else(|| {
                format!("browser.drag target_ref '{target_ref}' is stale or unknown")
            })?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.drag requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let navigation_start = arm_navigation(session).await?;
        actions::validate_ref(session, &target_node).await?;
        let (source_x, source_y) = pointer_point(session, &source_node, "browser.drag", false).await?.root;
        state.refs.clear();
        session.move_cursor("browser.drag", source_x, source_y, true).await?;
        session
            .command(
                "browser.drag",
                "Input.dispatchMouseEvent",
                json!({"type":"mouseMoved","x":source_x,"y":source_y}),
            )
            .await?;
        session
            .command(
                "browser.drag",
                "Input.dispatchMouseEvent",
                json!({"type":"mousePressed","x":source_x,"y":source_y,"button":"left","buttons":1,"clickCount":1}),
            )
            .await?;
        let mut last_point = (source_x, source_y);
        let mut release_dispatched = false;
        let drag_result: Result<(f64, f64), String> = async {
            let (target_x, target_y) = pointer_point(session, &target_node, "browser.drag", false).await?.root;
            for step in 1..=steps {
                let progress = step as f64 / steps as f64;
                let x = source_x + (target_x - source_x) * progress;
                let y = source_y + (target_y - source_y) * progress;
                session.move_cursor("browser.drag", x, y, false).await?;
                session.command("browser.drag", "Input.dispatchMouseEvent", json!({
                    "type":"mouseMoved","x":x,"y":y,"button":"left","buttons":1,
                })).await?;
                last_point = (x, y);
                sleep(Duration::from_millis(12)).await;
            }
            release_dispatched = true;
            session.command("browser.drag", "Input.dispatchMouseEvent", json!({
                "type":"mouseReleased","x":target_x,"y":target_y,"button":"left","buttons":0,"clickCount":1,
            })).await?;
            Ok((target_x, target_y))
        }.await;
        if drag_result.is_err() && !release_dispatched && session.usable {
            if let Err(release_error) = session.command("browser.drag", "Input.dispatchMouseEvent", json!({
                "type":"mouseReleased","x":last_point.0,"y":last_point.1,"button":"left","buttons":0,"clickCount":1,
            })).await {
                return Err(format!("{}; pointer release failed: {release_error}", drag_result.unwrap_err()));
            }
        }
        let (target_x, target_y) = drag_result?;
        settle_after_action(session, &navigation_start).await?;
        let snapshot = if include_snapshot {
            let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
            state.refs = refs;
            Some(snapshot)
        } else {
            None
        };
        if let Some(snapshot) = snapshot.as_ref() {
            let (url, title) = snapshot_identity(snapshot)?;
            state.target_url = url;
            state.target_title = title;
        }
        Ok(json!({
            "result": "browser.drag",
            "target_id": state.target_id,
            "source_ref": source_ref,
            "target_ref": target_ref,
            "source": {"x": source_x, "y": source_y},
            "target": {"x": target_x, "y": target_y},
            "snapshot": snapshot,
        }))
    }

    async fn wait(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        if decoded.call.arguments.get("phase").is_some() {
            let config = self.wait_completion.as_ref().ok_or("Browser wait completion dependency is not selected")?;
            config.validate(decoded, false)?;
            let BrowserTransport::Native(transport) = &self.transport else { return Err("Retained wait requires the native browser observation owner".into()); };
            return match decoded.call.arguments.get("phase").and_then(Value::as_str) {
                Some("start") => {
                    requested_target(decoded)?.ok_or("Retained wait start requires an explicit owned target")?;
                    let room = self.room(&decoded.formation).await?;
                    let mut state = room.lock().await;
                    let (_, page) = self.ensure_session(&mut state, false, decoded, recorder).await?;
                    page.session.as_mut().ok_or("Wait start has no exact owned page")?.command(&decoded.call.tool_id, "OctopusBrowser.startWait", json!({"arguments":decoded.call.arguments,"limits":{"max_waiters":16,"max_event_bytes":self.limits.max_event_bytes}})).await
                }
                Some("cancel") => {
                    let args = strict_object("browser.wait cancel", &decoded.call.arguments, &["phase", "waiter"])?;
                    let waiter = args.get("waiter").ok_or("Wait cancellation requires its exact handle")?;
                    crate::browser_wait_completion::validate_waiter(waiter, decoded)?;
                    let mut connection = transport.connect(None).await?;
                    connection.request("OctopusBrowser.cancelWait", json!({"waiter":waiter}), transport.request_timeout).await
                }
                _ => Err("Unknown retained browser wait phase".into()),
            };
        }
        if decoded.call.arguments.get("url_pattern").is_some() || decoded.call.arguments.get("load_state").is_some() {
            let object = page_object("browser.wait", &decoded.call.arguments, &["url_pattern", "wait_until", "load_state", "timeout_ms"])?;
            let pattern = optional_string(object, "url_pattern")?;
            let state = optional_string(object, "load_state")?;
            let until = optional_string(object, "wait_until")?;
            bounded_optional_u64(object, "timeout_ms", 0, 300_000)?;
            if pattern.is_some() == state.is_some()
                || pattern.as_ref().is_some_and(|value| value.len() > self.limits.max_eval_chars)
                || state.as_deref().is_some_and(|value| !matches!(value, "load" | "domcontentloaded"))
                || until.as_deref().is_some_and(|value| pattern.is_none() || !matches!(value, "commit" | "load" | "domcontentloaded")) {
                return Err("browser.wait requires one URL pattern or supported load state".into());
            }
            return self.browser_family(decoded, recorder).await;
        }
        let object = page_object(
            "browser.wait",
            &decoded.call.arguments,
            &[
                "duration_ms",
                "text",
                "expression",
                "ready_state",
                "timeout_ms",
                "poll_ms",
                "snapshot",
            ],
        )?;
        let duration_ms = bounded_optional_u64(object, "duration_ms", 1, 30_000)?;
        let text = optional_string_allow_empty(object, "text")?;
        let expression = optional_string(object, "expression")?;
        let ready_state = optional_string(object, "ready_state")?;
        let conditions = usize::from(duration_ms.is_some())
            + usize::from(text.is_some())
            + usize::from(expression.is_some())
            + usize::from(ready_state.is_some());
        if conditions != 1 {
            return Err(
                "browser.wait requires exactly one of duration_ms, text, expression, or ready_state"
                    .to_string(),
            );
        }
        if expression
            .as_deref()
            .is_some_and(|expression| expression.len() > self.limits.max_eval_chars)
        {
            return Err(format!(
                "browser.wait expression exceeds {} bytes",
                self.limits.max_eval_chars
            ));
        }
        if let Some(state) = ready_state.as_deref() {
            if !matches!(state, "interactive" | "complete") {
                return Err("browser.wait ready_state must be interactive or complete".into());
            }
        }
        let timeout_ms = bounded_optional_u64(object, "timeout_ms", 1, 300_000)?
            .unwrap_or(self.transport.navigation_timeout().as_millis() as u64);
        let poll_ms = bounded_optional_u64(object, "poll_ms", 10, 5000)?.unwrap_or(100);
        let include_snapshot = optional_bool(object, "snapshot")?.unwrap_or(false);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.wait requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let condition = if let Some(duration_ms) = duration_ms {
            sleep(Duration::from_millis(duration_ms)).await;
            "duration"
        } else {
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            loop {
                let matched = if let Some(expected) = text.as_deref() {
                    let expected = serde_json::to_string(expected)
                        .map_err(|error| format!("browser.wait text encode failed: {error}"))?;
                    evaluate_boolean(
                        session,
                        "browser.wait",
                        &format!("(document.body?.innerText || '').includes({expected})"),
                    )
                    .await?
                } else if let Some(expression) = expression.as_deref() {
                    evaluate_boolean(
                        session,
                        "browser.wait",
                        &format!("Promise.resolve(({expression})).then(value => Boolean(value))"),
                    )
                    .await?
                } else {
                    let observed = session
                        .command(
                            "browser.wait",
                            "Runtime.evaluate",
                            json!({"expression":"document.readyState","returnByValue":true}),
                        )
                        .await?
                        .get("result")
                        .and_then(|result| result.get("value"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    match ready_state.as_deref().unwrap_or_default() {
                        "interactive" => matches!(observed.as_str(), "interactive" | "complete"),
                        "complete" => observed == "complete",
                        _ => false,
                    }
                };
                if matched {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "browser.wait condition was not met within {timeout_ms} ms"
                    ));
                }
                sleep(Duration::from_millis(poll_ms)).await;
            }
            if text.is_some() {
                "text"
            } else if expression.is_some() {
                "expression"
            } else {
                "ready_state"
            }
        };
        let snapshot = if include_snapshot {
            let (snapshot, refs) = snapshot_page(session, &self.limits, None, false, decoded.source_movement_seq).await?;
            state.refs = refs;
            Some(snapshot)
        } else {
            None
        };
        Ok(json!({
            "result": "browser.wait",
            "target_id": state.target_id,
            "condition": condition,
            "snapshot": snapshot,
        }))
    }

    async fn images(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object("browser.images", &decoded.call.arguments, &["limit"])?;
        let limit = bounded_optional_u64(
            object,
            "limit",
            1,
            u64::try_from(self.limits.max_images).unwrap_or(u64::MAX),
        )?
        .map(|limit| {
            usize::try_from(limit).map_err(|_| "browser.images limit overflow".to_string())
        })
        .transpose()?
        .unwrap_or(self.limits.max_images);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.images requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let expression = format!(
            "(()=>{{const all=Array.from(document.images);return {{total:all.length,images:all.slice(0,{limit}).map(img=>({{src:img.currentSrc||img.src,alt:img.alt||'',title:img.title||'',width:img.naturalWidth,height:img.naturalHeight,displayWidth:img.clientWidth,displayHeight:img.clientHeight,loading:img.loading||'',complete:img.complete}})).filter(img=>img.src&&!img.src.startsWith('data:'))}}}})()"
        );
        let result = session
            .command(
                "browser.images",
                "Runtime.evaluate",
                json!({"expression":expression,"returnByValue":true}),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(format!(
                "browser.images evaluation failed: {}",
                truncate(&exception.to_string(), 1000)
            ));
        }
        let value = result
            .get("result")
            .and_then(|result| result.get("value"))
            .cloned()
            .ok_or_else(|| "browser.images evaluation returned no value".to_string())?;
        Ok(json!({
            "result": "browser.images",
            "target_id": state.target_id,
            "total": value.get("total").cloned().unwrap_or(Value::Null),
            "images": value.get("images").cloned().unwrap_or_else(|| json!([])),
        }))
    }

    async fn console(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.console",
            &decoded.call.arguments,
            &["clear", "expression"],
        )?;
        let clear = optional_bool(object, "clear")?.unwrap_or(false);
        let expression = optional_string(object, "expression")?;
        if expression
            .as_deref()
            .is_some_and(|expression| expression.len() > self.limits.max_eval_chars)
        {
            return Err(format!(
                "browser.console expression exceeds {} bytes",
                self.limits.max_eval_chars
            ));
        }
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.console requires browser.navigate or browser.tab_new first".to_string()
        })?;
        if let Some(expression) = expression {
            let result = session
                .command(
                    "browser.console",
                    "Runtime.evaluate",
                    json!({"expression":expression,"returnByValue":true,"awaitPromise":true,"userGesture":true}),
                )
                .await?;
            if let Some(exception) = result.get("exceptionDetails") {
                return Err(format!(
                    "browser.console expression threw: {}",
                    truncate(&exception.to_string(), 1000)
                ));
            }
            return Ok(json!({
                "result": "browser.console",
                "target_id": state.target_id,
                "value": result.get("result").and_then(|value| value.get("value")).cloned().unwrap_or(Value::Null),
                "value_type": result.get("result").and_then(|value| value.get("type")).cloned().unwrap_or(Value::Null),
            }));
        }
        session
            .command(
                "browser.console",
                "Runtime.evaluate",
                json!({"expression":"void 0","returnByValue":true}),
            )
            .await?;
        session.pump_events(Duration::from_millis(5)).await?;
        let methods = [
            "Runtime.consoleAPICalled",
            "Runtime.exceptionThrown",
            "Log.entryAdded",
            "Network.loadingFailed",
        ];
        let entries = if clear {
            session
                .take_events(&methods, self.limits.max_console_entries)
                .iter()
                .map(console_event_value)
                .collect::<Vec<_>>()
        } else {
            session
                .events
                .iter()
                .filter(|event| {
                    event
                        .value
                        .get("method")
                        .and_then(Value::as_str)
                        .is_some_and(|method| methods.contains(&method))
                })
                .take(self.limits.max_console_entries)
                .map(console_event_value)
                .collect::<Vec<_>>()
        };
        Ok(json!({
            "result": "browser.console",
            "target_id": state.target_id,
            "entries": entries,
            "entry_count": entries.len(),
            "cleared": clear,
            "dropped_events": session.dropped_events,
        }))
    }

    async fn dialog(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.dialog",
            &decoded.call.arguments,
            &["action", "prompt_text", "dialog_id"],
        )?;
        let action = required_string(object, "action")?;
        if !matches!(action, "accept" | "dismiss") {
            return Err("browser.dialog action must be accept or dismiss".into());
        }
        let prompt_text = optional_string_allow_empty(object, "prompt_text")?.unwrap_or_default();
        let requested_id = optional_string(object, "dialog_id")?;
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.dialog requires browser.navigate or browser.tab_new first".to_string()
        })?;
        session.pump_events(Duration::from_millis(10)).await?;
        let pending = session.pending_events("Page.javascriptDialogOpening", 100);
        if pending.is_empty() {
            return Err("browser.dialog has no pending JavaScript dialog".into());
        }
        let selected_id = match requested_id.as_deref() {
            Some(id) => parse_cdp_event_id(id)?,
            None if pending.len() == 1 => pending[0].id,
            None => {
                return Err(
                    "browser.dialog has multiple pending dialogs; dialog_id is required".into(),
                );
            }
        };
        let selected = pending
            .into_iter()
            .find(|event| event.id == selected_id)
            .ok_or_else(|| {
                format!("browser.dialog dialog_id 'cdp-{selected_id}' is not pending")
            })?;
        let dialog = dialog_event_value(selected);
        session
            .command(
                "browser.dialog",
                "Page.handleJavaScriptDialog",
                json!({
                    "accept": action == "accept",
                    "promptText": prompt_text,
                }),
            )
            .await?;
        session.take_event_by_id(selected_id);
        Ok(json!({
            "result": "browser.dialog",
            "target_id": state.target_id,
            "action": action,
            "dialog": dialog,
        }))
    }

    async fn evaluate(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.eval",
            &decoded.call.arguments,
            &["script", "await_promise"],
        )?;
        let script = required_string(object, "script")?;
        if script.len() > self.limits.max_eval_chars {
            return Err(format!(
                "browser.eval script exceeds {} bytes",
                self.limits.max_eval_chars
            ));
        }
        let await_promise = optional_bool(object, "await_promise")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.eval requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let result = session
            .command(
                "browser.eval",
                "Runtime.evaluate",
                json!({
                    "expression": script,
                    "returnByValue": true,
                    "awaitPromise": await_promise,
                    "userGesture": true
                }),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(format!(
                "browser.eval script threw: {}",
                truncate(&exception.to_string(), 1000)
            ));
        }
        let (url, title) = page_identity(session).await?;
        state.target_url = url;
        state.target_title = title;
        Ok(json!({
            "result": "browser.eval",
            "target_id": state.target_id,
            "value": result.get("result").and_then(|value| value.get("value")).cloned().unwrap_or(Value::Null),
            "value_type": result.get("result").and_then(|value| value.get("type")).cloned().unwrap_or(Value::Null),
        }))
    }

    async fn screenshot(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.screenshot",
            &decoded.call.arguments,
            &["format", "quality", "full_page", "clip", "pixel_mode"],
        )?;
        let format = optional_string(object, "format")?.unwrap_or_else(|| "png".to_string());
        if !matches!(format.as_str(), "png" | "jpeg" | "webp") {
            return Err("browser.screenshot format must be png, jpeg, or webp".to_string());
        }
        let quality = bounded_optional_u64(object, "quality", 0, 100)?;
        if format == "png" && quality.is_some() {
            return Err("browser.screenshot quality is not valid for png".to_string());
        }
        let full_page = optional_bool(object, "full_page")?.unwrap_or(false);
        let clip = object.get("clip").cloned();
        if let Some(clip) = &clip {
            let object = strict_object("browser.screenshot clip", clip, &["x", "y", "width", "height"])?;
            if object.len() != 4 { return Err("Screenshot clip requires x, y, width, height".into()); }
        }
        let pixel_mode = optional_string(object, "pixel_mode")?.unwrap_or_else(|| "device".into());
        if !matches!(pixel_mode.as_str(), "css" | "device") { return Err("Screenshot pixel_mode must be css or device".into()); }
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.screenshot requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let captured = capture::screenshot(
            session, &format, quality, full_page, clip, pixel_mode == "css", self.limits.max_screenshot_bytes,
        ).await?;
        let bytes = captured.image.bytes.as_ref();
        let extension = if format == "jpeg" {
            "jpg"
        } else {
            format.as_str()
        };
        let root = self
            .environment
            .var_dir
            .join("worker-environments")
            .join(decoded.formation.worker_environment_id.as_str())
            .join("captures");
        std::fs::create_dir_all(&root).map_err(|error| {
            format!(
                "browser capture directory '{}' cannot be created: {error}",
                root.display()
            )
        })?;
        let path = root.join(format!(
            "capture-{}.{}",
            decoded.source_movement_seq, extension
        ));
        {
            use std::os::unix::fs::OpenOptionsExt;
            match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
                .custom_flags(libc::O_NOFOLLOW).open(&path)
            {
                Ok(mut file) => file.write_all(bytes).and_then(|()| file.sync_all())
                    .map_err(|error| format!("browser screenshot cannot be persisted: {error}"))?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = octopus_interfaces::read_artifact_carrier_file_bounded(&path, bytes.len() as u64)
                        .map_err(|error| error.to_string())?;
                    if existing != bytes { return Err("browser screenshot conflicts with its existing immutable capture".into()); }
                    std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&path)
                        .and_then(|file| file.sync_all()).map_err(|error| format!("browser screenshot cannot be synced: {error}"))?;
                }
                Err(error) => return Err(format!("browser screenshot cannot be created: {error}")),
            }
            std::fs::File::open(&root).and_then(|directory| directory.sync_all())
                .map_err(|error| format!("browser screenshot directory cannot be synced: {error}"))?;
        }
        let manifest = serde_json::to_vec_pretty(&octopus_interfaces::InterfaceArtifactManifest {
            label: "browser.screenshot".into(), payload_path: path.clone(),
            payload_content_hash: Some(blake3::hash(bytes).to_hex().to_string()),
        }).map_err(|error| error.to_string())?;
        let media = InterfaceArtifactFile::store_manifest_bounded(
            root.join("artifacts"), &manifest,
            self.environment.host_io_limits.max_manifest_bytes,
            self.environment.host_io_limits.max_payload_bytes,
        ).map_err(|error| format!("browser screenshot artifact could not be written: {error}"))?
            .read_manifest()
            .map_err(|error| format!("browser screenshot artifact could not be read: {error}"))?;
        std::fs::File::open(root.join("artifacts")).and_then(|directory| directory.sync_all())
            .map_err(|error| format!("browser screenshot artifact directory cannot be synced: {error}"))?;
        let mut directory = root.as_path();
        loop {
            std::fs::File::open(directory).and_then(|directory| directory.sync_all())
                .map_err(|error| format!("browser screenshot parent directory cannot be synced: {error}"))?;
            if directory == self.environment.var_dir { break; }
            directory = directory.parent().filter(|parent| parent.starts_with(&self.environment.var_dir))
                .ok_or("browser screenshot directory left its private owner")?;
        }
        state.target_url = captured.url;
        state.target_title = captured.title;
        let mut capture = json!({
            "result": "browser.screenshot",
            "worker_environment_id": decoded.formation.worker_environment_id,
            "source_movement_seq": decoded.source_movement_seq,
            "target_id": state.target_id,
            "url": state.target_url,
            "title": state.target_title,
            "path": path,
            "content_hash": captured.content_hash,
            "bytes": bytes.len(),
            "media_type": format!("image/{format}"),
            "format": format,
            "full_page": full_page,
            "geometry": captured.geometry,
        });
        state.last_capture = Some(capture.clone());
        capture["media"] = json!([{"mime_type": format!("image/{format}"), "artifact": media, "payload_base64": captured.payload_base64}]);
        Ok(capture)
    }

    async fn pdf(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.pdf",
            &decoded.call.arguments,
            &[
                "landscape",
                "print_background",
                "scale",
                "page_ranges",
                "prefer_css_page_size",
            ],
        )?;
        let landscape = optional_bool(object, "landscape")?.unwrap_or(false);
        let print_background = optional_bool(object, "print_background")?.unwrap_or(true);
        let scale = optional_f64(object, "scale")?.unwrap_or(1.0);
        if !(0.1..=2.0).contains(&scale) {
            return Err("browser.pdf scale must be between 0.1 and 2.0".into());
        }
        let page_ranges = optional_string_allow_empty(object, "page_ranges")?.unwrap_or_default();
        let prefer_css_page_size = optional_bool(object, "prefer_css_page_size")?.unwrap_or(true);
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.pdf requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let result = session
            .command(
                "browser.pdf",
                "Page.printToPDF",
                json!({
                    "landscape": landscape,
                    "printBackground": print_background,
                    "scale": scale,
                    "pageRanges": page_ranges,
                    "preferCSSPageSize": prefer_css_page_size,
                    "transferMode": "ReturnAsBase64"
                }),
            )
            .await?;
        let encoded = result
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| "browser.pdf CDP reply has no data".to_string())?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| format!("browser.pdf data is not base64: {error}"))?;
        if bytes.len() > self.limits.max_pdf_bytes {
            return Err(format!(
                "browser.pdf result exceeds {} bytes",
                self.limits.max_pdf_bytes
            ));
        }
        let root = self
            .environment
            .var_dir
            .join("worker-environments")
            .join(decoded.formation.worker_environment_id.as_str())
            .join("captures");
        std::fs::create_dir_all(&root).map_err(|error| {
            format!(
                "browser capture directory '{}' cannot be created: {error}",
                root.display()
            )
        })?;
        let path = root.join(format!("document-{}.pdf", decoded.source_movement_seq));
        std::fs::write(&path, &bytes).map_err(|error| {
            format!(
                "browser PDF '{}' cannot be written: {error}",
                path.display()
            )
        })?;
        Ok(json!({
            "result": "browser.pdf",
            "target_id": state.target_id,
            "path": path,
            "content_hash": blake3::hash(&bytes).to_hex().to_string(),
            "bytes": bytes.len(),
            "landscape": landscape,
            "scale": scale,
        }))
    }

    async fn cookies(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object("browser.cookies", &decoded.call.arguments, &["urls"])?;
        let urls = optional_string_array(object, "urls", 100)?;
        for url in &urls {
            validate_navigation_url(url)?;
        }
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.cookies requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let result = session
            .command(
                "browser.cookies",
                "Network.getCookies",
                json!({"urls":urls}),
            )
            .await?;
        let cookies = result
            .get("cookies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(json!({
            "result": "browser.cookies",
            "target_id": state.target_id,
            "cookies": cookies,
            "count": cookies.len(),
        }))
    }

    async fn cookie_set(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.cookie_set",
            &decoded.call.arguments,
            &[
                "name",
                "value",
                "url",
                "domain",
                "path",
                "secure",
                "http_only",
                "same_site",
                "expires",
            ],
        )?;
        let name = required_string(object, "name")?;
        let value = required_string_allow_empty(object, "value")?;
        if name.len() > 4096 || value.len() > 65_536 || name.contains('\0') || value.contains('\0')
        {
            return Err("browser.cookie_set name or value exceeds its boundary".into());
        }
        let url = optional_string(object, "url")?;
        let domain = optional_string(object, "domain")?;
        if url.is_some() == domain.is_some() {
            return Err("browser.cookie_set requires exactly one of url or domain".into());
        }
        if let Some(url) = url.as_deref() {
            validate_navigation_url(url)?;
        }
        let same_site = optional_string(object, "same_site")?;
        if same_site
            .as_deref()
            .is_some_and(|same_site| !matches!(same_site, "Strict" | "Lax" | "None"))
        {
            return Err("browser.cookie_set same_site must be Strict, Lax, or None".into());
        }
        let expires = optional_f64(object, "expires")?;
        let mut params = json!({
            "name": name,
            "value": value,
            "path": optional_string_allow_empty(object, "path")?.unwrap_or_else(|| "/".into()),
            "secure": optional_bool(object, "secure")?.unwrap_or(false),
            "httpOnly": optional_bool(object, "http_only")?.unwrap_or(false),
        });
        if let Some(url) = url {
            params["url"] = json!(url);
        }
        if let Some(domain) = domain {
            params["domain"] = json!(domain);
        }
        if let Some(same_site) = same_site {
            params["sameSite"] = json!(same_site);
        }
        if let Some(expires) = expires {
            params["expires"] = json!(expires);
        }
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.cookie_set requires browser.navigate or browser.tab_new first".to_string()
        })?;
        let result = session
            .command("browser.cookie_set", "Network.setCookie", params)
            .await?;
        if result.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(format!(
                "browser.cookie_set was refused: {}",
                truncate(&result.to_string(), 1000)
            ));
        }
        Ok(
            json!({"result":"browser.cookie_set","target_id":state.target_id,"name":name,"set":true}),
        )
    }

    async fn cookie_delete(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = page_object(
            "browser.cookie_delete",
            &decoded.call.arguments,
            &["name", "url", "domain", "path"],
        )?;
        let name = required_string(object, "name")?;
        let url = optional_string(object, "url")?;
        let domain = optional_string(object, "domain")?;
        if url.is_some() == domain.is_some() {
            return Err("browser.cookie_delete requires exactly one of url or domain".into());
        }
        if let Some(url) = url.as_deref() {
            validate_navigation_url(url)?;
        }
        let mut params = json!({"name":name});
        if let Some(url) = url {
            params["url"] = json!(url);
        }
        if let Some(domain) = domain {
            params["domain"] = json!(domain);
        }
        if let Some(path) = optional_string_allow_empty(object, "path")? {
            params["path"] = json!(path);
        }
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let (_, state) = self.ensure_session(&mut room, false, decoded, recorder).await?;
        let session = state.session.as_mut().ok_or_else(|| {
            "browser.cookie_delete requires browser.navigate or browser.tab_new first".to_string()
        })?;
        session
            .command("browser.cookie_delete", "Network.deleteCookies", params)
            .await?;
        Ok(
            json!({"result":"browser.cookie_delete","target_id":state.target_id,"name":name,"deleted":true}),
        )
    }

    async fn tabs(&self, decoded: &DecodedRoomInvocation) -> Result<Value, String> {
        strict_object("browser.tabs", &decoded.call.arguments, &[])?;
        let room = self.room(&decoded.formation).await?;
        let mut state = room.lock().await;
        self.ensure_browser_context(&mut state, false).await?;
        let targets = self.context_targets(&mut state).await?;
        let current = state.target_id.clone();
        state.tabs = targets.into_iter().map(|target| json!({
            "target_id": target.target_id,
            "title": target.title,
            "url": target.url,
            "opener_id": target.opener_id,
            "browser_context_id": target.browser_context_id,
            "lifetime": target.lifetime,
            "model_controlled": target.model_controlled,
            "attached": current.as_deref() == Some(target.target_id.as_str()),
        })).collect();
        Ok(json!({
            "result": "browser.tabs",
            "worker_environment_id": decoded.formation.worker_environment_id,
            "current_target_id": current,
            "tabs": state.tabs,
        }))
    }

    async fn tab_new(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = strict_object("browser.tab_new", &decoded.call.arguments, &["url"])?;
        let url = optional_string(object, "url")?.unwrap_or_else(|| "about:blank".to_string());
        validate_navigation_url(&url)?;
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let state = &mut *room;
        self.select_history_collection(decoded).await?;
        let previous_context = self.prepare_new_page_context(state).await?;
        self.materialize_target(state, &url, decoded).await?;
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| "browser room generation overflowed".to_string())?;
        self.publish_attached_state(state, &decoded.formation, recorder)
            .await?;
        state.presentation_intent = Some(BrowserPresentationIntent {
            id: decoded.source_movement_seq,
            target_id: state.target_id.clone().ok_or("browser presentation has no target")?,
            source: BrowserPresentationSource::Tool,
            activate: true,
        });
        Ok(json!({
            "result": "browser.tab_new",
            "worker_environment_id": decoded.formation.worker_environment_id,
            "target_id": state.target_id,
            "browser_context_id": state.browser_context_id,
            "host_generation": state.native_identity.as_ref().map(|identity| &identity.host_generation),
            "engine_handle": state.native_identity.as_ref().map(|identity| &identity.engine_handle),
            "previous_context": previous_context,
            "url": state.target_id.as_ref().and_then(|id| state.pages.get(id)).and_then(|page| page.target_url.as_ref()),
            "title": state.target_id.as_ref().and_then(|id| state.pages.get(id)).and_then(|page| page.target_title.as_ref()),
            "generation": state.generation,
        }))
    }

    async fn tab_select(
        &self,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<Value, String> {
        let object = strict_object(
            "browser.tab_select",
            &decoded.call.arguments,
            &["target_id"],
        )?;
        let target_id = required_string(object, "target_id")?;
        validate_target_id(target_id)?;
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let state = &mut *room;
        self.ensure_browser_context(state, false).await?;
        self.require_context_target(state, target_id).await?;
        let target = self.context_target(state, target_id).await?;
        self.browser_command(state, "Target.activateTarget", json!({"targetId":target_id})).await?;
        self.connect_target(state, target, decoded, false, None).await?;
        state.target_id = Some(target_id.to_owned());
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| "browser room generation overflowed".to_string())?;
        self.publish_attached_state(state, &decoded.formation, recorder)
            .await?;
        state.presentation_intent = Some(BrowserPresentationIntent {
            id: decoded.source_movement_seq,
            target_id: state.target_id.clone().ok_or("browser presentation has no target")?,
            source: BrowserPresentationSource::Tool,
            activate: true,
        });
        Ok(json!({
            "result": "browser.tab_select",
            "target_id": state.target_id,
            "url": state.target_id.as_ref().and_then(|id| state.pages.get(id)).and_then(|page| page.target_url.as_ref()),
            "title": state.target_id.as_ref().and_then(|id| state.pages.get(id)).and_then(|page| page.target_title.as_ref()),
            "generation": state.generation,
        }))
    }

    async fn tab_mark(&self, decoded: &DecodedRoomInvocation) -> Result<Value, String> {
        let args = strict_object("browser.tab_mark", &decoded.call.arguments, &["target_id", "status"])?;
        let target_id = required_string(args, "target_id")?;
        validate_target_id(target_id)?;
        let status = required_string(args, "status")?;
        if !matches!(status, "handoff" | "deliverable") {
            return Err("browser.tab_mark status must be handoff or deliverable".into());
        }
        if !matches!(self.transport, BrowserTransport::Native(_)) {
            return Err("browser.tab_mark requires the native room's whole-turn presentation owner".into());
        }
        let room = self.room(&decoded.formation).await?;
        let mut state = room.lock().await;
        self.ensure_browser_context(&mut state, false).await?;
        self.require_context_target(&mut state, target_id).await?;
        let result = self.browser_command(&mut state, "OctopusBrowser.markTarget", json!({"target_id":target_id,"status":status})).await?;
        if result["target_id"].as_str() != Some(target_id)
            || result["browser_context_id"].as_str() != state.browser_context_id.as_deref()
            || result["disposition"]["status"].as_str() != Some(status)
            || result["disposition"]["source_movement_seq"].as_u64() != Some(decoded.source_movement_seq)
        {
            return Err("Native tab disposition does not match its admitted target and movement".into());
        }
        Ok(json!({"result":"browser.tab_mark","target_id":target_id,"browser_context_id":state.browser_context_id,"lifetime":result["lifetime"],"disposition":result["disposition"]}))
    }

    async fn tab_close(&self, decoded: &DecodedRoomInvocation) -> Result<Value, String> {
        let object = strict_object("browser.tab_close", &decoded.call.arguments, &["target_id"])?;
        let target_id = required_string(object, "target_id")?;
        validate_target_id(target_id)?;
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let state = &mut *room;
        self.ensure_browser_context(state, false).await?;
        self.require_context_target(state, target_id).await?;
        self.close_context_target(state, target_id).await?;
        let was_current = state.target_id.as_deref() == Some(target_id);
        state.tabs.retain(|tab| tab["target_id"].as_str() != Some(target_id));
        if state.presentation_intent.as_ref().is_some_and(|intent| intent.target_id == target_id) {
            state.presentation_intent = None;
        }
        if let Some(page) = state.pages.remove(target_id) { if let Some(session) = page.session { session.close().await; } }
        if was_current { state.target_id = None; }
        Ok(json!({
            "result": "browser.tab_close",
            "target_id": target_id,
            "closed": true,
            "was_current": was_current,
        }))
    }

    async fn close(&self, decoded: &DecodedRoomInvocation) -> Result<Value, String> {
        strict_object("browser.close", &decoded.call.arguments, &[])?;
        let room = self.room(&decoded.formation).await?;
        let mut room = room.lock().await;
        let state = &mut *room;
        let target_id = state.target_id.clone().ok_or_else(|| {
            format!(
                "worker environment '{}' has no open browser target",
                decoded.formation.worker_environment_id
            )
        })?;
        self.ensure_browser_context(state, false).await?;
        self.require_context_target(state, &target_id).await?;
        self.close_context_target(state, &target_id).await?;
        state.tabs.retain(|tab| tab["target_id"].as_str() != Some(target_id.as_str()));
        if let Some(page) = state.pages.remove(&target_id) { if let Some(session) = page.session { session.close().await; } }
        state.presentation_intent = None;
        state.target_id = None;
        Ok(json!({
            "result": "browser.close",
            "worker_environment_id": decoded.formation.worker_environment_id,
            "target_id": target_id,
            "closed": true,
        }))
    }

    async fn room(
        &self,
        formation: &WorkerEnvironmentFormation,
    ) -> Result<std::sync::Arc<tokio::sync::Mutex<RoomState>>, String> {
        let mut rooms = self.rooms.lock().await;
        if let Some(room) = rooms.get(&formation.worker_environment_id) {
            return Ok(std::sync::Arc::clone(room));
        }
        let recovered = recover_room_state(&self.state_dir, formation)?;
        let room = std::sync::Arc::new(tokio::sync::Mutex::new(recovered.unwrap_or_else(|| {
            RoomState {
                generation: 0,
                browser_context_id: None,
                native_identity: None,
                browser: None,
                target_id: None,
                pages: BTreeMap::new(),
                execution: std::sync::Arc::new(tokio::sync::Mutex::new(())),
                activity: None,
                presentation_intent: None,
                tabs: Vec::new(),
                viewport: viewport::ViewportState::default(),
            }
        })));
        rooms.insert(
            formation.worker_environment_id.clone(),
            std::sync::Arc::clone(&room),
        );
        Ok(room)
    }

    async fn ensure_session<'a>(
        &self,
        state: &'a mut RoomState,
        create_if_missing: bool,
        decoded: &DecodedRoomInvocation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<(SessionDisposition, &'a mut PageState), String> {
        let requested = requested_target(decoded)?;
        if create_if_missing && requested.is_none() { self.select_history_collection(decoded).await?; }
        self.ensure_browser_context(state, create_if_missing && requested.is_none()).await?;
        let (target_id, disposition) = match requested.or_else(|| state.target_id.clone()) {
            Some(target_id) => {
                let target = self.context_target(state, &target_id).await.map_err(|error| format!("browser target unavailable: exact target '{target_id}' was preserved; {error}; use browser.tab_new only if a new target is intended"))?;
                let disposition = if state.pages.get(&target_id).and_then(|page| page.session.as_ref()).is_some_and(|session| session.is_usable()) {
                    SessionDisposition::Attached
                } else { SessionDisposition::Reconnected };
                self.connect_target(state, target, decoded, false, None).await?;
                (target_id, disposition)
            }
            None if create_if_missing => {
                let target_id = self.materialize_target(state, "about:blank", decoded).await?;
                state.generation = state.generation.checked_add(1).ok_or("browser room generation overflowed")?;
                (target_id, SessionDisposition::Created)
            }
            None => return Err("browser room has no selected target; supply an owned target_id or create a page explicitly".into()),
        };
        self.publish_attached_state(state, &decoded.formation, recorder).await?;
        Ok((disposition, state.pages.get_mut(&target_id).ok_or("browser page connection disappeared")?))
    }

    async fn browser_command(&self, state: &mut RoomState, method: &str, params: Value) -> Result<Value, String> {
        state.browser.as_mut().ok_or("browser context has no control connection")?
            .command("browser.context", method, params).await
    }

    async fn connect_browser_control(&self, state: &mut RoomState) -> Result<(), String> {
        if state.browser.as_ref().is_some_and(|session| !session.is_usable()) {
            if let Some(session) = state.browser.take() { session.close().await; }
        }
        if state.browser.is_none() {
            state.browser = Some(self.transport.connect(&self.limits, None).await?);
        }
        Ok(())
    }

    async fn ensure_browser_context(&self, state: &mut RoomState, create: bool) -> Result<(), String> {
        self.connect_browser_control(state).await?;
        if !Self::browser_generation_matches(state) {
            return Err("native browser host restarted; preserved target was not replaced; a fresh page requires an explicitly granted browser.tab_new invocation".into());
        }
        if let Some(context) = state.browser_context_id.clone() {
            if !self.existing_context_available(state).await? {
                return Err(format!("owned browser context '{context}' is unavailable; preserved room state was not replaced; use browser.tab_new only if a new page is intended and granted"));
            }
        } else if create {
            self.create_browser_context(state).await?;
        } else {
            return Err("browser room has no owned context; call browser.navigate or browser.tab_new first".into());
        }
        Ok(())
    }

    async fn prepare_new_page_context(&self, state: &mut RoomState) -> Result<Option<Value>, String> {
        self.connect_browser_control(state).await?;
        if state.browser_context_id.is_some() && Self::browser_generation_matches(state)
            && self.existing_context_available(state).await? {
            return Ok(None);
        }
        let previous = state.browser_context_id.as_ref().map(|context| json!({
            "browser_context_id": context,
            "target_id": state.target_id,
            "host_generation": state.native_identity.as_ref().map(|identity| &identity.host_generation),
            "engine_handle": state.native_identity.as_ref().map(|identity| &identity.engine_handle),
        }));
        self.create_browser_context(state).await?;
        if previous.is_some() {
            for (_, page) in std::mem::take(&mut state.pages) {
                if let Some(session) = page.session { session.close().await; }
            }
            state.target_id = None;
            state.tabs.clear();
            state.presentation_intent = None;
            state.viewport = viewport::ViewportState::default();
        }
        Ok(previous)
    }

    async fn create_browser_context(&self, state: &mut RoomState) -> Result<(), String> {
        let result = self.browser_command(state, "Target.createBrowserContext", json!({"disposeOnDetach":false})).await?;
        let context = result.get("browserContextId").and_then(Value::as_str)
            .ok_or("Target.createBrowserContext returned no browserContextId")?;
        validate_target_id(context)?;
        if state.browser_context_id.as_deref() == Some(context) {
            return Err("new browser context reused the unavailable context identity".into());
        }
        let identity = if matches!(self.transport, BrowserTransport::Native(_)) {
            let engine_handle = result.get("engine_handle").and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or("native browser context has no engine handle")?;
            let host_generation = result.get("host_generation").and_then(Value::as_str).filter(|value| !value.is_empty()).ok_or("native browser context has no host generation")?;
            let CdpStream::Native(connection) = &state.browser.as_ref().ok_or("native browser has no connection")?.stream else { return Err("native browser stream disagrees with transport".into()); };
            if connection.generation != host_generation { return Err("native browser context changed host generation".into()); }
            Some(NativeIdentity { engine_handle: engine_handle.to_string(), host_generation: host_generation.to_string() })
        } else { None };
        state.native_identity = identity;
        state.browser_context_id = Some(context.to_string());
        Ok(())
    }

    fn browser_generation_matches(state: &RoomState) -> bool {
        match (&state.native_identity, state.browser.as_ref().map(|session| &session.stream)) {
            (Some(identity), Some(CdpStream::Native(connection))) => identity.host_generation == connection.generation,
            _ => true,
        }
    }

    async fn existing_context_available(&self, state: &mut RoomState) -> Result<bool, String> {
        let Some(context) = state.browser_context_id.clone() else { return Ok(false); };
        if !Self::browser_generation_matches(state) { return Ok(false); }
        let result = self.browser_command(state, "Target.getBrowserContexts", json!({})).await?;
        let contexts = result.get("browserContextIds").and_then(Value::as_array)
            .ok_or("Target.getBrowserContexts returned no browserContextIds")?;
        for value in contexts {
            validate_target_id(value.as_str().ok_or("Target.getBrowserContexts returned malformed context identity")?)?;
        }
        if !contexts.iter().any(|value| value.as_str() == Some(context.as_str())) { return Ok(false); }
        self.context_targets(state).await?;
        Ok(true)
    }

    async fn context_targets(&self, state: &mut RoomState) -> Result<Vec<CdpTargetInfo>, String> {
        let context = state.browser_context_id.clone().ok_or("browser room has no owned context")?;
        let result = self.browser_command(state, "Target.getTargets", json!({})).await?;
        let targets = result.get("targetInfos").cloned().ok_or("Target.getTargets returned no targetInfos")?;
        let targets: Vec<CdpTargetInfo> = serde_json::from_value(targets)
            .map_err(|error| format!("Target.getTargets returned malformed targetInfos: {error}"))?;
        let targets: Vec<_> = targets.into_iter().filter(|target| {
            target.target_type == "page" && target.browser_context_id.as_deref() == Some(context.as_str())
        }).collect();
        let mut seen = BTreeSet::new();
        for target in &targets {
            validate_target_id(&target.target_id)?;
            if !seen.insert(target.target_id.clone()) {
                return Err("Target.getTargets returned duplicate room targets".into());
            }
        }
        state.viewport.retain_targets(&targets);
        let removed: Vec<_> = state.pages.keys().filter(|id| state.target_id.as_ref() != Some(*id)
            && !targets.iter().any(|target| &target.target_id == *id)).cloned().collect();
        for id in removed { if let Some(page) = state.pages.remove(&id) { if let Some(session) = page.session { session.close().await; } } }
        for target in &targets { if let Some(page) = state.pages.get_mut(&target.target_id) {
            page.target_url = Some(target.url.clone()); page.target_title = Some(target.title.clone());
        } }
        state.tabs = targets.iter().map(|target| json!({
            "target_id": target.target_id, "browser_context_id": target.browser_context_id,
            "title":target.title, "url":target.url, "opener_id":target.opener_id,
            "attached":state.target_id.as_deref() == Some(target.target_id.as_str()),
        })).collect();
        Ok(targets)
    }

    async fn require_context_target(&self, state: &mut RoomState, target_id: &str) -> Result<(), String> {
        if !self.context_targets(state).await?.iter().any(|target| target.target_id == target_id) {
            return Err(format!("browser target '{target_id}' is unavailable in this room's owned context"));
        }
        Ok(())
    }

    async fn context_target(&self, state: &mut RoomState, target_id: &str) -> Result<CdpTarget, String> {
        let target = self.context_targets(state).await?.into_iter().find(|target| target.target_id == target_id)
            .ok_or_else(|| format!("browser target '{target_id}' is unavailable in this room's owned context"))?;
        match &self.transport {
            BrowserTransport::Cdp(transport) => transport.target(target_id).await,
            BrowserTransport::Native(_) => Ok(CdpTarget { id: target.target_id, target_type: target.target_type, title: target.title, url: target.url, websocket_debugger_url: None }),
        }
    }

    async fn create_context_target(&self, state: &mut RoomState, url: &str) -> Result<CdpTarget, String> {
        let context = state.browser_context_id.clone().ok_or("browser room has no owned context")?;
        let result = self.browser_command(state, "Target.createTarget", json!({"url":url,"browserContextId":context})).await?;
        let target = result.get("targetId").and_then(Value::as_str)
            .ok_or("Target.createTarget returned no targetId")?;
        validate_target_id(target)?;
        self.context_target(state, target).await
    }

    async fn close_context_target(&self, state: &mut RoomState, target_id: &str) -> Result<(), String> {
        let result = self.browser_command(state, "Target.closeTarget", json!({"targetId":target_id})).await?;
        if result.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(format!("Target.closeTarget did not close exact target '{target_id}'"));
        }
        Ok(())
    }

    async fn publish_attached_state(
        &self,
        state: &RoomState,
        formation: &WorkerEnvironmentFormation,
        recorder: &dyn WorkerEnvironmentRecorder,
    ) -> Result<(), String> {
        recorder
            .record(formation, WorkerEnvironmentLifecycle::Active)
            .await?;
        let live = self.live_state(formation, Some(state), presentation_status(state), None);
        self.persist_live_state(formation, &live)?;
        recorder.record_live_state(formation, &live).await
    }

    async fn materialize_target(&self, state: &mut RoomState, url: &str, decoded: &DecodedRoomInvocation) -> Result<String, String> {
        let pending = self.take_external_viewport(state, decoded).await?;
        let bootstrap = pending.is_some();
        let target = self.create_context_target(state, if bootstrap { "about:blank" } else { url }).await?;
        let id = target.id.clone();
        self.connect_target(state, target, decoded, true, pending).await?;
        state.target_id = Some(id.clone());
        if bootstrap && url != "about:blank" {
            let page = state.pages.get_mut(&id).ok_or("new viewport page disappeared")?;
            let result = page.session.as_mut().ok_or("new viewport page has no control connection")?
                .command(&decoded.call.tool_id, "Page.navigate", json!({"url":url})).await?;
            if result.get("errorText").and_then(Value::as_str).is_some_and(|e| !e.is_empty()) { return Err("new viewport page navigation failed".into()); }
            page.target_url = Some(url.into()); page.target_title = None;
        }
        Ok(id)
    }

    async fn connect_target(&self, state: &mut RoomState, target: CdpTarget, decoded: &DecodedRoomInvocation, created: bool, pending: Option<viewport::ViewportIntent>) -> Result<(), String> {
        let id = target.id.clone();
        if state.pages.get(&id).and_then(|page| page.session.as_ref()).is_some_and(|session| session.is_usable()) { return Ok(()); }
        let page = state.pages.entry(id.clone()).or_insert_with(|| PageState {
            target_id: id.clone(), target_url: Some(target.url.clone()), target_title: Some(target.title.clone()),
            session: None, refs: BTreeMap::new(), last_capture: None,
        });
        if let Some(session) = page.session.take() { session.close().await; }
        page.refs.clear();
        page.session = Some(self.transport.connect(&self.limits, Some(&target)).await?);
        self.connect_viewport(state, decoded, &id, created, pending).await?;
        let session = state.pages.get_mut(&id).and_then(|page| page.session.as_mut()).ok_or("browser target lost its control connection")?;
        for method in ["Page.enable", "Runtime.enable", "Log.enable", "Network.enable"] {
            session.command("browser.session", method, json!({})).await?;
        }
        state.tabs.retain(|tab| tab["target_id"].as_str() != Some(id.as_str()));
        state.tabs.push(json!({"target_id": id, "url": target.url, "title": target.title, "attached": state.target_id.as_deref() == Some(id.as_str()), "browser_context_id": state.browser_context_id}));
        Ok(())
    }

    async fn capture_live_state(
        &self,
        formation: &WorkerEnvironmentFormation,
        available: bool,
        detail: Option<String>,
    ) -> Value {
        let room = self
            .rooms
            .lock()
            .await
            .get(&formation.worker_environment_id)
            .cloned();
        let live_state = match room {
            Some(room) => {
                let state = room.lock().await;
                let status = if available {
                    if state.target_id.as_ref().is_some_and(|target| state.tabs.iter().any(|tab| tab["target_id"].as_str() == Some(target.as_str()))) {
                        "ready"
                    } else if state.target_id.is_some() {
                        "error"
                    } else {
                        "closed"
                    }
                } else {
                    "error"
                };
                self.live_state(formation, Some(&state), status, detail)
            }
            None => self.live_state(
                formation,
                None,
                if available { "closed" } else { "error" },
                detail,
            ),
        };
        live_state
    }

    fn live_state(
        &self,
        formation: &WorkerEnvironmentFormation,
        state: Option<&RoomState>,
        status: &str,
        detail: Option<String>,
    ) -> Value {
        let generation = state.map(|state| state.generation).unwrap_or(0);
        let target_id = state.and_then(|state| state.target_id.clone());
        let page = state.and_then(|state| state.target_id.as_ref().and_then(|id| state.pages.get(id)));
        let url = page.and_then(|page| page.target_url.clone());
        let title = page.and_then(|page| page.target_title.clone());
        let mut tabs = state.map(|state| state.tabs.clone()).unwrap_or_default();
        for tab in &mut tabs {
            let attached = target_id
                .as_deref()
                .is_some_and(|target| tab["target_id"].as_str() == Some(target));
            tab["attached"] = json!(attached);
            if attached {
                tab["url"] = json!(url);
                tab["title"] = json!(title);
            }
        }
        let viewer = match &self.transport {
            BrowserTransport::Cdp(transport) => json!({"protocol":"cdp","address":transport.discovery_url.as_str(),"target_id":target_id}),
            BrowserTransport::Native(_) => json!({
                "protocol":"browser.native.v1", "target_id":target_id,
                "engine_handle":state.and_then(|state| state.native_identity.as_ref().map(|value| value.engine_handle.clone())),
                "host_generation":state.and_then(|state| state.native_identity.as_ref().map(|value| value.host_generation.clone())),
                "browser_context_id":state.and_then(|state| state.browser_context_id.clone()),
            }),
        };
        json!({
            "schema_version": 1,
            "worker_environment_id": formation.worker_environment_id,
            "worker_environment_kind": formation.worker_environment_kind,
            "status": status,
            "generation": generation,
            "browser_context_id": state.and_then(|state| state.browser_context_id.clone()),
            "target_id": target_id,
            "url": url,
            "title": title,
            "viewer": viewer,
            "detail": detail,
            "activity": state.and_then(|state| state.activity.clone()),
            "presentation_intent": state.and_then(|state| state.presentation_intent.as_ref()),
            "tabs": tabs,
            "last_capture": page.and_then(|page| page.last_capture.clone()),
            "viewport": state.map(|state| &state.viewport).cloned().unwrap_or_default(),
        })
    }

    fn persist_live_state(
        &self,
        formation: &WorkerEnvironmentFormation,
        live_state: &Value,
    ) -> Result<(), String> {
        let path = room_live_state_path(&self.state_dir, formation);
        let parent = path
            .parent()
            .ok_or_else(|| "browser room live-state path has no parent".to_string())?;
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "browser room state directory '{}' cannot be created: {error}",
                parent.display()
            )
        })?;
        let bytes = serde_json::to_vec(live_state)
            .map_err(|error| format!("browser room live state did not encode: {error}"))?;
        let temporary = path.with_extension("tmp");
        let mut file = std::fs::File::create(&temporary).map_err(|error| {
            format!(
                "browser room live-state temporary file '{}' cannot be created: {error}",
                temporary.display()
            )
        })?;
        file.write_all(&bytes).map_err(|error| {
            format!(
                "browser room live-state temporary file '{}' cannot be written: {error}",
                temporary.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "browser room live-state temporary file '{}' cannot be synchronized: {error}",
                temporary.display()
            )
        })?;
        std::fs::rename(&temporary, &path).map_err(|error| {
            format!(
                "browser room live-state '{}' cannot be committed: {error}",
                path.display()
            )
        })
    }

    async fn release_room(
        &self,
        context: octopus_tool_door_runtime::WorkerEnvironmentReleaseContext<'_>,
    ) -> Result<(), ToolProcessError> {
        let formation = context.formation;
        if let BrowserTransport::Native(transport) = &self.transport {
            transport.release(context).await.map_err(ToolProcessError::Capability)?;
        }
        let room = self.room(formation).await.map_err(ToolProcessError::Capability)?;
        {
            let mut state = room.lock().await;
            if matches!(self.transport, BrowserTransport::Native(_)) { state.browser_context_id = None; }
            if let Some(context) = state.browser_context_id.clone() {
                self.ensure_browser_context(&mut state, false).await.map_err(ToolProcessError::Capability)?;
                self.browser_command(&mut state, "Target.disposeBrowserContext", json!({"browserContextId":context}))
                    .await.map_err(ToolProcessError::Capability)?;
                state.browser_context_id = None;
            }
            if let Some(browser) = state.browser.take() {
                browser.close().await;
            }
            for (_, page) in std::mem::take(&mut state.pages) { if let Some(session) = page.session { session.close().await; } }
            state.target_id = None;
            state.tabs.clear();
            state.presentation_intent = None;
            let live = self.live_state(formation, Some(&state), "closed", None);
            self.persist_live_state(formation, &live).map_err(ToolProcessError::Capability)?;
        }
        self.rooms.lock().await.remove(&formation.worker_environment_id);
        Ok(())
    }
}

impl ToolProcessHandler for BrowserRoom {
    fn substrate_family(&self) -> &str {
        crate::SUBSTRATE_FAMILY
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
            let operation = crate::browser_operation(&invocation.descriptor.handler_config, invocation.descriptor.id.as_str())?;
            let source = invocation.envelope.artifact.clone();
            let source_movement_seq = invocation.source_movement_seq;
            let response = self.receive(invocation, &io).await?;
            if operation != BrowserOperation::DownloadWait { return Ok(vec![response]); }
            self.publish_download_output(response, &source, source_movement_seq, &io)
                .await.map(|artifact| vec![artifact])
        })
    }

    fn release_with_context<'a>(
        &'a self,
        context: octopus_tool_door_runtime::WorkerEnvironmentReleaseContext<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>> {
        Box::pin(async move { self.release_room(context).await })
    }

    fn quiesce<'a>(&'a self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ToolProcessError>> + Send + 'a>> {
        Box::pin(async move { if let BrowserTransport::Native(transport) = &self.transport { transport.shutdown().await; } Ok(()) })
    }
}

impl CdpTransport {
    fn form(config: &CdpConfig) -> Result<Self, String> {

        let env_url = std::env::var(&config.discovery_url_env)
            .ok()
            .filter(|value| !value.trim().is_empty());
        let raw_url = env_url.as_deref().unwrap_or(&config.discovery_url);
        let mut discovery_url = reqwest::Url::parse(raw_url.trim())
            .map_err(|error| format!("browser discovery URL '{raw_url}' is invalid: {error}"))?;
        if !matches!(discovery_url.scheme(), "http" | "https") {
            return Err("browser discovery URL must use http or https".to_string());
        }
        if discovery_url.host_str().is_none()
            || !discovery_url.username().is_empty()
            || discovery_url.password().is_some()
        {
            return Err("browser discovery URL must have a host and no userinfo".to_string());
        }
        if discovery_url.scheme() == "http" && !url_is_loopback(&discovery_url) {
            return Err("browser discovery URL must use https outside loopback".to_string());
        }
        let mut allowed_websocket_hosts = BTreeSet::new();
        if let Some(host) = discovery_url.host_str() {
            allowed_websocket_hosts.insert(host.to_ascii_lowercase());
        }
        for host in &config.allowed_websocket_hosts {
            let normalized = host.trim().to_ascii_lowercase();
            if normalized.is_empty()
                || normalized.contains('/')
                || normalized.contains(':')
                || normalized != host.as_str()
            {
                return Err(format!(
                    "browser allowed websocket host '{host}' is invalid"
                ));
            }
            allowed_websocket_hosts.insert(normalized);
        }
        discovery_url.set_query(None);
        discovery_url.set_fragment(None);
        if !discovery_url.path().ends_with('/') {
            let path = format!("{}/", discovery_url.path().trim_end_matches('/'));
            discovery_url.set_path(&path);
        }
        let mut headers = HeaderMap::new();
        for (name, value) in &config.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| format!("browser header name '{name}' is invalid: {error}"))?;
            let value = HeaderValue::from_str(value)
                .map_err(|error| format!("browser header value is invalid: {error}"))?;
            headers.insert(name, value);
        }
        let auth = std::env::var(&config.auth_env_key)
            .ok()
            .filter(|value| !value.trim().is_empty());
        if config.auth_required && auth.is_none() {
            return Err(format!(
                "browser auth env var '{}' is required but unset/empty",
                config.auth_env_key
            ));
        }
        if let Some(secret) = auth {
            let name = if config.auth_header.trim().is_empty() {
                DEFAULT_AUTH_HEADER
            } else {
                config.auth_header.trim()
            };
            let prefix = if config.auth_value_prefix.is_empty() {
                DEFAULT_AUTH_VALUE_PREFIX
            } else {
                &config.auth_value_prefix
            };
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|error| format!("browser auth header name is invalid: {error}"))?,
                HeaderValue::from_str(&format!("{prefix}{secret}"))
                    .map_err(|error| format!("browser auth header value is invalid: {error}"))?,
            );
        }
        if config.request_timeout_ms == 0 || config.request_timeout_ms > 300_000 {
            return Err("browser request_timeout_ms must be between 1 and 300000".to_string());
        }
        if config.navigation_timeout_ms == 0 || config.navigation_timeout_ms > 300_000 {
            return Err("browser navigation_timeout_ms must be between 1 and 300000".to_string());
        }
        if config.max_control_response_bytes == 0 || config.max_control_response_bytes > 67_108_864
        {
            return Err(
                "browser max_control_response_bytes must be between 1 and 67108864".to_string(),
            );
        }
        let request_timeout = Duration::from_millis(config.request_timeout_ms);
        let http = reqwest::Client::builder()
            .no_proxy()
            .default_headers(headers.clone())
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("browser CDP HTTP client failed to form: {error}"))?;
        Ok(Self {
            discovery_url,
            headers,
            allowed_websocket_hosts,
            request_timeout,
            navigation_timeout: Duration::from_millis(config.navigation_timeout_ms),
            max_control_response_bytes: config.max_control_response_bytes,
            http,
        })
    }

    async fn list_targets(&self) -> Result<Vec<CdpTarget>, String> {
        let url = self.endpoint("json/list")?;
        let response = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|error| format!("browser CDP target list '{url}' failed: {error}"))?;
        let status = response.status();
        let bytes = read_bounded_http_body(
            response,
            self.max_control_response_bytes,
            "browser CDP target list",
        )
        .await?;
        if !status.is_success() {
            return Err(format!(
                "browser CDP target list returned HTTP {}; {}",
                status.as_u16(),
                safe_control_body_evidence(&bytes)
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("browser CDP target list is malformed: {error}"))
    }

    async fn target(&self, target_id: &str) -> Result<CdpTarget, String> {
        validate_target_id(target_id)?;
        self.list_targets()
            .await?
            .into_iter()
            .find(|target| target.id == target_id && target.target_type == "page")
            .ok_or_else(|| format!("browser CDP page target '{target_id}' is unavailable"))
    }

    async fn browser_websocket(&self) -> Result<String, String> {
        let url = self.endpoint("json/version")?;
        let response = self.http.get(url.clone()).send().await
            .map_err(|error| format!("browser CDP discovery '{url}' failed: {error}"))?;
        let status = response.status();
        let bytes = read_bounded_http_body(response, self.max_control_response_bytes, "browser CDP discovery").await?;
        if !status.is_success() {
            return Err(format!("browser CDP discovery returned HTTP {}; {}", status.as_u16(), safe_control_body_evidence(&bytes)));
        }
        let version: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("browser CDP discovery is malformed: {error}"))?;
        version.get("webSocketDebuggerUrl").and_then(Value::as_str).map(str::to_string)
            .ok_or_else(|| "browser CDP discovery has no browser webSocketDebuggerUrl".into())
    }

    fn endpoint(&self, relative: &str) -> Result<reqwest::Url, String> {
        self.discovery_url
            .join(relative)
            .map_err(|error| format!("browser CDP endpoint '{relative}' is invalid: {error}"))
    }
}

impl CdpSession {
    fn is_usable(&self) -> bool { self.usable && match &self.stream { CdpStream::Native(connection) => connection.is_current_owner(), CdpStream::WebSocket(_) => true } }

    async fn move_cursor(&mut self, tool: &str, x: f64, y: f64, wait_for_arrival: bool) -> Result<(), String> {
        if !matches!(self.stream, CdpStream::Native(_)) { return Ok(()); }
        let result = self.command(tool, "OctopusBrowser.moveCursor", json!({"x":x,"y":y,"waitForArrival":wait_for_arrival})).await?;
        if result["target_id"].as_str() != Some(self.target_id.as_str())
            || result["move_sequence"].as_u64().is_none_or(|value| value == 0)
            || !matches!(result["status"].as_str(), Some("arrived" | "timeout" | "unpresented" | "moving" | "superseded" | "withdrawn")) {
            return Err("browser cursor accompaniment returned an invalid target or outcome".into());
        }
        Ok(())
    }

    async fn connect(
        transport: &CdpTransport,
        limits: &BrowserLimits,
        target_id: &str,
        websocket: &str,
    ) -> Result<Self, String> {
        let websocket_url = reqwest::Url::parse(websocket)
            .map_err(|error| format!("browser CDP websocket is invalid: {error}"))?;
        if !matches!(websocket_url.scheme(), "ws" | "wss")
            || websocket_url.host_str().is_none()
            || !websocket_url.username().is_empty()
            || websocket_url.password().is_some()
        {
            return Err(
                "browser CDP websocket must be an absolute ws/wss URL without userinfo".to_string(),
            );
        }
        let loopback = url_is_loopback(&websocket_url);
        if websocket_url.scheme() == "ws" && !loopback {
            return Err("browser CDP websocket must use wss outside loopback".to_string());
        }
        let websocket_host = websocket_url
            .host_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let discovery_loopback = url_is_loopback(&transport.discovery_url);
        if !transport.allowed_websocket_hosts.contains(&websocket_host)
            && !(discovery_loopback && loopback)
        {
            return Err(format!(
                "browser CDP websocket host '{websocket_host}' is not declared by the room transport"
            ));
        }
        let public_websocket = public_url(&websocket_url);
        let mut request = websocket.into_client_request().map_err(|error| {
            format!("browser CDP websocket '{public_websocket}' is invalid: {error}")
        })?;
        for (name, value) in &transport.headers {
            request.headers_mut().insert(name.clone(), value.clone());
        }
        let (stream, _) = timeout(transport.request_timeout, connect_async(request))
            .await
            .map_err(|_| format!("browser CDP websocket '{public_websocket}' connect timed out"))?
            .map_err(|error| {
                format!("browser CDP websocket '{public_websocket}' connect failed: {error}")
            })?;
        Ok(Self {
            target_id: target_id.to_string(),
            stream: CdpStream::WebSocket(stream),
            usable: true,
            next_id: AtomicI64::new(1),
            request_timeout: transport.request_timeout,
            navigation_timeout: transport.navigation_timeout,
            observation: ObservationState::default(),
                    frames: actions::FrameState::default(),
            events: VecDeque::new(),
            event_bytes: 0,
            dropped_events: 0,
            next_event_id: 1,
            max_event_count: limits.max_event_count,
            max_event_bytes: limits.max_event_bytes,
        })
    }

    async fn command(&mut self, tool: &str, method: &str, params: Value) -> Result<Value, String> {
        self.command_in(tool, None, method, params).await
    }

    async fn command_in(&mut self, tool: &str, route: Option<&str>, method: &str, params: Value) -> Result<Value, String> {
        if !self.is_usable() { return Err("browser control connection is closed; prior sent effects are not replayed".into()); }
        let attachment = self.frames.route(route)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let deadline = Instant::now() + self.request_timeout;
        let mut received_response = false;
        let outcome = async {
            self.stream.send(id, route, method, params).await?;
            loop {
                let parsed = self.stream.next().await?;
                if let Some(response_id) = parsed.get("id") {
                    if response_id.as_i64() != Some(id) { return Err("browser response changed request identity".to_string()); }
                    let response_route = match parsed.get("sessionId") {
                        None | Some(Value::Null) => None,
                        Some(Value::String(id)) if !id.is_empty() => Some(id.as_str()),
                        _ => return Err("browser response has a malformed iframe route".into()),
                    };
                    if response_route != route { return Err("browser response changed its iframe route".into()); }
                    if let Some(error) = parsed.get("error") {
                        received_response = error.get("code").and_then(Value::as_str) != Some("execution_uncertain");
                        return Err(format!("{tool} command '{method}' failed; {}", safe_control_body_evidence(&serde_json::to_vec(error).unwrap_or_default())));
                    }
                    if matches!(self.stream, CdpStream::Native(_)) && self.target_id != "browser" {
                        self.frames.inventory(&self.target_id, parsed.get("frame_inventory").ok_or("native response has no frame inventory")?)?;
                    }
                    if self.frames.route(route)? != attachment { return Err("browser response belongs to a stale iframe attachment; no effect was replayed".into()); }
                    let result = parsed.get("result").cloned().ok_or_else(|| "browser response has no result".to_string())?;
                    received_response = true;
                    return Ok(result);
                }
                self.record_event(parsed)?;
            }
        };
        let result = match tokio::time::timeout_at(deadline, outcome).await {
            Ok(result) => result,
            Err(_) => Err(format!("{tool} command '{method}' timed out; a sent effect may have executed and was not replayed")),
        };
        if result.is_err() && !received_response {
            self.usable = false;
            self.stream.close().await;
        }
        result
    }

    async fn pump_events(&mut self, wait: Duration) -> Result<(), String> {
        let deadline = Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, self.stream.next()).await {
                Err(_) => return Ok(()),
                Ok(Err(error)) => {
                    self.usable = false;
                    self.stream.close().await;
                    return Err(error);
                }
                Ok(Ok(value)) => if let Err(error) = self.record_event(value) { self.usable=false; self.stream.close().await; return Err(error); },
            }
        }
    }

    fn record_event(&mut self, value: Value) -> Result<(), String> {
        self.frames.record(&value, matches!(self.stream, CdpStream::Native(_)))?;
        if value.get("sessionId").and_then(Value::as_str).is_none() { self.observation.record(&value); }
        else if matches!(value["method"].as_str(), Some("Accessibility.nodesUpdated" | "Accessibility.loadComplete")) { self.observation.ax_updated(); }
        if value.get("method").and_then(Value::as_str).is_none() {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&value)
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        if bytes == 0 || bytes > self.max_event_bytes {
            self.dropped_events = self.dropped_events.saturating_add(1);
            return Ok(());
        }
        while self.events.len() >= self.max_event_count
            || self.event_bytes.saturating_add(bytes) > self.max_event_bytes
        {
            let Some(dropped) = self.events.pop_front() else {
                break;
            };
            self.event_bytes = self.event_bytes.saturating_sub(dropped.bytes);
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
        let event = CdpEvent {
            id: self.next_event_id,
            bytes,
            value,
        };
        self.next_event_id = self.next_event_id.saturating_add(1);
        self.event_bytes = self.event_bytes.saturating_add(bytes);
        self.events.push_back(event);
        Ok(())
    }

    fn take_events(&mut self, methods: &[&str], limit: usize) -> Vec<CdpEvent> {
        let mut selected = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(event) = self.events.pop_front() {
            let matches = event
                .value
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|method| methods.contains(&method));
            if matches && selected.len() < limit {
                self.event_bytes = self.event_bytes.saturating_sub(event.bytes);
                selected.push(event);
            } else if matches {
                self.event_bytes = self.event_bytes.saturating_sub(event.bytes);
            } else {
                retained.push_back(event);
            }
        }
        self.events = retained;
        selected
    }

    fn pending_events(&self, method: &str, limit: usize) -> Vec<&CdpEvent> {
        self.events
            .iter()
            .filter(|event| event.value.get("method").and_then(Value::as_str) == Some(method))
            .take(limit)
            .collect()
    }

    fn take_event_by_id(&mut self, event_id: u64) -> Option<CdpEvent> {
        let index = self.events.iter().position(|event| event.id == event_id)?;
        let event = self.events.remove(index)?;
        self.event_bytes = self.event_bytes.saturating_sub(event.bytes);
        Some(event)
    }

    async fn close(mut self) {
        self.stream.close().await;
    }
}

fn load_browser_config(environment: &ToolProcessEnvironment) -> Result<BrowserConfig, String> {
    let descriptor = environment.resolved_interface_descriptor()?;
    let value = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("browser_room"))
        .cloned()
        .ok_or_else(|| {
            format!(
                "browser interface '{}' declares no interface_driver_config.private_config.browser_room",
                environment.interface
            )
        })?;
    let config: BrowserConfig = serde_json::from_value(value).map_err(|error| {
        format!(
            "browser interface '{}' room config is malformed: {error}",
            environment.interface
        )
    })?;
    if config.schema_version != 1 {
        return Err(format!(
            "browser interface '{}' room schema_version is {}, expected 1",
            environment.interface, config.schema_version
        ));
    }
    Ok(config)
}

fn validate_limits(limits: &BrowserLimits) -> Result<(), String> {
    if !(1..=20_000).contains(&limits.max_snapshot_nodes) {
        return Err("browser max_snapshot_nodes must be between 1 and 20000".to_string());
    }
    if !(1..=2_000_000).contains(&limits.max_snapshot_chars) {
        return Err("browser max_snapshot_chars must be between 1 and 2000000".to_string());
    }
    if !(1..=1_048_576).contains(&limits.max_type_chars) {
        return Err("browser max_type_chars must be between 1 and 1048576".to_string());
    }
    if !(1..=1_048_576).contains(&limits.max_eval_chars) {
        return Err("browser max_eval_chars must be between 1 and 1048576".to_string());
    }
    if !(1..=100_000_000).contains(&limits.max_screenshot_bytes) {
        return Err("browser max_screenshot_bytes must be between 1 and 100000000".to_string());
    }
    if !(1..=100_000_000).contains(&limits.max_pdf_bytes) {
        return Err("browser max_pdf_bytes must be between 1 and 100000000".to_string());
    }
    if !(1..=100_000).contains(&limits.max_event_count) {
        return Err("browser max_event_count must be between 1 and 100000".to_string());
    }
    if !(1024..=100_000_000).contains(&limits.max_event_bytes) {
        return Err("browser max_event_bytes must be between 1024 and 100000000".to_string());
    }
    if !(1..=100_000).contains(&limits.max_console_entries) {
        return Err("browser max_console_entries must be between 1 and 100000".to_string());
    }
    if !(1..=100_000).contains(&limits.max_images) {
        return Err("browser max_images must be between 1 and 100000".to_string());
    }
    Ok(())
}

async fn page_identity(
    session: &mut CdpSession,
) -> Result<(Option<String>, Option<String>), String> {
    let result = session
        .command(
            "browser.state",
            "Runtime.evaluate",
            json!({
                "expression": "({url: location.href, title: document.title})",
                "returnByValue": true
            }),
        )
        .await?;
    let value = result
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(Value::as_object)
        .ok_or_else(|| "browser state evaluation returned no value object".to_string())?;
    let url = value.get("url").and_then(Value::as_str).map(str::to_string);
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok((url, title))
}

fn snapshot_identity(snapshot: &Value) -> Result<(Option<String>, Option<String>), String> {
    let field = |name: &str| match snapshot.get(name) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) => Ok(None),
        _ => Err(format!("browser snapshot has malformed {name}")),
    };
    Ok((field("url")?, field("title")?))
}

async fn snapshot_page(
    session: &mut CdpSession,
    limits: &BrowserLimits,
    requested_max_nodes: Option<usize>,
    full: bool,
    source_movement_seq: u64,
) -> Result<(Value, BTreeMap<String, BrowserRef>), String> {
    if source_movement_seq == 0 { return Err("browser snapshot requires a positive source movement sequence".into()); }
    session
        .command("browser.snapshot", "Accessibility.enable", json!({}))
        .await?;
    session
        .command("browser.snapshot", "DOM.enable", json!({}))
        .await?;
    let mut candidate = None;
    for _ in 0..3 {
        let document = settle_observation(session).await?;
        let revision = session.observation.document_revision;
        let result = actions::capture_frames(session).await?;
        let identity = page_identity(session).await?;
        let after = current_document(session).await?;
        let frames_current = actions::validate_frames(session,&result.documents).await;
        if let Err(error)=frames_current.as_ref() { if !session.usable { return Err(error.clone()); } }
        if document == after && revision == session.observation.document_revision && frames_current.is_ok() {
            candidate = Some((result, document, revision, identity));
            break;
        }
    }
    let (result, document, document_revision, (url, title)) = candidate
        .ok_or("browser snapshot document changed before capture commit; request a fresh snapshot")?;
    let nodes = actions::ordered_ax_nodes(&result.nodes)?;
    let frame_warnings = &result.warnings;
    let max_nodes = requested_max_nodes
        .unwrap_or(limits.max_snapshot_nodes)
        .min(limits.max_snapshot_nodes);
    let depths = ax_depths(&nodes);
    let mut refs = BTreeMap::new();
    let mut ref_rows = Vec::new();
    let mut lines = String::new();
    let mut emitted_nodes = 0usize;
    let mut truncated_nodes = false;
    let mut truncated_chars = false;
    for node in &nodes {
        if node
            .get("ignored")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let node_id = node
            .get("nodeId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let role = ax_value(node, "role");
        let name = ax_value(node, "name");
        let value = ax_value(node, "value");
        let interactive = interactive_role(role);
        if !full && !interactive && !compact_role(role) {
            continue;
        }
        if name.is_empty() && value.is_empty() && !interactive {
            continue;
        }
        if emitted_nodes >= max_nodes {
            truncated_nodes = true;
            break;
        }
        let backend_node_id = node.get("backendDOMNodeId").and_then(Value::as_i64);
        let reference = if interactive {
            backend_node_id.map(|_| format!("@m{source_movement_seq}e{}", refs.len() + 1))
        } else {
            None
        };
        let depth = depths.get(node_id).copied().unwrap_or(0).min(24);
        let mut line = String::new();
        line.push_str(&"  ".repeat(depth));
        if let Some(reference) = reference.as_deref() {
            line.push('[');
            line.push_str(reference);
            line.push_str("] ");
        }
        line.push_str(if role.is_empty() { "node" } else { role });
        if !name.is_empty() {
            line.push_str(" \"");
            line.push_str(&sanitize_ax_text(name, 300));
            line.push('"');
        }
        if !value.is_empty() {
            line.push_str(" value=\"");
            line.push_str(&sanitize_ax_text(value, 300));
            line.push('"');
        }
        for property in ["checked", "disabled", "expanded", "focused", "selected"] {
            if let Some(value) = ax_property(node, property) {
                line.push(' ');
                line.push_str(property);
                line.push('=');
                line.push_str(&sanitize_ax_text(value, 64));
            }
        }
        line.push('\n');
        if lines.len().saturating_add(line.len()) > limits.max_snapshot_chars {
            truncated_chars = true;
            break;
        }
        lines.push_str(&line);
        emitted_nodes += 1;
        if let (Some(reference), Some(backend_node_id)) = (reference, backend_node_id) {
            let frame_path = result.provenance.get(node_id).ok_or("snapshot node lost its frame provenance")?.clone();
            let frame = frame_path.last().ok_or("snapshot node has no frame document")?;
            let ref_document = frame.document.clone();
            refs.insert(reference.clone(), BrowserRef { backend_node_id, target_id:session.target_id.clone(), frame_path });
            ref_rows.push(json!({
                "ref": reference,
                "backend_node_id": backend_node_id,
                "frame_id": ref_document.frame_id,
                "loader_id": ref_document.loader_id,
                "role": role,
                "name": name,
                "value": value,
            }));
        }
    }
    let ref_count = refs.len();
    Ok((
        json!({
            "source_movement_seq": source_movement_seq,
            "target_id": session.target_id,
            "frame_id": document.frame_id,
            "loader_id": document.loader_id,
            "document_revision": document_revision,
            "frame_scope": "attached_frames",
            "frames": result.documents.iter().map(|frame| json!({"frame_id":frame.document.frame_id,"loader_id":frame.document.loader_id})).collect::<Vec<_>>(),
            "frame_warnings": frame_warnings,
            "frame_capture_complete": frame_warnings.is_empty(),
            "url": url,
            "title": title,
            "text": lines,
            "refs": ref_rows,
            "node_count": emitted_nodes,
            "ref_count": ref_count,
            "truncated": truncated_nodes || truncated_chars,
            "truncated_by_nodes": truncated_nodes,
            "truncated_by_chars": truncated_chars,
            "full": full,
        }),
        refs,
    ))
}

fn ax_depths(nodes: &[Value]) -> BTreeMap<String, usize> {
    let mut children = BTreeMap::<String, Vec<String>>::new();
    let mut all_children = BTreeSet::new();
    for node in nodes {
        let Some(id) = node.get("nodeId").and_then(Value::as_str) else {
            continue;
        };
        let child_ids = node
            .get("childIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>();
        all_children.extend(child_ids.iter().cloned());
        children.insert(id.to_string(), child_ids);
    }
    let mut queue = VecDeque::new();
    for id in children.keys().filter(|id| !all_children.contains(*id)) {
        queue.push_back((id.clone(), 0usize));
    }
    let mut depths = BTreeMap::new();
    while let Some((id, depth)) = queue.pop_front() {
        if depths.contains_key(&id) {
            continue;
        }
        depths.insert(id.clone(), depth);
        if let Some(child_ids) = children.get(&id) {
            for child in child_ids {
                queue.push_back((child.clone(), depth.saturating_add(1)));
            }
        }
    }
    depths
}

fn ax_value<'a>(node: &'a Value, field: &str) -> &'a str {
    node.get(field)
        .and_then(|value| value.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn ax_property<'a>(node: &'a Value, name: &str) -> Option<&'a str> {
    node.get("properties")
        .and_then(Value::as_array)?
        .iter()
        .find(|property| property.get("name").and_then(Value::as_str) == Some(name))?
        .get("value")?
        .get("value")
        .and_then(|value| match value {
            Value::String(value) => Some(value.as_str()),
            Value::Bool(true) => Some("true"),
            Value::Bool(false) => Some("false"),
            _ => None,
        })
}

fn interactive_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "checkbox"
            | "combobox"
            | "link"
            | "listbox"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "option"
            | "radio"
            | "searchbox"
            | "slider"
            | "spinbutton"
            | "switch"
            | "tab"
            | "textbox"
            | "treeitem"
    )
}

fn compact_role(role: &str) -> bool {
    matches!(
        role,
        "heading"
            | "paragraph"
            | "StaticText"
            | "status"
            | "alert"
            | "img"
            | "table"
            | "row"
            | "cell"
            | "navigation"
            | "main"
            | "article"
            | "dialog"
    )
}

async fn evaluate_boolean(
    session: &mut CdpSession,
    tool: &str,
    expression: &str,
) -> Result<bool, String> {
    let result = session
        .command(
            tool,
            "Runtime.evaluate",
            json!({"expression":expression,"returnByValue":true,"awaitPromise":true}),
        )
        .await?;
    if let Some(exception) = result.get("exceptionDetails") {
        return Err(format!(
            "{tool} expression threw: {}",
            truncate(&exception.to_string(), 1000)
        ));
    }
    result
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{tool} expression did not return a boolean"))
}

fn console_event_value(event: &CdpEvent) -> Value {
    let method = event
        .value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = event.value.get("params").cloned().unwrap_or(Value::Null);
    let (level, text, timestamp, stack) = match method {
        "Runtime.consoleAPICalled" => {
            let level = params.get("type").and_then(Value::as_str).unwrap_or("log");
            let text = params
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .map(remote_object_text)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            (
                level.to_string(),
                text,
                params.get("timestamp").cloned().unwrap_or(Value::Null),
                params.get("stackTrace").cloned().unwrap_or(Value::Null),
            )
        }
        "Runtime.exceptionThrown" => {
            let details = params
                .get("exceptionDetails")
                .cloned()
                .unwrap_or(Value::Null);
            let text = details
                .get("exception")
                .map(remote_object_text)
                .filter(|text| !text.is_empty())
                .or_else(|| {
                    details
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "JavaScript exception".into());
            (
                "exception".into(),
                text,
                params.get("timestamp").cloned().unwrap_or(Value::Null),
                details.get("stackTrace").cloned().unwrap_or(Value::Null),
            )
        }
        "Log.entryAdded" => {
            let entry = params.get("entry").cloned().unwrap_or(Value::Null);
            (
                entry
                    .get("level")
                    .and_then(Value::as_str)
                    .unwrap_or("log")
                    .to_string(),
                entry
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry.get("timestamp").cloned().unwrap_or(Value::Null),
                entry.get("stackTrace").cloned().unwrap_or(Value::Null),
            )
        }
        "Network.loadingFailed" => (
            "network".into(),
            params
                .get("errorText")
                .and_then(Value::as_str)
                .unwrap_or("network loading failed")
                .to_string(),
            params.get("timestamp").cloned().unwrap_or(Value::Null),
            Value::Null,
        ),
        _ => ("event".into(), String::new(), Value::Null, Value::Null),
    };
    json!({
        "id": format!("cdp-{}", event.id),
        "method": method,
        "level": level,
        "text": truncate(&text, 20_000),
        "timestamp": timestamp,
        "stack": stack,
    })
}

fn remote_object_text(value: &Value) -> String {
    value
        .get("value")
        .map(|value| match value {
            Value::String(value) => value.clone(),
            other => other.to_string(),
        })
        .or_else(|| {
            value
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            value
                .get("unserializableValue")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn dialog_event_value(event: &CdpEvent) -> Value {
    let params = event.value.get("params").cloned().unwrap_or(Value::Null);
    json!({
        "id": format!("cdp-{}", event.id),
        "type": params.get("type").cloned().unwrap_or(Value::Null),
        "message": params.get("message").cloned().unwrap_or(Value::Null),
        "default_prompt": params.get("defaultPrompt").cloned().unwrap_or(Value::Null),
        "url": params.get("url").cloned().unwrap_or(Value::Null),
        "has_browser_handler": params.get("hasBrowserHandler").cloned().unwrap_or(Value::Null),
    })
}

fn pending_dialog_values(session: &CdpSession) -> Vec<Value> {
    session
        .pending_events("Page.javascriptDialogOpening", 100)
        .into_iter()
        .map(dialog_event_value)
        .collect()
}

fn parse_cdp_event_id(value: &str) -> Result<u64, String> {
    value
        .strip_prefix("cdp-")
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "browser dialog_id must match cdp-<positive integer>".to_string())
}

async fn dispatch_key(
    session: &mut CdpSession,
    tool: &str,
    key: &str,
    modifiers: u64,
    text: Option<&str>,
) -> Result<(), String> {
    dispatch_key_in(session, tool, None, key, modifiers, text).await
}

async fn dispatch_key_in(session: &mut CdpSession, tool: &str, route: Option<&str>, key: &str, modifiers: u64, text: Option<&str>) -> Result<(), String> {
    let mut down = json!({
        "type": "keyDown",
        "key": key,
        "modifiers": modifiers
    });
    if let Some(text) = text {
        down["text"] = json!(text);
    }
    session
        .command_in(tool, route, "Input.dispatchKeyEvent", down)
        .await?;
    session
        .command_in(
            tool, route,
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": key, "modifiers": modifiers }),
        )
        .await?;
    Ok(())
}

fn recover_room_state(
    process_var_dir: &std::path::Path,
    formation: &WorkerEnvironmentFormation,
) -> Result<Option<RoomState>, String> {
    let path = room_live_state_path(process_var_dir, formation);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "browser live state '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    let mut live: BrowserLiveState = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "browser live state '{}' is malformed: {error}",
            path.display()
        )
    })?;
    if live.schema_version != 1 {
        return Err(format!(
            "browser live state '{}' has schema_version {}, expected 1",
            path.display(),
            live.schema_version
        ));
    }
    if live.worker_environment_id != formation.worker_environment_id
        || live.worker_environment_kind != formation.worker_environment_kind
    {
        return Err(format!(
            "browser live state '{}' belongs to a different worker environment formation",
            path.display()
        ));
    }
    if !matches!(live.status.as_str(), "ready" | "error" | "closed") {
        return Err(format!(
            "browser live state '{}' has unsupported status '{}'",
            path.display(),
            live.status
        ));
    }
    if !matches!(live.viewer.protocol.as_str(), "cdp" | "browser.native.v1")
        || (live.viewer.protocol == CDP_WIRE && live.viewer.address.trim().is_empty()) {
        return Err(format!(
            "browser live state '{}' has an invalid viewer contract",
            path.display()
        ));
    }
    if live.viewer.target_id != live.target_id {
        return Err(format!(
            "browser live state '{}' disagrees about its target id",
            path.display()
        ));
    }
    if live.status == "closed" && live.target_id.is_some() {
        return Err(format!(
            "browser live state '{}' is closed but still names a target",
            path.display()
        ));
    }
    if let Some(context) = live.browser_context_id.as_deref() {
        validate_target_id(context)?;
    } else if live.target_id.is_some() || !live.tabs.is_empty() {
        return Err("browser live state names targets without an owned browser context".into());
    }
    if let Some(target) = live.target_id.as_deref() {
        validate_target_id(target)?;
    }
    if live.detail.as_deref().is_some_and(str::is_empty) {
        return Err(format!(
            "browser live state '{}' carries an empty detail",
            path.display()
        ));
    }
    if let Some(intent) = &live.presentation_intent {
        validate_target_id(&intent.target_id)?;
        if intent.id == 0 || intent.id > 9_007_199_254_740_991 || !intent.activate {
            return Err("browser live state has invalid presentation intent".into());
        }
    }
    let native_identity = if live.viewer.protocol == "browser.native.v1" && live.browser_context_id.is_some() {
        if live.viewer.browser_context_id != live.browser_context_id { return Err("native viewer context disagrees with its room".into()); }
        Some(NativeIdentity {
            engine_handle: live.viewer.engine_handle.filter(|value| !value.is_empty()).ok_or("native browser recovery has no engine handle")?,
            host_generation: live.viewer.host_generation.filter(|value| !value.is_empty()).ok_or("native browser recovery has no host generation")?,
        })
    } else { None };
    live.viewport.recover(formation)?;
    let mut pages = BTreeMap::new();
    if let Some(id) = &live.target_id {
        pages.insert(id.clone(), PageState { target_id: id.clone(), target_url: live.url, target_title: live.title,
            session: None, refs: BTreeMap::new(), last_capture: live.last_capture });
    }
    Ok(Some(RoomState {
        generation: live.generation,
        viewport: live.viewport,
        native_identity,
        browser_context_id: live.browser_context_id,
        browser: None,
        target_id: live.target_id,
        pages,
        execution: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        activity: live.activity.map(|mut activity| {
            if activity["status"] == "running" { activity["status"] = json!("interrupted"); }
            activity
        }),
        presentation_intent: live.presentation_intent,
        tabs: live.tabs,
    }))
}

fn room_live_state_path(
    process_var_dir: &std::path::Path,
    formation: &WorkerEnvironmentFormation,
) -> std::path::PathBuf {
    process_var_dir
        .join("worker-environments")
        .join(formation.worker_environment_id.as_str())
        .join("live-state.json")
}

fn presentation_status(state: &RoomState) -> &'static str {
    match &state.target_id {
        Some(id) if state.tabs.iter().any(|tab| tab["target_id"].as_str() == Some(id.as_str())) => "ready",
        Some(_) => "error",
        None => "closed",
    }
}

fn requested_target(decoded: &DecodedRoomInvocation) -> Result<Option<String>, String> {
    let object = decoded.call.arguments.as_object().ok_or("browser page arguments must be an object")?;
    let target = optional_string(object, "target_id")?;
    if let Some(target) = &target { validate_target_id(target)?; }
    Ok(target)
}

fn page_modifiers(object: &Map<String, Value>) -> Result<u64, String> {
    let values = optional_string_array(object, "modifiers", 16)?;
    let mut mask = 0;
    for value in values { mask |= match value.as_str() {
        "Alt" => 1, "Control" => 2, "Meta" | "ControlOrMeta" => 4, "Shift" => 8,
        _ => return Err("Unknown browser page modifier".into()),
    }; }
    Ok(mask)
}

fn page_object<'a>(tool: &str, value: &'a Value, fields: &[&str]) -> Result<&'a Map<String, Value>, String> {
    let mut fields = fields.to_vec(); fields.push("target_id");
    strict_object(tool, value, &fields)
}

fn validate_navigation_url(value: &str) -> Result<(), String> {
    if value == "about:blank" {
        return Ok(());
    }
    let parsed = reqwest::Url::parse(value)
        .map_err(|error| format!("browser URL '{value}' is invalid: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "browser URL scheme '{}' is not allowed by this room",
            parsed.scheme()
        ));
    }
    Ok(())
}

async fn read_bounded_http_body(
    mut response: reqwest::Response,
    maximum: usize,
    surface: &str,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(format!("{surface} response exceeds {maximum} bytes"));
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(maximum),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("{surface} body read failed: {error}"))?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| format!("{surface} response length overflowed"))?;
        if next_len > maximum {
            return Err(format!("{surface} response exceeds {maximum} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) fn safe_control_body_evidence(bytes: &[u8]) -> String {
    format!(
        "body_bytes={}, body_blake3={}",
        bytes.len(),
        blake3::hash(bytes).to_hex()
    )
}

pub fn url_is_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host.to_ascii_lowercase().ends_with(".localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn public_url(url: &reqwest::Url) -> String {
    let mut public = url.clone();
    public.set_query(None);
    public.set_fragment(None);
    public.to_string()
}

fn validate_target_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("browser target id '{value}' is invalid"));
    }
    Ok(())
}

pub fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}

fn required_string_allow_empty<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{field} must be a string"))
}

fn optional_string(object: &Map<String, Value>, field: &str) -> Result<Option<String>, String> {
    object
        .get(field)
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .ok_or_else(|| format!("{field} must be a non-empty string"))
        })
        .transpose()
}

fn optional_string_allow_empty(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<String>, String> {
    object
        .get(field)
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{field} must be a string"))
        })
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

fn required_string_array(
    object: &Map<String, Value>,
    field: &str,
    minimum: usize,
    maximum: usize,
) -> Result<Vec<String>, String> {
    let values = object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{field} must be an array"))?;
    if !(minimum..=maximum).contains(&values.len()) {
        return Err(format!(
            "{field} must contain between {minimum} and {maximum} strings"
        ));
    }
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|value| !value.contains('\0'))
                .map(str::to_string)
                .ok_or_else(|| format!("{field}[{index}] must be a string without NUL"))
        })
        .collect()
}

fn optional_string_array(
    object: &Map<String, Value>,
    field: &str,
    maximum: usize,
) -> Result<Vec<String>, String> {
    match object.get(field) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) if values.len() <= maximum => values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty() && !value.contains('\0'))
                    .map(str::to_string)
                    .ok_or_else(|| {
                        format!("{field}[{index}] must be a non-empty string without NUL")
                    })
            })
            .collect(),
        Some(Value::Array(_)) => Err(format!("{field} contains more than {maximum} entries")),
        Some(_) => Err(format!("{field} must be an array")),
    }
}

fn optional_f64(object: &Map<String, Value>, field: &str) -> Result<Option<f64>, String> {
    object
        .get(field)
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| format!("{field} must be a finite number"))
        })
        .transpose()
}

pub fn bounded_optional_u64(
    object: &Map<String, Value>,
    field: &str,
    minimum: u64,
    maximum: u64,
) -> Result<Option<u64>, String> {
    let value = object
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| format!("{field} must be a non-negative integer"))
        })
        .transpose()?;
    if let Some(value) = value {
        if !(minimum..=maximum).contains(&value) {
            return Err(format!("{field} must be between {minimum} and {maximum}"));
        }
    }
    Ok(value)
}

fn sanitize_ax_text(value: &str, max: usize) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    truncate(
        normalized
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_str(),
        max,
    )
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

pub(super) fn completion_transport(environment: &ToolProcessEnvironment) -> Result<(NativeTransport, crate::browser_wait_completion::WaitCompletionConfig), String> {
    let config = load_browser_config(environment)?;
    let BrowserTransportConfig::Native(transport) = config.transport else { return Err("Wait observer requires its exact native transport".into()); };
    let dependency = config.wait_completion.ok_or("Wait observer dependency is not selected")?;
    Ok((NativeTransport::new(environment, &transport)?, dependency))
}
