use chrono::Utc;
use reqwest::StatusCode;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use tokio::sync::Semaphore;
use tokio::sync::watch;

use crate::auth::default_client::try_create_client;
use crate::auth::storage::AuthCredentialsStoreMode;
use crate::auth::storage::AuthDotJson;
use crate::auth::storage::AuthMode;
use crate::auth::storage::AuthStorageBackend;
use crate::auth::storage::create_auth_storage;
use crate::token_data::AccountOAuthProvenance;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;
use crate::token_data::parse_jwt_expiration;
use octopus_protocol::auth::RefreshTokenFailedError;
use octopus_protocol::auth::RefreshTokenFailedReason;

const TOKEN_REFRESH_INTERVAL_DAYS: i64 = 8;
const CHATGPT_ACCESS_TOKEN_REFRESH_WINDOW_MINUTES: i64 = 5;

const REFRESH_TOKEN_EXPIRED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token has expired. Please log out and sign in again.";
const REFRESH_TOKEN_REUSED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.";
const REFRESH_TOKEN_INVALIDATED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.";
const REFRESH_TOKEN_UNKNOWN_MESSAGE: &str =
    "Your access token could not be refreshed. Please log out and sign in again.";
const REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE: &str = "Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.";
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
const CLIENT_ID_OVERRIDE_ENV_VAR: &str = "CODEX_APP_SERVER_LOGIN_CLIENT_ID";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_API_KEY_ENV_VAR: &str = "CODEX_API_KEY";

#[derive(Debug, Clone)]
pub enum CodexAuth {
    ApiKey(ApiKeyAuth),
    Chatgpt(ChatgptAuth),
}

#[derive(Debug, Clone)]
pub struct ApiKeyAuth {
    api_key: String,
}

#[derive(Debug, Clone)]
pub struct ChatgptAuth {
    auth_dot_json: Arc<AuthDotJson>,
    storage: Arc<dyn AuthStorageBackend>,
}

impl CodexAuth {
    fn from_auth_dot_json(
        auth_dot_json: AuthDotJson,
        storage: Arc<dyn AuthStorageBackend>,
    ) -> std::io::Result<Self> {
        match auth_dot_json.resolved_mode() {
            AuthMode::ApiKey => {
                let api_key = auth_dot_json
                    .openai_api_key
                    .as_deref()
                    .ok_or_else(|| std::io::Error::other("API key auth is missing a key."))?;
                Ok(Self::from_api_key(api_key))
            }
            AuthMode::Chatgpt => Ok(Self::Chatgpt(ChatgptAuth {
                auth_dot_json: Arc::new(auth_dot_json),
                storage,
            })),
            mode => Err(std::io::Error::other(format!(
                "stored auth mode {mode:?} is not a provider transport credential"
            ))),
        }
    }

    pub fn from_api_key(api_key: &str) -> Self {
        Self::ApiKey(ApiKeyAuth {
            api_key: api_key.to_owned(),
        })
    }

    pub fn is_chatgpt_auth(&self) -> bool {
        matches!(self, Self::Chatgpt(_))
    }

    pub fn get_token(&self) -> Result<String, std::io::Error> {
        match self {
            Self::ApiKey(auth) => Ok(auth.api_key.clone()),
            Self::Chatgpt(auth) => auth
                .refreshed_tokens()
                .map(|tokens| tokens.access_token.clone())
                .ok_or_else(|| std::io::Error::other("Token data is not available.")),
        }
    }

    pub fn get_account_id(&self) -> Option<String> {
        match self {
            Self::ApiKey(_) => None,
            Self::Chatgpt(auth) => auth
                .auth_dot_json
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.account_id.clone()),
        }
    }
}

impl ChatgptAuth {
    fn refreshed_tokens(&self) -> Option<&TokenData> {
        self.auth_dot_json
            .last_refresh
            .and(self.auth_dot_json.tokens.as_ref())
    }
}

