use std::collections::BTreeMap;
use std::time::Duration;

use octopus_browser_session_env::{
    bounded_optional_u64, browser_result, evidence_write_error, required_string, url_is_loopback,
};
use octopus_interfaces::InterfaceArtifactFile;
use octopus_tool_door_runtime::room::{
    admitted_tool, decode_tool_call, descriptor_operation, emit_json_artifact, formed_scope,
    strict_object,
};
use octopus_tool_door_runtime::{
    ToolProcessError, ToolProcessHandler, ToolProcessInvocation, ToolProcessIo,
    WorkerEnvironmentLifecycleRecorder,
};
use octopus_tool_handlers::ToolCall;
use octopus_worker_env::WorkerEnvironmentLifecycle;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::{json, Map, Value};

const ROOM: &str = "web search door";
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_RESPONSE_BYTES: usize = 2_000_000;
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WebOperation {
    Search,
    Extract,
}

pub struct WebSearchRoom;

impl WebSearchRoom {
    pub(crate) async fn receive(
        &self,
        invocation: ToolProcessInvocation,
        lifecycle_sink: &dyn WorkerEnvironmentLifecycleRecorder,
    ) -> Result<InterfaceArtifactFile, ToolProcessError> {
        let ToolProcessInvocation {
            descriptor,
            envelope,
            worker_environment,
            ..
        } = invocation;
        let formation = worker_environment.ok_or_else(|| {
            ToolProcessError::Capability(format!(
                "tool '{}' invocation has no formed worker environment",
                descriptor.id
            ))
        })?;
        formed_scope(ROOM, &envelope, &formation)?;
        let artifact = decode_tool_call(ROOM, &envelope)?;
        let tool = admitted_tool(ROOM, &envelope, &artifact)
            .map_err(|error| ToolProcessError::Capability(error.to_string()))?;
        if descriptor.id.as_str() != tool {
            return Err(ToolProcessError::Capability(format!(
                "tool descriptor '{}' does not match tool.call '{}'",
                descriptor.id, tool
            )));
        }
        let call = ToolCall {
            tool_id: tool,
            arguments: artifact.arguments,
            handler_config: descriptor.handler_config.clone(),
        };
        let result = execute(&call).await;
        let lifecycle = if result.is_ok() {
            WorkerEnvironmentLifecycle::Active
        } else {
            WorkerEnvironmentLifecycle::Failed
        };
        let mut evidence_write_errors = Vec::new();
        if let Err(error) = lifecycle_sink.record(&formation, lifecycle).await {
            evidence_write_errors.push(evidence_write_error("lifecycle", error));
        }
        let response = browser_result(
            &call.tool_id,
            artifact.correlation_id,
            result,
            evidence_write_errors,
        );
        emit_json_artifact(&envelope, &response.label, &response)
            .map_err(ToolProcessError::Capability)
    }
}

