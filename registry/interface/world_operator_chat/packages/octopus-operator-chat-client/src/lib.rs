use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_descriptors::{EeId, InterfaceId, ToolDescriptorId, parse_strict};
use octopus_ee::EeTemplateDescriptor;
use octopus_interface_transport::SpawnLifeConfig;
use octopus_interfaces::{InterfaceDescriptor, interface_is_worker_pin};
use octopus_ledger::RequiredVerifiedArtifact;
use octopus_policy::{
    AuthoredInterfaceBinding, BindingArtifactPolicy, InterfaceBindingConfig, WorkerInterfacePolicy,
};
use octopus_run_record::{ArtifactBinding, RunRecord};
use octopus_tool_registry::{ToolDescriptor, load_tool_descriptor_from_dir};
use octopus_world_terminal_client::{
    CapabilityFormationRequest, CapabilityRegistry, FormedCapabilityTopology,
    resolve_live_ee_policy_chain,
};
#[cfg(test)]
use octopus_world_terminal_client::form_capability_topology;
use sha2::{Digest, Sha256};

pub mod context_bindings;
pub mod collection_context;
mod maintenance_attachment;
pub mod maintenance_observer;
pub mod publication_selection;
pub mod input_intake;
mod retained_connections;
mod home_voice;
mod home_outputs;
pub mod native_run_authoring;
mod writer_runs;
pub use writer_runs::{OperatorWriterRunPlanRequest, plan_operator_writer_runs};
#[cfg(test)]
#[path = "../tests/support/home_delegation.rs"]
mod home_delegation;
#[cfg(test)]
#[path = "../tests/support/home_turn_controls.rs"]
mod home_turn_controls;
#[cfg(test)]
#[path = "../tests/support/retained_connections.rs"]
mod retained_connections_tests;
#[cfg(test)]
#[path = "../tests/support/input_intake.rs"]
mod input_intake_tests;
pub mod run_composition;
pub mod run_revision;
pub mod task_retirement;
pub use run_composition::{RunCompositionPlan, RunCompositionRequest, plan_run_composition};

