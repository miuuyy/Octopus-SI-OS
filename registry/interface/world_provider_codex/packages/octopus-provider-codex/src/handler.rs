use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_appserver_proto::{
    AccountLoginCompletedNotification, AccountUpdatedNotification, AdditionalContextEntry,
    AdditionalContextKind, CancelLoginAccountParams,
    CancelLoginAccountResponse, ClientRequest, CommandExecutionSource, CommandExecutionStatus,
    ConsumeAccountRateLimitResetCreditParams, ConsumeAccountRateLimitResetCreditResponse,
    DynamicToolCallOutputContentItem, DynamicToolCallResponse, DynamicToolFunctionSpec,
    DynamicToolNamespaceSpec, DynamicToolNamespaceTool, DynamicToolSpec, GetAccountParams,
    GetAccountRateLimitsResponse, GetAccountResponse, GetAccountTokenUsageParams,
    GetAccountTokenUsageResponse, GetWorkspaceMessagesResponse, HookHandlerMetadata, HookSource,
    HookTrustStatus, HooksListEntry, HooksListParams, HooksListResponse, ItemCompletedNotification,
    JSONRPCMessage, JSONRPCRequest, LoginAccountParams, LoginAccountResponse,
    LogoutAccountResponse, ModelListParams, ModelListResponse, ModelProviderCapabilitiesReadParams,
    ModelProviderCapabilitiesReadResponse, ModelSafetyBufferingUpdatedNotification, RequestId,
    SandboxMode, ServerNotification, ServerRequest, SortDirection, ThreadCompactStartParams,
    ThreadCompactStartResponse, ThreadInjectItemsParams, ThreadInjectItemsResponse, ThreadItem,
    ThreadItemsListParams, ThreadItemsListResponse, ThreadReadParams, ThreadReadResponse,
    ThreadResumeParams, ThreadResumeResponse, ThreadTurnsListParams, ThreadTurnsListResponse,
    ThreadStartParams, ThreadStartResponse, ThreadTokenUsage, TokenUsageBreakdown,
    TurnEnvironmentParams, TurnInterruptParams, TurnInterruptResponse, TurnItemsView, TurnPlanStep,
    TurnPlanUpdatedNotification, TurnStartParams, TurnStatus, TurnSteerParams, TurnSteerResponse,
    UserInput, WebSearchAction,
};
use octopus_descriptors::{InterfaceId, MarkId, VerifierId};
use octopus_interfaces::{
    ArtifactBindingPayload, InterfaceArtifactFile, InterfaceArtifactManifest,
    InterfaceBoundaryError as InterfaceDriverError, InterfaceDescriptor, InterfaceDriverConfig,
    InterfaceInvocationEnvelope, RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactRef, InvocationMetadata, RequiredVerifiedArtifact};
use octopus_login::AuthManager;
use octopus_model_context_render::{
    model_hidden_tool_names, model_tool_exclusions, model_visible_affordances, render_model_context_block,
    resolve_instruction_context,
};
use octopus_protocol::config_types::{ReasoningSummary, WebSearchMode};
use octopus_protocol::models::MessagePhase;
use octopus_protocol::openai_models::ReasoningEffort;
use octopus_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderInvocationSessionSnapshot,
    ProviderVerifiedInput, SessionContextArtifact,
};
use octopus_tool_handlers::ToolResultArtifact;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, watch};

use crate::auth::{ScopedSecretError, ScopedSecretStore};
use crate::lifecycle::{
    LIFECYCLE_CONNECTION_LABEL, LIFECYCLE_DECISION_LABEL, LIFECYCLE_EVENT_LABEL,
    LIFECYCLE_HANDLER_OUTPUT_LABEL, LIFECYCLE_HANDLER_PROMPT_LABEL, LIFECYCLE_HANDLER_TASK_LABEL,
    LifecycleBinding, LifecycleContinuation, LifecycleEventArtifact, LifecycleEventName,
    LifecycleInbound, LifecyclePlannedOutput, LifecycleRoute, LifecycleTrigger,
    LifecycleVerifiedInput, artifact_ref_for_file, decode_lifecycle_inbound, validate_held_file,
};
use crate::live_stream::{
    InputRecordedObservation, InputRecordedSource, LiveStreamDisposition, LiveStreamRules,
    LiveTurnIdentity, LiveTurnStream,
    artifact_payload_blake3,
};
use crate::{AppServerClient, AppServerEvent, AppServerRequestHandle, TypedRequestError};
use crate::run_affordances::{NativePending, NativeStep, PeerAction, PeerMessage, RunAffordances, PEER_MESSAGE_LABEL};

