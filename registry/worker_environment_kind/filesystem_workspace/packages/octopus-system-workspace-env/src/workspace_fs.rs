use std::io::{BufRead, BufReader, Read};
use std::num::NonZero;
use std::path::Path;
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use octopus_applypatch::{
    apply_patch_with_options, parse_patch, AppliedPatchDelta, AppliedPatchFileChange,
    ApplyPatchFileUpdateMode, ApplyPatchOptions, Hunk,
};
use octopus_file_search::{run as file_search_run, FileSearchOptions};
use octopus_file_system::LOCAL_FS;
use octopus_util_pathuri::PathUri;
use octopus_worker_env::AbsPath;
use regex::RegexBuilder;

use octopus_tool_handlers::{
    optional_string_argument, required_string_argument, ToolCall, ToolHandler, ToolHandlerContext,
    ToolHandlerError, ToolHandlerResult,
};

use crate::{workspace_handler_config, WorkspaceHandlerConfig, WorkspaceOperation};

const DEFAULT_FILE_READ_BYTES: usize = 64 * 1024;
const MAX_FILE_READ_BYTES: usize = 1024 * 1024;
const DEFAULT_DIRECTORY_LIMIT: usize = 200;
const MAX_DIRECTORY_LIMIT: usize = 1000;
const DEFAULT_GREP_RESULTS: usize = 100;
const MAX_GREP_RESULTS: usize = 500;
const MAX_GREP_PREVIEW_BYTES: usize = 1000;
const MAX_GREP_ERRORS: usize = 20;

#[derive(Clone, Copy, Debug)]
struct GrepLimits {
    max_scanned_files: usize,
    max_scanned_bytes: u64,
    deadline_ms: u64,
    max_line_bytes: usize,
}

pub struct WorkspaceFsHandler;

impl ToolHandler for WorkspaceFsHandler {
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
    let config =
        workspace_handler_config(call).map_err(|reason| ToolHandlerError::InvalidArguments {
            tool_id: call.tool_id.clone(),
            reason,
        })?;
    match config.operation {
        WorkspaceOperation::WriteFile => write_file(ctx, call).await,
        WorkspaceOperation::ApplyPatch => apply_patch(ctx, call).await,
        WorkspaceOperation::ReadFile => read_file(ctx, call).await,
        WorkspaceOperation::ListDir => list_dir(ctx, call).await,
        WorkspaceOperation::Grep => grep(ctx, call, &config).await,
        _ => Err(ToolHandlerError::UnsupportedTool {
            tool_id: call.tool_id.clone(),
        }),
    }
}

fn grep_limits(tool_id: &str, config: &WorkspaceHandlerConfig) -> Result<GrepLimits, ToolHandlerError> {
    let invalid = |field: &str| ToolHandlerError::InvalidArguments {
        tool_id: tool_id.to_string(),
        reason: format!("grep handler_config is missing '{field}'"),
    };
    Ok(GrepLimits {
        max_scanned_files: config
            .max_scanned_files
            .ok_or_else(|| invalid("max_scanned_files"))?,
        max_scanned_bytes: config
            .max_scanned_bytes
            .ok_or_else(|| invalid("max_scanned_bytes"))?,
        deadline_ms: config.deadline_ms.ok_or_else(|| invalid("deadline_ms"))?,
        max_line_bytes: config
            .max_line_bytes
            .ok_or_else(|| invalid("max_line_bytes"))?,
    })
}

async fn write_file(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let path = required_string_argument(tool_id, &call.arguments, "path")?;
    let content = required_string_argument(tool_id, &call.arguments, "content")?;
    let abs = resolve_room_path(ctx, tool_id, &path)?;
    let written = ctx
        .fs
        .write(&abs, content.as_bytes())
        .await
        .map_err(|e| execution(tool_id, e))?;
    let mut change = serde_json::json!({
        "path": abs.as_str(),
        "kind": "update",
        "after": content_identity(&content),
    });
    match written.previous_content {
        Ok(previous) => {
            change["kind"] = serde_json::json!(if previous.is_some() { "update" } else { "add" });
            change["before"] = previous
                .as_ref()
                .map(|bytes| {
                    serde_json::json!({
                        "bytes": bytes.len(),
                        "blake3": blake3::hash(bytes).to_hex().to_string(),
                    })
                })
                .unwrap_or(serde_json::Value::Null);
            match std::str::from_utf8(previous.as_deref().unwrap_or_default()) {
                Ok(before) => add_text_diff(&mut change, before, &content),
                Err(_) => {
                    change["diff_unavailable"] =
                        serde_json::json!("previous content is not UTF-8 text")
                }
            }
        }
        Err(reason) => change["diff_unavailable"] = serde_json::json!(reason),
    }
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "file_written",
        "path": abs.as_str(),
        "changes": [change],
    })))
}

