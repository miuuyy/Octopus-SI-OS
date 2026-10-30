mod file_update;
mod invocation;
mod parser;
mod seek_sequence;
mod standalone_executable;
mod streaming_parser;
mod text_file;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use octopus_file_system::CreateDirectoryOptions;
use octopus_file_system::ExecutorFileSystem;
use octopus_file_system::FileSystemSandboxContext;
use octopus_file_system::GetMetadataOptions;
use octopus_file_system::ReadFileOptions;
use octopus_file_system::RemoveOptions;
use octopus_file_system::WriteFileOptions;
use octopus_util_pathuri::PathUri;
use octopus_util_pathuri::PathUriParseError;
pub use parser::Hunk;
pub use parser::ParseError;
use parser::ParseError::*;
pub use parser::UpdateFileChunk;
pub use parser::parse_patch;
pub use streaming_parser::StreamingPatchParser;
use thiserror::Error;

use file_update::AppliedPatch;
pub use file_update::ApplyPatchFileUpdate;
use file_update::derive_new_contents_from_chunks;
pub use file_update::unified_diff_from_chunks;
pub use file_update::unified_diff_from_chunks_with_context;
pub(crate) use file_update::unified_diff_from_chunks_with_mode;
pub use invocation::MaybeApplyPatch;
pub use invocation::maybe_parse_apply_patch;
pub use invocation::maybe_parse_apply_patch_verified;
pub use invocation::maybe_parse_apply_patch_verified_with_mode;
pub use invocation::verify_apply_patch_args;
pub use invocation::verify_apply_patch_args_with_mode;
pub use standalone_executable::main;

use crate::invocation::ExtractHeredocError;

pub const CODEX_CORE_APPLY_PATCH_ARG1: &str = "--codex-run-as-apply-patch";

pub const CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS_ENV_VAR: &str =
    "CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ApplyPatchFileUpdateMode {
    #[default]
    NormalizeToLf,
    PreserveLineEndings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplyPatchOptions {
    pub update_file_mode: ApplyPatchFileUpdateMode,
    pub follow_symlinks: bool,
}

impl Default for ApplyPatchOptions {
    fn default() -> Self {
        Self {
            update_file_mode: ApplyPatchFileUpdateMode::default(),
            follow_symlinks: true,
        }
    }
}

