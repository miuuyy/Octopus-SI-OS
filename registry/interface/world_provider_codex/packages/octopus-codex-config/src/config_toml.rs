use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;

use crate::features::FeaturesToml;
use crate::path_utils::normalize_for_path_comparison;
use crate::permissions_toml::PermissionsToml;
use crate::profile_toml::ConfigProfile;
use crate::types::AnalyticsConfigToml;
use crate::types::ApprovalsReviewer;
use crate::types::AppsConfigToml;
use crate::types::AuthCredentialsStoreMode;
use crate::types::McpServerConfig;
use crate::types::MemoriesToml;
use crate::types::Notice;
use crate::types::OAuthCredentialsStoreMode;
use crate::types::OtelConfigToml;
use crate::types::SandboxWorkspaceWrite;
use crate::types::ShellEnvironmentPolicyToml;
use crate::types::ToolSuggestConfig;
use crate::types::UriBasedFileOpener;
use crate::types::WindowsToml;
use octopus_codex_model_provider_info::LEGACY_OLLAMA_CHAT_PROVIDER_ID;
use octopus_codex_model_provider_info::LMSTUDIO_OSS_PROVIDER_ID;
use octopus_codex_model_provider_info::ModelProviderInfo;
use octopus_codex_model_provider_info::OLLAMA_CHAT_PROVIDER_REMOVED_ERROR;
use octopus_codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID;
use octopus_protocol::config_types::AutoCompactTokenLimitScope;
use octopus_protocol::config_types::ForcedLoginMethod;
use octopus_protocol::config_types::Personality;
use octopus_protocol::config_types::ReasoningSummary;
use octopus_protocol::config_types::SandboxMode;
use octopus_protocol::config_types::TrustLevel;
use octopus_protocol::config_types::Verbosity;
use octopus_protocol::config_types::WebSearchMode;
use octopus_protocol::config_types::WebSearchToolConfig;
use octopus_protocol::config_types::WindowsSandboxLevel;
use octopus_protocol::models::PermissionProfile;
use octopus_protocol::openai_models::ReasoningEffort;
use octopus_protocol::permissions::NetworkSandboxPolicy;
use octopus_protocol::protocol::AskForApproval;
use octopus_util_abspath::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error as SerdeError;
use serde_json::Value as JsonValue;

pub const DEFAULT_PROJECT_DOC_MAX_BYTES: usize = 32 * 1024;

const fn default_allow_login_shell() -> Option<bool> {
    Some(true)
}

const fn default_project_doc_max_bytes() -> Option<usize> {
    Some(DEFAULT_PROJECT_DOC_MAX_BYTES)
}

fn default_project_doc_fallback_filenames() -> Option<Vec<String>> {
    Some(Vec::new())
}

const fn default_hide_agent_reasoning() -> Option<bool> {
    Some(false)
}

fn opaque_terminal_section_schema(
    _generator: &mut schemars::r#gen::SchemaGenerator,
) -> schemars::schema::Schema {
    schemars::schema::Schema::Object(schemars::schema::SchemaObject {
        instance_type: Some(schemars::schema::InstanceType::Object.into()),
        metadata: Some(Box::new(schemars::schema::Metadata {
            description: Some(
                "Codex terminal [tui] section, carried opaquely.".to_string(),
            ),
            ..Default::default()
        })),
        ..Default::default()
    })
}

const fn default_true() -> bool {
    true
}

#[derive(Serialize, Debug, Clone, PartialEq, JsonSchema)]
#[serde(untagged)]
pub enum ForcedChatgptWorkspaceIds {
    Single(String),
    Multiple(Vec<String>),
}

impl ForcedChatgptWorkspaceIds {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            Self::Single(value) => vec![value],
            Self::Multiple(values) => values,
        }
    }
}