async fn apply_patch(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let patch = required_string_argument(tool_id, &call.arguments, "patch")?;
    let parsed = parse_patch(&patch).map_err(|e| ToolHandlerError::Execution {
        tool_id: tool_id.to_string(),
        reason: format!("apply_patch parse failed closed: {e}"),
    })?;
    for hunk in &parsed.hunks {
        for rel in hunk_relative_paths(hunk) {
            let target = resolve_room_path(ctx, tool_id, &rel)?;
            ctx.sandbox
                .authorize_write(&target)
                .map_err(|error| execution(tool_id, error))?;
            if target
                .as_path()
                .components()
                .any(|component| component.as_os_str().to_str() == Some(".git"))
            {
                return Err(ToolHandlerError::Execution {
                    tool_id: tool_id.to_string(),
                    reason: format!(
                        "apply_patch refused: path '{}' writes into the protected .git subtree",
                        target.as_str()
                    ),
                });
            }
        }
    }
    let cwd = PathUri::from_path(&ctx.room_root).map_err(|e| ToolHandlerError::Execution {
        tool_id: tool_id.to_string(),
        reason: format!("workspace room cwd failed closed: {e}"),
    })?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let delta = match apply_patch_with_options(
        &patch,
        ApplyPatchOptions {
            update_file_mode: ApplyPatchFileUpdateMode::PreserveLineEndings,
            follow_symlinks: false,
        },
        &cwd,
        &mut stdout,
        &mut stderr,
        LOCAL_FS.as_ref(),
        None,
    )
    .await
    {
        Ok(delta) => delta,
        Err(failure) => {
            let (error, delta) = failure.into_parts();
            let (changed_paths, changes) = patch_delta_evidence(&delta, &ctx.room_root);
            let mut details = serde_json::Map::new();
            details.insert(
                "result".to_string(),
                serde_json::json!("patch_application_failed"),
            );
            details.insert(
                "partial_effect".to_string(),
                serde_json::json!(!changed_paths.is_empty()),
            );
            details.insert("exact".to_string(), serde_json::json!(delta.is_exact()));
            details.insert(
                "changed_paths".to_string(),
                serde_json::json!(changed_paths),
            );
            details.insert("changes".to_string(), serde_json::json!(changes));
            details.insert(
                "stdout".to_string(),
                serde_json::json!(String::from_utf8_lossy(&stdout).trim()),
            );
            details.insert(
                "stderr".to_string(),
                serde_json::json!(String::from_utf8_lossy(&stderr).trim()),
            );
            return Err(ToolHandlerError::ExecutionWithDetails {
                tool_id: tool_id.to_string(),
                reason: format!("apply_patch failed closed: {error}"),
                details,
            });
        }
    };
    let (changed_paths, changes) = patch_delta_evidence(&delta, &ctx.room_root);
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "patch_applied",
        "changed_paths": changed_paths,
        "changes": changes,
        "exact": delta.is_exact(),
    })))
}

