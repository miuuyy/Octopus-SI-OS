use super::*;

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderOperationBinding {
    pub(crate) request_label: String,
    pub(crate) output_label: String,
}

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderOperations {
    pub(crate) error_output_label: String,
    pub(crate) account_login_completed_output_label: String,
    pub(crate) account_updated_output_label: String,
    pub(crate) account_rate_limits_updated_output_label: String,
    pub(crate) account_login_start: ProviderOperationBinding,
    pub(crate) account_login_cancel: ProviderOperationBinding,
    pub(crate) account_login_status_read: ProviderOperationBinding,
    pub(crate) account_logout: ProviderOperationBinding,
    pub(crate) account_read: ProviderOperationBinding,
    pub(crate) account_rate_limits_read: ProviderOperationBinding,
    pub(crate) account_rate_limit_reset_consume: ProviderOperationBinding,
    pub(crate) account_usage_read: ProviderOperationBinding,
    pub(crate) account_workspace_messages_read: ProviderOperationBinding,
    pub(crate) capabilities_read: ProviderOperationBinding,
    #[serde(default)]
    pub(crate) thread_compact_start: Option<ProviderOperationBinding>,
    #[serde(default)]
    pub(crate) account_sessions_read: Option<ProviderOperationBinding>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderOperation {
    AccountLoginStart,
    AccountLoginCancel,
    AccountLoginStatusRead,
    AccountLogout,
    AccountRead,
    AccountRateLimitsRead,
    AccountRateLimitResetConsume,
    AccountUsageRead,
    AccountWorkspaceMessagesRead,
    CapabilitiesRead,
    AccountSessionsRead,
}

#[derive(serde::Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AccountLoginStartRequest {
    ApiKey { secret_handle: String },
    ChatgptBrowser { codex_streamlined_login: bool },
    ChatgptDeviceCode,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountLoginIdRequest {
    pub(crate) login_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountReadRequest {
    pub(crate) refresh_token: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountRateLimitResetConsumeRequest {
    pub(crate) idempotency_key: String,
    #[serde(default)]
    pub(crate) credit_id: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountUsageReadRequest {
    #[serde(default)]
    pub(crate) thread_id: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyProviderOperationRequest {}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum ProviderLoginState {
    Pending { method: &'static str },
    Completed { success: bool },
    Cancelled,
}

pub(crate) fn validate_provider_operations(
    operations: &ProviderOperations,
) -> Result<(), InterfaceDriverError> {
    let mut bindings = vec![
        (
            "account_login_start",
            &operations.account_login_start,
            PROVIDER_ACCOUNT_LOGIN_START_LABEL,
            PROVIDER_ACCOUNT_LOGIN_STARTED_LABEL,
        ),
        (
            "account_login_cancel",
            &operations.account_login_cancel,
            PROVIDER_ACCOUNT_LOGIN_CANCEL_LABEL,
            PROVIDER_ACCOUNT_LOGIN_CANCELLED_LABEL,
        ),
        (
            "account_login_status_read",
            &operations.account_login_status_read,
            PROVIDER_ACCOUNT_LOGIN_STATUS_READ_LABEL,
            PROVIDER_ACCOUNT_LOGIN_STATUS_LABEL,
        ),
        (
            "account_logout",
            &operations.account_logout,
            PROVIDER_ACCOUNT_LOGOUT_LABEL,
            PROVIDER_ACCOUNT_LOGGED_OUT_LABEL,
        ),
        (
            "account_read",
            &operations.account_read,
            PROVIDER_ACCOUNT_READ_LABEL,
            PROVIDER_ACCOUNT_SNAPSHOT_LABEL,
        ),
        (
            "account_rate_limits_read",
            &operations.account_rate_limits_read,
            PROVIDER_ACCOUNT_RATE_LIMITS_READ_LABEL,
            PROVIDER_ACCOUNT_RATE_LIMITS_LABEL,
        ),
        (
            "account_rate_limit_reset_consume",
            &operations.account_rate_limit_reset_consume,
            PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUME_LABEL,
            PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUMED_LABEL,
        ),
        (
            "account_usage_read",
            &operations.account_usage_read,
            PROVIDER_ACCOUNT_USAGE_READ_LABEL,
            PROVIDER_ACCOUNT_USAGE_LABEL,
        ),
        (
            "account_workspace_messages_read",
            &operations.account_workspace_messages_read,
            PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_READ_LABEL,
            PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_LABEL,
        ),
        (
            "capabilities_read",
            &operations.capabilities_read,
            PROVIDER_CAPABILITIES_READ_LABEL,
            PROVIDER_CAPABILITIES_LABEL,
        ),
    ];
    if let Some(binding) = operations.thread_compact_start.as_ref() {
        bindings.push((
            "thread_compact_start",
            binding,
            PROVIDER_THREAD_COMPACT_START_LABEL,
            TURN_CONTEXT_COMPACTION_LABEL,
        ));
    }
    if let Some(binding) = operations.account_sessions_read.as_ref() {
        bindings.push((
            "account_sessions_read",
            binding,
            PROVIDER_ACCOUNT_SESSIONS_READ_LABEL,
            PROVIDER_ACCOUNT_SESSIONS_LABEL,
        ));
    }
    let mut labels = BTreeSet::new();
    for (name, binding, request_label, output_label) in bindings {
        if binding.request_label != request_label || binding.output_label != output_label {
            return Err(InterfaceDriverError::Protocol(format!(
                "codex_app_server provider_operations.{name} must declare request '{request_label}' and output '{output_label}'"
            )));
        }
        if !labels.insert(binding.request_label.clone())
            || !labels.insert(binding.output_label.clone())
        {
            return Err(InterfaceDriverError::Protocol(
                "codex_app_server provider operation labels must be unique".to_string(),
            ));
        }
    }
    let event_labels = [
        (
            operations.error_output_label.as_str(),
            PROVIDER_OPERATION_ERROR_LABEL,
            "error_output_label",
        ),
        (
            operations.account_login_completed_output_label.as_str(),
            PROVIDER_ACCOUNT_LOGIN_COMPLETED_LABEL,
            "account_login_completed_output_label",
        ),
        (
            operations.account_updated_output_label.as_str(),
            PROVIDER_ACCOUNT_UPDATED_LABEL,
            "account_updated_output_label",
        ),
        (
            operations.account_rate_limits_updated_output_label.as_str(),
            PROVIDER_ACCOUNT_RATE_LIMITS_UPDATED_LABEL,
            "account_rate_limits_updated_output_label",
        ),
    ];
    for (actual, expected, field) in event_labels {
        if actual != expected {
            return Err(InterfaceDriverError::Protocol(format!(
                "codex_app_server provider_operations.{field} must be '{expected}'"
            )));
        }
        if !labels.insert(actual.to_string()) {
            return Err(InterfaceDriverError::Protocol(
                "codex_app_server provider operation labels must be unique".to_string(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn is_provider_operation_label(label: &str) -> bool {
    label.starts_with("provider.account.")
        || label.starts_with("provider.models.")
        || label.starts_with("provider.capabilities")
        || label == PROVIDER_THREAD_COMPACT_START_LABEL
}

pub(crate) fn is_undeclared_provider_operation(label: &str, thread_compact_requested: bool) -> bool {
    !thread_compact_requested
        && label != PROVIDER_MODELS_LIST_LABEL
        && is_provider_operation_label(label)
}

pub(crate) fn provider_operation_error(
    operation: &str,
    code: &'static str,
    category: &'static str,
    retryable: bool,
    detail: &'static str,
) -> CodexTurnOutput {
    CodexTurnOutput::client(
        PROVIDER_OPERATION_ERROR_LABEL,
        serde_json::json!({
            "operation": operation,
            "status": "error",
            "error": {
                "code": code,
                "category": category,
                "retryable": retryable,
                "detail": detail,
                "redacted": true,
            },
        }),
    )
}

pub(crate) fn upstream_operation_error(
    operation: &'static str,
    error: &TypedRequestError,
    detail: &'static str,
) -> CodexTurnOutput {
    let (code, category, retryable) = match error {
        TypedRequestError::Transport { .. } => ("upstream_transport_failed", "upstream", true),
        TypedRequestError::Server { .. } => ("upstream_request_rejected", "upstream", false),
        TypedRequestError::Deserialize { .. } => ("upstream_response_malformed", "upstream", false),
    };
    provider_operation_error(operation, code, category, retryable, detail)
}

pub(crate) fn scoped_secret_operation_error(error: ScopedSecretError) -> CodexTurnOutput {
    let (code, detail) = match error {
        ScopedSecretError::InvalidHandle(_) => (
            "secret_handle_invalid",
            "API-key login requires secret://<exact-interface>/<name>",
        ),
        ScopedSecretError::ScopeMismatch => (
            "secret_scope_mismatch",
            "API-key secret handle must use the exact provider interface scope",
        ),
        ScopedSecretError::Unavailable => (
            "secret_store_unavailable",
            "the exact-scoped secret store could not be read",
        ),
        ScopedSecretError::Missing => (
            "secret_not_configured",
            "the exact-scoped API-key secret is not configured",
        ),
        ScopedSecretError::Empty => ("secret_empty", "the exact-scoped API-key secret is empty"),
    };
    provider_operation_error(
        PROVIDER_ACCOUNT_LOGIN_START_LABEL,
        code,
        "secret",
        false,
        detail,
    )
}

pub(crate) fn parse_login_id_request(
    payload: &[u8],
    operation: &'static str,
    sink: &TurnSink<'_>,
) -> Result<Option<AccountLoginIdRequest>, InterfaceDriverError> {
    let request: AccountLoginIdRequest = match serde_json::from_slice(payload) {
        Ok(request) => request,
        Err(_) => {
            sink.send(provider_operation_error(
                operation,
                "operation_payload_malformed",
                "protocol",
                false,
                "login operation payload must contain only a login_id string",
            ))?;
            return Ok(None);
        }
    };
    if request.login_id.is_empty() || request.login_id.trim() != request.login_id {
        sink.send(provider_operation_error(
            operation,
            "login_id_invalid",
            "protocol",
            false,
            "login_id must be a non-empty trimmed string",
        ))?;
        return Ok(None);
    }
    Ok(Some(request))
}

pub(crate) fn parse_empty_operation(
    payload: &[u8],
    operation: &'static str,
    sink: &TurnSink<'_>,
) -> Result<bool, InterfaceDriverError> {
    if serde_json::from_slice::<EmptyProviderOperationRequest>(payload).is_err() {
        sink.send(provider_operation_error(
            operation,
            "operation_payload_malformed",
            "protocol",
            false,
            "operation payload must be an empty JSON object",
        ))?;
        return Ok(false);
    }
    Ok(true)
}

pub(crate) fn account_login_completed_output(
    notification: &AccountLoginCompletedNotification,
) -> CodexTurnOutput {
    CodexTurnOutput::client(
        PROVIDER_ACCOUNT_LOGIN_COMPLETED_LABEL,
        serde_json::json!({
            "login_id": notification.login_id,
            "status": if notification.success { "completed" } else { "failed" },
            "success": notification.success,
            "failure": (!notification.success).then(|| serde_json::json!({
                "code": "upstream_login_failed",
                "redacted": true,
            })),
        }),
    )
}

pub(crate) fn account_updated_output(
    notification: &AccountUpdatedNotification,
) -> Result<CodexTurnOutput, InterfaceDriverError> {
    serialized_client_output(PROVIDER_ACCOUNT_UPDATED_LABEL, notification)
}

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelCatalogOperation {
    pub(crate) request_label: String,
    pub(crate) output_label: String,
    pub(crate) default_limit: u32,
    pub(crate) max_limit: u32,
    pub(crate) include_hidden: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelCatalogArtifactRequest {
    #[serde(default)]
    pub(crate) cursor: Option<String>,
    #[serde(default)]
    pub(crate) limit: Option<u32>,
}
