use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::protocol::{
    self, Control, Evidence, Observation, Permissions, Request, Response, Screen,
};

pub type Execution<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

#[cfg(test)]
#[path = "../tests/support/host_connection.rs"]
mod host_connection_tests;

pub trait DesktopExecutor: Send + Sync {
    fn retained_process_id<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<u32, String>> + Send + 'a>> {
        Box::pin(async { Err("native presentation requires an existing retained process".into()) })
    }
    fn execute<'a>(&'a self, action: Value) -> Execution<'a>;
    fn shutdown<'a>(&'a self) -> Execution<'a> {
        Box::pin(async { Ok(json!({})) })
    }
}

struct DeviceState {
    control: Control,
    observations: BTreeMap<String, Observation>,
    active_cancel: Option<PathBuf>,
    device_lock: Option<Arc<std::fs::File>>,
    input_lease: Option<protocol::application::InputLease>,
    input_revoking: bool,
}

#[derive(Clone)]
struct ApplicationObservation {
    scope: protocol::application::Scope,
    app: protocol::application::AppRef,
    grant_request_id: String,
    observation_id: String,
    revision: u64,
    control_generation: u64,
}

#[derive(Default)]
struct PermissionState {
    snapshot: Option<protocol::permission::NativeSnapshot>,
    requests: BTreeSet<String>,
}

#[derive(Clone)]
struct PermissionPending {
    request: Request,
    cancel_file: PathBuf,
}

struct PermissionContinuation<'a> {
    pending: &'a Mutex<Option<PermissionPending>>,
    request: &'a Request,
    cancel_file: PathBuf,
    deadline: u64,
}

impl Drop for PermissionContinuation<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            if pending.as_ref().is_some_and(|entry| entry.request.request_id == self.request.request_id
                && entry.request.worker_environment_id == self.request.worker_environment_id) {
                *pending = None;
            }
        }
    }
}

pub struct DesktopHost {
    executor: Arc<dyn DesktopExecutor>,
    state: Mutex<DeviceState>,
    input_gate: tokio::sync::Mutex<()>,
    root: PathBuf,
    device_file: Arc<std::fs::File>,
    last_native: Mutex<Option<Value>>,
    app_approval: Option<(crate::config::AppApprovalConfig, String)>,
    application_behavior: Option<crate::config::ApplicationBehavior>,
    application_policy: Option<crate::config::ApplicationPolicy>,
    application_preferences: Option<PathBuf>,
    app_grants: Mutex<BTreeMap<String, crate::app_authorization::PendingRequest>>,
    app_observations: Mutex<BTreeMap<String, ApplicationObservation>>,
    app_sources: Mutex<BTreeMap<(String, String, String), protocol::application::LiveAppSource>>,
    presentation_timeout: Option<std::time::Duration>,
    presentation_requests: Mutex<BTreeSet<String>>,
    host_generation: Option<String>,
    permission_state: tokio::sync::Mutex<PermissionState>,
    permission_pending: Mutex<Option<PermissionPending>>,
}

impl DesktopHost {
    pub fn new(executor: Arc<dyn DesktopExecutor>, root: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let file = open_device_file(&root.join("device.lock"))?;
        Self::with_device_file(executor, root, Arc::new(file))
    }

    pub fn with_device_file(executor: Arc<dyn DesktopExecutor>, root: PathBuf, device_file: Arc<std::fs::File>) -> Result<Self, String> {
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let saved = root.join("control.json");
        let control = if saved.exists() {
            let saved: Control =
                serde_json::from_slice(&std::fs::read(&saved).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("invalid saved desktop control: {e}"))?;
            Control {
                mode: "stopped".into(),
                owner: None,
                generation: saved
                    .generation
                    .checked_add(1)
                    .ok_or("desktop control generation overflow")?,
            }
        } else {
            Control {
                mode: "idle".into(),
                owner: None,
                generation: 0,
            }
        };
        Ok(Self {
            executor,
            root,
            device_file,
            last_native: Mutex::new(None),
            app_approval: None,
            application_behavior: None,
            application_policy: None,
            application_preferences: None,
            app_grants: Mutex::new(BTreeMap::new()),
            app_observations: Mutex::new(BTreeMap::new()),
            app_sources: Mutex::new(BTreeMap::new()),
            presentation_timeout: None,
            presentation_requests: Mutex::new(BTreeSet::new()),
            host_generation: None,
            permission_state: tokio::sync::Mutex::new(PermissionState::default()),
            permission_pending: Mutex::new(None),
            input_gate: tokio::sync::Mutex::new(()),
            state: Mutex::new(DeviceState {
                control,
                observations: BTreeMap::new(),
                active_cancel: None,
                device_lock: None,
                input_lease: None,
                input_revoking: false,
            }),
        })
    }

