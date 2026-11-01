//! The live `turn.stream` of one Claude turn.
//!
//! The operator chat client tails an open stream at
//! `<interface var>/streams/<sha(provider, run)>/<sha(thread, turn)>/` while
//! the turn runs, and the provider hands the sealed stream to the ledger just
//! before the turn's terminal output. The layout, rows and seal are the ones
//! the Codex provider writes and `world_operator_chat/client/turnStreams.cjs`
//! validates: a header row, `event` rows, a `semantic_boundary` for every
//! artifact handed during the turn, then the terminal boundary and a `seal`
//! row whose manifest carries the body's blake3.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use octopus_interfaces::{InterfaceArtifactFile, InterfaceArtifactManifest};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const STREAM_LABEL: &str = "turn.stream";
const SCHEMA_VERSION: u32 = 1;
const MANIFEST_FILE: &str = "turn-stream.artifact.json";
const BODY_FILE: &str = "turn-stream.body.jsonl";
const ABANDONED_FILE: &str = "turn-stream.abandoned.json";

pub struct TurnStream {
    manifest_path: PathBuf,
    body_path: PathBuf,
    body: File,
    next_seq: u64,
    last_recorded_at_unix_ms: u64,
    last_source_movement_seq: u64,
}

impl TurnStream {
    pub fn open(
        streams_root: &Path,
        provider_interface: &str,
        run: &str,
        thread_id: &str,
        turn_id: &str,
        source_movement_seq: u64,
    ) -> Result<Self, String> {
        let turn_root = streams_root
            .join(identity_hash(&[provider_interface, run]))
            .join(identity_hash(&[thread_id, turn_id]));
        std::fs::create_dir_all(&turn_root)
            .map_err(|error| format!("turn stream directory '{}' could not be created: {error}", turn_root.display()))?;
        let turn_root = turn_root
            .canonicalize()
            .map_err(|error| format!("turn stream directory '{}' could not be resolved: {error}", turn_root.display()))?;
        let manifest_path = turn_root.join(MANIFEST_FILE);
        let body_path = turn_root.join(BODY_FILE);
        if manifest_path.exists() || body_path.exists() {
            return Err(format!("turn stream '{}' already exists; a turn is never streamed twice", turn_root.display()));
        }
        let mut body = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&body_path)
            .map_err(|error| format!("turn stream body '{}' could not be created: {error}", body_path.display()))?;
        let opened_at_unix_ms = unix_time_ms()?;
        write_line(
            &mut body,
            &json!({
                "type": "header",
                "schema_version": SCHEMA_VERSION,
                "seq": 0,
                "provider_interface": provider_interface,
                "run": run,
                "thread_id": thread_id,
                "turn_id": turn_id,
                "opened_source_movement_seq": source_movement_seq,
                "opened_at_unix_ms": opened_at_unix_ms,
            }),
        )?;
        body.sync_data().map_err(|error| format!("turn stream header could not be synced: {error}"))?;
        write_manifest(&manifest_path, &body_path, None)?;
        Ok(Self {
            manifest_path,
            body_path,
            body,
            next_seq: 1,
            last_recorded_at_unix_ms: opened_at_unix_ms,
            last_source_movement_seq: source_movement_seq,
        })
    }

    pub fn append_row_synced(&mut self, row: Value) -> Result<(), String> {
        self.append(row)?;
        self.body.sync_data().map_err(|error| format!("turn stream row could not be synced: {error}"))
    }

    pub fn append_event(&mut self, source_movement_seq: u64, label: &str, payload: Value) -> Result<(), String> {
        self.append(json!({"type": "event", "source_movement_seq": source_movement_seq, "label": label, "payload": payload}))
    }

    /// Records that an artifact was handed to the ledger at this point of the
    /// stream, so a reader can order streamed rows against ledger rows.
    pub fn append_boundary(&mut self, source_movement_seq: u64, label: &str, artifact_payload_blake3: &str) -> Result<(), String> {
        valid_blake3(artifact_payload_blake3)?;
        self.append(json!({
            "type": "semantic_boundary",
            "source_movement_seq": source_movement_seq,
            "label": label,
            "artifact_payload_blake3": artifact_payload_blake3,
        }))
    }

    /// Ends the stream at its terminal artifact and returns the sealed
    /// `turn.stream` artifact to hand before that terminal.
    pub fn seal(
        mut self,
        source_movement_seq: u64,
        terminal_label: &str,
        terminal_artifact_payload_blake3: &str,
    ) -> Result<InterfaceArtifactFile, String> {
        self.append_boundary(source_movement_seq, terminal_label, terminal_artifact_payload_blake3)?;
        self.append(json!({
            "type": "seal",
            "source_movement_seq": source_movement_seq,
            "terminal_label": terminal_label,
            "terminal_artifact_payload_blake3": terminal_artifact_payload_blake3,
        }))?;
        self.body.sync_all().map_err(|error| format!("turn stream body could not be synced before seal: {error}"))?;
        let hash = hash_file(&mut self.body)?;
        write_manifest(&self.manifest_path, &self.body_path, Some(hash))?;
        InterfaceArtifactFile::new(self.manifest_path.clone())
            .map_err(|error| format!("sealed turn stream manifest '{}' is invalid: {error}", self.manifest_path.display()))
    }

    /// Withdraws an open stream that will never be sealed, so the client
    /// stops presenting it as a live turn. The body is kept.
    pub fn abandon(self) -> Result<(), String> {
        retire(&self.manifest_path)
    }

    fn append(&mut self, mut row: Value) -> Result<(), String> {
        let object = row.as_object_mut().ok_or("turn stream row must be an object")?;
        let source = object
            .get("source_movement_seq")
            .and_then(Value::as_u64)
            .ok_or("turn stream row has no source movement")?;
        if source < self.last_source_movement_seq {
            return Err("turn stream source movement moved backwards".into());
        }
        let recorded_at_unix_ms = unix_time_ms()?.max(self.last_recorded_at_unix_ms);
        object.insert("seq".into(), json!(self.next_seq));
        object.insert("recorded_at_unix_ms".into(), json!(recorded_at_unix_ms));
        write_line(&mut self.body, &row)?;
        self.next_seq += 1;
        self.last_recorded_at_unix_ms = recorded_at_unix_ms;
        self.last_source_movement_seq = source;
        Ok(())
    }
}

