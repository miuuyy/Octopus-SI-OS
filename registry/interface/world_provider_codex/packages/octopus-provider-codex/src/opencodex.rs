use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use octopus_interfaces::{
    InterfaceDescriptor, InterfaceInvocationEnvelope, InterfaceTransportConfig,
};
use octopus_login::AuthManager;
use octopus_protocol::openai_models::ModelsResponse;
use octopus_secrets::{
    LocalFileKeyringStore, SecretName, SecretScope, SecretsBackendKind, SecretsManager,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::time::timeout;
use zeroize::Zeroize;

use crate::config::{AppServerSource, build_curated_opencodex_home};
use crate::handler::CodexAppServerInterfaceDriver;
use crate::{AppServerClient, SpawnAppServerClient};
use octopus_interface_transport::InterfaceLiftContext;

const OPENCODEX_CONFIG_KEY: &str = "opencodex";
const PROVIDER_SECRET_OWNER_ROOT_SLOT: &str = "provider_secret_owner_root";
const REGISTRY_INTERFACE_ROOT_SLOT: &str = "registry_interface_root";
const OPENCODEX_OWNER_INTERFACE: &str = "world_provider_codex";
const BRIDGE_RELATIVE_ROOT: &str = "packages/octopus-provider-codex/opencodex-bridge";
const BRIDGE_EXECUTABLE: &str = "bin/octopus-opencodex-bridge";
const BRIDGE_BUILD_RECEIPT: &str = "build-receipt.json";
const BRIDGE_READY_TIMEOUT: Duration = Duration::from_secs(20);
const BRIDGE_READY_MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OpenCodexAdapter {
    Anthropic,
    Azure,
    ClaudeCli,
    Google,
    GrokCli,
    OpenaiChat,
    OpenaiResponses,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OpenCodexCacheRetention {
    None,
    Short,
    Long,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenCodexProviderOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotate_empty_tool_outputs: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_transport: Option<OpenCodexApiKeyTransport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<OpenCodexAuthMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_tool_mode: Option<OpenCodexToolMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decodes_native_compaction_blobs: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_gemini_wire_renames: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_responses_reasoning_content: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_adjacent_responses_tool_results: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responses_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stateless_responses: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_service_tier: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_http_version: Option<OpenCodexHttpVersion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OpenCodexApiKeyTransport {
    Bearer,
    XApiKey,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OpenCodexAuthMode {
    Key,
    Oauth,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OpenCodexToolMode {
    CodeModeOnly,
    Shell,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) enum OpenCodexHttpVersion {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "h1")]
    H1,
    #[serde(rename = "h2")]
    H2,
    #[serde(rename = "http1.1")]
    Http11,
    #[serde(rename = "http2")]
    Http2,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenCodexLimits {
    #[serde(default = "default_max_request_bytes")]
    pub max_request_bytes: u64,
    #[serde(default = "default_max_translated_call_bytes")]
    pub max_translated_call_bytes: u64,
    #[serde(default = "default_max_translated_turn_bytes")]
    pub max_translated_turn_bytes: u64,
}

impl Default for OpenCodexLimits {
    fn default() -> Self {
        Self {
            max_request_bytes: default_max_request_bytes(),
            max_translated_call_bytes: default_max_translated_call_bytes(),
            max_translated_turn_bytes: default_max_translated_turn_bytes(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenCodexDescriptorConfig {
    pub schema_version: u32,
    pub adapter: OpenCodexAdapter,
    pub credential_owner_interface: String,
    #[serde(default)]
    pub registry_entry: Option<String>,
    #[serde(default)]
    pub model_env: Option<String>,
    #[serde(default)]
    pub bridge_env_names: Vec<String>,
    #[serde(default)]
    pub model_catalog: Option<ModelsResponse>,
    #[serde(default)]
    pub allow_private_network: bool,
    #[serde(default)]
    pub cache_retention: Option<OpenCodexCacheRetention>,
    #[serde(default)]
    pub options: OpenCodexProviderOptions,
    #[serde(default)]
    pub limits: OpenCodexLimits,
}

#[derive(Serialize)]
struct BridgeConfig {
    schema_version: u32,
    interface_id: String,
    provider: BridgeProvider,
    limits: BridgeLimits,
}

#[derive(Serialize)]
struct BridgeProvider {
    id: String,
    adapter: OpenCodexAdapter,
    #[serde(skip_serializing_if = "Option::is_none")]
    registry_entry: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    model_aliases: BTreeMap<String, String>,
    base_url: String,
    models: Vec<String>,
    default_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auth_header: Option<String>,
    auth_value_prefix: String,
    headers: BTreeMap<String, String>,
    allow_private_network: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_retention: Option<OpenCodexCacheRetention>,
    options: OpenCodexProviderOptions,
}

#[derive(Serialize)]
struct BridgeLimits {
    max_request_bytes: u64,
    max_response_bytes: u64,
    max_translated_call_bytes: u64,
    max_translated_turn_bytes: u64,
    request_timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeReady {
    schema_version: u32,
    status: String,
    port: u16,
    interface_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BridgeBuildReceipt {
    schema_version: u32,
    executable: String,
    executable_sha256: String,
    executable_bytes: u64,
    inputs: BTreeMap<String, String>,
    upstream: serde_json::Value,
}

pub(crate) fn descriptor_config(
    descriptor: &InterfaceDescriptor,
) -> Result<Option<OpenCodexDescriptorConfig>, String> {
    let Some(value) = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get(OPENCODEX_CONFIG_KEY))
    else {
        return Ok(None);
    };
    let parsed: OpenCodexDescriptorConfig =
        serde_json::from_value(value.clone()).map_err(|error| {
            format!(
                "interface '{}' private_config.{OPENCODEX_CONFIG_KEY} is malformed: {error}",
                descriptor.id
            )
        })?;
    if parsed.schema_version != 1 {
        return Err(format!(
            "interface '{}' private_config.{OPENCODEX_CONFIG_KEY}.schema_version must be 1",
            descriptor.id
        ));
    }
    validate_interface_segment(
        &parsed.credential_owner_interface,
        "opencodex credential_owner_interface",
    )?;
    if let Some(entry) = parsed.registry_entry.as_deref() {
        validate_interface_segment(entry, "opencodex registry_entry")?;
    }
    if parsed
        .model_env
        .as_deref()
        .is_some_and(|name| !valid_env_name(name))
    {
        return Err("opencodex model_env is not a valid environment variable name".to_string());
    }
    let mut bridge_env = BTreeSet::new();
    for name in &parsed.bridge_env_names {
        if !valid_env_name(name) || !bridge_env.insert(name.as_str()) {
            return Err(
                "opencodex bridge_env_names must be unique valid environment variable names"
                    .to_string(),
            );
        }
    }
    validate_limits(&parsed.limits)?;
    if let Some(catalog) = &parsed.model_catalog {
        if catalog.models.is_empty() {
            return Err("opencodex model_catalog must contain at least one model".to_string());
        }
        let mut slugs = BTreeSet::new();
        for model in &catalog.models {
            if !valid_text_atom(&model.slug) || !slugs.insert(&model.slug) {
                return Err("opencodex model_catalog requires unique non-empty model slugs".to_string());
            }
        }
    }
    if let Some(path) = parsed.options.responses_path.as_deref() {
        if !path.starts_with('/') || path.contains(['?', '#']) || path.contains("://") {
            return Err(
                "opencodex options.responses_path must be an absolute resource path".to_string(),
            );
        }
    }
    Ok(Some(parsed))
}

pub(crate) async fn connect_interface_driver(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    envelope: &InterfaceInvocationEnvelope,
    config: &OpenCodexDescriptorConfig,
    auth_manager: &Arc<AuthManager>,
    provider_home: PathBuf,
    auth_store_home: PathBuf,
) -> Result<Arc<CodexAppServerInterfaceDriver>, String> {
    let driver_config = descriptor.interface_driver_config.as_ref().ok_or_else(|| {
        format!(
            "OpenCodex interface '{}' has no interface_driver_config",
            descriptor.id
        )
    })?;
    let transport = driver_config.transport.as_ref().ok_or_else(|| {
        format!(
            "OpenCodex interface '{}' has no declared transport",
            descriptor.id
        )
    })?;
    let models = formed_models(envelope)?;
    let selected_model = selected_model(descriptor)?;
    if !models.iter().any(|model| model == &selected_model) {
        return Err(format!(
            "OpenCodex selected model '{selected_model}' is outside the freshly formed interface policy models"
        ));
    }
    let model_aliases = resolve_model_aliases(config, &models)?;
    let base_url = resolve_base_url(transport, config.allow_private_network)?;
    validate_static_headers(transport)?;
    let api_key = resolve_api_key(ctx, descriptor, transport, config)?;
    if let Some(name) = transport.auth_env_key.as_deref()
        && config.bridge_env_names.iter().any(|candidate| candidate == name)
    {
        return Err(format!(
            "OpenCodex interface '{}' cannot pass its credential env '{name}' to the bridge environment",
            descriptor.id
        ));
    }
    let auth_header = transport.auth_header.clone();
    let auth_value_prefix = transport.auth_value_prefix.clone().unwrap_or_default();
    match (&api_key, &auth_header, &transport.auth_value_prefix) {
        (Some(_), Some(_), Some(_)) => {}
        (Some(_), _, _) => {
            return Err(format!(
                "OpenCodex interface '{}' authenticated transport requires auth_header and auth_value_prefix",
                descriptor.id
            ));
        }
        (None, None, None) => {}
        (None, _, _) => {
            return Err(format!(
                "OpenCodex interface '{}' unauthenticated transport cannot declare auth_header or auth_value_prefix",
                descriptor.id
            ));
        }
    }
    if let Some(auth_header) = auth_header.as_deref()
        && transport
            .headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case(auth_header))
    {
        return Err(format!(
            "OpenCodex interface '{}' repeats its auth header in static transport headers",
            descriptor.id
        ));
    }
    let maximum_response = transport.max_response_bytes.unwrap_or(64 * 1024 * 1024);
    validate_bounded_integer(
        maximum_response,
        1024,
        256 * 1024 * 1024,
        "transport.max_response_bytes",
    )?;
    let request_timeout = transport.request_timeout_ms.unwrap_or(300_000);
    validate_bounded_integer(
        request_timeout,
        1000,
        1_800_000,
        "transport.request_timeout_ms",
    )?;
    let bridge_config = BridgeConfig {
        schema_version: 1,
        interface_id: descriptor.id.as_str().to_string(),
        provider: BridgeProvider {
            id: descriptor.id.as_str().to_string(),
            adapter: config.adapter.clone(),
            registry_entry: config.registry_entry.clone(),
            model_aliases,
            base_url,
            models,
            default_model: selected_model,
            api_key,
            auth_header,
            auth_value_prefix,
            headers: transport.headers.clone(),
            allow_private_network: config.allow_private_network,
            cache_retention: config.cache_retention.clone(),
            options: config.options.clone(),
        },
        limits: BridgeLimits {
            max_request_bytes: config.limits.max_request_bytes,
            max_response_bytes: maximum_response,
            max_translated_call_bytes: config.limits.max_translated_call_bytes,
            max_translated_turn_bytes: config.limits.max_translated_turn_bytes,
            request_timeout_ms: request_timeout,
        },
    };
    let bridge = resolve_bridge_executable(ctx)?;
    let bridge_cwd = provider_home.join("opencodex-bridge-cwd");
    std::fs::create_dir_all(&bridge_cwd).map_err(|error| {
        format!(
            "OpenCodex interface '{}' bridge cwd '{}' cannot be created: {error}",
            descriptor.id,
            bridge_cwd.display()
        )
    })?;
    let (bridge_child, port) = spawn_bridge(
        &bridge,
        &bridge_cwd,
        bridge_config,
        &config.bridge_env_names,
    )
    .await?;
    let local_base_url = format!("http://127.0.0.1:{port}/v1");
    let codex_home = build_curated_opencodex_home(
        &provider_home,
        &local_base_url,
        config.model_catalog.as_ref(),
    )
    .map_err(|error| error.to_string())?;
    let spawn_config = AppServerSource::from_descriptor(descriptor)
        .map_err(|error| error.to_string())?
        .spawn_config_with_curated_home(codex_home)
        .map_err(|error| error.to_string())?;
    let client = SpawnAppServerClient::connect_with_auxiliary(spawn_config, bridge_child)
        .await
        .map_err(|error| format!("OpenCodex app-server connection failed: {error}"))?;
    Ok(Arc::new(
        CodexAppServerInterfaceDriver::new_with_config_and_auth_boundaries(
            AppServerClient::Spawn(client),
            descriptor.interface_driver_config.as_ref(),
            None,
            auth_manager.clone(),
            auth_store_home,
        ),
    ))
}

fn formed_models(envelope: &InterfaceInvocationEnvelope) -> Result<Vec<String>, String> {
    let policy = envelope
        .interface_policy()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "OpenCodex invocation carries no formed interface policy".to_string())?;
    let values = policy
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            "OpenCodex formed interface policy models must be a string array".to_string()
        })?;
    if values.is_empty() {
        return Err("OpenCodex formed interface policy models is empty".to_string());
    }
    let mut seen = BTreeSet::new();
    let mut models = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let model = value
            .as_str()
            .filter(|model| valid_text_atom(model))
            .ok_or_else(|| {
                format!(
                    "OpenCodex formed interface policy models[{index}] must be a non-empty trimmed string"
                )
            })?;
        if !seen.insert(model.to_string()) {
            return Err(format!(
                "OpenCodex formed interface policy models repeats '{model}'"
            ));
        }
        models.push(model.to_string());
    }
    Ok(models)
}

fn selected_model(descriptor: &InterfaceDescriptor) -> Result<String, String> {
    descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("model"))
        .and_then(serde_json::Value::as_str)
        .filter(|model| valid_text_atom(model))
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "OpenCodex interface '{}' has no valid provider-local model selection",
                descriptor.id
            )
        })
}

