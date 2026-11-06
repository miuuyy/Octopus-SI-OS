use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use octopus_interfaces::{
    InterfaceArtifactFile, InterfaceArtifactManifest, InterfaceBoundaryError,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

const LIVE_STREAM_CONFIG_KEY: &str = "live_stream";
const LIVE_STREAM_SCHEMA_VERSION: u32 = 1;
const MAX_RECOVERY_BODY_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(test)]
#[path = "../tests/support/stream_ownership.rs"]
mod stream_ownership;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveStreamRules {
    enabled: bool,
    artifact_label: String,
    always_stream_labels: BTreeSet<String>,
    active_turn_stream_labels: BTreeSet<String>,
    phase_rules: BTreeMap<String, LiveStreamPhaseRule>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LiveStreamPhaseRule {
    field: String,
    stream_values: BTreeSet<String>,
    artifact_values: BTreeSet<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveStreamDescriptor {
    artifact_label: String,
    always_stream_labels: Vec<String>,
    active_turn_stream_labels: Vec<String>,
    phase_rules: Vec<LiveStreamPhaseRuleDescriptor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveStreamPhaseRuleDescriptor {
    label: String,
    field: String,
    stream_values: Vec<String>,
    artifact_values: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LiveStreamDisposition {
    Stream,
    Artifact,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveTurnIdentity {
    pub provider_interface: String,
    pub run: String,
    pub thread_id: String,
    pub turn_id: String,
}

pub(crate) struct LiveTurnStream {
    identity: LiveTurnIdentity,
    artifact_label: String,
    manifest_path: PathBuf,
    body_path: PathBuf,
    body: File,
    next_seq: u64,
    last_recorded_at_unix_ms: u64,
    last_source_movement_seq: u64,
    sealed_manifest_hash: Option<String>,
    pending_terminal: Option<TerminalSealEvidence>,
    recorded_inputs: BTreeMap<String, RecordedInputIdentity>,
}

struct RecordedInputIdentity {
    item_id: String,
    source_event: InputRecordedSource,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum InputRecordedSource {
    #[serde(rename = "item/started")]
    ItemStarted,
    #[serde(rename = "item/completed")]
    ItemCompleted,
    #[serde(rename = "thread/resume")]
    ThreadResume,
}

pub(crate) struct InputRecordedObservation<'a> {
    pub thread_id: &'a str,
    pub turn_id: &'a str,
    pub item_id: &'a str,
    pub client_id: &'a str,
    pub source_event: InputRecordedSource,
    pub native_event_at_ms: Option<i64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputRecordedRow {
    r#type: String,
    seq: u64,
    recorded_at_unix_ms: u64,
    source_movement_seq: u64,
    provider_interface: String,
    run: String,
    thread_id: String,
    turn_id: String,
    item_id: String,
    client_id: String,
    stage: String,
    source_event: InputRecordedSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_event_at_ms: Option<i64>,
}

impl InputRecordedRow {
    fn validate(&self, identity: &LiveTurnIdentity) -> Result<(), String> {
        if self.r#type != "input_recorded"
            || self.stage != "input_recorded_in_turn"
            || self.provider_interface != identity.provider_interface
            || self.run != identity.run
            || self.thread_id != identity.thread_id
            || self.turn_id != identity.turn_id
            || self.recorded_at_unix_ms == 0
            || [self.seq, self.recorded_at_unix_ms, self.source_movement_seq]
                .iter().any(|value| *value > 9_007_199_254_740_991)
        {
            return Err("Codex input-recorded receipt differs from its turn identity".to_string());
        }
        for value in [&self.item_id, &self.client_id] {
            if value.is_empty() || value.trim() != value || value.contains(['\r', '\n', '\0']) {
                return Err("Codex input-recorded receipt has an invalid message identity".to_string());
            }
        }
        match (self.source_event, self.native_event_at_ms) {
            (_, Some(ms)) if (0..=9_007_199_254_740_991).contains(&ms) => Ok(()),
            (InputRecordedSource::ThreadResume, None) => Ok(()),
            _ => Err("Codex input-recorded receipt has invalid native event timing".to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TerminalSealEvidence {
    label: String,
    artifact_payload_blake3: String,
}

pub(crate) fn artifact_payload_blake3(file: &InterfaceArtifactFile) -> Result<String, String> {
    file.read_manifest()
        .map_err(|error| format!("Codex artifact manifest is unreadable: {error}"))?
        .payload_content_hash
        .ok_or_else(|| "Codex immutable artifact manifest has no payload content hash".to_string())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamHeaderRow {
    r#type: String,
    schema_version: u32,
    seq: u64,
    provider_interface: String,
    run: String,
    thread_id: String,
    turn_id: String,
    opened_source_movement_seq: u64,
    opened_at_unix_ms: u64,
}

impl LiveStreamRules {
    pub(crate) fn from_private_config(
        private_config: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, InterfaceBoundaryError> {
        let value = private_config
            .get(LIVE_STREAM_CONFIG_KEY)
            .cloned()
            .ok_or_else(|| {
                InterfaceBoundaryError::FailedClosed(
                    "codex_app_server descriptor declares no live_stream".to_string(),
                )
            })?;
        let descriptor: LiveStreamDescriptor = serde_json::from_value(value).map_err(|error| {
            InterfaceBoundaryError::Protocol(format!(
                "codex_app_server live_stream is malformed: {error}"
            ))
        })?;
        let artifact_label = validated_label(&descriptor.artifact_label, "artifact_label")?;
        let always_stream_labels = validated_label_set(
            descriptor.always_stream_labels,
            "always_stream_labels",
            false,
        )?;
        let active_turn_stream_labels = validated_label_set(
            descriptor.active_turn_stream_labels,
            "active_turn_stream_labels",
            true,
        )?;
        if !always_stream_labels.is_disjoint(&active_turn_stream_labels) {
            return Err(InterfaceBoundaryError::Protocol(
                "codex_app_server live_stream label classes overlap".to_string(),
            ));
        }
        let mut phase_rules = BTreeMap::new();
        for descriptor in descriptor.phase_rules {
            let label = validated_label(&descriptor.label, "phase_rules[].label")?;
            let field = validated_label(&descriptor.field, "phase_rules[].field")?;
            let stream_values = validated_label_set(
                descriptor.stream_values,
                "phase_rules[].stream_values",
                false,
            )?;
            let artifact_values = validated_label_set(
                descriptor.artifact_values,
                "phase_rules[].artifact_values",
                false,
            )?;
            if !stream_values.is_disjoint(&artifact_values) {
                return Err(InterfaceBoundaryError::Protocol(format!(
                    "codex_app_server live_stream phase rule '{label}' has overlapping values"
                )));
            }
            if phase_rules
                .insert(
                    label.clone(),
                    LiveStreamPhaseRule {
                        field,
                        stream_values,
                        artifact_values,
                    },
                )
                .is_some()
            {
                return Err(InterfaceBoundaryError::Protocol(format!(
                    "codex_app_server live_stream repeats phase rule '{label}'"
                )));
            }
        }
        if phase_rules.keys().any(|label| {
            always_stream_labels.contains(label) || active_turn_stream_labels.contains(label)
        }) {
            return Err(InterfaceBoundaryError::Protocol(
                "codex_app_server live_stream phase labels overlap a whole-label rule".to_string(),
            ));
        }
        if always_stream_labels.contains(&artifact_label)
            || active_turn_stream_labels.contains(&artifact_label)
            || phase_rules.contains_key(&artifact_label)
        {
            return Err(InterfaceBoundaryError::Protocol(
                "codex_app_server live_stream artifact label cannot route into its own body"
                    .to_string(),
            ));
        }
        Ok(Self {
            enabled: true,
            artifact_label,
            always_stream_labels,
            active_turn_stream_labels,
            phase_rules,
        })
    }

    pub(crate) fn from_private_config_or_disabled(
        private_config: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, InterfaceBoundaryError> {
        if private_config.contains_key(LIVE_STREAM_CONFIG_KEY) {
            Self::from_private_config(private_config)
        } else {
            Ok(Self {
                enabled: false,
                artifact_label: String::new(),
                always_stream_labels: BTreeSet::new(),
                active_turn_stream_labels: BTreeSet::new(),
                phase_rules: BTreeMap::new(),
            })
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn validate_projection_labels(
        &self,
        context_output_labels: &BTreeSet<String>,
        session_snapshot_labels: &BTreeSet<String>,
        client_projection_output_labels: &BTreeSet<String>,
    ) -> Result<(), InterfaceBoundaryError> {
        if !self.enabled {
            return Ok(());
        }
        let mut stream_labels = self.always_stream_labels.clone();
        stream_labels.extend(self.active_turn_stream_labels.iter().cloned());
        if !stream_labels.is_disjoint(context_output_labels)
            || !stream_labels.is_disjoint(session_snapshot_labels)
            || context_output_labels.contains(&self.artifact_label)
            || session_snapshot_labels.contains(&self.artifact_label)
        {
            return Err(InterfaceBoundaryError::FailedClosed(
                "codex_app_server live_stream labels must be disjoint from context and session snapshot labels"
                    .to_string(),
            ));
        }
        let mut required = stream_labels;
        required.extend(self.phase_rules.keys().cloned());
        required.insert(self.artifact_label.clone());
        let missing = required
            .difference(client_projection_output_labels)
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(InterfaceBoundaryError::FailedClosed(format!(
                "codex_app_server client_projection_output_labels omit live-stream outputs: {}",
                missing.join(", ")
            )));
        }
        Ok(())
    }

    pub(crate) fn disposition(
        &self,
        label: &str,
        payload: &serde_json::Value,
        active_turn: bool,
    ) -> Result<LiveStreamDisposition, InterfaceBoundaryError> {
        if !self.enabled {
            return Ok(LiveStreamDisposition::Artifact);
        }
        if self.always_stream_labels.contains(label) {
            if !active_turn {
                return Err(InterfaceBoundaryError::FailedClosed(format!(
                    "codex_app_server transient output '{label}' has no active native turn stream"
                )));
            }
            return Ok(LiveStreamDisposition::Stream);
        }
        if self.active_turn_stream_labels.contains(label) {
            return Ok(if active_turn {
                LiveStreamDisposition::Stream
            } else {
                LiveStreamDisposition::Artifact
            });
        }
        let Some(rule) = self.phase_rules.get(label) else {
            return Ok(LiveStreamDisposition::Artifact);
        };
        let value = payload
            .get(&rule.field)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                InterfaceBoundaryError::FailedClosed(format!(
                    "codex_app_server phase-routed output '{label}' has no string field '{}'",
                    rule.field
                ))
            })?;
        if rule.stream_values.contains(value) {
            if !active_turn {
                return Err(InterfaceBoundaryError::FailedClosed(format!(
                    "codex_app_server transient phase '{label}:{value}' has no active native turn stream"
                )));
            }
            return Ok(LiveStreamDisposition::Stream);
        }
        if rule.artifact_values.contains(value) {
            return Ok(LiveStreamDisposition::Artifact);
        }
        Err(InterfaceBoundaryError::FailedClosed(format!(
            "codex_app_server phase-routed output '{label}' has undeclared {}='{value}'",
            rule.field
        )))
    }
}

impl LiveTurnStream {
    pub(crate) fn open(
        root: &Path,
        rules: &LiveStreamRules,
        identity: LiveTurnIdentity,
        source_movement_seq: u64,
    ) -> Result<Self, String> {
        validate_identity(&identity)?;
        let turn_root = stream_turn_root(root, &identity);
        std::fs::create_dir_all(&turn_root).map_err(|error| {
            format!(
                "Codex live stream directory '{}' could not be created: {error}",
                turn_root.display()
            )
        })?;
        let body_path = turn_root.join("turn-stream.body.jsonl");
        let manifest_path = turn_root.join("turn-stream.artifact.json");
        if manifest_path.exists() {
            return Self::reopen(
                identity,
                rules.artifact_label.clone(),
                manifest_path,
                body_path,
                source_movement_seq,
            );
        }
        if body_path.exists() {
            return Err(format!(
                "Codex live stream body '{}' exists without its manifest",
                body_path.display()
            ));
        }
        let mut body = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&body_path)
            .map_err(|error| {
                format!(
                    "Codex live stream body '{}' could not be created: {error}",
                    body_path.display()
                )
            })?;
        let opened_at_unix_ms = unix_time_ms()?;
        let header = StreamHeaderRow {
            r#type: "header".to_string(),
            schema_version: LIVE_STREAM_SCHEMA_VERSION,
            seq: 0,
            provider_interface: identity.provider_interface.clone(),
            run: identity.run.clone(),
            thread_id: identity.thread_id.clone(),
            turn_id: identity.turn_id.clone(),
            opened_source_movement_seq: source_movement_seq,
            opened_at_unix_ms,
        };
        write_json_line(&mut body, &header)?;
        body.sync_data().map_err(|error| {
            format!(
                "Codex live stream header '{}' could not be synced: {error}",
                body_path.display()
            )
        })?;
        let manifest = InterfaceArtifactManifest {
            label: rules.artifact_label.clone(),
            payload_path: body_path.clone(),
            payload_content_hash: None,
        };
        write_manifest_atomic(&manifest_path, &manifest)?;
        Ok(Self {
            identity,
            artifact_label: rules.artifact_label.clone(),
            manifest_path,
            body_path,
            body,
            next_seq: 1,
            last_recorded_at_unix_ms: opened_at_unix_ms,
            last_source_movement_seq: source_movement_seq,
            sealed_manifest_hash: None,
            pending_terminal: None,
            recorded_inputs: BTreeMap::new(),
        })
    }

    fn reopen(
        identity: LiveTurnIdentity,
        artifact_label: String,
        manifest_path: PathBuf,
        body_path: PathBuf,
        source_movement_seq: u64,
    ) -> Result<Self, String> {
        let manifest_bytes = std::fs::read(&manifest_path).map_err(|error| {
            format!(
                "Codex live stream manifest '{}' could not be read: {error}",
                manifest_path.display()
            )
        })?;
        let manifest: InterfaceArtifactManifest =
            serde_json::from_slice(&manifest_bytes).map_err(|error| {
                format!(
                    "Codex live stream manifest '{}' is malformed: {error}",
                    manifest_path.display()
                )
            })?;
        if manifest.label != artifact_label || manifest.payload_path != body_path {
            return Err(
                "Codex live stream manifest identity differs from its turn directory".to_string(),
            );
        }
        let mut body = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&body_path)
            .map_err(|error| {
                format!(
                    "Codex live stream body '{}' could not be reopened: {error}",
                    body_path.display()
                )
            })?;
        let metadata = body.metadata().map_err(|error| {
            format!(
                "Codex live stream body '{}' metadata failed: {error}",
                body_path.display()
            )
        })?;
        if metadata.len() > MAX_RECOVERY_BODY_BYTES {
            return Err(format!(
                "Codex live stream body '{}' exceeds recovery limit {}",
                body_path.display(),
                MAX_RECOVERY_BODY_BYTES
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        body.read_to_end(&mut bytes).map_err(|error| {
            format!(
                "Codex live stream body '{}' could not be read for recovery: {error}",
                body_path.display()
            )
        })?;
        let complete_len = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let discarded = bytes.len().saturating_sub(complete_len);
        if complete_len == 0 {
            return Err("Codex live stream body has no complete header row".to_string());
        }
        if discarded > 0 && manifest.payload_content_hash.is_some() {
            return Err("Codex sealed live stream body has an incomplete tail".to_string());
        }
        if discarded > 0 {
            body.set_len(complete_len as u64).map_err(|error| {
                format!("Codex live stream partial tail could not be discarded: {error}")
            })?;
            bytes.truncate(complete_len);
        }
        let (next_seq, mut pending_terminal, last_recorded_at_unix_ms, recorded_inputs) =
            validate_complete_rows(&bytes, &identity, source_movement_seq)?;
        let actual_hash = blake3::hash(&bytes).to_hex().to_string();
        if let Some(expected_hash) = manifest.payload_content_hash.as_deref() {
            if discarded != 0 || expected_hash != actual_hash {
                return Err(
                    "Codex sealed live stream body differs from its manifest hash".to_string(),
                );
            }
        }
        body.seek(SeekFrom::End(0))
            .map_err(|error| format!("Codex live stream append seek failed: {error}"))?;
        let mut stream = Self {
            identity,
            artifact_label,
            manifest_path,
            body_path,
            body,
            next_seq,
            last_recorded_at_unix_ms,
            last_source_movement_seq: source_movement_seq,
            sealed_manifest_hash: manifest.payload_content_hash,
            pending_terminal: pending_terminal.take(),
            recorded_inputs,
        };
        if discarded > 0 {
            stream.pending_terminal = None;
            stream.append_row(serde_json::json!({
                "type": "recovery",
                "source_movement_seq": source_movement_seq,
                "discarded_byte_count": discarded,
            }))?;
        }
        Ok(stream)
    }

    pub(crate) fn identity(&self) -> &LiveTurnIdentity {
        &self.identity
    }

    #[cfg(test)]
    pub(crate) fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    #[cfg(test)]
    pub(crate) fn body_path(&self) -> &Path {
        &self.body_path
    }

    pub(crate) fn append_event(
        &mut self,
        source_movement_seq: u64,
        label: &str,
        payload: &serde_json::Value,
    ) -> Result<(), String> {
        self.require_open()?;
        self.append_row(serde_json::json!({
            "type": "event",
            "source_movement_seq": source_movement_seq,
            "label": label,
            "payload": payload,
        }))
    }

    pub(crate) fn append_input_recorded(
        &mut self,
        source_movement_seq: u64,
        input: InputRecordedObservation<'_>,
    ) -> Result<bool, String> {
        let row = InputRecordedRow {
            r#type: "input_recorded".to_string(),
            seq: self.next_seq,
            recorded_at_unix_ms: self.last_recorded_at_unix_ms,
            source_movement_seq,
            provider_interface: self.identity.provider_interface.clone(),
            run: self.identity.run.clone(),
            thread_id: input.thread_id.to_string(),
            turn_id: input.turn_id.to_string(),
            item_id: input.item_id.to_string(),
            client_id: input.client_id.to_string(),
            stage: "input_recorded_in_turn".to_string(),
            source_event: input.source_event,
            native_event_at_ms: input.native_event_at_ms,
        };
        row.validate(&self.identity)?;
        if source_movement_seq < self.last_source_movement_seq {
            return Err("Codex input-recorded source movement moved backwards".to_string());
        }
        if let Some(recorded) = self.recorded_inputs.get(input.client_id) {
            if !matches!(recorded.source_event, InputRecordedSource::ThreadResume)
                && !matches!(input.source_event, InputRecordedSource::ThreadResume)
                && recorded.item_id != input.item_id
            {
                return Err("Codex native input correlation identifies two different message items".into());
            }
            return Ok(false);
        }
        self.require_open()?;
        self.append_row(serde_json::to_value(row).map_err(|error| {
            format!("Codex input-recorded receipt could not be encoded: {error}")
        })?)?;
        self.body.sync_data().map_err(|error| {
            format!("Codex input-recorded receipt could not be synced: {error}")
        })?;
        self.recorded_inputs.insert(input.client_id.to_string(), RecordedInputIdentity {
            item_id: input.item_id.to_string(), source_event: input.source_event,
        });
        Ok(true)
    }

    pub(crate) fn append_boundary(
        &mut self,
        source_movement_seq: u64,
        label: &str,
        artifact_payload_blake3: &str,
    ) -> Result<(), String> {
        self.require_open()?;
        if !valid_blake3(artifact_payload_blake3) {
            return Err("Codex semantic boundary has an invalid artifact payload hash".to_string());
        }
        self.append_row(serde_json::json!({
            "type": "semantic_boundary",
            "source_movement_seq": source_movement_seq,
            "label": label,
            "artifact_payload_blake3": artifact_payload_blake3,
        }))
    }

    pub(crate) fn seal_terminal(
        &mut self,
        source_movement_seq: u64,
        terminal_label: &str,
        terminal_artifact_payload_blake3: &str,
    ) -> Result<InterfaceArtifactFile, String> {
        if !valid_blake3(terminal_artifact_payload_blake3) {
            return Err("Codex terminal boundary has an invalid artifact payload hash".to_string());
        }
        let terminal = TerminalSealEvidence {
            label: terminal_label.to_string(),
            artifact_payload_blake3: terminal_artifact_payload_blake3.to_string(),
        };
        if let Some(existing) = self.pending_terminal.as_ref() {
            if existing != &terminal {
                return Err(
                    "Codex live stream terminal replay differs from the sealed boundary"
                        .to_string(),
                );
            }
        } else {
            self.require_open()?;
            self.append_boundary(
                source_movement_seq,
                terminal_label,
                terminal_artifact_payload_blake3,
            )?;
            self.append_row(serde_json::json!({
                "type": "seal",
                "source_movement_seq": source_movement_seq,
                "terminal_label": terminal.label,
                "terminal_artifact_payload_blake3": terminal.artifact_payload_blake3,
            }))?;
            self.pending_terminal = Some(terminal);
        }
        self.body.sync_all().map_err(|error| {
            format!(
                "Codex live stream body '{}' could not be synced before seal: {error}",
                self.body_path.display()
            )
        })?;
        let body_hash = hash_file_blake3(&mut self.body)?;
        if let Some(existing_hash) = self.sealed_manifest_hash.as_deref() {
            if existing_hash != body_hash {
                return Err(
                    "Codex live stream replay hash differs from sealed manifest".to_string()
                );
            }
        } else {
            let manifest = InterfaceArtifactManifest {
                label: self.artifact_label.clone(),
                payload_path: self.body_path.clone(),
                payload_content_hash: Some(body_hash.clone()),
            };
            write_manifest_atomic(&self.manifest_path, &manifest)?;
            self.sealed_manifest_hash = Some(body_hash);
        }
        InterfaceArtifactFile::new(self.manifest_path.clone()).map_err(|error| {
            format!(
                "Codex sealed live stream manifest '{}' is invalid: {error}",
                self.manifest_path.display()
            )
        })
    }

    fn append_row(&mut self, mut row: serde_json::Value) -> Result<(), String> {
        if self.sealed_manifest_hash.is_some() {
            return Err("Codex live stream is already sealed".to_string());
        }
        let object = row
            .as_object_mut()
            .ok_or_else(|| "Codex live stream row must be an object".to_string())?;
        let source_movement_seq = object.get("source_movement_seq")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "Codex live stream row has no source movement".to_string())?;
        if source_movement_seq < self.last_source_movement_seq {
            return Err("Codex live stream source movement moved backwards".to_string());
        }
        let recorded_at_unix_ms = unix_time_ms()?.max(self.last_recorded_at_unix_ms);
        object.insert("seq".to_string(), serde_json::json!(self.next_seq));
        object.insert(
            "recorded_at_unix_ms".to_string(),
            serde_json::json!(recorded_at_unix_ms),
        );
        write_json_line(&mut self.body, &row)?;
        self.last_recorded_at_unix_ms = recorded_at_unix_ms;
        self.last_source_movement_seq = source_movement_seq;
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or_else(|| "Codex live stream sequence overflowed".to_string())?;
        Ok(())
    }

    fn require_open(&self) -> Result<(), String> {
        if self.sealed_manifest_hash.is_some() || self.pending_terminal.is_some() {
            return Err("Codex live stream cannot append after its terminal boundary".to_string());
        }
        Ok(())
    }
}

pub(crate) fn seal_source_turn_stream(
    root: &Path,
    rules: &LiveStreamRules,
    provider_interface: &str,
    run: &str,
    source_movement_seq: u64,
    terminal_label: &str,
    terminal_artifact_payload_blake3: &str,
) -> Result<Vec<InterfaceArtifactFile>, String> {
    if !rules.enabled {
        return Ok(Vec::new());
    }
    let run_root = root.join(identity_hash(&[provider_interface, run]));
    let entries = match std::fs::read_dir(&run_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "Codex live stream run directory '{}' could not be read: {error}",
                run_root.display()
            ));
        }
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("Codex live stream directory failed: {error}"))?;
        if !entry
            .file_type()
            .map_err(|error| format!("Codex live stream entry type failed: {error}"))?
            .is_dir()
        {
            continue;
        }
        let manifest_path = entry.path().join("turn-stream.artifact.json");
        if !manifest_path.exists() {
            continue;
        }
        let manifest: InterfaceArtifactManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).map_err(|error| {
                format!(
                    "Codex live stream manifest '{}' could not be read: {error}",
                    manifest_path.display()
                )
            })?)
            .map_err(|error| {
                format!(
                    "Codex live stream manifest '{}' is malformed: {error}",
                    manifest_path.display()
                )
            })?;
        if manifest.label != rules.artifact_label {
            return Err("Codex stream directory contains a different artifact label".into());
        }
        if manifest.payload_content_hash.is_some() {
            continue;
        }
        if manifest.payload_path != entry.path().join("turn-stream.body.jsonl") {
            return Err("Codex stream manifest leaves its exact turn directory".into());
        }
        let identity = read_stream_header(&manifest.payload_path)?;
        if identity.provider_interface != provider_interface || identity.run != run
            || entry.path() != stream_turn_root(root, &identity)
        {
            return Err("Codex stream directory differs from its exact provider/run/turn identity".into());
        }
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&manifest.payload_path, MAX_RECOVERY_BODY_BYTES)
            .map_err(|error| error.to_string())?;
        let complete_end = bytes.iter().rposition(|byte| *byte == b'\n')
            .ok_or("Codex stream has no complete header")? + 1;
        let rows = bytes[..complete_end].split(|byte| *byte == b'\n').filter(|row| !row.is_empty())
            .map(serde_json::from_slice::<serde_json::Value>).collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let belongs_to_source = rows.first().is_some_and(|row| row["opened_source_movement_seq"].as_u64() == Some(source_movement_seq))
            || rows.iter().skip(1).any(|row| row["source_movement_seq"].as_u64() == Some(source_movement_seq));
        if belongs_to_source {
            validate_complete_rows(&bytes[..complete_end], &identity, source_movement_seq)?;
            let opened_at = rows.first().and_then(|row| row["opened_at_unix_ms"].as_u64())
                .ok_or("Codex stream header has no opening time")?;
            candidates.push((opened_at, identity));
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.turn_id.cmp(&right.1.turn_id)));
    candidates.into_iter().map(|(_, identity)| {
        LiveTurnStream::open(root, rules, identity, source_movement_seq)?
            .seal_terminal(source_movement_seq, terminal_label, terminal_artifact_payload_blake3)
    }).collect()
}

pub(crate) fn sealed_source_terminals(
    root: &Path,
    rules: &LiveStreamRules,
    provider_interface: &str,
    run: &str,
    source_movement_seq: u64,
) -> Result<Vec<(String, String)>, String> {
    if !rules.enabled {
        return Ok(Vec::new());
    }
    let run_root = root.join(identity_hash(&[provider_interface, run]));
    let entries = match std::fs::read_dir(&run_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("Codex live stream run directory '{}' could not be read: {error}", run_root.display())),
    };
    let mut terminals = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Codex live stream directory failed: {error}"))?;
        let manifest_path = entry.path().join("turn-stream.artifact.json");
        if !manifest_path.try_exists().map_err(|error| error.to_string())? {
            continue;
        }
        let manifest = InterfaceArtifactFile::new(manifest_path).map_err(|error| error.to_string())?
            .read_manifest().map_err(|error| error.to_string())?;
        if manifest.label != rules.artifact_label || manifest.payload_path != entry.path().join("turn-stream.body.jsonl") {
            return Err("Codex stream directory differs from its exact turn manifest".into());
        }
        let Some(expected_hash) = manifest.payload_content_hash else { continue };
        let identity = read_stream_header(&manifest.payload_path)?;
        if identity.provider_interface != provider_interface || identity.run != run || entry.path() != stream_turn_root(root, &identity) {
            return Err("Codex stream directory differs from its exact provider/run/turn identity".into());
        }
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&manifest.payload_path, MAX_RECOVERY_BODY_BYTES)
            .map_err(|error| error.to_string())?;
        if bytes.last() != Some(&b'\n') || blake3::hash(&bytes).to_hex().as_str() != expected_hash {
            return Err("sealed stream failed its immutable body hash".into());
        }
        let last: serde_json::Value = serde_json::from_slice(bytes.split(|byte| *byte == b'\n')
            .rfind(|line| !line.is_empty()).ok_or("sealed stream has no rows")?)
            .map_err(|error| error.to_string())?;
        if last["type"] != "seal" || last["source_movement_seq"].as_u64() != Some(source_movement_seq) {
            continue;
        }
        let (_, terminal, _, _) = validate_complete_rows(&bytes, &identity, source_movement_seq)?;
        let terminal = terminal.ok_or("sealed stream has a seal without its terminal boundary")?;
        terminals.push((terminal.label, terminal.artifact_payload_blake3));
    }
    terminals.sort();
    terminals.dedup();
    Ok(terminals)
}

pub(crate) fn recover_sealed_terminal_stream(
    root: &Path,
    rules: &LiveStreamRules,
    identity: &LiveTurnIdentity,
    source_movement_seq: u64,
    terminal_label: &str,
) -> Result<Option<(InterfaceArtifactFile, String)>, String> {
    if !rules.enabled { return Ok(None); }
    validate_identity(identity)?;
    let turn_root = stream_turn_root(root, identity);
    let manifest_path = turn_root.join("turn-stream.artifact.json");
    if !manifest_path.try_exists().map_err(|error| error.to_string())? { return Ok(None); }
    let artifact = InterfaceArtifactFile::new(manifest_path).map_err(|error| error.to_string())?;
    let manifest = artifact.read_manifest().map_err(|error| error.to_string())?;
    if manifest.label != rules.artifact_label || manifest.payload_path != turn_root.join("turn-stream.body.jsonl") {
        return Err("sealed stream recovery changed its exact turn manifest".into());
    }
    let expected_hash = manifest.payload_content_hash;
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&manifest.payload_path, MAX_RECOVERY_BODY_BYTES)
        .map_err(|error| error.to_string())?;
    if bytes.last() != Some(&b'\n') || expected_hash.as_ref().is_some_and(|expected| blake3::hash(&bytes).to_hex().as_str() != expected) {
        return Err("sealed stream recovery failed its immutable body hash".into());
    }
    let (_, terminal, _, _) = validate_complete_rows(&bytes, identity, source_movement_seq)?;
    let terminal = match terminal {
        Some(terminal) => terminal,
        None if expected_hash.is_none() => return Ok(None),
        None => return Err("sealed stream recovery has no terminal boundary".into()),
    };
    let last: serde_json::Value = serde_json::from_slice(bytes.split(|byte| *byte == b'\n')
        .rfind(|line| !line.is_empty()).ok_or("sealed stream recovery has no rows")?)
        .map_err(|error| error.to_string())?;
    if last["type"] != "seal" {
        return Err("sealed stream recovery has a terminal boundary without its seal".into());
    }
    if terminal.label != terminal_label || last["source_movement_seq"].as_u64() != Some(source_movement_seq) {
        return Ok(None);
    }
    let artifact = if expected_hash.is_none() {
        LiveTurnStream::open(root, rules, identity.clone(), source_movement_seq)?
            .seal_terminal(source_movement_seq, terminal_label, &terminal.artifact_payload_blake3)?
    } else { artifact };
    Ok(Some((artifact, terminal.artifact_payload_blake3)))
}

fn validated_label(value: &str, field: &str) -> Result<String, InterfaceBoundaryError> {
    if value.is_empty() || value.trim() != value || value.contains(['\r', '\n', '\0']) {
        return Err(InterfaceBoundaryError::Protocol(format!(
            "codex_app_server live_stream {field} must be a non-empty trimmed string"
        )));
    }
    Ok(value.to_string())
}

fn validated_label_set(
    values: Vec<String>,
    field: &str,
    allow_empty: bool,
) -> Result<BTreeSet<String>, InterfaceBoundaryError> {
    let mut labels = BTreeSet::new();
    for value in values {
        let value = validated_label(&value, field)?;
        if !labels.insert(value.clone()) {
            return Err(InterfaceBoundaryError::Protocol(format!(
                "codex_app_server live_stream {field} repeats '{value}'"
            )));
        }
    }
    if labels.is_empty() && !allow_empty {
        return Err(InterfaceBoundaryError::Protocol(format!(
            "codex_app_server live_stream {field} must not be empty"
        )));
    }
    Ok(labels)
}

fn validate_identity(identity: &LiveTurnIdentity) -> Result<(), String> {
    for (field, value) in [
        ("provider_interface", identity.provider_interface.as_str()),
        ("run", identity.run.as_str()),
        ("thread_id", identity.thread_id.as_str()),
        ("turn_id", identity.turn_id.as_str()),
    ] {
        if value.is_empty() || value.trim() != value || value.contains(['\r', '\n', '\0']) {
            return Err(format!("Codex live stream {field} is invalid"));
        }
    }
    Ok(())
}

fn stream_turn_root(root: &Path, identity: &LiveTurnIdentity) -> PathBuf {
    root.join(identity_hash(&[
        &identity.provider_interface,
        &identity.run,
    ]))
    .join(identity_hash(&[&identity.thread_id, &identity.turn_id]))
}

fn identity_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn valid_blake3(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn unix_time_ms() -> Result<u64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("Codex live stream clock is before Unix epoch: {error}"))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| "Codex live stream Unix timestamp overflowed u64".to_string())
}

