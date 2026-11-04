use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use octopus_interfaces::{InterfaceDescriptor, InterfaceDriverConfig};
use octopus_login::{AuthCredentialsStoreMode, AuthManager};
use octopus_util_abspath::AbsolutePathBuf;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use octopus_appserver_proto::{ClientInfo, InitializeCapabilities, InitializeParams};

use crate::auth::OctopusScopedSecretStore;
use crate::handler::CodexAppServerInterfaceDriver;
use crate::spawn::{SpawnAppServerClient, SpawnConfig};
use crate::{AppServerClient, DEFAULT_IN_PROCESS_CHANNEL_CAPACITY};

pub const ENV_APP_SERVER_URL: &str = "CODEX_APP_SERVER_URL";
pub const ENV_APP_SERVER_SOCKET: &str = "CODEX_APP_SERVER_SOCKET";
pub const ENV_APP_SERVER_TOKEN: &str = "CODEX_APP_SERVER_TOKEN";

pub const ENV_APP_SERVER_SPAWN: &str = "CODEX_APP_SERVER_SPAWN";
pub const ENV_APP_SERVER_SPAWN_ARGS: &str = "CODEX_APP_SERVER_SPAWN_ARGS";
pub const ENV_CODEX_HOME: &str = "CODEX_HOME";

const MANAGED_CLI_RELEASE: &str = include_str!("../managed-cli/release.json");

#[derive(Deserialize)]
struct ManagedCliRelease {
    notifications: ManagedCliNotificationPolicy,
}

#[derive(Deserialize)]
struct ManagedCliNotificationPolicy {
    opt_out: Vec<String>,
}

static NOTIFICATION_OPT_OUT: LazyLock<Vec<String>> = LazyLock::new(|| {
    serde_json::from_str::<ManagedCliRelease>(MANAGED_CLI_RELEASE)
        .map(|release| release.notifications.opt_out)
        .unwrap_or_else(|error| {
            panic!("managed-cli/release.json must declare its app-server notification policy: {error}")
        })
});

pub async fn auth_manager_from_provider_home(provider_home: PathBuf) -> Arc<AuthManager> {
    AuthManager::shared(provider_home, true, AuthCredentialsStoreMode::File).await
}

fn provider_home_from_env() -> PathBuf {
    if let Some(home) = std::env::var_os(ENV_CODEX_HOME).filter(|v| !v.is_empty()) {
        return PathBuf::from(home);
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".codex"))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

pub const SPAWN_DEFAULT_BIN: &str = "codex";
pub const SPAWN_DEFAULT_SUBCOMMAND: &str = "app-server";

pub const SPAWN_ISOLATED_CWD: &str = "codex-cwd";

pub const SPAWN_ISOLATION_CONFIG_OVERRIDES: &[&str] = &[
    "project_doc_max_bytes=0",
    "suppress_unstable_features_warning=true",
    "model_context_window=400000",
    "model_auto_compact_token_limit=350000",
    "features.shell_tool=false",
    "features.unified_exec=false",
    "features.shell_snapshot=false",
    "features.hooks=false",
    "features.multi_agent=false",
    "features.multi_agent_v2=false",
    "agents.enabled=false",
    "features.apps=false",
    "features.plugins=false",
    "features.memories=false",
    "features.tool_suggest=false",
    "features.in_app_browser=false",
    "features.browser_use=false",
    "features.browser_use_full_cdp_access=false",
    "features.browser_use_external=false",
    "features.computer_use=false",
    "features.remote_plugin=false",
    "features.plugin_sharing=false",
    "features.plugin_discovery_prefetch=false",
    "features.skill_mcp_dependency_install=false",
    "features.skill_search=false",
    "features.mentions_v2=false",
    "features.guardian_approval=false",
    "features.tool_call_mcp_elicitation=false",
    "features.auth_elicitation=false",
    "features.personality=false",
    "features.workspace_dependencies=false",
    "features.cli_daybreak=false",
    "features.code_mode_host=true",
    "features.instant_interrupt=true",
    "features.code_mode_interrupt=true",
    "features.unified_image_budget=true",
    "features.image_resize_notice=true",
    "features.incremental_tools=true",
    "features.reasoning_effort_override=true",
    "features.retain_client_tool_outputs=true",
    "features.mcp_oauth_refresh_coordination=true",
    "personality=none",
    "sandbox_mode=read-only",
    "skills.include_instructions=false",
    "include_apps_instructions=false",
    "include_permissions_instructions=false",
    "features.goals=false",
    "features.image_generation=false",
    "features.tool_search=false",
    "features.search_tool=false",
    "features.request_permissions_tool=false",
    "features.sleep_tool=false",
    "features.current_time_reminder=false",
    "tools.experimental_request_user_input.enabled=true",
    "features.default_mode_request_user_input=true",
];

const RESIDUAL_AMBIENT_SPAWN_KEY: &str = "spawn";

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexInstanceConfig {
    #[serde(default)]
    pub managed_cli: Option<crate::ManagedCliImport>,
    #[serde(default)]
    pub compatibility: Option<crate::CodexCompatibilityRequirements>,
    #[serde(default)]
    pub subcommand: Option<String>,
    #[serde(default)]
    pub extra_argv: Vec<String>,
    #[serde(default)]
    pub isolation_overrides: Option<Vec<String>>,
    #[serde(default)]
    pub home_env: Option<String>,
    #[serde(default)]
    pub isolated_cwd: Option<String>,
    #[serde(default)]
    pub connect_on_ready: bool,
    #[serde(default)]
    pub idle_disconnect_ms: Option<u64>,
}

#[derive(Debug)]
pub enum InstanceConfigError {
    Compatibility(String),
    Malformed(serde_json::Error),
    ResidualAmbientNotObject,
    EmptySubcommand,
    EmptyHomeEnv,
    UnsupportedHomeEnv(String),
    EmptyIsolatedCwd,
    ZeroIdleDisconnect,
    ReadyTransportMayNotExpire,
}

impl std::fmt::Display for InstanceConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compatibility(message) => write!(f, "invalid Codex compatibility requirements: {message}"),
            Self::Malformed(source) => write!(
                f,
                "interface_driver_config.residual_ambient.{RESIDUAL_AMBIENT_SPAWN_KEY} is malformed: \
                 {source}"
            ),
            Self::ResidualAmbientNotObject => write!(
                f,
                "interface_driver_config.residual_ambient must be a JSON object to carry a \
                 `{RESIDUAL_AMBIENT_SPAWN_KEY}` config block"
            ),
            Self::EmptySubcommand => write!(
                f,
                "codex instance config `{RESIDUAL_AMBIENT_SPAWN_KEY}.subcommand` must be non-empty"
            ),
            Self::EmptyHomeEnv => write!(
                f,
                "codex instance config `{RESIDUAL_AMBIENT_SPAWN_KEY}.home_env` must be a non-empty \
                 env var NAME"
            ),
            Self::UnsupportedHomeEnv(value) => write!(
                f,
                "codex instance config `{RESIDUAL_AMBIENT_SPAWN_KEY}.home_env` must be \
                 `{ENV_CODEX_HOME}`, got `{value}`; exact interface auth cannot be redirected to \
                 an ambient home"
            ),
            Self::EmptyIsolatedCwd => write!(
                f,
                "codex instance config `{RESIDUAL_AMBIENT_SPAWN_KEY}.isolated_cwd` must be a \
                 non-empty path"
            ),
            Self::ZeroIdleDisconnect => write!(
                f,
                "codex instance config `{RESIDUAL_AMBIENT_SPAWN_KEY}.idle_disconnect_ms` must be positive"
            ),
            Self::ReadyTransportMayNotExpire => write!(
                f,
                "codex instance config `{RESIDUAL_AMBIENT_SPAWN_KEY}` cannot combine \
                 `connect_on_ready=true` with `idle_disconnect_ms`; a ready-connected transport \
                 must remain ready for the interface life"
            ),
        }
    }
}

impl std::error::Error for InstanceConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Malformed(source) => Some(source),
            Self::Compatibility(_)
            | Self::ResidualAmbientNotObject
            | Self::EmptySubcommand
            | Self::EmptyHomeEnv
            | Self::UnsupportedHomeEnv(_)
            | Self::EmptyIsolatedCwd
            | Self::ZeroIdleDisconnect
            | Self::ReadyTransportMayNotExpire => None,
        }
    }
}

