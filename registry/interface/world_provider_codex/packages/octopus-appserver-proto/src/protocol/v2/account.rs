use super::ThreadUsage;
use crate::protocol::common::AuthMode;
use octopus_experimental_api_macros::ExperimentalApi;
use octopus_protocol::account::AmazonBedrockCredentialSource;
use octopus_protocol::account::PlanType;
use octopus_protocol::account::ProviderAccount;
use octopus_protocol::protocol::CreditsSnapshot as CoreCreditsSnapshot;
use octopus_protocol::protocol::RateLimitReachedType as CoreRateLimitReachedType;
use octopus_protocol::protocol::RateLimitSnapshot as CoreRateLimitSnapshot;
use octopus_protocol::protocol::RateLimitWindow as CoreRateLimitWindow;
use octopus_protocol::protocol::SpendControlLimitSnapshot as CoreSpendControlLimitSnapshot;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
#[ts(export_to = "v2/")]
pub enum Account {
    #[serde(rename = "apiKey", rename_all = "camelCase")]
    #[ts(rename = "apiKey", rename_all = "camelCase")]
    ApiKey {},

    #[serde(rename = "chatgpt", rename_all = "camelCase")]
    #[ts(rename = "chatgpt", rename_all = "camelCase")]
    Chatgpt {
        email: Option<String>,
        plan_type: PlanType,
        workspace_name: Option<String>,
    },

    #[serde(rename = "amazonBedrock", rename_all = "camelCase")]
    #[ts(rename = "amazonBedrock", rename_all = "camelCase")]
    AmazonBedrock {
        #[serde(default)]
        uses_codex_managed_credentials: bool,
    },

    #[serde(rename = "gcp", rename_all = "camelCase")]
    #[ts(rename = "gcp", rename_all = "camelCase")]
    Gcp {},
}

