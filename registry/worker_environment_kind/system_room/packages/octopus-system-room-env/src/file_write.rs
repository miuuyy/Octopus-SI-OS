use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fs2::FileExt;
use octopus_file_storage::publication::{self, DigestAlgorithm, PublicationOutcome, PublicationReceipt, WriteGuard};
use octopus_file_write_contract::{DecodeBudget, JsonFormat, Preimage};
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_worker_env::AbsPath;

#[path = "file_write/ordered.rs"]
mod ordered;

pub fn execute_file_write_result(
    input: &InterfaceInvocationEnvelope,
    arguments: &serde_json::Value,
    room_kinds: &[String],
    root_slots: &BTreeMap<String, PathBuf>,
    effect_root: &Path,
    max_payload_bytes: u64,
) -> Result<serde_json::Value, octopus_tool_handlers::ToolResultError> {
    if arguments.get("ordered").is_some() {
        ordered::execute(input, arguments, room_kinds, root_slots, effect_root, max_payload_bytes)
    } else {
        execute_file_write(input, arguments, room_kinds, root_slots, effect_root, max_payload_bytes)
            .map_err(|error| octopus_tool_handlers::ToolResultError::new("tool_execution_failed", error))
    }
}

pub fn execute_file_write(
    input: &InterfaceInvocationEnvelope,
    arguments: &serde_json::Value,
    room_kinds: &[String],
    root_slots: &BTreeMap<String, PathBuf>,
    effect_root: &Path,
    max_payload_bytes: u64,
) -> Result<serde_json::Value, String> {
    ensure_room_kind_granted(input, room_kinds)?;
    let roots = configured_write_roots(&root_slots.values().cloned().collect::<Vec<_>>())?;
    if !effect_root.is_absolute() {
        return Err("file.write effect root must be absolute".into());
    }
    let directory = effect_root.join("ordinary").join(input.artifact.content.to_hex());
    let _invocation_lock = lock_invocation(&directory)?;
    let effect_path = effect_root.join(format!("{}.json", input.artifact.content.to_hex()));
    let recorded = read_effect_record(&effect_path)?;
    if let Some(record) = &recorded {
        if record.schema_version == 1 {
            return Err("file.write retained legacy intent has no attributable publication receipt; its effect is uncertain and cannot be replayed".into());
        }
        if record.schema_version != 2 {
            return Err(format!("file.write effect record has unsupported schema_version {}", record.schema_version));
        }
        if record.artifact.as_ref() != Some(&input.artifact)
            || record.artifact_id != input.artifact.id.as_str()
        {
            return Err("file.write effect record differs from its original source artifact".into());
        }
    }
    let requests = parse_write_requests(arguments, &roots, max_payload_bytes, recorded.as_ref(), effect_root)?;
    retain_write_inputs(effect_root, &requests)?;
    bind_effect_record(input, arguments, effect_root, &requests)?;
    let mut evidence = Vec::with_capacity(requests.len());
    for (index, request) in requests.iter().enumerate() {
        let operation = publication_operation_id(input, false, index)?;
        let publication = {
            let guard = acquire_publication(&request.target, request.before_hash.as_deref(), &roots, &operation)?;
            guard.publish(&operation, request.before_hash.as_deref(), request.content.as_bytes(), DigestAlgorithm::Blake3)
                .map_err(|error| format!("file.write publication at '{}' requires reconciliation: {error}", request.target.display()))?
        };
        let after_hash = ContentHash::of_bytes(request.content.as_bytes()).to_hex();
        check_publication(&publication, &operation, &request.target, request.before_hash.as_deref(), &after_hash, request.content.len() as u64)?;
        if publication.outcome != PublicationOutcome::Applied {
            return Err(format!("file.write publication at '{}' is {:?}: {}", request.target.display(), publication.outcome,
                publication.error.as_deref().unwrap_or("durable publication could not be established")));
        }
        evidence.push(serde_json::json!({
            "path": request.target,
            "before_hash": request.before_hash,
            "after_hash": after_hash,
            "bytes": request.content.len(),
        }));
    }
    Ok(serde_json::json!({"result": "files_written", "writes": evidence}))
}

struct WriteRequest {
    target: PathBuf,
    content: String,
    before_hash: Option<String>,
    preimage_bytes: Option<Vec<u8>>,
}