impl ToolProcessHandler for WebSearchRoom {
    fn substrate_family(&self) -> &str {
        "web_search"
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolProcessInvocation,
        io: ToolProcessIo,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<InterfaceArtifactFile>, ToolProcessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.receive(invocation, &io)
                .await
                .map(|artifact| vec![artifact])
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderCatalog {
    #[serde(rename = "operation")]
    _operation: String,
    #[serde(default)]
    default_provider: Option<String>,
    #[serde(default)]
    request_timeout_ms: Option<u64>,
    #[serde(default)]
    max_response_bytes: Option<usize>,
    #[serde(default)]
    proxy_env: Option<String>,
    providers: BTreeMap<String, HttpProvider>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpProvider {
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    base_url_env: Option<String>,
    #[serde(default)]
    path: String,
    method: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    auth: Option<AuthProjection>,
    #[serde(default)]
    query: RequestProjection,
    #[serde(default)]
    body: RequestProjection,
    response: ResponseProjection,
    #[serde(default)]
    extraction_mode: Option<String>,
    #[serde(default)]
    retry: RetryProjection,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthProjection {
    env: String,
    placement: String,
    name: String,
    #[serde(default)]
    prefix: String,
    #[serde(default = "default_true")]
    required: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestProjection {
    #[serde(default)]
    static_values: Map<String, Value>,
    #[serde(default)]
    argument_fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseProjection {
    #[serde(default)]
    items_path: Option<String>,
    #[serde(default)]
    title_paths: Vec<String>,
    #[serde(default)]
    url_paths: Vec<String>,
    #[serde(default)]
    snippet_paths: Vec<String>,
    #[serde(default)]
    content_paths: Vec<String>,
    #[serde(default)]
    raw_content_paths: Vec<String>,
    #[serde(default)]
    published_paths: Vec<String>,
    #[serde(default)]
    image_paths: Vec<String>,
    #[serde(default)]
    error_paths: Vec<String>,
    #[serde(default)]
    failure_items_paths: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryProjection {
    #[serde(default)]
    max_attempts: Option<u32>,
    #[serde(default)]
    base_delay_ms: Option<u64>,
}

async fn execute(call: &ToolCall) -> Result<Value, String> {
    match web_operation(&call.tool_id, &call.handler_config)? {
        WebOperation::Search => execute_search(call).await,
        WebOperation::Extract => execute_extract(call).await,
    }
}

fn web_operation(tool_id: &str, handler_config: &Value) -> Result<WebOperation, String> {
    match descriptor_operation("web-search", tool_id, handler_config, &["search", "extract"])? {
        "search" => Ok(WebOperation::Search),
        _ => Ok(WebOperation::Extract),
    }
}

async fn execute_search(call: &ToolCall) -> Result<Value, String> {
    let object = strict_object(
        "web.search",
        &call.arguments,
        &[
            "query",
            "limit",
            "provider",
            "include_domains",
            "exclude_domains",
            "search_depth",
            "time_range",
            "country",
            "language",
            "include_raw_content",
        ],
    )?;
    let query = required_string(object, "query")?;
    let limit = bounded_optional_u64(object, "limit", 1, 100)?.unwrap_or(5);
    let catalog = parse_catalog(&call.handler_config, "web.search")?;
    let (provider_id, provider) = select_provider(&catalog, object)?;
    let mut arguments = object.clone();
    arguments.insert("limit".to_string(), json!(limit));
    let parsed = request_json(&catalog, &provider_id, provider, &arguments).await?;
    let items = response_items(&parsed, provider.response.items_path.as_deref())?;
    let candidates = items
        .into_iter()
        .filter_map(|item| search_candidate(&provider.response, item))
        .take(usize::try_from(limit).unwrap_or(usize::MAX))
        .enumerate()
        .map(|(index, mut candidate)| {
            candidate.insert("position".to_string(), json!(index + 1));
            Value::Object(candidate)
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "result": "web.search",
        "provider": provider_id,
        "query": query,
        "candidate_count": candidates.len(),
        "candidates": candidates,
    }))
}

async fn execute_extract(call: &ToolCall) -> Result<Value, String> {
    let object = strict_object(
        "web.extract",
        &call.arguments,
        &[
            "urls",
            "provider",
            "formats",
            "max_chars",
            "include_raw_content",
        ],
    )?;
    let urls = required_urls(object)?;
    let max_chars = bounded_optional_u64(object, "max_chars", 1, 2_000_000)?
        .map(|value| usize::try_from(value).unwrap_or(usize::MAX));
    let catalog = parse_catalog(&call.handler_config, "web.extract")?;
    let (provider_id, provider) = select_provider(&catalog, object)?;
    let mode = provider.extraction_mode.as_deref().unwrap_or("batch");
    let documents = match mode {
        "batch" => {
            let parsed = request_json(&catalog, &provider_id, provider, object).await?;
            extract_documents(&provider.response, &parsed, None, max_chars)?
        }
        "per_url" => {
            let mut documents = Vec::new();
            for url in &urls {
                let mut arguments = object.clone();
                arguments.insert("url".to_string(), Value::String(url.clone()));
                match request_json(&catalog, &provider_id, provider, &arguments).await {
                    Ok(parsed) => documents.extend(extract_documents(
                        &provider.response,
                        &parsed,
                        Some(url),
                        max_chars,
                    )?),
                    Err(error) => documents.push(json!({
                        "url": url,
                        "title": "",
                        "content": "",
                        "raw_content": "",
                        "error": error,
                        "metadata": {"sourceURL": url},
                    })),
                }
            }
            documents
        }
        other => {
            return Err(format!(
                "web.extract provider '{provider_id}' has unsupported extraction_mode '{other}'"
            ));
        }
    };
    Ok(json!({
        "result": "web.extract",
        "provider": provider_id,
        "document_count": documents.len(),
        "documents": documents,
    }))
}

fn parse_catalog(handler_config: &Value, tool: &str) -> Result<ProviderCatalog, String> {
    if handler_config.is_null() {
        return Err(format!("{tool} descriptor carries no provider catalog"));
    }
    let catalog: ProviderCatalog = serde_json::from_value(handler_config.clone())
        .map_err(|error| format!("{tool} provider catalog is malformed: {error}"))?;
    if catalog.providers.is_empty() {
        return Err(format!("{tool} provider catalog is empty"));
    }
    validate_limits(&catalog)?;
    Ok(catalog)
}

fn validate_limits(catalog: &ProviderCatalog) -> Result<(), String> {
    let timeout_ms = catalog
        .request_timeout_ms
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > 300_000 {
        return Err("web provider request_timeout_ms must be between 1 and 300000".to_string());
    }
    let max_response_bytes = catalog
        .max_response_bytes
        .unwrap_or(DEFAULT_MAX_RESPONSE_BYTES);
    if max_response_bytes == 0 || max_response_bytes > 20_000_000 {
        return Err("web provider max_response_bytes must be between 1 and 20000000".to_string());
    }
    Ok(())
}

fn select_provider<'a>(
    catalog: &'a ProviderCatalog,
    arguments: &Map<String, Value>,
) -> Result<(String, &'a HttpProvider), String> {
    let requested = arguments
        .get("provider")
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "provider must be a non-empty string".to_string())
        })
        .transpose()?;
    let provider_id = requested
        .or(catalog.default_provider.as_deref().map(str::trim))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "web tool requires an explicit provider".to_string())?;
    let provider = catalog.providers.get(provider_id).ok_or_else(|| {
        format!(
            "web provider '{provider_id}' is not declared; available providers: {}",
            catalog
                .providers
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    Ok((provider_id.to_string(), provider))
}

async fn request_json(
    catalog: &ProviderCatalog,
    provider_id: &str,
    provider: &HttpProvider,
    arguments: &Map<String, Value>,
) -> Result<Value, String> {
    let endpoint = provider_endpoint(provider_id, provider)?;
    let method = match provider.method.trim().to_ascii_uppercase().as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        other => {
            return Err(format!(
                "web provider '{provider_id}' has unsupported method '{other}'"
            ));
        }
    };
    let mut query = projected_values(&provider.query, arguments)?;
    let mut body = projected_values(&provider.body, arguments)?;
    let mut headers = provider.headers.clone();
    let secrets = apply_auth(
        provider_id,
        provider.auth.as_ref(),
        &mut headers,
        &mut query,
        &mut body,
    )?;
    let timeout_ms = catalog
        .request_timeout_ms
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
    let max_response_bytes = catalog
        .max_response_bytes
        .unwrap_or(DEFAULT_MAX_RESPONSE_BYTES);
    let max_attempts = provider.retry.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS);
    if max_attempts == 0 || max_attempts > 8 {
        return Err(format!(
            "web provider '{provider_id}' retry.max_attempts must be between 1 and 8"
        ));
    }
    let base_delay_ms = provider
        .retry
        .base_delay_ms
        .unwrap_or(DEFAULT_RETRY_BASE_DELAY_MS);
    if base_delay_ms > 30_000 {
        return Err(format!(
            "web provider '{provider_id}' retry.base_delay_ms must not exceed 30000"
        ));
    }
    let mut client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(reqwest::redirect::Policy::none());
    if let Some(proxy) = explicit_proxy(catalog.proxy_env.as_deref())? {
        client = client.proxy(proxy);
    }
    let client = client
        .build()
        .map_err(|error| format!("web provider HTTP client build failed: {error}"))?;
    let mut last_error = None;
    for attempt in 1..=max_attempts {
        let request = build_request(
            &client,
            method.clone(),
            endpoint.clone(),
            &headers,
            &query,
            &body,
        )?;
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                return read_json_response(provider_id, response, max_response_bytes)
                    .await
                    .map_err(|error| sanitize_web_error(&error, &secrets));
            }
            Ok(response) => {
                let status = response.status();
                let retry_after = retry_after_ms(&response);
                let body = bounded_response_text(response, max_response_bytes).await?;
                let error = sanitize_web_error(
                    &format!(
                        "web provider '{provider_id}' returned HTTP {} ({})",
                        status.as_u16(),
                        body_evidence(body.as_bytes())
                    ),
                    &secrets,
                );
                if !retryable_status(status) || attempt == max_attempts {
                    return Err(error);
                }
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(
                    retry_after.unwrap_or_else(|| retry_delay(base_delay_ms, attempt)),
                ))
                .await;
            }
            Err(error) => {
                let message = sanitize_web_error(
                    &format!(
                        "web provider '{provider_id}' request to '{endpoint}' failed: {error}"
                    ),
                    &secrets,
                );
                if attempt == max_attempts {
                    return Err(message);
                }
                last_error = Some(message);
                tokio::time::sleep(Duration::from_millis(retry_delay(base_delay_ms, attempt)))
                    .await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| format!("web provider '{provider_id}' request failed")))
}