fn patch_delta_evidence(
    delta: &AppliedPatchDelta,
    room_root: &Path,
) -> (Vec<String>, Vec<serde_json::Value>) {
    let changes = delta
        .changes()
        .iter()
        .map(|change| {
            let path = relative_delta_path(room_root, &change.path);
            let mut evidence = match &change.change {
                AppliedPatchFileChange::Add {
                    content,
                    overwritten_content,
                } => serde_json::json!({
                    "kind": if overwritten_content.is_some() { "update" } else { "add" },
                    "path": path,
                    "before": overwritten_content.as_deref().map(content_identity),
                    "after": content_identity(content),
                }),
                AppliedPatchFileChange::Delete { content } => serde_json::json!({
                    "kind": "delete",
                    "path": path,
                    "before": content_identity(content),
                    "after": null,
                }),
                AppliedPatchFileChange::Update {
                    move_path,
                    old_content,
                    overwritten_move_content,
                    new_content,
                } => serde_json::json!({
                    "kind": "update",
                    "path": path,
                    "move_path": move_path.as_ref().map(|path| relative_delta_path(room_root, path)),
                    "before": content_identity(old_content),
                    "overwritten_move_target": overwritten_move_content.as_deref().map(content_identity),
                    "after": content_identity(new_content),
                }),
            };
            let (before, after) = match &change.change {
                AppliedPatchFileChange::Add { content, overwritten_content } =>
                    (overwritten_content.as_deref().unwrap_or_default(), content.as_str()),
                AppliedPatchFileChange::Delete { content } => (content.as_str(), ""),
                AppliedPatchFileChange::Update { old_content, new_content, .. } =>
                    (old_content.as_str(), new_content.as_str()),
            };
            add_text_diff(&mut evidence, before, after);
            (path, evidence)
        })
        .collect::<Vec<_>>();
    (
        changes.iter().map(|(path, _)| path.clone()).collect(),
        changes.into_iter().map(|(_, evidence)| evidence).collect(),
    )
}

fn add_text_diff(evidence: &mut serde_json::Value, before: &str, after: &str) {
    const MAX_DIFF_INPUT_BYTES: usize = 256 * 1024;
    if before.len().saturating_add(after.len()) > MAX_DIFF_INPUT_BYTES {
        evidence["diff_unavailable"] = serde_json::json!("content exceeds the text diff limit");
        return;
    }
    if before.contains('\0') || after.contains('\0') {
        evidence["diff_unavailable"] = serde_json::json!("content is binary");
        return;
    }
    let diff = similar::TextDiff::configure()
        .timeout(Duration::from_millis(200))
        .diff_lines(before, after);
    let mut added = 0usize;
    let mut deleted = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => deleted += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    evidence["added"] = serde_json::json!(added);
    evidence["deleted"] = serde_json::json!(deleted);
    evidence["diff"] = serde_json::json!(diff.unified_diff().context_radius(3).to_string());
}

fn relative_delta_path(room_root: &Path, path: &PathUri) -> String {
    let Ok(native_path) = path.to_abs_path() else {
        return "<outside-room>".to_string();
    };
    let relative = native_path.as_path().strip_prefix(room_root).ok();
    relative
        .filter(|path| {
            !path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        })
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| native_path.as_path().to_string_lossy().to_string())
}

fn content_identity(content: &str) -> serde_json::Value {
    serde_json::json!({
        "bytes": content.len(),
        "blake3": blake3::hash(content.as_bytes()).to_hex().to_string(),
    })
}

fn hunk_relative_paths(hunk: &Hunk) -> Vec<String> {
    match hunk {
        Hunk::AddFile { path, .. } | Hunk::DeleteFile { path } => {
            vec![path.to_string_lossy().to_string()]
        }
        Hunk::UpdateFile {
            path, move_path, ..
        } => {
            let mut paths = vec![path.to_string_lossy().to_string()];
            if let Some(dest) = move_path {
                paths.push(dest.to_string_lossy().to_string());
            }
            paths
        }
    }
}

