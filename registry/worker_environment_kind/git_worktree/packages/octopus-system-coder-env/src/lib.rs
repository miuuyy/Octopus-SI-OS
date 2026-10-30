use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use octopus_git_sandbox::{
    GIT_COMMON_ROOT_SLOT, GitWorktreeEvidence, GitWorktreeEvidenceLimits, REPO_ROOT_SLOT,
    WORKTREE_ROOT_SLOT,
};
use octopus_system_workspace_env::{
    LayoutCall, LayoutOperation, WorkspaceExecHandler, WorkspaceFsHandler, WorkspaceLayout,
    run_workspace_room_tool_process,
};
use octopus_tool_door_runtime::ToolProcessEnvironment;
use octopus_tool_handlers::{ToolHandler, ToolHandlerResult};
use octopus_worker_env::{
    AbsPath, WorkerEnvDriver, WorkerEnvError, WorkerEnvKindSpec, WorkerEnvironmentFormation,
};
use serde_json::{Value, json};

#[cfg(test)]
mod tests;

const ROOM: &str = "system_coder_env";
const WORKTREE_OPERATIONS: &[&str] = &[
    "worktree_status",
    "worktree_diff",
    "worktree_snapshot",
    "worktree_abandon",
];

pub struct GitWorktree {
    repo_root: PathBuf,
    worktree_root: PathBuf,
    git_common_root: PathBuf,
    worktree_state_root: PathBuf,
}

impl GitWorktree {
    pub fn new(
        repo_root: PathBuf,
        worktree_root: PathBuf,
        git_common_root: PathBuf,
        worktree_state_root: PathBuf,
    ) -> Result<Self, String> {
        let absolute = |path: &PathBuf, name: &str| {
            AbsPath::new(path)
                .map(|path| path.as_path().to_path_buf())
                .map_err(|error| format!("{ROOM} {name} failed closed: {error}"))
        };
        Ok(Self {
            repo_root: absolute(&repo_root, "repo root")?,
            worktree_root: absolute(&worktree_root, "worktree root")?,
            git_common_root: absolute(&git_common_root, "git common root")?,
            worktree_state_root: absolute(&worktree_state_root, "worktree state root")?,
        })
    }

    pub fn from_process_environment(env: &ToolProcessEnvironment) -> Result<Self, String> {
        let worktree_state_root = env
            .durable_var_dir
            .clone()
            .ok_or_else(|| format!("{ROOM} requires formation-scoped durable process state"))?;
        Self::new(
            env.root_slot(REPO_ROOT_SLOT)?.to_path_buf(),
            env.root_slot(WORKTREE_ROOT_SLOT)?.to_path_buf(),
            env.root_slot(GIT_COMMON_ROOT_SLOT)?.to_path_buf(),
            worktree_state_root,
        )
    }
}