impl CodexInstanceConfig {
    pub fn from_driver_config(
        config: Option<&InterfaceDriverConfig>,
    ) -> Result<Self, InstanceConfigError> {
        let Some(residual) = config.and_then(|c| c.residual_ambient.as_ref()) else {
            return Ok(Self::default());
        };
        let residual = residual
            .as_object()
            .ok_or(InstanceConfigError::ResidualAmbientNotObject)?;
        let Some(spawn) = residual.get(RESIDUAL_AMBIENT_SPAWN_KEY) else {
            return Ok(Self::default());
        };
        let parsed: Self =
            serde_json::from_value(spawn.clone()).map_err(InstanceConfigError::Malformed)?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn from_descriptor(descriptor: &InterfaceDescriptor) -> Result<Self, InstanceConfigError> {
        Self::from_driver_config(descriptor.interface_driver_config.as_ref())
    }

    fn validate(&self) -> Result<(), InstanceConfigError> {
        if let Some(import) = &self.managed_cli {
            import.validate().map_err(InstanceConfigError::Compatibility)?;
        }
        if let Some(requirements) = &self.compatibility {
            requirements.validate().map_err(InstanceConfigError::Compatibility)?;
        }
        if matches!(self.subcommand.as_deref(), Some(s) if s.trim().is_empty()) {
            return Err(InstanceConfigError::EmptySubcommand);
        }
        if matches!(self.home_env.as_deref(), Some(s) if s.trim().is_empty()) {
            return Err(InstanceConfigError::EmptyHomeEnv);
        }
        if let Some(home_env) = self.home_env.as_deref()
            && home_env.trim() != ENV_CODEX_HOME
        {
            return Err(InstanceConfigError::UnsupportedHomeEnv(
                home_env.to_string(),
            ));
        }
        if matches!(self.isolated_cwd.as_deref(), Some(s) if s.trim().is_empty()) {
            return Err(InstanceConfigError::EmptyIsolatedCwd);
        }
        if self.idle_disconnect_ms == Some(0) {
            return Err(InstanceConfigError::ZeroIdleDisconnect);
        }
        if self.connect_on_ready && self.idle_disconnect_ms.is_some() {
            return Err(InstanceConfigError::ReadyTransportMayNotExpire);
        }
        Ok(())
    }

    fn subcommand(&self) -> &str {
        self.subcommand
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(SPAWN_DEFAULT_SUBCOMMAND)
    }

    fn isolation_overrides(&self) -> Vec<String> {
        match self.isolation_overrides.as_ref() {
            Some(overrides) => overrides.clone(),
            None => SPAWN_ISOLATION_CONFIG_OVERRIDES
                .iter()
                .map(|kv| (*kv).to_string())
                .collect(),
        }
    }

    fn home_env(&self) -> &str {
        self.home_env
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(ENV_CODEX_HOME)
    }

    fn isolated_cwd(&self) -> &str {
        self.isolated_cwd
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(SPAWN_ISOLATED_CWD)
    }

    pub fn connect_on_ready(&self) -> bool {
        self.connect_on_ready
    }

    pub fn idle_disconnect(&self) -> Option<std::time::Duration> {
        self.idle_disconnect_ms
            .map(std::time::Duration::from_millis)
    }
}

const CODEX_AUTH_FILE_NAME: &str = "auth.json";
const CODEX_AUTH_LOCK_FILE_NAME: &str = "auth.lock";
const CODEX_AUTH_BOUNDARY_FILES: [&str; 2] = [CODEX_AUTH_FILE_NAME, CODEX_AUTH_LOCK_FILE_NAME];
const CODEX_CONFIG_FILE_NAME: &str = "config.toml";
const CURATED_CODEX_HOME_DIR: &str = "app-server-home";
const CURATED_OPENCODEX_HOME_DIR: &str = "opencodex-app-server-home";
pub(crate) const CURATED_CODEX_TMP_DIR: &str = "tmp";
const CURATED_CONFIG_TOML: &str = "\
project_doc_max_bytes = 0
personality = \"none\"
sandbox_mode = \"read-only\"
web_search = \"disabled\"
include_apps_instructions = false
include_permissions_instructions = false
cli_auth_credentials_store = \"file\"
suppress_unstable_features_warning = true

[agents]
enabled = false

[features]
shell_tool = false
unified_exec = false
shell_snapshot = false
hooks = false
multi_agent = false
multi_agent_v2 = false
apps = false
plugins = false
memories = false
tool_suggest = false
in_app_browser = false
browser_use = false
browser_use_full_cdp_access = false
browser_use_external = false
computer_use = false
remote_plugin = false
plugin_sharing = false
plugin_discovery_prefetch = false
skill_mcp_dependency_install = false
skill_search = false
mentions_v2 = false
guardian_approval = false
tool_call_mcp_elicitation = false
auth_elicitation = false
personality = false
workspace_dependencies = false
cli_daybreak = false
code_mode_host = true
instant_interrupt = true
code_mode_interrupt = true
unified_image_budget = true
image_resize_notice = true
incremental_tools = true
reasoning_effort_override = true
retain_client_tool_outputs = true
mcp_oauth_refresh_coordination = true
goals = false
image_generation = false
tool_search = false
search_tool = false
request_permissions_tool = false
sleep_tool = false
current_time_reminder = false
default_mode_request_user_input = true

[skills]
include_instructions = false

[tools.experimental_request_user_input]
enabled = true
";

#[derive(Debug, Clone)]
pub enum AppServerSource {
    WebSocket {
        url: String,
        token: Option<String>,
    },
    UnixSocket {
        path: AbsolutePathBuf,
    },
    Spawn {
        bin: String,
        args: Vec<String>,
        instance: CodexInstanceConfig,
    },
}

#[derive(Debug)]
pub enum ConfigError {
    NoEndpoint,
    SocketNotAbsolute(String),
    UnisolatedRemoteTransport(&'static str),
    Connect(std::io::Error),
    CuratedHome(CuratedHomeError),
    SpawnArgs(SpawnArgsError),
    IsolatedCwd(IsolatedCwdError),
    InstanceConfig(InstanceConfigError),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoEndpoint => write!(
                f,
                "no curated app-server provider transport configured: set {ENV_APP_SERVER_SPAWN}=1 \
                 (remote URL/socket transports are not accepted as artifact-pure boundaries)"
            ),
            Self::SocketNotAbsolute(path) => {
                write!(f, "{ENV_APP_SERVER_SOCKET} is not an absolute path: {path}")
            }
            Self::UnisolatedRemoteTransport(kind) => write!(
                f,
                "{kind} app-server transport is not an Octopus-curated provider boundary; use \
                 {ENV_APP_SERVER_SPAWN}=1 so Octopus spawns codex with a curated CODEX_HOME/cwd"
            ),
            Self::Connect(source) => write!(f, "app-server connect failed: {source}"),
            Self::CuratedHome(source) => {
                write!(f, "could not build the isolated codex home: {source}")
            }
            Self::SpawnArgs(source) => write!(f, "invalid codex spawn argv: {source}"),
            Self::IsolatedCwd(source) => write!(f, "invalid isolated codex cwd: {source}"),
            Self::InstanceConfig(source) => {
                write!(f, "invalid codex instance folder config: {source}")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connect(source) => Some(source),
            Self::CuratedHome(source) => Some(source),
            Self::SpawnArgs(source) => Some(source),
            Self::IsolatedCwd(source) => Some(source),
            Self::InstanceConfig(source) => Some(source),
            Self::NoEndpoint | Self::SocketNotAbsolute(_) | Self::UnisolatedRemoteTransport(_) => {
                None
            }
        }
    }
}

impl ConfigError {
    pub(crate) fn is_retryable_transport_failure(&self) -> bool {
        matches!(self, Self::Connect(_))
    }
}

impl From<InstanceConfigError> for ConfigError {
    fn from(source: InstanceConfigError) -> Self {
        Self::InstanceConfig(source)
    }
}

impl From<CuratedHomeError> for ConfigError {
    fn from(source: CuratedHomeError) -> Self {
        Self::CuratedHome(source)
    }
}

impl From<SpawnArgsError> for ConfigError {
    fn from(source: SpawnArgsError) -> Self {
        Self::SpawnArgs(source)
    }
}

impl From<IsolatedCwdError> for ConfigError {
    fn from(source: IsolatedCwdError) -> Self {
        Self::IsolatedCwd(source)
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(source: std::io::Error) -> Self {
        Self::Connect(source)
    }
}

impl AppServerSource {
    pub fn from_descriptor(descriptor: &InterfaceDescriptor) -> Result<Self, ConfigError> {
        Self::from_instance_config(CodexInstanceConfig::from_descriptor(descriptor)?)
    }