async fn read_file(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let path = required_string_argument(tool_id, &call.arguments, "path")?;
    let offset =
        bounded_optional_u64_argument(tool_id, &call.arguments, "offset_bytes", 0, u64::MAX)?
            .unwrap_or(0);
    let max_bytes = bounded_optional_usize_argument(
        tool_id,
        &call.arguments,
        "max_bytes",
        4,
        MAX_FILE_READ_BYTES,
    )?
    .unwrap_or(DEFAULT_FILE_READ_BYTES);
    let abs = resolve_room_path(ctx, tool_id, &path)?;
    let read = ctx
        .fs
        .read_range(&abs, offset, max_bytes)
        .await
        .map_err(|e| execution(tool_id, e))?;
    let valid_len = match std::str::from_utf8(&read.bytes) {
        Ok(_) => read.bytes.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(error) => {
            return Err(ToolHandlerError::Execution {
                tool_id: tool_id.to_string(),
                reason: format!(
                    "file '{}' is not UTF-8 at byte {}",
                    relative_room_path(ctx, tool_id, &abs)?,
                    offset.saturating_add(error.valid_up_to() as u64)
                ),
            });
        }
    };
    let content = std::str::from_utf8(&read.bytes[..valid_len]).map_err(|error| {
        ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason: format!("UTF-8 boundary validation failed closed: {error}"),
        }
    })?;
    let next_offset =
        read.offset
            .checked_add(valid_len as u64)
            .ok_or_else(|| ToolHandlerError::Execution {
                tool_id: tool_id.to_string(),
                reason: "file offset overflowed u64".to_string(),
            })?;
    if valid_len == 0 && next_offset < read.file_size {
        return Err(ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason: format!(
                "file '{}' cannot make UTF-8 progress with max_bytes={max_bytes}",
                relative_room_path(ctx, tool_id, &abs)?
            ),
        });
    }
    let truncated = next_offset < read.file_size;
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "file_read",
        "path": relative_room_path(ctx, tool_id, &abs)?,
        "file_size_bytes": read.file_size,
        "offset_bytes": read.offset,
        "bytes_read": valid_len,
        "next_offset_bytes": truncated.then_some(next_offset),
        "truncated": truncated,
        "content": content,
    })))
}

async fn list_dir(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let path = optional_string_argument(tool_id, &call.arguments, "path")?
        .unwrap_or_else(|| ".".to_string());
    let offset =
        bounded_optional_usize_argument(tool_id, &call.arguments, "offset", 0, usize::MAX)?
            .unwrap_or(0);
    let limit =
        bounded_optional_usize_argument(tool_id, &call.arguments, "limit", 1, MAX_DIRECTORY_LIMIT)?
            .unwrap_or(DEFAULT_DIRECTORY_LIMIT);
    let abs = resolve_room_path(ctx, tool_id, &path)?;
    let mut entries = ctx.fs.list(&abs).await.map_err(|e| execution(tool_id, e))?;
    entries.sort();
    let total_entry_count = entries.len();
    let entries = entries
        .iter()
        .skip(offset)
        .take(limit)
        .map(|entry| relative_room_path(ctx, tool_id, entry))
        .collect::<Result<Vec<_>, _>>()?;
    let next_offset = offset
        .checked_add(entries.len())
        .filter(|next| *next < total_entry_count);
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "directory_listed",
        "path": relative_room_path(ctx, tool_id, &abs)?,
        "entries": entries,
        "offset": offset,
        "total_entry_count": total_entry_count,
        "next_offset": next_offset,
        "truncated": next_offset.is_some(),
    })))
}