impl WorkspaceLayout for GitWorktree {
    fn room(&self) -> &'static str {
        ROOM
    }

    fn room_root(&self, formation: &WorkerEnvironmentFormation) -> Result<PathBuf, String> {
        octopus_git_sandbox::instance_worktree_path(
            &self.worktree_root,
            &formation.worker_environment_id,
        )
        .map_err(|error| error.to_string())
    }

    fn lease_parts(
        &self,
        kind: &WorkerEnvKindSpec,
    ) -> Result<(BTreeMap<String, AbsPath>, Arc<dyn WorkerEnvDriver>), WorkerEnvError> {
        let driver = octopus_git_sandbox::git_worktree_driver(&self.worktree_state_root).map_err(
            |error| WorkerEnvError::OpenFailed {
                substrate_family: kind.substrate_family.clone(),
                reason: error.to_string(),
            },
        )?;
        Ok((
            BTreeMap::from([
                (REPO_ROOT_SLOT.to_string(), AbsPath::new(&self.repo_root)?),
                (WORKTREE_ROOT_SLOT.to_string(), AbsPath::new(&self.worktree_root)?),
                (
                    GIT_COMMON_ROOT_SLOT.to_string(),
                    AbsPath::new(&self.git_common_root)?,
                ),
            ]),
            driver,
        ))
    }

    fn release_unleased(&self, formation: &WorkerEnvironmentFormation) -> Result<(), String> {
        octopus_git_sandbox::release_git_worktree(
            &self.repo_root,
            &self.worktree_state_root,
            &self.room_root(formation)?,
            formation.worker_environment_id.as_str(),
        )
        .map_err(|error| error.to_string())
    }

    fn operation(&self, call: LayoutCall<'_>) -> Result<Option<LayoutOperation>, String> {
        let Some(config) = worktree_handler_config(call.tool_id, call.handler_config)? else {
            return Ok(None);
        };
        let limits = GitWorktreeEvidenceLimits::new(config.max_diff_bytes, config.max_changed_paths)
            .map_err(|error| error.to_string())?;
        let room_root = call.room_root.to_path_buf();
        let worker_environment_id = call.formation.worker_environment_id.as_str().to_string();
        if config.operation == WorktreeOperation::WorktreeAbandon {
            let repo_root = self.repo_root.clone();
            let state_root = self.worktree_state_root.clone();
            let cleanup_timeout = config.cleanup_timeout_ms.unwrap_or_default();
            return Ok(Some(LayoutOperation::EndsRoom {
                cleanup_timeout: Duration::from_millis(cleanup_timeout),
                run: Box::new(move || {
                    let evidence = octopus_git_sandbox::abandon_git_worktree(
                        &repo_root,
                        &state_root,
                        &room_root,
                        &worker_environment_id,
                        limits,
                    )
                    .map_err(|error| error.to_string())?;
                    Ok(ToolHandlerResult::new(json!({
                        "result": "worktree_abandoned",
                        "worker_environment_id": worker_environment_id,
                        "evidence": evidence_json(evidence),
                        "branch_deleted": true,
                        "worktree_deleted": true
                    })))
                }),
            }));
        }
        let operation = config.operation;
        Ok(Some(LayoutOperation::InRoom(Box::new(move || {
            let evidence = octopus_git_sandbox::capture_git_worktree_evidence(&room_root, limits)
                .map_err(|error| error.to_string())?;
            Ok(ToolHandlerResult::new(match operation {
                WorktreeOperation::WorktreeStatus => json!({
                    "result": "worktree_status",
                    "worker_environment_id": worker_environment_id,
                    "branch": evidence.branch,
                    "head_commit": evidence.head_commit,
                    "changed_paths": evidence.changed_paths,
                    "clean": evidence.clean,
                }),
                WorktreeOperation::WorktreeDiff => json!({
                    "result": "worktree_diff",
                    "worker_environment_id": worker_environment_id,
                    "diff_base": evidence.diff_base,
                    "unified_diff": evidence.unified_diff,
                    "diff_blake3": evidence.diff_blake3,
                    "changed_paths": evidence.changed_paths,
                    "line_count": evidence.line_count,
                    "clean": evidence.clean,
                }),
                _ => json!({
                    "result": "worktree_snapshot",
                    "worker_environment_id": worker_environment_id,
                    "evidence": evidence_json(evidence)
                }),
            }))
        }))))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeOperation {
    WorktreeStatus,
    WorktreeDiff,
    WorktreeSnapshot,
    WorktreeAbandon,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeHandlerConfig {
    pub operation: WorktreeOperation,
    max_diff_bytes: usize,
    max_changed_paths: usize,
    #[serde(default)]
    cleanup_timeout_ms: Option<u64>,
}

pub fn worktree_handler_config(
    tool_id: &str,
    handler_config: &Value,
) -> Result<Option<WorktreeHandlerConfig>, String> {
    let Some(operation) = handler_config.get("operation").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !WORKTREE_OPERATIONS.contains(&operation) {
        return Ok(None);
    }
    let config: WorktreeHandlerConfig = serde_json::from_value(handler_config.clone())
        .map_err(|error| format!("tool '{tool_id}' has malformed worktree handler_config: {error}"))?;
    match (config.operation, config.cleanup_timeout_ms) {
        (WorktreeOperation::WorktreeAbandon, Some(timeout)) if timeout > 0 && timeout <= 60_000 => {}
        (WorktreeOperation::WorktreeAbandon, _) => {
            return Err(format!(
                "tool '{tool_id}' worktree abandon operation requires a cleanup timeout of at most 60000 ms"
            ));
        }
        (_, Some(_)) => {
            return Err(format!(
                "tool '{tool_id}' worktree evidence operation cannot declare a cleanup timeout"
            ));
        }
        (_, None) => {}
    }
    Ok(Some(config))
}

fn evidence_json(evidence: GitWorktreeEvidence) -> Value {
    json!({
        "branch": evidence.branch,
        "head_commit": evidence.head_commit,
        "diff_base": evidence.diff_base,
        "unified_diff": evidence.unified_diff,
        "diff_blake3": evidence.diff_blake3,
        "changed_paths": evidence.changed_paths,
        "line_count": evidence.line_count,
        "clean": evidence.clean
    })
}

fn git_layout(env: &ToolProcessEnvironment) -> Result<Box<dyn WorkspaceLayout>, String> {
    Ok(Box::new(GitWorktree::from_process_environment(env)?))
}

async fn run_coder_tool_process(handler: Arc<dyn ToolHandler>) -> Result<(), String> {
    run_workspace_room_tool_process(git_layout, handler).await
}

pub async fn run_coder_fs_tool_process() -> Result<(), String> {
    run_coder_tool_process(Arc::new(WorkspaceFsHandler)).await
}

pub async fn run_coder_exec_tool_process() -> Result<(), String> {
    run_coder_tool_process(Arc::new(WorkspaceExecHandler)).await
}