pub(crate) fn lock_invocation(directory: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let lock = options.open(directory.join("lock")).map_err(|error| error.to_string())?;
    if !lock.metadata().map_err(|error| error.to_string())?.is_file() {
        return Err("file.write invocation lock is not a regular file".into());
    }
    lock.try_lock_exclusive()
        .map_err(|error| format!("file.write exact artifact is already executing: {error}"))?;
    Ok(lock)
}

fn publication_operation_id(input: &InterfaceInvocationEnvelope, ordered: bool, index: usize) -> Result<String, String> {
    let artifact = serde_json::to_value(&input.artifact).map_err(|error| error.to_string())?;
    let identity = ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&artifact)).to_hex();
    Ok(format!("file.write:{identity}:{}:{index}", if ordered { "ordered" } else { "ordinary" }))
}

fn acquire_publication(target: &Path, before_hash: Option<&str>, roots: &[AbsPath], operation: &str) -> Result<WriteGuard, String> {
    let guard = if before_hash.is_none() {
        WriteGuard::acquire_creating_operation(target, operation)
    } else {
        WriteGuard::acquire_operation(target, false, operation)
    }.map_err(|error| {
        format!("file.write publication ownership at '{}' failed: {error}", target.display())
    })?;
    let path = canonicalize_declared_path(target)?;
    if path.as_path() != target || !roots.iter().any(|root| path.is_within(root)) {
        return Err(format!("file.write target '{}' changed its granted root before publication", target.display()));
    }
    Ok(guard)
}

fn check_publication(receipt: &PublicationReceipt, operation: &str, path: &Path, before: Option<&str>, after: &str, bytes: u64) -> Result<(), String> {
    if receipt.digest_algorithm != DigestAlgorithm::Blake3
        || receipt.operation_id != operation || receipt.path != path
        || receipt.before_hash.as_deref() != before || receipt.after_hash.as_deref() != Some(after)
        || receipt.bytes != bytes || (receipt.outcome == PublicationOutcome::Applied && receipt.error.is_some())
    {
        return Err("file.write publication receipt differs from its immutable BLAKE3 operation".into());
    }
    Ok(())
}