fn resolve_model_aliases(
    config: &OpenCodexDescriptorConfig,
    models: &[String],
) -> Result<BTreeMap<String, String>, String> {
    let Some(name) = config.model_env.as_deref() else {
        return Ok(BTreeMap::new());
    };
    let [declared] = models else {
        return Err(format!(
            "OpenCodex model_env '{name}' requires exactly one declared model slug"
        ));
    };
    let upstream = std::env::var(name)
        .ok()
        .filter(|value| valid_text_atom(value))
        .ok_or_else(|| {
            format!(
                "OpenCodex model is not configured: env var '{name}' is unset, empty or untrimmed"
            )
        })?;
    Ok(BTreeMap::from([(declared.clone(), upstream)]))
}

fn resolve_base_url(
    transport: &InterfaceTransportConfig,
    allow_private_network: bool,
) -> Result<String, String> {
    let base_url = match transport
        .base_url_env
        .as_deref()
        .filter(|name| valid_env_name(name))
    {
        Some(name) => std::env::var(name)
            .ok()
            .filter(|value| valid_text_atom(value))
            .ok_or_else(|| {
                format!("OpenCodex base URL is not configured: env var '{name}' is unset, empty or untrimmed")
            })?,
        None if transport.base_url_env.is_some() => {
            return Err("OpenCodex transport.base_url_env is invalid".to_string());
        }
        None => transport.base_url.clone(),
    };
    let url = url::Url::parse(&base_url)
        .map_err(|error| format!("OpenCodex transport.base_url is invalid: {error}"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return Err(
            "OpenCodex transport.base_url cannot contain credentials, query or fragment"
                .to_string(),
        );
    }
    match url.scheme() {
        "https" => {}
        "http" if allow_private_network => {}
        "http" => {
            return Err("OpenCodex HTTP transport requires allow_private_network=true".to_string());
        }
        scheme => {
            return Err(format!(
                "OpenCodex transport.base_url scheme '{scheme}' is unsupported"
            ));
        }
    }
    Ok(base_url)
}

fn validate_static_headers(transport: &InterfaceTransportConfig) -> Result<(), String> {
    for (name, value) in &transport.headers {
        if !valid_header_name(name) || !valid_text_atom(value) {
            return Err(format!("OpenCodex transport header '{name}' is malformed"));
        }
        if secret_header(name) {
            return Err(format!(
                "OpenCodex secret-bearing header '{name}' must come from auth_secret_ref or auth_env_key"
            ));
        }
    }
    Ok(())
}

fn resolve_api_key(
    ctx: &InterfaceLiftContext,
    descriptor: &InterfaceDescriptor,
    transport: &InterfaceTransportConfig,
    config: &OpenCodexDescriptorConfig,
) -> Result<Option<String>, String> {
    let secret_ref = transport
        .auth_secret_ref
        .as_deref()
        .filter(|value| valid_text_atom(value));
    let env_key = transport
        .auth_env_key
        .as_deref()
        .filter(|value| valid_env_name(value));
    if transport.auth_secret_ref.is_some() && secret_ref.is_none() {
        return Err("OpenCodex transport.auth_secret_ref is invalid".to_string());
    }
    if transport.auth_env_key.is_some() && env_key.is_none() {
        return Err("OpenCodex transport.auth_env_key is invalid".to_string());
    }
    match (secret_ref, env_key) {
        (Some(_), Some(_)) => Err(
            "OpenCodex transport cannot declare both auth_secret_ref and auth_env_key".to_string(),
        ),
        (None, Some(name)) => std::env::var(name)
            .ok()
            .filter(|value| valid_text_atom(value))
            .map(Some)
            .ok_or_else(|| format!("OpenCodex API key is not configured: env var '{name}' is unset, empty or untrimmed")),
        (Some(reference), None) => {
            let (scope, name) = parse_secret_ref(reference)?;
            if scope.name() != Some(config.credential_owner_interface.as_str()) {
                return Err(format!(
                    "OpenCodex auth_secret_ref scope must equal credential owner '{}'",
                    config.credential_owner_interface
                ));
            }
            let root_slot = if config.credential_owner_interface == descriptor.id.as_str() {
                "activation_secrets_home"
            } else {
                PROVIDER_SECRET_OWNER_ROOT_SLOT
            };
            let owner_root = ctx.root_slot(root_slot)?;
            let owner_metadata = std::fs::symlink_metadata(owner_root).map_err(|error| {
                format!(
                    "OpenCodex credential owner root '{}' cannot be inspected: {error}",
                    owner_root.display()
                )
            })?;
            if owner_metadata.file_type().is_symlink() || !owner_metadata.is_dir() {
                return Err(format!(
                    "OpenCodex credential owner root '{}' is not a regular directory",
                    owner_root.display()
                ));
            }
            if owner_root.file_name().and_then(|name| name.to_str())
                != Some(config.credential_owner_interface.as_str())
            {
                return Err(format!(
                    "OpenCodex credential owner root '{}' does not name exact interface '{}'",
                    owner_root.display(),
                    config.credential_owner_interface
                ));
            }
            let owner_root = std::fs::canonicalize(owner_root).map_err(|error| {
                format!(
                    "OpenCodex credential owner root '{}' cannot be resolved: {error}",
                    owner_root.display()
                )
            })?;
            let keyring = owner_root.join("secrets").join("keyring");
            let manager = SecretsManager::new_with_keyring_store(
                owner_root,
                SecretsBackendKind::Local,
                Arc::new(LocalFileKeyringStore::new(keyring)),
            );
            let value = manager
                .get(&scope, &name)
                .map_err(|_| "OpenCodex credential store is unavailable".to_string())?
                .filter(|value| valid_text_atom(value))
                .ok_or_else(|| "OpenCodex credential is missing, empty or untrimmed".to_string())?;
            Ok(Some(value))
        }
        (None, None) => Ok(None),
    }
}

fn parse_secret_ref(reference: &str) -> Result<(SecretScope, SecretName), String> {
    let path = reference
        .strip_prefix("secret://")
        .ok_or_else(|| "OpenCodex auth_secret_ref must start with secret://".to_string())?;
    let (scope, name) = path.split_once('/').ok_or_else(|| {
        "OpenCodex auth_secret_ref must be secret://<interface>/<name>".to_string()
    })?;
    if name.contains('/') {
        return Err("OpenCodex auth_secret_ref must name one scoped secret".to_string());
    }
    Ok((
        SecretScope::named(scope.to_string()).map_err(|error| error.to_string())?,
        SecretName::new(name).map_err(|error| error.to_string())?,
    ))
}

fn resolve_bridge_executable(ctx: &InterfaceLiftContext) -> Result<PathBuf, String> {
    let interface_root = ctx.root_slot(REGISTRY_INTERFACE_ROOT_SLOT)?;
    let interface_root = std::fs::canonicalize(interface_root).map_err(|error| {
        format!(
            "OpenCodex interface registry root '{}' cannot be resolved: {error}",
            interface_root.display()
        )
    })?;
    let owner =
        std::fs::canonicalize(interface_root.join(OPENCODEX_OWNER_INTERFACE)).map_err(|error| {
            format!(
                "OpenCodex executable owner '{}' cannot be resolved: {error}",
                interface_root.join(OPENCODEX_OWNER_INTERFACE).display()
            )
        })?;
    if owner.parent() != Some(interface_root.as_path()) {
        return Err("OpenCodex executable owner escaped the interface registry".to_string());
    }
    let bridge_root = std::fs::canonicalize(owner.join(BRIDGE_RELATIVE_ROOT)).map_err(|error| {
        format!(
            "OpenCodex bridge root '{}' cannot be resolved: {error}",
            owner.join(BRIDGE_RELATIVE_ROOT).display()
        )
    })?;
    if !bridge_root.starts_with(&owner) {
        return Err("OpenCodex bridge root escaped its exact interface owner".to_string());
    }
    let executable = bridge_root.join(BRIDGE_EXECUTABLE);
    let executable_metadata = std::fs::symlink_metadata(&executable).map_err(|error| {
        format!(
            "OpenCodex bridge executable '{}' cannot be inspected: {error}",
            executable.display()
        )
    })?;
    if executable_metadata.file_type().is_symlink() || !executable_metadata.is_file() {
        return Err(format!(
            "OpenCodex bridge executable '{}' is not a regular file",
            executable.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if executable_metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!(
                "OpenCodex bridge executable '{}' is not executable",
                executable.display()
            ));
        }
    }
    let executable = std::fs::canonicalize(&executable).map_err(|error| {
        format!(
            "OpenCodex bridge executable '{}' cannot be resolved: {error}",
            executable.display()
        )
    })?;
    if !executable.starts_with(&bridge_root) {
        return Err("OpenCodex bridge executable escaped its package root".to_string());
    }
    verify_bridge_receipt(&bridge_root, &executable, executable_metadata.len())?;
    Ok(executable)
}

fn verify_bridge_receipt(
    bridge_root: &Path,
    executable: &Path,
    executable_bytes: u64,
) -> Result<(), String> {
    let receipt_path = bridge_root.join(BRIDGE_BUILD_RECEIPT);
    let metadata = std::fs::symlink_metadata(&receipt_path).map_err(|error| {
        format!(
            "OpenCodex bridge build receipt '{}' cannot be inspected: {error}",
            receipt_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "OpenCodex bridge build receipt '{}' is not a regular file",
            receipt_path.display()
        ));
    }
    let bytes = std::fs::read(&receipt_path).map_err(|error| {
        format!(
            "OpenCodex bridge build receipt '{}' cannot be read: {error}",
            receipt_path.display()
        )
    })?;
    let receipt: BridgeBuildReceipt = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "OpenCodex bridge build receipt '{}' is malformed: {error}",
            receipt_path.display()
        )
    })?;
    let declared_executable = portable_relative_path(&receipt.executable, "receipt executable")?;
    let expected_executable = bridge_root.join(declared_executable);
    let executable_content = std::fs::read(executable).map_err(|error| {
        format!(
            "OpenCodex bridge executable '{}' cannot be read: {error}",
            executable.display()
        )
    })?;
    if receipt.schema_version != 1
        || expected_executable != executable
        || receipt.executable_bytes != executable_bytes
        || !valid_sha256(&receipt.executable_sha256)
        || sha256(&executable_content) != receipt.executable_sha256
        || !receipt.upstream.is_object()
    {
        return Err("OpenCodex bridge build receipt does not match its executable".to_string());
    }
    if receipt.inputs.is_empty() {
        return Err("OpenCodex bridge build receipt declares no inputs".to_string());
    }
    for (name, expected) in receipt.inputs {
        if !valid_sha256(&expected) {
            return Err(format!(
                "OpenCodex bridge build receipt input '{name}' has an invalid digest"
            ));
        }
        let relative = portable_relative_path(&name, "receipt input")?;
        let path = bridge_root.join(relative);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "OpenCodex bridge build input '{}' cannot be inspected: {error}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "OpenCodex bridge build input '{}' is not a regular file",
                path.display()
            ));
        }
        let canonical = std::fs::canonicalize(&path).map_err(|error| {
            format!(
                "OpenCodex bridge build input '{}' cannot be resolved: {error}",
                path.display()
            )
        })?;
        if !canonical.starts_with(bridge_root) {
            return Err("OpenCodex bridge build input escaped its package root".to_string());
        }
        let content = std::fs::read(&canonical).map_err(|error| {
            format!(
                "OpenCodex bridge build input '{}' cannot be read: {error}",
                canonical.display()
            )
        })?;
        if sha256(&content) != expected {
            return Err(format!(
                "OpenCodex bridge build input '{name}' does not match its receipt"
            ));
        }
    }
    Ok(())
}

