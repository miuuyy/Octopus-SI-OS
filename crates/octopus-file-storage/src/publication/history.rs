use super::*;

pub(super) const DIRECTORY_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "receipt", rename_all = "snake_case")]
enum RetainedReceipt {
    File(PublicationReceipt),
    Directory(DirectoryMoveReceipt),
    Reclaim(reclaim::ReclaimReceipt),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedRecord {
    schema_version: u32,
    original: RetainedReceipt,
}

fn file_key(path: &Path, operation: &str) -> io::Result<String> {
    Ok(format!("file-{}.json", hash(&serde_json::to_vec(&(path, operation)).map_err(|error| invalid(error.to_string()))?)))
}

fn directory_key(source: &Path, operation: &str, index: usize) -> io::Result<String> {
    Ok(format!("directory-{}.json", hash(&serde_json::to_vec(&(source, operation, index)).map_err(|error| invalid(error.to_string()))?)))
}

fn validate_file(receipt: &PublicationReceipt) -> io::Result<()> {
    validate_operation(&receipt.operation_id)?;
    validate_hash(receipt.before_hash.as_deref())?;
    validate_hash(receipt.after_hash.as_deref())?;
    namespace::validate_absolute(&receipt.path)?;
    if receipt.path.file_name().is_none() || receipt.after_hash.is_none() && (receipt.before_hash.is_none() || receipt.bytes != 0) {
        return Err(invalid("retained file receipt has invalid effect coordinates"));
    }
    Ok(())
}

fn receipt_names(receipt_path: &Path, operation: &str) -> [String; 2] {
    [hash(receipt_path.as_os_str().as_encoded_bytes()), hash(receipt_path.file_name().unwrap().as_encoded_bytes())]
        .map(|key| format!("{key}-{}.receipt", hash(operation.as_bytes())))
}

fn validate(record: &RetainedRecord, filename: &str) -> io::Result<()> {
    if record.schema_version != 1 { return Err(invalid("unsupported retained publication history")); }
    let key = match &record.original {
        RetainedReceipt::File(receipt) => { validate_file(receipt)?; file_key(&receipt.path, &receipt.operation_id)? },
        RetainedReceipt::Directory(receipt) => {
            directory::validate_receipt(receipt)?;
            directory_key(&receipt.movement.source, &receipt.operation_id, receipt.index)?
        },
        RetainedReceipt::Reclaim(receipt) => {
            reclaim::validate_receipt(receipt)?;
            format!("reclaim-{}.json", reclaim::key(&receipt.tombstone, &receipt.publication)?)
        },
    };
    if key != filename { return Err(invalid("retained publication history has a different identity")); }
    Ok(())
}

fn merge<T: PartialEq>(found: &mut Option<T>, candidate: Option<T>) -> io::Result<()> {
    if let Some(candidate) = candidate {
        if found.as_ref().is_some_and(|existing| existing != &candidate) {
            return Err(invalid("immutable publication history contains conflicting receipts"));
        }
        *found = Some(candidate);
    }
    Ok(())
}

struct HistoryRoot {
    path: PathBuf,
    directory: File,
}

fn private_directory(parent: &File, name: &std::ffi::OsStr) -> io::Result<Option<File>> {
    let state = match native::state(parent, name) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if state.kind != DirectoryEntryKind::Directory || state.mode & 0o077 != 0 {
        return Err(invalid("publication history requires a real private coordination directory"));
    }
    let directory = native::child(parent, name, true)?;
    let reopened = native::state(parent, name)?;
    if FileIdentity::of(&directory.metadata()?)? != state.identity
        || reopened.identity != state.identity
        || reopened.kind != state.kind
        || reopened.mode != state.mode
    {
        return Err(invalid("publication history directory changed identity"));
    }
    Ok(Some(directory))
}

fn roots(path: &Path) -> io::Result<Vec<HistoryRoot>> {
    roots_with_ancestry(path, &mut Vec::new()).map(|(roots, _)| roots)
}

fn roots_with_ancestry(path: &Path, ancestors: &mut Vec<PathBuf>) -> io::Result<(Vec<HistoryRoot>, bool)> {
    namespace::validate_absolute(path)?;
    let parent = path.parent().ok_or_else(|| invalid("history path has no parent"))?;
    let mut current_path = PathBuf::from("/");
    let mut current = native::directory(&current_path)?;
    let mut roots = Vec::new();
    let mut parts = parent.components().filter_map(|part| match part {
        std::path::Component::Normal(name) => Some(name), _ => None,
    });
    let writable_parent = loop {
        ancestors.push(current_path.clone());
        if let Some(directory) = private_directory(&current, std::ffi::OsStr::new(COORDINATION_DIRECTORY))? {
            roots.push(HistoryRoot { path: current_path.join(COORDINATION_DIRECTORY), directory });
        }
        let Some(name) = parts.next() else { break true; };
        let directory = match native::child(&current, name, true) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => break true,
            Err(error) if error.kind() == io::ErrorKind::NotADirectory => break false,
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::ELOOP) => break false,
            Err(error) => return Err(error),
        };
        let state = native::state(&current, name)?;
        if state.kind != DirectoryEntryKind::Directory || FileIdentity::of(&directory.metadata()?)? != state.identity {
            return Err(invalid("original history ancestor changed identity"));
        }
        current = directory;
        current_path.push(name);
    };
    roots.reverse();
    Ok((roots, writable_parent))
}