fn same_credentials(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(CodexAuth::ApiKey(a)), Some(CodexAuth::ApiKey(b))) => a.api_key == b.api_key,
        (Some(CodexAuth::Chatgpt(a)), Some(CodexAuth::Chatgpt(b))) => {
            a.auth_dot_json == b.auth_dot_json
        }
        _ => false,
    }
}

#[derive(Debug)]
enum RefreshTokenError {
    Permanent(RefreshTokenFailedError),
    Transient(std::io::Error),
}

impl std::fmt::Display for RefreshTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Permanent(error) => f.write_str(&error.message),
            Self::Transient(error) => write!(f, "{error}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RefreshReason {
    ProactiveExpiry,
    Scheduled,
}

impl RefreshReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProactiveExpiry => "proactive_expiry",
            Self::Scheduled => "scheduled",
        }
    }
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    client_id: &'a str,
    grant_type: &'static str,
    refresh_token: &'a str,
    refresh_reason: &'static str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

fn oauth_client_id() -> String {
    std::env::var(CLIENT_ID_OVERRIDE_ENV_VAR)
        .ok()
        .filter(|client_id| !client_id.trim().is_empty())
        .unwrap_or_else(|| CLIENT_ID.to_string())
}

fn refresh_token_endpoint(provenance: Option<&AccountOAuthProvenance>) -> String {
    std::env::var(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR).unwrap_or_else(|_| {
        provenance
            .map(|provenance| format!("{}/oauth/token", provenance.issuer.trim_end_matches('/')))
            .unwrap_or_else(|| REFRESH_TOKEN_URL.to_string())
    })
}

