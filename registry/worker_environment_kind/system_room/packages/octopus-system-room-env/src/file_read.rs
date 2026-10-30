use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_interfaces::InterfaceArtifactFile;
use octopus_ledger::ContentHash;
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo,
};
use octopus_tool_handlers::{ToolResultArtifact, ToolResultError};
use octopus_worker_env::WorkerEnvironmentLifecycle;
use serde::Deserialize;

struct FileReadProcess {
    roots: BTreeMap<String, PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, tag = "operation", rename_all = "snake_case")]
enum ReadConfig {
    Read { max_bytes: u64 },
    List { max_entries: usize },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    path: PathBuf,
    after: Option<String>,
    limit: usize,
}

impl ToolProcessHandler for FileReadProcess {
    fn substrate_family(&self) -> &str {
        super::SUBSTRATE_FAMILY
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
            let call = super::decode_invocation(&invocation.descriptor, &invocation.envelope)
                .map_err(super::capability)?;
            let formation = invocation.worker_environment.as_ref().ok_or_else(|| {
                super::capability("file read requires a formed worker environment")
            })?;
            let granted = invocation
                .envelope
                .worker_environment_kinds()
                .map_err(super::capability)?;
            if !invocation
                .descriptor
                .supported_worker_environment_kinds
                .iter()
                .any(|kind| granted.iter().any(|item| item == kind.as_str()))
            {
                return Err(super::capability(
                    "file read room is absent from the formed grant",
                ));
            }
            let result = execute(
                &invocation.descriptor.handler_config,
                &call.arguments,
                &self.roots,
            );
            let (lifecycle, response) = match result {
                Ok(body) => (
                    WorkerEnvironmentLifecycle::Active,
                    ToolResultArtifact::success(&call.tool, call.correlation_id, body),
                ),
                Err(error) => (
                    WorkerEnvironmentLifecycle::Failed,
                    ToolResultArtifact::failure(
                        &call.tool,
                        call.correlation_id,
                        ToolResultError::new("file_read_failed", error),
                    ),
                ),
            };
            io.record_worker_environment_lifecycle(formation, lifecycle)
                .await
                .map_err(ToolProcessError::Unavailable)?;
            Ok(vec![super::materialize_response(
                &invocation.envelope,
                response,
            )
            .map_err(super::capability)?])
        })
    }
}

pub async fn run_file_read_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    octopus_tool_door_runtime::run_tool_process(Arc::new(FileReadProcess {
        roots: environment.root_slots,
    }))
    .await
}

fn admitted_path(path: &Path, roots: &BTreeMap<String, PathBuf>) -> Result<PathBuf, String> {
    if !path.is_absolute() || roots.is_empty() {
        return Err("file read requires an absolute path and explicit room roots".into());
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("cannot resolve '{}': {error}", path.display()))?;
    for root in roots.values() {
        if !root.is_absolute() {
            return Err("file read room root is not absolute".into());
        }
        let resolved = root.canonicalize().map_err(|error| {
            format!("cannot resolve granted root '{}': {error}", root.display())
        })?;
        if canonical.starts_with(resolved) {
            return Ok(canonical);
        }
    }
    Err(format!(
        "path '{}' is outside this room's granted roots",
        path.display()
    ))
}

fn execute(
    config: &serde_json::Value,
    arguments: &serde_json::Value,
    roots: &BTreeMap<String, PathBuf>,
) -> Result<serde_json::Value, String> {
    match serde_json::from_value::<ReadConfig>(config.clone())
        .map_err(|error| format!("file read descriptor is invalid: {error}"))?
    {
        ReadConfig::Read { max_bytes } => {
            if max_bytes == 0 || max_bytes == u64::MAX {
                return Err("max_bytes must be a finite positive limit".into());
            }
            let args: ReadArgs =
                serde_json::from_value(arguments.clone()).map_err(|error| error.to_string())?;
            let path = admitted_path(&args.path, roots)?;
            let mut file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
            let before = file.metadata().map_err(|error| error.to_string())?;
            if !before.is_file() {
                return Err(
                    "file.read accepts regular files only; use file.list for directories".into(),
                );
            }
            let mut bytes = Vec::new();
            (&mut file)
                .take(max_bytes + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() as u64 > max_bytes {
                return Err(format!("file exceeds descriptor max_bytes={max_bytes}"));
            }
            let after = file.metadata().map_err(|error| error.to_string())?;
            if before.len() != after.len()
                || before.modified().map_err(|error| error.to_string())?
                    != after.modified().map_err(|error| error.to_string())?
            {
                return Err("file changed during reading; read it again before authoring".into());
            }
            let content_hash = ContentHash::of_bytes(&bytes).to_hex();
            let text = String::from_utf8(bytes).map_err(|_| "file.read accepts UTF-8 text only")?;
            Ok(
                serde_json::json!({"path": path, "content": text, "content_hash": content_hash, "bytes": text.len()}),
            )
        }
        ReadConfig::List { max_entries } => {
            let args: ListArgs =
                serde_json::from_value(arguments.clone()).map_err(|error| error.to_string())?;
            if args.limit == 0 || args.limit > max_entries {
                return Err(format!("limit must be between 1 and {max_entries}"));
            }
            let path = admitted_path(&args.path, roots)?;
            if !path.is_dir() {
                return Err("file.list accepts directories only".into());
            }
            let mut entries = Vec::new();
            for entry in std::fs::read_dir(&path).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "directory contains a non-UTF-8 filename")?;
                if args.after.as_ref().is_some_and(|after| name <= *after) {
                    continue;
                }
                let kind = entry.file_type().map_err(|error| error.to_string())?;
                entries.push((
                    name,
                    if kind.is_symlink() {
                        "symlink"
                    } else if kind.is_file() {
                        "file"
                    } else if kind.is_dir() {
                        "directory"
                    } else {
                        "other"
                    },
                ));
            }
            entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            let has_more = entries.len() > args.limit;
            entries.truncate(args.limit);
            let next_after =
                has_more.then(|| entries.last().expect("positive page size").0.clone());
            let entries = entries.into_iter().map(|(name, kind)| serde_json::json!({"path": path.join(&name), "name": name, "type": kind})).collect::<Vec<_>>();
            Ok(serde_json::json!({"path":path,"entries":entries,"next_after":next_after}))
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/file_read.rs"]
mod tests;
