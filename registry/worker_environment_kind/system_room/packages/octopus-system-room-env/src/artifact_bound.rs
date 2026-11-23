use octopus_worker_env::{ArtifactSurface, BoundArtifact};

use octopus_tool_handlers::{
    ToolCall, ToolHandler, ToolHandlerContext, ToolHandlerError, ToolHandlerResult,
    optional_string_argument, required_string_argument,
};

pub struct ArtifactBoundHandler;

impl ToolHandler for ArtifactBoundHandler {
    fn execute<'a>(
        &'a self,
        ctx: &'a ToolHandlerContext,
        call: &'a ToolCall,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<ToolHandlerResult, ToolHandlerError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { execute(ctx, call).await })
    }
}

async fn execute(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let operation = descriptor_operation(call)?;
    let surface = ctx
        .artifacts
        .as_ref()
        .ok_or_else(|| ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason: "no bound artifact surface leased to this room".to_string(),
        })?;
    match operation {
        "append" => append(surface.as_ref(), call).await,
        "read" => read(surface.as_ref(), call).await,
        "list" => list(surface.as_ref(), call).await,
        "search" => search(surface.as_ref(), call).await,
        other => Err(ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason: format!("artifact-bound operation '{other}' was not dispatched"),
        }),
    }
}

fn descriptor_operation(call: &ToolCall) -> Result<&str, ToolHandlerError> {
    let config = call
        .handler_config
        .as_object()
        .ok_or_else(|| ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason: "handler_config must be an object with an operation".to_string(),
        })?;
    let operation = config
        .get("operation")
        .ok_or_else(|| ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason: "handler_config.operation is absent".to_string(),
        })?
        .as_str()
        .ok_or_else(|| ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason: "handler_config.operation must be a string".to_string(),
        })?;
    if operation.is_empty() || operation.trim() != operation {
        return Err(ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason: "handler_config.operation must be a non-empty trimmed string".to_string(),
        });
    }
    match operation {
        "append" | "read" | "list" | "search" => Ok(operation),
        other => Err(ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason: format!("unknown artifact-bound operation '{other}'"),
        }),
    }
}

async fn append(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let label = required_string_argument(tool_id, &call.arguments, "label")?;
    let payload = match (call.arguments.get("payload"), call.arguments.get("record")) {
        (Some(payload), None) => payload.clone(),
        (None, Some(record)) => record.clone(),
        (Some(_), Some(_)) => {
            return Err(ToolHandlerError::InvalidArguments {
                tool_id: tool_id.clone(),
                reason: "arguments must carry exactly one of 'payload' or 'record'".to_string(),
            });
        }
        (None, None) => {
            return Err(ToolHandlerError::InvalidArguments {
                tool_id: tool_id.clone(),
                reason: "arguments must include 'payload' or 'record'".to_string(),
            });
        }
    };
    let bytes = serde_json::to_vec(&payload).map_err(|e| ToolHandlerError::InvalidArguments {
        tool_id: tool_id.clone(),
        reason: format!("payload is not encodable JSON: {e}"),
    })?;
    let artifact = surface
        .append(&label, &bytes)
        .await
        .map_err(|e| execution(tool_id, e))?;
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "artifact_appended",
        "artifact": {
            "id": artifact.id.as_str(),
            "content": artifact.content.to_hex(),
        },
    })))
}