fn read_codex_api_key_from_env() -> Option<String> {
    std::env::var(CODEX_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn request_chatgpt_token_refresh(
    tokens: &TokenData,
    endpoint: &str,
    client_id: &str,
    reason: RefreshReason,
) -> Result<RefreshResponse, RefreshTokenError> {
    let client = try_create_client().map_err(|error| {
        RefreshTokenError::Transient(std::io::Error::other(format!(
            "token refresh client could not be built: {error}"
        )))
    })?;
    let response = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .json(&RefreshRequest {
            client_id,
            grant_type: "refresh_token",
            refresh_token: &tokens.refresh_token,
            refresh_reason: reason.as_str(),
        })
        .send()
        .await
        .map_err(|error| {
            RefreshTokenError::Transient(std::io::Error::other(error.without_url()))
        })?;
    let status = response.status();
    if status.is_success() {
        return response.json::<RefreshResponse>().await.map_err(|_| {
            RefreshTokenError::Transient(std::io::Error::other("OAuth token response is invalid"))
        });
    }
    tracing::error!(%status, "Failed to refresh token");
    let body = response.text().await.unwrap_or_default();
    let code = refresh_error_code(&body);
    let is_invalid_grant_bad_request = status == StatusCode::BAD_REQUEST
        && code
            .as_deref()
            .is_some_and(|code| code.eq_ignore_ascii_case("invalid_grant"));
    let failed = classify_refresh_token_failure(code.as_deref(), is_invalid_grant_bad_request);
    if status == StatusCode::UNAUTHORIZED
        || failed.reason != RefreshTokenFailedReason::Other
        || is_invalid_grant_bad_request
    {
        Err(RefreshTokenError::Permanent(failed))
    } else {
        Err(RefreshTokenError::Transient(std::io::Error::other(
            format!("Failed to refresh token: {status}"),
        )))
    }
}

fn refresh_error_code(body: &str) -> Option<String> {
    let json = serde_json::from_str::<Value>(body.trim()).ok()?;
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    };
    text(json.get("error"))
        .or_else(|| text(json.get("error").and_then(|error| error.get("code"))))
        .or_else(|| text(json.get("code")))
}

fn classify_refresh_token_failure(
    code: Option<&str>,
    is_invalid_grant_bad_request: bool,
) -> RefreshTokenFailedError {
    let reason = match code.map(str::to_ascii_lowercase).as_deref() {
        Some("refresh_token_expired") => RefreshTokenFailedReason::Expired,
        Some("refresh_token_reused") => RefreshTokenFailedReason::Exhausted,
        Some("refresh_token_invalidated") => RefreshTokenFailedReason::Revoked,
        _ => RefreshTokenFailedReason::Other,
    };
    if reason == RefreshTokenFailedReason::Other && !is_invalid_grant_bad_request {
        tracing::warn!("Encountered unknown response while refreshing token");
    }
    let message = match reason {
        RefreshTokenFailedReason::Expired => REFRESH_TOKEN_EXPIRED_MESSAGE,
        RefreshTokenFailedReason::Exhausted => REFRESH_TOKEN_REUSED_MESSAGE,
        RefreshTokenFailedReason::Revoked => REFRESH_TOKEN_INVALIDATED_MESSAGE,
        RefreshTokenFailedReason::Other => REFRESH_TOKEN_UNKNOWN_MESSAGE,
    };
    RefreshTokenFailedError::new(reason, message)
}

fn same_account_login(a: &TokenData, b: &TokenData) -> bool {
    a.login_owner_id == b.login_owner_id
        && a.account_id == b.account_id
        && a.id_token.chatgpt_user_id == b.id_token.chatgpt_user_id
}

fn same_login_for_persistence(expected: &TokenData, stored: &TokenData) -> bool {
    same_account_login(expected, stored)
        || (expected.login_owner_id.is_some()
            && stored.login_owner_id.is_none()
            && stored.account_id == expected.account_id
            && stored.id_token.chatgpt_user_id == expected.id_token.chatgpt_user_id
            && !stored.refresh_token.is_empty()
            && stored.refresh_token == expected.refresh_token)
}

fn persist_tokens(
    storage: &Arc<dyn AuthStorageBackend>,
    expected: &TokenData,
    provenance: Option<AccountOAuthProvenance>,
    refreshed: RefreshResponse,
) -> std::io::Result<()> {
    let _lock = storage.lock_for_write()?;
    let mut auth_dot_json = storage
        .load()?
        .ok_or_else(|| std::io::Error::other("Token data is not available."))?;
    if auth_dot_json.resolved_mode() != AuthMode::Chatgpt
        || !auth_dot_json
            .tokens
            .as_ref()
            .is_some_and(|tokens| same_login_for_persistence(expected, tokens))
    {
        return Err(std::io::Error::other(
            "Account credentials changed during refresh.",
        ));
    }
    let tokens = auth_dot_json
        .tokens
        .as_mut()
        .ok_or_else(|| std::io::Error::other("Token data is not available."))?;
    tokens.oauth_provenance = provenance.filter(|_| {
        refreshed.refresh_token.is_some() || tokens.refresh_token == expected.refresh_token
    });
    if let Some(id_token) = refreshed.id_token {
        tokens.id_token = parse_chatgpt_jwt_claims(&id_token).map_err(std::io::Error::other)?;
    }
    if let Some(access_token) = refreshed.access_token {
        tokens.access_token = access_token;
    }
    if let Some(refresh_token) = refreshed.refresh_token {
        tokens.refresh_token = refresh_token;
    }
    auth_dot_json.last_refresh = Some(Utc::now());
    storage.save(&auth_dot_json)
}

struct StoredAuth {
    auth: Option<CodexAuth>,
    unusable: Option<String>,
}

fn load_auth(
    codex_home: &Path,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<StoredAuth> {
    if enable_codex_api_key_env && let Some(api_key) = read_codex_api_key_from_env() {
        return Ok(StoredAuth {
            auth: Some(CodexAuth::from_api_key(&api_key)),
            unusable: None,
        });
    }
    let storage = create_auth_storage(codex_home.to_path_buf(), auth_credentials_store_mode);
    let Some(auth_dot_json) = storage.load()? else {
        return Ok(StoredAuth {
            auth: None,
            unusable: None,
        });
    };
    let stored = match CodexAuth::from_auth_dot_json(auth_dot_json, storage) {
        Ok(auth) => StoredAuth {
            auth: Some(auth),
            unusable: None,
        },
        Err(error) => {
            tracing::warn!("Stored auth is not usable: {error}");
            StoredAuth {
                auth: None,
                unusable: Some(error.to_string()),
            }
        }
    };
    Ok(stored)
}

struct CachedAuth {
    auth: Option<CodexAuth>,
    store_failure: Option<String>,
    permanent_refresh_failure: Option<AuthScopedRefreshFailure>,
    generation: u64,
}

struct AuthScopedRefreshFailure {
    auth: CodexAuth,
    error: RefreshTokenFailedError,
}

enum ReloadOutcome {
    ReloadedChanged,
    ReloadedNoChange,
    Skipped,
    Unreadable(std::io::Error),
}

pub struct AuthManager {
    codex_home: PathBuf,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    inner: RwLock<CachedAuth>,
    auth_change_tx: watch::Sender<u64>,
    refresh_lock: Semaphore,
}

impl Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManager")
            .field("codex_home", &self.codex_home)
            .field("enable_codex_api_key_env", &self.enable_codex_api_key_env)
            .field(
                "auth_credentials_store_mode",
                &self.auth_credentials_store_mode,
            )
            .finish_non_exhaustive()
    }
}

