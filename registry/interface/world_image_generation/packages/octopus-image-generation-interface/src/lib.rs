use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{
    DeliveryVerdict, InterfaceArtifactFile, InterfaceDescriptor, InterfaceTransportConfig,
    RefusedReason,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use reqwest::{Client, RequestBuilder, Response, Url};
use serde::Deserialize;
use serde_json::{json, Map, Value};

const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const ABSOLUTE_MAX_BYTES: usize = 512 * 1024 * 1024;
const MAX_ACCOUNT_FILE_BYTES: u64 = 1024 * 1024;
const ACCESS_TOKEN_EXPIRY_SKEW_SECS: u64 = 30;
const MAX_PROMPT_CHARS: usize = 32_000;
const MAX_OPTION_CHARS: usize = 32;
const MAX_CORRELATION_CHARS: usize = 512;
const MAX_SERVICE_MESSAGE_CHARS: usize = 600;
const SIGN_IN_ADVICE: &str = "tell the user to sign in to ChatGPT in Octopus";
const INPUT_SCHEMA_KEYWORDS: [&str; 4] = ["type", "properties", "required", "additionalProperties"];
const ARGUMENT_SCHEMA_KEYWORDS: [&str; 5] =
    ["type", "description", "enum", "minLength", "maxLength"];
const INTERRUPTED_CALL: &str = "Octopus stopped while the image service was creating this image, so the image was lost and may still count against the plan's image usage. It was not requested again; call the operation again only if the image is still needed.";

#[derive(Clone)]
struct ImageGenerationProcess {
    transport: ImageTransport,
    client: Client,
}

#[derive(Clone)]
struct ImageTransport {
    endpoint: Url,
    headers: HeaderMap,
    auth: Option<ImageAuth>,
    max_response_bytes: usize,
}

#[derive(Clone)]
enum ImageAuth {
    Key {
        header: HeaderName,
        value_prefix: String,
        env_key: String,
    },
    ChatgptAccount {
        auth_file_env: String,
        account_header: HeaderName,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageConfig {
    operation_catalog_dir: String,
    operation_catalog_label: String,
    operation_request_label: String,
    prompt_field: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    quality: Option<String>,
    #[serde(default)]
    background: Option<String>,
    #[serde(default)]
    n: Option<u32>,
    #[serde(default)]
    response_format: Option<String>,
    data_path: String,
    b64_field: String,
    url_field: String,
    allow_endpoint_origin: bool,
    allowed_url_origins: Vec<String>,
    #[serde(default)]
    revised_prompt_field: Option<String>,
    media_label: String,
    #[serde(default)]
    max_image_bytes: Option<u64>,
    #[serde(default)]
    auth: Option<AccountAuth>,
    #[serde(default, rename = "account_check")]
    _account_check: Option<serde::de::IgnoredAny>,
    #[serde(default, rename = "client_connection")]
    _client_connection: Option<serde::de::IgnoredAny>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum AccountAuth {
    ChatgptAccount {
        auth_file_env: String,
        account_header: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationFile {
    id: String,
    request_label: String,
    output_label: String,
    handler_config: ImageHandler,
    description: String,
    input_schema: Value,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ImageHandler {
    Generate,
}

#[derive(Debug)]
struct ImageCatalog {
    request_label: String,
    result_label: String,
    operations: BTreeMap<String, CatalogOperation>,
}

#[derive(Debug)]
struct CatalogOperation {
    handler: ImageHandler,
    input_schema: Value,
}

struct CallJournal {
    root: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationRequest {
    operation: String,
    arguments: Value,
    #[serde(rename = "correlation_id")]
    _correlation_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerateArguments {
    prompt: String,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    quality: Option<String>,
    #[serde(default)]
    transparent_background: Option<bool>,
}

#[derive(Deserialize)]
struct SignInDocument {
    #[serde(default)]
    auth_mode: Option<String>,
    #[serde(default)]
    tokens: Option<SignInTokens>,
}

#[derive(Deserialize)]
struct SignInTokens {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
}

struct GeneratedImage {
    bytes: Vec<u8>,
    revised_prompt: Option<String>,
    reported: Map<String, Value>,
}

#[derive(Debug)]
struct ImageCallFailure {
    code: &'static str,
    message: String,
    retryable: bool,
    endpoint_status: Option<u16>,
    resets_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HttpOrigin {
    scheme: String,
    host: String,
    port: u16,
}

impl ImageCallFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            endpoint_status: None,
            resets_at: None,
        }
    }

    fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    fn answered(mut self, status: u16) -> Self {
        self.endpoint_status = Some(status);
        self
    }

    fn value(&self) -> Value {
        let mut error = json!({
            "code": self.code,
            "message": self.message,
            "retryable": self.retryable,
        });
        if let Some(status) = self.endpoint_status {
            error["endpoint_status"] = json!(status);
        }
        if let Some(resets_at) = self.resets_at {
            error["resets_at"] = json!(resets_at);
        }
        error
    }
}

impl ImageAuth {
    fn is_account(&self) -> bool {
        matches!(self, Self::ChatgptAccount { .. })
    }

    fn headers(&self) -> Result<Vec<(HeaderName, HeaderValue)>, ImageCallFailure> {
        match self {
            Self::Key {
                header,
                value_prefix,
                env_key,
            } => {
                let secret = std::env::var(env_key)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        ImageCallFailure::new(
                            "api_key_missing",
                            format!("the image interface has no API key in '{env_key}'; tell the user to connect Image Generation in Octopus"),
                        )
                    })?;
                let mut value = HeaderValue::from_str(&format!("{value_prefix}{}", secret.trim()))
                    .map_err(|_| {
                        ImageCallFailure::new(
                            "api_key_unusable",
                            format!("the API key in '{env_key}' cannot form a request header"),
                        )
                    })?;
                value.set_sensitive(true);
                Ok(vec![(header.clone(), value)])
            }
            Self::ChatgptAccount {
                auth_file_env,
                account_header,
            } => chatgpt_credential(auth_file_env, account_header),
        }
    }
}

fn chatgpt_credential(
    auth_file_env: &str,
    account_header: &HeaderName,
) -> Result<Vec<(HeaderName, HeaderValue)>, ImageCallFailure> {
    let required = |reason: &str| {
        ImageCallFailure::new(
            "account_sign_in_required",
            format!("{reason}; {SIGN_IN_ADVICE}"),
        )
    };
    let unreadable = |reason: &str| {
        ImageCallFailure::new(
            "account_sign_in_unreadable",
            format!("the ChatGPT sign-in on this Mac {reason}; {SIGN_IN_ADVICE} again"),
        )
    };
    let path = std::env::var_os(auth_file_env)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            required("this interface was not given the location of the ChatGPT sign-in")
        })?;
    if !path.is_absolute() {
        return Err(unreadable("is not named by an absolute path"));
    }
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(required("this Mac has no ChatGPT sign-in"));
        }
        Err(_) => return Err(unreadable("cannot be inspected")),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_ACCOUNT_FILE_BYTES
    {
        return Err(unreadable("is not a bounded regular file"));
    }
    let bytes = std::fs::read(&path).map_err(|_| unreadable("cannot be read"))?;
    if bytes.len() as u64 > MAX_ACCOUNT_FILE_BYTES {
        return Err(unreadable("grew beyond its bound while it was read"));
    }
    let document: SignInDocument =
        serde_json::from_slice(&bytes).map_err(|_| unreadable("is not JSON"))?;
    if document
        .auth_mode
        .as_deref()
        .is_some_and(|mode| mode != "chatgpt")
    {
        return Err(required("this Mac is signed in without a ChatGPT account"));
    }
    let tokens = document
        .tokens
        .ok_or_else(|| required("the ChatGPT sign-in carries no account tokens"))?;
    let access_token = nonempty(tokens.access_token)
        .ok_or_else(|| required("the ChatGPT sign-in has no access token"))?;
    let account_id = nonempty(tokens.account_id)
        .ok_or_else(|| required("the ChatGPT sign-in has no account identity"))?;
    let expires_at =
        token_expiry(&access_token).ok_or_else(|| unreadable("has an unreadable access token"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| unreadable("cannot be dated because the clock precedes 1970"))?
        .as_secs();
    if expires_at <= now.saturating_add(ACCESS_TOKEN_EXPIRY_SKEW_SECS) {
        return Err(ImageCallFailure::new(
            "account_sign_in_expired",
            format!("the ChatGPT sign-in on this Mac has expired; {SIGN_IN_ADVICE} again"),
        ));
    }
    let mut authorization = HeaderValue::from_str(&format!("Bearer {access_token}"))
        .map_err(|_| unreadable("has an access token that cannot form a header"))?;
    authorization.set_sensitive(true);
    let mut account = HeaderValue::from_str(&account_id)
        .map_err(|_| unreadable("has an account identity that cannot form a header"))?;
    account.set_sensitive(true);
    Ok(vec![
        (AUTHORIZATION, authorization),
        (account_header.clone(), account),
    ])
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn token_expiry(token: &str) -> Option<u64> {
    let mut parts = token.split('.');
    let (_, claims, _, None) = (parts.next()?, parts.next()?, parts.next()?, parts.next()) else {
        return None;
    };
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).ok()?).ok()?;
    let exp = claims.get("exp")?;
    exp.as_u64().or_else(|| {
        exp.as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| value as u64)
    })
}

impl ImageGenerationProcess {
    fn request(
        &self,
        config: &ImageConfig,
        arguments: &GenerateArguments,
    ) -> Result<RequestBuilder, ImageCallFailure> {
        let body = build_request_body(config, arguments)?;
        let mut request = self
            .client
            .post(self.transport.endpoint.clone())
            .headers(self.transport.headers.clone())
            .json(&body);
        if let Some(auth) = &self.transport.auth {
            for (name, value) in auth.headers()? {
                request = request.header(name, value);
            }
        }
        Ok(request)
    }

    async fn generate(
        &self,
        config: &ImageConfig,
        request: RequestBuilder,
    ) -> Result<GeneratedImage, ImageCallFailure> {
        let response = request.send().await.map_err(|error| {
            ImageCallFailure::new(
                "transport_failed",
                format!("the image service could not be reached: {error}"),
            )
            .retryable()
        })?;
        let status = response.status().as_u16();
        let response_bytes = read_bounded(
            response,
            self.transport.max_response_bytes,
            "the image service response",
        )
        .await?;
        if !(200..300).contains(&status) {
            return Err(service_refusal(
                status,
                &response_bytes,
                self.transport
                    .auth
                    .as_ref()
                    .is_some_and(ImageAuth::is_account),
            ));
        }
        let body: Value = serde_json::from_slice(&response_bytes).map_err(|error| {
            ImageCallFailure::new(
                "invalid_response",
                format!("the image service answered with invalid JSON: {error}"),
            )
        })?;
        let entry = dotted_value(&body, config.data_path.trim())
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .ok_or_else(|| {
                ImageCallFailure::new(
                    "invalid_response",
                    format!(
                        "the image service answered without an image at '{}'",
                        config.data_path.trim()
                    ),
                )
            })?;
        let revised_prompt = config
            .revised_prompt_field
            .as_deref()
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .and_then(|field| entry.get(field))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let reported = ["size", "quality", "background", "output_format"]
            .into_iter()
            .filter_map(|field| {
                body.get(field)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty() && value.len() <= MAX_OPTION_CHARS)
                    .map(|value| (field.to_string(), Value::String(value.to_string())))
            })
            .collect();
        let max_image_bytes = checked_limit(
            config.max_image_bytes,
            DEFAULT_MAX_IMAGE_BYTES,
            "interface private_config.max_image_bytes",
        )
        .map_err(|message| ImageCallFailure::new("interface_misconfigured", message))?;
        if let Some(encoded) = entry
            .get(config.b64_field.trim())
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let maximum_decoded = encoded
                .len()
                .checked_add(3)
                .and_then(|value| value.checked_div(4))
                .and_then(|value| value.checked_mul(3))
                .ok_or_else(|| {
                    ImageCallFailure::new("invalid_response", "the base64 image size overflowed")
                })?;
            if maximum_decoded > max_image_bytes.saturating_add(2) {
                return Err(ImageCallFailure::new(
                    "invalid_response",
                    format!("the generated image exceeds max_image_bytes={max_image_bytes}"),
                ));
            }
            let bytes = BASE64_STANDARD
                .decode(encoded.as_bytes())
                .map_err(|error| {
                    ImageCallFailure::new(
                        "invalid_response",
                        format!("the generated image is not valid base64: {error}"),
                    )
                })?;
            if bytes.is_empty() || bytes.len() > max_image_bytes {
                return Err(ImageCallFailure::new(
                    "invalid_response",
                    format!(
                        "the generated image has {} bytes, outside 1..={max_image_bytes}",
                        bytes.len()
                    ),
                ));
            }
            return Ok(GeneratedImage {
                bytes,
                revised_prompt,
                reported,
            });
        }
        if let Some(url) = entry
            .get(config.url_field.trim())
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let media_url = parse_http_url(url, "image response URL")
                .map_err(|message| ImageCallFailure::new("invalid_response", message))?;
            require_allowed_media_origin(
                &media_url,
                &self.transport.endpoint,
                config.allow_endpoint_origin,
                &config.allowed_url_origins,
            )
            .map_err(|message| ImageCallFailure::new("invalid_response", message))?;
            let media_response = self.client.get(media_url).send().await.map_err(|error| {
                ImageCallFailure::new(
                    "transport_failed",
                    format!("the generated image could not be downloaded: {error}"),
                )
                .retryable()
            })?;
            let media_status = media_response.status().as_u16();
            let bytes =
                read_bounded(media_response, max_image_bytes, "the generated image").await?;
            if !(200..300).contains(&media_status) {
                return Err(ImageCallFailure::new(
                    "service_unavailable",
                    format!("the generated image URL answered HTTP {media_status}"),
                )
                .answered(media_status)
                .retryable());
            }
            if bytes.is_empty() {
                return Err(ImageCallFailure::new(
                    "invalid_response",
                    "the generated image URL returned no bytes",
                ));
            }
            return Ok(GeneratedImage {
                bytes,
                revised_prompt,
                reported,
            });
        }
        Err(ImageCallFailure::new(
            "invalid_response",
            format!(
                "the image service answered with neither '{}' nor '{}'",
                config.b64_field.trim(),
                config.url_field.trim()
            ),
        ))
    }
}