    pub fn with_device_lock(mut self, path: PathBuf) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("desktop device lock must be an absolute declared path".into());
        }
        self.device_file = Arc::new(open_device_file(&path)?);
        Ok(self)
    }

    fn with_app_approval(mut self, config: Option<crate::config::AppApprovalConfig>, generation: String) -> Self {
        self.host_generation = Some(generation.clone());
        self.app_approval = config.map(|config| (config, generation));
        self
    }

    fn with_application_policy(mut self, policy: Option<crate::config::ApplicationPolicy>) -> Self {
        self.application_policy = policy; self
    }

    pub fn with_application_preferences(mut self, root: Option<PathBuf>) -> Result<Self, String> {
        if let Some(root) = &root { crate::native_connection::validate_directory(root)?; }
        self.application_preferences = root;
        Ok(self)
    }

    fn with_application_behavior(mut self, behavior: crate::config::ApplicationBehavior) -> Self {
        self.application_behavior = Some(behavior); self
    }

    fn with_presentation_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.presentation_timeout = Some(timeout); self
    }

    fn persist_control(&self, control: &Control) -> Result<(), String> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.root.join("control.json"))
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec(control).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    }

    pub async fn handle(&self, request: Request, operator: bool) -> Response {
        if request.schema_version != protocol::VERSION
            || request.worker_environment_id.is_empty()
            || request.request_id.is_empty()
            || request.request_id.len() > 512
            || !request.action.is_object()
        {
            return protocol::failure(
                &request,
                "invalid_request",
                "desktop request identity or protocol is invalid",
            );
        }
        if request.action["type"] == "application_permissions" {
            let result = (|| -> Result<Value, String> {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Action { r#type: String, operation: String, host_generation: String, #[serde(default)] bundle_id: Option<String> }
                let action: Action = serde_json::from_value(request.action.clone()).map_err(|error| error.to_string())?;
                if !operator || request.tool != "operator.desktop" || action.r#type != "application_permissions"
                    || Some(action.host_generation.as_str()) != self.host_generation.as_deref()
                    || !matches!(action.operation.as_str(), "list" | "remove")
                    || (action.operation == "remove") != action.bundle_id.is_some() {
                    return Err("application settings require the exact private operator host and action".into());
                }
                let root = self.application_preferences.as_deref().ok_or("always application preferences are not configured")?;
                let mut value = crate::app_authorization::application_preferences(root, action.bundle_id.as_deref())?;
                value["type"] = json!("application_permissions"); value["host_generation"] = json!(action.host_generation);
                Ok(value)
            })();
            return match result {
                Ok(value) => Response { schema_version: protocol::VERSION,
                    worker_environment_id: request.worker_environment_id.clone(), request_id: request.request_id.clone(),
                    operation: "application_permissions".into(), accepted: true, evidence: None, error: None,
                    application_result: Some(value), receiver_bootstrap: None, presentation_result: None,
                    permission_result: None, permission_settings_source: None },
                Err(error) => protocol::failure(&request, "application_preferences_unavailable", error),
            };
        }
        if request.action["type"] == "permission_settings_source" {
            return match self.permission_settings_source(&request, operator).await {
                Ok(source) => Response { schema_version: protocol::VERSION,
                    worker_environment_id: request.worker_environment_id.clone(), request_id: request.request_id.clone(),
                    operation: "permission_settings_source".into(), accepted: true, evidence: None, error: None,
                    application_result: None, receiver_bootstrap: None, presentation_result: None,
                    permission_result: None, permission_settings_source: Some(source) },
                Err(error) => protocol::failure(&request, "native_permission_source_unavailable", error),
            };
        }
        if request.action["type"] == "permissions" {
            return match self.permissions(&request, operator).await {
                Ok(snapshot) => Response { schema_version: protocol::VERSION,
                    worker_environment_id: request.worker_environment_id.clone(), request_id: request.request_id.clone(),
                    operation: "permissions".into(), accepted: true, evidence: None, error: None,
                    application_result: None, receiver_bootstrap: None, presentation_result: None, permission_result: Some(snapshot), permission_settings_source: None },
                Err(error) => protocol::failure(&request, "native_permission_unavailable", error),
            };
        }
        if request.action["type"] == "presentation_complete" {
            return match self.presentation_complete(&request, operator).await {
                Ok(result) => Response { schema_version: protocol::VERSION,
                    worker_environment_id: request.worker_environment_id.clone(), request_id: request.request_id.clone(),
                    operation: "presentation_complete".into(), accepted: true, evidence: None, error: None,
                    application_result: None, receiver_bootstrap: None, presentation_result: Some(result), permission_settings_source: None, permission_result: None },
                Err(error) => protocol::failure(&request, "native_presentation_unavailable", error),
            };
        }
        if request.action["type"] == "receiver_bootstrap" {
            return match self.receiver_bootstrap(&request, operator).await {
                Ok(bootstrap) => Response { schema_version: protocol::VERSION,
                    worker_environment_id: request.worker_environment_id.clone(), request_id: request.request_id.clone(),
                    operation: "receiver_bootstrap".into(), accepted: true, evidence: None, error: None,
                    application_result: None, receiver_bootstrap: Some(bootstrap), presentation_result: None, permission_settings_source: None, permission_result: None },
                Err(error) => protocol::failure(&request, "native_presentation_unavailable", error),
            };
        }
        let mut response = if request.action["type"].as_str().is_some_and(protocol::is_application) {
            match self.perform_application(&request, operator).await {
                Ok((evidence, result)) => Response {
                    schema_version: protocol::VERSION, worker_environment_id: request.worker_environment_id.clone(),
                    request_id: request.request_id.clone(), operation: request.action["type"].as_str().unwrap_or("").into(),
                    accepted: true, evidence: Some(evidence), error: None, application_result: Some(result), receiver_bootstrap: None, presentation_result: None, permission_settings_source: None, permission_result: None,
                },
                Err(error) => protocol::failure(&request, "application_action_refused", error),
            }
        } else { match self.perform(&request, operator).await {
            Ok(evidence) => Response {
                schema_version: protocol::VERSION,
                worker_environment_id: request.worker_environment_id.clone(),
                request_id: request.request_id.clone(),
                operation: request.action["type"].as_str().unwrap_or("").into(),
                accepted: true,
                evidence: Some(evidence),
                error: None,
                application_result: None,
                receiver_bootstrap: None,
                presentation_result: None,
                permission_settings_source: None, permission_result: None,
            },
            Err(error) => protocol::failure(&request, "desktop_action_refused", error),
        }};
        if let Some(evidence) = &mut response.evidence {
            match self.app_sources.lock() {
                Ok(sources) => evidence.applications = sources.values().filter(|source|
                    source.scope.worker_environment_id == request.worker_environment_id).cloned().collect(),
                Err(_) => return protocol::failure(&request, "application_source_unavailable", "application source inventory lock poisoned"),
            }
        }
        response
    }

    async fn permission_settings_source(&self, request: &Request, operator: bool) -> Result<protocol::permission::SettingsSource, String> {
        use protocol::permission::{SettingsAction, SettingsSource, SettingsState};
        if !operator || request.tool != "operator.desktop" || request.worker_environment_id.len() > 512
            || request.worker_environment_id.contains('\0') {
            return Err("native Settings source requires the private operator room".into());
        }
        protocol::permission::validate_request_id(&request.request_id)?;
        let SettingsAction::PermissionSettingsSource { permission_request_id } = serde_json::from_value(request.action.clone())
            .map_err(|_| "native Settings source action has invalid fields")?;
        protocol::permission::validate_request_id(&permission_request_id)?;
        let generation = self.host_generation.as_deref().ok_or("native Settings host generation is unavailable")?;
        let absent = SettingsSource::absent(generation, &permission_request_id);
        absent.validate(generation, &permission_request_id)?;
        let pending = || -> Result<Option<PermissionPending>, String> {
            let current = self.permission_pending.lock().map_err(|_| "native permission cancellation lock poisoned")?;
            let Some(entry) = current.as_ref() else { return Ok(None); };
            if entry.request.worker_environment_id != request.worker_environment_id
                || entry.request.request_id != permission_request_id || entry.request.tool != "operator.desktop"
                || entry.request.action["type"] != "permissions" || entry.request.action["operation"] != "open_settings" {
                return Err("native Settings source belongs to another pending request".into());
            }
            if entry.cancel_file.try_exists().map_err(|error| error.to_string())? { return Ok(None); }
            Ok(Some(entry.clone()))
        };
        let Some(original) = pending()? else { return Ok(absent); };
        let permission = original.request.action["permission"].as_str().ok_or("pending Settings requirement is invalid")?;
        let process_id = self.executor.retained_process_id().await?;
        if pending()?.is_none() { return Ok(absent); }
        let value = self.executor.execute(json!({"type":"permission_settings_source",
            "permission_request_id":permission_request_id,"worker_environment_id":request.worker_environment_id,
            "host_generation":generation})).await?;
        if self.executor.retained_process_id().await? != process_id {
            return Err("native Settings helper changed during observation".into());
        }
        let Some(current) = pending()? else { return Ok(absent); };
        if current.request.action != original.request.action || current.cancel_file != original.cancel_file {
            return Err("native Settings continuation changed during observation".into());
        }
        if serde_json::to_vec(&value).map_err(|_| "native Settings source cannot encode")?.len() > protocol::permission::MAX_BYTES {
            return Err("native Settings source exceeds its control bound".into());
        }
        let mut source: SettingsSource = serde_json::from_value(value).map_err(|_| "native Settings source has invalid fields")?;
        source.validate(generation, &permission_request_id)?;
        if source.permission.is_none() && source.state == SettingsState::Ended {
            source.permission = Some(permission.into()); source.state = SettingsState::Waiting;
        } else if source.permission.as_deref() != Some(permission) {
            return Err("native Settings source changed its exact permission requirement".into());
        }
        Ok(source)
    }

    async fn permission_snapshot(&self, action: Value, process_id: u32) -> Result<protocol::permission::NativeSnapshot, String> {
        let value = self.executor.execute(action).await?;
        if serde_json::to_vec(&value).map_err(|_| "native permission status cannot encode")?.len() > protocol::permission::MAX_BYTES {
            return Err("native permission status exceeds its control bound".into());
        }
        let snapshot: protocol::permission::NativeSnapshot = serde_json::from_value(value)
            .map_err(|_| "native permission status has invalid fields")?;
        snapshot.validate(process_id)?;
        if self.executor.retained_process_id().await? != process_id {
            return Err("native permission helper changed during the operation".into());
        }
        Ok(snapshot)
    }

    async fn permissions(&self, request: &Request, operator: bool) -> Result<protocol::permission::Snapshot, String> {
        use protocol::permission::{Action, Operation, Snapshot};
        if !operator || request.tool != "operator.desktop" {
            return Err("native permission access is a private operator route".into());
        }
        if request.worker_environment_id.len() > 512 || request.worker_environment_id.contains('\0') {
            return Err("native permission room identity is invalid".into());
        }
        protocol::permission::validate_request_id(&request.request_id)?;
        let Action::Permissions { operation, permission } = serde_json::from_value(request.action.clone())
            .map_err(|_| "native permission action has invalid fields")?;
        if (operation == Operation::Status) != permission.is_none()
            || permission.as_deref().is_some_and(|id| id.is_empty() || id.len() > 4096 || id.contains('\0')) {
            return Err("native permission action requires its exact observed requirement".into());
        }
        let generation = self.host_generation.as_ref().ok_or("native permission host generation is unavailable")?;
        if generation.len() != 32 || !generation.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
            return Err("native permission host generation is invalid".into());
        }
        let mut state = self.permission_state.try_lock().map_err(|_| "native permission operation is already pending; refresh status")?;
        if state.requests.contains(&request.request_id) {
            return Err("native permission request was already used; refresh status without repeating it".into());
        }
        let previous = state.snapshot.clone();
        if let Some(id) = &permission {
            let row = previous.as_ref().and_then(|snapshot| snapshot.permissions.iter().find(|row| &row.id == id))
                .ok_or("native permission requirement has not been observed for this helper")?;
            if operation == Operation::Request && !row.can_request {
                return Err("this native permission is changed in System Settings; no direct request is supported".into());
            }
            if operation == Operation::OpenSettings && !row.can_open_settings {
                return Err("this native permission has no available Settings destination".into());
            }
            if state.requests.len() >= 4096 { return Err("native permission request replay bound reached".into()); }
        }
        let continuation = if operation == Operation::OpenSettings {
            let timeout = self.presentation_timeout.ok_or("native permission request timeout is not configured")?
                .min(std::time::Duration::from_secs(120));
            let deadline = presentation_deadline(timeout)?;
            let key = blake3::hash(format!("{}:{}", request.worker_environment_id, request.request_id).as_bytes());
            let cancel_file = self.root.join(format!("permission-{key}.cancel"));
            if cancel_file.try_exists().map_err(|error| error.to_string())? {
                return Err("native permission request was cancelled previously; refresh status with a new request".into());
            }
            let mut pending = self.permission_pending.lock().map_err(|_| "native permission cancellation lock poisoned")?;
            if pending.is_some() { return Err("native permission continuation is already pending".into()); }
            *pending = Some(PermissionPending { request: request.clone(), cancel_file: cancel_file.clone() });
            Some(PermissionContinuation { pending: &self.permission_pending, request, cancel_file, deadline })
        } else { None };
        state.snapshot = None;
        let process_id = self.executor.retained_process_id().await?;
        if permission.is_some() {
            let fresh = self.permission_snapshot(json!({"type":"permissions","operation":"status","permission":null}), process_id).await?;
            if !previous.as_ref().is_some_and(|old| fresh.same_identity(old)) {
                return Err("native permission helper or target changed; refresh status".into());
            }
            let row = fresh.permissions.iter().find(|row| Some(&row.id) == permission.as_ref())
                .ok_or("native permission requirement is no longer available")?;
            if operation == Operation::OpenSettings && !row.can_open_settings {
                return Err("native permission Settings destination is no longer available".into());
            }
            state.requests.insert(request.request_id.clone());
        }
        let mut action = request.action.clone();
        if let Some(continuation) = &continuation {
            if continuation.cancel_file.try_exists().map_err(|error| error.to_string())? {
                return Err("native permission caller disconnected before Settings dispatch".into());
            }
            action["worker_environment_id"] = json!(request.worker_environment_id);
            action["host_generation"] = json!(generation);
            action["request_id"] = json!(request.request_id);
            action["cancel_file"] = json!(continuation.cancel_file);
            action["expires_at_mach_continuous_ticks"] = json!(continuation.deadline.to_string());
        }
        let snapshot = self.permission_snapshot(action, process_id).await?;
        if let Some(continuation) = &continuation {
            if continuation.cancel_file.try_exists().map_err(|error| error.to_string())? {
                return Err("native permission caller disconnected; inspect current status without repeating the Settings request".into());
            }
        }
        if previous.as_ref().is_some_and(|old| !snapshot.same_identity(old)) {
            return Err("native permission helper or target changed during the operation".into());
        }
        state.snapshot = Some(snapshot.clone());
        Ok(Snapshot { schema_version: 1, host_generation: generation.clone(), helper: snapshot.helper, permissions: snapshot.permissions })
    }

    fn presentation_source(&self, scope: &protocol::presentation::Scope, window: &str) -> Result<protocol::application::LiveAppSource, String> {
        let (_, generation) = self.app_approval.as_ref().ok_or("native application approval is not configured")?;
        if scope.host_generation != *generation { return Err("native presentation belongs to another host generation".into()); }
        let grants = self.app_grants.lock().map_err(|_| "native application grants unavailable")?;
        let grant = grants.values().find(|grant| grant.request.request_id == scope.grant_request_id)
            .ok_or("native presentation has no current accepted application target")?;
        if grant.request.scope != scope.room() { return Err("native presentation does not belong to the accepted formation".into()); }
        let sources = self.app_sources.lock().map_err(|_| "native application sources unavailable")?;
        let source = sources.get(&(scope.worker_environment_id.clone(), scope.formation_pin.clone(), scope.app_instance_id.clone()))
            .ok_or("native presentation source is no longer retained")?;
        source.validate()?;
        if source.grant_request_id != scope.grant_request_id || source.scope != scope.room()
            || source.application.bundle_id != grant.request.application.bundle_id
            || source.application.bundle_path != grant.request.application.bundle_path
            || !source.windows.iter().any(|entry| entry.window_id == window) {
            return Err("native presentation source is outside the exact accepted application window set".into());
        }
        Ok(source.clone())
    }

    async fn presentation_complete(&self, request: &Request, operator: bool) -> Result<protocol::presentation::CompletionResult, String> {
        use protocol::presentation::{Action, CompletionResult};
        if !operator || request.tool != "operator.desktop" {
            return Err("native presentation completion is a private operator route".into());
        }
        let Action::PresentationComplete(target) = serde_json::from_value(request.action.clone())
            .map_err(|_| "native presentation completion has invalid fields")?
            else { return Err("native presentation action is not a completion".into()); };
        target.validate(&request.worker_environment_id)?;
        self.presentation_source(&target.scope, &target.source_window_id)?;
        let producer_pid = self.executor.retained_process_id().await?;
        let mut action = serde_json::to_value(&target).map_err(|_| "native presentation target cannot be encoded")?;
        action["type"] = json!("pip_complete");
        let result: CompletionResult = serde_json::from_value(self.executor.execute(action).await?)
            .map_err(|_| "native presentation completion acknowledgement has invalid fields")?;
        result.validate_for(&target)?;
        if self.executor.retained_process_id().await? != producer_pid {
            return Err("native presentation producer changed during completion".into());
        }
        self.presentation_source(&target.scope, &target.source_window_id)?;
        Ok(result)
    }

    async fn receiver_bootstrap(&self, request: &Request, operator: bool) -> Result<protocol::presentation::Bootstrap, String> {
        use base64::Engine;
        use protocol::presentation::{Action, Binding, Bootstrap, PreparedSource};
        if !operator || request.tool != "operator.desktop" { return Err("native presentation bootstrap is a private operator route".into()); }
        self.require_computer_use_enabled()?;
        let timeout = self.presentation_timeout.ok_or("native presentation timeout is not configured")?;
        let deadline = presentation_deadline(timeout)?;
        let Action::ReceiverBootstrap { scope, source_window_id, receiver_pid, receiver_nonce, receiver_instance_nonce } =
            serde_json::from_value(request.action.clone()).map_err(|_| "native presentation action has invalid fields")?
            else { return Err("native presentation action is not a bootstrap".into()); };
        if scope.worker_environment_id != request.worker_environment_id || receiver_pid <= 0
            || [&receiver_nonce, &receiver_instance_nonce].iter().any(|value| value.len() != 64
                || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))) {
            return Err("native receiver identity is invalid".into());
        }
        let source = self.presentation_source(&scope, &source_window_id)?;
        let producer_pid = self.executor.retained_process_id().await?;
        if producer_pid == 0 || producer_pid > i32::MAX as u32 || receiver_pid as u32 == producer_pid {
            return Err("native producer process identity is invalid".into());
        }
        {
            let mut requests = self.presentation_requests.lock().map_err(|_| "native presentation replay state unavailable")?;
            if requests.len() >= 4096 || !requests.insert(request.request_id.clone()) {
                return Err("native presentation request was already used or its replay bound was reached".into());
            }
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| "native presentation random identity unavailable")?;
        let bootstrap_id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        self.require_computer_use_enabled()?;
        let prepared = self.executor.execute(json!({"type":"pip_prepare","bootstrap_id":bootstrap_id,
            "request_id":request.request_id,"scope":scope,"source":source.source,"source_window_id":source_window_id,
            "receiver_pid":receiver_pid,"receiver_nonce":receiver_nonce,"receiver_instance_nonce":receiver_instance_nonce,
            "expires_at_mach_continuous_ticks":deadline.to_string()})).await;
        let result = async {
            self.require_computer_use_enabled()?;
            let prepared: PreparedSource = serde_json::from_value(prepared?).map_err(|_| "native presentation preparation has invalid identity")?;
            let incarnation = prepared.native_window_incarnation.parse::<u64>().map_err(|_| "native window incarnation is invalid")?;
            if prepared.schema_version != 1 || prepared.producer_pid != producer_pid as i32 || incarnation == 0
                || incarnation.to_string() != prepared.native_window_incarnation
                || prepared.presentation_id.is_empty() || prepared.presentation_id.len() > 512
                || prepared.stream_generation == 0 || prepared.stream_generation > 9_007_199_254_740_991
                || !source.windows.iter().any(|window| window.window_id == source_window_id && window.cg_window_id == prepared.native_cg_window_id) {
                return Err("native presentation preparation changed its exact retained source".into());
            }
            let current = self.presentation_source(&scope, &source_window_id)?;
            if serde_json::to_value(&current).map_err(|e| e.to_string())? != serde_json::to_value(&source).map_err(|e| e.to_string())?
                || self.executor.retained_process_id().await? != producer_pid || presentation_clock()? >= deadline {
                return Err("native presentation source or process changed during preparation".into());
            }
            let binding = Binding { schema_version: 1, host_generation: scope.host_generation.clone(), bootstrap_id: bootstrap_id.clone(),
                request_id: request.request_id.clone(), receiver_nonce, receiver_instance_nonce, scope: scope.clone(), source: source.source.clone(),
                presentation_id: prepared.presentation_id, stream_generation: prepared.stream_generation,
                source_window_id: source_window_id.clone(), native_window_incarnation: prepared.native_window_incarnation,
                native_cg_window_id: prepared.native_cg_window_id, receiver_pid, producer_pid: prepared.producer_pid,
                expires_at_mach_continuous_ticks: deadline.to_string() };
            let bytes = serde_json::to_vec(&binding).map_err(|_| "native presentation binding serialization failed")?;
            if bytes.len() > 65536 { return Err("native presentation binding exceeds its private byte bound".into()); }
            let mut key = [0u8; 32]; getrandom::fill(&mut key).map_err(|_| "native presentation private credential unavailable")?;
            let response = Bootstrap { schema_version: 1, bootstrap_id: bootstrap_id.clone(),
                bootstrap_key_base64: base64::engine::general_purpose::STANDARD.encode(key),
                binding_bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes) };
            key.fill(0);
            self.require_computer_use_enabled()?;
            let installed = self.executor.execute(json!({"type":"pip_install","schema_version":1,"bootstrap_id":bootstrap_id,
                "bootstrap_key_base64":response.bootstrap_key_base64,"binding_bytes_base64":response.binding_bytes_base64})).await?;
            self.require_computer_use_enabled()?;
            if installed["schema_version"] != 1 || installed["installed"] != true || installed["bootstrap_id"] != bootstrap_id
                || installed["producer_pid"] != producer_pid || self.executor.retained_process_id().await? != producer_pid
                || presentation_clock()? >= deadline {
                return Err("native presentation install acknowledgement is stale or invalid".into());
            }
            let current = self.presentation_source(&scope, &source_window_id)?;
            if serde_json::to_value(&current).map_err(|e| e.to_string())? != serde_json::to_value(&source).map_err(|e| e.to_string())? {
                return Err("native application source changed before bootstrap publication".into());
            }
            Ok(response)
        }.await;
        if result.is_err() {
            let _ = self.executor.execute(json!({"type":"pip_cancel","bootstrap_id":bootstrap_id})).await;
        }
        result
    }

    async fn perform(&self, request: &Request, operator: bool) -> Result<Evidence, String> {
        let operation = request.action["type"]
            .as_str()
            .ok_or("action type is missing")?;
        if protocol::is_input(operation) || matches!(operation, "observe" | "screenshot" | "view")
            || operation == "control" && request.action["mode"] == "acquire" {
            self.require_computer_use_enabled()?;
        }
        if operator
            && !matches!(
                operation,
                "status" | "view" | "pause_view" | "take_control" | "release_control" | "stop"
            )
        {
            return Err("operator endpoint does not execute model tools".into());
        }
        if !operator
            && matches!(
                operation,
                "view" | "pause_view" | "take_control" | "release_control" | "stop"
            )
        {
            return Err("operator control is not a model tool".into());
        }
        if operation == "pause_view" {
            self.executor.execute(request.action.clone()).await?;
            return self.cached_status();
        }
        if operation == "view" {
            let native = self.executor.execute(request.action.clone()).await?;
            self.require_computer_use_enabled()?;
            let mut observation: Observation =
                serde_json::from_value(native["observation"].clone())
                    .map_err(|e| format!("native stream observation: {e}"))?;
            let mut evidence = self.cached_status()?;
            if observation.image.as_ref().is_some_and(|image| {
                request.action["known_image_sha256"].as_str() == Some(&image.sha256)
            }) {
                evidence.view_image_ref =
                    observation
                        .image
                        .take()
                        .map(|image| protocol::ViewImageRef {
                            sha256: image.sha256,
                        });
            }
            evidence.observation = Some(observation);
            return Ok(evidence);
        }
        if matches!(
            operation,
            "cancel" | "release" | "take_control" | "release_control" | "stop"
        ) {
            let revoked = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "desktop control lock poisoned")?;
                if state.input_revoking { return Err("desktop_busy: native input teardown is still in progress".into()); }
                if operator && matches!(operation, "take_control" | "release_control") {
                    let expected = request.action["control_generation"]
                        .as_u64()
                        .ok_or("operator control requires its observed generation")?;
                    if expected != state.control.generation {
                        return Err("stale_control: desktop ownership changed before the operator action".into());
                    }
                }
                if !operator
                    && operation == "release"
                    && matches!(state.control.mode.as_str(), "human" | "stopped")
                {
                    return Err(
                        "human_intervention: only the operator can release a human/stopped hold"
                            .into(),
                    );
                }
                if !operator
                    && state
                        .control
                        .owner
                        .as_deref()
                        .is_some_and(|owner| owner != request.worker_environment_id)
                {
                    return Err("desktop control belongs to another room".into());
                }
                if let Some(path) = &state.active_cancel {
                    std::fs::write(path, b"cancelled").map_err(|e| e.to_string())?;
                }
                if !matches!(operation, "release" | "release_control")
                    && state.device_lock.is_none()
                {
                    acquire_device_lock(&self.device_file)?;
                    state.device_lock = Some(self.device_file.clone());
                }
                state.control.generation = state
                    .control
                    .generation
                    .checked_add(1)
                    .ok_or("desktop control generation overflow")?;
                state.control.mode = if operation == "take_control" {
                    "human"
                } else if matches!(operation, "stop" | "cancel") {
                    "stopped"
                } else {
                    "idle"
                }
                .into();
                state.control.owner = None;
                state.observations.clear();
                self.app_observations.lock().map_err(|_| "application observations lock poisoned")?.clear();
                let revoked = state.input_lease.take();
                state.input_revoking = revoked.is_some();
                if state.control.mode == "idle" && state.active_cancel.is_none() && !state.input_revoking {
                    release_device_lock(&mut state)?;
                }
                self.persist_control(&state.control)?;
                revoked
            };
            if let Some(lease) = revoked { self.revoke_native_input(lease).await?; }
            if operation == "release" {
                if let Some(pin) = request.action["formation_pin"].as_str() {
                    self.app_grants.lock().map_err(|_| "application grants lock poisoned")?.retain(|_, grant|
                        grant.request.scope.worker_environment_id != request.worker_environment_id || grant.request.scope.formation_pin != pin);
                    self.app_sources.lock().map_err(|_| "application sources lock poisoned")?.retain(|_, source|
                        source.scope.worker_environment_id != request.worker_environment_id || source.scope.formation_pin != pin);
                    self.executor.execute(json!({"type":"app_release","worker_environment_id":request.worker_environment_id,"formation_pin":pin})).await?;
                }
            }
            return self.cached_status();
        }
        if operation == "control" {
            if request.action["mode"] == "release" {
                let mut release = request.clone();
                release.action = json!({"type":"release"});
                return Box::pin(self.perform(&release, false)).await;
            }
            if request.action["mode"] != "acquire" {
                return Err("unknown control mode".into());
            }
            let status = self.status().await?;
            if !status.permissions.accessibility || !status.permissions.screen_capture {
                return Err("permissions_required: grant Screen Recording and Accessibility to the desktop helper".into());
            }
            {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| "desktop control lock poisoned")?;
                if state.input_revoking { return Err("desktop_busy: native input teardown is still in progress".into()); }
                if matches!(state.control.mode.as_str(), "human" | "stopped") {
                    return Err("human_intervention: operator must explicitly release control before model input".into());
                }
                if state
                    .control
                    .owner
                    .as_deref()
                    .is_some_and(|owner| owner != request.worker_environment_id)
                {
                    return Err("desktop is controlled by another room".into());
                }
                if state.control.owner.is_none() {
                    acquire_device_lock(&self.device_file)?;
                    state.device_lock = Some(self.device_file.clone());
                    state.control.generation = state
                        .control
                        .generation
                        .checked_add(1)
                        .ok_or("desktop control generation overflow")?;
                    state.control.owner = Some(request.worker_environment_id.clone());
                    state.control.mode = "model".into();
                    self.persist_control(&state.control)?;
                }
            }
            return self.status().await;
        }
        if operation == "status" {
            return self.status().await;
        }
        if matches!(operation, "observe" | "screenshot") {
            let _gate = self
                .input_gate
                .try_lock()
                .map_err(|_| "desktop_busy: input is in progress")?;
            let mut action = request.action.clone();
            action["type"] = json!("observe");
            action["include_screenshot"] =
                json!(operation != "observe" || request.action["include_screenshot"] == true);
            let native = self.executor.execute(action).await?;
            self.require_computer_use_enabled()?;
            let mut observation: Observation =
                serde_json::from_value(native["observation"].clone())
                    .map_err(|e| format!("native observation protocol: {e}"))?;
            let now = now_ms()?;
            observation.id = blake3::hash(
                format!(
                    "{}:{}:{}",
                    request.worker_environment_id, request.request_id, now
                )
                .as_bytes(),
            )
            .to_hex()
            .to_string();
            if operation != "view" {
                self.state
                    .lock()
                    .map_err(|_| "desktop control lock poisoned")?
                    .observations
                    .insert(request.worker_environment_id.clone(), observation.clone());
            }
            let mut evidence = self.status_from_native(&native)?;
            evidence.observation = Some(observation);
            return Ok(evidence);
        }
        if !protocol::is_input(operation) {
            return Err("unsupported desktop action".into());
        }
        let _gate = self
            .input_gate
            .try_lock()
            .map_err(|_| "desktop_busy: another input is in progress; no action was queued")?;
        let (screen_id, observation_id, generation) = protocol::action_target(&request.action)?;
        let observation = {
            let state = self
                .state
                .lock()
                .map_err(|_| "desktop control lock poisoned")?;
            require_owner(&state.control, request, generation)?;
            let observation = state
                .observations
                .get(&request.worker_environment_id)
                .ok_or("fresh_observation_required")?;
            if observation.id != observation_id || observation.screen.id != screen_id {
                return Err("stale_observation: screen or observation identity changed".into());
            }
            observation.clone()
        };
        let key = blake3::hash(
            format!("{}:{}", request.worker_environment_id, request.request_id).as_bytes(),
        )
        .to_hex()
        .to_string();
        let pending = self.root.join(format!("{key}.pending"));
        let completed = self.root.join(format!("{key}.json"));
        if completed.exists() {
            return Err(
                "duplicate_action: recorded receipt already exists; action was not replayed".into(),
            );
        }
        if pending.exists() {
            return Err("effect_uncertain: an earlier attempt has no completed receipt; observe before any new action".into());
        }
        let cancel = self.root.join(format!("{key}.cancel"));
        let mut action = request.action.clone();
        action["expected"] = serde_json::to_value(&observation).map_err(|e| e.to_string())?;
        action["expected"]["image"] = Value::Null;
        action["cancel_file"] = json!(cancel);
        match operation {
            "click" | "move" => {
                action["point"] = protocol::screen_point(
                    &observation.screen,
                    number(&action, "x")?,
                    number(&action, "y")?,
                )?;
            }
            "drag" => {
                action["from"] = protocol::screen_point(
                    &observation.screen,
                    number(&action, "from_x")?,
                    number(&action, "from_y")?,
                )?;
                action["to"] = protocol::screen_point(
                    &observation.screen,
                    number(&action, "to_x")?,
                    number(&action, "to_y")?,
                )?;
            }
            _ => {}
        }
        {
            use std::io::Write;
            let mut state = self
                .state
                .lock()
                .map_err(|_| "desktop control lock poisoned")?;
            require_owner(&state.control, request, generation)?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&pending)
                .map_err(|e| e.to_string())?;
            file.write_all(&serde_json::to_vec(request).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            state.active_cancel = Some(cancel.clone());
            state.observations.remove(&request.worker_environment_id);
        }
        let result = self.with_computer_use_input(&cancel, self.executor.execute(action)).await;
        let interrupted = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "desktop control lock poisoned")?;
            state.active_cancel = None;
            if state.control.mode == "idle" && !state.input_revoking {
                release_device_lock(&mut state)?;
            }
            state.control.generation != generation
        };
        let native = result?;
        std::fs::write(
            &completed,
            serde_json::to_vec(&native).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        std::fs::OpenOptions::new()
            .read(true)
            .open(&completed)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        if interrupted || native["completed"] != true {
            return Err(format!(
                "input_interrupted: execution receipt {}; events_posted={}",
                completed.display(),
                native["events_posted"]
            ));
        }
        let mut evidence = self.status_from_native(&native)?;
        evidence.effect = Some(
            json!({"screen_id":screen_id,"observation_id":observation_id,
            "control_generation":generation,"events_posted":native["events_posted"],"receipt_path":completed,
            "meaning":"input events posted; application outcome requires a new observation"}),
        );
        Ok(evidence)
    }

    async fn perform_application(&self, request: &Request, operator: bool) -> Result<(Evidence, Value), String> {
        use crate::app_authorization::{self as authorization, AdmittedDecision, ApplicationResult, CallContext, Decision, OperatorRequest, PendingRequest};
        if operator { return Err("operator viewer cannot resolve, approve or execute application tools".into()); }
        if request.action["type"] != "apps" { self.require_computer_use_enabled()?; }
        let (config, generation) = self.app_approval.as_ref().ok_or("application tools have no declared approval configuration")?;
        let context = CallContext::from_request(request)?;
        if context.scope.host_generation != *generation || request.action["resume_tool"].as_str() != Some(config.resume_tool.as_str())
            || request.action["operator_interface"].as_str() != Some(config.operator_interface.as_str()) {
            return Err("application request belongs to a different native host or configured resume tool".into());
        }
        let _gate = self.input_gate.try_lock().map_err(|_| "desktop_busy: another native application operation is in progress")?;
        if request.action["type"] == "apps" {
            if context.call.arguments != json!({}) { return Err("application catalog accepts no arguments".into()); }
            let native = self.executor.execute(json!({"type":"apps","scope":context.scope})).await?;
            let catalog: authorization::ApplicationCatalog = serde_json::from_value(native["catalog"].clone())
                .map_err(|error| format!("native application catalog: {error}"))?;
            catalog.validate(&context.scope)?;
            let result = ApplicationResult::Catalog { schema_version: 3, catalog };
            return Ok((self.status_from_native(&native)?, serde_json::to_value(result).map_err(|error| error.to_string())?));
        }
        if request.action["type"].as_str().is_some_and(protocol::is_app_effect) {
            return self.perform_application_effect(request, &context).await;
        }
        if request.action["type"] == "app_state" {
            let selection: protocol::application::CaptureSelection = serde_json::from_value(request.action["capture_selection"].clone())
                .map_err(|error| format!("application capture selection: {error}"))?;
            if request.action["app"] != context.call.arguments["app"]
                || request.action["presentation_base"] != context.call.arguments["presentation_base"]
                || selection != protocol::application::CaptureSelection::from_arguments(&context.call.arguments)?
                || request.action["include_screenshots"].as_bool() != Some(context.call.arguments["include_screenshots"].as_bool().unwrap_or(true)) {
                return Err("application action differs from the original arguments".into());
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Resolved {
                target_handle: String,
                application: authorization::ApplicationTarget,
                #[serde(default)] native_policy: Option<protocol::NativeAppPolicy>,
                #[serde(default)] native_policy_digest: Option<String>,
            }
            let native = self.executor.execute(json!({"type":"app_resolve","scope":context.scope,"selector":request.action["app"]})).await?;
            let resolved: Resolved = serde_json::from_value(native["resolved"].clone()).map_err(|error| format!("native application target: {error}"))?;
            resolved.application.validate_selector(&context.call.arguments["app"])?;
            if resolved.target_handle.is_empty() || resolved.target_handle.len() > 256 { return Err("native target has no bounded private handle".into()); }
            authorization::validate_native_policy(&resolved.application, resolved.native_policy.as_ref(),
                resolved.native_policy_digest.as_deref(), self.application_policy.as_ref())?;
            let evidence = self.status_from_native(&native)?;
            let offer = resolved.native_policy.as_ref().map(|policy|
                authorization::permission_offer(config.operator_interface.clone(), policy, self.application_preferences.is_some()));
            let candidate = OperatorRequest::new(context.call.clone(), context.scope.clone(), resolved.application,
                config.resume_tool.clone(), resolved.native_policy.clone(), offer)?;
            let saved = authorization::lookup_permission(&self.root, self.application_preferences.as_deref(), &candidate)?;
            if saved.as_ref().is_some_and(|record| record.decision.decision == Decision::Decline) {
                return Err("application_approval_declined: operator denied this application in the current conversation".into());
            }
            let key = application_grant_key(&candidate);
            let grant = if candidate.permission_offer.is_none() {
                self.app_grants.lock().map_err(|_| "application grants lock poisoned")?.get(&key).cloned()
            } else { None };
            if let Some(grant) = grant {
                let state = self.capture_application(request, &context.scope, &resolved.target_handle, &candidate.application, &grant.request.request_id,
                    &context.call, request.action["include_screenshots"].as_bool().ok_or("application capture mode is missing")?).await?;
                let result = ApplicationResult::State { schema_version: 3, grant_request_id: grant.request.request_id, application: state };
                return Ok((self.cached_status()?, serde_json::to_value(result).map_err(|error| error.to_string())?));
            }
            let pending = PendingRequest { request: candidate, native_target_handle: resolved.target_handle,
                operator_interface: config.operator_interface.clone(), provider_interface: context.inbound_interface,
                native_policy: resolved.native_policy, native_policy_digest: resolved.native_policy_digest };
            pending.validate_config(config)?;
            let path = authorization::record_path(&self.root, &pending.request.request_id, "request")?;
            if path.try_exists().map_err(|error| error.to_string())? {
                let existing: PendingRequest = authorization::read_record(&path)?;
                if existing.request != pending.request || existing.operator_interface != pending.operator_interface
                    || existing.provider_interface != pending.provider_interface
                    || existing.native_policy != pending.native_policy || existing.native_policy_digest != pending.native_policy_digest {
                    return Err("application request identity conflicts with its existing immutable record".into());
                }
            } else { authorization::create_record(&path, &pending)?; }
            if let Some(saved) = saved {
                {
                    let mut grants = self.app_grants.lock().map_err(|_| "application grants lock poisoned")?;
                    if grants.len() >= 1024 && !grants.contains_key(&key) { return Err("native application grant inventory exceeds its bound".into()); }
                    grants.insert(key, pending.clone());
                }
                let state = self.capture_application(request, &context.scope, &pending.native_target_handle, &pending.request.application,
                    &pending.request.request_id, &context.call, request.action["include_screenshots"].as_bool().ok_or("application capture mode is missing")?).await?;
                let mut evidence = self.cached_status()?;
                evidence.effect = Some(json!({"application_permission_reused":{"request_id":saved.request.request_id,
                    "operator_artifact":saved.decision.operator_artifact,"selected_persistence":saved.decision.selected_persistence}}));
                let result = ApplicationResult::State { schema_version: 3, grant_request_id: pending.request.request_id, application: state };
                return Ok((evidence, serde_json::to_value(result).map_err(|error| error.to_string())?));
            }
            let result = ApplicationResult::AwaitingApproval { schema_version: 3, operator_request: pending.request, effect_performed: false };
            return Ok((evidence, serde_json::to_value(result).map_err(|error| error.to_string())?));
        }
        let request_id = request.action["request_id"].as_str().ok_or("application resume has no request identity")?;
        let pending: PendingRequest = authorization::read_record(&authorization::record_path(&self.root, request_id, "request")?)?;
        let original = &pending.request;
        pending.validate_config(config)?;
        authorization::validate_native_policy(&original.application, pending.native_policy.as_ref(),
            pending.native_policy_digest.as_deref(), self.application_policy.as_ref())?;
        if original.scope != context.scope || original.original.run != context.call.run || original.original.ee != context.call.ee
            || original.original.worker != context.call.worker || original.original.door_interface != context.call.door_interface
            || original.resume.tool != request.tool || original.resume.tool != config.resume_tool
            || original.resume.correlation_id != context.call.correlation_id || pending.operator_interface != config.operator_interface
            || context.inbound_interface != pending.operator_interface || context.call.arguments != json!({"request_id":request_id}) {
            return Err("application resume does not identify the exact pending formed call".into());
        }
        let decision: AdmittedDecision = authorization::read_record(&authorization::record_path(&self.root, request_id, "decision")?)?;
        if decision.request_id != original.request_id || decision.call_id != original.call_id
            || decision.operator_artifact != context.call.source_artifact || decision.operator_interface != pending.operator_interface {
            return Err("application resume has no exact door-admitted operator artifact".into());
        }
        decision.validate(original)?;
        let completed = authorization::record_path(&self.root, request_id, "result")?;
        let consumed = authorization::record_path(&self.root, request_id, "consumed")?;
        if completed.try_exists().map_err(|error| error.to_string())? {
            let result: Value = authorization::read_record_bounded(&completed, protocol::MAX_NATIVE_RESULT_BYTES as u64)?;
            authorization::validate_result(&result, request)?;
            return Ok((self.status().await?, result));
        }
        if consumed.try_exists().map_err(|error| error.to_string())? {
            return Err("effect_uncertain: application approval was consumed without a terminal receipt; native operation was not replayed".into());
        }
        authorization::remember_permission(&self.root, self.application_preferences.as_deref(), original, &decision)?;
        if decision.decision == Decision::Accept {
            let mut grants = self.app_grants.lock().map_err(|_| "application grants lock poisoned")?;
            let key = application_grant_key(original);
            if grants.len() >= 1024 && !grants.contains_key(&key) { return Err("native application grant inventory exceeds its bound".into()); }
            grants.insert(key, pending.clone());
        }
        authorization::create_record(&consumed, &decision)?;
        let original_result = if decision.decision != Decision::Accept {
            let (code, detail) = match decision.decision {
                Decision::Cancel => ("application_approval_cancelled", "Operator cancelled the exact application request; no launch, capture or input was performed"),
                _ => ("application_approval_declined", "Operator declined the exact application target; no launch, capture or input was performed"),
            };
            octopus_tool_handlers::ToolResultArtifact::failure(&original.original.tool, Some(original.call_id.clone()),
                octopus_tool_handlers::ToolResultError::new(code, detail))
        } else {
            let include = original.original.arguments["include_screenshots"].as_bool().unwrap_or(true);
            match self.capture_application(request, &context.scope, &pending.native_target_handle, &original.application, &original.request_id, &original.original, include).await {
                Ok(state) => {
                    octopus_tool_handlers::ToolResultArtifact::success(&original.original.tool, Some(original.call_id.clone()),
                        json!({"accepted":true,"app_grant_request_id":original.request_id,"evidence":{"application":state}}))
                },
                Err(error) => octopus_tool_handlers::ToolResultArtifact::failure(&original.original.tool, Some(original.call_id.clone()),
                    octopus_tool_handlers::ToolResultError::new("application_operation_failed", error)
                        .with_details(serde_json::Map::from_iter([("effect_uncertain".into(), json!(true)), ("reexecuted".into(), json!(false))]))),
            }
        };
        let result = serde_json::to_value(ApplicationResult::Resumed { schema_version: 3, operator_request: original.clone(), original_result })
            .map_err(|error| error.to_string())?;
        authorization::create_record_bounded(&completed, &result, protocol::MAX_NATIVE_RESULT_BYTES as u64)?;
        Ok((self.status().await?, result))
    }

    async fn capture_application(&self, request: &Request, scope: &protocol::application::Scope, target: &str,
        approved: &crate::app_authorization::ApplicationTarget, grant_request_id: &str,
        source: &crate::app_authorization::OriginalCall, include: bool) -> Result<protocol::application::State, String> {
        let capture_selection = protocol::application::CaptureSelection::from_arguments(&source.arguments)?;
        if let Some(value) = source.arguments.get("presentation_base") {
            let base: protocol::application::PresentationBase = serde_json::from_value(value.clone()).map_err(|error| format!("presentation base: {error}"))?;
            base.validate()?;
            if approved.instance.as_ref().map(|instance| &instance.app) != Some(&base.app) {
                return Err("diff_base_foreign: display base does not identify the exact approved native application".into());
            }
        }
        let key = blake3::hash(format!("{}:{}", request.worker_environment_id, request.request_id).as_bytes()).to_hex().to_string();
        let cancel = self.root.join(format!("{key}.cancel"));
        let needs_focus = self.application_behavior.as_ref().ok_or("native application behavior is not declared")?
            .resolve(&approved.bundle_id)?.full_ui.enforce_focus_before_enhanced;
        let mut temporary_device = false;
        let mut preparation_lease = None;
        let generation = {
            let mut state = self.state.lock().map_err(|_| "desktop control lock poisoned")?;
            if state.input_revoking { return Err("desktop_busy: native input teardown is still in progress".into()); }
            if matches!(state.control.mode.as_str(), "human" | "stopped") || state.control.owner.as_deref().is_some_and(|owner| owner != request.worker_environment_id) {
                return Err("human_intervention: application operation was not started while control is held".into());
            }
            if cancel.try_exists().map_err(|error| error.to_string())? { return Err("application invocation has already been cancelled".into()); }
            if needs_focus {
                if state.device_lock.is_none() {
                    if state.input_lease.is_some() { return Err("native input lease has no held physical device".into()); }
                    let next = state.control.generation.checked_add(1).ok_or("desktop control generation overflow")?;
                    acquire_device_lock(&self.device_file)?;
                    state.device_lock = Some(self.device_file.clone());
                    state.control.generation = next;
                    temporary_device = true;
                }
                let lease = protocol::application::InputLease { scope:scope.clone(), control_generation:state.control.generation };
                if state.input_lease.as_ref().is_some_and(|current| current != &lease) {
                    return Err("native read preparation belongs to a different held input lease".into());
                }
                state.input_lease = Some(lease.clone());
                preparation_lease = Some(lease);
            }
            state.active_cancel = Some(cancel.clone());
            state.control.generation
        };
        let result = self.with_computer_use_input(&cancel, async {
            if let Some(lease) = &preparation_lease {
                let activated = self.executor.execute(json!({"type":"input_lease_begin","lease":lease})).await?;
                if activated["input_lease"] != serde_json::to_value(lease).map_err(|error| error.to_string())? {
                    return Err("native read preparation did not acknowledge its exact device lease".into());
                }
            }
            self.executor.execute(json!({"type":"app_state","scope":scope,"target_handle":target,
                "include_screenshots":include,"capture_selection":capture_selection,"presentation_base":source.arguments.get("presentation_base"),"cancel_file":cancel,"control_generation":generation,"grant_request_id":grant_request_id,"source":source.source()})).await
        }).await;
        let interrupted = {
            let mut state = self.state.lock().map_err(|_| "desktop control lock poisoned")?;
            state.active_cancel = None;
            state.control.generation != generation
        };
        if temporary_device {
            let lease = {
                let mut state = self.state.lock().map_err(|_| "desktop control lock poisoned")?;
                if state.input_lease == preparation_lease {
                    let lease = state.input_lease.take(); state.input_revoking = lease.is_some(); lease
                } else { None }
            };
            if let Some(lease) = lease { self.revoke_native_input(lease).await?; }
        }
        let native = result?;
        if interrupted { return Err("application operation was interrupted; no stale state was published".into()); }
        self.status_from_native(&native)?;
        let application: protocol::application::State = serde_json::from_value(native["application"].clone()).map_err(|error| format!("native application state: {error}"))?;
        application.validate(scope, &application.application.app)?;
        application.validate_capture_selection(capture_selection, include)?;
        if application.accessibility.presentation.mode != "full" {
            let base: protocol::application::PresentationBase = serde_json::from_value(source.arguments.get("presentation_base").cloned()
                .ok_or("native display compressed without an exact requested base")?).map_err(|error| error.to_string())?;
            if base.app != application.application.app || application.accessibility.presentation.base_revision != Some(base.revision) {
                return Err("native display compression differs from the exact requested base".into());
            }
        }
        if application.application.bundle_id != approved.bundle_id || application.application.bundle_path != approved.bundle_path {
            return Err("native application does not match the approved exact target".into());
        }
        if application.control_generation != generation { return Err("native application state changed its observed control generation".into()); }
        let live_source = protocol::application::LiveAppSource::from_state(&application, grant_request_id, source.source())?;
        let observation = ApplicationObservation { scope: scope.clone(), app: application.application.app.clone(),
            grant_request_id: grant_request_id.into(), observation_id: application.accessibility.observation_id.clone(),
            revision: application.accessibility.revision, control_generation: generation };
        let key = format!("{}:{}", scope.worker_environment_id, observation.app.app_instance_id);
        let state = self.state.lock().map_err(|_| "desktop control lock poisoned")?;
        if state.control.generation != generation { return Err("application control changed before observation publication".into()); }
        let mut observations = self.app_observations.lock().map_err(|_| "application observations lock poisoned")?;
        if observations.len() >= 1024 && !observations.contains_key(&key) { return Err("application observation inventory exceeds its bound".into()); }
        let mut sources = self.app_sources.lock().map_err(|_| "application sources lock poisoned")?;
        let source_key = (scope.worker_environment_id.clone(), scope.formation_pin.clone(), observation.app.app_instance_id.clone());
        if sources.len() >= 1024 && !sources.contains_key(&source_key) { return Err("application source inventory exceeds its bound".into()); }
        sources.insert(source_key, live_source);
        observations.insert(key, observation);
        Ok(application)
    }

    async fn perform_application_effect(&self, request: &Request, context: &crate::app_authorization::CallContext) -> Result<(Evidence, Value), String> {
        use crate::app_authorization::{self as authorization, ApplicationResult};
        let operation = request.action["type"].as_str().ok_or("application action has no operation")?;
        let call = protocol::application::ActionCall::parse(operation, &context.call.arguments)?;
        let declared: protocol::application::ActionCall = serde_json::from_value(request.action["app_call"].clone()).map_err(|error| error.to_string())?;
        if declared != call { return Err("application action differs from its admitted tool arguments".into()); }
        call.target.app.validate(&context.scope)?;
        let grant = self.app_grants.lock().map_err(|_| "application grants lock poisoned")?.values()
            .find(|grant| grant.request.request_id == call.target.grant_request_id).cloned().ok_or("application action has no live accepted target")?;
        let original = &grant.request.original;
        if grant.request.scope != context.scope || original.run != context.call.run || original.ee != context.call.ee
            || original.worker != context.call.worker || original.door_interface != context.call.door_interface
            || grant.provider_interface != context.inbound_interface { return Err("application action does not belong to the accepted formed caller".into()); }
        let digest = blake3::hash(format!("{}:{}", request.worker_environment_id, request.request_id).as_bytes()).to_hex().to_string();
        let pending = self.root.join(format!("app-action-{digest}.request.json"));
        let completed = self.root.join(format!("app-action-{digest}.result.json"));
        if completed.try_exists().map_err(|error| error.to_string())? {
            let result: Value = authorization::read_record(&completed)?;
            authorization::validate_result(&result, request)?;
            return Ok((self.status().await?, result));
        }
        if pending.try_exists().map_err(|error| error.to_string())? {
            return Err("effect_uncertain: application action was dispatched without a terminal receipt and was not replayed".into());
        }
        let key = format!("{}:{}", context.scope.worker_environment_id, call.target.app.app_instance_id);
        {
            let observations = self.app_observations.lock().map_err(|_| "application observations lock poisoned")?;
            let observed = observations.get(&key).ok_or("stale_observation: application action requires a fresh admitted app state")?;
            if observed.scope != context.scope || observed.app != call.target.app || observed.grant_request_id != call.target.grant_request_id
                || observed.observation_id != call.target.observation_id || observed.revision != call.target.revision
                || observed.control_generation != call.target.control_generation { return Err("application action does not identify the exact retained observation and control generation".into()); }
        }
        let cancel = self.root.join(format!("{digest}.cancel"));
        let lease = protocol::application::InputLease { scope: context.scope.clone(), control_generation: call.target.control_generation };
        {
            let mut state = self.state.lock().map_err(|_| "desktop control lock poisoned")?;
            require_owner(&state.control, request, call.target.control_generation)?;
            if state.input_revoking || state.device_lock.is_none()
                || state.input_lease.as_ref().is_some_and(|current| current != &lease) {
                return Err("control_changed: native application input has no exact current device lease".into());
            }
            if cancel.try_exists().map_err(|error| error.to_string())? { return Err("application action was already cancelled".into()); }
            authorization::create_record(&pending, &json!({"invocation":context,"call":call}))?;
            state.active_cancel = Some(cancel.clone());
            state.input_lease = Some(lease.clone());
            self.app_observations.lock().map_err(|_| "application observations lock poisoned")?.remove(&key);
        }
        let native = self.with_computer_use_input(&cancel, async {
            let activated = self.executor.execute(json!({"type":"input_lease_begin","lease":lease})).await?;
            if activated["input_lease"] != serde_json::to_value(&lease).map_err(|error| error.to_string())? {
                return Err("native input lease acknowledgement did not identify the exact held device".into());
            }
            self.executor.execute(json!({"type":"app_action","scope":context.scope,"call":call,"cancel_file":cancel})).await
        }).await;
        let interrupted = {
            let mut state = self.state.lock().map_err(|_| "desktop control lock poisoned")?;
            state.active_cancel = None;
            if state.control.mode == "idle" && !state.input_revoking { release_device_lock(&mut state)?; }
            state.control.generation != call.target.control_generation
        };
        let native = native.map_err(|error| format!("effect_uncertain: native application action did not produce a terminal receipt: {error}; not replayed"))?;
        let mut action: protocol::application::ActionResult = serde_json::from_value(native["application_action"].clone())
            .map_err(|error| format!("effect_uncertain: native application receipt malformed: {error}"))?;
        action.validate(&context.scope, &call)?;
        if interrupted && action.completed {
            action.completed = false;
            action.error = Some(protocol::application::ActionError { code: "application_interrupted".into(),
                message: "Control changed before native effect evidence was published; observe again".into(),
                dispatch_attempted: true,
                dispatched_events: action.input_delivery.as_ref().map_or(0, |delivery| delivery.dispatched_events)
                    .saturating_add(action.focus_effects.iter().map(|effect| effect.dispatched_events).sum::<u32>()),
                effect_uncertain: true, native_error_code: None });
        }
        let result = serde_json::to_value(ApplicationResult::Action { schema_version: 3, action }).map_err(|error| error.to_string())?;
        authorization::create_record(&completed, &result)?;
        Ok((self.status_from_native(&native)?, result))
    }

    fn require_computer_use_enabled(&self) -> Result<(), String> {
        match self.application_preferences.as_deref() {
            Some(root) if crate::app_authorization::computer_use_enabled(root)? => Ok(()),
            Some(_) => Err("computer_use_disabled: enable Computer use in Settings before using apps".into()),
            None => Err("computer_use_preference_unavailable: native input has no declared preference store".into()),
        }
    }

    async fn with_computer_use_input<F>(&self, cancel: &Path, input: F) -> Result<Value, String>
    where F: Future<Output = Result<Value, String>> {
        self.require_computer_use_enabled()?;
        tokio::pin!(input);
        let withdrawal = async {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                if let Err(error) = self.require_computer_use_enabled() { break error; }
            }
        };
        tokio::pin!(withdrawal);
        tokio::select! {
            result = &mut input => result,
            reason = &mut withdrawal => {
                let revoked = {
                    let mut state = self.state.lock().map_err(|_| "desktop control lock poisoned")?;
                    std::fs::write(cancel, b"computer_use_disabled").map_err(|error| error.to_string())?;
                    state.control.generation = state.control.generation.checked_add(1).ok_or("desktop control generation overflow")?;
                    state.control.mode = "stopped".into(); state.control.owner = None;
                    state.observations.clear();
                    self.app_observations.lock().map_err(|_| "application observations lock poisoned")?.clear();
                    let lease = state.input_lease.take(); state.input_revoking = lease.is_some();
                    self.persist_control(&state.control)?; lease
                };
                if let Some(lease) = revoked { self.revoke_native_input(lease).await?; }
                input.await.map_err(|error| format!("{reason}; native input was cancelled: {error}"))
            }
        }
    }

    async fn status(&self) -> Result<Evidence, String> {
        let native = self.executor.execute(json!({"type":"status"})).await?;
        self.status_from_native(&native)
    }

    async fn revoke_native_input(&self, lease: protocol::application::InputLease) -> Result<(), String> {
        let result = self.executor.execute(json!({"type":"input_lease_revoke","lease":lease})).await.and_then(|native| {
            if native["input_quiesced"] != true || native["input_lease"] != serde_json::to_value(&lease).map_err(|error| error.to_string())? {
                return Err("native input teardown did not acknowledge the exact lease".into());
            }
            Ok(())
        });
        if let Err(error) = &result {
            self.executor.shutdown().await.map_err(|shutdown|
                format!("input_teardown_uncertain: {error}; helper reap failed: {shutdown}; device lock retained"))?;
            self.app_sources.lock().map_err(|_| "application sources lock poisoned")?.clear();
            self.app_grants.lock().map_err(|_| "application grants lock poisoned")?.clear();
        }
        let mut state = self.state.lock().map_err(|_| "desktop control lock poisoned")?;
        state.input_revoking = false;
        if state.control.mode == "idle" && state.active_cancel.is_none() { release_device_lock(&mut state)?; }
        result.map_err(|error| format!("input_teardown_failed: {error}; owned helper was reaped; no effect replayed"))
    }

    async fn cancel_disconnected_request(&self, request: &Request) -> Result<(), String> {
        if request.action["type"] == "permissions" {
            let pending = self.permission_pending.lock().map_err(|_| "native permission cancellation lock poisoned")?;
            if let Some(entry) = pending.as_ref().filter(|entry| entry.request.request_id == request.request_id
                && entry.request.worker_environment_id == request.worker_environment_id
                && entry.request.tool == request.tool && entry.request.action == request.action) {
                std::fs::write(&entry.cancel_file, b"caller_disconnected").map_err(|error| error.to_string())?;
            }
            return Ok(());
        }
        if !request.action["type"]
            .as_str()
            .is_some_and(|operation| protocol::is_input(operation) || protocol::is_application(operation))
        {
            return Ok(());
        }
        let key = blake3::hash(
            format!("{}:{}", request.worker_environment_id, request.request_id).as_bytes(),
        )
        .to_hex()
        .to_string();
        let cancel = self.root.join(format!("{key}.cancel"));
        let revoked = {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "desktop control lock poisoned")?;
        if state.active_cancel.as_ref() == Some(&cancel) {
            std::fs::write(cancel, b"caller_disconnected").map_err(|e| e.to_string())?;
            state.control = Control {
                mode: "stopped".into(),
                owner: None,
                generation: state
                    .control
                    .generation
                    .checked_add(1)
                    .ok_or("desktop control generation overflow")?,
            };
            state.observations.clear();
            self.app_observations.lock().map_err(|_| "application observations lock poisoned")?.clear();
            self.persist_control(&state.control)?;
            let revoked = state.input_lease.take();
            state.input_revoking = revoked.is_some();
            revoked
        } else { None }
        };
        if let Some(lease) = revoked { self.revoke_native_input(lease).await?; }
        Ok(())
    }

    fn cached_status(&self) -> Result<Evidence, String> {
        let native = self
            .last_native
            .lock()
            .map_err(|_| "native status poisoned")?
            .clone()
            .ok_or("desktop status has not been observed")?;
        self.status_from_native(&native)
    }

    fn status_from_native(&self, native: &Value) -> Result<Evidence, String> {
        let permissions: Permissions = serde_json::from_value(native["permissions"].clone())
            .map_err(|e| format!("native permission protocol: {e}"))?;
        let screens: Vec<Screen> = serde_json::from_value(native["screens"].clone())
            .map_err(|e| format!("native display protocol: {e}"))?;
        for screen in &screens {
            protocol::validate_screen(screen)?;
        }
        *self
            .last_native
            .lock()
            .map_err(|_| "native status poisoned")? =
            Some(json!({"permissions":permissions,"screens":screens}));
        let control = self
            .state
            .lock()
            .map_err(|_| "desktop control lock poisoned")?
            .control
            .clone();
        let status = if control.mode == "stopped" { "stopped" } else {
            match self.application_preferences.as_deref()
                .map(crate::app_authorization::computer_use_enabled).transpose()? {
                None => "unavailable",
                Some(false) => "disabled",
                Some(true) if !permissions.screen_capture || !permissions.accessibility => "permission_required",
                Some(true) => "ready",
            }
        };
        Ok(Evidence {
            status: status.into(),
            permissions,
            control,
            screens,
            observation: None,
            effect: None,
            view_image_ref: None,
            applications: Vec::new(),
            presentation_intent: None,
        })
    }
}

