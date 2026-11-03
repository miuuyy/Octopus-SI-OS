use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;

use crate::token_data::TokenData;

const AUTH_FILE_NAME: &str = "auth.json";
const AUTH_LOCK_FILE_NAME: &str = "auth.lock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthCredentialsStoreMode {
    File,
    Ephemeral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AuthMode {
    ApiKey,
    Chatgpt,
    #[serde(rename = "chatgptAuthTokens")]
    ChatgptAuthTokens,
    #[serde(rename = "headers")]
    Headers,
    #[serde(rename = "agentIdentity")]
    AgentIdentity,
    #[serde(rename = "personalAccessToken")]
    PersonalAccessToken,
    #[serde(rename = "bedrockApiKey")]
    BedrockApiKey,
    #[serde(rename = "bedrockAccessKeys")]
    BedrockAccessKeys,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub(crate) struct AuthDotJson {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<AuthMode>,

    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenData>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refresh: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_identity: Option<Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personal_access_token: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bedrock_api_key: Option<Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bedrock_access_keys: Option<Value>,

    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl AuthDotJson {
    pub(crate) fn resolved_mode(&self) -> AuthMode {
        if let Some(mode) = self.auth_mode {
            return mode;
        }
        if self.personal_access_token.is_some() {
            return AuthMode::PersonalAccessToken;
        }
        if self.bedrock_api_key.is_some() {
            return AuthMode::BedrockApiKey;
        }
        if self.bedrock_access_keys.is_some() {
            return AuthMode::BedrockAccessKeys;
        }
        if self.openai_api_key.is_some() {
            return AuthMode::ApiKey;
        }
        AuthMode::Chatgpt
    }
}

static AUTH_STORAGE_COMMIT: Mutex<()> = Mutex::new(());

pub(crate) struct AuthStorageLock {
    _file: Option<File>,
    _commit: MutexGuard<'static, ()>,
}

fn auth_storage_commit_guard() -> std::io::Result<MutexGuard<'static, ()>> {
    AUTH_STORAGE_COMMIT
        .lock()
        .map_err(|_| std::io::Error::other("Auth storage commit lock is unavailable."))
}

fn open_lock_file(home: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join(AUTH_LOCK_FILE_NAME))
}

fn store_error(path: &Path, action: &str, error: std::io::Error) -> std::io::Error {
    std::io::Error::new(
        error.kind(),
        format!(
            "Codex auth store {} could not be {action}: {error}",
            path.display()
        ),
    )
}

pub(crate) trait AuthStorageBackend: Debug + Send + Sync {
    fn persistent_home(&self) -> Option<&Path> {
        None
    }

    fn lock_for_write(&self) -> std::io::Result<AuthStorageLock> {
        let commit = auth_storage_commit_guard()?;
        let file = match self.persistent_home() {
            Some(home) => {
                std::fs::create_dir_all(home)?;
                let file = open_lock_file(home)
                    .and_then(|file| file.lock().map(|()| file))
                    .map_err(|error| {
                        store_error(&home.join(AUTH_LOCK_FILE_NAME), "locked", error)
                    })?;
                Some(file)
            }
            None => None,
        };
        Ok(AuthStorageLock {
            _file: file,
            _commit: commit,
        })
    }

    fn load(&self) -> std::io::Result<Option<AuthDotJson>>;
    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()>;
    fn delete(&self) -> std::io::Result<bool>;
}

#[derive(Clone, Debug)]
pub(crate) struct FileAuthStorage {
    codex_home: PathBuf,
}

impl FileAuthStorage {
    pub(crate) fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }
}

impl AuthStorageBackend for FileAuthStorage {
    fn persistent_home(&self) -> Option<&Path> {
        Some(&self.codex_home)
    }

    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let path = self.codex_home.join(AUTH_FILE_NAME);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(store_error(&path, "read", err)),
        };
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|error| store_error(&path, "read", error))?;
        serde_json::from_str(&contents).map(Some).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Codex auth store {} could not be parsed: {:?} error at line {} column {}",
                    path.display(),
                    error.classify(),
                    error.line(),
                    error.column()
                ),
            )
        })
    }

    fn save(&self, auth_dot_json: &AuthDotJson) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.codex_home)?;
        let json_data = serde_json::to_string_pretty(auth_dot_json)?;
        let mut options = OpenOptions::new();
        options.truncate(true).write(true).create(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(self.codex_home.join(AUTH_FILE_NAME))?;
        file.write_all(json_data.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    fn delete(&self) -> std::io::Result<bool> {
        match std::fs::remove_file(self.codex_home.join(AUTH_FILE_NAME)) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        }
    }
}

static EPHEMERAL_AUTH_STORE: LazyLock<Mutex<HashMap<PathBuf, AuthDotJson>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
struct EphemeralAuthStorage {
    codex_home: PathBuf,
}

impl EphemeralAuthStorage {
    fn with_store<F, T>(&self, action: F) -> std::io::Result<T>
    where
        F: FnOnce(&mut HashMap<PathBuf, AuthDotJson>, PathBuf) -> T,
    {
        let key = self
            .codex_home
            .canonicalize()
            .unwrap_or_else(|_| self.codex_home.clone());
        let mut store = EPHEMERAL_AUTH_STORE
            .lock()
            .map_err(|_| std::io::Error::other("failed to lock ephemeral auth storage"))?;
        Ok(action(&mut store, key))
    }
}

impl AuthStorageBackend for EphemeralAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        self.with_store(|store, key| store.get(&key).cloned())
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        self.with_store(|store, key| {
            store.insert(key, auth.clone());
        })
    }

    fn delete(&self) -> std::io::Result<bool> {
        self.with_store(|store, key| store.remove(&key).is_some())
    }
}

pub(crate) fn create_auth_storage(
    codex_home: PathBuf,
    mode: AuthCredentialsStoreMode,
) -> Arc<dyn AuthStorageBackend> {
    match mode {
        AuthCredentialsStoreMode::File => Arc::new(FileAuthStorage::new(codex_home)),
        AuthCredentialsStoreMode::Ephemeral => Arc::new(EphemeralAuthStorage { codex_home }),
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