fn service_refusal(status: u16, body: &[u8], account: bool) -> ImageCallFailure {
    let error = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .filter(|error| error.is_object())
                .cloned()
        });
    let field = |name| error_field(error.as_ref(), name);
    let said = field("message").map(|message| {
        let line = message
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .take(MAX_SERVICE_MESSAGE_CHARS)
            .collect::<String>();
        line.trim().to_string()
    });
    let kind = field("type").or_else(|| field("code"));
    let message = said.unwrap_or_else(|| {
        format!(
            "the image service answered HTTP {status} without an error message ({})",
            body_evidence(body)
        )
    });
    let failure = match status {
        401 if account => ImageCallFailure::new(
            "account_sign_in_expired",
            format!("ChatGPT no longer accepts the sign-in on this Mac ({message}); {SIGN_IN_ADVICE} again"),
        ),
        401 | 403 => ImageCallFailure::new("authorization_refused", message),
        429 if kind == Some("usage_limit_reached") => {
            let mut failure = ImageCallFailure::new("usage_limit_reached", message);
            failure.resets_at = error
                .as_ref()
                .and_then(|error| error.get("resets_at"))
                .and_then(Value::as_i64);
            failure
        }
        429 => ImageCallFailure::new("rate_limited", message).retryable(),
        500..=599 => ImageCallFailure::new("service_unavailable", message).retryable(),
        _ => ImageCallFailure::new("request_refused", message),
    };
    failure.answered(status)
}