impl From<ProviderAccount> for Account {
    fn from(account: ProviderAccount) -> Self {
        match account {
            ProviderAccount::ApiKey => Self::ApiKey {},
            ProviderAccount::Chatgpt { email, plan_type } => Self::Chatgpt {
                email: Some(email),
                plan_type,
                workspace_name: None,
            },
            ProviderAccount::AmazonBedrock { credential_source } => Self::AmazonBedrock {
                uses_codex_managed_credentials: credential_source
                    == AmazonBedrockCredentialSource::CodexManaged,
            },
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, ExperimentalApi)]
#[serde(tag = "type")]
#[ts(tag = "type")]
#[ts(export_to = "v2/")]
pub enum LoginAccountParams {
    #[serde(rename = "apiKey", rename_all = "camelCase")]
    #[ts(rename = "apiKey", rename_all = "camelCase")]
    ApiKey {
        #[serde(rename = "apiKey")]
        #[ts(rename = "apiKey")]
        api_key: String,
    },
    #[serde(rename = "chatgpt", rename_all = "camelCase")]
    #[ts(rename = "chatgpt", rename_all = "camelCase")]
    Chatgpt {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        codex_streamlined_login: bool,
    },
    #[serde(rename = "chatgptDeviceCode")]
    #[ts(rename = "chatgptDeviceCode")]
    ChatgptDeviceCode,
    #[experimental("account/login/start.chatgptAuthTokens")]
    #[serde(rename = "chatgptAuthTokens", rename_all = "camelCase")]
    #[ts(rename = "chatgptAuthTokens", rename_all = "camelCase")]
    ChatgptAuthTokens {
        access_token: String,
        chatgpt_account_id: String,
        #[ts(optional = nullable)]
        chatgpt_plan_type: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type")]
#[ts(export_to = "v2/")]
pub enum LoginAccountResponse {
    #[serde(rename = "apiKey", rename_all = "camelCase")]
    #[ts(rename = "apiKey", rename_all = "camelCase")]
    ApiKey {},
    #[serde(rename = "chatgpt", rename_all = "camelCase")]
    #[ts(rename = "chatgpt", rename_all = "camelCase")]
    Chatgpt { login_id: String, auth_url: String },
    #[serde(rename = "chatgptDeviceCode", rename_all = "camelCase")]
    #[ts(rename = "chatgptDeviceCode", rename_all = "camelCase")]
    ChatgptDeviceCode {
        login_id: String,
        verification_url: String,
        user_code: String,
    },
    #[serde(rename = "chatgptAuthTokens", rename_all = "camelCase")]
    #[ts(rename = "chatgptAuthTokens", rename_all = "camelCase")]
    ChatgptAuthTokens {},
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CancelLoginAccountParams {
    pub login_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum CancelLoginAccountStatus {
    Canceled,
    NotFound,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CancelLoginAccountResponse {
    pub status: CancelLoginAccountStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountSessionsAddParams {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub switch_to_added_account: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountSessionsListParams {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh_workspace_metadata: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountSessionsLogoutParams {
    pub session_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountSessionsSwitchParams {
    pub session_id: String,
    pub account_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub allow_browser_login: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(untagged, rename_all_fields = "camelCase")]
#[ts(untagged, rename_all_fields = "camelCase", export_to = "v2/")]
pub enum AccountSessionsSwitchResponse {
    Switched {
        #[serde(flatten)]
        sessions: AccountSessionsResponse,
    },
    BrowserLogin { login_id: String, auth_url: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountSessionsResponse {
    pub active_session_id: Option<String>,
    pub sessions: Vec<AccountSession>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountSession {
    pub session_id: String,
    pub email: Option<String>,
    pub user_id: Option<String>,
    pub display_name: Option<String>,
    pub image_url: Option<String>,
    pub last_used_at: i64,
    pub is_active: bool,
    pub selected_workspace_account_id: Option<String>,
    pub workspaces: Vec<AccountSessionWorkspace>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountSessionWorkspace {
    pub account_id: String,
    pub name: Option<String>,
    pub image_url: Option<String>,
    pub kind: Option<AccountSessionWorkspaceKind>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum AccountSessionWorkspaceKind {
    Personal,
    Workspace,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct LogoutAccountResponse {}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum ChatgptAuthTokensRefreshReason {
    Unauthorized,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ChatgptAuthTokensRefreshParams {
    pub reason: ChatgptAuthTokensRefreshReason,
    #[ts(optional = nullable)]
    pub previous_account_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ChatgptAuthTokensRefreshResponse {
    pub access_token: String,
    pub chatgpt_account_id: String,
    pub chatgpt_plan_type: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GetAccountRateLimitsResponse {
    pub ordinary_usage_allowed: Option<bool>,
    pub rate_limits: RateLimitSnapshot,
    pub rate_limits_by_limit_id: Option<HashMap<String, RateLimitSnapshot>>,
    pub rate_limit_reset_credits: Option<RateLimitResetCreditsSummary>,
    pub account_id: Option<String>,
    pub rate_limit_upsell: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RateLimitResetCreditsSummary {
    pub available_count: i64,
    pub credits: Option<Vec<RateLimitResetCredit>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RateLimitResetCredit {
    pub id: String,
    pub reset_type: RateLimitResetType,
    pub status: RateLimitResetCreditStatus,
    pub granted_at: i64,
    pub expires_at: Option<i64>,
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/", rename_all = "camelCase")]
pub enum RateLimitResetType {
    CodexRateLimits,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/", rename_all = "camelCase")]
pub enum RateLimitResetCreditStatus {
    Available,
    Redeeming,
    Redeemed,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ConsumeAccountRateLimitResetCreditParams {
    pub idempotency_key: String,
    #[ts(optional = nullable)]
    pub credit_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ConsumeAccountRateLimitResetCreditResponse {
    pub outcome: ConsumeAccountRateLimitResetCreditOutcome,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/", rename_all = "camelCase")]
pub enum ConsumeAccountRateLimitResetCreditOutcome {
    Reset,
    NothingToReset,
    NoCredit,
    AlreadyRedeemed,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GetAccountTokenUsageParams {
    #[ts(optional = nullable)]
    pub thread_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GetAccountTokenUsageResponse {
    pub summary: AccountTokenUsageSummary,
    pub daily_usage_buckets: Option<Vec<AccountTokenUsageDailyBucket>>,
    #[serde(default)]
    #[ts(optional, as = "Option<Option<ThreadUsage>>")]
    pub thread_usage: Option<ThreadUsage>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GetWorkspaceMessagesResponse {
    pub feature_enabled: bool,
    pub messages: Vec<WorkspaceMessage>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkspaceMessage {
    pub message_id: String,
    pub message_type: WorkspaceMessageType,
    pub message_body: String,
    #[ts(type = "number | null")]
    pub created_at: Option<i64>,
    #[ts(type = "number | null")]
    pub archived_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/", rename_all = "snake_case")]
pub enum WorkspaceMessageType {
    Headline,
    Announcement,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountTokenUsageSummary {
    pub lifetime_tokens: Option<i64>,
    pub peak_daily_tokens: Option<i64>,
    pub longest_running_turn_sec: Option<i64>,
    pub current_streak_days: Option<i64>,
    pub longest_streak_days: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountTokenUsageDailyBucket {
    pub start_date: String,
    pub tokens: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SendAddCreditsNudgeEmailParams {
    pub credit_type: AddCreditsNudgeCreditType,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/", rename_all = "snake_case")]
pub enum AddCreditsNudgeCreditType {
    Credits,
    UsageLimit,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SendAddCreditsNudgeEmailResponse {
    pub status: AddCreditsNudgeEmailStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/", rename_all = "snake_case")]
pub enum AddCreditsNudgeEmailStatus {
    Sent,
    CooldownActive,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GetAccountParams {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refresh_token: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS, ExperimentalApi)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct GetAccountResponse {
    pub account: Option<Account>,
    pub requires_openai_auth: bool,
    #[experimental("account/read.workspaceRouting")]
    pub workspace_routing: Option<WorkspaceRouting>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WorkspaceRouting {
    pub chatgpt_account_id: String,
    pub backend_origin: String,
    pub account_routing_override: AccountRoutingOverride,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/", rename_all = "snake_case")]
pub enum AccountRoutingOverride {
    #[serde(rename = "NO_CONSTRAINT")]
    #[ts(rename = "NO_CONSTRAINT")]
    NoConstraint,
    Us,
    UsCr,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountUpdatedNotification {
    pub auth_mode: Option<AuthMode>,
    pub plan_type: Option<PlanType>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountRateLimitsUpdatedNotification {
    pub rate_limits: RateLimitSnapshot,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RateLimitSnapshot {
    pub limit_id: Option<String>,
    pub limit_name: Option<String>,
    pub normal_model_slug: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits: Option<CreditsSnapshot>,
    pub individual_limit: Option<SpendControlLimitSnapshot>,
    pub spend_control_reached: Option<bool>,
    pub plan_type: Option<PlanType>,
    pub rate_limit_reached_type: Option<RateLimitReachedType>,
}

impl From<CoreRateLimitSnapshot> for RateLimitSnapshot {
    fn from(value: CoreRateLimitSnapshot) -> Self {
        Self {
            limit_id: value.limit_id,
            limit_name: value.limit_name,
            normal_model_slug: None,
            primary: value.primary.map(RateLimitWindow::from),
            secondary: value.secondary.map(RateLimitWindow::from),
            credits: value.credits.map(CreditsSnapshot::from),
            individual_limit: value.individual_limit.map(SpendControlLimitSnapshot::from),
            plan_type: value.plan_type,
            rate_limit_reached_type: value
                .rate_limit_reached_type
                .map(RateLimitReachedType::from),
            spend_control_reached: None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "v2/", rename_all = "snake_case")]
pub enum RateLimitReachedType {
    RateLimitReached,
    WorkspaceOwnerCreditsDepleted,
    WorkspaceMemberCreditsDepleted,
    WorkspaceOwnerUsageLimitReached,
    WorkspaceMemberUsageLimitReached,
}

impl From<CoreRateLimitReachedType> for RateLimitReachedType {
    fn from(value: CoreRateLimitReachedType) -> Self {
        match value {
            CoreRateLimitReachedType::RateLimitReached => Self::RateLimitReached,
            CoreRateLimitReachedType::WorkspaceOwnerCreditsDepleted => {
                Self::WorkspaceOwnerCreditsDepleted
            }
            CoreRateLimitReachedType::WorkspaceMemberCreditsDepleted => {
                Self::WorkspaceMemberCreditsDepleted
            }
            CoreRateLimitReachedType::WorkspaceOwnerUsageLimitReached => {
                Self::WorkspaceOwnerUsageLimitReached
            }
            CoreRateLimitReachedType::WorkspaceMemberUsageLimitReached => {
                Self::WorkspaceMemberUsageLimitReached
            }
        }
    }
}

impl From<RateLimitReachedType> for CoreRateLimitReachedType {
    fn from(value: RateLimitReachedType) -> Self {
        match value {
            RateLimitReachedType::RateLimitReached => Self::RateLimitReached,
            RateLimitReachedType::WorkspaceOwnerCreditsDepleted => {
                Self::WorkspaceOwnerCreditsDepleted
            }
            RateLimitReachedType::WorkspaceMemberCreditsDepleted => {
                Self::WorkspaceMemberCreditsDepleted
            }
            RateLimitReachedType::WorkspaceOwnerUsageLimitReached => {
                Self::WorkspaceOwnerUsageLimitReached
            }
            RateLimitReachedType::WorkspaceMemberUsageLimitReached => {
                Self::WorkspaceMemberUsageLimitReached
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct RateLimitWindow {
    pub used_percent: i32,
    #[ts(type = "number | null")]
    pub window_duration_mins: Option<i64>,
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
}

impl From<CoreRateLimitWindow> for RateLimitWindow {
    fn from(value: CoreRateLimitWindow) -> Self {
        Self {
            used_percent: value.used_percent.round() as i32,
            window_duration_mins: value.window_minutes,
            resets_at: value.resets_at,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CreditsSnapshot {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

impl From<CoreCreditsSnapshot> for CreditsSnapshot {
    fn from(value: CoreCreditsSnapshot) -> Self {
        Self {
            has_credits: value.has_credits,
            unlimited: value.unlimited,
            balance: value.balance,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SpendControlLimitSnapshot {
    pub limit: String,
    pub used: String,
    pub remaining_percent: i32,
    #[ts(type = "number")]
    pub resets_at: i64,
}

impl From<CoreSpendControlLimitSnapshot> for SpendControlLimitSnapshot {
    fn from(value: CoreSpendControlLimitSnapshot) -> Self {
        Self {
            limit: value.limit,
            used: value.used,
            remaining_percent: value.remaining_percent,
            resets_at: value.resets_at,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AccountLoginCompletedNotification {
    pub login_id: Option<String>,
    pub success: bool,
    pub error: Option<String>,
    pub onboarding_entrypoint: Option<DesktopOnboardingEntrypoint>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
#[ts(export_to = "v2/")]
pub enum DesktopOnboardingEntrypoint {
    LifeSciences,
}