pub(super) struct FileAccess {
    pub(super) ancestors: Vec<PathBuf>,
    pub(super) directories: Vec<PathBuf>,
    pub(super) files: Vec<PathBuf>,
    pub(super) writable_parent: bool,
}

pub(super) fn file_access(path: &Path) -> io::Result<FileAccess> {
    let mut ancestors = Vec::new();
    let (roots, writable_parent) = roots_with_ancestry(path, &mut ancestors)?;
    let mut access = FileAccess { ancestors, directories: Vec::new(), files: Vec::new(), writable_parent };
    let mut receipts = BTreeMap::<String, PublicationReceipt>::new();
    for root in roots {
        let history = root.path.join("history");
        access.directories.push(history.clone());
        let Some(directory) = private_directory(&root.directory, std::ffi::OsStr::new("history"))? else { continue; };
        for name in native::names(&directory, 16384)? {
            let Some(name) = name.to_str() else { continue; };
            if !name.starts_with("file-") || !name.ends_with(".json") { continue; }
            let record: RetainedRecord = read_from(&directory, name, DIRECTORY_LIMIT)?
                .ok_or_else(|| invalid("publication history disappeared during access projection"))?;
            validate(&record, name)?;
            let RetainedReceipt::File(receipt) = record.original else {
                return Err(invalid("file history contains a different receipt type"));
            };
            if receipt.path == path {
                if receipts.get(&receipt.operation_id).is_some_and(|prior| prior != &receipt) {
                    return Err(invalid("immutable publication history contains conflicting receipts"));
                }
                receipts.insert(receipt.operation_id.clone(), receipt);
                access.files.push(history.join(name));
            }
        }
    }
    Ok(access)
}

fn read_from<T: serde::de::DeserializeOwned>(directory: &File, name: &str, limit: u64) -> io::Result<Option<T>> {
    let file = match native::child(directory, std::ffi::OsStr::new(name), false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit { return Err(invalid("publication history exceeds its declared bound")); }
    serde_json::from_slice(&bytes).map(Some).map_err(|error| invalid(error.to_string()))
}

fn read_retained(root: &HistoryRoot, key: &str) -> io::Result<Option<RetainedRecord>> {
    let Some(directory) = private_directory(&root.directory, std::ffi::OsStr::new("history"))? else { return Ok(None); };
    let record: Option<RetainedRecord> = read_from(&directory, key, DIRECTORY_LIMIT)?;
    if let Some(record) = &record { validate(record, key)?; }
    Ok(record)
}

pub(super) fn find_file(path: &Path, operation: &str) -> io::Result<Option<PublicationReceipt>> {
    validate_operation(operation)?;
    let mut found = None;
    for root in roots(path)? {
        if root.path.parent() == path.parent() {
            for (index, name) in receipt_names(path, operation).into_iter().enumerate() {
                if let Some(local) = read_from::<PublicationReceipt>(&root.directory, &name, RECORD_LIMIT)? {
                    validate_file(&local)?;
                    if local.operation_id != operation || !receipt_names(&local.path, operation).contains(&name)
                        || index == 0 && local.path != path
                    { return Err(invalid("original publication receipt changed identity")); }
                    if local.path == path { merge(&mut found, Some(local))?; }
                }
            }
        }
        if let Some(record) = read_retained(&root, &file_key(path, operation)?)? {
            let RetainedReceipt::File(receipt) = record.original else { return Err(invalid("file history contains a different receipt type")); };
            merge(&mut found, Some(receipt))?;
        }
    }
    Ok(found)
}

pub(super) fn find_directory(source: &Path, operation: &str, index: usize) -> io::Result<Option<DirectoryMoveReceipt>> {
    let mut found = None;
    for root in roots(source)? {
        if let Some(record) = read_retained(&root, &directory_key(source, operation, index)?)? {
            let RetainedReceipt::Directory(receipt) = record.original else { return Err(invalid("directory history contains a different receipt type")); };
            merge(&mut found, Some(receipt))?;
        }
    }
    Ok(found)
}

pub(super) fn find_reclaim(path: &Path, publication: &DirectoryMoveReceipt) -> io::Result<Option<reclaim::ReclaimReceipt>> {
    let mut found = None;
    for root in roots(path)? {
        if let Some(record) = read_retained(&root, &format!("reclaim-{}.json", reclaim::key(path, publication)?))? {
            let RetainedReceipt::Reclaim(receipt) = record.original else { return Err(invalid("reclamation history contains a different receipt type")); };
            if receipt.tombstone != path || receipt.publication != *publication { return Err(invalid("reclamation history changed original coordinates")); }
            merge(&mut found, Some(receipt))?;
        }
    }
    Ok(found)
}

fn ensure_history_directory(root: &Path) -> io::Result<PathBuf> {
    validate_coordination_root(root)?;
    let directory = root.join("history");
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    match builder.create(&directory) {
        Ok(()) => sync_directory(root)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error),
    }
    validate_coordination_root(&directory)?;
    Ok(directory)
}