pub const OPERATOR_CHAT_INTERFACE: &str = "world_operator_chat";
const PROVIDER_HANDOFF_COMPLETE_LABEL: &str = "provider.handoff.complete";
const PROVIDER_TRANSPORT_DISPATCH_LABEL: &str = "provider.transport.dispatch";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorChatPlanRequest {
    pub registry_root: PathBuf,
    pub ee_instances_root: PathBuf,
    #[serde(default)]
    pub ee_policy_root: Option<PathBuf>,
    #[serde(default)]
    pub run_records_root: Option<PathBuf>,
    pub write_hash_algorithm: WriteHashAlgorithm,
    #[serde(default)]
    pub interface_private_state_root: Option<PathBuf>,
    pub chat_id: String,
    pub provider_interface_id: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub ee_id: Option<String>,
    #[serde(default)]
    pub worker_interface_id: Option<String>,
    pub client_surface_role: String,
    pub cycle: String,
    #[serde(default)]
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    #[serde(default)]
    pub external_inputs: Vec<ExternalInputSeed>,
    #[serde(default)]
    pub external_relays: Vec<ExternalRelaySeed>,
    #[serde(default)]
    pub model_outputs: Vec<ModelOutputSeed>,
    #[serde(default)]
    pub context_artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(default)]
    pub payload_artifact_bindings: Vec<octopus_ledger::ArtifactRef>,
    #[serde(default)]
    pub publications: Vec<octopus_context_collections::CollectionSelection>,
    #[serde(default)]
    pub selected_worker_environment_kinds: Vec<String>,
    #[serde(default)]
    pub selected_tools: Vec<SelectedToolSeed>,
    #[serde(default)]
    pub retain_current_capabilities: bool,
    #[serde(default)]
    pub existing_run_record: Option<serde_json::Value>,
    pub conversation: ConversationSeed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteHashAlgorithm {
    Blake3,
    Sha256,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalInputSeed {
    pub checkpoint_id: String,
    pub interface_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalRelaySeed {
    pub checkpoint_id: String,
    pub interface_id: String,
    pub outbound_interface_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOutputSeed {
    pub checkpoint_id: String,
    pub interface_id: String,
    pub required_verified_artifact: RequiredVerifiedArtifact,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedToolSeed {
    pub tool: String,
    pub interface: String,
    pub worker_environment_kind: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSeed {
    pub ledger_offset: u64,
    #[serde(default)]
    pub artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(default)]
    pub pinned_artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(default)]
    pub pinned_context_items: Vec<PinnedContextSeed>,
    #[serde(default)]
    pub continued_from: Option<ContinuationSeed>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedContextSeed {
    pub artifact_id: String,
    pub source_artifact_id: String,
    pub source_message_id: String,
    pub text: String,
    pub start: u64,
    pub end: u64,
    pub role: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBindingSeed {
    pub id: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationSeed {
    pub run_id: String,
    pub artifact_id: String,
}

#[derive(Debug, serde::Serialize)]
pub struct OperatorChatPlan {
    pub run_id: String,
    pub chat_interface_id: String,
    pub model_interface_id: String,
    pub worker_interface_id: String,
    pub client_surface_role: String,
    pub authoring_source_interface_id: String,
    pub authoring_environment_interface_id: String,
    pub model: String,
    pub reasoning_effort: String,
    pub service_tier: Option<String>,
    pub expected_before_hash_algorithm: WriteHashAlgorithm,
    pub ee_template_chain: Vec<String>,
    pub capability_doors: BTreeMap<String, String>,
    pub capability_tools: BTreeMap<String, String>,
    pub authoring_tool_id: String,
    pub worker_relays: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub publication_evidence: Vec<publication_selection::PublicationSelectionEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_expansion: Option<publication_selection::PublicationExpansionReceipt>,
    #[serde(skip)]
    pub authoring_profile: OctopusOsAuthoringProfile,
    #[serde(rename = "authoring_profile")]
    authored_authoring_profile: serde_json::Value,
    pub required_interface_ids: Vec<String>,
    pub required_tool_ids: Vec<String>,
    pub interface_writes: Vec<PlannedClientWrite>,
    pub run_writes: Vec<PlannedClientWrite>,
    pub removals: Vec<PlannedClientRemoval>,
    pub run_record_content: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedClientWrite {
    pub path: PathBuf,
    pub content: String,
    pub tag: String,
    pub expected_before_hash: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct PlannedClientRemoval {
    pub path: PathBuf,
    pub tag: String,
    pub files: Vec<PlannedClientRemovalFile>,
}

#[derive(Debug, serde::Serialize)]
pub struct PlannedClientRemovalFile {
    pub relative_path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, serde::Deserialize)]
struct ClientConfig {
    schema_version: u32,
    interface: String,
    artifact_labels: ClientArtifactLabels,
    authoring: ClientAuthoringConfig,
    run: ClientRunConfig,
    #[serde(default)]
    home_voice: Option<home_voice::HomeVoiceConfig>,
    #[serde(default)]
    result_receiving_updates: Vec<ResultReceivingCeilingPredecessor>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultReceivingCeilingPredecessor {
    worker_template_interface: String,
    mark_outcomes: Vec<octopus_descriptors::MarkId>,
}

#[derive(Debug, serde::Deserialize)]
struct ClientArtifactLabels {
    message: String,
    turn_control: String,
    tool_call: String,
    #[serde(flatten)]
    _rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct ClientAuthoringConfig {
    source_interface: String,
    room_interface: String,
    tool: String,
    result_timeout_ms: u64,
    #[serde(flatten)]
    _rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct ClientRunConfig {
    interface_prefix: String,
    run_prefix: String,
    ee: String,
    worker: String,
    tool_call_mark: String,
    tool_call_verifier: String,
    #[serde(default)]
    tool_result_receiving: Option<ToolResultReceiving>,
    listen_env: String,
    context_artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(flatten)]
    _rest: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultReceiving {
    mark: octopus_descriptors::MarkId,
    verifier: octopus_descriptors::VerifierId,
    params: serde_json::Value,
}

impl ToolResultReceiving {
    fn requirement(&self, tool: &str) -> Result<RequiredVerifiedArtifact, String> {
        self.contract(&format!("{tool}.result"))
    }

    fn contract(&self, artifact_label: &str) -> Result<RequiredVerifiedArtifact, String> {
        if self.mark.as_str().is_empty() || self.mark.as_str().trim() != self.mark.as_str()
            || self.verifier.as_str().is_empty() || self.verifier.as_str().trim() != self.verifier.as_str()
            || !self.params.is_object()
        {
            return Err("tool_result_receiving requires exact mark/verifier ids and object params".into());
        }
        Ok(RequiredVerifiedArtifact { mark: self.mark.clone(), artifact_label: artifact_label.into(),
            verifier: self.verifier.clone(), params: self.params.clone() })
    }
}

struct PlannedIdentity {
    voice: Option<home_voice::PlannedVoice>,
    native_communication: Option<run_composition::CompositionCommunication>,
    chat_id: String,
    run_id: String,
    ee_id: String,
    chat_interface_id: String,
    model_interface_id: String,
    worker_interface_id: String,
    authoring_source_id: String,
    authoring_environment_id: String,
    surface_role: String,
    provider_choice_interface: String,
    worker_choice_interface: String,
    capability_doors: BTreeMap<String, String>,
    capability_tools: BTreeMap<String, String>,
    capability_tool_sources: BTreeMap<String, ToolDescriptor>,
    authoring_tool_id: String,
    code_mode_owners: BTreeMap<String, PlannedCodeModeOwner>,
}

struct ResolvedProvider {
    interface: InterfaceDescriptor,
    model: String,
    reasoning_effort: String,
    service_tier: Option<String>,
    client_operation_request_labels: Vec<String>,
}

struct ResolvedProviderModel {
    model: String,
    default_reasoning_effort: String,
    supported_reasoning_efforts: Vec<String>,
}

struct RunSelection {
    ee_id: String,
    worker_choice_interface: String,
    provider_choice_interface: String,
    provider_runtime_interface: Option<String>,
    run_id: Option<String>,
    chat_interface_id: Option<String>,
    worker_runtime_interface: Option<String>,
    surface_role: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OctopusOsInterfaceSidecar {
    schema_version: u32,
    workspace: String,
    interface: String,
    owner_interface: String,
    component: String,
    surface_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_choice_interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worker_choice_interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authoring_profile: Option<OctopusOsAuthoringProfile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    task_authoring_profiles: Vec<OctopusOsAuthoringProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    presentation: Option<OctopusOsPresentation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    voice_context: Option<home_voice::HomeVoiceContext>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OctopusOsAuthoringProfile {
    pub ee_id: String,
    pub worker_choice_interface: String,
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_inputs: Vec<ExternalInputSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_relays: Vec<ExternalRelaySeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_outputs: Vec<ModelOutputSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_artifact_bindings: Vec<ArtifactBindingSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payload_artifact_bindings: Vec<octopus_ledger::ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_worker_environment_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_tools: Vec<SelectedToolSeed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publication_evidence: Vec<publication_selection::PublicationSelectionEvidence>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OctopusOsPresentation {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    name: String,
    projects: Vec<OctopusOsProjectPresentation>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OctopusOsProjectPresentation {
    ee_id: String,
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    icon: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    color: String,
}

struct PreservedOctopusOsClientConfiguration {
    declaration: OctopusOsInterfaceSidecar,
    authored: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum BoundCheckpointRole {
    Stable,
    IndependentInput,
    OwnerInput,
    VerifiedInput,
    ResultInput,
    ProviderArtifactOutput,
    ProviderInterfaceOutput,
    ProviderToolOutput,
    ProviderTransportDispatch,
    ProviderHandoffCompletion,
    AuthoringCall,
    AuthoringResult,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundAuthoringWriter {
    owner: InterfaceId,
    source: InterfaceId,
    door: InterfaceId,
    tool: String,
    call_label: String,
    result_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundCheckpointTemplate {
    name: String,
    role: BoundCheckpointRole,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    result_receiving: BTreeMap<String, RequiredVerifiedArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    static_artifact_bindings: Vec<octopus_descriptors::ArtifactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_authoring: Option<BoundAuthoringWriter>,
    interface_binding: AuthoredInterfaceBinding,
    outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    include_context: bool,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundRunAuthoringTopology {
    schema_version: u32,
    run: octopus_descriptors::RunId,
    worker: InterfaceId,
    provider_interface: InterfaceId,
    authoring_source_interface: InterfaceId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    payload_artifact_bindings: Vec<octopus_ledger::ArtifactRef>,
    checkpoints: Vec<BoundCheckpointTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writer_run: Option<octopus_run_authoring::WriterRunBinding>,
}


mod topology_plan;
pub use topology_plan::*;
mod client_config;
pub(crate) use client_config::*;
mod run_selection;
pub(crate) use run_selection::*;
mod provider_resolution;
pub(crate) use provider_resolution::*;
mod identity;
pub(crate) use identity::*;
mod capability_plan;
pub(crate) use capability_plan::*;
mod code_mode_plan;
pub(crate) use code_mode_plan::*;
mod approval_references;
pub(crate) use approval_references::*;
mod private_tools;
pub(crate) use private_tools::*;
mod worker_pin;
pub(crate) use worker_pin::*;
mod provider_interface;
pub(crate) use provider_interface::*;
mod octopus_os_sidecar;
pub(crate) use octopus_os_sidecar::*;
mod interface_life;
pub(crate) use interface_life::*;
mod capability_selection;
pub(crate) use capability_selection::*;
mod chat_bindings;
pub(crate) use chat_bindings::*;
mod chat_run_record;
pub(crate) use chat_run_record::*;
mod bound_topology;
pub(crate) use bound_topology::*;
mod descriptor_copies;
pub(crate) use descriptor_copies::*;
mod private_ownership;
pub(crate) use private_ownership::*;
mod planned_writes;
pub(crate) use planned_writes::*;
mod external_routes;
pub(crate) use external_routes::*;
mod operation_routes;
pub(crate) use operation_routes::*;
mod segments;
pub(crate) use segments::*;

#[cfg(test)]
#[path = "../tests/support/lib_tests.rs"]
mod tests;