#[path = "tool_operator_request.rs"]
mod tool_operator_request;
use tool_operator_request::{ToolOperatorContinuity, ToolOperatorPhase, ToolOperatorRequest};

#[path = "context_refresh.rs"]
mod context_refresh;
use context_refresh::{ContextRefresh, CurrentNativeContext, NativeHistoryProof, NativeHistoryRetirement};

#[path = "native_history.rs"]
mod native_history;
use native_history::{read_native_turn, read_native_turn_items};

#[path = "composer_prediction.rs"]
pub(crate) mod composer_prediction;
#[path = "client_generation.rs"]
pub(crate) mod client_generation;

pub const CODEX_APP_SERVER_INTERFACE_DRIVER_REF: &str = "codex_app_server";

pub(crate) const PROVIDER_TRANSPORT_DISPATCH_LABEL: &str = "provider.transport.dispatch";
const RECOVERY_ROLLOUT_MAX_LINE_BYTES: usize = crate::result_media::RECOVERY_ROLLOUT_MAX_LINE_BYTES;
const SOURCE_ARTIFACT_FIELD: &str = "source_artifact";
const OPERATOR_MESSAGE_LABEL: &str = "operator.message";
const INTERFACE_ARTIFACT_OUTPUT_LABEL: &str = "interface.artifact.output";
const TOOL_CALL_LABEL: &str = "tool.call";
const INTERFACE_CONTEXT_UPDATED_LABEL: &str = "interface.context.updated";
const OPERATOR_APPROVAL_REQUEST_LABEL: &str = "operator.approval.request";
const OPERATOR_ELICITATION_REQUEST_LABEL: &str = "operator.elicitation.request";
const OPERATOR_USER_INPUT_REQUEST_LABEL: &str = "operator.user_input.request";
const OPERATOR_APPROVAL_RESPONSE_LABEL: &str = "operator.approval.response";
const OPERATOR_ELICITATION_RESPONSE_LABEL: &str = "operator.elicitation.response";
const OPERATOR_USER_INPUT_RESPONSE_LABEL: &str = "operator.user_input.response";
pub(crate) const OPERATOR_TURN_CONTROL_LABEL: &str = "operator.turn.control";
pub(crate) const TURN_CONTROL_ACKNOWLEDGED_LABEL: &str = "turn.control.acknowledged";
pub(crate) const TURN_INPUT_ACKNOWLEDGED_LABEL: &str = "turn.input.acknowledged";
pub(crate) const TURN_CONTROL_FAILED_LABEL: &str = "turn.control.failed";

