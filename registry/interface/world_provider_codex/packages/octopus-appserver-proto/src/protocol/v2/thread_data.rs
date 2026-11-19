use super::CodexErrorInfo;
use super::ThreadItem;
use super::ThreadStatus;
use super::TurnStatus;
use octopus_protocol::protocol::SessionSource as CoreSessionSource;
use octopus_protocol::protocol::SubAgentSource as CoreSubAgentSource;
use octopus_protocol::protocol::ThreadSource as CoreThreadSource;
use octopus_util_abspath::AbsolutePathBuf;
use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::Schema;
use serde::Deserialize;
use serde::Serialize;
use std::path::PathBuf;
use thiserror::Error;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
#[derive(Default)]
pub enum SessionSource {
    Cli,
    #[serde(rename = "vscode")]
    #[ts(rename = "vscode")]
    #[default]
    VsCode,
    Exec,
    AppServer,
    Custom(String),
    SubAgent(CoreSubAgentSource),
    #[serde(other)]
    Unknown,
}

impl From<CoreSessionSource> for SessionSource {
    fn from(value: CoreSessionSource) -> Self {
        match value {
            CoreSessionSource::Cli => SessionSource::Cli,
            CoreSessionSource::VSCode => SessionSource::VsCode,
            CoreSessionSource::Exec => SessionSource::Exec,
            CoreSessionSource::Mcp => SessionSource::AppServer,
            CoreSessionSource::Custom(source) => SessionSource::Custom(source),
            CoreSessionSource::Internal(_) => SessionSource::Unknown,
            CoreSessionSource::SubAgent(sub) => SessionSource::SubAgent(sub),
            CoreSessionSource::Unknown => SessionSource::Unknown,
        }
    }
}

impl From<SessionSource> for CoreSessionSource {
    fn from(value: SessionSource) -> Self {
        match value {
            SessionSource::Cli => CoreSessionSource::Cli,
            SessionSource::VsCode => CoreSessionSource::VSCode,
            SessionSource::Exec => CoreSessionSource::Exec,
            SessionSource::AppServer => CoreSessionSource::Mcp,
            SessionSource::Custom(source) => CoreSessionSource::Custom(source),
            SessionSource::SubAgent(sub) => CoreSessionSource::SubAgent(sub),
            SessionSource::Unknown => CoreSessionSource::Unknown,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
#[ts(export_to = "v2/")]
pub enum ThreadSource {
    User,
    Subagent,
    Feature(String),
    MemoryConsolidation,
}

impl JsonSchema for ThreadSource {
    fn schema_name() -> String {
        "ThreadSource".to_string()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(generator)
    }
}

impl TryFrom<String> for ThreadSource {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse::<CoreThreadSource>().map(Into::into)
    }
}

impl From<ThreadSource> for String {
    fn from(value: ThreadSource) -> Self {
        CoreThreadSource::from(value).into()
    }
}

impl From<CoreThreadSource> for ThreadSource {
    fn from(value: CoreThreadSource) -> Self {
        match value {
            CoreThreadSource::User => ThreadSource::User,
            CoreThreadSource::Subagent => ThreadSource::Subagent,
            CoreThreadSource::Feature(feature) => ThreadSource::Feature(feature),
            CoreThreadSource::MemoryConsolidation => ThreadSource::MemoryConsolidation,
        }
    }
}

impl From<ThreadSource> for CoreThreadSource {
    fn from(value: ThreadSource) -> Self {
        match value {
            ThreadSource::User => CoreThreadSource::User,
            ThreadSource::Subagent => CoreThreadSource::Subagent,
            ThreadSource::Feature(feature) => CoreThreadSource::Feature(feature),
            ThreadSource::MemoryConsolidation => CoreThreadSource::MemoryConsolidation,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GitInfo {
    pub sha: Option<String>,
    pub branch: Option<String>,
    pub origin_url: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct Thread {
    pub id: String,
    pub session_id: String,
    pub forked_from_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub preview: String,
    pub ephemeral: bool,
    pub model_provider: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    #[ts(type = "number | null")]
    pub recency_at: Option<i64>,
    pub status: ThreadStatus,
    pub path: Option<PathBuf>,
    pub cwd: AbsolutePathBuf,
    pub cli_version: String,
    pub source: SessionSource,
    pub thread_source: Option<ThreadSource>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub git_info: Option<GitInfo>,
    pub name: Option<String>,
    pub turns: Vec<Turn>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct Turn {
    pub id: String,
    pub items: Vec<ThreadItem>,
    #[serde(default)]
    pub items_view: TurnItemsView,
    pub status: TurnStatus,
    pub error: Option<TurnError>,
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    #[ts(type = "number | null")]
    pub completed_at: Option<i64>,
    #[ts(type = "number | null")]
    pub duration_ms: Option<i64>,
}

#[derive(Default, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum TurnItemsView {
    NotLoaded,
    Summary,
    #[default]
    Full,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, Error)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
#[error("{message}")]
pub struct TurnError {
    pub message: String,
    pub codex_error_info: Option<CodexErrorInfo>,
    #[serde(default)]
    pub additional_details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "http_status_code")]
    #[ts(optional)]
    pub http_status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "retry_after_seconds")]
    #[ts(optional)]
    pub retry_after_seconds: Option<u64>,
}