fn write_json_line(writer: &mut File, value: &impl Serialize) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, value)
        .map_err(|error| format!("Codex live stream row could not be encoded: {error}"))?;
    writer
        .write_all(b"\n")
        .map_err(|error| format!("Codex live stream row could not be appended: {error}"))
}

fn write_manifest_atomic(path: &Path, manifest: &InterfaceArtifactManifest) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Codex live stream manifest has no parent directory".to_string())?;
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("Codex live stream manifest failed to encode: {error}"))?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".turn-stream.artifact.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "Codex live stream temporary manifest '{}' could not be created: {error}",
                temporary.display()
            )
        })?;
    file.write_all(&bytes).map_err(|error| {
        format!(
            "Codex live stream temporary manifest '{}' could not be written: {error}",
            temporary.display()
        )
    })?;
    file.sync_all().map_err(|error| {
        format!(
            "Codex live stream temporary manifest '{}' could not be synced: {error}",
            temporary.display()
        )
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        format!(
            "Codex live stream manifest '{}' could not be atomically installed: {error}",
            path.display()
        )
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            format!(
                "Codex live stream directory '{}' could not be synced: {error}",
                parent.display()
            )
        })
}

fn validate_complete_rows(
    bytes: &[u8],
    identity: &LiveTurnIdentity,
    source_movement_seq: u64,
) -> Result<(u64, Option<TerminalSealEvidence>, u64, BTreeMap<String, RecordedInputIdentity>), String> {
    let mut lines = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty());
    let header: StreamHeaderRow = serde_json::from_slice(
        lines
            .next()
            .ok_or_else(|| "Codex live stream body has no header".to_string())?,
    )
    .map_err(|error| format!("Codex live stream header is malformed: {error}"))?;
    if header.r#type != "header"
        || header.schema_version != LIVE_STREAM_SCHEMA_VERSION
        || header.seq != 0
        || header.provider_interface != identity.provider_interface
        || header.run != identity.run
        || header.thread_id != identity.thread_id
        || header.turn_id != identity.turn_id
        || header.opened_at_unix_ms == 0
    {
        return Err("Codex live stream header identity differs from its native turn".to_string());
    }
    let mut expected_seq = 1u64;
    let mut last_recorded_at_unix_ms = header.opened_at_unix_ms;
    let mut terminal = None;
    let mut recorded_inputs = BTreeMap::new();
    let mut previous_source_movement_seq = header.opened_source_movement_seq;
    for line in lines {
        let row: serde_json::Value = serde_json::from_slice(line)
            .map_err(|error| format!("Codex live stream row is malformed: {error}"))?;
        let seq = row
            .get("seq")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "Codex live stream row has no unsigned seq".to_string())?;
        if seq != expected_seq {
            return Err(format!(
                "Codex live stream row sequence {seq} differs from expected {expected_seq}"
            ));
        }
        expected_seq = expected_seq
            .checked_add(1)
            .ok_or_else(|| "Codex live stream sequence overflowed".to_string())?;
        let recorded_at_unix_ms = row
            .get("recorded_at_unix_ms")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "Codex live stream row has no recorded_at_unix_ms".to_string())?;
        if recorded_at_unix_ms < last_recorded_at_unix_ms {
            return Err("Codex live stream row time moved backwards".to_string());
        }
        last_recorded_at_unix_ms = recorded_at_unix_ms;
        let row_source = row.get("source_movement_seq").and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "Codex live stream row has no source movement".to_string())?;
        if row_source < previous_source_movement_seq {
            return Err("Codex live stream source movement moved backwards".to_string());
        }
        previous_source_movement_seq = row_source;
        match row.get("type").and_then(serde_json::Value::as_str) {
            Some("input_recorded") => {
                if terminal.is_some() {
                    return Err("Codex live stream contains input after its seal".to_string());
                }
                let receipt: InputRecordedRow = serde_json::from_value(row.clone())
                    .map_err(|error| format!("Codex input-recorded receipt is malformed: {error}"))?;
                receipt.validate(identity)?;
                if recorded_inputs.insert(receipt.client_id, RecordedInputIdentity {
                    item_id: receipt.item_id, source_event: receipt.source_event,
                }).is_some() {
                    return Err("Codex live stream repeats an input-recorded receipt".to_string());
                }
            }
            Some("seal") => {
                if terminal.is_some() {
                    return Err("Codex live stream repeats its seal row".to_string());
                }
                terminal = Some(TerminalSealEvidence {
                    label: row
                        .get("terminal_label")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| "Codex live stream seal has no terminal label".to_string())?
                        .to_string(),
                    artifact_payload_blake3: row
                        .get("terminal_artifact_payload_blake3")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            "Codex live stream seal has no terminal artifact payload hash"
                                .to_string()
                        })?
                        .to_string(),
                });
            }
            Some("recovery") => terminal = None,
            Some("event") => {
                if terminal.is_some() {
                    return Err("Codex live stream contains data after its seal".to_string());
                }
            }
            Some("semantic_boundary") => {
                if terminal.is_some()
                    || !row
                        .get("artifact_payload_blake3")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(valid_blake3)
                {
                    return Err("Codex live stream semantic boundary is invalid".to_string());
                }
            }
            Some(kind) => {
                return Err(format!("Codex live stream row has unknown type '{kind}'"));
            }
            None => return Err("Codex live stream row has no type".to_string()),
        }
    }
    if terminal
        .as_ref()
        .is_some_and(|terminal| !valid_blake3(&terminal.artifact_payload_blake3))
    {
        return Err("Codex live stream seal artifact hash is invalid".to_string());
    }
    if source_movement_seq < previous_source_movement_seq {
        return Err("Codex live stream recovery source movement moved backwards".to_string());
    }
    Ok((expected_seq, terminal, last_recorded_at_unix_ms, recorded_inputs))
}

