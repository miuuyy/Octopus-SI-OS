use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use octopus_app_server_interface::{
    OPERATOR_APPROVAL_REQUEST_LABEL, OPERATOR_APPROVAL_RESPONSE_LABEL,
    OPERATOR_ELICITATION_REQUEST_LABEL, OPERATOR_ELICITATION_RESPONSE_LABEL,
    OPERATOR_USER_INPUT_REQUEST_LABEL, OPERATOR_USER_INPUT_RESPONSE_LABEL,
};
use octopus_descriptors::{InterfaceId, RunId};
use octopus_interfaces::InterfaceDescriptor;
use octopus_ledger::ArtifactRef;
use octopus_ledger_store::DurableMovementDecoder;
use octopus_run_record::RunRecord;

use crate::chat_topology::{
    TerminalClientDescriptor, TerminalHostRoots, load_private_run_topology,
};
use crate::ledger_transcript::{
    TranscriptSide, blob_artifact_display_text, blob_artifact_payload_bytes,
    transcript_artifact_from_movement,
};
use crate::render_outbound::{OutboundRenderKind, classify_outbound_artifact};
use crate::task_state::{TaskStateError, TaskStateStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskStatus {
    NeedsInput,
    Working,
    Ready,
    Finished,
}

impl TaskStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsInput => "Needs input",
            Self::Working => "Working",
            Self::Ready => "Ready",
            Self::Finished => "Finished",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskEntry {
    pub run: RunId,
    pub ee: String,
    pub worker: String,
    pub record_path: PathBuf,
    pub terminal_interface: InterfaceId,
    pub root: PathBuf,
    pub title: String,
    pub preview: Option<String>,
    pub status: TaskStatus,
    pub pending_request_count: usize,
    pub last_activity_unix_ms: Option<u64>,
    pub movement_count: usize,
    pub latest_reference: Option<ArtifactRef>,
    pub hidden: bool,
    pub side_parent: Option<RunId>,
    pub continued_from: Option<RunId>,
}

impl TaskEntry {
    pub fn matches_query(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || self.title.to_lowercase().contains(&query)
            || self.run.as_str().to_lowercase().contains(&query)
            || self.ee.to_lowercase().contains(&query)
            || self.worker.to_lowercase().contains(&query)
            || self.root.to_string_lossy().to_lowercase().contains(&query)
            || self
                .preview
                .as_ref()
                .is_some_and(|preview| preview.to_lowercase().contains(&query))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TaskProjectionError {
    #[error(transparent)]
    State(#[from] TaskStateError),
    #[error("task projection could not load terminal roots: {0}")]
    Roots(String),
    #[error("task projection could not read run directory '{path}': {source}")]
    Runs {
        path: String,
        source: std::io::Error,
    },
    #[error("task projection could not read run record '{path}': {source}")]
    RunRead {
        path: String,
        source: std::io::Error,
    },
    #[error("task projection rejected run record '{path}': {reason}")]
    RunMalformed { path: String, reason: String },
    #[error("task projection rejected ledger row {line}: {reason}")]
    LedgerRow { line: usize, reason: String },
    #[error("task projection could not read ledger '{path}': {source}")]
    LedgerRead {
        path: String,
        source: std::io::Error,
    },
    #[error("task projection rejected operator request artifact '{artifact}': {reason}")]
    RequestArtifact { artifact: String, reason: String },
}

#[derive(Clone, Debug)]
struct TaskAccumulator {
    entry: TaskEntry,
    active: bool,
    unresolved_requests: BTreeSet<String>,
    first_inbound: Option<ArtifactRef>,
    latest_transcript: Option<ArtifactRef>,
    latest_reply: Option<ArtifactRef>,
    latest_transcript_seq: Option<u64>,
    latest_reply_seq: Option<u64>,
}

pub struct TerminalTaskProjection;

impl TerminalTaskProjection {
    pub fn load(octopus_home: &Path) -> Result<Vec<TaskEntry>, TaskProjectionError> {
        let roots = TerminalHostRoots::load_from_home(octopus_home)
            .map_err(|error| TaskProjectionError::Roots(error.to_string()))?;
        let descriptor = TerminalClientDescriptor::load(&roots.registry_root)
            .map_err(|error| TaskProjectionError::Roots(error.to_string()))?;
        let state = TaskStateStore::open(octopus_home)?;
        let mut tasks = load_authored_tasks(octopus_home, &roots, &descriptor, &state)?;
        fold_ledger(&roots.durable_ledger_root, &mut tasks)?;
        let blobs_dir = roots.durable_ledger_root.join("blobs");
        let mut entries = Vec::with_capacity(tasks.len());
        for (_, mut task) in tasks {
            task.entry.pending_request_count = task.unresolved_requests.len();
            task.entry.status = if !task.unresolved_requests.is_empty() {
                TaskStatus::NeedsInput
            } else if !task.active {
                TaskStatus::Finished
            } else if task.latest_transcript_seq > task.latest_reply_seq {
                TaskStatus::Working
            } else {
                TaskStatus::Ready
            };
            let preview_ref = task.latest_transcript.as_ref();
            task.entry.preview = preview_ref
                .map(|artifact| blob_artifact_display_text(&blobs_dir, artifact))
                .transpose()
                .map_err(|reason| TaskProjectionError::RequestArtifact {
                    artifact: preview_ref
                        .map(|artifact| artifact.id.as_str().to_string())
                        .unwrap_or_default(),
                    reason,
                })?
                .map(|text| compact_text(&text, 240));
            if task.entry.title == task.entry.run.as_str() {
                if let Some(artifact) = task.first_inbound.as_ref() {
                    let text =
                        blob_artifact_display_text(&blobs_dir, artifact).map_err(|reason| {
                            TaskProjectionError::RequestArtifact {
                                artifact: artifact.id.as_str().to_string(),
                                reason,
                            }
                        })?;
                    let title = compact_text(&text, 80);
                    if !title.is_empty() {
                        task.entry.title = title;
                    }
                }
            }
            task.entry.latest_reference = task.latest_reply.or(task.latest_transcript);
            entries.push(task.entry);
        }
        entries.sort_by(|left, right| {
            left.status
                .cmp(&right.status)
                .then_with(|| right.last_activity_unix_ms.cmp(&left.last_activity_unix_ms))
                .then_with(|| left.run.as_str().cmp(right.run.as_str()))
        });
        Ok(entries)
    }
}

fn load_authored_tasks(
    octopus_home: &Path,
    roots: &TerminalHostRoots,
    descriptor: &TerminalClientDescriptor,
    state: &TaskStateStore,
) -> Result<BTreeMap<String, TaskAccumulator>, TaskProjectionError> {
    let read_dir = match std::fs::read_dir(&roots.run_records_root) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => {
            return Err(TaskProjectionError::Runs {
                path: roots.run_records_root.display().to_string(),
                source,
            });
        }
    };
    let mut paths = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|source| TaskProjectionError::Runs {
            path: roots.run_records_root.display().to_string(),
            source,
        })?;
        if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(entry.path());
        }
    }
    paths.sort();
    let mut tasks = BTreeMap::new();
    for path in paths {
        let bytes = std::fs::read(&path).map_err(|source| TaskProjectionError::RunRead {
            path: path.display().to_string(),
            source,
        })?;
        let record =
            RunRecord::from_bytes(&bytes).map_err(|error| TaskProjectionError::RunMalformed {
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        if !descriptor.is_private_run(&record.id) {
            continue;
        }
        let topology = load_private_run_topology(
            octopus_home,
            &roots.registry_root,
            &roots.run_records_root,
            &record.id,
        )
        .map_err(|error| TaskProjectionError::RunMalformed {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
        let presentation = state.presentation(&record.id);
        let root = projected_task_root(roots, &topology)?;
        let entry = TaskEntry {
            run: record.id.clone(),
            ee: record.ee.as_str().to_string(),
            worker: record.worker.as_str().to_string(),
            record_path: path,
            terminal_interface: topology.terminal_interface,
            root,
            title: presentation
                .title
                .clone()
                .unwrap_or_else(|| record.id.as_str().to_string()),
            preview: None,
            status: TaskStatus::Ready,
            pending_request_count: 0,
            last_activity_unix_ms: None,
            movement_count: 0,
            latest_reference: None,
            hidden: presentation.hidden,
            side_parent: presentation.side_parent,
            continued_from: presentation.continued_from,
        };
        tasks.insert(
            record.id.as_str().to_string(),
            TaskAccumulator {
                entry,
                active: !record.active_checkpoints.is_empty(),
                unresolved_requests: BTreeSet::new(),
                first_inbound: None,
                latest_transcript: None,
                latest_reply: None,
                latest_transcript_seq: None,
                latest_reply_seq: None,
            },
        );
    }
    Ok(tasks)
}

fn projected_task_root(
    roots: &TerminalHostRoots,
    topology: &crate::chat_topology::PrivateRunTopology,
) -> Result<PathBuf, TaskProjectionError> {
    let Some(interface) = topology
        .worker_environment_interfaces
        .get("system_coder_env")
    else {
        return Ok(roots.worktree_root.clone());
    };
    let path = roots
        .registry_interface_root
        .join(interface.as_str())
        .join("interface.json");
    let bytes = std::fs::read(&path).map_err(|source| TaskProjectionError::RunRead {
        path: path.display().to_string(),
        source,
    })?;
    let descriptor =
        octopus_descriptors::parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| {
            TaskProjectionError::RunMalformed {
                path: path.display().to_string(),
                reason: error.to_string(),
            }
        })?;
    let root = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| {
            config
                .host_slots
                .get("worktree_root")
                .or_else(|| config.host_slots.get("repo_root"))
        })
        .map(|slot| slot.path.clone())
        .unwrap_or_else(|| roots.worktree_root.clone());
    Ok(root)
}

fn fold_ledger(
    durable_root: &Path,
    tasks: &mut BTreeMap<String, TaskAccumulator>,
) -> Result<(), TaskProjectionError> {
    let path = durable_root.join("movements.jsonl");
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(TaskProjectionError::LedgerRead {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let decoder = DurableMovementDecoder::for_log(&path);
    let blobs_dir = durable_root.join("blobs");
    for (index, row) in std::io::BufReader::new(file).split(b'\n').enumerate() {
        let row = row.map_err(|source| TaskProjectionError::LedgerRead {
            path: path.display().to_string(),
            source,
        })?;
        if row.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let movement =
            decoder
                .decode(&row, index)
                .map_err(|error| TaskProjectionError::LedgerRow {
                    line: index + 1,
                    reason: error.to_string(),
                })?;
        let Some(runtime) = crate::run_index::movement_invocation_metadata(&movement)
            .and_then(|metadata| metadata.runtime)
        else {
            continue;
        };
        let Some(task) = tasks.get_mut(runtime.run.as_str()) else {
            continue;
        };
        task.entry.movement_count += 1;
        task.entry.last_activity_unix_ms = Some(
            task.entry
                .last_activity_unix_ms
                .map_or(movement.recorded_at_unix_ms, |current| {
                    current.max(movement.recorded_at_unix_ms)
                }),
        );
        if let Some((side, _, artifact)) = transcript_artifact_from_movement(
            &movement,
            &task.entry.run,
            &task.entry.terminal_interface,
        ) {
            if side == TranscriptSide::Inbound && task.first_inbound.is_none() {
                task.first_inbound = Some(artifact.clone());
            }
            task.latest_transcript = Some(artifact.clone());
            task.latest_transcript_seq = Some(movement.seq.0);
            if side == TranscriptSide::Outbound
                && classify_outbound_artifact(artifact.id.as_str())
                    == OutboundRenderKind::TerminalReply
            {
                task.latest_reply = Some(artifact);
                task.latest_reply_seq = Some(movement.seq.0);
            }
        }
        let Some(artifact) = movement.artifact.as_ref() else {
            continue;
        };
        let label = artifact_label(artifact);
        let request_kind = request_label_kind(label);
        let response_kind = response_label_kind(label);
        if request_kind.is_none() && response_kind.is_none() {
            continue;
        }
        let bytes = blob_artifact_payload_bytes(&blobs_dir, artifact).map_err(|reason| {
            TaskProjectionError::RequestArtifact {
                artifact: artifact.id.as_str().to_string(),
                reason,
            }
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            TaskProjectionError::RequestArtifact {
                artifact: artifact.id.as_str().to_string(),
                reason: error.to_string(),
            }
        })?;
        let (request_id, is_blocking) =
            find_request_fact(&value).ok_or_else(|| TaskProjectionError::RequestArtifact {
                artifact: artifact.id.as_str().to_string(),
                reason: "request payload contains no non-empty request_id".to_string(),
            })?;
        let key = format!("{}:{request_id}", request_kind.or(response_kind).unwrap());
        if request_kind.is_some() {
            if is_blocking {
                task.unresolved_requests.insert(key);
            } else {
                task.unresolved_requests.remove(&key);
            }
        } else {
            task.unresolved_requests.remove(&key);
        }
    }
    Ok(())
}

fn artifact_label(artifact: &ArtifactRef) -> &str {
    artifact
        .id
        .as_str()
        .split_once(':')
        .map_or(artifact.id.as_str(), |(label, _)| label)
}

fn request_label_kind(label: &str) -> Option<&'static str> {
    match label {
        OPERATOR_APPROVAL_REQUEST_LABEL => Some("approval"),
        OPERATOR_ELICITATION_REQUEST_LABEL => Some("elicitation"),
        OPERATOR_USER_INPUT_REQUEST_LABEL => Some("user_input"),
        _ => None,
    }
}

fn response_label_kind(label: &str) -> Option<&'static str> {
    match label {
        OPERATOR_APPROVAL_RESPONSE_LABEL => Some("approval"),
        OPERATOR_ELICITATION_RESPONSE_LABEL => Some("elicitation"),
        OPERATOR_USER_INPUT_RESPONSE_LABEL => Some("user_input"),
        _ => None,
    }
}

fn find_request_fact(value: &serde_json::Value) -> Option<(String, bool)> {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(request_id) = object
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                let is_blocking = object
                    .get("is_blocking")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                return Some((request_id.to_string(), is_blocking));
            }
            object.values().find_map(find_request_fact)
        }
        serde_json::Value::Array(values) => values.iter().find_map(find_request_fact),
        _ => None,
    }
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= max_chars {
        return flattened;
    }
    let mut compact = flattened
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    compact.push('…');
    compact
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_fact_preserves_non_blocking_operator_input() {
        let value = serde_json::json!({
            "payload": {
                "request_id": "request-7",
                "is_blocking": false
            }
        });
        assert_eq!(
            find_request_fact(&value),
            Some(("request-7".to_string(), false))
        );
        assert_eq!(
            find_request_fact(&serde_json::json!({"request_id": "legacy"})),
            Some(("legacy".to_string(), true))
        );
    }
}