async fn read(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let id = required_string_argument(tool_id, &call.arguments, "id")?;
    let default_max_bytes = required_handler_usize(call, "default_max_bytes")?;
    let max_max_bytes = required_handler_usize(call, "max_max_bytes")?;
    if default_max_bytes > max_max_bytes {
        return Err(ToolHandlerError::Execution {
            tool_id: tool_id.clone(),
            reason: "handler_config.default_max_bytes exceeds max_max_bytes".to_string(),
        });
    }
    let max_bytes =
        bounded_optional_usize_argument(tool_id, &call.arguments, "max_bytes", 4, max_max_bytes)?
            .unwrap_or(default_max_bytes);
    let offset_bytes =
        optional_nonnegative_usize_argument(tool_id, &call.arguments, "offset_bytes")?.unwrap_or(0);
    let bytes = surface
        .read(&id)
        .await
        .map_err(|e| execution(tool_id, e))?
        .ok_or_else(|| ToolHandlerError::Execution {
            tool_id: tool_id.clone(),
            reason: format!("artifact '{id}' is not bound to this run"),
        })?;
    let text = String::from_utf8(bytes).map_err(|_| ToolHandlerError::Execution {
        tool_id: tool_id.clone(),
        reason: format!(
            "artifact '{id}' is not a text surface; a supporting interface must materialize it"
        ),
    })?;
    if offset_bytes > text.len() || !text.is_char_boundary(offset_bytes) {
        return Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.clone(),
            reason: format!(
                "offset_bytes {offset_bytes} is not a UTF-8 boundary within artifact '{id}' of {} bytes",
                text.len()
            ),
        });
    }
    let mut end = offset_bytes.saturating_add(max_bytes).min(text.len());
    while end > offset_bytes && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == offset_bytes && offset_bytes < text.len() {
        return Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.clone(),
            reason: format!(
                "max_bytes {max_bytes} cannot retain the next UTF-8 scalar at offset {offset_bytes}"
            ),
        });
    }
    let artifact_size_bytes = text.len();
    let page = &text[offset_bytes..end];
    let truncated = end < artifact_size_bytes;
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "artifact_read",
        "id": id,
        "text": page,
        "artifact_size_bytes": artifact_size_bytes,
        "offset_bytes": offset_bytes,
        "bytes_read": page.len(),
        "next_offset_bytes": truncated.then_some(end),
        "truncated": truncated,
    })))
}

async fn list(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let label = optional_string_argument(tool_id, &call.arguments, "label")?;
    let id_prefix = optional_string_argument(tool_id, &call.arguments, "id_prefix")?;
    let bound = surface.list().await.map_err(|e| execution(tool_id, e))?;
    let mut artifacts = Vec::new();
    for entry in &bound {
        if !matches_filters(entry, label.as_deref(), id_prefix.as_deref()) {
            continue;
        }
        artifacts.push(serde_json::json!({
            "id": entry.artifact.id.as_str(),
            "content": entry.artifact.content.to_hex(),
            "has_text": entry.text.is_some(),
        }));
    }
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "artifacts_listed",
        "artifacts": artifacts,
    })))
}

async fn search(
    surface: &dyn ArtifactSurface,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let query = required_string_argument(tool_id, &call.arguments, "query")?;
    let label = optional_string_argument(tool_id, &call.arguments, "label")?;
    let max_results = optional_usize_argument(tool_id, &call.arguments, "max_results")?;
    let bound = surface.list().await.map_err(|e| execution(tool_id, e))?;

    let corpus: Vec<&BoundArtifact> = bound
        .iter()
        .filter(|entry| matches_filters(entry, label.as_deref(), None))
        .filter(|entry| entry.text.is_some())
        .collect();
    let ranked = relevance::rank(&query, &corpus);

    let total_matches = ranked.len();
    let mut body = serde_json::json!({
        "result": "artifacts_searched",
        "query": query,
        "matches": [],
        "total_matches": total_matches,
        "omitted_matches": total_matches,
        "truncated": false,
        "max_result_bytes": 65536,
        "max_snippet_bytes": relevance::MAX_SNIPPET_BYTES,
    });
    if serde_json::to_vec(&body).map_err(|e| execution_encoding(tool_id, e))?.len() > 65536 {
        return Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.clone(),
            reason: "query exceeds the artifact search response byte bound".to_string(),
        });
    }
    for scored in ranked.into_iter().take(max_results.unwrap_or(20)) {
        let snippet = &scored.snippet;
        let value = serde_json::json!({
            "id": scored.entry.artifact.id.as_str(),
            "score": scored.score,
            "line": snippet.text,
            "offset_bytes": snippet.offset_bytes,
            "bytes_returned": snippet.text.len(),
            "line_offset_bytes": snippet.line_offset_bytes,
            "line_bytes": snippet.line_bytes,
            "truncated": snippet.text.len() < snippet.line_bytes,
            "next_offset_bytes": (snippet.offset_bytes + snippet.text.len()
                < snippet.line_offset_bytes + snippet.line_bytes)
                .then_some(snippet.offset_bytes + snippet.text.len()),
        });
        body["matches"].as_array_mut().unwrap().push(value);
        if serde_json::to_vec(&body).map_err(|e| execution_encoding(tool_id, e))?.len() > 65536 {
            body["matches"].as_array_mut().unwrap().pop();
            break;
        }
    }
    let matches = body["matches"].as_array().unwrap();
    let omitted = total_matches - matches.len();
    let truncated = omitted > 0 || matches.iter().any(|entry| entry["truncated"] == true);
    body["omitted_matches"] = omitted.into();
    body["truncated"] = truncated.into();
    Ok(ToolHandlerResult::new(body))
}