fn optional_hash(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(ContentHash::of_bytes(&bytes).to_hex())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "file.write could not read target '{}' under its write lock: {error}",
            path.display()
        )),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWriteEffectRecord {
    schema_version: u32,
    artifact_id: String,
    writes: Vec<FileWriteEffectEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    arguments_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWriteEffectEntry {
    path: PathBuf,
    before_hash: Option<String>,
    after_hash: String,
}

fn parse_write_requests(
    arguments: &serde_json::Value,
    roots: &[AbsPath],
    max_payload_bytes: u64,
    recorded: Option<&FileWriteEffectRecord>,
    effect_root: &Path,
) -> Result<Vec<WriteRequest>, String> {
    let mut budget = DecodeBudget::new(max_payload_bytes)?;
    let values = match arguments.get("writes") {
        Some(serde_json::Value::Array(values)) if !values.is_empty() => values.clone(),
        Some(serde_json::Value::Array(_)) => {
            return Err("file.write 'writes' must not be empty".to_string());
        }
        Some(_) => return Err("file.write 'writes' must be an array".to_string()),
        None => vec![arguments.clone()],
    };
    let mut seen = BTreeSet::new();
    let mut requests = Vec::with_capacity(values.len());
    if recorded.is_some_and(|record| record.writes.len() != values.len()) {
        return Err("file.write retained write count differs from its immutable invocation".into());
    }
    for (index, value) in values.into_iter().enumerate() {
        let path = value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "file.write entry must include non-empty string 'path'".to_string())?;
        let requested = PathBuf::from(path);
        if !requested.is_absolute() {
            return Err(format!(
                "file.write path must be absolute (the path is the permission): {path}"
            ));
        }
        let declared = AbsPath::new(&requested)
            .map_err(|e| format!("file.write path failed closed before the root check: {e}"))?;
        let requested = canonicalize_declared_path(declared.as_path())?;
        if !roots.iter().any(|root| requested.is_within(root)) {
            return Err(format!(
                "file.write refused closed: path '{}' is outside this room's permission — the \
                 granted roots are [{}]",
                declared.as_str(),
                roots
                    .iter()
                    .map(AbsPath::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let target = requested.as_path().to_path_buf();
        if !seen.insert(target.clone()) {
            return Err(format!(
                "file.write batch repeats path '{}'",
                target.display()
            ));
        }
        let pinned = recorded.map(|record| &record.writes[index]);
        let observed_before_bytes = if let Some(pinned) = pinned {
            if pinned.path != target {
                return Err("file.write retained path differs from its immutable invocation".into());
            }
            pinned.before_hash.as_deref().map(|hash| read_preimage(effect_root, hash, max_payload_bytes)).transpose()?
        } else {
            match std::fs::File::open(&target) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(max_payload_bytes.checked_add(1).ok_or("file.write read limit overflow")?)
                        .read_to_end(&mut bytes).map_err(|error| error.to_string())?;
                    if bytes.len() as u64 > max_payload_bytes { return Err("file.write target exceeds host payload limit".into()); }
                    Some(bytes)
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(format!("file.write could not read existing target '{}': {error}", target.display())),
            }
        };
        if let Some(before) = &observed_before_bytes { budget.charge(before.len() as u64)?; }
        let format: Option<JsonFormat> = value.get("json_format").cloned()
            .map(serde_json::from_value).transpose().map_err(|error| error.to_string())?;
        if format.is_some() && value.get("patch").is_none() {
            return Err("file.write json_format belongs only to patch writes".into());
        }
        let observed_before_hash = observed_before_bytes
            .as_deref()
            .map(ContentHash::of_bytes)
            .map(|hash| hash.to_hex());
        let (content, evidence_before_hash) = match (
            value.get("content"),
            value.get("patch"),
        ) {
            (Some(_), Some(_)) => {
                return Err(
                    "file.write entry accepts exactly one of 'content' or 'patch'".to_string(),
                );
            }
            (None, None) => {
                return Err(
                    "file.write entry requires exactly one of 'content' or 'patch'".to_string(),
                );
            }
            (Some(serde_json::Value::String(content)), None) => {
                if value.get("expected_after_hash").is_some() {
                    return Err(
                        "file.write expected_after_hash belongs only to patch writes".to_string(),
                    );
                }
                let expected_before = match (
                    value.get("expected_before_hash"),
                    value.get("expected_before_content"),
                ) {
                    (None, None) => None,
                    (Some(_), Some(_)) => {
                        return Err(
                                "file.write accepts only one of expected_before_hash or expected_before_content"
                                    .to_string(),
                            );
                    }
                    (Some(serde_json::Value::Null), None) => {
                        Some((observed_before_bytes.is_none(), None))
                    }
                    (Some(serde_json::Value::String(expected)), None) => {
                        let expected =
                            parse_content_hash(expected, "expected_before_hash")?.to_string();
                        Some((
                            observed_before_hash.as_deref() == Some(expected.as_str()),
                            Some(expected),
                        ))
                    }
                    (Some(_), None) => {
                        return Err(
                                "file.write expected_before_hash must be a lowercase 64-character hash or null"
                                    .to_string(),
                            );
                    }
                    (None, Some(serde_json::Value::Null)) => {
                        Some((observed_before_bytes.is_none(), None))
                    }
                    (None, Some(serde_json::Value::String(expected))) => Some((
                        observed_before_bytes.as_deref() == Some(expected.as_bytes()),
                        Some(ContentHash::of_bytes(expected.as_bytes()).to_hex()),
                    )),
                    (None, Some(_)) => {
                        return Err(
                            "file.write expected_before_content must be a string or null"
                                .to_string(),
                        );
                    }
                };
                let evidence_before_hash = match expected_before.as_ref() {
                    Some((false, expected)) => {
                        return Err(format!(
                            "file.write optimistic hash mismatch for '{}': expected {}, observed {}",
                            target.display(),
                            expected.as_deref().unwrap_or("<absent>"),
                            observed_before_hash.as_deref().unwrap_or("<absent>")
                        ));
                    }
                    Some((true, expected)) => expected.clone(),
                    None => observed_before_hash.clone(),
                };
                (content.clone(), evidence_before_hash)
            }
            (Some(_), None) => {
                return Err("file.write entry 'content' must be a string".to_string());
            }
            (None, Some(patch_value)) => {
                if value.get("expected_before_content").is_some() {
                    return Err(
                        "file.write patch does not accept expected_before_content".to_string()
                    );
                }
                let expected_before = value
                    .get("expected_before_hash")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        "file.write patch requires string expected_before_hash".to_string()
                    })?;
                let expected_before =
                    parse_content_hash(expected_before, "expected_before_hash")?.to_string();
                let expected_after = match value.get("expected_after_hash") {
                    None => None,
                    Some(serde_json::Value::String(expected)) => {
                        Some(parse_content_hash(expected, "expected_after_hash")?.to_string())
                    }
                    Some(_) => {
                        return Err(
                            "file.write expected_after_hash must be a lowercase 64-character hash"
                                .to_string(),
                        );
                    }
                };
                let patch: json_patch::Patch = serde_json::from_value(patch_value.clone())
                    .map_err(|error| format!("file.write patch is malformed: {error}"))?;
                if patch.is_empty() {
                    return Err("file.write patch must not be empty".to_string());
                }
                if observed_before_hash.as_deref() != Some(expected_before.as_str()) {
                    return Err(format!(
                        "file.write patch optimistic hash mismatch for '{}': expected {}, observed {}",
                        target.display(), expected_before, observed_before_hash.as_deref().unwrap_or("<absent>")
                    ));
                }
                let before = observed_before_bytes.as_ref().ok_or_else(|| {
                    format!("file.write patch target '{}' does not exist", target.display())
                })?;
                let content = apply_checked_json_patch(&target, before, &patch, expected_after.as_deref(), format.unwrap_or_default())?;
                (content, Some(expected_before))
            }
        };
        budget.charge(content.len() as u64)?;
        requests.push(WriteRequest {
            target,
            content,
            before_hash: evidence_before_hash,
            preimage_bytes: observed_before_bytes,
        });
    }
    Ok(requests)
}

fn apply_checked_json_patch(
    target: &Path,
    before: &[u8],
    patch: &json_patch::Patch,
    expected_after: Option<&str>,
    format: JsonFormat,
) -> Result<String, String> {
    if let Some(expected_after) = expected_after {
        parse_content_hash(expected_after, "expected_after_hash")?;
    }
    if patch.is_empty() {
        return Err("file.write patch must not be empty".into());
    }
    let mut document: serde_json::Value = serde_json::from_slice(before)
        .map_err(|error| format!("file.write patch target '{}' is not JSON: {error}", target.display()))?;
    json_patch::patch(&mut document, patch)
        .map_err(|error| format!("file.write patch could not apply to '{}': {error}", target.display()))?;
    let bytes = format.encode(&document)
        .map_err(|error| format!("file.write patched JSON '{}' could not encode: {error}", target.display()))?;
    let observed_after = ContentHash::of_bytes(&bytes).to_hex();
    if let Some(expected_after) = expected_after.filter(|expected| *expected != observed_after) {
        return Err(format!("file.write patch for '{}' produced hash {}, expected {}", target.display(), observed_after, expected_after));
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("file.write patched JSON '{}' is not UTF-8: {error}", target.display()))
}

fn parse_content_hash<'a>(value: &'a str, field: &str) -> Result<&'a str, String> {
    if value.len() != 64
        || value.bytes().any(|byte| !byte.is_ascii_hexdigit())
        || value.bytes().any(|byte| byte.is_ascii_uppercase())
        || ContentHash::from_hex(value).is_none()
    {
        return Err(format!(
            "file.write {field} must be a lowercase 64-character content hash"
        ));
    }
    Ok(value)
}

