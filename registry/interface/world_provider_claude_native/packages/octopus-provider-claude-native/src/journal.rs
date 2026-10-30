use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const JOURNAL_FILE: &str = "claude-native-journal.json";
const JOURNAL_VERSION: u32 = 1;
const HANDLED_CONTROLS_KEPT: usize = 512;

/// What this life remembers about one run between turns and across restarts:
/// the Claude session that continues the run's conversation, and the turn
/// that was in flight when the life last wrote, so a restart can resume it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunSession {
    #[serde(default)]
    pub claude_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inflight: Option<InflightTurn>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InflightTurn {
    pub source_seq: u64,
    pub turn: String,
    /// The admitted input the turn answers, kept so a reply handed after a
    /// restart still names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_call: Option<PendingCallRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PendingCallRecord {
    pub call_id: String,
    pub tool: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalState {
    version: u32,
    runs: BTreeMap<String, RunSession>,
    /// Ledger movements this life already answered as turn controls while a
    /// turn was running, so the main reader does not start them as turns.
    #[serde(default)]
    handled_controls: Vec<u64>,
}

pub struct Journal {
    path: PathBuf,
    max_bytes: u64,
    state: JournalState,
}

impl Journal {
    pub fn open(var_dir: &Path, max_bytes: u64) -> Result<Self, String> {
        let path = var_dir.join(JOURNAL_FILE);
        let state = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.len() > max_bytes {
                    return Err("claude native journal is not a bounded regular file".into());
                }
                let state: JournalState = serde_json::from_slice(
                    &std::fs::read(&path).map_err(|error| format!("claude native journal is unreadable: {error}"))?,
                )
                .map_err(|error| format!("claude native journal is malformed: {error}"))?;
                if state.version != JOURNAL_VERSION {
                    return Err(format!("unsupported claude native journal version {}", state.version));
                }
                state
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                JournalState { version: JOURNAL_VERSION, runs: BTreeMap::new(), handled_controls: Vec::new() }
            }
            Err(error) => return Err(format!("claude native journal cannot be inspected: {error}")),
        };
        Ok(Self { path, max_bytes, state })
    }

    pub fn runs(&self) -> impl Iterator<Item = (&String, &RunSession)> {
        self.state.runs.iter()
    }

    pub fn claude_session_id(&self, run: &str) -> Option<String> {
        self.state.runs.get(run).and_then(|entry| entry.claude_session_id.clone())
    }

    pub fn record_session(&mut self, run: &str, claude_session_id: &str) -> Result<(), String> {
        let entry = self.state.runs.entry(run.to_string()).or_default();
        if entry.claude_session_id.as_deref() == Some(claude_session_id) {
            return Ok(());
        }
        entry.claude_session_id = Some(claude_session_id.to_string());
        self.save()
    }

    pub fn inflight(&self, run: &str) -> Option<&InflightTurn> {
        self.state.runs.get(run).and_then(|entry| entry.inflight.as_ref())
    }

    pub fn record_inflight(&mut self, run: &str, inflight: Option<InflightTurn>) -> Result<(), String> {
        let entry = self.state.runs.entry(run.to_string()).or_default();
        if entry.inflight == inflight {
            return Ok(());
        }
        entry.inflight = inflight;
        self.save()
    }

    pub fn control_handled(&self, seq: u64) -> bool {
        self.state.handled_controls.contains(&seq)
    }

    pub fn record_handled_control(&mut self, seq: u64) -> Result<(), String> {
        if self.control_handled(seq) {
            return Ok(());
        }
        self.state.handled_controls.push(seq);
        if self.state.handled_controls.len() > HANDLED_CONTROLS_KEPT {
            let excess = self.state.handled_controls.len() - HANDLED_CONTROLS_KEPT;
            self.state.handled_controls.drain(..excess);
        }
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self.state).map_err(|error| error.to_string())?;
        if bytes.len() as u64 > self.max_bytes {
            return Err("claude native journal exceeds its declared limit".into());
        }
        let root = self.path.parent().ok_or("claude native journal has no directory")?;
        let next = root.join(format!("{JOURNAL_FILE}.next"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&next).map_err(|error| format!("claude native journal cannot be prepared: {error}"))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("claude native journal cannot be persisted: {error}"))?;
        std::fs::rename(&next, &self.path).map_err(|error| format!("claude native journal cannot be installed: {error}"))?;
        std::fs::File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("claude native journal directory cannot be synced: {error}"))
    }
}