fn execution_encoding(tool_id: &str, error: serde_json::Error) -> ToolHandlerError {
    ToolHandlerError::Execution {
        tool_id: tool_id.to_string(),
        reason: format!("artifact search result is not encodable JSON: {error}"),
    }
}

mod relevance {
    use octopus_worker_env::BoundArtifact;

    const K1: f64 = 1.5;
    const B: f64 = 0.75;
    pub(super) const MAX_SNIPPET_BYTES: usize = 2048;

    pub(super) struct Snippet<'a> {
        pub text: &'a str,
        pub offset_bytes: usize,
        pub line_offset_bytes: usize,
        pub line_bytes: usize,
    }

    pub(super) struct Scored<'a> {
        pub entry: &'a BoundArtifact,
        pub score: f64,
        pub snippet: Snippet<'a>,
    }

    struct Doc<'a> {
        entry: &'a BoundArtifact,
        terms: Vec<String>,
        len: usize,
    }

    pub(super) fn rank<'a>(query: &str, corpus: &[&'a BoundArtifact]) -> Vec<Scored<'a>> {
        let query_terms = tokenize(query);
        if query_terms.is_empty() || corpus.is_empty() {
            return Vec::new();
        }

        let docs: Vec<Doc> = corpus
            .iter()
            .filter_map(|entry| {
                let text = entry.text.as_deref()?;
                let terms = tokenize(text);
                if terms.is_empty() {
                    return None;
                }
                let len = terms.len();
                Some(Doc { entry, terms, len })
            })
            .collect();
        if docs.is_empty() {
            return Vec::new();
        }

        let total_len: usize = docs.iter().map(|d| d.len).sum();
        let avg_len = total_len as f64 / docs.len() as f64;
        let n = docs.len() as f64;

        let mut query_set: Vec<&String> = query_terms.iter().collect();
        query_set.sort();
        query_set.dedup();

        let idf: Vec<(&String, f64)> = query_set
            .iter()
            .map(|term| {
                let df = docs
                    .iter()
                    .filter(|doc| doc.terms.iter().any(|t| t == *term))
                    .count() as f64;
                let idf = (((n - df + 0.5) / (df + 0.5)) + 1.0).ln();
                (*term, idf)
            })
            .collect();

        let mut scored: Vec<Scored> = docs
            .iter()
            .filter_map(|doc| {
                let mut score = 0.0;
                for (term, idf) in &idf {
                    let tf = doc.terms.iter().filter(|t| *t == *term).count() as f64;
                    if tf == 0.0 {
                        continue;
                    }
                    let norm =
                        tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * doc.len as f64 / avg_len));
                    score += idf * norm;
                }
                if score <= 0.0 {
                    return None;
                }
                Some(Scored {
                    entry: doc.entry,
                    score,
                    snippet: best_snippet(doc.entry.text.as_deref().unwrap_or(""), &query_set),
                })
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.entry
                        .artifact
                        .id
                        .as_str()
                        .cmp(b.entry.artifact.id.as_str())
                })
        });
        scored
    }

    fn tokenize(text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(|token| token.to_lowercase())
            .collect()
    }

    fn best_snippet<'a>(text: &'a str, query_set: &[&String]) -> Snippet<'a> {
        let line = text.lines()
            .map(|line| {
                let line_terms = tokenize(line);
                let hits = query_set
                    .iter()
                    .filter(|term| line_terms.iter().any(|t| &t == *term))
                    .count();
                (hits, line)
            })
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, line)| line.trim())
            .unwrap_or("");
        let line_offset_bytes = if line.is_empty() { 0 } else {
            line.as_ptr() as usize - text.as_ptr() as usize
        };
        let hit = line.split(|c: char| !c.is_alphanumeric())
            .find(|token| query_set.iter().any(|term| token.to_lowercase() == term.as_str()))
            .map(|token| token.as_ptr() as usize - line.as_ptr() as usize)
            .unwrap_or(0);
        let mut start = if line.len() > MAX_SNIPPET_BYTES {
            hit.saturating_sub(MAX_SNIPPET_BYTES / 4)
        } else { 0 };
        while !line.is_char_boundary(start) { start += 1; }
        let mut end = (start + MAX_SNIPPET_BYTES).min(line.len());
        while !line.is_char_boundary(end) { end -= 1; }
        Snippet {
            text: &line[start..end],
            offset_bytes: line_offset_bytes + start,
            line_offset_bytes,
            line_bytes: line.len(),
        }
    }
}

