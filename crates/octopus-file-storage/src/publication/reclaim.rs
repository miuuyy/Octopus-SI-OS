use super::*;

const RECLAIM_BATCH_SIZE: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema_version: u32,
    tombstone: PathBuf,
    publication: DirectoryMoveReceipt,
    parent_identity: FileIdentity,
    manifest: DirectoryPreimage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    schema_version: u32,
    intent_hash: String,
    next: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    batch_end: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReclaimReceipt {
    pub(super) schema_version: u32,
    pub(super) tombstone: PathBuf,
    pub(super) publication: DirectoryMoveReceipt,
    pub(super) manifest_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fence {
    schema_version: u32,
    intent_path: PathBuf,
    intent_hash: String,
}

pub(super) fn key(tombstone: &Path, publication: &DirectoryMoveReceipt) -> io::Result<String> {
    Ok(hash(&serde_json::to_vec(&(tombstone, publication)).map_err(|error| invalid(error.to_string()))?))
}

fn encoded_hash<T: Serialize>(value: &T) -> io::Result<String> {
    Ok(hash(&serde_json::to_vec(value).map_err(|error| invalid(error.to_string()))?))
}

pub(super) fn marker(path: &Path) -> PathBuf {
    path.parent().unwrap().join(COORDINATION_DIRECTORY).join(format!("{}.reclaim", hash(path.file_name().unwrap().as_encoded_bytes())))
}

fn intent_path(tombstone: &Path, publication: &DirectoryMoveReceipt) -> io::Result<PathBuf> {
    Ok(tombstone.parent().ok_or_else(|| invalid("tombstone has no parent"))?.join(COORDINATION_DIRECTORY).join(format!("gc-{}.intent", key(tombstone, publication)?)))
}

pub(super) fn validate_receipt(receipt: &ReclaimReceipt) -> io::Result<()> {
    directory::validate_receipt(&receipt.publication)?;
    namespace::validate_absolute(&receipt.tombstone)?;
    validate_hash(Some(&receipt.manifest_hash))?;
    if receipt.schema_version != 1 || receipt.publication.outcome != PublicationOutcome::Applied
        || receipt.tombstone != receipt.publication.movement.destination
    { return Err(invalid("reclamation receipt has invalid original movement evidence")); }
    Ok(())
}

fn validate_intent(intent: &Intent, path: &Path, retained: bool) -> io::Result<()> {
    directory::validate_receipt(&intent.publication)?;
    directory::validate_preimage(&intent.manifest)?;
    let public_entries = intent.manifest.entries.iter().filter(|entry| !entry.relative_path.components().any(|part| part.as_os_str() == COORDINATION_DIRECTORY)).cloned().collect();
    let public = DirectoryPreimage { entries: public_entries, ..intent.manifest.clone() };
    if intent.schema_version != 1 || intent.publication.outcome != PublicationOutcome::Applied
        || intent.tombstone != intent.publication.movement.destination
        || public != intent.publication.movement.expected
    { return Err(invalid("reclamation intent differs from its exact published tombstone")); }
    let original = intent_path(&intent.tombstone, &intent.publication)?;
    if if retained {
        path.file_name() != original.file_name() || path.parent().and_then(Path::file_name).is_none_or(|name| name != COORDINATION_DIRECTORY)
    } else { path != original } { return Err(invalid("reclamation metadata changed its original identity")); }
    Ok(())
}

fn load_intent(path: &Path) -> io::Result<Intent> {
    load_intent_at(path, false)
}

fn load_intent_at(path: &Path, retained: bool) -> io::Result<Intent> {
    validate_coordination_root(path.parent().ok_or_else(|| invalid("reclamation intent has no metadata directory"))?)?;
    let intent = read_record_bounded(path, history::DIRECTORY_LIMIT)?.ok_or_else(|| invalid("reclamation lost its durable intent"))?;
    validate_intent(&intent, path, retained)?;
    Ok(intent)
}

fn completion(intent: &Intent) -> io::Result<ReclaimReceipt> {
    Ok(ReclaimReceipt { schema_version: 1, tombstone: intent.tombstone.clone(), publication: intent.publication.clone(), manifest_hash: encoded_hash(&intent.manifest)? })
}

fn validate_fence(fence: &Fence, intent: &Intent, path: &Path) -> io::Result<()> {
    if fence.schema_version != 1 || fence.intent_path != intent_path(&intent.tombstone, &intent.publication)?
        || fence.intent_hash != encoded_hash(intent)? || marker(&intent.tombstone) != path
    { return Err(invalid("reclamation fence has different original coordinates")); }
    Ok(())
}

fn finish(intent: &Intent, path: &Path) -> io::Result<()> {
    let receipt = completion(intent)?;
    write_record_once_bounded(&path.with_extension("done"), &receipt, history::DIRECTORY_LIMIT)?;
    history::save_reclaim(path.parent().unwrap(), &receipt)?;
    let marker = marker(&intent.tombstone);
    if let Some(fence) = read_record::<Fence>(&marker)? {
        validate_fence(&fence, intent, &marker)?;
        std::fs::remove_file(&marker)?;
        sync_directory(marker.parent().unwrap())?;
    }
    Ok(())
}

pub(super) fn check_fence(endpoint: &Path) -> io::Result<()> {
    let path = marker(endpoint);
    settle_reference(&path)
}

pub(super) fn settle_reference(path: &Path) -> io::Result<()> {
    let Some(fence): Option<Fence> = read_record(&path)? else { return Ok(()); };
    let intent = load_intent(&fence.intent_path)?;
    validate_fence(&fence, &intent, &namespace::normalize_entry(path)?)?;
    if let Some(done) = read_record_bounded::<ReclaimReceipt>(&fence.intent_path.with_extension("done"), history::DIRECTORY_LIMIT)? {
        if done != completion(&intent)? { return Err(invalid("reclamation completion differs from its durable intent")); }
        return finish(&intent, &fence.intent_path);
    }
    Err(invalid("tombstone reclamation is unfinished; endpoint and descendants remain fenced"))
}

pub(super) fn is_metadata_filename(name: &str) -> bool {
    let Some((stem, extension)) = name.rsplit_once('.') else { return false; };
    stem.strip_prefix("gc-").is_some_and(|key| validate_hash(Some(key)).is_ok())
        && matches!(extension, "intent" | "progress" | "done" | "lock")
}

pub(super) fn validate_metadata(root: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| invalid("reclamation metadata name is not UTF-8"))?;
        if !name.starts_with("gc-") { continue; }
        if temporary_stem(&name).is_some_and(|stem| stem.strip_prefix("gc-").is_some_and(|key| validate_hash(Some(key)).is_ok())) {
            if !std::fs::symlink_metadata(entry.path())?.is_file() { return Err(invalid("uncommitted reclamation metadata is not a regular file")); }
            continue;
        }
        if !is_metadata_filename(&name) { return Err(invalid("unknown reclamation metadata entry")); }
        let path = entry.path().with_extension("intent");
        if entry.path().extension().is_some_and(|extension| extension == "lock") && directory::absent(&path)? { continue; }
        let intent = load_intent_at(&path, true)?;
        let done = read_record_bounded::<ReclaimReceipt>(&path.with_extension("done"), history::DIRECTORY_LIMIT)?;
        if done.as_ref() != Some(&completion(&intent)?) {
            return Err(invalid("unfinished tombstone reclamation prevents ancestor movement"));
        }
        let progress: Progress = read_record(&path.with_extension("progress"))?.ok_or_else(|| invalid("completed reclamation lost its progress evidence"))?;
        validate_progress(&intent, &progress)?;
        if progress.next != intent.manifest.entries.len() { return Err(invalid("completed reclamation has an unfinished frontier")); }
        if path == intent_path(&intent.tombstone, &intent.publication)? {
            finish(&intent, &path)?;
        } else {
            history::save_reclaim(root, &completion(&intent)?)?;
        }
    }
    Ok(())
}

