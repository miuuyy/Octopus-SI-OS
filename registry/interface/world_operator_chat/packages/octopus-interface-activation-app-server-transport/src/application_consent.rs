use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interface_transport::InterfaceLiftContext;
use octopus_interfaces::{InterfaceArtifactFile, InterfaceArtifactManifest};
use octopus_ledger::{ArtifactRef, CheckpointBindingPolicySnapshot, ContentHash, InvocationMetadata, Movement, MovementKind, Endpoint};
use octopus_ledger_index::LedgerSqliteReader;
use octopus_run_record::{CheckpointRecord, RunRecord};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const OWNER_STATE: &str = "application_consent";

pub use octopus_app_server_interface::application_consent_contract::{
    ConsentBounds, ConsentPersistence, ConsentDecision, ConsentChoice, BrowserConsentOperation,
    BrowserConsentPolicyDecision, BrowserOriginAccess, BrowserOriginPolicyValues,
    BrowserOriginPolicyRule, BrowserOriginPolicyLayer, BrowserOriginConsentPolicy,
    BrowserHistoryConsentPolicy, ConsentSubject, ConsentPolicy, ConsentSession,
    NativeAppPolicyDecision, NativeAppRisk, NativeConsentTarget, NativeAppPolicy, ConsentOffer,
    ConsentEligibility, ConsentEvidenceRef, ConsentHumanEvidence, ConsentIntake, ConsentCommand,
    ConsentPolicyAuthorization, SelectedConsentReturn, CheckedRunInstall,
};
use octopus_app_server_interface::application_consent_contract::{
    digest_json, exact_text, normal_path, require, string_error,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentWriter {
    pub door: InterfaceId,
    pub tool: String,
    pub call_label: String,
    pub result_label: String,
    pub run_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentCheckpointTemplates {
    pub command: CheckpointRecord,
    pub call: CheckpointRecord,
    pub result: CheckpointRecord,
    pub management: CheckpointRecord,
    pub publication: CheckpointRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentAuthorityConfig {
    pub schema_version: u32,
    pub policy: ArtifactRef,
    pub authority_artifact: ArtifactId,
    pub root_ee: EeId,
    pub home_run: RunId,
    pub home_interface: InterfaceId,
    pub authority_run: RunId,
    pub authority_worker: InterfaceId,
    pub writer: ConsentWriter,
    pub command_label: String,
    pub checkpoints: ConsentCheckpointTemplates,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentAuthorityBinding {
    pub config: ConsentAuthorityConfig,
    pub genesis_run_hash: String,
    pub immutable_config_digest: String,
    pub bounds: ConsentBounds,
}

impl ConsentAuthorityConfig {
    pub fn validate(&self) -> Result<(), String> {
        require(self.schema_version == 2 && self.home_run != self.authority_run
            && self.home_interface != self.writer.door && self.home_interface != self.authority_worker
            && self.authority_worker != self.writer.door, "consent owner identities are not independent")?;
        for value in [self.root_ee.as_str(), self.home_run.as_str(), self.home_interface.as_str(),
            self.authority_run.as_str(), self.authority_worker.as_str(), self.writer.door.as_str(),
            self.writer.tool.as_str(), self.command_label.as_str(), self.authority_artifact.as_str()] {
            exact_text(value)?;
        }
        require(normal_path(&self.writer.run_path)
            && self.writer.run_path.file_name().and_then(|v| v.to_str()) == Some(&format!("{}.json", self.authority_run)),
            "consent writer must name the exact absolute authority RunRecord")?;
        require(self.writer.call_label == "tool.call" && self.writer.result_label == format!("{}.result", self.writer.tool),
            "consent writer must use the existing file.write artifact contract")?;
        let expected = [(&self.checkpoints.command, &self.home_interface, &self.home_interface, self.command_label.as_str()),
            (&self.checkpoints.call, &self.home_interface, &self.writer.door, self.writer.call_label.as_str()),
            (&self.checkpoints.result, &self.writer.door, &self.home_interface, self.writer.result_label.as_str()),
            (&self.checkpoints.management, &self.home_interface, &self.home_interface, "application.consent.intake"),
            (&self.checkpoints.publication, &self.home_interface, &self.home_interface, "application.consent.management.result")];
        let mut names = BTreeSet::new();
        for (checkpoint, source, target, label) in expected {
            require(names.insert(checkpoint.id.clone()) && checkpoint.run == self.authority_run
                && checkpoint.inbound_interface() == Some(source) && checkpoint.outbound_interfaces().eq([target])
                && checkpoint.interface_binding.as_ref().is_some_and(|b| exact_name_binding(b, label))
                && checkpoint.outbound_interface_bindings.iter().all(|b| exact_name_binding(b, label)),
                "consent checkpoint template changed exact owner edges or names")?;
            let mut ids = BTreeSet::new();
            require(checkpoint.artifact_bindings.iter().all(|b| ids.insert(&b.id)
                && b.access.read && !b.access.change && !b.access.create && !b.access.delete)
                && ids.contains(&self.policy.id) && ids.contains(&self.authority_artifact),
                "consent checkpoint must bind its policy and authority read-only")?;
        }
        let [requirement] = self.checkpoints.call.required_verified_artifacts.as_slice() else {
            return Err("consent file.write requires one exact tool-call verifier".into());
        };
        require(requirement.mark.as_str() == "tool.call.allowed" && requirement.verifier.as_str() == "tool_call_allowed"
            && requirement.artifact_label == self.writer.call_label
            && requirement.params == json!({"tools":[self.writer.tool]})
            && self.checkpoints.command.required_verified_artifacts.is_empty()
            && self.checkpoints.result.required_verified_artifacts.is_empty()
            && self.checkpoints.management.required_verified_artifacts.is_empty()
            && self.checkpoints.publication.required_verified_artifacts.is_empty(),
            "consent authority checkpoint requirements differ from the declared writer")
    }

    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        digest_json(self)
    }

    pub fn checkpoints_at(&self, generation: u64) -> Result<Vec<CheckpointRecord>, String> {
        self.validate()?;
        Ok([&self.checkpoints.command, &self.checkpoints.call, &self.checkpoints.result, &self.checkpoints.management, &self.checkpoints.publication].into_iter().map(|template| {
            let mut checkpoint = template.clone();
            checkpoint.id = CheckpointId::new(format!("{}:{generation}", template.id));
            checkpoint.epoch = octopus_run_record::GrantEpoch(generation);
            checkpoint
        }).collect())
    }
}

impl ConsentAuthorityConfig {
    fn checkpoints_for_state(&self, state: &ConsentState) -> Result<Vec<CheckpointRecord>, String> {
        let mut checkpoints = self.checkpoints_at(state.generation)?;
        for list in state.management.values().filter(|list| list.removal.is_none() && !list.displayed_receipts.is_empty()) {
            let mut checkpoint = self.checkpoints.management.clone();
            checkpoint.id = CheckpointId::new(format!("{}:{}:{}", checkpoint.id, state.generation, list.request_id));
            checkpoint.epoch = octopus_run_record::GrantEpoch(state.generation);
            let binding = |interface: InterfaceId| octopus_policy::AuthoredInterfaceBinding {
                interface, config: octopus_policy::InterfaceBindingConfig {
                    artifact_policy: octopus_policy::BindingArtifactPolicy::Allow {
                        names: vec!["operator.user_input.response".into()], namespaces: Vec::new(),
                    },
                },
            };
            checkpoint.interface_binding = Some(binding(self.home_interface.clone()));
            checkpoint.outbound_interface_bindings = vec![binding(self.home_interface.clone())];
            checkpoint.required_verified_artifacts = vec![serde_json::from_value(json!({
                "mark":"operator.input.provided", "artifact_label":"operator.user_input.response",
                "verifier":"operator_input", "params":{"request_id":list.request_id},
            })).map_err(string_error)?];
            checkpoints.push(checkpoint);
        }
        Ok(checkpoints)
    }
}

impl ConsentAuthorityBinding {
    pub fn prepare(config: ConsentAuthorityConfig, policy: &ConsentPolicy) -> Result<(Self, String), String> {
        config.validate()?;
        policy.validate()?;
        let content = consent_genesis_content(&config)?;
        let binding = Self { genesis_run_hash: ContentHash::of_bytes(content.as_bytes()).to_hex(),
            immutable_config_digest: config.digest()?, config, bounds: policy.bounds.clone() };
        Ok((binding, content))
    }
}

pub fn consent_genesis_content(config: &ConsentAuthorityConfig) -> Result<String, String> {
    let checkpoints = config.checkpoints_at(0)?;
    let record = RunRecord { id: config.authority_run.clone(), ee: config.root_ee.clone(), worker: config.authority_worker.clone(),
        trace: config.checkpoints.command.trace.clone(), status: json!("running"),
        interface_state: BTreeMap::from([(config.home_interface.clone(), BTreeMap::from([
            (OWNER_STATE.into(), serde_json::to_value(ConsentState::genesis(config)?).map_err(string_error)?) ]))]),
        active_checkpoints: checkpoints.iter().map(|checkpoint| checkpoint.id.clone()).collect(), checkpoints };
    record.validate_artifact_arrival_shape()?;
    let mut content = serde_json::to_string_pretty(&record).map_err(string_error)?;
    content.push('\n');
    Ok(content)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentRequestRecord {
    pub request_id: String,
    pub call_id: String,
    pub request: ConsentEvidenceRef,
    pub original_call: ConsentEvidenceRef,
    pub owner_interface: InterfaceId,
    pub offer: ConsentOffer,
    pub authorization: Option<ConsentDecisionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_return: Option<SelectedConsentReturn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_return_install: Option<CheckedRunInstall>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentDecisionRecord {
    pub record_id: String,
    pub decision: ConsentDecision,
    pub command: ConsentEvidenceRef,
    pub origin: ConsentDecisionOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConsentDecisionOrigin {
    Human { evidence: ConsentHumanEvidence, persistence: ConsentPersistence },
    Reused { receipt_id: String },
    Policy { request: ConsentEvidenceRef },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentSessionRecord {
    pub session: ConsentSession,
    pub subject: ConsentSubject,
    pub receipt: ConsentDecisionRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentAlwaysRecord {
    pub subject: ConsentSubject,
    pub eligibility_at_approval: ConsentEligibility,
    pub receipt: ConsentDecisionRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentManagementRecord {
    pub request_id: String,
    pub list_command: ConsentEvidenceRef,
    pub displayed_receipts: BTreeMap<String, String>,
    pub removal: Option<ConsentHumanEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsentState {
    pub immutable_config_digest: String,
    pub generation: u64,
    pub predecessor_run_hash: Option<String>,
    pub command: Option<ConsentEvidenceRef>,
    pub always: BTreeMap<String, ConsentAlwaysRecord>,
    pub sessions: BTreeMap<String, ConsentSessionRecord>,
    pub requests: BTreeMap<String, ConsentRequestRecord>,
    pub management: BTreeMap<String, ConsentManagementRecord>,
}

impl ConsentState {
    pub fn genesis(config: &ConsentAuthorityConfig) -> Result<Self, String> {
        Ok(Self { immutable_config_digest: config.digest()?, generation: 0, predecessor_run_hash: None,
            command: None, always: BTreeMap::new(), sessions: BTreeMap::new(), requests: BTreeMap::new(), management: BTreeMap::new() })
    }

    fn validate(&self, binding: &ConsentAuthorityBinding) -> Result<(), String> {
        require(self.immutable_config_digest == binding.immutable_config_digest
            && if self.generation == 0 { self.predecessor_run_hash.is_none() && self.command.is_none() }
                else { self.predecessor_run_hash.is_some() && self.command.is_some() },
            "consent state changed immutable owner configuration or generation evidence")?;
        if let Some(hash) = &self.predecessor_run_hash { exact_hash(hash)?; }
        for (key, record) in &self.always {
            require(*key == record.subject.key()?, "remembered consent changed its exact typed subject")?;
        }
        for (key, record) in &self.sessions {
            require(*key == session_key(&record.session, &record.subject)?, "session consent changed its exact principal or subject")?;
        }
        let count = self.always.len().checked_add(self.sessions.len())
            .and_then(|n| n.checked_add(self.requests.len())).and_then(|n| n.checked_add(self.management.len()))
            .ok_or("consent record count overflow")?;
        require(count <= binding.bounds.max_records
            && self.requests.values().filter(|r| r.authorization.is_none()).count() <= binding.bounds.max_pending,
            "consent state record bound reached; no receipt may be silently evicted")?;
        require(serde_json::to_vec(self).map_err(string_error)?.len() as u64 <= binding.bounds.max_state_bytes,
            "consent state exceeds its byte bound")
    }
}

pub struct ConsentEvidenceReader {
    movements_path: PathBuf,
    ledger_index_path: PathBuf,
    metadata_dir: PathBuf,
    blobs_dir: PathBuf,
    max_row_bytes: u64,
    max_manifest_bytes: u64,
    max_payload_bytes: u64,
    remaining: u64,
    row_offsets: BTreeMap<u64, (u64, u64)>,
}

pub struct AdmittedConsentArtifact {
    movement: Movement,
    metadata: InvocationMetadata,
    manifest: InterfaceArtifactManifest,
    payload: Vec<u8>,
    record_bytes: Vec<u8>,
    record: RunRecord,
}

impl AdmittedConsentArtifact {
    pub fn evidence(&self) -> ConsentEvidenceRef {
        ConsentEvidenceRef { artifact: self.movement.artifact.clone().expect("admitted artifact"), movement_seq: self.movement.seq.0 }
    }
    pub fn record_bytes(&self) -> &[u8] { &self.record_bytes }
    pub fn payload_bytes(&self) -> &[u8] { &self.payload }
}

impl octopus_run_authoring::CheckedRunEvidenceReader for ConsentEvidenceReader {
    fn max_payload_bytes(&self) -> u64 { self.max_payload_bytes }
    fn movement(&mut self, seq: u64) -> Result<Option<Movement>, String> {
        ConsentEvidenceReader::movement(self, seq).map(Some)
    }
    fn artifact(&mut self, reference: &ArtifactRef) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
        ConsentEvidenceReader::artifact(self, reference)
    }
    fn snapshot(&mut self, hash: &ContentHash) -> Result<Vec<u8>, String> {
        self.blob(hash, self.max_payload_bytes)
    }
}

impl ConsentEvidenceReader {
    pub fn new(ctx: &InterfaceLiftContext, bounds: &ConsentBounds) -> Result<Self, String> {
        let limits = ctx.required_host_io_limits()?;
        Self::from_read_handles(&ctx.movements_path, &ctx.ledger_index_path, &ctx.blobs_dir,
            limits.max_control_frame_bytes, limits.max_manifest_bytes, limits.max_payload_bytes, bounds)
    }

    pub fn from_read_handles(movements_path: &Path, ledger_index_path: &Path, blobs_dir: &Path,
        max_row_bytes: u64, max_manifest_bytes: u64, max_payload_bytes: u64, bounds: &ConsentBounds) -> Result<Self, String> {
        bounds.validate()?;
        require([movements_path, ledger_index_path, blobs_dir].into_iter().all(Path::is_absolute),
            "application consent requires its own explicit ledger, index and blob read handles")?;
        require([max_row_bytes, max_manifest_bytes, max_payload_bytes].into_iter().all(|limit| limit > 0),
            "application consent read limits must be positive")?;
        usize::try_from(max_row_bytes).map_err(|_| "consent ledger row ceiling exceeds host address space".to_string())?;
        let metadata_dir = movements_path.parent().ok_or("consent ledger read handle has no metadata directory")?.join("metadata");
        Ok(Self { movements_path: movements_path.to_path_buf(), ledger_index_path: ledger_index_path.to_path_buf(), metadata_dir,
            blobs_dir: blobs_dir.to_path_buf(), max_row_bytes, max_manifest_bytes, max_payload_bytes,
            remaining: bounds.max_proof_bytes, row_offsets: BTreeMap::new() })
    }

    fn charge(&mut self, bytes: u64) -> Result<(), String> {
        self.remaining = self.remaining.checked_sub(bytes).ok_or("consent immutable evidence exceeds its bounded proof budget")?;
        Ok(())
    }

    pub fn movement(&mut self, seq: u64) -> Result<Movement, String> {
        let range = self.row_offsets.get(&seq).copied().or_else(|| {
            LedgerSqliteReader::open(&self.ledger_index_path).ok()
                .and_then(|index| index.movement_row_range(seq).ok().flatten())
                .map(|range| (range.start_offset, range.end_offset))
        });
        let file = std::fs::File::open(&self.movements_path).map_err(string_error)?;
        let mut reader = std::io::BufReader::new(file);
        if let Some((start, end)) = range {
            require(end.checked_sub(start).is_some_and(|len| len > 0 && len <= self.max_row_bytes)
                && end <= reader.get_ref().metadata().map_err(string_error)?.len(), "consent index has no exact durable byte range")?;
            reader.seek(SeekFrom::Start(start)).map_err(string_error)?;
            let movement = self.read_row(&mut reader, seq)?;
            require(reader.stream_position().map_err(string_error)? == end, "consent index differs from the authoritative complete row")?;
            return Ok(movement);
        }
        let (mut current, offset) = match self.row_offsets.range(..seq).next_back() {
            Some((previous, (_, end))) => (previous.checked_add(1).ok_or("consent sequence overflow")?, *end),
            None => (0, 0),
        };
        reader.seek(SeekFrom::Start(offset)).map_err(string_error)?;
        loop {
            let movement = self.read_row(&mut reader, current)?;
            if current == seq { return Ok(movement); }
            current = current.checked_add(1).ok_or("consent sequence overflow")?;
        }
    }

    fn read_row(&mut self, reader: &mut std::io::BufReader<std::fs::File>, seq: u64) -> Result<Movement, String> {
        let start = reader.stream_position().map_err(string_error)?;
        let limit = self.max_row_bytes.min(self.remaining);
        require(limit > 0, "consent authoritative ledger scan exhausted its proof budget")?;
        let mut row = Vec::new();
        let len = Read::by_ref(reader).take(limit.checked_add(1).ok_or("consent row limit overflow")?).read_until(b'\n', &mut row).map_err(string_error)?;
        require(len > 0 && len as u64 <= limit, "consent authoritative ledger row is absent or exceeds its bound")?;
        self.charge(len as u64)?;
        require(row.last() == Some(&b'\n'), "consent evidence names an incomplete ledger row")?;
        let line = usize::try_from(seq).map_err(string_error)?;
        let movement = octopus_ledger::decode_durable_movement_row(&row, line, |kind, hash| {
            let mut resolve = || -> Result<Value, String> {
                let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&self.metadata_dir.join(hash.to_hex()),
                    self.max_payload_bytes.min(self.remaining)).map_err(string_error)?;
                self.charge(bytes.len() as u64)?;
                require(ContentHash::of_bytes(&bytes) == hash, &format!("consent {} changed its immutable content address", kind.describe()))?;
                serde_json::from_slice(&bytes).map_err(string_error)
            };
            resolve().map_err(|message| octopus_ledger::LedgerError::MalformedMovementMetadata { line, message })
        }).map_err(string_error)?;
        require(movement.seq.0 == seq, "consent authoritative ledger has a sequence gap or a wrong indexed anchor")?;
        let end = start.checked_add(len as u64).ok_or("consent ledger offset overflow")?;
        if let Some(existing) = self.row_offsets.insert(seq, (start, end)) {
            require(existing == (start, end), "consent evidence row changed its original durable byte range")?;
        }
        Ok(movement)
    }

    fn run_interval(&mut self, run: &RunId, after: u64, before: u64, maximum: usize) -> Result<Vec<Movement>, String> {
        let mut result = Vec::new();
        self.visit_run_interval(run, after, before, maximum, |movement| {
            result.push(movement);
            Ok(false)
        })?;
        Ok(result)
    }

    fn visit_run_interval(&mut self, run: &RunId, after: u64, before: u64, maximum: usize,
        mut visit: impl FnMut(Movement) -> Result<bool, String>) -> Result<(), String> {
        require(after < before, "consent evidence interval is empty or reversed")?;
        self.movement(after)?;
        let offset = self.row_offsets.get(&after).ok_or("consent evidence interval has no verified byte anchor")?.0;
        let mut reader = std::io::BufReader::new(std::fs::File::open(&self.movements_path).map_err(string_error)?);
        reader.seek(SeekFrom::Start(offset)).map_err(string_error)?;
        let mut count = 0;
        for seq in after..before {
            let movement = self.read_row(&mut reader, seq)?;
            if movement.kind != MovementKind::InterfaceOutbound { continue; }
            let runtime = movement.invocation_metadata.as_ref().and_then(|m| m.invocation())
                .and_then(|m| m.runtime.as_ref()).ok_or("consent evidence interval has an outbound without runtime provenance")?;
            if &runtime.run != run { continue; }
            require(count < maximum, "consent authority interval exceeds its per-head movement bound")?;
            count += 1;
            if visit(movement)? { break; }
        }
        Ok(())
    }

    fn blob(&mut self, hash: &ContentHash, limit: u64) -> Result<Vec<u8>, String> {
        let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&self.blobs_dir.join(hash.to_hex()),
            limit.min(self.max_payload_bytes).min(self.remaining)).map_err(string_error)?;
        self.charge(bytes.len() as u64)?;
        require(ContentHash::of_bytes(&bytes) == *hash, "consent evidence blob changed content")?;
        Ok(bytes)
    }

    fn artifact(&mut self, reference: &ArtifactRef) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
        let bytes = self.blob(&reference.content, self.max_manifest_bytes)?;
        let manifest: InterfaceArtifactManifest = serde_json::from_slice(&bytes).map_err(string_error)?;
        require(reference.id.as_str() == format!("{}:{}", manifest.label, reference.content.to_hex()),
            "consent artifact identity does not match its immutable manifest")?;
        let hash = exact_hash(manifest.payload_content_hash.as_deref().ok_or("consent manifest has no payload digest")?)?;
        let payload = self.blob(&hash, self.max_payload_bytes)?;
        Ok((manifest, payload))
    }

    pub fn admitted(&mut self, evidence: &ConsentEvidenceRef) -> Result<AdmittedConsentArtifact, String> {
        let movement = self.movement(evidence.movement_seq)?;
        require(movement.kind == MovementKind::InterfaceOutbound && movement.artifact.as_ref() == Some(&evidence.artifact),
            "consent evidence is not the exact admitted outbound artifact")?;
        let metadata = movement.invocation_metadata.as_ref().and_then(|m| m.invocation()).cloned()
            .ok_or("consent evidence has no runtime metadata")?;
        let runtime = metadata.runtime.as_ref().ok_or("consent evidence has no runtime coordinates")?;
        let record_hash = exact_hash(runtime.run_record_hash.as_deref().ok_or("consent evidence has no pinned RunRecord")?)?;
        let record_bytes = self.blob(&record_hash, self.max_payload_bytes)?;
        let record = RunRecord::from_bytes(&record_bytes).map_err(string_error)?;
        let checkpoint = record.checkpoint(&runtime.checkpoint).ok_or("consent evidence checkpoint is absent from its pinned RunRecord")?;
        let inbound = checkpoint.interface_binding.as_ref().ok_or("consent evidence checkpoint has no inbound binding")?;
        let Endpoint::Interface(target) = &movement.to;
        let snapshot = CheckpointBindingPolicySnapshot { inbound: inbound.clone(), outbound: checkpoint.outbound_interface_bindings.clone() };
        require(runtime.run == record.id && runtime.ee == record.ee && runtime.worker == record.worker
            && runtime.interface == *target && runtime.epoch == checkpoint.epoch.0 && movement.trace == checkpoint.trace
            && record.active_checkpoints.contains(&checkpoint.id)
            && movement.from == Endpoint::Interface(inbound.interface.clone())
            && checkpoint.outbound_interfaces().any(|i| i == target)
            && metadata.checkpoint_binding_policy.as_ref() == Some(&snapshot)
            && metadata.outbound_interface_bindings == checkpoint.outbound_interfaces().cloned().collect::<Vec<_>>()
            && metadata.required_verified_artifacts == checkpoint.required_verified_artifacts
            && metadata.verification_failure.is_none(), "consent evidence differs from its actual admitted checkpoint")?;
        let bound_ids = metadata.artifact_binding_refs.iter().map(|r| &r.id).collect::<BTreeSet<_>>();
        require(bound_ids.len() == metadata.artifact_binding_refs.len()
            && bound_ids == checkpoint.artifact_bindings.iter().map(|b| &b.id).collect::<BTreeSet<_>>()
            && checkpoint.artifact_bindings.iter().all(|b| !b.access.read || metadata.grant.artifact_access.get(b.id.as_str()).is_some_and(|a| a.read)),
            "consent evidence lost its exact formed readable bindings")?;
        let (manifest, payload) = self.artifact(&evidence.artifact)?;
        require(metadata.checkpoint_admission.as_ref().is_some_and(|a| a.complete && a.artifact_label == manifest.label
            && a.required_artifact_labels == checkpoint.required_verified_artifacts.iter().map(|r| r.artifact_label.clone()).collect::<Vec<_>>())
            && inbound.admits(&manifest.label)
            && checkpoint.outbound_interface_bindings.iter().filter(|b| b.interface == *target).all(|b| b.admits(&manifest.label)),
            "consent artifact was not admitted against its exact named checkpoint")?;
        Ok(AdmittedConsentArtifact { movement, metadata, manifest, payload, record_bytes, record })
    }

    fn admitted_requirement_mark(&mut self, admitted: &AdmittedConsentArtifact, after: u64) -> Result<u64, String> {
        let runtime = admitted.metadata.runtime.as_ref().ok_or("consent reply has no admitted runtime")?;
        let [requirement] = admitted.metadata.required_verified_artifacts.as_slice() else {
            return Err("consent reply requires one exact verifier outcome".into());
        };
        require(admitted.metadata.grant.mark_outcomes.contains(&requirement.mark.to_string()),
            "consent reply mark is outside the actual formed grant")?;
        let mut found = None;
        let first = after.checked_add(1).ok_or("consent reply sequence overflow")?;
        for seq in first..admitted.movement.seq.0 {
            let movement = self.movement(seq)?;
            if movement.artifact.as_ref() != admitted.movement.artifact.as_ref() { continue; }
            let Some(mark) = movement.invocation_metadata.as_ref().and_then(|metadata| metadata.mark_outcome()) else { continue; };
            if mark.run != runtime.run || mark.subject != runtime.checkpoint || mark.run_epoch != runtime.epoch { continue; }
            require(movement.kind == MovementKind::MarkOutcomeAdmitted(requirement.mark.clone())
                && mark.evidence.as_ref() == admitted.movement.artifact.as_ref()
                && mark.name == requirement.mark && mark.verifier == requirement.verifier
                && mark.verifier_package.is_some() && movement.trace == admitted.movement.trace,
                "consent reply mark changed its exact admitted requirement")?;
            require(found.replace(seq).is_none(), "consent reply has ambiguous verifier outcomes")?;
        }
        found.ok_or("consent reply has no actual admitted verifier outcome".into())
    }

    pub fn selected_human_evidence(&mut self, admitted: &AdmittedConsentArtifact,
        request_id: &str, call_id: &str, offer: &ConsentOffer, owner: &InterfaceId,
        client: &InterfaceId, after: u64) -> Result<ConsentHumanEvidence, String> {
        require(offer.session.client_interface == *client && admitted.movement.from == Endpoint::Interface(client.clone())
            && admitted.movement.to == Endpoint::Interface(owner.clone()) && admitted.movement.seq.0 > after,
            "selected human response changed its exact task client, owner or checked prompt order")?;
        let body: HumanApprovalBody = serde_json::from_slice(&admitted.payload).map_err(string_error)?;
        let answer = body.operator_approval_response;
        require(answer.request_id == request_id && answer.call_id == call_id && offer.allows(answer.decision, answer.selected_persistence),
            "selected human response differs from its immutable offered choices")?;
        let evidence = ConsentHumanEvidence {response:admitted.evidence(), mark_seq:self.admitted_requirement_mark(admitted, after)?};
        self.human(&evidence, request_id, Some(call_id), &offer.session, owner,
            answer.decision == ConsentDecision::Accept, "operator.approval.response")?;
        Ok(evidence)
    }

    fn human(&mut self, evidence: &ConsentHumanEvidence, request_id: &str, call_id: Option<&str>,
        session: &ConsentSession, target: &InterfaceId, accepted: bool, label: &str) -> Result<AdmittedConsentArtifact, String> {
        let admitted = self.admitted(&evidence.response)?;
        let runtime = admitted.metadata.runtime.as_ref().expect("admitted runtime");
        let (mark_name, verifier) = if accepted { ("operator.approval", "operator_approval") }
            else { ("operator.input.provided", "operator_input") };
        let params = if let Some(call_id) = call_id { json!({"request_id":request_id,"call_id":call_id}) }
            else { json!({"request_id":request_id}) };
        let [requirement] = admitted.metadata.required_verified_artifacts.as_slice() else {
            return Err("consent human response needs its one exact verifier requirement".into());
        };
        require(admitted.record.id == session.run && admitted.movement.from == Endpoint::Interface(session.client_interface.clone())
            && admitted.movement.to == Endpoint::Interface(target.clone()) && admitted.manifest.label == label
            && requirement.artifact_label == label && requirement.params == params
            && requirement.mark.as_str() == mark_name && requirement.verifier.as_str() == verifier
            && admitted.metadata.grant.mark_outcomes.iter().any(|m| m == mark_name)
            && evidence.mark_seq < evidence.response.movement_seq, "consent human evidence belongs to another principal or requirement")?;
        let mark_movement = self.movement(evidence.mark_seq)?;
        let mark = mark_movement.invocation_metadata.as_ref().and_then(|m| m.mark_outcome())
            .ok_or("consent human evidence does not name an actual admitted mark")?;
        require(mark_movement.kind == MovementKind::MarkOutcomeAdmitted(requirement.mark.clone())
            && mark_movement.artifact.as_ref() == Some(&evidence.response.artifact)
            && mark.evidence.as_ref() == Some(&evidence.response.artifact)
            && mark.name == requirement.mark && mark.verifier == requirement.verifier
            && mark.verifier_package.is_some() && mark.run == runtime.run && mark.subject == runtime.checkpoint
            && mark.run_epoch == runtime.epoch && mark_movement.trace == admitted.movement.trace,
            "consent human mark differs from the admitted response")?;
        Ok(admitted)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingOriginal {
    run: RunId,
    ee: EeId,
    worker: InterfaceId,
    door_interface: InterfaceId,
    tool: String,
    correlation_id: String,
    arguments: Value,
    source_movement_seq: u64,
    source_artifact: ArtifactRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingScope { worker_environment_id: String, formation_pin: String, host_generation: String }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingResume { tool: String, correlation_id: String }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingApplication {
    bundle_id: String,
    bundle_path: String,
    display_name: String,
    instance: Option<PendingInstance>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingInstance { app: PendingAppRef, process_id: i32, launch_time_unix_ms: Option<u64> }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingAppRef { host_generation: String, app_instance_id: String }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingApplicationRequest {
    schema_version: u32,
    request_id: String,
    call_id: String,
    original: PendingOriginal,
    scope: PendingScope,
    application: PendingApplication,
    approval_scope: String,
    resume: PendingResume,
    consent_offer: ConsentOffer,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingUploadFile { artifact_ref: ArtifactRef, filename: String, payload_sha256: String, size_bytes: u64 }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingUpload {
    browser_context_id: String, target_id: String, reservation_id: String, file_chooser_id: String,
    document_token: String, backend_node_id: u64, top_level_origin: String, files: Vec<PendingUploadFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingUploadRequest {
    schema_version: u32, request_id: String, call_id: String, original: PendingOriginal, scope: PendingScope,
    approval_scope: String, upload: PendingUpload, resume: PendingResume, consent_offer: ConsentOffer,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingHistoryQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    queries: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingHistory { source: ArtifactRef, source_payload_digest: String, source_epoch: String, query: PendingHistoryQuery }

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingHistoryRequest {
    schema_version: u32, request_id: String, call_id: String, original: PendingOriginal, scope: PendingScope,
    approval_scope: String, history: PendingHistory, resume: PendingResume, consent_offer: ConsentOffer,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanApprovalBody { operator_approval_response: HumanApproval }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanApproval {
    request_id: String,
    call_id: String,
    decision: ConsentDecision,
    selected_persistence: ConsentPersistence,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanRemovalBody { operator_user_input_response: HumanRemoval }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanRemoval { request_id: String, answers: RemovalAnswer }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemovalAnswer { subject: ConsentSubject, receipt_id: String, remove: bool }

pub struct VerifiedConsentIntake {
    evidence: ConsentEvidenceRef,
    operation: VerifiedConsentOperation,
}

enum VerifiedConsentOperation {
    Query(ConsentRequestRecord),
    Human { request_id: String, human: ConsentHumanEvidence, decision: ConsentDecision, persistence: ConsentPersistence },
    List,
    Remove { request_id: String, subject: ConsentSubject, receipt_id: String, human: ConsentHumanEvidence },
}

pub struct ConsentAuthority {
    reference: ArtifactRef,
    binding: ConsentAuthorityBinding,
    policy: ConsentPolicy,
    genesis: Vec<u8>,
    head: Vec<u8>,
    state: ConsentState,
    last_result_seq: Option<u64>,
    checked_heads: BTreeSet<String>,
}

impl ConsentAuthority {
    pub fn load(reader: &mut ConsentEvidenceReader, reference: &ArtifactRef,
        admitted: &AdmittedConsentArtifact) -> Result<Self, String> {
        require_bound(&admitted.metadata, reference)?;
        let binding_bytes = reader.blob(&reference.content, 65536)?;
        let binding: ConsentAuthorityBinding = serde_json::from_slice(&binding_bytes).map_err(string_error)?;
        binding.config.validate()?;
        binding.bounds.validate()?;
        require(binding.config.authority_artifact == reference.id
            && binding.immutable_config_digest == binding.config.digest()?, "consent authority binding changed its exact immutable configuration")?;
        require_bound(&admitted.metadata, &binding.config.policy)?;
        let policy_bytes = reader.blob(&binding.config.policy.content, binding.bounds.max_policy_bytes)?;
        let policy: ConsentPolicy = serde_json::from_slice(&policy_bytes).map_err(string_error)?;
        policy.validate()?;
        require(policy.bounds == binding.bounds, "consent binding changes its source-owned bounds")?;
        let genesis = consent_genesis_content(&binding.config)?.into_bytes();
        require(ContentHash::of_bytes(&genesis) == exact_hash(&binding.genesis_run_hash)?,
            "authority binding does not commit the exact independently authored genesis")?;
        let record = RunRecord::from_bytes(&genesis).map_err(string_error)?;
        let state = state_from_record(&record, &binding)?;
        require(state == ConsentState::genesis(&binding.config)?, "consent genesis contains unproven reusable decisions")?;
        validate_record(&record, &binding, &state)?;
        Ok(Self { reference: reference.clone(), binding, policy, checked_heads: BTreeSet::from([ContentHash::of_bytes(&genesis).to_hex()]),
            head: genesis.clone(), genesis, state, last_result_seq: None })
    }

    pub fn binding(&self) -> &ConsentAuthorityBinding { &self.binding }
    pub fn reference(&self) -> &ArtifactRef { &self.reference }
    pub fn state(&self) -> &ConsentState { &self.state }
    pub fn head_hash(&self) -> String { ContentHash::of_bytes(&self.head).to_hex() }

    pub fn verify_intake(&self, reader: &mut ConsentEvidenceReader, evidence: &ConsentEvidenceRef) -> Result<VerifiedConsentIntake, String> {
        let admitted = reader.admitted(evidence)?;
        require(admitted.movement.to == Endpoint::Interface(self.binding.config.home_interface.clone())
            && admitted.manifest.label == "application.consent.intake", "consent intake did not reach its exact Home owner")?;
        require_bound(&admitted.metadata, &self.reference)?;
        let intake: ConsentIntake = serde_json::from_slice(&admitted.payload).map_err(string_error)?;
        require(intake.authority() == &self.reference, "consent intake names a different authority artifact")?;
        let operation = match intake {
            ConsentIntake::Query { request, original_call, selected_return, .. } => {
                let mut request_record = self.verify_request(reader, &request, &original_call)?;
                require(admitted.record.id == request_record.offer.session.run
                    && admitted.movement.from == Endpoint::Interface(request_record.owner_interface.clone())
                    && request.movement_seq < evidence.movement_seq,
                    "consent query has another task principal or precedes its native pending request")?;
                if let Some(selected) = &selected_return {
                    request_record.selected_return_install = Some(self.verify_selected_return(reader, &request_record, selected, &admitted)?);
                }
                request_record.selected_return = selected_return;
                VerifiedConsentOperation::Query(request_record)
            }
            ConsentIntake::RecordHuman { request_id, human, human_required, .. } => {
                let request = self.state.requests.get(&request_id).ok_or("human consent has no preserved pending request")?;
                require(request.authorization.is_none() && admitted.record.id == request.offer.session.run
                    && admitted.movement.from == Endpoint::Interface(request.owner_interface.clone())
                    && human.response.movement_seq < evidence.movement_seq && request.request.movement_seq < human.mark_seq,
                    "human consent is stale, already settled or belongs to another task")?;
                let response = reader.admitted(&human.response)?;
                let body: HumanApprovalBody = serde_json::from_slice(&response.payload).map_err(string_error)?;
                let answer = body.operator_approval_response;
                require(answer.request_id == request_id && answer.call_id == request.call_id
                    && request.offer.allows(answer.decision, answer.selected_persistence),
                    "human consent answer differs from the immutable offered choice")?;
                reader.human(&human, &request_id, Some(&request.call_id), &request.offer.session,
                    &request.owner_interface, answer.decision == ConsentDecision::Accept, "operator.approval.response")?;
                if let Some(selected) = &request.selected_return {
                    let evidence = human_required.as_ref().ok_or("selected human decision has no admitted checked human-required reply")?;
                    require(evidence.movement_seq < human.mark_seq, "human decision precedes the checked Home human-required reply")?;
                    let reply = reader.admitted(evidence)?;
                    let (prefix, proof) = self.checked_query_prefix(reader, &reply)?;
                    require(prefix.validate_human_required_reply(reader, &reply, &proof)? == *request,
                        "human decision changed its original Home-checked pending request")?;
                    let install = request.selected_return_install.as_ref().ok_or("selected human decision lost its checked return installation")?;
                    let installed = reader.admitted(&install.result)?;
                    octopus_run_authoring::checked_return_interval(reader, selected, &installed.record_bytes, &admitted.record_bytes,
                        install.result.movement_seq, admitted.movement.seq.0,
                        &[selected.human_required.clone(), selected.approved.clone(), selected.declined.clone()])?;
                    octopus_run_authoring::validate_selected_return_reads(&admitted.metadata, &selected.selected_artifacts)?;
                } else {
                    require(human_required.is_none(), "unselected human decision supplied an undeclared Home continuation")?;
                }
                VerifiedConsentOperation::Human { request_id, human, decision: answer.decision, persistence: answer.selected_persistence }
            }
            ConsentIntake::ListAlways { .. } => {
                self.require_home_intake(&admitted)?;
                VerifiedConsentOperation::List
            }
            ConsentIntake::RemoveAlways { request_id, subject, receipt_id, human, .. } => {
                self.require_home_intake(&admitted)?;
                let list = self.state.management.get(&request_id).ok_or("consent removal has no preserved list request")?;
                require(list.removal.is_none() && list.displayed_receipts.get(&subject.key()?) == Some(&receipt_id)
                    && human.response.movement_seq < evidence.movement_seq && list.list_command.movement_seq < human.mark_seq,
                    "consent removal changed its listed receipt or reused an old confirmation")?;
                let home_session = ConsentSession { run: self.binding.config.authority_run.clone(), client_interface: self.binding.config.home_interface.clone() };
                let response = reader.human(&human, &request_id, None, &home_session, &self.binding.config.home_interface,
                    false, "operator.user_input.response")?;
                let body: HumanRemovalBody = serde_json::from_slice(&response.payload).map_err(string_error)?;
                let answer = body.operator_user_input_response;
                require(answer.request_id == request_id && answer.answers.subject == subject
                    && answer.answers.receipt_id == receipt_id && answer.answers.remove, "consent removal is not the exact confirmed listed receipt")?;
                VerifiedConsentOperation::Remove { request_id, subject, receipt_id, human }
            }
        };
        Ok(VerifiedConsentIntake { evidence: evidence.clone(), operation })
    }

    fn verify_selected_return(
        &self, reader: &mut ConsentEvidenceReader, request: &ConsentRequestRecord,
        selected: &SelectedConsentReturn, intake: &AdmittedConsentArtifact,
    ) -> Result<CheckedRunInstall, String> {
        use octopus_run_authoring::{checked_run_install, checked_return_interval,
            validate_checked_owner_transition, validate_checked_provider_transition, validate_selected_return_reads};
        let owner = selected.owner_writer.as_ref().unwrap_or(&selected.provider_writer);
        require(owner.owner == request.owner_interface && request.offer.session.client_interface != self.binding.config.home_interface
            && selected.original_install.result.movement_seq < request.original_call.movement_seq
            && request.request.movement_seq < intake.movement.seq.0,
            "selected consent return changed its original installation, owner or task client")?;
        let original = reader.admitted(&request.original_call)?;
        let pending = reader.admitted(&request.request)?;
        require(selected.selected_artifacts.iter().any(|reference| reference == &self.reference)
            && selected.selected_artifacts.iter().any(|reference| reference == &self.binding.config.policy),
            "selected consent return omitted its exact authority or policy read binding")?;
        for admitted in [&original, &pending, intake] {
            validate_selected_return_reads(&admitted.metadata, &selected.selected_artifacts)?;
        }
        let result: ToolResultArtifact = serde_json::from_slice(&pending.payload).map_err(string_error)?;
        let request_value = result.body.as_ref().and_then(|body| body.get("operator_request"))
            .ok_or("selected consent return lost its original pending request")?;
        let pending_request: octopus_app_server_interface::OwnerOperatorRequest = serde_json::from_value(request_value.clone()).map_err(string_error)?;
        pending_request.validate_bound_files(&intake.metadata)?;
        let mut writers = vec![selected.provider_writer.clone()];
        writers.extend(selected.owner_writer.clone());
        let original_install = checked_run_install(reader, &selected.run_path, &selected.original_install, &writers)?;
        if original_install.writer == selected.provider_writer { validate_checked_provider_transition(&original_install)?; }
        else if Some(&original_install.writer) == selected.owner_writer.as_ref() { validate_checked_owner_transition(&original_install)?; }
        else { return Err("selected original installation belongs to another writer".into()); }
        let original_checkpoint = original.metadata.runtime.as_ref().expect("admitted runtime").checkpoint.clone();
        let protected = [selected.human_required.clone(), selected.approved.clone(), selected.declined.clone()];
        require(protected.iter().collect::<BTreeSet<_>>().len() == 3,
            "selected consent return repeats a phase checkpoint")?;
        let initial = RunRecord::from_bytes(&original_install.after).map_err(string_error)?;
        require(protected.iter().all(|id| initial.checkpoint(id).is_none()),
            "selected consent return substituted a preexisting reply checkpoint")?;
        let chain = checked_return_interval(reader, selected, &original_install.after, &intake.record_bytes,
            selected.original_install.result.movement_seq, intake.movement.seq.0, &[])?;
        let mut return_install = None;
        for transition in &chain {
            if transition.result.seq.0 < original.movement.seq.0 {
                octopus_run_authoring::validate_unchanged_admitted_checkpoint(&transition.before, &transition.after, &original_checkpoint)?;
            }
            let prior = RunRecord::from_bytes(&transition.before).map_err(string_error)?;
            let next = RunRecord::from_bytes(&transition.after).map_err(string_error)?;
            if return_install.is_some() {
                for id in &protected { octopus_run_authoring::validate_unchanged_admitted_checkpoint(&transition.before, &transition.after, id)?; }
            } else if protected.iter().any(|id| next.checkpoint(id).is_some()) {
                require(protected.iter().all(|id| prior.checkpoint(id).is_none())
                    && transition.writer == *owner && transition.call.seq.0 > request.request.movement_seq,
                    "selected return was not installed by the original owner after the pending request")?;
                self.validate_selected_return_rows(&next, selected, request, &pending_request.original.door_interface)?;
                return_install = Some(CheckedRunInstall {
                    call: ConsentEvidenceRef { artifact: transition.call.artifact.clone().ok_or("selected return install lost its call artifact")?, movement_seq: transition.call.seq.0 },
                    result: ConsentEvidenceRef { artifact: transition.result.artifact.clone().ok_or("selected return install lost its result artifact")?, movement_seq: transition.result.seq.0 },
                });
            }
        }
        self.validate_selected_return_rows(&intake.record, selected, request, &pending_request.original.door_interface)?;
        return_install.ok_or("selected consent return has no actual checked owner installation".into())
    }

    fn validate_selected_return_rows(&self, record: &RunRecord, selected: &SelectedConsentReturn,
        request: &ConsentRequestRecord, door: &InterfaceId) -> Result<(), String> {
        require(record.id == request.offer.session.run, "selected consent reply belongs to another task")?;
        for (id, decision) in [(&selected.human_required, None), (&selected.approved, Some(true)), (&selected.declined, Some(false))] {
            let checkpoint = record.checkpoint(id).ok_or("selected consent reply checkpoint is absent")?;
            let label = if decision.is_some() { "operator.approval.response".to_string() }
                else { format!("application.consent.reply:{}", request.request_id) };
            let targets = if decision.is_some() { vec![&request.owner_interface, door] } else { vec![&request.owner_interface] };
            require(record.active_checkpoints.contains(id) && checkpoint.inbound_interface() == Some(&self.binding.config.home_interface)
                && checkpoint.outbound_interfaces().collect::<Vec<_>>() == targets
                && checkpoint.interface_binding.as_ref().is_some_and(|binding| exact_name_binding(binding, &label))
                && checkpoint.outbound_interface_bindings.iter().all(|binding| exact_name_binding(binding, &label))
                && selected.selected_artifacts.iter().all(|reference| checkpoint.artifact_bindings.iter()
                    .filter(|binding| binding.id == reference.id).count() == 1
                    && checkpoint.artifact_bindings.iter().any(|binding| binding.id == reference.id && binding.access.read)),
                "selected consent return changed its exact ordered edges or immutable read bindings")?;
            match decision {
                Some(accepted) => {
                    let [requirement] = checkpoint.required_verified_artifacts.as_slice() else {
                        return Err("selected consent decision requires one verifier-gated artifact".into());
                    };
                    require(requirement.mark.as_str() == if accepted { "operator.approval" } else { "operator.input.provided" }
                        && requirement.verifier.as_str() == if accepted { "operator_approval" } else { "operator_input" }
                        && requirement.artifact_label == label
                        && requirement.params == json!({"request_id":request.request_id,"call_id":request.call_id}),
                        "selected consent decision changed its exact verifier requirement")?;
                }
                None => require(checkpoint.required_verified_artifacts.is_empty(), "human-required reply changed its owner-only contract")?,
            }
        }
        Ok(())
    }

    fn require_home_intake(&self, admitted: &AdmittedConsentArtifact) -> Result<(), String> {
        require(self.checked_heads.contains(&ContentHash::of_bytes(&admitted.record_bytes).to_hex()),
            "consent management head has no checked authority transition")?;
        self.require_authority_edge(admitted, &admitted.record_bytes, &self.binding.config.home_interface,
            &self.binding.config.home_interface, 3)
    }

    fn require_passive_management(&self, admitted: &AdmittedConsentArtifact) -> Result<(), String> {
        let state = state_from_record(&admitted.record, &self.binding)?;
        validate_record(&admitted.record, &self.binding, &state)?;
        let checkpoint_id = &admitted.metadata.runtime.as_ref().ok_or("management has no admitted runtime")?.checkpoint;
        let index = admitted.record.checkpoints.iter().position(|c| &c.id == checkpoint_id)
            .ok_or("management checkpoint is not in the actual admitted snapshot")?;
        require(index == 3 || index >= 5, "authority has an overlapping admitted command, publication or result before its checked result")?;
        self.require_authority_edge(admitted, &admitted.record_bytes, &self.binding.config.home_interface,
            &self.binding.config.home_interface, index)?;
        if index == 3 {
            let intake: ConsentIntake = serde_json::from_slice(&admitted.payload).map_err(string_error)?;
            require(intake.authority() == &self.reference
                && matches!(intake, ConsentIntake::ListAlways { .. } | ConsentIntake::RemoveAlways { .. }),
                "authority management edge received another owner's intake")
        } else {
            let body: HumanRemovalBody = serde_json::from_slice(&admitted.payload).map_err(string_error)?;
            let answer = body.operator_user_input_response;
            let list = state.management.get(&answer.request_id).ok_or("management response has no preserved list")?;
            require(list.removal.is_none() && answer.answers.remove
                && list.displayed_receipts.get(&answer.answers.subject.key()?) == Some(&answer.answers.receipt_id),
                "management response changed its displayed exact receipt")
        }
    }

    fn verify_request(&self, reader: &mut ConsentEvidenceReader, request_ref: &ConsentEvidenceRef,
        original_ref: &ConsentEvidenceRef) -> Result<ConsentRequestRecord, String> {
        let request = reader.admitted(request_ref)?;
        let original = reader.admitted(original_ref)?;
        for metadata in [&request.metadata, &original.metadata] {
            require_bound(metadata, &self.reference)?;
            require_bound(metadata, &self.binding.config.policy)?;
        }
        let result: ToolResultArtifact = serde_json::from_slice(&request.payload).map_err(string_error)?;
        let body = result.body.as_ref().and_then(|body| body.get("operator_request")).ok_or("consent query has no owner-authored pending request")?;
        let (version, request_id, call_id, pending_original, scope, resume, offer, commitment, correlation_prefix) = match body.get("approval_scope").and_then(Value::as_str) {
            Some("formed_app_target") => {
                let pending: PendingApplicationRequest = serde_json::from_value(body.clone()).map_err(string_error)?;
                let native = pending.consent_offer.native_policy().ok_or("application request has non-native eligibility")?;
                require(native.target == NativeConsentTarget { bundle_id: pending.application.bundle_id.clone(),
                    bundle_path: pending.application.bundle_path.clone(), display_name: pending.application.display_name.clone() },
                    "application consent changed its original canonical native target")?;
                if let Some(instance) = &pending.application.instance {
                    exact_text(&instance.app.app_instance_id)?;
                    require(instance.app.host_generation == pending.scope.host_generation && instance.process_id > 0
                        && instance.launch_time_unix_ms != Some(0), "consent pending instance has a different host or fabricated launch identity")?;
                }
                let commitment = json!({"schema_version":pending.schema_version,"original":pending.original,"scope":pending.scope,
                    "application":pending.application,"approval_scope":pending.approval_scope,"consent_offer":pending.consent_offer});
                (pending.schema_version, pending.request_id, pending.call_id, pending.original, pending.scope,
                    pending.resume, pending.consent_offer, commitment, "app-resume")
            }
            Some("browser_file_upload") => {
                let pending: PendingUploadRequest = serde_json::from_value(body.clone()).map_err(string_error)?;
                require(pending.consent_offer.subject == ConsentSubject::BrowserOrigin {
                    origin: pending.upload.top_level_origin.clone(), operation: BrowserConsentOperation::Upload },
                    "upload consent changed its observed origin or transfer direction")?;
                for text in [&pending.upload.browser_context_id, &pending.upload.target_id, &pending.upload.reservation_id,
                    &pending.upload.file_chooser_id, &pending.upload.document_token] { exact_text(text)?; }
                for token in [&pending.upload.reservation_id, &pending.upload.file_chooser_id, &pending.upload.document_token] {
                    require(token.len() == 32 && token.bytes().all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
                        "upload consent lost its native immutable reservation or document")?;
                }
                require(pending.upload.backend_node_id > 0 && pending.upload.backend_node_id <= 9_007_199_254_740_991
                    && !pending.upload.files.is_empty() && pending.upload.files.len() <= self.binding.bounds.max_records,
                    "upload consent has invalid node or file bounds")?;
                let mut total = 0u64;
                let mut files = Vec::new();
                for file in &pending.upload.files {
                    require_bound(&original.metadata, &file.artifact_ref)?;
                    require_bound(&request.metadata, &file.artifact_ref)?;
                    exact_text(&file.filename)?;
                    require(file.filename.len() <= 255 && !matches!(file.filename.as_str(), "." | "..")
                        && !file.filename.contains('/') && !file.filename.contains('\\')
                        && file.payload_sha256.len() == 64 && file.payload_sha256.bytes().all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
                        "upload consent changed its exact file name or payload digest")?;
                    total = total.checked_add(file.size_bytes).ok_or("upload consent byte count overflow")?;
                    files.push(json!({"artifact_ref":file.artifact_ref,"filename":file.filename}));
                }
                require(total <= 268435456 && pending.original.arguments.get("files") == Some(&Value::Array(files))
                    && pending.original.arguments.get("reservation_id") == Some(&json!(pending.upload.reservation_id))
                    && pending.original.arguments.get("file_chooser_id") == Some(&json!(pending.upload.file_chooser_id)),
                    "upload consent changed original admitted files, reservation or byte quota")?;
                let commitment = json!({"schema_version":pending.schema_version,"original":pending.original,"scope":pending.scope,
                    "approval_scope":pending.approval_scope,"upload":pending.upload,"consent_offer":pending.consent_offer});
                (pending.schema_version, pending.request_id, pending.call_id, pending.original, pending.scope,
                    pending.resume, pending.consent_offer, commitment, "browser-upload-resume")
            }
            Some("browser_history_read") => {
                let pending: PendingHistoryRequest = serde_json::from_value(body.clone()).map_err(string_error)?;
                require_bound(&original.metadata, &pending.history.source)?;
                require_bound(&request.metadata, &pending.history.source)?;
                let carrier = reader.blob(&pending.history.source.content, 65536)?;
                let source_bytes = match serde_json::from_slice::<InterfaceArtifactManifest>(&carrier) {
                    Ok(manifest) if !manifest.payload_path.as_os_str().is_empty() => {
                        require(manifest.payload_path.is_absolute() && !manifest.label.trim().is_empty(),
                            "history source has a malformed admitted carrier")?;
                        let hash = exact_hash(manifest.payload_content_hash.as_deref().ok_or("history source carrier has no immutable payload pin")?)?;
                        reader.blob(&hash, 65536)?
                    }
                    _ => carrier,
                };
                require(format!("{:x}", Sha256::digest(&source_bytes)) == pending.history.source_payload_digest,
                    "history consent differs from its actual admitted source payload bytes")?;
                let ConsentEligibility::BrowserHistory { source, source_payload_digest, source_epoch, .. } = &pending.consent_offer.eligibility
                    else { return Err("history request has non-history eligibility".into()); };
                require(pending.consent_offer.subject == ConsentSubject::BrowserHistory { source: pending.history.source.clone() }
                    && source == &pending.history.source && source_payload_digest == &pending.history.source_payload_digest
                    && source_epoch == &pending.history.source_epoch,
                    "history consent changed its exact selected source, payload or native epoch")?;
                let mut arguments = pending.original.arguments.as_object().ok_or("history query is not an object")?.clone();
                let query = serde_json::to_value(&pending.history.query).map_err(string_error)?;
                require(arguments.remove("source") == Some(json!(pending.history.source))
                    && Value::Object(arguments) == query && body.get("history").and_then(|history| history.get("query")) == Some(&query)
                    && pending.history.query.queries.as_ref().is_none_or(|queries| !queries.is_empty())
                    && pending.history.query.limit != Some(0),
                    "history consent changed its original query or replaced an omitted option")?;
                require(pending.history.source_epoch.len() == 36
                    && pending.history.source_epoch.split('-').map(str::len).eq([8, 4, 4, 4, 12])
                    && pending.history.source_epoch.split('-').all(|part| part.bytes().all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)))
                    && pending.scope.host_generation.len() == 32
                    && pending.scope.host_generation.bytes().all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
                    "history consent has no exact native source epoch or host generation")?;
                let commitment = json!({"schema_version":pending.schema_version,"original":pending.original,"scope":pending.scope,
                    "approval_scope":pending.approval_scope,"history":pending.history,"consent_offer":pending.consent_offer});
                (pending.schema_version, pending.request_id, pending.call_id, pending.original, pending.scope,
                    pending.resume, pending.consent_offer, commitment, "browser-history-resume")
            }
            _ => return Err("consent pending request has no declared owner subject contract".into()),
        };
        offer.validate(&self.reference, &self.policy)?;
        if let ConsentEligibility::BrowserOrigin { policy, .. } | ConsentEligibility::BrowserHistory { policy, .. } = &offer.eligibility {
            require(*policy == self.binding.config.policy, "consent eligibility names another bound policy artifact")?;
        }
        let call: ToolCallArtifact = serde_json::from_slice(&original.payload).map_err(string_error)?;
        let digest = digest_json(&commitment)?;
        let Endpoint::Interface(owner) = &request.movement.to;
        require(version == 1 && request_id == format!("sha256:{digest}") && resume.correlation_id == format!("{correlation_prefix}:{digest}")
            && call_id == pending_original.correlation_id && resume.tool != pending_original.tool
            && pending_original.source_artifact == original_ref.artifact && pending_original.source_movement_seq == original_ref.movement_seq
            && pending_original.source_movement_seq > 0 && pending_original.source_movement_seq <= 9_007_199_254_740_991
            && pending_original.run == original.record.id && pending_original.ee == original.record.ee
            && pending_original.worker == original.record.worker && original.record.id == request.record.id
            && original.record.worker == request.record.worker
            && original.movement.from == Endpoint::Interface(owner.clone())
            && original.movement.to == Endpoint::Interface(pending_original.door_interface.clone())
            && request.movement.from == original.movement.to && request_ref.movement_seq > original_ref.movement_seq
            && original.manifest.label == "tool.call" && request.manifest.label == format!("{}.result", pending_original.tool)
            && result.label == request.manifest.label && result.ok && result.error.is_none()
            && result.body.as_ref().and_then(|body| body.get("effect_performed")) == Some(&Value::Bool(false))
            && result.tool == pending_original.tool && call.tool == pending_original.tool && call.arguments == pending_original.arguments
            && call.correlation_id.as_deref() == Some(&call_id) && result.correlation_id.as_deref() == Some(&call_id)
            && offer.session.run == request.record.id
            && request.metadata.grant.interfaces.contains(&offer.session.client_interface.to_string())
            && original.metadata.grant.tools.contains(&pending_original.tool)
            && original.metadata.grant.tools.contains(&resume.tool)
            && !original.metadata.grant.hard_blocked_tools.contains(&pending_original.tool)
            && !original.metadata.grant.hard_blocked_tools.contains(&resume.tool),
            "consent request changed its original operation, principal, subject, offer or digest")?;
        for text in [&scope.worker_environment_id, &scope.formation_pin, &scope.host_generation,
            &call_id, &pending_original.tool, &resume.tool] { exact_text(text)?; }
        Ok(ConsentRequestRecord { request_id, call_id, request: request_ref.clone(), original_call: original_ref.clone(),
            owner_interface: owner.clone(), offer, authorization: None, selected_return: None, selected_return_install: None })
    }

    fn successor_state(&self, intake: &VerifiedConsentIntake, command: &ConsentEvidenceRef) -> Result<ConsentState, String> {
        let mut state = self.state.clone();
        state.generation = state.generation.checked_add(1).ok_or("consent generation overflow")?;
        state.predecessor_run_hash = Some(self.head_hash());
        state.command = Some(command.clone());
        let receipt = |decision, origin| -> Result<ConsentDecisionRecord, String> {
            Ok(ConsentDecisionRecord { record_id: digest_json(&json!({"authority":self.reference,"command":command,"decision":decision,"origin":origin}))?,
                decision, command: command.clone(), origin })
        };
        match &intake.operation {
            VerifiedConsentOperation::Query(request) => {
                require(!state.requests.contains_key(&request.request_id), "consent request already has an ordered authority record")?;
                let mut request = request.clone();
                let key = session_key(&request.offer.session, &request.offer.subject)?;
                let session = state.sessions.get(&key).map(|row| &row.receipt);
                let always = state.always.get(&request.offer.subject.key()?).map(|row| &row.receipt);
                let native = matches!(request.offer.subject, ConsentSubject::NativeApplication { .. });
                let reusable = if native {
                    session.or_else(|| always.filter(|row| row.decision == ConsentDecision::Accept
                        && request.offer.allows(ConsentDecision::Accept, ConsentPersistence::Always)))
                } else {
                    session.filter(|row| row.decision == ConsentDecision::Decline)
                        .or_else(|| always.filter(|row| row.decision == ConsentDecision::Decline))
                        .or_else(|| if request.offer.policy_denied() { None } else {
                            session.filter(|row| row.decision == ConsentDecision::Accept)
                                .or_else(|| always.filter(|row| row.decision == ConsentDecision::Accept
                                    && request.offer.allows(ConsentDecision::Accept, ConsentPersistence::Always)))
                        })
                };
                let decision = if let Some(reusable) = reusable {
                    Some(receipt(reusable.decision, ConsentDecisionOrigin::Reused { receipt_id: reusable.record_id.clone() })?)
                } else if request.offer.policy_denied() {
                    Some(receipt(ConsentDecision::Decline, ConsentDecisionOrigin::Policy { request: request.request.clone() })?)
                } else { None };
                if native && !state.sessions.contains_key(&key) {
                    if let Some(decision) = &decision {
                        state.sessions.insert(key, ConsentSessionRecord { session: request.offer.session.clone(),
                            subject: request.offer.subject.clone(), receipt: decision.clone() });
                    }
                }
                request.authorization = decision;
                state.requests.insert(request.request_id.clone(), request);
            }
            VerifiedConsentOperation::Human { request_id, human, decision, persistence } => {
                let request = state.requests.get_mut(request_id).ok_or("human response lost its pending authority request")?;
                require(request.authorization.is_none() && request.offer.allows(*decision, *persistence),
                    "human response repeats a decision or chooses outside the immutable offer")?;
                let decision_record = receipt(*decision, ConsentDecisionOrigin::Human { evidence: human.clone(), persistence: *persistence })?;
                request.authorization = Some(decision_record.clone());
                if *persistence == ConsentPersistence::Session || matches!(request.offer.subject, ConsentSubject::NativeApplication { .. }) {
                    state.sessions.insert(session_key(&request.offer.session, &request.offer.subject)?, ConsentSessionRecord {
                        session: request.offer.session.clone(), subject: request.offer.subject.clone(), receipt: decision_record.clone() });
                }
                if *persistence == ConsentPersistence::Always {
                    state.always.insert(request.offer.subject.key()?, ConsentAlwaysRecord {
                        subject: request.offer.subject.clone(), eligibility_at_approval: request.offer.eligibility.clone(), receipt: decision_record });
                }
            }
            VerifiedConsentOperation::List => {
                let request_id = format!("application-consent-list:{}", digest_json(command)?);
                require(!state.management.contains_key(&request_id), "consent list command was already recorded")?;
                state.management.insert(request_id.clone(), ConsentManagementRecord { request_id, list_command: command.clone(),
                    displayed_receipts: state.always.iter().map(|(key, record)| (key.clone(), record.receipt.record_id.clone())).collect(), removal: None });
            }
            VerifiedConsentOperation::Remove { request_id, subject, receipt_id, human } => {
                let key = subject.key()?;
                let list = state.management.get_mut(request_id).ok_or("consent removal lost its management request")?;
                require(list.removal.is_none() && list.displayed_receipts.get(&key) == Some(receipt_id)
                    && state.always.get(&key).is_some_and(|r| r.subject == *subject && r.receipt.record_id == *receipt_id),
                    "consent removal is stale relative to the current checked authority head")?;
                state.always.remove(&key);
                list.removal = Some(human.clone());
            }
        }
        state.validate(&self.binding)?;
        Ok(state)
    }

}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedConsentWrite {
    pub command: ConsentEvidenceRef,
    pub intake: ConsentEvidenceRef,
    pub path: PathBuf,
    pub before_hash: String,
    pub after_hash: String,
    pub content: String,
    pub patch: json_patch::Patch,
    pub correlation_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsentWriteResult { result: String, writes: Vec<ConsentWriteEvidence> }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsentWriteEvidence { path: PathBuf, before_hash: Option<String>, after_hash: String, bytes: u64 }

impl PreparedConsentWrite {
    fn arguments(&self) -> Value {
        json!({"path":self.path,"patch":self.patch,"expected_before_hash":self.before_hash,"expected_after_hash":self.after_hash})
    }

    fn call_body(&self, writer: &ConsentWriter) -> Result<Value, String> {
        serde_json::to_value(ToolCallArtifact { tool: writer.tool.clone(), arguments: self.arguments(), correlation_id: Some(self.correlation_id.clone()),
            label: Some(writer.call_label.clone()), source_interface_evidence: BTreeMap::new() }).map_err(string_error)
    }

    fn materialize(&self, root: &Path, writer: &ConsentWriter) -> Result<InterfaceArtifactFile, String> {
        let call = ToolCallArtifact { tool: writer.tool.clone(), arguments: self.arguments(), correlation_id: Some(self.correlation_id.clone()),
            label: Some(writer.call_label.clone()), source_interface_evidence: BTreeMap::new() };
        InterfaceArtifactFile::write(root, &writer.call_label, &serde_json::to_vec(&call).map_err(string_error)?).map_err(string_error)
    }
}

impl ConsentAuthority {
    fn prepare_write(&self, reader: &mut ConsentEvidenceReader, command: &AdmittedConsentArtifact) -> Result<PreparedConsentWrite, String> {
        self.require_authority_edge(command, &self.head, &self.binding.config.home_interface,
            &self.binding.config.home_interface, 0)?;
        require(command.manifest.label == self.binding.config.command_label
            && self.last_result_seq.is_none_or(|seq| command.movement.seq.0 > seq),
            "authority command does not follow its last committed checked write")?;
        let body: ConsentCommand = serde_json::from_slice(&command.payload).map_err(string_error)?;
        require(body.authority == self.reference && body.before_hash == self.head_hash()
            && body.generation == self.state.generation && body.intake.movement_seq < command.movement.seq.0,
            "authority command is stale or claims another admission head")?;
        let intake = self.verify_intake(reader, &body.intake)?;
        let successor = self.successor_state(&intake, &command.evidence())?;
        let mut record = RunRecord::from_bytes(&self.head).map_err(string_error)?;
        record.interface_state.get_mut(&self.binding.config.home_interface).ok_or("authority owner state is absent")?
            .insert(OWNER_STATE.into(), serde_json::to_value(&successor).map_err(string_error)?);
        record.checkpoints = self.binding.config.checkpoints_for_state(&successor)?;
        record.active_checkpoints = record.checkpoints.iter().map(|c| c.id.clone()).collect();
        validate_record(&record, &self.binding, &successor)?;
        let before: Value = serde_json::from_slice(&self.head).map_err(string_error)?;
        let after = serde_json::to_value(&record).map_err(string_error)?;
        let patch = json_patch::diff(&before, &after);
        require(!patch.is_empty(), "authority write contains no checked transition")?;
        let mut installed = before;
        json_patch::patch(&mut installed, &patch).map_err(string_error)?;
        require(installed == after, "authority patch differs from its validated prospective record")?;
        let mut content = serde_json::to_string_pretty(&installed).map_err(string_error)?;
        content.push('\n');
        Ok(PreparedConsentWrite { command: command.evidence(), intake: intake.evidence, path: self.binding.config.writer.run_path.clone(),
            before_hash: self.head_hash(), after_hash: ContentHash::of_bytes(content.as_bytes()).to_hex(), content, patch,
            correlation_id: format!("application-consent-write:{}:{}", self.binding.config.authority_run, successor.generation) })
    }

    fn require_authority_edge(&self, admitted: &AdmittedConsentArtifact, expected: &[u8],
        from: &InterfaceId, to: &InterfaceId, checkpoint_index: usize) -> Result<(), String> {
        let record = RunRecord::from_bytes(expected).map_err(string_error)?;
        let state = state_from_record(&record, &self.binding)?;
        validate_record(&record, &self.binding, &state)?;
        let expected_checkpoint = record.checkpoints.get(checkpoint_index).ok_or("authority checkpoint index is absent")?;
        let runtime = admitted.metadata.runtime.as_ref().expect("admitted runtime");
        require(admitted.record_bytes == expected && admitted.movement.from == Endpoint::Interface(from.clone())
            && admitted.movement.to == Endpoint::Interface(to.clone()) && runtime.checkpoint == expected_checkpoint.id,
            "authority input borrowed another run, head, checkpoint or interface admission")?;
        require_bound(&admitted.metadata, &self.reference)?;
        require_bound(&admitted.metadata, &self.binding.config.policy)?;
        let grant = &admitted.metadata.grant;
        require(grant.interfaces.contains(&self.binding.config.home_interface.to_string())
            && grant.interfaces.contains(&self.binding.config.writer.door.to_string())
            && grant.tools.contains(&self.binding.config.writer.tool)
            && !grant.hard_blocked_tools.contains(&self.binding.config.writer.tool)
            && grant.worker_environment_kinds.iter().any(|kind| kind == "system_room")
            && grant.ee_root_slot_allowance.as_ref().is_none_or(|slots| slots.iter().any(|slot| slot == "run_records_root")),
            "authority's actual formed grant does not admit its own exact file writer")?;
        let mut writers = grant.interface_affordance_specs.iter().filter(|tool| tool.name.as_str() == self.binding.config.writer.tool);
        let writer = writers.next().ok_or("authority has no actual formed writer argument contract")?;
        require(writers.next().is_none(), "authority repeats its exact writer affordance")?;
        let properties = writer.args_schema.get("properties").and_then(Value::as_object).ok_or("authority writer has no object argument schema")?;
        let required = writer.args_schema.get("required").and_then(Value::as_array).ok_or("authority writer has no required checked arguments")?;
        require(writer.args_schema.get("type") == Some(&json!("object"))
            && writer.args_schema.get("additionalProperties") == Some(&json!(false))
            && properties.keys().map(String::as_str).collect::<BTreeSet<_>>() == BTreeSet::from(["path","patch","expected_before_hash","expected_after_hash"])
            && properties.get("path").and_then(|value| value.get("const")) == Some(&json!(self.binding.config.writer.run_path))
            && required.len() == 4 && required.iter().filter_map(Value::as_str).collect::<BTreeSet<_>>() == BTreeSet::from(["path","patch","expected_before_hash","expected_after_hash"]),
            "authority writer argument schema does not restrict its exact single RunRecord patch")
    }

    fn validate_write_result(&self, reader: &mut ConsentEvidenceReader, prepared: &PreparedConsentWrite,
        expected_call: &ArtifactRef, result: &AdmittedConsentArtifact) -> Result<ConsentPolicyAuthorization, String> {
        require(prepared.before_hash == self.head_hash(), "checked authority result belongs to another current head")?;
        let command = reader.admitted(&prepared.command)?;
        let reconstructed = self.prepare_write(reader, &command)?;
        require(&reconstructed == prepared, "authority preparation differs from its original admitted command")?;
        self.require_authority_edge(result, prepared.content.as_bytes(), &self.binding.config.writer.door,
            &self.binding.config.home_interface, 2)?;
        let call = self.exact_write_call(reader, prepared, Some(expected_call), result.movement.seq.0)?;
        self.require_authority_edge(&call, &self.head, &self.binding.config.home_interface, &self.binding.config.writer.door, 1)?;
        require(command.metadata.grant.formation_pin == call.metadata.grant.formation_pin
            && command.metadata.grant.formation_pin == result.metadata.grant.formation_pin,
            "consent authoring changed policy formation between command, call and result")?;
        let call_body: ToolCallArtifact = serde_json::from_slice(&call.payload).map_err(string_error)?;
        require(call.manifest.label == self.binding.config.writer.call_label && call_body.tool == self.binding.config.writer.tool
            && call_body.arguments == prepared.arguments() && call_body.correlation_id.as_deref() == Some(&prepared.correlation_id)
            && call_body.label.as_deref().is_none_or(|label| label == self.binding.config.writer.call_label)
            && call_body.source_interface_evidence.is_empty(), "admitted authority file.write differs from its immutable preparation")?;
        let result_body: ToolResultArtifact = serde_json::from_slice(&result.payload).map_err(string_error)?;
        require(result.manifest.label == self.binding.config.writer.result_label && result_body.label == self.binding.config.writer.result_label
            && result_body.tool == self.binding.config.writer.tool && result_body.ok && result_body.error.is_none()
            && result_body.correlation_id.as_deref() == Some(&prepared.correlation_id),
            "authority file.write failed or returned another correlation")?;
        let result_body: ConsentWriteResult = serde_json::from_value(result_body.body.ok_or("authority file.write returned no body")?).map_err(string_error)?;
        let [write] = result_body.writes.as_slice() else { return Err("authority file.write must prove exactly one written file".into()); };
        require(result_body.result == "files_written" && write.path == prepared.path
            && write.before_hash.as_deref() == Some(&prepared.before_hash) && write.after_hash == prepared.after_hash
            && write.bytes == prepared.content.len() as u64,
            "authority file.write result does not prove its exact installed prospective bytes")?;
        Ok(ConsentPolicyAuthorization { authority: self.reference.clone(), command: prepared.command.clone(), writer_call: call.evidence(),
            writer_result: result.evidence(), committed_run_hash: prepared.after_hash.clone(), record_id: digest_json(&prepared.command)? })
    }

    fn exact_write_call(&self, reader: &mut ConsentEvidenceReader, prepared: &PreparedConsentWrite,
        expected: Option<&ArtifactRef>, result_seq: u64) -> Result<AdmittedConsentArtifact, String> {
        let after_seq = prepared.command.movement_seq.checked_add(1).ok_or("authority call sequence overflow")?;
        require(after_seq < result_seq, "authority result precedes its command")?;
        let movements = reader.run_interval(&self.binding.config.authority_run, after_seq, result_seq, self.binding.bounds.ledger_batch_rows)?;
        let mut found = None;
        for movement in movements {
            if movement.kind != MovementKind::InterfaceOutbound { continue; }
            let runtime = movement.invocation_metadata.as_ref().and_then(|m| m.invocation()).and_then(|m| m.runtime.as_ref())
                .ok_or("authority write interval has an outbound without runtime coordinates")?;
            require(runtime.run == self.binding.config.authority_run && runtime.ee == self.binding.config.root_ee
                && runtime.worker == self.binding.config.authority_worker, "authority write interval contains another admitted principal")?;
            if movement.to == Endpoint::Interface(self.binding.config.writer.door.clone()) {
                require(movement.from == Endpoint::Interface(self.binding.config.home_interface.clone())
                    && expected.is_none_or(|expected| movement.artifact.as_ref() == Some(expected)) && found.is_none(),
                    "authority write interval contains another writer call or a duplicate")?;
                found = Some(ConsentEvidenceRef { artifact: movement.artifact.clone().ok_or("authority writer call has no artifact")?, movement_seq: movement.seq.0 });
            } else {
                let evidence = ConsentEvidenceRef { artifact: movement.artifact.clone().ok_or("passive authority input has no artifact")?,
                    movement_seq: movement.seq.0 };
                let admitted = reader.admitted(&evidence)?;
                require(admitted.record_bytes == self.head || admitted.record_bytes == prepared.content.as_bytes(),
                    "passive authority input changed an unrelated checkpoint or prospective snapshot")?;
                self.require_passive_management(&admitted)?;
            }
        }
        reader.admitted(&found.ok_or("authority result has no exact admitted file.write call")?)
    }

    fn commit(&mut self, prepared: &PreparedConsentWrite, proof: &ConsentPolicyAuthorization) -> Result<(), String> {
        require(proof.authority == self.reference && prepared.before_hash == self.head_hash()
            && proof.committed_run_hash == prepared.after_hash && proof.command == prepared.command,
            "authority commit differs from checked transition")?;
        let record = RunRecord::from_bytes(prepared.content.as_bytes()).map_err(string_error)?;
        let state = state_from_record(&record, &self.binding)?;
        require(state.generation == self.state.generation.checked_add(1).ok_or("authority generation overflow")?,
            "authority commit skipped a generation")?;
        self.checked_heads.insert(ContentHash::of_bytes(prepared.content.as_bytes()).to_hex());
        self.head = prepared.content.as_bytes().to_vec();
        self.state = state;
        self.last_result_seq = Some(proof.writer_result.movement_seq);
        Ok(())
    }

    pub fn replay_through(&mut self, reader: &mut ConsentEvidenceReader, terminal: &ConsentEvidenceRef) -> Result<(), String> {
        self.replay_through_checked(reader, terminal).map(|_| ())
    }

    fn replay_through_checked(&mut self, reader: &mut ConsentEvidenceReader, terminal: &ConsentEvidenceRef)
        -> Result<ConsentPolicyAuthorization, String> {
        let mut result = reader.admitted(terminal)?;
        let mut reverse = Vec::new();
        loop {
            let after_state = state_from_record(&result.record, &self.binding)?;
            require(after_state.generation > self.state.generation && reverse.len() < self.binding.bounds.max_records,
                "authority proof rewinds, repeats or exceeds the retained transition bound")?;
            validate_record(&result.record, &self.binding, &after_state)?;
            let command_ref = after_state.command.as_ref().ok_or("authority successor lost its admitted command")?;
            let command = reader.admitted(command_ref)?;
            let before_hash = ContentHash::of_bytes(&command.record_bytes).to_hex();
            require(after_state.predecessor_run_hash.as_deref() == Some(&before_hash)
                && command.movement.seq < result.movement.seq,
                "authority successor skips its command's actual original snapshot")?;
            let before_state = state_from_record(&command.record, &self.binding)?;
            require(before_state.generation.checked_add(1) == Some(after_state.generation), "authority proof skips a checked generation")?;
            let before = command.record_bytes.clone();
            reverse.push((command.evidence(), result.evidence()));
            if before == self.head { break; }
            require(before_state.generation > self.state.generation, "authority proof has a branch outside the retained checked prefix")?;
            let previous_command = before_state.command.as_ref().ok_or("authority proof lost its previous command")?;
            let predecessor = self.predecessor_result(reader, previous_command, &before_hash, command.movement.seq.0)?;
            result = reader.admitted(&predecessor)?;
        }
        let mut terminal_proof = None;
        for (command_ref, result_ref) in reverse.into_iter().rev() {
            let command = reader.admitted(&command_ref)?;
            let prepared = self.prepare_write(reader, &command)?;
            let result = reader.admitted(&result_ref)?;
            let call = self.exact_write_call(reader, &prepared, None, result.movement.seq.0)?;
            let proof = self.validate_write_result(reader, &prepared, &call.evidence().artifact, &result)?;
            self.commit(&prepared, &proof)?;
            terminal_proof = Some(proof);
        }
        terminal_proof.ok_or_else(|| "authority proof contains no checked transition".to_string())
    }

    fn checked_query_prefix(&self, reader: &mut ConsentEvidenceReader, reply: &AdmittedConsentArtifact)
        -> Result<(ConsentAuthority, ConsentPolicyAuthorization), String> {
        let payload: Value = serde_json::from_slice(&reply.payload).map_err(string_error)?;
        let proof: ConsentPolicyAuthorization = serde_json::from_value(payload.pointer("/application_consent_reply/policy_authorization")
            .cloned().ok_or("human-required reply has no checked query proof")?).map_err(string_error)?;
        require(proof.authority == self.reference && self.checked_heads.contains(&proof.committed_run_hash),
            "human-required query is outside the already validated authority prefix")?;
        let result = reader.admitted(&proof.writer_result)?;
        let command = reader.admitted(&proof.command)?;
        let before_hash = ContentHash::of_bytes(&command.record_bytes).to_hex();
        require(self.checked_heads.contains(&before_hash)
            && ContentHash::of_bytes(&result.record_bytes).to_hex() == proof.committed_run_hash,
            "human-required query changed its actual validated authority snapshots")?;
        let body: ConsentCommand = serde_json::from_slice(&command.payload).map_err(string_error)?;
        let intake = reader.admitted(&body.intake)?;
        require(matches!(serde_json::from_slice::<ConsentIntake>(&intake.payload).map_err(string_error)?, ConsentIntake::Query { .. }),
            "human-required reply did not follow an actual query transition")?;
        let state = state_from_record(&command.record, &self.binding)?;
        validate_record(&command.record, &self.binding, &state)?;
        let last_result_seq = match &state.command {
            Some(previous) => Some(self.predecessor_result(reader, previous, &before_hash, command.movement.seq.0)?.movement_seq),
            None if state.generation == 0 => None,
            None => return Err("human-required query lost its actual predecessor".into()),
        };
        let mut prefix = ConsentAuthority { reference: self.reference.clone(), binding: self.binding.clone(), policy: self.policy.clone(),
            genesis: self.genesis.clone(), head: command.record_bytes.clone(), state, last_result_seq,
            checked_heads: self.checked_heads.clone() };
        let prepared = prefix.prepare_write(reader, &command)?;
        let actual = prefix.validate_write_result(reader, &prepared, &proof.writer_call.artifact, &result)?;
        require(actual == proof, "human-required query changed its complete checked writer evidence")?;
        prefix.commit(&prepared, &actual)?;
        Ok((prefix, actual))
    }

    fn predecessor_result(&self, reader: &mut ConsentEvidenceReader, previous_command: &ConsentEvidenceRef,
        before_hash: &str, before_seq: u64) -> Result<ConsentEvidenceRef, String> {
        let movements = reader.run_interval(&self.binding.config.authority_run,
            previous_command.movement_seq.checked_add(1).ok_or("authority proof sequence overflow")?,
            before_seq, self.binding.bounds.ledger_batch_rows)?;
        let mut predecessor = None;
        for movement in movements {
            if movement.kind != MovementKind::InterfaceOutbound { continue; }
            if movement.from != Endpoint::Interface(self.binding.config.writer.door.clone())
                || movement.to != Endpoint::Interface(self.binding.config.home_interface.clone()) { continue; }
            let metadata = movement.invocation_metadata.as_ref().and_then(|m| m.invocation()).ok_or("authority result has no admitted metadata")?;
            let runtime = metadata.runtime.as_ref().ok_or("authority result has no scope")?;
            require(runtime.run == self.binding.config.authority_run && runtime.run_record_hash.as_deref() == Some(before_hash)
                && predecessor.is_none(), "authority predecessor result is ambiguous or has another installed snapshot")?;
            predecessor = Some(ConsentEvidenceRef { artifact: movement.artifact.ok_or("authority predecessor has no result artifact")?, movement_seq: movement.seq.0 });
        }
        predecessor.ok_or("authority proof has no committed predecessor; file equality is not evidence".into())
    }

    pub fn verify_human_required(&mut self, reader: &mut ConsentEvidenceReader,
        reply: &AdmittedConsentArtifact) -> Result<ConsentRequestRecord, String> {
        require(reply.movement.from == Endpoint::Interface(self.binding.config.home_interface.clone()),
            "human-required consent was not emitted by its exact Home owner")?;
        require_bound(&reply.metadata, &self.reference)?;
        require_bound(&reply.metadata, &self.binding.config.policy)?;
        let payload: Value = serde_json::from_slice(&reply.payload).map_err(string_error)?;
        let body = payload.get("application_consent_reply").ok_or("Home reply has no checked human-required body")?;
        let proof: ConsentPolicyAuthorization = serde_json::from_value(body.get("policy_authorization").cloned()
            .ok_or("Home human-required reply lost its actual checked authorization")?).map_err(string_error)?;
        require(proof.authority == self.reference && proof.writer_result.movement_seq < reply.movement.seq.0,
            "Home human-required reply precedes its checked authority write")?;
        let actual = self.replay_through_checked(reader, &proof.writer_result)?;
        require(actual == proof, "Home human-required reply changed its complete checked write proof")?;
        self.validate_human_required_reply(reader, reply, &proof)
    }

    fn validate_human_required_reply(&self, reader: &mut ConsentEvidenceReader, reply: &AdmittedConsentArtifact,
        proof: &ConsentPolicyAuthorization) -> Result<ConsentRequestRecord, String> {
        require(reply.movement.from == Endpoint::Interface(self.binding.config.home_interface.clone()),
            "human-required consent was not emitted by its exact Home owner")?;
        require_bound(&reply.metadata, &self.reference)?;
        require_bound(&reply.metadata, &self.binding.config.policy)?;
        let payload: Value = serde_json::from_slice(&reply.payload).map_err(string_error)?;
        let id = payload.pointer("/application_consent_reply/request_id").and_then(Value::as_str)
            .ok_or("Home human-required reply has no exact request id")?;
        require(proof.authority == self.reference && proof.writer_result.movement_seq < reply.movement.seq.0,
            "Home human-required reply precedes its checked authority write")?;
        let request = self.state.requests.get(id).cloned().ok_or("Home human-required reply lost its checked pending request")?;
        require(request.authorization.is_none() && reply.manifest.label == format!("application.consent.reply:{id}")
            && reply.movement.to == Endpoint::Interface(request.owner_interface.clone())
            && reply.metadata.outbound_interface_bindings == [request.owner_interface.clone()]
            && reply.metadata.required_verified_artifacts.is_empty()
            && payload == self.request_output(&self.state, id, &proof)?,
            "Home human-required reply does not match its actual checked pending state")?;
        let selected = request.selected_return.as_ref().ok_or("Home human-required reply has no selected return contract")?;
        let install = request.selected_return_install.as_ref().ok_or("Home human-required reply lost its checked return installation")?;
        let installed = reader.admitted(&install.result)?;
        octopus_run_authoring::checked_return_interval(reader, selected, &installed.record_bytes, &reply.record_bytes,
            install.result.movement_seq, reply.movement.seq.0,
            &[selected.human_required.clone(), selected.approved.clone(), selected.declined.clone()])?;
        octopus_run_authoring::validate_selected_return_reads(&reply.metadata, &selected.selected_artifacts)?;
        require(reply.metadata.runtime.as_ref().is_some_and(|runtime| runtime.checkpoint == selected.human_required),
            "Home human-required reply used another original return checkpoint")?;
        Ok(request)
    }

    pub fn verify_final_reply(&mut self, reader: &mut ConsentEvidenceReader,
        reply: &AdmittedConsentArtifact) -> Result<ConsentRequestRecord, String> {
        require(reply.movement.from == Endpoint::Interface(self.binding.config.home_interface.clone())
            && reply.manifest.label == "operator.approval.response", "final consent was not emitted by its actual Home owner")?;
        require_bound(&reply.metadata, &self.reference)?;
        require_bound(&reply.metadata, &self.binding.config.policy)?;
        let payload: Value = serde_json::from_slice(&reply.payload).map_err(string_error)?;
        let body = payload.get("operator_approval_response").and_then(Value::as_object).ok_or("Home final has no exact response body")?;
        require(body.len() == 4, "selected Home final has undeclared response fields")?;
        let id = body.get("request_id").and_then(Value::as_str).ok_or("Home final has no request id")?;
        let decision: ConsentDecision = serde_json::from_value(body.get("decision").cloned().ok_or("Home final has no decision")?).map_err(string_error)?;
        let proof: ConsentPolicyAuthorization = serde_json::from_value(body.get("policy_authorization").cloned().ok_or("Home final has no checked policy authorization")?).map_err(string_error)?;
        require(proof.writer_result.movement_seq < reply.movement.seq.0, "Home final precedes its actual checked authority commit")?;
        let request = self.verify_authorization(reader, &proof, id, decision)?;
        require(payload == self.request_output(&self.state, id, &proof)?
            && reply.movement.to == Endpoint::Interface(request.owner_interface.clone()),
            "Home final changed its checked request, original call or original owner")?;
        let selected = request.selected_return.as_ref().ok_or("Home final has no selected original return contract")?;
        let install = request.selected_return_install.as_ref().ok_or("Home final lost its actual return installation")?;
        let installed = reader.admitted(&install.result)?;
        octopus_run_authoring::checked_return_interval(reader, selected, &installed.record_bytes, &reply.record_bytes,
            install.result.movement_seq, reply.movement.seq.0,
            &[selected.human_required.clone(), selected.approved.clone(), selected.declined.clone()])?;
        octopus_run_authoring::validate_selected_return_reads(&reply.metadata, &selected.selected_artifacts)?;
        let pending = reader.admitted(&request.request)?;
        let result: ToolResultArtifact = serde_json::from_slice(&pending.payload).map_err(string_error)?;
        let pending: octopus_app_server_interface::OwnerOperatorRequest = serde_json::from_value(result.body.as_ref()
            .and_then(|body| body.get("operator_request")).cloned().ok_or("Home final lost its immutable pending operation")?).map_err(string_error)?;
        self.validate_selected_return_rows(&reply.record, selected, &request, &pending.original.door_interface)?;
        let expected = if decision == ConsentDecision::Accept { &selected.approved } else { &selected.declined };
        require(reply.metadata.runtime.as_ref().is_some_and(|runtime| &runtime.checkpoint == expected)
            && reply.metadata.outbound_interface_bindings == [request.owner_interface.clone(), pending.original.door_interface],
            "Home final did not cross its exact ordered owner/tool return pair")?;
        reader.admitted_requirement_mark(reply, proof.writer_result.movement_seq)?;
        Ok(request)
    }

    pub fn verify_authorization(&mut self, reader: &mut ConsentEvidenceReader, proof: &ConsentPolicyAuthorization,
        request_id: &str, decision: ConsentDecision) -> Result<ConsentRequestRecord, String> {
        require(proof.authority == self.reference, "application authorization names another owner")?;
        let command = reader.admitted(&proof.command)?;
        let state_before = state_from_record(&command.record, &self.binding)?;
        if state_before.generation < self.state.generation {
            return Err("authorization verification requires its preserved immutable prefix, not a mutable later head".into());
        }
        let mut actual = self.replay_through_checked(reader, &proof.writer_result)?;
        require(self.head_hash() == proof.committed_run_hash && self.state.command.as_ref() == Some(&proof.command),
            "application authorization does not name its actual committed authority transition")?;
        let request = self.state.requests.get(request_id).ok_or("application authorization has no exact pending request record")?;
        let record = request.authorization.as_ref().ok_or("application authorization is still awaiting a human decision")?;
        require(record.record_id == proof.record_id && record.decision == decision && record.command == proof.command,
            "application authorization changed its decision, record or command")?;
        actual.record_id = record.record_id.clone();
        require(&actual == proof, "application authorization changed its exact admitted writer evidence")?;
        Ok(request.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedConsentCommand {
    pub command: ConsentCommand,
    pub artifact: ArtifactRef,
    pub manifest_path: PathBuf,
}

pub enum ConsentOwnerEmission {
    Command { artifact: InterfaceArtifactFile, identity: ArtifactRef },
    Write { artifact: InterfaceArtifactFile, identity: ArtifactRef },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedConsentArtifact {
    artifact: ArtifactRef,
    manifest_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsentWriteVerdict {
    artifact: ArtifactRef,
    seq: u64,
}

struct ConsentWriteFlight {
    prepared: PreparedConsentWrite,
    artifact: ArtifactRef,
    verdict: Option<u64>,
    result: Option<ConsentEvidenceRef>,
    verdict_receiver_lost: bool,
}

enum ConsentFlight {
    PreparedCommand(PreparedConsentCommand),
    AwaitingWrite(ConsentWriteFlight),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommittedConsentTransition {
    pub proof: ConsentPolicyAuthorization,
    pub intake: ConsentEvidenceRef,
    pub output: Value,
}

pub struct RecoveredConsentController {
    pub controller: ConsentController,
    pub committed: Vec<CommittedConsentTransition>,
}

pub struct ConsentController {
    authority: ConsentAuthority,
    queued: VecDeque<ConsentEvidenceRef>,
    flight: Option<ConsentFlight>,
}

impl ConsentController {
    pub fn new(authority: ConsentAuthority) -> Self { Self { authority, queued: VecDeque::new(), flight: None } }
    pub fn authority(&self) -> &ConsentAuthority { &self.authority }
    pub fn has_writer_in_flight(&self) -> bool { self.flight.is_some() }


    pub fn recover(mut authority: ConsentAuthority, reader: &mut ConsentEvidenceReader,
        private_root: &Path) -> Result<RecoveredConsentController, String> {
        require(normal_path(private_root) && authority.state.generation == 0, "owner recovery requires its exact private root and independently verified genesis")?;
        let mut committed = Vec::new();
        let mut completed_intakes = BTreeSet::new();
        loop {
            let generation = authority.state.generation;
            let Some(saved) = read_private::<CommittedConsentTransition>(private_root, &format!("committed-{generation}"), reader.max_payload_bytes)? else { break; };
            require(committed.len() < authority.binding.bounds.max_records, "owner recovery exceeds its retained transition bound")?;
            let command = reader.admitted(&saved.proof.command)?;
            let prepared = authority.prepare_write(reader, &command)?;
            let result = reader.admitted(&saved.proof.writer_result)?;
            let proof = authority.validate_write_result(reader, &prepared, &saved.proof.writer_call.artifact, &result)?;
            require(proof == saved.proof && prepared.intake == saved.intake
                && authority.output_for_transition(reader, &prepared, &proof)? == saved.output
                && completed_intakes.insert(saved.intake.movement_seq),
                "saved owner commit changed actual writer proof, intake or deterministic output")?;
            authority.commit(&prepared, &proof)?;
            committed.push(saved);
        }
        let generation = authority.state.generation;
        let pending = read_private::<PreparedConsentCommand>(private_root, &format!("command-{generation}"), authority.binding.bounds.max_binding_bytes)?;
        let mut controller = Self::new(authority);
        if let Some(pending) = pending {
            require(pending.command.authority == controller.authority.reference
                && pending.command.generation == generation && pending.command.before_hash == controller.authority.head_hash()
                && !completed_intakes.contains(&pending.command.intake.movement_seq),
                "saved pending command changed its authority, head or original intake")?;
            controller.authority.verify_intake(reader, &pending.command.intake)?;
            restore_materialized(private_root, &PreparedConsentArtifact { artifact: pending.artifact.clone(), manifest_path: pending.manifest_path.clone() },
                &controller.authority.binding.config.command_label, &serde_json::to_value(&pending.command).map_err(string_error)?, reader)?;
            let write = read_private::<PreparedConsentWrite>(private_root, &format!("write-{generation}"), reader.max_payload_bytes)?;
            let artifact = read_private::<PreparedConsentArtifact>(private_root, &format!("write-artifact-{generation}"), controller.authority.binding.bounds.max_binding_bytes)?;
            match (write, artifact) {
                (Some(prepared), Some(artifact)) => {
                    let command = reader.admitted(&prepared.command)?;
                    require(prepared.command.artifact == pending.artifact
                        && controller.authority.prepare_write(reader, &command)? == prepared,
                        "saved checked write is not the exact admitted original command")?;
                    restore_materialized(private_root, &artifact, &controller.authority.binding.config.writer.call_label,
                        &prepared.call_body(&controller.authority.binding.config.writer)?, reader)?;
                    let verdict = read_private::<ConsentWriteVerdict>(private_root, &format!("write-verdict-{generation}"), controller.authority.binding.bounds.max_binding_bytes)?;
                    if let Some(verdict) = &verdict {
                        let movement = reader.movement(verdict.seq)?;
                        require(verdict.artifact == artifact.artifact && movement.artifact.as_ref() == Some(&artifact.artifact)
                            && !matches!(movement.kind, MovementKind::VerificationFailed | MovementKind::DeliveryRefused),
                            "saved write admission does not name its actual admitted artifact")?;
                    }
                    let result = read_private::<ConsentEvidenceRef>(private_root, &format!("write-result-{generation}"), controller.authority.binding.bounds.max_binding_bytes)?;
                    let mut admitted = verdict.map(|verdict| verdict.seq);
                    if let Some(result) = &result {
                        let result = reader.admitted(result)?;
                        let proof = controller.authority.validate_write_result(reader, &prepared, &artifact.artifact, &result)?;
                        if admitted.is_none() { admitted = Some(proof.writer_call.movement_seq); }
                    }
                    controller.flight = Some(ConsentFlight::AwaitingWrite(ConsentWriteFlight {
                        prepared, artifact: artifact.artifact, verdict: admitted, result, verdict_receiver_lost: true,
                    }));
                }
                (Some(prepared), None) => {
                    let command = reader.admitted(&prepared.command)?;
                    require(prepared.command.artifact == pending.artifact
                        && controller.authority.prepare_write(reader, &command)? == prepared,
                        "incomplete writer publication changed its actual checked preparation")?;
                    controller.flight = Some(ConsentFlight::PreparedCommand(pending));
                }
                (None, None) => controller.flight = Some(ConsentFlight::PreparedCommand(pending)),
                (None, Some(_)) => return Err("saved writer artifact lost its immutable checked preparation".into()),
            }
        }
        let pending_intake = match &controller.flight {
            Some(ConsentFlight::PreparedCommand(command)) => Some(command.command.intake.movement_seq),
            Some(ConsentFlight::AwaitingWrite(write)) => Some(write.prepared.intake.movement_seq),
            None => None,
        };
        let entries = match std::fs::read_dir(private_root) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let mut queued = BTreeMap::new();
        let maximum_files = controller.authority.binding.bounds.max_records.checked_add(controller.authority.binding.bounds.max_pending)
            .and_then(|count| count.checked_mul(16)).and_then(|count| count.checked_add(2)).ok_or("consent recovery file count overflow")?;
        for (index, entry) in entries.into_iter().flatten().enumerate() {
            require(index < maximum_files, "consent recovery exceeds its owner-private preparation bound")?;
            let entry = entry.map_err(string_error)?;
            let name = entry.file_name();
            if let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".json")) {
                for prefix in ["committed-", "command-", "write-artifact-", "write-verdict-", "write-result-", "write-"] {
                    let Some(value) = name.strip_prefix(prefix) else { continue; };
                    let saved_generation: u64 = value.parse().map_err(string_error)?;
                    require(saved_generation.to_string() == value
                        && if prefix == "committed-" { saved_generation < generation }
                            else { saved_generation <= generation && (saved_generation < generation || controller.flight.is_some()) },
                        "consent recovery found a skipped generation or orphaned pending writer")?;
                    break;
                }
            }
            let Some(name) = name.to_str().and_then(|name| name.strip_prefix("intake-")).and_then(|name| name.strip_suffix(".json")) else { continue; };
            let seq: u64 = name.parse().map_err(string_error)?;
            require(seq.to_string() == name, "saved intake filename is not an exact movement sequence")?;
            if completed_intakes.contains(&seq) || pending_intake == Some(seq) { continue; }
            require(queued.len() < controller.authority.binding.bounds.max_pending, "recovered consent queue exceeds its declared bound")?;
            let intake = read_private::<ConsentEvidenceRef>(private_root, &format!("intake-{seq}"), controller.authority.binding.bounds.max_binding_bytes)?
                .ok_or("saved intake disappeared during owner recovery")?;
            require(intake.movement_seq == seq && !queued.contains_key(&seq), "saved intake changed its exact sequence identity")?;
            controller.validate_queued_intake(reader, &intake)?;
            queued.insert(seq, intake);
        }
        controller.queued = queued.into_values().collect();
        Ok(RecoveredConsentController { controller, committed })
    }

    pub fn finish_recovered_write(&mut self, reader: &mut ConsentEvidenceReader,
        private_root: &Path) -> Result<Option<CommittedConsentTransition>, String> {
        self.join_write(reader, private_root)
    }

    pub fn queue(&mut self, reader: &mut ConsentEvidenceReader, intake: ConsentEvidenceRef, private_root: &Path) -> Result<(), String> {
        self.validate_queued_intake(reader, &intake)?;
        let pending = match &self.flight {
            Some(ConsentFlight::PreparedCommand(command)) => Some(&command.command.intake),
            Some(ConsentFlight::AwaitingWrite(write)) => Some(&write.prepared.intake),
            None => None,
        };
        require(self.queued.len() < self.authority.binding.bounds.max_pending && !self.queued.contains(&intake) && pending != Some(&intake),
            "consent intake queue is full or repeats an exact admitted artifact")?;
        write_private_once(private_root, &format!("intake-{}", intake.movement_seq), &intake, self.authority.binding.bounds.max_binding_bytes)?;
        self.queued.push_back(intake);
        Ok(())
    }

    fn validate_queued_intake(&self, reader: &mut ConsentEvidenceReader, evidence: &ConsentEvidenceRef) -> Result<(), String> {
        let admitted = reader.admitted(evidence)?;
        require(admitted.movement.to == Endpoint::Interface(self.authority.binding.config.home_interface.clone())
            && admitted.manifest.label == "application.consent.intake", "queued consent is not an admitted owner intake")?;
        require_bound(&admitted.metadata, &self.authority.reference)?;
        require_bound(&admitted.metadata, &self.authority.binding.config.policy)?;
        let intake: ConsentIntake = serde_json::from_slice(&admitted.payload).map_err(string_error)?;
        require(intake.authority() == &self.authority.reference, "queued consent changed authority")
    }

    pub fn prepare_next(&mut self, reader: &mut ConsentEvidenceReader, private_root: &Path) -> Result<Option<ConsentOwnerEmission>, String> {
        if self.flight.is_some() { return Ok(None); }
        let Some(intake) = self.queued.front().cloned() else { return Ok(None); };
        self.authority.verify_intake(reader, &intake)?;
        let command = ConsentCommand { authority: self.authority.reference.clone(), intake,
            before_hash: self.authority.head_hash(), generation: self.authority.state.generation };
        let name = format!("command-{}", command.generation);
        let (prepared, artifact) = if let Some(prepared) = read_private::<PreparedConsentCommand>(private_root, &name, self.authority.binding.bounds.max_binding_bytes)? {
            require(prepared.command == command, "recovered command belongs to another intake or authority head")?;
            let artifact = restore_materialized(private_root, &PreparedConsentArtifact {
                artifact: prepared.artifact.clone(), manifest_path: prepared.manifest_path.clone(),
            }, &self.authority.binding.config.command_label, &serde_json::to_value(&command).map_err(string_error)?, reader)?;
            (prepared, artifact)
        } else {
            let artifact = InterfaceArtifactFile::write(private_root.join("artifacts"), &self.authority.binding.config.command_label,
                &serde_json::to_vec(&command).map_err(string_error)?).map_err(string_error)?;
            let prepared = PreparedConsentCommand { command, artifact: materialized_ref(&artifact, reader)?, manifest_path: artifact.manifest_path().to_path_buf() };
            write_private_once(private_root, &name, &prepared, self.authority.binding.bounds.max_binding_bytes)?;
            (prepared, artifact)
        };
        let reference = prepared.artifact.clone();
        self.flight = Some(ConsentFlight::PreparedCommand(prepared));
        self.queued.pop_front();
        Ok(Some(ConsentOwnerEmission::Command { artifact, identity: reference }))
    }

    pub fn admitted_command(&mut self, reader: &mut ConsentEvidenceReader, evidence: ConsentEvidenceRef,
        private_root: &Path) -> Result<ConsentOwnerEmission, String> {
        let Some(ConsentFlight::PreparedCommand(expected)) = &self.flight else { return Err("authority command arrived without its preserved preparation".into()); };
        require(evidence.artifact == expected.artifact, "authority command changed its prepared immutable artifact")?;
        let admitted = reader.admitted(&evidence)?;
        let body: ConsentCommand = serde_json::from_slice(&admitted.payload).map_err(string_error)?;
        require(body == expected.command, "admitted authority command changed its prepared body")?;
        let prepared = self.authority.prepare_write(reader, &admitted)?;
        let generation = self.authority.state.generation;
        write_private_once(private_root, &format!("write-{generation}"), &prepared, reader.max_payload_bytes)?;
        let artifact_name = format!("write-artifact-{generation}");
        let artifact = if let Some(saved) = read_private::<PreparedConsentArtifact>(private_root, &artifact_name, self.authority.binding.bounds.max_binding_bytes)? {
            restore_materialized(private_root, &saved, &self.authority.binding.config.writer.call_label,
                &prepared.call_body(&self.authority.binding.config.writer)?, reader)?
        } else {
            let artifact = prepared.materialize(&private_root.join("artifacts"), &self.authority.binding.config.writer)?;
            write_private_once(private_root, &artifact_name, &PreparedConsentArtifact {
                artifact: materialized_ref(&artifact, reader)?, manifest_path: artifact.manifest_path().to_path_buf(),
            }, self.authority.binding.bounds.max_binding_bytes)?;
            artifact
        };
        let reference = materialized_ref(&artifact, reader)?;
        self.flight = Some(ConsentFlight::AwaitingWrite(ConsentWriteFlight { prepared, artifact: reference.clone(), verdict: None, result: None, verdict_receiver_lost: false }));
        Ok(ConsentOwnerEmission::Write { artifact, identity: reference })
    }

    pub fn write_admission_verdict(&mut self, reader: &mut ConsentEvidenceReader, artifact: &ArtifactRef,
        admitted_seq: u64, private_root: &Path) -> Result<Option<CommittedConsentTransition>, String> {
        let Some(ConsentFlight::AwaitingWrite(flight)) = &mut self.flight else { return Err("authority write verdict has no active immutable write".into()); };
        require(&flight.artifact == artifact && flight.verdict.is_none(), "authority write verdict repeats or names another artifact")?;
        write_private_once(private_root, &format!("write-verdict-{}", self.authority.state.generation),
            &ConsentWriteVerdict { artifact: artifact.clone(), seq: admitted_seq }, self.authority.binding.bounds.max_binding_bytes)?;
        flight.verdict = Some(admitted_seq);
        self.join_write(reader, private_root)
    }

    pub fn admitted_write_result(&mut self, reader: &mut ConsentEvidenceReader, evidence: ConsentEvidenceRef,
        private_root: &Path) -> Result<Option<CommittedConsentTransition>, String> {
        let Some(ConsentFlight::AwaitingWrite(flight)) = &mut self.flight else { return Err("authority result has no pending checked write".into()); };
        let result = reader.admitted(&evidence)?;
        let proof = self.authority.validate_write_result(reader, &flight.prepared, &flight.artifact, &result)?;
        require(flight.result.is_none(), "authority write received a duplicate admitted result")?;
        write_private_once(private_root, &format!("write-result-{}", self.authority.state.generation),
            &evidence, self.authority.binding.bounds.max_binding_bytes)?;
        flight.result = Some(evidence);
        if flight.verdict_receiver_lost && flight.verdict.is_none() {
            flight.verdict = Some(proof.writer_call.movement_seq);
        }
        self.join_write(reader, private_root)
    }

    fn join_write(&mut self, reader: &mut ConsentEvidenceReader, private_root: &Path) -> Result<Option<CommittedConsentTransition>, String> {
        let Some(ConsentFlight::AwaitingWrite(flight)) = &self.flight else { return Ok(None); };
        let (Some(verdict), Some(result)) = (flight.verdict, &flight.result) else { return Ok(None); };
        let verdict_row = reader.movement(verdict)?;
        require(verdict_row.artifact.as_ref() == Some(&flight.artifact)
            && !matches!(verdict_row.kind, MovementKind::VerificationFailed | MovementKind::DeliveryRefused)
            && verdict <= result.movement_seq, "authority write verdict does not reference this admitted artifact")?;
        let admitted_result = reader.admitted(result)?;
        let proof = self.authority.validate_write_result(reader, &flight.prepared, &flight.artifact, &admitted_result)?;
        let intake = flight.prepared.intake.clone();
        let prepared = flight.prepared.clone();
        let output = self.authority.output_for_transition(reader, &prepared, &proof)?;
        let committed = CommittedConsentTransition { proof, intake, output };
        write_private_once(private_root, &format!("committed-{}", self.authority.state.generation),
            &json!({"proof":committed.proof,"intake":committed.intake,"output":committed.output}), reader.max_payload_bytes)?;
        self.authority.commit(&prepared, &committed.proof)?;
        self.flight = None;
        Ok(Some(committed))
    }
}

impl ConsentAuthority {
    fn output_for_transition(&self, reader: &mut ConsentEvidenceReader, prepared: &PreparedConsentWrite,
        proof: &ConsentPolicyAuthorization) -> Result<Value, String> {
        let intake = self.verify_intake(reader, &prepared.intake)?;
        let record = RunRecord::from_bytes(prepared.content.as_bytes()).map_err(string_error)?;
        let state = state_from_record(&record, &self.binding)?;
        match intake.operation {
            VerifiedConsentOperation::Query(request) => self.request_output(&state, &request.request_id, proof),
            VerifiedConsentOperation::Human { request_id, .. } => self.request_output(&state, &request_id, proof),
            VerifiedConsentOperation::List => {
                let request_id = format!("application-consent-list:{}", digest_json(&prepared.command)?);
                let entries = state.always.values().map(|record| json!({"subject":record.subject,
                    "receipt_id":record.receipt.record_id,"decision":record.receipt.decision,
                    "eligibility_at_approval":record.eligibility_at_approval})).collect::<Vec<_>>();
                Ok(json!({"operation":"list_always","request_id":request_id,"entries":entries,"policy_authorization":proof}))
            }
            VerifiedConsentOperation::Remove { request_id, subject, receipt_id, .. } => {
                Ok(json!({"operation":"remove_always","request_id":request_id,"subject":subject,
                    "removed_receipt_id":receipt_id,"policy_authorization":proof}))
            }
        }
    }

    fn request_output(&self, state: &ConsentState, request_id: &str, proof: &ConsentPolicyAuthorization) -> Result<Value, String> {
        let request = state.requests.get(request_id).ok_or("committed consent output lost its exact request")?;
        if let Some(decision) = &request.authorization {
            let mut authorization = proof.clone();
            authorization.record_id = decision.record_id.clone();
            Ok(json!({"operator_approval_response":{"request_id":request.request_id,"call_id":request.call_id,
                "decision":decision.decision,"policy_authorization":authorization}}))
        } else {
            Ok(json!({"application_consent_reply":{"request_id":request.request_id,"call_id":request.call_id,
                "result":"human_required","request":request.request,"consent_offer":request.offer,
                "policy_authorization":proof}}))
        }
    }
}

fn materialized_ref(artifact: &InterfaceArtifactFile, reader: &ConsentEvidenceReader) -> Result<ArtifactRef, String> {
    let captured = artifact.read_bounded(reader.max_manifest_bytes, reader.max_payload_bytes).map_err(string_error)?;
    let hash = ContentHash::of_bytes(&captured.manifest_bytes);
    Ok(ArtifactRef { id: ArtifactId::new(format!("{}:{}", captured.manifest.label, hash.to_hex())), content: hash })
}


fn read_private<T: serde::de::DeserializeOwned>(root: &Path, name: &str, limit: u64) -> Result<Option<T>, String> {
    require(normal_path(root) && !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "consent recovery path is not exact owner-private state")?;
    let path = root.join(format!("{name}.json"));
    match std::fs::metadata(&path) {
        Ok(metadata) => require(metadata.is_file(), "consent recovery preparation is not a regular file")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&path, limit).map_err(string_error)?;
    serde_json::from_slice(&bytes).map(Some).map_err(string_error)
}

fn restore_materialized(root: &Path, saved: &PreparedConsentArtifact, label: &str,
    body: &Value, reader: &ConsentEvidenceReader) -> Result<InterfaceArtifactFile, String> {
    require(normal_path(&saved.manifest_path) && saved.manifest_path.starts_with(root.join("artifacts")),
        "saved consent artifact left its original owner-private artifact root")?;
    let file = InterfaceArtifactFile::new(&saved.manifest_path).map_err(string_error)?;
    let captured = file.read_bounded(reader.max_manifest_bytes, reader.max_payload_bytes).map_err(string_error)?;
    require(captured.manifest.label == label && materialized_ref(&file, reader)? == saved.artifact
        && serde_json::from_slice::<Value>(&captured.payload_bytes).map_err(string_error)? == *body,
        "saved consent artifact changed its original identity, label or body")?;
    Ok(file)
}

fn write_private_once(root: &Path, name: &str, value: &impl Serialize, limit: u64) -> Result<(), String> {
    require(root.is_absolute() && !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'),
        "consent preparation path is not owner-private and exact")?;
    let bytes = serde_json::to_vec(value).map_err(string_error)?;
    require(bytes.len() as u64 <= limit, "consent private preparation exceeds its carrier bound")?;
    std::fs::create_dir_all(root).map_err(string_error)?;
    let path = root.join(format!("{name}.json"));
    if path.exists() {
        let existing = octopus_interfaces::read_artifact_carrier_file_bounded(&path, limit).map_err(string_error)?;
        return require(existing == bytes, "consent private immutable preparation already has different bytes");
    }
    let staging = root.join(format!("{name}.preparing"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file = options.open(&staging).map_err(string_error)?;
    file.write_all(&bytes).map_err(string_error)?;
    file.sync_all().map_err(string_error)?;
    std::fs::hard_link(&staging, &path).map_err(string_error)?;
    std::fs::remove_file(&staging).map_err(string_error)?;
    std::fs::File::open(root).and_then(|f| f.sync_all()).map_err(string_error)
}

fn session_key(session: &ConsentSession, subject: &ConsentSubject) -> Result<String, String> {
    subject.validate()?;
    digest_json(&json!({"session":session,"subject":subject}))
}

fn require_bound(metadata: &InvocationMetadata, reference: &ArtifactRef) -> Result<(), String> {
    require(metadata.artifact_binding_refs.iter().filter(|r| r.id == reference.id).eq([reference])
        && metadata.grant.artifact_access.get(reference.id.as_str()).is_some_and(|access| access.read),
        "consent authority/policy was not bound readably by exact ArtifactRef")
}

fn state_from_record(record: &RunRecord, binding: &ConsentAuthorityBinding) -> Result<ConsentState, String> {
    let row = record.interface_state.get(&binding.config.home_interface).ok_or("authority RunRecord has no exact owner state")?;
    let state: ConsentState = serde_json::from_value(row.get(OWNER_STATE).ok_or("authority RunRecord has no consent state")?.clone()).map_err(string_error)?;
    state.validate(binding)?;
    Ok(state)
}

fn validate_record(record: &RunRecord, binding: &ConsentAuthorityBinding, state: &ConsentState) -> Result<(), String> {
    let config = &binding.config;
    let checkpoints = config.checkpoints_for_state(state)?;
    require(record.id == config.authority_run && record.ee == config.root_ee && record.worker == config.authority_worker
        && record.checkpoints == checkpoints && record.active_checkpoints == checkpoints.iter().map(|c| c.id.clone()).collect::<Vec<_>>()
        && record.trace == config.checkpoints.command.trace
        && config.checkpoints.call.trace == record.trace && config.checkpoints.result.trace == record.trace
        && config.checkpoints.management.trace == record.trace && config.checkpoints.publication.trace == record.trace,
        "authority RunRecord changed owner identity, active checkpoints or exact templates")
}

fn exact_name_binding(binding: &octopus_policy::AuthoredInterfaceBinding, name: &str) -> bool {
    matches!(&binding.config.artifact_policy, octopus_policy::BindingArtifactPolicy::Allow { names, namespaces }
        if names == &[name] && namespaces.is_empty())
}

fn exact_hash(value: &str) -> Result<ContentHash, String> {
    ContentHash::from_hex(value).filter(|hash| hash.to_hex() == value).ok_or_else(|| "consent digest is not canonical lowercase hex".into())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsentPublication {
    prepared: PreparedConsentArtifact,
    origin: ConsentEvidenceRef,
    run: RunId,
    target: InterfaceId,
    label: String,
    run_hash: String,
    checkpoint: CheckpointId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    paired_target: Option<InterfaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_return: Option<SelectedConsentReturn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    return_install: Option<CheckedRunInstall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

struct ConsentPendingPublication {
    publication: ConsentPublication,
    cold: bool,
    verdict: Option<u64>,
}

#[derive(Clone)]
enum ConsentLiveEmission {
    Command(ArtifactRef),
    Write(ArtifactRef),
    Publication(ArtifactRef),
}

type ConsentAdmissionFuture = futures::future::BoxFuture<'static, (ConsentLiveEmission, Result<octopus_interfaces::DeliveryVerdict, String>)>;

pub async fn run_application_consent_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let selected = descriptor.interface_driver_config.as_ref()
        .and_then(|config| config.private_config.get("application_consent_authority"))
        .map(|value| serde_json::from_value::<ArtifactRef>(value.clone()).map_err(string_error)).transpose()?;
    let Some(reference) = selected else {
        return octopus_world_pyecho_life_runtime::run_descriptor_activation_life(ctx, crate::TRANSPORT_REF, crate::activation_start).await;
    };
    let observer = observe_application_consent(ctx.clone(), reference);
    let activation = octopus_world_pyecho_life_runtime::run_descriptor_activation_life(ctx.clone(), crate::TRANSPORT_REF, crate::activation_start);
    tokio::pin!(observer, activation);
    tokio::select! {
        result = &mut observer => {
            ctx.cancel.cancel();
            let stopped = activation.await;
            result.and(stopped)
        }
        result = &mut activation => {
            ctx.cancel.cancel();
            let stopped = observer.await;
            result.and(stopped)
        }
    }
}

struct ConsentLiveOwner {
    controller: ConsentController,
    publications: VecDeque<ConsentPendingPublication>,
    completed: BTreeMap<u64, CommittedConsentTransition>,
    removals: BTreeMap<u64, ConsentEvidenceRef>,
    cold_flight: bool,
}

fn consent_reader(ctx: &InterfaceLiftContext, controller: Option<&ConsentController>) -> Result<ConsentEvidenceReader, String> {
    let bounds = controller.map(|c| c.authority.binding.bounds.clone()).unwrap_or(ConsentBounds {
        max_policy_bytes: 65536, max_binding_bytes: 65536, max_state_bytes: 1048576,
        max_records: 4096, max_pending: 256, ledger_batch_rows: 256, max_proof_bytes: 16777216,
    });
    ConsentEvidenceReader::new(ctx, &bounds)
}

fn consent_start_admission(ctx: &InterfaceLiftContext, root: &Path, reader: &ConsentEvidenceReader,
    artifact: InterfaceArtifactFile, emission: ConsentLiveEmission,
    admissions: &mut futures::stream::FuturesUnordered<ConsentAdmissionFuture>) -> Result<(), String> {
    use futures::FutureExt;
    let identity = match &emission { ConsentLiveEmission::Command(id) | ConsentLiveEmission::Write(id)
        | ConsentLiveEmission::Publication(id) => id };
    require(materialized_ref(&artifact, reader)? == *identity, "consent publication changed its prepared artifact")?;
    let key = format!("publication-start-{}", identity.content.to_hex());
    require(read_private::<ArtifactRef>(root, &key, reader.max_manifest_bytes)?.is_none(),
        "consent publication is uncertain: original admitted evidence is required before continuation; it will not be handed in again")?;
    write_private_once(root, &key, identity, reader.max_manifest_bytes)?;
    let ctx = ctx.clone();
    admissions.push(async move { (emission, ctx.hand_arrival(artifact, Vec::new()).await) }.boxed());
    Ok(())
}

fn consent_start_owner_emission(ctx: &InterfaceLiftContext, root: &Path, reader: &ConsentEvidenceReader,
    emission: ConsentOwnerEmission, admissions: &mut futures::stream::FuturesUnordered<ConsentAdmissionFuture>) -> Result<(), String> {
    match emission {
        ConsentOwnerEmission::Command { artifact, identity } => consent_start_admission(ctx, root, reader, artifact, ConsentLiveEmission::Command(identity), admissions),
        ConsentOwnerEmission::Write { artifact, identity } => consent_start_admission(ctx, root, reader, artifact, ConsentLiveEmission::Write(identity), admissions),
    }
}

impl ConsentLiveOwner {
    fn recover(ctx: &InterfaceLiftContext, root: &Path, reference: &ArtifactRef,
        anchor: &ConsentEvidenceRef) -> Result<Self, String> {
        let mut reader = consent_reader(ctx, None)?;
        let admitted = reader.admitted(anchor)?;
        require(admitted.movement.to == Endpoint::Interface(ctx.interface.clone()), "consent life borrowed another owner's read anchor")?;
        let authority = ConsentAuthority::load(&mut reader, reference, &admitted)?;
        require(authority.binding.config.home_interface == ctx.interface, "selected consent authority names another Home interface")?;
        let recovered = ConsentController::recover(authority, &mut reader, root)?;
        let cold_flight = recovered.controller.flight.is_some();
        let mut owner = Self { controller: recovered.controller, publications: VecDeque::new(), completed: BTreeMap::new(), removals: BTreeMap::new(), cold_flight };
        for committed in recovered.committed {
            owner.remember_commit(ctx, root, committed, true)?;
        }
        let mut count = 0usize;
        for entry in std::fs::read_dir(root).map_err(string_error)? {
            let entry = entry.map_err(string_error)?;
            let name = entry.file_name();
            if name.to_str().is_some_and(|n| n.starts_with("removal-intake-")) { continue; }
            let Some(raw) = name.to_str().and_then(|n| n.strip_prefix("removal-")).and_then(|n| n.strip_suffix(".json")) else { continue; };
            count += 1;
            require(count <= owner.controller.authority.binding.bounds.max_records, "consent retained removal evidence exceeds its bound")?;
            let seq: u64 = raw.parse().map_err(string_error)?;
            require(raw == seq.to_string(), "consent removal filename changed its exact sequence")?;
            let evidence: ConsentEvidenceRef = read_private(root, &format!("removal-{seq}"), reader.max_manifest_bytes)?.ok_or("consent removal disappeared")?;
            require(evidence.movement_seq == seq, "consent removal changed its actual sequence")?;
            owner.removals.insert(seq, evidence);
        }
        Ok(owner)
    }

    fn remember_commit(&mut self, ctx: &InterfaceLiftContext, root: &Path,
        committed: CommittedConsentTransition, cold: bool) -> Result<(), String> {
        let mut reader = consent_reader(ctx, Some(&self.controller))?;
        let intake = reader.admitted(&committed.intake)?;
        let config = &self.controller.authority.binding.config;
        let operation: ConsentIntake = serde_json::from_slice(&intake.payload).map_err(string_error)?;
        let mut selected_return = None;
        let mut return_install = None;
        let mut request_id = None;
        let mut paired_target = None;
        let mut selected_checkpoint = None;
        let (record, run_hash, label, target) = match operation {
            ConsentIntake::ListAlways { .. } | ConsentIntake::RemoveAlways { .. } => {
                let result = reader.admitted(&committed.proof.writer_result)?;
                (result.record, ContentHash::of_bytes(&result.record_bytes).to_hex(),
                    "application.consent.management.result".to_string(), config.home_interface.clone())
            }
            ConsentIntake::Query { .. } | ConsentIntake::RecordHuman { .. } => {
                let response = committed.output.get("operator_approval_response");
                let body = response.or_else(|| committed.output.get("application_consent_reply"))
                    .ok_or("consent committed output lost its exact reply")?;
                let id = body.get("request_id").and_then(Value::as_str).ok_or("consent committed output lost its exact request id")?;
                let request = self.controller.authority.state.requests.get(id).ok_or("consent publication lost its checked request")?;
                let Endpoint::Interface(target) = &intake.movement.from;
                require(target == &request.owner_interface, "consent publication changed its original return owner")?;
                let mut label = format!("application.consent.reply:{id}");
                if let Some(selected) = &request.selected_return {
                    let pending = reader.admitted(&request.request)?;
                    let result: ToolResultArtifact = serde_json::from_slice(&pending.payload).map_err(string_error)?;
                    let owner_request: octopus_app_server_interface::OwnerOperatorRequest = serde_json::from_value(
                        result.body.and_then(|body| body.get("operator_request").cloned()).ok_or("selected publication lost its pending request")?).map_err(string_error)?;
                    self.controller.authority.validate_selected_return_rows(&intake.record, selected, request, &owner_request.original.door_interface)?;
                    selected_checkpoint = Some(if let Some(response) = response {
                        label = "operator.approval.response".into();
                        paired_target = Some(owner_request.original.door_interface);
                        match serde_json::from_value::<ConsentDecision>(response.get("decision").cloned().ok_or("consent reply lost its actual checked decision")?).map_err(string_error)? {
                            ConsentDecision::Accept => selected.approved.clone(),
                            ConsentDecision::Decline => selected.declined.clone(),
                        }
                    } else { selected.human_required.clone() });
                    return_install = Some(request.selected_return_install.clone().ok_or("selected publication lost its checked return installation")?);
                    selected_return = Some(selected.clone());
                    request_id = Some(id.to_string());
                }
                (intake.record, ContentHash::of_bytes(&intake.record_bytes).to_hex(), label, target.clone())
            }
        };
        let targets = std::iter::once(&target).chain(paired_target.as_ref()).collect::<Vec<_>>();
        let mut outputs = record.checkpoints.iter().filter(|checkpoint| record.active_checkpoints.contains(&checkpoint.id)
            && selected_checkpoint.as_ref().is_none_or(|id| id == &checkpoint.id)
            && checkpoint.inbound_interface() == Some(&config.home_interface)
            && checkpoint.outbound_interfaces().collect::<Vec<_>>() == targets
            && checkpoint.interface_binding.as_ref().is_some_and(|binding| exact_name_binding(binding, &label))
            && checkpoint.outbound_interface_bindings.iter().all(|binding| exact_name_binding(binding, &label)));
        let checkpoint = outputs.next().ok_or("consent reply has no exact independently authored return checkpoint")?;
        require(outputs.next().is_none() && (paired_target.is_some() || checkpoint.required_verified_artifacts.is_empty())
            && checkpoint.artifact_bindings.iter().any(|binding| binding.id == self.controller.authority.reference.id && binding.access.read),
            "consent return checkpoint is ambiguous or drops its authority binding")?;
        let name = format!("reply-{}", committed.proof.writer_result.movement_seq);
        let saved = read_private::<ConsentPublication>(root, &name, reader.max_manifest_bytes)?;
        let prepared = if let Some(saved) = &saved { saved.prepared.clone() } else {
            let artifact = InterfaceArtifactFile::write(root.join("artifacts"), &label, &serde_json::to_vec(&committed.output).map_err(string_error)?).map_err(string_error)?;
            PreparedConsentArtifact { artifact: materialized_ref(&artifact, &reader)?, manifest_path: artifact.manifest_path().to_path_buf() }
        };
        let publication = ConsentPublication { prepared, origin: committed.proof.writer_result.clone(), run: record.id.clone(), target,
            label, run_hash, checkpoint: checkpoint.id.clone(), paired_target, selected_return, return_install, request_id };
        require(saved.as_ref().is_none_or(|saved| saved == &publication), "consent saved reply changed its exact source or return checkpoint")?;
        restore_materialized(root, &publication.prepared, &publication.label, &committed.output, &reader)?;
        write_private_once(root, &name, &publication, reader.max_manifest_bytes)?;
        let admitted = if publication.paired_target.is_some() {
            read_private::<Vec<ConsentEvidenceRef>>(root, &format!("reply-pair-admitted-{}", publication.origin.movement_seq), reader.max_manifest_bytes)?
        } else {
            read_private::<ConsentEvidenceRef>(root, &format!("reply-admitted-{}", publication.origin.movement_seq), reader.max_manifest_bytes)?.map(|evidence| vec![evidence])
        };
        if let Some(admitted) = admitted { self.validate_publication_set(&mut reader, &publication, &admitted)?; }
        else { self.publications.push_back(ConsentPendingPublication { publication, cold: cold && saved.is_some(), verdict: None }); }
        require(self.completed.insert(committed.intake.movement_seq, committed).is_none(), "consent commit repeats an original intake")
    }

    fn validate_publication(&self, reader: &mut ConsentEvidenceReader, publication: &ConsentPublication,
        evidence: &ConsentEvidenceRef) -> Result<(), String> {
        let admitted = reader.admitted(evidence)?;
        let targets = std::iter::once(&publication.target).chain(publication.paired_target.as_ref()).cloned().collect::<Vec<_>>();
        require(evidence.artifact == publication.prepared.artifact && evidence.movement_seq > publication.origin.movement_seq
            && admitted.record.id == publication.run && admitted.manifest.label == publication.label
            && admitted.movement.from == Endpoint::Interface(self.controller.authority.binding.config.home_interface.clone())
            && targets.iter().any(|target| admitted.movement.to == Endpoint::Interface(target.clone()))
            && admitted.metadata.outbound_interface_bindings == targets
            && admitted.metadata.runtime.as_ref().is_some_and(|runtime| runtime.checkpoint == publication.checkpoint),
            "consent reply was not admitted to its exact original authored return")?;
        if let Some(selected) = &publication.selected_return {
            let original = reader.blob(&exact_hash(&publication.run_hash)?, reader.max_payload_bytes)?;
            let request = self.controller.authority.state.requests.get(publication.request_id.as_deref().ok_or("selected publication has no exact request")?)
                .ok_or("selected publication lost its original checked request")?;
            let return_install = publication.return_install.as_ref().ok_or("selected publication lost its actual checked return install")?;
            let installed = reader.admitted(&return_install.result)?;
            let protected = [selected.human_required.clone(), selected.approved.clone(), selected.declined.clone()];
            octopus_run_authoring::checked_return_interval(reader, selected, &installed.record_bytes, &admitted.record_bytes,
                return_install.result.movement_seq, evidence.movement_seq, &protected)?;
            octopus_run_authoring::validate_unchanged_admitted_checkpoint(&original, &admitted.record_bytes, &publication.checkpoint)?;
            octopus_run_authoring::validate_selected_return_reads(&admitted.metadata, &selected.selected_artifacts)?;
            if publication.paired_target.is_some() {
                let response: Value = serde_json::from_slice(&admitted.payload).map_err(string_error)?;
                let answer = response.get("operator_approval_response").ok_or("selected final publication has no approval body")?;
                require(answer.get("request_id") == Some(&json!(request.request_id)) && answer.get("call_id") == Some(&json!(request.call_id)),
                    "selected final publication changed its original request")?;
                let [requirement] = admitted.metadata.required_verified_artifacts.as_slice() else { return Err("selected final publication lost its verifier".into()); };
                let accepted = answer.get("decision") == Some(&json!(ConsentDecision::Accept));
                require(requirement.mark.as_str() == if accepted { "operator.approval" } else { "operator.input.provided" }
                    && requirement.verifier.as_str() == if accepted { "operator_approval" } else { "operator_input" }
                    && requirement.artifact_label == publication.label
                    && requirement.params == json!({"request_id":request.request_id,"call_id":request.call_id}),
                    "selected final publication changed its actual decision requirement")?;
                reader.admitted_requirement_mark(&admitted, publication.origin.movement_seq)?;
            }
        } else {
            require(ContentHash::of_bytes(&admitted.record_bytes).to_hex() == publication.run_hash,
                "consent reply was not admitted to its exact original authored return")?;
        }
        require_bound(&admitted.metadata, &self.controller.authority.reference)
    }

    fn validate_publication_set(&self, reader: &mut ConsentEvidenceReader, publication: &ConsentPublication,
        evidence: &[ConsentEvidenceRef]) -> Result<(), String> {
        let targets = std::iter::once(&publication.target).chain(publication.paired_target.as_ref()).collect::<Vec<_>>();
        require(evidence.len() == targets.len(), "consent publication has only a partial admitted output set")?;
        let mut previous = publication.origin.movement_seq;
        let mut boundary = None;
        for (evidence, target) in evidence.iter().zip(targets) {
            require(evidence.movement_seq > previous, "consent publication reversed its exact ordered output admissions")?;
            self.validate_publication(reader, publication, evidence)?;
            let admitted = reader.admitted(evidence)?;
            require(admitted.movement.to == Endpoint::Interface(target.clone()), "consent publication changed an ordered output target")?;
            let current = (ContentHash::of_bytes(&admitted.record_bytes), admitted.metadata.checkpoint_binding_policy.clone());
            require(boundary.as_ref().is_none_or(|expected| expected == &current), "consent paired publication did not share its exact admitted checkpoint snapshot")?;
            boundary = Some(current);
            previous = evidence.movement_seq;
        }
        Ok(())
    }

    fn reconcile_publication(&mut self, ctx: &InterfaceLiftContext, root: &Path, end: u64,
        admissions: &mut futures::stream::FuturesUnordered<ConsentAdmissionFuture>) -> Result<(), String> {
        let Some(pending) = self.publications.front() else { return Ok(()); };
        let publication = pending.publication.clone();
        let mut reader = consent_reader(ctx, Some(&self.controller))?;
        let after = publication.origin.movement_seq.checked_add(1).ok_or("consent publication sequence overflow")?;
        let targets = std::iter::once(&publication.target).chain(publication.paired_target.as_ref()).collect::<Vec<_>>();
        let mut found = BTreeMap::new();
        if after < end {
            reader.visit_run_interval(&publication.run, after, end, self.controller.authority.binding.bounds.ledger_batch_rows, |row| {
                if row.artifact.as_ref() != Some(&publication.prepared.artifact) { return Ok(false); }
                let evidence = ConsentEvidenceRef { artifact: publication.prepared.artifact.clone(), movement_seq: row.seq.0 };
                let Endpoint::Interface(target) = row.to;
                require(targets.contains(&&target), "consent publication changed its exact output set")?;
                require(found.insert(target, evidence).is_none(), "consent publication has duplicate admitted returns")?;
                Ok(found.len() == targets.len())
            })?;
        }
        if found.len() == targets.len() {
            let evidence = targets.iter().map(|target| found.get(*target).cloned().ok_or("consent publication changed its exact output set"))
                .collect::<Result<Vec<_>, _>>()?;
            self.validate_publication_set(&mut reader, &publication, &evidence)?;
            if publication.paired_target.is_some() {
                write_private_once(root, &format!("reply-pair-admitted-{}", publication.origin.movement_seq), &evidence, reader.max_manifest_bytes)?;
            } else {
                write_private_once(root, &format!("reply-admitted-{}", publication.origin.movement_seq), &evidence[0], reader.max_manifest_bytes)?;
            }
            self.publications.pop_front();
            return Ok(());
        }
        if !found.is_empty() {
            for evidence in found.values() { self.validate_publication(&mut reader, &publication, evidence)?; }
            require(read_private::<ArtifactRef>(root, &format!("publication-start-{}", publication.prepared.artifact.content.to_hex()), reader.max_manifest_bytes)?
                .as_ref() == Some(&publication.prepared.artifact), "partial consent publication lost its original no-resend evidence")?;
            return Ok(());
        }
        let started = read_private::<ArtifactRef>(root, &format!("publication-start-{}", publication.prepared.artifact.content.to_hex()), reader.max_manifest_bytes)?;
        if started.is_some() {
            require(!pending.cold || !reader.prefix_reaches_current_end(end)?, "consent reply publication is uncertain; no original admitted return is present in the reconciled ledger prefix, and no resend is allowed")?;
            return Ok(());
        }
        let committed = self.completed.values().find(|c| c.proof.writer_result == publication.origin).ok_or("consent reply lost its checked output")?;
        let artifact = restore_materialized(root, &publication.prepared, &publication.label, &committed.output, &reader)?;
        consent_start_admission(ctx, root, &reader, artifact, ConsentLiveEmission::Publication(publication.prepared.artifact), admissions)
    }

    fn reconcile_cold_flight(&mut self, ctx: &InterfaceLiftContext, root: &Path, end: u64) -> Result<(), String> {
        if !self.cold_flight { return Ok(()); }
        let mut reader = consent_reader(ctx, Some(&self.controller))?;
        if !reader.prefix_reaches_current_end(end)? { return Ok(()); }
        let Some(ConsentFlight::AwaitingWrite(flight)) = &self.controller.flight else {
            require(self.controller.flight.is_none(), "consent command publication is uncertain; restore its actual admitted command before continuation, without retransmission")?;
            self.cold_flight = false;
            return Ok(());
        };
        let prepared = flight.prepared.clone();
        let artifact = flight.artifact.clone();
        let after = prepared.command.movement_seq.checked_add(1).ok_or("consent pending sequence overflow")?;
        if after < end {
            let rows = reader.run_interval(&self.controller.authority.binding.config.authority_run, after, end, self.controller.authority.binding.bounds.ledger_batch_rows)?;
            let mut result = None;
            for row in rows {
                if row.from == Endpoint::Interface(self.controller.authority.binding.config.writer.door.clone()) {
                    let evidence = ConsentEvidenceRef { artifact: row.artifact.ok_or("consent writer result has no artifact")?, movement_seq: row.seq.0 };
                    require(result.replace(evidence).is_none(), "consent cold writer has ambiguous results")?;
                }
            }
            if let Some(result) = result {
                let joined = match &self.controller.flight {
                    Some(ConsentFlight::AwaitingWrite(flight)) if flight.result.as_ref() == Some(&result) => self.controller.finish_recovered_write(&mut reader, root)?,
                    _ => self.controller.admitted_write_result(&mut reader, result, root)?,
                };
                if let Some(joined) = joined { self.remember_commit(ctx, root, joined, true)?; }
                self.cold_flight = false;
                return Ok(());
            }
            let call = self.controller.authority.exact_write_call(&mut reader, &prepared, Some(&artifact), end)?;
            self.controller.authority.require_authority_edge(&call, &self.controller.authority.head,
                &self.controller.authority.binding.config.home_interface, &self.controller.authority.binding.config.writer.door, 1)?;
            let body: ToolCallArtifact = serde_json::from_slice(&call.payload).map_err(string_error)?;
            require(serde_json::to_value(&body).map_err(string_error)? == prepared.call_body(&self.controller.authority.binding.config.writer)?,
                "consent cold call changed its original prepared body")?;
            if let Some(ConsentFlight::AwaitingWrite(flight)) = &mut self.controller.flight {
                if flight.verdict.is_none() { flight.verdict = Some(call.movement.seq.0); }
            }
            self.cold_flight = false;
            return Ok(());
        }
        Err("consent writer publication is uncertain; no admitted original call/result proves continuation, and the writer will not be handed in again".into())
    }
}

impl ConsentLiveOwner {
    fn observe(&mut self, ctx: &InterfaceLiftContext, root: &Path, evidence: ConsentEvidenceRef,
        admissions: &mut futures::stream::FuturesUnordered<ConsentAdmissionFuture>) -> Result<(), String> {
        let mut reader = consent_reader(ctx, Some(&self.controller))?;
        let admitted = reader.admitted(&evidence)?;
        let config = self.controller.authority.binding.config.clone();
        if admitted.manifest.label == "application.consent.intake" {
            if let Some(completed) = self.completed.get(&evidence.movement_seq) {
                return require(completed.intake == evidence, "consent intake cursor changed committed evidence");
            }
            let known = self.controller.queued.contains(&evidence) || match &self.controller.flight {
                Some(ConsentFlight::PreparedCommand(pending)) => pending.command.intake == evidence,
                Some(ConsentFlight::AwaitingWrite(pending)) => pending.prepared.intake == evidence,
                None => false,
            };
            if !known { self.controller.queue(&mut reader, evidence, root)?; }
            return Ok(());
        }
        if admitted.record.id != config.authority_run { return Ok(()); }
        if admitted.manifest.label == config.command_label {
            if self.completed.values().any(|c| c.proof.command == evidence)
                || matches!(&self.controller.flight, Some(ConsentFlight::AwaitingWrite(f)) if f.prepared.command == evidence) {
                return Ok(());
            }
            let emission = self.controller.admitted_command(&mut reader, evidence, root)?;
            if !self.cold_flight { consent_start_owner_emission(ctx, root, &reader, emission, admissions)?; }
        } else if admitted.manifest.label == config.writer.result_label {
            if self.completed.values().any(|c| c.proof.writer_result == evidence) { return Ok(()); }
            let committed = match &self.controller.flight {
                Some(ConsentFlight::AwaitingWrite(f)) if f.result.as_ref() == Some(&evidence) => self.controller.finish_recovered_write(&mut reader, root)?,
                _ => self.controller.admitted_write_result(&mut reader, evidence, root)?,
            };
            if let Some(committed) = committed {
                self.cold_flight = false;
                self.remember_commit(ctx, root, committed, false)?;
            }
        } else if admitted.manifest.label == "operator.user_input.response" {
            self.controller.authority.require_passive_management(&admitted)?;
            write_private_once(root, &format!("removal-{}", evidence.movement_seq), &evidence, reader.max_manifest_bytes)?;
            if let Some(previous) = self.removals.insert(evidence.movement_seq, evidence.clone()) {
                require(previous == evidence, "consent removal cursor changed original evidence")?;
            }
        } else if admitted.manifest.label == "application.consent.management.result" {
            let publication = self.publications.iter().find(|p| p.publication.prepared.artifact == evidence.artifact)
                .map(|p| p.publication.clone());
            if let Some(publication) = publication {
                self.validate_publication(&mut reader, &publication, &evidence)?;
            } else {
                let mut known = false;
                for committed in self.completed.values() {
                    let key = format!("reply-admitted-{}", committed.proof.writer_result.movement_seq);
                    if read_private::<ConsentEvidenceRef>(root, &key, reader.max_manifest_bytes)?.as_ref() == Some(&evidence) { known = true; break; }
                }
                require(known, "consent management output has no original prepared checked publication")?;
            }
        } else {
            return Err("consent authority received an undeclared owner artifact".into());
        }
        Ok(())
    }

    fn prepare_removals(&mut self, ctx: &InterfaceLiftContext, root: &Path, end: u64,
        admissions: &mut futures::stream::FuturesUnordered<ConsentAdmissionFuture>) -> Result<(), String> {
        if self.controller.flight.is_some() || !self.publications.is_empty() { return Ok(()); }
        let mut reader = consent_reader(ctx, Some(&self.controller))?;
        for evidence in self.removals.values() {
            let admitted = reader.admitted(evidence)?;
            self.controller.authority.require_passive_management(&admitted)?;
            require(self.controller.authority.checked_heads.contains(&ContentHash::of_bytes(&admitted.record_bytes).to_hex()),
                "consent removal refers to an unproven authority head")?;
            let body: HumanRemovalBody = serde_json::from_slice(&admitted.payload).map_err(string_error)?;
            let response = body.operator_user_input_response;
            let list = self.controller.authority.state.management.get(&response.request_id).ok_or("consent removal lost its list")?;
            if list.removal.as_ref().is_some_and(|human| human.response == *evidence) { continue; }
            require(list.removal.is_none(), "consent removal repeats a retired list decision")?;
            let mut mark = None;
            for seq in list.list_command.movement_seq..evidence.movement_seq {
                let movement = reader.movement(seq)?;
                let Some(outcome) = movement.invocation_metadata.as_ref().and_then(|m| m.mark_outcome()) else { continue; };
                if movement.artifact.as_ref() == Some(&evidence.artifact) && outcome.run == self.controller.authority.binding.config.authority_run
                    && admitted.metadata.runtime.as_ref().is_some_and(|runtime| outcome.subject == runtime.checkpoint) {
                    require(mark.replace(seq).is_none(), "consent removal has ambiguous admitted human evidence")?;
                }
            }
            let human = ConsentHumanEvidence { response: evidence.clone(), mark_seq: mark.ok_or("consent removal has no actual admitted human mark")? };
            reader.human(&human, &response.request_id, None,
                &ConsentSession { run: self.controller.authority.binding.config.authority_run.clone(), client_interface: ctx.interface.clone() },
                &ctx.interface, false, "operator.user_input.response")?;
            let intake = ConsentIntake::RemoveAlways { authority: self.controller.authority.reference.clone(), request_id: response.request_id,
                subject: response.answers.subject, receipt_id: response.answers.receipt_id, human };
            let name = format!("removal-intake-{}", evidence.movement_seq);
            let saved = read_private::<PreparedConsentArtifact>(root, &name, reader.max_manifest_bytes)?;
            let (prepared, file) = if let Some(saved) = saved {
                let file = restore_materialized(root, &saved, "application.consent.intake", &serde_json::to_value(&intake).map_err(string_error)?, &reader)?;
                (saved, file)
            } else {
                let file = InterfaceArtifactFile::write(root.join("artifacts"), "application.consent.intake", &serde_json::to_vec(&intake).map_err(string_error)?).map_err(string_error)?;
                let saved = PreparedConsentArtifact { artifact: materialized_ref(&file, &reader)?, manifest_path: file.manifest_path().to_path_buf() };
                write_private_once(root, &name, &saved, reader.max_manifest_bytes)?;
                (saved, file)
            };
            let known = self.controller.queued.iter().any(|e| e.artifact == prepared.artifact)
                || self.completed.values().any(|c| c.intake.artifact == prepared.artifact);
            if known { continue; }
            let started = read_private::<ArtifactRef>(root, &format!("publication-start-{}", prepared.artifact.content.to_hex()), reader.max_manifest_bytes)?;
            if started.is_some() {
                let mut admitted_intake = None;
                if evidence.movement_seq.checked_add(1).is_some_and(|after| after < end) {
                    for movement in reader.run_interval(&self.controller.authority.binding.config.authority_run,
                        evidence.movement_seq + 1, end, self.controller.authority.binding.bounds.ledger_batch_rows)? {
                        if movement.artifact.as_ref() != Some(&prepared.artifact) { continue; }
                        let intake = ConsentEvidenceRef { artifact: prepared.artifact.clone(), movement_seq: movement.seq.0 };
                        self.controller.validate_queued_intake(&mut reader, &intake)?;
                        require(admitted_intake.replace(intake).is_none(), "consent removal intake was admitted twice")?;
                    }
                }
                if let Some(intake) = admitted_intake { self.controller.queue(&mut reader, intake, root)?; }
                else if admissions.is_empty() && reader.prefix_reaches_current_end(end)? { return Err("consent removal publication is uncertain; original admitted intake is required, without retransmission".into()); }
            } else {
                consent_start_admission(ctx, root, &reader, file, ConsentLiveEmission::Command(prepared.artifact), admissions)?;
            }
            break;
        }
        Ok(())
    }
}

pub async fn observe_application_consent(ctx: InterfaceLiftContext, reference: ArtifactRef) -> Result<(), String> {
    use futures::StreamExt;
    let root = ctx.interface_var_dir().join("application-consent");
    let mut tail = octopus_world_pyecho_life_runtime::LedgerTailReader::open_named(&ctx, "application-consent")?;
    let limits = ctx.required_host_io_limits()?;
    let anchor = read_private::<ConsentEvidenceRef>(&root, "bound-anchor", limits.max_control_frame_bytes)?;
    let mut owner = anchor.as_ref().map(|anchor| ConsentLiveOwner::recover(&ctx, &root, &reference, anchor)).transpose()?;
    let mut admissions = futures::stream::FuturesUnordered::<ConsentAdmissionFuture>::new();
    loop {
        if ctx.cancel.is_cancelled() { return Ok(()); }
        let batch = owner.as_ref().map(|owner| owner.controller.authority.binding.bounds.ledger_batch_rows).unwrap_or(256);
        for row in tail.read_new_uncommitted_limit(batch)? {
            let metadata = row.metadata()?;
            let selected = metadata.artifact_binding_refs.iter().any(|r| r == &reference);
            if selected {
                let evidence = ConsentEvidenceRef { artifact: row.movement.artifact.clone().ok_or("consent own movement has no artifact")?, movement_seq: row.movement.seq.0 };
                if owner.is_none() {
                    let mut reader = consent_reader(&ctx, None)?;
                    let admitted = reader.admitted(&evidence)?;
                    let authority = ConsentAuthority::load(&mut reader, &reference, &admitted)?;
                    require(authority.binding.config.home_interface == ctx.interface, "consent binding has another Home principal")?;
                    write_private_once(&root, "bound-anchor", &evidence, limits.max_control_frame_bytes)?;
                    owner = Some(ConsentLiveOwner::recover(&ctx, &root, &reference, &evidence)?);
                }
                owner.as_mut().expect("owner initialized").observe(&ctx, &root, evidence, &mut admissions)?;
            } else if let Some(owner) = &owner {
                require(metadata.runtime.as_ref().is_none_or(|runtime| runtime.run != owner.controller.authority.binding.config.authority_run),
                    "authority movement withdrew its exact selected authority binding")?;
            }
            tail.commit_through(&row)?;
        }
        tail.advance_settled_scan()?;
        if let Some(owner) = &mut owner {
            owner.reconcile_cold_flight(&ctx, &root, tail.next_seq())?;
            owner.reconcile_publication(&ctx, &root, tail.next_seq(), &mut admissions)?;
            owner.prepare_removals(&ctx, &root, tail.next_seq(), &mut admissions)?;
            if owner.publications.is_empty() {
                let mut reader = consent_reader(&ctx, Some(&owner.controller))?;
                if let Some(emission) = owner.controller.prepare_next(&mut reader, &root)? {
                    consent_start_owner_emission(&ctx, &root, &reader, emission, &mut admissions)?;
                }
            }
            require(admissions.len() <= owner.controller.authority.binding.bounds.max_pending,
                "consent live admission set exceeds its declared bound")?;
        }
        tokio::select! {
            _ = ctx.cancel.cancelled() => return Ok(()),
            changed = tail.wait_for_change(&ctx.cancel) => changed?,
            Some((emission, verdict)) = admissions.next(), if !admissions.is_empty() => {
                let owner = owner.as_mut().ok_or("consent admission verdict has no admitted owner")?;
                let mut reader = consent_reader(&ctx, Some(&owner.controller))?;
                let verdict = verdict?;
                let octopus_interfaces::DeliveryVerdict::Admitted { seq } = verdict else {
                    return Err(format!("consent owner artifact admission refused: {verdict:?}"));
                };
                let identity = match &emission { ConsentLiveEmission::Command(id) | ConsentLiveEmission::Write(id)
                    | ConsentLiveEmission::Publication(id) => id };
                let movement = reader.movement(seq)?;
                require(movement.kind == MovementKind::InterfaceArrival && movement.artifact.as_ref() == Some(identity)
                    && movement.from == Endpoint::Interface(ctx.interface.clone()), "consent publication verdict changed its actual admitted artifact")?;
                write_private_once(&root, &format!("publication-verdict-{}", identity.content.to_hex()),
                    &ConsentWriteVerdict { artifact: identity.clone(), seq }, reader.max_manifest_bytes)?;
                match emission {
                    ConsentLiveEmission::Write(identity) => {
                        if let Some(committed) = owner.controller.write_admission_verdict(&mut reader, &identity, seq, &root)? {
                            owner.remember_commit(&ctx, &root, committed, false)?;
                        }
                    }
                    ConsentLiveEmission::Publication(identity) => {
                        if let Some(pending) = owner.publications.iter_mut().find(|p| p.publication.prepared.artifact == identity) {
                            pending.verdict = Some(seq);
                        }
                    }
                    ConsentLiveEmission::Command(_) => {},
                }
            }
        }
    }
}

impl ConsentEvidenceReader {
    fn prefix_reaches_current_end(&mut self, end: u64) -> Result<bool, String> {
        let length = std::fs::metadata(&self.movements_path).map_err(string_error)?.len();
        if end == 0 { return Ok(length == 0); }
        self.movement(end - 1)?;
        let offset = self.row_offsets.get(&(end - 1)).ok_or("consent reconciled prefix has no verified byte boundary")?.1;
        require(offset <= length, "consent ledger was truncated below its reconciled prefix")?;
        Ok(offset == length)
    }
}
