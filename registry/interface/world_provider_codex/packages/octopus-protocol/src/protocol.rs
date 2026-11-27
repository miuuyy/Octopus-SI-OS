use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt;
use std::ops::Mul;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use strum_macros::EnumIter;

use crate::AgentPath;
use crate::approvals::ElicitationRequestEvent;
use crate::config_types::ApprovalsReviewer;
use crate::config_types::CollaborationMode;
use crate::config_types::ModeKind;
use crate::config_types::MultiAgentMode;
use crate::config_types::Personality;
use crate::config_types::ReasoningSummary as ReasoningSummaryConfig;
use crate::config_types::WindowsSandboxLevel;
use crate::dynamic_tools::DynamicToolCallOutputContentItem;
use crate::dynamic_tools::DynamicToolCallRequest;
use crate::dynamic_tools::DynamicToolResponse;
use crate::dynamic_tools::DynamicToolSpec;
use crate::items::TurnItem;
use crate::mcp::CallToolResult;
use crate::mcp::RequestId;
use crate::models::ActivePermissionProfile;
use crate::models::AgentMessageInputContent;
use crate::models::BaseInstructions;
use crate::models::ContentItem;
use crate::models::ImageDetail;
use crate::models::MessagePhase;
use crate::models::PermissionProfile;
use crate::models::ResponseInputItem;
use crate::models::ResponseItem;
use crate::models::ResponseItemMetadata;
use crate::models::SandboxEnforcement;
use crate::models::WebSearchAction;
use crate::num_format::format_with_separators;
use crate::openai_models::ReasoningEffort as ReasoningEffortConfig;
use crate::parse_command::ParsedCommand;
use crate::plan_tool::UpdatePlanArgs;
use crate::request_permissions::RequestPermissionsEvent;
use crate::request_permissions::RequestPermissionsResponse;
use crate::request_user_input::RequestUserInputResponse;
use crate::user_input::UserInput;
use octopus_util_abspath::AbsolutePathBuf;
use octopus_util_pathuri::PathUri;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_with::serde_as;
use strum_macros::Display;
use ts_rs::TS;

pub use crate::approvals::ApplyPatchApprovalRequestEvent;
pub use crate::approvals::ElicitationAction;
pub use crate::approvals::ExecApprovalRequestEvent;
pub use crate::approvals::ExecPolicyAmendment;
pub use crate::approvals::GuardianAssessmentAction;
pub use crate::approvals::GuardianAssessmentDecisionSource;
pub use crate::approvals::GuardianAssessmentEvent;
pub use crate::approvals::GuardianAssessmentOutcome;
pub use crate::approvals::GuardianAssessmentStatus;
pub use crate::approvals::GuardianCommandSource;
pub use crate::approvals::GuardianRiskLevel;
pub use crate::approvals::GuardianUserAuthorization;
pub use crate::approvals::NetworkApprovalContext;
pub use crate::approvals::NetworkApprovalProtocol;
pub use crate::approvals::NetworkPolicyAmendment;
pub use crate::approvals::NetworkPolicyRuleAction;
pub use crate::permissions::FileSystemAccessMode;
pub use crate::permissions::FileSystemPath;
pub use crate::permissions::FileSystemSandboxEntry;
pub use crate::permissions::FileSystemSandboxKind;
pub use crate::permissions::FileSystemSandboxPolicy;
pub use crate::permissions::FileSystemSpecialPath;
pub use crate::permissions::NetworkSandboxPolicy;
pub use crate::request_permissions::RequestPermissionsArgs;
pub use crate::request_user_input::RequestUserInputEvent;

pub const USER_INSTRUCTIONS_OPEN_TAG: &str = "<user_instructions>";
pub const USER_INSTRUCTIONS_CLOSE_TAG: &str = "</user_instructions>";
pub const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
pub const ENVIRONMENT_CONTEXT_CLOSE_TAG: &str = "</environment_context>";
pub const APPS_INSTRUCTIONS_OPEN_TAG: &str = "<apps_instructions>";
pub const APPS_INSTRUCTIONS_CLOSE_TAG: &str = "</apps_instructions>";
pub const SKILLS_INSTRUCTIONS_OPEN_TAG: &str = "<skills_instructions>";
pub const SKILLS_INSTRUCTIONS_CLOSE_TAG: &str = "</skills_instructions>";
pub const PLUGINS_INSTRUCTIONS_OPEN_TAG: &str = "<plugins_instructions>";
pub const PLUGINS_INSTRUCTIONS_CLOSE_TAG: &str = "</plugins_instructions>";
pub const COLLABORATION_MODE_OPEN_TAG: &str = "<collaboration_mode>";
pub const COLLABORATION_MODE_CLOSE_TAG: &str = "</collaboration_mode>";
pub const MULTI_AGENT_MODE_OPEN_TAG: &str = "<multi_agent_mode>";
pub const MULTI_AGENT_MODE_CLOSE_TAG: &str = "</multi_agent_mode>";
pub const REALTIME_CONVERSATION_OPEN_TAG: &str = "<realtime_conversation>";
pub const REALTIME_CONVERSATION_CLOSE_TAG: &str = "</realtime_conversation>";
pub const USER_MESSAGE_BEGIN: &str = "## My request for Codex:";