fn read_stream_header(path: &Path) -> Result<LiveTurnIdentity, String> {
    let file = File::open(path).map_err(|error| {
        format!(
            "Codex live stream body '{}' could not be opened: {error}",
            path.display()
        )
    })?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    std::io::BufRead::read_line(&mut reader, &mut line).map_err(|error| {
        format!(
            "Codex live stream header '{}' could not be read: {error}",
            path.display()
        )
    })?;
    let header: StreamHeaderRow = serde_json::from_str(line.trim_end())
        .map_err(|error| format!("Codex live stream header is malformed: {error}"))?;
    if header.r#type != "header"
        || header.schema_version != LIVE_STREAM_SCHEMA_VERSION
        || header.seq != 0
    {
        return Err("Codex live stream header shape is invalid".to_string());
    }
    Ok(LiveTurnIdentity {
        provider_interface: header.provider_interface,
        run: header.run,
        thread_id: header.thread_id,
        turn_id: header.turn_id,
    })
}

fn hash_file_blake3(file: &mut File) -> Result<String, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("Codex live stream hash seek failed: {error}"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Codex live stream hash read failed: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    file.seek(SeekFrom::End(0))
        .map_err(|error| format!("Codex live stream append seek failed: {error}"))?;
    Ok(blake3::Hasher::finalize(&hasher).to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/support/recorded_input_correlation.rs");

    fn rules() -> LiveStreamRules {
        LiveStreamRules {
            enabled: true,
            artifact_label: "turn.stream".to_string(),
            always_stream_labels: BTreeSet::from(["agent.message.delta".to_string()]),
            active_turn_stream_labels: BTreeSet::new(),
            phase_rules: BTreeMap::new(),
        }
    }

    fn identity() -> LiveTurnIdentity {
        LiveTurnIdentity {
            provider_interface: "world_provider_codex_task".to_string(),
            run: "run-task".to_string(),
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
        }
    }

    #[test]
    fn input_recorded_retries_preserve_exact_rows_and_sealed_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let input = || InputRecordedObservation {
            thread_id: "thread-1", turn_id: "turn-1", item_id: "native-1",
            client_id: "control-1", source_event: InputRecordedSource::ItemStarted,
            native_event_at_ms: Some(100),
        };
        let mut stream = LiveTurnStream::open(temp.path(), &rules(), identity(), 7).unwrap();
        assert!(stream.append_input_recorded(7, input()).unwrap());
        let body_path = stream.body_path.clone();
        let first = std::fs::read(&body_path).unwrap();
        assert!(!stream.append_input_recorded(7, InputRecordedObservation {
            source_event: InputRecordedSource::ItemCompleted, native_event_at_ms: Some(101), ..input()
        }).unwrap());
        assert!(stream.append_input_recorded(7, InputRecordedObservation { turn_id: "other", ..input() }).is_err());
        assert!(stream.append_input_recorded(6, input()).is_err());
        assert_eq!(std::fs::read(&body_path).unwrap(), first);
        drop(stream);
        let mut stream = LiveTurnStream::open(temp.path(), &rules(), identity(), 9).unwrap();
        assert!(!stream.append_input_recorded(9, InputRecordedObservation {
            item_id: "item-1", source_event: InputRecordedSource::ThreadResume,
            native_event_at_ms: Some(150), ..input()
        }).unwrap());
        assert_eq!(std::fs::read(&body_path).unwrap(), first);
        let sealed = stream.seal_terminal(9, "interface.artifact.output", &"a".repeat(64)).unwrap();
        let sealed_bytes = sealed.read_payload_bytes().unwrap();
        let manifest_bytes = std::fs::read(sealed.manifest_path()).unwrap();
        drop(stream);
        let mut replay = LiveTurnStream::open(temp.path(), &rules(), identity(), 10).unwrap();
        assert!(!replay.append_input_recorded(10, input()).unwrap());
        assert!(replay.append_input_recorded(10, InputRecordedObservation { client_id: "other", ..input() }).is_err());
        assert_eq!(sealed.read_payload_bytes().unwrap(), sealed_bytes);
        assert_eq!(std::fs::read(sealed.manifest_path()).unwrap(), manifest_bytes);
        drop(replay);
        let mut body = OpenOptions::new().append(true).open(&body_path).unwrap();
        body.write_all(b"{\"partial\":").unwrap();
        let corrupt_bytes = std::fs::read(&body_path).unwrap();
        assert!(LiveTurnStream::open(temp.path(), &rules(), identity(), 10).is_err());
        assert_eq!(std::fs::read(&body_path).unwrap(), corrupt_bytes);
        let rows: Vec<serde_json::Value> = first.split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty()).map(|line| serde_json::from_slice(line).unwrap()).collect();
        for (field, value) in [("client_id", serde_json::Value::Null), ("run", serde_json::json!("other")),
            ("stage", serde_json::json!("consumed")), ("source_event", serde_json::json!("turn/steer")),
            ("native_event_at_ms", serde_json::json!(-1)), ("text", serde_json::json!("guess"))]
        {
            let mut invalid = rows.clone();
            invalid[1][field] = value;
            let bytes = invalid.iter().map(|row| format!("{row}\n")).collect::<String>();
            assert!(validate_complete_rows(bytes.as_bytes(), &identity(), 9).is_err());
        }
        let duplicate = format!("{}{}\n", String::from_utf8(first).unwrap(), {
            let mut row = rows[1].clone(); row["seq"] = serde_json::json!(2); row
        });
        assert!(validate_complete_rows(duplicate.as_bytes(), &identity(), 9).is_err());
    }

    #[test]
    fn a_stream_sealed_by_another_terminal_is_not_recovered_as_this_one() {
        let temp = tempfile::tempdir().unwrap();
        let mut stream = LiveTurnStream::open(temp.path(), &rules(), identity(), 9).unwrap();
        stream.append_event(9, "agent.message.delta", &serde_json::json!({"text":"a"})).unwrap();
        stream.seal_terminal(9, "turn.error", &"a".repeat(64)).unwrap();
        drop(stream);
        let recover = |seq, label| recover_sealed_terminal_stream(temp.path(), &rules(), &identity(), seq, label);
        assert!(recover(9, "turn.interrupted.result").unwrap().is_none());
        assert!(recover(10, "turn.error").unwrap().is_none());
        let (_, hash) = recover(9, "turn.error").unwrap().unwrap();
        assert_eq!(hash, "a".repeat(64));
        let id = identity();
        let sealed = |seq| sealed_source_terminals(temp.path(), &rules(), &id.provider_interface, &id.run, seq).unwrap();
        assert_eq!(sealed(9), vec![("turn.error".to_string(), "a".repeat(64))]);
        assert!(sealed(10).is_empty());
    }

    #[test]
    fn one_stream_reopens_across_source_continuations_and_seals_with_hash() {
        let temp = tempfile::tempdir().unwrap();
        let mut first = LiveTurnStream::open(temp.path(), &rules(), identity(), 7).unwrap();
        first
            .append_event(7, "agent.message.delta", &serde_json::json!({"text":"a"}))
            .unwrap();
        let manifest_path = first.manifest_path.clone();
        drop(first);

        let mut resumed = LiveTurnStream::open(temp.path(), &rules(), identity(), 9).unwrap();
        assert_eq!(resumed.manifest_path, manifest_path);
        resumed
            .append_event(9, "agent.message.delta", &serde_json::json!({"text":"b"}))
            .unwrap();
        let sealed = resumed
            .seal_terminal(9, "interface.artifact.output", &"a".repeat(64))
            .unwrap();
        let manifest = sealed.read_manifest().unwrap();
        assert_eq!(manifest.label, "turn.stream");
        assert!(manifest.payload_content_hash.is_some());
        let rows = String::from_utf8(sealed.read_payload_bytes().unwrap()).unwrap();
        assert!(rows.contains("\"source_movement_seq\":7"));
        assert!(rows.contains("\"source_movement_seq\":9"));
        assert!(rows.contains("\"type\":\"seal\""));
        let rows = rows
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let opened_at = rows[0]["opened_at_unix_ms"].as_u64().unwrap();
        assert!(opened_at > 0);
        assert!(rows[0].get("recorded_at_unix_ms").is_none());
        let mut previous = opened_at;
        for row in rows.iter().skip(1) {
            let recorded = row["recorded_at_unix_ms"].as_u64().unwrap();
            assert!(recorded >= previous);
            previous = recorded;
        }
        drop(resumed);
        let mut replay = LiveTurnStream::open(temp.path(), &rules(), identity(), 11).unwrap();
        let replayed = replay
            .seal_terminal(11, "interface.artifact.output", &"a".repeat(64))
            .unwrap();
        assert_eq!(replayed.manifest_path(), sealed.manifest_path());
        assert!(
            replay
                .seal_terminal(11, "interface.artifact.output", &"b".repeat(64),)
                .is_err()
        );
    }

    #[test]
    fn recovery_discards_only_partial_tail_and_records_its_byte_count() {
        let temp = tempfile::tempdir().unwrap();
        let mut stream = LiveTurnStream::open(temp.path(), &rules(), identity(), 3).unwrap();
        let input = || InputRecordedObservation {
            thread_id: "thread-1", turn_id: "turn-1", item_id: "native-1",
            client_id: "control-1", source_event: InputRecordedSource::ItemCompleted,
            native_event_at_ms: Some(100),
        };
        assert!(stream.append_input_recorded(3, input()).unwrap());
        stream
            .append_event(
                3,
                "agent.message.delta",
                &serde_json::json!({"text":"kept"}),
            )
            .unwrap();
        let body_path = stream.body_path.clone();
        drop(stream);
        let mut body = OpenOptions::new().append(true).open(&body_path).unwrap();
        body.write_all(b"{\"partial\":").unwrap();
        body.sync_all().unwrap();

        let mut recovered = LiveTurnStream::open(temp.path(), &rules(), identity(), 4).unwrap();
        assert!(!recovered.append_input_recorded(4, input()).unwrap());
        recovered
            .append_event(
                4,
                "agent.message.delta",
                &serde_json::json!({"text":"after"}),
            )
            .unwrap();
        let rows = std::fs::read_to_string(body_path).unwrap();
        assert!(rows.contains("\"discarded_byte_count\":11"));
        assert!(rows.contains("\"text\":\"kept\""));
        assert!(rows.contains("\"text\":\"after\""));
        assert!(rows.ends_with('\n'));
        let recovery = rows
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|row| row["type"] == "recovery")
            .unwrap();
        assert!(recovery["recorded_at_unix_ms"].as_u64().unwrap() > 0);
    }
}
