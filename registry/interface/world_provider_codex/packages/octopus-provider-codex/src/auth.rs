use std::path::PathBuf;

use octopus_descriptors::InterfaceId;
use octopus_secrets::{SecretName, SecretScope, SecretsBackendKind, SecretsManager};

#[derive(Debug)]
pub(crate) enum ScopedSecretError {
    InvalidHandle(String),
    ScopeMismatch,
    Unavailable,
    Missing,
    Empty,
}

impl std::fmt::Display for ScopedSecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHandle(reason) => write!(f, "invalid scoped secret handle: {reason}"),
            Self::ScopeMismatch => write!(
                f,
                "scoped secret handle does not use the exact provider interface scope"
            ),
            Self::Unavailable => write!(f, "scoped secret store is unavailable"),
            Self::Missing => write!(f, "scoped secret is not configured"),
            Self::Empty => write!(f, "scoped secret is empty"),
        }
    }
}

impl std::error::Error for ScopedSecretError {}

pub(crate) struct ScopedSecretValue(String);

impl ScopedSecretValue {
    #[cfg(test)]
    pub(crate) fn from_test_value(value: &str) -> Self {
        Self(value.to_string())
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ScopedSecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

pub(crate) trait ScopedSecretStore: Send + Sync {
    fn resolve(
        &self,
        exact_interface: &InterfaceId,
        handle: &str,
    ) -> Result<ScopedSecretValue, ScopedSecretError>;
}

pub(crate) struct OctopusScopedSecretStore {
    manager: SecretsManager,
}

impl OctopusScopedSecretStore {
    pub(crate) fn new(octopus_home: PathBuf) -> Self {
        Self {
            manager: SecretsManager::new(octopus_home, SecretsBackendKind::Local),
        }
    }
}

impl ScopedSecretStore for OctopusScopedSecretStore {
    fn resolve(
        &self,
        exact_interface: &InterfaceId,
        handle: &str,
    ) -> Result<ScopedSecretValue, ScopedSecretError> {
        let (scope, name) = parse_scoped_secret_handle(handle)?;
        if scope.name() != Some(exact_interface.as_str()) {
            return Err(ScopedSecretError::ScopeMismatch);
        }
        let value = self
            .manager
            .get(&scope, &name)
            .map_err(|_| ScopedSecretError::Unavailable)?
            .ok_or(ScopedSecretError::Missing)?;
        if value.trim().is_empty() {
            return Err(ScopedSecretError::Empty);
        }
        Ok(ScopedSecretValue(value))
    }
}

fn parse_scoped_secret_handle(
    handle: &str,
) -> Result<(SecretScope, SecretName), ScopedSecretError> {
    if handle.trim() != handle {
        return Err(ScopedSecretError::InvalidHandle(
            "handle must be trimmed".to_string(),
        ));
    }
    let path = handle.strip_prefix("secret://").ok_or_else(|| {
        ScopedSecretError::InvalidHandle("handle must start with secret://".to_string())
    })?;
    let (scope, name) = path.split_once('/').ok_or_else(|| {
        ScopedSecretError::InvalidHandle(
            "handle must be secret://<exact-interface>/<name>".to_string(),
        )
    })?;
    if name.contains('/') {
        return Err(ScopedSecretError::InvalidHandle(
            "handle must name one scoped secret".to_string(),
        ));
    }
    Ok((
        SecretScope::named(scope.to_string())
            .map_err(|error| ScopedSecretError::InvalidHandle(error.to_string()))?,
        SecretName::new(name)
            .map_err(|error| ScopedSecretError::InvalidHandle(error.to_string()))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_secret_handles_are_exact_and_values_are_debug_redacted() {
        let (scope, name) =
            parse_scoped_secret_handle("secret://world_provider_codex/OPENAI_API_KEY")
                .expect("exact-scoped handle must parse");
        assert_eq!(scope.name(), Some("world_provider_codex"));
        assert_eq!(name.as_str(), "OPENAI_API_KEY");
        for malformed in [
            "OPENAI_API_KEY",
            "secret://world_provider_codex",
            "secret://world_provider_codex/nested/OPENAI_API_KEY",
            " secret://world_provider_codex/OPENAI_API_KEY",
        ] {
            assert!(parse_scoped_secret_handle(malformed).is_err());
        }
        let secret = ScopedSecretValue("ledger-must-never-see-this".to_string());
        assert_eq!(format!("{secret:?}"), "<redacted>");
    }
}
