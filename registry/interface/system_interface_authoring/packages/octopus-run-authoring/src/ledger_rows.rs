use super::*;
use std::sync::OnceLock;

const SHARED_LEDGERS: usize = 8;

static SHARED: OnceLock<Mutex<Vec<(PathBuf, Arc<LedgerRows>)>>> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_provider_index_materializes_only_its_own_grants() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let metadata = dir.path().join("metadata");
        std::fs::create_dir(&metadata).unwrap();
        let mut content = String::new();
        let mut hashes = Vec::new();
        for seq in 0..25 {
            let movement = Movement {
                seq: octopus_ledger::Seq(seq), lamport: octopus_ledger::Lamport(seq + 1),
                recorded_at_unix_ms: seq + 1, trace: octopus_ledger::TraceId(format!("trace-{seq}")),
                kind: MovementKind::InterfaceOutbound,
                from: Endpoint::Interface("source".into()),
                to: Endpoint::Interface(if seq == 24 { "own".into() } else { format!("foreign-{seq}").into() }),
                artifact: None,
                invocation_metadata: Some(octopus_ledger::InvocationMetadata::new(octopus_ledger::GrantProjection {
                    tools: vec![format!("tool-{seq}")], ..Default::default()
                }).into()),
            };
            let row = octopus_ledger::encode_durable_movement_row(&movement, seq as usize, |_, bytes| {
                let hash = ContentHash::of_bytes(bytes);
                std::fs::write(metadata.join(hash.to_hex()), bytes).unwrap();
                hashes.push(hash);
                Ok(hash)
            }).unwrap();
            content.push_str(&row);
            content.push('\n');
        }
        std::fs::write(&path, content).unwrap();
        let mut index = ProviderLedgerEvidenceIndex::new("own".into(), path.clone(), 1_048_576);
        index.catch_up().unwrap();
        assert_eq!(index.next_seq, 25);
        let grants = index.ledger.grants.lock().unwrap();
        assert_eq!(grants.0.len(), 1);
        assert_eq!(grants.0[hashes.last().unwrap()].1.tools, ["tool-24"]);
        drop(grants);
        // Unselected content remains integrity-checked, even though its full
        // authority is not retained by this provider's recovery index.
        std::fs::write(metadata.join(hashes[0].to_hex()), b"corrupt").unwrap();
        let mut fresh = ProviderLedgerEvidenceIndex::new("own".into(), path, 1_048_576);
        assert!(fresh.catch_up().is_err());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LedgerFile {
    device: u64,
    inode: u64,
}

impl LedgerFile {
    pub(crate) fn of(file: &std::fs::File) -> Result<Self, String> {
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        Ok(Self::from_metadata(&metadata))
    }

    #[cfg(unix)]
    pub(crate) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self { device: metadata.dev(), inode: metadata.ino() }
    }

    #[cfg(not(unix))]
    pub(crate) fn from_metadata(_: &std::fs::Metadata) -> Self {
        Self { device: 0, inode: 0 }
    }
}

#[derive(Clone)]
pub(crate) struct LedgerRow {
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) digest: ContentHash,
    pub(crate) outbound: Option<OutboundRow>,
}

#[derive(Clone)]
pub(crate) struct OutboundRow {
    pub(crate) from: Endpoint,
    pub(crate) to: Endpoint,
    pub(crate) artifact: Option<ArtifactRef>,
    pub(crate) runtime: Option<octopus_ledger::RuntimeInvocationMetadata>,
    pub(crate) invocation: bool,
}

impl OutboundRow {
    pub(crate) fn of(movement: &Movement) -> Option<Self> {
        if movement.kind != MovementKind::InterfaceOutbound { return None; }
        Some(Self {
            from: movement.from.clone(),
            to: movement.to.clone(),
            artifact: movement.artifact.clone(),
            runtime: movement.runtime().cloned(),
            invocation: movement.invocation_metadata.as_ref().and_then(|metadata| metadata.invocation()).is_some(),
        })
    }