fn provider_endpoint(provider_id: &str, provider: &HttpProvider) -> Result<Url, String> {
    let env_url = provider
        .base_url_env
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|key| std::env::var(key).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let base = env_url
        .as_deref()
        .unwrap_or_else(|| provider.base_url.trim());
    if base.is_empty() {
        return Err(format!(
            "web provider '{provider_id}' has no base URL; configure '{}' or descriptor base_url",
            provider.base_url_env.as_deref().unwrap_or("base_url_env")
        ));
    }
    let endpoint = if provider.path.trim().is_empty() {
        base.to_string()
    } else {
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            provider.path.trim().trim_start_matches('/')
        )
    };
    let parsed = Url::parse(&endpoint)
        .map_err(|error| format!("web provider '{provider_id}' endpoint is invalid: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "web provider '{provider_id}' endpoint must use http or https"
        ));
    }
    if parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(format!(
            "web provider '{provider_id}' endpoint must have a host and no userinfo, query, or fragment"
        ));
    }
    if parsed.scheme() == "http" && !url_is_loopback(&parsed) {
        return Err(format!(
            "web provider '{provider_id}' endpoint must use https outside loopback"
        ));
    }
    Ok(parsed)
}

fn explicit_proxy(env_name: Option<&str>) -> Result<Option<reqwest::Proxy>, String> {
    let Some(env_name) = env_name.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if env_name
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err("web provider proxy_env is malformed".to_string());
    }
    let Some(raw) = std::env::var(env_name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let mut url = Url::parse(&raw)
        .map_err(|_| format!("web provider proxy env var '{env_name}' is malformed"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "web provider proxy env var '{env_name}' must be an HTTP(S) origin"
        ));
    }
    let username = if url.username().is_empty() {
        None
    } else {
        Some(
            percent_decode_str(url.username())
                .decode_utf8()
                .map_err(|_| {
                    format!("web provider proxy env var '{env_name}' has malformed credentials")
                })?
                .into_owned(),
        )
    };
    let password = url
        .password()
        .map(|value| {
            percent_decode_str(value)
                .decode_utf8()
                .map(|value| value.into_owned())
                .map_err(|_| {
                    format!("web provider proxy env var '{env_name}' has malformed credentials")
                })
        })
        .transpose()?;
    if username.is_some() != password.is_some()
        || username.as_deref().is_some_and(str::is_empty)
        || password.as_deref().is_some_and(str::is_empty)
    {
        return Err(format!(
            "web provider proxy env var '{env_name}' requires a non-empty username and password"
        ));
    }
    if username.is_some() && url.scheme() != "https" && !url_is_loopback(&url) {
        return Err(format!(
            "web provider authenticated proxy env var '{env_name}' must use HTTPS outside loopback"
        ));
    }
    url.set_username("").map_err(|_| {
        format!("web provider proxy env var '{env_name}' could not remove credentials")
    })?;
    url.set_password(None).map_err(|_| {
        format!("web provider proxy env var '{env_name}' could not remove credentials")
    })?;
    let mut proxy = reqwest::Proxy::all(url.as_str())
        .map_err(|error| format!("web provider proxy env var '{env_name}' is invalid: {error}"))?;
    if let (Some(username), Some(password)) = (username, password) {
        proxy = proxy.basic_auth(&username, &password);
    }
    Ok(Some(proxy))
}

