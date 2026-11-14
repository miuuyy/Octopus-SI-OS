use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_file_storage::publication::{self, DigestAlgorithm, PublicationOutcome, WriteGuard};
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_ledger::ContentHash;
use serde::Deserialize;

use super::file_write::{canonicalize_declared_path, configured_write_roots, ensure_room_kind_granted};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    deletes: Vec<DeleteEntry>,
    #[serde(default)]
    empty_directories: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteEntry {
    path: PathBuf,
    expected_before_hash: String,
}

/// POLICY.md §4: delete is an artifact verb. A file under this room's granted roots is
/// removed only under a formed grant that holds the verb, guarded by its exact content hash.
pub fn execute_file_delete(
    input: &InterfaceInvocationEnvelope,
    arguments: &serde_json::Value,
    room_kinds: &[String],
    root_slots: &BTreeMap<String, PathBuf>,
) -> Result<serde_json::Value, String> {
    ensure_room_kind_granted(input, room_kinds)?;
    let grant = input.grant().map_err(|error| error.to_string())?;
    if !grant.artifact_verbs.iter().any(|verb| verb == "delete") {
        return Err("file.delete is refused: the formed grant holds no delete artifact verb".into());
    }
    let roots = configured_write_roots(&root_slots.values().cloned().collect::<Vec<_>>())?;
    let args: DeleteArgs = serde_json::from_value(arguments.clone())
        .map_err(|error| format!("file.delete arguments are malformed: {error}"))?;
    if args.deletes.is_empty() && args.empty_directories.is_empty() {
        return Err("file.delete names nothing to delete".into());
    }
    let within = |path: &Path| -> Result<PathBuf, String> {
        let target = canonicalize_declared_path(path)?;
        if target.as_path() != path || !roots.iter().any(|root| target.is_within(root) && target.as_path() != root.as_path()) {
            return Err(format!("file.delete target '{}' is not an exact path inside this room's granted roots", path.display()));
        }
        Ok(target.as_path().to_path_buf())
    };
    let artifact = serde_json::to_value(&input.artifact).map_err(|error| error.to_string())?;
    let identity = ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&artifact)).to_hex();
    let mut seen = BTreeSet::new();
    let mut targets = Vec::with_capacity(args.deletes.len());
    for entry in &args.deletes {
        let target = within(&entry.path)?;
        if ContentHash::from_hex(&entry.expected_before_hash).is_none_or(|hash| hash.to_hex() != entry.expected_before_hash) {
            return Err(format!("file.delete expected_before_hash for '{}' is not a canonical content hash", entry.path.display()));
        }
        if !seen.insert(target.clone()) {
            return Err(format!("file.delete repeats '{}'", entry.path.display()));
        }
        targets.push(target);
    }
    let directories = args.empty_directories.iter().map(|path| within(path)).collect::<Result<Vec<_>, _>>()?;
    let mut deleted = Vec::with_capacity(targets.len());
    for (index, (target, entry)) in targets.iter().zip(&args.deletes).enumerate() {
        if directories.iter().any(|directory| target.starts_with(directory) && !directory.exists()) {
            deleted.push(serde_json::json!({"path": target, "before_hash": entry.expected_before_hash}));
            continue;
        }
        let operation = format!("file.delete:{identity}:{index}");
        let receipt = WriteGuard::acquire_operation(target, true, &operation)
            .and_then(|guard| guard.remove(&operation, &entry.expected_before_hash, DigestAlgorithm::Blake3))
            .map_err(|error| format!("file.delete of '{}' failed: {error}", target.display()))?;
        if receipt.outcome != PublicationOutcome::Applied || receipt.operation_id != operation || receipt.path != *target {
            return Err(format!(
                "file.delete of '{}' was not applied: {}",
                target.display(),
                receipt.error.unwrap_or_else(|| "its removal receipt differs from this operation".into())
            ));
        }
        deleted.push(serde_json::json!({"path": target, "before_hash": entry.expected_before_hash}));
    }
    let mut removed = Vec::with_capacity(directories.len());
    for directory in directories {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => { removed.push(directory); continue; }
            Err(error) => return Err(format!("file.delete cannot read directory '{}': {error}", directory.display())),
        };
        if let Some(entry) = entries.iter().find(|entry| entry.file_name() != publication::COORDINATION_DIRECTORY) {
            return Err(format!("file.delete directory '{}' still holds '{}'", directory.display(), entry.file_name().to_string_lossy()));
        }
        if !entries.is_empty() {
            std::fs::remove_dir_all(directory.join(publication::COORDINATION_DIRECTORY))
                .map_err(|error| format!("file.delete cannot clear '{}': {error}", directory.display()))?;
        }
        std::fs::remove_dir(&directory).map_err(|error| format!("file.delete cannot remove directory '{}': {error}", directory.display()))?;
        removed.push(directory);
    }
    Ok(serde_json::json!({"result": "files_deleted", "deletes": deleted, "removed_directories": removed}))
}

#[cfg(test)]
mod tests {
    #[test]
    fn delete_arguments_are_exact() {
        assert!(serde_json::from_value::<super::DeleteArgs>(serde_json::json!({"deletes":[{"path":"/a","expected_before_hash":"00"}],"force":true})).is_err());
        assert!(serde_json::from_value::<super::DeleteArgs>(serde_json::json!({"deletes":[{"path":"/a","expected_before_hash":"00"}]})).is_ok());
    }
}