    pub(crate) fn writes_through(&self, writer: &CheckedRunWriter) -> bool {
        matches!((&self.from, &self.to), (Endpoint::Interface(from), Endpoint::Interface(to)) if *from == writer.source && *to == writer.door)
    }

    pub(crate) fn run_snapshot(&self, run: &RunId) -> Option<&str> {
        let runtime = self.runtime.as_ref().filter(|runtime| self.invocation && runtime.run == *run)?;
        runtime.run_record_hash.as_deref()
    }
}

#[derive(Default)]
struct RowState {
    file: Option<LedgerFile>,
    rows: Vec<LedgerRow>,
}

type Grants = (HashMap<ContentHash, (BlobFingerprint, Arc<octopus_ledger::GrantProjection>)>, std::collections::VecDeque<ContentHash>);

pub(crate) struct LedgerRows {
    pub(crate) decoder: DurableMovementDecoder,
    metadata_dir: PathBuf,
    grants: Mutex<Grants>,
    state: Mutex<RowState>,
}

impl LedgerRows {
    pub(crate) fn detached(movements_path: &Path) -> Arc<Self> {
        let metadata_dir = movements_path.parent().filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")).join("metadata");
        Arc::new(Self {
            decoder: DurableMovementDecoder::for_log(movements_path), metadata_dir,
            grants: Mutex::new(Grants::default()), state: Mutex::new(RowState::default()),
        })
    }

    pub(crate) fn decode_with_grant_if(
        &self, row: &[u8], seq: u64, needs_grant: impl FnOnce(&Movement) -> bool,
    ) -> Result<Movement, String> {
        let line = usize::try_from(seq).map_err(|_| "ledger row sequence overflow".to_string())?;
        let (mut movement, content) = self.decoder.decode_projection(row, line).map_err(|error| error.to_string())?;
        if !needs_grant(&movement) { return Ok(movement); }
        let Some(content) = content else { return Ok(movement); };
        let grant = self.grant(content)?;
        match (movement.invocation_metadata.as_mut(), grant) {
            (Some(octopus_ledger::MovementMetadata::Invocation(metadata)), Some(grant)) => {
                metadata.grant = (*grant).clone();
                Ok(movement)
            }
            _ => self.decoder.decode(row, line).map_err(|error| error.to_string()),
        }
    }

    fn grant(&self, content: ContentHash) -> Result<Option<Arc<octopus_ledger::GrantProjection>>, String> {
        let path = self.metadata_dir.join(content.to_hex());
        let Some(fingerprint) = blob_fingerprint(&path) else { return Ok(None); };
        let mut grants = self.grants.lock().map_err(|_| "ledger grant index lock is poisoned".to_string())?;
        if let Some((known, grant)) = grants.0.get(&content) {
            if *known == fingerprint { return Ok(Some(grant.clone())); }
        }
        let Ok(bytes) = std::fs::read(&path) else { return Ok(None); };
        if ContentHash::of_bytes(&bytes) != content { return Ok(None); }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return Ok(None); };
        let Ok(grant) = serde_json::from_value::<octopus_ledger::GrantProjection>(value) else { return Ok(None); };
        if blob_fingerprint(&path) != Some(fingerprint) { return Ok(None); }
        let grant = Arc::new(grant);
        if grants.0.insert(content, (fingerprint, grant.clone())).is_none() {
            grants.1.push_back(content);
            if grants.1.len() > 64 {
                if let Some(oldest) = grants.1.pop_front() { grants.0.remove(&oldest); }
            }
        }
        Ok(Some(grant))
    }

    pub(crate) fn shared(movements_path: &Path) -> Arc<Self> {
        let mut shared = SHARED.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let rows = match shared.iter().position(|(path, _)| path == movements_path) {
            Some(index) => shared.remove(index).1,
            None => Self::detached(movements_path),
        };
        shared.push((movements_path.to_path_buf(), rows.clone()));
        if shared.len() > SHARED_LEDGERS { shared.remove(0); }
        rows
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, RowState>, String> {
        self.state.lock().map_err(|_| "ledger row index lock is poisoned".to_string())
    }

    pub(crate) fn row(&self, file: LedgerFile, seq: u64) -> Result<Option<LedgerRow>, String> {
        let state = self.state()?;
        if state.file != Some(file) { return Ok(None); }
        Ok(usize::try_from(seq).ok().and_then(|index| state.rows.get(index)).cloned())
    }

    pub(crate) fn known(&self, file: LedgerFile) -> Result<u64, String> {
        let state = self.state()?;
        Ok(if state.file == Some(file) { state.rows.len() as u64 } else { 0 })
    }

    pub(crate) fn forget(&self) -> Result<(), String> {
        let mut state = self.state()?;
        state.file = None;
        state.rows.clear();
        Ok(())
    }

    pub(crate) fn note(&self, file: LedgerFile, seq: u64, row: LedgerRow) -> Result<(), String> {
        let mut state = self.state()?;
        if state.file != Some(file) {
            state.file = Some(file);
            state.rows.clear();
        }
        let index = usize::try_from(seq).map_err(|_| "ledger row index sequence overflow".to_string())?;
        if let Some(known) = state.rows.get(index) {
            if known.start == row.start && known.end == row.end && known.digest == row.digest { return Ok(()); }
            state.rows.truncate(index);
        }
        if index == state.rows.len() && state.rows.last().map_or(0, |last| last.end) == row.start {
            state.rows.push(row);
        }
        Ok(())
    }

    fn outbound_matching(&self, file: LedgerFile, upper: u64, predicate: impl Fn(&OutboundRow) -> bool) -> Result<Vec<u64>, String> {
        let state = self.state()?;
        if state.file != Some(file) { return Err("ledger row index lost its exact ledger".into()); }
        Ok(state.rows.iter().take(usize::try_from(upper).unwrap_or(usize::MAX)).enumerate()
            .filter(|(_, row)| row.outbound.as_ref().is_some_and(&predicate)).map(|(seq, _)| seq as u64).collect())
    }
}