fn bind_effect_record(
    input: &InterfaceInvocationEnvelope,
    arguments: &serde_json::Value,
    effect_root: &Path,
    requests: &[WriteRequest],
) -> Result<(), String> {
    let effect_path = effect_root.join(format!("{}.json", input.artifact.content.to_hex()));
    let record = FileWriteEffectRecord {
        schema_version: 2,
        artifact_id: input.artifact.id.as_str().to_string(),
        artifact: Some(input.artifact.clone()),
        arguments_hash: Some(ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(arguments)).to_hex()),
        writes: requests.iter().map(|request| FileWriteEffectEntry {
            path: request.target.clone(), before_hash: request.before_hash.clone(),
            after_hash: ContentHash::of_bytes(request.content.as_bytes()).to_hex(),
        }).collect(),
    };
    if let Some(retained) = read_effect_record(&effect_path)? {
        if retained != record {
            return Err("file.write effect record differs from its immutable invocation".into());
        }
    } else {
        let mut bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        install_effect_record(&effect_path, &bytes)?;
    }
    Ok(())
}

fn read_preimage(effect_root: &Path, hash: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
    parse_content_hash(hash, "recorded before_hash")?;
    let path = effect_root.join("blobs").join(hash);
    let mut bytes = Vec::new();
    std::fs::File::open(&path).map_err(|error| format!("file.write preimage '{}' is unavailable: {error}", path.display()))?
        .take(max_bytes.checked_add(1).ok_or("file.write read limit overflow")?)
        .read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > max_bytes || ContentHash::of_bytes(&bytes).to_hex() != hash {
        return Err(format!("file.write preimage '{}' differs from its pinned hash or exceeds the payload limit", path.display()));
    }
    Ok(bytes)
}