fn application_grant_key(request: &crate::app_authorization::OperatorRequest) -> String {
    blake3::hash(&octopus_descriptors::canonical_bytes(&json!({"scope":request.scope,
        "bundle_id":request.application.bundle_id,"bundle_path":request.application.bundle_path}))).to_hex().to_string()
}

fn require_owner(control: &Control, request: &Request, generation: u64) -> Result<(), String> {
    if control.mode != "model"
        || control.owner.as_deref() != Some(&request.worker_environment_id)
        || control.generation != generation
    {
        return Err("control_changed: acquire control and observe again before input".into());
    }
    Ok(())
}

fn number(value: &Value, key: &str) -> Result<f64, String> {
    value[key]
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{key} must be a finite number"))
}

fn now_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_millis() as u64)
        .map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub native_program: String,
    pub application_behavior: crate::config::ApplicationBehavior,
    #[serde(default)]
    pub application_policy: Option<crate::config::ApplicationPolicy>,
    #[serde(default)]
    pub application_preferences: Option<crate::config::ApplicationPreferences>,
    #[serde(default)]
    pub native_sandbox_mode: octopus_descriptors::ProcessSandboxMode,
    pub native_sandbox_profile: String,
    pub state_directory: String,
    pub device_lock: crate::config::DeviceLock,
    pub connection_file: String,
}