/// Withdraws every stream a previous life left open. Their turns were never
/// finished, and the restarted provider answers their sources as interrupted.
pub fn retire_open_streams(streams_root: &Path) -> Result<usize, String> {
    let mut retired = 0;
    let runs = match std::fs::read_dir(streams_root) {
        Ok(runs) => runs,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("turn streams '{}' could not be read: {error}", streams_root.display())),
    };
    for run in runs {
        let run = run.map_err(|error| error.to_string())?.path();
        if !run.is_dir() {
            continue;
        }
        for turn in std::fs::read_dir(&run).map_err(|error| error.to_string())? {
            let manifest_path = turn.map_err(|error| error.to_string())?.path().join(MANIFEST_FILE);
            let Ok(bytes) = std::fs::read(&manifest_path) else { continue };
            let manifest: InterfaceArtifactManifest = serde_json::from_slice(&bytes)
                .map_err(|error| format!("turn stream manifest '{}' is malformed: {error}", manifest_path.display()))?;
            if manifest.payload_content_hash.is_none() {
                retire(&manifest_path)?;
                retired += 1;
            }
        }
    }
    Ok(retired)
}

pub fn identity_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn retire(manifest_path: &Path) -> Result<(), String> {
    let abandoned = manifest_path.with_file_name(ABANDONED_FILE);
    std::fs::rename(manifest_path, &abandoned)
        .map_err(|error| format!("open turn stream '{}' could not be withdrawn: {error}", manifest_path.display()))
}

