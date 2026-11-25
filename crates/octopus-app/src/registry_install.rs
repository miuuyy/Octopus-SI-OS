use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use octopus_file_storage::publication::{
    self, DigestAlgorithm, PublicationOutcome, PublicationReceipt, WriteGuard,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const INSTALL_MANIFEST_FILE: &str = ".octopus-source-manifest.json";
const SOURCE_IDENTITY_FILE: &str = ".octopus-source.json";
const INSTALL_MANIFEST_VERSION: u32 = 2;
const SOURCE_IDENTITY_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstalledFile {
    sha256: String,
    mode: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallManifest {
    schema_version: u32,
    source_id: String,
    source_registry: PathBuf,
    files: BTreeMap<String, InstalledFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceIdentity {
    schema_version: u32,
    id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRegistry {
    pub root: PathBuf,
    pub copied: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrySyncProgress<'a> {
    Scanned {
        files: usize,
    },
    Applying {
        done: usize,
        total: usize,
        relative: &'a str,
    },
}

#[derive(Debug)]
pub enum RegistryInstallError {
    SourceRoot {
        path: PathBuf,
        source: std::io::Error,
    },
    ActiveRoot {
        path: PathBuf,
        source: std::io::Error,
    },
    SameRoot {
        path: PathBuf,
    },
    OverlappingRoots {
        source_registry: PathBuf,
        active_registry: PathBuf,
    },
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    EntryType {
        path: PathBuf,
        source: std::io::Error,
    },
    UnsupportedEntry {
        path: PathBuf,
    },
    PublicationMetadata {
        path: PathBuf,
    },
    ReservedSourcePath {
        path: PathBuf,
    },
    UnsafeManifestPath {
        path: String,
    },
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    ManifestParse {
        path: PathBuf,
        source: serde_json::Error,
    },
    ManifestVersion {
        path: PathBuf,
        version: u32,
    },
    SourceIdentity {
        path: PathBuf,
        reason: String,
    },
    SourceChanged {
        manifest: PathBuf,
        expected: String,
        actual: String,
    },
    Conflict {
        path: PathBuf,
        reason: String,
    },
    SourceFileChanged {
        path: PathBuf,
        expected_sha256: String,
        expected_mode: Option<u32>,
        actual_sha256: String,
        actual_mode: Option<u32>,
    },
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },
    Publication {
        receipt: PublicationReceipt,
    },
    EncodeManifest {
        path: PathBuf,
        source: serde_json::Error,
    },
}

impl std::fmt::Display for RegistryInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceRoot { path, source } => write!(
                f,
                "source registry '{}' cannot be canonicalized: {source}",
                path.display()
            ),
            Self::ActiveRoot { path, source } => write!(
                f,
                "active registry root '{}' cannot be prepared: {source}",
                path.display()
            ),
            Self::SameRoot { path } => write!(
                f,
                "source and active registries resolve to the same path '{}'; the source catalog must remain read-only",
                path.display()
            ),
            Self::OverlappingRoots {
                source_registry,
                active_registry,
            } => write!(
                f,
                "source registry '{}' and active registry '{}' overlap; neither registry may contain the other",
                source_registry.display(),
                active_registry.display()
            ),
            Self::ReadDirectory { path, source } => write!(
                f,
                "registry directory '{}' cannot be read: {source}",
                path.display()
            ),
            Self::EntryType { path, source } => write!(
                f,
                "registry entry '{}' cannot be inspected: {source}",
                path.display()
            ),
            Self::UnsupportedEntry { path } => write!(
                f,
                "registry entry '{}' is neither a regular file nor a directory; links and special files are not installable",
                path.display()
            ),
            Self::PublicationMetadata { path } => write!(
                f,
                "reserved publication metadata '{}' must be a real private directory",
                path.display()
            ),
            Self::ReservedSourcePath { path } => write!(
                f,
                "source registry contains reserved installer path '{}'",
                path.display()
            ),
            Self::UnsafeManifestPath { path } => {
                write!(
                    f,
                    "registry install manifest contains unsafe relative path '{path}'"
                )
            }
            Self::ReadFile { path, source } => {
                write!(
                    f,
                    "registry file '{}' cannot be read: {source}",
                    path.display()
                )
            }
            Self::ManifestParse { path, source } => write!(
                f,
                "registry install manifest '{}' is malformed: {source}",
                path.display()
            ),
            Self::ManifestVersion { path, version } => write!(
                f,
                "registry install manifest '{}' has schema_version {version}, expected {INSTALL_MANIFEST_VERSION}",
                path.display()
            ),
            Self::SourceIdentity { path, reason } => write!(
                f,
                "registry source identity '{}' is invalid: {reason}",
                path.display()
            ),
            Self::SourceChanged {
                manifest,
                expected,
                actual,
            } => write!(
                f,
                "registry install manifest '{}' belongs to source identity '{}', not '{}'; refusing an ambiguous source overlay",
                manifest.display(),
                expected,
                actual
            ),
            Self::Conflict { path, reason } => write!(
                f,
                "active registry path '{}' conflicts with the source catalog: {reason}",
                path.display()
            ),
            Self::SourceFileChanged {
                path,
                expected_sha256,
                expected_mode,
                actual_sha256,
                actual_mode,
            } => write!(
                f,
                "source registry file '{}' changed during installation (expected sha256 {} mode {:?}, read sha256 {} mode {:?})",
                path.display(),
                expected_sha256,
                expected_mode,
                actual_sha256,
                actual_mode
            ),
            Self::WriteFile { path, source } => write!(
                f,
                "registry installer cannot write '{}': {source}",
                path.display()
            ),
            Self::Publication { receipt } => write!(
                f,
                "registry publication '{}' for '{}' is {:?}: {}",
                receipt.operation_id,
                receipt.path.display(),
                receipt.outcome,
                receipt.error.as_deref().unwrap_or("no final applied receipt")
            ),
            Self::EncodeManifest { path, source } => write!(
                f,
                "registry install manifest '{}' cannot be encoded: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for RegistryInstallError {}

#[derive(Debug)]
enum TargetEntry {
    Missing,
    File(InstalledFile),
    Directory,
    Unsupported,
}

#[derive(Debug)]
struct PreparedCopy {
    staged: PathBuf,
    installed: InstalledFile,
    bytes: u64,
    operation_id: String,
}

impl Drop for PreparedCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.staged);
    }
}

pub fn install_registry(
    source_registry: &Path,
    active_registry: &Path,
) -> Result<InstalledRegistry, RegistryInstallError> {
    install_registry_with_progress(source_registry, active_registry, &mut |_| {})
}

pub fn install_registry_with_progress(
    source_registry: &Path,
    active_registry: &Path,
    progress: &mut dyn FnMut(RegistrySyncProgress<'_>),
) -> Result<InstalledRegistry, RegistryInstallError> {
    let source_registry = std::fs::canonicalize(source_registry).map_err(|source| {
        RegistryInstallError::SourceRoot {
            path: source_registry.to_path_buf(),
            source,
        }
    })?;
    if !source_registry.is_dir() {
        return Err(RegistryInstallError::UnsupportedEntry {
            path: source_registry,
        });
    }
    let active_registry = std::path::absolute(active_registry).map_err(|source| {
        RegistryInstallError::ActiveRoot {
            path: active_registry.to_path_buf(),
            source,
        }
    })?;
    publication::create_dir_all(&active_registry).map_err(|source| {
        RegistryInstallError::ActiveRoot {
            path: active_registry.to_path_buf(),
            source,
        }
    })?;
    let active_registry = std::fs::canonicalize(&active_registry).map_err(|source| {
        RegistryInstallError::ActiveRoot {
            path: active_registry.to_path_buf(),
            source,
        }
    })?;
    if source_registry == active_registry {
        return Err(RegistryInstallError::SameRoot {
            path: source_registry,
        });
    }
    if source_registry.starts_with(&active_registry)
        || active_registry.starts_with(&source_registry)
    {
        return Err(RegistryInstallError::OverlappingRoots {
            source_registry,
            active_registry,
        });
    }

    validate_tree_entries(&active_registry, &active_registry, true)?;
    let source_id = load_source_identity(&source_registry)?;
    let manifest_path = active_registry.join(INSTALL_MANIFEST_FILE);
    if let Some((installed, _)) = load_manifest(&manifest_path)? {
        if installed.source_id != source_id {
            return Err(RegistryInstallError::SourceChanged {
                manifest: manifest_path,
                expected: installed.source_id,
                actual: source_id,
            });
        }
        return Ok(InstalledRegistry { root: active_registry, copied: 0 });
    }

    let source_files = collect_source_files(&source_registry)?;
    let source_kind_roots = collect_source_kind_roots(&source_registry)?;
    progress(RegistrySyncProgress::Scanned { files: source_files.len() });
    let mut pending = Vec::new();
    for (relative, source_file) in &source_files {
        let target = active_registry.join(safe_relative_path(relative)?);
        match inspect_target(&target)? {
            TargetEntry::Missing => pending.push(relative),
            TargetEntry::File(existing) if existing == *source_file => {}
            _ => return Err(RegistryInstallError::Conflict {
                path: target,
                reason: "initial installation conflicts with an existing active entry".into(),
            }),
        }
    }
    let mut prepared = Vec::with_capacity(pending.len());
    let batch_mode = pending.len() > 1;
    for relative in pending {
        let relative_path = safe_relative_path(relative)?;
        let copy = prepare_copy_with_sync(
            &source_registry.join(&relative_path),
            &active_registry.join(&relative_path),
            &source_files[relative],
            !batch_mode,
        )?;
        prepared.push((relative, copy));
    }
    if batch_mode {
        let active_dir = File::open(&active_registry).map_err(|source| RegistryInstallError::ActiveRoot {
            path: active_registry.clone(),
            source,
        })?;
        let fs_synced = octopus_file_storage::sync_filesystem_ordered(&active_dir).map_err(|source| {
            RegistryInstallError::ActiveRoot {
                path: active_registry.clone(),
                source,
            }
        })?;
        if !fs_synced {
            for (_, copy) in &prepared {
                let file = File::open(&copy.staged).map_err(|source| RegistryInstallError::WriteFile {
                    path: copy.staged.clone(),
                    source,
                })?;
                octopus_file_storage::sync_ordered(&file).map_err(|source| {
                    RegistryInstallError::WriteFile {
                        path: copy.staged.clone(),
                        source,
                    }
                })?;
            }
        }
    }
    materialize_source_kind_roots(&active_registry, &source_kind_roots)?;
    if batch_mode {
        commit_prepared_batch(&active_registry, &prepared, progress)?;
    } else {
        for (index, (relative, copy)) in prepared.iter().enumerate() {
            let target = active_registry.join(safe_relative_path(relative)?);
            commit_prepared_copy(copy, &target, None)?;
            progress(RegistrySyncProgress::Applying {
                done: index + 1,
                total: prepared.len(),
                relative,
            });
        }
    }
    let copied = prepared.len();
    drop(prepared);

    let manifest = InstallManifest {
        schema_version: INSTALL_MANIFEST_VERSION,
        source_id,
        source_registry,
        files: source_files,
    };
    write_manifest(&manifest_path, &manifest, None)?;
    Ok(InstalledRegistry {
        root: active_registry,
        copied,
    })
}

fn collect_source_kind_roots(source_registry: &Path) -> Result<Vec<PathBuf>, RegistryInstallError> {
    let mut entries = std::fs::read_dir(source_registry)
        .map_err(|source| RegistryInstallError::ReadDirectory {
            path: source_registry.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| RegistryInstallError::ReadDirectory {
            path: source_registry.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut roots = Vec::new();
    for entry in entries {
        let path = entry.path();
        if is_publication_metadata(&path)? {
            continue;
        }
        if entry
            .file_type()
            .map_err(|source| RegistryInstallError::EntryType {
                path: path.clone(),
                source,
            })?
            .is_dir()
        {
            roots.push(PathBuf::from(entry.file_name()));
        }
    }
    Ok(roots)
}

fn materialize_source_kind_roots(
    active_registry: &Path,
    roots: &[PathBuf],
) -> Result<(), RegistryInstallError> {
    for root in roots {
        let path = active_registry.join(root);
        publication::create_dir_all(&path)
            .map_err(|source| RegistryInstallError::ActiveRoot { path, source })?;
    }
    Ok(())
}

fn is_publication_metadata(path: &Path) -> Result<bool, RegistryInstallError> {
    if path
        .file_name()
        .is_none_or(|name| name != publication::COORDINATION_DIRECTORY)
    {
        return Ok(false);
    }
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| RegistryInstallError::EntryType {
            path: path.to_path_buf(),
            source,
        })?;
    if !metadata.is_dir() || file_mode(&metadata).is_some_and(|mode| mode & 0o077 != 0) {
        return Err(RegistryInstallError::PublicationMetadata {
            path: path.to_path_buf(),
        });
    }
    Ok(true)
}

fn collect_source_files(
    source_registry: &Path,
) -> Result<BTreeMap<String, InstalledFile>, RegistryInstallError> {
    let mut files = BTreeMap::new();
    collect_source_directory(source_registry, source_registry, &mut files)?;
    Ok(files)
}

fn collect_source_directory(
    source_registry: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, InstalledFile>,
) -> Result<(), RegistryInstallError> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(|source| RegistryInstallError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| RegistryInstallError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if is_publication_metadata(&path)? {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|source| RegistryInstallError::EntryType {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            collect_source_directory(source_registry, &path, files)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(RegistryInstallError::UnsupportedEntry { path });
        }
        let relative = relative_key(source_registry, &path)?;
        if relative == INSTALL_MANIFEST_FILE {
            return Err(RegistryInstallError::ReservedSourcePath { path });
        }
        if relative == SOURCE_IDENTITY_FILE {
            continue;
        }
        files.insert(relative, digest_file(&path)?);
    }
    Ok(())
}

fn validate_tree_entries(
    root: &Path,
    directory: &Path,
    allow_manifest: bool,
) -> Result<(), RegistryInstallError> {
    let entries =
        std::fs::read_dir(directory).map_err(|source| RegistryInstallError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
    for entry in entries {
        let entry = entry.map_err(|source| RegistryInstallError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if is_publication_metadata(&path)? {
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|source| RegistryInstallError::EntryType {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            validate_tree_entries(root, &path, allow_manifest)?;
        } else if file_type.is_file() {
            let relative = relative_key(root, &path)?;
            if !allow_manifest && relative == INSTALL_MANIFEST_FILE {
                return Err(RegistryInstallError::ReservedSourcePath { path });
            }
        } else {
            return Err(RegistryInstallError::UnsupportedEntry { path });
        }
    }
    Ok(())
}

fn relative_key(root: &Path, path: &Path) -> Result<String, RegistryInstallError> {
    let relative =
        path.strip_prefix(root)
            .map_err(|_| RegistryInstallError::UnsafeManifestPath {
                path: path.display().to_string(),
            })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(RegistryInstallError::UnsafeManifestPath {
                path: relative.display().to_string(),
            });
        };
        let part = part
            .to_str()
            .ok_or_else(|| RegistryInstallError::UnsafeManifestPath {
                path: relative.display().to_string(),
            })?;
        parts.push(part);
    }
    if parts.is_empty() {
        return Err(RegistryInstallError::UnsafeManifestPath {
            path: relative.display().to_string(),
        });
    }
    Ok(parts.join("/"))
}

fn safe_relative_path(relative: &str) -> Result<PathBuf, RegistryInstallError> {
    let path = Path::new(relative);
    if path.is_absolute() || relative.is_empty() {
        return Err(RegistryInstallError::UnsafeManifestPath {
            path: relative.to_string(),
        });
    }
    let mut output = PathBuf::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(RegistryInstallError::UnsafeManifestPath {
                path: relative.to_string(),
            });
        };
        if part == publication::COORDINATION_DIRECTORY {
            return Err(RegistryInstallError::UnsafeManifestPath {
                path: relative.to_string(),
            });
        }
        output.push(part);
    }
    if output.as_os_str().is_empty() {
        return Err(RegistryInstallError::UnsafeManifestPath {
            path: relative.to_string(),
        });
    }
    Ok(output)
}

fn inspect_target(path: &Path) -> Result<TargetEntry, RegistryInstallError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(TargetEntry::File(digest_file(path)?)),
        Ok(metadata) if metadata.file_type().is_dir() => Ok(TargetEntry::Directory),
        Ok(_) => Ok(TargetEntry::Unsupported),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(TargetEntry::Missing),
        Err(source) => Err(RegistryInstallError::EntryType {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn digest_file(path: &Path) -> Result<InstalledFile, RegistryInstallError> {
    let mut file = File::open(path).map_err(|source| RegistryInstallError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file
        .metadata()
        .map_err(|source| RegistryInstallError::EntryType {
            path: path.to_path_buf(),
            source,
        })?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| RegistryInstallError::ReadFile {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let sha256 = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(InstalledFile {
        sha256,
        mode: file_mode(&metadata),
    })
}

#[cfg(unix)]
fn file_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn file_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

fn load_manifest(
    path: &Path,
) -> Result<Option<(InstallManifest, InstalledFile)>, RegistryInstallError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(RegistryInstallError::ReadFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let metadata = file
        .metadata()
        .map_err(|source| RegistryInstallError::EntryType {
            path: path.to_path_buf(),
            source,
        })?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|source| RegistryInstallError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
    let previous = installed_bytes(&bytes, &metadata);
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|source| RegistryInstallError::ManifestParse {
            path: path.to_path_buf(),
            source,
        })?;
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    match version {
        INSTALL_MANIFEST_VERSION => serde_json::from_value(value)
            .map(|manifest| Some((manifest, previous)))
            .map_err(|source| RegistryInstallError::ManifestParse {
                path: path.to_path_buf(),
                source,
            }),
        version => Err(RegistryInstallError::ManifestVersion {
            path: path.to_path_buf(),
            version,
        }),
    }
}

fn load_source_identity(source_registry: &Path) -> Result<String, RegistryInstallError> {
    let path = source_registry.join(SOURCE_IDENTITY_FILE);
    let bytes = std::fs::read(&path).map_err(|source| RegistryInstallError::ReadFile {
        path: path.clone(),
        source,
    })?;
    let identity: SourceIdentity =
        serde_json::from_slice(&bytes).map_err(|source| RegistryInstallError::ManifestParse {
            path: path.clone(),
            source,
        })?;
    if identity.schema_version != SOURCE_IDENTITY_VERSION {
        return Err(RegistryInstallError::SourceIdentity {
            path,
            reason: format!(
                "schema_version {} does not equal {}",
                identity.schema_version, SOURCE_IDENTITY_VERSION
            ),
        });
    }
    if identity.id.is_empty()
        || identity.id.trim() != identity.id
        || !identity
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RegistryInstallError::SourceIdentity {
            path,
            reason: "id must be a non-empty portable identifier".to_string(),
        });
    }
    Ok(identity.id)
}

#[cfg(test)]
fn prepare_copy(
    source: &Path,
    target: &Path,
    expected: &InstalledFile,
) -> Result<PreparedCopy, RegistryInstallError> {
    prepare_copy_with_sync(source, target, expected, true)
}

fn prepare_copy_with_sync(
    source: &Path,
    target: &Path,
    expected: &InstalledFile,
    sync: bool,
) -> Result<PreparedCopy, RegistryInstallError> {
    let mut source_file =
        File::open(source).map_err(|source_error| RegistryInstallError::ReadFile {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    let source_metadata =
        source_file
            .metadata()
            .map_err(|source_error| RegistryInstallError::EntryType {
                path: source.to_path_buf(),
                source: source_error,
            })?;
    let mut bytes = Vec::new();
    source_file
        .read_to_end(&mut bytes)
        .map_err(|source_error| RegistryInstallError::ReadFile {
            path: source.to_path_buf(),
            source: source_error,
        })?;
    let actual = installed_bytes(&bytes, &source_metadata);
    if &actual != expected {
        return Err(RegistryInstallError::SourceFileChanged {
            path: source.to_path_buf(),
            expected_sha256: expected.sha256.clone(),
            expected_mode: expected.mode,
            actual_sha256: actual.sha256,
            actual_mode: actual.mode,
        });
    }
    prepare_bytes_with_sync(target, &bytes, Some(&source_metadata), sync)
}

fn prepare_bytes(
    target: &Path,
    bytes: &[u8],
    source_metadata: Option<&std::fs::Metadata>,
) -> Result<PreparedCopy, RegistryInstallError> {
    prepare_bytes_with_sync(target, bytes, source_metadata, true)
}

fn prepare_bytes_with_sync(
    target: &Path,
    bytes: &[u8],
    source_metadata: Option<&std::fs::Metadata>,
    sync: bool,
) -> Result<PreparedCopy, RegistryInstallError> {
    let parent = target
        .parent()
        .ok_or_else(|| RegistryInstallError::WriteFile {
            path: target.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "target has no parent directory",
            ),
        })?;
    publication::create_dir_all(parent).map_err(|source| RegistryInstallError::WriteFile {
        path: parent.to_path_buf(),
        source,
    })?;
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let temp = temp_path(target, &attempt);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|source| RegistryInstallError::WriteFile {
            path: temp.clone(),
            source,
        })?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|source| RegistryInstallError::WriteFile {
                path: temp.clone(),
                source,
            })?;
        if let Some(metadata) = source_metadata {
            apply_source_permissions(metadata, &temp)?;
        }
        if sync {
            file.sync_all()
                .map_err(|source| RegistryInstallError::WriteFile {
                    path: temp.clone(),
                    source,
                })?;
        }
        let metadata = file
            .metadata()
            .map_err(|source| RegistryInstallError::EntryType {
                path: temp.clone(),
                source,
            })?;
        let installed = installed_bytes(bytes, &metadata);
        if source_metadata.is_some_and(|source| file_mode(source) != installed.mode) {
            return Err(RegistryInstallError::Conflict {
                path: temp.clone(),
                reason: "the prepared source permissions were not retained".to_string(),
            });
        }
        Ok(PreparedCopy {
            staged: temp.clone(),
            installed,
            bytes: bytes.len() as u64,
            operation_id: format!("registry-install:{attempt}"),
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn apply_source_permissions(
    source_metadata: &std::fs::Metadata,
    target: &Path,
) -> Result<(), RegistryInstallError> {
    let permissions = source_metadata.permissions();
    std::fs::set_permissions(target, permissions).map_err(|source_error| {
        RegistryInstallError::WriteFile {
            path: target.to_path_buf(),
            source: source_error,
        }
    })
}

#[cfg(not(unix))]
fn apply_source_permissions(
    _source_metadata: &std::fs::Metadata,
    _target: &Path,
) -> Result<(), RegistryInstallError> {
    Ok(())
}

fn installed_bytes(bytes: &[u8], metadata: &std::fs::Metadata) -> InstalledFile {
    let sha256 = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    InstalledFile {
        sha256,
        mode: file_mode(metadata),
    }
}

fn commit_prepared_batch(
    active_registry: &Path,
    prepared: &[(&String, PreparedCopy)],
    progress: &mut dyn FnMut(RegistrySyncProgress<'_>),
) -> Result<(), RegistryInstallError> {
    let total = prepared.len();
    let mut done = 0usize;
    for chunk in prepared.chunks(64) {
        let mut targets = Vec::with_capacity(chunk.len());
        let mut payloads = Vec::with_capacity(chunk.len());
        for (relative, copy) in chunk {
            let target = active_registry.join(safe_relative_path(relative)?);
            if !std::fs::symlink_metadata(&copy.staged)
                .map_err(|source| RegistryInstallError::ReadFile {
                    path: copy.staged.clone(),
                    source,
                })?
                .is_file()
            {
                return Err(RegistryInstallError::UnsupportedEntry {
                    path: copy.staged.clone(),
                });
            }
            let mut file =
                File::open(&copy.staged).map_err(|source| RegistryInstallError::ReadFile {
                    path: copy.staged.clone(),
                    source,
                })?;
            let metadata = file
                .metadata()
                .map_err(|source| RegistryInstallError::EntryType {
                    path: copy.staged.clone(),
                    source,
                })?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|source| RegistryInstallError::ReadFile {
                    path: copy.staged.clone(),
                    source,
                })?;
            if installed_bytes(&bytes, &metadata) != copy.installed
                || bytes.len() as u64 != copy.bytes
            {
                return Err(RegistryInstallError::Conflict {
                    path: copy.staged.clone(),
                    reason: "the prepared source bytes or permissions changed before publication"
                        .to_string(),
                });
            }
            targets.push(target);
            payloads.push(bytes);
        }
        let requests: Vec<publication::BatchPublish<'_>> = chunk
            .iter()
            .enumerate()
            .map(|(idx, (_, copy))| publication::BatchPublish {
                path: &targets[idx],
                operation_id: &copy.operation_id,
                expected_before: None,
                bytes: &payloads[idx],
                algorithm: DigestAlgorithm::Sha256,
                retained_stage: Some(&copy.staged),
            })
            .collect();
        let receipts = publication::publish_batch(&requests).map_err(|source| {
            RegistryInstallError::WriteFile {
                path: active_registry.to_path_buf(),
                source,
            }
        })?;
        if receipts.len() != chunk.len() {
            return Err(RegistryInstallError::Conflict {
                path: active_registry.to_path_buf(),
                reason: "batched registry publication stopped early on a conflict".to_string(),
            });
        }
        for ((idx, (relative, copy)), receipt) in chunk.iter().enumerate().zip(receipts.into_iter())
        {
            check_publication(
                receipt,
                &targets[idx],
                &copy.operation_id,
                None,
                Some(&copy.installed),
                copy.bytes,
            )?;
            done += 1;
            progress(RegistrySyncProgress::Applying {
                done,
                total,
                relative,
            });
        }
    }
    Ok(())
}

fn commit_prepared_copy(
    copy: &PreparedCopy,
    target: &Path,
    previous: Option<&InstalledFile>,
) -> Result<PublicationReceipt, RegistryInstallError> {
    let (guard, historical) =
        guard_prepared_publication(target, &copy.operation_id, previous, previous.is_none())?;
    let receipt = if let Some(receipt) = historical {
        receipt
    } else {
        if !std::fs::symlink_metadata(&copy.staged)
            .map_err(|source| RegistryInstallError::ReadFile {
                path: copy.staged.clone(),
                source,
            })?
            .is_file()
        {
            return Err(RegistryInstallError::UnsupportedEntry {
                path: copy.staged.clone(),
            });
        }
        let mut file =
            File::open(&copy.staged).map_err(|source| RegistryInstallError::ReadFile {
                path: copy.staged.clone(),
                source,
            })?;
        let metadata = file
            .metadata()
            .map_err(|source| RegistryInstallError::EntryType {
                path: copy.staged.clone(),
                source,
            })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| RegistryInstallError::ReadFile {
                path: copy.staged.clone(),
                source,
            })?;
        if installed_bytes(&bytes, &metadata) != copy.installed || bytes.len() as u64 != copy.bytes
        {
            return Err(RegistryInstallError::Conflict {
                path: copy.staged.clone(),
                reason: "the prepared source bytes or permissions changed before publication"
                    .to_string(),
            });
        }
        guard
            .publish_retained(
                &copy.operation_id,
                previous.map(|file| file.sha256.as_str()),
                &bytes,
                DigestAlgorithm::Sha256,
                &copy.staged,
            )
            .map_err(|source| RegistryInstallError::WriteFile {
                path: target.to_path_buf(),
                source,
            })?
    };
    check_publication(
        receipt,
        target,
        &copy.operation_id,
        previous,
        Some(&copy.installed),
        copy.bytes,
    )
}

fn guard_prepared_publication(
    target: &Path,
    operation_id: &str,
    previous: Option<&InstalledFile>,
    membership_change: bool,
) -> Result<(WriteGuard, Option<PublicationReceipt>), RegistryInstallError> {
    let write_error = |source| RegistryInstallError::WriteFile {
        path: target.to_path_buf(),
        source,
    };
    let guard = WriteGuard::acquire_operation(target, membership_change, operation_id)
        .map_err(write_error)?;
    let historical = guard.reconcile_operation(operation_id).map_err(write_error)?;
    if historical.is_none() {
        let unchanged = match (inspect_target(target)?, previous) {
            (TargetEntry::Missing, None) => true,
            (TargetEntry::File(current), Some(previous)) => &current == previous,
            _ => false,
        };
        if !unchanged {
            return Err(RegistryInstallError::Conflict {
                path: target.to_path_buf(),
                reason: "the active file bytes, permissions or presence changed after preflight"
                    .to_string(),
            });
        }
    }
    Ok((guard, historical))
}

fn check_publication(
    receipt: PublicationReceipt,
    target: &Path,
    operation_id: &str,
    previous: Option<&InstalledFile>,
    installed: Option<&InstalledFile>,
    bytes: u64,
) -> Result<PublicationReceipt, RegistryInstallError> {
    if receipt.path != target
        || receipt.operation_id != operation_id
        || receipt.digest_algorithm != DigestAlgorithm::Sha256
        || receipt.before_hash.as_deref() != previous.map(|file| file.sha256.as_str())
        || receipt.after_hash.as_deref() != installed.map(|file| file.sha256.as_str())
        || receipt.bytes != bytes
    {
        return Err(RegistryInstallError::Conflict {
            path: target.to_path_buf(),
            reason: "the native publication receipt differs from the exact prepared attempt"
                .to_string(),
        });
    }
    if receipt.outcome != PublicationOutcome::Applied || receipt.error.is_some() {
        return Err(RegistryInstallError::Publication { receipt });
    }
    Ok(receipt)
}

fn write_manifest(
    path: &Path,
    manifest: &InstallManifest,
    previous: Option<&InstalledFile>,
) -> Result<PublicationReceipt, RegistryInstallError> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(|source| {
        RegistryInstallError::EncodeManifest {
            path: path.to_path_buf(),
            source,
        }
    })?;
    bytes.push(b'\n');
    let copy = prepare_bytes(path, &bytes, None)?;
    commit_prepared_copy(&copy, path, previous)
}

fn temp_path(path: &Path, attempt: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("registry-install");
    path.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), attempt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn prepare_source(path: &Path, id: &str) {
        write(
            &path.join(SOURCE_IDENTITY_FILE),
            &format!(r#"{{"schema_version":1,"id":"{id}"}}"#),
        );
    }

    #[test]
    fn install_materializes_empty_registry_kind_roots() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let active = temp.path().join("home/registry");
        prepare_source(&source, "test-baseline");
        for kind in ["artifact", "interface", "tool", "worker_environment_kind"] {
            std::fs::create_dir_all(source.join(kind)).unwrap();
        }

        let installed = install_registry(&source, &active).unwrap();

        assert_eq!(installed.copied, 0);
        for kind in ["artifact", "interface", "tool", "worker_environment_kind"] {
            assert!(active.join(kind).is_dir());
        }
    }

    #[test]
    fn installed_home_is_not_updated_from_a_changed_distribution() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let active = temp.path().join("home/registry");
        prepare_source(&source, "test-baseline");
        let relative = "interface/base/interface.json";
        write(&source.join(relative), "initial");
        assert_eq!(install_registry(&source, &active).unwrap().copied, 1);
        write(&active.join(relative), "local");
        let manifest = std::fs::read(active.join(INSTALL_MANIFEST_FILE)).unwrap();
        write(&source.join(relative), "new release");
        write(&source.join("tool/new/tool.json"), "new owner");
        assert_eq!(install_registry(&source, &active).unwrap().copied, 0);
        assert_eq!(std::fs::read_to_string(active.join(relative)).unwrap(), "local");
        assert!(!active.join("tool/new/tool.json").exists());
        assert_eq!(std::fs::read(active.join(INSTALL_MANIFEST_FILE)).unwrap(), manifest);
    }

    #[test]
    fn initial_installation_preflights_conflicts_before_copying() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let active = temp.path().join("home/registry");
        prepare_source(&source, "test-baseline");
        write(&source.join("tool/a/tool.json"), "a");
        write(&source.join("tool/z/tool.json"), "z");
        write(&active.join("tool/z/tool.json"), "local");
        assert!(matches!(install_registry(&source, &active), Err(RegistryInstallError::Conflict { .. })));
        assert!(!active.join("tool/a/tool.json").exists());
        assert_eq!(std::fs::read_to_string(active.join("tool/z/tool.json")).unwrap(), "local");
    }

    #[test]
    fn install_refuses_registry_roots_nested_in_either_direction() {
        let first = tempfile::tempdir().unwrap();
        let source = first.path().join("source");
        prepare_source(&source, "test-baseline");
        write(&source.join("interface/base/interface.json"), "source");
        let active_inside_source = source.join("active");
        let error = install_registry(&source, &active_inside_source).unwrap_err();
        assert!(matches!(
            error,
            RegistryInstallError::OverlappingRoots { .. }
        ));

        let second = tempfile::tempdir().unwrap();
        let active = second.path().join("active");
        let source_inside_active = active.join("source");
        prepare_source(&source_inside_active, "test-baseline");
        write(
            &source_inside_active.join("interface/base/interface.json"),
            "source",
        );
        let error = install_registry(&source_inside_active, &active).unwrap_err();
        assert!(matches!(
            error,
            RegistryInstallError::OverlappingRoots { .. }
        ));
    }

    #[test]
    fn prepared_copy_refuses_source_drift_without_touching_active_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source/interface.json");
        let target = temp.path().join("active/interface.json");
        write(&source, "source-v1");
        write(&target, "active-local");
        let expected = digest_file(&source).unwrap();
        write(&source, "source-v2");

        let error = prepare_copy(&source, &target, &expected).unwrap_err();
        assert!(matches!(
            error,
            RegistryInstallError::SourceFileChanged { .. }
        ));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "active-local");
        assert_eq!(
            std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn prepared_publications_keep_exact_predecessors_and_original_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        let target = root.join("active/file");
        write(&source, "after");
        let expected = digest_file(&source).unwrap();
        let copy = prepare_copy(&source, &target, &expected).unwrap();
        publication::publish(&target, "equal-concurrent-create", None, b"after").unwrap();
        assert!(matches!(
            commit_prepared_copy(&copy, &target, None),
            Err(RegistryInstallError::Conflict { .. })
        ));
        assert!(publication::read_receipt(&target, &copy.operation_id)
            .unwrap()
            .is_none());

        let previous = digest_file(&target).unwrap();
        let copy = prepare_copy(&source, &target, &expected).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
            let mode_change =
                prepare_copy(&source, &target, &digest_file(&source).unwrap()).unwrap();
            commit_prepared_copy(&mode_change, &target, Some(&previous)).unwrap();
            assert!(matches!(
                commit_prepared_copy(&copy, &target, Some(&previous)),
                Err(RegistryInstallError::Conflict { .. })
            ));
        }
        let previous = digest_file(&target).unwrap();
        let previous_attempt = copy.operation_id.clone();
        let copy = prepare_copy(&source, &target, &digest_file(&source).unwrap()).unwrap();
        assert_ne!(copy.operation_id, previous_attempt);
        let first = commit_prepared_copy(&copy, &target, Some(&previous)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(&copy.staged).unwrap().ino(),
                std::fs::metadata(&target).unwrap().ino()
            );
        }
        publication::publish(
            &target,
            "later-write",
            first.after_hash.as_deref(),
            b"later",
        )
        .unwrap();
        std::fs::remove_file(&copy.staged).unwrap();
        assert_eq!(
            commit_prepared_copy(&copy, &target, Some(&previous)).unwrap(),
            first
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"later");
        assert_eq!(
            publication::read_receipt(&target, &copy.operation_id).unwrap(),
            Some(first)
        );
    }

    #[test]
    fn install_accepts_a_relocated_source_with_the_same_identity() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first/registry");
        let second = temp.path().join("second/registry");
        let active = temp.path().join("home/registry");
        prepare_source(&first, "release-baseline");
        prepare_source(&second, "release-baseline");
        write(&first.join("interface/base/interface.json"), "v1");
        write(&second.join("interface/base/interface.json"), "v2");

        install_registry(&first, &active).unwrap();
        let installed = install_registry(&second, &active).unwrap();

        assert_eq!(installed.copied, 0);
        assert_eq!(
            std::fs::read_to_string(active.join("interface/base/interface.json")).unwrap(),
            "v1"
        );
    }

    #[test]
    fn install_refuses_a_different_source_identity() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first/registry");
        let second = temp.path().join("second/registry");
        let active = temp.path().join("home/registry");
        prepare_source(&first, "release-baseline");
        prepare_source(&second, "different-baseline");
        write(&first.join("interface/base/interface.json"), "v1");
        write(&second.join("interface/base/interface.json"), "v1");

        install_registry(&first, &active).unwrap();
        let error = install_registry(&second, &active).unwrap_err();

        assert!(matches!(error, RegistryInstallError::SourceChanged { .. }));
    }
}