fn retained_key(original: &RetainedReceipt) -> io::Result<String> {
    match original {
        RetainedReceipt::File(receipt) => file_key(&receipt.path, &receipt.operation_id),
        RetainedReceipt::Directory(receipt) => directory_key(&receipt.movement.source, &receipt.operation_id, receipt.index),
        RetainedReceipt::Reclaim(receipt) => Ok(format!("reclaim-{}.json", reclaim::key(&receipt.tombstone, &receipt.publication)?)),
    }
}

fn save(root: &Path, original: RetainedReceipt) -> io::Result<()> {
    let directory = ensure_history_directory(root)?;
    let key = retained_key(&original)?;
    let record = RetainedRecord { schema_version: 1, original };
    validate(&record, &key)?;
    write_record_once_bounded(&directory.join(key), &record, DIRECTORY_LIMIT)
}

pub(super) fn stage_directory(root: &Path, receipt: &DirectoryMoveReceipt) -> io::Result<(Option<StagedRecord>, PathBuf)> {
    let directory = ensure_history_directory(root)?;
    let original = RetainedReceipt::Directory(receipt.clone());
    let key = retained_key(&original)?;
    let record = RetainedRecord { schema_version: 1, original };
    validate(&record, &key)?;
    let staged = stage_record_once_bounded(&directory.join(key), &record, DIRECTORY_LIMIT)?;
    Ok((staged, directory))
}

pub(super) fn save_directory(root: &Path, receipt: &DirectoryMoveReceipt) -> io::Result<()> {
    save(root, RetainedReceipt::Directory(receipt.clone()))
}

pub(super) fn save_reclaim(root: &Path, receipt: &reclaim::ReclaimReceipt) -> io::Result<()> {
    save(root, RetainedReceipt::Reclaim(receipt.clone()))
}

pub(super) fn export_tree(source: &Path, identity: &FileIdentity, destination_root: &Path) -> io::Result<bool> {
    let directory = native::pinned_directory(source, identity)?;
    let mut exported = false;
    for name in native::names(&directory, 16384)? {
        let state = native::state(&directory, &name)?;
        let path = source.join(&name);
        if name == COORDINATION_DIRECTORY {
            if state.kind != DirectoryEntryKind::Directory || state.mode & 0o077 != 0 {
                return Err(invalid("history export rejects aliased or nonprivate publication metadata"));
            }
            let _metadata = native::pinned_directory(&path, &state.identity)?;
            export_metadata(&path, destination_root)?;
            exported = true;
        } else if state.kind == DirectoryEntryKind::Directory {
            exported |= export_tree(&path, &state.identity, destination_root)?;
        }
    }
    Ok(exported)
}