#[derive(Debug, Clone, PartialEq)]
pub struct TurnEnvironmentSelection {
    pub environment_id: String,
    pub cwd: PathUri,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnEnvironmentSelections {
    pub legacy_fallback_cwd: AbsolutePathBuf,
    pub environments: Vec<TurnEnvironmentSelection>,
}

impl TurnEnvironmentSelections {
    pub fn new(
        legacy_fallback_cwd: AbsolutePathBuf,
        environments: Vec<TurnEnvironmentSelection>,
    ) -> Self {
        Self {
            legacy_fallback_cwd,
            environments,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Submission {
    pub id: String,
    pub op: Op,
    pub client_user_message_id: Option<String>,
    pub trace: Option<W3cTraceContext>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct W3cTraceContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub traceparent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tracestate: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpServerRefreshConfig {
    pub mcp_servers: Value,
    pub mcp_oauth_credentials_store_mode: Value,
    pub auth_keyring_backend_kind: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationStartParams {
    pub client_managed_handoffs: bool,
    pub codex_responses_as_items: bool,
    pub codex_response_item_prefix: Option<String>,
    pub codex_response_handoff_prefix: Option<String>,
    pub model: Option<String>,
    pub output_modality: RealtimeOutputModality,
    pub include_startup_context: bool,
    pub prompt: Option<Option<String>>,
    pub realtime_session_id: Option<String>,
    pub transport: Option<ConversationStartTransport>,
    pub version: Option<RealtimeConversationVersion>,
    pub voice: Option<RealtimeVoice>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConversationStartTransport {
    Websocket,
    Webrtc { sdp: String },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeOutputModality {
    Text,
    Audio,
}

#[derive(
    Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash, JsonSchema, TS, Ord, PartialOrd,
)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum RealtimeVoice {
    Alloy,
    Arbor,
    Ash,
    Ballad,
    Breeze,
    Cedar,
    Coral,
    Cove,
    Echo,
    Ember,
    Juniper,
    Maple,
    Marin,
    Sage,
    Shimmer,
    Sol,
    Spruce,
    Vale,
    Verse,
}

impl RealtimeVoice {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Alloy => "alloy",
            Self::Arbor => "arbor",
            Self::Ash => "ash",
            Self::Ballad => "ballad",
            Self::Breeze => "breeze",
            Self::Cedar => "cedar",
            Self::Coral => "coral",
            Self::Cove => "cove",
            Self::Echo => "echo",
            Self::Ember => "ember",
            Self::Juniper => "juniper",
            Self::Maple => "maple",
            Self::Marin => "marin",
            Self::Sage => "sage",
            Self::Shimmer => "shimmer",
            Self::Sol => "sol",
            Self::Spruce => "spruce",
            Self::Vale => "vale",
            Self::Verse => "verse",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RealtimeVoicesList {
    pub v1: Vec<RealtimeVoice>,
    pub v2: Vec<RealtimeVoice>,
    pub default_v1: RealtimeVoice,
    pub default_v2: RealtimeVoice,
}

impl RealtimeVoicesList {
    pub fn builtin() -> Self {
        Self {
            v1: vec![
                RealtimeVoice::Juniper,
                RealtimeVoice::Maple,
                RealtimeVoice::Spruce,
                RealtimeVoice::Ember,
                RealtimeVoice::Vale,
                RealtimeVoice::Breeze,
                RealtimeVoice::Arbor,
                RealtimeVoice::Sol,
                RealtimeVoice::Cove,
            ],
            v2: vec![
                RealtimeVoice::Alloy,
                RealtimeVoice::Ash,
                RealtimeVoice::Ballad,
                RealtimeVoice::Coral,
                RealtimeVoice::Echo,
                RealtimeVoice::Sage,
                RealtimeVoice::Shimmer,
                RealtimeVoice::Verse,
                RealtimeVoice::Marin,
                RealtimeVoice::Cedar,
            ],
            default_v1: RealtimeVoice::Cove,
            default_v2: RealtimeVoice::Marin,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeAudioFrame {
    pub data: String,
    pub sample_rate: u32,
    pub num_channels: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples_per_channel: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeTranscriptDelta {
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeTranscriptDone {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeTranscriptEntry {
    pub role: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeHandoffRequested {
    pub handoff_id: String,
    pub item_id: String,
    pub input_transcript: String,
    pub active_transcript: Vec<RealtimeTranscriptEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeNoopRequested {
    pub call_id: String,
    pub item_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeInputAudioSpeechStarted {
    pub item_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeResponseCancelled {
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeResponseCreated {
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeResponseDone {
    pub response_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub enum RealtimeEvent {
    SessionUpdated {
        realtime_session_id: String,
        instructions: Option<String>,
    },
    InputAudioSpeechStarted(RealtimeInputAudioSpeechStarted),
    InputTranscriptDelta(RealtimeTranscriptDelta),
    InputTranscriptDone(RealtimeTranscriptDone),
    OutputTranscriptDelta(RealtimeTranscriptDelta),
    OutputTranscriptDone(RealtimeTranscriptDone),
    AudioOut(RealtimeAudioFrame),
    ResponseCreated(RealtimeResponseCreated),
    ResponseCancelled(RealtimeResponseCancelled),
    ResponseDone(RealtimeResponseDone),
    ConversationItemAdded(Value),
    ConversationItemDone {
        item_id: String,
    },
    HandoffRequested(RealtimeHandoffRequested),
    NoopRequested(RealtimeNoopRequested),
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationAudioParams {
    pub frame: RealtimeAudioFrame,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationTextParams {
    pub text: String,
    pub role: ConversationTextRole,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ConversationTextRole {
    #[default]
    User,
    Developer,
    Assistant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationSpeechParams {
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadSettingsOverrides {
    pub environments: Option<TurnEnvironmentSelections>,

    pub workspace_roots: Option<Vec<AbsolutePathBuf>>,

    pub profile_workspace_roots: Option<Vec<AbsolutePathBuf>>,

    pub approval_policy: Option<AskForApproval>,

    pub approvals_reviewer: Option<ApprovalsReviewer>,

    pub sandbox_policy: Option<SandboxPolicy>,

    pub permission_profile: Option<PermissionProfile>,

    pub active_permission_profile: Option<ActivePermissionProfile>,

    pub windows_sandbox_level: Option<WindowsSandboxLevel>,

    pub model: Option<String>,

    pub effort: Option<Option<ReasoningEffortConfig>>,

    pub summary: Option<ReasoningSummaryConfig>,

    pub service_tier: Option<Option<String>>,

    pub collaboration_mode: Option<CollaborationMode>,

    pub multi_agent_mode: Option<MultiAgentMode>,

    pub personality: Option<Personality>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdditionalContextKind {
    Untrusted,
    Application,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionalContextEntry {
    pub value: String,
    pub kind: AdditionalContextKind,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
#[non_exhaustive]
pub enum Op {
    Interrupt,

    CleanBackgroundTerminals,

    RealtimeConversationStart(ConversationStartParams),

    RealtimeConversationAudio(ConversationAudioParams),

    RealtimeConversationText(ConversationTextParams),

    RealtimeConversationSpeech(ConversationSpeechParams),

    RealtimeConversationClose,

    RealtimeConversationListVoices,

    UserInput {
        items: Vec<UserInput>,
        final_output_json_schema: Option<Value>,
        responsesapi_client_metadata: Option<HashMap<String, String>>,
        additional_context: BTreeMap<String, AdditionalContextEntry>,

        thread_settings: ThreadSettingsOverrides,
    },

    ThreadSettings {
        thread_settings: ThreadSettingsOverrides,
    },

    InterAgentCommunication {
        communication: InterAgentCommunication,
    },

    ExecApproval {
        id: String,
        turn_id: Option<String>,
        decision: ReviewDecision,
    },

    PatchApproval {
        id: String,
        decision: ReviewDecision,
    },

    ResolveElicitation {
        server_name: String,
        request_id: RequestId,
        decision: ElicitationAction,
        content: Option<Value>,
        meta: Option<Value>,
    },

    UserInputAnswer {
        id: String,
        response: RequestUserInputResponse,
    },

    RequestPermissionsResponse {
        id: String,
        response: RequestPermissionsResponse,
    },

    DynamicToolResponse {
        id: String,
        response: DynamicToolResponse,
    },

    RefreshMcpServers {
        config: McpServerRefreshConfig,
    },

    ReloadUserConfig,

    Compact,

    SetThreadMemoryMode {
        mode: ThreadMemoryMode,
    },

    ThreadRollback {
        num_turns: u32,
    },

    Review {
        review_request: ReviewRequest,
    },

    ApproveGuardianDeniedAction {
        event: GuardianAssessmentEvent,
    },

    Shutdown,

    RunUserShellCommand {
        command: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ThreadMemoryMode {
    Enabled,
    Disabled,
}

impl From<Vec<UserInput>> for Op {
    fn from(value: Vec<UserInput>) -> Self {
        Op::UserInput {
            items: value,
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: ThreadSettingsOverrides::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
pub struct InterAgentCommunication {
    pub author: AgentPath,
    pub recipient: AgentPath,
    #[serde(default)]
    pub other_recipients: Vec<AgentPath>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub encrypted_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub metadata: Option<ResponseItemMetadata>,
    pub trigger_turn: bool,
}

impl InterAgentCommunication {
    pub fn new(
        author: AgentPath,
        recipient: AgentPath,
        other_recipients: Vec<AgentPath>,
        content: String,
        trigger_turn: bool,
    ) -> Self {
        Self {
            author,
            recipient,
            other_recipients,
            content,
            encrypted_content: None,
            metadata: None,
            trigger_turn,
        }
    }

    pub fn new_encrypted(
        author: AgentPath,
        recipient: AgentPath,
        other_recipients: Vec<AgentPath>,
        encrypted_content: String,
        trigger_turn: bool,
    ) -> Self {
        Self {
            author,
            recipient,
            other_recipients,
            content: String::new(),
            encrypted_content: Some(encrypted_content),
            metadata: None,
            trigger_turn,
        }
    }

    pub fn to_response_input_item(&self) -> ResponseInputItem {
        ResponseInputItem::Message {
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: serde_json::to_string(self).unwrap_or_default(),
            }],
            phase: Some(MessagePhase::Commentary),
        }
    }

    pub fn to_model_input_item(&self) -> ResponseItem {
        let content = match &self.encrypted_content {
            Some(encrypted_content) => {
                let message_type = if self.trigger_turn {
                    "NEW_TASK"
                } else {
                    "MESSAGE"
                };
                vec![
                    AgentMessageInputContent::InputText {
                        text: format!(
                            "Message Type: {message_type}\nTask name: {}\nSender: {}\nPayload:\n",
                            self.recipient, self.author
                        ),
                    },
                    AgentMessageInputContent::EncryptedContent {
                        encrypted_content: encrypted_content.clone(),
                    },
                ]
            }
            None => vec![AgentMessageInputContent::InputText {
                text: self.content.clone(),
            }],
        };
        ResponseItem::AgentMessage {
            id: None,
            author: self.author.to_string(),
            recipient: self.recipient.to_string(),
            content,
            metadata: self.metadata.clone(),
        }
    }

    pub fn is_message_content(content: &[ContentItem]) -> bool {
        Self::from_message_content(content).is_some()
    }

    pub fn from_message_content(content: &[ContentItem]) -> Option<Self> {
        match content {
            [ContentItem::InputText { text }] | [ContentItem::OutputText { text }] => {
                serde_json::from_str(text).ok()
            }
            _ => None,
        }
    }
}

impl Op {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::CleanBackgroundTerminals => "clean_background_terminals",
            Self::RealtimeConversationStart(_) => "realtime_conversation_start",
            Self::RealtimeConversationAudio(_) => "realtime_conversation_audio",
            Self::RealtimeConversationText(_) => "realtime_conversation_text",
            Self::RealtimeConversationSpeech(_) => "realtime_conversation_speech",
            Self::RealtimeConversationClose => "realtime_conversation_close",
            Self::RealtimeConversationListVoices => "realtime_conversation_list_voices",
            Self::UserInput { .. } => "user_input",
            Self::ThreadSettings { .. } => "thread_settings",
            Self::InterAgentCommunication { .. } => "inter_agent_communication",
            Self::ExecApproval { .. } => "exec_approval",
            Self::PatchApproval { .. } => "patch_approval",
            Self::ResolveElicitation { .. } => "resolve_elicitation",
            Self::UserInputAnswer { .. } => "user_input_answer",
            Self::RequestPermissionsResponse { .. } => "request_permissions_response",
            Self::DynamicToolResponse { .. } => "dynamic_tool_response",
            Self::RefreshMcpServers { .. } => "refresh_mcp_servers",
            Self::ReloadUserConfig => "reload_user_config",
            Self::Compact => "compact",
            Self::SetThreadMemoryMode { .. } => "set_thread_memory_mode",
            Self::ThreadRollback { .. } => "thread_rollback",
            Self::Review { .. } => "review",
            Self::ApproveGuardianDeniedAction { .. } => "approve_guardian_denied_action",
            Self::Shutdown => "shutdown",
            Self::RunUserShellCommand { .. } => "run_user_shell_command",
        }
    }
}

pub use octopus_substrate_contract::approvals_substrate::AskForApproval;
pub use octopus_substrate_contract::approvals_substrate::GranularApprovalConfig;

pub use octopus_substrate_contract::sandbox_policy::GitSha;
pub use octopus_substrate_contract::sandbox_policy::NetworkAccess;
pub use octopus_substrate_contract::sandbox_policy::SandboxPolicy;
pub use octopus_substrate_contract::sandbox_policy::WritableRoot;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    pub id: String,
    pub msg: EventMsg,
}

#[derive(Debug, Clone, Deserialize, Serialize, Display, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
#[strum(serialize_all = "snake_case")]
pub enum EventMsg {
    Error(ErrorEvent),

    Warning(WarningEvent),

    GuardianWarning(WarningEvent),

    RealtimeConversationStarted(RealtimeConversationStartedEvent),

    RealtimeConversationRealtime(RealtimeConversationRealtimeEvent),

    RealtimeConversationClosed(RealtimeConversationClosedEvent),

    RealtimeConversationSdp(RealtimeConversationSdpEvent),

    ModelReroute(ModelRerouteEvent),

    ModelVerification(ModelVerificationEvent),

    TurnModerationMetadata(TurnModerationMetadataEvent),

    ContextCompacted(ContextCompactedEvent),

    ThreadRolledBack(ThreadRolledBackEvent),

    #[serde(rename = "task_started", alias = "turn_started")]
    TurnStarted(TurnStartedEvent),

    ThreadSettingsApplied(ThreadSettingsAppliedEvent),

    #[serde(rename = "task_complete", alias = "turn_complete")]
    TurnComplete(TurnCompleteEvent),

    TokenCount(TokenCountEvent),

    AgentMessage(AgentMessageEvent),

    UserMessage(UserMessageEvent),

    AgentReasoning(AgentReasoningEvent),

    AgentReasoningRawContent(AgentReasoningRawContentEvent),

    AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent),

    SessionConfigured(SessionConfiguredEvent),

    ThreadGoalUpdated(ThreadGoalUpdatedEvent),

    McpStartupUpdate(McpStartupUpdateEvent),

    McpStartupComplete(McpStartupCompleteEvent),

    McpToolCallBegin(McpToolCallBeginEvent),

    McpToolCallEnd(McpToolCallEndEvent),

    WebSearchBegin(WebSearchBeginEvent),

    WebSearchEnd(WebSearchEndEvent),

    ImageGenerationBegin(ImageGenerationBeginEvent),

    ImageGenerationEnd(ImageGenerationEndEvent),

    ExecCommandBegin(ExecCommandBeginEvent),

    ExecCommandOutputDelta(ExecCommandOutputDeltaEvent),

    TerminalInteraction(TerminalInteractionEvent),

    ExecCommandEnd(ExecCommandEndEvent),

    ViewImageToolCall(ViewImageToolCallEvent),

    ExecApprovalRequest(ExecApprovalRequestEvent),

    RequestPermissions(RequestPermissionsEvent),

    RequestUserInput(RequestUserInputEvent),

    DynamicToolCallRequest(DynamicToolCallRequest),

    DynamicToolCallResponse(DynamicToolCallResponseEvent),

    ElicitationRequest(ElicitationRequestEvent),

    ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent),

    GuardianAssessment(GuardianAssessmentEvent),

    DeprecationNotice(DeprecationNoticeEvent),

    StreamError(StreamErrorEvent),

    PatchApplyBegin(PatchApplyBeginEvent),

    PatchApplyUpdated(PatchApplyUpdatedEvent),

    PatchApplyEnd(PatchApplyEndEvent),

    TurnDiff(TurnDiffEvent),

    RealtimeConversationListVoicesResponse(RealtimeConversationListVoicesResponseEvent),

    PlanUpdate(UpdatePlanArgs),

    TurnAborted(TurnAbortedEvent),

    ShutdownComplete,

    EnteredReviewMode(ReviewRequest),

    ExitedReviewMode(ExitedReviewModeEvent),

    RawResponseItem(RawResponseItemEvent),

    ItemStarted(ItemStartedEvent),
    ItemCompleted(ItemCompletedEvent),
    HookStarted(HookStartedEvent),
    HookCompleted(HookCompletedEvent),

    AgentMessageContentDelta(AgentMessageContentDeltaEvent),
    PlanDelta(PlanDeltaEvent),
    ReasoningContentDelta(ReasoningContentDeltaEvent),
    ReasoningRawContentDelta(ReasoningRawContentDeltaEvent),

    CollabAgentSpawnBegin(CollabAgentSpawnBeginEvent),
    CollabAgentSpawnEnd(CollabAgentSpawnEndEvent),
    CollabAgentInteractionBegin(CollabAgentInteractionBeginEvent),
    CollabAgentInteractionEnd(CollabAgentInteractionEndEvent),
    CollabWaitingBegin(CollabWaitingBeginEvent),
    CollabWaitingEnd(CollabWaitingEndEvent),
    CollabCloseBegin(CollabCloseBeginEvent),
    CollabCloseEnd(CollabCloseEndEvent),
    CollabResumeBegin(CollabResumeBeginEvent),
    CollabResumeEnd(CollabResumeEndEvent),

    SubAgentActivity(SubAgentActivityEvent),
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, EnumIter)]
#[serde(rename_all = "snake_case")]
pub enum HookEventName {
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PreCompact,
    PostCompact,
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    SubagentStart,
    SubagentStop,
    Stop,
    Interrupt,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookHandlerType {
    Command,
    McpTool,
    Prompt,
    Agent,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookExecutionMode {
    Sync,
    Async,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookScope {
    Thread,
    Turn,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookSource {
    System,
    User,
    Project,
    Mdm,
    SessionFlags,
    Plugin,
    CloudRequirements,
    CloudManagedConfig,
    LegacyManagedConfigFile,
    LegacyManagedConfigMdm,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookTrustStatus {
    Managed,
    Untrusted,
    Trusted,
    Modified,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookRunStatus {
    Running,
    Completed,
    Failed,
    Blocked,
    Stopped,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum HookOutputEntryKind {
    Warning,
    Stop,
    Feedback,
    Context,
    Error,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct HookOutputEntry {
    pub kind: HookOutputEntryKind,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct HookRunSummary {
    pub id: String,
    pub event_name: HookEventName,
    pub handler_type: HookHandlerType,
    pub execution_mode: HookExecutionMode,
    pub scope: HookScope,
    pub source_path: AbsolutePathBuf,
    #[serde(default)]
    pub source: HookSource,
    pub display_order: i64,
    pub status: HookRunStatus,
    pub status_message: Option<String>,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub completed_at: Option<i64>,
    #[ts(type = "number | null")]
    pub duration_ms: Option<i64>,
    pub entries: Vec<HookOutputEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct HookStartedEvent {
    pub turn_id: Option<String>,
    pub run: HookRunSummary,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub struct HookCompletedEvent {
    pub turn_id: Option<String>,
    pub run: HookRunSummary,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeConversationVersion {
    V1,
    #[default]
    V2,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct RealtimeConversationStartedEvent {
    pub realtime_session_id: Option<String>,
    pub version: RealtimeConversationVersion,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct RealtimeConversationRealtimeEvent {
    pub payload: RealtimeEvent,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct RealtimeConversationClosedEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct RealtimeConversationSdpEvent {
    pub sdp: String,
}

impl From<CollabAgentSpawnBeginEvent> for EventMsg {
    fn from(event: CollabAgentSpawnBeginEvent) -> Self {
        EventMsg::CollabAgentSpawnBegin(event)
    }
}

impl From<CollabAgentSpawnEndEvent> for EventMsg {
    fn from(event: CollabAgentSpawnEndEvent) -> Self {
        EventMsg::CollabAgentSpawnEnd(event)
    }
}

impl From<CollabAgentInteractionBeginEvent> for EventMsg {
    fn from(event: CollabAgentInteractionBeginEvent) -> Self {
        EventMsg::CollabAgentInteractionBegin(event)
    }
}

impl From<CollabAgentInteractionEndEvent> for EventMsg {
    fn from(event: CollabAgentInteractionEndEvent) -> Self {
        EventMsg::CollabAgentInteractionEnd(event)
    }
}

impl From<CollabWaitingBeginEvent> for EventMsg {
    fn from(event: CollabWaitingBeginEvent) -> Self {
        EventMsg::CollabWaitingBegin(event)
    }
}

impl From<CollabWaitingEndEvent> for EventMsg {
    fn from(event: CollabWaitingEndEvent) -> Self {
        EventMsg::CollabWaitingEnd(event)
    }
}

impl From<CollabCloseBeginEvent> for EventMsg {
    fn from(event: CollabCloseBeginEvent) -> Self {
        EventMsg::CollabCloseBegin(event)
    }
}

impl From<CollabCloseEndEvent> for EventMsg {
    fn from(event: CollabCloseEndEvent) -> Self {
        EventMsg::CollabCloseEnd(event)
    }
}

impl From<CollabResumeBeginEvent> for EventMsg {
    fn from(event: CollabResumeBeginEvent) -> Self {
        EventMsg::CollabResumeBegin(event)
    }
}

impl From<CollabResumeEndEvent> for EventMsg {
    fn from(event: CollabResumeEndEvent) -> Self {
        EventMsg::CollabResumeEnd(event)
    }
}

impl From<SubAgentActivityEvent> for EventMsg {
    fn from(event: SubAgentActivityEvent) -> Self {
        EventMsg::SubAgentActivity(event)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum AgentStatus {
    #[default]
    PendingInit,
    Running,
    Interrupted,
    Completed(Option<String>),
    Errored(String),
    Shutdown,
    NotFound,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum NonSteerableTurnKind {
    Review,
    Compact,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum CodexErrorInfo {
    ContextWindowExceeded,
    UsageLimitExceeded,
    ServerOverloaded,
    CyberPolicy,
    HttpConnectionFailed { http_status_code: Option<u16> },
    ResponseStreamConnectionFailed { http_status_code: Option<u16> },
    InternalServerError,
    Unauthorized,
    BadRequest,
    SandboxError,
    ResponseStreamDisconnected { http_status_code: Option<u16> },
    ResponseTooManyFailedAttempts { http_status_code: Option<u16> },
    ActiveTurnNotSteerable { turn_kind: NonSteerableTurnKind },
    ThreadRollbackFailed,
    Other,
}

impl CodexErrorInfo {
    pub fn affects_turn_status(&self) -> bool {
        match self {
            Self::ThreadRollbackFailed | Self::ActiveTurnNotSteerable { .. } => false,
            Self::ContextWindowExceeded
            | Self::UsageLimitExceeded
            | Self::ServerOverloaded
            | Self::CyberPolicy
            | Self::HttpConnectionFailed { .. }
            | Self::ResponseStreamConnectionFailed { .. }
            | Self::InternalServerError
            | Self::Unauthorized
            | Self::BadRequest
            | Self::SandboxError
            | Self::ResponseStreamDisconnected { .. }
            | Self::ResponseTooManyFailedAttempts { .. }
            | Self::Other => true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct RawResponseItemEvent {
    pub item: ResponseItem,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ItemStartedEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item: TurnItem,
    pub started_at_ms: i64,
}

impl HasLegacyEvent for ItemStartedEvent {
    fn as_legacy_events(&self, _: bool) -> Vec<EventMsg> {
        match &self.item {
            TurnItem::WebSearch(item) => vec![EventMsg::WebSearchBegin(WebSearchBeginEvent {
                call_id: item.id.clone(),
            })],
            TurnItem::ImageView(_) => Vec::new(),
            TurnItem::ImageGeneration(item) => {
                vec![EventMsg::ImageGenerationBegin(ImageGenerationBeginEvent {
                    call_id: item.id.clone(),
                })]
            }
            TurnItem::FileChange(item) => vec![item.as_legacy_begin_event(self.turn_id.clone())],
            TurnItem::McpToolCall(item) => vec![item.as_legacy_begin_event()],
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ItemCompletedEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item: TurnItem,
    #[serde(default = "default_item_completed_at_ms")]
    pub completed_at_ms: i64,
}

const fn default_item_completed_at_ms() -> i64 {
    0
}

pub trait HasLegacyEvent {
    fn as_legacy_events(&self, show_raw_agent_reasoning: bool) -> Vec<EventMsg>;
}

impl HasLegacyEvent for ItemCompletedEvent {
    fn as_legacy_events(&self, show_raw_agent_reasoning: bool) -> Vec<EventMsg> {
        match &self.item {
            TurnItem::FileChange(item) => item
                .as_legacy_end_event(self.turn_id.clone())
                .into_iter()
                .collect(),
            _ => self.item.as_legacy_events(show_raw_agent_reasoning),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct AgentMessageContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

impl HasLegacyEvent for AgentMessageContentDeltaEvent {
    fn as_legacy_events(&self, _: bool) -> Vec<EventMsg> {
        Vec::new()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct PlanDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ReasoningContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    #[serde(default)]
    pub summary_index: i64,
}

impl HasLegacyEvent for ReasoningContentDeltaEvent {
    fn as_legacy_events(&self, _: bool) -> Vec<EventMsg> {
        Vec::new()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, TS, JsonSchema)]
pub struct ReasoningRawContentDeltaEvent {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    #[serde(default)]
    pub content_index: i64,
}

impl HasLegacyEvent for ReasoningRawContentDeltaEvent {
    fn as_legacy_events(&self, _: bool) -> Vec<EventMsg> {
        Vec::new()
    }
}

impl HasLegacyEvent for EventMsg {
    fn as_legacy_events(&self, show_raw_agent_reasoning: bool) -> Vec<EventMsg> {
        match self {
            EventMsg::ItemStarted(event) => event.as_legacy_events(show_raw_agent_reasoning),
            EventMsg::ItemCompleted(event) => event.as_legacy_events(show_raw_agent_reasoning),
            EventMsg::AgentMessageContentDelta(event) => {
                event.as_legacy_events(show_raw_agent_reasoning)
            }
            EventMsg::ReasoningContentDelta(event) => {
                event.as_legacy_events(show_raw_agent_reasoning)
            }
            EventMsg::ReasoningRawContentDelta(event) => {
                event.as_legacy_events(show_raw_agent_reasoning)
            }
            _ => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExitedReviewModeEvent {
    pub review_output: Option<ReviewOutputEvent>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ErrorEvent {
    pub message: String,
    #[serde(default)]
    pub codex_error_info: Option<CodexErrorInfo>,
}

impl ErrorEvent {
    pub fn affects_turn_status(&self) -> bool {
        self.codex_error_info
            .as_ref()
            .is_none_or(CodexErrorInfo::affects_turn_status)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WarningEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelRerouteReason {
    HighRiskCyberActivity,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ModelRerouteEvent {
    pub from_model: String,
    pub to_model: String,
    pub reason: ModelRerouteReason,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum ModelVerification {
    TrustedAccessForCyber,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ModelVerificationEvent {
    pub verifications: Vec<ModelVerification>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct TurnModerationMetadataEvent {
    pub metadata: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ContextCompactedEvent;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnCompleteEvent {
    pub turn_id: String,
    pub last_agent_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub completed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub time_to_first_token_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnStartedEvent {
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub started_at: Option<i64>,
    pub model_context_window: Option<i64>,
    #[serde(default)]
    pub collaboration_mode_kind: ModeKind,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ThreadSettingsAppliedEvent {
    pub thread_settings: ThreadSettingsSnapshot,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ThreadSettingsSnapshot {
    pub model: String,
    pub model_provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub approval_policy: AskForApproval,
    pub approvals_reviewer: ApprovalsReviewer,
    pub permission_profile: PermissionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_permission_profile: Option<ActivePermissionProfile>,
    pub cwd: AbsolutePathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<ReasoningSummaryConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    pub collaboration_mode: CollaborationMode,
    #[serde(default)]
    pub multi_agent_mode: Option<MultiAgentMode>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq, JsonSchema, TS)]
pub struct TokenUsage {
    #[ts(type = "number")]
    pub input_tokens: i64,
    #[ts(type = "number")]
    pub cached_input_tokens: i64,
    #[ts(type = "number")]
    pub output_tokens: i64,
    #[ts(type = "number")]
    pub reasoning_output_tokens: i64,
    #[ts(type = "number")]
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct TokenUsageInfo {
    pub total_token_usage: TokenUsage,
    pub last_token_usage: TokenUsage,
    #[ts(type = "number | null")]
    pub model_context_window: Option<i64>,
}

impl TokenUsageInfo {
    pub fn new_or_append(
        info: &Option<TokenUsageInfo>,
        last: &Option<TokenUsage>,
        model_context_window: Option<i64>,
    ) -> Option<Self> {
        if info.is_none() && last.is_none() {
            return None;
        }

        let mut info = match info {
            Some(info) => info.clone(),
            None => Self {
                total_token_usage: TokenUsage::default(),
                last_token_usage: TokenUsage::default(),
                model_context_window,
            },
        };
        if let Some(last) = last {
            info.append_last_usage(last);
        }
        if let Some(model_context_window) = model_context_window {
            info.model_context_window = Some(model_context_window);
        }
        Some(info)
    }

    pub fn append_last_usage(&mut self, last: &TokenUsage) {
        self.total_token_usage.add_assign(last);
        self.last_token_usage = last.clone();
    }

    pub fn fill_to_context_window(&mut self, context_window: i64) {
        let previous_total = self.total_token_usage.total_tokens;
        let delta = (context_window - previous_total).max(0);

        self.model_context_window = Some(context_window);
        self.total_token_usage = TokenUsage {
            total_tokens: context_window,
            ..TokenUsage::default()
        };
        self.last_token_usage = TokenUsage {
            total_tokens: delta,
            ..TokenUsage::default()
        };
    }

    pub fn full_context_window(context_window: i64) -> Self {
        let mut info = Self {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage::default(),
            model_context_window: Some(context_window),
        };
        info.fill_to_context_window(context_window);
        info
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TokenCountEvent {
    pub info: Option<TokenUsageInfo>,
    pub rate_limits: Option<RateLimitSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct RateLimitSnapshot {
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits: Option<CreditsSnapshot>,
    pub individual_limit: Option<SpendControlLimitSnapshot>,
    pub plan_type: Option<crate::account::PlanType>,
    pub rate_limit_reached_type: Option<RateLimitReachedType>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum RateLimitReachedType {
    RateLimitReached,
    WorkspaceOwnerCreditsDepleted,
    WorkspaceMemberCreditsDepleted,
    WorkspaceOwnerUsageLimitReached,
    WorkspaceMemberUsageLimitReached,
}

impl FromStr for RateLimitReachedType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "rate_limit_reached" => Ok(Self::RateLimitReached),
            "workspace_owner_credits_depleted" => Ok(Self::WorkspaceOwnerCreditsDepleted),
            "workspace_member_credits_depleted" => Ok(Self::WorkspaceMemberCreditsDepleted),
            "workspace_owner_usage_limit_reached" => Ok(Self::WorkspaceOwnerUsageLimitReached),
            "workspace_member_usage_limit_reached" => Ok(Self::WorkspaceMemberUsageLimitReached),
            other => Err(format!("unknown rate limit reached type: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct RateLimitWindow {
    pub used_percent: f64,
    #[ts(type = "number | null")]
    pub window_minutes: Option<i64>,
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct CreditsSnapshot {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema, TS)]
pub struct SpendControlLimitSnapshot {
    pub limit: String,
    pub used: String,
    pub remaining_percent: i32,
    pub resets_at: i64,
}

const BASELINE_TOKENS: i64 = 12000;

impl TokenUsage {
    pub fn is_zero(&self) -> bool {
        self.total_tokens == 0
    }

    pub fn cached_input(&self) -> i64 {
        self.cached_input_tokens.max(0)
    }

    pub fn non_cached_input(&self) -> i64 {
        (self.input_tokens - self.cached_input()).max(0)
    }

    pub fn blended_total(&self) -> i64 {
        (self.non_cached_input() + self.output_tokens.max(0)).max(0)
    }

    pub fn tokens_in_context_window(&self) -> i64 {
        self.total_tokens
    }

    pub fn percent_of_context_window_remaining(&self, context_window: i64) -> i64 {
        if context_window <= BASELINE_TOKENS {
            return 0;
        }

        let effective_window = context_window - BASELINE_TOKENS;
        let used = (self.tokens_in_context_window() - BASELINE_TOKENS).max(0);
        let remaining = (effective_window - used).max(0);
        ((remaining as f64 / effective_window as f64) * 100.0)
            .clamp(0.0, 100.0)
            .round() as i64
    }

    pub fn add_assign(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FinalOutput {
    pub token_usage: TokenUsage,
}

impl From<TokenUsage> for FinalOutput {
    fn from(token_usage: TokenUsage) -> Self {
        Self { token_usage }
    }
}

impl fmt::Display for FinalOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let token_usage = &self.token_usage;

        write!(
            f,
            "Token usage: total={} input={}{} output={}{}",
            format_with_separators(token_usage.blended_total()),
            format_with_separators(token_usage.non_cached_input()),
            if token_usage.cached_input() > 0 {
                format!(
                    " (+ {} cached)",
                    format_with_separators(token_usage.cached_input())
                )
            } else {
                String::new()
            },
            format_with_separators(token_usage.output_tokens),
            if token_usage.reasoning_output_tokens > 0 {
                format!(
                    " (reasoning {})",
                    format_with_separators(token_usage.reasoning_output_tokens)
                )
            } else {
                String::new()
            }
        )
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentMessageEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub delivery: Option<crate::models::AgentMessageDelivery>,
    pub message: String,
    #[serde(default)]
    pub phase: Option<MessagePhase>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, TS)]
pub struct UserMessageEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_details: Vec<Option<ImageDetail>>,
    #[serde(default)]
    pub local_images: Vec<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_image_details: Vec<Option<ImageDetail>>,
    #[serde(default)]
    pub text_elements: Vec<crate::user_input::TextElement>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningRawContentEvent {
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct AgentReasoningSectionBreakEvent {
    #[serde(default)]
    pub item_id: String,
    #[serde(default)]
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpInvocation {
    pub server: String,
    pub tool: String,
    pub arguments: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpToolCallBeginEvent {
    pub call_id: String,
    pub invocation: McpInvocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub connector_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_app_resource_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub link_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct McpToolCallEndEvent {
    pub call_id: String,
    pub invocation: McpInvocation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub connector_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mcp_app_resource_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub link_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin_id: Option<String>,
    #[ts(type = "string")]
    pub duration: Duration,
    pub result: Result<CallToolResult, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq)]
pub struct DynamicToolCallResponseEvent {
    pub call_id: String,
    pub turn_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    #[serde(default)]
    pub namespace: Option<String>,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
    pub success: bool,
    pub error: Option<String>,
    #[ts(type = "string")]
    pub duration: Duration,
}

impl McpToolCallEndEvent {
    pub fn is_success(&self) -> bool {
        match &self.result {
            Ok(result) => !result.is_error.unwrap_or(false),
            Err(_) => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WebSearchBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct WebSearchEndEvent {
    pub call_id: String,
    pub query: String,
    pub action: WebSearchAction,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ImageGenerationBeginEvent {
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ImageGenerationEndEvent {
    pub call_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub revised_prompt: Option<String>,
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub saved_path: Option<AbsolutePathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ConversationPathResponseEvent {
    pub conversation_id: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ResumedHistory {
    pub conversation_id: String,
    pub history: Vec<RolloutItem>,
    pub rollout_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub enum InitialHistory {
    New,
    Cleared,
    Resumed(ResumedHistory),
    Forked(Vec<RolloutItem>),
}

impl InitialHistory {
    pub fn scan_rollout_items(&self, mut predicate: impl FnMut(&RolloutItem) -> bool) -> bool {
        match self {
            InitialHistory::New | InitialHistory::Cleared => false,
            InitialHistory::Resumed(resumed) => resumed.history.iter().any(&mut predicate),
            InitialHistory::Forked(items) => items.iter().any(predicate),
        }
    }

    pub fn forked_from_id(&self) -> Option<String> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => meta_line.meta.forked_from_id.clone(),
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => Some(meta_line.meta.id.clone()),
                _ => None,
            }),
        }
    }

    pub fn session_cwd(&self) -> Option<PathBuf> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => session_cwd_from_items(&resumed.history),
            InitialHistory::Forked(items) => session_cwd_from_items(items),
        }
    }

    pub fn get_rollout_items(&self) -> Vec<RolloutItem> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => Vec::new(),
            InitialHistory::Resumed(resumed) => resumed.history.clone(),
            InitialHistory::Forked(items) => items.clone(),
        }
    }

    pub fn get_event_msgs(&self) -> Option<Vec<EventMsg>> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => Some(
                resumed
                    .history
                    .iter()
                    .filter_map(|ri| match ri {
                        RolloutItem::EventMsg(ev) => Some(ev.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            InitialHistory::Forked(items) => Some(
                items
                    .iter()
                    .filter_map(|ri| match ri {
                        RolloutItem::EventMsg(ev) => Some(ev.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
        }
    }

    pub fn get_base_instructions(&self) -> Option<BaseInstructions> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => meta_line.meta.base_instructions.clone(),
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => meta_line.meta.base_instructions.clone(),
                _ => None,
            }),
        }
    }

    pub fn get_dynamic_tools(&self) -> Option<Vec<DynamicToolSpec>> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => meta_line.meta.dynamic_tools.clone(),
                    _ => None,
                })
            }
            InitialHistory::Forked(items) => items.iter().find_map(|item| match item {
                RolloutItem::SessionMeta(meta_line) => meta_line.meta.dynamic_tools.clone(),
                _ => None,
            }),
        }
    }

    pub fn get_multi_agent_version(&self) -> Option<MultiAgentVersion> {
        match self {
            InitialHistory::New | InitialHistory::Cleared => None,
            InitialHistory::Resumed(resumed) => multi_agent_version_from_items(
                &resumed.history,
                Some(resumed.conversation_id.clone()),
            ),
            InitialHistory::Forked(items) => multi_agent_version_from_items(items, None),
        }
    }

    pub fn get_latest_effective_multi_agent_mode(&self) -> Option<MultiAgentMode> {
        let items = match self {
            InitialHistory::New | InitialHistory::Cleared => return None,
            InitialHistory::Resumed(resumed) => &resumed.history,
            InitialHistory::Forked(items) => items,
        };
        items
            .iter()
            .rev()
            .find_map(|item| match item {
                RolloutItem::TurnContext(turn_context) => Some(turn_context),
                RolloutItem::SessionMeta(_)
                | RolloutItem::ResponseItem(_)
                | RolloutItem::InterAgentCommunication(_)
                | RolloutItem::Compacted(_)
                | RolloutItem::EventMsg(_) => None,
            })
            .and_then(|turn_context| turn_context.multi_agent_mode)
    }

    pub fn get_resumed_session_sources(&self) -> Option<(SessionSource, Option<ThreadSource>)> {
        let meta = self.get_resumed_session_meta()?;
        Some((meta.source.clone(), meta.thread_source.clone()))
    }

    pub fn get_resumed_thread_source(&self) -> Option<ThreadSource> {
        self.get_resumed_session_meta()
            .and_then(|meta| meta.thread_source.clone())
    }

    pub fn get_resumed_parent_thread_id(&self) -> Option<String> {
        self.get_resumed_session_meta()
            .and_then(|meta| meta.parent_thread_id.clone())
    }

    fn get_resumed_session_meta(&self) -> Option<&SessionMeta> {
        match self {
            InitialHistory::New | InitialHistory::Cleared | InitialHistory::Forked(_) => None,
            InitialHistory::Resumed(resumed) => {
                resumed.history.iter().find_map(|item| match item {
                    RolloutItem::SessionMeta(meta_line) => Some(&meta_line.meta),
                    _ => None,
                })
            }
        }
    }
}

fn session_cwd_from_items(items: &[RolloutItem]) -> Option<PathBuf> {
    items.iter().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => Some(meta_line.meta.cwd.clone()),
        _ => None,
    })
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS, Default)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum SessionSource {
    Cli,
    #[default]
    VSCode,
    Exec,
    Mcp,
    Custom(String),
    Internal(InternalSessionSource),
    SubAgent(SubAgentSource),
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
#[ts(type = "string")]
pub enum ThreadSource {
    User,
    Subagent,
    Feature(String),
    MemoryConsolidation,
}

impl ThreadSource {
    pub fn as_str(&self) -> &str {
        match self {
            ThreadSource::User => "user",
            ThreadSource::Subagent => "subagent",
            ThreadSource::Feature(feature) => feature,
            ThreadSource::MemoryConsolidation => "memory_consolidation",
        }
    }
}

impl fmt::Display for ThreadSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<String> for ThreadSource {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ThreadSource> for String {
    fn from(value: ThreadSource) -> Self {
        value.to_string()
    }
}

impl FromStr for ThreadSource {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "user" => Ok(ThreadSource::User),
            "subagent" => Ok(ThreadSource::Subagent),
            "memory_consolidation" => Ok(ThreadSource::MemoryConsolidation),
            other => Ok(ThreadSource::Feature(other.to_string())),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum InternalSessionSource {
    MemoryConsolidation,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SubAgentSource {
    Review,
    Compact,
    ThreadSpawn {
        parent_thread_id: String,
        depth: i32,
        #[serde(default)]
        agent_path: Option<AgentPath>,
        #[serde(default)]
        agent_nickname: Option<String>,
        #[serde(default, alias = "agent_type")]
        agent_role: Option<String>,
    },
    MemoryConsolidation,
    Other(String),
}

impl fmt::Display for SessionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionSource::Cli => f.write_str("cli"),
            SessionSource::VSCode => f.write_str("vscode"),
            SessionSource::Exec => f.write_str("exec"),
            SessionSource::Mcp => f.write_str("mcp"),
            SessionSource::Custom(source) => f.write_str(source),
            SessionSource::Internal(source) => write!(f, "internal_{source}"),
            SessionSource::SubAgent(sub_source) => write!(f, "subagent_{sub_source}"),
            SessionSource::Unknown => f.write_str("unknown"),
        }
    }
}

impl SessionSource {
    pub fn from_startup_arg(value: &str) -> Result<Self, &'static str> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("session source must not be empty");
        }

        let normalized = trimmed.to_ascii_lowercase();
        Ok(match normalized.as_str() {
            "cli" => SessionSource::Cli,
            "vscode" => SessionSource::VSCode,
            "exec" => SessionSource::Exec,
            "mcp" | "appserver" | "app-server" | "app_server" => SessionSource::Mcp,
            "unknown" => SessionSource::Unknown,
            _ => SessionSource::Custom(normalized),
        })
    }

    pub fn is_internal(&self) -> bool {
        matches!(self, SessionSource::Internal(_))
    }

    pub fn is_non_root_agent(&self) -> bool {
        matches!(
            self,
            SessionSource::Internal(_) | SessionSource::SubAgent(_)
        )
    }

    pub fn get_nickname(&self) -> Option<String> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_nickname, .. }) => {
                agent_nickname.clone()
            }
            _ => None,
        }
    }

    pub fn get_agent_role(&self) -> Option<String> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_role, .. }) => {
                agent_role.clone()
            }
            _ => None,
        }
    }

    pub fn get_agent_path(&self) -> Option<AgentPath> {
        match self {
            SessionSource::SubAgent(SubAgentSource::ThreadSpawn { agent_path, .. }) => {
                agent_path.clone()
            }
            _ => None,
        }
    }

    pub fn restriction_product(&self) -> Option<Product> {
        match self {
            SessionSource::Custom(source) => Product::from_session_source_name(source),
            SessionSource::Cli
            | SessionSource::VSCode
            | SessionSource::Exec
            | SessionSource::Mcp
            | SessionSource::Unknown => Some(Product::Codex),
            SessionSource::Internal(_) | SessionSource::SubAgent(_) => None,
        }
    }

    pub fn matches_product_restriction(&self, products: &[Product]) -> bool {
        products.is_empty()
            || self
                .restriction_product()
                .is_some_and(|product| product.matches_product_restriction(products))
    }

    pub fn parent_thread_id(&self) -> Option<String> {
        match self {
            SessionSource::SubAgent(subagent_source) => subagent_source.parent_thread_id(),
            SessionSource::Cli
            | SessionSource::VSCode
            | SessionSource::Exec
            | SessionSource::Mcp
            | SessionSource::Custom(_)
            | SessionSource::Internal(_)
            | SessionSource::Unknown => None,
        }
    }
}

impl fmt::Display for SubAgentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubAgentSource::Review => f.write_str("review"),
            SubAgentSource::Compact => f.write_str("compact"),
            SubAgentSource::MemoryConsolidation => f.write_str("memory_consolidation"),
            SubAgentSource::ThreadSpawn {
                parent_thread_id,
                depth,
                ..
            } => {
                write!(f, "thread_spawn_{parent_thread_id}_d{depth}")
            }
            SubAgentSource::Other(other) => f.write_str(other),
        }
    }
}

impl SubAgentSource {
    pub fn kind(&self) -> &str {
        match self {
            SubAgentSource::Review => "review",
            SubAgentSource::Compact => "compact",
            SubAgentSource::ThreadSpawn { .. } => "thread_spawn",
            SubAgentSource::MemoryConsolidation => "memory_consolidation",
            SubAgentSource::Other(other) => other,
        }
    }

    pub fn parent_thread_id(&self) -> Option<String> {
        match self {
            SubAgentSource::ThreadSpawn {
                parent_thread_id, ..
            } => Some(parent_thread_id.clone()),
            SubAgentSource::Review
            | SubAgentSource::Compact
            | SubAgentSource::MemoryConsolidation
            | SubAgentSource::Other(_) => None,
        }
    }
}

impl fmt::Display for InternalSessionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InternalSessionSource::MemoryConsolidation => f.write_str("memory_consolidation"),
        }
    }
}

fn multi_agent_version_from_items(
    items: &[RolloutItem],
    thread_id: Option<String>,
) -> Option<MultiAgentVersion> {
    let session_meta_version = items.iter().rev().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line)
            if thread_id
                .as_ref()
                .is_none_or(|thread_id| &meta_line.meta.id == thread_id) =>
        {
            meta_line.meta.multi_agent_version
        }
        _ => None,
    });

    session_meta_version.or_else(|| {
        items.iter().rev().find_map(|item| match item {
            RolloutItem::TurnContext(turn_context) => turn_context.multi_agent_version,
            RolloutItem::SessionMeta(_)
            | RolloutItem::ResponseItem(_)
            | RolloutItem::InterAgentCommunication(_)
            | RolloutItem::Compacted(_)
            | RolloutItem::EventMsg(_) => None,
        })
    })
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum MultiAgentVersion {
    Disabled,
    V1,
    V2,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct SessionMeta {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    pub timestamp: String,
    pub cwd: PathBuf,
    pub originator: String,
    pub cli_version: String,
    #[serde(default)]
    pub source: SessionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<ThreadSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(default, alias = "agent_type", skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_path: Option<String>,
    pub model_provider: Option<String>,
    pub base_instructions: Option<BaseInstructions>,
    #[serde(
        default,
        deserialize_with = "crate::dynamic_tools::deserialize_dynamic_tool_specs",
        skip_serializing_if = "Option::is_none"
    )]
    pub dynamic_tools: Option<Vec<DynamicToolSpec>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multi_agent_version: Option<MultiAgentVersion>,
}

impl Default for SessionMeta {
    fn default() -> Self {
        SessionMeta {
            id: uuid::Uuid::now_v7().to_string(),
            forked_from_id: None,
            parent_thread_id: None,
            timestamp: String::new(),
            cwd: PathBuf::new(),
            originator: String::new(),
            cli_version: String::new(),
            source: SessionSource::default(),
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            agent_path: None,
            model_provider: None,
            base_instructions: None,
            dynamic_tools: None,
            memory_mode: None,
            multi_agent_version: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, TS)]
pub struct SessionMetaLine {
    #[serde(flatten)]
    pub meta: SessionMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema, TS)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RolloutItem {
    SessionMeta(SessionMetaLine),
    ResponseItem(ResponseItem),
    InterAgentCommunication(InterAgentCommunication),
    Compacted(CompactedItem),
    TurnContext(TurnContextItem),
    EventMsg(EventMsg),
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
pub struct CompactedItem {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_history: Option<Vec<ResponseItem>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
}

impl From<CompactedItem> for ResponseItem {
    fn from(value: CompactedItem) -> Self {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: value.message,
            }],
            phase: None,
            metadata: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema, TS)]
pub struct TurnContextNetworkItem {
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
pub struct TurnContextItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub cwd: AbsolutePathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_roots: Option<Vec<AbsolutePathBuf>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    pub approval_policy: AskForApproval,
    pub sandbox_policy: SandboxPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_profile: Option<PermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<TurnContextNetworkItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_system_sandbox_policy: Option<FileSystemSandboxPolicy>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comp_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub personality: Option<Personality>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<CollaborationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_agent_version: Option<MultiAgentVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_agent_mode: Option<MultiAgentMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realtime_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffortConfig>,
    pub summary: ReasoningSummaryConfig,
}

impl TurnContextItem {
    pub fn permission_profile(&self) -> PermissionProfile {
        self.permission_profile.clone().unwrap_or_else(|| {
            let file_system_sandbox_policy =
                self.file_system_sandbox_policy.clone().unwrap_or_else(|| {
                    FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
                        &self.sandbox_policy,
                        self.cwd.as_path(),
                    )
                });
            PermissionProfile::from_runtime_permissions_with_enforcement(
                SandboxEnforcement::from_legacy_sandbox_policy(&self.sandbox_policy),
                &file_system_sandbox_policy,
                NetworkSandboxPolicy::from(&self.sandbox_policy),
            )
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "mode", content = "limit", rename_all = "snake_case")]
pub enum TruncationPolicy {
    Bytes(usize),
    Tokens(usize),
}

impl From<crate::openai_models::TruncationPolicyConfig> for TruncationPolicy {
    fn from(config: crate::openai_models::TruncationPolicyConfig) -> Self {
        match config.mode {
            crate::openai_models::TruncationMode::Bytes => Self::Bytes(config.limit as usize),
            crate::openai_models::TruncationMode::Tokens => Self::Tokens(config.limit as usize),
        }
    }
}

impl TruncationPolicy {
    pub fn token_budget(&self) -> usize {
        match self {
            TruncationPolicy::Bytes(bytes) => {
                usize::try_from(octopus_util_string::approx_tokens_from_byte_count(*bytes))
                    .unwrap_or(usize::MAX)
            }
            TruncationPolicy::Tokens(tokens) => *tokens,
        }
    }

    pub fn byte_budget(&self) -> usize {
        match self {
            TruncationPolicy::Bytes(bytes) => *bytes,
            TruncationPolicy::Tokens(tokens) => {
                octopus_util_string::approx_bytes_for_tokens(*tokens)
            }
        }
    }
}

impl Mul<f64> for TruncationPolicy {
    type Output = Self;

    fn mul(self, multiplier: f64) -> Self::Output {
        match self {
            TruncationPolicy::Bytes(bytes) => {
                TruncationPolicy::Bytes((bytes as f64 * multiplier).ceil() as usize)
            }
            TruncationPolicy::Tokens(tokens) => {
                TruncationPolicy::Tokens((tokens as f64 * multiplier).ceil() as usize)
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, JsonSchema)]
pub struct RolloutLine {
    pub timestamp: String,
    #[serde(flatten)]
    pub item: RolloutItem,
}

#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema, TS)]
pub struct GitInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<GitSha>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDelivery {
    Inline,
    Detached,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
pub enum ReviewTarget {
    UncommittedChanges,

    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    BaseBranch {
        branch: String,
    },

    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Commit {
        sha: String,
        title: Option<String>,
    },

    #[serde(rename_all = "camelCase")]
    #[ts(rename_all = "camelCase")]
    Custom {
        instructions: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewRequest {
    pub target: ReviewTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub user_facing_hint: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewOutputEvent {
    pub findings: Vec<ReviewFinding>,
    pub overall_correctness: String,
    pub overall_explanation: String,
    pub overall_confidence_score: f32,
}

impl Default for ReviewOutputEvent {
    fn default() -> Self {
        Self {
            findings: Vec::new(),
            overall_correctness: String::default(),
            overall_explanation: String::default(),
            overall_confidence_score: 0.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewFinding {
    pub title: String,
    pub body: String,
    pub confidence_score: f32,
    pub priority: i32,
    pub code_location: ReviewCodeLocation,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewCodeLocation {
    pub absolute_file_path: PathBuf,
    pub line_range: ReviewLineRange,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ReviewLineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(
    Debug, Clone, Copy, Display, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandSource {
    #[default]
    Agent,
    UserShell,
    UnifiedExecStartup,
    UnifiedExecInteraction,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandBeginEvent {
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    pub turn_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    pub command: Vec<String>,
    pub cwd: PathUri,
    pub parsed_cmd: Vec<ParsedCommand>,
    #[serde(default)]
    pub source: ExecCommandSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ExecCommandEndEvent {
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub process_id: Option<String>,
    pub turn_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    pub command: Vec<String>,
    pub cwd: PathUri,
    pub parsed_cmd: Vec<ParsedCommand>,
    #[serde(default)]
    pub source: ExecCommandSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interaction_input: Option<String>,

    pub stdout: String,
    pub stderr: String,
    #[serde(default)]
    pub aggregated_output: String,
    pub exit_code: i32,
    #[ts(type = "string")]
    pub duration: Duration,
    pub formatted_output: String,
    pub status: ExecCommandStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ViewImageToolCallEvent {
    pub call_id: String,
    pub path: AbsolutePathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecOutputStream {
    Stdout,
    Stderr,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct ExecCommandOutputDeltaEvent {
    pub call_id: String,
    pub stream: ExecOutputStream,
    #[serde_as(as = "serde_with::base64::Base64")]
    #[schemars(with = "String")]
    #[ts(type = "string")]
    pub chunk: Vec<u8>,
}

#[serde_as]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct TerminalInteractionEvent {
    pub call_id: String,
    pub process_id: String,
    pub stdin: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct DeprecationNoticeEvent {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ThreadRolledBackEvent {
    pub num_turns: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct StreamErrorEvent {
    pub message: String,
    #[serde(default)]
    pub codex_error_info: Option<CodexErrorInfo>,
    #[serde(default)]
    pub additional_details: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct StreamInfoEvent {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyBeginEvent {
    pub call_id: String,
    #[serde(default)]
    pub turn_id: String,
    pub auto_approved: bool,
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyUpdatedEvent {
    pub call_id: String,
    pub changes: HashMap<PathBuf, FileChange>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct PatchApplyEndEvent {
    pub call_id: String,
    #[serde(default)]
    pub turn_id: String,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    #[serde(default)]
    pub changes: HashMap<PathBuf, FileChange>,
    pub status: PatchApplyStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum PatchApplyStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnDiffEvent {
    pub unified_diff: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct McpStartupUpdateEvent {
    pub server: String,
    pub status: McpStartupStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case", tag = "state")]
#[ts(rename_all = "snake_case", tag = "state")]
pub enum McpStartupStatus {
    Starting,
    Ready,
    Failed { error: String },
    Cancelled,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, Default)]
pub struct McpStartupCompleteEvent {
    pub ready: Vec<String>,
    pub failed: Vec<McpStartupFailure>,
    pub cancelled: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct McpStartupFailure {
    pub server: String,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum McpAuthStatus {
    Unsupported,
    NotLoggedIn,
    BearerToken,
    OAuth,
}

impl fmt::Display for McpAuthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            McpAuthStatus::Unsupported => "Unsupported",
            McpAuthStatus::NotLoggedIn => "Not logged in",
            McpAuthStatus::BearerToken => "Bearer token",
            McpAuthStatus::OAuth => "OAuth",
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct RealtimeConversationListVoicesResponseEvent {
    pub voices: RealtimeVoicesList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum Product {
    #[serde(alias = "CHATGPT")]
    Chatgpt,
    #[serde(alias = "CODEX")]
    Codex,
    #[serde(alias = "ATLAS")]
    Atlas,
}
impl Product {
    pub fn to_app_platform(self) -> &'static str {
        match self {
            Self::Chatgpt => "chat",
            Self::Codex => "codex",
            Self::Atlas => "atlas",
        }
    }

    pub fn from_session_source_name(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "chatgpt" => Some(Self::Chatgpt),
            "codex" => Some(Self::Codex),
            "atlas" => Some(Self::Atlas),
            _ => None,
        }
    }

    pub fn matches_product_restriction(&self, products: &[Product]) -> bool {
        products.is_empty() || products.contains(self)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SkillScope {
    User,
    Repo,
    System,
    Admin,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub short_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interface: Option<SkillInterface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dependencies: Option<SkillDependencies>,
    pub path: AbsolutePathBuf,
    pub scope: SkillScope,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillInterface {
    #[ts(optional)]
    pub display_name: Option<String>,
    #[ts(optional)]
    pub short_description: Option<String>,
    #[ts(optional)]
    pub icon_small: Option<AbsolutePathBuf>,
    #[ts(optional)]
    pub icon_large: Option<AbsolutePathBuf>,
    #[ts(optional)]
    pub brand_color: Option<String>,
    #[ts(optional)]
    pub default_prompt: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillDependencies {
    pub tools: Vec<SkillToolDependency>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillToolDependency {
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub r#type: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SessionNetworkProxyRuntime {
    pub http_addr: String,
    pub socks_addr: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema, TS)]
pub struct SessionConfiguredEvent {
    pub session_id: String,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forked_from_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_source: Option<ThreadSource>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thread_name: Option<String>,

    pub model: String,

    pub model_provider_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    pub approval_policy: AskForApproval,

    #[serde(default)]
    pub approvals_reviewer: ApprovalsReviewer,

    pub permission_profile: PermissionProfile,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active_permission_profile: Option<ActivePermissionProfile>,

    pub cwd: AbsolutePathBuf,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortConfig>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_messages: Option<Vec<EventMsg>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub network_proxy: Option<SessionNetworkProxyRuntime>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rollout_path: Option<PathBuf>,
}

impl<'de> Deserialize<'de> for SessionConfiguredEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            session_id: String,
            #[serde(default)]
            thread_id: Option<String>,
            forked_from_id: Option<String>,
            parent_thread_id: Option<String>,
            #[serde(default)]
            thread_source: Option<ThreadSource>,
            #[serde(default)]
            thread_name: Option<String>,
            model: String,
            model_provider_id: String,
            service_tier: Option<String>,
            approval_policy: AskForApproval,
            #[serde(default)]
            approvals_reviewer: ApprovalsReviewer,
            sandbox_policy: Option<SandboxPolicy>,
            permission_profile: Option<PermissionProfile>,
            #[serde(default)]
            active_permission_profile: Option<ActivePermissionProfile>,
            cwd: AbsolutePathBuf,
            reasoning_effort: Option<ReasoningEffortConfig>,
            initial_messages: Option<Vec<EventMsg>>,
            network_proxy: Option<SessionNetworkProxyRuntime>,
            rollout_path: Option<PathBuf>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let permission_profile = match (wire.permission_profile, wire.sandbox_policy) {
            (Some(permission_profile), _) => permission_profile,
            (None, Some(sandbox_policy)) => PermissionProfile::from_legacy_sandbox_policy_for_cwd(
                &sandbox_policy,
                wire.cwd.as_path(),
            ),
            (None, None) => {
                return Err(serde::de::Error::missing_field("permission_profile"));
            }
        };

        let thread_id = match wire.thread_id {
            Some(thread_id) => thread_id,
            None => wire.session_id.clone(),
        };
        Ok(Self {
            session_id: wire.session_id,
            thread_id,
            forked_from_id: wire.forked_from_id,
            parent_thread_id: wire.parent_thread_id,
            thread_source: wire.thread_source,
            thread_name: wire.thread_name,
            model: wire.model,
            model_provider_id: wire.model_provider_id,
            service_tier: wire.service_tier,
            approval_policy: wire.approval_policy,
            approvals_reviewer: wire.approvals_reviewer,
            permission_profile,
            active_permission_profile: wire.active_permission_profile,
            cwd: wire.cwd,
            reasoning_effort: wire.reasoning_effort,
            initial_messages: wire.initial_messages,
            network_proxy: wire.network_proxy,
            rollout_path: wire.rollout_path,
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub enum ThreadGoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

pub const MAX_THREAD_GOAL_OBJECTIVE_CHARS: usize = 4_000;

pub fn validate_thread_goal_objective(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("goal objective must not be empty".to_string());
    }
    if value.chars().count() > MAX_THREAD_GOAL_OBJECTIVE_CHARS {
        return Err(format!(
            "goal objective must be at most {MAX_THREAD_GOAL_OBJECTIVE_CHARS} characters"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub struct ThreadGoal {
    pub thread_id: String,
    pub objective: String,
    pub status: ThreadGoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "protocol/")]
pub struct ThreadGoalUpdatedEvent {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    pub goal: ThreadGoal,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq, Display, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,

    ApprovedExecpolicyAmendment {
        proposed_execpolicy_amendment: ExecPolicyAmendment,
    },

    ApprovedForSession,

    NetworkPolicyAmendment {
        network_policy_amendment: NetworkPolicyAmendment,
    },

    #[default]
    Denied,

    TimedOut,

    Abort,
}

impl ReviewDecision {
    pub fn to_opaque_string(&self) -> &'static str {
        match self {
            ReviewDecision::Approved => "approved",
            ReviewDecision::ApprovedExecpolicyAmendment { .. } => "approved_with_amendment",
            ReviewDecision::ApprovedForSession => "approved_for_session",
            ReviewDecision::NetworkPolicyAmendment {
                network_policy_amendment,
            } => match network_policy_amendment.action {
                NetworkPolicyRuleAction::Allow => "approved_with_network_policy_allow",
                NetworkPolicyRuleAction::Deny => "denied_with_network_policy_deny",
            },
            ReviewDecision::Denied => "denied",
            ReviewDecision::TimedOut => "timed_out",
            ReviewDecision::Abort => "abort",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum FileChange {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct Chunk {
    pub orig_index: u32,
    pub deleted_lines: Vec<String>,
    pub inserted_lines: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct TurnAbortedEvent {
    pub turn_id: Option<String>,
    pub reason: TurnAbortReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub completed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "number | null", optional)]
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnAbortReason {
    Interrupted,
    Replaced,
    ReviewEnded,
    BudgetLimited,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentSpawnBeginEvent {
    pub call_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    pub sender_thread_id: String,
    pub prompt: String,
    pub model: String,
    pub reasoning_effort: ReasoningEffortConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct CollabAgentRef {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(default, alias = "agent_type", skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct CollabAgentStatusEntry {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_nickname: Option<String>,
    #[serde(default, alias = "agent_type", skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentSpawnEndEvent {
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    pub sender_thread_id: String,
    pub new_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_agent_nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_agent_role: Option<String>,
    pub prompt: String,
    pub model: String,
    pub reasoning_effort: ReasoningEffortConfig,
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentInteractionBeginEvent {
    pub call_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    pub sender_thread_id: String,
    pub receiver_thread_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabAgentInteractionEndEvent {
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    pub sender_thread_id: String,
    pub receiver_thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
    pub prompt: String,
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SubAgentActivityKind {
    Started,
    Interacted,
    Interrupted,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
pub struct SubAgentActivityEvent {
    pub event_id: String,
    #[serde(default)]
    pub occurred_at_ms: i64,
    pub agent_thread_id: String,
    pub agent_path: AgentPath,
    pub kind: SubAgentActivityKind,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabWaitingBeginEvent {
    #[serde(default)]
    pub started_at_ms: i64,
    pub sender_thread_id: String,
    pub receiver_thread_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receiver_agents: Vec<CollabAgentRef>,
    pub call_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabWaitingEndEvent {
    pub sender_thread_id: String,
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_statuses: Vec<CollabAgentStatusEntry>,
    pub statuses: HashMap<String, AgentStatus>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabCloseBeginEvent {
    pub call_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    pub sender_thread_id: String,
    pub receiver_thread_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabCloseEndEvent {
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    pub sender_thread_id: String,
    pub receiver_thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
    pub status: AgentStatus,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabResumeBeginEvent {
    pub call_id: String,
    #[serde(default)]
    pub started_at_ms: i64,
    pub sender_thread_id: String,
    pub receiver_thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, JsonSchema, TS)]
pub struct CollabResumeEndEvent {
    pub call_id: String,
    #[serde(default)]
    pub completed_at_ms: i64,
    pub sender_thread_id: String,
    pub receiver_thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_agent_role: Option<String>,
    pub status: AgentStatus,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::FileChangeItem;
    use crate::items::ImageGenerationItem;
    use crate::items::McpToolCallItem;
    use crate::items::McpToolCallStatus;
    use crate::items::UserMessageItem;
    use crate::items::WebSearchItem;
    use crate::mcp::CallToolResult;
    use crate::permissions::FileSystemAccessMode;
    use crate::permissions::FileSystemPath;
    use crate::permissions::FileSystemSandboxEntry;
    use crate::permissions::FileSystemSandboxPolicy;
    use crate::permissions::FileSystemSpecialPath;
    use crate::permissions::NetworkSandboxPolicy;
    use anyhow::Result;
    use octopus_util_abspath::AbsolutePathBuf;
    use octopus_util_abspath::test_support::PathBufExt;
    use octopus_util_abspath::test_support::test_path_buf;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::path::Path;
    use std::path::PathBuf;
    use tempfile::NamedTempFile;
    use tempfile::TempDir;

    #[test]
    fn feature_thread_source_serializes_as_its_app_owned_label() -> Result<()> {
        let source = ThreadSource::Feature("automation".to_string());

        assert_eq!(serde_json::to_value(&source)?, json!("automation"));
        assert_eq!(
            serde_json::from_value::<ThreadSource>(json!("automation"))?,
            source
        );
        Ok(())
    }

    #[test]
    fn session_meta_normalizes_legacy_dynamic_tools() -> Result<()> {
        let mut value = serde_json::to_value(SessionMeta::default())?;
        value["dynamic_tools"] = json!([
            {
                "namespace": "legacy_app",
                "name": "lookup_ticket",
                "description": "Look up a ticket",
                "inputSchema": {"type": "object", "properties": {}},
                "exposeToContext": false
            },
            {
                "namespace": "legacy_app",
                "name": "update_ticket",
                "description": "Update a ticket",
                "inputSchema": {"type": "object", "properties": {}},
                "deferLoading": false,
                "exposeToContext": false
            }
        ]);

        let meta: SessionMeta = serde_json::from_value(value)?;

        assert_eq!(
            meta.dynamic_tools,
            Some(vec![DynamicToolSpec::Namespace(
                crate::dynamic_tools::DynamicToolNamespaceSpec {
                    name: "legacy_app".to_string(),
                    description: String::new(),
                    tools: vec![
                        crate::dynamic_tools::DynamicToolNamespaceTool::Function(
                            crate::dynamic_tools::DynamicToolFunctionSpec {
                                name: "lookup_ticket".to_string(),
                                description: "Look up a ticket".to_string(),
                                input_schema: json!({"type": "object", "properties": {}}),
                                defer_loading: true,
                            },
                        ),
                        crate::dynamic_tools::DynamicToolNamespaceTool::Function(
                            crate::dynamic_tools::DynamicToolFunctionSpec {
                                name: "update_ticket".to_string(),
                                description: "Update a ticket".to_string(),
                                input_schema: json!({"type": "object", "properties": {}}),
                                defer_loading: false,
                            },
                        ),
                    ],
                },
            )])
        );
        Ok(())
    }

    fn sorted_writable_roots(roots: Vec<WritableRoot>) -> Vec<(PathBuf, Vec<PathBuf>)> {
        let mut sorted_roots: Vec<(PathBuf, Vec<PathBuf>)> = roots
            .into_iter()
            .map(|root| {
                let mut read_only_subpaths: Vec<PathBuf> = root
                    .read_only_subpaths
                    .into_iter()
                    .map(|path| path.to_path_buf())
                    .collect();
                read_only_subpaths.sort();
                (root.root.to_path_buf(), read_only_subpaths)
            })
            .collect();
        sorted_roots.sort_by(|left, right| left.0.cmp(&right.0));
        sorted_roots
    }

    fn sandbox_policy_allows_read(policy: &SandboxPolicy, _path: &Path, _cwd: &Path) -> bool {
        policy.has_full_disk_read_access()
    }

    fn sandbox_policy_allows_write(policy: &SandboxPolicy, path: &Path, cwd: &Path) -> bool {
        if policy.has_full_disk_write_access() {
            return true;
        }

        policy
            .get_writable_roots_with_cwd(cwd)
            .iter()
            .any(|root| root.is_path_writable(path))
    }

    #[test]
    fn session_source_from_startup_arg_maps_known_values() {
        assert_eq!(
            SessionSource::from_startup_arg("vscode").unwrap(),
            SessionSource::VSCode
        );
        assert_eq!(
            SessionSource::from_startup_arg("app-server").unwrap(),
            SessionSource::Mcp
        );
    }

    #[test]
    fn inter_agent_communication_response_input_item_preserves_commentary_phase() {
        let communication = InterAgentCommunication {
            author: AgentPath::root(),
            recipient: AgentPath::root().join("reviewer").expect("recipient path"),
            other_recipients: vec![AgentPath::root().join("worker").expect("recipient path")],
            content: "review the diff".to_string(),
            encrypted_content: None,
            metadata: None,
            trigger_turn: true,
        };

        assert_eq!(
            communication.to_response_input_item(),
            ResponseInputItem::Message {
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: serde_json::to_string(&communication).expect("serialize communication"),
                }],
                phase: Some(MessagePhase::Commentary),
            }
        );
    }

    #[test]
    fn queued_encrypted_inter_agent_communication_renders_message_envelope() {
        let communication = InterAgentCommunication::new_encrypted(
            AgentPath::root().join("worker").expect("author path"),
            AgentPath::root(),
            Vec::new(),
            "encrypted payload".to_string(),
            false,
        );

        assert_eq!(
            communication.to_model_input_item(),
            ResponseItem::AgentMessage {
                id: None,
                author: "/root/worker".to_string(),
                recipient: "/root".to_string(),
                content: vec![
                    AgentMessageInputContent::InputText {
                        text: "Message Type: MESSAGE\nTask name: /root\nSender: /root/worker\nPayload:\n"
                            .to_string(),
                    },
                    AgentMessageInputContent::EncryptedContent {
                        encrypted_content: "encrypted payload".to_string(),
                    },
                ],
                metadata: None,
            }
        );
    }

    #[test]
    fn session_source_from_startup_arg_normalizes_custom_values() {
        assert_eq!(
            SessionSource::from_startup_arg("atlas").unwrap(),
            SessionSource::Custom("atlas".to_string())
        );
        assert_eq!(
            SessionSource::from_startup_arg(" Atlas ").unwrap(),
            SessionSource::Custom("atlas".to_string())
        );
    }

    #[test]
    fn session_source_restriction_product_defaults_non_subagent_sources_to_codex() {
        assert_eq!(
            SessionSource::Cli.restriction_product(),
            Some(Product::Codex)
        );
        assert_eq!(
            SessionSource::VSCode.restriction_product(),
            Some(Product::Codex)
        );
        assert_eq!(
            SessionSource::Exec.restriction_product(),
            Some(Product::Codex)
        );
        assert_eq!(
            SessionSource::Mcp.restriction_product(),
            Some(Product::Codex)
        );
        assert_eq!(
            SessionSource::Unknown.restriction_product(),
            Some(Product::Codex)
        );
    }

    #[test]
    fn session_source_restriction_product_does_not_guess_subagent_products() {
        assert_eq!(
            SessionSource::SubAgent(SubAgentSource::Review).restriction_product(),
            None
        );
        assert_eq!(
            SessionSource::Internal(InternalSessionSource::MemoryConsolidation)
                .restriction_product(),
            None
        );
    }

    #[test]
    fn session_source_restriction_product_maps_custom_sources_to_products() {
        assert_eq!(
            SessionSource::Custom("chatgpt".to_string()).restriction_product(),
            Some(Product::Chatgpt)
        );
        assert_eq!(
            SessionSource::Custom("ATLAS".to_string()).restriction_product(),
            Some(Product::Atlas)
        );
        assert_eq!(
            SessionSource::Custom("codex".to_string()).restriction_product(),
            Some(Product::Codex)
        );
        assert_eq!(
            SessionSource::Custom("atlas-dev".to_string()).restriction_product(),
            None
        );
    }

    #[test]
    fn session_source_matches_product_restriction() {
        assert!(
            SessionSource::Custom("chatgpt".to_string())
                .matches_product_restriction(&[Product::Chatgpt])
        );
        assert!(
            !SessionSource::Custom("chatgpt".to_string())
                .matches_product_restriction(&[Product::Codex])
        );
        assert!(SessionSource::VSCode.matches_product_restriction(&[Product::Codex]));
        assert!(
            !SessionSource::Custom("atlas-dev".to_string())
                .matches_product_restriction(&[Product::Atlas])
        );
        assert!(SessionSource::Custom("atlas-dev".to_string()).matches_product_restriction(&[]));
    }

    fn sandbox_policy_probe_paths(policy: &SandboxPolicy, cwd: &Path) -> Vec<PathBuf> {
        let mut paths = vec![cwd.to_path_buf()];
        for root in policy.get_writable_roots_with_cwd(cwd) {
            paths.push(root.root.to_path_buf());
            paths.extend(
                root.read_only_subpaths
                    .into_iter()
                    .map(|path| path.to_path_buf()),
            );
        }
        paths.sort();
        paths.dedup();
        paths
    }

    fn assert_same_sandbox_policy_semantics(
        expected: &SandboxPolicy,
        actual: &SandboxPolicy,
        cwd: &Path,
    ) {
        assert_eq!(
            actual.has_full_disk_read_access(),
            expected.has_full_disk_read_access()
        );
        assert_eq!(
            actual.has_full_disk_write_access(),
            expected.has_full_disk_write_access()
        );
        assert_eq!(
            actual.has_full_network_access(),
            expected.has_full_network_access()
        );
        let mut probe_paths = sandbox_policy_probe_paths(expected, cwd);
        probe_paths.extend(sandbox_policy_probe_paths(actual, cwd));
        probe_paths.sort();
        probe_paths.dedup();

        for path in probe_paths {
            assert_eq!(
                sandbox_policy_allows_read(actual, &path, cwd),
                sandbox_policy_allows_read(expected, &path, cwd),
                "read access mismatch for {}",
                path.display()
            );
            assert_eq!(
                sandbox_policy_allows_write(actual, &path, cwd),
                sandbox_policy_allows_write(expected, &path, cwd),
                "write access mismatch for {}",
                path.display()
            );
        }
    }

    #[test]
    fn external_sandbox_reports_full_access_flags() {
        let restricted = SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Restricted,
        };
        assert!(restricted.has_full_disk_write_access());
        assert!(!restricted.has_full_network_access());

        let enabled = SandboxPolicy::ExternalSandbox {
            network_access: NetworkAccess::Enabled,
        };
        assert!(enabled.has_full_disk_write_access());
        assert!(enabled.has_full_network_access());
    }

    #[test]
    fn read_only_reports_network_access_flags() {
        let restricted = SandboxPolicy::new_read_only_policy();
        assert!(!restricted.has_full_network_access());

        let enabled = SandboxPolicy::ReadOnly {
            network_access: true,
        };
        assert!(enabled.has_full_network_access());
    }

    #[test]
    fn granular_approval_config_mcp_elicitation_flag_is_field_driven() {
        assert!(
            GranularApprovalConfig {
                sandbox_approval: false,
                rules: false,
                skill_approval: false,
                request_permissions: false,
                mcp_elicitations: true,
            }
            .allows_mcp_elicitations()
        );
        assert!(
            !GranularApprovalConfig {
                sandbox_approval: false,
                rules: false,
                skill_approval: false,
                request_permissions: false,
                mcp_elicitations: false,
            }
            .allows_mcp_elicitations()
        );
    }

    #[test]
    fn granular_approval_config_skill_approval_flag_is_field_driven() {
        assert!(
            GranularApprovalConfig {
                sandbox_approval: false,
                rules: false,
                skill_approval: true,
                request_permissions: false,
                mcp_elicitations: false,
            }
            .allows_skill_approval()
        );
        assert!(
            !GranularApprovalConfig {
                sandbox_approval: false,
                rules: false,
                skill_approval: false,
                request_permissions: false,
                mcp_elicitations: false,
            }
            .allows_skill_approval()
        );
    }

    #[test]
    fn granular_approval_config_request_permissions_flag_is_field_driven() {
        assert!(
            GranularApprovalConfig {
                sandbox_approval: false,
                rules: false,
                skill_approval: false,
                request_permissions: true,
                mcp_elicitations: false,
            }
            .allows_request_permissions()
        );
        assert!(
            !GranularApprovalConfig {
                sandbox_approval: false,
                rules: false,
                skill_approval: false,
                request_permissions: false,
                mcp_elicitations: false,
            }
            .allows_request_permissions()
        );
    }

    #[test]
    fn granular_approval_config_defaults_missing_optional_flags_to_false() {
        let decoded = serde_json::from_value::<GranularApprovalConfig>(serde_json::json!({
            "sandbox_approval": true,
            "rules": false,
            "mcp_elicitations": true,
        }))
        .expect("granular approval config should deserialize");

        assert_eq!(
            decoded,
            GranularApprovalConfig {
                sandbox_approval: true,
                rules: false,
                skill_approval: false,
                request_permissions: false,
                mcp_elicitations: true,
            }
        );
    }

    #[test]
    fn restricted_file_system_policy_reports_full_access_from_root_entries() {
        let read_only = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
        }]);
        assert!(read_only.has_full_disk_read_access());
        assert!(!read_only.has_full_disk_write_access());
        assert!(!read_only.include_platform_defaults());

        let writable = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
        }]);
        assert!(writable.has_full_disk_read_access());
        assert!(writable.has_full_disk_write_access());
    }

    #[test]
    fn restricted_file_system_policy_treats_root_with_carveouts_as_scoped_access() {
        let cwd = TempDir::new().expect("tempdir");
        let canonical_cwd = octopus_util_abspath::canonicalize_preserving_symlinks(cwd.path())
            .expect("canonicalize cwd");
        let root = AbsolutePathBuf::from_absolute_path(&canonical_cwd)
            .expect("absolute canonical tempdir")
            .as_path()
            .ancestors()
            .last()
            .and_then(|path| AbsolutePathBuf::from_absolute_path(path).ok())
            .expect("filesystem root");
        let blocked = AbsolutePathBuf::resolve_path_against_base("blocked", cwd.path());
        let expected_blocked = AbsolutePathBuf::from_absolute_path(
            octopus_util_abspath::canonicalize_preserving_symlinks(cwd.path())
                .expect("canonicalize cwd")
                .join("blocked"),
        )
        .expect("canonical blocked");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Write,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: blocked },
                access: FileSystemAccessMode::Deny,
            },
        ]);

        assert!(!policy.has_full_disk_read_access());
        assert!(!policy.has_full_disk_write_access());
        assert_eq!(
            policy.get_readable_roots_with_cwd(cwd.path()),
            vec![root.clone()]
        );
        assert_eq!(
            policy.get_unreadable_roots_with_cwd(cwd.path()),
            vec![expected_blocked.clone()]
        );

        let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
        assert_eq!(writable_roots.len(), 1);
        assert_eq!(writable_roots[0].root, root);
        assert!(
            writable_roots[0]
                .read_only_subpaths
                .iter()
                .any(|path| path.as_path() == expected_blocked.as_path())
        );
    }

    #[test]
    fn restricted_file_system_policy_derives_effective_paths() {
        let cwd = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(cwd.path().join(".agents")).expect("create .agents");
        std::fs::create_dir_all(cwd.path().join(".codex")).expect("create .codex");
        let canonical_cwd = octopus_util_abspath::canonicalize_preserving_symlinks(cwd.path())
            .expect("canonicalize cwd");
        let cwd_absolute =
            AbsolutePathBuf::from_absolute_path(&canonical_cwd).expect("absolute tempdir");
        let secret = AbsolutePathBuf::resolve_path_against_base("secret", cwd.path());
        let expected_secret = AbsolutePathBuf::from_absolute_path(canonical_cwd.join("secret"))
            .expect("canonical secret");
        let expected_agents = AbsolutePathBuf::from_absolute_path(canonical_cwd.join(".agents"))
            .expect("canonical .agents");
        let expected_codex = AbsolutePathBuf::from_absolute_path(canonical_cwd.join(".codex"))
            .expect("canonical .codex");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Minimal,
                },
                access: FileSystemAccessMode::Read,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(None),
                },
                access: FileSystemAccessMode::Write,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: secret },
                access: FileSystemAccessMode::Deny,
            },
        ]);

        assert!(!policy.has_full_disk_read_access());
        assert!(!policy.has_full_disk_write_access());
        assert!(policy.include_platform_defaults());
        assert_eq!(
            policy.get_readable_roots_with_cwd(cwd.path()),
            vec![cwd_absolute.clone()]
        );
        assert_eq!(
            policy.get_unreadable_roots_with_cwd(cwd.path()),
            vec![expected_secret.clone()]
        );

        let writable_roots = policy.get_writable_roots_with_cwd(cwd.path());
        assert_eq!(writable_roots.len(), 1);
        assert_eq!(writable_roots[0].root, cwd_absolute);
        assert!(
            writable_roots[0]
                .read_only_subpaths
                .iter()
                .any(|path| path.as_path() == expected_secret.as_path())
        );
        assert!(
            writable_roots[0]
                .read_only_subpaths
                .iter()
                .any(|path| path.as_path() == expected_agents.as_path())
        );
        assert!(
            writable_roots[0]
                .read_only_subpaths
                .iter()
                .any(|path| path.as_path() == expected_codex.as_path())
        );
    }

    #[test]
    fn restricted_file_system_policy_treats_read_entries_as_read_only_subpaths() {
        let cwd = TempDir::new().expect("tempdir");
        let canonical_cwd = octopus_util_abspath::canonicalize_preserving_symlinks(cwd.path())
            .expect("canonicalize cwd");
        let docs = AbsolutePathBuf::resolve_path_against_base("docs", cwd.path());
        let docs_public = AbsolutePathBuf::resolve_path_against_base("docs/public", cwd.path());
        let expected_docs = AbsolutePathBuf::from_absolute_path(canonical_cwd.join("docs"))
            .expect("canonical docs");
        let expected_docs_public =
            AbsolutePathBuf::from_absolute_path(canonical_cwd.join("docs/public"))
                .expect("canonical docs/public");
        let expected_dot_codex = AbsolutePathBuf::from_absolute_path(canonical_cwd.join(".codex"))
            .expect("canonical .codex");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(None),
                },
                access: FileSystemAccessMode::Write,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: docs },
                access: FileSystemAccessMode::Read,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: docs_public },
                access: FileSystemAccessMode::Write,
            },
        ]);

        assert!(!policy.has_full_disk_write_access());
        assert_eq!(
            sorted_writable_roots(policy.get_writable_roots_with_cwd(cwd.path())),
            vec![
                (
                    canonical_cwd,
                    vec![
                        expected_dot_codex.to_path_buf(),
                        expected_docs.to_path_buf()
                    ],
                ),
                (expected_docs_public.to_path_buf(), Vec::new()),
            ]
        );
    }

    #[test]
    fn file_system_policy_rejects_legacy_bridge_for_non_workspace_writes() {
        let cwd = if cfg!(windows) {
            Path::new(r"C:\workspace")
        } else {
            Path::new("/tmp/workspace")
        };
        let external_write_path = if cfg!(windows) {
            AbsolutePathBuf::from_absolute_path(r"C:\temp").expect("absolute windows temp path")
        } else {
            AbsolutePathBuf::from_absolute_path("/tmp").expect("absolute tmp path")
        };
        let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: external_write_path,
            },
            access: FileSystemAccessMode::Write,
        }]);

        let err = policy
            .to_legacy_sandbox_policy(NetworkSandboxPolicy::Restricted, cwd)
            .expect_err("non-workspace writes should be rejected");

        assert!(
            err.to_string()
                .contains("filesystem writes outside the workspace root"),
            "{err}"
        );
    }

    #[test]
    fn legacy_sandbox_policy_semantics_survive_split_bridge() {
        let cwd = TempDir::new().expect("tempdir");
        let writable_root = AbsolutePathBuf::resolve_path_against_base("writable", cwd.path());
        let policies = [
            SandboxPolicy::DangerFullAccess,
            SandboxPolicy::ExternalSandbox {
                network_access: NetworkAccess::Restricted,
            },
            SandboxPolicy::ExternalSandbox {
                network_access: NetworkAccess::Enabled,
            },
            SandboxPolicy::ReadOnly {
                network_access: false,
            },
            SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![],
                network_access: false,
                exclude_tmpdir_env_var: true,
                exclude_slash_tmp: true,
            },
            SandboxPolicy::WorkspaceWrite {
                writable_roots: vec![writable_root],
                network_access: true,
                exclude_tmpdir_env_var: false,
                exclude_slash_tmp: true,
            },
        ];

        for expected in policies {
            let actual =
                FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(&expected, cwd.path())
                    .to_legacy_sandbox_policy(NetworkSandboxPolicy::from(&expected), cwd.path())
                    .expect("legacy bridge should preserve legacy policy semantics");

            assert_same_sandbox_policy_semantics(&expected, &actual, cwd.path());
        }
    }

    #[test]
    fn item_started_event_from_web_search_emits_begin_event() {
        let event = ItemStartedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            item: TurnItem::WebSearch(WebSearchItem {
                id: "search-1".into(),
                query: "find docs".into(),
                action: WebSearchAction::Search {
                    query: Some("find docs".into()),
                    queries: None,
                },
            }),
            started_at_ms: 0,
        };

        let legacy_events = event.as_legacy_events(false);
        assert_eq!(legacy_events.len(), 1);
        match &legacy_events[0] {
            EventMsg::WebSearchBegin(event) => assert_eq!(event.call_id, "search-1"),
            _ => panic!("expected WebSearchBegin event"),
        }
    }

    #[test]
    fn item_started_event_from_non_web_search_emits_no_legacy_events() {
        let event = ItemStartedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            item: TurnItem::UserMessage(UserMessageItem::new(&[])),
            started_at_ms: 0,
        };

        assert!(event.as_legacy_events(false).is_empty());
    }

    #[test]
    fn item_started_event_from_image_generation_emits_begin_event() {
        let event = ItemStartedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            item: TurnItem::ImageGeneration(ImageGenerationItem {
                id: "ig-1".into(),
                status: "in_progress".into(),
                revised_prompt: None,
                result: String::new(),
                saved_path: None,
            }),
            started_at_ms: 0,
        };

        let legacy_events = event.as_legacy_events(false);
        assert_eq!(legacy_events.len(), 1);
        match &legacy_events[0] {
            EventMsg::ImageGenerationBegin(event) => assert_eq!(event.call_id, "ig-1"),
            _ => panic!("expected ImageGenerationBegin event"),
        }
    }

    #[test]
    fn item_started_event_from_file_change_emits_patch_begin_event() {
        let event = ItemStartedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            started_at_ms: 0,
            item: TurnItem::FileChange(FileChangeItem {
                id: "patch-1".into(),
                changes: [(
                    PathBuf::from("new.txt"),
                    FileChange::Add {
                        content: "hello".into(),
                    },
                )]
                .into_iter()
                .collect(),
                status: None,
                auto_approved: Some(true),
                stdout: None,
                stderr: None,
            }),
        };

        let legacy_events = event.as_legacy_events(false);
        assert_eq!(legacy_events.len(), 1);
        match &legacy_events[0] {
            EventMsg::PatchApplyBegin(event) => {
                assert_eq!(event.call_id, "patch-1");
                assert_eq!(event.turn_id, "turn-1");
                assert!(event.auto_approved);
                assert!(event.changes.contains_key(&PathBuf::from("new.txt")));
            }
            _ => panic!("expected PatchApplyBegin event"),
        }
    }

    #[test]
    fn item_started_event_from_mcp_tool_call_emits_begin_event() {
        let event = ItemStartedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            started_at_ms: 0,
            item: TurnItem::McpToolCall(McpToolCallItem {
                id: "mcp-1".into(),
                server: "server".into(),
                tool: "tool".into(),
                arguments: json!({"arg": "value"}),
                connector_id: Some("connector".into()),
                mcp_app_resource_uri: Some("app://connector".into()),
                link_id: Some("link_123".into()),
                plugin_id: Some("sample@test".into()),
                status: McpToolCallStatus::InProgress,
                result: None,
                error: None,
                duration: None,
            }),
        };

        let legacy_events = event.as_legacy_events(false);
        assert_eq!(legacy_events.len(), 1);
        match &legacy_events[0] {
            EventMsg::McpToolCallBegin(event) => {
                assert_eq!(event.call_id, "mcp-1");
                assert_eq!(event.invocation.server, "server");
                assert_eq!(event.invocation.tool, "tool");
                assert_eq!(event.connector_id.as_deref(), Some("connector"));
                assert_eq!(
                    event.mcp_app_resource_uri.as_deref(),
                    Some("app://connector")
                );
                assert_eq!(event.link_id.as_deref(), Some("link_123"));
                assert_eq!(event.plugin_id.as_deref(), Some("sample@test"));
            }
            _ => panic!("expected McpToolCallBegin event"),
        }
    }

    #[test]
    fn item_completed_event_from_image_generation_emits_end_event() {
        let event = ItemCompletedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            item: TurnItem::ImageGeneration(ImageGenerationItem {
                id: "ig-1".into(),
                status: "completed".into(),
                revised_prompt: Some("A tiny blue square".into()),
                result: "Zm9v".into(),
                saved_path: Some(test_path_buf("/tmp/ig-1.png").abs()),
            }),
            completed_at_ms: 0,
        };

        let legacy_events = event.as_legacy_events(false);
        assert_eq!(legacy_events.len(), 1);
        match &legacy_events[0] {
            EventMsg::ImageGenerationEnd(event) => {
                assert_eq!(event.call_id, "ig-1");
                assert_eq!(event.status, "completed");
                assert_eq!(event.revised_prompt.as_deref(), Some("A tiny blue square"));
                assert_eq!(event.result, "Zm9v");
                assert_eq!(
                    event.saved_path.as_ref().map(AbsolutePathBuf::as_path),
                    Some(test_path_buf("/tmp/ig-1.png").as_path())
                );
            }
            _ => panic!("expected ImageGenerationEnd event"),
        }
    }

    #[test]
    fn item_completed_event_from_file_change_emits_patch_end_event() {
        let event = ItemCompletedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            completed_at_ms: 0,
            item: TurnItem::FileChange(FileChangeItem {
                id: "patch-1".into(),
                changes: [(
                    PathBuf::from("new.txt"),
                    FileChange::Add {
                        content: "hello".into(),
                    },
                )]
                .into_iter()
                .collect(),
                status: Some(PatchApplyStatus::Completed),
                auto_approved: None,
                stdout: Some("Done!".into()),
                stderr: Some(String::new()),
            }),
        };

        let legacy_events = event.as_legacy_events(false);
        assert_eq!(legacy_events.len(), 1);
        match &legacy_events[0] {
            EventMsg::PatchApplyEnd(event) => {
                assert_eq!(event.call_id, "patch-1");
                assert_eq!(event.turn_id, "turn-1");
                assert_eq!(event.stdout, "Done!");
                assert!(event.success);
                assert_eq!(event.status, PatchApplyStatus::Completed);
                assert!(event.changes.contains_key(&PathBuf::from("new.txt")));
            }
            _ => panic!("expected PatchApplyEnd event"),
        }
    }

    #[test]
    fn item_completed_event_from_mcp_tool_call_emits_end_event() {
        let event = ItemCompletedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            completed_at_ms: 0,
            item: TurnItem::McpToolCall(McpToolCallItem {
                id: "mcp-1".into(),
                server: "server".into(),
                tool: "tool".into(),
                arguments: json!({"arg": "value"}),
                connector_id: Some("connector".into()),
                mcp_app_resource_uri: Some("app://connector".into()),
                link_id: Some("link_123".into()),
                plugin_id: Some("sample@test".into()),
                status: McpToolCallStatus::Completed,
                result: Some(CallToolResult {
                    content: vec![json!({"type": "text", "text": "ok"})],
                    structured_content: None,
                    is_error: Some(false),
                    meta: None,
                }),
                error: None,
                duration: Some(Duration::from_millis(42)),
            }),
        };

        let legacy_events = event.as_legacy_events(false);
        assert_eq!(legacy_events.len(), 1);
        match &legacy_events[0] {
            EventMsg::McpToolCallEnd(event) => {
                assert_eq!(event.call_id, "mcp-1");
                assert_eq!(event.invocation.server, "server");
                assert_eq!(event.invocation.tool, "tool");
                assert_eq!(event.connector_id.as_deref(), Some("connector"));
                assert_eq!(
                    event.mcp_app_resource_uri.as_deref(),
                    Some("app://connector")
                );
                assert_eq!(event.link_id.as_deref(), Some("link_123"));
                assert_eq!(event.plugin_id.as_deref(), Some("sample@test"));
                assert_eq!(event.duration, Duration::from_millis(42));
                assert!(event.is_success());
            }
            _ => panic!("expected McpToolCallEnd event"),
        }
    }

    #[test]
    fn item_started_event_requires_started_at_ms() {
        let mut value = serde_json::to_value(ItemStartedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            item: TurnItem::UserMessage(UserMessageItem::new(&[])),
            started_at_ms: 123,
        })
        .unwrap();
        value.as_object_mut().unwrap().remove("started_at_ms");

        assert!(serde_json::from_value::<ItemStartedEvent>(value).is_err());
    }

    #[test]
    fn item_completed_event_defaults_missing_completed_at_ms() {
        let mut value = serde_json::to_value(ItemCompletedEvent {
            thread_id: uuid::Uuid::now_v7().to_string(),
            turn_id: "turn-1".into(),
            item: TurnItem::UserMessage(UserMessageItem::new(&[])),
            completed_at_ms: 123,
        })
        .unwrap();
        value.as_object_mut().unwrap().remove("completed_at_ms");

        let event = serde_json::from_value::<ItemCompletedEvent>(value).unwrap();
        assert_eq!(event.completed_at_ms, 0);
    }
    #[test]
    fn rollback_failed_error_does_not_affect_turn_status() {
        let event = ErrorEvent {
            message: "rollback failed".into(),
            codex_error_info: Some(CodexErrorInfo::ThreadRollbackFailed),
        };
        assert!(!event.affects_turn_status());
    }

    #[test]
    fn active_turn_not_steerable_error_does_not_affect_turn_status() {
        let event = ErrorEvent {
            message: "cannot steer a review turn".into(),
            codex_error_info: Some(CodexErrorInfo::ActiveTurnNotSteerable {
                turn_kind: NonSteerableTurnKind::Review,
            }),
        };
        assert!(!event.affects_turn_status());
    }

    #[test]
    fn generic_error_affects_turn_status() {
        let event = ErrorEvent {
            message: "generic".into(),
            codex_error_info: Some(CodexErrorInfo::Other),
        };
        assert!(event.affects_turn_status());
    }

    #[test]
    fn realtime_conversation_started_event_uses_realtime_session_id() {
        let event = RealtimeConversationStartedEvent {
            realtime_session_id: Some("conv_1".to_string()),
            version: RealtimeConversationVersion::V2,
        };

        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({
                "realtime_session_id": "conv_1",
                "version": "v2"
            })
        );
    }

    #[test]
    fn realtime_voice_list_is_stable() {
        assert_eq!(
            RealtimeVoicesList::builtin(),
            RealtimeVoicesList {
                v1: vec![
                    RealtimeVoice::Juniper,
                    RealtimeVoice::Maple,
                    RealtimeVoice::Spruce,
                    RealtimeVoice::Ember,
                    RealtimeVoice::Vale,
                    RealtimeVoice::Breeze,
                    RealtimeVoice::Arbor,
                    RealtimeVoice::Sol,
                    RealtimeVoice::Cove,
                ],
                v2: vec![
                    RealtimeVoice::Alloy,
                    RealtimeVoice::Ash,
                    RealtimeVoice::Ballad,
                    RealtimeVoice::Coral,
                    RealtimeVoice::Echo,
                    RealtimeVoice::Sage,
                    RealtimeVoice::Shimmer,
                    RealtimeVoice::Verse,
                    RealtimeVoice::Marin,
                    RealtimeVoice::Cedar,
                ],
                default_v1: RealtimeVoice::Cove,
                default_v2: RealtimeVoice::Marin,
            }
        );
    }

    #[test]
    fn user_input_text_serializes_empty_text_elements() -> Result<()> {
        let input = UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        };

        let json_input = serde_json::to_value(input)?;
        assert_eq!(
            json_input,
            json!({
                "type": "text",
                "text": "hello",
                "text_elements": [],
            })
        );

        Ok(())
    }

    #[test]
    fn user_message_event_serializes_empty_metadata_vectors() -> Result<()> {
        let event = UserMessageEvent {
            client_id: None,
            message: "hello".to_string(),
            images: None,
            local_images: Vec::new(),
            text_elements: Vec::new(),
            ..Default::default()
        };

        let json_event = serde_json::to_value(event)?;
        assert_eq!(
            json_event,
            json!({
                "message": "hello",
                "local_images": [],
                "text_elements": [],
            })
        );

        Ok(())
    }

    #[test]
    fn user_message_event_deserializes_without_image_detail_fields() -> Result<()> {
        let event: UserMessageEvent = serde_json::from_value(json!({
            "message": "hello",
            "images": ["https://example.com/image.png"],
            "local_images": ["/tmp/local.png"],
            "text_elements": [],
        }))?;

        assert_eq!(event.message, "hello");
        assert_eq!(
            event.images,
            Some(vec!["https://example.com/image.png".to_string()])
        );
        assert_eq!(event.image_details, Vec::<Option<ImageDetail>>::new());
        assert_eq!(event.local_images, vec![PathBuf::from("/tmp/local.png")]);
        assert_eq!(event.local_image_details, Vec::<Option<ImageDetail>>::new());
        assert_eq!(event.text_elements, Vec::new());

        Ok(())
    }

    #[test]
    fn user_message_item_legacy_event_preserves_image_details() {
        let local_path = PathBuf::from("/tmp/local.png");
        let mut item = UserMessageItem::new(&[
            crate::user_input::UserInput::Image {
                image_url: "https://example.com/first.png".to_string(),
                detail: Some(ImageDetail::Original),
            },
            crate::user_input::UserInput::Image {
                image_url: "https://example.com/second.png".to_string(),
                detail: None,
            },
            crate::user_input::UserInput::LocalImage {
                path: local_path.clone(),
                detail: Some(ImageDetail::Original),
            },
        ]);
        item.client_id = Some("client-message-1".to_string());

        let EventMsg::UserMessage(event) = item.as_legacy_event() else {
            panic!("expected user message event");
        };

        assert_eq!(
            event.images,
            Some(vec![
                "https://example.com/first.png".to_string(),
                "https://example.com/second.png".to_string(),
            ])
        );
        assert_eq!(event.client_id, Some("client-message-1".to_string()));
        assert_eq!(event.image_details, vec![Some(ImageDetail::Original)]);
        assert_eq!(event.local_images, vec![local_path]);
        assert_eq!(event.local_image_details, vec![Some(ImageDetail::Original)]);
    }

    #[test]
    fn turn_aborted_event_deserializes_without_turn_id() -> Result<()> {
        let event: EventMsg = serde_json::from_value(json!({
            "type": "turn_aborted",
            "reason": "interrupted",
        }))?;

        match event {
            EventMsg::TurnAborted(TurnAbortedEvent {
                turn_id, reason, ..
            }) => {
                assert_eq!(turn_id, None);
                assert_eq!(reason, TurnAbortReason::Interrupted);
            }
            _ => panic!("expected turn_aborted event"),
        }

        Ok(())
    }

    #[test]
    fn turn_context_item_deserializes_without_network() -> Result<()> {
        let item: TurnContextItem = serde_json::from_value(json!({
            "cwd": test_path_buf("/tmp"),
            "approval_policy": "never",
            "sandbox_policy": { "type": "danger-full-access" },
            "model": "gpt-5",
            "summary": "auto",
        }))?;

        assert_eq!(item.network, None);
        assert_eq!(item.file_system_sandbox_policy, None);
        assert_eq!(item.comp_hash, None);
        Ok(())
    }

    #[test]
    fn multi_agent_version_uses_newest_present_session_meta_value() -> Result<()> {
        let thread_id = "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string();
        let older_meta = SessionMetaLine {
            meta: SessionMeta {
                id: thread_id.clone(),
                multi_agent_version: Some(MultiAgentVersion::V2),
                ..Default::default()
            },
            git: None,
        };
        let newer_meta_without_version = SessionMetaLine {
            meta: SessionMeta {
                id: thread_id.clone(),
                multi_agent_version: None,
                ..Default::default()
            },
            git: None,
        };

        assert_eq!(
            multi_agent_version_from_items(
                &[
                    RolloutItem::SessionMeta(older_meta),
                    RolloutItem::SessionMeta(newer_meta_without_version),
                ],
                Some(thread_id),
            ),
            Some(MultiAgentVersion::V2)
        );
        Ok(())
    }

    #[test]
    fn latest_effective_multi_agent_mode_uses_latest_turn_context_even_when_unset() -> Result<()> {
        let turn_context_item = |multi_agent_mode| -> Result<RolloutItem> {
            let mut value = json!({
                "cwd": test_path_buf("/tmp"),
                "approval_policy": "never",
                "sandbox_policy": { "type": "danger-full-access" },
                "model": "gpt-5",
                "summary": "auto",
            });
            value["multi_agent_mode"] = serde_json::to_value(multi_agent_mode)?;
            Ok(RolloutItem::TurnContext(serde_json::from_value(value)?))
        };

        assert_eq!(
            InitialHistory::Forked(vec![
                turn_context_item(Some(MultiAgentMode::Proactive))?,
                turn_context_item(None)?,
            ])
            .get_latest_effective_multi_agent_mode(),
            None
        );
        Ok(())
    }

    #[test]
    fn turn_context_item_serializes_network_when_present() -> Result<()> {
        let item = TurnContextItem {
            turn_id: None,
            cwd: test_path_buf("/tmp").abs(),
            workspace_roots: None,
            current_date: None,
            timezone: None,
            approval_policy: AskForApproval::Never,
            sandbox_policy: SandboxPolicy::DangerFullAccess,
            permission_profile: None,
            network: Some(TurnContextNetworkItem {
                allowed_domains: vec!["api.example.com".to_string()],
                denied_domains: vec!["blocked.example.com".to_string()],
            }),
            file_system_sandbox_policy: Some(FileSystemSandboxPolicy::restricted(vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::GlobPattern {
                        pattern: "/tmp/private/**/*.txt".to_string(),
                    },
                    access: FileSystemAccessMode::Deny,
                },
            ])),
            model: "gpt-5".to_string(),
            comp_hash: None,
            personality: None,
            collaboration_mode: None,
            multi_agent_version: None,
            multi_agent_mode: None,
            realtime_active: None,
            effort: None,
            summary: ReasoningSummaryConfig::Auto,
        };

        let value = serde_json::to_value(item)?;
        assert_eq!(
            value["network"],
            json!({
                "allowed_domains": ["api.example.com"],
                "denied_domains": ["blocked.example.com"],
            })
        );
        assert_eq!(
            value["file_system_sandbox_policy"],
            json!({
                "kind": "restricted",
                "entries": [{
                    "path": {
                        "type": "glob_pattern",
                        "pattern": "/tmp/private/**/*.txt"
                    },
                    "access": "deny"
                }]
            })
        );
        assert_eq!(value["summary"], json!("auto"));
        Ok(())
    }

    #[test]
    fn serialize_event() -> Result<()> {
        let session_id = "67e55044-10b1-426f-9247-bb680e5fe0c7".to_string();
        let thread_id = "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string();
        let rollout_file = NamedTempFile::new()?;
        let permission_profile = PermissionProfile::read_only();
        let event = Event {
            id: "1234".to_string(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id,
                thread_id,
                forked_from_id: None,
                parent_thread_id: None,
                thread_source: None,
                thread_name: None,
                model: "codex-mini-latest".to_string(),
                model_provider_id: "openai".to_string(),
                service_tier: None,
                approval_policy: AskForApproval::Never,
                approvals_reviewer: ApprovalsReviewer::User,
                permission_profile: permission_profile.clone(),
                active_permission_profile: None,
                cwd: test_path_buf("/home/user/project").abs(),
                reasoning_effort: Some(ReasoningEffortConfig::default()),
                initial_messages: None,
                network_proxy: None,
                rollout_path: Some(rollout_file.path().to_path_buf()),
            }),
        };

        let expected = json!({
            "id": "1234",
            "msg": {
                "type": "session_configured",
                "session_id": "67e55044-10b1-426f-9247-bb680e5fe0c7",
                "thread_id": "67e55044-10b1-426f-9247-bb680e5fe0c8",
                "model": "codex-mini-latest",
                "model_provider_id": "openai",
                "approval_policy": "never",
                "approvals_reviewer": "user",
                "permission_profile": permission_profile,
                "cwd": test_path_buf("/home/user/project"),
                "reasoning_effort": "medium",
                "rollout_path": format!("{}", rollout_file.path().display()),
            }
        });
        assert_eq!(expected, serde_json::to_value(&event)?);
        Ok(())
    }

    #[test]
    fn deserialize_legacy_session_configured_event_uses_sandbox_policy() -> Result<()> {
        let cwd = test_path_buf("/home/user/project");
        let value = json!({
            "session_id": "67e55044-10b1-426f-9247-bb680e5fe0c8",
            "model": "codex-mini-latest",
            "model_provider_id": "openai",
            "approval_policy": "never",
            "approvals_reviewer": "user",
            "sandbox_policy": {
                "type": "read-only"
            },
            "cwd": cwd,
        });

        let event: SessionConfiguredEvent = serde_json::from_value(value)?;
        assert_eq!(event.permission_profile, PermissionProfile::read_only());
        Ok(())
    }

    #[test]
    fn vec_u8_as_base64_serialization_and_deserialization() -> Result<()> {
        let event = ExecCommandOutputDeltaEvent {
            call_id: "call21".to_string(),
            stream: ExecOutputStream::Stdout,
            chunk: vec![1, 2, 3, 4, 5],
        };
        let serialized = serde_json::to_string(&event)?;
        assert_eq!(
            r#"{"call_id":"call21","stream":"stdout","chunk":"AQIDBAU="}"#,
            serialized,
        );

        let deserialized: ExecCommandOutputDeltaEvent = serde_json::from_str(&serialized)?;
        assert_eq!(deserialized, event);
        Ok(())
    }

    #[test]
    fn serialize_mcp_startup_update_event() -> Result<()> {
        let event = Event {
            id: "init".to_string(),
            msg: EventMsg::McpStartupUpdate(McpStartupUpdateEvent {
                server: "srv".to_string(),
                status: McpStartupStatus::Failed {
                    error: "boom".to_string(),
                },
            }),
        };

        let value = serde_json::to_value(&event)?;
        assert_eq!(value["msg"]["type"], "mcp_startup_update");
        assert_eq!(value["msg"]["server"], "srv");
        assert_eq!(value["msg"]["status"]["state"], "failed");
        assert_eq!(value["msg"]["status"]["error"], "boom");
        Ok(())
    }

    #[test]
    fn serialize_mcp_startup_complete_event() -> Result<()> {
        let event = Event {
            id: "init".to_string(),
            msg: EventMsg::McpStartupComplete(McpStartupCompleteEvent {
                ready: vec!["a".to_string()],
                failed: vec![McpStartupFailure {
                    server: "b".to_string(),
                    error: "bad".to_string(),
                }],
                cancelled: vec!["c".to_string()],
            }),
        };

        let value = serde_json::to_value(&event)?;
        assert_eq!(value["msg"]["type"], "mcp_startup_complete");
        assert_eq!(value["msg"]["ready"][0], "a");
        assert_eq!(value["msg"]["failed"][0]["server"], "b");
        assert_eq!(value["msg"]["failed"][0]["error"], "bad");
        assert_eq!(value["msg"]["cancelled"][0], "c");
        Ok(())
    }

    #[test]
    fn token_usage_info_new_or_append_updates_context_window_when_provided() {
        let initial = Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage::default(),
            model_context_window: Some(258_400),
        });
        let last = Some(TokenUsage {
            input_tokens: 10,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 10,
        });

        let info = TokenUsageInfo::new_or_append(&initial, &last, Some(128_000))
            .expect("new_or_append should return info");

        assert_eq!(info.model_context_window, Some(128_000));
    }

    #[test]
    fn token_usage_info_new_or_append_preserves_context_window_when_not_provided() {
        let initial = Some(TokenUsageInfo {
            total_token_usage: TokenUsage::default(),
            last_token_usage: TokenUsage::default(),
            model_context_window: Some(258_400),
        });
        let last = Some(TokenUsage {
            input_tokens: 10,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 10,
        });

        let info = TokenUsageInfo::new_or_append(&initial, &last, None)
            .expect("new_or_append should return info");

        assert_eq!(info.model_context_window, Some(258_400));
    }
}