#[doc(hidden)]
pub fn apply_patch_file_update_mode_from_env() -> ApplyPatchFileUpdateMode {
    match std::env::var(CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS_ENV_VAR).as_deref() {
        Ok("1") => ApplyPatchFileUpdateMode::PreserveLineEndings,
        _ => ApplyPatchFileUpdateMode::NormalizeToLf,
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum ApplyPatchError {
    #[error(transparent)]
    ParseError(#[from] ParseError),
    #[error(transparent)]
    IoError(#[from] IoError),
    #[error("{0}")]
    ComputeReplacements(String),
    #[error(transparent)]
    PathUri(#[from] PathUriParseError),
    #[error(
        "patch detected without explicit call to apply_patch. Rerun as [\"apply_patch\", \"<patch>\"]"
    )]
    ImplicitInvocation,
}

impl From<std::io::Error> for ApplyPatchError {
    fn from(err: std::io::Error) -> Self {
        ApplyPatchError::IoError(IoError {
            context: "I/O error".to_string(),
            source: err,
        })
    }
}

impl From<&std::io::Error> for ApplyPatchError {
    fn from(err: &std::io::Error) -> Self {
        ApplyPatchError::IoError(IoError {
            context: "I/O error".to_string(),
            source: std::io::Error::new(err.kind(), err.to_string()),
        })
    }
}

#[derive(Debug, Error)]
#[error("{context}: {source}")]
pub struct IoError {
    context: String,
    #[source]
    source: std::io::Error,
}

impl PartialEq for IoError {
    fn eq(&self, other: &Self) -> bool {
        self.context == other.context && self.source.to_string() == other.source.to_string()
    }
}

#[derive(Debug, PartialEq)]
pub struct ApplyPatchArgs {
    pub patch: String,
    pub hunks: Vec<Hunk>,
    pub workdir: Option<String>,
    pub environment_id: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum ApplyPatchFileChange {
    Add {
        content: String,
    },
    Delete {
        content: String,
    },
    Update {
        unified_diff: String,
        move_path: Option<PathUri>,
        new_content: String,
    },
}

#[derive(Debug, PartialEq)]
pub enum MaybeApplyPatchVerified {
    Body(ApplyPatchAction),
    ShellParseError(ExtractHeredocError),
    CorrectnessError(ApplyPatchError),
    NotApplyPatch,
}

#[derive(Debug, PartialEq)]
pub struct ApplyPatchAction {
    changes: HashMap<PathUri, ApplyPatchFileChange>,

    update_file_mode: ApplyPatchFileUpdateMode,

    pub patch: String,

    pub cwd: PathUri,
}

impl ApplyPatchAction {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn changes(&self) -> &HashMap<PathUri, ApplyPatchFileChange> {
        &self.changes
    }

    pub fn update_file_mode(&self) -> ApplyPatchFileUpdateMode {
        self.update_file_mode
    }

    pub fn new_add_for_test(path: &PathUri, content: String) -> Self {
        #[expect(clippy::expect_used)]
        let filename = path.basename().expect("path should not be empty");
        let patch = format!(
            r#"*** Begin Patch
*** Update File: {filename}
@@
+ {content}
*** End Patch"#,
        );
        let changes = HashMap::from([(path.clone(), ApplyPatchFileChange::Add { content })]);
        #[expect(clippy::expect_used)]
        Self {
            changes,
            update_file_mode: ApplyPatchFileUpdateMode::default(),
            cwd: path.parent().expect("path should have parent"),
            patch,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppliedPatchDelta {
    changes: Vec<AppliedPatchChange>,
    exact: bool,
}

impl AppliedPatchDelta {
    fn new(changes: Vec<AppliedPatchChange>, exact: bool) -> Self {
        Self { changes, exact }
    }

    fn empty() -> Self {
        Self::new(Vec::new(), true)
    }

    pub fn changes(&self) -> &[AppliedPatchChange] {
        &self.changes
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn is_exact(&self) -> bool {
        self.exact
    }

    pub fn append(&mut self, other: Self) {
        self.changes.extend(other.changes);
        self.exact &= other.exact;
    }
}

impl Default for AppliedPatchDelta {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppliedPatchChange {
    pub path: PathUri,
    pub change: AppliedPatchFileChange,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AppliedPatchFileChange {
    Add {
        content: String,
        overwritten_content: Option<String>,
    },
    Delete {
        content: String,
    },
    Update {
        move_path: Option<PathUri>,
        old_content: String,
        overwritten_move_content: Option<String>,
        new_content: String,
    },
}

#[derive(Debug, Error)]
#[error("{error}")]
pub struct ApplyPatchFailure {
    #[source]
    error: ApplyPatchError,
    delta: AppliedPatchDelta,
}

impl ApplyPatchFailure {
    fn new(error: ApplyPatchError, delta: AppliedPatchDelta) -> Self {
        Self { error, delta }
    }

    fn without_delta(error: ApplyPatchError) -> Self {
        Self::new(error, AppliedPatchDelta::empty())
    }

    pub fn delta(&self) -> &AppliedPatchDelta {
        &self.delta
    }

    pub fn into_parts(self) -> (ApplyPatchError, AppliedPatchDelta) {
        (self.error, self.delta)
    }
}

pub async fn apply_patch(
    patch: &str,
    cwd: &PathUri,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<AppliedPatchDelta, ApplyPatchFailure> {
    apply_patch_with_options(
        patch,
        ApplyPatchOptions::default(),
        cwd,
        stdout,
        stderr,
        fs,
        sandbox,
    )
    .await
}

pub async fn apply_patch_with_options(
    patch: &str,
    options: ApplyPatchOptions,
    cwd: &PathUri,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<AppliedPatchDelta, ApplyPatchFailure> {
    let hunks = match parse_patch(patch) {
        Ok(source) => source.hunks,
        Err(e) => {
            match &e {
                InvalidPatchError(message) => {
                    writeln!(stderr, "Invalid patch: {message}")
                        .map_err(ApplyPatchError::from)
                        .map_err(ApplyPatchFailure::without_delta)?;
                }
                InvalidHunkError {
                    message,
                    line_number,
                } => {
                    writeln!(
                        stderr,
                        "Invalid patch hunk on line {line_number}: {message}"
                    )
                    .map_err(ApplyPatchError::from)
                    .map_err(ApplyPatchFailure::without_delta)?;
                }
            }
            return Err(ApplyPatchFailure::without_delta(
                ApplyPatchError::ParseError(e),
            ));
        }
    };

    apply_hunks_with_options(&hunks, options, cwd, stdout, stderr, fs, sandbox).await
}

pub async fn apply_hunks(
    hunks: &[Hunk],
    cwd: &PathUri,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<AppliedPatchDelta, ApplyPatchFailure> {
    apply_hunks_with_options(
        hunks,
        ApplyPatchOptions::default(),
        cwd,
        stdout,
        stderr,
        fs,
        sandbox,
    )
    .await
}

async fn apply_hunks_with_options(
    hunks: &[Hunk],
    options: ApplyPatchOptions,
    cwd: &PathUri,
    stdout: &mut impl std::io::Write,
    stderr: &mut impl std::io::Write,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
) -> Result<AppliedPatchDelta, ApplyPatchFailure> {
    let mut delta = AppliedPatchDelta::empty();
    match apply_hunks_to_files(hunks, options, cwd, fs, sandbox, &mut delta).await {
        Ok(affected_paths) => {
            print_summary(&affected_paths, stdout).map_err(|error| {
                ApplyPatchFailure::new(ApplyPatchError::from(error), delta.clone())
            })?;
            Ok(delta)
        }
        Err(error) => {
            let msg = error.to_string();
            writeln!(stderr, "{msg}").map_err(|error| {
                ApplyPatchFailure::new(ApplyPatchError::from(error), delta.clone())
            })?;
            let error = if let Some(io) = error.downcast_ref::<std::io::Error>() {
                ApplyPatchError::from(io)
            } else {
                ApplyPatchError::IoError(IoError {
                    context: msg,
                    source: std::io::Error::other(error),
                })
            };
            Err(ApplyPatchFailure::new(error, delta))
        }
    }
}

pub struct AffectedPaths {
    pub added: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

async fn apply_hunks_to_files(
    hunks: &[Hunk],
    options: ApplyPatchOptions,
    cwd: &PathUri,
    fs: &dyn ExecutorFileSystem,
    sandbox: Option<&FileSystemSandboxContext>,
    delta: &mut AppliedPatchDelta,
) -> anyhow::Result<AffectedPaths> {
    let ApplyPatchOptions {
        update_file_mode,
        follow_symlinks,
    } = options;
    if hunks.is_empty() {
        anyhow::bail!("No files were modified.");
    }

    let mut added: Vec<PathBuf> = Vec::new();
    let mut modified: Vec<PathBuf> = Vec::new();
    let mut deleted: Vec<PathBuf> = Vec::new();
    macro_rules! try_write {
        ($result:expr) => {
            match $result {
                Ok(value) => value,
                Err(error) => {
                    delta.exact = false;
                    return Err(anyhow::Error::from(error));
                }
            }
        };
    }

    for hunk in hunks {
        let affected_path = hunk.path().to_path_buf();
        let path_uri = hunk.resolve_path(cwd)?;
        match hunk {
            Hunk::AddFile { contents, .. } => {
                let overwritten_content = read_optional_file_text_for_delta(
                    &path_uri,
                    fs,
                    follow_symlinks,
                    sandbox,
                    &mut delta.exact,
                )
                .await;
                try_write!(
                    write_file_with_missing_parent_retry(
                        fs,
                        &path_uri,
                        contents.clone().into_bytes(),
                        follow_symlinks,
                        sandbox,
                    )
                    .await
                );
                delta.changes.push(AppliedPatchChange {
                    path: path_uri,
                    change: AppliedPatchFileChange::Add {
                        content: contents.clone(),
                        overwritten_content,
                    },
                });
                added.push(affected_path);
            }
            Hunk::DeleteFile { .. } => {
                note_existing_path_delta_support(
                    &path_uri,
                    fs,
                    follow_symlinks,
                    sandbox,
                    &mut delta.exact,
                )
                .await;
                let deleted_content = fs
                    .read_file_text_with_options(
                        &path_uri,
                        ReadFileOptions { follow_symlinks },
                        sandbox,
                    )
                    .await
                    .ok();
                if deleted_content.is_none() {
                    delta.exact = false;
                }
                ensure_not_directory(&path_uri, fs, follow_symlinks, sandbox)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to delete file {}",
                            path_uri.inferred_native_path_string()
                        )
                    })?;
                if let Err(error) = fs
                    .remove(
                        &path_uri,
                        RemoveOptions {
                            recursive: false,
                            force: false,
                            follow_symlinks,
                        },
                        sandbox,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to delete file {}",
                            path_uri.inferred_native_path_string()
                        )
                    })
                {
                    delta.exact &= remove_failure_was_side_effect_free(
                        &path_uri,
                        deleted_content.as_deref(),
                        fs,
                        follow_symlinks,
                        sandbox,
                    )
                    .await;
                    return Err(error);
                }
                if let Some(content) = deleted_content {
                    delta.changes.push(AppliedPatchChange {
                        path: path_uri,
                        change: AppliedPatchFileChange::Delete { content },
                    });
                }
                deleted.push(affected_path);
            }
            Hunk::UpdateFile {
                move_path, chunks, ..
            } => {
                note_existing_path_delta_support(
                    &path_uri,
                    fs,
                    follow_symlinks,
                    sandbox,
                    &mut delta.exact,
                )
                .await;
                let AppliedPatch {
                    original_contents,
                    new_contents,
                } = derive_new_contents_from_chunks(
                    &path_uri,
                    chunks,
                    update_file_mode,
                    fs,
                    follow_symlinks,
                    sandbox,
                )
                .await?;
                if let Some(dest) = move_path {
                    let dest_uri = cwd.join(&dest.to_string_lossy())?;
                    let overwritten_move_content = read_optional_file_text_for_delta(
                        &dest_uri,
                        fs,
                        follow_symlinks,
                        sandbox,
                        &mut delta.exact,
                    )
                    .await;
                    try_write!(
                        write_file_with_missing_parent_retry(
                            fs,
                            &dest_uri,
                            new_contents.clone().into_bytes(),
                            follow_symlinks,
                            sandbox,
                        )
                        .await
                    );
                    let dest_write_change_index = delta.changes.len();
                    delta.changes.push(AppliedPatchChange {
                        path: dest_uri.clone(),
                        change: AppliedPatchFileChange::Add {
                            content: new_contents.clone(),
                            overwritten_content: overwritten_move_content.clone(),
                        },
                    });
                    ensure_not_directory(&path_uri, fs, follow_symlinks, sandbox)
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to remove original {}",
                                path_uri.inferred_native_path_string()
                            )
                        })?;
                    if let Err(error) = fs
                        .remove(
                            &path_uri,
                            RemoveOptions {
                                recursive: false,
                                force: false,
                                follow_symlinks,
                            },
                            sandbox,
                        )
                        .await
                        .with_context(|| {
                            format!(
                                "Failed to remove original {}",
                                path_uri.inferred_native_path_string()
                            )
                        })
                    {
                        delta.exact &= remove_failure_was_side_effect_free(
                            &path_uri,
                            Some(&original_contents),
                            fs,
                            follow_symlinks,
                            sandbox,
                        )
                        .await;
                        return Err(error);
                    }
                    delta.changes[dest_write_change_index] = AppliedPatchChange {
                        path: path_uri,
                        change: AppliedPatchFileChange::Update {
                            move_path: Some(dest_uri),
                            old_content: original_contents,
                            overwritten_move_content,
                            new_content: new_contents,
                        },
                    };
                    modified.push(affected_path);
                } else {
                    try_write!(
                        fs.write_file_with_options(
                            &path_uri,
                            new_contents.clone().into_bytes(),
                            WriteFileOptions { follow_symlinks },
                            sandbox,
                        )
                        .await
                        .with_context(|| format!(
                            "Failed to write file {}",
                            path_uri.inferred_native_path_string()
                        ))
                    );
                    delta.changes.push(AppliedPatchChange {
                        path: path_uri,
                        change: AppliedPatchFileChange::Update {
                            move_path: None,
                            old_content: original_contents,
                            overwritten_move_content: None,
                            new_content: new_contents,
                        },
                    });
                    modified.push(affected_path);
                }
            }
        }
    }
    Ok(AffectedPaths {
        added,
        modified,
        deleted,
    })
}

async fn ensure_not_directory(
    path: &PathUri,
    fs: &dyn ExecutorFileSystem,
    follow_symlinks: bool,
    sandbox: Option<&FileSystemSandboxContext>,
) -> io::Result<()> {
    let metadata = fs
        .get_metadata_with_options(path, GetMetadataOptions { follow_symlinks }, sandbox)
        .await?;
    if metadata.is_directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is a directory",
        ));
    }
    Ok(())
}

async fn remove_failure_was_side_effect_free(
    path: &PathUri,
    expected_content: Option<&str>,
    fs: &dyn ExecutorFileSystem,
    follow_symlinks: bool,
    sandbox: Option<&FileSystemSandboxContext>,
) -> bool {
    match expected_content {
        Some(expected_content) => fs
            .read_file_text_with_options(path, ReadFileOptions { follow_symlinks }, sandbox)
            .await
            .is_ok_and(|content| content == expected_content),
        None => false,
    }
}

async fn read_optional_file_text_for_delta(
    path: &PathUri,
    fs: &dyn ExecutorFileSystem,
    follow_symlinks: bool,
    sandbox: Option<&FileSystemSandboxContext>,
    exact: &mut bool,
) -> Option<String> {
    note_existing_path_delta_support(path, fs, follow_symlinks, sandbox, exact).await;
    match fs
        .read_file_text_with_options(path, ReadFileOptions { follow_symlinks }, sandbox)
        .await
    {
        Ok(content) => Some(content),
        Err(source) if source.kind() == io::ErrorKind::NotFound => None,
        Err(_) => {
            *exact = false;
            None
        }
    }
}

async fn note_existing_path_delta_support(
    path: &PathUri,
    fs: &dyn ExecutorFileSystem,
    follow_symlinks: bool,
    sandbox: Option<&FileSystemSandboxContext>,
    exact: &mut bool,
) {
    match fs
        .get_metadata_with_options(path, GetMetadataOptions { follow_symlinks }, sandbox)
        .await
    {
        Ok(metadata) if metadata.is_file && !metadata.is_symlink => {}
        Ok(_) => *exact = false,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(_) => *exact = false,
    }
}

async fn write_file_with_missing_parent_retry(
    fs: &dyn ExecutorFileSystem,
    path: &PathUri,
    contents: Vec<u8>,
    follow_symlinks: bool,
    sandbox: Option<&FileSystemSandboxContext>,
) -> anyhow::Result<()> {
    match fs
        .write_file_with_options(
            path,
            contents.clone(),
            WriteFileOptions { follow_symlinks },
            sandbox,
        )
        .await
    {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs.create_directory(
                    &parent,
                    CreateDirectoryOptions {
                        recursive: true,
                        follow_symlinks,
                    },
                    sandbox,
                )
                .await
                .with_context(|| {
                    format!(
                        "Failed to create parent directories for {}",
                        path.inferred_native_path_string()
                    )
                })?;
            }
            fs.write_file_with_options(
                path,
                contents,
                WriteFileOptions { follow_symlinks },
                sandbox,
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to write file {}",
                    path.inferred_native_path_string()
                )
            })?;
            Ok(())
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "Failed to write file {}",
                path.inferred_native_path_string()
            )
        }),
    }
}

