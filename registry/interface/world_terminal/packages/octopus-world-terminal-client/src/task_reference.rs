use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::Path;

use octopus_descriptors::{ArtifactId, InterfaceId, RunId};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_ledger_store::DurableMovementDecoder;
use octopus_run_record::{ArtifactBinding, RunRecord};

use crate::chat_topology::{TerminalHostRoots, load_private_run_topology};
use crate::emit_input::TerminalUserInput;
use crate::ledger_transcript::{blob_artifact_payload_bytes, transcript_artifact_from_movement};
use crate::task_projection::TaskEntry;

const TASK_REFERENCE_PREFIX: &str = "octopus-artifact://";
const MAX_TASK_REFERENCES: usize = 16;
const MAX_TASK_REFERENCE_ID_BYTES: usize = 768;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskArtifactReference {
    pub source_run: RunId,
    pub artifact: ArtifactRef,
}

impl TaskArtifactReference {
    pub fn uri(&self) -> String {
        format!(
            "{TASK_REFERENCE_PREFIX}{}/{}/{}",
            self.source_run.as_str(),
            self.artifact.content.to_hex(),
            self.artifact.id.as_str()
        )
    }

    pub fn parse(uri: &str) -> Result<Option<Self>, TaskReferenceError> {
        let Some(rest) = uri.strip_prefix(TASK_REFERENCE_PREFIX) else {
            return Ok(None);
        };
        let mut parts = rest.splitn(3, '/');
        let source_run = parts.next().unwrap_or_default();
        let content = parts.next().unwrap_or_default();
        let artifact = parts.next().unwrap_or_default();
        if source_run.trim().is_empty()
            || artifact.trim().is_empty()
            || source_run != source_run.trim()
            || artifact != artifact.trim()
            || artifact.contains('/')
        {
            return Err(TaskReferenceError::MalformedUri(uri.to_string()));
        }
        let content = ContentHash::from_hex(content)
            .ok_or_else(|| TaskReferenceError::MalformedUri(uri.to_string()))?;
        Ok(Some(Self {
            source_run: RunId::new(source_run),
            artifact: ArtifactRef {
                id: ArtifactId::new(artifact),
                content,
            },
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskMentionCandidate {
    pub title: String,
    pub run: RunId,
    pub preview: Option<String>,
    pub uri: String,
    pub same_root: bool,
}

pub fn task_mention_candidates(
    tasks: &[TaskEntry],
    current_run: &RunId,
    current_root: &Path,
) -> Vec<TaskMentionCandidate> {
    let mut candidates = tasks
        .iter()
        .filter(|task| !task.hidden && task.run != *current_run)
        .filter_map(|task| {
            let artifact = task.latest_reference.clone()?;
            let reference = TaskArtifactReference {
                source_run: task.run.clone(),
                artifact,
            };
            Some(TaskMentionCandidate {
                title: task.title.clone(),
                run: task.run.clone(),
                preview: task.preview.clone(),
                uri: reference.uri(),
                same_root: task.root == current_root,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .same_root
            .cmp(&left.same_root)
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
            .then_with(|| left.run.as_str().cmp(right.run.as_str()))
    });
    candidates.truncate(50);
    candidates
}

#[derive(Debug, thiserror::Error)]
pub enum TaskReferenceError {
    #[error("task reference URI is malformed: {0}")]
    MalformedUri(String),
    #[error("task reference list exceeds {MAX_TASK_REFERENCES} exact artifacts")]
    TooMany,
    #[error("task reference ids exceed {MAX_TASK_REFERENCE_ID_BYTES} bytes")]
    TooLarge,
    #[error("task reference points back to the current run '{0}'")]
    CurrentRun(String),
    #[error("task reference '{artifact}' is not a transcript/result artifact of run '{run}'")]
    NotFound { run: String, artifact: String },
    #[error("task reference source topology for run '{run}' is invalid: {reason}")]
    SourceTopology { run: String, reason: String },
    #[error("task reference ledger '{path}' could not be read: {source}")]
    LedgerRead {
        path: String,
        source: std::io::Error,
    },
    #[error("task reference ledger row {line} is malformed: {reason}")]
    LedgerRow { line: usize, reason: String },
    #[error("task reference artifact '{artifact}' is unreadable: {reason}")]
    Artifact { artifact: String, reason: String },
    #[error("current run record '{path}' could not be read: {source}")]
    RunRead {
        path: String,
        source: std::io::Error,
    },
    #[error("current run record '{path}' is malformed: {reason}")]
    RunMalformed { path: String, reason: String },
    #[error("current run has no projected checkpoint bound to exact terminal interface '{0}'")]
    NoProjectedCheckpoint(String),
    #[error("current run record changed while exact task references were being bound")]
    StaleRunRecord,
    #[error("current run record '{path}' could not be updated: {source}")]
    RunWrite {
        path: String,
        source: std::io::Error,
    },
}

pub fn bind_task_references(
    octopus_home: &Path,
    registry_root: &Path,
    run_records_root: &Path,
    durable_root: &Path,
    current_run: &RunId,
    current_terminal_interface: &InterfaceId,
    items: Vec<TerminalUserInput>,
) -> Result<Vec<TerminalUserInput>, TaskReferenceError> {
    let mut references = Vec::new();
    let mut retained = Vec::with_capacity(items.len());
    for item in items {
        match &item {
            TerminalUserInput::Mention { path, .. } => {
                if let Some(reference) = TaskArtifactReference::parse(path)? {
                    references.push(reference);
                } else {
                    retained.push(item);
                }
            }
            _ => retained.push(item),
        }
    }
    if references.is_empty() {
        return Ok(retained);
    }
    let mut unique = BTreeMap::new();
    for reference in references {
        if reference.source_run == *current_run {
            return Err(TaskReferenceError::CurrentRun(
                current_run.as_str().to_string(),
            ));
        }
        unique.insert(reference.uri(), reference);
    }
    let references = unique.into_values().collect::<Vec<_>>();
    if references.len() > MAX_TASK_REFERENCES {
        return Err(TaskReferenceError::TooMany);
    }
    let id_bytes = references
        .iter()
        .map(|reference| reference.uri().len())
        .sum::<usize>();
    if id_bytes > MAX_TASK_REFERENCE_ID_BYTES {
        return Err(TaskReferenceError::TooLarge);
    }
    validate_references(
        octopus_home,
        registry_root,
        run_records_root,
        durable_root,
        &references,
    )?;
    bind_into_projected_checkpoint(
        run_records_root,
        current_run,
        current_terminal_interface,
        &references,
    )?;
    Ok(retained)
}

fn validate_references(
    octopus_home: &Path,
    registry_root: &Path,
    run_records_root: &Path,
    durable_root: &Path,
    references: &[TaskArtifactReference],
) -> Result<(), TaskReferenceError> {
    let roots = TerminalHostRoots::load(octopus_home, registry_root).map_err(|error| {
        TaskReferenceError::SourceTopology {
            run: "<host>".to_string(),
            reason: error.to_string(),
        }
    })?;
    let mut terminals = BTreeMap::new();
    for reference in references {
        if terminals.contains_key(reference.source_run.as_str()) {
            continue;
        }
        let topology = load_private_run_topology(
            octopus_home,
            &roots.registry_root,
            run_records_root,
            &reference.source_run,
        )
        .map_err(|error| TaskReferenceError::SourceTopology {
            run: reference.source_run.as_str().to_string(),
            reason: error.to_string(),
        })?;
        terminals.insert(
            reference.source_run.as_str().to_string(),
            topology.terminal_interface,
        );
    }
    let requested = references
        .iter()
        .map(|reference| (reference.uri(), reference))
        .collect::<BTreeMap<_, _>>();
    let mut found = BTreeSet::new();
    let path = durable_root.join("movements.jsonl");
    let file = std::fs::File::open(&path).map_err(|source| TaskReferenceError::LedgerRead {
        path: path.display().to_string(),
        source,
    })?;
    let decoder = DurableMovementDecoder::for_log(&path);
    for (index, row) in std::io::BufReader::new(file).split(b'\n').enumerate() {
        let row = row.map_err(|source| TaskReferenceError::LedgerRead {
            path: path.display().to_string(),
            source,
        })?;
        if row.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let movement =
            decoder
                .decode(&row, index)
                .map_err(|error| TaskReferenceError::LedgerRow {
                    line: index + 1,
                    reason: error.to_string(),
                })?;
        let Some(runtime) = crate::run_index::movement_invocation_metadata(&movement)
            .and_then(|metadata| metadata.runtime)
        else {
            continue;
        };
        let Some(terminal) = terminals.get(runtime.run.as_str()) else {
            continue;
        };
        let Some((_, _, artifact)) =
            transcript_artifact_from_movement(&movement, &runtime.run, terminal)
        else {
            continue;
        };
        let reference = TaskArtifactReference {
            source_run: runtime.run,
            artifact,
        };
        let uri = reference.uri();
        if requested.contains_key(&uri) {
            found.insert(uri);
        }
    }
    for reference in references {
        if !found.contains(&reference.uri()) {
            return Err(TaskReferenceError::NotFound {
                run: reference.source_run.as_str().to_string(),
                artifact: reference.artifact.id.as_str().to_string(),
            });
        }
        blob_artifact_payload_bytes(&durable_root.join("blobs"), &reference.artifact).map_err(
            |reason| TaskReferenceError::Artifact {
                artifact: reference.artifact.id.as_str().to_string(),
                reason,
            },
        )?;
    }
    Ok(())
}

fn bind_into_projected_checkpoint(
    run_records_root: &Path,
    current_run: &RunId,
    current_terminal_interface: &InterfaceId,
    references: &[TaskArtifactReference],
) -> Result<(), TaskReferenceError> {
    let path = run_records_root.join(format!("{}.json", current_run.as_str()));
    let before = std::fs::read(&path).map_err(|source| TaskReferenceError::RunRead {
        path: path.display().to_string(),
        source,
    })?;
    let before_hash = ContentHash::of_bytes(&before);
    let mut record =
        RunRecord::from_bytes(&before).map_err(|error| TaskReferenceError::RunMalformed {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    if record.id != *current_run {
        return Err(TaskReferenceError::RunMalformed {
            path: path.display().to_string(),
            reason: format!(
                "record declares run '{}', expected '{}'",
                record.id.as_str(),
                current_run.as_str()
            ),
        });
    }
    let active = record
        .active_checkpoints
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut bound = false;
    for checkpoint in &mut record.checkpoints {
        if !active.contains(&checkpoint.id)
            || checkpoint.inbound_interface() != Some(current_terminal_interface)
        {
            continue;
        }
        bound = true;
        let mut ids = checkpoint
            .artifact_bindings
            .iter()
            .map(|binding| binding.id.as_str().to_string())
            .collect::<BTreeSet<_>>();
        for reference in references {
            if ids.insert(reference.artifact.id.as_str().to_string()) {
                checkpoint.artifact_bindings.push(ArtifactBinding {
                    id: reference.artifact.id.clone(),
                    access: Default::default(),
                });
            }
        }
    }
    if !bound {
        return Err(TaskReferenceError::NoProjectedCheckpoint(
            current_terminal_interface.as_str().to_string(),
        ));
    }
    let bytes =
        serde_json::to_vec_pretty(&record).map_err(|error| TaskReferenceError::RunMalformed {
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    RunRecord::from_bytes(&bytes).map_err(|error| TaskReferenceError::RunMalformed {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let current = std::fs::read(&path).map_err(|source| TaskReferenceError::RunRead {
        path: path.display().to_string(),
        source,
    })?;
    if ContentHash::of_bytes(&current) != before_hash {
        return Err(TaskReferenceError::StaleRunRecord);
    }
    atomic_write(&path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_task_artifact_uri_round_trips() {
        let reference = TaskArtifactReference {
            source_run: RunId::new("run_source"),
            artifact: ArtifactRef {
                id: ArtifactId::new("reply-7"),
                content: ContentHash::of_bytes(b"answer"),
            },
        };
        let uri = reference.uri();
        assert_eq!(TaskArtifactReference::parse(&uri).unwrap(), Some(reference));
        assert!(
            TaskArtifactReference::parse("thread://run_source")
                .unwrap()
                .is_none()
        );
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), TaskReferenceError> {
    let parent = path
        .parent()
        .ok_or_else(|| TaskReferenceError::RunMalformed {
            path: path.display().to_string(),
            reason: "run record path has no parent".to_string(),
        })?;
    let staged = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("run.json"),
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&staged)
        .map_err(|source| TaskReferenceError::RunWrite {
            path: staged.display().to_string(),
            source,
        })?;
    let result = (|| {
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&staged, path)?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(source) = result {
        let _ = std::fs::remove_file(&staged);
        return Err(TaskReferenceError::RunWrite {
            path: path.display().to_string(),
            source,
        });
    }
    Ok(())
}
