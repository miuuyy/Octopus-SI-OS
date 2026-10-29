use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::mcp_client::McpClientError;

const MAX_ACCOUNT_FILE_BYTES: u64 = 1024 * 1024;
const ACCESS_TOKEN_EXPIRY_SKEW_SECS: u64 = 30;
const TRANSPORT_OWNED_HEADERS: [&str; 6] = [
    "authorization",
    "content-type",
    "accept",
    "mcp-protocol-version",
    "mcp-session-id",
    "last-event-id",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum AccountAuthSettings {
    ChatgptAccount {
        auth_file_env: String,
        account_header: String,
    },
}

pub(crate) struct AccountCredential {
    pub(crate) headers: Vec<(HeaderName, HeaderValue)>,
    pub(crate) fingerprint: blake3::Hash,
}

#[derive(Deserialize)]
struct AccountDocument {
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default)]
    tokens: Option<AccountTokens>,
}

#[derive(Deserialize)]
struct AccountTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

impl AccountAuthSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let Self::ChatgptAccount { auth_file_env, .. } = self;
        if auth_file_env.is_empty()
            || !auth_file_env
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || auth_file_env.as_bytes()[0].is_ascii_digit()
        {
            return Err(format!(
                "private_config.auth.auth_file_env '{auth_file_env}' is not an environment variable name"
            ));
        }
        self.account_header_name()?;
        Ok(())
    }

    pub(crate) fn owned_header_names(&self) -> Result<Vec<HeaderName>, McpClientError> {
        Ok(vec![
            AUTHORIZATION,
            self.account_header_name().map_err(McpClientError::Config)?,
        ])
    }

    fn account_header_name(&self) -> Result<HeaderName, String> {
        let Self::ChatgptAccount { account_header, .. } = self;
        let name = account_header.parse::<HeaderName>().map_err(|error| {
            format!("private_config.auth.account_header '{account_header}' is invalid: {error}")
        })?;
        if TRANSPORT_OWNED_HEADERS.contains(&name.as_str()) {
            return Err(format!(
                "private_config.auth.account_header '{account_header}' is owned by the transport contract"
            ));
        }
        Ok(name)
    }

    pub(crate) fn read_credential(&self) -> Result<AccountCredential, McpClientError> {
        let Self::ChatgptAccount { auth_file_env, .. } = self;
        let path = std::env::var_os(auth_file_env)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                McpClientError::AccountSignInRequired(format!(
                    "the ChatGPT sign-in location '{auth_file_env}' is not provided to this interface"
                ))
            })?;
        if !path.is_absolute() {
            return Err(McpClientError::AccountCredential(format!(
                "'{auth_file_env}' does not name an absolute file"
            )));
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(McpClientError::AccountSignInRequired(
                    "this machine has no ChatGPT sign-in".to_string(),
                ));
            }
            Err(error) => {
                return Err(McpClientError::AccountCredential(format!(
                    "the ChatGPT sign-in file cannot be inspected: {}",
                    error.kind()
                )));
            }
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_ACCOUNT_FILE_BYTES
        {
            return Err(McpClientError::AccountCredential(
                "the ChatGPT sign-in file is not a bounded regular file".to_string(),
            ));
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            McpClientError::AccountCredential(format!(
                "the ChatGPT sign-in file cannot be read: {}",
                error.kind()
            ))
        })?;
        if bytes.len() as u64 > MAX_ACCOUNT_FILE_BYTES {
            return Err(McpClientError::AccountCredential(
                "the ChatGPT sign-in file grew beyond its bound while it was read".to_string(),
            ));
        }
        let document: AccountDocument = serde_json::from_slice(&bytes).map_err(|_| {
            McpClientError::AccountCredential("the ChatGPT sign-in file is not JSON".to_string())
        })?;
        if document
            .auth_mode
            .as_deref()
            .is_some_and(|mode| mode != "chatgpt")
        {
            return Err(McpClientError::AccountSignInRequired(
                "this machine is signed in without a ChatGPT account".to_string(),
            ));
        }
        let tokens = document.tokens.ok_or_else(|| {
            McpClientError::AccountSignInRequired(
                "this machine has no ChatGPT account tokens".to_string(),
            )
        })?;
        let access_token = nonempty(tokens.access_token).ok_or_else(|| {
            McpClientError::AccountSignInRequired(
                "the ChatGPT sign-in has no access token".to_string(),
            )
        })?;
        let account_id = nonempty(tokens.account_id).ok_or_else(|| {
            McpClientError::AccountSignInRequired(
                "the ChatGPT sign-in has no account identity".to_string(),
            )
        })?;
        let expires_at = access_token_expiry(&access_token)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| McpClientError::AccountCredential("system clock precedes 1970".into()))?
            .as_secs();
        if expires_at <= now.saturating_add(ACCESS_TOKEN_EXPIRY_SKEW_SECS) {
            return Err(McpClientError::AccountSignInExpired);
        }
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {access_token}")).map_err(|_| {
                McpClientError::AccountCredential(
                    "the ChatGPT access token cannot form a header".to_string(),
                )
            })?;
        authorization.set_sensitive(true);
        let mut account = HeaderValue::from_str(&account_id).map_err(|_| {
            McpClientError::AccountCredential(
                "the ChatGPT account identity cannot form a header".to_string(),
            )
        })?;
        account.set_sensitive(true);
        Ok(AccountCredential {
            headers: vec![
                (AUTHORIZATION, authorization),
                (
                    self.account_header_name().map_err(McpClientError::Config)?,
                    account,
                ),
            ],
            fingerprint: blake3::hash(access_token.as_bytes()),
        })
    }

    pub(crate) fn status(&self) -> Value {
        let Self::ChatgptAccount { auth_file_env, .. } = self;
        let state = match self.read_credential() {
            Ok(_) => "signed_in",
            Err(McpClientError::AccountSignInExpired) => "expired",
            Err(McpClientError::AccountSignInRequired(_)) => "signed_out",
            Err(_) => "unreadable",
        };
        json!({
            "mode": "chatgpt_account",
            "auth_file_env": auth_file_env,
            "state": state,
        })
    }
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty() && value.trim() == value)
}

fn access_token_expiry(token: &str) -> Result<u64, McpClientError> {
    let unreadable =
        || McpClientError::AccountCredential("the ChatGPT access token is unreadable".to_string());
    let mut parts = token.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(unreadable());
    };
    let claims: Value = serde_json::from_slice(&base64url_decode(payload).ok_or_else(unreadable)?)
        .map_err(|_| unreadable())?;
    claims.get("exp").and_then(Value::as_u64).ok_or_else(|| {
        McpClientError::AccountCredential("the ChatGPT access token declares no expiry".to_string())
    })
}

fn base64url_decode(value: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in value.bytes() {
        let sextet = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(sextet);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    (bits < 6 && buffer == 0).then_some(output)
}