async fn spawn_bridge(
    executable: &Path,
    cwd: &Path,
    mut config: BridgeConfig,
    environment: &[String],
) -> Result<(Child, u16), String> {
    let mut payload = serde_json::to_vec(&config)
        .map_err(|error| format!("OpenCodex bridge config could not be encoded: {error}"))?;
    if let Some(value) = config.provider.api_key.as_mut() {
        value.zeroize();
    }
    let mut command = Command::new(executable);
    command
        .env_clear()
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"]
        .into_iter()
        .chain(environment.iter().map(String::as_str))
    {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.env("TMPDIR", cwd);
    let mut child = command.spawn().map_err(|error| {
        format!(
            "OpenCodex bridge executable '{}' could not be spawned: {error}",
            executable.display()
        )
    })?;
    let write_result = async {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            "OpenCodex bridge did not expose its private config stdin".to_string()
        })?;
        stdin
            .write_all(&payload)
            .await
            .map_err(|error| format!("OpenCodex bridge config write failed: {error}"))?;
        stdin
            .shutdown()
            .await
            .map_err(|error| format!("OpenCodex bridge config close failed: {error}"))
    }
    .await;
    payload.zeroize();
    if let Err(error) = write_result {
        terminate_child(&mut child).await;
        return Err(error);
    }
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "OpenCodex bridge did not expose readiness stdout".to_string())?;
    let line = match timeout(BRIDGE_READY_TIMEOUT, read_ready_line(&mut stdout)).await {
        Ok(result) => result,
        Err(_) => Err("OpenCodex bridge readiness timed out".to_string()),
    };
    let line = match line {
        Ok(line) => line,
        Err(error) => {
            terminate_child(&mut child).await;
            return Err(error);
        }
    };
    let ready: BridgeReady = match serde_json::from_slice(&line) {
        Ok(ready) => ready,
        Err(error) => {
            terminate_child(&mut child).await;
            return Err(format!("OpenCodex bridge readiness is malformed: {error}"));
        }
    };
    if ready.schema_version != 1
        || ready.status != "ready"
        || ready.port == 0
        || ready.interface_id != config.interface_id
    {
        terminate_child(&mut child).await;
        return Err("OpenCodex bridge readiness disagrees with its exact interface".to_string());
    }
    if let Some(status) = child
        .try_wait()
        .map_err(|error| format!("OpenCodex bridge status check failed: {error}"))?
    {
        return Err(format!(
            "OpenCodex bridge exited immediately after readiness with status {status}"
        ));
    }
    Ok((child, ready.port))
}