fn error_field<'a>(error: Option<&'a Value>, name: &str) -> Option<&'a str> {
    error
        .and_then(|error| error.get(name))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(run_image_generation_interface_life(ctx)) as InterfaceLifeFuture)
}

async fn run_image_generation_interface_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let limits = ctx.required_host_io_limits()?;
    let config = parse_config(&descriptor, &ctx.interface)?;
    let catalog = load_catalog(&ctx.interface_dir, &config)?;
    let transport = load_transport(&descriptor, &ctx.interface, &config)?;
    let timeout = transport_timeout(&descriptor, &ctx.interface)?;
    let client = Client::builder()
        .no_proxy()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("image interface HTTP client build failed: {error}"))?;
    let process = ImageGenerationProcess { transport, client };
    let output_root = ctx.interface_var_dir().join("operation-results");
    let journal = CallJournal {
        root: ctx.interface_var_dir().join("operation-calls"),
    };
    let mut reader = LedgerTailReader::open_named(&ctx, "image-generation")?;
    ctx.signal_ready().await?;
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        let batch = reader.read_new()?;
        if batch.is_empty() {
            reader.wait_for_change(&ctx.cancel).await?;
            continue;
        }
        for own in batch {
            let seq = own.movement.seq.0;
            let envelope = match reader.envelope(&own) {
                Ok(envelope) => envelope,
                Err(error) => {
                    offer_failure(&ctx, seq, "poisoned_image_input", json!({ "error": error }))
                        .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let manifest = match envelope.read_artifact_manifest_bounded(limits.max_manifest_bytes)
            {
                Ok(manifest) => manifest,
                Err(error) => {
                    offer_failure(
                        &ctx,
                        seq,
                        "poisoned_image_input",
                        json!({ "error": error.to_string() }),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            if manifest.manifest.label != catalog.request_label {
                reader.commit_through(&own)?;
                continue;
            }
            let payload =
                match envelope.read_artifact_payload_bounded(limits.max_control_frame_bytes) {
                    Ok(payload) => payload,
                    Err(error) => {
                        offer_failure(
                            &ctx,
                            seq,
                            "poisoned_image_request",
                            json!({ "error": error.to_string() }),
                        )
                        .await?;
                        reader.commit_through(&own)?;
                        continue;
                    }
                };
            let Some(call) = request_identity(&payload) else {
                offer_failure(
                    &ctx,
                    seq,
                    "malformed_image_request",
                    json!({ "error": format!("the request is not a JSON object with a trimmed correlation_id of at most {MAX_CORRELATION_CHARS} characters") }),
                )
                .await?;
                reader.commit_through(&own)?;
                continue;
            };
            let outcome = if journal.interrupted(seq, &call)? {
                Err(ImageCallFailure::new(
                    "indeterminate_external_effect",
                    INTERRUPTED_CALL,
                ))
            } else {
                answer_call(&process, &config, &catalog, &payload, || {
                    journal.begin(seq, &call)
                })
                .await?
            };
            let answer = result_artifact(
                &output_root,
                &config,
                &catalog,
                &call,
                outcome,
                limits.max_manifest_bytes,
            )?;
            offer(&ctx, answer).await?;
            reader.commit_through(&own)?;
            journal.settle(seq)?;
        }
    }
}

struct CallIdentity {
    operation: String,
    correlation_id: String,
}

fn request_identity(payload: &[u8]) -> Option<CallIdentity> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let object = value.as_object()?;
    let correlation_id = object
        .get("correlation_id")
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.trim() == *value
                && value.chars().count() <= MAX_CORRELATION_CHARS
        })?
        .to_string();
    let operation = object
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(CallIdentity {
        operation,
        correlation_id,
    })
}