fn matches_filters(entry: &BoundArtifact, label: Option<&str>, id_prefix: Option<&str>) -> bool {
    let id = entry.artifact.id.as_str();
    if let Some(prefix) = id_prefix {
        if !id.starts_with(prefix) {
            return false;
        }
    }
    if let Some(label) = label {
        let entry_label = id.split_once(':').map(|(head, _)| head).unwrap_or(id);
        if entry_label != label {
            return false;
        }
    }
    true
}

fn optional_usize_argument(
    tool_id: &str,
    args: &serde_json::Value,
    name: &str,
) -> Result<Option<usize>, ToolHandlerError> {
    match args.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .filter(|value| *value >= 1)
            .and_then(|value| usize::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| ToolHandlerError::InvalidArguments {
                tool_id: tool_id.to_string(),
                reason: format!("arguments field '{name}' must be a positive integer"),
            }),
        Some(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!("arguments field '{name}' must be a positive integer"),
        }),
    }
}

fn optional_nonnegative_usize_argument(
    tool_id: &str,
    args: &serde_json::Value,
    name: &str,
) -> Result<Option<usize>, ToolHandlerError> {
    match args.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| ToolHandlerError::InvalidArguments {
                tool_id: tool_id.to_string(),
                reason: format!("arguments field '{name}' must be a non-negative integer"),
            }),
        Some(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!("arguments field '{name}' must be a non-negative integer"),
        }),
    }
}

fn bounded_optional_usize_argument(
    tool_id: &str,
    args: &serde_json::Value,
    name: &str,
    minimum: usize,
    maximum: usize,
) -> Result<Option<usize>, ToolHandlerError> {
    let value = optional_nonnegative_usize_argument(tool_id, args, name)?;
    match value {
        Some(value) if !(minimum..=maximum).contains(&value) => {
            Err(ToolHandlerError::InvalidArguments {
                tool_id: tool_id.to_string(),
                reason: format!(
                    "arguments field '{name}' must be an integer between {minimum} and {maximum}"
                ),
            })
        }
        value => Ok(value),
    }
}