pub(crate) struct OutboundPrefix {
    pub(crate) available: u64,
    index: Arc<LedgerRows>,
    ledger: LedgerFile,
    file: std::fs::File,
    max_row_bytes: u64,
}

impl OutboundPrefix {
    pub(crate) fn matching(&self, predicate: impl Fn(&OutboundRow) -> bool) -> Result<Vec<u64>, String> {
        self.index.outbound_matching(self.ledger, self.available, predicate)
    }

    pub(crate) fn verified(&mut self, seq: u64) -> Result<OutboundRow, String> {
        let row = self.index.row(self.ledger, seq)?.ok_or("ledger row index lost an indexed movement")?;
        let length = row.end.checked_sub(row.start).filter(|length| *length <= self.max_row_bytes)
            .ok_or("indexed movement exceeds its bounded row")?;
        self.file.seek(std::io::SeekFrom::Start(row.start)).map_err(|error| error.to_string())?;
        let mut bytes = vec![0; usize::try_from(length).map_err(|_| "indexed movement row overflow".to_string())?];
        self.file.read_exact(&mut bytes).map_err(|error| error.to_string())?;
        if ContentHash::of_bytes(&bytes) != row.digest { return Err("indexed movement differs from its durable row".into()); }
        row.outbound.ok_or_else(|| "indexed movement is not an outbound movement".into())
    }
}