impl CallJournal {
    fn record(seq: u64, call: &CallIdentity) -> Value {
        json!({
            "source_movement_seq": seq,
            "operation": call.operation,
            "correlation_id": call.correlation_id,
        })
    }

    fn path(&self, seq: u64) -> PathBuf {
        self.root.join(format!("{seq}.json"))
    }

    fn interrupted(&self, seq: u64, call: &CallIdentity) -> Result<bool, String> {
        let bytes = match std::fs::read(self.path(seq)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(format!(
                    "image call record for movement {seq} cannot be read: {error}"
                ))
            }
        };
        let recorded: Value = serde_json::from_slice(&bytes).map_err(|error| {
            format!("image call record for movement {seq} is malformed: {error}")
        })?;
        if recorded != Self::record(seq, call) {
            return Err(format!(
                "image call record for movement {seq} names a different call"
            ));
        }
        Ok(true)
    }

    fn begin(&self, seq: u64, call: &CallIdentity) -> Result<(), String> {
        let failed = |error: std::io::Error| {
            format!("image call record for movement {seq} could not be written: {error}")
        };
        let bytes = serde_json::to_vec(&Self::record(seq, call))
            .map_err(|error| format!("image call record did not encode: {error}"))?;
        std::fs::create_dir_all(&self.root).map_err(failed)?;
        let staged = self.root.join(format!("{seq}.json.partial"));
        let mut file = std::fs::File::create(&staged).map_err(failed)?;
        file.write_all(&bytes).map_err(failed)?;
        file.sync_all().map_err(failed)?;
        std::fs::rename(&staged, self.path(seq)).map_err(failed)
    }

    fn settle(&self, seq: u64) -> Result<(), String> {
        match std::fs::remove_file(self.path(seq)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "image call record for movement {seq} could not be settled: {error}"
            )),
        }
    }
}

async fn answer_call(
    process: &ImageGenerationProcess,
    config: &ImageConfig,
    catalog: &ImageCatalog,
    payload: &[u8],
    begin: impl FnOnce() -> Result<(), String>,
) -> Result<Result<GeneratedImage, ImageCallFailure>, String> {
    let request = match prepare_call(process, config, catalog, payload) {
        Ok(request) => request,
        Err(failure) => return Ok(Err(failure)),
    };
    begin()?;
    Ok(process.generate(config, request).await)
}

fn prepare_call(
    process: &ImageGenerationProcess,
    config: &ImageConfig,
    catalog: &ImageCatalog,
    payload: &[u8],
) -> Result<RequestBuilder, ImageCallFailure> {
    let request: OperationRequest = serde_json::from_slice(payload).map_err(|error| {
        ImageCallFailure::new(
            "malformed_request",
            format!(
                "the request must carry exactly operation, arguments and correlation_id: {error}"
            ),
        )
    })?;
    let operation = catalog.operations.get(&request.operation).ok_or_else(|| {
        ImageCallFailure::new(
            "unknown_operation",
            format!(
                "'{}' is not an operation of this interface",
                request.operation
            ),
        )
    })?;
    check_arguments(&operation.input_schema, &request.arguments).map_err(|message| {
        ImageCallFailure::new(
            "invalid_arguments",
            format!("arguments are invalid: {message}"),
        )
    })?;
    match operation.handler {
        ImageHandler::Generate => {
            let arguments: GenerateArguments =
                serde_json::from_value(request.arguments).map_err(|error| {
                    ImageCallFailure::new(
                        "invalid_arguments",
                        format!("arguments are invalid: {error}"),
                    )
                })?;
            process.request(config, &arguments)
        }
    }
}