fn read_effect_record(path: &Path) -> Result<Option<FileWriteEffectRecord>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "file.write effect record '{}' cannot be read: {error}",
                path.display()
            ));
        }
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        format!(
            "file.write effect record '{}' is malformed: {error}",
            path.display()
        )
    })
}

pub(crate) fn install_effect_record(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let staging_root = path.parent().ok_or_else(|| {
        format!(
            "file.write effect record '{}' has no staging parent",
            path.display()
        )
    })?;
    let staged = stage_write(staging_root, path, bytes)?;
    let linked = std::fs::hard_link(&staged, path);
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => sync_parent(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = std::fs::read(path).map_err(|read_error| {
                format!(
                    "file.write concurrent effect record '{}' cannot be read: {read_error}",
                    path.display()
                )
            })?;
            if existing != bytes {
                return Err(format!(
                    "file.write concurrent effect record '{}' has different bytes",
                    path.display()
                ));
            }
            std::fs::File::open(path).and_then(|file| octopus_file_storage::sync_ordered(&file))
                .map_err(|error| format!("file.write existing effect record '{}' cannot be synchronized: {error}", path.display()))?;
            sync_parent(path)
        }
        Err(error) => Err(format!(
            "file.write effect record '{}' cannot be installed: {error}",
            path.display()
        )),
    }
}

fn ensure_room_kind_granted(
    input: &InterfaceInvocationEnvelope,
    room_kinds: &[String],
) -> Result<(), String> {
    if room_kinds.is_empty() {
        return Err(
            "file.write registry row names no supported_worker_environment_kinds — the tool \
             has no room to be granted through"
                .to_string(),
        );
    }
    let granted = invocation_granted_wek(input)?;
    if !room_kinds
        .iter()
        .any(|kind| granted.iter().any(|g| g == kind))
    {
        return Err(format!(
            "file.write refused closed: none of this tool's rooms [{}] is inside the formed \
             worker-environment grant [{}]",
            room_kinds.join(", "),
            granted.join(", ")
        ));
    }
    Ok(())
}

fn invocation_granted_wek(input: &InterfaceInvocationEnvelope) -> Result<Vec<String>, String> {
    Ok(input
        .worker_environment_kinds()
        .map_err(|e| e.to_string())?
        .to_vec())
}

fn configured_write_roots(values: &[PathBuf]) -> Result<Vec<AbsPath>, String> {
    let mut roots = Vec::with_capacity(values.len());
    let mut unresolved = Vec::new();
    for path in values {
        if !path.is_absolute() {
            return Err(format!(
                "file.write configured root must be absolute: {}",
                path.display()
            ));
        }
        match canonicalize_declared_path(path) {
            Ok(root) => roots.push(root),
            Err(error) => unresolved.push(format!("'{}': {error}", path.display())),
        }
    }
    if roots.is_empty() && !unresolved.is_empty() {
        return Err(format!(
            "file.write refused closed: no declared write root resolves ({})",
            unresolved.join("; ")
        ));
    }
    if roots.is_empty() {
        return Err(
            "file.write refused closed: configured write roots are empty — the room holds no \
             roots, so it holds no permission"
                .to_string(),
        );
    }
    Ok(roots)
}