impl<'de> Deserialize<'de> for ForcedChatgptWorkspaceIds {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Single(String),
            Multiple(Vec<String>),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Single(value) if value.contains(',') => Err(D::Error::custom(
                "forced_chatgpt_workspace_id must be a single workspace ID string or a TOML list \
of strings; comma-separated strings are not supported. Use \
`forced_chatgpt_workspace_id = [\"123e4567-e89b-42d3-a456-426614174000\", \
\"123e4567-e89b-42d3-a456-426614174001\"]` instead.",
            )),
            Repr::Single(value) => Ok(Self::Single(value)),
            Repr::Multiple(values) => Ok(Self::Multiple(values)),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OrchestratorToml {
    pub skills: Option<OrchestratorFeatureToml>,
    pub mcp: Option<OrchestratorFeatureToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct OrchestratorFeatureToml {
    pub enabled: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ConfigToml {
    pub model: Option<String>,
    pub review_model: Option<String>,

    pub model_provider: Option<String>,

    pub model_context_window: Option<i64>,

    pub model_auto_compact_token_limit: Option<i64>,

    pub model_auto_compact_token_limit_scope: Option<AutoCompactTokenLimitScope>,

    pub approval_policy: Option<AskForApproval>,

    pub approvals_reviewer: Option<ApprovalsReviewer>,

    #[serde(default)]
    pub auto_review: Option<AutoReviewToml>,

    #[serde(default)]
    pub shell_environment_policy: ShellEnvironmentPolicyToml,

    #[serde(default = "default_allow_login_shell")]
    pub allow_login_shell: Option<bool>,

    pub sandbox_mode: Option<SandboxMode>,

    pub sandbox_workspace_write: Option<SandboxWorkspaceWrite>,

    pub default_permissions: Option<String>,

    #[serde(default)]
    pub permissions: Option<PermissionsToml>,

    #[serde(default)]
    pub notify: Option<Vec<String>>,

    pub instructions: Option<String>,

    #[serde(default)]
    pub developer_instructions: Option<String>,

    pub include_permissions_instructions: Option<bool>,

    pub include_apps_instructions: Option<bool>,

    pub include_collaboration_mode_instructions: Option<bool>,

    pub include_environment_context: Option<bool>,

    pub model_instructions_file: Option<AbsolutePathBuf>,

    pub compact_prompt: Option<String>,

    #[serde(default)]
    pub forced_chatgpt_workspace_id: Option<ForcedChatgptWorkspaceIds>,

    #[serde(default)]
    pub forced_login_method: Option<ForcedLoginMethod>,

    #[serde(default)]
    pub cli_auth_credentials_store: Option<AuthCredentialsStoreMode>,

    #[serde(default)]
    #[schemars(schema_with = "crate::schema::mcp_servers_schema")]
    pub mcp_servers: HashMap<String, McpServerConfig>,

    #[serde(default)]
    pub mcp_oauth_credentials_store: Option<OAuthCredentialsStoreMode>,

    pub mcp_oauth_callback_port: Option<u16>,

    pub mcp_oauth_callback_url: Option<String>,

    #[serde(default, deserialize_with = "deserialize_model_providers")]
    pub model_providers: HashMap<String, ModelProviderInfo>,

    #[serde(default = "default_project_doc_max_bytes")]
    pub project_doc_max_bytes: Option<usize>,

    #[serde(default = "default_project_doc_fallback_filenames")]
    pub project_doc_fallback_filenames: Option<Vec<String>>,

    pub tool_output_token_limit: Option<usize>,

    pub background_terminal_max_timeout: Option<u64>,

    #[schemars(skip)]
    pub js_repl_node_path: Option<AbsolutePathBuf>,

    #[schemars(skip)]
    pub js_repl_node_module_dirs: Option<Vec<AbsolutePathBuf>>,

    pub profile: Option<String>,

    #[serde(default)]
    pub profiles: HashMap<String, ConfigProfile>,

    #[schemars(skip)]
    pub sqlite_home: Option<AbsolutePathBuf>,

    pub log_dir: Option<AbsolutePathBuf>,

    pub debug: Option<DebugToml>,

    pub file_opener: Option<UriBasedFileOpener>,

    #[schemars(schema_with = "opaque_terminal_section_schema")]
    pub tui: Option<toml::Value>,

    #[serde(default = "default_hide_agent_reasoning")]
    pub hide_agent_reasoning: Option<bool>,

    pub show_raw_agent_reasoning: Option<bool>,

    pub model_reasoning_effort: Option<ReasoningEffort>,
    pub plan_mode_reasoning_effort: Option<ReasoningEffort>,
    pub model_reasoning_summary: Option<ReasoningSummary>,
    pub model_verbosity: Option<Verbosity>,

    pub model_supports_reasoning_summaries: Option<bool>,

    pub model_catalog_json: Option<AbsolutePathBuf>,

    pub personality: Option<Personality>,

    pub service_tier: Option<String>,

    pub chatgpt_base_url: Option<String>,

    pub apps_mcp_product_sku: Option<String>,

    pub orchestrator: Option<OrchestratorToml>,

    pub openai_base_url: Option<String>,

    #[serde(default)]
    pub audio: Option<RealtimeAudioToml>,

    pub experimental_realtime_ws_base_url: Option<String>,
    pub experimental_realtime_webrtc_call_base_url: Option<String>,
    pub experimental_realtime_ws_model: Option<String>,
    #[serde(default)]
    pub realtime: Option<RealtimeToml>,
    pub experimental_realtime_ws_backend_prompt: Option<String>,
    pub experimental_realtime_ws_startup_context: Option<String>,
    pub experimental_realtime_start_instructions: Option<String>,

    #[schemars(skip)]
    pub experimental_thread_store_endpoint: Option<String>,

    #[schemars(skip)]
    pub experimental_thread_store: Option<ThreadStoreToml>,
    pub projects: Option<HashMap<String, ProjectConfig>>,

    pub web_search: Option<WebSearchMode>,

    pub tools: Option<ToolsToml>,

    pub tool_suggest: Option<ToolSuggestConfig>,

    pub agents: Option<AgentsToml>,

    pub memories: Option<MemoriesToml>,

    #[serde(default)]
    #[schemars(schema_with = "crate::schema::features_schema")]
    pub features: Option<FeaturesToml>,

    pub suppress_unstable_features_warning: Option<bool>,

    #[serde(default)]
    pub ghost_snapshot: Option<GhostSnapshotToml>,

    #[serde(default)]
    pub project_root_markers: Option<Vec<String>>,

    pub check_for_update_on_startup: Option<bool>,

    pub disable_paste_burst: Option<bool>,

    pub analytics: Option<AnalyticsConfigToml>,

    #[serde(default)]
    pub apps: Option<AppsConfigToml>,

    #[serde(default)]
    pub desktop: Option<HashMap<String, JsonValue>>,

    pub otel: Option<OtelConfigToml>,

    #[serde(default)]
    pub windows: Option<WindowsToml>,

    pub notice: Option<Notice>,

    pub experimental_compact_prompt_file: Option<AbsolutePathBuf>,
    pub experimental_use_unified_exec_tool: Option<bool>,
    pub oss_provider: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ConfigLockfileToml {
    pub version: u32,
    pub codex_version: String,

    pub config: ConfigToml,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DebugToml {
    pub config_lockfile: Option<DebugConfigLockToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DebugConfigLockToml {
    pub export_dir: Option<AbsolutePathBuf>,

    pub load_path: Option<AbsolutePathBuf>,

    pub allow_codex_version_mismatch: Option<bool>,

    pub save_fields_resolved_from_model_catalog: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadStoreToml {
    Local {},
    #[schemars(skip)]
    InMemory {
        id: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
pub struct AutoReviewToml {
    pub policy: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ProjectConfig {
    pub trust_level: Option<TrustLevel>,
}

impl ProjectConfig {
    pub fn is_trusted(&self) -> bool {
        matches!(self.trust_level, Some(TrustLevel::Trusted))
    }

    pub fn is_untrusted(&self) -> bool {
        matches!(self.trust_level, Some(TrustLevel::Untrusted))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RealtimeAudioConfig {
    pub microphone: Option<String>,
    pub speaker: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeWsMode {
    #[default]
    Conversational,
    Transcription,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeTransport {
    #[default]
    #[serde(rename = "webrtc")]
    WebRtc,
    Websocket,
}

pub use octopus_protocol::protocol::RealtimeConversationVersion as RealtimeWsVersion;
pub use octopus_protocol::protocol::RealtimeVoice;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RealtimeConfig {
    pub version: RealtimeWsVersion,
    #[serde(rename = "type")]
    pub session_type: RealtimeWsMode,
    pub transport: RealtimeTransport,
    pub voice: Option<RealtimeVoice>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RealtimeToml {
    pub version: Option<RealtimeWsVersion>,
    #[serde(rename = "type")]
    pub session_type: Option<RealtimeWsMode>,
    pub transport: Option<RealtimeTransport>,
    pub voice: Option<RealtimeVoice>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct RealtimeAudioToml {
    pub microphone: Option<String>,
    pub speaker: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ToolsToml {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_web_search_tool_config"
    )]
    pub web_search: Option<WebSearchToolConfig>,
    pub experimental_request_user_input: Option<ExperimentalRequestUserInput>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ExperimentalRequestUserInput {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WebSearchToolConfigInput {
    Enabled(bool),
    Config(WebSearchToolConfig),
}

fn deserialize_optional_web_search_tool_config<'de, D>(
    deserializer: D,
) -> Result<Option<WebSearchToolConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<WebSearchToolConfigInput>::deserialize(deserializer)?;

    Ok(match value {
        None => None,
        Some(WebSearchToolConfigInput::Enabled(enabled)) => {
            let _ = enabled;
            None
        }
        Some(WebSearchToolConfigInput::Config(config)) => Some(config),
    })
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentsToml {
    #[schemars(range(min = 1))]
    pub max_threads: Option<usize>,
    #[schemars(range(min = 1))]
    pub max_depth: Option<i32>,
    #[schemars(range(min = 1))]
    pub job_max_runtime_seconds: Option<u64>,
    pub interrupt_message: Option<bool>,

    #[serde(default, flatten)]
    pub roles: BTreeMap<String, AgentRoleToml>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct AgentRoleToml {
    pub description: Option<String>,

    pub config_file: Option<AbsolutePathBuf>,

    pub nickname_candidates: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct GhostSnapshotToml {
    #[serde(alias = "ignore_untracked_files_over_bytes")]
    pub ignore_large_untracked_files: Option<i64>,
    #[serde(alias = "large_untracked_dir_warning_threshold")]
    pub ignore_large_untracked_dirs: Option<i64>,
    pub disable_warnings: Option<bool>,
}

impl ConfigToml {
    pub async fn derive_permission_profile(
        &self,
        sandbox_mode_override: Option<SandboxMode>,
        windows_sandbox_level: WindowsSandboxLevel,
        active_project: Option<&ProjectConfig>,
        permission_profile_constraint: Option<&crate::Constrained<PermissionProfile>>,
    ) -> PermissionProfile {
        let configured_sandbox_mode = sandbox_mode_override.or(self.sandbox_mode);
        let resolved_sandbox_mode = configured_sandbox_mode
            .or_else(|| {
                active_project
                    .filter(|project| project.is_trusted() || project.is_untrusted())
                    .map(|_| {
                        if cfg!(target_os = "windows")
                            && windows_sandbox_level == WindowsSandboxLevel::Disabled
                        {
                            SandboxMode::ReadOnly
                        } else {
                            SandboxMode::WorkspaceWrite
                        }
                    })
            })
            .unwrap_or_default();
        let effective_sandbox_mode = if cfg!(target_os = "windows")
            && windows_sandbox_level == WindowsSandboxLevel::Disabled
            && matches!(resolved_sandbox_mode, SandboxMode::WorkspaceWrite)
        {
            SandboxMode::ReadOnly
        } else {
            resolved_sandbox_mode
        };

        let permission_profile = match effective_sandbox_mode {
            SandboxMode::ReadOnly => PermissionProfile::read_only(),
            SandboxMode::WorkspaceWrite => match self.sandbox_workspace_write.as_ref() {
                Some(SandboxWorkspaceWrite {
                    writable_roots,
                    network_access,
                    exclude_tmpdir_env_var,
                    exclude_slash_tmp,
                }) => {
                    let network_policy = if *network_access {
                        NetworkSandboxPolicy::Enabled
                    } else {
                        NetworkSandboxPolicy::Restricted
                    };
                    PermissionProfile::workspace_write_with(
                        writable_roots,
                        network_policy,
                        *exclude_tmpdir_env_var,
                        *exclude_slash_tmp,
                    )
                }
                None => PermissionProfile::workspace_write(),
            },
            SandboxMode::DangerFullAccess => PermissionProfile::Disabled,
        };
        if configured_sandbox_mode.is_none()
            && let Some(constraint) = permission_profile_constraint
            && let Err(err) = constraint.can_set(&permission_profile)
        {
            tracing::warn!(
                error = %err,
                "default sandbox policy is disallowed by requirements; falling back to required default"
            );
            PermissionProfile::read_only()
        } else {
            permission_profile
        }
    }

    pub fn get_active_project(
        &self,
        resolved_cwd: &Path,
        repo_root: Option<&Path>,
    ) -> Option<ProjectConfig> {
        let projects = self.projects.as_ref()?;

        for normalized_cwd in normalized_project_lookup_keys(resolved_cwd) {
            if let Some(project_config) = project_config_for_lookup_key(projects, &normalized_cwd) {
                return Some(project_config);
            }
        }

        if let Some(repo_root) = repo_root {
            for normalized_repo_root in normalized_project_lookup_keys(repo_root) {
                if let Some(project_config_for_root) =
                    project_config_for_lookup_key(projects, &normalized_repo_root)
                {
                    return Some(project_config_for_root);
                }
            }
        }

        None
    }
}

fn normalized_project_lookup_keys(path: &Path) -> Vec<String> {
    let normalized_path = normalize_project_lookup_key(path.to_string_lossy().to_string());
    let normalized_canonical_path = normalize_project_lookup_key(
        normalize_for_path_comparison(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string(),
    );
    if normalized_path == normalized_canonical_path {
        vec![normalized_canonical_path]
    } else {
        vec![normalized_canonical_path, normalized_path]
    }
}

fn normalize_project_lookup_key(key: String) -> String {
    if cfg!(windows) {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

fn project_config_for_lookup_key(
    projects: &HashMap<String, ProjectConfig>,
    lookup_key: &str,
) -> Option<ProjectConfig> {
    if let Some(project_config) = projects.get(lookup_key) {
        return Some(project_config.clone());
    }

    let mut normalized_matches: Vec<_> = projects
        .iter()
        .filter(|(key, _)| normalize_project_lookup_key((*key).clone()) == lookup_key)
        .collect();
    normalized_matches.sort_by_key(|(key, _)| *key);
    normalized_matches
        .first()
        .map(|(_, project_config)| (**project_config).clone())
}

pub fn validate_model_providers(
    model_providers: &HashMap<String, ModelProviderInfo>,
) -> Result<(), String> {
    for (key, provider) in model_providers {
        if provider.name.trim().is_empty() {
            return Err(format!(
                "model_providers.{key}: provider name must not be empty"
            ));
        }
        provider
            .validate()
            .map_err(|message| format!("model_providers.{key}: {message}"))?;
    }
    Ok(())
}

fn deserialize_model_providers<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, ModelProviderInfo>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let model_providers = HashMap::<String, ModelProviderInfo>::deserialize(deserializer)?;
    validate_model_providers(&model_providers).map_err(serde::de::Error::custom)?;
    Ok(model_providers)
}

pub fn validate_oss_provider(provider: &str) -> std::io::Result<()> {
    match provider {
        LMSTUDIO_OSS_PROVIDER_ID | OLLAMA_OSS_PROVIDER_ID => Ok(()),
        LEGACY_OLLAMA_CHAT_PROVIDER_ID => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            OLLAMA_CHAT_PROVIDER_REMOVED_ERROR,
        )),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Invalid OSS provider '{provider}'. Must be one of: {LMSTUDIO_OSS_PROVIDER_ID}, {OLLAMA_OSS_PROVIDER_ID}"
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const WORKSPACE_ID_A: &str = "123e4567-e89b-42d3-a456-426614174000";
    const WORKSPACE_ID_B: &str = "123e4567-e89b-42d3-a456-426614174001";

    #[test]
    fn forced_chatgpt_workspace_id_accepts_single_string() {
        let config: ConfigToml = toml::from_str(&format!(
            r#"forced_chatgpt_workspace_id = "{WORKSPACE_ID_A}""#
        ))
        .expect("single workspace id should deserialize");

        assert_eq!(
            config
                .forced_chatgpt_workspace_id
                .expect("workspace id should be set")
                .into_vec(),
            vec![WORKSPACE_ID_A.to_string()]
        );
    }

    #[test]
    fn forced_chatgpt_workspace_id_accepts_string_list() {
        let config: ConfigToml = toml::from_str(&format!(
            r#"forced_chatgpt_workspace_id = ["{WORKSPACE_ID_A}", "{WORKSPACE_ID_B}"]"#
        ))
        .expect("workspace id list should deserialize");

        assert_eq!(
            config
                .forced_chatgpt_workspace_id
                .expect("workspace ids should be set")
                .into_vec(),
            vec![WORKSPACE_ID_A.to_string(), WORKSPACE_ID_B.to_string()]
        );
    }

    #[test]
    fn forced_chatgpt_workspace_id_rejects_comma_separated_string() {
        let err = toml::from_str::<ConfigToml>(&format!(
            r#"forced_chatgpt_workspace_id = "{WORKSPACE_ID_A},{WORKSPACE_ID_B}""#
        ))
        .expect_err("comma-separated string should be rejected");

        let message = err.to_string();
        assert!(message.contains("TOML list of strings"));
        assert!(message.contains("comma-separated strings are not supported"));
    }
}