fn check_arguments(schema: &Value, arguments: &Value) -> Result<(), String> {
    let arguments = arguments
        .as_object()
        .ok_or("arguments must be a JSON object")?;
    let properties = schema.get("properties").and_then(Value::as_object);
    for name in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !arguments.contains_key(name) {
            return Err(format!("'{name}' is required"));
        }
    }
    for (name, value) in arguments {
        let argument = properties
            .and_then(|properties| properties.get(name))
            .ok_or_else(|| format!("'{name}' is not an argument of this operation"))?;
        if argument.get("type").and_then(Value::as_str) == Some("boolean") {
            if !value.is_boolean() {
                return Err(format!("'{name}' must be true or false"));
            }
            continue;
        }
        let text = value
            .as_str()
            .ok_or_else(|| format!("'{name}' must be a string"))?;
        if let Some(choices) = argument.get("enum").and_then(Value::as_array) {
            if !choices.contains(value) {
                let choices = choices
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!("'{name}' must be one of {choices}"));
            }
        }
        let length = text.chars().count() as u64;
        let bound = |keyword| argument.get(keyword).and_then(Value::as_u64);
        if let Some(minimum) = bound("minLength").filter(|minimum| length < *minimum) {
            return Err(format!("'{name}' must hold at least {minimum} characters"));
        }
        if let Some(maximum) = bound("maxLength").filter(|maximum| length > *maximum) {
            return Err(format!("'{name}' must hold at most {maximum} characters"));
        }
    }
    Ok(())
}

fn admit_input_schema(schema: &Value) -> Result<(), String> {
    let root = schema
        .as_object()
        .ok_or("its input schema is not a JSON object")?;
    if let Some(keyword) = root
        .keys()
        .find(|keyword| !INPUT_SCHEMA_KEYWORDS.contains(&keyword.as_str()))
    {
        return Err(format!(
            "its input schema uses '{keyword}', which this interface does not check"
        ));
    }
    let properties = root
        .get("properties")
        .and_then(Value::as_object)
        .filter(|_| {
            root.get("type").and_then(Value::as_str) == Some("object")
                && root.get("additionalProperties") == Some(&Value::Bool(false))
        })
        .ok_or("its input schema must be a closed object with type object, properties and additionalProperties false")?;
    for (name, argument) in properties {
        let argument = argument
            .as_object()
            .ok_or_else(|| format!("argument '{name}' is not a schema object"))?;
        if let Some(keyword) = argument
            .keys()
            .find(|keyword| !ARGUMENT_SCHEMA_KEYWORDS.contains(&keyword.as_str()))
        {
            return Err(format!(
                "argument '{name}' uses '{keyword}', which this interface does not check"
            ));
        }
        let fits = match argument.get("type").and_then(Value::as_str) {
            Some("boolean") => argument
                .keys()
                .all(|keyword| keyword == "type" || keyword == "description"),
            Some("string") => {
                ["minLength", "maxLength"]
                    .iter()
                    .all(|keyword| argument.get(*keyword).is_none_or(Value::is_u64))
                    && argument.get("enum").is_none_or(|choices| {
                        choices.as_array().is_some_and(|choices| {
                            !choices.is_empty() && choices.iter().all(Value::is_string)
                        })
                    })
            }
            _ => false,
        };
        if !fits {
            return Err(format!(
                "argument '{name}' must be a boolean, or a string with optional enum, minLength and maxLength"
            ));
        }
    }
    let required = root.get("required").is_none_or(|required| {
        required.as_array().is_some_and(|names| {
            names.iter().all(|name| {
                name.as_str()
                    .is_some_and(|name| properties.contains_key(name))
            })
        })
    });
    if !required {
        return Err("its input schema requires an argument it does not declare".to_string());
    }
    Ok(())
}