const TURN_REASONING_LABEL: &str = "turn.reasoning";
const TURN_PLAN_UPDATE_LABEL: &str = "turn.plan.update";
const CONTEXT_WINDOW_USAGE_FIELD: &str = "context_window_usage";
const PROVIDER_TOKEN_USAGE_FIELD: &str = "provider_token_usage";
const TURN_EXEC_COMMAND_LABEL: &str = "turn.exec.command";
const HOOK_RESULT_LABEL: &str = "hook.result";
const GOAL_STATUS_LABEL: &str = "goal.status";
pub(crate) const TURN_ERROR_LABEL: &str = "turn.error";
const PROVIDER_AUTHENTICATION_FAILURE_CODE: &str = "provider.authentication.invalid";
const PROVIDER_CONTINUITY_INCOMPATIBLE_CODE: &str = "provider.continuity.incompatible";
const CODEX_CONTINUITY_MALFORMED_PREFIX: &str = "Codex continuity state is malformed: ";
const AGENT_MESSAGE_DELTA_LABEL: &str = "agent.message.delta";
const TURN_AGENT_MESSAGE_LABEL: &str = "turn.agent_message";
const TURN_WEB_SEARCH_LABEL: &str = "turn.web_search";
const HOOK_CATALOG_LABEL: &str = "hook.catalog";
const HOOK_CATALOG_ERROR_LABEL: &str = "hook.catalog.error";
const TURN_HOOK_PROMPT_LABEL: &str = "turn.hook.prompt";
const TURN_FILE_CHANGE_LABEL: &str = "turn.file.change";
const TURN_MCP_TOOL_CALL_LABEL: &str = "turn.mcp.tool_call";
const TURN_DYNAMIC_TOOL_CALL_LABEL: &str = "turn.dynamic_tool_call";
pub(crate) const TURN_INTERRUPTED_RESULT_LABEL: &str = "turn.interrupted.result";
const TURN_COLLABORATION_LABEL: &str = "turn.collaboration";
const TURN_SUBAGENT_ACTIVITY_LABEL: &str = "turn.subagent.activity";
const TURN_IMAGE_VIEW_LABEL: &str = "turn.image.view";
const TURN_SLEEP_LABEL: &str = "turn.sleep";
const TURN_IMAGE_GENERATION_LABEL: &str = "turn.image.generation";
const TURN_REVIEW_ENTERED_LABEL: &str = "turn.review.entered";
const TURN_REVIEW_EXITED_LABEL: &str = "turn.review.exited";
const TURN_PLAN_DELTA_LABEL: &str = "turn.plan.delta";
const TURN_REASONING_DELTA_LABEL: &str = "turn.reasoning.delta";
const TURN_EXEC_OUTPUT_DELTA_LABEL: &str = "turn.exec.output_delta";
const TURN_FILE_CHANGE_DELTA_LABEL: &str = "turn.file.change_delta";
const TURN_MCP_PROGRESS_LABEL: &str = "turn.mcp.progress";
const TURN_DIFF_UPDATED_LABEL: &str = "turn.diff.updated";
const TURN_TOKEN_USAGE_LABEL: &str = "turn.token_usage";
const TURN_CONTEXT_COMPACTION_LABEL: &str = "turn.context.compaction";
const TURN_MODEL_REROUTED_LABEL: &str = "turn.model.rerouted";
const TURN_MODEL_VERIFICATION_LABEL: &str = "turn.model.verification";
const TURN_AUTH_RECOVERY_STARTED_LABEL: &str = "turn.auth_recovery.started";
const TURN_AUTH_RECOVERY_COMPLETED_LABEL: &str = "turn.auth_recovery.completed";
const TURN_WARNING_LABEL: &str = "turn.warning";
const TURN_STARTED_LABEL: &str = "turn.started";
const TURN_COMPLETED_LABEL: &str = "turn.completed";
const TURN_TERMINAL_INTERACTION_LABEL: &str = "turn.exec.terminal_interaction";
const TURN_AUTO_REVIEW_STARTED_LABEL: &str = "turn.auto_review.started";
const TURN_AUTO_REVIEW_COMPLETED_LABEL: &str = "turn.auto_review.completed";
const TURN_OPERATOR_REQUEST_RESOLVED_LABEL: &str = "turn.operator_request.resolved";
const TURN_MCP_STATUS_LABEL: &str = "turn.mcp.status";
const ACCOUNT_RATE_LIMITS_LABEL: &str = "account.rate_limits";
const PROVIDER_MODELS_LIST_LABEL: &str = "provider.models.list";
const PROVIDER_MODELS_CATALOG_LABEL: &str = "provider.models.catalog";
const PROVIDER_OPERATION_ERROR_LABEL: &str = "provider.operation.error";
const PROVIDER_ACCOUNT_LOGIN_START_LABEL: &str = "provider.account.login.start";
const PROVIDER_ACCOUNT_LOGIN_STARTED_LABEL: &str = "provider.account.login.started";
const PROVIDER_ACCOUNT_LOGIN_CANCEL_LABEL: &str = "provider.account.login.cancel";
const PROVIDER_ACCOUNT_LOGIN_CANCELLED_LABEL: &str = "provider.account.login.cancelled";
const PROVIDER_ACCOUNT_LOGIN_STATUS_READ_LABEL: &str = "provider.account.login.status.read";
const PROVIDER_ACCOUNT_LOGIN_STATUS_LABEL: &str = "provider.account.login.status";
const PROVIDER_ACCOUNT_LOGOUT_LABEL: &str = "provider.account.logout";
const PROVIDER_ACCOUNT_LOGGED_OUT_LABEL: &str = "provider.account.logged_out";
const PROVIDER_ACCOUNT_READ_LABEL: &str = "provider.account.read";
const PROVIDER_ACCOUNT_SNAPSHOT_LABEL: &str = "provider.account.snapshot";
const PROVIDER_ACCOUNT_RATE_LIMITS_READ_LABEL: &str = "provider.account.rate_limits.read";
const PROVIDER_ACCOUNT_RATE_LIMITS_LABEL: &str = "provider.account.rate_limits";
const PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUME_LABEL: &str =
    "provider.account.rate_limit_reset.consume";
const PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUMED_LABEL: &str =
    "provider.account.rate_limit_reset.consumed";
const PROVIDER_ACCOUNT_USAGE_READ_LABEL: &str = "provider.account.usage.read";
const PROVIDER_ACCOUNT_USAGE_LABEL: &str = "provider.account.usage";
const PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_READ_LABEL: &str =
    "provider.account.workspace_messages.read";
const PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_LABEL: &str = "provider.account.workspace_messages";
const PROVIDER_CAPABILITIES_READ_LABEL: &str = "provider.capabilities.read";
const PROVIDER_CAPABILITIES_LABEL: &str = "provider.capabilities";
const PROVIDER_THREAD_COMPACT_START_LABEL: &str = "provider.thread.compact.start";
const TURN_TRIGGER_INPUT: &str = "octopus_input";
const TURN_TRIGGER_TOOL_RESULT_RECOVERY: &str = "octopus_tool_result_recovery";
const TURN_TRIGGER_RESUME_INTERRUPTED_TASK: &str = "resume_interrupted_task";
const PROVIDER_ACCOUNT_LOGIN_COMPLETED_LABEL: &str = "provider.account.login.completed";
const PROVIDER_ACCOUNT_UPDATED_LABEL: &str = "provider.account.updated";
const PROVIDER_ACCOUNT_RATE_LIMITS_UPDATED_LABEL: &str = "provider.account.rate_limits.updated";
const CLIENT_ACTIVITY_OUTPUT_LABELS: &[&str] = &[
    AGENT_MESSAGE_DELTA_LABEL,
    TURN_AGENT_MESSAGE_LABEL,
    TURN_REASONING_LABEL,
    TURN_REASONING_DELTA_LABEL,
    TURN_PLAN_UPDATE_LABEL,
    TURN_PLAN_DELTA_LABEL,
    TURN_EXEC_COMMAND_LABEL,
    TURN_EXEC_OUTPUT_DELTA_LABEL,
    TURN_TERMINAL_INTERACTION_LABEL,
    TURN_FILE_CHANGE_LABEL,
    TURN_FILE_CHANGE_DELTA_LABEL,
    TURN_DIFF_UPDATED_LABEL,
    TURN_WEB_SEARCH_LABEL,
    HOOK_CATALOG_LABEL,
    HOOK_CATALOG_ERROR_LABEL,
    HOOK_RESULT_LABEL,
    TURN_HOOK_PROMPT_LABEL,
    TURN_MCP_TOOL_CALL_LABEL,
    TURN_MCP_PROGRESS_LABEL,
    TURN_MCP_STATUS_LABEL,
    TURN_DYNAMIC_TOOL_CALL_LABEL,
    TURN_COLLABORATION_LABEL,
    TURN_SUBAGENT_ACTIVITY_LABEL,
    TURN_IMAGE_VIEW_LABEL,
    TURN_SLEEP_LABEL,
    TURN_IMAGE_GENERATION_LABEL,
    TURN_REVIEW_ENTERED_LABEL,
    TURN_REVIEW_EXITED_LABEL,
    TURN_CONTEXT_COMPACTION_LABEL,
    TURN_STARTED_LABEL,
    TURN_COMPLETED_LABEL,
    TURN_MODEL_REROUTED_LABEL,
    TURN_MODEL_VERIFICATION_LABEL,
    TURN_AUTH_RECOVERY_STARTED_LABEL,
    TURN_AUTH_RECOVERY_COMPLETED_LABEL,
    TURN_WARNING_LABEL,
    TURN_AUTO_REVIEW_STARTED_LABEL,
    TURN_AUTO_REVIEW_COMPLETED_LABEL,
    TURN_OPERATOR_REQUEST_RESOLVED_LABEL,
    GOAL_STATUS_LABEL,
    ACCOUNT_RATE_LIMITS_LABEL,
];