pub async fn run_desktop_host() -> Result<(), String> {
    let interface = PathBuf::from(
        std::env::var("OCTOPUS_INTERFACE_DIR")
            .map_err(|_| "desktop host has no interface directory")?,
    );
    let config = crate::config::DesktopConfig::read(&interface.join("desktop.json"))?;
    let host = config
        .host
        .as_ref()
        .ok_or("desktop.json has no host configuration")?;
    let roots: BTreeMap<String, PathBuf> = serde_json::from_str(
        &std::env::var("OCTOPUS_ROOT_SLOTS_JSON").map_err(|_| "desktop host has no root slots")?,
    )
    .map_err(|e| e.to_string())?;
    let kinds = roots
        .get("registry_worker_environment_kind_root")
        .ok_or("desktop host needs registry_worker_environment_kind_root")?;
    let preferences = host.application_preferences.as_ref().map(|preferences| {
        preferences.validate()?;
        let path = roots.get(&preferences.root_slot).ok_or("application preferences root slot was not declared to this desktop host")?;
        crate::native_connection::validate_directory(path)?;
        Ok::<PathBuf, String>(path.clone())
    }).transpose()?;
    let device_lock = host.device_lock.resolve(&roots)?;
    let native = resolve_member(kinds, &host.native_program)?;
    let instructions = host.application_behavior.instruction_resource.read(kinds)?;
    let private = PathBuf::from(
        std::env::var("OCTOPUS_VAR_DIR").map_err(|_| "desktop host has no private directory")?,
    );
    let state = resolve_new_member(&private, &host.state_directory)?;
    let interface_id = std::env::var("OCTOPUS_INTERFACE_ID").map_err(|_| "desktop host has no exact interface identity")?;
    let mut publication = crate::native_connection::HostPublication::prepare(&private, &interface_id)?;
    let token = publication.connection.token.clone();
    let url = url::Url::parse(&config.action_url).map_err(|e| e.to_string())?;
    let ip: std::net::IpAddr = url
        .host_str()
        .ok_or("desktop host has no address")?
        .parse()
        .map_err(|_| "native desktop host must bind an explicit loopback IP")?;
    if !ip.is_loopback() || url.scheme() != "http" || url.path() != "/v1/action" {
        return Err("native desktop host requires loopback http /v1/action".into());
    }
    let listener = tokio::net::TcpListener::bind((
        ip,
        url.port()
            .ok_or("native desktop host requires an explicit port")?,
    ))
    .await
    .map_err(|e| format!("desktop host bind failed: {e}"))?;
    let address = listener.local_addr().map_err(|_| "desktop native bound address unavailable")?;
    let device_file = Arc::new(open_device_file(&device_lock)?);
    let host = Arc::new(
        DesktopHost::with_device_file(
            Arc::new({
                let executor = crate::native_client::NativeExecutor::new(
                    native,
                    config.max_response_bytes,
                    config.timeout(),
                ).with_device_file(device_file.clone())?.with_application_behavior(host.application_behavior.clone())?
                    .with_application_instructions(instructions)?.with_application_policy(host.application_policy.clone())?;
                match host.native_sandbox_mode {
                    octopus_descriptors::ProcessSandboxMode::Enforced => executor.with_sandbox(
                        resolve_member(kinds, &host.native_sandbox_profile)?, state.clone(),
                    )?,
                    octopus_descriptors::ProcessSandboxMode::Disabled => executor,
                }
            }),
            state,
            device_file,
        )?
        .with_app_approval(config.app_approval.clone(), publication.connection.host_generation.clone())
        .with_application_behavior(host.application_behavior.clone())
        .with_application_policy(host.application_policy.clone())
        .with_application_preferences(preferences)?
        .with_presentation_timeout(config.timeout()),
    );
    host.status().await?;
    publication.publish(address)?;
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(b"{\"kind\":\"ready\"}\n")
        .await
        .map_err(|e| format!("desktop life readiness write failed: {e}"))?;
    stdout
        .flush()
        .await
        .map_err(|e| format!("desktop life readiness flush failed: {e}"))?;
    let mut stdin = tokio::io::stdin();
    let mut input = [0u8; 1024];
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|e| e.to_string())?;
                let host = host.clone(); let token = token.clone();
                tokio::spawn(async move { let _ = connection(stream, host, &token).await; });
            },
            read = stdin.read(&mut input) => { if read.map_err(|e| e.to_string())? == 0 { break; } }
        }
    }
    if let Some(path) = &host
        .state
        .lock()
        .map_err(|_| "desktop control lock poisoned")?
        .active_cancel
    {
        std::fs::write(path, b"shutdown").map_err(|e| e.to_string())?;
    }
    let _drained = host.input_gate.lock().await;
    host.executor.shutdown().await?;
    Ok(())
}

