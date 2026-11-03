mod auth;
mod compatibility;
mod config;
mod handler;
mod intake;
pub mod intake_state;
pub use intake::interface_life as input_intake_life;
mod life;
mod lifecycle;
mod live_stream;
mod managed_cli;
mod opencodex;
pub use life::{
    doctor_native_interface, doctor_opencodex_interface, doctor_requested, interface_life,
    opencodex_interface_life,
};
mod path;
mod peer_reader;
mod remote;
mod result_media;
mod run_affordances;
mod rustls_provider;
mod spawn;
mod uds;

use std::error::Error;
use std::fmt;
use std::io::Error as IoError;
use std::time::Duration;

use octopus_appserver_proto::ClientNotification;
use octopus_appserver_proto::ClientRequest;
use octopus_appserver_proto::JSONRPCErrorError;
use octopus_appserver_proto::JSONRPCNotification;
use octopus_appserver_proto::RequestId;
use octopus_appserver_proto::Result as JsonRpcResult;
use octopus_appserver_proto::ServerNotification;
use octopus_appserver_proto::ServerRequest;
use octopus_util_abspath::AbsolutePathBuf;
use serde::de::DeserializeOwned;

pub use crate::compatibility::{CodexCompatibilityEvidence, CodexCompatibilityRequirements, CODEX_SCHEMA_BASELINE};
pub use crate::managed_cli::{verify_managed_cli, ManagedCliImport, ManagedCliReceipt, ManagedCliSigning, MANAGED_CLI_RECEIPT};
pub use crate::config::{
    AppServerSource, CodexInstanceConfig, ConfigError, CuratedHomeError, ENV_APP_SERVER_SOCKET,
    ENV_APP_SERVER_SPAWN, ENV_APP_SERVER_SPAWN_ARGS, ENV_APP_SERVER_TOKEN, ENV_APP_SERVER_URL,
    ENV_CODEX_HOME, InstanceConfigError, IsolatedCwdError, SPAWN_DEFAULT_BIN,
    SPAWN_DEFAULT_SUBCOMMAND, SPAWN_ISOLATED_CWD, SPAWN_ISOLATION_CONFIG_OVERRIDES, SpawnArgsError,
    auth_manager_from_provider_home, build_curated_codex_home, connect_from_descriptor_with_auth,
    connect_from_env, connect_from_env_with_auth,
    connect_interface_driver_from_descriptor_with_auth,
    connect_interface_driver_from_descriptor_with_auth_and_secrets,
    connect_interface_driver_from_env_with_auth,
};
pub use crate::handler::{
    CODEX_APP_SERVER_INTERFACE_DRIVER_REF, CodexAppServerInterfaceDriver, into_interface_driver,
    into_interface_driver_with_config,
};
pub use crate::path::AppServerPath;
pub use crate::remote::RemoteAppServerClient;
pub use crate::remote::RemoteAppServerConnectArgs;
pub use crate::remote::RemoteAppServerEndpoint;
pub use crate::remote::RemoteAppServerRequestHandle;
pub use crate::spawn::SpawnAppServerClient;
pub use crate::spawn::SpawnAppServerRequestHandle;
pub use crate::spawn::SpawnConfig;

pub(crate) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

pub const DEFAULT_IN_PROCESS_CHANNEL_CAPACITY: usize = 256;

pub type RequestResult = std::result::Result<JsonRpcResult, JSONRPCErrorError>;

#[derive(Debug, Clone)]
pub enum AppServerEvent {
    Lagged { skipped: usize },
    ServerNotification(ServerNotification),
    UnknownServerNotification(JSONRPCNotification),
    ServerRequest(ServerRequest),
    Disconnected { message: String },
}

impl AppServerEvent {
    pub(crate) fn from_notification(notification: JSONRPCNotification) -> Self {
        match ServerNotification::try_from(notification.clone()) {
            Ok(notification) => Self::ServerNotification(notification),
            Err(_) => Self::UnknownServerNotification(notification),
        }
    }
}

#[derive(Debug)]
pub enum TypedRequestError {
    Transport {
        method: String,
        source: IoError,
    },
    Server {
        method: String,
        source: JSONRPCErrorError,
    },
    Deserialize {
        method: String,
        source: serde_json::Error,
    },
}

impl fmt::Display for TypedRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { method, source } => {
                write!(f, "{method} transport error: {source}")
            }
            Self::Server { method, source } => {
                write!(
                    f,
                    "{method} failed: {} (code {})",
                    source.message, source.code
                )?;
                if let Some(data) = source.data.as_ref() {
                    write!(f, ", data: {data}")?;
                }
                Ok(())
            }
            Self::Deserialize { method, source } => {
                write!(f, "{method} response decode error: {source}")
            }
        }
    }
}

impl Error for TypedRequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Transport { source, .. } => Some(source),
            Self::Server { .. } => None,
            Self::Deserialize { source, .. } => Some(source),
        }
    }
}

pub enum AppServerClient {
    Remote(RemoteAppServerClient),
    Spawn(SpawnAppServerClient),
}

#[derive(Clone)]
pub enum AppServerRequestHandle {
    Remote(RemoteAppServerRequestHandle),
    Spawn(SpawnAppServerRequestHandle),
    Unavailable,
}

