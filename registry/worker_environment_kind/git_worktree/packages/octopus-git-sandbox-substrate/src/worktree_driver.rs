use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use octopus_worker_env::{
    AbsPath, EnvContext, ReleaseHook, WekDescriptorHash, WorkerEnvCaps, WorkerEnvDriver,
    WorkerEnvError, WorkerEnvKindSpec, WorkerEnvironment, WorkerEnvironmentFormation,
    WorkerEnvironmentId, WorkerEnvironmentParts,
};
use octopus_workspace_substrate::boundary::project_sandbox;
use octopus_workspace_substrate::{FAMILY_PROCESS_EXEC, gated_file_surface};

pub const REPO_ROOT_SLOT: &str = "repo_root";
pub const WORKTREE_ROOT_SLOT: &str = "worktree_root";
pub const PROTECTED_BRANCH_SLOT: &str = "protected_branch_ref";
pub const GIT_COMMON_ROOT_SLOT: &str = "git_common_root";

const PROTECTED_BRANCHES: &[&str] = &["main", "master", "trunk", "release"];
const ABANDONED_FORMATIONS_DIR: &str = "octopus/abandoned-worker-environments";
const MAX_WORKTREE_DIFF_BYTES: usize = 64 * 1024 * 1024;
const MAX_WORKTREE_CHANGED_PATHS: usize = 100_000;
const MAX_GIT_STDERR_BYTES: usize = 256 * 1024;
const MAX_ABANDONED_FORMATION_BYTES: u64 = (MAX_WORKTREE_DIFF_BYTES as u64) * 3 + 8 * 1024 * 1024;
static ABANDONED_FORMATION_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn instance_worktree_path(
    base: &Path,
    worker_environment_id: &WorkerEnvironmentId,
) -> Result<PathBuf, WorkerEnvError> {
    let base = AbsPath::new(base)?;
    let instance = AbsPath::new(&base.as_path().join(worker_environment_id.as_str()))?;
    if !instance.is_within(&base) || instance == base {
        return Err(WorkerEnvError::OpenFailed {
            substrate_family: "git_worktree".to_string(),
            reason: format!(
                "worker environment '{}' does not resolve below worktree base '{}'",
                worker_environment_id,
                base.as_str()
            ),
        });
    }
    Ok(instance.as_path().to_path_buf())
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitWorktreeEvidence {
    pub branch: String,
    pub head_commit: String,
    pub diff_base: String,
    pub unified_diff: String,
    pub diff_blake3: String,
    pub changed_paths: Vec<String>,
    pub line_count: usize,
    pub clean: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GitWorktreeEvidenceLimits {
    max_diff_bytes: usize,
    max_changed_paths: usize,
}

impl GitWorktreeEvidenceLimits {
    pub fn new(max_diff_bytes: usize, max_changed_paths: usize) -> Result<Self, WorkerEnvError> {
        if max_diff_bytes == 0 || max_diff_bytes > MAX_WORKTREE_DIFF_BYTES {
            return Err(open_failed(format!(
                "worktree evidence max_diff_bytes must be within 1..={MAX_WORKTREE_DIFF_BYTES}"
            )));
        }
        if max_changed_paths == 0 || max_changed_paths > MAX_WORKTREE_CHANGED_PATHS {
            return Err(open_failed(format!(
                "worktree evidence max_changed_paths must be within 1..={MAX_WORKTREE_CHANGED_PATHS}"
            )));
        }
        Ok(Self {
            max_diff_bytes,
            max_changed_paths,
        })
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AbandonedFormation {
    schema_version: u32,
    worker_environment_id: String,
    evidence: GitWorktreeEvidence,
}

pub fn git_worktree_driver(
    state_root: &Path,
) -> Result<Arc<dyn WorkerEnvDriver>, WorkerEnvError> {
    Ok(Arc::new(GitWorktreeDriver {
        state_root: AbsPath::new(state_root)?,
    }))
}

struct GitWorktreeDriver {
    state_root: AbsPath,
}

impl WorkerEnvDriver for GitWorktreeDriver {
    fn substrate_family(&self) -> &str {
        FAMILY_PROCESS_EXEC
    }
    fn open(
        &self,
        spec: &WorkerEnvKindSpec,
        descriptor_hash: WekDescriptorHash,
        caps: WorkerEnvCaps,
        ctx: EnvContext,
        id: WorkerEnvironmentId,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<WorkerEnvironment, WorkerEnvError>> + Send + '_,
        >,
    > {
        let spec = spec.clone();
        Box::pin(async move {
            let mut ctx = ctx;
            let repo_root = ctx.resolve_root(REPO_ROOT_SLOT)?;
            let worktree_base = ctx.resolve_root(WORKTREE_ROOT_SLOT)?;
            let worktree_root =
                AbsPath::new(&instance_worktree_path(worktree_base.as_path(), &id)?)?;
            let git_common_root = ctx.resolve_root(GIT_COMMON_ROOT_SLOT)?;
            ctx.root_slots
                .insert(WORKTREE_ROOT_SLOT.to_string(), worktree_root.clone());
            ctx.root_slots.insert(
                PROTECTED_BRANCH_SLOT.to_string(),
                AbsPath::new(&worktree_root.as_path().join(".git"))?,
            );
            let sandbox = project_sandbox(&spec, &caps, &ctx)?;
            let state_root = self.state_root.clone();
            sandbox.authorize_write(&worktree_root)?;
            sandbox.authorize_write(&git_common_root)?;
            ensure_formation_not_abandoned(&state_root, &id)?;
            let branch = branch_for_formation(id.as_str(), &worktree_root)?;

            materialize_worktree(&repo_root, &worktree_root, &branch)?;

            let fs = gated_file_surface(Arc::new(sandbox.clone()), worktree_root.clone());

            let release: ReleaseHook = {
                let repo_root = repo_root.clone();
                let state_root = state_root.clone();
                let worktree_root = worktree_root.clone();
                let env_id = id.as_str().to_string();
                Box::new(move || {
                    Box::pin(async move {
                        release_worktree(&repo_root, &state_root, &worktree_root, &env_id)
                    })
                })
            };
            WorkerEnvironment::assemble(
                &spec,
                descriptor_hash,
                id,
                WorkerEnvironmentParts {
                    sandbox,
                    cwd: worktree_root,
                    fs,
                    release: Some(release),
                },
            )
        })
    }
}

pub async fn lease_git_worktree(
    spec: &WorkerEnvKindSpec,
    formation: &WorkerEnvironmentFormation,
    repo_root: &Path,
    worktree_root: &Path,
    state_root: &Path,
    ee_root_allowance: Option<BTreeSet<String>>,
) -> Result<WorkerEnvironment, WorkerEnvError> {
    let git_common_root = formation
        .door_root_bindings
        .get(GIT_COMMON_ROOT_SLOT)
        .cloned()
        .ok_or_else(|| {
            open_failed(format!(
                "formed worker environment has no '{GIT_COMMON_ROOT_SLOT}' root binding"
            ))
        })?;
    let extra_root_slots = BTreeMap::from([(GIT_COMMON_ROOT_SLOT.to_string(), git_common_root)]);
    lease_git_worktree_with_root_slots(
        spec,
        formation,
        repo_root,
        worktree_root,
        state_root,
        &extra_root_slots,
        ee_root_allowance,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn lease_git_worktree_with_root_slots(
    spec: &WorkerEnvKindSpec,
    formation: &WorkerEnvironmentFormation,
    repo_root: &Path,
    worktree_root: &Path,
    state_root: &Path,
    extra_root_slots: &BTreeMap<String, PathBuf>,
    ee_root_allowance: Option<BTreeSet<String>>,
) -> Result<WorkerEnvironment, WorkerEnvError> {
    let repo_root_abs = AbsPath::new(repo_root)?;
    let worktree_abs = AbsPath::new(worktree_root)?;
    let mut root_slots = BTreeMap::from([
        (REPO_ROOT_SLOT.to_string(), repo_root_abs),
        (WORKTREE_ROOT_SLOT.to_string(), worktree_abs),
    ]);
    for (name, path) in extra_root_slots {
        let slot = name.trim();
        if slot.is_empty() {
            return Err(WorkerEnvError::CapExceedsBoundary(name.clone()));
        }
        if root_slots.contains_key(slot) {
            return Err(WorkerEnvError::CapExceedsBoundary(slot.to_string()));
        }
        root_slots.insert(slot.to_string(), AbsPath::new(path)?);
    }
    let ctx = EnvContext {
        additional_root_slots: formation.door_additional_root_bindings.iter().map(|(slot, paths)| Ok((slot.clone(), paths.iter().map(|path| AbsPath::new(path)).collect::<Result<Vec<_>, _>>()?))).collect::<Result<_, WorkerEnvError>>()?,
        root_slots,
        env: BTreeMap::new(),
    };

    octopus_worker_env::lease(
        spec,
        formation,
        git_worktree_driver(state_root)?,
        ctx,
        ee_root_allowance.as_ref(),
    )
    .await
}

pub fn capture_git_worktree_evidence(
    worktree_root: &Path,
    limits: GitWorktreeEvidenceLimits,
) -> Result<GitWorktreeEvidence, WorkerEnvError> {
    let worktree_root = AbsPath::new(worktree_root)?;
    let branch = current_worktree_branch(&worktree_root)?;
    ensure_run_branch(&branch)?;
    let head_commit = git_utf8_line(
        &worktree_root,
        &["rev-parse", "--verify", "HEAD"],
        "reading worktree HEAD commit",
    )?;
    let mut tracked_paths = git_nul_paths_bounded(
        &worktree_root,
        &["diff", "--name-only", "-z", "HEAD", "--"],
        "collecting tracked worktree paths",
        limits.max_diff_bytes,
        limits.max_changed_paths,
    )?;
    let untracked_paths = git_nul_paths_bounded(
        &worktree_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        "collecting untracked worktree paths",
        limits.max_diff_bytes,
        limits.max_changed_paths,
    )?;
    tracked_paths.extend(untracked_paths.iter().cloned());
    tracked_paths.sort();
    tracked_paths.dedup();
    if tracked_paths.len() > limits.max_changed_paths {
        return Err(open_failed(format!(
            "worktree evidence names {} changed paths, exceeding descriptor limit {}",
            tracked_paths.len(),
            limits.max_changed_paths
        )));
    }

    let mut unified = git_capture_bounded(
        &worktree_root,
        &["diff", "--binary", "--no-ext-diff", "HEAD", "--"],
        "collecting tracked worktree diff",
        limits.max_diff_bytes,
        &[0],
    )?;
    for path in &untracked_paths {
        let source = worktree_root.as_path().join(path);
        let file_type = std::fs::symlink_metadata(&source)
            .map_err(|error| {
                open_failed(format!(
                    "reading untracked worktree path {} failed: {error}",
                    path.display()
                ))
            })?
            .file_type();
        if !file_type.is_file() && !file_type.is_symlink() {
            return Err(open_failed(format!(
                "untracked worktree path {} is neither a file nor a symlink",
                path.display()
            )));
        }
        let remaining = limits.max_diff_bytes.saturating_sub(unified.len());
        if remaining == 0 {
            return Err(open_failed(format!(
                "worktree diff exceeds descriptor limit of {} bytes",
                limits.max_diff_bytes
            )));
        }
        let path = path.to_str().ok_or_else(|| {
            open_failed(format!(
                "untracked worktree path is not utf-8: {}",
                path.display()
            ))
        })?;
        let patch = git_capture_bounded(
            &worktree_root,
            &[
                "diff",
                "--no-index",
                "--binary",
                "--no-ext-diff",
                "--",
                "/dev/null",
                path,
            ],
            "collecting untracked worktree diff",
            remaining,
            &[0, 1],
        )?;
        if !unified.is_empty() && !unified.ends_with(b"\n") {
            unified.push(b'\n');
        }
        unified.extend_from_slice(&patch);
    }
    if unified.len() > limits.max_diff_bytes {
        return Err(open_failed(format!(
            "worktree diff exceeds descriptor limit of {} bytes",
            limits.max_diff_bytes
        )));
    }
    let unified_diff = String::from_utf8(unified)
        .map_err(|error| open_failed(format!("worktree diff is not utf-8: {error}")))?;
    let line_count = unified_diff
        .lines()
        .filter(|line| {
            (line.starts_with('+') && !line.starts_with("+++"))
                || (line.starts_with('-') && !line.starts_with("---"))
        })
        .count();
    let changed_paths = tracked_paths
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    Ok(GitWorktreeEvidence {
        branch,
        head_commit,
        diff_base: "HEAD".to_string(),
        diff_blake3: blake3::hash(unified_diff.as_bytes()).to_hex().to_string(),
        unified_diff,
        clean: changed_paths.is_empty(),
        changed_paths,
        line_count,
    })
}

pub fn abandon_git_worktree(
    repo_root: &Path,
    state_root: &Path,
    worktree_root: &Path,
    worker_environment_id: &str,
    limits: GitWorktreeEvidenceLimits,
) -> Result<GitWorktreeEvidence, WorkerEnvError> {
    let repo_root = AbsPath::new(repo_root)?;
    let state_root = AbsPath::new(state_root)?;
    let worktree_root = AbsPath::new(worktree_root)?;
    let expected_branch = branch_for_formation(worker_environment_id, &worktree_root)?;
    let marker = abandoned_formation_path(&state_root, worker_environment_id);
    if marker.exists() {
        let abandoned = read_abandoned_formation(&marker, worker_environment_id)?;
        if abandoned.evidence.branch != expected_branch {
            return Err(open_failed(format!(
                "abandoned-formation branch '{}' does not match formed branch '{}'",
                abandoned.evidence.branch, expected_branch
            )));
        }
        complete_worktree_abandon(&repo_root, &worktree_root, &abandoned.evidence)?;
        return Ok(abandoned.evidence);
    }
    let evidence = capture_git_worktree_evidence(worktree_root.as_path(), limits)?;
    if evidence.branch != expected_branch {
        return Err(open_failed(format!(
            "worktree branch '{}' does not match formed branch '{}'",
            evidence.branch, expected_branch
        )));
    }
    let abandoned = AbandonedFormation {
        schema_version: 1,
        worker_environment_id: worker_environment_id.to_string(),
        evidence: evidence.clone(),
    };
    write_abandoned_formation(&marker, &abandoned)?;
    complete_worktree_abandon(&repo_root, &worktree_root, &evidence)?;
    Ok(evidence)
}

fn release_worktree(
    repo_root: &AbsPath,
    state_root: &AbsPath,
    worktree_root: &AbsPath,
    env_id: &str,
) -> Result<(), WorkerEnvError> {
    let expected_branch = branch_for_formation(env_id, worktree_root).map_err(|error| {
        WorkerEnvError::ReleaseFailed {
            id: env_id.to_string(),
            reason: error.to_string(),
        }
    })?;
    let marker = abandoned_formation_path(state_root, env_id);
    if marker.exists() {
        let abandoned = read_abandoned_formation(&marker, env_id).map_err(|error| {
            WorkerEnvError::ReleaseFailed {
                id: env_id.to_string(),
                reason: error.to_string(),
            }
        })?;
        if abandoned.evidence.branch != expected_branch {
            return Err(WorkerEnvError::ReleaseFailed {
                id: env_id.to_string(),
                reason: format!(
                    "abandoned-formation branch '{}' does not match formed branch '{}'",
                    abandoned.evidence.branch, expected_branch
                ),
            });
        }
        return complete_worktree_abandon(repo_root, worktree_root, &abandoned.evidence).map_err(
            |error| WorkerEnvError::ReleaseFailed {
                id: env_id.to_string(),
                reason: error.to_string(),
            },
        );
    }
    match std::fs::symlink_metadata(worktree_root.as_path()) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(attached_path) = worktree_path_for_branch(repo_root, &expected_branch)
                .map_err(|error| WorkerEnvError::ReleaseFailed {
                    id: env_id.to_string(),
                    reason: error.to_string(),
                })?
            {
                return Err(WorkerEnvError::ReleaseFailed {
                    id: env_id.to_string(),
                    reason: format!(
                        "formed worktree path is absent but branch {expected_branch} remains attached at {}",
                        attached_path.display()
                    ),
                });
            }
            branch_head(repo_root, &expected_branch).map_err(|error| {
                WorkerEnvError::ReleaseFailed {
                    id: env_id.to_string(),
                    reason: error.to_string(),
                }
            })?;
            return Ok(());
        }
        Err(error) => {
            return Err(WorkerEnvError::ReleaseFailed {
                id: env_id.to_string(),
                reason: format!("formed worktree path cannot be inspected: {error}"),
            });
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(WorkerEnvError::ReleaseFailed {
                id: env_id.to_string(),
                reason: "formed worktree path is not a regular directory".to_string(),
            });
        }
        Ok(_) => {}
    }
    let branch =
        current_worktree_branch(worktree_root).map_err(|error| WorkerEnvError::ReleaseFailed {
            id: env_id.to_string(),
            reason: error.to_string(),
        })?;
    if branch != expected_branch {
        return Err(WorkerEnvError::ReleaseFailed {
            id: env_id.to_string(),
            reason: format!(
                "worktree branch '{branch}' does not match formed branch '{expected_branch}'"
            ),
        });
    }
    let head_commit =
        branch_head(repo_root, &branch).map_err(|error| WorkerEnvError::ReleaseFailed {
            id: env_id.to_string(),
            reason: error.to_string(),
        })?;
    let status = git_capture_bounded(
        worktree_root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        "checking worktree cleanliness before release",
        1024 * 1024,
        &[0],
    )
    .map_err(|error| WorkerEnvError::ReleaseFailed {
        id: env_id.to_string(),
        reason: error.to_string(),
    })?;
    if !status.is_empty() {
        return Err(WorkerEnvError::ReleaseFailed {
            id: env_id.to_string(),
            reason: "dirty worktree cannot be released because that would discard uncommitted capability; snapshot and commit it or abandon it explicitly"
                .to_string(),
        });
    }
    git_capture(
        repo_root,
        &["worktree", "remove", worktree_root.as_str()],
        "releasing the leased worktree",
    )
    .map_err(|e| WorkerEnvError::ReleaseFailed {
        id: env_id.to_string(),
        reason: e.to_string(),
    })?;
    git_capture(
        repo_root,
        &["worktree", "prune"],
        "pruning released worktrees",
    )
    .map_err(|e| WorkerEnvError::ReleaseFailed {
        id: env_id.to_string(),
        reason: e.to_string(),
    })?;
    if worktree_path_for_branch(repo_root, &branch)
        .map_err(|error| WorkerEnvError::ReleaseFailed {
            id: env_id.to_string(),
            reason: error.to_string(),
        })?
        .is_some()
    {
        return Err(WorkerEnvError::ReleaseFailed {
            id: env_id.to_string(),
            reason: format!("released branch {branch} is still attached to a worktree"),
        });
    }
    let preserved_head =
        branch_head(repo_root, &branch).map_err(|error| WorkerEnvError::ReleaseFailed {
            id: env_id.to_string(),
            reason: error.to_string(),
        })?;
    if preserved_head != head_commit {
        return Err(WorkerEnvError::ReleaseFailed {
            id: env_id.to_string(),
            reason: format!(
                "released branch {branch} moved from {head_commit} to {preserved_head} during release"
            ),
        });
    }
    Ok(())
}

pub fn release_git_worktree(
    repo_root: &Path,
    state_root: &Path,
    worktree_root: &Path,
    worker_environment_id: &str,
) -> Result<(), WorkerEnvError> {
    let repo_root = AbsPath::new(repo_root)?;
    let state_root = AbsPath::new(state_root)?;
    let worktree_root = AbsPath::new(worktree_root)?;
    release_worktree(
        &repo_root,
        &state_root,
        &worktree_root,
        worker_environment_id,
    )
}

fn ensure_formation_not_abandoned(
    state_root: &AbsPath,
    id: &WorkerEnvironmentId,
) -> Result<(), WorkerEnvError> {
    let marker = abandoned_formation_path(state_root, id.as_str());
    if marker.exists() {
        return Err(open_failed(format!(
            "worker environment '{}' was explicitly abandoned and cannot rematerialize its run branch",
            id.as_str()
        )));
    }
    Ok(())
}

fn abandoned_formation_path(state_root: &AbsPath, worker_environment_id: &str) -> PathBuf {
    let identity = blake3::hash(worker_environment_id.as_bytes())
        .to_hex()
        .to_string();
    state_root
        .as_path()
        .join(ABANDONED_FORMATIONS_DIR)
        .join(format!("{identity}.json"))
}

fn write_abandoned_formation(
    marker: &Path,
    abandoned: &AbandonedFormation,
) -> Result<(), WorkerEnvError> {
    let parent = marker
        .parent()
        .ok_or_else(|| open_failed("abandoned-formation marker has no parent directory"))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        open_failed(format!(
            "creating abandoned-formation marker directory {} failed: {error}",
            parent.display()
        ))
    })?;
    let bytes = serde_json::to_vec(abandoned).map_err(|error| {
        open_failed(format!(
            "serializing abandoned-formation marker failed: {error}"
        ))
    })?;
    if bytes.len() as u64 > MAX_ABANDONED_FORMATION_BYTES {
        return Err(open_failed(format!(
            "abandoned-formation marker {} exceeds {} bytes",
            marker.display(),
            MAX_ABANDONED_FORMATION_BYTES
        )));
    }
    if marker.exists() {
        verify_existing_abandoned_formation(marker, abandoned)?;
        sync_directory(parent)?;
        return Ok(());
    }

    let temporary = create_abandoned_formation_temporary(parent, marker, &bytes)?;
    let linked = match std::fs::hard_link(&temporary, marker) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(open_failed(format!(
                "committing abandoned-formation marker {} failed: {error}",
                marker.display()
            )));
        }
    };
    if linked {
        sync_directory(parent)?;
    } else {
        verify_existing_abandoned_formation(marker, abandoned)?;
    }
    std::fs::remove_file(&temporary).map_err(|error| {
        open_failed(format!(
            "removing abandoned-formation temporary {} failed: {error}",
            temporary.display()
        ))
    })?;
    sync_directory(parent)?;
    Ok(())
}

fn create_abandoned_formation_temporary(
    parent: &Path,
    marker: &Path,
    bytes: &[u8],
) -> Result<PathBuf, WorkerEnvError> {
    let name = marker
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| open_failed("abandoned-formation marker name is not utf-8"))?;
    for _ in 0..128 {
        let sequence = ABANDONED_FORMATION_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(".{name}.tmp-{}-{sequence}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(open_failed(format!(
                        "writing abandoned-formation temporary {} failed: {error}",
                        temporary.display()
                    )));
                }
                return Ok(temporary);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(open_failed(format!(
                    "creating abandoned-formation temporary {} failed: {error}",
                    temporary.display()
                )));
            }
        }
    }
    Err(open_failed(format!(
        "could not allocate an abandoned-formation temporary beside {}",
        marker.display()
    )))
}

