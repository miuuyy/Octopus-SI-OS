use std::io::{Read, Seek};
use std::path::{Component, Path};
use std::pin::Pin;
use std::sync::Arc;

use octopus_applypatch::Hunk;
use octopus_file_system::DirectFileSystem;
use octopus_util_pathuri::PathUri;
use octopus_worker_env::{
    AbsPath, EnvContext, FileSurface, FileWriteOutput, PatchOutcome, ProjectedSandbox,
    WekDescriptorHash, WorkerEnvCaps, WorkerEnvDriver, WorkerEnvError, WorkerEnvKindSpec,
    WorkerEnvironment, WorkerEnvironmentId, WorkerEnvironmentParts,
};

use crate::boundary::project_sandbox;

pub const FAMILY_PROCESS_EXEC: &str = "process_exec";
pub const WORKSPACE_ROOT_SLOT: &str = "workspace_root";

pub fn gated_file_surface(sandbox: Arc<ProjectedSandbox>, patch_root: AbsPath) -> Arc<dyn FileSurface> {
    Arc::new(GatedFs {
        sandbox,
        patch_root,
    })
}

pub fn direct_workspace_driver() -> Arc<dyn WorkerEnvDriver> {
    Arc::new(DirectWorkspaceDriver)
}

struct DirectWorkspaceDriver;

impl WorkerEnvDriver for DirectWorkspaceDriver {
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
            let workspace_root = ctx.resolve_root(WORKSPACE_ROOT_SLOT)?;
            ensure_real_directory(&workspace_root, "filesystem workspace root")?;
            let sandbox = project_sandbox(&spec, &caps, &ctx)?;
            sandbox.authorize_read(&workspace_root)?;
            sandbox.authorize_write(&workspace_root)?;
            let fs: Arc<dyn FileSurface> = Arc::new(GatedFs {
                sandbox: Arc::new(sandbox.clone()),
                patch_root: workspace_root.clone(),
            });
            WorkerEnvironment::assemble(
                &spec,
                descriptor_hash,
                id,
                WorkerEnvironmentParts {
                    sandbox,
                    cwd: workspace_root,
                    fs,
                    release: None,
                },
            )
        })
    }
}