fn result_artifact(
    root: &Path,
    config: &ImageConfig,
    catalog: &ImageCatalog,
    call: &CallIdentity,
    outcome: Result<GeneratedImage, ImageCallFailure>,
    max_manifest_bytes: u64,
) -> Result<InterfaceArtifactFile, String> {
    let mut body = json!({
        "label": catalog.result_label,
        "operation": call.operation,
        "correlation_id": call.correlation_id,
    });
    let outcome = outcome.and_then(|generated| {
        image_mime(&generated.bytes)
            .map(|mime| (generated, mime))
            .ok_or_else(|| {
                ImageCallFailure::new(
                    "invalid_response",
                    "the image service returned an image that is not PNG, JPEG or WebP",
                )
            })
    });
    match outcome {
        Ok((generated, mime)) => {
            let image =
                InterfaceArtifactFile::write(root, config.media_label.trim(), &generated.bytes)
                    .map_err(|error| {
                        format!("image interface could not materialize generated media: {error}")
                    })?;
            let manifest = image
                .read_manifest_bounded(max_manifest_bytes)
                .map_err(|error| format!("generated image manifest is invalid: {error}"))?
                .manifest;
            let mut result = Map::new();
            result.insert(
                "image".to_string(),
                json!({
                    "media": 0,
                    "mime_type": mime,
                    "byte_count": generated.bytes.len(),
                    "payload_content_hash": manifest.payload_content_hash,
                }),
            );
            if let Some(model) = config
                .model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
            {
                result.insert("model".to_string(), json!(model));
            }
            if let Some(revised_prompt) = generated.revised_prompt {
                result.insert("revised_prompt".to_string(), json!(revised_prompt));
            }
            result.extend(generated.reported);
            body["ok"] = json!(true);
            body["result"] = Value::Object(result);
            body["media"] = json!([{
                "mime_type": mime,
                "artifact": manifest,
                "payload_base64": BASE64_STANDARD.encode(&generated.bytes),
            }]);
        }
        Err(failure) => {
            body["ok"] = json!(false);
            body["error"] = failure.value();
        }
    }
    let bytes = serde_json::to_vec(&body)
        .map_err(|error| format!("image result artifact did not encode: {error}"))?;
    InterfaceArtifactFile::write(root, &catalog.result_label, &bytes)
        .map_err(|error| format!("image result artifact could not be written: {error}"))
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

async fn offer_failure(
    ctx: &InterfaceLiftContext,
    source_movement_seq: u64,
    failure_class: &str,
    details: Value,
) -> Result<(), String> {
    offer(
        ctx,
        ctx.failure_artifact(source_movement_seq, failure_class, details)?,
    )
    .await
}

async fn offer(ctx: &InterfaceLiftContext, artifact: InterfaceArtifactFile) -> Result<(), String> {
    match ctx.hand_arrival(artifact, Vec::new()).await? {
        DeliveryVerdict::Admitted { .. } => Ok(()),
        DeliveryVerdict::Refused {
            reason:
                RefusedReason::NoBinding | RefusedReason::Policy | RefusedReason::VerificationFailed,
            ..
        } => Ok(()),
        DeliveryVerdict::Refused { reason, detail } => Err(format!(
            "runtime refused image interface output as '{}': {detail}",
            reason.as_str()
        )),
    }
}

fn parse_config(
    descriptor: &InterfaceDescriptor,
    interface: &InterfaceId,
) -> Result<ImageConfig, String> {
    let driver = descriptor
        .interface_driver_config
        .as_ref()
        .ok_or_else(|| format!("image interface '{}' resolves no driver config", interface))?;
    let config: ImageConfig = serde_json::from_value(Value::Object(driver.private_config.clone()))
        .map_err(|error| {
            format!(
                "image interface '{}' private_config is malformed: {error}",
                interface
            )
        })?;
    for (name, value) in [
        (
            "operation_catalog_dir",
            config.operation_catalog_dir.as_str(),
        ),
        (
            "operation_catalog_label",
            config.operation_catalog_label.as_str(),
        ),
        (
            "operation_request_label",
            config.operation_request_label.as_str(),
        ),
        ("prompt_field", config.prompt_field.as_str()),
        ("data_path", config.data_path.as_str()),
        ("b64_field", config.b64_field.as_str()),
        ("url_field", config.url_field.as_str()),
        ("media_label", config.media_label.as_str()),
    ] {
        require_trimmed(name, value)?;
    }
    if config.n.is_some_and(|n| n != 1) {
        return Err(format!(
            "image interface '{}' private_config.n must be exactly 1",
            interface
        ));
    }
    checked_limit(
        config.max_image_bytes,
        DEFAULT_MAX_IMAGE_BYTES,
        "interface private_config.max_image_bytes",
    )?;
    let mut origins = BTreeSet::new();
    for (index, origin) in config.allowed_url_origins.iter().enumerate() {
        let origin = parse_declared_origin(
            origin,
            &format!("interface private_config.allowed_url_origins[{index}]"),
        )?;
        if !origins.insert(origin) {
            return Err(format!(
                "image interface '{}' private_config.allowed_url_origins contains a duplicate origin",
                interface
            ));
        }
    }
    Ok(config)
}

fn load_catalog(interface_dir: &Path, config: &ImageConfig) -> Result<ImageCatalog, String> {
    let relative = Path::new(&config.operation_catalog_dir);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("image operation_catalog_dir must be a normalized relative path".to_string());
    }
    let root = interface_dir.join(relative);
    let metadata = std::fs::symlink_metadata(&root).map_err(|error| {
        format!(
            "image operation catalog '{}' cannot be inspected: {error}",
            root.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "image operation catalog '{}' is not a directory",
            root.display()
        ));
    }
    let mut entries = std::fs::read_dir(&root)
        .and_then(|entries| entries.collect::<Result<Vec<_>, _>>())
        .map_err(|error| {
            format!(
                "image operation catalog '{}' cannot be read: {error}",
                root.display()
            )
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut operations = BTreeMap::new();
    let mut result_labels = BTreeSet::new();
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            format!(
                "image operation '{}' cannot be inspected: {error}",
                path.display()
            )
        })?;
        if file_type.is_symlink()
            || !file_type.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            return Err(format!(
                "image operation catalog entry '{}' is not a regular JSON file",
                path.display()
            ));
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            format!(
                "image operation '{}' cannot be read: {error}",
                path.display()
            )
        })?;
        let operation: OperationFile = serde_json::from_slice(&bytes).map_err(|error| {
            format!("image operation '{}' is malformed: {error}", path.display())
        })?;
        for (name, value) in [
            ("id", operation.id.as_str()),
            ("output_label", operation.output_label.as_str()),
            ("description", operation.description.as_str()),
        ] {
            require_trimmed(
                &format!("image operation '{}' {name}", path.display()),
                value,
            )?;
        }
        if path.file_name().and_then(|name| name.to_str())
            != Some(format!("{}.json", operation.id).as_str())
            || operation.request_label != config.operation_request_label
            || operation.input_schema.get("type").and_then(Value::as_str) != Some("object")
        {
            return Err(format!(
                "image operation '{}' must live in <id>.json, use request label '{}' and declare an object input schema",
                path.display(),
                config.operation_request_label
            ));
        }
        admit_input_schema(&operation.input_schema).map_err(|reason| {
            format!(
                "image operation '{}' cannot be offered: {reason}",
                path.display()
            )
        })?;
        result_labels.insert(operation.output_label);
        operations.insert(
            operation.id,
            CatalogOperation {
                handler: operation.handler_config,
                input_schema: operation.input_schema,
            },
        );
    }
    let mut result_labels = result_labels.into_iter();
    match (result_labels.next(), result_labels.next()) {
        (Some(result_label), None) if !operations.is_empty() => Ok(ImageCatalog {
            request_label: config.operation_request_label.clone(),
            result_label,
            operations,
        }),
        _ => Err(format!(
            "image operation catalog '{}' must hold operations that answer on one result label",
            root.display()
        )),
    }
}