fn verify_existing_abandoned_formation(
    marker: &Path,
    abandoned: &AbandonedFormation,
) -> Result<(), WorkerEnvError> {
    let existing = read_abandoned_formation(marker, &abandoned.worker_environment_id)?;
    if existing.evidence != abandoned.evidence {
        return Err(open_failed(format!(
            "abandoned-formation marker {} already exists with different evidence",
            marker.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), WorkerEnvError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            open_failed(format!(
                "syncing abandoned-formation marker directory {} failed: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), WorkerEnvError> {
    Ok(())
}

fn read_abandoned_formation(
    marker: &Path,
    worker_environment_id: &str,
) -> Result<AbandonedFormation, WorkerEnvError> {
    let mut file = std::fs::File::open(marker).map_err(|error| {
        open_failed(format!(
            "opening abandoned-formation marker {} failed: {error}",
            marker.display()
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        open_failed(format!(
            "reading abandoned-formation marker metadata {} failed: {error}",
            marker.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(open_failed(format!(
            "abandoned-formation marker {} is not a regular file",
            marker.display()
        )));
    }
    let size = metadata.len();
    if size > MAX_ABANDONED_FORMATION_BYTES {
        return Err(open_failed(format!(
            "abandoned-formation marker {} exceeds {} bytes",
            marker.display(),
            MAX_ABANDONED_FORMATION_BYTES
        )));
    }
    let mut bytes = Vec::with_capacity(size as usize);
    Read::by_ref(&mut file)
        .take(MAX_ABANDONED_FORMATION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            open_failed(format!(
                "reading abandoned-formation marker {} failed: {error}",
                marker.display()
            ))
        })?;
    if bytes.len() as u64 > MAX_ABANDONED_FORMATION_BYTES {
        return Err(open_failed(format!(
            "abandoned-formation marker {} grew beyond {} bytes while reading",
            marker.display(),
            MAX_ABANDONED_FORMATION_BYTES
        )));
    }
    let abandoned: AbandonedFormation = serde_json::from_slice(&bytes).map_err(|error| {
        open_failed(format!(
            "abandoned-formation marker {} is malformed: {error}",
            marker.display()
        ))
    })?;
    if abandoned.schema_version != 1 || abandoned.worker_environment_id != worker_environment_id {
        return Err(open_failed(format!(
            "abandoned-formation marker {} does not match worker environment '{}'",
            marker.display(),
            worker_environment_id
        )));
    }
    validate_abandoned_evidence(&abandoned.evidence)?;
    Ok(abandoned)
}

fn validate_abandoned_evidence(evidence: &GitWorktreeEvidence) -> Result<(), WorkerEnvError> {
    ensure_run_branch(&evidence.branch)?;
    if evidence.head_commit.trim().is_empty() || evidence.diff_base != "HEAD" {
        return Err(open_failed(
            "abandoned-formation evidence has invalid Git identity",
        ));
    }
    if evidence.diff_blake3
        != blake3::hash(evidence.unified_diff.as_bytes())
            .to_hex()
            .to_string()
    {
        return Err(open_failed(
            "abandoned-formation evidence diff digest does not match its payload",
        ));
    }
    let line_count = evidence
        .unified_diff
        .lines()
        .filter(|line| {
            (line.starts_with('+') && !line.starts_with("+++"))
                || (line.starts_with('-') && !line.starts_with("---"))
        })
        .count();
    if evidence.line_count != line_count || evidence.clean != evidence.changed_paths.is_empty() {
        return Err(open_failed(
            "abandoned-formation evidence summary does not match its payload",
        ));
    }
    Ok(())
}

fn branch_for_formation(
    worker_environment_id: &str,
    worktree_root: &AbsPath,
) -> Result<String, WorkerEnvError> {
    let lease_hash = blake3::hash(worktree_root.as_str().as_bytes());
    let branch = format!(
        "run/{}-{}",
        worker_environment_id,
        &lease_hash.to_hex()[..12]
    );
    ensure_run_branch(&branch)?;
    Ok(branch)
}

fn complete_worktree_abandon(
    repo_root: &AbsPath,
    worktree_root: &AbsPath,
    expected: &GitWorktreeEvidence,
) -> Result<(), WorkerEnvError> {
    validate_abandoned_evidence(expected)?;
    if worktree_root.as_path().exists() {
        let observed = capture_git_worktree_evidence(
            worktree_root.as_path(),
            GitWorktreeEvidenceLimits::new(MAX_WORKTREE_DIFF_BYTES, MAX_WORKTREE_CHANGED_PATHS)?,
        )?;
        if observed != *expected {
            return Err(open_failed(format!(
                "worktree changed after abandon evidence for branch '{}'",
                expected.branch
            )));
        }
        git_capture(
            repo_root,
            &["worktree", "remove", "--force", worktree_root.as_str()],
            "abandoning the leased worktree",
        )?;
    }
    if git_branch_exists(repo_root, &expected.branch)? {
        let head_commit = branch_head(repo_root, &expected.branch)?;
        if head_commit != expected.head_commit {
            return Err(open_failed(format!(
                "run branch {} moved from {} to {} after abandon evidence",
                expected.branch, expected.head_commit, head_commit
            )));
        }
        git_capture(
            repo_root,
            &["branch", "-D", &expected.branch],
            "deleting the abandoned run branch",
        )?;
    }
    git_capture(
        repo_root,
        &["worktree", "prune"],
        "pruning abandoned worktrees",
    )?;
    if git_branch_exists(repo_root, &expected.branch)? {
        return Err(open_failed(format!(
            "abandoned run branch {} still exists after deletion",
            expected.branch
        )));
    }
    if worktree_path_for_branch(repo_root, &expected.branch)?.is_some() {
        return Err(open_failed(format!(
            "abandoned run branch {} is still attached to a worktree",
            expected.branch
        )));
    }
    Ok(())
}

fn ensure_run_branch(branch: &str) -> Result<(), WorkerEnvError> {
    if branch.starts_with("run/")
        && !PROTECTED_BRANCHES
            .iter()
            .any(|protected| branch == *protected || branch.ends_with(&format!("/{protected}")))
    {
        return Ok(());
    }
    Err(open_failed(format!(
        "refusing worktree operation on non-run branch {branch}"
    )))
}

fn current_worktree_branch(worktree_root: &AbsPath) -> Result<String, WorkerEnvError> {
    git_utf8_line(
        worktree_root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        "reading leased worktree branch",
    )
}

fn branch_head(repo_root: &AbsPath, branch: &str) -> Result<String, WorkerEnvError> {
    git_utf8_line(
        repo_root,
        &["rev-parse", "--verify", branch],
        "reading run branch HEAD",
    )
}

fn git_utf8_line(workdir: &AbsPath, args: &[&str], action: &str) -> Result<String, WorkerEnvError> {
    let bytes = git_capture_bounded(workdir, args, action, 4096, &[0])?;
    let value = std::str::from_utf8(&bytes)
        .map_err(|error| open_failed(format!("{action} returned non-utf8 data: {error}")))?
        .trim();
    if value.is_empty() || value.lines().count() != 1 {
        return Err(open_failed(format!(
            "{action} returned an invalid single-line value"
        )));
    }
    Ok(value.to_string())
}

fn git_capture(workdir: &AbsPath, args: &[&str], action: &str) -> Result<Vec<u8>, WorkerEnvError> {
    let output = git_command(workdir)
        .args(args)
        .output()
        .map_err(|e| open_failed(format!("{action} failed to spawn git: {e}")))?;
    if !output.status.success() {
        return Err(open_failed(format!(
            "{action} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn git_capture_bounded(
    workdir: &AbsPath,
    args: &[&str],
    action: &str,
    max_stdout_bytes: usize,
    accepted_status_codes: &[i32],
) -> Result<Vec<u8>, WorkerEnvError> {
    if max_stdout_bytes == 0 {
        return Err(open_failed(format!(
            "{action} received a zero-byte output limit"
        )));
    }
    let mut child = git_command(workdir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| open_failed(format!("{action} failed to spawn git: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| open_failed(format!("{action} has no stdout pipe")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| open_failed(format!("{action} has no stderr pipe")))?;
    let stdout_reader = std::thread::spawn(move || drain_bounded(stdout, max_stdout_bytes));
    let stderr_reader = std::thread::spawn(move || drain_bounded(stderr, MAX_GIT_STDERR_BYTES));
    let status = child
        .wait()
        .map_err(|error| open_failed(format!("{action} failed while waiting for git: {error}")))?;
    let (stdout, stdout_overflow) = stdout_reader
        .join()
        .map_err(|_| open_failed(format!("{action} stdout reader panicked")))?
        .map_err(|error| open_failed(format!("{action} stdout read failed: {error}")))?;
    let (stderr, stderr_overflow) = stderr_reader
        .join()
        .map_err(|_| open_failed(format!("{action} stderr reader panicked")))?
        .map_err(|error| open_failed(format!("{action} stderr read failed: {error}")))?;
    if stdout_overflow {
        return Err(open_failed(format!(
            "{action} exceeded its configured {max_stdout_bytes}-byte output limit"
        )));
    }
    let status_code = status
        .code()
        .ok_or_else(|| open_failed(format!("{action} terminated without an exit status")))?;
    if !accepted_status_codes.contains(&status_code) {
        let suffix = if stderr_overflow {
            " [stderr truncated at package safety ceiling]"
        } else {
            ""
        };
        return Err(open_failed(format!(
            "{action} failed with exit {status_code}: {}{suffix}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(stdout)
}

fn drain_bounded(mut reader: impl Read, limit: usize) -> Result<(Vec<u8>, bool), std::io::Error> {
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut overflow = false;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        let keep = remaining.min(read);
        output.extend_from_slice(&buffer[..keep]);
        if keep < read {
            overflow = true;
        }
    }
    Ok((output, overflow))
}

fn materialize_worktree(
    repo_root: &AbsPath,
    worktree_root: &AbsPath,
    branch: &str,
) -> Result<(), WorkerEnvError> {
    if let Some(attached_path) = worktree_path_for_branch(repo_root, branch)? {
        validate_attached_worktree(worktree_root, branch, &attached_path)?;
        return Ok(());
    }

    if worktree_root.as_path().exists() {
        return Err(open_failed(format!(
            "worktree path {} exists without the expected branch {}",
            worktree_root.as_str(),
            branch
        )));
    }

    if !git_branch_exists(repo_root, branch)? {
        git_capture(
            repo_root,
            &["branch", branch, "HEAD"],
            "creating the leased run branch",
        )?;
    }

    if let Some(attached_path) = worktree_path_for_branch(repo_root, branch)? {
        return Err(open_failed(format!(
            "leased branch {} became attached at {} before {} could be materialized",
            branch,
            attached_path.display(),
            worktree_root.as_str()
        )));
    }

    git_capture(
        repo_root,
        &[
            "worktree",
            "add",
            "--no-checkout",
            worktree_root.as_str(),
            branch,
        ],
        &format!(
            "materializing worktree {} on branch {}",
            worktree_root.as_str(),
            branch
        ),
    )?;
    let attached_path = worktree_path_for_branch(repo_root, branch)?.ok_or_else(|| {
        open_failed(format!(
            "branch {} has no registered worktree after materializing {}",
            branch,
            worktree_root.as_str()
        ))
    })?;
    let materialization = (|| {
        validate_attached_worktree(worktree_root, branch, &attached_path)?;
        git_capture(
            worktree_root,
            &["read-tree", "HEAD"],
            "initializing the leased worktree index",
        )?;
        snapshot_worktree_from_live_checkout(repo_root, worktree_root)
    })();
    if let Err(error) = materialization {
        return Err(rollback_failed_materialization(
            repo_root,
            worktree_root,
            branch,
            error,
        ));
    }
    Ok(())
}

fn rollback_failed_materialization(
    repo_root: &AbsPath,
    worktree_root: &AbsPath,
    branch: &str,
    materialization_error: WorkerEnvError,
) -> WorkerEnvError {
    match git_capture(
        repo_root,
        &["worktree", "remove", "--force", worktree_root.as_str()],
        "rolling back an incomplete worktree materialization",
    ) {
        Ok(_) => materialization_error,
        Err(rollback_error) => open_failed(format!(
            "worktree {branch} materialization failed ({materialization_error}); rollback also failed ({rollback_error})"
        )),
    }
}

fn git_branch_exists(repo_root: &AbsPath, branch: &str) -> Result<bool, WorkerEnvError> {
    let reference = format!("refs/heads/{branch}");
    let output = git_command(repo_root)
        .args(["show-ref", "--verify", "--quiet", &reference])
        .output()
        .map_err(|error| open_failed(format!("checking branch {branch} failed: {error}")))?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    Err(open_failed(format!(
        "checking branch {} failed: {}",
        branch,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn worktree_path_for_branch(
    repo_root: &AbsPath,
    branch: &str,
) -> Result<Option<PathBuf>, WorkerEnvError> {
    let output = git_capture(
        repo_root,
        &["worktree", "list", "--porcelain", "-z"],
        "reading registered worktrees",
    )?;
    let expected = format!("branch refs/heads/{branch}");
    let mut current_path = None;
    let mut matches = Vec::new();
    for field in output.split(|byte| *byte == 0) {
        if field.is_empty() {
            current_path = None;
            continue;
        }
        let field = std::str::from_utf8(field)
            .map_err(|error| open_failed(format!("worktree registry is not utf-8: {error}")))?;
        if let Some(path) = field.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(path));
        } else if field == expected {
            matches.push(current_path.clone().ok_or_else(|| {
                open_failed(format!("worktree registry branch {branch} has no path"))
            })?);
        }
    }
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        count => Err(open_failed(format!(
            "branch {branch} is attached to {count} worktrees"
        ))),
    }
}

fn validate_attached_worktree(
    worktree_root: &AbsPath,
    branch: &str,
    attached_path: &Path,
) -> Result<(), WorkerEnvError> {
    let expected_path = std::fs::canonicalize(worktree_root.as_path()).map_err(|error| {
        open_failed(format!(
            "materialized worktree {} cannot be canonicalized: {error}",
            worktree_root.as_str()
        ))
    })?;
    let attached_path = std::fs::canonicalize(attached_path).map_err(|error| {
        open_failed(format!(
            "registered worktree {} for branch {} cannot be canonicalized: {error}",
            attached_path.display(),
            branch
        ))
    })?;
    if attached_path != expected_path {
        return Err(open_failed(format!(
            "branch {} is attached at {}, not {}",
            branch,
            attached_path.display(),
            expected_path.display()
        )));
    }
    let head = git_capture(
        worktree_root,
        &["symbolic-ref", "--quiet", "HEAD"],
        "reading leased worktree branch",
    )?;
    let head = std::str::from_utf8(&head)
        .map_err(|error| open_failed(format!("worktree HEAD is not utf-8: {error}")))?
        .trim();
    let expected_head = format!("refs/heads/{branch}");
    if head != expected_head {
        return Err(open_failed(format!(
            "worktree {} carries branch {}, not {}",
            worktree_root.as_str(),
            head,
            expected_head
        )));
    }
    Ok(())
}

fn git_command(workdir: &AbsPath) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_CONFIG_SYSTEM")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .arg("-C")
        .arg(workdir.as_path());
    command
}

fn snapshot_worktree_from_live_checkout(
    repo_root: &AbsPath,
    worktree_root: &AbsPath,
) -> Result<(), WorkerEnvError> {
    let mut tracked_paths = git_nul_paths(
        repo_root,
        &["ls-files", "--cached", "-z"],
        "collecting tracked snapshot paths",
    )?;
    let mut untracked_paths = git_nul_paths(
        repo_root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        "collecting untracked paths",
    )?;
    tracked_paths.sort();
    tracked_paths.dedup();
    untracked_paths.sort();
    untracked_paths.dedup();
    let clean_missing_paths = git_assumed_or_sparse_missing_paths(repo_root)?;

    let mut snapshot_paths = Vec::with_capacity(tracked_paths.len() + untracked_paths.len());
    snapshot_paths.extend(tracked_paths);
    snapshot_paths.extend(untracked_paths);
    snapshot_paths.sort();
    snapshot_paths.dedup();
    let mut allowed_paths = snapshot_paths.clone();
    allowed_paths.extend(clean_missing_paths.iter().cloned());
    allowed_paths.sort();
    allowed_paths.dedup();
    if clone_snapshot_roots(repo_root, worktree_root, &snapshot_paths)? {
        sanitize_cloned_snapshot(worktree_root, &allowed_paths)?;
        checkout_clean_missing_paths(worktree_root, &clean_missing_paths)?;
        return Ok(());
    }
    sync_snapshot_paths(repo_root, worktree_root, &snapshot_paths)?;
    checkout_clean_missing_paths(worktree_root, &clean_missing_paths)?;
    Ok(())
}

fn git_assumed_or_sparse_missing_paths(
    repo_root: &AbsPath,
) -> Result<Vec<PathBuf>, WorkerEnvError> {
    let stdout = git_capture(
        repo_root,
        &["ls-files", "-v", "-z"],
        "collecting sparse tracked snapshot paths",
    )?;
    let mut paths = Vec::new();
    for raw in stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        if raw.len() < 3 || raw[1] != b' ' {
            return Err(open_failed(
                "collecting sparse tracked snapshot paths returned malformed tagged output",
            ));
        }
        let tag = raw[0];
        if tag != b'S' && !tag.is_ascii_lowercase() {
            continue;
        }
        let rel = std::str::from_utf8(&raw[2..]).map_err(|error| {
            open_failed(format!(
                "collecting sparse tracked snapshot paths returned non-utf8 path: {error}"
            ))
        })?;
        let path = PathBuf::from(rel);
        validate_repo_relative_path(&path, "collecting sparse tracked snapshot paths")?;
        match std::fs::symlink_metadata(repo_root.as_path().join(&path)) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => paths.push(path),
            Err(error) => {
                return Err(open_failed(format!(
                    "reading sparse tracked snapshot path {} failed: {error}",
                    rel
                )));
            }
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn clone_snapshot_roots(
    repo_root: &AbsPath,
    worktree_root: &AbsPath,
    rel_paths: &[PathBuf],
) -> Result<bool, WorkerEnvError> {
    if !cfg!(target_os = "macos") {
        return Ok(false);
    }
    let roots = rel_paths
        .iter()
        .filter_map(|path| path.components().next())
        .filter_map(|component| match component {
            Component::Normal(name) => Some(PathBuf::from(name)),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut clonable_roots = Vec::new();
    let mut symlink_roots = Vec::new();
    for root in roots {
        let source = repo_root.as_path().join(&root);
        let target = worktree_root.as_path().join(&root);
        let metadata = match std::fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(open_failed(format!(
                    "reading snapshot root {} failed: {error}",
                    source.display()
                )));
            }
        };
        if metadata.file_type().is_symlink() {
            symlink_roots.push((source, target));
        } else {
            if worktree_root.as_path().starts_with(&source) {
                return Ok(false);
            }
            clonable_roots.push((source, target, metadata.file_type().is_dir()));
        }
    }
    if clonable_roots.is_empty() {
        for (source, target) in symlink_roots {
            sync_symlink(&source, &target)?;
        }
        return Ok(true);
    }
    clonable_roots.sort_by_key(|(_, _, is_directory)| !*is_directory);

    for (index, root) in clonable_roots.iter().enumerate() {
        if !clone_snapshot_root(root)? {
            if index == 0 {
                return Ok(false);
            }
            return Err(open_failed(format!(
                "copy-on-write support changed while cloning snapshot root {}",
                root.0.display()
            )));
        }
    }
    for (source, target) in symlink_roots {
        sync_symlink(&source, &target)?;
    }
    Ok(true)
}

fn clone_snapshot_root(root: &(PathBuf, PathBuf, bool)) -> Result<bool, WorkerEnvError> {
    let (source, target, is_directory) = root;
    prepare_snapshot_file_target(target)?;
    if *is_directory {
        try_clone_snapshot_tree(source, target)
    } else {
        try_clone_snapshot_file(source, target)
    }
    .map_err(|error| {
        open_failed(format!(
            "cloning live checkout snapshot root {} to {} failed: {error}",
            source.display(),
            target.display()
        ))
    })
}

fn sanitize_cloned_snapshot(
    worktree_root: &AbsPath,
    allowed_paths: &[PathBuf],
) -> Result<(), WorkerEnvError> {
    let allowed_paths = allowed_paths.iter().cloned().collect::<HashSet<_>>();
    let mut allowed_directories = HashSet::new();
    for path in &allowed_paths {
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if directory.as_os_str().is_empty() {
                break;
            }
            allowed_directories.insert(directory.to_path_buf());
            parent = directory.parent();
        }
    }
    sanitize_cloned_directory(
        worktree_root.as_path(),
        Path::new(""),
        &allowed_paths,
        &allowed_directories,
    )
}

fn sanitize_cloned_directory(
    worktree_root: &Path,
    relative_directory: &Path,
    allowed_paths: &HashSet<PathBuf>,
    allowed_directories: &HashSet<PathBuf>,
) -> Result<(), WorkerEnvError> {
    let directory = worktree_root.join(relative_directory);
    let entries = std::fs::read_dir(&directory).map_err(|error| {
        open_failed(format!(
            "reading cloned snapshot directory {} failed: {error}",
            directory.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            open_failed(format!(
                "reading cloned snapshot entry below {} failed: {error}",
                directory.display()
            ))
        })?;
        let relative_path = relative_directory.join(entry.file_name());
        if relative_path == Path::new(".git") {
            continue;
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            open_failed(format!(
                "reading cloned snapshot path {} failed: {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_dir() {
            if allowed_directories.contains(&relative_path) {
                sanitize_cloned_directory(
                    worktree_root,
                    &relative_path,
                    allowed_paths,
                    allowed_directories,
                )?;
            } else if allowed_paths.contains(&relative_path) {
                std::fs::remove_dir_all(&path).map_err(|error| {
                    open_failed(format!(
                        "emptying cloned snapshot directory {} failed: {error}",
                        path.display()
                    ))
                })?;
                std::fs::create_dir(&path).map_err(|error| {
                    open_failed(format!(
                        "recreating empty snapshot directory {} failed: {error}",
                        path.display()
                    ))
                })?;
            } else {
                std::fs::remove_dir_all(&path).map_err(|error| {
                    open_failed(format!(
                        "pruning unbound cloned snapshot directory {} failed: {error}",
                        path.display()
                    ))
                })?;
            }
        } else if !allowed_paths.contains(&relative_path) {
            std::fs::remove_file(&path).map_err(|error| {
                open_failed(format!(
                    "pruning unbound cloned snapshot file {} failed: {error}",
                    path.display()
                ))
            })?;
        }
    }
    Ok(())
}

fn sync_snapshot_paths(
    repo_root: &AbsPath,
    worktree_root: &AbsPath,
    rel_paths: &[PathBuf],
) -> Result<(), WorkerEnvError> {
    if rel_paths.is_empty() {
        return Ok(());
    }
    let worker_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .min(8)
        .min(rel_paths.len().max(1));
    let chunk_size = rel_paths.len().div_ceil(worker_count);
    std::thread::scope(|scope| {
        let handles = rel_paths
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut copier = SnapshotFileCopier::new();
                    for rel_path in chunk {
                        sync_live_checkout_path(repo_root, worktree_root, rel_path, &mut copier)?;
                    }
                    Ok(())
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle
                .join()
                .map_err(|_| open_failed("worktree snapshot worker panicked"))??;
        }
        Ok(())
    })
}

fn checkout_clean_missing_paths(
    worktree_root: &AbsPath,
    rel_paths: &[PathBuf],
) -> Result<(), WorkerEnvError> {
    if rel_paths.is_empty() {
        return Ok(());
    }
    let mut input = Vec::new();
    for rel_path in rel_paths {
        let path = rel_path.to_str().ok_or_else(|| {
            open_failed(format!(
                "clean tracked snapshot path is not utf-8: {}",
                rel_path.display()
            ))
        })?;
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    git_with_input(
        worktree_root,
        &["checkout-index", "--force", "-z", "--stdin"],
        &input,
        "materializing clean tracked paths absent from the live checkout",
    )
}

fn git_with_input(
    workdir: &AbsPath,
    args: &[&str],
    input: &[u8],
    action: &str,
) -> Result<(), WorkerEnvError> {
    let mut child = git_command(workdir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| open_failed(format!("{action} failed to spawn git: {error}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| open_failed(format!("{action} has no stdin pipe")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| open_failed(format!("{action} has no stderr pipe")))?;
    let input = input.to_vec();
    let input_writer = std::thread::spawn(move || stdin.write_all(&input));
    let stderr_reader = std::thread::spawn(move || drain_bounded(stderr, MAX_GIT_STDERR_BYTES));
    let status = child
        .wait()
        .map_err(|error| open_failed(format!("{action} failed while waiting for git: {error}")))?;
    input_writer
        .join()
        .map_err(|_| open_failed(format!("{action} stdin writer panicked")))?
        .map_err(|error| open_failed(format!("{action} stdin write failed: {error}")))?;
    let (stderr, stderr_overflow) = stderr_reader
        .join()
        .map_err(|_| open_failed(format!("{action} stderr reader panicked")))?
        .map_err(|error| open_failed(format!("{action} stderr read failed: {error}")))?;
    if !status.success() {
        let suffix = if stderr_overflow {
            " [stderr truncated at package safety ceiling]"
        } else {
            ""
        };
        return Err(open_failed(format!(
            "{action} failed: {}{suffix}",
            String::from_utf8_lossy(&stderr).trim()
        )));
    }
    Ok(())
}

fn git_nul_paths(
    repo_root: &AbsPath,
    args: &[&str],
    action: &str,
) -> Result<Vec<PathBuf>, WorkerEnvError> {
    let stdout = git_capture(repo_root, args, action)?;
    let mut out = Vec::new();
    for raw in stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let rel = std::str::from_utf8(raw)
            .map_err(|e| open_failed(format!("{action} returned non-utf8 path: {e}")))?;
        let path = PathBuf::from(rel);
        validate_repo_relative_path(&path, action)?;
        out.push(path);
    }
    Ok(out)
}

fn git_nul_paths_bounded(
    repo_root: &AbsPath,
    args: &[&str],
    action: &str,
    max_bytes: usize,
    max_paths: usize,
) -> Result<Vec<PathBuf>, WorkerEnvError> {
    let stdout = git_capture_bounded(repo_root, args, action, max_bytes, &[0])?;
    let mut out = Vec::new();
    for raw in stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        if out.len() == max_paths {
            return Err(open_failed(format!(
                "{action} exceeds descriptor limit of {max_paths} paths"
            )));
        }
        let rel = std::str::from_utf8(raw)
            .map_err(|error| open_failed(format!("{action} returned non-utf8 path: {error}")))?;
        let path = PathBuf::from(rel);
        validate_repo_relative_path(&path, action)?;
        out.push(path);
    }
    Ok(out)
}

fn validate_repo_relative_path(path: &Path, action: &str) -> Result<(), WorkerEnvError> {
    if path.as_os_str().is_empty() {
        return Err(open_failed(format!("{action} returned an empty path")));
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if part != OsStr::new(".git") => {}
            _ => {
                return Err(open_failed(format!(
                    "{action} returned path outside the repository file surface: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn sync_live_checkout_path(
    repo_root: &AbsPath,
    worktree_root: &AbsPath,
    rel_path: &Path,
    copier: &mut SnapshotFileCopier,
) -> Result<(), WorkerEnvError> {
    let source = repo_root.as_path().join(rel_path);
    let target = worktree_root.as_path().join(rel_path);

    if source.starts_with(worktree_root.as_path()) {
        return Ok(());
    }

    match std::fs::symlink_metadata(&source) {
        Ok(metadata) => {
            if metadata.file_type().is_dir() {
                std::fs::create_dir_all(&target).map_err(|e| {
                    open_failed(format!(
                        "creating dirty snapshot directory {} failed: {e}",
                        target.display()
                    ))
                })?;
            } else if metadata.file_type().is_symlink() {
                sync_symlink(&source, &target)?;
            } else {
                copier.copy(&source, &target)?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            remove_snapshot_target(&target)?;
            prune_empty_snapshot_dirs(worktree_root.as_path(), target.parent())?;
        }
        Err(e) => {
            return Err(open_failed(format!(
                "reading live checkout path {} failed: {e}",
                source.display()
            )));
        }
    }
    Ok(())
}

fn prune_empty_snapshot_dirs(root: &Path, start: Option<&Path>) -> Result<(), WorkerEnvError> {
    let Some(mut dir) = start else {
        return Ok(());
    };
    while dir.starts_with(root) && dir != root {
        match std::fs::read_dir(dir) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                dir = match dir.parent() {
                    Some(parent) => parent,
                    None => break,
                };
                continue;
            }
            Err(e) => {
                return Err(open_failed(format!(
                    "checking empty snapshot directory {} failed: {e}",
                    dir.display()
                )));
            }
        }
        std::fs::remove_dir(dir).map_err(|e| {
            open_failed(format!(
                "removing empty dirty snapshot directory {} failed: {e}",
                dir.display()
            ))
        })?;
        dir = match dir.parent() {
            Some(parent) => parent,
            None => break,
        };
    }
    Ok(())
}

struct SnapshotFileCopier {
    clone_supported: bool,
}

impl SnapshotFileCopier {
    fn new() -> Self {
        Self {
            clone_supported: cfg!(target_os = "macos"),
        }
    }

    fn copy(&mut self, source: &Path, target: &Path) -> Result<(), WorkerEnvError> {
        prepare_snapshot_file_target(target)?;
        if self.clone_supported {
            match try_clone_snapshot_file(source, target) {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    self.clone_supported = false;
                    tracing::warn!(
                        source = %source.display(),
                        target = %target.display(),
                        "copy-on-write snapshot cloning is unavailable; using byte copies"
                    );
                }
                Err(error) => {
                    return Err(open_failed(format!(
                        "cloning live checkout file {} to worktree {} failed: {error}",
                        source.display(),
                        target.display()
                    )));
                }
            }
        }
        std::fs::copy(source, target).map_err(|error| {
            open_failed(format!(
                "copying live checkout file {} to worktree {} failed: {error}",
                source.display(),
                target.display()
            ))
        })?;
        Ok(())
    }
}

fn prepare_snapshot_file_target(target: &Path) -> Result<(), WorkerEnvError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            open_failed(format!(
                "creating dirty snapshot parent {} failed: {e}",
                parent.display()
            ))
        })?;
    }
    remove_snapshot_target(target)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn try_clone_snapshot_file(source: &Path, target: &Path) -> Result<bool, std::io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "snapshot source path contains a NUL byte",
        )
    })?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "snapshot target path contains a NUL byte",
        )
    })?;
    if unsafe { libc::clonefile(source.as_ptr(), target.as_ptr(), 0) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ENOTSUP | libc::EXDEV) => Ok(false),
        _ => Err(error),
    }
}

#[cfg(not(target_os = "macos"))]
fn try_clone_snapshot_file(_source: &Path, _target: &Path) -> Result<bool, std::io::Error> {
    Ok(false)
}

#[cfg(target_os = "macos")]
fn try_clone_snapshot_tree(source: &Path, target: &Path) -> Result<bool, std::io::Error> {
    match try_clone_snapshot_file(source, target) {
        Err(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(false),
        result => result,
    }
}

#[cfg(not(target_os = "macos"))]
fn try_clone_snapshot_tree(_source: &Path, _target: &Path) -> Result<bool, std::io::Error> {
    Ok(false)
}

fn sync_symlink(source: &Path, target: &Path) -> Result<(), WorkerEnvError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            open_failed(format!(
                "creating dirty snapshot symlink parent {} failed: {e}",
                parent.display()
            ))
        })?;
    }
    remove_snapshot_target(target)?;
    let link_target = std::fs::read_link(source).map_err(|e| {
        open_failed(format!(
            "reading live checkout symlink {} failed: {e}",
            source.display()
        ))
    })?;
    create_snapshot_symlink(&link_target, target)
}

#[cfg(unix)]
fn create_snapshot_symlink(link_target: &Path, target: &Path) -> Result<(), WorkerEnvError> {
    std::os::unix::fs::symlink(link_target, target).map_err(|e| {
        open_failed(format!(
            "creating dirty snapshot symlink {} -> {} failed: {e}",
            target.display(),
            link_target.display()
        ))
    })
}

#[cfg(windows)]
fn create_snapshot_symlink(link_target: &Path, target: &Path) -> Result<(), WorkerEnvError> {
    std::os::windows::fs::symlink_file(link_target, target).map_err(|e| {
        open_failed(format!(
            "creating dirty snapshot symlink {} -> {} failed: {e}",
            target.display(),
            link_target.display()
        ))
    })
}

fn remove_snapshot_target(target: &Path) -> Result<(), WorkerEnvError> {
    match std::fs::symlink_metadata(target) {
        Ok(metadata) => {
            if metadata.file_type().is_dir() {
                std::fs::remove_dir_all(target).map_err(|e| {
                    open_failed(format!(
                        "removing worktree directory {} for dirty snapshot failed: {e}",
                        target.display()
                    ))
                })?;
            } else {
                std::fs::remove_file(target).map_err(|e| {
                    open_failed(format!(
                        "removing worktree file {} for dirty snapshot failed: {e}",
                        target.display()
                    ))
                })?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(open_failed(format!(
                "checking worktree snapshot target {} failed: {e}",
                target.display()
            )));
        }
    }
    Ok(())
}

fn open_failed(reason: impl Into<String>) -> WorkerEnvError {
    WorkerEnvError::OpenFailed {
        substrate_family: FAMILY_PROCESS_EXEC.to_string(),
        reason: reason.into(),
    }
}

#[cfg(test)]
#[path = "worktree_driver_tests.rs"]
mod tests;