fn apply_auth(
    provider_id: &str,
    auth: Option<&AuthProjection>,
    headers: &mut BTreeMap<String, String>,
    query: &mut Map<String, Value>,
    body: &mut Map<String, Value>,
) -> Result<Vec<String>, String> {
    let Some(auth) = auth else {
        return Ok(Vec::new());
    };
    let env = auth.env.trim();
    let name = auth.name.trim();
    if env.is_empty() || name.is_empty() {
        return Err(format!(
            "web provider '{provider_id}' auth env/name must not be blank"
        ));
    }
    let secret = std::env::var(env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(secret) = secret else {
        if auth.required {
            return Err(format!(
                "web provider '{provider_id}' auth env var '{env}' is unset/empty"
            ));
        }
        return Ok(Vec::new());
    };
    let value = format!("{}{}", auth.prefix, secret);
    let secrets = vec![secret, value.clone()];
    match auth.placement.as_str() {
        "header" => {
            headers.insert(name.to_string(), value);
        }
        "query" => set_dotted(query, name, Value::String(value))?,
        "body" => set_dotted(body, name, Value::String(value))?,
        other => {
            return Err(format!(
                "web provider '{provider_id}' auth placement '{other}' is unsupported"
            ));
        }
    }
    Ok(secrets)
}

fn sanitize_web_error(value: &str, secrets: &[String]) -> String {
    let mut result = value.to_string();
    for secret in secrets {
        if secret.len() < 4 {
            continue;
        }
        result = result.replace(secret, "[secret-redacted]");
        result = result.replace(
            &utf8_percent_encode(secret, NON_ALPHANUMERIC).to_string(),
            "[secret-redacted]",
        );
    }
    result
}

fn projected_values(
    projection: &RequestProjection,
    arguments: &Map<String, Value>,
) -> Result<Map<String, Value>, String> {
    let mut values = projection.static_values.clone();
    for (argument, field) in &projection.argument_fields {
        if let Some(value) = arguments.get(argument) {
            set_dotted(&mut values, field, value.clone())?;
        }
    }
    Ok(values)
}

fn set_dotted(root: &mut Map<String, Value>, path: &str, value: Value) -> Result<(), String> {
    let segments = path
        .split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let Some((last, parents)) = segments.split_last() else {
        return Err("web provider projection path must not be blank".to_string());
    };
    let mut current = root;
    for segment in parents {
        let entry = current
            .entry((*segment).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        current = entry.as_object_mut().ok_or_else(|| {
            format!("web provider projection path '{path}' crosses a non-object value")
        })?;
    }
    current.insert((*last).to_string(), value);
    Ok(())
}

fn build_request(
    client: &reqwest::Client,
    method: Method,
    endpoint: Url,
    headers: &BTreeMap<String, String>,
    query: &Map<String, Value>,
    body: &Map<String, Value>,
) -> Result<reqwest::RequestBuilder, String> {
    let mut request = client.request(method.clone(), endpoint);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let query_pairs = query
        .iter()
        .flat_map(|(name, value)| query_values(name, value))
        .collect::<Result<Vec<_>, _>>()?;
    if !query_pairs.is_empty() {
        request = request.query(&query_pairs);
    }
    if method != Method::GET && !body.is_empty() {
        request = request.json(&Value::Object(body.clone()));
    }
    Ok(request)
}

fn query_values(name: &str, value: &Value) -> Vec<Result<(String, String), String>> {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|value| scalar_query_value(value).map(|value| (name.to_string(), value)))
            .collect(),
        value => vec![scalar_query_value(value).map(|value| (name.to_string(), value))],
    }
}