async fn grep(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
    config: &WorkspaceHandlerConfig,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = call.tool_id.clone();
    let limits = grep_limits(&tool_id, config)?;
    let query = required_string_argument(&tool_id, &call.arguments, "query")?;
    let mode = optional_string_argument(&tool_id, &call.arguments, "mode")?
        .unwrap_or_else(|| "content".to_string());
    let path = optional_string_argument(&tool_id, &call.arguments, "path")?
        .unwrap_or_else(|| ".".to_string());
    let max_results = bounded_optional_usize_argument(
        &tool_id,
        &call.arguments,
        "max_results",
        1,
        MAX_GREP_RESULTS,
    )?
    .unwrap_or(DEFAULT_GREP_RESULTS);
    let search_root = resolve_room_path(ctx, &tool_id, &path)?;
    let room_root = AbsPath::new(&ctx.room_root).map_err(|e| execution(&tool_id, e))?;
    match mode.as_str() {
        "content" => {
            let case_sensitive =
                optional_bool_argument(&tool_id, &call.arguments, "case_sensitive")?
                    .unwrap_or_else(|| query.chars().any(char::is_uppercase));
            let root = search_root.as_path().to_path_buf();
            let room = room_root.as_path().to_path_buf();
            let search_query = query.clone();
            let scan = tokio::task::spawn_blocking(move || {
                grep_content(
                    &room,
                    &root,
                    &search_query,
                    case_sensitive,
                    max_results,
                    limits,
                )
            })
            .await
            .map_err(|e| ToolHandlerError::Execution {
                tool_id: tool_id.clone(),
                reason: format!("content grep task join failed closed: {e}"),
            })?
            .map_err(|reason| ToolHandlerError::Execution {
                tool_id: tool_id.clone(),
                reason,
            })?;
            Ok(ToolHandlerResult::new(serde_json::json!({
                "result": "content_searched",
                "mode": "content",
                "query": query,
                "path": relative_room_path(ctx, &tool_id, &search_root)?,
                "case_sensitive": case_sensitive,
                "matches": scan.matches,
                "returned_match_count": scan.returned_match_count,
                "truncated": scan.truncated,
                "truncation_reasons": scan.truncation_reasons,
                "binary_files_skipped": scan.binary_files_skipped,
                "oversized_lines_skipped": scan.oversized_lines_skipped,
                "scanned_files": scan.scanned_files,
                "scanned_bytes": scan.scanned_bytes,
                "elapsed_ms": scan.elapsed_ms,
                "scan_limits": {
                    "max_scanned_files": limits.max_scanned_files,
                    "max_scanned_bytes": limits.max_scanned_bytes,
                    "deadline_ms": limits.deadline_ms,
                    "max_line_bytes": limits.max_line_bytes,
                },
                "scan_errors": scan.scan_errors,
                "scan_errors_truncated": scan.scan_errors_truncated,
            })))
        }
        "path" => {
            if call.arguments.get("case_sensitive").is_some() {
                return Err(ToolHandlerError::InvalidArguments {
                    tool_id,
                    reason: "case_sensitive is only valid for content mode".to_string(),
                });
            }
            let limit =
                NonZero::new(max_results).ok_or_else(|| ToolHandlerError::InvalidArguments {
                    tool_id: tool_id.clone(),
                    reason: "max_results must be non-zero".to_string(),
                })?;
            let root = search_root.as_path().to_path_buf();
            let search_query = query.clone();
            let results = tokio::task::spawn_blocking(move || {
                file_search_run(
                    &search_query,
                    vec![root],
                    FileSearchOptions {
                        limit,
                        ..FileSearchOptions::default()
                    },
                    None,
                )
            })
            .await
            .map_err(|e| ToolHandlerError::Execution {
                tool_id: tool_id.clone(),
                reason: format!("path search task join failed closed: {e}"),
            })?
            .map_err(|e| ToolHandlerError::Execution {
                tool_id: tool_id.clone(),
                reason: format!("path search failed closed: {e}"),
            })?;
            let matches = results
                .matches
                .iter()
                .map(|entry| {
                    let path = entry.full_path();
                    if !path.starts_with(search_root.as_path()) {
                        return Err(ToolHandlerError::Execution {
                            tool_id: tool_id.clone(),
                            reason: format!(
                                "path search returned '{}' outside the requested search root",
                                path.display()
                            ),
                        });
                    }
                    Ok(display_room_path(room_root.as_path(), &path))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ToolHandlerResult::new(serde_json::json!({
                "result": "file_paths_searched",
                "mode": "path",
                "query": query,
                "path": relative_room_path(ctx, &tool_id, &search_root)?,
                "matches": matches,
                "returned_match_count": matches.len(),
                "total_match_count": results.total_match_count,
                "truncated": results.total_match_count > matches.len(),
            })))
        }
        _ => Err(ToolHandlerError::InvalidArguments {
            tool_id,
            reason: "mode must be 'content' or 'path'".to_string(),
        }),
    }
}

struct GrepScan {
    matches: Vec<serde_json::Value>,
    returned_match_count: usize,
    truncated: bool,
    truncation_reasons: Vec<String>,
    binary_files_skipped: usize,
    oversized_lines_skipped: usize,
    scanned_files: usize,
    scanned_bytes: u64,
    elapsed_ms: u64,
    scan_errors: Vec<String>,
    scan_errors_truncated: bool,
}

enum BoundedLine {
    Eof,
    Line(Vec<u8>),
    Oversized,
    Binary,
    Stopped,
}

fn grep_content(
    room_root: &std::path::Path,
    search_root: &std::path::Path,
    query: &str,
    case_sensitive: bool,
    max_results: usize,
    config: GrepLimits,
) -> Result<GrepScan, String> {
    let started = Instant::now();
    let deadline = started
        .checked_add(Duration::from_millis(config.deadline_ms))
        .ok_or_else(|| "content grep deadline overflowed monotonic time".to_string())?;
    let matcher = RegexBuilder::new(query)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| format!("content grep query is not a valid regex: {error}"))?;
    let mut builder = WalkBuilder::new(search_root);
    builder
        .hidden(false)
        .follow_links(false)
        .standard_filters(true)
        .sort_by_file_path(|left, right| left.cmp(right))
        .filter_entry(|entry| entry.depth() == 0 || entry.file_name() != ".git");
    let mut matches = Vec::new();
    let mut binary_files_skipped = 0usize;
    let mut oversized_lines_skipped = 0usize;
    let mut scanned_files = 0usize;
    let mut scanned_bytes = 0u64;
    let mut scan_errors = Vec::new();
    let mut scan_errors_truncated = false;
    let mut truncation_reasons = Vec::new();
    'walk: for entry in builder.build() {
        if Instant::now() >= deadline {
            record_grep_truncation(&mut truncation_reasons, "deadline");
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                record_grep_truncation(&mut truncation_reasons, "scan_error");
                push_grep_error(
                    &mut scan_errors,
                    &mut scan_errors_truncated,
                    error.to_string(),
                );
                continue;
            }
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        if scanned_files >= config.max_scanned_files {
            record_grep_truncation(&mut truncation_reasons, "max_scanned_files");
            break;
        }
        if scanned_bytes >= config.max_scanned_bytes {
            record_grep_truncation(&mut truncation_reasons, "max_scanned_bytes");
            break;
        }
        scanned_files = scanned_files.saturating_add(1);
        if !entry.path().starts_with(search_root) {
            return Err(format!(
                "content grep encountered '{}' outside the requested search root",
                entry.path().display()
            ));
        }
        let relative = display_room_path(room_root, entry.path());
        let file = match std::fs::File::open(entry.path()) {
            Ok(file) => file,
            Err(error) => {
                record_grep_truncation(&mut truncation_reasons, "scan_error");
                push_grep_error(
                    &mut scan_errors,
                    &mut scan_errors_truncated,
                    format!("{relative}: {error}"),
                );
                continue;
            }
        };
        let file_size = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                record_grep_truncation(&mut truncation_reasons, "scan_error");
                push_grep_error(
                    &mut scan_errors,
                    &mut scan_errors_truncated,
                    format!("{relative}: {error}"),
                );
                continue;
            }
        };
        let remaining_scan_bytes = config.max_scanned_bytes.saturating_sub(scanned_bytes);
        let mut reader = BufReader::new(file.take(remaining_scan_bytes));
        let mut file_bytes_remaining = file_size;
        let mut line_number = 0usize;
        loop {
            let line = match read_bounded_line(
                &mut reader,
                config.max_line_bytes,
                config.max_scanned_bytes,
                deadline,
                &mut scanned_bytes,
                &mut file_bytes_remaining,
                &mut truncation_reasons,
            ) {
                Ok(line) => line,
                Err(error) => {
                    record_grep_truncation(&mut truncation_reasons, "scan_error");
                    push_grep_error(
                        &mut scan_errors,
                        &mut scan_errors_truncated,
                        format!("{relative}: {error}"),
                    );
                    break;
                }
            };
            let line = match line {
                BoundedLine::Eof => break,
                BoundedLine::Stopped => break 'walk,
                BoundedLine::Binary => {
                    binary_files_skipped = binary_files_skipped.saturating_add(1);
                    break;
                }
                BoundedLine::Oversized => {
                    line_number = line_number.saturating_add(1);
                    oversized_lines_skipped = oversized_lines_skipped.saturating_add(1);
                    record_grep_truncation(&mut truncation_reasons, "max_line_bytes");
                    continue;
                }
                BoundedLine::Line(line) => line,
            };
            line_number = line_number.saturating_add(1);
            let text = match std::str::from_utf8(&line) {
                Ok(text) => text.trim_end_matches(['\r', '\n']),
                Err(_) => {
                    binary_files_skipped = binary_files_skipped.saturating_add(1);
                    break;
                }
            };
            let Some(found) = matcher.find(text) else {
                continue;
            };
            let (preview, prefix_omitted, suffix_omitted) =
                grep_line_preview(text, found.start(), found.end());
            matches.push(serde_json::json!({
                "path": relative,
                "line": line_number,
                "column": found.start().saturating_add(1),
                "text": preview,
                "prefix_omitted": prefix_omitted,
                "suffix_omitted": suffix_omitted,
            }));
            if matches.len() >= max_results {
                record_grep_truncation(&mut truncation_reasons, "max_results");
                break 'walk;
            }
        }
    }
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(GrepScan {
        returned_match_count: matches.len(),
        matches,
        truncated: !truncation_reasons.is_empty(),
        truncation_reasons,
        binary_files_skipped,
        oversized_lines_skipped,
        scanned_files,
        scanned_bytes,
        elapsed_ms,
        scan_errors,
        scan_errors_truncated,
    })
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    max_line_bytes: usize,
    max_scanned_bytes: u64,
    deadline: Instant,
    scanned_bytes: &mut u64,
    file_bytes_remaining: &mut u64,
    truncation_reasons: &mut Vec<String>,
) -> Result<BoundedLine, std::io::Error> {
    let mut line = Vec::with_capacity(max_line_bytes.min(8 * 1024));
    let mut oversized = false;
    let mut binary = false;
    loop {
        if *file_bytes_remaining == 0 {
            return Ok(finish_bounded_line(line, oversized, binary));
        }
        if Instant::now() >= deadline {
            record_grep_truncation(truncation_reasons, "deadline");
            return Ok(BoundedLine::Stopped);
        }
        let remaining_scan_bytes = max_scanned_bytes.saturating_sub(*scanned_bytes);
        if remaining_scan_bytes == 0 {
            record_grep_truncation(truncation_reasons, "max_scanned_bytes");
            return Ok(BoundedLine::Stopped);
        }
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(finish_bounded_line(line, oversized, binary));
        }
        let allowed = buffer
            .len()
            .min(usize::try_from(remaining_scan_bytes).unwrap_or(usize::MAX));
        let newline = buffer[..allowed].iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(allowed, |index| index.saturating_add(1));
        let chunk = &buffer[..consumed];
        binary |= chunk.contains(&0);
        if !oversized {
            if line
                .len()
                .checked_add(chunk.len())
                .is_some_and(|length| length <= max_line_bytes)
            {
                line.extend_from_slice(chunk);
            } else {
                line.clear();
                oversized = true;
            }
        }
        reader.consume(consumed);
        let consumed = u64::try_from(consumed).unwrap_or(u64::MAX);
        *scanned_bytes = scanned_bytes.saturating_add(consumed);
        *file_bytes_remaining = file_bytes_remaining.saturating_sub(consumed);
        if newline.is_some() {
            return Ok(finish_bounded_line(line, oversized, binary));
        }
        if *scanned_bytes >= max_scanned_bytes && *file_bytes_remaining > 0 {
            record_grep_truncation(truncation_reasons, "max_scanned_bytes");
            return Ok(BoundedLine::Stopped);
        }
    }
}

