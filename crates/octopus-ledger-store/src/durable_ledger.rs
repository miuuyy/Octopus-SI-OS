use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use octopus_file_storage::metadata::FileStamp;
use octopus_ledger::{
    BlobStore, ContentHash, DurableMetadataContent, GrantProjection, Lamport, LedgerError, LedgerResult, Movement,
    MovementDraft, MovementLog, MovementMetadata, Seq, decode_durable_movement_row,
    encode_durable_movement_row, movement_recorded_at_unix_ms_now,
};

struct RowIndex {
    offsets: Vec<u64>,
    grant_contents: Vec<Option<ContentHash>>,
    clock: u64,
    committed_len: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LedgerIdentity {
    device: u64,
    inode: u64,
    len: u64,
}

impl LedgerIdentity {
    fn of(path: &Path) -> LedgerResult<Self> {
        let metadata = std::fs::metadata(path).map_err(|e| io_err(path, &e))?;
        Ok(Self::from_metadata(&metadata))
    }

    #[cfg(unix)]
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
        }
    }

    #[cfg(not(unix))]
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: 0,
            inode: 0,
            len: metadata.len(),
        }
    }
}

fn movement_metadata_root(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("metadata")
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MetadataFileFingerprint {
    identity: LedgerIdentity,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
}

impl MetadataFileFingerprint {
    fn from_metadata(path: &Path, metadata: &std::fs::Metadata) -> LedgerResult<Self> {
        Ok(Self {
            identity: LedgerIdentity::from_metadata(metadata),
            modified: metadata.modified().map_err(|error| io_err(path, &error))?,
            #[cfg(unix)]
            changed: {
                use std::os::unix::fs::MetadataExt;
                (metadata.ctime(), metadata.ctime_nsec())
            },
        })
    }
}

struct CachedMetadata {
    hash: ContentHash,
    fingerprint: MetadataFileFingerprint,
    value: Arc<serde_json::Value>,
    byte_len: usize,
}

#[derive(Default)]
struct MetadataValueCache {
    entries: VecDeque<CachedMetadata>,
    byte_len: usize,
}

impl MetadataValueCache {
    fn get(
        &mut self,
        hash: ContentHash,
        fingerprint: MetadataFileFingerprint,
    ) -> Option<Arc<serde_json::Value>> {
        let index = self.entries.iter().position(|entry| entry.hash == hash)?;
        let entry = self.entries.remove(index)?;
        if entry.fingerprint != fingerprint {
            self.byte_len -= entry.byte_len;
            return None;
        }
        let value = Arc::clone(&entry.value);
        self.entries.push_front(entry);
        Some(value)
    }

    fn insert(&mut self, entry: CachedMetadata) {
        const MAX_ENTRIES: usize = 64;
        const MAX_BYTES: usize = 8 * 1024 * 1024;
        if entry.byte_len > MAX_BYTES {
            return;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|cached| cached.hash == entry.hash)
        {
            self.byte_len -= self
                .entries
                .remove(index)
                .expect("cached metadata exists")
                .byte_len;
        }
        while self.entries.len() >= MAX_ENTRIES || self.byte_len + entry.byte_len > MAX_BYTES {
            self.byte_len -= self
                .entries
                .pop_back()
                .expect("bounded metadata cache is nonempty")
                .byte_len;
        }
        self.byte_len += entry.byte_len;
        self.entries.push_front(entry);
    }
}

#[derive(Default)]
struct MetadataCaches {
    values: Mutex<MetadataValueCache>,
    validated_grants: Mutex<HashMap<ContentHash, (MetadataFileFingerprint, Vec<String>)>>,
}

fn ledger_metadata_caches(root: &Path) -> Arc<MetadataCaches> {
    static CACHES: std::sync::OnceLock<Mutex<HashMap<PathBuf, Arc<MetadataCaches>>>> =
        std::sync::OnceLock::new();
    let mut caches = CACHES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(caches.entry(root.to_path_buf()).or_default())
}

pub struct DurableMovementDecoder {
    blobs: DurableBlobs,
    caches: Arc<MetadataCaches>,
}

impl DurableMovementDecoder {
    pub fn for_log(movements_path: impl AsRef<Path>) -> Self {
        let dir = movement_metadata_root(movements_path.as_ref());
        Self {
            caches: ledger_metadata_caches(&dir),
            blobs: DurableBlobs::from_dir(dir),
        }
    }

    pub fn open(movements_path: impl AsRef<Path>) -> LedgerResult<Self> {
        let root = movement_metadata_root(movements_path.as_ref());
        Ok(Self {
            caches: ledger_metadata_caches(&root),
            blobs: DurableBlobs::open(root)?,
        })
    }

    pub fn decode(&self, row: &[u8], line: usize) -> LedgerResult<Movement> {
        decode_durable_movement_row(row, line, |_, hash| self.metadata_value(hash, line))
    }

    pub fn decode_projection(
        &self,
        row: &[u8],
        line: usize,
    ) -> LedgerResult<(Movement, Option<ContentHash>)> {
        let mut grant_content = None;
        let mut movement = decode_durable_movement_row(row, line, |kind, hash| match kind {
            DurableMetadataContent::Grant => {
                let interfaces = self.validate_grant(hash, line)?;
                grant_content = Some(hash);
                Ok(serde_json::json!({"interfaces": interfaces}))
            }
            DurableMetadataContent::ArtifactBindingRefs => self.metadata_value(hash, line),
        })?;
        if let Some(MovementMetadata::Invocation(metadata)) = movement.invocation_metadata.as_mut()
        {
            if grant_content.is_none() {
                let bytes = serde_json::to_vec(&metadata.grant).map_err(|error| {
                    LedgerError::MalformedMovementMetadata {
                        line,
                        message: error.to_string(),
                    }
                })?;
                grant_content = Some(ContentHash::of_bytes(&bytes));
            }
            metadata.grant = GrantProjection {
                interfaces: std::mem::take(&mut metadata.grant.interfaces),
                ..GrantProjection::default()
            };
        }
        Ok((movement, grant_content))
    }

    pub fn validated_grant_count(&self) -> LedgerResult<usize> {
        self.caches.validated_grants
            .lock()
            .map(|grants| grants.len())
            .map_err(|_| LedgerError::MalformedMovementMetadata {
                line: 0,
                message: "movement metadata grant validation cache is poisoned".to_string(),
            })
    }

    fn metadata_fingerprint(&self, hash: ContentHash) -> LedgerResult<MetadataFileFingerprint> {
        let path = self.blobs.blob_path(&hash);
        match std::fs::metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(LedgerError::BlobMissing(hash))
            }
            Err(error) => Err(io_err(&path, &error)),
            Ok(metadata) => MetadataFileFingerprint::from_metadata(&path, &metadata),
        }
    }

    fn read_metadata(&self, hash: ContentHash, before: MetadataFileFingerprint) -> LedgerResult<Vec<u8>> {
        let bytes = self
            .blobs
            .try_get(&hash)?
            .ok_or(LedgerError::BlobMissing(hash))?;
        if before != self.metadata_fingerprint(hash)? {
            return Err(LedgerError::Io {
                path: self.blobs.blob_path(&hash).display().to_string(),
                message: "movement metadata content changed while being read".to_string(),
            });
        }
        Ok(bytes)
    }

    pub fn validate_grant(&self, hash: ContentHash, line: usize) -> LedgerResult<Vec<String>> {
        let fingerprint = self.metadata_fingerprint(hash)?;
        let validated = self
            .caches
            .validated_grants
            .lock()
            .map_err(|_| LedgerError::MalformedMovementMetadata {
                line,
                message: "movement metadata grant validation cache is poisoned".to_string(),
            })?
            .get(&hash)
            .cloned();
        if let Some((prior, interfaces)) = validated {
            if prior == fingerprint {
                return Ok(interfaces);
            }
        }
        let bytes = self.read_metadata(hash, fingerprint)?;
        let grant = serde_json::from_slice::<GrantProjection>(&bytes).map_err(|error| {
            LedgerError::MalformedMovementMetadata {
                line,
                message: format!("content-addressed invocation grant is malformed: {error}"),
            }
        })?;
        self.caches.validated_grants
            .lock()
            .map_err(|_| LedgerError::MalformedMovementMetadata {
                line,
                message: "movement metadata grant validation cache is poisoned".to_string(),
            })?
            .insert(hash, (fingerprint, grant.interfaces.clone()));
        Ok(grant.interfaces)
    }

    pub fn validate_metadata_content(&self, kind: DurableMetadataContent, hash: ContentHash, line: usize) -> LedgerResult<()> {
        match kind {
            DurableMetadataContent::Grant => self.validate_grant(hash, line).map(|_| ()),
            DurableMetadataContent::ArtifactBindingRefs => {
                let refs: Vec<octopus_ledger::ArtifactRef> = serde_json::from_value(self.metadata_value(hash, line)?)
                    .map_err(|error| LedgerError::MalformedMovementMetadata { line, message: error.to_string() })?;
                if refs.is_empty() {
                    return Err(LedgerError::MalformedMovementMetadata { line,
                        message: "content-addressed artifact binding refs are empty".into() });
                }
                Ok(())
            }
        }
    }

    fn metadata_value(&self, hash: ContentHash, line: usize) -> LedgerResult<serde_json::Value> {
        let fingerprint = self.metadata_fingerprint(hash)?;
        let cached = self
            .caches
            .values
            .lock()
            .map_err(|_| LedgerError::MalformedMovementMetadata {
                line,
                message: "movement metadata value cache is poisoned".to_string(),
            })?
            .get(hash, fingerprint);
        if let Some(value) = cached {
            return Ok((*value).clone());
        }
        let bytes = self.read_metadata(hash, fingerprint)?;
        let grant = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
            LedgerError::MalformedMovementMetadata {
                line,
                message: format!("content-addressed movement metadata is malformed: {error}"),
            }
        })?;
        let value = Arc::new(grant);
        self.caches.values
            .lock()
            .map_err(|_| LedgerError::MalformedMovementMetadata {
                line,
                message: "movement metadata value cache is poisoned".to_string(),
            })?
            .insert(CachedMetadata {
                hash,
                fingerprint,
                value: Arc::clone(&value),
                byte_len: bytes.len(),
            });
        Ok((*value).clone())
    }
}