fn scalar_query_value(value: &Value) -> Result<String, String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        _ => Err("web provider query projection accepts only scalars or scalar arrays".to_string()),
    }
}

async fn read_json_response(
    provider_id: &str,
    response: reqwest::Response,
    max_response_bytes: usize,
) -> Result<Value, String> {
    let text = bounded_response_text(response, max_response_bytes).await?;
    serde_json::from_str(&text).map_err(|error| {
        format!(
            "web provider '{provider_id}' response is not valid JSON: {error} ({})",
            body_evidence(text.as_bytes())
        )
    })
}

async fn bounded_response_text(
    mut response: reqwest::Response,
    max_response_bytes: usize,
) -> Result<String, String> {
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes as u64)
    {
        return Err(format!(
            "web provider response exceeds {max_response_bytes} bytes"
        ));
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(max_response_bytes),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("web provider response body read failed: {error}"))?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| "web provider response length overflowed".to_string())?;
        if next_len > max_response_bytes {
            return Err(format!(
                "web provider response exceeds {max_response_bytes} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|error| format!("web provider response is not UTF-8: {error}"))
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retry_after_ms(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1_000).min(30_000))
}

fn retry_delay(base_delay_ms: u64, attempt: u32) -> u64 {
    base_delay_ms
        .saturating_mul(1u64 << attempt.saturating_sub(1).min(6))
        .min(30_000)
}

fn response_items<'a>(parsed: &'a Value, path: Option<&str>) -> Result<Vec<&'a Value>, String> {
    let selected = match path.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => value_by_dotted(parsed, path)
            .ok_or_else(|| format!("web provider response has no items path '{path}'"))?,
        None => parsed,
    };
    match selected {
        Value::Array(items) => Ok(items.iter().collect()),
        Value::Object(_) => Ok(vec![selected]),
        _ => Err("web provider response items must be an array or object".to_string()),
    }
}