fn finish_bounded_line(line: Vec<u8>, oversized: bool, binary: bool) -> BoundedLine {
    if binary {
        BoundedLine::Binary
    } else if oversized {
        BoundedLine::Oversized
    } else if line.is_empty() {
        BoundedLine::Eof
    } else {
        BoundedLine::Line(line)
    }
}

fn record_grep_truncation(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|candidate| candidate == reason) {
        reasons.push(reason.to_string());
    }
}

fn grep_line_preview(text: &str, match_start: usize, match_end: usize) -> (String, bool, bool) {
    if text.len() <= MAX_GREP_PREVIEW_BYTES {
        return (text.to_string(), false, false);
    }
    let match_len = match_end.saturating_sub(match_start);
    let left_budget = MAX_GREP_PREVIEW_BYTES.saturating_sub(match_len) / 2;
    let mut start = match_start.saturating_sub(left_budget);
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = start.saturating_add(MAX_GREP_PREVIEW_BYTES).min(text.len());
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[start..end].to_string(), start > 0, end < text.len())
}

fn push_grep_error(errors: &mut Vec<String>, truncated: &mut bool, error: String) {
    if errors.len() < MAX_GREP_ERRORS {
        errors.push(error);
    } else {
        *truncated = true;
    }
}

fn execution(tool_id: &str, error: octopus_worker_env::WorkerEnvError) -> ToolHandlerError {
    ToolHandlerError::Execution {
        tool_id: tool_id.to_string(),
        reason: error.to_string(),
    }
}