fn order(manifest: &DirectoryPreimage) -> Vec<&DirectoryEntry> {
    let mut entries = manifest.entries.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| right.relative_path.components().count().cmp(&left.relative_path.components().count())
        .then(left.relative_path.cmp(&right.relative_path)));
    entries
}

fn validate_progress(intent: &Intent, progress: &Progress) -> io::Result<()> {
    let len = intent.manifest.entries.len();
    if progress.schema_version != 1
        || progress.intent_hash != encoded_hash(intent)?
        || progress.next > len
        || progress.batch_end.is_some_and(|end| end <= progress.next || end > len)
    {
        return Err(invalid("reclamation progress differs from its durable intent"));
    }
    Ok(())
}

fn write_progress(path: &Path, progress: &Progress) -> io::Result<()> {
    let bytes = serde_json::to_vec(progress).map_err(|error| invalid(error.to_string()))?;
    let temporary = path.with_extension(format!("{}.{}.tmp", std::process::id(), NEXT_TEMP.fetch_add(1, Ordering::Relaxed)));
    let file = write_new_regular(&temporary, &bytes)?;
    crate::sync_ordered(&file)?;
    std::fs::rename(&temporary, path)?;
    sync_directory(path.parent().unwrap())
}

fn validate_frontier(intent: &Intent, progress: &Progress) -> io::Result<bool> {
    let parent = std::fs::symlink_metadata(intent.tombstone.parent().unwrap())?;
    if !parent.is_dir() || FileIdentity::of(&parent)? != intent.parent_identity {
        return Err(invalid("reclamation parent changed after the durable intent was prepared"));
    }
    if directory::absent(&intent.tombstone)? {
        if progress.next == intent.manifest.entries.len() { return Ok(false); }
        return Err(invalid("reclamation lost its exact root before the durable frontier reached it"));
    }
    let current = directory::capture(&intent.tombstone, intent.manifest.digest_algorithm, true, &intent.manifest.identity)?;
    if current.identity != intent.manifest.identity || current.mode != intent.manifest.mode {
        return Err(invalid("reclamation found a foreign tombstone inode"));
    }
    let mut entries = current.entries.into_iter().map(|entry| (entry.relative_path.clone(), entry)).collect::<BTreeMap<_, _>>();
    let frontier_end = progress.batch_end.unwrap_or((progress.next + 1).min(intent.manifest.entries.len()));
    for (index, expected) in order(&intent.manifest).into_iter().enumerate() {
        let observed = entries.remove(&expected.relative_path);
        if if index < progress.next { observed.is_some() }
            else if index < frontier_end { observed.as_ref().is_some_and(|observed| observed != expected) }
            else { observed.as_ref() != Some(expected) }
        { return Err(invalid("reclamation tree differs from its exact durable frontier")); }
    }
    if !entries.is_empty() { return Err(invalid("reclamation tree contains an undeclared entry")); }
    Ok(true)
}

