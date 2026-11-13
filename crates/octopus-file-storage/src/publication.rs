use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod access;
mod directory;
mod history;
mod namespace;
mod native;
mod reclaim;

pub use access::{FilePublicationAccess, PublicationFileSelector, file_publication_access, publication_ancestor_directories};
pub use directory::{DirectoryEntry, DirectoryEntryKind, DirectoryMove, DirectoryMoveReceipt,
    DirectoryPreimage, DirectoryPublicationResult, publish_directory_moves, snapshot_directory};
pub use reclaim::reclaim_directory;
use namespace::NamespaceGuard;

pub const COORDINATION_DIRECTORY: &str = ".octopus-publication";
const RECORD_LIMIT: u64 = 64 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationOutcome {
    Applied,
    NotApplied,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestAlgorithm { Sha256, Blake3 }

impl DigestAlgorithm {
    fn digest(self, bytes: &[u8]) -> String {
        match self {
            Self::Sha256 => hash(bytes),
            Self::Blake3 => blake3::hash(bytes).to_hex().to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReceipt {
    pub digest_algorithm: DigestAlgorithm,
    pub operation_id: String,
    pub path: PathBuf,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub bytes: u64,
    pub outcome: PublicationOutcome,
    pub error: Option<String>,
}

pub fn read_receipt(path: &Path, operation_id: &str) -> io::Result<Option<PublicationReceipt>> {
    namespace::validate_absolute(path)?;
    validate_operation(operation_id)?;
    if path.file_name().is_none() || path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY) {
        return Err(invalid("publication receipt lookup requires an ordinary absolute file path"));
    }
    history::find_file(path, operation_id)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

impl FileIdentity {
    fn of(metadata: &Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self { device: metadata.dev(), inode: metadata.ino() })
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Err(io::Error::new(io::ErrorKind::Unsupported, "file publication requires durable file identity"))
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingPublication {
    schema_version: u32,
    receipt: PublicationReceipt,
    before_identity: Option<FileIdentity>,
    stage_identity: FileIdentity,
}

struct HeldFile {
    file: File,
    path: PathBuf,
    directory: bool,
    version: Option<FileVersion>,
}

#[derive(PartialEq, Eq)]
struct FileVersion {
    length: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
}

impl FileVersion {
    fn of(metadata: &Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            length: metadata.len(), modified: metadata.modified()?,
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

impl HeldFile {
    fn acquire(path: PathBuf, exclusive: bool, wait: bool) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        secure_options(&mut options);
        let file = options.open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("publication lock is not a regular file"));
        }
        take_lock(&file, exclusive, wait)?;
        let held = Self { file, path, directory: false, version: None };
        held.verify()?;
        Ok(held)
    }

    fn verify(&self) -> io::Result<()> {
        let current = std::fs::symlink_metadata(&self.path)?;
        if (if self.directory { !current.is_dir() } else { !current.is_file() })
            || FileIdentity::of(&current)? != FileIdentity::of(&self.file.metadata()?)?
        {
            return Err(invalid("publication lock was replaced while held"));
        }
        if let Some(version) = &self.version {
            if FileVersion::of(&current)? != *version {
                return Err(invalid("admission source changed while its read guard was held"));
            }
        }
        Ok(())
    }

    fn existing(path: PathBuf, exclusive: bool, directory: bool, wait: bool) -> io::Result<Self> {
        let file = if directory { File::open(&path)? } else { open_regular(&path)? };
        take_lock(&file, exclusive, wait)?;
        let version = if directory || exclusive { None } else { Some(FileVersion::of(&file.metadata()?)?) };
        let held = Self { file, path, directory, version };
        held.verify()?;
        Ok(held)
    }
}

fn take_lock(file: &File, exclusive: bool, wait: bool) -> io::Result<()> {
    match (exclusive, wait) {
        (true, true) => FileExt::lock_exclusive(file),
        (false, true) => FileExt::lock_shared(file),
        (true, false) => FileExt::try_lock_exclusive(file),
        (false, false) => FileExt::try_lock_shared(file),
    }
}

struct Target {
    path: PathBuf,
    root: PathBuf,
    key: String,
}

impl Target {
    fn coordinates(path: PathBuf) -> Self {
        let root = path.parent().unwrap().join(COORDINATION_DIRECTORY);
        let key = hash(path.file_name().unwrap().as_encoded_bytes());
        Self { path, root, key }
    }

    fn open(path: &Path, initialize: bool) -> io::Result<Self> {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(invalid("publication target must be an absolute file path"));
        }
        let parent = std::fs::canonicalize(path.parent().unwrap())?;
        let name = path.file_name().unwrap();
        if path.components().any(|component| component.as_os_str() == COORDINATION_DIRECTORY) {
            return Err(invalid("publication metadata is not a publication target"));
        }
        let path = parent.join(name);
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| !metadata.is_file()) {
            return Err(invalid("publication target must be a regular file or absent"));
        }
        if initialize { coordination_root(&parent)?; }
        Ok(Self::coordinates(path))
    }

    fn lock_path(&self) -> PathBuf { self.root.join(format!("{}.lock", self.key)) }

    fn lock(&self, exclusive: bool, wait: bool) -> io::Result<HeldFile> {
        HeldFile::acquire(self.lock_path(), exclusive, wait)
    }

    fn pending(&self) -> PathBuf {
        self.root.join(format!("{}.pending", self.key))
    }

    fn stage(&self, operation_id: &str) -> PathBuf {
        self.root.join(format!("{}{}.bytes", self.stage_prefix(), hash(operation_id.as_bytes())))
    }

    fn stage_prefix(&self) -> String { format!("{}-", self.key) }

    fn predecessor(&self, operation_id: &str) -> PathBuf {
        self.root.join(format!("{}{}.preimage", self.stage_prefix(), hash(operation_id.as_bytes())))
    }

    fn retain_predecessor(&self, operation_id: &str, file: &File) -> io::Result<()> {
        let path = self.predecessor(operation_id);
        match std::fs::hard_link(&self.path, &path) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
        if FileIdentity::of(&open_regular(&path)?.metadata()?)? != FileIdentity::of(&file.metadata()?)? {
            return Err(invalid("publication predecessor has foreign retained identity"));
        }
        crate::sync_ordered(file)?;
        sync_directory(&self.root)
    }

    fn install(&self) -> PathBuf {
        self.root.join(format!("{}.install", self.key))
    }

    fn receipt(&self, operation_id: &str) -> PathBuf {
        self.root.join(format!("{}{}.receipt", self.receipt_prefix(), hash(operation_id.as_bytes())))
    }

    fn receipt_prefix(&self) -> String { format!("{}-", hash(self.path.as_os_str().as_encoded_bytes())) }

    fn read_receipt(&self, operation_id: &str) -> io::Result<Option<PublicationReceipt>> {
        let receipt = history::find_file(&self.path, operation_id)?;
        if let Some(receipt) = &receipt {
            if receipt.path != self.path || receipt.operation_id != operation_id {
                return Err(invalid("publication receipt identity differs from its target"));
            }
            validate_hash(receipt.before_hash.as_deref())?;
            validate_hash(receipt.after_hash.as_deref())?;
        }
        Ok(receipt)
    }

    fn prior_receipt(&self, expected: &PublicationReceipt) -> io::Result<Option<PublicationReceipt>> {
        if let Some(receipt) = self.read_receipt(&expected.operation_id)? {
            ensure_same_request(&receipt, expected)?;
            return Ok(Some(receipt));
        }
        if let Some(receipt) = self.settle_pending(Some(&expected.operation_id))? {
            if receipt.outcome == PublicationOutcome::Uncertain && receipt.operation_id != expected.operation_id {
                return Err(invalid("prior publication is uncertain; the target cannot be superseded"));
            }
        }
        let receipt = self.read_receipt(&expected.operation_id)?;
        if let Some(receipt) = &receipt {
            ensure_same_request(receipt, expected)?;
        }
        Ok(receipt)
    }

    fn settle_pending(&self, resume: Option<&str>) -> io::Result<Option<PublicationReceipt>> {
        let Some(pending): Option<PendingPublication> = read_record(&self.pending())? else {
            return Ok(None);
        };
        let expected = &pending.receipt;
        if expected.path != self.path
            || expected.outcome != PublicationOutcome::NotApplied || expected.error.is_some()
        {
            return Err(invalid("publication intent has invalid target or state"));
        }
        validate_operation(&expected.operation_id)?;
        validate_hash(expected.before_hash.as_deref())?;
        validate_hash(expected.after_hash.as_deref())?;
        let removing = match (pending.schema_version, expected.after_hash.as_ref()) {
            (1 | 3, Some(_)) => false,
            (2 | 4, None) if expected.before_hash.is_some() && expected.bytes == 0
                && pending.before_identity.as_ref() == Some(&pending.stage_identity) => true,
            _ => return Err(invalid("publication intent has invalid change evidence")),
        };
        if let Some(receipt) = self.read_receipt(&expected.operation_id)? {
            ensure_same_request(&receipt, expected)?;
            if receipt.outcome == PublicationOutcome::Uncertain {
                return Ok(Some(receipt));
            }
            self.retire_pending(&pending)?;
            return Ok(Some(receipt));
        }
        let stage = self.stage(&expected.operation_id);
        let staged = current_file(&stage)?;
        let stage_identity = staged.as_ref().map(|file| FileIdentity::of(&file.metadata()?)).transpose()?;
        let stage_hash = staged.as_ref().map(|file| digest_file(file, expected.digest_algorithm)).transpose()?;
        if !removing && (stage_identity.as_ref() != Some(&pending.stage_identity)
            || stage_hash != expected.after_hash
            || staged.as_ref().unwrap().metadata()?.len() != expected.bytes)
        {
            return Err(invalid("publication intent lost its exact staged bytes"));
        }
        let current = current_file(&self.path)?;
        let current_identity = current.as_ref().map(|file| FileIdentity::of(&file.metadata()?)).transpose()?;
        let current_hash = current.as_ref().map(|file| digest_file(file, expected.digest_algorithm)).transpose()?;
        let predecessor = if pending.schema_version >= 3 && pending.before_identity.is_some() {
            current_file(&self.predecessor(&expected.operation_id))?
        } else { None };
        let retained_before = match &pending.before_identity {
            None => true,
            Some(identity) => match &predecessor {
                Some(file) => FileIdentity::of(&file.metadata()?)? == *identity
                    && Some(digest_file(file, expected.digest_algorithm)?) == expected.before_hash,
                None => false,
            },
        };
        let mut receipt = expected.clone();
        let applied = if removing {
            current.is_none() && stage_identity.as_ref() == Some(&pending.stage_identity)
                && stage_hash == expected.before_hash
        } else {
            current_identity.as_ref() == Some(&pending.stage_identity) && current_hash == expected.after_hash
        };
        if applied {
            crate::sync_ordered(staged.as_ref().unwrap())?;
            sync_directory(&self.root)?;
            sync_directory(self.path.parent().unwrap())?;
            receipt.outcome = PublicationOutcome::Applied;
        } else if retained_before && (!removing || staged.is_none())
            && current_identity == pending.before_identity && current_hash == expected.before_hash
        {
            if resume == Some(expected.operation_id.as_str()) {
                return Ok(None);
            }
            receipt.error = Some(if removing {
                "publisher stopped before removing the prepared file"
            } else { "publisher stopped before installing the prepared file" }.into());
        } else {
            receipt.outcome = PublicationOutcome::Uncertain;
            receipt.error = Some(if removing {
                "interrupted removal cannot be attributed to its exact tombstone"
            } else { "interrupted publication cannot be attributed to its prepared file" }.into());
        }
        write_record_once(&self.receipt(&receipt.operation_id), &receipt)?;
        if receipt.outcome == PublicationOutcome::Uncertain {
            return Ok(Some(receipt));
        }
        self.retire_pending(&pending)?;
        Ok(Some(receipt))
    }

    fn retire_pending(&self, pending: &PendingPublication) -> io::Result<()> {
        for path in [self.install(), self.stage(&pending.receipt.operation_id)] {
            if let Some(file) = current_file(&path)? {
                if FileIdentity::of(&file.metadata()?)? != pending.stage_identity {
                    return Err(invalid("publication cleanup found foreign file identity"));
                }
                std::fs::remove_file(path)?;
            }
        }
        if pending.schema_version >= 3 {
            let path = self.predecessor(&pending.receipt.operation_id);
            if let Some(file) = current_file(&path)? {
                if Some(FileIdentity::of(&file.metadata()?)?) != pending.before_identity {
                    return Err(invalid("publication cleanup found foreign predecessor identity"));
                }
                std::fs::remove_file(path)?;
            }
        }
        sync_directory(&self.root)?;
        remove_if_present(&self.pending())?;
        sync_directory(&self.root)
    }
}

pub fn publish(
    path: &Path,
    operation_id: &str,
    expected_before: Option<&str>,
    bytes: &[u8],
) -> io::Result<PublicationReceipt> {
    let guard = if expected_before.is_none() {
        WriteGuard::acquire_creating_operation(path, operation_id)?
    } else { WriteGuard::acquire_operation(path, false, operation_id)? };
    guard
        .publish(operation_id, expected_before, bytes, DigestAlgorithm::Sha256)
}

pub fn remove(path: &Path, operation_id: &str, expected_before: &str) -> io::Result<PublicationReceipt> {
    WriteGuard::acquire_operation(path, true, operation_id)?.remove(operation_id, expected_before, DigestAlgorithm::Sha256)
}

pub struct WriteGuard {
    target: Target,
    membership: Option<NamespaceGuard>,
    lock: Option<HeldFile>,
    current: Option<HeldFile>,
    may_create: bool,
    historical: Option<PublicationReceipt>,
}

impl WriteGuard {
    pub fn acquire(path: &Path, may_create: bool) -> io::Result<Self> {
        Self::acquire_inner(path, may_create, None, false)
    }

    pub fn acquire_operation(path: &Path, may_create: bool, operation_id: &str) -> io::Result<Self> {
        validate_operation(operation_id)?;
        Self::acquire_inner(path, may_create, Some(operation_id), false)
    }

    pub fn acquire_creating_operation(path: &Path, operation_id: &str) -> io::Result<Self> {
        validate_operation(operation_id)?;
        Self::acquire_inner(path, true, Some(operation_id), true)
    }

    fn acquire_inner(path: &Path, may_create: bool, operation_id: Option<&str>, create_parents: bool) -> io::Result<Self> {
        namespace::validate_absolute(path)?;
        if path.file_name().is_none() || path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY) {
            return Err(invalid("publication requires an ordinary absolute file path"));
        }
        if let Some(receipt) = operation_id.map(|id| history::find_file(path, id)).transpose()?.flatten() {
            return Ok(Self::from_history(receipt, may_create));
        }
        let original_path = path.to_path_buf();
        let path = namespace::normalize_entry(path)?;
        if path.file_name().is_none() || path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY) {
            return Err(invalid("publication requires an ordinary absolute file path"));
        }
        let parent = path.parent().ok_or_else(|| invalid("publication target has no parent"))?;
        let membership = NamespaceGuard::acquire(&[(parent.to_path_buf(), may_create), (original_path.parent().unwrap().to_path_buf(), false)], true)?;
        if let Some(receipt) = operation_id.map(|id| history::find_file(&path, id)).transpose()?.flatten() {
            drop(membership);
            return Ok(Self::from_history(receipt, may_create));
        }
        directory::check_fences(&[original_path, path.clone()], None, true)?;
        if !parent.try_exists()? {
            if !create_parents { return Err(io::Error::new(io::ErrorKind::NotFound, "publication parent is absent; operation-aware creation is required")); }
            namespace::create_missing(parent)?;
            drop(membership);
            return Self::acquire_inner(&path, may_create, operation_id, false);
        }
        let target = Target::open(&path, true)?;
        let lock = target.lock(true, true)?;
        let current = match HeldFile::existing(target.path.clone(), true, false, true) {
            Ok(held) => Some(held),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        Ok(Self { target, membership: Some(membership), lock: Some(lock), current, may_create, historical: None })
    }

    fn from_history(receipt: PublicationReceipt, may_create: bool) -> Self {
        let target = Target::coordinates(receipt.path.clone());
        Self { target, membership: None, lock: None, current: None, may_create, historical: Some(receipt) }
    }

    fn prior_receipt(&self, expected: &PublicationReceipt) -> io::Result<Option<PublicationReceipt>> {
        if let Some(receipt) = &self.historical {
            ensure_same_request(receipt, expected)?;
            return Ok(Some(receipt.clone()));
        }
        self.target.prior_receipt(expected)
    }

    pub fn reconcile(&self) -> io::Result<()> {
        if let Some(receipt) = &self.historical {
            return if receipt.outcome == PublicationOutcome::Uncertain {
                Err(invalid("historical publication remains uncertain"))
            } else { Ok(()) };
        }
        let receipt = self.target.settle_pending(None)?;
        if receipt.is_some_and(|receipt| receipt.outcome == PublicationOutcome::Uncertain) {
            return Err(invalid("publication remains uncertain; the target cannot be superseded"));
        }
        Ok(())
    }

    pub fn reconcile_operation(&self, operation_id: &str) -> io::Result<Option<PublicationReceipt>> {
        validate_operation(operation_id)?;
        let receipt = if let Some(receipt) = &self.historical {
            if receipt.operation_id != operation_id || receipt.path != self.target.path {
                return Err(invalid("historical guard cannot reconcile a different operation"));
            }
            Some(receipt.clone())
        } else { self.target.read_receipt(operation_id)? };
        if receipt.is_some() { return Ok(receipt); }
        let lock = self.lock.as_ref().ok_or_else(|| invalid("reconciliation requires target ownership"))?;
        let membership = self.membership.as_ref().ok_or_else(|| invalid("reconciliation requires namespace ownership"))?;
        let verify = || -> io::Result<()> {
            lock.verify()?;
            membership.verify()?;
            if let Some(current) = &self.current { current.verify()?; }
            Ok(())
        };
        verify()?;
        let settled = self.target.settle_pending(Some(operation_id))?;
        if settled.is_some_and(|receipt| receipt.outcome == PublicationOutcome::Uncertain && receipt.operation_id != operation_id) {
            return Err(invalid("prior publication is uncertain; the target cannot be superseded"));
        }
        verify()?;
        self.target.read_receipt(operation_id)
    }

    pub fn publish(&self, operation_id: &str, expected_before: Option<&str>, bytes: &[u8], algorithm: DigestAlgorithm) -> io::Result<PublicationReceipt> {
        self.publish_with_stage(operation_id, expected_before, bytes, algorithm, None)
    }

    pub fn publish_retained(&self, operation_id: &str, expected_before: Option<&str>, bytes: &[u8], algorithm: DigestAlgorithm, retained_stage: &Path) -> io::Result<PublicationReceipt> {
        self.publish_with_stage(operation_id, expected_before, bytes, algorithm, Some(retained_stage))
    }

    pub fn remove(&self, operation_id: &str, expected_before: &str, algorithm: DigestAlgorithm) -> io::Result<PublicationReceipt> {
        validate_operation(operation_id)?;
        validate_hash(Some(expected_before))?;
        if !self.may_create {
            return Err(invalid("publication removal requires directory membership ownership"));
        }
        let target = &self.target;
        let expected = PublicationReceipt {
            digest_algorithm: algorithm,
            operation_id: operation_id.into(),
            path: target.path.clone(),
            before_hash: Some(expected_before.into()),
            after_hash: None,
            bytes: 0,
            outcome: PublicationOutcome::NotApplied,
            error: None,
        };
        if let Some(receipt) = self.prior_receipt(&expected)? {
            return Ok(receipt);
        }
        let lock = self.lock.as_ref().ok_or_else(|| invalid("historical guard cannot publish a different operation"))?;
        let membership = self.membership.as_ref().ok_or_else(|| invalid("new publication has no namespace ownership"))?;
        let current = current_file(&target.path)?;
        let observed = current.as_ref().map(|file| digest_file(file, algorithm)).transpose()?;
        if observed.as_deref() != Some(expected_before) {
            let mut receipt = expected;
            receipt.error = Some(format!("publication predecessor changed at '{}'", target.path.display()));
            write_record_once(&target.receipt(operation_id), &receipt)?;
            return Ok(receipt);
        }
        let current = self.current.as_ref().ok_or_else(|| invalid("removal predecessor is not locked"))?;
        lock.verify()?;
        membership.verify()?;
        current.verify()?;
        let stage = target.stage(operation_id);
        if current_file(&stage)?.is_some() {
            return Err(invalid("removal tombstone already exists without a settled receipt"));
        }
        let identity = FileIdentity::of(&current.file.metadata()?)?;
        target.retain_predecessor(operation_id, &current.file)?;
        let pending = PendingPublication {
            schema_version: 4,
            receipt: expected,
            before_identity: Some(identity.clone()),
            stage_identity: identity,
        };
        crate::sync_ordered(&current.file)?;
        write_record_once(&target.pending(), &pending)?;
        #[cfg(test)]
        interrupt_at(Interruption::Prepared);
        lock.verify()?;
        membership.verify()?;
        current.verify()?;
        std::fs::rename(&target.path, &stage)?;
        crate::sync_ordered(&current.file)?;
        sync_directory(&target.root)?;
        sync_directory(target.path.parent().unwrap())?;
        #[cfg(test)]
        interrupt_at(Interruption::Installed);
        let receipt = target.settle_pending(None)?
            .ok_or_else(|| invalid("removed publication lost its intent"))?;
        lock.verify()?;
        membership.verify()?;
        Ok(receipt)
    }

    fn publish_with_stage(&self, operation_id: &str, expected_before: Option<&str>, bytes: &[u8], algorithm: DigestAlgorithm, retained_stage: Option<&Path>) -> io::Result<PublicationReceipt> {
    validate_operation(operation_id)?;
    validate_hash(expected_before)?;
    let Self { target, membership, lock, current: held_current, may_create, .. } = self;
    if expected_before.is_none() && !may_create {
        return Err(invalid("publication creation requires directory membership ownership"));
    }
    let expected = PublicationReceipt {
        digest_algorithm: algorithm,
        operation_id: operation_id.into(),
        path: target.path.clone(),
        before_hash: expected_before.map(str::to_string),
        after_hash: Some(algorithm.digest(bytes)),
        bytes: bytes.len() as u64,
        outcome: PublicationOutcome::NotApplied,
        error: None,
    };
    if let Some(receipt) = self.prior_receipt(&expected)? {
        return Ok(receipt);
    }
    let lock = lock.as_ref().ok_or_else(|| invalid("historical guard cannot publish a different operation"))?;
    let membership = membership.as_ref().ok_or_else(|| invalid("new publication has no namespace ownership"))?;
    let current = current_file(&target.path)?;
    let observed = current.as_ref().map(|file| digest_file(file, algorithm)).transpose()?;
    let retained = retained_stage.map(|path| {
        let file = open_regular(path)?;
        if file.metadata()?.len() != bytes.len() as u64
            || digest_file(&file, algorithm)? != algorithm.digest(bytes)
        {
            return Err(invalid("retained publication stage differs from the exact requested bytes"));
        }
        Ok(file)
    }).transpose()?;
    if observed.as_deref() != expected_before && retained.is_none() {
        let mut receipt = expected;
        receipt.error = Some(format!("publication predecessor changed at '{}'", target.path.display()));
        write_record_once(&target.receipt(operation_id), &receipt)?;
        return Ok(receipt);
    }
    let stage = target.stage(operation_id);
    let pending_path = target.pending();
    let prepared = if let Some(pending) = read_record::<PendingPublication>(&pending_path)? {
        ensure_same_request(&pending.receipt, &expected)?;
        open_regular(&stage)?
    } else {
        remove_if_present(&stage)?;
        let file = if let Some(retained_path) = retained_stage {
            std::fs::hard_link(retained_path, &stage)?;
            let file = open_regular(&stage)?;
            if FileIdentity::of(&file.metadata()?)? != FileIdentity::of(&retained.as_ref().unwrap().metadata()?)? {
                return Err(invalid("retained publication stage changed while being imported"));
            }
            file
        } else {
            let file = write_new_regular(&stage, bytes)?;
            if let Some(current) = &current {
                file.set_permissions(current.metadata()?.permissions())?;
            }
            file
        };
        crate::sync_ordered(&file)?;
        sync_directory(&target.root)?;
        if observed.as_deref() == expected_before {
            if let Some(current) = &current { target.retain_predecessor(operation_id, current)?; }
        }
        let pending = PendingPublication {
            schema_version: 3,
            receipt: expected.clone(),
            before_identity: if observed.as_deref() == expected_before {
                current.as_ref().map(|file| FileIdentity::of(&file.metadata()?)).transpose()?
            } else { None },
            stage_identity: FileIdentity::of(&file.metadata()?)?,
        };
        write_record_once(&pending_path, &pending)?;
        file
    };
    if retained.is_some() {
        if let Some(receipt) = target.settle_pending(Some(operation_id))? {
            return Ok(receipt);
        }
    }
    prepared.try_lock_exclusive()?;
    lock.verify()?;
    membership.verify()?;
    if let Some(current) = held_current { current.verify()?; }
    #[cfg(test)]
    interrupt_at(Interruption::Prepared);
    if expected_before.is_none() {
        std::fs::hard_link(&stage, &target.path)?;
    } else {
        let link = target.install();
        match std::fs::hard_link(&stage, &link) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if FileIdentity::of(&open_regular(&link)?.metadata()?)? != FileIdentity::of(&prepared.metadata()?)? {
                    return Err(invalid("publication install link has foreign file identity"));
                }
            }
            Err(error) => return Err(error),
        }
        sync_directory(&target.root)?;
        #[cfg(test)]
        interrupt_at(Interruption::Linked);
        std::fs::rename(&link, &target.path)?;
    }
    crate::sync_ordered(&prepared)?;
    sync_directory(target.path.parent().unwrap())?;
    #[cfg(test)]
    interrupt_at(Interruption::Installed);
    let receipt = target.settle_pending(None)?
        .ok_or_else(|| invalid("installed publication lost its intent"))?;
    lock.verify()?;
    membership.verify()?;
    Ok(receipt)
    }
}