pub(crate) fn resolve_room_path(
    ctx: &ToolHandlerContext,
    tool_id: &str,
    rel_path: &str,
) -> Result<AbsPath, ToolHandlerError> {
    let raw = std::path::Path::new(rel_path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else if rel_path == "." || rel_path.is_empty() {
        ctx.room_root.clone()
    } else {
        ctx.room_root.join(rel_path)
    };
    let resolved = AbsPath::new(&joined).map_err(|error| ToolHandlerError::Execution {
        tool_id: tool_id.to_string(),
        reason: format!("path failed closed: {error}"),
    })?;
    resolve_authorized_path(ctx, tool_id, &resolved)
}

fn resolve_authorized_path(
    ctx: &ToolHandlerContext,
    tool_id: &str,
    target: &AbsPath,
) -> Result<AbsPath, ToolHandlerError> {
    let mut existing = target.as_path().to_path_buf();
    let mut missing_suffix = Vec::new();
    loop {
        match std::fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .ok_or_else(|| ToolHandlerError::Execution {
                        tool_id: tool_id.to_string(),
                        reason: format!("path '{}' has no existing ancestor", target.as_str()),
                    })?;
                missing_suffix.push(name.to_os_string());
                existing.pop();
            }
            Err(error) => {
                return Err(ToolHandlerError::Execution {
                    tool_id: tool_id.to_string(),
                    reason: format!("path '{}' cannot be inspected: {error}", target.as_str()),
                });
            }
        }
    }
    let mut resolved =
        std::fs::canonicalize(&existing).map_err(|error| ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason: format!("path '{}' cannot be resolved: {error}", target.as_str()),
        })?;
    for component in missing_suffix.iter().rev() {
        resolved.push(component);
    }
    let resolved = AbsPath::new(&resolved).map_err(|error| ToolHandlerError::Execution {
        tool_id: tool_id.to_string(),
        reason: format!("resolved path failed closed: {error}"),
    })?;
    ctx.sandbox
        .authorize_read(&resolved)
        .map_err(|error| execution(tool_id, error))?;
    Ok(resolved)
}

