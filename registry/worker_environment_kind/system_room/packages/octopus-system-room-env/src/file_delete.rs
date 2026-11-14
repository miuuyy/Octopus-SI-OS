use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_file_storage::publication::{self, DigestAlgorithm, PublicationOutcome, WriteGuard};
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_ledger::ContentHash;
use serde::Deserialize;

use super::file_write::{canonicalize_declared_path, configured_write_roots, ensure_room_kind_granted, install_effect_record, lock_invocation};

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
    effect_root: &Path,
    max_payload_bytes: u64,
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
    if !effect_root.is_absolute() { return Err("file.delete effect root must be absolute".into()); }
    let retained = effect_root.join("delete").join(&identity);
    let _invocation_lock = lock_invocation(&retained)?;
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
        if target.starts_with(&retained) { return Err("file.delete cannot remove its own retained evidence".into()); }
        targets.push(target);
    }
    let directories = args.empty_directories.iter().map(|path| within(path)).collect::<Result<Vec<_>, _>>()?;
    for (index, directory) in directories.iter().enumerate() {
        if !seen.insert(directory.clone()) || directories[..index].iter().any(|prior| directory.starts_with(prior)) {
            return Err("file.delete directories must be distinct, deepest first, and separate from file targets".into());
        }
        if retained.starts_with(directory) || directory.starts_with(&retained) { return Err("file.delete cannot remove its own retained evidence".into()); }
    }
    let mut deleted = Vec::with_capacity(targets.len());
    for (index, (target, entry)) in targets.iter().zip(&args.deletes).enumerate() {
        let operation = format!("file.delete:{identity}:{index}");
        let receipt = WriteGuard::acquire_operation(target, true, &operation)
            .and_then(|guard| guard.remove(&operation, &entry.expected_before_hash, DigestAlgorithm::Blake3))
            .map_err(|error| format!("file.delete of '{}' failed: {error}", target.display()))?;
        if receipt.outcome != PublicationOutcome::Applied || receipt.operation_id != operation || receipt.path != *target
            || receipt.digest_algorithm != DigestAlgorithm::Blake3 || receipt.before_hash.as_deref() != Some(&entry.expected_before_hash)
            || receipt.after_hash.is_some() || receipt.bytes != 0 || receipt.error.is_some() {
            return Err(format!(
                "file.delete of '{}' was not applied: {}",
                target.display(),
                receipt.error.unwrap_or_else(|| "its removal receipt differs from this operation".into())
            ));
        }
        deleted.push(serde_json::json!({"path": target, "before_hash": entry.expected_before_hash}));
    }
    let mut removed = Vec::with_capacity(directories.len());
    for (index, directory) in directories.into_iter().enumerate() {
        let plan_path = retained.join(format!("directory-{index}.json"));
        let destination = retained.join(format!("directory-{index}"));
        let movement: publication::DirectoryMove = match octopus_interfaces::read_artifact_carrier_file_bounded(&plan_path, max_payload_bytes) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| format!("file.delete retained directory plan is malformed: {error}"))?,
            Err(octopus_interfaces::InterfaceArtifactFileError::ReadPayload { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                let expected = publication::snapshot_directory(&directory, DigestAlgorithm::Blake3)
                    .map_err(|error| format!("file.delete cannot observe directory '{}': {error}", directory.display()))?;
                if !expected.entries.is_empty() {
                    return Err(format!("file.delete directory '{}' is not empty", directory.display()));
                }
                let movement = publication::DirectoryMove { source: directory.clone(), destination: destination.clone(), expected };
                let bytes = serde_json::to_vec(&movement).map_err(|error| error.to_string())?;
                if bytes.len() as u64 > max_payload_bytes { return Err("file.delete directory plan exceeds its payload limit".into()); }
                install_effect_record(&plan_path, &bytes)?;
                movement
            }
            Err(error) => return Err(format!("file.delete cannot read its retained directory plan: {error}")),
        };
        if movement.source != directory || movement.destination != destination
            || movement.expected.digest_algorithm != DigestAlgorithm::Blake3 || !movement.expected.entries.is_empty() {
            return Err("file.delete retained directory plan differs from its exact empty directory".into());
        }
        let operation = format!("file.delete:{identity}:directory:{index}");
        let published = publication::publish_directory_moves(&operation, std::slice::from_ref(&movement))
            .map_err(|error| format!("file.delete directory publication failed: {error}"))?;
        let [receipt] = published.receipts.as_slice() else { return Err("file.delete directory has no exact publication receipt".into()); };
        if published.error.is_some() || receipt.outcome != PublicationOutcome::Applied || receipt.operation_id != operation
            || receipt.index != 0 || receipt.movement != movement || receipt.error.is_some() {
            return Err(format!("file.delete directory '{}' was not removed: {}", directory.display(),
                published.error.or_else(|| receipt.error.clone()).unwrap_or_else(|| "publication receipt differs".into())));
        }
        publication::reclaim_directory(&destination, receipt, &movement.expected)
            .map_err(|error| format!("file.delete directory reclamation failed: {error}"))?;
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