async fn read_ready_line(stdout: &mut tokio::process::ChildStdout) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let count = stdout
            .read(&mut buffer)
            .await
            .map_err(|error| format!("OpenCodex bridge readiness read failed: {error}"))?;
        if count == 0 {
            return Err("OpenCodex bridge exited before readiness".to_string());
        }
        let chunk = &buffer[..count];
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(chunk.len());
        if line.len().saturating_add(take) > BRIDGE_READY_MAX_BYTES {
            return Err("OpenCodex bridge readiness exceeds 64 KiB".to_string());
        }
        line.extend_from_slice(&chunk[..take]);
        if let Some(index) = newline {
            if chunk[index + 1..]
                .iter()
                .any(|byte| !byte.is_ascii_whitespace())
            {
                return Err("OpenCodex bridge wrote undeclared readiness output".to_string());
            }
            return Ok(line);
        }
    }
}

async fn terminate_child(child: &mut Child) {
    let _ = child.start_kill();
    let _ = timeout(Duration::from_secs(5), child.wait()).await;
}

fn portable_relative_path(value: &str, field: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains(['\\', '\0'])
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("OpenCodex {field} is not a portable relative path"));
    }
    Ok(path.to_path_buf())
}

fn validate_limits(limits: &OpenCodexLimits) -> Result<(), String> {
    validate_bounded_integer(
        limits.max_request_bytes,
        1024,
        256 * 1024 * 1024,
        "limits.max_request_bytes",
    )?;
    validate_bounded_integer(
        limits.max_translated_call_bytes,
        1024,
        32 * 1024 * 1024,
        "limits.max_translated_call_bytes",
    )?;
    validate_bounded_integer(
        limits.max_translated_turn_bytes,
        1024,
        128 * 1024 * 1024,
        "limits.max_translated_turn_bytes",
    )
}

