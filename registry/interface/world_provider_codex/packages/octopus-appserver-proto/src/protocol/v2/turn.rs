use super::ApprovalsReviewer;
use super::AskForApproval;
use super::SandboxPolicy;
use super::Turn;
use octopus_experimental_api_macros::ExperimentalApi;
use octopus_protocol::config_types::CollaborationMode;
use octopus_protocol::config_types::MultiAgentMode;
use octopus_protocol::config_types::Personality;
use octopus_protocol::config_types::ReasoningSummary;
use octopus_protocol::models::FunctionCallOutputBody;
use octopus_protocol::models::ImageDetail;
use octopus_protocol::openai_models::ReasoningEffort;
use octopus_protocol::plan_tool::PlanItemArg as CorePlanItemArg;
use octopus_protocol::plan_tool::StepStatus as CorePlanStepStatus;
use octopus_protocol::user_input::ByteRange as CoreByteRange;
use octopus_protocol::user_input::TextElement as CoreTextElement;
use octopus_protocol::user_input::UserInput as CoreUserInput;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_pathuri::LegacyAppPathString;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::PathBuf;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
    InProgress,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, ExperimentalApi)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnEnvironmentParams {
    pub environment_id: String,
    pub cwd: LegacyAppPathString,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
#[ts(export_to = "v2/")]
pub enum AdditionalContextKind {
    Untrusted,
    Application,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AdditionalContextEntry {
    pub value: String,
    pub kind: AdditionalContextKind,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnToolOutput {
    pub name: String,
    pub namespace: Option<String>,
    pub output: FunctionCallOutputBody,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retain: bool,
}

#[derive(
    Serialize, Deserialize, Debug, Default, Clone, PartialEq, JsonSchema, TS, ExperimentalApi,
)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnStartParams {
    pub thread_id: String,
    #[ts(optional = nullable)]
    pub disabled_plugin_ids: Option<Vec<String>>,
    #[ts(optional = nullable)]
    pub client_user_message_id: Option<String>,
    pub input: Vec<UserInput>,
    #[ts(optional = nullable)]
    pub turn_trigger: Option<String>,
    #[ts(optional = nullable)]
    pub parent_turn_id: Option<String>,
    #[ts(optional = nullable)]
    pub root_turn_id: Option<String>,
    #[ts(optional = nullable)]
    pub tool_output: Option<Box<TurnToolOutput>>,
    #[experimental("turn/start.responsesapiClientMetadata")]
    #[ts(optional = nullable)]
    pub responsesapi_client_metadata: Option<HashMap<String, String>>,
    #[experimental("turn/start.responsesapiClientMetadataPatch")]
    #[ts(optional = nullable)]
    pub responsesapi_client_metadata_patch: Option<HashMap<String, String>>,
    #[experimental("turn/start.additionalContext")]
    #[ts(optional = nullable)]
    pub additional_context: Option<HashMap<String, AdditionalContextEntry>>,
    #[experimental("turn/start.environments")]
    #[ts(optional = nullable)]
    pub environments: Option<Vec<TurnEnvironmentParams>>,
    #[ts(optional = nullable)]
    pub cwd: Option<PathBuf>,
    #[experimental("turn/start.runtimeWorkspaceRoots")]
    #[ts(optional = nullable)]
    pub runtime_workspace_roots: Option<Vec<AbsolutePathBuf>>,
    #[experimental(nested)]
    #[ts(optional = nullable)]
    pub approval_policy: Option<AskForApproval>,
    #[ts(optional = nullable)]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[ts(optional = nullable)]
    pub sandbox_policy: Option<SandboxPolicy>,
    #[experimental("turn/start.permissions")]
    #[ts(optional = nullable)]
    pub permissions: Option<String>,
    #[ts(optional = nullable)]
    pub model: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::protocol::serde_helpers::deserialize_double_option",
        serialize_with = "crate::protocol::serde_helpers::serialize_double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional = nullable)]
    pub service_tier: Option<Option<String>>,
    #[ts(optional = nullable)]
    pub service_tier_for_turn: Option<String>,
    #[ts(optional = nullable)]
    pub effort: Option<ReasoningEffort>,
    #[ts(optional = nullable)]
    pub summary: Option<ReasoningSummary>,
    #[ts(optional = nullable)]
    pub personality: Option<Personality>,
    #[ts(optional = nullable)]
    pub output_schema: Option<JsonValue>,

    #[experimental("turn/start.collaborationMode")]
    #[ts(optional = nullable)]
    pub collaboration_mode: Option<CollaborationMode>,

    #[experimental("turn/start.multiAgentMode")]
    #[ts(optional = nullable)]
    pub multi_agent_mode: Option<MultiAgentMode>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnStartResponse {
    pub turn: Turn,
}

#[derive(
    Serialize, Deserialize, Debug, Default, Clone, PartialEq, JsonSchema, TS, ExperimentalApi,
)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnSteerParams {
    pub thread_id: String,
    #[ts(optional = nullable)]
    pub client_user_message_id: Option<String>,
    pub input: Vec<UserInput>,
    #[experimental("turn/steer.responsesapiClientMetadata")]
    #[ts(optional = nullable)]
    pub responsesapi_client_metadata: Option<HashMap<String, String>>,
    #[experimental("turn/steer.additionalContext")]
    #[ts(optional = nullable)]
    pub additional_context: Option<HashMap<String, AdditionalContextEntry>>,
    pub expected_turn_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnSteerResponse {
    pub turn_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnInterruptResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl From<CoreByteRange> for ByteRange {
    fn from(value: CoreByteRange) -> Self {
        Self {
            start: value.start,
            end: value.end,
        }
    }
}

impl From<ByteRange> for CoreByteRange {
    fn from(value: ByteRange) -> Self {
        Self {
            start: value.start,
            end: value.end,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TextElement {
    pub byte_range: ByteRange,
    placeholder: Option<String>,
}

impl TextElement {
    pub fn new(byte_range: ByteRange, placeholder: Option<String>) -> Self {
        Self {
            byte_range,
            placeholder,
        }
    }

    pub fn set_placeholder(&mut self, placeholder: Option<String>) {
        self.placeholder = placeholder;
    }

    pub fn placeholder(&self) -> Option<&str> {
        self.placeholder.as_deref()
    }
}

impl From<CoreTextElement> for TextElement {
    fn from(value: CoreTextElement) -> Self {
        Self::new(
            value.byte_range.into(),
            value._placeholder_for_conversion_only().map(str::to_string),
        )
    }
}

impl From<TextElement> for CoreTextElement {
    fn from(value: TextElement) -> Self {
        Self::new(value.byte_range.into(), value.placeholder)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
#[ts(export_to = "v2/")]
pub enum UserInput {
    Text {
        text: String,
        #[serde(default)]
        text_elements: Vec<TextElement>,
    },
    Image {
        #[serde(default)]
        #[ts(optional)]
        detail: Option<ImageDetail>,
        url: String,
    },
    LocalImage {
        #[serde(default)]
        #[ts(optional)]
        detail: Option<ImageDetail>,
        path: PathBuf,
    },
    Skill {
        name: String,
        path: PathBuf,
    },
    Mention {
        name: String,
        path: String,
    },
}

impl UserInput {
    pub fn into_core(self) -> CoreUserInput {
        match self {
            UserInput::Text {
                text,
                text_elements,
            } => CoreUserInput::Text {
                text,
                text_elements: text_elements.into_iter().map(Into::into).collect(),
            },
            UserInput::Image { url, detail } => CoreUserInput::Image {
                image_url: url,
                detail,
            },
            UserInput::LocalImage { path, detail } => CoreUserInput::LocalImage { path, detail },
            UserInput::Skill { name, path } => CoreUserInput::Skill { name, path },
            UserInput::Mention { name, path } => CoreUserInput::Mention { name, path },
        }
    }
}

impl From<CoreUserInput> for UserInput {
    fn from(value: CoreUserInput) -> Self {
        match value {
            CoreUserInput::Text {
                text,
                text_elements,
            } => UserInput::Text {
                text,
                text_elements: text_elements.into_iter().map(Into::into).collect(),
            },
            CoreUserInput::Image { image_url, detail } => UserInput::Image {
                url: image_url,
                detail,
            },
            CoreUserInput::LocalImage { path, detail } => UserInput::LocalImage { path, detail },
            CoreUserInput::Skill { name, path } => UserInput::Skill { name, path },
            CoreUserInput::Mention { name, path } => UserInput::Mention { name, path },
            _ => unreachable!("unsupported user input variant"),
        }
    }
}

impl UserInput {
    pub fn text_char_count(&self) -> usize {
        match self {
            UserInput::Text { text, .. } => text.chars().count(),
            UserInput::Image { .. }
            | UserInput::LocalImage { .. }
            | UserInput::Skill { .. }
            | UserInput::Mention { .. } => 0,
        }
    }
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnStartedNotification {
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct Usage {
    pub input_tokens: i32,
    pub cached_input_tokens: i32,
    pub output_tokens: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnCompletedNotification {
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnDiffUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub diff: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnPlanUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub explanation: Option<String>,
    pub plan: Vec<TurnPlanStep>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct TurnPlanStep {
    pub step: String,
    pub status: TurnPlanStepStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum TurnPlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

impl From<CorePlanItemArg> for TurnPlanStep {
    fn from(value: CorePlanItemArg) -> Self {
        Self {
            step: value.step,
            status: value.status.into(),
        }
    }
}

impl From<CorePlanStepStatus> for TurnPlanStepStatus {
    fn from(value: CorePlanStepStatus) -> Self {
        match value {
            CorePlanStepStatus::Pending => Self::Pending,
            CorePlanStepStatus::InProgress => Self::InProgress,
            CorePlanStepStatus::Completed => Self::Completed,
        }
    }
}