pub(crate) fn relative_room_path(
    ctx: &ToolHandlerContext,
    _tool_id: &str,
    path: &AbsPath,
) -> Result<String, ToolHandlerError> {
    Ok(display_room_path(&ctx.room_root, path.as_path()))
}

fn display_room_path(room_root: &Path, path: &Path) -> String {
    let Ok(relative) = path.strip_prefix(room_root) else {
        return path.to_string_lossy().to_string();
    };
    if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.to_string_lossy().to_string()
    }
}

fn bounded_optional_usize_argument(
    tool_id: &str,
    arguments: &serde_json::Value,
    name: &str,
    minimum: usize,
    maximum: usize,
) -> Result<Option<usize>, ToolHandlerError> {
    match arguments.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| (minimum..=maximum).contains(value))
            .map(Some)
            .ok_or_else(|| ToolHandlerError::InvalidArguments {
                tool_id: tool_id.to_string(),
                reason: format!(
                    "arguments field '{name}' must be an integer between {minimum} and {maximum}"
                ),
            }),
        Some(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!(
                "arguments field '{name}' must be an integer between {minimum} and {maximum}"
            ),
        }),
    }
}

fn bounded_optional_u64_argument(
    tool_id: &str,
    arguments: &serde_json::Value,
    name: &str,
    minimum: u64,
    maximum: u64,
) -> Result<Option<u64>, ToolHandlerError> {
    match arguments.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .filter(|value| (minimum..=maximum).contains(value))
            .map(Some)
            .ok_or_else(|| ToolHandlerError::InvalidArguments {
                tool_id: tool_id.to_string(),
                reason: format!(
                    "arguments field '{name}' must be an integer between {minimum} and {maximum}"
                ),
            }),
        Some(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!(
                "arguments field '{name}' must be an integer between {minimum} and {maximum}"
            ),
        }),
    }
}

fn optional_bool_argument(
    tool_id: &str,
    arguments: &serde_json::Value,
    name: &str,
) -> Result<Option<bool>, ToolHandlerError> {
    match arguments.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!("arguments field '{name}' must be a boolean"),
        }),
    }
}