fn write_manifest(manifest_path: &Path, body_path: &Path, payload_content_hash: Option<String>) -> Result<(), String> {
    let manifest = InterfaceArtifactManifest {
        label: STREAM_LABEL.to_string(),
        payload_path: body_path.to_path_buf(),
        payload_content_hash,
    };
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    let temporary = manifest_path.with_file_name(format!(".{MANIFEST_FILE}.{}.tmp", std::process::id()));
    let mut file = File::create(&temporary).map_err(|error| format!("turn stream manifest could not be staged: {error}"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("turn stream manifest could not be written: {error}"))?;
    std::fs::rename(&temporary, manifest_path).map_err(|error| format!("turn stream manifest could not be published: {error}"))
}

fn write_line(file: &mut File, value: &Value) -> Result<(), String> {
    let mut line = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    line.push(b'\n');
    file.write_all(&line).map_err(|error| format!("turn stream row could not be appended: {error}"))
}

fn hash_file(file: &mut File) -> Result<String, String> {
    file.seek(SeekFrom::Start(0)).map_err(|error| error.to_string())?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    file.seek(SeekFrom::End(0)).map_err(|error| error.to_string())?;
    Ok(blake3::Hasher::finalize(&hasher).to_hex().to_string())
}

fn valid_blake3(value: &str) -> Result<(), String> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        Ok(())
    } else {
        Err("turn stream boundary has an invalid artifact payload hash".into())
    }
}

fn unix_time_ms() -> Result<u64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))?
        .as_millis();
    u64::try_from(millis).map_err(|_| "system clock overflowed".into())
}

pub const DELTA_LABEL: &str = "agent.message.delta";
pub const DELTA_FLUSH_WINDOW: std::time::Duration = std::time::Duration::from_millis(150);

/// The streaming state of one Claude turn, which may span several source
/// movements when Claude calls a tool. Text deltas are coalesced for up to
/// [`DELTA_FLUSH_WINDOW`], as Codex coalesces them, and written as
/// `agent.message.delta` events keyed by Claude's message id, so the client
/// folds them into one item per assistant message.
pub struct LiveTurn {
    streams_root: PathBuf,
    provider_interface: String,
    run: String,
    thread_id: Option<String>,
    pub turn_id: String,
    pub source_movement_seq: u64,
    stream: Option<TurnStream>,
    delta_seq: u64,
    pending: Option<(String, String)>,
    last_flush: std::time::Instant,
    streamed: Vec<(String, String)>,
}

impl LiveTurn {
    /// `thread_id` is the Claude session id when it is already known; a
    /// fresh session reports it before its first text, and
    /// [`LiveTurn::set_thread_id`] records it then. It is the id the
    /// terminal reply also carries, so the client keys the stream and the
    /// reply to one native turn.
    pub fn new(streams_root: PathBuf, provider_interface: &str, run: &str, thread_id: Option<&str>, turn_id: &str, source_movement_seq: u64) -> Self {
        Self {
            streams_root,
            provider_interface: provider_interface.into(),
            run: run.into(),
            thread_id: thread_id.map(str::to_owned),
            turn_id: turn_id.into(),
            source_movement_seq,
            stream: None,
            delta_seq: 0,
            pending: None,
            last_flush: std::time::Instant::now(),
            streamed: Vec::new(),
        }
    }