fn validate_bounded_integer(
    value: u64,
    minimum: u64,
    maximum: u64,
    field: &str,
) -> Result<(), String> {
    if value < minimum || value > maximum {
        return Err(format!(
            "OpenCodex {field} must be within {minimum}..={maximum}"
        ));
    }
    Ok(())
}

fn validate_interface_segment(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 320
        || value.trim() != value
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(format!("{field} is not a valid exact interface id"));
    }
    Ok(())
}

fn valid_text_atom(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.contains(['\r', '\n', '\0'])
}

fn valid_env_name(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.chars().all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

fn valid_header_name(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn secret_header(value: &str) -> bool {
    let name = value.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "api-key"
            | "x-api-key"
            | "x-auth-token"
            | "anthropic-auth-token"
    ) || name.ends_with("-api-key")
        || name.ends_with("-access-token")
        || name.ends_with("-auth-token")
        || name.contains("secret")
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

const fn default_max_request_bytes() -> u64 {
    32 * 1024 * 1024
}

const fn default_max_translated_call_bytes() -> u64 {
    2 * 1024 * 1024
}

const fn default_max_translated_turn_bytes() -> u64 {
    32 * 1024 * 1024
}

#[cfg(test)]
#[path = "../tests/support/opencodex_secrets.rs"]
mod tests;