    pub fn from_instance_config(instance: CodexInstanceConfig) -> Result<Self, ConfigError> {
        let bin = match non_empty_env(ENV_APP_SERVER_SPAWN) {
            Some(spawn) if !is_truthy(&spawn) => spawn,
            _ if instance.managed_cli.is_some() => instance.managed_cli.as_ref().expect("declared CLI import").resolve()
                .map_err(|error| ConfigError::Connect(std::io::Error::new(std::io::ErrorKind::InvalidData, error)))?
                .to_string_lossy().into_owned(),
            _ => SPAWN_DEFAULT_BIN.to_string(),
        };
        let args = build_spawn_args_for_instance(
            &instance,
            non_empty_env(ENV_APP_SERVER_SPAWN_ARGS).as_deref(),
        )?;
        Ok(Self::Spawn {
            bin,
            args,
            instance,
        })
    }

    pub fn from_env() -> Result<Self, ConfigError> {
        if non_empty_env(ENV_APP_SERVER_SPAWN).is_some() {
            return Self::from_instance_config(CodexInstanceConfig::default());
        }
        if non_empty_env(ENV_APP_SERVER_URL).is_some() {
            return Err(ConfigError::UnisolatedRemoteTransport("websocket"));
        }
        if let Some(socket) = non_empty_env(ENV_APP_SERVER_SOCKET) {
            let path = AbsolutePathBuf::from_absolute_path(&socket)
                .map_err(|_| ConfigError::SocketNotAbsolute(socket))?;
            let _ = path;
            return Err(ConfigError::UnisolatedRemoteTransport("unix-socket"));
        }
        Err(ConfigError::NoEndpoint)
    }

    pub fn is_self_authenticating_spawn(&self) -> bool {
        matches!(self, Self::Spawn { .. })
    }

    fn transport_kind(&self) -> &'static str {
        match self {
            Self::WebSocket { .. } => "websocket",
            Self::UnixSocket { .. } => "unix-socket",
            Self::Spawn { .. } => "spawn",
        }
    }

    fn spawn_config(
        self,
        provider_home: &std::path::Path,
        auth_store_home: &std::path::Path,
    ) -> Result<SpawnConfig, ConfigError> {
        let transport_kind = self.transport_kind();
        match self {
            Self::Spawn {
                bin,
                args,
                instance,
            } => {
                if instance.home_env() != ENV_CODEX_HOME {
                    return Err(ConfigError::InstanceConfig(
                        InstanceConfigError::UnsupportedHomeEnv(instance.home_env().to_string()),
                    ));
                }
                let mut config = spawn_config_for(
                    bin,
                    args,
                    Some(isolated_spawn_cwd(instance.isolated_cwd())?),
                    Some(build_curated_codex_home_with_auth_store(
                        provider_home,
                        auth_store_home,
                    )?),
                );
                config.compatibility = instance.compatibility.clone();
                Ok(config)
            }
            Self::WebSocket { .. } | Self::UnixSocket { .. } => {
                Err(ConfigError::UnisolatedRemoteTransport(transport_kind))
            }
        }
    }

    pub(crate) fn spawn_config_with_curated_home(
        self,
        codex_home: std::path::PathBuf,
    ) -> Result<SpawnConfig, ConfigError> {
        let transport_kind = self.transport_kind();
        match self {
            Self::Spawn {
                bin,
                args,
                instance,
            } => {
                if instance.home_env() != ENV_CODEX_HOME {
                    return Err(ConfigError::InstanceConfig(
                        InstanceConfigError::UnsupportedHomeEnv(instance.home_env().to_string()),
                    ));
                }
                let mut config = spawn_config_for(
                    bin,
                    args,
                    Some(isolated_spawn_cwd(instance.isolated_cwd())?),
                    Some(codex_home),
                );
                config.compatibility = instance.compatibility.clone();
                Ok(config)
            }
            Self::WebSocket { .. } | Self::UnixSocket { .. } => {
                Err(ConfigError::UnisolatedRemoteTransport(transport_kind))
            }
        }
    }

    pub async fn connect(self) -> Result<AppServerClient, ConfigError> {
        let provider_home = provider_home_from_env();
        self.connect_from_provider_homes(&provider_home, &provider_home)
            .await
    }