const REQUIRED_CLIENT_BOUNDARY_OUTPUT_LABELS: &[&str] = &[
    OPERATOR_USER_INPUT_REQUEST_LABEL,
    PROVIDER_MODELS_CATALOG_LABEL,
    PROVIDER_OPERATION_ERROR_LABEL,
    TURN_CONTROL_FAILED_LABEL,
    LIFECYCLE_EVENT_LABEL,
];


#[path = "stopped_result.rs"]
pub(crate) mod stopped_result;
#[cfg(test)]
#[path = "../tests/support/input_decoder.rs"]
mod input_decoder_tests;

mod provider_operations;
pub(crate) use provider_operations::*;
mod operator_continuity;
pub(crate) use operator_continuity::*;
mod turn_output;
pub(crate) use turn_output::*;
mod turn_state;
pub(crate) use turn_state::*;
mod output_stream;
pub(crate) use output_stream::*;
mod transport_dispatch;
pub(crate) use transport_dispatch::*;
mod turn_control;
pub(crate) use turn_control::*;
mod native_catalog;
pub(crate) use native_catalog::*;
mod driver;
pub use driver::*;
mod projection_contract;
pub(crate) use projection_contract::*;
mod continuity_recovery;
pub(crate) use continuity_recovery::*;
mod driver_operations;
mod driver_receive;
mod result_arrival;
pub(crate) use result_arrival::*;
mod lifecycle_resumption;
pub(crate) use lifecycle_resumption::*;
mod operator_approval;
pub(crate) use operator_approval::*;
mod turn_thread;
pub(crate) use turn_thread::*;
mod rollout_recovery;
mod turn_start;
pub(crate) use turn_start::*;
mod drain_events;
pub(crate) use drain_events::*;
mod driver_pending;
mod driver_resume;
mod turn_sink;
pub(crate) use turn_sink::*;
mod driver_compaction;
mod driver_turn;
mod thread_params;
mod tool_schema;
pub(crate) use thread_params::*;
mod operation_catalog;
pub(crate) use operation_catalog::*;
mod recovered_inputs;
pub(crate) use recovered_inputs::*;
mod input_decoding;
pub(crate) use input_decoding::*;
mod input_messages;
mod message_revision;
use message_revision::revised_message_history;
pub(crate) use input_messages::*;
mod interrupted_results;
pub(crate) use interrupted_results::*;
mod native_results;
pub(crate) use native_results::*;
mod operator_requests;
pub(crate) use operator_requests::*;
mod hooks;
pub(crate) use hooks::*;
mod item_outputs;
pub(crate) use item_outputs::*;
mod session_continuity;
pub(crate) use session_continuity::*;
mod context_assembly;
pub use context_assembly::*;

#[cfg(test)]
#[path = "../tests/support/handler_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "wire_roundtrip.rs"]
mod wire_roundtrip;
