use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{InterfaceArtifactFile, InterfaceDescriptor, InterfaceTransportConfig};
use octopus_ledger::ContentHash;
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Response, Url};
use serde::Deserialize;
use serde_json::{json, Map, Value};

const RESULT_LABEL: &str = "image.generate.result";
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const ABSOLUTE_MAX_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone)]
struct ImageGenerationProcess {
    transport: ImageTransport,
    max_manifest_bytes: u64,
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
struct ImageAuth {
    header: HeaderName,
    value_prefix: String,
    env_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageOperation {
    input_request_label: String,
    prompt_field: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    size: Option<String>,
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
    #[serde(default, rename = "client_connection")]
    _client_connection: Option<serde::de::IgnoredAny>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImageArguments {
    prompt: String,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

struct GeneratedImage {
    bytes: Vec<u8>,
    revised_prompt: Option<String>,
    source: &'static str,
    endpoint_status: u16,
}

#[derive(Debug)]
struct ImageCallFailure {
    class: &'static str,
    detail: String,
    endpoint_status: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HttpOrigin {
    scheme: String,
    host: String,
    port: u16,
}

impl ImageCallFailure {
    fn contract(detail: impl Into<String>) -> Self {
        Self {
            class: "contract",
            detail: detail.into(),
            endpoint_status: None,
        }
    }

    fn transport(detail: impl Into<String>) -> Self {
        Self {
            class: "transport",
            detail: detail.into(),
            endpoint_status: None,
        }
    }

    fn endpoint(status: u16, detail: impl Into<String>) -> Self {
        Self {
            class: "endpoint",
            detail: detail.into(),
            endpoint_status: Some(status),
        }
    }
}

impl ImageAuth {
    fn header_value(&self) -> Result<HeaderValue, ImageCallFailure> {
        let secret = std::env::var(&self.env_key)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                ImageCallFailure::contract(format!(
                    "image interface auth env var '{}' is unset or empty",
                    self.env_key
                ))
            })?;
        HeaderValue::from_str(&format!("{}{secret}", self.value_prefix)).map_err(|_| {
            ImageCallFailure::contract(format!(
                "image interface auth env var '{}' cannot form a valid header value",
                self.env_key
            ))
        })
    }
}

impl ImageGenerationProcess {
    async fn generate(
        &self,
        operation: &ImageOperation,
        arguments: &ImageArguments,
    ) -> Result<GeneratedImage, ImageCallFailure> {
        let body = build_request_body(operation, arguments)?;
        let mut request = self
            .client
            .post(self.transport.endpoint.clone())
            .headers(self.transport.headers.clone())
            .json(&body);
        if let Some(auth) = &self.transport.auth {
            request = request.header(auth.header.clone(), auth.header_value()?);
        }
        let response = request.send().await.map_err(|error| {
            ImageCallFailure::transport(format!("image endpoint request failed: {error}"))
        })?;
        let status = response.status().as_u16();
        let response_bytes = read_bounded(
            response,
            self.transport.max_response_bytes,
            "image endpoint response",
        )
        .await?;
        if !(200..300).contains(&status) {
            return Err(ImageCallFailure::endpoint(
                status,
                format!(
                    "image endpoint refused the request ({})",
                    body_evidence(&response_bytes)
                ),
            ));
        }
        let body: Value = serde_json::from_slice(&response_bytes).map_err(|error| {
            ImageCallFailure::contract(format!(
                "image endpoint response is not valid JSON: {error}"
            ))
        })?;
        let entry = dotted_value(&body, operation.data_path.trim())
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .ok_or_else(|| {
                ImageCallFailure::contract(format!(
                    "image endpoint response has no first entry at '{}'",
                    operation.data_path.trim()
                ))
            })?;
        let revised_prompt = operation
            .revised_prompt_field
            .as_deref()
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .and_then(|field| entry.get(field))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let max_image_bytes = checked_limit(
            operation.max_image_bytes,
            DEFAULT_MAX_IMAGE_BYTES,
            "interface private_config.max_image_bytes",
        )
        .map_err(ImageCallFailure::contract)?;
        if let Some(encoded) = entry
            .get(operation.b64_field.trim())
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let maximum_decoded = encoded
                .len()
                .checked_add(3)
                .and_then(|value| value.checked_div(4))
                .and_then(|value| value.checked_mul(3))
                .ok_or_else(|| ImageCallFailure::contract("base64 image size overflowed"))?;
            if maximum_decoded > max_image_bytes.saturating_add(2) {
                return Err(ImageCallFailure::contract(format!(
                    "base64 image exceeds max_image_bytes={max_image_bytes}"
                )));
            }
            let bytes = BASE64_STANDARD
                .decode(encoded.as_bytes())
                .map_err(|error| {
                    ImageCallFailure::contract(format!("base64 image payload is invalid: {error}"))
                })?;
            if bytes.is_empty() || bytes.len() > max_image_bytes {
                return Err(ImageCallFailure::contract(format!(
                    "decoded image byte count {} is outside 1..={max_image_bytes}",
                    bytes.len()
                )));
            }
            return Ok(GeneratedImage {
                bytes,
                revised_prompt,
                source: "inline_base64",
                endpoint_status: status,
            });
        }
        if let Some(url) = entry
            .get(operation.url_field.trim())
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let media_url =
                parse_http_url(url, "image response URL").map_err(ImageCallFailure::contract)?;
            require_allowed_media_origin(
                &media_url,
                &self.transport.endpoint,
                operation.allow_endpoint_origin,
                &operation.allowed_url_origins,
            )
            .map_err(ImageCallFailure::contract)?;
            let media_response = self.client.get(media_url).send().await.map_err(|error| {
                ImageCallFailure::transport(format!("generated image download failed: {error}"))
            })?;
            let media_status = media_response.status().as_u16();
            let bytes = read_bounded(media_response, max_image_bytes, "generated image").await?;
            if !(200..300).contains(&media_status) {
                return Err(ImageCallFailure::endpoint(
                    media_status,
                    "generated image URL refused the download",
                ));
            }
            if bytes.is_empty() {
                return Err(ImageCallFailure::contract(
                    "generated image URL returned zero bytes",
                ));
            }
            return Ok(GeneratedImage {
                bytes,
                revised_prompt,
                source: "url",
                endpoint_status: status,
            });
        }
        Err(ImageCallFailure::contract(format!(
            "image response entry carries neither '{}' nor '{}'",
            operation.b64_field.trim(),
            operation.url_field.trim()
        )))
    }
}

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(run_image_generation_interface_life(ctx)) as InterfaceLifeFuture)
}