fn build_request_body(
    config: &ImageConfig,
    arguments: &GenerateArguments,
) -> Result<Value, ImageCallFailure> {
    let prompt = arguments.prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(ImageCallFailure::new(
            "invalid_arguments",
            format!("prompt must hold 1..={MAX_PROMPT_CHARS} characters"),
        ));
    }
    let mut body = Map::new();
    insert_request_field(
        &mut body,
        config.prompt_field.trim(),
        Value::String(prompt.to_string()),
    )?;
    let option = |name: &str, argument: Option<&str>, configured: Option<&str>| match argument {
        Some(value)
            if value.is_empty() || value.trim() != value || value.len() > MAX_OPTION_CHARS =>
        {
            Err(ImageCallFailure::new(
                "invalid_arguments",
                format!("{name} must be a trimmed value of 1..={MAX_OPTION_CHARS} characters"),
            ))
        }
        Some(value) => Ok(Some(value.to_string())),
        None => Ok(configured
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)),
    };
    let background = match arguments.transparent_background {
        Some(true) => Some("transparent"),
        Some(false) => Some("opaque"),
        None => None,
    };
    for (name, value) in [
        ("model", option("model", None, config.model.as_deref())?),
        (
            "size",
            option("size", arguments.size.as_deref(), config.size.as_deref())?,
        ),
        (
            "quality",
            option(
                "quality",
                arguments.quality.as_deref(),
                config.quality.as_deref(),
            )?,
        ),
        (
            "background",
            option("background", background, config.background.as_deref())?,
        ),
        (
            "response_format",
            option("response_format", None, config.response_format.as_deref())?,
        ),
    ] {
        if let Some(value) = value {
            insert_request_field(&mut body, name, Value::String(value))?;
        }
    }
    if let Some(n) = config.n {
        insert_request_field(&mut body, "n", Value::Number(n.into()))?;
    }
    Ok(Value::Object(body))
}

fn insert_request_field(
    body: &mut Map<String, Value>,
    name: &str,
    value: Value,
) -> Result<(), ImageCallFailure> {
    if name.is_empty() {
        return Err(ImageCallFailure::new(
            "interface_misconfigured",
            "an image request field name is empty",
        ));
    }
    if body.insert(name.to_string(), value).is_some() {
        return Err(ImageCallFailure::new(
            "interface_misconfigured",
            format!("image request field '{name}' is declared more than once"),
        ));
    }
    Ok(())
}

async fn read_bounded(
    mut response: Response,
    max_bytes: usize,
    surface: &str,
) -> Result<Vec<u8>, ImageCallFailure> {
    if let Some(length) = response.content_length() {
        if length > max_bytes as u64 {
            return Err(ImageCallFailure::new(
                "invalid_response",
                format!("{surface} declares {length} bytes, more than {max_bytes}"),
            ));
        }
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        ImageCallFailure::new(
            "transport_failed",
            format!("{surface} could not be read: {error}"),
        )
        .retryable()
    })? {
        let next = bytes.len().checked_add(chunk.len()).ok_or_else(|| {
            ImageCallFailure::new("invalid_response", format!("{surface} size overflowed"))
        })?;
        if next > max_bytes {
            return Err(ImageCallFailure::new(
                "invalid_response",
                format!("{surface} exceeds {max_bytes} bytes"),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn load_transport(
    descriptor: &InterfaceDescriptor,
    interface: &InterfaceId,
    config: &ImageConfig,
) -> Result<ImageTransport, String> {
    let transport = resolved_transport(descriptor, interface)?;
    if transport.wire.as_deref() != Some("image_json_http") {
        return Err(format!(
            "image interface '{}' transport.wire must be 'image_json_http'",
            interface
        ));
    }
    let endpoint = resolve_endpoint(&transport)?;
    let max_response_bytes = checked_limit(
        transport.max_response_bytes,
        DEFAULT_MAX_RESPONSE_BYTES,
        "interface transport.max_response_bytes",
    )?;
    let auth = resolve_auth(&transport, config.auth.as_ref())?;
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("application/json"),
    );
    for (name, value) in &transport.headers {
        let name = parse_header_name(name, "interface transport header")?;
        reject_secret_header(&name)?;
        if let Some(ImageAuth::ChatgptAccount { account_header, .. }) = &auth {
            if name == *account_header {
                return Err(format!(
                    "interface transport header '{}' is set from the ChatGPT sign-in",
                    name.as_str()
                ));
            }
        }
        let value = HeaderValue::from_str(value)
            .map_err(|error| format!("interface transport header value is invalid: {error}"))?;
        headers.insert(name, value);
    }
    Ok(ImageTransport {
        endpoint,
        headers,
        auth,
        max_response_bytes,
    })
}

fn transport_timeout(
    descriptor: &InterfaceDescriptor,
    interface: &InterfaceId,
) -> Result<Duration, String> {
    let transport = resolved_transport(descriptor, interface)?;
    let timeout_ms = transport
        .request_timeout_ms
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > 30 * 60 * 1000 {
        return Err(format!(
            "image interface '{}' request_timeout_ms must be within 1..=1800000",
            interface
        ));
    }
    Ok(Duration::from_millis(timeout_ms))
}

fn resolved_transport(
    descriptor: &InterfaceDescriptor,
    interface: &InterfaceId,
) -> Result<InterfaceTransportConfig, String> {
    if descriptor.id != *interface {
        return Err(format!(
            "resolved image interface '{}' does not match process interface '{}'",
            descriptor.id, interface
        ));
    }
    descriptor
        .interface_driver_config
        .clone()
        .and_then(|config| config.transport)
        .ok_or_else(|| {
            format!(
                "image interface '{}' resolves no interface_driver_config.transport",
                interface
            )
        })
}

fn resolve_endpoint(transport: &InterfaceTransportConfig) -> Result<Url, String> {
    let configured_override = transport
        .base_url_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|env_key| std::env::var(env_key).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let endpoint = match configured_override {
        Some(endpoint) => endpoint,
        None => {
            let configured = transport.base_url.trim();
            if configured.is_empty() {
                return Err(
                    "image interface endpoint is not configured: set the API URL in the Image Generation setup"
                        .to_string(),
                );
            }
            configured.to_string()
        }
    };
    parse_http_url(&endpoint, "image interface endpoint")
}

fn resolve_auth(
    transport: &InterfaceTransportConfig,
    account: Option<&AccountAuth>,
) -> Result<Option<ImageAuth>, String> {
    let env_key = transport
        .auth_env_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (env_key, account) {
        (Some(_), Some(_)) => Err(
            "image interface declares both an API key and a ChatGPT account sign-in".to_string(),
        ),
        (Some(env_key), None) => Ok(Some(ImageAuth::Key {
            header: parse_header_name(
                transport.auth_header.as_deref().unwrap_or("Authorization"),
                "image interface auth header",
            )?,
            value_prefix: transport
                .auth_value_prefix
                .as_deref()
                .unwrap_or("Bearer ")
                .to_string(),
            env_key: env_key.to_string(),
        })),
        (
            None,
            Some(AccountAuth::ChatgptAccount {
                auth_file_env,
                account_header,
            }),
        ) => {
            if auth_file_env.is_empty()
                || auth_file_env.as_bytes()[0].is_ascii_digit()
                || !auth_file_env
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Err(format!(
                    "private_config.auth.auth_file_env '{auth_file_env}' is not an environment variable name"
                ));
            }
            let account_header =
                parse_header_name(account_header, "private_config.auth.account_header")?;
            if matches!(
                account_header.as_str(),
                "authorization" | "content-type" | "accept"
            ) {
                return Err(format!(
                    "private_config.auth.account_header '{}' is owned by the transport",
                    account_header.as_str()
                ));
            }
            Ok(Some(ImageAuth::ChatgptAccount {
                auth_file_env: auth_file_env.clone(),
                account_header,
            }))
        }
        (None, None) => Ok(None),
    }
}

fn parse_header_name(value: &str, surface: &str) -> Result<HeaderName, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{surface} must not be empty"));
    }
    HeaderName::from_bytes(value.as_bytes())
        .map_err(|error| format!("{surface} '{value}' is invalid: {error}"))
}