    async fn connect_from_provider_homes(
        self,
        provider_home: &std::path::Path,
        auth_store_home: &std::path::Path,
    ) -> Result<AppServerClient, ConfigError> {
        let config = self.spawn_config(provider_home, auth_store_home)?;
        let client = SpawnAppServerClient::connect(config).await?;
        Ok(AppServerClient::Spawn(client))
    }
}

fn spawn_config_for(
    bin: String,
    args: Vec<String>,
    cwd: Option<std::path::PathBuf>,
    codex_home: Option<std::path::PathBuf>,
) -> SpawnConfig {
    SpawnConfig {
        bin,
        args,
        cwd,
        codex_home,
        initialize_params: octopus_initialize_params(),
        compatibility: None,
        channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    }
}

fn octopus_initialize_params() -> InitializeParams {
    InitializeParams {
        client_info: ClientInfo {
            name: "octopus".to_string(),
            title: None,
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        capabilities: Some(InitializeCapabilities {
            experimental_api: true,
            request_attestation: false,
            opt_out_notification_methods: Some(NOTIFICATION_OPT_OUT.clone()),
            mcp_server_openai_form_elicitation: false,
        }),
    }
}

pub async fn connect_from_env() -> Result<AppServerClient, ConfigError> {
    let source = AppServerSource::from_env()?;
    source.connect().await
}

pub async fn connect_from_env_with_auth(
    auth_manager: &Arc<AuthManager>,
) -> Result<AppServerClient, ConfigError> {
    let source = AppServerSource::from_env()?;
    connect_with_auth(source, auth_manager).await
}

pub async fn connect_interface_driver_from_env_with_auth(
    auth_manager: &Arc<AuthManager>,
) -> Result<Arc<CodexAppServerInterfaceDriver>, ConfigError> {
    let client = connect_from_env_with_auth(auth_manager).await?;
    Ok(crate::handler::into_interface_driver(client))
}

pub async fn connect_from_descriptor_with_auth(
    descriptor: &InterfaceDescriptor,
    auth_manager: &Arc<AuthManager>,
    provider_home: &std::path::Path,
) -> Result<AppServerClient, ConfigError> {
    connect_from_descriptor_with_auth_store(descriptor, auth_manager, provider_home, provider_home)
        .await
}

async fn connect_from_descriptor_with_auth_store(
    descriptor: &InterfaceDescriptor,
    _auth_manager: &Arc<AuthManager>,
    provider_home: &std::path::Path,
    auth_store_home: &std::path::Path,
) -> Result<AppServerClient, ConfigError> {
    let source = AppServerSource::from_descriptor(descriptor)?;
    connect_with_auth_from_provider_homes(source, provider_home, auth_store_home).await
}

pub async fn connect_interface_driver_from_descriptor_with_auth(
    descriptor: &InterfaceDescriptor,
    auth_manager: &Arc<AuthManager>,
    provider_home: PathBuf,
) -> Result<Arc<CodexAppServerInterfaceDriver>, ConfigError> {
    let client =
        connect_from_descriptor_with_auth(descriptor, auth_manager, &provider_home).await?;
    Ok(Arc::new(
        CodexAppServerInterfaceDriver::new_with_config_and_auth_boundaries(
            client,
            descriptor.interface_driver_config.as_ref(),
            None,
            auth_manager.clone(),
            provider_home,
        ),
    ))
}

pub async fn connect_interface_driver_from_descriptor_with_auth_and_secrets(
    descriptor: &InterfaceDescriptor,
    auth_manager: &Arc<AuthManager>,
    provider_home: PathBuf,
    auth_store_home: PathBuf,
    activation_secrets_home: PathBuf,
) -> Result<Arc<CodexAppServerInterfaceDriver>, ConfigError> {
    let client = connect_from_descriptor_with_auth_store(
        descriptor,
        auth_manager,
        &provider_home,
        &auth_store_home,
    )
    .await?;
    Ok(Arc::new(
        CodexAppServerInterfaceDriver::new_with_config_and_auth_boundaries(
            client,
            descriptor.interface_driver_config.as_ref(),
            Some(Arc::new(OctopusScopedSecretStore::new(
                activation_secrets_home,
            ))),
            auth_manager.clone(),
            auth_store_home,
        ),
    ))
}

async fn connect_with_auth(
    source: AppServerSource,
    _auth_manager: &Arc<AuthManager>,
) -> Result<AppServerClient, ConfigError> {
    if !source.is_self_authenticating_spawn() {
        return Err(ConfigError::UnisolatedRemoteTransport(
            source.transport_kind(),
        ));
    }
    source.connect().await
}

async fn connect_with_auth_from_provider_homes(
    source: AppServerSource,
    provider_home: &std::path::Path,
    auth_store_home: &std::path::Path,
) -> Result<AppServerClient, ConfigError> {
    if source.is_self_authenticating_spawn() {
        return source
            .connect_from_provider_homes(provider_home, auth_store_home)
            .await;
    }
    Err(ConfigError::UnisolatedRemoteTransport(
        source.transport_kind(),
    ))
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn build_spawn_args_for_instance(
    instance: &CodexInstanceConfig,
    env_prefix: Option<&str>,
) -> Result<Vec<String>, SpawnArgsError> {
    build_spawn_args(instance, env_prefix, None)
}

fn build_spawn_args(
    instance: &CodexInstanceConfig,
    raw_args: Option<&str>,
    model: Option<&str>,
) -> Result<Vec<String>, SpawnArgsError> {
    let subcommand = instance.subcommand();
    let isolation = instance.isolation_overrides();
    let mut args = match raw_args {
        Some(raw) => {
            let args: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
            validate_spawn_args_prefix(&args, subcommand)?;
            reject_interface_model_knobs(&args)?;
            reject_isolation_override_conflicts(&args, &isolation)?;
            args
        }
        None => {
            let mut args = vec![subcommand.to_string()];
            args.extend(instance.extra_argv.iter().cloned());
            reject_interface_model_knobs(&args)?;
            args
        }
    };
    append_isolation_overrides(&mut args, &isolation);
    if let Some(model) = model {
        return Err(SpawnArgsError::ModelOverrideForbidden(model.to_string()));
    }
    Ok(args)
}

#[cfg(test)]
fn build_default_spawn_args(model: Option<&str>) -> Vec<String> {
    build_spawn_args(&CodexInstanceConfig::default(), None, model)
        .expect("the built-in app-server argv is valid")
}

fn append_isolation_overrides(args: &mut Vec<String>, isolation: &[String]) {
    for override_kv in isolation {
        args.push("-c".to_string());
        args.push(override_kv.clone());
    }
}

fn validate_spawn_args_prefix(args: &[String], subcommand: &str) -> Result<(), SpawnArgsError> {
    let Some(first) = args.first() else {
        return Err(SpawnArgsError::Empty);
    };
    if first != subcommand {
        return Err(SpawnArgsError::NotAppServer {
            first: first.clone(),
            expected: subcommand.to_string(),
        });
    }
    Ok(())
}

fn reject_interface_model_knobs(args: &[String]) -> Result<(), SpawnArgsError> {
    for arg in args.iter().skip(1) {
        if is_forbidden_model_arg(arg) {
            return Err(SpawnArgsError::InterfaceModelKnobForbidden {
                arg: arg.clone(),
                primitive: "model",
            });
        }
        if is_forbidden_cwd_arg(arg) {
            return Err(SpawnArgsError::InterfaceModelKnobForbidden {
                arg: arg.clone(),
                primitive: "cwd",
            });
        }
    }
    Ok(())
}

fn is_forbidden_model_arg(arg: &str) -> bool {
    matches!(arg, "--model" | "-m")
        || arg.starts_with("--model=")
        || arg.starts_with("-m=")
        || arg.starts_with("model=")
}

fn is_forbidden_cwd_arg(arg: &str) -> bool {
    matches!(arg, "-C" | "--cwd" | "--workdir" | "--working-directory")
        || arg.starts_with("-C=")
        || arg.starts_with("--cwd=")
        || arg.starts_with("--workdir=")
        || arg.starts_with("--working-directory=")
        || arg.starts_with("cwd=")
        || arg.starts_with("workdir=")
        || arg.starts_with("working_directory=")
}

fn reject_isolation_override_conflicts(
    args: &[String],
    isolation: &[String],
) -> Result<(), SpawnArgsError> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg != "-c" {
            continue;
        }
        let Some(setting) = iter.next() else {
            return Err(SpawnArgsError::DanglingConfigFlag);
        };
        let Some((key, value)) = split_config_kv(setting) else {
            continue;
        };
        if key == "model" {
            return Err(SpawnArgsError::ModelOverrideForbidden(value.to_string()));
        }
        let Some(expected) = expected_isolation_value(key, isolation) else {
            continue;
        };
        if value != expected {
            return Err(SpawnArgsError::ContradictsIsolation {
                key: key.to_string(),
                got: value.to_string(),
                expected: expected.to_string(),
            });
        }
    }
    Ok(())
}

fn split_config_kv(setting: &str) -> Option<(&str, &str)> {
    let (key, value) = setting.split_once('=')?;
    Some((key.trim(), value.trim()))
}

fn expected_isolation_value(key: &str, isolation: &[String]) -> Option<String> {
    isolation.iter().find_map(|override_kv| {
        let (expected_key, expected_value) = split_config_kv(override_kv)?;
        (expected_key == key).then(|| expected_value.to_string())
    })
}

#[derive(Debug)]
pub enum SpawnArgsError {
    Empty,
    NotAppServer {
        first: String,
        expected: String,
    },
    DanglingConfigFlag,
    ContradictsIsolation {
        key: String,
        got: String,
        expected: String,
    },
    InterfaceModelKnobForbidden {
        arg: String,
        primitive: &'static str,
    },
    ModelOverrideForbidden(String),
}

impl std::fmt::Display for SpawnArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(
                f,
                "{ENV_APP_SERVER_SPAWN_ARGS} was set but contained no argv"
            ),
            Self::NotAppServer { first, expected } => write!(
                f,
                "{ENV_APP_SERVER_SPAWN_ARGS} must start with the configured subcommand `{expected}`, \
                 got `{first}`"
            ),
            Self::DanglingConfigFlag => write!(
                f,
                "{ENV_APP_SERVER_SPAWN_ARGS} contains `-c` without a following config value"
            ),
            Self::ContradictsIsolation { key, got, expected } => write!(
                f,
                "{ENV_APP_SERVER_SPAWN_ARGS} tries to set `{key}={got}`, but the curated provider \
                 transport requires `{key}={expected}`"
            ),
            Self::InterfaceModelKnobForbidden { arg, primitive } => write!(
                f,
                "{ENV_APP_SERVER_SPAWN_ARGS} must not pass `{arg}`; provider {primitive} is model \
                 interface data/turn evidence, not spawn argv"
            ),
            Self::ModelOverrideForbidden(model) => write!(
                f,
                "spawn argv must not set model `{model}`; provider model is resolved from the model \
                 interface and passed on thread/start + turn/start"
            ),
        }
    }
}

impl std::error::Error for SpawnArgsError {}

#[derive(Debug)]
pub enum IsolatedCwdError {
    Relative(std::path::PathBuf),
    ParentTraversal(std::path::PathBuf),
    PrivateRootUnavailable,
    Create {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    Missing {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    NotDirectory(std::path::PathBuf),
    ProjectMarker {
        cwd: std::path::PathBuf,
        marker: std::path::PathBuf,
    },
}

impl std::fmt::Display for IsolatedCwdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relative(path) => {
                write!(f, "isolated cwd must be absolute, got `{}`", path.display())
            }
            Self::ParentTraversal(path) => write!(
                f,
                "isolated cwd `{}` contains parent traversal",
                path.display()
            ),
            Self::PrivateRootUnavailable => write!(
                f,
                "OCTOPUS_VAR_DIR is required to resolve a private isolated cwd"
            ),
            Self::Create { path, source } => write!(
                f,
                "isolated cwd `{}` cannot be created: {source}",
                path.display()
            ),
            Self::Missing { path, source } => write!(
                f,
                "isolated cwd `{}` cannot be inspected: {source}",
                path.display()
            ),
            Self::NotDirectory(path) => {
                write!(f, "isolated cwd `{}` is not a directory", path.display())
            }
            Self::ProjectMarker { cwd, marker } => write!(
                f,
                "isolated cwd `{}` is under project marker `{}`; codex project-doc walk would no \
                 longer be artifact-pure",
                cwd.display(),
                marker.display()
            ),
        }
    }
}

impl std::error::Error for IsolatedCwdError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Create { source, .. } | Self::Missing { source, .. } => Some(source),
            Self::Relative(_)
            | Self::ParentTraversal(_)
            | Self::PrivateRootUnavailable
            | Self::NotDirectory(_)
            | Self::ProjectMarker { .. } => None,
        }
    }
}