fn search_candidate(shape: &ResponseProjection, item: &Value) -> Option<Map<String, Value>> {
    let url = first_text(item, &shape.url_paths)?;
    if url.trim().is_empty() {
        return None;
    }
    let mut candidate = Map::new();
    candidate.insert(
        "title".to_string(),
        json!(first_text(item, &shape.title_paths).unwrap_or_default()),
    );
    candidate.insert("url".to_string(), json!(url));
    candidate.insert(
        "snippet".to_string(),
        json!(first_text(item, &shape.snippet_paths).unwrap_or_default()),
    );
    if let Some(value) = first_text(item, &shape.published_paths) {
        candidate.insert("published_at".to_string(), json!(value));
    }
    if let Some(value) = first_text(item, &shape.image_paths) {
        candidate.insert("image_url".to_string(), json!(value));
    }
    Some(candidate)
}

fn extract_documents(
    shape: &ResponseProjection,
    parsed: &Value,
    fallback_url: Option<&str>,
    max_chars: Option<usize>,
) -> Result<Vec<Value>, String> {
    let mut documents = response_items(parsed, shape.items_path.as_deref())?
        .into_iter()
        .map(|item| extract_document(shape, item, fallback_url, max_chars, None))
        .collect::<Vec<_>>();
    for failure_path in &shape.failure_items_paths {
        let Some(Value::Array(items)) = value_by_dotted(parsed, failure_path) else {
            continue;
        };
        documents.extend(items.iter().map(|item| {
            extract_document(
                shape,
                item,
                fallback_url,
                max_chars,
                Some("extraction failed"),
            )
        }));
    }
    Ok(documents)
}

