use std::path::PathBuf;

use octopus_login::{AuthCredentialsStoreMode, AuthManager};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::Deserialize;

#[derive(Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthConfig {
    CodexMachine { auth_file_env: String },
    BearerEnv { env_key: String },
}

#[derive(Clone)]
pub struct Credential {
    pub headers: HeaderMap,
    pub owner: String,
    secrets: Vec<String>,
    revision: Option<(u64, std::time::SystemTime)>,
}

impl Credential {
    pub fn contains_secret(&self, bytes: &[u8]) -> bool {
        self.secrets.iter().any(|value| {
            !value.is_empty()
                && bytes
                    .windows(value.len())
                    .any(|part| part == value.as_bytes())
        })
    }
}

impl AuthConfig {
    fn revision(&self) -> Result<Option<(u64, std::time::SystemTime)>, String> {
        match self {
            Self::BearerEnv { .. } => Ok(None),
            Self::CodexMachine { auth_file_env } => {
                let path = std::env::var_os(auth_file_env)
                    .ok_or("machine Codex auth file is unavailable")?;
                let metadata = std::fs::symlink_metadata(path)
                    .map_err(|_| "machine Codex auth file is unavailable")?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() > 1_048_576
                {
                    return Err("machine Codex auth file is not a bounded regular file".into());
                }
                Ok(Some((
                    metadata.len(),
                    metadata
                        .modified()
                        .map_err(|_| "machine Codex auth modification time is unavailable")?,
                )))
            }
        }
    }

    pub fn changed(&self, credential: &Credential) -> Result<bool, String> {
        Ok(self.revision()? != credential.revision)
    }

    pub async fn resolve(&self) -> Result<Credential, String> {
        let (bearer, account, owner) = match self {
            Self::CodexMachine { auth_file_env } => {
                if auth_file_env != "OCTOPUS_MACHINE_CODEX_AUTH_FILE" {
                    return Err(
                        "Codex auth requires declared OCTOPUS_MACHINE_CODEX_AUTH_FILE".into(),
                    );
                }
                let path = std::env::var_os(auth_file_env)
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .ok_or("machine Codex auth file is not configured")?;
                if !path.is_absolute() || path.file_name().is_none_or(|name| name != "auth.json") {
                    return Err("machine Codex auth must name an absolute auth.json".into());
                }
                let metadata = std::fs::symlink_metadata(&path)
                    .map_err(|_| "machine Codex auth file is unavailable")?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() > 1_048_576
                {
                    return Err("machine Codex auth file is not a bounded regular file".into());
                }
                let manager = AuthManager::new(
                    path.parent()
                        .ok_or("auth file has no parent")?
                        .to_path_buf(),
                    false,
                    AuthCredentialsStoreMode::File,
                )
                .await;
                let Some(auth) = manager.auth().await else {
                    return Err(match manager.auth_store_failure() {
                        Some(reason) => format!("Codex auth store is unavailable: {reason}"),
                        None => {
                            "Codex authentication is unavailable; sign in through the Codex provider"
                                .into()
                        }
                    });
                };
                if !auth.is_chatgpt_auth() {
                    return Err("codex_machine requires ChatGPT auth; public API authorization must be configured explicitly".into());
                }
                if manager.refresh_failure_for_auth(&auth).is_some() {
                    return Err(
                        "Codex credential refresh failed; repair authentication explicitly".into(),
                    );
                }
                let token = auth
                    .get_token()
                    .map_err(|_| "Codex access token is unavailable")?;
                let account = auth
                    .get_account_id()
                    .ok_or("Codex ChatGPT account identity is unavailable")?;
                let owner = blake3::hash(account.as_bytes()).to_hex().to_string();
                (token, Some(account), owner)
            }
            Self::BearerEnv { env_key } => {
                super::validate_identity(env_key, "auth environment key")?;
                let token = std::env::var(env_key)
                    .map_err(|_| "declared realtime bearer is unavailable")?;
                let owner = blake3::hash(token.as_bytes()).to_hex().to_string();
                (token, None, owner)
            }
        };
        if bearer.trim().is_empty() {
            return Err("realtime credential is empty".into());
        }
        let mut headers = HeaderMap::new();
        let mut value = HeaderValue::from_str(&format!("Bearer {bearer}"))
            .map_err(|_| "invalid realtime bearer header")?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
        let mut secrets = vec![bearer];
        if let Some(account) = account {
            let mut value =
                HeaderValue::from_str(&account).map_err(|_| "invalid ChatGPT account header")?;
            value.set_sensitive(true);
            headers.insert("ChatGPT-Account-Id", value);
            secrets.push(account);
        }
        Ok(Credential {
            headers,
            owner,
            secrets,
            revision: self.revision()?,
        })
    }
}