fn unlink_unsynced(path: &Path, directory: bool, parent_identity: &FileIdentity, identity: &FileIdentity) -> io::Result<File> {
    #[cfg(unix)] {
        use std::ffi::CString;
        use std::os::fd::AsRawFd;
        let parent = native::pinned_directory(path.parent().ok_or_else(|| invalid("reclamation entry has no parent"))?, parent_identity)?;
        let name = CString::new(path.file_name().ok_or_else(|| invalid("reclamation entry has no name"))?.as_encoded_bytes()).map_err(|_| invalid("reclamation name contains NUL"))?;
        let state = native::state(&parent, path.file_name().unwrap())?;
        if state.identity != *identity || (state.kind == DirectoryEntryKind::Directory) != directory {
            return Err(invalid("native reclamation entry differs from its original inode"));
        }
        let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), if directory { libc::AT_REMOVEDIR } else { 0 }) };
        if result != 0 { return Err(io::Error::last_os_error()); }
        Ok(parent)
    }
    #[cfg(not(unix))] { let _ = (path, directory, parent_identity, identity); Err(io::Error::new(io::ErrorKind::Unsupported, "exact tombstone reclamation requires native unlink")) }
}

fn unlink(path: &Path, directory: bool, parent_identity: &FileIdentity, identity: &FileIdentity) -> io::Result<()> {
    let parent = unlink_unsynced(path, directory, parent_identity, identity)?;
    crate::sync_ordered(&parent)
}

fn entry_parent_identity<'a>(intent: &'a Intent, relative: &Path) -> io::Result<&'a FileIdentity> {
    let parent = relative.parent().ok_or_else(|| invalid("reclamation member has no relative parent"))?;
    if parent.as_os_str().is_empty() { return Ok(&intent.manifest.identity); }
    intent.manifest.entries.iter().find(|entry| entry.relative_path == parent && entry.kind == DirectoryEntryKind::Directory)
        .map(|entry| &entry.identity).ok_or_else(|| invalid("reclamation member lost its declared parent identity"))
}