pub fn print_summary(
    affected: &AffectedPaths,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    writeln!(out, "Success. Updated the following files:")?;
    for path in &affected.added {
        writeln!(out, "A {}", path.display())?;
    }
    for path in &affected.modified {
        writeln!(out, "M {}", path.display())?;
    }
    for path in &affected.deleted {
        writeln!(out, "D {}", path.display())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_file_system::LOCAL_FS;
    use pretty_assertions::assert_eq;
    use std::fs;
    use tempfile::tempdir;

    fn wrap_patch(body: &str) -> String {
        format!("*** Begin Patch\n{body}\n*** End Patch")
    }

    #[tokio::test]
    async fn test_add_file_hunk_creates_file_with_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("add.txt");
        let patch = wrap_patch(&format!(
            r#"*** Add File: {}
+ab
+cd"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nA {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(contents, "ab\ncd\n");
    }

    #[tokio::test]
    async fn test_apply_patch_hunks_accept_relative_and_absolute_paths() {
        let dir = tempdir().unwrap();
        let cwd = PathUri::from_host_native_path(dir.path()).expect("absolute test path");
        let relative_add = dir.path().join("relative-add.txt");
        let absolute_add = dir.path().join("absolute-add.txt");
        let relative_delete = dir.path().join("relative-delete.txt");
        let absolute_delete = dir.path().join("absolute-delete.txt");
        let relative_update = dir.path().join("relative-update.txt");
        let absolute_update = dir.path().join("absolute-update.txt");
        fs::write(&relative_delete, "delete relative\n").unwrap();
        fs::write(&absolute_delete, "delete absolute\n").unwrap();
        fs::write(&relative_update, "relative old\n").unwrap();
        fs::write(&absolute_update, "absolute old\n").unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Add File: relative-add.txt
+relative add
*** Add File: {}
+absolute add
*** Delete File: relative-delete.txt
*** Delete File: {}
*** Update File: relative-update.txt
@@
-relative old
+relative new
*** Update File: {}
@@
-absolute old
+absolute new"#,
            absolute_add.display(),
            absolute_delete.display(),
            absolute_update.display(),
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        apply_patch(
            &patch,
            &cwd,
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(fs::read_to_string(&relative_add).unwrap(), "relative add\n");
        assert_eq!(fs::read_to_string(&absolute_add).unwrap(), "absolute add\n");
        assert!(!relative_delete.exists());
        assert!(!absolute_delete.exists());
        assert_eq!(
            fs::read_to_string(&relative_update).unwrap(),
            "relative new\n"
        );
        assert_eq!(
            fs::read_to_string(&absolute_update).unwrap(),
            "absolute new\n"
        );
        assert_eq!(String::from_utf8(stderr).unwrap(), "");
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!(
                "Success. Updated the following files:\nA relative-add.txt\nA {}\nM relative-update.txt\nM {}\nD relative-delete.txt\nD {}\n",
                absolute_add.display(),
                absolute_update.display(),
                absolute_delete.display(),
            )
        );
    }

    #[tokio::test]
    async fn test_delete_file_hunk_removes_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("del.txt");
        fs::write(&path, "x").unwrap();
        let patch = wrap_patch(&format!("*** Delete File: {}", path.display()));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nD {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn test_update_file_hunk_modifies_content() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("update.txt");
        fs::write(&path, "foo\nbar\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
-bar
+baz"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "foo\nbaz\n");
    }

    #[tokio::test]
    async fn test_update_file_hunk_can_move_file() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dest = dir.path().join("dst.txt");
        fs::write(&src, "line\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
*** Move to: {}
@@
-line
+line2"#,
            src.display(),
            dest.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            dest.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        assert!(!src.exists());
        let contents = fs::read_to_string(&dest).unwrap();
        assert_eq!(contents, "line2\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_failed_move_returns_committed_destination_delta() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let source_dir = dir.path().join("locked");
        let dest_dir = dir.path().join("out");
        fs::create_dir(&source_dir).unwrap();
        fs::create_dir(&dest_dir).unwrap();
        let src = source_dir.join("src.txt");
        let dest = dest_dir.join("dst.txt");
        fs::write(&src, "line\n").unwrap();
        fs::set_permissions(&source_dir, fs::Permissions::from_mode(0o555)).unwrap();

        let patch = wrap_patch(
            "*** Update File: locked/src.txt\n*** Move to: out/dst.txt\n@@\n-line\n+line2",
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let failure = apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .expect_err("source removal should fail after destination write");

        fs::set_permissions(&source_dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            String::from_utf8(stderr)
                .unwrap()
                .contains(&format!("Failed to remove original {}", src.display()))
        );
        assert_eq!(
            failure.delta(),
            &AppliedPatchDelta::new(
                vec![AppliedPatchChange {
                    path: PathUri::from_host_native_path(&dest).expect("absolute destination path"),
                    change: AppliedPatchFileChange::Add {
                        content: "line2\n".to_string(),
                        overwritten_content: None,
                    },
                }],
                true,
            )
        );
        assert_eq!(fs::read_to_string(src).unwrap(), "line\n");
        assert_eq!(fs::read_to_string(dest).unwrap(), "line2\n");
    }

    #[tokio::test]
    async fn test_multiple_update_chunks_apply_to_single_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        fs::write(&path, "foo\nbar\nbaz\nqux\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 foo
-bar
+BAR
@@
 baz
-qux
+QUX"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();
        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "foo\nBAR\nbaz\nQUX\n");
    }

    #[tokio::test]
    async fn test_update_file_hunk_interleaved_changes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("interleaved.txt");

        fs::write(&path, "a\nb\nc\nd\ne\nf\n").unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
 a
-b
+B
@@
 c
 d
-e
+E
@@
 f
+g
*** End of File"#,
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();

        let stdout_str = String::from_utf8(stdout).unwrap();
        let stderr_str = String::from_utf8(stderr).unwrap();

        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);
        assert_eq!(stderr_str, "");

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "a\nB\nc\nd\nE\nf\ng\n");
    }

    #[tokio::test]
    async fn test_pure_addition_chunk_followed_by_removal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("panic.txt");
        fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
+after-context
+second-line
@@
 line1
-line2
-line3
+line2-replacement"#,
            path.display()
        ));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(
            contents,
            "line1\nline2-replacement\nafter-context\nsecond-line\n"
        );
    }

    #[tokio::test]
    async fn test_update_line_with_unicode_dash() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("unicode.py");

        let original = "import asyncio  # local import \u{2013} avoids top\u{2011}level dep\n";
        std::fs::write(&path, original).unwrap();

        let patch = wrap_patch(&format!(
            r#"*** Update File: {}
@@
-import asyncio  # local import - avoids top-level dep
+import asyncio  # HELLO"#,
            path.display()
        ));

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();

        let expected = "import asyncio  # HELLO\n";
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, expected);

        let stdout_str = String::from_utf8(stdout).unwrap();
        let expected_out = format!(
            "Success. Updated the following files:\nM {}\n",
            path.display()
        );
        assert_eq!(stdout_str, expected_out);

        assert_eq!(String::from_utf8(stderr).unwrap(), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_apply_patch_fails_on_write_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let locked_dir = dir.path().join("locked");
        fs::create_dir(&locked_dir).unwrap();
        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o555)).unwrap();

        let patch = wrap_patch("*** Add File: locked/new.txt\n+after");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await;
        let failure = result.expect_err("write should fail");

        fs::set_permissions(&locked_dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(!failure.delta().is_exact());
    }

    #[tokio::test]
    async fn test_unreadable_destinations_return_inexact_delta() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("binary.dat");
        fs::write(dir.path().join("source.txt"), "before\n").unwrap();
        let cwd = PathUri::from_host_native_path(dir.path()).expect("absolute test path");

        for patch in [
            wrap_patch("*** Add File: binary.dat\n+text"),
            wrap_patch("*** Update File: source.txt\n*** Move to: binary.dat\n@@\n-before\n+after"),
        ] {
            fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let delta = apply_patch(
                &patch,
                &cwd,
                &mut stdout,
                &mut stderr,
                LOCAL_FS.as_ref(),
                None,
            )
            .await
            .unwrap();

            assert!(!delta.is_exact());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_delete_symlink_returns_inexact_delta() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        fs::write(dir.path().join("target.txt"), "target\n").unwrap();
        symlink(dir.path().join("target.txt"), dir.path().join("link.txt")).unwrap();
        let patch = wrap_patch("*** Delete File: link.txt");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let delta = apply_patch(
            &patch,
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut stdout,
            &mut stderr,
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();

        assert!(!delta.is_exact());
    }

    #[tokio::test]
    async fn test_preserve_mode_keeps_mixed_line_endings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mixed.txt");
        fs::write(&path, b"one\r\ntwo\rthree\nfour\r\n").unwrap();
        let patch = wrap_patch("*** Update File: mixed.txt\n@@\n one\n two\n-three\n+THREE\n four");

        apply_patch_with_options(
            &patch,
            ApplyPatchOptions {
                update_file_mode: ApplyPatchFileUpdateMode::PreserveLineEndings,
                follow_symlinks: true,
            },
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut Vec::new(),
            &mut Vec::new(),
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(fs::read(path).unwrap(), b"one\r\ntwo\rTHREE\r\nfour\r\n");
    }

    #[tokio::test]
    async fn test_preserve_mode_rejects_overlapping_eof_chunks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("overlapping.txt");
        fs::write(&path, "one\n").unwrap();
        let patch = wrap_patch(
            "*** Update File: overlapping.txt\n@@\n-one\n+first\n@@\n-one\n+second\n*** End of File",
        );

        apply_patch_with_options(
            &patch,
            ApplyPatchOptions {
                update_file_mode: ApplyPatchFileUpdateMode::PreserveLineEndings,
                follow_symlinks: true,
            },
            &PathUri::from_host_native_path(dir.path()).expect("absolute test path"),
            &mut Vec::new(),
            &mut Vec::new(),
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .expect_err("overlapping EOF chunk must fail");

        assert_eq!(fs::read_to_string(path).unwrap(), "one\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_no_follow_rejects_leaf_and_ancestor_links_for_patch_operations() {
        use std::os::unix::fs::symlink;

        for path in ["link.txt", "linked/victim.txt"] {
            for body in [
                format!("*** Add File: {path}\n+changed"),
                format!("*** Update File: {path}\n@@\n-original\n+changed"),
                format!("*** Delete File: {path}"),
                format!(
                    "*** Update File: {path}\n*** Move to: moved.txt\n@@\n-original\n+changed"
                ),
                format!(
                    "*** Update File: source.txt\n*** Move to: {path}\n@@\n-original\n+changed"
                ),
                "*** Add File: linked/new/nested.txt\n+changed".to_string(),
                "*** Update File: source.txt\n*** Move to: linked/new/nested.txt\n@@\n-original\n+changed"
                    .to_string(),
            ] {
                let temp = tempdir().unwrap();
                let root = temp.path().canonicalize().unwrap();
                let work = root.join("work");
                let outside = root.join("outside");
                fs::create_dir(&work).unwrap();
                fs::create_dir(&outside).unwrap();
                fs::write(outside.join("victim.txt"), "original\n").unwrap();
                fs::write(work.join("source.txt"), "original\n").unwrap();
                symlink(outside.join("victim.txt"), work.join("link.txt")).unwrap();
                symlink(&outside, work.join("linked")).unwrap();
                let patch = wrap_patch(&body);

                let failure = apply_patch_with_options(
                    &patch,
                    ApplyPatchOptions {
                        follow_symlinks: false,
                        ..Default::default()
                    },
                    &PathUri::from_host_native_path(&work).expect("absolute test path"),
                    &mut Vec::new(),
                    &mut Vec::new(),
                    LOCAL_FS.as_ref(),
                    None,
                )
                .await
                .expect_err(&body);

                assert!(failure.delta().is_empty(), "{body}");
                assert_eq!(
                    fs::read_to_string(outside.join("victim.txt")).unwrap(),
                    "original\n"
                );
                assert_eq!(
                    fs::read_to_string(work.join("source.txt")).unwrap(),
                    "original\n"
                );
                assert!(!outside.join("new").exists(), "{body}");
                assert!(!work.join("moved.txt").exists(), "{body}");
                assert!(
                    fs::symlink_metadata(work.join("link.txt"))
                        .unwrap()
                        .is_symlink()
                );
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_no_follow_applies_regular_file_operations() {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let patch = wrap_patch(
            "*** Add File: new/file.txt\n+original\n*** Update File: new/file.txt\n*** Move to: moved/file.txt\n@@\n-original\n+changed\n*** Add File: remove.txt\n+remove\n*** Delete File: remove.txt",
        );

        apply_patch_with_options(
            &patch,
            ApplyPatchOptions {
                follow_symlinks: false,
                ..Default::default()
            },
            &PathUri::from_host_native_path(&root).expect("absolute test path"),
            &mut Vec::new(),
            &mut Vec::new(),
            LOCAL_FS.as_ref(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            fs::read_to_string(root.join("moved/file.txt")).unwrap(),
            "changed\n"
        );
        assert!(!root.join("new/file.txt").exists());
        assert!(!root.join("remove.txt").exists());
    }
}