fn isolated_spawn_cwd(cwd: &str) -> Result<std::path::PathBuf, IsolatedCwdError> {
    let declared = std::path::PathBuf::from(cwd);
    if declared.is_absolute() {
        return validate_isolated_cwd(declared);
    }
    if declared
        .components()
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(IsolatedCwdError::ParentTraversal(declared));
    }
    let private_identity = std::env::var_os("OCTOPUS_VAR_DIR")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or(IsolatedCwdError::PrivateRootUnavailable)?;
    let digest = Sha256::digest(private_identity.to_string_lossy().as_bytes());
    #[cfg(unix)]
    let scratch_root = std::path::PathBuf::from("/tmp/octopus-provider-cwd");
    #[cfg(not(unix))]
    let scratch_root = std::env::temp_dir().join("octopus-provider-cwd");
    let private_root = scratch_root.join(&format!("{digest:x}")[..24]);
    let resolved = private_root.join(declared);
    std::fs::create_dir_all(&resolved).map_err(|source| IsolatedCwdError::Create {
        path: resolved.clone(),
        source,
    })?;
    validate_isolated_cwd(resolved)
}

fn validate_isolated_cwd(cwd: std::path::PathBuf) -> Result<std::path::PathBuf, IsolatedCwdError> {
    if !cwd.is_absolute() {
        return Err(IsolatedCwdError::Relative(cwd));
    }
    let metadata = std::fs::metadata(&cwd).map_err(|source| IsolatedCwdError::Missing {
        path: cwd.clone(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(IsolatedCwdError::NotDirectory(cwd));
    }
    for ancestor in cwd.ancestors() {
        for marker_name in ["AGENTS.md", "AGENTS.override.md", "CLAUDE.md", ".git"] {
            let marker = ancestor.join(marker_name);
            if marker.exists() {
                return Err(IsolatedCwdError::ProjectMarker { cwd, marker });
            }
        }
    }
    Ok(cwd)
}

#[derive(Debug)]
pub enum CuratedHomeError {
    Io(std::io::Error),
}

impl std::fmt::Display for CuratedHomeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(source) => write!(f, "failed to build the curated codex home: {source}"),
        }
    }
}

impl std::error::Error for CuratedHomeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
        }
    }
}

impl From<CuratedHomeError> for std::io::Error {
    fn from(err: CuratedHomeError) -> Self {
        match err {
            CuratedHomeError::Io(source) => source,
        }
    }
}

pub fn build_curated_codex_home(
    inherited_home: &std::path::Path,
) -> Result<std::path::PathBuf, CuratedHomeError> {
    build_curated_codex_home_with_auth_store(inherited_home, inherited_home)
}

pub(crate) fn build_curated_opencodex_home(
    provider_home: &std::path::Path,
    base_url: &str,
    model_catalog: Option<&octopus_protocol::openai_models::ModelsResponse>,
) -> Result<std::path::PathBuf, CuratedHomeError> {
    std::fs::create_dir_all(provider_home).map_err(CuratedHomeError::Io)?;
    let curated = provider_home.join(CURATED_OPENCODEX_HOME_DIR);
    std::fs::create_dir_all(&curated).map_err(CuratedHomeError::Io)?;
    std::fs::create_dir_all(curated.join(CURATED_CODEX_TMP_DIR)).map_err(CuratedHomeError::Io)?;
    let provider = format!(
        "[model_providers.octopus_opencodex]\nname = \"Octopus OpenCodex\"\nbase_url = {base_url:?}\nwire_api = \"responses\"\nrequires_openai_auth = false\nrequest_max_retries = 4\nstream_max_retries = 5\nsupports_websockets = false\n"
    );
    let mut root = "model_provider = \"octopus_opencodex\"\n".to_string();
    if let Some(catalog) = model_catalog {
        let path = curated.join("models.json");
        let bytes = serde_json::to_vec(catalog)
            .map_err(|error| CuratedHomeError::Io(std::io::Error::other(error)))?;
        std::fs::write(&path, bytes).map_err(CuratedHomeError::Io)?;
        let path = path.to_str().ok_or_else(|| {
            CuratedHomeError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "OpenCodex model catalog path is not UTF-8",
            ))
        })?;
        root.push_str(&format!("model_catalog_json = {path:?}\n"));
    }
    let config = format!("{root}{CURATED_CONFIG_TOML}\n{provider}");
    std::fs::write(curated.join(CODEX_CONFIG_FILE_NAME), config).map_err(CuratedHomeError::Io)?;
    Ok(curated)
}

fn build_curated_codex_home_with_auth_store(
    inherited_home: &std::path::Path,
    auth_store_home: &std::path::Path,
) -> Result<std::path::PathBuf, CuratedHomeError> {
    std::fs::create_dir_all(inherited_home).map_err(CuratedHomeError::Io)?;
    std::fs::create_dir_all(auth_store_home).map_err(CuratedHomeError::Io)?;

    let curated = inherited_home.join(CURATED_CODEX_HOME_DIR);
    std::fs::create_dir_all(&curated).map_err(CuratedHomeError::Io)?;
    std::fs::create_dir_all(curated.join(CURATED_CODEX_TMP_DIR)).map_err(CuratedHomeError::Io)?;

    for name in CODEX_AUTH_BOUNDARY_FILES {
        let real_auth = auth_store_home.join(name);
        let curated_auth = curated.join(name);
        match std::fs::symlink_metadata(&curated_auth) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = std::fs::read_link(&curated_auth).map_err(CuratedHomeError::Io)?;
                if target != real_auth {
                    replace_auth_boundary(&real_auth, &curated_auth)
                        .map_err(CuratedHomeError::Io)?;
                }
            }
            Ok(_) => {
                return Err(CuratedHomeError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "curated auth boundary '{}' is not a symlink",
                        curated_auth.display()
                    ),
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                link_auth_boundary(&real_auth, &curated_auth).map_err(CuratedHomeError::Io)?;
            }
            Err(error) => return Err(CuratedHomeError::Io(error)),
        }
    }

    let config = format!("model_context_window = 400000\nmodel_auto_compact_token_limit = 350000\n{CURATED_CONFIG_TOML}");
    std::fs::write(curated.join(CODEX_CONFIG_FILE_NAME), config)
        .map_err(CuratedHomeError::Io)?;

    Ok(curated)
}

#[cfg(unix)]
pub(crate) fn link_auth_boundary(
    real_auth: &std::path::Path,
    curated_auth: &std::path::Path,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(real_auth, curated_auth)
}

#[cfg(windows)]
pub(crate) fn link_auth_boundary(
    real_auth: &std::path::Path,
    curated_auth: &std::path::Path,
) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(real_auth, curated_auth)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn link_auth_boundary(
    _real_auth: &std::path::Path,
    _curated_auth: &std::path::Path,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Codex auth-store boundary requires file symlink support",
    ))
}