pub struct ReadGuard {
    locks: Vec<HeldFile>,
    membership: NamespaceGuard,
    absent: BTreeSet<PathBuf>,
}

impl ReadGuard {
    pub fn try_acquire(paths: &[PathBuf], directories: &[PathBuf]) -> io::Result<Self> {
        let mut parents = BTreeSet::new();
        for path in paths {
            namespace::validate_absolute(path)?;
            if path.file_name().is_none() || path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY) {
                return Err(invalid("read ownership requires an ordinary absolute file path"));
            }
            parents.insert(path.parent().ok_or_else(|| invalid("read target has no parent"))?.to_path_buf());
        }
        for directory in directories {
            parents.insert(directory.clone());
        }
        let membership = NamespaceGuard::acquire(&parents.into_iter().map(|parent| (parent, false)).collect::<Vec<_>>(), false)?;
        let mut guarded = paths.to_vec();
        guarded.extend_from_slice(directories);
        let mut guard = Self { locks: Vec::new(), membership, absent: BTreeSet::new() };
        let mut targets = BTreeMap::new();
        for path in paths {
            match Target::open(path, false) {
                Ok(target) => { targets.insert(target.path.clone(), target); },
                Err(error) if error.kind() == io::ErrorKind::NotFound => guard.retain_missing_parent(path)?,
                Err(error) => return Err(error),
            }
        }
        directory::check_fences(&guarded, None, false)?;
        for target in targets.into_values() {
            let mut lock = match HeldFile::existing(target.path.clone(), false, false, false) {
                Ok(lock) => Some(lock),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            if target.pending().try_exists()? {
                let recovery = target.lock(true, false)?;
                if target.settle_pending(None)?.is_some_and(|receipt| receipt.outcome == PublicationOutcome::Uncertain) {
                    return Err(invalid("admission source has an uncertain file publication"));
                }
                drop(recovery);
                if let Some(lock) = lock.as_mut() {
                    lock.version = Some(FileVersion::of(&lock.file.metadata()?)?);
                }
                if target.pending().try_exists()? {
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "publication changed during recovery"));
                }
            }
            if let Some(lock) = lock { guard.locks.push(lock); }
            else { guard.absent.insert(target.path); }
        }
        guard.verify()?;
        Ok(guard)
    }

    pub fn verify(&self) -> io::Result<()> {
        self.membership.verify()?;
        for lock in &self.locks {
            lock.verify()?;
        }
        for path in &self.absent {
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
                Ok(_) => return Err(invalid("absent admission source appeared while held")),
            }
        }
        Ok(())
    }

    fn retain_missing_parent(&mut self, path: &Path) -> io::Result<()> {
        let mut absent = path;
        let mut parent = path.parent().ok_or_else(|| invalid("absent read target has no parent"))?;
        loop {
            match std::fs::symlink_metadata(parent) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    absent = parent;
                    parent = parent.parent().ok_or_else(|| invalid("absent read target has no existing parent"))?;
                },
                Err(error) => return Err(error),
                Ok(metadata) => {
                    if !metadata.is_dir() { return Err(invalid("absent read target parent is not a real directory")); }
                    native::pinned_directory(parent, self.membership.identity(parent)?)?;
                    self.absent.insert(absent.to_path_buf());
                    self.absent.insert(path.to_path_buf());
                    return Ok(());
                },
            }
        }
    }
}

