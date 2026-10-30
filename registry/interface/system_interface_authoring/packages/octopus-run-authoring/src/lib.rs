pub use octopus_file_write_contract::{DecodeBudget, JsonFormat, Preimage};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};

use octopus_descriptors::{ArtifactId, CheckpointId, InterfaceId, RunId};
use octopus_interface_transport::InterfaceHostIoLimits;
use octopus_interface_transport::InterfaceLiftContext;
use octopus_interfaces::{
    CheckpointBindingPolicySnapshot, DeliveryVerdict, InterfaceArtifactFile,
    InterfaceArtifactManifest, InterfaceDescriptor, InterfaceInvocationEnvelope,
    InvocationMetadata,
};
use octopus_ledger::{ArtifactRef, Endpoint, Movement, MovementKind};
use octopus_ledger_index::LedgerSqliteReader;
use octopus_ledger_store::DurableMovementDecoder;
use octopus_ledger::{ContentHash, RequiredVerifiedArtifact};
use octopus_policy::{AuthoredInterfaceBinding, BindingArtifactPolicy, InterfaceBindingConfig};
use octopus_run_record::{ArtifactBinding, RunRecord};
use octopus_tool_handlers::ToolResultArtifact;
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message as WsMessage;

const FRONTIER_CHECKPOINT_IDS: &str = "frontier_checkpoint_ids";
const RUN_AUTHORING_BINDING: &str = "run_authoring_binding";
const CONTEXT_REFERENCE_LABEL: &str = "context.reference";
const INSTRUCTION_TEXT_LABEL: &str = "instruction.text";
const SESSION_CONTEXT_FIELD: &str = "session_context";
#[cfg(test)]
const TOOL_ROUTE_VERIFIER: &str = "tool_call_allowed";
const PROVIDER_HANDOFF_COMPLETE_LABEL: &str = "provider.handoff.complete";
const ONE_SHOT_PROGRESS_FIELD: &str = "provider_handoff_progress";
pub const PROVIDER_TRANSPORT_DISPATCH_LABEL: &str = "provider.transport.dispatch";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderEmissionRoute {
    ClientProjection,
    ClientProjectionWithVerifiedInputs { inputs: Vec<ProviderVerifiedInput> },
    Tool { tool: String },
    Interface { interface: InterfaceId },
    Operation { interface: InterfaceId, result_label: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderVerifiedInput {
    pub also_deliver_to_tool: Option<String>,
    pub also_deliver_to_interface: Option<InterfaceId>,
    pub artifact_label: String,
    pub required_verified_artifact: RequiredVerifiedArtifact,
}

pub struct ProviderEmission {
    pub artifact: InterfaceArtifactFile,
    pub route: ProviderEmissionRoute,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOutgoingBinding {
    pub interface: InterfaceId,
    pub artifact_label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_bindings: Vec<ArtifactId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProviderHandoffRoute {
    ClientProjection,
    ClientProjectionWithVerifiedInputs { inputs: Vec<ProviderVerifiedInput> },
    Tool { tool: String },
    Interface { interface: InterfaceId },
    Operation { interface: InterfaceId, result_label: String },
    TransportDispatch { attempt: u32 },
}

struct ProviderHandoffEmission {
    artifact: InterfaceArtifactFile,
    route: ProviderHandoffRoute,
}

impl From<ProviderEmission> for ProviderHandoffEmission {
    fn from(emission: ProviderEmission) -> Self {
        let route = match emission.route {
            ProviderEmissionRoute::ClientProjection => ProviderHandoffRoute::ClientProjection,
            ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { inputs } => {
                ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { inputs }
            }
            ProviderEmissionRoute::Tool { tool } => ProviderHandoffRoute::Tool { tool },
            ProviderEmissionRoute::Interface { interface } => {
                ProviderHandoffRoute::Interface { interface }
            }
            ProviderEmissionRoute::Operation { interface, result_label } => {
                ProviderHandoffRoute::Operation { interface, result_label }
            }
        };
        Self {
            artifact: emission.artifact,
            route,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderSourceRecovery {
    Process,
    Bridged { invocation_movement_seq: u64 },
    Completed,
    Interrupted { last_artifact_label: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderHandoffOutcome {
    Admitted {
        seq: u64,
        artifact: ArtifactRef,
    },
    TerminalFailure {
        failure_seq: u64,
        client_seq: u64,
        failure_class: String,
    },
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderTransportDispatch {
    label: String,
    provider_interface: InterfaceId,
    source_movement_seq: u64,
    attempt: u32,
    method: String,
    transport: String,
    endpoint: ProviderTransportEndpoint,
    request: ProviderTransportRequest,
    credential_handle_fingerprint: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderTransportEndpoint {
    scheme: String,
    host: String,
    port: Option<u16>,
    path: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderTransportRequest {
    bytes: u64,
    blake3: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderHandoffCompletion {
    label: String,
    provider_interface: InterfaceId,
    source_movement_seq: u64,
    artifacts: Vec<ArtifactRef>,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct OneShotProgressIdentity {
    provider_interface: InterfaceId,
    run: RunId,
    source_movement_seq: u64,
}

#[derive(Clone)]
pub struct ProviderHandoffSession {
    interface: InterfaceId,
    movements_path: PathBuf,
    evidence: Arc<Mutex<ProviderLedgerEvidenceIndex>>,
    writer_history: Arc<Mutex<BTreeMap<RunId, writer_run::WriterHistory>>>,
    writer_call_generations: Arc<Mutex<writer_run::WriterCallIndex>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum HandoffDeliveryMode {
    OneShot(OneShotHandoffPlan),
    Rearm,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OneShotHandoffPlan {
    terminal: BTreeMap<String, OneShotHandoffSlot>,
    dispatch: BTreeMap<u32, OneShotHandoffSlot>,
    completion: OneShotHandoffSlot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OneShotHandoffSlot {
    checkpoint: CheckpointId,
    epoch: u64,
    required_mark: String,
    required_verifier: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OneShotAdmittedArtifact {
    artifact: ArtifactRef,
    arrival_seq: u64,
    mark_seq: u64,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct RuntimeCoordinateKey {
    run: String,
    worker: String,
    interface: String,
    checkpoint: String,
    epoch: u64,
}

#[derive(Clone, Debug)]
enum UniqueEvidence {
    One(u64),
    Repeated,
}

#[derive(Clone, Debug)]
enum UniqueArrivalCoordinate {
    One(RuntimeCoordinateKey),
    Repeated,
}

struct ProviderLedgerEvidenceIndex {
    interface: InterfaceId,
    movements_path: PathBuf,
    ledger: Arc<ledger_rows::LedgerRows>,
    max_row_bytes: u64,
    spotlight: Option<LedgerSqliteReader>,
    byte_offset: u64,
    observed_len: u64,
    next_seq: u64,
    through_seq: Option<u64>,
    poison: Option<String>,
    handoff_metadata_poison: HashMap<(String, String), String>,
    session_metadata_poison: HashMap<(String, String), String>,
    context_metadata_poison: HashMap<(String, String, String), String>,
    latest_run_movement: HashMap<(String, String), u64>,
    first_run_placements: HashMap<String, (u64, String)>,
    provider_run_arrivals: HashMap<(String, String, String), BTreeSet<u64>>,
    session_run_movements: HashMap<(String, String), BTreeSet<u64>>,
    session_provider_arrivals: HashMap<(String, String, String), BTreeSet<u64>>,
    session_provider_inputs: HashMap<(String, String), BTreeMap<u64, String>>,
    coordinate_arrivals: HashMap<(String, RuntimeCoordinateKey), UniqueEvidence>,
    pending_arrival_proofs: HashMap<(String, RuntimeCoordinateKey), Movement>,
    mark_arrival_coordinates: HashMap<(String, String, String, u64), UniqueArrivalCoordinate>,
    coordinate_artifacts: HashMap<RuntimeCoordinateKey, Vec<(ArtifactRef, u64)>>,
    coordinate_outbounds: HashMap<RuntimeCoordinateKey, Vec<IndexedOutboundArtifact>>,
    coordinate_marks: HashMap<(String, RuntimeCoordinateKey, String, String), UniqueEvidence>,
    retained_delivery: pending_delivery::Ledger,
    ledger_file: Option<ledger_rows::LedgerFile>,
    tail: Option<(u64, ContentHash)>,
}

type ContextPublications = BTreeMap<String, BTreeSet<u64>>;

#[derive(Clone, PartialEq, Eq)]
struct IndexedOutboundArtifact {
    source: InterfaceId,
    artifact: ArtifactRef,
    seq: u64,
    run_record_hash: Option<String>,
    outbound_interface_bindings: Vec<InterfaceId>,
    checkpoint_binding_policy: Option<CheckpointBindingPolicySnapshot>,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
    artifact_binding_refs: Vec<ArtifactRef>,
    readable_artifacts: BTreeSet<ArtifactId>,
    role_artifacts: Vec<String>,
}

struct AuthoringFailureSourceValidation<'a> {
    ctx: &'a InterfaceLiftContext,
    record: &'a RunRecord,
    record_bytes: &'a [u8],
    config: &'a RunAuthoringConfig,
    failure: &'a InterfaceFailureEnvelope,
    failure_seq: u64,
    after_seq: Option<u64>,
}

struct AuthoringAttemptSourceValidation<'a> {
    ctx: &'a InterfaceLiftContext,
    record: &'a RunRecord,
    record_bytes: &'a [u8],
    config: &'a RunAuthoringConfig,
    call: FileWriteCallEnvelope,
    manifest_path: PathBuf,
}

struct ValidatedAuthoringAttempt {
    manifest_path: PathBuf,
    provider_source_movement_seq: u64,
    authored: AuthoredRun,
}

enum AuthoringAttemptRecovery {
    None,
    InFlight(ValidatedAuthoringAttempt),
    OtherSource(u64),
}

struct ValidatedAuthoringFailureSource {
    provider_source_movement_seq: u64,
    correlation_id: String,
}

#[derive(Clone)]
pub struct ProviderOutputHandoff {
    ctx: InterfaceLiftContext,
    invocation_metadata: InvocationMetadata,
    config: RunAuthoringConfig,
    run_path: PathBuf,
    hand_in_root: PathBuf,
    var_dir: PathBuf,
    source_movement_seq: u64,
    source_carries_context: bool,
    failed_source: bool,
    checked_settlement_output: Option<(ArtifactRef, ArtifactRef)>,
    context_output_labels: BTreeSet<String>,
    session_snapshot_labels: BTreeSet<String>,
    standing_candidate_labels: BTreeSet<String>,
    granted_tools: BTreeSet<String>,
    granted_interfaces: BTreeSet<InterfaceId>,
    granted_mark_outcomes: BTreeSet<String>,
    bound_topology: BoundRunAuthoringTopology,
    delivery_mode: HandoffDeliveryMode,
    session: ProviderHandoffSession,
    invocation_session_snapshot: Option<ProviderInvocationSessionSnapshot>,
    state: Arc<tokio::sync::Mutex<HandoffState>>,
    result_reader: Arc<tokio::sync::Mutex<LedgerTailReader>>,
}

#[derive(Clone)]
struct HandoffState {
    validated_withdrawal_output: Option<ArtifactRef>,
    context_artifacts: Vec<ArtifactId>,
    static_context_artifacts: Vec<ArtifactId>,
    session_context: ResolvedSessionContext,
    session_snapshot_head: Option<SessionSnapshotHead>,
    run_record_bytes: Vec<u8>,
    run_record_evidence_seq: Option<u64>,
    consumed_input: bool,
    consumed_context_free_independent_input: Option<RunAuthoringCheckpoint>,
    consumed_verified_input: bool,
    deferred_context_generations: u64,
}

#[derive(Clone)]
struct ResolvedSessionContext {
    provider_interface: InterfaceId,
    artifacts: Vec<SessionContextArtifact>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct SessionContextSnapshot {
    schema_version: u32,
    provider_interface: InterfaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<ArtifactRef>,
    artifacts: Vec<SessionContextArtifact>,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionContextArtifact {
    pub label: String,
    pub payload: serde_json::Value,
    pub source_artifact: ArtifactRef,
    pub source_movement_seq: Option<u64>,
}

#[derive(Clone)]
pub struct ProviderInvocationSessionSnapshot {
    pub carrier: ArtifactRef,
    pub artifacts: Arc<Vec<SessionContextArtifact>>,
}

#[derive(Clone)]
struct SessionSnapshotHead {
    artifact: ArtifactRef,
    covered_artifact_count: usize,
    covered_fingerprint: ContentHash,
}

#[derive(Clone, Default)]
struct BoundArtifactProvenance {
    movement_seq: Option<u64>,
    provider_arrival_seq: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PendingHandoff {
    source_movement_seq: u64,
    items: Vec<PendingHandoffItem>,
    terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_publication: Option<Vec<ArtifactId>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    deferred_publication: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    source_without_context: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settlement_of: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    dispatch_fenced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    withdrawal: Option<Box<source_withdrawal::Withdrawal>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replacement: Option<Box<rejected_final::Rejection>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    standing: Vec<PendingHandoffItem>,
}

impl PendingHandoff {
    fn publication_unfinished(&self) -> bool {
        self.context_publication.is_some() && !self.deferred_publication
    }

    fn with_standing_items(&self) -> Self {
        let mut pending = self.clone();
        pending.items = std::mem::take(&mut pending.standing);
        pending.items.extend(self.items.iter().cloned());
        pending
    }
}

mod context_publication;
mod pending_delivery;
mod rejected_final;
pub use rejected_final::RejectedFinalOutput;
mod standing_routes;
pub use standing_routes::{open_standing_handoff_before, standing_route_admits_only};
mod result_inputs;
mod output_connections;
mod input_revision;
pub use input_revision::{declared_owner_input_bindings, prepare_provider_input_revision, prepare_provider_input_rearm, prepare_provider_input_payloads, provider_input_rearm_ready, provider_input_payloads_ready, ProviderInputRevision};
mod authoring_retry;
mod failed_source;
pub use failed_source::{payload_carries_session_context, FailedSourceHandoffError};
mod writer_run;
pub mod placement;
pub use writer_run::{separate_writer_run, separate_owner_writer_run, owner_writer_binding, OwnerWriterEdit, OwnerWriterEvidence, WriterRunBinding};
mod source_replay;
mod source_settlement;
pub use source_settlement::PendingSourceSettlement;
mod source_withdrawal;
pub use source_withdrawal::{PendingSourceIntent, PendingSourceWithdrawal};
mod settled_definition;
pub mod capability_amendment;
pub use capability_amendment::ProviderCapabilityAmendment;

#[cfg(test)]
#[path = "../tests/support/source_replay.rs"]
mod source_replay_tests;


pub mod generated_artifacts;
pub mod input_artifacts;
pub mod snapshot_projection;

mod handoff_model;
pub use handoff_model::*;
mod run_write;
pub use run_write::*;
mod checked_chain;
pub use checked_chain::*;
mod authoring_result;
pub use authoring_result::*;
mod checked_edit;
pub use checked_edit::*;
mod authoring_attempt;
use authoring_attempt::*;
mod evidence_index;
use evidence_index::*;
mod ledger_rows;
mod profile;
pub use profile::*;
mod topology;
pub use topology::*;
mod config;
mod frontier;
use frontier::*;
mod pending;
use pending::*;
mod transport;
use transport::*;
mod artifacts;
use artifacts::*;
mod session_context;
pub use session_context::*;
mod output_handoff;

#[cfg(test)]
#[path = "../tests/support/lib_tests.rs"]
mod tests;