pub struct DurableLog {
    path: PathBuf,
    file: File,
    metadata: DurableMovementDecoder,
    offsets: Vec<u64>,
    grant_contents: Vec<Option<ContentHash>>,
    clock: u64,
    end_offset: u64,
    identity: LedgerIdentity,
}

impl DurableLog {
    pub fn open(path: impl AsRef<Path>) -> LedgerResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| io_err(parent, &e))?;
            }
        }
        let metadata = DurableMovementDecoder::open(&path)?;
        let RowIndex {
            offsets,
            grant_contents,
            clock,
            committed_len,
        } = Self::index_rows(&path, &metadata)?;
        Self::discard_torn_tail(&path, committed_len)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| io_err(&path, &e))?;
        let file_metadata = file.metadata().map_err(|e| io_err(&path, &e))?;
        let end_offset = file_metadata.len();
        let identity = LedgerIdentity::from_metadata(&file_metadata);
        Ok(Self {
            path,
            file,
            metadata,
            offsets,
            grant_contents,
            clock,
            end_offset,
            identity,
        })
    }

    fn require_same_ledger(&self) -> LedgerResult<()> {
        let current = LedgerIdentity::of(&self.path)?;
        if current == self.identity {
            return Ok(());
        }
        Err(LedgerError::Io {
            path: self.path.display().to_string(),
            message: format!(
                "durable ledger changed underneath its offset index \
                 (indexed {} bytes at device {}/inode {}, found {} bytes at device {}/inode {}); \
                 one index pairs with one ledger",
                self.identity.len,
                self.identity.device,
                self.identity.inode,
                current.len,
                current.device,
                current.inode,
            ),
        })
    }

    fn index_rows(path: &Path, metadata: &DurableMovementDecoder) -> LedgerResult<RowIndex> {
        let mut offsets = Vec::new();
        let mut grant_contents = Vec::new();
        let mut clock = 0u64;
        let committed_len = Self::visit_projected_rows_from(
            path,
            metadata,
            0,
            0,
            &mut |offset, movement, grant_content| {
                clock = clock.max(movement.lamport.0);
                offsets.push(offset);
                grant_contents.push(grant_content);
                ControlFlow::Continue(())
            },
        )?;
        Ok(RowIndex {
            offsets,
            grant_contents,
            clock,
            committed_len,
        })
    }

    fn visit_projected_rows_from(
        path: &Path,
        metadata: &DurableMovementDecoder,
        start_offset: u64,
        start_seq: usize,
        visit: &mut dyn FnMut(u64, Movement, Option<ContentHash>) -> ControlFlow<()>,
    ) -> LedgerResult<u64> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(io_err(path, &e)),
        };
        let mut reader = BufReader::with_capacity(1 << 20, file);
        reader
            .seek(SeekFrom::Start(start_offset))
            .map_err(|e| io_err(path, &e))?;
        let mut offset = start_offset;
        let mut seq = start_seq;
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let read = reader
                .read_until(b'\n', &mut buf)
                .map_err(|e| io_err(path, &e))?;
            if read == 0 || buf.last() != Some(&b'\n') {
                break;
            }
            let segment = &buf[..read - 1];
            if !segment.iter().all(|b| b.is_ascii_whitespace()) {
                let (movement, grant_content) = metadata.decode_projection(segment, seq)?;
                seq += 1;
                if visit(offset, movement, grant_content).is_break() {
                    return Ok(offset + read as u64);
                }
            }
            offset += read as u64;
        }
        Ok(offset)
    }

    fn visit_rows_from(
        path: &Path,
        metadata: &DurableMovementDecoder,
        start_offset: u64,
        start_seq: usize,
        end_offset: Option<u64>,
        visit: &mut dyn FnMut(u64, Movement) -> ControlFlow<()>,
    ) -> LedgerResult<u64> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(io_err(path, &e)),
        };
        file.seek(SeekFrom::Start(start_offset))
            .map_err(|e| io_err(path, &e))?;
        let maximum_bytes = match end_offset {
            Some(end) => end
                .checked_sub(start_offset)
                .ok_or_else(|| LedgerError::Io {
                    path: path.display().to_string(),
                    message: "durable ledger row range ends before its start".to_string(),
                })?,
            None => u64::MAX,
        };
        let mut reader = BufReader::with_capacity(1 << 20, file.take(maximum_bytes));
        let mut offset = start_offset;
        let mut seq = start_seq;
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let read = reader
                .read_until(b'\n', &mut buf)
                .map_err(|e| io_err(path, &e))?;
            if read == 0 || buf.last() != Some(&b'\n') {
                break;
            }
            let segment = &buf[..read - 1];
            if !segment.iter().all(|b| b.is_ascii_whitespace()) {
                let movement = metadata.decode(segment, seq)?;
                seq += 1;
                if visit(offset, movement).is_break() {
                    return Ok(offset + read as u64);
                }
            }
            offset += read as u64;
        }
        Ok(offset)
    }

    fn discard_torn_tail(path: &Path, committed_len: u64) -> LedgerResult<()> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_err(path, &e)),
        };
        if metadata.len() <= committed_len {
            return Ok(());
        }
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| io_err(path, &e))?;
        file.set_len(committed_len).map_err(|e| io_err(path, &e))?;
        file.sync_all().map_err(|e| io_err(path, &e))?;
        Ok(())
    }

    pub fn replay(path: impl AsRef<Path>) -> LedgerResult<Vec<Movement>> {
        let metadata = DurableMovementDecoder::open(path.as_ref())?;
        let mut out = Vec::new();
        Self::visit_rows_from(path.as_ref(), &metadata, 0, 0, None, &mut |_, movement| {
            out.push(movement);
            ControlFlow::Continue(())
        })?;
        Ok(out)
    }

    pub fn try_append(&mut self, draft: MovementDraft) -> LedgerResult<Movement> {
        if draft.trace.0.trim().is_empty() {
            return Err(LedgerError::MissingTrace {
                line: self.offsets.len(),
            });
        }
        self.require_same_ledger()?;
        let floor = draft.causal_floor.map(|l| l.0).unwrap_or(0);
        let clock = self
            .clock
            .max(floor)
            .checked_add(1)
            .ok_or_else(|| LedgerError::Io {
                path: self.path.display().to_string(),
                message: "durable ledger Lamport clock overflowed".to_string(),
            })?;
        let placed = Movement {
            seq: Seq(self.offsets.len() as u64),
            lamport: Lamport(clock),
            recorded_at_unix_ms: movement_recorded_at_unix_ms_now(),
            trace: draft.trace,
            kind: draft.kind,
            from: draft.from,
            to: draft.to,
            artifact: draft.artifact,
            invocation_metadata: draft.invocation_metadata,
        };
        let mut grant_content = None;
        let mut pending_metadata_sync = Vec::new();
        let mut json = encode_durable_movement_row(&placed, self.offsets.len(), |kind, bytes| {
            let staged = self.metadata.blobs.stage_blob(bytes)?;
            if let Some(entry) = staged.pending_sync {
                pending_metadata_sync.push(entry);
            }
            if kind == DurableMetadataContent::Grant {
                grant_content = Some(staged.hash);
            }
            Ok(staged.hash)
        })?;
        self.metadata
            .blobs
            .finish_staged_dir_sync(&pending_metadata_sync)?;
        json.push('\n');
        self.file
            .write_all(json.as_bytes())
            .map_err(|e| io_err(&self.path, &e))?;
        self.file.flush().map_err(|e| io_err(&self.path, &e))?;
        self.file.sync_all().map_err(|e| io_err(&self.path, &e))?;
        self.clock = clock;
        self.offsets.push(self.end_offset);
        self.grant_contents.push(grant_content);
        self.end_offset = self
            .end_offset
            .checked_add(json.len() as u64)
            .expect("durable ledger byte offset overflowed u64");
        self.identity.len = self.end_offset;
        Ok(placed)
    }

    pub fn movements_from(&self, from: Seq) -> Vec<Movement> {
        let mut out = Vec::new();
        self.scan_from(from, &mut |movement| {
            out.push(movement);
            ControlFlow::Continue(())
        });
        out
    }

    pub fn try_movements_from(&self, from: Seq) -> LedgerResult<Vec<Movement>> {
        self.require_same_ledger()?;
        let start = from.0 as usize;
        let Some(&offset) = self.offsets.get(start) else {
            return Ok(Vec::new());
        };
        let mut movements = Vec::new();
        Self::visit_rows_from(
            &self.path,
            &self.metadata,
            offset,
            start,
            Some(self.end_offset),
            &mut |_, movement| {
                movements.push(movement);
                ControlFlow::Continue(())
            },
        )?;
        Ok(movements)
    }

    pub fn try_boot_movements_from(&self, from: Seq) -> LedgerResult<Vec<Movement>> {
        self.require_same_ledger()?;
        let start = from.0 as usize;
        let Some(&offset) = self.offsets.get(start) else {
            return Ok(Vec::new());
        };
        let mut movements = Vec::new();
        Self::visit_projected_rows_from(
            &self.path,
            &self.metadata,
            offset,
            start,
            &mut |_, movement, _| {
                movements.push(movement);
                ControlFlow::Continue(())
            },
        )?;
        Ok(movements)
    }

    pub fn try_movement_at(&self, seq: Seq) -> LedgerResult<Option<Movement>> {
        self.require_same_ledger()?;
        let start = seq.0 as usize;
        let Some(&offset) = self.offsets.get(start) else {
            return Ok(None);
        };
        let mut movement = None;
        let end_offset = self
            .offsets
            .get(start + 1)
            .copied()
            .unwrap_or(self.end_offset);
        Self::visit_rows_from(
            &self.path,
            &self.metadata,
            offset,
            start,
            Some(end_offset),
            &mut |_, value| {
                movement = Some(value);
                ControlFlow::Break(())
            },
        )?;
        if movement
            .as_ref()
            .is_some_and(|movement| movement.seq != seq)
        {
            return Err(LedgerError::Io {
                path: self.path.display().to_string(),
                message: format!(
                    "durable movement at indexed sequence {} carries another sequence",
                    seq.0
                ),
            });
        }
        Ok(movement)
    }

    pub fn grant_content_at(&self, seq: Seq) -> Option<ContentHash> {
        usize::try_from(seq.0)
            .ok()
            .and_then(|index| self.grant_contents.get(index))
            .copied()
            .flatten()
    }

    pub fn validated_grant_count(&self) -> LedgerResult<usize> {
        self.metadata.validated_grant_count()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl MovementLog for DurableLog {
    fn append(&mut self, draft: MovementDraft) -> Movement {
        match self.try_append(draft) {
            Ok(m) => m,
            Err(e) => {
                panic!("durable ledger append failed (fail-closed, not skipped): {e}");
            }
        }
    }

    fn read_from(&self, from: Seq) -> Vec<Movement> {
        self.movements_from(from)
    }

    fn read_at(&self, seq: Seq) -> Option<Movement> {
        self.try_movement_at(seq).unwrap_or_else(|error| {
            panic!("durable ledger read failed (fail-closed, not skipped): {error}")
        })
    }

    fn scan_from(&self, from: Seq, visit: &mut dyn FnMut(Movement) -> ControlFlow<()>) {
        if let Err(e) = self.require_same_ledger() {
            panic!("durable ledger read failed (fail-closed, not skipped): {e}");
        }
        let start = from.0 as usize;
        let Some(&offset) = self.offsets.get(start) else {
            return;
        };
        if let Err(e) = Self::visit_rows_from(
            &self.path,
            &self.metadata,
            offset,
            start,
            Some(self.end_offset),
            &mut |_, movement| visit(movement),
        ) {
            panic!("durable ledger read failed (fail-closed, not skipped): {e}");
        }
    }

    fn len(&self) -> usize {
        self.offsets.len()
    }

    fn durable_end_offset(&self) -> Option<u64> {
        Some(self.end_offset)
    }

    fn durable_row_byte_range(&self, seq: Seq) -> Option<(u64, u64)> {
        let index = usize::try_from(seq.0).ok()?;
        let start = *self.offsets.get(index)?;
        let end = self
            .offsets
            .get(index.checked_add(1)?)
            .copied()
            .unwrap_or(self.end_offset);
        Some((start, end))
    }
}

const SYNCED_BLOB_DIR_CACHE_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct SyncedBlobEntry {
    hash: ContentHash,
    stamp: FileStamp,
}

#[derive(Default)]
struct SyncedBlobDirCache {
    entries: HashSet<SyncedBlobEntry>,
    order: VecDeque<SyncedBlobEntry>,
}

impl SyncedBlobDirCache {
    fn contains(&self, entry: &SyncedBlobEntry) -> bool {
        self.entries.contains(entry)
    }

    fn record(&mut self, entry: SyncedBlobEntry) {
        if self.entries.insert(entry) {
            self.order.push_back(entry);
            while self.entries.len() > SYNCED_BLOB_DIR_CACHE_LIMIT {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
        }
    }
}

struct StagedBlob {
    hash: ContentHash,
    pending_sync: Option<SyncedBlobEntry>,
}

pub struct DurableBlobs {
    dir: PathBuf,
    synced_dir_entries: SyncedBlobDirCache,
    #[cfg(test)]
    dir_syncs: usize,
}

static BLOB_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl DurableBlobs {
    fn from_dir(dir: PathBuf) -> Self {
        Self {
            dir,
            synced_dir_entries: SyncedBlobDirCache::default(),
            #[cfg(test)]
            dir_syncs: 0,
        }
    }

    pub fn open(dir: impl AsRef<Path>) -> LedgerResult<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, &e))?;
        Ok(Self::from_dir(dir))
    }

    fn blob_path(&self, hash: &ContentHash) -> PathBuf {
        self.dir.join(hash.to_hex())
    }

    pub fn try_put(&mut self, bytes: &[u8]) -> LedgerResult<ContentHash> {
        Ok(self.try_put_batch(&[bytes])?[0])
    }

    /// Formation evidence is durable before any movement may reference it:
    /// sync every blob's data, then its directory and the device before returning.
    pub fn try_put_batch(&mut self, contents: &[&[u8]]) -> LedgerResult<Vec<ContentHash>> {
        let mut hashes = Vec::with_capacity(contents.len());
        let mut pending_sync = Vec::new();
        for bytes in contents {
            let staged = self.stage_blob(bytes)?;
            hashes.push(staged.hash);
            if let Some(entry) = staged.pending_sync {
                pending_sync.push(entry);
            }
        }
        self.finish_staged_dir_sync(&pending_sync)?;
        Ok(hashes)
    }

    fn finish_staged_dir_sync(&mut self, pending: &[SyncedBlobEntry]) -> LedgerResult<()> {
        if pending.is_empty() {
            return Ok(());
        }
        sync_directory(&self.dir)?;
        #[cfg(test)]
        {
            self.dir_syncs += 1;
        }
        for entry in pending {
            self.synced_dir_entries.record(*entry);
        }
        Ok(())
    }

    fn stage_blob(&mut self, bytes: &[u8]) -> LedgerResult<StagedBlob> {
        let hash = ContentHash::of_bytes(bytes);
        let path = self.blob_path(&hash);
        if let Some(stamp) = verify_existing_blob(&path, &hash)? {
            let entry = SyncedBlobEntry { hash, stamp };
            let pending_sync = (!self.synced_dir_entries.contains(&entry)).then_some(entry);
            return Ok(StagedBlob { hash, pending_sync });
        }
        let tmp = self.dir.join(format!(
            ".{}.{}.{}.tmp",
            hash.to_hex(),
            std::process::id(),
            BLOB_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut f = octopus_file_storage::write_new(&tmp, bytes)
            .map_err(|error| io_err(&tmp, &error))?;
        let result = (|| {
            f.flush().map_err(|e| io_err(&tmp, &e))?;
            verify_blob_file(&tmp, &hash)?;
            sync_blob_data(&f).map_err(|e| io_err(&tmp, &e))?;
            let existing_stamp = match std::fs::hard_link(&tmp, &path) {
                Ok(()) => None,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Some(verify_blob_file_with_stamp(&path, &hash)?)
                }
                Err(error) => return Err(io_err(&path, &error)),
            };
            std::fs::remove_file(&tmp).map_err(|error| io_err(&tmp, &error))?;
            let stamp = match existing_stamp {
                Some(stamp) => stamp,
                None => FileStamp::of(&f.metadata().map_err(|error| io_err(&path, &error))?),
            };
            Ok(StagedBlob {
                hash,
                pending_sync: Some(SyncedBlobEntry { hash, stamp }),
            })
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    pub fn try_get(&self, hash: &ContentHash) -> LedgerResult<Option<Vec<u8>>> {
        let path = self.blob_path(hash);
        match std::fs::read(&path) {
            Ok(b) => {
                let actual = ContentHash::of_bytes(&b);
                if &actual != hash {
                    return Err(LedgerError::BlobIntegrity {
                        expected: *hash,
                        actual,
                    });
                }
                Ok(Some(b))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(&path, &e)),
        }
    }
}

#[cfg(target_vendor = "apple")]
fn sync_blob_data(file: &File) -> std::io::Result<()> {
    rustix::fs::fsync(file).map_err(Into::into)
}

#[cfg(not(target_vendor = "apple"))]
fn sync_blob_data(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

pub(super) fn verify_blob_file(path: &Path, expected: &ContentHash) -> LedgerResult<()> {
    verify_blob_file_with_stamp(path, expected).map(|_| ())
}

fn verify_existing_blob(
    path: &Path,
    expected: &ContentHash,
) -> LedgerResult<Option<FileStamp>> {
    match File::open(path) {
        Ok(file) => verify_open_blob_file(file, path, expected).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_err(path, &error)),
    }
}

fn verify_blob_file_with_stamp(
    path: &Path,
    expected: &ContentHash,
) -> LedgerResult<FileStamp> {
    let file = File::open(path).map_err(|error| io_err(path, &error))?;
    verify_open_blob_file(file, path, expected)
}

fn verify_open_blob_file(
    mut file: File,
    path: &Path,
    expected: &ContentHash,
) -> LedgerResult<FileStamp> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| io_err(path, &error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = ContentHash::from_hex(hasher.finalize().to_hex().as_str())
        .expect("BLAKE3 always renders a canonical 32-byte hex digest");
    if &actual != expected {
        return Err(LedgerError::BlobIntegrity {
            expected: *expected,
            actual,
        });
    }
    let metadata = file.metadata().map_err(|error| io_err(path, &error))?;
    Ok(FileStamp::of(&metadata))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> LedgerResult<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_err(path, &error))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> LedgerResult<()> {
    Ok(())
}

impl BlobStore for DurableBlobs {
    fn put(&mut self, bytes: &[u8]) -> ContentHash {
        match self.try_put(bytes) {
            Ok(h) => h,
            Err(e) => panic!("durable blob put failed (fail-closed, not skipped): {e}"),
        }
    }
    fn put_batch(&mut self, contents: &[&[u8]]) -> Vec<ContentHash> {
        self.try_put_batch(contents)
            .unwrap_or_else(|error| panic!("durable blob batch put failed: {error}"))
    }
    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        match self.try_get(hash) {
            Ok(opt) => opt,
            Err(e) => panic!("durable blob get failed (fail-closed, not skipped): {e}"),
        }
    }
    fn contains(&self, hash: &ContentHash) -> bool {
        self.blob_path(hash).exists()
    }
}

fn io_err(path: impl AsRef<Path>, e: &std::io::Error) -> LedgerError {
    LedgerError::Io {
        path: path.as_ref().display().to_string(),
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{ArtifactId, CheckpointId, InterfaceId, RunId};
    use octopus_ledger::{
        ArtifactRef, CheckpointAdmissionMetadata, Endpoint, InMemoryBlobs, MovementKind,
        RuntimeInvocationMetadata, TraceId,
    };

    fn run(s: &str) -> Endpoint {
        Endpoint::Interface(InterfaceId::new(s))
    }

    fn artifact_draft(
        trace: &str,
        from: Endpoint,
        to: Endpoint,
        aref: ArtifactRef,
        floor: Option<Lamport>,
    ) -> MovementDraft {
        MovementDraft {
            trace: TraceId(trace.to_string()),
            kind: MovementKind::ArtifactMoved,
            from,
            to,
            artifact: Some(aref),
            invocation_metadata: None,
            causal_floor: floor,
        }
    }

    #[test]
    fn durable_log_replays_bit_identical() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("movements.jsonl");
        let blob_dir = dir.path().join("blobs");

        let mut blobs = DurableBlobs::open(&blob_dir).unwrap();
        let content = blobs.try_put(br#"{"label":"a1","payload":"hi"}"#).unwrap();
        let aref = ArtifactRef {
            id: ArtifactId::new("a1"),
            content,
        };

        let placed: Vec<Movement> = {
            let mut log = DurableLog::open(&log_path).unwrap();
            let m0 = log.append(MovementDraft {
                trace: TraceId("t".into()),
                kind: MovementKind::ArtifactMoved,
                from: run("r1"),
                to: run("r1"),
                artifact: None,
                invocation_metadata: None,
                causal_floor: None,
            });
            let m1 = log
                .try_append(artifact_draft(
                    "t",
                    run("r1"),
                    run("r2"),
                    aref.clone(),
                    Some(m0.lamport),
                ))
                .unwrap();
            let m2 = log.append(MovementDraft {
                trace: TraceId("t".into()),
                kind: MovementKind::ArtifactMoved,
                from: run("r2"),
                to: run("r2"),
                artifact: None,
                invocation_metadata: Some(
                    octopus_ledger::InvocationMetadata::new(octopus_ledger::GrantProjection {
                        tools: vec!["coder.exec".to_string()],
                        ..Default::default()
                    }).with_artifact_binding_refs(vec![aref.clone()])
                    .into(),
                ),
                causal_floor: None,
            });
            assert_eq!(m0.seq, Seq(0));
            assert_eq!(m1.seq, Seq(1));
            assert_eq!(m2.seq, Seq(2));
            assert!(m1.lamport > m0.lamport, "lamport respects causal floor");
            vec![m0, m1, m2]
        };
        let durable_rows = std::fs::read_to_string(&log_path).unwrap();
        assert!(durable_rows.contains("\"grant_content\""));
        assert!(durable_rows.contains("\"artifact_binding_refs_content\""));
        assert!(!durable_rows.contains("\"artifact_binding_refs\""));
        assert!(!durable_rows.contains("\"grant\":{\""));
        assert_eq!(
            std::fs::read_dir(dir.path().join("metadata"))
                .unwrap()
                .count(),
            2
        );

        let reopened = DurableLog::open(&log_path).unwrap();
        let replayed = reopened.read_from(Seq(0));
        assert_eq!(replayed, placed, "durable log replays bit-identically");
        assert_eq!(reopened.len(), 3);
        assert_eq!(reopened.read_from(Seq(1)).len(), 2);

        let mut reopened = reopened;
        let m3 = reopened.append(MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::ArtifactMoved,
            from: run("r1"),
            to: run("r1"),
            artifact: None,
            invocation_metadata: None,
            causal_floor: None,
        });
        assert_eq!(m3.seq, Seq(3), "Seq continues after reopen");
        assert!(
            m3.lamport > placed[2].lamport,
            "Lamport continues after reopen (clock recovered from durable max)"
        );

        drop(reopened);
        let final_log = DurableLog::open(&log_path).unwrap();
        assert_eq!(final_log.len(), 4);
    }

    #[test]
    fn boot_fails_closed_when_content_addressed_metadata_is_missing_or_corrupt() {
        for (field, corrupt) in ["grant_content", "artifact_binding_refs_content"].into_iter().flat_map(|field| [false, true].map(|corrupt| (field, corrupt))) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("movements.jsonl");
            let mut log = DurableLog::open(&path).unwrap();
            log.append(MovementDraft {
                trace: TraceId("t".into()),
                kind: MovementKind::ArtifactMoved,
                from: run("r1"),
                to: run("r2"),
                artifact: None,
                invocation_metadata: Some(
                    octopus_ledger::InvocationMetadata::new(octopus_ledger::GrantProjection {
                        tools: vec!["coder.exec".to_string()],
                        ..Default::default()
                    }).with_artifact_binding_refs(vec![ArtifactRef { id: ArtifactId::new("binding"), content: ContentHash::of_bytes(b"binding") }])
                    .into(),
                ),
                causal_floor: None,
            });
            drop(log);

            let row: serde_json::Value = serde_json::from_str(std::fs::read_to_string(&path).unwrap().trim()).unwrap();
            let grant = dir.path().join("metadata").join(row["invocation_metadata"][field].as_str().unwrap());
            if corrupt {
                std::fs::write(&grant, b"corrupt grant evidence").unwrap();
            } else {
                std::fs::remove_file(&grant).unwrap();
            }

            let error = DurableLog::open(&path)
                .err()
                .expect("boot must reject missing or corrupt grant evidence");
            assert!(
                matches!(
                    (corrupt, error),
                    (false, LedgerError::BlobMissing(_))
                        | (true, LedgerError::BlobIntegrity { .. })
                ),
                "unexpected boot failure for corrupt={corrupt}"
            );
        }
    }

    #[test]
    fn boot_projection_validates_unique_grants_once_and_preserves_recovery_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let runtime = RuntimeInvocationMetadata {
            run: RunId::new("run_a"),
            ee: octopus_descriptors::EeId::new("ee_a"),
            worker: InterfaceId::new("system_worker_flash"),
            checkpoint: CheckpointId::new("checkpoint_a"),
            interface: InterfaceId::new("world_input"),
            epoch: 4,
            run_record_hash: Some("record-a".to_string()),
        };
        let metadata = octopus_ledger::InvocationMetadata::new(octopus_ledger::GrantProjection {
            tools: vec!["coder.exec".to_string()],
            interfaces: vec!["world_output".to_string()],
            ..Default::default()
        })
        .with_runtime(runtime.clone())
        .with_outbound_interface_bindings(vec![InterfaceId::new("world_output")])
        .with_checkpoint_admission(CheckpointAdmissionMetadata {
            artifact_label: "input.exact".to_string(),
            required_artifact_labels: Vec::new(),
            complete: true,
        });
        let mut log = DurableLog::open(&path).unwrap();
        for artifact in ["artifact-a", "artifact-b"] {
            log.append(MovementDraft {
                trace: TraceId("trace-a".into()),
                kind: MovementKind::InterfaceArrival,
                from: Endpoint::Interface(InterfaceId::new("world_input")),
                to: Endpoint::Interface(runtime.worker.clone()),
                artifact: Some(ArtifactRef {
                    id: ArtifactId::new(artifact),
                    content: ContentHash::of_bytes(artifact.as_bytes()),
                }),
                invocation_metadata: Some(metadata.clone().into()),
                causal_floor: None,
            });
        }
        drop(log);

        let reopened = DurableLog::open(&path).unwrap();
        assert_eq!(reopened.validated_grant_count().unwrap(), 1);
        let projected = reopened.try_boot_movements_from(Seq(0)).unwrap();
        assert_eq!(reopened.validated_grant_count().unwrap(), 1);
        assert_eq!(projected.len(), 2);
        let projection = projected[0]
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation())
            .unwrap();
        assert_eq!(projection.runtime.as_ref(), Some(&runtime));
        assert_eq!(
            projection
                .checkpoint_admission
                .as_ref()
                .unwrap()
                .artifact_label,
            "input.exact"
        );
        assert_eq!(
            projection.outbound_interface_bindings,
            vec![InterfaceId::new("world_output")]
        );
        assert!(projection.grant.tools.is_empty());
        assert_eq!(projection.grant.interfaces, ["world_output"]);
        let pinned_content = reopened.grant_content_at(Seq(0)).unwrap();
        let first_row = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        let mut inline_row = serde_json::from_str::<serde_json::Value>(&first_row).unwrap();
        let inline_metadata = inline_row["invocation_metadata"].as_object_mut().unwrap();
        let pinned_hex = inline_metadata
            .remove("grant_content")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(pinned_hex, pinned_content.to_hex());
        let inline_grant = std::fs::read(movement_metadata_root(&path).join(&pinned_hex)).unwrap();
        inline_metadata.insert(
            "grant".to_string(),
            serde_json::from_slice(&inline_grant).unwrap(),
        );
        let inline_row = serde_json::to_vec(&inline_row).unwrap();
        let inline_decoder = DurableMovementDecoder::open(&path).unwrap();
        let (_, inline_content) = inline_decoder.decode_projection(&inline_row, 0).unwrap();
        assert_eq!(inline_content, Some(pinned_content));
        let exact = reopened.try_movement_at(Seq(0)).unwrap().unwrap();
        assert_eq!(
            exact
                .invocation_metadata
                .as_ref()
                .and_then(|metadata| metadata.invocation())
                .unwrap()
                .grant
                .tools,
            vec!["coder.exec".to_string()]
        );
    }

    #[test]
    fn repeated_binding_refs_are_pinned_once_and_keep_the_exact_grant_projection() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("movements.jsonl");
        let refs = (0..512).map(|index| ArtifactRef {
            id: ArtifactId::new(format!("binding-{index}")),
            content: ContentHash::of_bytes(index.to_string().as_bytes()),
        }).collect::<Vec<_>>();
        let metadata = octopus_ledger::InvocationMetadata::new(GrantProjection {
            interfaces: vec!["declared".into()], ..Default::default()
        }).with_artifact_binding_refs(refs.clone());
        let mut log = DurableLog::open(&path).unwrap();
        for _ in 0..16 {
            log.try_append(MovementDraft { trace: TraceId("repeat".into()), kind: MovementKind::ArtifactMoved,
                from: run("source"), to: run("target"), artifact: None, invocation_metadata: Some(metadata.clone().into()), causal_floor: None }).unwrap();
        }
        drop(log);
        assert!(std::fs::metadata(&path).unwrap().len() < 16 * 1024);
        assert_eq!(std::fs::read_dir(root.path().join("metadata")).unwrap().count(), 2);
        let decoder = DurableMovementDecoder::for_log(&path);
        let rows = std::fs::read_to_string(&path).unwrap();
        let first = rows.lines().next().unwrap();
        let raw: serde_json::Value = serde_json::from_str(first).unwrap();
        let (projected, grant) = decoder.decode_projection(first.as_bytes(), 0).unwrap();
        assert_eq!(grant.unwrap().to_hex(), raw["invocation_metadata"]["grant_content"].as_str().unwrap());
        assert_eq!(projected.invocation_metadata.unwrap().invocation().unwrap().artifact_binding_refs, refs);
        let mut ambiguous = raw.clone();
        ambiguous["invocation_metadata"]["artifact_binding_refs"] = serde_json::json!(refs);
        assert!(decoder.decode(ambiguous.to_string().as_bytes(), 0).is_err());
        let mut invalid_address = raw;
        invalid_address["invocation_metadata"]["artifact_binding_refs_content"] = serde_json::json!("NOT-A-HASH");
        assert!(decoder.decode(invalid_address.to_string().as_bytes(), 0).is_err());
    }

    #[test]
    fn warm_grant_reads_revalidate_file_generations_and_bound_retention() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let decoder = DurableMovementDecoder::open(&path).unwrap();
        let metadata_root = movement_metadata_root(&path);
        let mut rows = Vec::new();
        let mut hashes = Vec::new();
        for i in 0..70 {
            let mut movement = torn_row(i, i + 1);
            movement.invocation_metadata = Some(
                octopus_ledger::InvocationMetadata::new(GrantProjection {
                    tools: vec![format!("tool-{i}")],
                    ..Default::default()
                })
                .into(),
            );
            rows.push(
                encode_durable_movement_row(&movement, i as usize, |_, bytes| {
                    let hash = ContentHash::of_bytes(bytes);
                    std::fs::write(metadata_root.join(hash.to_hex()), bytes).unwrap();
                    hashes.push(hash);
                    Ok(hash)
                })
                .unwrap(),
            );
        }
        for (i, row) in rows.iter().enumerate().chain(rows.iter().enumerate().rev()) {
            let decoded = decoder.decode(row.as_bytes(), i).unwrap();
            assert_eq!(
                decoded
                    .invocation_metadata
                    .unwrap()
                    .invocation()
                    .unwrap()
                    .grant
                    .tools,
                vec![format!("tool-{i}")]
            );
            decoder.decode_projection(row.as_bytes(), i).unwrap();
        }
        assert!(decoder.caches.values.lock().unwrap().entries.len() <= 64);
        let grant_path = metadata_root.join(hashes[0].to_hex());
        let original = std::fs::read(&grant_path).unwrap();
        let modified = std::fs::metadata(&grant_path).unwrap().modified().unwrap();
        let replacement = metadata_root.join("replacement");
        std::fs::write(&replacement, vec![b' '; original.len()]).unwrap();
        File::options()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        std::fs::rename(replacement, &grant_path).unwrap();
        assert!(matches!(
            decoder.decode(rows[0].as_bytes(), 0),
            Err(LedgerError::BlobIntegrity { .. })
        ));
        assert!(matches!(
            decoder.decode_projection(rows[0].as_bytes(), 0),
            Err(LedgerError::BlobIntegrity { .. })
        ));
        std::fs::write(&grant_path, original).unwrap();
        decoder.decode(rows[0].as_bytes(), 0).unwrap();
        decoder.decode_projection(rows[0].as_bytes(), 0).unwrap();
        std::fs::remove_file(grant_path).unwrap();
        assert!(matches!(
            decoder.decode(rows[0].as_bytes(), 0),
            Err(LedgerError::BlobMissing(_))
        ));
        assert!(matches!(
            decoder.decode_projection(rows[0].as_bytes(), 0),
            Err(LedgerError::BlobMissing(_))
        ));
    }

    #[test]
    fn durable_blobs_round_trip_and_idempotent_put() {
        let dir = tempfile::tempdir().unwrap();
        let mut blobs = DurableBlobs::open(dir.path().join("blobs")).unwrap();

        let bytes = b"opaque payload bytes";
        let h1 = blobs.try_put(bytes).unwrap();
        let h2 = blobs.try_put(bytes).unwrap();
        assert_eq!(h1, h2, "same bytes -> same content hash (idempotent)");
        assert_eq!(blobs.try_get(&h1).unwrap().as_deref(), Some(&bytes[..]));
        assert!(blobs.contains(&h1));

        let phantom = ContentHash::of_bytes(b"never-stored");
        assert_eq!(blobs.try_get(&phantom).unwrap(), None);
        assert!(!blobs.contains(&phantom));

        let mut mem = InMemoryBlobs::new();
        assert_eq!(mem.put(bytes), h1);
    }

    #[test]
    fn durable_blob_batch_reopens_by_hash_and_refuses_corrupted_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let contents: &[&[u8]] = &[b"worker ceiling", b"EE policy", b"worker ceiling"];
        let mut blobs = DurableBlobs::open(dir.path()).unwrap();
        let hashes = blobs.try_put_batch(contents).unwrap();
        assert_eq!(hashes[0], hashes[2]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        drop(blobs);
        let mut reopened = DurableBlobs::open(dir.path()).unwrap();
        for (hash, bytes) in hashes.iter().zip(contents) {
            assert_eq!(reopened.try_get(hash).unwrap().as_deref(), Some(*bytes));
        }
        assert_eq!(reopened.try_put_batch(contents).unwrap(), hashes);
        std::fs::write(dir.path().join(hashes[1].to_hex()), b"changed policy").unwrap();
        assert!(matches!(
            reopened.try_put_batch(contents),
            Err(LedgerError::BlobIntegrity { .. })
        ));
        assert!(reopened.try_put_batch(&[]).unwrap().is_empty());
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn durable_blobs_compress_without_changing_raw_read_or_publication_contracts() {
        use std::os::macos::fs::MetadataExt as _;
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let mut blobs = DurableBlobs::open(directory.path()).unwrap();
        let original = (0..3 * 1024 * 1024)
            .map(|index| (index / 4096 + index % 251) as u8)
            .collect::<Vec<_>>();
        let first = blobs.try_put(&original).unwrap();
        let mut changed = original.clone();
        changed[7000..7100].fill(0x7f);
        changed.extend_from_slice(b"new exact bytes");
        let second = blobs.try_put(&changed).unwrap();
        let shorter = changed[..80 * 1024 + 3].to_vec();
        let third = blobs.try_put(&shorter).unwrap();
        for (hash, bytes) in [(first, &original), (second, &changed), (third, &shorter)] {
            assert_eq!(std::fs::read(blobs.blob_path(&hash)).unwrap(), *bytes);
            assert_eq!(blobs.try_get(&hash).unwrap().unwrap(), *bytes);
            let metadata = std::fs::metadata(blobs.blob_path(&hash)).unwrap();
            assert_eq!(metadata.nlink(), 1);
            assert_ne!(metadata.st_flags() & 0x20, 0);
            assert!(metadata.blocks() * 512 < metadata.len());
        }
        let inodes = [first, second, third]
            .map(|hash| std::fs::metadata(blobs.blob_path(&hash)).unwrap().ino());
        assert_ne!(inodes[0], inodes[1]);
        assert_ne!(inodes[1], inodes[2]);
        let mut concurrent = original.clone();
        concurrent[9000] ^= 0xff;
        let barrier = std::sync::Barrier::new(2);
        let published = std::thread::scope(|scope| {
            let threads = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        let mut writer = DurableBlobs::open(directory.path()).unwrap();
                        writer.try_put(&original).unwrap();
                        barrier.wait();
                        writer.try_put(&concurrent).unwrap()
                    })
                })
                .collect::<Vec<_>>();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(published[0], published[1]);
        assert_eq!(blobs.try_get(&published[0]).unwrap().unwrap(), concurrent);
        assert_eq!(
            std::fs::metadata(blobs.blob_path(&published[0]))
                .unwrap()
                .nlink(),
            1
        );
        let noise = (0..4096u64)
            .flat_map(|index| *blake3::hash(&index.to_le_bytes()).as_bytes())
            .collect::<Vec<_>>();
        let noise_hash = blobs.try_put(&noise).unwrap();
        assert_eq!(std::fs::read(blobs.blob_path(&noise_hash)).unwrap(), noise);
        assert_eq!(std::fs::metadata(blobs.blob_path(&noise_hash)).unwrap().st_flags() & 0x20, 0);
        let blocked = directory.path().join("blocked.tmp");
        std::os::unix::fs::symlink(blobs.blob_path(&second), &blocked).unwrap();
        assert!(octopus_file_storage::write_new(&blocked, &original).is_err());
        assert_eq!(std::fs::read(blobs.blob_path(&second)).unwrap(), changed);
        std::fs::remove_file(blocked).unwrap();
        std::fs::write(blobs.blob_path(&first), b"corrupted original").unwrap();
        assert_eq!(blobs.try_get(&second).unwrap().unwrap(), changed);
        assert_eq!(blobs.try_get(&third).unwrap().unwrap(), shorter);
        assert!(blobs.try_get(&first).is_err());
        assert!(blobs.try_put(&original).is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 5);
        drop(blobs);
        let reopened = DurableBlobs::open(directory.path()).unwrap();
        assert_eq!(reopened.try_get(&second).unwrap().unwrap(), changed);
        assert!(reopened.try_get(&first).is_err());
    }

    #[test]
    fn durable_blob_read_fails_closed_when_the_named_file_no_longer_hashes_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = dir.path().join("blobs");
        let mut blobs = DurableBlobs::open(&blob_dir).unwrap();

        let hash = blobs.try_put(b"the true, hash-addressed bytes").unwrap();
        std::fs::write(
            blob_dir.join(hash.to_hex()),
            b"tampered bytes under the same name",
        )
        .unwrap();
        assert!(matches!(
            blobs.try_put(b"the true, hash-addressed bytes"),
            Err(LedgerError::BlobIntegrity { .. })
        ));
        match blobs.try_get(&hash) {
            Err(LedgerError::BlobIntegrity { expected, actual }) => {
                assert_eq!(
                    expected, hash,
                    "the refusal names the hash the ledger asked for"
                );
                assert_ne!(actual, hash, "the on-disk bytes hash to something else");
            }
            other => panic!("a hash-mismatched blob must fail closed, got {other:?}"),
        }
    }

    #[test]
    fn malformed_durable_row_fails_closed() {
        let dir = tempfile::tempdir().unwrap();

        let bad_path = dir.path().join("bad_unknown.jsonl");
        std::fs::write(
            &bad_path,
            r#"{"seq":0,"lamport":1,"recorded_at_unix_ms":1,"trace":"t","kind":"ArtifactMoved","from":{"Interface":"source"},"to":{"Interface":"source"},"artifact":null,"surprise":true}
"#,
        )
        .unwrap();
        match DurableLog::replay(&bad_path) {
            Err(LedgerError::MalformedRow { line, .. }) => assert_eq!(line, 0),
            other => panic!("expected MalformedRow, got {other:?}"),
        }
        assert!(matches!(
            DurableLog::open(&bad_path),
            Err(LedgerError::MalformedRow { .. })
        ));

        let no_trace = dir.path().join("bad_trace.jsonl");
        std::fs::write(
            &no_trace,
            r#"{"seq":0,"lamport":1,"recorded_at_unix_ms":1,"trace":"","kind":"ArtifactMoved","from":{"Interface":"source"},"to":{"Interface":"source"},"artifact":null}
"#,
        )
        .unwrap();
        match DurableLog::replay(&no_trace) {
            Err(LedgerError::MissingTrace { line }) => assert_eq!(line, 0),
            other => panic!("expected MissingTrace, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "one index pairs with one ledger")]
    fn scan_refuses_a_ledger_that_changed_under_the_offset_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movements.jsonl");
        let mut log = DurableLog::open(&path).unwrap();
        for _ in 0..3 {
            log.append(MovementDraft {
                trace: TraceId("t".into()),
                kind: MovementKind::ArtifactMoved,
                from: run("r1"),
                to: run("r2"),
                artifact: None,
                invocation_metadata: None,
                causal_floor: None,
            });
        }
        assert_eq!(log.len(), 3);

        let len = std::fs::metadata(&path).unwrap().len();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(len / 2).unwrap();
        file.sync_all().unwrap();
        drop(file);

        assert!(
            log.try_append(MovementDraft {
                trace: TraceId("t".into()),
                kind: MovementKind::ArtifactMoved,
                from: run("r1"),
                to: run("r2"),
                artifact: None,
                invocation_metadata: None,
                causal_floor: None,
            })
            .is_err()
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), len / 2);
        log.scan_from(Seq(0), &mut |_| ControlFlow::Continue(()));
    }

    fn torn_row(seq: u64, lamport: u64) -> Movement {
        Movement {
            seq: Seq(seq),
            lamport: Lamport(lamport),
            recorded_at_unix_ms: 1,
            trace: TraceId("t".into()),
            kind: MovementKind::ArtifactMoved,
            from: run("r1"),
            to: run("r1"),
            artifact: None,
            invocation_metadata: None,
        }
    }

    #[test]
    fn open_drops_and_truncates_a_crash_torn_tail_but_keeps_committed_rows() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("movements.jsonl");

        let committed: Vec<Movement> = {
            let mut log = DurableLog::open(&log_path).unwrap();
            let m0 = log.append(MovementDraft {
                trace: TraceId("t".into()),
                kind: MovementKind::ArtifactMoved,
                from: run("r1"),
                to: run("r1"),
                artifact: None,
                invocation_metadata: None,
                causal_floor: None,
            });
            let m1 = log.append(MovementDraft {
                trace: TraceId("t".into()),
                kind: MovementKind::ArtifactMoved,
                from: run("r1"),
                to: run("r1"),
                artifact: None,
                invocation_metadata: None,
                causal_floor: None,
            });
            vec![m0, m1]
        };

        let torn = serde_json::to_string(&torn_row(2, 3)).unwrap();
        let mut raw = std::fs::read(&log_path).unwrap();
        raw.extend_from_slice(&torn.as_bytes()[..torn.len() / 2]);
        std::fs::write(&log_path, &raw).unwrap();

        assert_eq!(
            DurableLog::replay(&log_path).unwrap(),
            committed,
            "the un-terminated tail is dropped; committed rows survive replay"
        );

        let mut log = DurableLog::open(&log_path).unwrap();
        assert_eq!(log.len(), 2, "open recovered exactly the committed rows");
        let m2 = log.append(MovementDraft {
            trace: TraceId("t".into()),
            kind: MovementKind::ArtifactMoved,
            from: run("r1"),
            to: run("r1"),
            artifact: None,
            invocation_metadata: None,
            causal_floor: None,
        });
        assert_eq!(
            m2.seq,
            Seq(2),
            "seq 2 was free — the torn tail never committed"
        );
        drop(log);

        let reopened = DurableLog::open(&log_path).unwrap();
        assert_eq!(
            reopened.len(),
            3,
            "the torn bytes were physically discarded, not fused onto the next append"
        );
        assert_eq!(reopened.read_from(Seq(2))[0], m2);
    }

    #[test]
    fn replay_fails_closed_on_a_committed_unparseable_row_in_the_middle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.jsonl");
        let good = serde_json::to_string(&torn_row(0, 1)).unwrap();
        std::fs::write(&path, format!("{good}\nnot a movement at all\n{good}\n")).unwrap();
        assert!(
            matches!(
                DurableLog::replay(&path),
                Err(LedgerError::MalformedRow { line: 1, .. })
            ),
            "a committed (newline-terminated) unparseable middle row fails closed at its line"
        );
    }

    #[test]
    fn durable_blobs_skip_redundant_dir_sync_and_still_fail_closed_on_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let contents: &[&[u8]] = &[b"worker ceiling", b"ee policy"];
        let mut blobs = DurableBlobs::open(dir.path()).unwrap();
        let hashes = blobs.try_put_batch(contents).unwrap();
        assert_eq!(blobs.dir_syncs, 1);
        assert_eq!(blobs.try_put_batch(contents).unwrap(), hashes);
        assert_eq!(blobs.dir_syncs, 1);

        let replaced = blobs.blob_path(&hashes[0]);
        std::fs::remove_file(&replaced).unwrap();
        std::fs::write(&replaced, contents[0]).unwrap();
        assert_eq!(blobs.try_put_batch(contents).unwrap(), hashes);
        assert_eq!(blobs.dir_syncs, 2);

        std::fs::write(&replaced, b"tampered").unwrap();
        assert!(matches!(
            blobs.try_put_batch(contents),
            Err(LedgerError::BlobIntegrity { .. })
        ));
    }
}