pub fn create_dir_all(path: &Path) -> io::Result<()> {
    let original = path.to_path_buf();
    let path = namespace::normalize(path)?;
    let guard = NamespaceGuard::acquire(&[(path.clone(), true), (original.clone(), false)], true)?;
    directory::check_fences(&[original, path.clone()], None, true)?;
    namespace::create_missing(&path)?;
    guard.verify()
}

#[cfg(test)]
fn directory_lock(directory: &Path, exclusive: bool, wait: bool) -> io::Result<HeldFile> {
    HeldFile::existing(directory.to_path_buf(), exclusive, true, wait)
}

fn coordination_root(parent: &Path) -> io::Result<PathBuf> {
    let root = parent.join(COORDINATION_DIRECTORY);
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&root) {
        Ok(()) => {
            sync_directory(parent)?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    validate_coordination_root(&root)?;
    Ok(root)
}

fn validate_coordination_root(root: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Err(invalid("publication metadata directory is not a real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid("publication metadata directory must be private to its owner"));
        }
    }
    Ok(())
}

fn current_file(path: &Path) -> io::Result<Option<File>> {
    match open_regular(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn secure_options(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
}

fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    secure_options(&mut options);
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("publication file is not regular"));
    }
    Ok(file)
}

fn write_new_regular(path: &Path, bytes: &[u8]) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    secure_options(&mut options);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    Ok(file)
}