fn retain_write_inputs(effect_root: &Path, requests: &[WriteRequest]) -> Result<(), String> {
    let blobs = effect_root.join("blobs");
    std::fs::create_dir_all(&blobs).map_err(|error| {
        format!(
            "file.write evidence root '{}' cannot be created: {error}",
            blobs.display()
        )
    })?;
    for request in requests {
        for bytes in request
            .preimage_bytes
            .as_deref()
            .into_iter()
            .chain(std::iter::once(request.content.as_bytes()))
        {
            let hash = ContentHash::of_bytes(bytes).to_hex();
            let path = blobs.join(hash);
            install_effect_record(&path, bytes)?;
            if std::fs::read(&path).map_err(|error| {
                format!(
                    "file.write evidence '{}' cannot be read: {error}",
                    path.display()
                )
            })? != bytes
            {
                return Err(format!(
                    "file.write evidence '{}' has inconsistent bytes",
                    path.display()
                ));
            }
        }
        if let Some(before_hash) = &request.before_hash {
            parse_content_hash(before_hash, "recorded before_hash")?;
            let path = blobs.join(before_hash);
            let bytes = std::fs::read(&path).map_err(|error| {
                format!(
                    "file.write preimage '{}' is unavailable: {error}",
                    path.display()
                )
            })?;
            if ContentHash::of_bytes(&bytes).to_hex() != *before_hash {
                return Err(format!(
                    "file.write preimage '{}' differs from its pinned hash",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn canonicalize_declared_path(path: &Path) -> Result<AbsPath, String> {
    let normalized = AbsPath::new(path)
        .map_err(|error| format!("path '{}' is invalid: {error}", path.display()))?;
    let mut existing = normalized.as_path().to_path_buf();
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(mut canonical) => {
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                if canonical.components().any(|component| component.as_os_str() == publication::COORDINATION_DIRECTORY) {
                    return Err("file.write publication metadata is not an ordinary file target".into());
                }
                return AbsPath::new(&canonical).map_err(|error| {
                    format!(
                        "canonical path '{}' is invalid: {error}",
                        canonical.display()
                    )
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = existing.file_name().ok_or_else(|| {
                    format!("path '{}' has no existing ancestor", normalized.as_str())
                })?;
                suffix.push(component.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| {
                        format!("path '{}' has no existing ancestor", normalized.as_str())
                    })?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(format!(
                    "path '{}' cannot be canonicalized: {error}",
                    existing.display()
                ));
            }
        }
    }
}

pub(crate) fn stage_write(
    staging_root: &std::path::Path,
    target: &std::path::Path,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "file.write path '{}' has no file name to stage beside",
                target.display()
            )
        })?;
    for _ in 0..128 {
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        let staged = staging_root.join(format!(
            ".{name}.octopus-file-write-{}-{nonce}.tmp",
            std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "file.write could not create staging file '{}': {error}",
                    staged.display()
                ));
            }
        };
        if let Err(error) = file.write_all(bytes).and_then(|_| octopus_file_storage::sync_ordered(&file)) {
            let _ = std::fs::remove_file(&staged);
            return Err(format!(
                "file.write could not durably stage '{}': {error}",
                staged.display()
            ));
        }
        return Ok(staged);
    }
    Err(format!(
        "file.write could not reserve a staging file beside '{}'",
        target.display()
    ))
}

pub(crate) fn sync_parent(target: &std::path::Path) -> Result<(), String> {
    let parent = target.parent().ok_or_else(|| {
        format!(
            "file.write target '{}' has no parent to synchronize",
            target.display()
        )
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| octopus_file_storage::sync_ordered(&directory))
        .map_err(|error| {
            format!(
                "file.write could not synchronize parent directory '{}': {error}",
                parent.display()
            )
        })
}

#[cfg(all(test, unix))]
mod path_tests {
    use super::*;

    #[test]
    fn canonical_roots_accept_aliases_and_reject_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let alias = temp.path().join("alias");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&foreign).unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        std::os::unix::fs::symlink(&foreign, real.join("escape")).unwrap();

        let roots = configured_write_roots(std::slice::from_ref(&real)).unwrap();
        let through_alias = canonicalize_declared_path(&alias.join("inside.json")).unwrap();
        let escaped = canonicalize_declared_path(&real.join("escape/outside.json")).unwrap();

        assert!(through_alias.is_within(&roots[0]));
        assert!(!escaped.is_within(&roots[0]));
    }
}