fn resolve_member(root: &Path, member: &str) -> Result<PathBuf, String> {
    let candidate = resolve_new_member(root, member)?
        .canonicalize()
        .map_err(|e| format!("desktop native helper unavailable: {e}"))?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if !candidate.starts_with(root) || !candidate.is_file() {
        return Err("desktop native helper escapes its declared owner".into());
    }
    Ok(candidate)
}

fn resolve_new_member(root: &Path, member: &str) -> Result<PathBuf, String> {
    if member.is_empty()
        || !Path::new(member)
            .components()
            .all(|v| matches!(v, std::path::Component::Normal(_)))
    {
        return Err("desktop package paths must be relative normal components".into());
    }
    Ok(root.join(member))
}

async fn connection(
    mut stream: tokio::net::TcpStream,
    host: Arc<DesktopHost>,
    token: &str,
) -> Result<(), String> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let boundary = loop {
        let count = stream.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("desktop client disconnected".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            break end + 4;
        }
        if bytes.len() > 16384 {
            return Err("desktop HTTP headers too large".into());
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary]).map_err(|e| e.to_string())?;
    let mut lines = head.lines();
    let first = lines.next().ok_or("desktop HTTP request is empty")?;
    let route = first
        .strip_prefix("POST ")
        .and_then(|v| v.strip_suffix(" HTTP/1.1"))
        .ok_or("desktop host requires POST HTTP/1.1")?;
    if !matches!(route, "/v1/action" | "/v1/view" | "/v1/operator") {
        return Err("desktop route not found".into());
    }
    let operator = route != "/v1/action";
    let permission_route = route == "/v1/operator";
    let mut headers = BTreeMap::new();
    for line in lines.filter(|v| !v.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or("desktop HTTP header malformed")?;
        if headers
            .insert(key.to_ascii_lowercase(), value.trim())
            .is_some()
        {
            return Err("duplicate desktop HTTP header".into());
        }
    }
    if headers.contains_key("origin") || headers.contains_key("transfer-encoding") {
        return Err("desktop host accepts authenticated host clients only".into());
    }
    let expected = format!("Bearer {token}");
    let supplied = headers.get("authorization").copied().unwrap_or("");
    if supplied.len() != expected.len()
        || supplied
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            != 0
    {
        return Err("desktop authentication denied".into());
    }
    let length: usize = headers
        .get("content-length")
        .ok_or("desktop Content-Length is required")?
        .parse()
        .map_err(|_| "desktop Content-Length invalid")?;
    if length > 1024 * 1024 {
        return Err("desktop request too large".into());
    }
    while bytes.len() < boundary + length {
        let count = stream.read(&mut buffer).await.map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("desktop request body truncated".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let request: Request =
        serde_json::from_slice(&bytes[boundary..boundary + length]).map_err(|e| e.to_string())?;
    if matches!(request.action["type"].as_str(), Some("permissions" | "permission_settings_source" | "application_permissions")) && !permission_route {
        return Err("native permission access requires the private operator endpoint".into());
    }
    let running = host.handle(request.clone(), operator);
    tokio::pin!(running);
    let mut extra = [0u8; 1];
    let response = tokio::select! {
        biased;
        result = &mut running => result,
        disconnected = stream.read(&mut extra) => {
            host.cancel_disconnected_request(&request).await?;
            let _completed = running.await;
            return Err(match disconnected { Ok(0) => "desktop caller disconnected; inspect native outcome before retrying".into(), Ok(_) => "desktop HTTP pipelining is unsupported".into(), Err(error) => error.to_string() });
        }
    };
    let body = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
    if matches!(request.action["type"].as_str(), Some("permissions" | "permission_settings_source")) && body.len() > protocol::permission::MAX_BYTES {
        return Err("native permission response exceeds its control bound".into());
    }
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.map_err(|e| e.to_string())?;
    stream.write_all(&body).await.map_err(|e| e.to_string())?;
    stream.shutdown().await.map_err(|e| e.to_string())
}

#[cfg(unix)]
pub fn open_device_file(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| format!("desktop device lock '{}' unavailable: {e}", path.display()))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600 || metadata.nlink() != 1
    {
        return Err("desktop device lock must be an owner-only regular file".into());
    }
    Ok(file)
}