fn reject_secret_header(name: &HeaderName) -> Result<(), String> {
    if matches!(
        name.as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    ) {
        return Err(format!(
            "interface transport header '{}' must use a secret handle instead of tracked JSON",
            name.as_str()
        ));
    }
    Ok(())
}

fn parse_http_url(value: &str, surface: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|error| format!("{surface} is invalid: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{surface} must use http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{surface} must not carry credentials"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| format!("{surface} has no host"))?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() == "http" && !loopback {
        return Err(format!("{surface} requires https outside loopback"));
    }
    Ok(url)
}

fn require_allowed_media_origin(
    candidate: &Url,
    endpoint: &Url,
    allow_endpoint_origin: bool,
    allowed_origins: &[String],
) -> Result<(), String> {
    let candidate_origin = http_origin(candidate, "image response URL")?;
    if allow_endpoint_origin
        && candidate_origin == http_origin(endpoint, "image interface endpoint")?
    {
        return Ok(());
    }
    for (index, declared) in allowed_origins.iter().enumerate() {
        if candidate_origin
            == parse_declared_origin(
                declared,
                &format!("interface private_config.allowed_url_origins[{index}]"),
            )?
        {
            return Ok(());
        }
    }
    Err(format!(
        "image response URL origin {}://{}:{} is not declared by the interface",
        candidate_origin.scheme, candidate_origin.host, candidate_origin.port
    ))
}

fn parse_declared_origin(value: &str, surface: &str) -> Result<HttpOrigin, String> {
    require_trimmed(surface, value)?;
    let url = parse_http_url(value, surface)?;
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(format!(
            "{surface} must be an origin without path, query, or fragment"
        ));
    }
    http_origin(&url, surface)
}

fn http_origin(url: &Url, surface: &str) -> Result<HttpOrigin, String> {
    let host = url
        .host_str()
        .ok_or_else(|| format!("{surface} has no host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("{surface} has no effective port"))?;
    Ok(HttpOrigin {
        scheme: url.scheme().to_ascii_lowercase(),
        host: host.to_ascii_lowercase(),
        port,
    })
}

fn checked_limit(value: Option<u64>, default: usize, surface: &str) -> Result<usize, String> {
    let value = value.unwrap_or(default as u64);
    let value = usize::try_from(value).map_err(|_| format!("{surface} exceeds usize"))?;
    if value == 0 || value > ABSOLUTE_MAX_BYTES {
        return Err(format!("{surface} must be within 1..={ABSOLUTE_MAX_BYTES}"));
    }
    Ok(value)
}

fn require_trimmed(surface: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!("{surface} must be non-empty and trimmed"));
    }
    Ok(())
}

fn dotted_value<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return None;
    }
    let mut current = value;
    for part in path.split('.') {
        if part.is_empty() {
            return None;
        }
        current = current.get(part)?;
    }
    Some(current)
}

fn body_evidence(value: &[u8]) -> String {
    format!(
        "bytes={} blake3={}",
        value.len(),
        blake3::hash(value).to_hex()
    )
}

#[cfg(test)]
mod tests;