fn replace_auth_boundary(
    real_auth: &std::path::Path,
    curated_auth: &std::path::Path,
) -> std::io::Result<()> {
    let parent = curated_auth.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "curated auth boundary '{}' has no parent directory",
                curated_auth.display()
            ),
        )
    })?;
    let name = curated_auth.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "curated auth boundary '{}' has no file name",
                curated_auth.display()
            ),
        )
    })?;
    let mut pending_name = std::ffi::OsString::from(".");
    pending_name.push(name);
    pending_name.push(".next");
    let pending = parent.join(pending_name);
    match std::fs::symlink_metadata(&pending) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::remove_file(&pending)?,
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "curated auth boundary staging path '{}' is not a symlink",
                    pending.display()
                ),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    link_auth_boundary(real_auth, &pending)?;
    #[cfg(windows)]
    std::fs::remove_file(curated_auth)?;
    if let Err(error) = std::fs::rename(&pending, curated_auth) {
        let _ = std::fs::remove_file(&pending);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_login::CodexAuth;

    fn websocket_source() -> AppServerSource {
        AppServerSource::WebSocket {
            url: "ws://127.0.0.1:1455/rpc".to_string(),
            token: Some("remote-token".to_string()),
        }
    }

    fn spawn_source() -> AppServerSource {
        AppServerSource::Spawn {
            bin: "codex".to_string(),
            args: vec!["app-server".to_string(), "--experimental-foo".to_string()],
            instance: CodexInstanceConfig::default(),
        }
    }

    #[test]
    fn spawn_source_yields_a_spawn_config() {
        let source = spawn_source();
        assert!(source.is_self_authenticating_spawn());
        let AppServerSource::Spawn { bin, args, .. } = source else {
            panic!("spawn_source() must be the Spawn variant");
        };
        let config = spawn_config_for(bin, args, Some(std::path::PathBuf::from("/tmp")), None);
        assert_eq!(config.bin, "codex");
        assert_eq!(config.args, vec!["app-server", "--experimental-foo"]);
        assert_eq!(config.initialize_params.client_info.name, "octopus");
    }

    #[test]
    fn build_curated_codex_home_keeps_only_auth_and_drops_the_owner_ambient() {
        let inherited = std::env::temp_dir().join(format!(
            "octopus-curated-test-inherited-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&inherited);
        std::fs::create_dir_all(&inherited).expect("mk inherited home");
        std::fs::write(
            inherited.join("auth.json"),
            b"{\"tokens\":\"REAL-OWNER-AUTH\"}",
        )
        .expect("write fake real auth");
        std::fs::write(
            inherited.join("AGENTS.md"),
            "You are a staff developer. ambient-persona-marker. ENGLISH ONLY.".as_bytes(),
        )
        .expect("write owner AGENTS.md persona");
        std::fs::write(
            inherited.join("config.toml"),
            b"model = \"gpt-5.5\"\npersonality = \"friendly\"\n[plugins.\"agentmemory@local\"]\nenabled = true\n[mcp_servers.ambient]\nurl = \"https://ambient.invalid/mcp\"\nbearer_token_env_var = \"AMBIENT_MCP_TOKEN\"\n",
        )
        .expect("write owner config with plugins");
        std::fs::create_dir_all(inherited.join("memories")).expect("mk owner memories dir");
        std::fs::write(inherited.join(".credentials.json"), b"{\"mcp_oauth\":{\"ambient\":\"MCP-ONLY-TEST-SECRET\"}}").unwrap();

        let curated = build_curated_codex_home(&inherited).expect("curated home must build");

        let curated_auth = curated.join("auth.json");
        assert!(curated_auth.exists(), "curated home must carry auth.json");
        assert!(
            std::fs::symlink_metadata(&curated_auth)
                .expect("curated auth boundary metadata")
                .file_type()
                .is_symlink(),
            "curated auth must be the exact inherited auth-store boundary"
        );
        let auth_bytes = std::fs::read(&curated_auth).expect("read curated auth");
        assert_eq!(
            auth_bytes, b"{\"tokens\":\"REAL-OWNER-AUTH\"}",
            "curated auth must resolve to the real inherited auth"
        );

        let cfg =
            std::fs::read_to_string(curated.join("config.toml")).expect("read curated config");
        let isolated: toml::Value = toml::from_str(&cfg).unwrap();
        assert!(isolated.get("mcp_servers").is_none());
        assert_eq!(isolated["features"]["code_mode_host"].as_bool(), Some(true));
        assert!(!curated.join(".credentials.json").exists());
        assert!(
            !cfg.contains("[plugins."),
            "curated config must carry NO plugin tables: {cfg}"
        );
        assert!(isolated.get("plugins").is_none(), "curated config must enable no plugins");
        assert_eq!(isolated["tools"]["experimental_request_user_input"]["enabled"].as_bool(), Some(true));
        assert!(
            !cfg.contains("friendly"),
            "curated config must carry NO friendly personality: {cfg}"
        );
        assert!(
            !cfg.contains("model ="),
            "curated config must carry NO model default (the model is threaded per-turn): {cfg}"
        );
        assert!(
            cfg.contains("plugins = false"),
            "curated config must pin plugins off (defense-in-depth): {cfg}"
        );
        assert!(
            cfg.contains("sandbox_mode = \"read-only\""),
            "curated config must pin the sandbox read-only: {cfg}"
        );
        assert!(
            cfg.contains("model_context_window = 400000")
                && cfg.contains("model_auto_compact_token_limit = 350000"),
            "curated config must pin the full owner-selected Codex context budget: {cfg}"
        );
        assert!(
            cfg.contains("web_search = \"disabled\""),
            "curated config must pin web_search disabled: {cfg}"
        );
        assert!(
            cfg.contains("include_instructions = false"),
            "curated config must pin skills.include_instructions off: {cfg}"
        );
        assert!(
            cfg.contains("include_apps_instructions = false"),
            "curated config must pin apps instructions off: {cfg}"
        );
        assert!(
            cfg.contains("include_permissions_instructions = false"),
            "curated config must pin Codex permissions instructions off: {cfg}"
        );
        assert!(
            cfg.contains("shell_snapshot = false"),
            "curated config must disable the native shell snapshot: {cfg}"
        );
        assert!(
            cfg.contains("skill_search = false"),
            "curated config must disable native skill selection: {cfg}"
        );
        assert!(
            !cfg.contains("remote_compaction_v2 = false"),
            "curated config must leave Codex native compaction enabled: {cfg}"
        );
        assert!(
            !cfg.contains("token_budget"),
            "curated config must leave model-owned token budgeting unset: {cfg}"
        );

        let repeated =
            build_curated_codex_home(&inherited).expect("curated home must be idempotent");
        assert_eq!(
            repeated, curated,
            "one exact interface must keep one stable app-server home"
        );

        assert!(
            !curated.join("AGENTS.md").exists(),
            "curated home must NOT contain AGENTS.md (the owner persona must be gone)"
        );
        assert!(
            !curated.join("AGENTS.override.md").exists(),
            "curated home must NOT contain AGENTS.override.md"
        );
        assert!(
            !curated.join("plugins").exists(),
            "curated home must NOT contain plugins/"
        );
        assert!(
            !curated.join("memories").exists(),
            "curated home must NOT contain memories/"
        );

        let _ = std::fs::remove_dir_all(&inherited);
        let _ = std::fs::remove_dir_all(&curated);
    }

    #[test]
    fn build_curated_codex_home_keeps_an_empty_auth_store_login_capable() {
        let inherited = std::env::temp_dir().join(format!(
            "octopus-curated-test-noauth-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&inherited);
        std::fs::create_dir_all(&inherited).expect("mk inherited home");
        let curated = build_curated_codex_home(&inherited)
            .expect("an empty auth store must still boot account/login/start");
        let curated_auth = curated.join("auth.json");
        assert!(
            std::fs::symlink_metadata(&curated_auth)
                .expect("dangling auth boundary metadata")
                .file_type()
                .is_symlink()
        );
        assert!(!inherited.join("auth.json").exists());
        std::fs::write(&curated_auth, b"{\"OPENAI_API_KEY\":\"redacted-test\"}")
            .expect("app-server login must be able to populate the current auth store");
        assert_eq!(
            std::fs::read(inherited.join("auth.json")).unwrap(),
            b"{\"OPENAI_API_KEY\":\"redacted-test\"}"
        );
        let _ = std::fs::remove_dir_all(&inherited);
        let _ = std::fs::remove_dir_all(&curated);
    }

    #[test]
    fn curated_homes_share_only_machine_auth_and_keep_provider_state_isolated() {
        let root = std::env::temp_dir().join(format!(
            "octopus-curated-machine-auth-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let machine_home = root.join("machine-codex-home");
        let first_home = root.join("world_provider_codex_first");
        let second_home = root.join("world_provider_codex_second");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&machine_home).unwrap();
        std::fs::write(
            machine_home.join(CODEX_AUTH_FILE_NAME),
            b"{\"tokens\":\"machine-account\"}",
        )
        .unwrap();
        let first_curated =
            build_curated_codex_home_with_auth_store(&first_home, &machine_home).unwrap();
        let second_curated =
            build_curated_codex_home_with_auth_store(&second_home, &machine_home).unwrap();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(first_curated.join(CODEX_AUTH_LOCK_FILE_NAME))
            .unwrap();
        assert!(machine_home.join(CODEX_AUTH_LOCK_FILE_NAME).is_file());
        assert_eq!(
            std::fs::read_link(second_curated.join(CODEX_AUTH_LOCK_FILE_NAME)).unwrap(),
            machine_home.join(CODEX_AUTH_LOCK_FILE_NAME),
            "every local Codex interface must serialize auth writes on the machine lock"
        );

        std::fs::write(
            first_curated.join(CODEX_AUTH_FILE_NAME),
            b"{\"tokens\":\"new-machine-account\"}",
        )
        .unwrap();

        assert_eq!(
            std::fs::read(machine_home.join(CODEX_AUTH_FILE_NAME)).unwrap(),
            b"{\"tokens\":\"new-machine-account\"}"
        );
        assert_eq!(
            std::fs::read(second_curated.join(CODEX_AUTH_FILE_NAME)).unwrap(),
            b"{\"tokens\":\"new-machine-account\"}",
            "every local Codex interface must observe the same machine account"
        );
        assert!(
            !first_home.join("threads").exists() && !second_home.join("threads").exists(),
            "sharing auth must not copy machine sessions into provider-private homes"
        );
        assert_ne!(first_curated, second_curated);

        let replacement_home = root.join("replacement-machine-codex-home");
        std::fs::create_dir_all(&replacement_home).unwrap();
        std::fs::write(
            replacement_home.join(CODEX_AUTH_FILE_NAME),
            b"{\"tokens\":\"replacement-machine-account\"}",
        )
        .unwrap();
        build_curated_codex_home_with_auth_store(&first_home, &replacement_home).unwrap();
        for name in CODEX_AUTH_BOUNDARY_FILES {
            assert_eq!(
                std::fs::read_link(first_curated.join(name)).unwrap(),
                replacement_home.join(name)
            );
        }
        std::fs::remove_file(second_curated.join(CODEX_AUTH_LOCK_FILE_NAME)).unwrap();
        std::fs::write(second_curated.join(CODEX_AUTH_LOCK_FILE_NAME), b"").unwrap();
        assert!(build_curated_codex_home_with_auth_store(&second_home, &machine_home).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn default_spawn_argv_is_the_isolated_form() {
        let args = build_default_spawn_args(None);

        let mut expected = vec!["app-server".to_string()];
        for override_kv in SPAWN_ISOLATION_CONFIG_OVERRIDES {
            expected.push("-c".to_string());
            expected.push((*override_kv).to_string());
        }
        assert_eq!(
            args, expected,
            "the default spawn argv must carry only context-isolation + tool-suppression overrides"
        );

        assert_eq!(args.first().map(String::as_str), Some("app-server"));
        assert!(!args.iter().any(|arg| arg.starts_with("web_search=")));
        assert!(args.contains(&"project_doc_max_bytes=0".to_string()));
        for override_kv in SPAWN_ISOLATION_CONFIG_OVERRIDES {
            assert!(
                args.iter().any(|a| a == override_kv),
                "isolation override `{override_kv}` must be on the default argv"
            );
        }
    }

    #[test]
    fn isolated_argv_carries_the_tool_suppression_overrides() {
        let args = build_default_spawn_args(None);
        for key in [
            "sandbox_mode=read-only",
            "skills.include_instructions=false",
            "include_apps_instructions=false",
            "include_permissions_instructions=false",
            "features.code_mode_host=true",
            "features.instant_interrupt=true",
        ] {
            assert!(
                args.iter().any(|a| a == key),
                "tool-suppression override `{key}` must be on the isolated argv"
            );
        }
    }

    #[test]
    fn spawn_initialize_advertises_experimental_api_for_tool_suppression() {
        let params = octopus_initialize_params();
        let capabilities = params
            .capabilities
            .expect("the spawn initialize handshake must carry capabilities");
        assert!(
            capabilities.experimental_api,
            "experimental_api must be advertised so the empty-environments tool-suppression lever \
             is accepted, not rejected by the experimental gate"
        );
        let declared: serde_json::Value = serde_json::from_str(MANAGED_CLI_RELEASE).unwrap();
        assert_eq!(
            serde_json::json!(capabilities.opt_out_notification_methods),
            declared["notifications"]["opt_out"],
            "initialize must opt out of exactly the notifications the managed release declares"
        );
    }

    #[test]
    fn isolated_cwd_default_is_a_neutral_repo_free_path() {
        let declared = std::path::PathBuf::from(SPAWN_ISOLATED_CWD);
        assert!(
            !declared.is_absolute()
                && !declared
                    .components()
                    .any(|component| component == std::path::Component::ParentDir),
            "the default isolated cwd must be a private path relative to the interface var root"
        );
        let private_root = std::env::temp_dir().join(format!(
            "octopus-codex-private-cwd-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let cwd = private_root.join(declared);
        let _ = std::fs::remove_dir_all(&private_root);
        std::fs::create_dir_all(&cwd).expect("the private isolated cwd creates");
        assert!(
            cwd.is_absolute() && cwd.exists() && cwd.is_dir(),
            "the isolated cwd must be a real neutral directory"
        );
        validate_isolated_cwd(cwd.clone())
            .expect("the default isolated cwd must pass the project-marker guard");
        let _ = std::fs::remove_dir_all(&private_root);
    }

    #[test]
    fn custom_spawn_args_are_still_forced_through_isolation() {
        let args = build_spawn_args(
            &CodexInstanceConfig::default(),
            Some("app-server --experimental-foo"),
            None,
        )
        .unwrap();
        assert_eq!(args.first().map(String::as_str), Some("app-server"));
        assert!(
            args.iter().any(|a| a == "--experimental-foo"),
            "custom app-server flags should be preserved"
        );
        for override_kv in SPAWN_ISOLATION_CONFIG_OVERRIDES {
            assert!(
                args.iter().any(|a| a == override_kv),
                "isolation override `{override_kv}` must still be present on custom argv"
            );
        }
        assert!(
            !args.iter().any(|arg| arg.starts_with("model=")),
            "spawn argv must not contain provider model selection"
        );
    }

    #[test]
    fn custom_spawn_args_cannot_contradict_isolation_suppressors() {
        match build_spawn_args(
            &CodexInstanceConfig::default(),
            Some("app-server -c project_doc_max_bytes=999"),
            None,
        ) {
            Err(SpawnArgsError::ContradictsIsolation { key, got, expected }) => {
                assert_eq!(key, "project_doc_max_bytes");
                assert_eq!(got, "999");
                assert_eq!(expected, "0");
            }
            other => panic!("contradictory project_doc_max_bytes must fail closed, got {other:?}"),
        }

        match build_spawn_args(&CodexInstanceConfig::default(), Some("exec"), None) {
            Err(SpawnArgsError::NotAppServer { first, expected }) => {
                assert_eq!(first, "exec");
                assert_eq!(expected, "app-server");
            }
            other => panic!("non-app-server custom argv must fail closed, got {other:?}"),
        }
    }

    #[test]
    fn isolated_cwd_rejects_project_doc_markers_in_ancestors() {
        let root = std::env::temp_dir().join(format!(
            "octopus-cwd-marker-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let child = root.join("neutral-looking-child");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&child).expect("mk child cwd");
        std::fs::write(root.join("AGENTS.md"), b"ambient persona").expect("write marker");

        match validate_isolated_cwd(child.clone()) {
            Err(IsolatedCwdError::ProjectMarker { cwd, marker }) => {
                assert_eq!(cwd, child);
                assert_eq!(marker, root.join("AGENTS.md"));
            }
            other => panic!("cwd under AGENTS.md must fail closed, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn exact_private_home_spawn_uses_the_declared_auth_store_without_copying_it() {
        let root = std::env::temp_dir().join(format!(
            "octopus-codex-self-authenticating-spawn-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let provider_home = root.join("provider-home");
        let auth_store_home = root.join("auth-store-home");
        let isolated_cwd = root.join("isolated-cwd");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&provider_home).expect("provider home creates");
        std::fs::create_dir_all(&auth_store_home).expect("auth store home creates");
        std::fs::create_dir_all(&isolated_cwd).expect("isolated cwd creates");
        std::fs::write(auth_store_home.join("auth.json"), b"{}").expect("auth file writes");
        let source = AppServerSource::Spawn {
            bin: "/nonexistent/octopus-spawn-no-such-binary".to_string(),
            args: vec!["app-server".to_string()],
            instance: CodexInstanceConfig {
                isolated_cwd: Some(isolated_cwd.to_string_lossy().to_string()),
                ..CodexInstanceConfig::default()
            },
        };
        let result =
            connect_with_auth_from_provider_homes(source, &provider_home, &auth_store_home).await;
        let curated_auth = provider_home
            .join(CURATED_CODEX_HOME_DIR)
            .join(CODEX_AUTH_FILE_NAME);
        assert!(
            std::fs::symlink_metadata(&curated_auth)
                .expect("curated auth boundary exists")
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_link(&curated_auth).expect("curated auth boundary resolves"),
            auth_store_home.join(CODEX_AUTH_FILE_NAME)
        );
        assert!(!provider_home.join(CODEX_AUTH_FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(&root);
        match result {
            Err(ConfigError::Connect(_)) => {}
            Err(other) => panic!(
                "a self-authenticating spawn must fail on the SPAWN (Connect), never on the \
                 empty store, got: {other}"
            ),
            Ok(_) => panic!("spawning a nonexistent binary cannot succeed"),
        }
    }

    #[test]
    fn spawn_args_default_and_model_override_is_forbidden() {
        let without_model = build_default_spawn_args(None);
        assert_eq!(
            without_model.first().map(String::as_str),
            Some("app-server")
        );
        assert!(
            without_model
                .iter()
                .any(|arg| arg == "project_doc_max_bytes=0"),
            "the default argv must be isolated even without a model override"
        );

        let default_instance = CodexInstanceConfig::default();
        match build_spawn_args(&default_instance, None, Some("gpt-5.5")) {
            Err(SpawnArgsError::ModelOverrideForbidden(model)) => {
                assert_eq!(model, "gpt-5.5");
            }
            other => panic!("model selection in spawn config must fail closed, got {other:?}"),
        }
        match build_spawn_args(&default_instance, Some("app-server -c model=gpt-5.5"), None) {
            Err(SpawnArgsError::InterfaceModelKnobForbidden { primitive, .. }) => {
                assert_eq!(primitive, "model");
            }
            other => panic!("model selection in raw spawn argv must fail closed, got {other:?}"),
        }
        match build_spawn_args(&default_instance, Some("app-server --model gpt-5.5"), None) {
            Err(SpawnArgsError::InterfaceModelKnobForbidden { primitive, .. }) => {
                assert_eq!(primitive, "model");
            }
            other => {
                panic!("model selection flag in raw spawn argv must fail closed, got {other:?}")
            }
        }
        match build_spawn_args(&default_instance, Some("app-server -C /repo"), None) {
            Err(SpawnArgsError::InterfaceModelKnobForbidden { primitive, .. }) => {
                assert_eq!(primitive, "cwd");
            }
            other => panic!("cwd selection in raw spawn argv must fail closed, got {other:?}"),
        }
    }

    #[test]
    fn is_truthy_only_for_boolean_values() {
        assert!(is_truthy("1"));
        assert!(is_truthy("TRUE"));
        assert!(is_truthy(" yes "));
        assert!(!is_truthy("codex"));
        assert!(!is_truthy("/usr/local/bin/codex"));
    }

    #[tokio::test]
    async fn connect_with_auth_refuses_remote_even_when_authenticated() {
        let manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("sk-store-credential"));
        match connect_with_auth(websocket_source(), &manager).await {
            Err(ConfigError::UnisolatedRemoteTransport("websocket")) => {}
            Err(other) => panic!(
                "remote websocket must fail closed as UnisolatedRemoteTransport even with auth, got: \
                 {other}"
            ),
            Ok(_) => {
                panic!("a remote websocket must fail closed, never yield a provider transport")
            }
        }
    }

    fn driver_config_with_spawn(spawn: serde_json::Value) -> InterfaceDriverConfig {
        InterfaceDriverConfig {
            private_config: serde_json::Map::new(),
            residual_ambient: Some(serde_json::json!({ "spawn": spawn })),
            host_slots: std::collections::BTreeMap::new(),
            transport: None,
            supported_reasoning_efforts: Vec::new(),
        }
    }

    #[test]
    fn instance_config_defaults_when_folder_absent() {
        let cfg = CodexInstanceConfig::from_driver_config(None)
            .expect("absent driver config yields defaults");
        assert_eq!(cfg, CodexInstanceConfig::default());
        assert_eq!(cfg.subcommand(), SPAWN_DEFAULT_SUBCOMMAND);
        assert_eq!(cfg.home_env(), ENV_CODEX_HOME);
        assert_eq!(cfg.isolated_cwd(), SPAWN_ISOLATED_CWD);
        assert!(!cfg.connect_on_ready());
        assert_eq!(cfg.idle_disconnect(), None);
        let expected: Vec<String> = SPAWN_ISOLATION_CONFIG_OVERRIDES
            .iter()
            .map(|kv| (*kv).to_string())
            .collect();
        assert_eq!(cfg.isolation_overrides(), expected);
        assert!(
            CodexInstanceConfig::from_driver_config(Some(&driver_config_with_spawn(
                serde_json::json!({ "connect_on_ready": true }),
            )))
            .unwrap()
            .connect_on_ready()
        );
        assert_eq!(
            CodexInstanceConfig::from_driver_config(Some(&driver_config_with_spawn(
                serde_json::json!({ "idle_disconnect_ms": 300_000 }),
            )))
            .unwrap()
            .idle_disconnect(),
            Some(std::time::Duration::from_secs(300))
        );
        assert!(matches!(
            CodexInstanceConfig::from_driver_config(Some(&driver_config_with_spawn(
                serde_json::json!({ "idle_disconnect_ms": 0 }),
            ))),
            Err(InstanceConfigError::ZeroIdleDisconnect)
        ));
        assert!(matches!(
            CodexInstanceConfig::from_driver_config(Some(&driver_config_with_spawn(
                serde_json::json!({
                    "connect_on_ready": true,
                    "idle_disconnect_ms": 300_000
                }),
            ))),
            Err(InstanceConfigError::ReadyTransportMayNotExpire)
        ));

        let evidence_only = InterfaceDriverConfig {
            private_config: serde_json::Map::new(),
            residual_ambient: Some(serde_json::json!({ "boundary": "declared" })),
            host_slots: std::collections::BTreeMap::new(),
            transport: None,
            supported_reasoning_efforts: Vec::new(),
        };
        assert_eq!(
            CodexInstanceConfig::from_driver_config(Some(&evidence_only)).unwrap(),
            CodexInstanceConfig::default()
        );
    }

    #[test]
    fn instance_config_rejects_an_ambient_home_redirect() {
        let config = driver_config_with_spawn(serde_json::json!({
            "subcommand": "app-server",
            "extra_argv": ["--experimental-foo"],
            "isolation_overrides": ["project_doc_max_bytes=0", "web_search=disabled"],
            "home_env": "OCTOPUS_CODEX_HOME",
            "isolated_cwd": "/var/tmp"
        }));
        match CodexInstanceConfig::from_driver_config(Some(&config)) {
            Err(InstanceConfigError::UnsupportedHomeEnv(value)) => {
                assert_eq!(value, "OCTOPUS_CODEX_HOME")
            }
            other => panic!("ambient home redirect must fail closed, got {other:?}"),
        }
    }

    #[test]
    fn instance_config_fails_closed_on_unknown_spawn_key() {
        let config = driver_config_with_spawn(serde_json::json!({
            "subcommand": "app-server",
            "reopen_project_docs": true
        }));
        match CodexInstanceConfig::from_driver_config(Some(&config)) {
            Err(InstanceConfigError::Malformed(_)) => {}
            other => panic!("an unknown spawn key must fail closed, got {other:?}"),
        }
    }

    #[test]
    fn instance_config_fails_closed_on_empty_ref_slots() {
        match CodexInstanceConfig::from_driver_config(Some(&driver_config_with_spawn(
            serde_json::json!({ "subcommand": "  " }),
        ))) {
            Err(InstanceConfigError::EmptySubcommand) => {}
            other => panic!("empty subcommand must fail closed, got {other:?}"),
        }
        match CodexInstanceConfig::from_driver_config(Some(&driver_config_with_spawn(
            serde_json::json!({ "home_env": "" }),
        ))) {
            Err(InstanceConfigError::EmptyHomeEnv) => {}
            other => panic!("empty home_env must fail closed, got {other:?}"),
        }
        match CodexInstanceConfig::from_driver_config(Some(&driver_config_with_spawn(
            serde_json::json!({ "isolated_cwd": "" }),
        ))) {
            Err(InstanceConfigError::EmptyIsolatedCwd) => {}
            other => panic!("empty isolated_cwd must fail closed, got {other:?}"),
        }
    }

    #[test]
    fn live_world_provider_codex_descriptor_parses_into_instance_config() {
        let body = include_str!("../../../interface.json");
        let descriptor: InterfaceDescriptor = serde_json::from_str(body)
            .expect("the live world_provider_codex descriptor must parse");
        let cfg = CodexInstanceConfig::from_descriptor(&descriptor)
            .expect("the live descriptor's folder spawn block must be well-formed");
        assert_eq!(cfg.subcommand(), SPAWN_DEFAULT_SUBCOMMAND);
        assert_eq!(cfg.home_env(), ENV_CODEX_HOME);
        assert_eq!(cfg.isolated_cwd(), SPAWN_ISOLATED_CWD);
        let expected: Vec<String> = SPAWN_ISOLATION_CONFIG_OVERRIDES
            .iter()
            .map(|kv| (*kv).to_string())
            .collect();
        assert_eq!(
            cfg.isolation_overrides(),
            expected,
            "the live descriptor's isolation pins must match the built-in default set"
        );
    }
}