fn export_metadata(root: &Path, destination_root: &Path) -> io::Result<()> {
    validate_coordination_root(root)?;
    let entries = std::fs::read_dir(root)?.collect::<io::Result<Vec<_>>>()?;
    for entry in &entries {
        if directory::absent(&entry.path())? { continue; }
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| invalid("publication metadata name is not UTF-8"))?;
        if name.ends_with(".pending") {
            let pending: PendingPublication = read_record(&entry.path())?.ok_or_else(|| invalid("pending publication disappeared"))?;
            if pending.receipt.path.parent() != root.parent() { return Err(invalid("unsettled publication was moved without original evidence")); }
            let target = Target::open(&pending.receipt.path, false)?;
            if target.pending() != entry.path() { return Err(invalid("pending publication has a different metadata key")); }
            let _lock = target.lock(true, false)?;
            if target.settle_pending(None)?.is_some_and(|receipt| receipt.outcome == PublicationOutcome::Uncertain) {
                return Err(invalid("uncertain child publication prevents directory movement"));
            }
        } else if name.ends_with(".plan") {
            directory::settle_path(&entry.path(), None, false)?;
        } else if name.ends_with(".directory") {
            directory::settle_reference(&entry.path(), None, false)?;
        } else if name.ends_with(".reclaim") {
            reclaim::settle_reference(&entry.path())?;
        }
    }
    reclaim::validate_metadata(root)?;
    let mut destination_history: Option<PathBuf> = None;
    let mut staged_records: Vec<StagedRecord> = Vec::new();
    let mut touched_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut stage_retained = |original: RetainedReceipt| -> io::Result<()> {
        let directory = match &destination_history {
            Some(directory) => directory,
            None => {
                let created = ensure_history_directory(destination_root)?;
                touched_dirs.insert(created.clone());
                destination_history.insert(created)
            }
        };
        let key = retained_key(&original)?;
        let record = RetainedRecord { schema_version: 1, original };
        validate(&record, &key)?;
        if let Some(staged) = stage_record_once_bounded(&directory.join(key), &record, DIRECTORY_LIMIT)? {
            staged_records.push(staged);
        }
        Ok(())
    };
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| invalid("publication metadata name is not UTF-8"))?;
        if name == "history" {
            validate_coordination_root(&entry.path())?;
            for item in std::fs::read_dir(entry.path())? {
                let item = item?;
                let filename = item.file_name().into_string().map_err(|_| invalid("history name is not UTF-8"))?;
                if let Some(stem) = temporary_stem(&filename) {
                    if !std::fs::symlink_metadata(item.path())?.is_file()
                        || !["file-", "directory-", "reclaim-"].iter().any(|prefix| stem.strip_prefix(prefix).is_some_and(|key| validate_hash(Some(key)).is_ok()))
                    { return Err(invalid("history contains an unknown uncommitted metadata entry")); }
                    continue;
                }
                let record: RetainedRecord = read_record_bounded(&item.path(), DIRECTORY_LIMIT)?.ok_or_else(|| invalid("history disappeared during export"))?;
                validate(&record, &filename)?;
                if match &record.original {
                    RetainedReceipt::File(receipt) => receipt.outcome == PublicationOutcome::Uncertain,
                    RetainedReceipt::Directory(receipt) => receipt.outcome == PublicationOutcome::Uncertain,
                    RetainedReceipt::Reclaim(_) => false,
                } { return Err(invalid("uncertain history prevents ancestor publication")); }
                stage_retained(record.original)?;
            }
        } else if name.ends_with(".receipt") {
            let receipt: PublicationReceipt = read_record(&entry.path())?.ok_or_else(|| invalid("receipt disappeared during export"))?;
            validate_file(&receipt)?;
            if !receipt_names(&receipt.path, &receipt.operation_id).iter().any(|key| key == name)
                || receipt.outcome == PublicationOutcome::Uncertain { return Err(invalid("child receipt cannot be exported as settled history")); }
            stage_retained(RetainedReceipt::File(receipt))?;
        } else if !internal_filename(name) || !std::fs::symlink_metadata(entry.path())?.is_file() {
            return Err(invalid("publication metadata contains an unknown entry"));
        }
    }
    if !staged_records.is_empty() {
        let barrier_files: Vec<&File> = staged_records.iter().map(|item| &item.file).collect();
        sync_batch_barrier(&barrier_files, &BTreeSet::new())?;
        drop(barrier_files);
        for staged in staged_records {
            commit_staged_record(staged)?;
        }
    }
    touched_dirs.insert(destination_root.to_path_buf());
    sync_batch_barrier(&[], &touched_dirs)?;
    Ok(())
}

fn internal_filename(name: &str) -> bool {
    let hash_name = |value: &str| validate_hash(Some(value)).is_ok();
    if name.starts_with("gc-") && reclaim::is_metadata_filename(name) { return true; }
    if let Some(base) = name.strip_suffix(".lock") {
        return hash_name(base) || base.strip_prefix("dir-").is_some_and(hash_name);
    }
    if let Some(base) = name.strip_suffix(".install") { return hash_name(base); }
    if let Some(base) = name.strip_suffix(".bytes").or_else(|| name.strip_suffix(".preimage")) {
        return base.split_once('-').is_some_and(|(left, right)| hash_name(left) && hash_name(right));
    }
    if let Some(stem) = temporary_stem(name) {
        return hash_name(stem) || stem.strip_prefix("dir-").is_some_and(hash_name)
            || stem.strip_prefix("gc-").is_some_and(hash_name)
            || stem.split_once('-').is_some_and(|(left, right)| hash_name(left) && hash_name(right));
    }
    false
}