async fn run_image_generation_interface_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let limits = ctx.required_host_io_limits()?;
    let operation = parse_operation(&descriptor, &ctx.interface)?;
    let transport = load_transport(&descriptor, &ctx.interface)?;
    let timeout = transport_timeout(&descriptor, &ctx.interface)?;
    let client = Client::builder()
        .no_proxy()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("image interface HTTP client build failed: {error}"))?;
    let process = ImageGenerationProcess {
        transport,
        max_manifest_bytes: limits.max_manifest_bytes,
        client,
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
            let envelope = match reader.envelope(&own) {
                Ok(envelope) => envelope,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_image_input",
                        json!({ "error": error }),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let manifest = match envelope.read_artifact_manifest_bounded(process.max_manifest_bytes)
            {
                Ok(manifest) => manifest,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_image_input",
                        json!({ "error": error.to_string() }),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            if manifest.manifest.label != operation.input_request_label {
                reader.commit_through(&own)?;
                continue;
            }
            let artifact =
                match envelope.read_artifact_payload_bounded(limits.max_control_frame_bytes) {
                    Ok(payload_bytes) => payload_bytes,
                    Err(error) => {
                        ctx.hand_admitted_failure(
                            own.movement.seq.0,
                            "poisoned_image_request",
                            json!({ "error": error.to_string() }),
                        )
                        .await?;
                        reader.commit_through(&own)?;
                        continue;
                    }
                };
            let arguments: ImageArguments = match serde_json::from_slice(&artifact) {
                Ok(arguments) => arguments,
                Err(error) => {
                    ctx.hand_admitted_failure(
                        own.movement.seq.0,
                        "poisoned_image_request",
                        json!({ "error": error.to_string() }),
                    )
                    .await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            emit_image_result(&ctx, &process, &operation, &arguments, own.movement.seq.0).await?;
            reader.commit_through(&own)?;
        }
    }
}

async fn emit_image_result(
    ctx: &InterfaceLiftContext,
    process: &ImageGenerationProcess,
    operation: &ImageOperation,
    arguments: &ImageArguments,
    source_movement_seq: u64,
) -> Result<(), String> {
    let prompt = arguments.prompt.trim();
    let emitted_root = ctx.interface_var_dir().join("emitted");
    match process.generate(operation, arguments).await {
        Ok(generated) => {
            let image = InterfaceArtifactFile::write(
                &emitted_root,
                operation.media_label.trim(),
                &generated.bytes,
            )
            .map_err(|error| {
                format!("image interface could not materialize generated media: {error}")
            })?;
            let image_ref = artifact_identity(&image, process.max_manifest_bytes)?;
            let result = materialize_json(
                &emitted_root,
                RESULT_LABEL,
                &json!({
                    "interface": ctx.interface.as_str(),
                    "ok": true,
                    "prompt": prompt,
                    "source_movement_seq": source_movement_seq,
                    "revised_prompt": generated.revised_prompt,
                    "source": generated.source,
                    "endpoint_status": generated.endpoint_status,
                    "byte_count": generated.bytes.len(),
                    "media_label": operation.media_label.trim(),
                    "artifact": image_ref,
                }),
            )?;
            ctx.hand_admitted_arrival(image, Vec::new()).await?;
            ctx.hand_admitted_arrival(result, Vec::new()).await?;
            Ok(())
        }
        Err(failure) => {
            let result = materialize_json(
                &emitted_root,
                RESULT_LABEL,
                &json!({
                    "interface": ctx.interface.as_str(),
                    "ok": false,
                    "prompt": prompt,
                    "source_movement_seq": source_movement_seq,
                    "error": {
                        "class": failure.class,
                        "detail": failure.detail,
                        "endpoint_status": failure.endpoint_status,
                    },
                }),
            )?;
            ctx.hand_admitted_arrival(result, Vec::new()).await?;
            Ok(())
        }
    }
}

fn parse_operation(
    descriptor: &InterfaceDescriptor,
    interface: &InterfaceId,
) -> Result<ImageOperation, String> {
    let config = descriptor
        .interface_driver_config
        .as_ref()
        .ok_or_else(|| format!("image interface '{}' resolves no driver config", interface))?;
    let operation: ImageOperation =
        serde_json::from_value(Value::Object(config.private_config.clone())).map_err(|error| {
            format!(
                "image interface '{}' private_config is malformed: {error}",
                interface
            )
        })?;
    for (name, value) in [
        (
            "input_request_label",
            operation.input_request_label.as_str(),
        ),
        ("prompt_field", operation.prompt_field.as_str()),
        ("data_path", operation.data_path.as_str()),
        ("b64_field", operation.b64_field.as_str()),
        ("url_field", operation.url_field.as_str()),
        ("media_label", operation.media_label.as_str()),
    ] {
        require_trimmed(name, value)?;
    }
    if operation.n.is_some_and(|n| n != 1) {
        return Err(format!(
            "image interface '{}' private_config.n must be exactly 1",
            interface
        ));
    }
    checked_limit(
        operation.max_image_bytes,
        DEFAULT_MAX_IMAGE_BYTES,
        "interface private_config.max_image_bytes",
    )?;
    let mut origins = BTreeSet::new();
    for (index, origin) in operation.allowed_url_origins.iter().enumerate() {
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
    Ok(operation)
}

fn build_request_body(
    operation: &ImageOperation,
    arguments: &ImageArguments,
) -> Result<Value, ImageCallFailure> {
    let prompt = arguments.prompt.trim();
    if prompt.is_empty() {
        return Err(ImageCallFailure::contract("image prompt must not be blank"));
    }
    let mut body = Map::new();
    insert_request_field(
        &mut body,
        operation.prompt_field.trim(),
        Value::String(prompt.to_string()),
    )?;
    let model = arguments
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            operation
                .model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        });
    if let Some(model) = model {
        insert_request_field(&mut body, "model", Value::String(model.to_string()))?;
    }
    let size = arguments
        .size
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            operation
                .size
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        });
    if let Some(size) = size {
        insert_request_field(&mut body, "size", Value::String(size.to_string()))?;
    }
    if let Some(n) = operation.n {
        insert_request_field(&mut body, "n", Value::Number(n.into()))?;
    }
    if let Some(response_format) = operation
        .response_format
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        insert_request_field(
            &mut body,
            "response_format",
            Value::String(response_format.to_string()),
        )?;
    }
    Ok(Value::Object(body))
}