fn required_handler_usize(call: &ToolCall, name: &str) -> Result<usize, ToolHandlerError> {
    call.handler_config
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason: format!("handler_config.{name} must be a positive integer"),
        })
}

fn execution(tool_id: &str, error: octopus_worker_env::WorkerEnvError) -> ToolHandlerError {
    ToolHandlerError::Execution {
        tool_id: tool_id.to_string(),
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use octopus_descriptors::ArtifactId;
    use octopus_ledger::{ArtifactRef, BlobStore, InMemoryBlobs};
    use octopus_worker_env::{
        AbsPath, ArtifactSurface, BoundArtifact, ExecOutput, ExecSurface, FileSurface,
        NetworkCeiling, PatchOutcome, ProjectedSandbox, WorkerEnvError,
    };

    use octopus_tool_handlers::{ToolCall, ToolHandler, ToolHandlerContext, ToolHandlerError};

    use super::ArtifactBoundHandler;

    struct RunBoundArtifacts {
        blobs: Mutex<InMemoryBlobs>,
        bound: Mutex<Vec<ArtifactRef>>,
    }

    impl RunBoundArtifacts {
        fn new() -> Self {
            Self {
                blobs: Mutex::new(InMemoryBlobs::new()),
                bound: Mutex::new(Vec::new()),
            }
        }
    }

    impl ArtifactSurface for RunBoundArtifacts {
        fn append(
            &self,
            label: &str,
            payload: &[u8],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ArtifactRef, WorkerEnvError>> + Send + '_>,
        > {
            let label = label.to_string();
            let payload = payload.to_vec();
            Box::pin(async move {
                let content = self.blobs.lock().unwrap().put(&payload);
                let artifact = ArtifactRef {
                    id: ArtifactId::new(format!("{label}:{}", content.to_hex())),
                    content,
                };
                self.bound.lock().unwrap().push(artifact.clone());
                Ok(artifact)
            })
        }

        fn read(
            &self,
            id: &str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<Vec<u8>>, WorkerEnvError>>
                    + Send
                    + '_,
            >,
        > {
            let id = id.to_string();
            Box::pin(async move {
                let bound = self.bound.lock().unwrap();
                let Some(artifact) = bound
                    .iter()
                    .find(|artifact| artifact.id.as_str() == id || artifact.content.to_hex() == id)
                else {
                    return Ok(None);
                };
                Ok(self.blobs.lock().unwrap().get(&artifact.content))
            })
        }

        fn list(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Vec<BoundArtifact>, WorkerEnvError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                let bound = self.bound.lock().unwrap();
                let blobs = self.blobs.lock().unwrap();
                let entries = bound
                    .iter()
                    .map(|artifact| BoundArtifact {
                        artifact: artifact.clone(),
                        text: blobs
                            .get(&artifact.content)
                            .and_then(|bytes| String::from_utf8(bytes).ok()),
                    })
                    .collect();
                Ok(entries)
            })
        }
    }

    struct UnusedFs;

    impl FileSurface for UnusedFs {
        fn read(
            &self,
            _path: &AbsPath,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<u8>, WorkerEnvError>> + Send + '_>,
        > {
            Box::pin(async { panic!("artifact_bound family must not touch the file surface") })
        }
        fn write(
            &self,
            _path: &AbsPath,
            _bytes: &[u8],
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<octopus_worker_env::FileWriteOutput, WorkerEnvError>,
                    > + Send
                    + '_,
            >,
        > {
            Box::pin(async { panic!("artifact_bound family must not touch the file surface") })
        }
        fn list(
            &self,
            _dir: &AbsPath,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<AbsPath>, WorkerEnvError>> + Send + '_>,
        > {
            Box::pin(async { panic!("artifact_bound family must not touch the file surface") })
        }
        fn apply_patch(
            &self,
            _patch: &str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<PatchOutcome, WorkerEnvError>> + Send + '_>,
        > {
            Box::pin(async { panic!("artifact_bound family must not touch the file surface") })
        }
    }

    struct UnusedExec;

    impl ExecSurface for UnusedExec {
        fn run(
            &self,
            _request: octopus_worker_env::ExecRequest,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ExecOutput, WorkerEnvError>> + Send + '_>,
        > {
            Box::pin(async { panic!("artifact_bound family must not touch the exec surface") })
        }
    }

    fn ctx_with(surface: Arc<RunBoundArtifacts>) -> ToolHandlerContext {
        ToolHandlerContext {
            run: "run_artifacts".to_string(),
            room_root: PathBuf::from("/nonexistent"),
            sandbox: ProjectedSandbox::from_parts(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                NetworkCeiling::Denied,
            ),
            fs: Arc::new(UnusedFs),
            exec: Arc::new(UnusedExec),
            artifacts: Some(surface),
        }
    }

    async fn run_tool(
        ctx: &ToolHandlerContext,
        tool_id: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ToolHandlerError> {
        let operation = match tool_id {
            "artifact.append" => "append",
            "artifact.read" => "read",
            "artifact.list" => "list",
            "artifact.search" => "search",
            other => panic!("test has no declared operation for '{other}'"),
        };
        run_tool_with_operation(ctx, tool_id, operation, arguments).await
    }

    async fn run_tool_with_operation(
        ctx: &ToolHandlerContext,
        tool_id: &str,
        operation: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ToolHandlerError> {
        let family = ArtifactBoundHandler;
        let handler_config = if operation == "read" {
            serde_json::json!({
                "operation": operation,
                "default_max_bytes": 65536,
                "max_max_bytes": 1048576
            })
        } else {
            serde_json::json!({"operation": operation})
        };
        let call = ToolCall {
            tool_id: tool_id.to_string(),
            arguments,
            handler_config,
        };
        family.execute(ctx, &call).await.map(|result| result.body)
    }

    #[tokio::test]
    async fn exact_clone_executes_only_its_descriptor_operation() {
        let surface = Arc::new(RunBoundArtifacts::new());
        let ctx = ctx_with(surface);
        let body = run_tool_with_operation(
            &ctx,
            "artifact.custom_append",
            "append",
            serde_json::json!({"label": "clone", "record": "same operation"}),
        )
        .await
        .unwrap();
        assert_eq!(body["result"], "artifact_appended");

        let family = ArtifactBoundHandler;
        for handler_config in [
            serde_json::Value::Null,
            serde_json::json!({"operation": 1}),
            serde_json::json!({"operation": "unknown"}),
        ] {
            let call = ToolCall {
                tool_id: "artifact.custom_append".to_string(),
                arguments: serde_json::json!({}),
                handler_config,
            };
            assert!(family.execute(&ctx, &call).await.is_err());
        }
    }

    #[tokio::test]
    async fn append_returns_content_addressed_ref() {
        let surface = Arc::new(RunBoundArtifacts::new());
        let ctx = ctx_with(surface.clone());

        let body = run_tool(
            &ctx,
            "artifact.append",
            serde_json::json!({ "label": "finding", "payload": { "note": "the house lives" } }),
        )
        .await
        .expect("append shapes into a result body");

        assert_eq!(body["result"], "artifact_appended");
        let id = body["artifact"]["id"].as_str().unwrap();
        let content = body["artifact"]["content"].as_str().unwrap();
        assert!(id.starts_with("finding:"));
        assert!(id.ends_with(content));
        let expected = octopus_ledger::ContentHash::of_bytes(
            &serde_json::to_vec(&serde_json::json!({ "note": "the house lives" })).unwrap(),
        );
        assert_eq!(content, expected.to_hex());
    }

    #[tokio::test]
    async fn read_list_search_operate_over_bound_refs() {
        let surface = Arc::new(RunBoundArtifacts::new());
        let ctx = ctx_with(surface.clone());

        let appended = run_tool(
            &ctx,
            "artifact.append",
            serde_json::json!({ "label": "line", "record": "octopus lives here" }),
        )
        .await
        .expect("append shapes into a result body");
        let id = appended["artifact"]["id"].as_str().unwrap().to_string();

        let read = run_tool(&ctx, "artifact.read", serde_json::json!({ "id": id }))
            .await
            .expect("read shapes into a result body");
        assert_eq!(read["text"], "\"octopus lives here\"");
        assert_eq!(read["artifact_size_bytes"], 20);
        assert_eq!(read["truncated"], false);

        let first_page = run_tool(
            &ctx,
            "artifact.read",
            serde_json::json!({ "id": id, "max_bytes": 8 }),
        )
        .await
        .expect("bounded read returns an exact continuation");
        assert_eq!(first_page["text"], "\"octopus");
        assert_eq!(first_page["next_offset_bytes"], 8);
        assert_eq!(first_page["truncated"], true);

        let listed = run_tool(&ctx, "artifact.list", serde_json::json!({}))
            .await
            .expect("list shapes into a result body");
        assert_eq!(listed["artifacts"].as_array().unwrap().len(), 1);
        assert_eq!(listed["artifacts"][0]["has_text"], true);

        let searched = run_tool(
            &ctx,
            "artifact.search",
            serde_json::json!({ "query": "octopus" }),
        )
        .await
        .expect("search shapes into a result body");
        assert_eq!(searched["matches"].as_array().unwrap().len(), 1);

        let missed = run_tool(
            &ctx,
            "artifact.search",
            serde_json::json!({ "query": "absent-token" }),
        )
        .await
        .expect("search shapes into a result body");
        assert!(missed["matches"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_ranks_by_meaning_not_substring() {
        let surface = Arc::new(RunBoundArtifacts::new());
        let ctx = ctx_with(surface.clone());

        for (label, text) in [
            (
                "memory.knowledge_graph",
                "consolidate the knowledge graph: dedup entities and merge duplicate relations \
                 so the knowledge graph stays consolidated across conversations",
            ),
            (
                "memory.ad_hoc_note",
                "the user knowledge base mentions their cat once in passing",
            ),
            (
                "memory.summary",
                "the browser session opened three tabs and closed one",
            ),
        ] {
            run_tool(
                &ctx,
                "artifact.append",
                serde_json::json!({ "label": label, "record": text }),
            )
            .await
            .expect("append shapes into a result body");
        }

        let searched = run_tool(
            &ctx,
            "artifact.search",
            serde_json::json!({ "query": "consolidate knowledge graph dedup entities relations" }),
        )
        .await
        .expect("search shapes into a result body");
        let matches = searched["matches"].as_array().unwrap();
        assert!(matches.len() >= 2, "several artifacts share query meaning");

        let top_id = matches[0]["id"].as_str().unwrap();
        assert!(
            top_id.starts_with("memory.knowledge_graph:"),
            "the consolidation artifact ranks first by shared meaning, got {top_id}"
        );

        assert!(
            !matches
                .iter()
                .any(|m| m["id"].as_str().unwrap().starts_with("memory.summary:")),
            "an artifact with zero shared meaning is not a result"
        );

        let top_score = matches[0]["score"].as_f64().unwrap();
        let last_score = matches.last().unwrap()["score"].as_f64().unwrap();
        assert!(
            top_score >= last_score && top_score > 0.0,
            "results are ranked by descending relevance score"
        );

        let decoy_query = run_tool(
            &ctx,
            "artifact.search",
            serde_json::json!({ "query": "knowledge graph", "max_results": 1 }),
        )
        .await
        .expect("search shapes into a result body");
        let decoy_top = decoy_query["matches"][0]["id"].as_str().unwrap();
        assert!(
            decoy_top.starts_with("memory.knowledge_graph:"),
            "the richer knowledge-graph artifact still wins on the shared phrase, got {decoy_top}"
        );
    }

    #[tokio::test]
    async fn search_bounds_large_utf8_lines_and_preserves_read_offsets() {
        let surface = Arc::new(RunBoundArtifacts::new());
        let ctx = ctx_with(surface.clone());
        for index in 0..40 {
            let text = format!("unrelated\r\n  {} NeEdLe {} {index}\r\n",
                "🙂 ".repeat(10000), "\u{0001}界".repeat(2000));
            surface.append("large", text.as_bytes()).await.unwrap();
        }
        for arguments in [
            serde_json::json!({"query":"needle"}),
            serde_json::json!({"query":"needle", "max_results":1000}),
            serde_json::json!({"query":"needle", "max_results":1}),
        ] {
            let result = run_tool(&ctx, "artifact.search", arguments.clone()).await.unwrap();
            assert!(serde_json::to_vec(&result).unwrap().len() <= 65536);
            let matches = result["matches"].as_array().unwrap();
            assert!(!matches.is_empty());
            assert!(matches.len() <= arguments["max_results"].as_u64().unwrap_or(20) as usize);
            assert_eq!(result["total_matches"], 40);
            assert_eq!(result["omitted_matches"], 40 - matches.len());
            assert_eq!(result["truncated"], true);
            for hit in matches {
                let excerpt = hit["line"].as_str().unwrap();
                assert!(excerpt.contains("NeEdLe") && excerpt.len() <= 2048);
                assert_eq!(hit["truncated"], true);
                assert!(hit["offset_bytes"].as_u64().unwrap() > 11);
                let read = run_tool(&ctx, "artifact.read", serde_json::json!({
                    "id":hit["id"], "offset_bytes":hit["offset_bytes"],
                    "max_bytes":hit["bytes_returned"],
                })).await.unwrap();
                assert_eq!(read["text"], hit["line"]);
                assert_eq!(read["next_offset_bytes"], hit["next_offset_bytes"]);
            }
        }
        let filtered = run_tool(&ctx, "artifact.search",
            serde_json::json!({"query":"needle", "label":"other"})).await.unwrap();
        assert_eq!(filtered["total_matches"], 0);
        assert_eq!(filtered["truncated"], false);
        assert!(run_tool(&ctx, "artifact.search",
            serde_json::json!({"query":"x".repeat(65536)})).await.is_err());
    }

    #[tokio::test]
    async fn read_unbound_id_fails_closed() {
        let surface = Arc::new(RunBoundArtifacts::new());
        let ctx = ctx_with(surface);
        let err = run_tool(
            &ctx,
            "artifact.read",
            serde_json::json!({ "id": "finding:deadbeef" }),
        )
        .await
        .expect_err("an id outside the run binding is refused, never read from a global pool");
        assert!(matches!(err, ToolHandlerError::Execution { .. }));
    }

    #[tokio::test]
    async fn append_without_label_fails_closed() {
        let surface = Arc::new(RunBoundArtifacts::new());
        let ctx = ctx_with(surface);
        let err = run_tool(
            &ctx,
            "artifact.append",
            serde_json::json!({ "payload": { "note": "no label" } }),
        )
        .await
        .expect_err("a missing label fails closed rather than inventing a semantic name");
        assert!(matches!(err, ToolHandlerError::InvalidArguments { .. }));
    }

    #[tokio::test]
    async fn no_leased_surface_fails_closed() {
        let ctx = ToolHandlerContext {
            run: "run_artifacts".to_string(),
            room_root: PathBuf::from("/nonexistent"),
            sandbox: ProjectedSandbox::from_parts(
                Vec::new(),
                Vec::new(),
                Vec::new(),
                NetworkCeiling::Denied,
            ),
            fs: Arc::new(UnusedFs),
            exec: Arc::new(UnusedExec),
            artifacts: None,
        };
        let err = run_tool(&ctx, "artifact.list", serde_json::json!({}))
            .await
            .expect_err(
                "a room without a leased artifact surface fails closed, never a placeholder",
            );
        assert!(matches!(err, ToolHandlerError::Execution { .. }));
    }
}