#[cfg(unix)]
fn acquire_device_lock(file: &std::fs::File) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("desktop_device_busy: another host owns this physical computer".into());
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn open_device_file(_path: &Path) -> Result<std::fs::File, String> {
    Err("desktop native input is unavailable on this platform".into())
}

fn release_device_lock(state: &mut DeviceState) -> Result<(), String> {
    if state.input_lease.is_some() || state.input_revoking {
        return Err("native input must be quiesced before releasing the physical device".into());
    }
    #[cfg(unix)]
    if let Some(file) = &state.device_lock {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } != 0 {
            return Err("native physical device unlock failed".into());
        }
    }
    state.device_lock = None;
    Ok(())
}

#[cfg(target_os = "macos")]
fn presentation_clock() -> Result<u64, String> {
    unsafe extern "C" { fn mach_continuous_time() -> u64; }
    Ok(unsafe { mach_continuous_time() })
}
#[cfg(not(target_os = "macos"))]
fn presentation_clock() -> Result<u64, String> { Err("native presentation requires the macOS continuous clock".into()) }

#[cfg(target_os = "macos")]
fn presentation_deadline(timeout: std::time::Duration) -> Result<u64, String> {
    #[repr(C)] struct Timebase { numer: u32, denom: u32 }
    unsafe extern "C" { fn mach_timebase_info(value: *mut Timebase) -> i32; }
    let mut value = Timebase { numer: 0, denom: 0 };
    if unsafe { mach_timebase_info(&mut value) } != 0 || value.numer == 0 || value.denom == 0 {
        return Err("native continuous timebase unavailable".into());
    }
    let ticks = timeout.as_nanos().checked_mul(u128::from(value.denom)).ok_or("native deadline overflow")? / u128::from(value.numer);
    presentation_clock()?.checked_add(u64::try_from(ticks).map_err(|_| "native deadline overflow")?).ok_or_else(|| "native deadline overflow".into())
}
#[cfg(not(target_os = "macos"))]
fn presentation_deadline(_: std::time::Duration) -> Result<u64, String> { Err("native presentation requires macOS".into()) }