pub(crate) fn outbound_prefix(evidence: &CheckedRunEvidence<'_>, upper: u64) -> Result<OutboundPrefix, String> {
    let mut reader = FileCheckedRunReader::open(evidence, None)?;
    if let Some(last) = upper.checked_sub(1) { reader.project_through(last)?; }
    let available = reader.next_seq.min(upper);
    if reader.rows.known(reader.ledger)? < available { return Err("ledger row index lost its contiguous durable prefix".into()); }
    let file = std::fs::File::open(evidence.movements_path).map_err(|error| error.to_string())?;
    if LedgerFile::of(&file)? != reader.ledger { return Err("ledger row index lost its exact ledger".into()); }
    Ok(OutboundPrefix { available, index: reader.rows.clone(), ledger: reader.ledger, file, max_row_bytes: evidence.max_control_frame_bytes })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BlobFingerprint {
    device: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

#[cfg(unix)]
fn blob_fingerprint(path: &Path) -> Option<BlobFingerprint> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).ok().filter(|metadata| metadata.is_file())?;
    Some(BlobFingerprint {
        device: metadata.dev(), inode: metadata.ino(), len: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()), changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

#[cfg(not(unix))]
fn blob_fingerprint(_: &Path) -> Option<BlobFingerprint> {
    None
}

#[derive(Clone)]
struct WriterCallFacts {
    label: String,
    call: Result<(String, Option<Option<String>>), String>,
}

struct KnownWriterCall {
    manifest: BlobFingerprint,
    payload_path: PathBuf,
    payload: BlobFingerprint,
    facts: WriterCallFacts,
}

type KnownWriterCalls = (HashMap<(PathBuf, String), KnownWriterCall>, std::collections::VecDeque<(PathBuf, String)>);

fn known_writer_calls() -> Result<std::sync::MutexGuard<'static, KnownWriterCalls>, String> {
    static KNOWN: OnceLock<Mutex<KnownWriterCalls>> = OnceLock::new();
    KNOWN.get_or_init(Default::default).lock().map_err(|_| "writer call index lock is poisoned".to_string())
}

fn writer_call_facts(evidence: &CheckedRunEvidence<'_>, artifact: &ArtifactRef) -> Result<WriterCallFacts, String> {
    let manifest_path = evidence.blobs_dir.join(artifact.content.to_hex());
    let key = (manifest_path.clone(), artifact_ref_key(artifact));
    if let Some(known) = known_writer_calls()?.0.get(&key) {
        if blob_fingerprint(&manifest_path) == Some(known.manifest) && blob_fingerprint(&known.payload_path) == Some(known.payload) {
            return Ok(known.facts.clone());
        }
    }
    let (manifest, payload) = read_pinned_artifact_with_bounds(evidence.blobs_dir, evidence.max_manifest_bytes, evidence.max_payload_bytes, artifact)?;
    let call = serde_json::from_slice::<octopus_tool_handlers::ToolCallArtifact>(&payload).map_err(|error| error.to_string())
        .map(|call| {
            let ordered = call.arguments.get("ordered").is_some().then(|| call.arguments.pointer("/writes/1/expected_before_hash")
                .and_then(|value| value.as_str()).map(str::to_string));
            (call.tool, ordered)
        });
    let payload_path = manifest.payload_content_hash.as_deref().map(|hash| evidence.blobs_dir.join(hash));
    let facts = WriterCallFacts { label: manifest.label.clone(), call };
    let fingerprints = payload_path.as_ref().and_then(|path| blob_fingerprint(&manifest_path).zip(blob_fingerprint(path)));
    let verified = fingerprints.filter(|_| read_pinned_artifact_with_bounds(evidence.blobs_dir, evidence.max_manifest_bytes,
        evidence.max_payload_bytes, artifact).is_ok_and(|(_, again)| again == payload));
    if let (Some((manifest, payload)), Some(payload_path)) = (verified, payload_path) {
        if blob_fingerprint(&manifest_path) == Some(manifest) && blob_fingerprint(&payload_path) == Some(payload) {
            let mut known = known_writer_calls()?;
            if known.0.insert(key.clone(), KnownWriterCall { manifest, payload_path, payload, facts: facts.clone() }).is_none() {
                known.1.push_back(key);
                if known.1.len() > 4096 {
                    if let Some(oldest) = known.1.pop_front() { known.0.remove(&oldest); }
                }
            }
        }
    }
    Ok(facts)
}

pub(crate) fn writer_call_excluded(
    evidence: &CheckedRunEvidence<'_>, artifact: &ArtifactRef, writer: &CheckedRunWriter, before_hash: &str,
) -> Result<bool, String> {
    let facts = writer_call_facts(evidence, artifact)?;
    if facts.label != writer.call_label { return Ok(true); }
    let (tool, ordered) = facts.call?;
    Ok(tool != writer.tool || ordered.is_none_or(|expected| expected.as_deref() != Some(before_hash)))
}