impl AuthManager {
    pub async fn new(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Self {
        let (auth, store_failure) = match load_auth(
            &codex_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
        ) {
            Ok(stored) => (stored.auth, stored.unusable),
            Err(error) => {
                tracing::error!(kind = ?error.kind(), "Failed to load auth: {error}");
                (None, Some(error.to_string()))
            }
        };
        Self::with_cached(
            codex_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            auth,
            store_failure,
        )
    }

    pub async fn shared(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> Arc<Self> {
        Arc::new(
            Self::new(
                codex_home,
                enable_codex_api_key_env,
                auth_credentials_store_mode,
            )
            .await,
        )
    }

    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        Arc::new(Self::with_cached(
            PathBuf::from("non-existent"),
            false,
            AuthCredentialsStoreMode::Ephemeral,
            Some(auth),
            None,
        ))
    }

    fn with_cached(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        auth: Option<CodexAuth>,
        store_failure: Option<String>,
    ) -> Self {
        Self {
            codex_home,
            enable_codex_api_key_env,
            auth_credentials_store_mode,
            inner: RwLock::new(CachedAuth {
                auth,
                store_failure,
                permanent_refresh_failure: None,
                generation: 0,
            }),
            auth_change_tx: watch::channel(0).0,
            refresh_lock: Semaphore::new(1),
        }
    }

    pub fn auth_change_receiver(&self) -> watch::Receiver<u64> {
        self.auth_change_tx.subscribe()
    }

    pub fn auth_store_failure(&self) -> Option<String> {
        self.inner
            .read()
            .ok()
            .and_then(|cached| cached.store_failure.clone())
    }

    pub fn refresh_failure_for_auth(&self, auth: &CodexAuth) -> Option<RefreshTokenFailedError> {
        self.inner.read().ok().and_then(|cached| {
            cached
                .permanent_refresh_failure
                .as_ref()
                .filter(|failure| same_credentials(Some(auth), Some(&failure.auth)))
                .map(|failure| failure.error.clone())
        })
    }

    pub async fn auth(&self) -> Option<CodexAuth> {
        let auth = self.auth_cached()?;
        if let Some(reason) = Self::proactive_refresh_reason(&auth)
            && let Err(error) = self.refresh_if_unchanged(reason, &auth).await
        {
            tracing::error!("Failed to refresh token: {error}");
            return Some(auth);
        }
        self.auth_cached()
    }

    pub async fn reload(&self) -> bool {
        self.reload_from_store().unwrap_or(false)
    }

    pub async fn logout(&self) -> std::io::Result<bool> {
        let removed = if self.auth_credentials_store_mode == AuthCredentialsStoreMode::File
            && !self.codex_home.try_exists()?
        {
            false
        } else {
            let storage = self.storage();
            let _lock = storage.lock_for_write()?;
            storage.delete()?
        };
        self.invalidate_pending_auth_reads();
        self.reload_from_store()?;
        Ok(removed)
    }

    fn reload_from_store(&self) -> std::io::Result<bool> {
        loop {
            let generation = self.generation();
            let stored = self.load_auth_recording_failure()?;
            if let Some(changed) = self.publish_reloaded_auth(generation, stored) {
                return Ok(changed);
            }
        }
    }

    fn storage(&self) -> Arc<dyn AuthStorageBackend> {
        create_auth_storage(self.codex_home.clone(), self.auth_credentials_store_mode)
    }

    fn auth_cached(&self) -> Option<CodexAuth> {
        self.inner
            .read()
            .ok()
            .and_then(|cached| cached.auth.clone())
    }

    fn generation(&self) -> u64 {
        self.inner
            .read()
            .map(|cached| cached.generation)
            .unwrap_or_default()
    }

    fn load_auth_recording_failure(&self) -> std::io::Result<StoredAuth> {
        load_auth(
            &self.codex_home,
            self.enable_codex_api_key_env,
            self.auth_credentials_store_mode,
        )
        .inspect_err(|error| {
            tracing::error!(kind = ?error.kind(), "Failed to reload auth: {error}");
            if let Ok(mut cached) = self.inner.write() {
                cached.store_failure = Some(error.to_string());
            }
        })
    }

    fn publish_reloaded_auth(&self, generation: u64, stored: StoredAuth) -> Option<bool> {
        let Ok(mut cached) = self.inner.write() else {
            return Some(false);
        };
        if cached.generation != generation {
            return None;
        }
        let changed = !same_credentials(cached.auth.as_ref(), stored.auth.as_ref());
        cached.auth = stored.auth;
        cached.store_failure = stored.unusable;
        if changed {
            cached.permanent_refresh_failure = None;
            cached.generation += 1;
            self.auth_change_tx.send_modify(|revision| *revision += 1);
        }
        tracing::info!("Reloaded auth, changed: {changed}");
        Some(changed)
    }

    fn invalidate_pending_auth_reads(&self) {
        if let Ok(mut cached) = self.inner.write() {
            cached.generation += 1;
        }
    }

    fn reload_if_account_id_matches(&self, expected_account_id: Option<&str>) -> ReloadOutcome {
        let Some(expected_account_id) = expected_account_id else {
            tracing::info!("Skipping auth reload because no account id is available.");
            return ReloadOutcome::Skipped;
        };
        loop {
            let generation = self.generation();
            let stored = match self.load_auth_recording_failure() {
                Ok(stored) => stored,
                Err(error) => return ReloadOutcome::Unreadable(error),
            };
            if stored
                .auth
                .as_ref()
                .and_then(CodexAuth::get_account_id)
                .as_deref()
                != Some(expected_account_id)
            {
                tracing::info!("Skipping auth reload due to account id mismatch.");
                return ReloadOutcome::Skipped;
            }
            match self.publish_reloaded_auth(generation, stored) {
                Some(true) => return ReloadOutcome::ReloadedChanged,
                Some(false) => return ReloadOutcome::ReloadedNoChange,
                None => {}
            }
        }
    }

    fn record_permanent_refresh_failure_if_unchanged(
        &self,
        attempted_auth: &CodexAuth,
        error: &RefreshTokenFailedError,
    ) {
        if let Ok(mut cached) = self.inner.write()
            && same_credentials(Some(attempted_auth), cached.auth.as_ref())
        {
            cached.permanent_refresh_failure = Some(AuthScopedRefreshFailure {
                auth: attempted_auth.clone(),
                error: error.clone(),
            });
        }
    }

    fn proactive_refresh_reason(auth: &CodexAuth) -> Option<RefreshReason> {
        let CodexAuth::Chatgpt(chatgpt_auth) = auth else {
            return None;
        };
        let auth_dot_json = &chatgpt_auth.auth_dot_json;
        if let Some(tokens) = auth_dot_json.tokens.as_ref()
            && let Ok(Some(expires_at)) = parse_jwt_expiration(&tokens.access_token)
        {
            return (expires_at
                <= Utc::now()
                    + chrono::Duration::minutes(CHATGPT_ACCESS_TOKEN_REFRESH_WINDOW_MINUTES))
            .then_some(RefreshReason::ProactiveExpiry);
        }
        let last_refresh = auth_dot_json.last_refresh?;
        (last_refresh < Utc::now() - chrono::Duration::days(TOKEN_REFRESH_INTERVAL_DAYS))
            .then_some(RefreshReason::Scheduled)
    }

    async fn refresh_if_unchanged(
        &self,
        reason: RefreshReason,
        expected: &CodexAuth,
    ) -> Result<(), RefreshTokenError> {
        let _refresh_guard = self.refresh_lock.acquire().await.map_err(|_| {
            RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                REFRESH_TOKEN_UNKNOWN_MESSAGE,
            ))
        })?;
        if !same_credentials(self.auth_cached().as_ref(), Some(expected)) {
            return Ok(());
        }
        match self.reload_if_account_id_matches(expected.get_account_id().as_deref()) {
            ReloadOutcome::ReloadedChanged => {
                tracing::info!("Skipping token refresh because auth changed after guarded reload.");
                Ok(())
            }
            ReloadOutcome::ReloadedNoChange => self.refresh_from_authority(reason).await,
            ReloadOutcome::Skipped => {
                Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                    RefreshTokenFailedReason::Other,
                    REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE,
                )))
            }
            ReloadOutcome::Unreadable(error) => Err(RefreshTokenError::Transient(error)),
        }
    }

    async fn refresh_from_authority(&self, reason: RefreshReason) -> Result<(), RefreshTokenError> {
        let Some(attempted_auth) = self.auth_cached() else {
            return Ok(());
        };
        if let Some(error) = self.refresh_failure_for_auth(&attempted_auth) {
            return Err(RefreshTokenError::Permanent(error));
        }
        let CodexAuth::Chatgpt(chatgpt_auth) = &attempted_auth else {
            return Ok(());
        };
        let result = self.refresh_and_persist(chatgpt_auth, reason).await;
        if let Err(RefreshTokenError::Permanent(error)) = &result {
            self.record_permanent_refresh_failure_if_unchanged(&attempted_auth, error);
        }
        result
    }

    async fn refresh_and_persist(
        &self,
        auth: &ChatgptAuth,
        reason: RefreshReason,
    ) -> Result<(), RefreshTokenError> {
        let tokens = auth.auth_dot_json.tokens.as_ref().ok_or_else(|| {
            RefreshTokenError::Transient(std::io::Error::other("Token data is not available."))
        })?;
        let endpoint = refresh_token_endpoint(tokens.oauth_provenance.as_ref());
        let client_id = tokens
            .oauth_provenance
            .as_ref()
            .map(|provenance| provenance.client_id.clone())
            .unwrap_or_else(oauth_client_id);
        let refreshed =
            request_chatgpt_token_refresh(tokens, &endpoint, &client_id, reason).await?;
        let provenance = tokens.oauth_provenance.clone().filter(|provenance| {
            endpoint == format!("{}/oauth/token", provenance.issuer.trim_end_matches('/'))
        });
        persist_tokens(&auth.storage, tokens, provenance, refreshed)
            .map_err(RefreshTokenError::Transient)?;
        self.reload().await;
        Ok(())
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