fn ensure_real_directory(path: &AbsPath, name: &str) -> Result<(), WorkerEnvError> {
    let metadata =
        std::fs::symlink_metadata(path.as_path()).map_err(|error| WorkerEnvError::OpenFailed {
            substrate_family: FAMILY_PROCESS_EXEC.to_string(),
            reason: format!("cannot inspect {name} '{}': {error}", path.as_str()),
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(WorkerEnvError::OpenFailed {
            substrate_family: FAMILY_PROCESS_EXEC.to_string(),
            reason: format!("{name} '{}' is not a real directory", path.as_str()),
        });
    }
    let canonical =
        std::fs::canonicalize(path.as_path()).map_err(|error| WorkerEnvError::OpenFailed {
            substrate_family: FAMILY_PROCESS_EXEC.to_string(),
            reason: format!("cannot canonicalize {name} '{}': {error}", path.as_str()),
        })?;
    if canonical != path.as_path() {
        return Err(WorkerEnvError::OpenFailed {
            substrate_family: FAMILY_PROCESS_EXEC.to_string(),
            reason: format!(
                "{name} '{}' is not the canonical directory '{}'",
                path.as_str(),
                canonical.display()
            ),
        });
    }
    Ok(())
}

struct GatedFs {
    sandbox: Arc<ProjectedSandbox>,
    patch_root: AbsPath,
}

impl GatedFs {
    fn previous_content(&self, path: &AbsPath) -> Result<Option<Vec<u8>>, String> {
        const MAX_BYTES: u64 = 1024 * 1024;
        self.sandbox
            .authorize_read(path)
            .map_err(|e| e.to_string())?;
        authorize_resolved_sandbox_path(&self.sandbox, path, false, true)
            .map_err(|e| e.to_string())?;
        let file = match std::fs::File::open(path.as_path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err("previous content exceeds the regular-file capture limit".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("previous content exceeds the capture limit".into());
        }
        Ok(Some(bytes))
    }
}

impl FileSurface for GatedFs {
    fn read(
        &self,
        path: &AbsPath,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, WorkerEnvError>> + Send + '_>>
    {
        let res = self.sandbox.authorize_read(path);
        let resolved_target = authorize_resolved_sandbox_path(&self.sandbox, path, false, false);
        let path = path.clone();
        Box::pin(async move {
            res?;
            resolved_target?;
            std::fs::read(path.as_path())
                .map_err(|e| WorkerEnvError::ReadOutsideRoots(format!("{}: {e}", path.as_str())))
        })
    }

    fn read_range(
        &self,
        path: &AbsPath,
        offset: u64,
        max_bytes: usize,
    ) -> Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<octopus_worker_env::FileReadOutput, WorkerEnvError>,
                > + Send
                + '_,
        >,
    > {
        let res = self.sandbox.authorize_read(path);
        let resolved_target = authorize_resolved_sandbox_path(&self.sandbox, path, false, false);
        let path = path.clone();
        Box::pin(async move {
            res?;
            resolved_target?;
            let mut file = std::fs::File::open(path.as_path())
                .map_err(|e| WorkerEnvError::ReadOutsideRoots(format!("{}: {e}", path.as_str())))?;
            let file_size = file
                .metadata()
                .map_err(|e| WorkerEnvError::ReadOutsideRoots(format!("{}: {e}", path.as_str())))?
                .len();
            if offset > file_size {
                return Err(WorkerEnvError::ReadOutsideRoots(format!(
                    "{} offset {offset} exceeds file size {file_size}",
                    path.as_str()
                )));
            }
            file.seek(std::io::SeekFrom::Start(offset))
                .map_err(|e| WorkerEnvError::ReadOutsideRoots(format!("{}: {e}", path.as_str())))?;
            let limit = u64::try_from(max_bytes).map_err(|_| {
                WorkerEnvError::ReadOutsideRoots(format!(
                    "{} bounded read exceeds u64",
                    path.as_str()
                ))
            })?;
            let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
            file.take(limit)
                .read_to_end(&mut bytes)
                .map_err(|e| WorkerEnvError::ReadOutsideRoots(format!("{}: {e}", path.as_str())))?;
            Ok(octopus_worker_env::FileReadOutput {
                file_size,
                offset,
                bytes,
            })
        })
    }

    fn write(
        &self,
        path: &AbsPath,
        bytes: &[u8],
    ) -> Pin<
        Box<dyn std::future::Future<Output = Result<FileWriteOutput, WorkerEnvError>> + Send + '_>,
    > {
        let res = self.sandbox.authorize_write(path);
        let resolved_target = authorize_resolved_sandbox_path(&self.sandbox, path, true, true);
        let sandbox = self.sandbox.clone();
        let path = path.clone();
        let bytes = bytes.to_vec();
        Box::pin(async move {
            res?;
            resolved_target?;
            let previous_content = self.previous_content(&path);
            if let Some(parent) = path.as_path().parent() {
                let parent = AbsPath::new(parent)?;
                sandbox.authorize_write(&parent)?;
                std::fs::create_dir_all(parent.as_path()).map_err(|e| {
                    WorkerEnvError::WriteOutsideRoots(format!(
                        "creating parent dir {} failed: {e}",
                        parent.as_str()
                    ))
                })?;
            }
            std::fs::write(path.as_path(), &bytes).map_err(|e| {
                WorkerEnvError::WriteOutsideRoots(format!("{}: {e}", path.as_str()))
            })?;
            Ok(FileWriteOutput { previous_content })
        })
    }

    fn list(
        &self,
        dir: &AbsPath,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<AbsPath>, WorkerEnvError>> + Send + '_>>
    {
        let res = self.sandbox.authorize_read(dir);
        let resolved_target = authorize_resolved_sandbox_path(&self.sandbox, dir, false, false);
        let dir = dir.clone();
        Box::pin(async move {
            res?;
            resolved_target?;
            let mut out = Vec::new();
            let rd = std::fs::read_dir(dir.as_path())
                .map_err(|e| WorkerEnvError::ReadOutsideRoots(format!("{}: {e}", dir.as_str())))?;
            for entry in rd.flatten() {
                if let Ok(p) = AbsPath::new(&entry.path()) {
                    out.push(p);
                }
            }
            out.sort();
            Ok(out)
        })
    }

    fn apply_patch(
        &self,
        patch: &str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<PatchOutcome, WorkerEnvError>> + Send + '_>>
    {
        let sandbox = self.sandbox.clone();
        let anchor = self.patch_root.clone();
        let patch = patch.to_string();
        Box::pin(async move {
            let hunks = octopus_applypatch::parse_patch(&patch)
                .map_err(|e| WorkerEnvError::PatchFailed(e.to_string()))?
                .hunks;
            if hunks.is_empty() {
                return Err(WorkerEnvError::PatchFailed(
                    "patch names no files (failing closed)".into(),
                ));
            }

            for hunk in &hunks {
                match hunk {
                    Hunk::AddFile { path, .. } => {
                        let path = anchor_rel(&anchor, path)?;
                        sandbox.authorize_write(&path)?;
                        reject_symlinks_below(&anchor, &path, true)?;
                    }
                    Hunk::DeleteFile { path } => {
                        let path = anchor_rel(&anchor, path)?;
                        sandbox.authorize_write(&path)?;
                        reject_symlinks_below(&anchor, &path, false)?;
                    }
                    Hunk::UpdateFile {
                        path, move_path, ..
                    } => {
                        let path = anchor_rel(&anchor, path)?;
                        sandbox.authorize_write(&path)?;
                        reject_symlinks_below(&anchor, &path, false)?;
                        if let Some(move_path) = move_path {
                            let move_path = anchor_rel(&anchor, move_path)?;
                            sandbox.authorize_write(&move_path)?;
                            reject_symlinks_below(&anchor, &move_path, true)?;
                        }
                    }
                }
            }

            let cwd = PathUri::from_path(anchor.as_path())
                .map_err(|e| WorkerEnvError::PatchFailed(format!("anchor path uri: {e}")))?;
            let mut stdout: Vec<u8> = Vec::new();
            let mut stderr: Vec<u8> = Vec::new();
            let delta = octopus_applypatch::apply_patch_with_options(
                &patch,
                octopus_applypatch::ApplyPatchOptions {
                    update_file_mode:
                        octopus_applypatch::ApplyPatchFileUpdateMode::PreserveLineEndings,
                    follow_symlinks: false,
                },
                &cwd,
                &mut stdout,
                &mut stderr,
                &DirectFileSystem,
                None,
            )
            .await
            .map_err(|failure| {
                let (error, _delta) = failure.into_parts();
                WorkerEnvError::PatchFailed(format!(
                    "{error}: {}",
                    String::from_utf8_lossy(&stderr).trim()
                ))
            })?;

            let mut changed = Vec::with_capacity(delta.changes().len());
            for change in delta.changes() {
                let native_path = change
                    .path
                    .to_abs_path()
                    .map_err(|error| WorkerEnvError::PatchFailed(error.to_string()))?;
                let path = AbsPath::new(native_path.as_path())?;
                if !path.is_within(&anchor) {
                    return Err(WorkerEnvError::PatchFailed(format!(
                        "applied path '{}' is outside room root '{}'",
                        path.as_str(),
                        anchor.as_str()
                    )));
                }
                changed.push(path);
            }
            if changed.is_empty() {
                return Err(WorkerEnvError::PatchFailed(
                    "patch applied no changes (no resolvable file content) — failing closed".into(),
                ));
            }
            Ok(PatchOutcome {
                changed_paths: changed,
                is_exact: delta.is_exact(),
            })
        })
    }
}

fn authorize_resolved_sandbox_path(
    sandbox: &ProjectedSandbox,
    target: &AbsPath,
    write: bool,
    allow_missing: bool,
) -> Result<(), WorkerEnvError> {
    let resolved = resolve_existing_ancestor(target, allow_missing)?;
    if write {
        sandbox.authorize_write(&resolved)
    } else {
        sandbox.authorize_read(&resolved)
    }
}

fn resolve_existing_ancestor(
    target: &AbsPath,
    allow_missing: bool,
) -> Result<AbsPath, WorkerEnvError> {
    let mut existing = target.as_path().to_path_buf();
    let mut missing_suffix = Vec::new();
    loop {
        match std::fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && allow_missing => {
                let name = existing.file_name().ok_or_else(|| {
                    WorkerEnvError::WriteOutsideRoots(format!(
                        "path '{}' has no existing ancestor",
                        target.as_str()
                    ))
                })?;
                missing_suffix.push(name.to_os_string());
                existing.pop();
            }
            Err(error) => {
                return Err(if allow_missing {
                    WorkerEnvError::WriteOutsideRoots(format!(
                        "cannot inspect path '{}': {error}",
                        target.as_str()
                    ))
                } else {
                    WorkerEnvError::ReadOutsideRoots(format!(
                        "cannot inspect path '{}': {error}",
                        target.as_str()
                    ))
                });
            }
        }
    }
    let mut resolved = std::fs::canonicalize(&existing).map_err(|error| {
        if allow_missing {
            WorkerEnvError::WriteOutsideRoots(format!(
                "cannot resolve path '{}': {error}",
                target.as_str()
            ))
        } else {
            WorkerEnvError::ReadOutsideRoots(format!(
                "cannot resolve path '{}': {error}",
                target.as_str()
            ))
        }
    })?;
    for component in missing_suffix.iter().rev() {
        resolved.push(component);
    }
    AbsPath::new(&resolved)
}

fn reject_symlinks_below(
    root: &AbsPath,
    target: &AbsPath,
    allow_missing: bool,
) -> Result<(), WorkerEnvError> {
    if !target.is_within(root) {
        return Err(WorkerEnvError::WriteOutsideRoots(format!(
            "path '{}' is outside room root '{}'",
            target.as_str(),
            root.as_str()
        )));
    }
    let relative = target.as_path().strip_prefix(root.as_path()).map_err(|_| {
        WorkerEnvError::WriteOutsideRoots(format!(
            "path '{}' is outside room root '{}'",
            target.as_str(),
            root.as_str()
        ))
    })?;
    let mut current = root.as_path().to_path_buf();
    let mut missing = false;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(WorkerEnvError::WriteOutsideRoots(format!(
                "path '{}' has a non-normal component",
                target.as_str()
            )));
        };
        current.push(component);
        if missing {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(WorkerEnvError::WriteOutsideRoots(format!(
                    "path '{}' traverses symbolic link '{}'",
                    target.as_str(),
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && allow_missing => {
                missing = true;
            }
            Err(error) => {
                return Err(WorkerEnvError::WriteOutsideRoots(format!(
                    "cannot inspect path component '{}': {error}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

fn anchor_rel(anchor: &AbsPath, rel: &Path) -> Result<AbsPath, WorkerEnvError> {
    if rel.as_os_str().is_empty() {
        return Err(WorkerEnvError::PatchFailed(
            "patch path must be a non-empty relative path".into(),
        ));
    }
    if rel.is_absolute() {
        return AbsPath::new(rel);
    }
    AbsPath::new(&anchor.as_path().join(rel))
}

#[cfg(test)]
#[path = "workspace_driver_tests.rs"]
mod tests;