fn extract_document(
    shape: &ResponseProjection,
    item: &Value,
    fallback_url: Option<&str>,
    max_chars: Option<usize>,
    fallback_error: Option<&str>,
) -> Value {
    let url = first_text(item, &shape.url_paths)
        .or_else(|| item.as_str().map(str::to_string))
        .or_else(|| fallback_url.map(str::to_string))
        .unwrap_or_default();
    let title = first_text(item, &shape.title_paths).unwrap_or_default();
    let content = bounded_text(
        first_text(item, &shape.content_paths).unwrap_or_default(),
        max_chars,
    );
    let raw_content = bounded_text(
        first_text(item, &shape.raw_content_paths).unwrap_or_else(|| content.clone()),
        max_chars,
    );
    let error = first_text(item, &shape.error_paths).or_else(|| fallback_error.map(str::to_string));
    let mut document = Map::new();
    document.insert("url".to_string(), json!(url));
    document.insert("title".to_string(), json!(title));
    document.insert("content".to_string(), json!(content));
    document.insert("raw_content".to_string(), json!(raw_content));
    document.insert(
        "metadata".to_string(),
        json!({"sourceURL": url, "title": title}),
    );
    if let Some(error) = error {
        document.insert("error".to_string(), json!(error));
    }
    Value::Object(document)
}

fn value_by_dotted<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        current = match current {
            Value::Object(object) => object.get(segment)?,
            Value::Array(array) => array.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn first_text(value: &Value, paths: &[String]) -> Option<String> {
    paths
        .iter()
        .filter_map(|path| value_by_dotted(value, path))
        .find_map(text_value)
}

fn text_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Array(values) => {
            let joined = values
                .iter()
                .filter_map(text_value)
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.is_empty()).then_some(joined)
        }
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn required_urls(object: &Map<String, Value>) -> Result<Vec<String>, String> {
    let urls = object
        .get("urls")
        .and_then(Value::as_array)
        .ok_or_else(|| "urls must be a non-empty array of HTTP(S) URLs".to_string())?;
    if urls.is_empty() || urls.len() > 50 {
        return Err("urls must contain between 1 and 50 entries".to_string());
    }
    urls.iter()
        .map(|value| {
            let raw = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "each urls entry must be a non-empty string".to_string())?;
            let parsed =
                Url::parse(raw).map_err(|error| format!("URL '{raw}' is invalid: {error}"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(format!("URL '{raw}' must use http or https"));
            }
            Ok(raw.to_string())
        })
        .collect()
}

fn bounded_text(value: String, max_chars: Option<usize>) -> String {
    let Some(max_chars) = max_chars else {
        return value;
    };
    if value.chars().count() <= max_chars {
        return value;
    }
    value.chars().take(max_chars).collect()
}

fn body_evidence(value: &[u8]) -> String {
    format!(
        "bytes={} blake3={}",
        value.len(),
        blake3::hash(value).to_hex()
    )
}

fn default_true() -> bool {
    true
}