fn insert_request_field(
    body: &mut Map<String, Value>,
    name: &str,
    value: Value,
) -> Result<(), ImageCallFailure> {
    if name.is_empty() {
        return Err(ImageCallFailure::contract(
            "image request field name must not be empty",
        ));
    }
    if body.insert(name.to_string(), value).is_some() {
        return Err(ImageCallFailure::contract(format!(
            "image request field '{name}' is declared more than once"
        )));
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
            return Err(ImageCallFailure::contract(format!(
                "{surface} content-length {length} exceeds {max_bytes} bytes"
            )));
        }
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        ImageCallFailure::transport(format!("{surface} body read failed: {error}"))
    })? {
        let next = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| ImageCallFailure::contract(format!("{surface} size overflowed")))?;
        if next > max_bytes {
            return Err(ImageCallFailure::contract(format!(
                "{surface} exceeds {max_bytes} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn load_transport(
    descriptor: &InterfaceDescriptor,
    interface: &InterfaceId,
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
        let value = HeaderValue::from_str(value)
            .map_err(|error| format!("interface transport header value is invalid: {error}"))?;
        headers.insert(name, value);
    }
    let auth = resolve_auth(&transport)?;
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

fn resolve_auth(transport: &InterfaceTransportConfig) -> Result<Option<ImageAuth>, String> {
    let Some(env_key) = transport
        .auth_env_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let name = parse_header_name(
        transport.auth_header.as_deref().unwrap_or("Authorization"),
        "image interface auth header",
    )?;
    Ok(Some(ImageAuth {
        header: name,
        value_prefix: transport
            .auth_value_prefix
            .as_deref()
            .unwrap_or("Bearer ")
            .to_string(),
        env_key: env_key.to_string(),
    }))
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

fn materialize_json(
    root: &Path,
    label: &str,
    value: &Value,
) -> Result<InterfaceArtifactFile, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("image result artifact did not encode: {error}"))?;
    InterfaceArtifactFile::write(root, label, &bytes)
        .map_err(|error| format!("image result artifact could not be written: {error}"))
}

fn artifact_identity(
    artifact: &InterfaceArtifactFile,
    max_manifest_bytes: u64,
) -> Result<Value, String> {
    let bounded = artifact
        .read_manifest_bounded(max_manifest_bytes)
        .map_err(|error| format!("image artifact manifest is invalid: {error}"))?;
    let content = ContentHash::of_bytes(&bounded.manifest_bytes).to_hex();
    Ok(json!({
        "id": format!("{}:{content}", bounded.manifest.label),
        "content": content,
        "payload_content": bounded.manifest.payload_content_hash,
    }))
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
