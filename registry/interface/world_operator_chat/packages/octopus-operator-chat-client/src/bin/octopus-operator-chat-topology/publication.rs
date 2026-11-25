use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};

use octopus_file_storage::publication::{self, DigestAlgorithm, PublicationOutcome, PublicationReceipt, ReadGuard};
use octopus_interface_transport::runtime_instance::RuntimeInstanceLock;
use sha2::{Digest, Sha256};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FilePublicationRequest {
    writes: Vec<FilePublicationWrite>,
    condition: Option<PublicationCondition>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePublicationWrite {
    path: PathBuf,
    operation_id: String,
    expected_before_hash: serde_json::Value,
    content: serde_json::Value,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationCondition {
    octopus_home: PathBuf,
    preimages: Vec<Preimage>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Preimage {
    path: PathBuf,
    sha256: serde_json::Value,
}

#[derive(serde::Serialize)]
pub(super) struct FilePublicationResult {
    receipts: Vec<PublicationReceipt>,
    error: Option<String>,
}

fn valid_hash(value: &serde_json::Value) -> bool {
    value.is_null() || value.as_str().is_some_and(|text| text.len() == 64
        && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
}

fn normal_absolute(path: &Path) -> bool {
    path.is_absolute() && path.components().all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
        && !path.as_os_str().as_encoded_bytes().contains(&0)
        && !path.components().any(|part| part.as_os_str() == publication::COORDINATION_DIRECTORY)
}

fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

fn same_request(receipt: &PublicationReceipt, write: &FilePublicationWrite) -> bool {
    receipt.digest_algorithm == DigestAlgorithm::Sha256 && receipt.path == write.path
        && receipt.operation_id == write.operation_id && receipt.before_hash.as_deref() == write.expected_before_hash.as_str()
        && receipt.after_hash == write.content.as_str().map(|content| hash(content.as_bytes()))
        && receipt.bytes == write.content.as_str().map_or(0, |content| content.len() as u64)
        && (receipt.outcome != PublicationOutcome::Applied || receipt.error.is_none())
}

fn history_prefix(request: &FilePublicationRequest, result: &mut FilePublicationResult) -> io::Result<()> {
    while let Some(write) = request.writes.get(result.receipts.len()) {
        let Some(receipt) = publication::read_receipt(&write.path, &write.operation_id)? else { break; };
        if !same_request(&receipt, write) { return Err(invalid("original file receipt belongs to another submitted operation")); }
        let applied = receipt.outcome == PublicationOutcome::Applied;
        result.receipts.push(receipt);
        if !applied { break; }
    }
    Ok(())
}

struct ConditionOwnership {
    runtime: RuntimeInstanceLock,
    dependencies: ReadGuard,
}

impl ConditionOwnership {
    fn acquire(runtime: RuntimeInstanceLock, condition: &PublicationCondition, writes: &[FilePublicationWrite]) -> io::Result<Self> {
        let targets = writes.iter().map(|write| write.path.clone()).collect::<BTreeSet<_>>();
        let paths = condition.preimages.iter().filter(|pin| !targets.contains(&pin.path))
            .map(|pin| pin.path.clone()).collect::<Vec<_>>();
        let dependencies = ReadGuard::try_acquire(&paths, &[])?;
        let owner = Self { runtime, dependencies };
        let target_ids = writes.iter().filter_map(|write| match identity(&write.path) {
            Ok(Some(identity)) => Some(Ok(identity)), Ok(None) => None, Err(error) => Some(Err(error)),
        }).collect::<io::Result<BTreeSet<_>>>()?;
        for path in &paths {
            if identity(path)?.is_some_and(|identity| target_ids.contains(&identity)) {
                return Err(invalid("condition dependency aliases a write target inode"));
            }
        }
        for pin in &condition.preimages {
            if targets.contains(&pin.path) { continue; }
            let actual = match std::fs::read(&pin.path) {
                Ok(bytes) => Some(hash(&bytes)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            if actual.as_deref() != pin.sha256.as_str() {
                return Err(invalid(&format!("publication dependency changed at '{}'", pin.path.display())));
            }
        }
        owner.verify()?;
        Ok(owner)
    }

    fn verify(&self) -> io::Result<()> {
        self.runtime.verify()?;
        self.dependencies.verify()
    }
}

fn identity(path: &Path) -> io::Result<Option<(u64, u64)>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Err(invalid("conditioned publication requires regular files or absent dependency pins")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Some((metadata.dev(), metadata.ino())))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(io::Error::new(io::ErrorKind::Unsupported, "conditioned publication requires native file identity"))
    }
}

pub(super) fn publish_files(request: FilePublicationRequest) -> Result<FilePublicationResult, String> {
    if request.writes.len() > super::PUBLICATION_MAX_WRITES { return Err("file publication exceeds its write bound".into()); }
    let mut targets = BTreeSet::new();
    for write in &request.writes {
        if !write.path.is_absolute() || write.path.file_name().is_none()
            || write.path.as_os_str().as_encoded_bytes().contains(&0) || write.operation_id.is_empty()
            || write.operation_id.trim() != write.operation_id || write.operation_id.len() > 256
            || write.operation_id.chars().any(char::is_control) || !valid_hash(&write.expected_before_hash)
            || !(write.content.is_string() || write.content.is_null())
            || write.content.is_null() && !write.expected_before_hash.is_string()
        { return Err("file publication requires an absolute target, operation id, text or null removal, and SHA-256 preimage (nonnull for removal)".into()); }
        if request.condition.is_some() && (!normal_absolute(&write.path) || !write.content.is_string()
            || !write.expected_before_hash.is_string() || !targets.insert(write.path.clone())) {
            return Err("conditioned publication supports distinct existing-file updates only".into());
        }
    }
    if let Some(condition) = &request.condition {
        if !normal_absolute(&condition.octopus_home) || condition.preimages.len() > 4096 {
            return Err("publication condition requires a normal absolute Home and bounded preimages".into());
        }
        let mut seen = BTreeSet::new();
        for pin in &condition.preimages {
            if !normal_absolute(&pin.path) || !valid_hash(&pin.sha256) || !seen.insert(&pin.path)
                || request.writes.iter().any(|write| write.path == pin.path && write.expected_before_hash != pin.sha256) {
                return Err("publication condition has invalid, duplicated or mismatched preimages".into());
            }
        }
    }
    let mut result = FilePublicationResult { receipts: Vec::new(), error: None };
    let effect = (|| -> io::Result<()> {
        if request.condition.is_some() {
            history_prefix(&request, &mut result)?;
            if result.receipts.len() == request.writes.len()
                || result.receipts.last().is_some_and(|receipt| receipt.outcome != PublicationOutcome::Applied) { return Ok(()); }
        }
        let ownership = if let Some(condition) = &request.condition {
            let runtime = RuntimeInstanceLock::try_acquire(&condition.octopus_home)?;
            history_prefix(&request, &mut result)?;
            if result.receipts.len() == request.writes.len()
                || result.receipts.last().is_some_and(|receipt| receipt.outcome != PublicationOutcome::Applied) { return Ok(()); }
            Some(ConditionOwnership::acquire(runtime, condition, &request.writes)?)
        } else { None };
        let remaining = &request.writes[result.receipts.len()..];
        if ownership.is_none() && remaining.len() > 1 && remaining.iter().all(|write| write.content.is_string()) {
            let batch: Vec<publication::BatchPublish<'_>> = remaining.iter().map(|write| publication::BatchPublish {
                path: &write.path,
                operation_id: &write.operation_id,
                expected_before: write.expected_before_hash.as_str(),
                bytes: write.content.as_str().unwrap().as_bytes(),
                algorithm: DigestAlgorithm::Sha256,
                retained_stage: None,
            }).collect();
            for receipt in publication::publish_batch(&batch)? {
                let sha256 = receipt.digest_algorithm == DigestAlgorithm::Sha256;
                result.receipts.push(receipt);
                if !sha256 { return Err(invalid("client file publication received a non-SHA-256 receipt")); }
            }
        } else {
            for write in remaining {
                if let Some(owner) = &ownership {
                    owner.verify()?;
                    let parent = write.path.parent().ok_or_else(|| invalid("conditioned publication target has no parent"))?;
                    if std::fs::canonicalize(parent)?.as_os_str() != parent.as_os_str() {
                        return Err(invalid("conditioned publication target parent must be canonical"));
                    }
                }
                let receipt = match write.content.as_str() {
                    Some(content) => publication::publish(&write.path, &write.operation_id, write.expected_before_hash.as_str(), content.as_bytes()),
                    None => publication::remove(&write.path, &write.operation_id, write.expected_before_hash.as_str().unwrap()),
                }?;
                let applied = receipt.outcome == PublicationOutcome::Applied;
                if request.condition.is_some() && !same_request(&receipt, write) { return Err(invalid("file publication returned a receipt for another submitted operation")); }
                let sha256 = receipt.digest_algorithm == DigestAlgorithm::Sha256;
                result.receipts.push(receipt);
                if !sha256 { return Err(invalid("client file publication received a non-SHA-256 receipt")); }
                if let Some(owner) = &ownership { owner.verify()?; }
                if !applied { break; }
            }
        }
        Ok(())
    })();
    if let Err(error) = effect { result.error = Some(format!("file publication failed: {error}")); }
    Ok(result)
}

fn invalid(message: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidData, message) }