fn read_record<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    read_record_bounded(path, RECORD_LIMIT)
}

fn read_record_bounded<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> io::Result<Option<T>> {
    let Some(file) = current_file(path)? else { return Ok(None); };
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("publication metadata exceeds its declared bound"));
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|error| invalid(error.to_string()))
}

fn write_record_once<T: Serialize>(path: &Path, record: &T) -> io::Result<()> {
    write_record_once_bounded(path, record, RECORD_LIMIT)
}

fn write_record_once_bounded<T: Serialize>(path: &Path, record: &T, limit: u64) -> io::Result<()> {
    let bytes = serde_json::to_vec(record).map_err(|error| invalid(error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(invalid("publication metadata exceeds its declared bound"));
    }
    if let Some(file) = current_file(path)? {
        if digest_file(&file, DigestAlgorithm::Sha256)? != hash(&bytes) {
            return Err(invalid("publication evidence already contains a different record"));
        }
        crate::sync_ordered(&file)?;
        return sync_directory(path.parent().unwrap());
    }
    let mut temporary_name = record_temporary_prefix(path);
    temporary_name.push(format!("{}.{}.tmp", std::process::id(), NEXT_TEMP.fetch_add(1, Ordering::Relaxed)));
    let temporary = path.with_file_name(temporary_name);
    let file = write_new_regular(&temporary, &bytes)?;
    crate::sync_ordered(&file)?;
    let installed = std::fs::hard_link(&temporary, path);
    let cleanup = std::fs::remove_file(&temporary);
    installed?;
    cleanup?;
    sync_directory(path.parent().unwrap())
}

fn digest_file(mut file: &File, algorithm: DigestAlgorithm) -> io::Result<String> {
    use std::io::Seek;
    file.rewind()?;
    let mut digest = Sha256::new();
    let mut blake = blake3::Hasher::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        match algorithm {
            DigestAlgorithm::Sha256 => { digest.update(&buffer[..count]); },
            DigestAlgorithm::Blake3 => { blake.update(&buffer[..count]); },
        }
    }
    Ok(match algorithm {
        DigestAlgorithm::Sha256 => format!("{:x}", digest.finalize()),
        DigestAlgorithm::Blake3 => blake3::Hasher::finalize(&blake).to_hex().to_string(),
    })
}

fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn invalid(message: impl Into<String>) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message.into()) }
fn sync_directory(path: &Path) -> io::Result<()> { crate::sync_directory_ordered(path) }
fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn temporary_stem(name: &str) -> Option<&str> {
    let mut parts = name.strip_suffix(".tmp")?.rsplitn(3, '.');
    parts.next()?.parse::<u64>().ok()?;
    parts.next()?.parse::<u32>().ok()?;
    parts.next()
}

fn record_temporary_prefix(path: &Path) -> std::ffi::OsString {
    let mut prefix = path.file_stem().unwrap().to_os_string();
    prefix.push(".");
    prefix
}

fn validate_operation(operation: &str) -> io::Result<()> {
    if operation.is_empty() || operation.len() > 256 || operation.chars().any(char::is_control) {
        return Err(invalid("publication operation identity must be nonempty bounded text"));
    }
    Ok(())
}

fn validate_hash(value: Option<&str>) -> io::Result<()> {
    if value.is_some_and(|value| value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))) {
        return Err(invalid("publication digest must be 64 lowercase hexadecimal characters or absent"));
    }
    Ok(())
}

fn ensure_same_request(left: &PublicationReceipt, right: &PublicationReceipt) -> io::Result<()> {
    if left.digest_algorithm != right.digest_algorithm || left.operation_id != right.operation_id || left.path != right.path
        || left.before_hash != right.before_hash || left.after_hash != right.after_hash || left.bytes != right.bytes
    {
        return Err(invalid("publication identity was reused for a different file change"));
    }
    Ok(())
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Interruption { Prepared, Linked, Installed, DirectoryPrepared(usize), DirectoryInstalled(usize), Reclaim(usize) }
#[cfg(test)]
thread_local! { static INTERRUPTION: std::cell::Cell<Option<Interruption>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
fn interrupt_at(point: Interruption) {
    INTERRUPTION.with(|selected| {
        if selected.get() == Some(point) {
            selected.set(None);
            panic!("interrupted file publication");
        }
    });
}

#[cfg(all(test, unix))]
#[path = "../tests/support/publication.rs"]
mod tests;