    pub fn set_thread_id(&mut self, thread_id: &str) -> Result<(), String> {
        match &self.thread_id {
            Some(current) if current == thread_id => Ok(()),
            Some(_) if self.stream.is_some() => Err("a turn stream cannot change its thread id once open".into()),
            _ => {
                self.thread_id = Some(thread_id.to_string());
                Ok(())
            }
        }
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    fn ensure_open(&mut self) -> Result<&mut TurnStream, String> {
        if self.stream.is_none() {
            let thread_id = self.thread_id.as_deref().ok_or("a turn stream cannot open before Claude reports its session")?;
            self.stream = Some(TurnStream::open(
                &self.streams_root,
                &self.provider_interface,
                &self.run,
                thread_id,
                &self.turn_id,
                self.source_movement_seq,
            )?);
        }
        Ok(self.stream.as_mut().expect("opened above"))
    }

    /// Records that a steering message entered this turn, in the row the
    /// chat client reads to mark the message as included.
    pub fn input_recorded(&mut self, client_id: &str, item_id: &str) -> Result<(), String> {
        self.flush()?;
        let row = json!({
            "type": "input_recorded",
            "source_movement_seq": self.source_movement_seq,
            "provider_interface": self.provider_interface,
            "run": self.run,
            "thread_id": self.thread_id.as_deref().ok_or("a turn stream cannot record input before Claude reports its session")?,
            "turn_id": self.turn_id,
            "item_id": item_id,
            "client_id": client_id,
            "stage": "input_recorded_in_turn",
            "source_event": "item/started",
            "native_event_at_ms": unix_time_ms()?,
        });
        self.ensure_open()?.append_row_synced(row)
    }

    pub fn delta(&mut self, message_id: &str, text: &str) -> Result<(), String> {
        if self.pending.as_ref().is_some_and(|(id, _)| id != message_id) {
            self.flush()?;
        }
        match &mut self.pending {
            Some((_, pending)) => pending.push_str(text),
            None => self.pending = Some((message_id.to_string(), text.to_string())),
        }
        if self.last_flush.elapsed() >= DELTA_FLUSH_WINDOW {
            self.flush()?;
        }
        Ok(())
    }

    /// When the coalesced text must be written even if no further delta comes.
    pub fn flush_due(&self) -> Option<tokio::time::Instant> {
        self.pending.as_ref().map(|_| tokio::time::Instant::from_std(self.last_flush + DELTA_FLUSH_WINDOW))
    }

    pub fn flush(&mut self) -> Result<(), String> {
        if self.pending.is_none() {
            return Ok(());
        }
        self.ensure_open()?;
        let (message_id, text) = self.pending.take().expect("checked above");
        let payload = json!({
            "seq": self.delta_seq,
            "text": text,
            "turn_id": self.turn_id,
            "item_id": message_id,
            "phase": "final_answer",
            "lifecycle": "streaming",
        });
        self.stream.as_mut().expect("opened above").append_event(self.source_movement_seq, DELTA_LABEL, payload)?;
        self.delta_seq += 1;
        self.last_flush = std::time::Instant::now();
        match self.streamed.iter_mut().find(|(id, _)| id == &message_id) {
            Some((_, streamed)) => streamed.push_str(&text),
            None => self.streamed.push((message_id, text)),
        }
        Ok(())
    }

    /// The item the final reply completes: the last streamed message when its
    /// streamed text is the reply, so the client and the voice call see the
    /// reply finish that item instead of starting a second one.
    pub fn reply_item_id(&self, reply_text: &str) -> Option<String> {
        let (id, text) = match (&self.pending, self.streamed.last()) {
            (Some((pending_id, pending)), Some((id, streamed))) if id == pending_id => (id, format!("{streamed}{pending}")),
            (Some((pending_id, pending)), _) => (pending_id, pending.clone()),
            (None, Some((id, streamed))) => (id, streamed.clone()),
            (None, None) => return None,
        };
        (text.trim() == reply_text.trim()).then(|| id.clone())
    }

    /// The text streamed so far for the latest message, written or pending.
    pub fn streamed_text(&self) -> Option<String> {
        match (&self.pending, self.streamed.last()) {
            (Some((pending_id, pending)), Some((id, streamed))) if id == pending_id => Some(format!("{streamed}{pending}")),
            (Some((_, pending)), _) => Some(pending.clone()),
            (None, Some((_, streamed))) => Some(streamed.clone()),
            (None, None) => None,
        }
    }

    pub fn boundary(&mut self, label: &str, artifact: &InterfaceArtifactFile) -> Result<(), String> {
        self.flush()?;
        let Some(stream) = self.stream.as_mut() else { return Ok(()) };
        stream.append_boundary(self.source_movement_seq, label, &payload_hash(artifact)?)
    }

    /// Seals the stream at the terminal artifact. `None` when nothing was
    /// streamed in this turn.
    pub fn seal(mut self, label: &str, terminal: &InterfaceArtifactFile) -> Result<Option<InterfaceArtifactFile>, String> {
        self.flush()?;
        match self.stream.take() {
            Some(stream) => stream.seal(self.source_movement_seq, label, &payload_hash(terminal)?).map(Some),
            None => Ok(None),
        }
    }

    pub fn abandon(mut self) -> Result<(), String> {
        self.pending = None;
        match self.stream.take() {
            Some(stream) => stream.abandon(),
            None => Ok(()),
        }
    }
}

fn payload_hash(artifact: &InterfaceArtifactFile) -> Result<String, String> {
    artifact
        .read_manifest()
        .map_err(|error| error.to_string())?
        .payload_content_hash
        .ok_or_else(|| "a handed artifact has no payload content hash".into())
}