impl AppServerRequestHandle {
    fn unavailable_io_error(request: &ClientRequest) -> IoError {
        IoError::other(format!(
            "{}: no remote app-server transport (PURE-EMBEDDED boot — the embedded spine drives turns \
             locally; Codex is bound only as a provider interface). Failing closed rather than \
             fabricating a daemon response.",
            request_method_name(request)
        ))
    }

    pub async fn request(&self, request: ClientRequest) -> std::io::Result<RequestResult> {
        match self {
            Self::Remote(handle) => handle.request(request).await,
            Self::Spawn(handle) => handle.request(request).await,
            Self::Unavailable => Err(Self::unavailable_io_error(&request)),
        }
    }

    pub async fn request_typed<T>(&self, request: ClientRequest) -> Result<T, TypedRequestError>
    where
        T: DeserializeOwned,
    {
        match self {
            Self::Remote(handle) => handle.request_typed(request).await,
            Self::Spawn(handle) => handle.request_typed(request).await,
            Self::Unavailable => {
                let method = request_method_name(&request);
                Err(TypedRequestError::Transport {
                    source: Self::unavailable_io_error(&request),
                    method,
                })
            }
        }
    }
}

impl AppServerClient {
    pub(crate) fn compatibility(&self) -> Option<&CodexCompatibilityEvidence> {
        match self {
            Self::Spawn(client) => client.compatibility(),
            Self::Remote(_) => None,
        }
    }

    pub(crate) fn server_version(&self) -> Option<&str> {
        match self {
            Self::Remote(client) => client.server_version(),
            Self::Spawn(client) => client.server_version(),
        }
    }

    pub(crate) fn transport_kind(&self) -> &'static str {
        match self {
            Self::Remote(_) => "remote",
            Self::Spawn(_) => "spawn",
        }
    }

    pub(crate) fn app_server_codex_home(&self) -> Option<&str> {
        match self {
            Self::Remote(client) => client.codex_home(),
            Self::Spawn(client) => client.codex_home(),
        }
    }

    pub fn codex_home(&self, local_codex_home: &AbsolutePathBuf) -> Option<AppServerPath> {
        let _ = &local_codex_home;
        match self {
            Self::Remote(client) => client.codex_home().map(AppServerPath::from_app_server),
            Self::Spawn(client) => client.codex_home().map(AppServerPath::from_app_server),
        }
    }

    pub async fn request(&self, request: ClientRequest) -> std::io::Result<RequestResult> {
        match self {
            Self::Remote(remote) => remote.request(request).await,
            Self::Spawn(spawned) => spawned.request(request).await,
        }
    }

    pub async fn request_typed<T>(&self, request: ClientRequest) -> Result<T, TypedRequestError>
    where
        T: DeserializeOwned,
    {
        match self {
            Self::Remote(remote) => remote.request_typed(request).await,
            Self::Spawn(spawned) => spawned.request_typed(request).await,
        }
    }

    pub async fn notify(&self, notification: ClientNotification) -> std::io::Result<()> {
        match self {
            Self::Remote(client) => client.notify(notification).await,
            Self::Spawn(client) => client.notify(notification).await,
        }
    }

    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: JsonRpcResult,
    ) -> std::io::Result<()> {
        match self {
            Self::Remote(client) => client.resolve_server_request(request_id, result).await,
            Self::Spawn(client) => client.resolve_server_request(request_id, result).await,
        }
    }

    pub async fn reject_server_request(
        &self,
        request_id: RequestId,
        error: JSONRPCErrorError,
    ) -> std::io::Result<()> {
        match self {
            Self::Remote(client) => client.reject_server_request(request_id, error).await,
            Self::Spawn(client) => client.reject_server_request(request_id, error).await,
        }
    }

    pub async fn next_event(&mut self) -> Option<AppServerEvent> {
        match self {
            Self::Remote(client) => client.next_event().await,
            Self::Spawn(client) => client.next_event().await,
        }
    }

    pub(crate) fn take_ready_events(&mut self) -> Vec<AppServerEvent> {
        match self {
            Self::Remote(client) => client.take_ready_events(),
            Self::Spawn(client) => client.take_ready_events(),
        }
    }

    pub(crate) fn restore_ready_events(&mut self, events: Vec<AppServerEvent>) {
        match self {
            Self::Remote(client) => client.restore_ready_events(events),
            Self::Spawn(client) => client.restore_ready_events(events),
        }
    }

    pub async fn shutdown(self) -> std::io::Result<()> {
        match self {
            Self::Remote(client) => client.shutdown().await,
            Self::Spawn(client) => client.shutdown().await,
        }
    }

    pub fn request_handle(&self) -> AppServerRequestHandle {
        match self {
            Self::Remote(remote) => AppServerRequestHandle::Remote(remote.request_handle()),
            Self::Spawn(spawned) => AppServerRequestHandle::Spawn(spawned.request_handle()),
        }
    }
}

pub(crate) fn request_method_name(request: &ClientRequest) -> String {
    serde_json::to_value(request)
        .ok()
        .and_then(|value| {
            value
                .get("method")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "<unknown>".to_string())
}