pub fn reclaim_directory(tombstone: &Path, proof: &DirectoryMoveReceipt, expected: &DirectoryPreimage) -> io::Result<()> {
    directory::validate_receipt(proof)?;
    if proof.outcome != PublicationOutcome::Applied || expected != &proof.movement.expected || tombstone != proof.movement.destination {
        return Err(invalid("reclamation requires the original applied destination and exact public manifest"));
    }
    if let Some(receipt) = history::find_reclaim(tombstone, proof)? {
        validate_receipt(&receipt)?;
        return Ok(());
    }
    if history::find_directory(&proof.movement.source, &proof.operation_id, proof.index)?.as_ref() != Some(proof) {
        return Err(invalid("reclamation requires a saved original directory publication receipt"));
    }
    directory::exact_path(tombstone)?;
    let tombstone = tombstone.to_path_buf();
    let mut requests = vec![(tombstone.parent().unwrap().to_path_buf(), true), (tombstone.clone(), true)];
    for entry in &expected.entries {
        if entry.kind == DirectoryEntryKind::Directory { namespace::member_directory(&mut requests, tombstone.join(&entry.relative_path))?; }
    }
    let mut namespace = NamespaceGuard::acquire(&requests, true)?;
    if history::find_reclaim(&tombstone, proof)?.is_some() { return Ok(()); }
    directory::exact_path(&tombstone)?;
    namespace.verify()?;
    let path = intent_path(&tombstone, proof)?;
    let existing = !directory::absent(&path)?;
    directory::check_fences_reclaim(std::slice::from_ref(&tombstone), None, true, Some(&tombstone))?;
    if existing {
        let intent = load_intent(&path)?;
        if intent.publication != *proof { return Err(invalid("reclamation identity was reused")); }
    } else {
        if directory::capture(&tombstone, expected.digest_algorithm, false, &expected.identity)? != *expected {
            return Err(invalid("reclamation tombstone differs from the exact published tree"));
        }
    }
    let root = coordination_root(tombstone.parent().unwrap())?;
    let members = expected.entries.iter().filter(|entry| entry.kind == DirectoryEntryKind::File)
        .map(|entry| (tombstone.join(&entry.relative_path), entry.identity.clone())).collect::<Vec<_>>();
    let _files = namespace::TreeFiles::acquire(&members, true)?;
    let _lock = HeldFile::acquire(path.with_extension("lock"), true, true)?;
    let intent = if existing { load_intent(&path)? } else {
        history::export_tree(&tombstone, &expected.identity, &root)?;
        let manifest = directory::capture(&tombstone, expected.digest_algorithm, true, &expected.identity)?;
        let intent = Intent { schema_version: 1, tombstone: tombstone.clone(), publication: proof.clone(),
            parent_identity: FileIdentity::of(&std::fs::symlink_metadata(tombstone.parent().unwrap())?)?, manifest };
        validate_intent(&intent, &path, false)?;
        write_record_once_bounded(&path, &intent, history::DIRECTORY_LIMIT)?;
        intent
    };
    let progress_path = path.with_extension("progress");
    let mut progress = match read_record::<Progress>(&progress_path)? {
        Some(progress) => progress,
        None => {
            if existing && directory::capture(&tombstone, expected.digest_algorithm, true, &expected.identity)? != intent.manifest {
                return Err(invalid("reclamation intent lost its initial exact frontier"));
            }
            let progress = Progress { schema_version: 1, intent_hash: encoded_hash(&intent)?, next: 0, batch_end: None };
            write_record_once(&progress_path, &progress)?;
            progress
        }
    };
    validate_progress(&intent, &progress)?;
    let fence = Fence { schema_version: 1, intent_path: path.clone(), intent_hash: encoded_hash(&intent)? };
    write_record_once(&marker(&tombstone), &fence)?;
    if !validate_frontier(&intent, &progress)? { return finish(&intent, &path); }
    let ordered = order(&intent.manifest);
    while progress.next < ordered.len() {
        let start = progress.next;
        let end = if let Some(existing_end) = progress.batch_end {
            existing_end
        } else {
            let depth = ordered[start].relative_path.components().count();
            let limit = (start + RECLAIM_BATCH_SIZE).min(ordered.len());
            let mut cursor = start + 1;
            while cursor < limit && ordered[cursor].relative_path.components().count() == depth {
                cursor += 1;
            }
            cursor
        };
        if end - start > 1 && progress.batch_end != Some(end) {
            progress.batch_end = Some(end);
            write_progress(&progress_path, &progress)?;
        }
        let mut touched_parents: BTreeSet<PathBuf> = BTreeSet::new();
        for index in start..end {
            let expected = ordered[index];
            let entry = tombstone.join(&expected.relative_path);
            if !directory::absent(&entry)? {
                if directory::capture_entry(&entry, expected.relative_path.clone(), intent.manifest.digest_algorithm)? != *expected {
                    return Err(invalid("reclamation entry changed after frontier validation"));
                }
                namespace.verify()?;
                let _parent = unlink_unsynced(
                    &entry,
                    expected.kind == DirectoryEntryKind::Directory,
                    entry_parent_identity(&intent, &expected.relative_path)?,
                    &expected.identity,
                )?;
                if let Some(parent_path) = entry.parent() {
                    touched_parents.insert(parent_path.to_path_buf());
                }
                if expected.kind == DirectoryEntryKind::Directory {
                    namespace.removed(&entry);
                    touched_parents.remove(&entry);
                }
            }
            #[cfg(test)] interrupt_at(Interruption::Reclaim(index));
        }
        if !touched_parents.is_empty() {
            sync_batch_barrier(&[], &touched_parents)?;
        }
        progress.next = end;
        progress.batch_end = None;
        write_progress(&progress_path, &progress)?;
    }
    namespace.verify()?;
    let root = native::pinned_directory(&tombstone, &intent.manifest.identity)?;
    if !native::names(&root, 1)?.is_empty() { return Err(invalid("reclamation root is no longer the exact empty tombstone")); }
    unlink(&tombstone, true, &intent.parent_identity, &intent.manifest.identity)?;
    namespace.removed(&tombstone);
    namespace.verify()?;
    finish(&intent, &path)
}
