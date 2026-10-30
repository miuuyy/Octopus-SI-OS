use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use octopus_app_server_interface::InterfaceInputArtifactBody;
use octopus_descriptors::{
    ArtifactId, EeId, InterfaceId, MarkId, RunId, ToolDescriptorId, VerifierId,
    WorkerEnvironmentKindId,
};
use octopus_ee::{
    EeInstance, EeInstancePolicyNarrowing, EeTemplateDescriptor, bootstrap_ee_instance_id,
};
use octopus_interface_transport::{InterfaceHostIoLimits, SpawnLifeConfig, spawn_life_config_of};
use octopus_interfaces::{DeliveryVerdict, InterfaceDescriptor, InterfaceDriverConfig};
use octopus_ledger::ContentHash;
use octopus_policy::{
    AuthoredInterfaceBinding, BindingArtifactPolicy, InterfaceBindingConfig, WorkerInterfacePolicy,
};
use octopus_runtime::RequiredVerifiedArtifact;
use octopus_run_record::RunRecord;
use octopus_tool_handlers::ToolResultArtifact;
use octopus_tool_registry::{ToolDescriptor, load_tool_descriptor_from_dir};

use crate::capability_formation::{
    CapabilityFormationRequest, FormedInterfaceOperation, WorkerRelayCapabilityBinding,
    form_capability_topology, load_interface_operation_binding, resolve_live_ee_policy_chain,
};
use crate::daemon_arrival::{TerminalDaemonEmitter, interface_control_socket_path};
use crate::emit_input::materialize_input_artifact;
use crate::read_outbound::TerminalOutboundReader;

fn all_binding(interface: &InterfaceId) -> AuthoredInterfaceBinding {
    AuthoredInterfaceBinding::all(interface.clone())
}

fn names_binding(
    interface: &InterfaceId,
    names: impl IntoIterator<Item = String>,
) -> Result<AuthoredInterfaceBinding, TopologyError> {
    let names = names.into_iter().collect::<BTreeSet<_>>();
    if names.is_empty() {
        return Err(contract(format!(
            "interface '{}' artifact-policy name set is empty",
            interface.as_str()
        )));
    }
    let binding = AuthoredInterfaceBinding {
        interface: interface.clone(),
        config: InterfaceBindingConfig {
            artifact_policy: BindingArtifactPolicy::Allow {
                names: names.into_iter().collect(),
                namespaces: Vec::new(),
            },
        },
    };
    binding.validate().map_err(contract)?;
    Ok(binding)
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalClientDescriptor {
    schema_version: u32,
    interface: String,
    artifact_labels: ArtifactLabels,
    authoring: AuthoringConfig,
    shell: ShellConfig,
    private_run: PrivateRunConfig,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactLabels {
    operator_input: String,
    turn_control: String,
    tool_call: String,
    file_write_result: String,
    terminal_output: String,
    error_output: String,
    stream_outputs: Vec<String>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthoringConfig {
    room_interface: String,
    tool: String,
    result_timeout_ms: u64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellConfig {
    tool: String,
    result_label: String,
    result_timeout_ms: u64,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateRunConfig {
    ee_template: String,
    parent_ee: String,
    parent_ee_template: String,
    interface_templates: InterfaceTemplates,
    id_prefixes: IdPrefixes,
    activation_driver_ref: String,
    terminal_listen_env_prefix: String,
    shell_terminal_listen_env_prefix: String,
    interface_authoring_listen_env_prefix: String,
    model_hidden_tools: Vec<String>,
    tool_call_mark: String,
    tool_call_verifier: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InterfaceTemplates {
    terminal: String,
    shell_terminal: String,
    provider: String,
    interface_authoring: String,
    authoring_environment: String,
    shell_environment: String,
    worker: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IdPrefixes {
    run: String,
    ee: String,
    terminal: String,
    shell_terminal: String,
    provider: String,
    interface_authoring: String,
    authoring_environment: String,
    shell_environment: String,
    worker: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalHostRoots {
    pub repo_root: PathBuf,
    pub worktree_root: PathBuf,
    pub root_slots: BTreeMap<String, PathBuf>,
    pub registry_root: PathBuf,
    pub registry_artifact_root: PathBuf,
    pub registry_interface_root: PathBuf,
    pub run_records_root: PathBuf,
    pub ee_instances_root: PathBuf,
    pub durable_ledger_root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateRunRequest {
    pub model: String,
    pub reasoning_effort: String,
    pub goal: String,
    pub seed_artifacts: Vec<String>,
    pub existing_ee: Option<EeId>,
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
}

impl PrivateRunRequest {
    pub fn new(
        model: impl Into<String>,
        reasoning_effort: impl Into<String>,
        goal: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            reasoning_effort: reasoning_effort.into(),
            goal: goal.into(),
            seed_artifacts: Vec::new(),
            existing_ee: None,
            door_host_slots: BTreeMap::new(),
        }
    }

    pub fn with_seed_artifacts(mut self, seed_artifacts: Vec<String>) -> Self {
        self.seed_artifacts = seed_artifacts;
        self
    }

    pub fn with_existing_ee(mut self, ee: EeId) -> Self {
        self.existing_ee = Some(ee);
        self
    }

    pub fn with_door_host_slots(
        mut self,
        door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    ) -> Self {
        self.door_host_slots = door_host_slots;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateRunTopology {
    pub opaque_id: String,
    pub run: RunId,
    pub ee: EeId,
    pub terminal_interface: InterfaceId,
    pub shell_terminal_interface: InterfaceId,
    pub provider_interface: InterfaceId,
    pub interface_authoring_interface: InterfaceId,
    pub authoring_environment_interface: InterfaceId,
    pub worker_environment_interfaces: BTreeMap<String, InterfaceId>,
    pub tool_interfaces: BTreeMap<String, InterfaceId>,
    pub operation_interfaces: BTreeMap<String, InterfaceId>,
    pub exact_tools: BTreeMap<String, String>,
    pub shell_environment_interface: InterfaceId,
    pub worker_interface: InterfaceId,
    pub model: String,
    pub reasoning_effort: String,
    pub operator_input_label: String,
    pub turn_control_label: String,
    pub terminal_socket: PathBuf,
    pub shell_terminal_socket: PathBuf,
    pub interface_authoring_socket: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(from = "ProviderReasoningEffortEntry")]
pub struct ProviderReasoningEffortPreset {
    pub effort: String,
    pub description: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderReasoningEffortObject {
    effort: String,
    #[serde(default)]
    description: String,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ProviderReasoningEffortEntry {
    Effort(String),
    Preset(ProviderReasoningEffortObject),
}

impl From<ProviderReasoningEffortEntry> for ProviderReasoningEffortPreset {
    fn from(entry: ProviderReasoningEffortEntry) -> Self {
        match entry {
            ProviderReasoningEffortEntry::Effort(effort) => Self {
                effort,
                description: String::new(),
            },
            ProviderReasoningEffortEntry::Preset(preset) => Self {
                effort: preset.effort,
                description: preset.description,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelPreset {
    pub model: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ProviderReasoningEffortPreset>,
    #[serde(default = "default_true")]
    pub show_in_picker: bool,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub output_modalities: Vec<String>,
    #[serde(default)]
    pub supports_tools: Option<bool>,
    #[serde(default)]
    pub supports_reasoning: Option<bool>,
    #[serde(default)]
    pub service_tiers: Vec<serde_json::Value>,
    #[serde(default)]
    pub default_service_tier: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateShellExecution {
    pub command: String,
    pub worktree: PathBuf,
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub admitted_seq: u64,
    pub result_seq: u64,
    pub result_artifact: ArtifactId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoredPrivateRun {
    pub topology: PrivateRunTopology,
    pub authored_files: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    #[error("{surface} at '{path}' could not be read: {source}")]
    Read {
        surface: &'static str,
        path: String,
        source: std::io::Error,
    },
    #[error("{surface} at '{path}' is malformed: {reason}")]
    Malformed {
        surface: &'static str,
        path: String,
        reason: String,
    },
    #[error("terminal topology contract failed closed: {0}")]
    Contract(String),
    #[error("terminal topology authoring conflict: {0}")]
    Conflict(String),
    #[error(transparent)]
    Daemon(#[from] crate::daemon_arrival::DaemonArrivalError),
    #[error(transparent)]
    Outbound(#[from] crate::read_outbound::TerminalOutboundError),
    #[error(transparent)]
    Input(#[from] crate::emit_input::TerminalInputError),
    #[error("terminal topology authoring timed out after {timeout_ms}ms waiting for {waiting_for}")]
    Timeout {
        timeout_ms: u64,
        waiting_for: String,
    },
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostManifest {
    host_manifest_version: u32,
    root_refs: BTreeMap<String, PathBuf>,
}

#[derive(Clone)]
struct RootCatalog {
    terminal: InterfaceDescriptor,
    shell_terminal: InterfaceDescriptor,
    provider: InterfaceDescriptor,
    interface_authoring: InterfaceDescriptor,
    authoring_environment: InterfaceDescriptor,
    capabilities: BTreeMap<String, CapabilityCatalog>,
    operation_capabilities: BTreeMap<String, OperationCapabilityCatalog>,
    shell_environment: InterfaceDescriptor,
    worker: InterfaceDescriptor,
    worker_policy: WorkerInterfacePolicy,
    mandatory_role_artifacts: Vec<ArtifactId>,
    ee_template: EeTemplateDescriptor,
    parent_ee_template: EeTemplateDescriptor,
}

#[derive(Clone)]
struct CapabilityCatalog {
    template: InterfaceDescriptor,
    tools: Vec<String>,
    tool_worker_environment_kinds: BTreeMap<String, WorkerEnvironmentKindId>,
    worker_environment_kinds: Vec<octopus_descriptors::WorkerEnvironmentKindId>,
    sidecars: BTreeMap<String, String>,
}

#[derive(Clone)]
struct OperationCapabilityCatalog {
    template: InterfaceDescriptor,
    catalog_dir: String,
    catalog_label: String,
    request_label: String,
    operations: Vec<FormedInterfaceOperation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthoredCheckpointTemplate {
    name: String,
    role: AuthoredCheckpointRole,
    interface_binding: AuthoredInterfaceBinding,
    outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    include_context: bool,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

impl AuthoredCheckpointTemplate {
    fn interface(&self) -> &InterfaceId {
        &self.interface_binding.interface
    }

    fn outbound_interfaces(&self) -> impl Iterator<Item = &InterfaceId> {
        self.outbound_interface_bindings
            .iter()
            .map(|binding| &binding.interface)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRunAuthoringProfile {
    file_write_tool: String,
    file_write_label: String,
    file_write_result_label: String,
    result_timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AuthoredCheckpointRole {
    Stable,
    ProviderClientOutput,
    ProviderToolOutput,
    ProviderInterfaceOutput,
    AuthoringCall,
    AuthoringResult,
}

#[derive(Clone)]
struct PlannedWrite {
    path: PathBuf,
    content: String,
    before_hash: Option<String>,
    after_hash: String,
    phase: PlannedWritePhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PlannedWritePhase {
    ArtifactDescriptor,
    InterfaceCompanion,
    InterfaceDescriptor,
    ToolDescriptor,
    SharedConsent,
    EeInstance,
    RunRecord,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellResultBody {
    result: String,
    output: ShellResultOutput,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellResultOutput {
    exit_code: i64,
    stdout: String,
    stderr: String,
}

impl TerminalClientDescriptor {
    pub fn load(registry_root: &Path) -> Result<Self, TopologyError> {
        require_absolute("registry root", registry_root)?;
        let path = registry_root
            .join("interface")
            .join("world_terminal")
            .join("client.json");
        let bytes = read_bytes("terminal client descriptor", &path)?;
        let descriptor: Self =
            serde_json::from_slice(&bytes).map_err(|error| TopologyError::Malformed {
                surface: "terminal client descriptor",
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        descriptor.validate()?;
        Ok(descriptor)
    }

    fn validate(&self) -> Result<(), TopologyError> {
        if self.schema_version != 7 {
            return Err(contract(format!(
                "client descriptor schema_version is {}, expected 7",
                self.schema_version
            )));
        }
        for (name, value) in [
            ("interface", self.interface.as_str()),
            (
                "authoring.room_interface",
                self.authoring.room_interface.as_str(),
            ),
            ("authoring.tool", self.authoring.tool.as_str()),
            ("shell.tool", self.shell.tool.as_str()),
            ("shell.result_label", self.shell.result_label.as_str()),
            (
                "private_run.ee_template",
                self.private_run.ee_template.as_str(),
            ),
            ("private_run.parent_ee", self.private_run.parent_ee.as_str()),
            (
                "private_run.parent_ee_template",
                self.private_run.parent_ee_template.as_str(),
            ),
            (
                "private_run.activation_driver_ref",
                self.private_run.activation_driver_ref.as_str(),
            ),
            (
                "private_run.tool_call_mark",
                self.private_run.tool_call_mark.as_str(),
            ),
            (
                "private_run.tool_call_verifier",
                self.private_run.tool_call_verifier.as_str(),
            ),
        ] {
            require_non_empty(name, value)?;
        }
        for (name, value) in [
            (
                "terminal",
                self.private_run.interface_templates.terminal.as_str(),
            ),
            (
                "shell_terminal",
                self.private_run.interface_templates.shell_terminal.as_str(),
            ),
            (
                "provider",
                self.private_run.interface_templates.provider.as_str(),
            ),
            (
                "interface_authoring",
                self.private_run
                    .interface_templates
                    .interface_authoring
                    .as_str(),
            ),
            (
                "authoring_environment",
                self.private_run
                    .interface_templates
                    .authoring_environment
                    .as_str(),
            ),
            (
                "shell_environment",
                self.private_run
                    .interface_templates
                    .shell_environment
                    .as_str(),
            ),
            (
                "worker",
                self.private_run.interface_templates.worker.as_str(),
            ),
        ] {
            require_non_empty(&format!("private_run.interface_templates.{name}"), value)?;
        }
        for (name, value) in [
            ("run", self.private_run.id_prefixes.run.as_str()),
            ("ee", self.private_run.id_prefixes.ee.as_str()),
            ("terminal", self.private_run.id_prefixes.terminal.as_str()),
            (
                "shell_terminal",
                self.private_run.id_prefixes.shell_terminal.as_str(),
            ),
            ("provider", self.private_run.id_prefixes.provider.as_str()),
            (
                "interface_authoring",
                self.private_run.id_prefixes.interface_authoring.as_str(),
            ),
            (
                "authoring_environment",
                self.private_run.id_prefixes.authoring_environment.as_str(),
            ),
            (
                "shell_environment",
                self.private_run.id_prefixes.shell_environment.as_str(),
            ),
            ("worker", self.private_run.id_prefixes.worker.as_str()),
        ] {
            require_id_fragment(&format!("private_run.id_prefixes.{name}"), value)?;
        }
        require_env_prefix(
            "private_run.terminal_listen_env_prefix",
            &self.private_run.terminal_listen_env_prefix,
        )?;
        require_env_prefix(
            "private_run.shell_terminal_listen_env_prefix",
            &self.private_run.shell_terminal_listen_env_prefix,
        )?;
        require_env_prefix(
            "private_run.interface_authoring_listen_env_prefix",
            &self.private_run.interface_authoring_listen_env_prefix,
        )?;
        let mut hidden_tools = BTreeSet::new();
        for tool in &self.private_run.model_hidden_tools {
            require_non_empty("private_run.model_hidden_tools[]", tool)?;
            if tool.trim() != tool {
                return Err(contract(
                    "private_run.model_hidden_tools entries must be trimmed",
                ));
            }
            if !hidden_tools.insert(tool) {
                return Err(contract(format!(
                    "private_run.model_hidden_tools repeats '{tool}'"
                )));
            }
        }
        if self.authoring.result_timeout_ms == 0 {
            return Err(contract("authoring result_timeout_ms must be positive"));
        }
        if self.shell.result_timeout_ms == 0 {
            return Err(contract("shell result_timeout_ms must be positive"));
        }
        let mut labels = BTreeSet::new();
        for (name, value) in [
            (
                "operator_input",
                self.artifact_labels.operator_input.as_str(),
            ),
            ("turn_control", self.artifact_labels.turn_control.as_str()),
            ("tool_call", self.artifact_labels.tool_call.as_str()),
            (
                "file_write_result",
                self.artifact_labels.file_write_result.as_str(),
            ),
            (
                "terminal_output",
                self.artifact_labels.terminal_output.as_str(),
            ),
            ("error_output", self.artifact_labels.error_output.as_str()),
        ] {
            require_non_empty(&format!("artifact_labels.{name}"), value)?;
            if !labels.insert(value) {
                return Err(contract(format!(
                    "artifact_labels.{name} duplicates label '{value}'"
                )));
            }
        }
        for value in &self.artifact_labels.stream_outputs {
            require_non_empty("artifact_labels.stream_outputs[]", value)?;
            if !labels.insert(value) {
                return Err(contract(format!(
                    "artifact_labels.stream_outputs repeats label '{value}'"
                )));
            }
        }
        if !labels.insert(self.shell.result_label.as_str()) {
            return Err(contract(format!(
                "shell.result_label duplicates label '{}'",
                self.shell.result_label
            )));
        }
        Ok(())
    }

    pub fn is_private_run(&self, run: &RunId) -> bool {
        run.as_str()
            .strip_prefix(&self.private_run.id_prefixes.run)
            .is_some_and(|suffix| !suffix.is_empty())
    }
}

impl TerminalHostRoots {
    pub fn load_from_home(octopus_home: &Path) -> Result<Self, TopologyError> {
        require_absolute("OCTOPUS_HOME", octopus_home)?;
        let path = octopus_home.join("host.json");
        let bytes = read_bytes("host manifest", &path)?;
        let manifest: HostManifest =
            serde_json::from_slice(&bytes).map_err(|error| TopologyError::Malformed {
                surface: "host manifest",
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        if manifest.host_manifest_version != 1 {
            return Err(contract(format!(
                "host manifest version is {}, expected 1",
                manifest.host_manifest_version
            )));
        }
        let mut root_slots = BTreeMap::new();
        for (slot, root) in &manifest.root_refs {
            require_absolute(&format!("host root slot '{slot}'"), root)?;
            let canonical = canonicalize_declared_root(root).map_err(|reason| {
                contract(format!(
                    "host manifest root slot '{slot}' path '{}' cannot be resolved: {reason}",
                    root.display()
                ))
            })?;
            root_slots.insert(slot.clone(), canonical);
        }
        let repo_root = required_root(&root_slots, "repo_root")?;
        let worktree_root = required_root(&root_slots, "worktree_root")?;
        let registry_path = octopus_home.join("registry");
        let registry_root =
            std::fs::canonicalize(&registry_path).map_err(|source| TopologyError::Read {
                surface: "active registry root",
                path: registry_path.display().to_string(),
                source,
            })?;
        let run_records_root = required_root(&root_slots, "run_records_root")?;
        let ee_instances_root = required_root(&root_slots, "ee_instances_root")?;
        Ok(Self {
            repo_root,
            worktree_root,
            root_slots,
            registry_artifact_root: registry_root.join("artifact"),
            registry_interface_root: registry_root.join("interface"),
            registry_root,
            run_records_root,
            ee_instances_root,
            durable_ledger_root: octopus_home.join("ledger"),
        })
    }

    pub fn load(octopus_home: &Path, registry_root: &Path) -> Result<Self, TopologyError> {
        require_absolute("registry root", registry_root)?;
        let roots = Self::load_from_home(octopus_home)?;
        let requested =
            std::fs::canonicalize(registry_root).map_err(|source| TopologyError::Read {
                surface: "active registry root",
                path: registry_root.display().to_string(),
                source,
            })?;
        if roots.registry_root != requested {
            return Err(contract(format!(
                "registry root '{}' does not match OCTOPUS_HOME active registry '{}'",
                requested.display(),
                roots.registry_root.display()
            )));
        }
        Ok(roots)
    }
}

pub async fn author_private_run(
    octopus_home: &Path,
    registry_root: &Path,
    request: PrivateRunRequest,
) -> Result<AuthoredPrivateRun, TopologyError> {
    let descriptor = TerminalClientDescriptor::load(registry_root)?;
    let roots = TerminalHostRoots::load(octopus_home, registry_root)?;
    let opaque_id = uuid::Uuid::new_v4().simple().to_string();
    let mut conflict_attempts = 0_u8;
    let (topology, authored_files) = loop {
        let catalog = load_root_catalog(registry_root, &descriptor, &request)?;
        let planned = plan_private_run(
            octopus_home,
            &roots,
            &descriptor,
            &catalog,
            request.clone(),
            opaque_id.clone(),
        );
        let (topology, writes) = match planned {
            Ok(planned) => planned,
            Err(TopologyError::Conflict(_)) if conflict_attempts < 7 => {
                conflict_attempts += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        match commit_client_writes(&writes) {
            Ok(()) => break (topology, writes.len()),
            Err(TopologyError::Conflict(_)) if conflict_attempts < 7 => {
                conflict_attempts += 1;
            }
            Err(error) => return Err(error),
        }
    };
    wait_for_socket(
        &topology.terminal_socket,
        descriptor.authoring.result_timeout_ms,
    )
    .await?;
    wait_for_socket(
        &topology.shell_terminal_socket,
        descriptor.authoring.result_timeout_ms,
    )
    .await?;
    wait_for_socket(
        &topology.interface_authoring_socket,
        descriptor.authoring.result_timeout_ms,
    )
    .await?;
    Ok(AuthoredPrivateRun {
        topology,
        authored_files,
    })
}

pub async fn await_private_run_interfaces(
    registry_root: &Path,
    topology: &PrivateRunTopology,
) -> Result<(), TopologyError> {
    let descriptor = TerminalClientDescriptor::load(registry_root)?;
    wait_for_socket(
        &topology.terminal_socket,
        descriptor.authoring.result_timeout_ms,
    )
    .await?;
    wait_for_socket(
        &topology.shell_terminal_socket,
        descriptor.authoring.result_timeout_ms,
    )
    .await?;
    wait_for_socket(
        &topology.interface_authoring_socket,
        descriptor.authoring.result_timeout_ms,
    )
    .await
}

pub async fn await_private_terminal_reply(
    octopus_home: &Path,
    registry_root: &Path,
    durable_root: &Path,
    topology: &PrivateRunTopology,
    after_seq: u64,
    reader_name: &str,
) -> Result<ArtifactId, TopologyError> {
    require_absolute("terminal durable ledger root", durable_root)?;
    require_non_empty("terminal reply reader name", reader_name)?;
    let descriptor = TerminalClientDescriptor::load(registry_root)?;
    let host_io_limits =
        load_interface_host_io_limits(registry_root, &topology.terminal_interface)?;
    let mut reader = TerminalOutboundReader::open_named(
        topology.terminal_interface.clone(),
        octopus_home.to_path_buf(),
        durable_root.to_path_buf(),
        reader_name,
        host_io_limits,
    )?;
    let timeout = Duration::from_millis(descriptor.authoring.result_timeout_ms);
    let deadline = Instant::now() + timeout;
    loop {
        let movements = reader.read_new()?;
        let mut terminal_reply = None;
        for movement in &movements {
            if movement.movement.seq.0 <= after_seq {
                continue;
            }
            let envelope = reader.envelope(movement)?;
            let label = envelope
                .artifact
                .id
                .as_str()
                .split_once(':')
                .map_or(envelope.artifact.id.as_str(), |(label, _)| label);
            if label == descriptor.artifact_labels.error_output {
                let payload = envelope.read_artifact_payload().map_err(|error| {
                    contract(format!(
                        "terminal error artifact could not be read: {error}"
                    ))
                })?;
                return Err(contract(format!(
                    "terminal provider returned an error while producing a compact summary: {}",
                    String::from_utf8_lossy(&payload)
                )));
            }
            if label == descriptor.artifact_labels.terminal_output {
                terminal_reply = Some(envelope.artifact.id.clone());
            }
        }
        if !movements.is_empty() {
            reader.commit()?;
        }
        if let Some(artifact) = terminal_reply {
            return Ok(artifact);
        }
        if Instant::now() >= deadline {
            return Err(TopologyError::Timeout {
                timeout_ms: descriptor.authoring.result_timeout_ms,
                waiting_for: format!(
                    "terminal output '{}' after movement {}",
                    descriptor.artifact_labels.terminal_output, after_seq
                ),
            });
        }
        wait_for_reader_change(
            &mut reader,
            deadline,
            descriptor.authoring.result_timeout_ms,
            format!(
                "terminal output '{}' after movement {}",
                descriptor.artifact_labels.terminal_output, after_seq
            ),
        )
        .await?;
    }
}

pub async fn execute_private_shell(
    octopus_home: &Path,
    registry_root: &Path,
    durable_root: &Path,
    artifact_root: &Path,
    topology: &PrivateRunTopology,
    command: String,
) -> Result<PrivateShellExecution, TopologyError> {
    require_absolute("terminal durable ledger root", durable_root)?;
    require_absolute("terminal shell artifact root", artifact_root)?;
    require_non_empty("terminal shell command", &command)?;
    let descriptor = TerminalClientDescriptor::load(registry_root)?;
    let roots = TerminalHostRoots::load(octopus_home, registry_root)?;
    let loaded = load_private_run_topology(
        octopus_home,
        registry_root,
        &roots.run_records_root,
        &topology.run,
    )?;
    if loaded != *topology {
        return Err(contract(format!(
            "private shell topology for run '{}' changed before command execution",
            topology.run.as_str()
        )));
    }
    wait_for_socket(
        &topology.shell_terminal_socket,
        descriptor.shell.result_timeout_ms,
    )
    .await?;
    let host_io_limits =
        load_interface_host_io_limits(registry_root, &topology.shell_terminal_interface)?;
    let mut reader = TerminalOutboundReader::open_named(
        topology.shell_terminal_interface.clone(),
        octopus_home.to_path_buf(),
        durable_root.to_path_buf(),
        &format!("shell_{}", topology.opaque_id),
        host_io_limits,
    )?;
    let historical = reader.read_new()?;
    if !historical.is_empty() {
        reader.commit()?;
    }
    let shell_tool = exact_tool(&topology.exact_tools, &descriptor.shell.tool)?;
    let shell_result_label = exact_shell_result_label(&descriptor, &shell_tool)?;
    let body = InterfaceInputArtifactBody::from_json(serde_json::json!({
        "tool": shell_tool,
        "arguments": { "command": command }
    }))
    .map_err(|error| contract(format!("shell tool.call body is invalid: {error}")))?;
    let artifact =
        materialize_input_artifact(artifact_root, &descriptor.artifact_labels.tool_call, &body)?;
    let emitter = TerminalDaemonEmitter::with_socket_path(topology.shell_terminal_socket.clone());
    let started = Instant::now();
    let verdict = emitter
        .emit_input_manifest(&artifact.manifest_bytes)
        .await?;
    let admitted_seq = match verdict {
        DeliveryVerdict::Admitted { seq } => seq,
        DeliveryVerdict::Refused { .. } => {
            return Err(contract(
                "shell emitter returned a refused verdict without raising DaemonArrivalError",
            ));
        }
    };
    let (result_seq, result_artifact, exit_code, stdout, stderr, tool_error) =
        wait_for_shell_result(
            &mut reader,
            admitted_seq,
            &descriptor,
            topology,
            &shell_tool,
            &shell_result_label,
        )
        .await?;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let rearm_error = rearm_private_shell(&descriptor, &roots, topology)
        .await
        .err()
        .map(|error| format!("shell checkpoint rearm failed: {error}"));
    let error = match (tool_error, rearm_error) {
        (Some(tool), Some(rearm)) => Some(format!("{tool}\n{rearm}")),
        (Some(tool), None) => Some(tool),
        (None, Some(rearm)) => Some(rearm),
        (None, None) => None,
    };
    Ok(PrivateShellExecution {
        command,
        worktree: roots
            .worktree_root
            .join(worker_environment_segment(topology.run.as_str())),
        exit_code,
        stdout,
        stderr,
        error,
        duration_ms,
        admitted_seq,
        result_seq,
        result_artifact,
    })
}

pub async fn configure_private_model_interface(
    octopus_home: &Path,
    registry_root: &Path,
    topology: &PrivateRunTopology,
    model: String,
    reasoning_effort: String,
) -> Result<PrivateRunTopology, TopologyError> {
    require_non_empty("private model interface model", &model)?;
    require_non_empty(
        "private model interface reasoning effort",
        &reasoning_effort,
    )?;
    let roots = TerminalHostRoots::load(octopus_home, registry_root)?;
    let loaded = load_private_run_topology(
        octopus_home,
        registry_root,
        &roots.run_records_root,
        &topology.run,
    )?;
    if loaded != *topology {
        return Err(contract(format!(
            "private model topology for run '{}' changed before interface configuration",
            topology.run.as_str()
        )));
    }
    let writes = plan_private_model_selection(&roots, topology, &model, &reasoning_effort)?;
    commit_client_writes(&writes)?;
    let updated = load_private_run_topology(
        octopus_home,
        registry_root,
        &roots.run_records_root,
        &topology.run,
    )?;
    let mut expected = topology.clone();
    expected.model = model;
    expected.reasoning_effort = reasoning_effort;
    if updated != expected {
        return Err(contract(format!(
            "private model interface configuration changed topology beyond model metadata for run '{}'",
            topology.run.as_str()
        )));
    }
    Ok(updated)
}

async fn wait_for_shell_result(
    reader: &mut TerminalOutboundReader,
    admitted_seq: u64,
    descriptor: &TerminalClientDescriptor,
    topology: &PrivateRunTopology,
    shell_tool: &str,
    shell_result_label: &str,
) -> Result<(u64, ArtifactId, Option<i64>, String, String, Option<String>), TopologyError> {
    let timeout_ms = descriptor.shell.result_timeout_ms;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let movements = reader.read_new()?;
        let mut matched = None;
        for movement in &movements {
            if movement.movement.seq.0 <= admitted_seq {
                continue;
            }
            let metadata = movement.metadata().map_err(|error| {
                contract(format!(
                    "shell result movement seq={} metadata is malformed: {error}",
                    movement.movement.seq.0
                ))
            })?;
            let runtime = metadata.runtime.as_ref().ok_or_else(|| {
                contract(format!(
                    "shell result movement seq={} has no runtime coordinates",
                    movement.movement.seq.0
                ))
            })?;
            if runtime.run != topology.run || runtime.interface != topology.shell_terminal_interface
            {
                return Err(contract(format!(
                    "shell result movement seq={} belongs to run='{}' interface='{}', expected run='{}' interface='{}'",
                    movement.movement.seq.0,
                    runtime.run.as_str(),
                    runtime.interface.as_str(),
                    topology.run.as_str(),
                    topology.shell_terminal_interface.as_str()
                )));
            }
            let envelope = reader.envelope(movement)?;
            let manifest = envelope.read_artifact_manifest().map_err(|error| {
                contract(format!(
                    "shell result movement seq={} manifest could not be read: {error}",
                    movement.movement.seq.0
                ))
            })?;
            if manifest.label != shell_result_label {
                return Err(contract(format!(
                    "shell interface '{}' emitted unexpected artifact label '{}' after command admission; expected '{}'",
                    topology.shell_terminal_interface.as_str(),
                    manifest.label,
                    shell_result_label
                )));
            }
            if matched.is_some() {
                return Err(contract(format!(
                    "shell command admitted at movement {} produced multiple result artifacts",
                    admitted_seq
                )));
            }
            let payload = envelope.read_artifact_payload().map_err(|error| {
                contract(format!(
                    "shell result movement seq={} payload could not be read: {error}",
                    movement.movement.seq.0
                ))
            })?;
            let result: ToolResultArtifact = serde_json::from_slice(&payload).map_err(|error| {
                contract(format!(
                    "shell result movement seq={} is malformed: {error}",
                    movement.movement.seq.0
                ))
            })?;
            let (exit_code, stdout, stderr, error) =
                decode_shell_result(&result, shell_tool, shell_result_label)?;
            matched = Some((
                movement.movement.seq.0,
                envelope.artifact.id.clone(),
                exit_code,
                stdout,
                stderr,
                error,
            ));
        }
        if !movements.is_empty() {
            reader.commit()?;
        }
        if let Some(result) = matched {
            return Ok(result);
        }
        if Instant::now() >= deadline {
            return Err(TopologyError::Timeout {
                timeout_ms,
                waiting_for: format!(
                    "shell result '{}' after movement {}",
                    shell_result_label, admitted_seq
                ),
            });
        }
        wait_for_reader_change(
            reader,
            deadline,
            timeout_ms,
            format!(
                "shell result '{}' after movement {}",
                shell_result_label, admitted_seq
            ),
        )
        .await?;
    }
}

fn decode_shell_result(
    result: &ToolResultArtifact,
    shell_tool: &str,
    shell_result_label: &str,
) -> Result<(Option<i64>, String, String, Option<String>), TopologyError> {
    if result.label != shell_result_label || result.tool != shell_tool {
        return Err(contract(format!(
            "shell result names label='{}' tool='{}', expected label='{}' tool='{}'",
            result.label, result.tool, shell_result_label, shell_tool
        )));
    }
    if result.ok {
        if result.error.is_some() {
            return Err(contract("successful shell result carries an error"));
        }
        let body: ShellResultBody = serde_json::from_value(
            result
                .body
                .clone()
                .ok_or_else(|| contract("successful shell result has no body"))?,
        )
        .map_err(|error| {
            contract(format!(
                "successful shell result body is malformed: {error}"
            ))
        })?;
        if body.result != "command_finished" {
            return Err(contract(format!(
                "successful shell result body names result '{}', expected 'command_finished'",
                body.result
            )));
        }
        return Ok((
            Some(body.output.exit_code),
            body.output.stdout,
            body.output.stderr,
            None,
        ));
    }
    if result.body.is_some() {
        return Err(contract("failed shell result carries a success body"));
    }
    let error = result
        .error
        .as_ref()
        .ok_or_else(|| contract("failed shell result has no error"))?;
    let error = serde_json::to_string(error)
        .map_err(|cause| contract(format!("failed shell result error did not encode: {cause}")))?;
    Ok((None, String::new(), String::new(), Some(error)))
}

async fn rearm_private_shell(
    descriptor: &TerminalClientDescriptor,
    roots: &TerminalHostRoots,
    topology: &PrivateRunTopology,
) -> Result<(), TopologyError> {
    let write = plan_private_shell_rearm(roots, descriptor, topology)?;
    commit_client_writes(&[write])
}

fn plan_private_shell_rearm(
    roots: &TerminalHostRoots,
    descriptor: &TerminalClientDescriptor,
    topology: &PrivateRunTopology,
) -> Result<PlannedWrite, TopologyError> {
    let path = roots
        .run_records_root
        .join(format!("{}.json", topology.run.as_str()));
    let current = read_bytes("private shell run", &path)?;
    let mut record = RunRecord::from_bytes(&current).map_err(|error| TopologyError::Malformed {
        surface: "private shell run",
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    if record.id != topology.run
        || record.ee != topology.ee
        || record.worker != topology.worker_interface
    {
        return Err(contract(format!(
            "private shell run topology drifted: run='{}' ee='{}' worker='{}'",
            record.id.as_str(),
            record.ee.as_str(),
            record.worker.as_str()
        )));
    }
    let mandatory_role_bindings = artifact_binding_values(&live_mandatory_role_artifacts(
        roots,
        &record.ee,
        &record.worker,
    )?);
    let state = record
        .interface_state
        .entry(topology.shell_terminal_interface.clone())
        .or_default();
    let previous_generation = match state.get("shell_generation") {
        None => 0,
        Some(value) => value.as_u64().ok_or_else(|| {
            contract(format!(
                "private shell run '{}' interface_state.shell_generation is not an unsigned integer",
                topology.run.as_str()
            ))
        })?,
    };
    let generation = previous_generation
        .checked_add(1)
        .ok_or_else(|| contract("private shell generation overflowed"))?;
    let shell_tool = exact_tool(&topology.exact_tools, &descriptor.shell.tool)?;
    state.insert(
        "shell_generation".to_string(),
        serde_json::json!(generation),
    );
    let mut removed = BTreeSet::new();
    let mut pair_counts = [0_u32; 2];
    for checkpoint in &record.checkpoints {
        let pair = if checkpoint_pair(
            checkpoint,
            &topology.shell_terminal_interface,
            &topology.shell_environment_interface,
        ) {
            Some(0)
        } else if checkpoint_pair(
            checkpoint,
            &topology.shell_environment_interface,
            &topology.shell_terminal_interface,
        ) {
            Some(1)
        } else {
            None
        };
        if let Some(index) = pair {
            pair_counts[index] += 1;
            removed.insert(checkpoint.id.as_str().to_string());
        }
    }
    if pair_counts != [1, 1] {
        return Err(contract(format!(
            "private shell topology expected one shell call and one shell result checkpoint; observed {:?}",
            pair_counts
        )));
    }
    let mut checkpoints = record
        .checkpoints
        .iter()
        .filter(|checkpoint| !removed.contains(checkpoint.id.as_str()))
        .map(|checkpoint| {
            serde_json::to_value(checkpoint).map_err(|error| {
                contract(format!(
                    "private shell retained checkpoint '{}' did not encode: {error}",
                    checkpoint.id.as_str()
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let suffix = &topology.opaque_id[..12];
    let shell_call = format!("cp_shell_call_{suffix}_{generation}");
    let shell_result = format!("cp_shell_result_{suffix}_{generation}");
    checkpoints.extend([
        serde_json::json!({
            "id": shell_call,
            "run": record.id,
            "trace": format!("trace:{}:{shell_call}", record.id.as_str()),
            "epoch": 0,
            "interface_binding": all_binding(&topology.shell_terminal_interface),
            "outbound_interface_bindings": [all_binding(&topology.shell_environment_interface)],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": [{
                "mark": descriptor.private_run.tool_call_mark,
                "artifact_label": descriptor.artifact_labels.tool_call,
                "verifier": descriptor.private_run.tool_call_verifier,
                "params": { "tools": [shell_tool] }
            }]
        }),
        serde_json::json!({
            "id": shell_result,
            "run": record.id,
            "trace": format!("trace:{}:{shell_result}", record.id.as_str()),
            "epoch": 0,
            "interface_binding": all_binding(&topology.shell_environment_interface),
            "outbound_interface_bindings": [all_binding(&topology.shell_terminal_interface)],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": []
        }),
    ]);
    let mut active_checkpoints = record
        .active_checkpoints
        .iter()
        .filter(|id| !removed.contains(id.as_str()))
        .map(|id| serde_json::json!(id))
        .collect::<Vec<_>>();
    active_checkpoints.extend([
        serde_json::json!(shell_call),
        serde_json::json!(shell_result),
    ]);
    let content = pretty_json(
        &serde_json::json!({
            "id": record.id,
            "ee": record.ee,
            "worker": record.worker,
            "trace": record.trace,
            "status": record.status,
            "interface_state": record.interface_state,
            "checkpoints": checkpoints,
            "active_checkpoints": active_checkpoints
        }),
        "rearmed private shell run",
    )?;
    RunRecord::from_bytes(content.as_bytes()).map_err(|error| {
        contract(format!(
            "rearmed private shell run failed validation: {error}"
        ))
    })?;
    let before_hash = ContentHash::of_bytes(&current).to_hex();
    let write = PlannedWrite {
        path,
        after_hash: ContentHash::of_bytes(content.as_bytes()).to_hex(),
        content,
        before_hash: Some(before_hash),
        phase: PlannedWritePhase::RunRecord,
    };
    Ok(write)
}

fn checkpoint_pair(
    checkpoint: &octopus_run_record::CheckpointRecord,
    inbound: &InterfaceId,
    outbound: &InterfaceId,
) -> bool {
    checkpoint.inbound_interface() == Some(inbound)
        && checkpoint.outbound_interface_bindings.len() == 1
        && checkpoint.outbound_interfaces().next() == Some(outbound)
}

fn plan_private_model_selection(
    roots: &TerminalHostRoots,
    topology: &PrivateRunTopology,
    model: &str,
    reasoning_effort: &str,
) -> Result<Vec<PlannedWrite>, TopologyError> {
    let provider_path = roots
        .registry_interface_root
        .join(topology.provider_interface.as_str())
        .join("interface.json");
    let provider_bytes = read_bytes("private provider interface", &provider_path)?;
    let mut provider = octopus_descriptors::parse_strict::<InterfaceDescriptor>(&provider_bytes)
        .map_err(|error| TopologyError::Malformed {
            surface: "private provider interface",
            path: provider_path.display().to_string(),
            reason: error.to_string(),
        })?;
    if provider.id != topology.provider_interface {
        return Err(contract(format!(
            "private provider path '{}' does not describe topology interface '{}'",
            provider_path.display(),
            topology.provider_interface.as_str()
        )));
    }
    let provider_config = provider.interface_driver_config.as_ref().ok_or_else(|| {
        contract(format!(
            "private provider interface '{}' carries no driver config",
            provider.id.as_str()
        ))
    })?;
    let formed_models = load_live_formed_provider_models(
        &roots.registry_root,
        &roots.ee_instances_root,
        &topology.ee,
        &topology.worker_interface,
        &provider,
    )?;
    validate_provider_model_selection(
        provider_config,
        &provider.id,
        model,
        reasoning_effort,
        &formed_models,
    )?;
    let provider_config = provider.interface_driver_config.as_mut().ok_or_else(|| {
        contract(format!(
            "private provider interface '{}' carries no instance driver config",
            provider.id.as_str()
        ))
    })?;
    provider_config
        .private_config
        .insert("model".to_string(), serde_json::json!(model));
    provider_config.private_config.insert(
        "reasoning_effort".to_string(),
        serde_json::json!(reasoning_effort),
    );
    let provider_content = pretty_typed(&provider, "configured private provider interface")?;
    let configured =
        octopus_descriptors::parse_strict::<InterfaceDescriptor>(provider_content.as_bytes())
            .map_err(|error| {
                contract(format!(
                    "configured private provider interface failed validation: {error}"
                ))
            })?;
    if configured.id != topology.provider_interface || !configured.has_interface_driver() {
        return Err(contract(format!(
            "configured private provider interface '{}' lost its exact identity or driver",
            topology.provider_interface.as_str()
        )));
    }
    let provider_write = PlannedWrite {
        path: provider_path,
        before_hash: Some(ContentHash::of_bytes(&provider_bytes).to_hex()),
        after_hash: ContentHash::of_bytes(provider_content.as_bytes()).to_hex(),
        content: provider_content,
        phase: PlannedWritePhase::InterfaceDescriptor,
    };
    Ok(vec![provider_write])
}

pub fn default_private_run_request(
    registry_root: &Path,
) -> Result<PrivateRunRequest, TopologyError> {
    let descriptor = TerminalClientDescriptor::load(registry_root)?;
    let provider = load_template_interface(
        registry_root,
        &descriptor.private_run.interface_templates.provider,
    )?;
    let provider_config = provider.interface_driver_config.as_ref().ok_or_else(|| {
        contract(format!(
            "provider template '{}' has no interface_driver_config",
            provider.id.as_str()
        ))
    })?;
    let model = metadata_string(provider_config, "model", &provider.id)?;
    let reasoning_effort = configured_reasoning_effort(provider_config, &provider.id)?;
    let ee_template = load_ee_template(registry_root, &descriptor.private_run.ee_template)?;
    let request = PrivateRunRequest::new(model, reasoning_effort, ee_template.goal_class);
    let catalog = load_root_catalog(registry_root, &descriptor, &request)?;
    validate_private_run_model_selection(&catalog, &request)?;
    Ok(request)
}

pub fn fork_private_run_request(
    octopus_home: &Path,
    registry_root: &Path,
    run_records_root: &Path,
    source_run: &RunId,
) -> Result<PrivateRunRequest, TopologyError> {
    let topology =
        load_private_run_topology(octopus_home, registry_root, run_records_root, source_run)?;
    let run_path = run_records_root.join(format!("{}.json", source_run.as_str()));
    let run_bytes = read_bytes("fork source run", &run_path)?;
    let record = RunRecord::from_bytes(&run_bytes).map_err(|error| TopologyError::Malformed {
        surface: "fork source run",
        path: run_path.display().to_string(),
        reason: error.to_string(),
    })?;
    let active = record
        .active_checkpoints
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let seed_artifacts = record
        .checkpoints
        .iter()
        .filter(|checkpoint| active.contains(&checkpoint.id))
        .flat_map(|checkpoint| checkpoint.artifact_bindings.iter())
        .map(|binding| binding.id.as_str().to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let roots = TerminalHostRoots::load(octopus_home, registry_root)?;
    let ee_path = roots
        .ee_instances_root
        .join(format!("{}.json", topology.ee.as_str()));
    let ee_bytes = read_bytes("fork source EE", &ee_path)?;
    let ee: EeInstance =
        serde_json::from_slice(&ee_bytes).map_err(|error| TopologyError::Malformed {
            surface: "fork source EE",
            path: ee_path.display().to_string(),
            reason: error.to_string(),
        })?;
    if ee.ee_id != topology.ee {
        return Err(contract(format!(
            "fork source EE file '{}' declares id '{}'",
            ee_path.display(),
            ee.ee_id.as_str()
        )));
    }
    let door_host_slots = source_door_host_slots(registry_root, &topology)?;
    let request = PrivateRunRequest::new(topology.model, topology.reasoning_effort, ee.goal)
        .with_seed_artifacts(seed_artifacts)
        .with_existing_ee(ee.ee_id)
        .with_door_host_slots(door_host_slots);
    let descriptor = TerminalClientDescriptor::load(registry_root)?;
    let catalog = load_root_catalog(registry_root, &descriptor, &request)?;
    validate_private_run_model_selection(&catalog, &request)?;
    Ok(request)
}

fn source_door_host_slots(
    registry_root: &Path,
    topology: &PrivateRunTopology,
) -> Result<BTreeMap<String, BTreeMap<String, PathBuf>>, TopologyError> {
    let mut slots = BTreeMap::new();
    for (template, interface) in topology
        .worker_environment_interfaces
        .iter()
        .chain(&topology.tool_interfaces)
    {
        let descriptor = load_template_interface(registry_root, interface.as_str())?;
        let Some(config) = descriptor.interface_driver_config else {
            continue;
        };
        let values = config
            .host_slots
            .into_iter()
            .map(|(slot, value)| (slot, value.path))
            .collect::<BTreeMap<_, _>>();
        if !values.is_empty() {
            slots.insert(template.clone(), values);
        }
    }
    Ok(slots)
}

pub fn load_private_run_topology(
    octopus_home: &Path,
    registry_root: &Path,
    run_records_root: &Path,
    run: &RunId,
) -> Result<PrivateRunTopology, TopologyError> {
    let descriptor = TerminalClientDescriptor::load(registry_root)?;
    require_absolute("run records root", run_records_root)?;
    let path = run_records_root.join(format!("{}.json", run.as_str()));
    let bytes = read_bytes("private terminal run", &path)?;
    let record = RunRecord::from_bytes(&bytes).map_err(|error| TopologyError::Malformed {
        surface: "private terminal run",
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    if record.id != *run {
        return Err(contract(format!(
            "run file '{}' declares id '{}'",
            path.display(),
            record.id.as_str()
        )));
    }

    let opaque_id = run
        .as_str()
        .strip_prefix(&descriptor.private_run.id_prefixes.run)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            contract(format!(
                "run '{}' does not follow configured private run prefix '{}'",
                run.as_str(),
                descriptor.private_run.id_prefixes.run
            ))
        })?
        .to_string();

    let mut candidates = BTreeSet::new();
    candidates.insert(record.worker.clone());
    for checkpoint in &record.checkpoints {
        if let Some(interface) = checkpoint.inbound_interface() {
            candidates.insert(interface.clone());
        }
        candidates.extend(checkpoint.outbound_interfaces().cloned());
    }
    let provider_id = InterfaceId::new(format!(
        "{}{}",
        descriptor.private_run.id_prefixes.provider, opaque_id
    ));
    if !candidates.contains(&provider_id) {
        return Err(contract(format!(
            "private run '{}' does not bind its exact provider interface '{}'",
            run.as_str(),
            provider_id.as_str()
        )));
    }
    let provider = load_interface(registry_root, &provider_id)?;
    let provider_config = provider.interface_driver_config.as_ref().ok_or_else(|| {
        contract(format!(
            "private provider interface '{}' carries no instance config",
            provider.id.as_str()
        ))
    })?;
    let model = metadata_string(provider_config, "model", &provider.id)?;
    let reasoning_effort = configured_reasoning_effort(provider_config, &provider.id)?;
    let expected_catalog = load_root_catalog(
        registry_root,
        &descriptor,
        &PrivateRunRequest::new(
            model.clone(),
            reasoning_effort.clone(),
            "private_run_topology_validation",
        ),
    )?;
    let exact_tools = planned_exact_tools(&expected_catalog, &opaque_id)?;
    let authored_hidden_tools = provider_config
        .private_config
        .get("model_hidden_tools")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            contract(format!(
                "private provider interface '{}' has no model_hidden_tools array",
                provider.id.as_str()
            ))
        })?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|tool| !tool.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    contract(format!(
                        "private provider interface '{}' has malformed model_hidden_tools",
                        provider.id.as_str()
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if authored_hidden_tools != exact_model_hidden_tools(&descriptor, &exact_tools)? {
        return Err(contract(format!(
            "private provider interface '{}' does not carry exact model-hidden tool ids",
            provider.id.as_str()
        )));
    }

    if provider_config.private_config.contains_key("run_authoring") {
        return Err(contract(format!(
            "private provider interface '{}' carries forbidden reverse-owned run_authoring topology",
            provider.id.as_str()
        )));
    }
    let run_authoring_profile = provider_config
        .private_config
        .get("run_authoring_profile")
        .cloned()
        .ok_or_else(|| {
            contract(format!(
                "private provider interface '{}' has no run_authoring_profile",
                provider.id.as_str()
            ))
        })?;
    let run_authoring_profile: ProviderRunAuthoringProfile =
        serde_json::from_value(run_authoring_profile).map_err(|error| {
            contract(format!(
                "private provider interface '{}' has malformed run_authoring_profile: {error}",
                provider.id.as_str()
            ))
        })?;
    let exact_file_write_tool = exact_tool(&exact_tools, &descriptor.authoring.tool)?;
    let exact_file_write_result_label = exact_tool_result_label(
        &descriptor.authoring.tool,
        &descriptor.artifact_labels.file_write_result,
        &exact_file_write_tool,
    )?;
    let expected_profile = ProviderRunAuthoringProfile {
        file_write_tool: exact_file_write_tool.clone(),
        file_write_label: descriptor.artifact_labels.tool_call.clone(),
        file_write_result_label: exact_file_write_result_label,
        result_timeout_ms: descriptor.authoring.result_timeout_ms,
    };
    if run_authoring_profile != expected_profile {
        return Err(contract(format!(
            "private provider interface '{}' run_authoring_profile does not match the terminal-owned exact authoring surface",
            provider.id.as_str(),
        )));
    }

    let prefixes = &descriptor.private_run.id_prefixes;
    let terminal_id = InterfaceId::new(format!("{}{}", prefixes.terminal, opaque_id));
    let shell_terminal_id = InterfaceId::new(format!("{}{}", prefixes.shell_terminal, opaque_id));
    let interface_authoring_id =
        InterfaceId::new(format!("{}{}", prefixes.interface_authoring, opaque_id));
    let authoring_environment_id =
        InterfaceId::new(format!("{}{}", prefixes.authoring_environment, opaque_id));
    let worker_id = InterfaceId::new(format!("{}{}", prefixes.worker, opaque_id));
    let mut expected_capability_interfaces = BTreeMap::new();
    for (template, capability) in &expected_catalog.capabilities {
        if exact_capability_tools(&descriptor, template, capability, &exact_tools)?.is_empty() {
            continue;
        }
        expected_capability_interfaces.insert(
            template.clone(),
            InterfaceId::new(format!("{}_{}", template, opaque_id)),
        );
    }
    let shell_environment_id = expected_capability_interfaces
        .get(expected_catalog.shell_environment.id.as_str())
        .cloned()
        .ok_or_else(|| {
            contract(format!(
                "private run '{}' has no exact shell capability interface",
                run.as_str()
            ))
        })?;
    let expected_operation_interfaces = expected_catalog
        .operation_capabilities
        .keys()
        .map(|template| {
            (
                template.clone(),
                InterfaceId::new(format!("{}_{}", template, opaque_id)),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let client_projection_labels = provider_client_projection_labels(&provider)?;
    let authored_checkpoints =
        index_authored_checkpoint_templates(private_run_authoring_checkpoints(
            &descriptor,
            &expected_catalog,
            &terminal_id,
            &shell_terminal_id,
            &provider.id,
            &interface_authoring_id,
            &authoring_environment_id,
            &shell_environment_id,
            &worker_id,
            &expected_capability_interfaces,
            &expected_operation_interfaces,
            &exact_tools,
            &client_projection_labels,
        )?)?;
    let mut expected_capability_count = 0_usize;
    for (template, capability) in &expected_catalog.capabilities {
        if !exact_capability_tools(&descriptor, template, capability, &exact_tools)?.is_empty() {
            expected_capability_count += 1;
        }
    }
    let expected_checkpoint_count =
        8 + expected_capability_count * 2 + expected_catalog.operation_capabilities.len() * 2;
    if authored_checkpoints.len() != expected_checkpoint_count {
        return Err(contract(format!(
            "private run '{}' declares {} provider-authored checkpoints, formed topology requires {}",
            run.as_str(),
            authored_checkpoints.len(),
            expected_checkpoint_count
        )));
    }

    let terminal_input =
        authored_checkpoint(&authored_checkpoints, "terminal_input", &provider.id)?;
    require_checkpoint_route(
        terminal_input,
        "terminal_input",
        &terminal_id,
        &[&provider.id],
        true,
    )?;
    require_checkpoint_without_verifier(terminal_input, "terminal_input")?;
    require_checkpoint_role(
        terminal_input,
        "terminal_input",
        AuthoredCheckpointRole::Stable,
    )?;
    let turn_control_input =
        authored_checkpoint(&authored_checkpoints, "turn_control_input", &provider.id)?;
    require_checkpoint_route(
        turn_control_input,
        "turn_control_input",
        &terminal_id,
        &[&provider.id],
        false,
    )?;
    require_checkpoint_without_verifier(turn_control_input, "turn_control_input")?;
    require_checkpoint_role(
        turn_control_input,
        "turn_control_input",
        AuthoredCheckpointRole::Stable,
    )?;
    if turn_control_input.interface_binding.config.artifact_policy
        != (BindingArtifactPolicy::Allow {
            names: vec![descriptor.artifact_labels.turn_control.clone()],
            namespaces: Vec::new(),
        })
    {
        return Err(contract(
            "run_authoring checkpoint 'turn_control_input' does not exclusively admit the descriptor-selected control label",
        ));
    }
    let worker_input = authored_checkpoint(&authored_checkpoints, "worker_input", &provider.id)?;
    require_checkpoint_route(
        worker_input,
        "worker_input",
        &worker_id,
        &[&provider.id],
        true,
    )?;
    require_checkpoint_without_verifier(worker_input, "worker_input")?;
    require_checkpoint_role(worker_input, "worker_input", AuthoredCheckpointRole::Stable)?;
    let terminal_output =
        authored_checkpoint(&authored_checkpoints, "terminal_output", &provider.id)?;
    require_checkpoint_route(
        terminal_output,
        "terminal_output",
        &provider.id,
        &[&terminal_id],
        false,
    )?;
    require_checkpoint_without_verifier(terminal_output, "terminal_output")?;
    require_checkpoint_role(
        terminal_output,
        "terminal_output",
        AuthoredCheckpointRole::ProviderClientOutput,
    )?;

    let shell_call = authored_checkpoint(&authored_checkpoints, "shell_call", &provider.id)?;
    if single_checkpoint_outbound(shell_call, "shell_call")? != shell_environment_id {
        return Err(contract(format!(
            "private run '{}' shell checkpoint does not use the exact formed shell environment",
            run.as_str()
        )));
    }
    require_checkpoint_route(
        shell_call,
        "shell_call",
        &shell_terminal_id,
        &[&shell_environment_id],
        false,
    )?;
    let shell_tools = checkpoint_tools(
        shell_call,
        "shell_call",
        &descriptor.private_run.tool_call_mark,
        &descriptor.artifact_labels.tool_call,
        &descriptor.private_run.tool_call_verifier,
    )?;
    require_checkpoint_role(shell_call, "shell_call", AuthoredCheckpointRole::Stable)?;
    let exact_shell_tool = exact_tool(&exact_tools, &descriptor.shell.tool)?;
    if shell_tools != vec![exact_shell_tool.clone()] {
        return Err(contract(format!(
            "private run '{}' shell route grants [{}], expected '{}'",
            run.as_str(),
            shell_tools.join(", "),
            exact_shell_tool
        )));
    }
    let shell_result = authored_checkpoint(&authored_checkpoints, "shell_result", &provider.id)?;
    require_checkpoint_route(
        shell_result,
        "shell_result",
        &shell_environment_id,
        &[&shell_terminal_id],
        false,
    )?;
    require_checkpoint_without_verifier(shell_result, "shell_result")?;
    require_checkpoint_role(shell_result, "shell_result", AuthoredCheckpointRole::Stable)?;

    let authoring_call =
        authored_checkpoint(&authored_checkpoints, "authoring_call", &provider.id)?;
    if single_checkpoint_outbound(authoring_call, "authoring_call")? != authoring_environment_id {
        return Err(contract(format!(
            "private run '{}' authoring checkpoint does not use the exact formed authoring environment",
            run.as_str()
        )));
    }
    require_checkpoint_route(
        authoring_call,
        "authoring_call",
        &interface_authoring_id,
        &[&authoring_environment_id],
        false,
    )?;
    require_checkpoint_without_verifier(authoring_call, "authoring_call")?;
    require_checkpoint_role(
        authoring_call,
        "authoring_call",
        AuthoredCheckpointRole::AuthoringCall,
    )?;
    let authoring_result =
        authored_checkpoint(&authored_checkpoints, "authoring_result", &provider.id)?;
    require_checkpoint_route(
        authoring_result,
        "authoring_result",
        &authoring_environment_id,
        &[&provider.id],
        false,
    )?;
    require_checkpoint_without_verifier(authoring_result, "authoring_result")?;
    require_checkpoint_role(
        authoring_result,
        "authoring_result",
        AuthoredCheckpointRole::AuthoringResult,
    )?;
    validate_active_terminal_topology(
        &record,
        &authored_checkpoints,
        &provider.id,
        &expected_catalog.mandatory_role_artifacts,
    )?;

    let terminal = load_interface(registry_root, &terminal_id)?;
    let shell_terminal = load_interface(registry_root, &shell_terminal_id)?;
    let interface_authoring = load_interface(registry_root, &interface_authoring_id)?;
    let authoring_environment = load_interface(registry_root, &authoring_environment_id)?;
    let shell_environment = load_interface(registry_root, &shell_environment_id)?;
    let worker = load_interface(registry_root, &worker_id)?;
    validate_exact_tool_descriptor(
        registry_root,
        &descriptor.authoring.tool,
        &exact_file_write_tool,
        &authoring_environment.id,
    )?;

    let mut worker_environment_interfaces = BTreeMap::new();
    let mut tool_interfaces = BTreeMap::new();
    let mut routed_tools = BTreeSet::new();
    for (template, expected) in &expected_catalog.capabilities {
        let exact_capability_tools =
            exact_capability_tools(&descriptor, template, expected, &exact_tools)?;
        if exact_capability_tools.is_empty() {
            continue;
        }
        let segment = worker_environment_segment(template);
        let input_name = format!("capability_{}_input", segment);
        let input = authored_checkpoint(&authored_checkpoints, &input_name, &provider.id)?;
        let interface = input.interface().clone();
        require_checkpoint_route(input, &input_name, &interface, &[&provider.id], true)?;
        require_checkpoint_without_verifier(input, &input_name)?;
        require_checkpoint_role(input, &input_name, AuthoredCheckpointRole::Stable)?;

        let output_name = format!("capability_{}_output", segment);
        let output = authored_checkpoint(&authored_checkpoints, &output_name, &provider.id)?;
        require_checkpoint_route(output, &output_name, &provider.id, &[&interface], false)?;
        let tools = checkpoint_tools(
            output,
            &output_name,
            &descriptor.private_run.tool_call_mark,
            &descriptor.artifact_labels.tool_call,
            &descriptor.private_run.tool_call_verifier,
        )?;
        require_checkpoint_role(
            output,
            &output_name,
            AuthoredCheckpointRole::ProviderToolOutput,
        )?;
        if tools != exact_capability_tools {
            return Err(contract(format!(
                "private run '{}' capability template '{}' tool surface [{}] does not match formed catalog [{}]",
                run.as_str(),
                template,
                tools.join(", "),
                exact_capability_tools.join(", ")
            )));
        }
        for tool in &tools {
            if !routed_tools.insert(tool.clone()) {
                return Err(contract(format!(
                    "private run '{}' grants tool '{}' to more than one capability route",
                    run.as_str(),
                    tool
                )));
            }
        }

        let instance = load_interface(registry_root, &interface)?;
        for base_tool in &expected.tools {
            if template == &descriptor.authoring.room_interface
                && base_tool == &descriptor.authoring.tool
            {
                continue;
            }
            validate_exact_tool_descriptor(
                registry_root,
                base_tool,
                exact_tool(&exact_tools, base_tool)?.as_str(),
                &instance.id,
            )?;
        }
        for sidecar in expected.sidecars.keys() {
            load_json_sidecar(
                registry_root,
                &instance.id,
                sidecar,
                "private capability sidecar",
            )?;
        }
        if expected.worker_environment_kinds.is_empty() {
            if tool_interfaces
                .insert(template.clone(), instance.id)
                .is_some()
            {
                return Err(contract(format!(
                    "private run '{}' repeats pure capability template '{}'",
                    run.as_str(),
                    template
                )));
            }
        } else if worker_environment_interfaces
            .insert(template.clone(), instance.id)
            .is_some()
        {
            return Err(contract(format!(
                "private run '{}' repeats worker-environment capability template '{}'",
                run.as_str(),
                template
            )));
        }
    }
    let expected_tools = expected_catalog
        .capabilities
        .iter()
        .map(|(template, capability)| {
            exact_capability_tools(&descriptor, template, capability, &exact_tools)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    if routed_tools != expected_tools {
        return Err(contract(format!(
            "private run '{}' capability routing does not cover the formed tool surface",
            run.as_str()
        )));
    }
    if worker_environment_interfaces.get(expected_catalog.shell_environment.id.as_str())
        != Some(&shell_environment.id)
    {
        return Err(contract(format!(
            "private run '{}' shell route does not reuse exact capability door for '{}'",
            run.as_str(),
            expected_catalog.shell_environment.id.as_str()
        )));
    }

    let mut operation_interfaces = BTreeMap::new();
    for (template, expected) in &expected_catalog.operation_capabilities {
        let segment = worker_environment_segment(template);
        let input_name = format!("operation_{}_input", segment);
        let input = authored_checkpoint(&authored_checkpoints, &input_name, &provider.id)?;
        let interface = input.interface().clone();
        require_checkpoint_route(input, &input_name, &interface, &[&provider.id], true)?;
        require_checkpoint_without_verifier(input, &input_name)?;
        require_checkpoint_role(input, &input_name, AuthoredCheckpointRole::Stable)?;

        let output_name = format!("operation_{}_output", segment);
        let output = authored_checkpoint(&authored_checkpoints, &output_name, &provider.id)?;
        require_checkpoint_route(output, &output_name, &provider.id, &[&interface], false)?;
        require_checkpoint_without_verifier(output, &output_name)?;
        require_checkpoint_role(
            output,
            &output_name,
            AuthoredCheckpointRole::ProviderInterfaceOutput,
        )?;

        let instance = load_interface(registry_root, &interface)?;
        let private = private_operation_capability(registry_root, &instance, expected)?;
        validate_exact_operation_descriptors(registry_root, &instance.id, &private)?;
        let catalog_artifact =
            validate_operation_catalog_artifact(registry_root, &instance.id, &private)?;
        require_active_operation_catalog_binding(
            &record,
            &authored_checkpoints,
            &catalog_artifact,
        )?;
        if operation_interfaces
            .insert(template.clone(), instance.id)
            .is_some()
        {
            return Err(contract(format!(
                "private run '{}' repeats operation interface template '{}'",
                run.as_str(),
                template
            )));
        }
    }

    let mut linked_interfaces = vec![
        &terminal.id,
        &shell_terminal.id,
        &provider.id,
        &interface_authoring.id,
        &authoring_environment.id,
        &shell_environment.id,
        &worker.id,
    ];
    linked_interfaces.extend(worker_environment_interfaces.values());
    linked_interfaces.extend(tool_interfaces.values());
    linked_interfaces.extend(operation_interfaces.values());
    for interface in linked_interfaces {
        if !candidates.contains(interface) {
            return Err(contract(format!(
                "private run '{}' does not bind declared interface '{}'",
                run.as_str(),
                interface.as_str()
            )));
        }
    }
    if record.worker != worker.id {
        return Err(contract(format!(
            "run '{}' wears worker '{}', but provider authoring declares worker '{}'",
            run.as_str(),
            record.worker.as_str(),
            worker.id.as_str()
        )));
    }
    let capability_interfaces =
        capability_interfaces_from_topology(&worker_environment_interfaces, &tool_interfaces);
    let interface_replacements = planned_private_interface_replacements(
        &expected_catalog,
        &terminal.id,
        &shell_terminal.id,
        &provider.id,
        &interface_authoring.id,
        &authoring_environment.id,
        &worker.id,
        &capability_interfaces,
        &operation_interfaces,
    )?;
    validate_private_policy_and_consents(
        registry_root,
        &expected_catalog,
        &worker,
        &exact_tools,
        &interface_replacements,
        private_topology_interfaces(
            &terminal.id,
            &shell_terminal.id,
            &provider.id,
            &interface_authoring.id,
            &authoring_environment.id,
            &worker.id,
            worker_environment_interfaces
                .values()
                .chain(tool_interfaces.values())
                .chain(operation_interfaces.values()),
        ),
    )?;

    let provider_config = provider.interface_driver_config.as_ref().ok_or_else(|| {
        contract(format!(
            "private provider interface '{}' carries no driver config",
            provider.id.as_str()
        ))
    })?;
    let roots = TerminalHostRoots::load(octopus_home, registry_root)?;
    let formed_models = load_live_formed_provider_models(
        registry_root,
        &roots.ee_instances_root,
        &record.ee,
        &worker.id,
        &provider,
    )?;
    validate_provider_model_selection(
        provider_config,
        &provider.id,
        &model,
        &reasoning_effort,
        &formed_models,
    )?;
    let ee_path = roots
        .ee_instances_root
        .join(format!("{}.json", record.ee.as_str()));
    let ee_bytes = read_bytes("private run EE", &ee_path)?;
    let ee: EeInstance =
        serde_json::from_slice(&ee_bytes).map_err(|error| TopologyError::Malformed {
            surface: "private run EE",
            path: ee_path.display().to_string(),
            reason: error.to_string(),
        })?;
    if ee.ee_id != record.ee || ee.template != expected_catalog.ee_template.id {
        return Err(contract(format!(
            "private run '{}' does not live in the expected EE template '{}'",
            run.as_str(),
            expected_catalog.ee_template.id.as_str()
        )));
    }

    Ok(PrivateRunTopology {
        opaque_id,
        run: record.id,
        ee: record.ee,
        terminal_socket: interface_control_socket_path(octopus_home, &terminal.id),
        shell_terminal_socket: interface_control_socket_path(octopus_home, &shell_terminal.id),
        interface_authoring_socket: interface_control_socket_path(
            octopus_home,
            &interface_authoring.id,
        ),
        terminal_interface: terminal.id,
        shell_terminal_interface: shell_terminal.id,
        provider_interface: provider.id,
        interface_authoring_interface: interface_authoring.id,
        authoring_environment_interface: authoring_environment.id,
        worker_environment_interfaces,
        tool_interfaces,
        operation_interfaces,
        exact_tools,
        shell_environment_interface: shell_environment.id,
        worker_interface: worker.id,
        model,
        reasoning_effort,
        operator_input_label: descriptor.artifact_labels.operator_input,
        turn_control_label: descriptor.artifact_labels.turn_control,
    })
}
pub fn load_private_provider_model_catalog(
    registry_root: &Path,
    ee_instances_root: &Path,
    topology: &PrivateRunTopology,
) -> Result<Vec<ProviderModelPreset>, TopologyError> {
    require_absolute("registry root", registry_root)?;
    require_absolute("EE instances root", ee_instances_root)?;
    let provider_path = registry_root
        .join("interface")
        .join(topology.provider_interface.as_str())
        .join("interface.json");
    let provider_bytes = read_bytes("private provider interface", &provider_path)?;
    let provider = octopus_descriptors::parse_strict::<InterfaceDescriptor>(&provider_bytes)
        .map_err(|error| TopologyError::Malformed {
            surface: "private provider interface",
            path: provider_path.display().to_string(),
            reason: error.to_string(),
        })?;
    if provider.id != topology.provider_interface {
        return Err(contract(format!(
            "private provider file '{}' declares interface '{}', expected '{}'",
            provider_path.display(),
            provider.id.as_str(),
            topology.provider_interface.as_str()
        )));
    }
    let config = provider.interface_driver_config.as_ref().ok_or_else(|| {
        contract(format!(
            "private provider interface '{}' carries no driver config",
            provider.id.as_str()
        ))
    })?;
    let formed_models = load_live_formed_provider_models(
        registry_root,
        ee_instances_root,
        &topology.ee,
        &topology.worker_interface,
        &provider,
    )?;
    validate_provider_model_selection(
        config,
        &provider.id,
        &topology.model,
        &topology.reasoning_effort,
        &formed_models,
    )
}

fn load_root_catalog(
    registry_root: &Path,
    descriptor: &TerminalClientDescriptor,
    request: &PrivateRunRequest,
) -> Result<RootCatalog, TopologyError> {
    require_non_empty("private run model", &request.model)?;
    require_non_empty("private run goal", &request.goal)?;
    let templates = &descriptor.private_run.interface_templates;
    let terminal = load_template_interface(registry_root, &templates.terminal)?;
    let shell_terminal = load_template_interface(registry_root, &templates.shell_terminal)?;
    let provider = load_template_interface(registry_root, &templates.provider)?;
    let interface_authoring =
        load_template_interface(registry_root, &templates.interface_authoring)?;
    let authoring_environment =
        load_template_interface(registry_root, &templates.authoring_environment)?;
    let shell_environment = load_template_interface(registry_root, &templates.shell_environment)?;
    let worker = load_template_interface(registry_root, &templates.worker)?;
    if descriptor.interface != terminal.id.as_str() {
        return Err(contract(format!(
            "client descriptor interface '{}' is not terminal template '{}'",
            descriptor.interface,
            terminal.id.as_str()
        )));
    }
    let worker_policy = load_worker_policy(registry_root, &worker.id)?;
    let ee_template = load_ee_template(registry_root, &descriptor.private_run.ee_template)?;
    let parent_ee_template =
        load_ee_template(registry_root, &descriptor.private_run.parent_ee_template)?;
    let expected_parent = bootstrap_ee_instance_id(&parent_ee_template);
    if expected_parent.as_str() != descriptor.private_run.parent_ee {
        return Err(contract(format!(
            "configured parent EE '{}' is not boot instance '{}' of template '{}'",
            descriptor.private_run.parent_ee,
            expected_parent.as_str(),
            parent_ee_template.id.as_str()
        )));
    }
    if ee_template.id != parent_ee_template.id
        && !parent_ee_template
            .allowed_child_ee_templates
            .iter()
            .any(|template| template == ee_template.id.as_str())
    {
        return Err(contract(format!(
            "parent EE template '{}' does not allow child template '{}'",
            parent_ee_template.id.as_str(),
            ee_template.id.as_str()
        )));
    }

    let ee_template_chain = if ee_template.id == parent_ee_template.id {
        vec![parent_ee_template.id.clone()]
    } else {
        vec![parent_ee_template.id.clone(), ee_template.id.clone()]
    };
    let formed_capabilities = form_capability_topology(
        registry_root,
        &CapabilityFormationRequest::new(worker.id.clone(), ee_template_chain),
    )
    .map_err(|error| contract(format!("terminal capability formation failed: {error}")))?;
    if formed_capabilities.worker != worker.id {
        return Err(contract(format!(
            "terminal capability formation resolved worker '{}', expected template worker '{}'",
            formed_capabilities.worker.as_str(),
            worker.id.as_str()
        )));
    }

    let worker_relays = formed_capabilities
        .worker_relays
        .values()
        .map(|relay| relay.interface.id.clone())
        .collect::<BTreeSet<_>>();
    if !worker_relays.contains(&worker.id) {
        return Err(contract(format!(
            "worker '{}' does not form its own runtime relay",
            worker.id.as_str()
        )));
    }
    let mandatory_role_artifacts = ordered_mandatory_role_artifacts(
        &worker.id,
        &worker_policy.role_artifacts,
        &formed_capabilities.worker_relays,
    );

    let mut capabilities = BTreeMap::new();
    for (interface_id, binding) in formed_capabilities.tool_bindings {
        if interface_id != binding.interface.id {
            return Err(contract(format!(
                "formed capability key '{}' disagrees with interface '{}'",
                interface_id.as_str(),
                binding.interface.id.as_str()
            )));
        }
        let name = interface_id.as_str().to_string();
        let mut tools = binding
            .tools
            .iter()
            .map(|tool| tool.tool.as_str().to_string())
            .collect::<Vec<_>>();
        tools.sort();
        if tools.is_empty() {
            return Err(contract(format!(
                "formed capability interface '{}' has no tools",
                interface_id.as_str()
            )));
        }
        let mut tool_worker_environment_kinds = BTreeMap::new();
        for tool in &binding.tools {
            if tool_worker_environment_kinds
                .insert(
                    tool.tool.as_str().to_string(),
                    tool.worker_environment_kind.clone(),
                )
                .is_some()
            {
                return Err(contract(format!(
                    "formed capability interface '{}' repeats tool '{}'",
                    interface_id.as_str(),
                    tool.tool.as_str()
                )));
            }
        }
        let mut sidecars = BTreeMap::new();
        for requirement in binding.sidecars {
            require_sidecar_name("formed capability sidecar", &requirement.file_name)?;
            let content = load_json_sidecar(
                registry_root,
                &binding.interface.id,
                &requirement.file_name,
                "formed capability interface sidecar",
            )?;
            if sidecars
                .insert(requirement.file_name.clone(), content)
                .is_some()
            {
                return Err(contract(format!(
                    "formed capability interface '{}' repeats sidecar '{}'",
                    interface_id.as_str(),
                    requirement.file_name
                )));
            }
        }
        if capabilities
            .insert(
                name.clone(),
                CapabilityCatalog {
                    template: binding.interface,
                    tools,
                    tool_worker_environment_kinds,
                    worker_environment_kinds: binding.worker_environment_kinds,
                    sidecars,
                },
            )
            .is_some()
        {
            return Err(contract(format!(
                "formed capability catalog repeats interface '{}'",
                name
            )));
        }
    }
    if capabilities.is_empty() {
        return Err(contract(format!(
            "worker '{}' forms no terminal capabilities",
            worker.id.as_str()
        )));
    }

    let mut operation_capabilities = BTreeMap::new();
    for (interface_id, binding) in formed_capabilities.operation_bindings {
        if receives_world_events(&binding.interface)? {
            continue;
        }
        if interface_id != binding.interface.id {
            return Err(contract(format!(
                "formed operation capability key '{}' disagrees with interface '{}'",
                interface_id.as_str(),
                binding.interface.id.as_str()
            )));
        }
        if binding.operations.is_empty() {
            return Err(contract(format!(
                "formed operation capability interface '{}' has no operations",
                interface_id.as_str()
            )));
        }
        let name = interface_id.as_str().to_string();
        if operation_capabilities
            .insert(
                name.clone(),
                OperationCapabilityCatalog {
                    template: binding.interface,
                    catalog_dir: binding.catalog_dir,
                    catalog_label: binding.catalog_label,
                    request_label: binding.request_label,
                    operations: binding.operations,
                },
            )
            .is_some()
        {
            return Err(contract(format!(
                "formed operation capability catalog repeats interface '{}'",
                name
            )));
        }
    }

    let mut explicitly_bound_interfaces = vec![
        &terminal.id,
        &shell_terminal.id,
        &provider.id,
        &interface_authoring.id,
        &authoring_environment.id,
        &shell_environment.id,
        &worker.id,
    ];
    explicitly_bound_interfaces.extend(
        capabilities
            .values()
            .map(|capability| &capability.template.id),
    );
    explicitly_bound_interfaces.extend(
        operation_capabilities
            .values()
            .map(|capability| &capability.template.id),
    );
    for interface in explicitly_bound_interfaces {
        for template in [&parent_ee_template, &ee_template] {
            if !optional_axis_allows(&template.allowed_interfaces, interface.as_str()) {
                return Err(contract(format!(
                    "EE template '{}' does not allow exact interface '{}'",
                    template.id.as_str(),
                    interface.as_str()
                )));
            }
        }
        if !worker_policy
            .ceiling
            .interfaces
            .iter()
            .any(|allowed| allowed == interface)
        {
            return Err(contract(format!(
                "worker '{}' ceiling does not allow exact interface '{}'",
                worker.id.as_str(),
                interface.as_str()
            )));
        }
    }
    if !worker_policy
        .ceiling
        .mark_outcomes
        .iter()
        .any(|mark| mark.as_str() == descriptor.private_run.tool_call_mark)
    {
        return Err(contract(format!(
            "worker '{}' does not grant mark outcome '{}'",
            worker.id.as_str(),
            descriptor.private_run.tool_call_mark
        )));
    }

    let shell_capability = capabilities
        .get(shell_environment.id.as_str())
        .ok_or_else(|| {
            contract(format!(
                "shell environment interface '{}' is absent from formed capabilities",
                shell_environment.id.as_str()
            ))
        })?;
    if !shell_capability
        .tools
        .iter()
        .any(|tool| tool == &descriptor.shell.tool)
    {
        return Err(contract(format!(
            "shell tool '{}' is absent from worker '{}' formed capability interface '{}'",
            descriptor.shell.tool,
            worker.id.as_str(),
            shell_environment.id.as_str()
        )));
    }
    let authoring_capability = capabilities
        .get(authoring_environment.id.as_str())
        .ok_or_else(|| {
            contract(format!(
                "authoring environment interface '{}' is absent from formed capabilities",
                authoring_environment.id.as_str()
            ))
        })?;
    if !authoring_capability
        .tools
        .iter()
        .any(|tool| tool == &descriptor.authoring.tool)
    {
        return Err(contract(format!(
            "authoring tool '{}' is absent from worker '{}' formed capability interface '{}'",
            descriptor.authoring.tool,
            worker.id.as_str(),
            authoring_environment.id.as_str()
        )));
    }

    Ok(RootCatalog {
        terminal,
        shell_terminal,
        provider,
        interface_authoring,
        authoring_environment,
        capabilities,
        operation_capabilities,
        shell_environment,
        worker,
        worker_policy,
        mandatory_role_artifacts,
        ee_template,
        parent_ee_template,
    })
}

fn ordered_mandatory_role_artifacts(
    worker: &InterfaceId,
    current: &[ArtifactId],
    relays: &BTreeMap<InterfaceId, WorkerRelayCapabilityBinding>,
) -> Vec<ArtifactId> {
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::new();
    for artifact in current {
        if seen.insert(artifact.clone()) {
            ordered.push(artifact.clone());
        }
    }
    for (target, relay) in relays {
        if target == worker {
            continue;
        }
        for artifact in &relay.role_artifacts {
            if seen.insert(artifact.clone()) {
                ordered.push(artifact.clone());
            }
        }
    }
    ordered
}

fn live_mandatory_role_artifacts(
    roots: &TerminalHostRoots,
    ee: &EeId,
    worker: &InterfaceId,
) -> Result<Vec<ArtifactId>, TopologyError> {
    let policy = load_worker_policy(&roots.registry_root, worker)?;
    let chain = resolve_live_ee_policy_chain(&roots.ee_instances_root, ee)
        .map_err(|error| contract(format!("terminal live EE formation failed: {error}")))?;
    let formed = form_capability_topology(
        &roots.registry_root,
        &CapabilityFormationRequest::from_live_chain(worker.clone(), chain),
    )
    .map_err(|error| {
        contract(format!(
            "terminal live capability formation failed: {error}"
        ))
    })?;
    if formed.worker != *worker {
        return Err(contract(format!(
            "terminal live capability formation resolved worker '{}', expected '{}'",
            formed.worker.as_str(),
            worker.as_str()
        )));
    }
    Ok(ordered_mandatory_role_artifacts(
        worker,
        &policy.role_artifacts,
        &formed.worker_relays,
    ))
}

fn artifact_binding_values(artifacts: &[ArtifactId]) -> Vec<serde_json::Value> {
    artifacts
        .iter()
        .map(|artifact| serde_json::json!({"id": artifact}))
        .collect()
}

fn private_run_authoring_binding(
    run: &RunId,
    worker: &InterfaceId,
    provider: &InterfaceId,
    authoring_source: &InterfaceId,
    checkpoints: Vec<AuthoredCheckpointTemplate>,
) -> Result<serde_json::Value, TopologyError> {
    let checkpoints = checkpoints
        .into_iter()
        .filter_map(|checkpoint| {
            let role = match checkpoint.role {
                AuthoredCheckpointRole::Stable
                    if checkpoint.outbound_interface_bindings.len() == 1
                        && checkpoint.outbound_interfaces().next() == Some(provider) =>
                {
                    if checkpoint.include_context {
                        "stable"
                    } else {
                        "independent_input"
                    }
                }
                AuthoredCheckpointRole::ProviderClientOutput
                | AuthoredCheckpointRole::ProviderInterfaceOutput => "provider_artifact_output",
                AuthoredCheckpointRole::ProviderToolOutput => "provider_tool_output",
                AuthoredCheckpointRole::AuthoringCall => "authoring_call",
                AuthoredCheckpointRole::AuthoringResult => "authoring_result",
                AuthoredCheckpointRole::Stable => return None,
            };
            Some(serde_json::json!({
                "name": checkpoint.name,
                "role": role,
                "interface_binding": checkpoint.interface_binding,
                "outbound_interface_bindings": checkpoint.outbound_interface_bindings,
                "include_context": checkpoint.include_context,
                "required_verified_artifacts": checkpoint.required_verified_artifacts,
            }))
        })
        .collect::<Vec<_>>();
    if checkpoints.is_empty() {
        return Err(contract("private run has no provider run-authoring checkpoints"));
    }
    Ok(serde_json::json!({
        "schema_version": 2,
        "run": run,
        "worker": worker,
        "provider_interface": provider,
        "authoring_source_interface": authoring_source,
        "checkpoints": checkpoints,
    }))
}

fn private_run_authoring_checkpoints(
    descriptor: &TerminalClientDescriptor,
    catalog: &RootCatalog,
    terminal: &InterfaceId,
    shell_terminal: &InterfaceId,
    provider: &InterfaceId,
    interface_authoring: &InterfaceId,
    authoring_environment: &InterfaceId,
    shell_environment: &InterfaceId,
    worker: &InterfaceId,
    capability_interfaces: &BTreeMap<String, InterfaceId>,
    operation_interfaces: &BTreeMap<String, InterfaceId>,
    exact_tools: &BTreeMap<String, String>,
    client_projection_labels: &[String],
) -> Result<Vec<AuthoredCheckpointTemplate>, TopologyError> {
    let mut checkpoints = vec![
        AuthoredCheckpointTemplate {
            name: "terminal_input".to_string(),
            role: AuthoredCheckpointRole::Stable,
            interface_binding: names_binding(
                terminal,
                [descriptor.artifact_labels.operator_input.clone()],
            )?,
            outbound_interface_bindings: vec![all_binding(provider)],
            include_context: true,
            required_verified_artifacts: Vec::new(),
        },
        AuthoredCheckpointTemplate {
            name: "turn_control_input".to_string(),
            role: AuthoredCheckpointRole::Stable,
            interface_binding: names_binding(
                terminal,
                [descriptor.artifact_labels.turn_control.clone()],
            )?,
            outbound_interface_bindings: vec![all_binding(provider)],
            include_context: false,
            required_verified_artifacts: Vec::new(),
        },
        AuthoredCheckpointTemplate {
            name: "worker_input".to_string(),
            role: AuthoredCheckpointRole::Stable,
            interface_binding: all_binding(worker),
            outbound_interface_bindings: vec![all_binding(provider)],
            include_context: true,
            required_verified_artifacts: Vec::new(),
        },
    ];
    for (template_name, capability) in &catalog.capabilities {
        let tools = exact_capability_tools(descriptor, template_name, capability, exact_tools)?;
        if tools.is_empty() {
            continue;
        }
        let interface = capability_interfaces.get(template_name).ok_or_else(|| {
            contract(format!(
                "capability interface template '{}' has no planned exact id",
                template_name
            ))
        })?;
        let segment = worker_environment_segment(template_name);
        let tool_call_label = descriptor.artifact_labels.tool_call.clone();
        checkpoints.extend([
            AuthoredCheckpointTemplate {
                name: format!("capability_{}_input", segment),
                role: AuthoredCheckpointRole::Stable,
                interface_binding: all_binding(interface),
                outbound_interface_bindings: vec![all_binding(provider)],
                include_context: true,
                required_verified_artifacts: Vec::new(),
            },
            AuthoredCheckpointTemplate {
                name: format!("capability_{}_output", segment),
                role: AuthoredCheckpointRole::ProviderToolOutput,
                interface_binding: names_binding(provider, [tool_call_label.clone()])?,
                outbound_interface_bindings: vec![names_binding(
                    interface,
                    [tool_call_label.clone()],
                )?],
                include_context: false,
                required_verified_artifacts: vec![RequiredVerifiedArtifact {
                    mark: MarkId::new(&descriptor.private_run.tool_call_mark),
                    artifact_label: tool_call_label,
                    verifier: VerifierId::new(&descriptor.private_run.tool_call_verifier),
                    params: serde_json::json!({ "tools": tools }),
                }],
            },
        ]);
    }
    for (template_name, capability) in &catalog.operation_capabilities {
        let interface = operation_interfaces.get(template_name).ok_or_else(|| {
            contract(format!(
                "operation interface template '{}' has no planned exact id",
                template_name
            ))
        })?;
        let segment = worker_environment_segment(template_name);
        let request_labels = capability
            .operations
            .iter()
            .map(|operation| operation.request_label.clone())
            .collect::<Vec<_>>();
        checkpoints.extend([
            AuthoredCheckpointTemplate {
                name: format!("operation_{}_input", segment),
                role: AuthoredCheckpointRole::Stable,
                interface_binding: all_binding(interface),
                outbound_interface_bindings: vec![all_binding(provider)],
                include_context: true,
                required_verified_artifacts: Vec::new(),
            },
            AuthoredCheckpointTemplate {
                name: format!("operation_{}_output", segment),
                role: AuthoredCheckpointRole::ProviderInterfaceOutput,
                interface_binding: names_binding(provider, request_labels.clone())?,
                outbound_interface_bindings: vec![names_binding(interface, request_labels)?],
                include_context: false,
                required_verified_artifacts: Vec::new(),
            },
        ]);
    }
    let shell_tool = exact_tool(exact_tools, &descriptor.shell.tool)?;
    checkpoints.extend([
        AuthoredCheckpointTemplate {
            name: "shell_call".to_string(),
            role: AuthoredCheckpointRole::Stable,
            interface_binding: all_binding(shell_terminal),
            outbound_interface_bindings: vec![all_binding(shell_environment)],
            include_context: false,
            required_verified_artifacts: vec![RequiredVerifiedArtifact {
                mark: MarkId::new(&descriptor.private_run.tool_call_mark),
                artifact_label: descriptor.artifact_labels.tool_call.clone(),
                verifier: VerifierId::new(&descriptor.private_run.tool_call_verifier),
                params: serde_json::json!({ "tools": [shell_tool] }),
            }],
        },
        AuthoredCheckpointTemplate {
            name: "shell_result".to_string(),
            role: AuthoredCheckpointRole::Stable,
            interface_binding: all_binding(shell_environment),
            outbound_interface_bindings: vec![all_binding(shell_terminal)],
            include_context: false,
            required_verified_artifacts: Vec::new(),
        },
        AuthoredCheckpointTemplate {
            name: "terminal_output".to_string(),
            role: AuthoredCheckpointRole::ProviderClientOutput,
            interface_binding: names_binding(provider, client_projection_labels.to_vec())?,
            outbound_interface_bindings: vec![names_binding(
                terminal,
                client_projection_labels.to_vec(),
            )?],
            include_context: false,
            required_verified_artifacts: Vec::new(),
        },
        AuthoredCheckpointTemplate {
            name: "authoring_call".to_string(),
            role: AuthoredCheckpointRole::AuthoringCall,
            interface_binding: all_binding(interface_authoring),
            outbound_interface_bindings: vec![all_binding(authoring_environment)],
            include_context: false,
            required_verified_artifacts: Vec::new(),
        },
        AuthoredCheckpointTemplate {
            name: "authoring_result".to_string(),
            role: AuthoredCheckpointRole::AuthoringResult,
            interface_binding: all_binding(authoring_environment),
            outbound_interface_bindings: vec![all_binding(provider)],
            include_context: false,
            required_verified_artifacts: Vec::new(),
        },
    ]);
    Ok(checkpoints)
}

fn planned_exact_tools(
    catalog: &RootCatalog,
    opaque_id: &str,
) -> Result<BTreeMap<String, String>, TopologyError> {
    let mut exact = BTreeMap::new();
    for capability in catalog.capabilities.values() {
        for tool in &capability.tools {
            let exact_id = format!("{tool}__{opaque_id}");
            if exact.insert(tool.clone(), exact_id).is_some() {
                return Err(contract(format!(
                    "formed tool '{}' belongs to more than one capability interface",
                    tool
                )));
            }
        }
    }
    Ok(exact)
}

fn receives_world_events(interface: &InterfaceDescriptor) -> Result<bool, TopologyError> {
    let Some(labels) = interface
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("task_input_labels"))
    else {
        return Ok(false);
    };
    let labels = labels
        .as_array()
        .filter(|labels| labels.iter().all(|label| label.as_str().is_some_and(|label| !label.trim().is_empty())))
        .ok_or_else(|| contract(format!(
            "interface '{}' task_input_labels must be a list of labels",
            interface.id.as_str()
        )))?;
    Ok(!labels.is_empty())
}

fn operation_catalog_artifact_id(interface: &InterfaceId) -> ArtifactId {
    ArtifactId::new(format!("artifact-operation-catalog-{}", interface.as_str()))
}

fn operation_catalog_artifact_value(
    interface: &InterfaceId,
    capability: &OperationCapabilityCatalog,
) -> serde_json::Value {
    let artifact = operation_catalog_artifact_id(interface);
    let operations = capability
        .operations
        .iter()
        .map(|operation| {
            serde_json::json!({
                "name": operation.id,
                "description": operation.description,
                "input_schema": operation.input_schema,
                "request_label": operation.request_label,
                "output_label": operation.output_label
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "id": artifact,
        "label": capability.catalog_label,
        "interface": interface,
        "request_label": capability.request_label,
        "operations": operations
    })
}

fn plan_private_operation_catalog_writes(
    roots: &TerminalHostRoots,
    catalog: &RootCatalog,
    operation_interfaces: &BTreeMap<String, InterfaceId>,
    writes: &mut Vec<PlannedWrite>,
) -> Result<BTreeMap<String, ArtifactId>, TopologyError> {
    let mut artifacts = BTreeMap::new();
    for (template, interface) in operation_interfaces {
        let capability = catalog
            .operation_capabilities
            .get(template)
            .ok_or_else(|| {
                contract(format!(
                    "planned operation interface '{}' has no formed catalog entry",
                    template
                ))
            })?;
        let artifact = operation_catalog_artifact_id(interface);
        let content = pretty_json(
            &operation_catalog_artifact_value(interface, capability),
            "private interface operation catalog artifact",
        )?;
        push_new_write(
            writes,
            roots
                .registry_artifact_root
                .join(artifact.as_str())
                .join("artifact.json"),
            content,
            PlannedWritePhase::ArtifactDescriptor,
        )?;
        if artifacts.insert(template.clone(), artifact).is_some() {
            return Err(contract(format!(
                "private operation catalog repeats interface template '{}'",
                template
            )));
        }
    }
    Ok(artifacts)
}

fn private_operation_capability(
    registry_root: &Path,
    instance: &InterfaceDescriptor,
    template: &OperationCapabilityCatalog,
) -> Result<OperationCapabilityCatalog, TopologyError> {
    let binding = load_interface_operation_binding(&registry_root.join("interface"), instance)
        .map_err(|error| contract(format!("private operation catalog failed to form: {error}")))?
        .ok_or_else(|| {
            contract(format!(
                "private operation interface '{}' declares no operation catalog",
                instance.id.as_str()
            ))
        })?;
    if binding.catalog_dir != template.catalog_dir
        || binding.catalog_label != template.catalog_label
        || binding.request_label != template.request_label
    {
        return Err(contract(format!(
            "private operation interface '{}' changed its template's catalog contract",
            instance.id.as_str()
        )));
    }
    Ok(OperationCapabilityCatalog {
        template: template.template.clone(),
        catalog_dir: binding.catalog_dir,
        catalog_label: binding.catalog_label,
        request_label: binding.request_label,
        operations: binding.operations,
    })
}

fn validate_exact_operation_descriptors(
    registry_root: &Path,
    interface: &InterfaceId,
    capability: &OperationCapabilityCatalog,
) -> Result<(), TopologyError> {
    let root = registry_root
        .join("interface")
        .join(interface.as_str())
        .join(&capability.catalog_dir);
    let metadata = std::fs::symlink_metadata(&root).map_err(|source| TopologyError::Read {
        surface: "private operation catalog",
        path: root.display().to_string(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(contract(format!(
            "private operation catalog '{}' is not a regular non-symlink directory",
            root.display()
        )));
    }
    let mut expected_files = BTreeSet::new();
    for operation in &capability.operations {
        let file_name = format!("{}.json", operation.id);
        expected_files.insert(file_name.clone());
        let path = root.join(&file_name);
        let bytes = read_bytes("private operation descriptor", &path)?;
        let actual = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
            TopologyError::Malformed {
                surface: "private operation descriptor",
                path: path.display().to_string(),
                reason: error.to_string(),
            }
        })?;
        if actual != operation.descriptor {
            return Err(contract(format!(
                "private operation descriptor '{}' differs from the formed interface catalog",
                path.display()
            )));
        }
    }
    let mut actual_files = BTreeSet::new();
    for entry in std::fs::read_dir(&root).map_err(|source| TopologyError::Read {
        surface: "private operation catalog",
        path: root.display().to_string(),
        source,
    })? {
        let entry = entry.map_err(|source| TopologyError::Read {
            surface: "private operation catalog",
            path: root.display().to_string(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| TopologyError::Read {
            surface: "private operation catalog",
            path: entry.path().display().to_string(),
            source,
        })?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(contract(format!(
                "private operation catalog entry '{}' is not a regular file",
                entry.path().display()
            )));
        }
        let name = entry.file_name().into_string().map_err(|_| {
            contract(format!(
                "private operation catalog entry '{}' is not UTF-8",
                entry.path().display()
            ))
        })?;
        actual_files.insert(name);
    }
    if actual_files != expected_files {
        return Err(contract(format!(
            "private operation catalog '{}' does not exactly match the formed operation set",
            root.display()
        )));
    }
    Ok(())
}

fn validate_operation_catalog_artifact(
    registry_root: &Path,
    interface: &InterfaceId,
    capability: &OperationCapabilityCatalog,
) -> Result<ArtifactId, TopologyError> {
    let artifact = operation_catalog_artifact_id(interface);
    let path = registry_root
        .join("artifact")
        .join(artifact.as_str())
        .join("artifact.json");
    let bytes = read_bytes("private operation catalog artifact", &path)?;
    let actual = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|error| {
        TopologyError::Malformed {
            surface: "private operation catalog artifact",
            path: path.display().to_string(),
            reason: error.to_string(),
        }
    })?;
    if actual != operation_catalog_artifact_value(interface, capability) {
        return Err(contract(format!(
            "private operation catalog artifact '{}' differs from the formed catalog",
            path.display()
        )));
    }
    Ok(artifact)
}

fn require_active_operation_catalog_binding(
    record: &RunRecord,
    authored: &BTreeMap<String, AuthoredCheckpointTemplate>,
    artifact: &ArtifactId,
) -> Result<(), TopologyError> {
    let active = record.active_checkpoints.iter().collect::<BTreeSet<_>>();
    for template in authored.values().filter(|template| {
        template.role == AuthoredCheckpointRole::Stable && template.include_context
    }) {
        let matching = record
            .checkpoints
            .iter()
            .filter(|checkpoint| active.contains(&checkpoint.id))
            .filter(|checkpoint| {
                checkpoint.interface_binding.as_ref() == Some(&template.interface_binding)
                    && checkpoint.outbound_interface_bindings
                        == template.outbound_interface_bindings
                    && checkpoint.required_verified_artifacts
                        == template.required_verified_artifacts
            })
            .collect::<Vec<_>>();
        let [checkpoint] = matching.as_slice() else {
            return Err(contract(format!(
                "private context route '{}' has {} active checkpoints",
                template.name,
                matching.len()
            )));
        };
        if !checkpoint
            .artifact_bindings
            .iter()
            .any(|binding| binding.id == *artifact)
        {
            return Err(contract(format!(
                "private context checkpoint '{}' does not bind operation catalog artifact '{}'",
                checkpoint.id.as_str(),
                artifact.as_str()
            )));
        }
    }
    Ok(())
}

fn exact_capability_tools(
    descriptor: &TerminalClientDescriptor,
    template_name: &str,
    capability: &CapabilityCatalog,
    exact_tools: &BTreeMap<String, String>,
) -> Result<Vec<String>, TopologyError> {
    capability
        .tools
        .iter()
        .filter(|tool| {
            !(template_name == descriptor.authoring.room_interface
                && tool.as_str() == descriptor.authoring.tool)
        })
        .map(|tool| exact_tool(exact_tools, tool))
        .collect()
}

fn exact_tool(
    exact_tools: &BTreeMap<String, String>,
    base_tool: &str,
) -> Result<String, TopologyError> {
    exact_tools.get(base_tool).cloned().ok_or_else(|| {
        contract(format!(
            "formed base tool '{}' has no planned exact identity",
            base_tool
        ))
    })
}

fn exact_shell_result_label(
    descriptor: &TerminalClientDescriptor,
    exact_shell_tool: &str,
) -> Result<String, TopologyError> {
    exact_tool_result_label(
        &descriptor.shell.tool,
        &descriptor.shell.result_label,
        exact_shell_tool,
    )
}

fn exact_tool_result_label(
    base_tool: &str,
    configured_base_label: &str,
    exact_tool: &str,
) -> Result<String, TopologyError> {
    let expected_base = format!("{base_tool}.result");
    if configured_base_label != expected_base {
        return Err(contract(format!(
            "tool result label '{}' is not derived from configured tool '{}'",
            configured_base_label, base_tool
        )));
    }
    Ok(format!("{exact_tool}.result"))
}

fn exact_model_hidden_tools(
    descriptor: &TerminalClientDescriptor,
    exact_tools: &BTreeMap<String, String>,
) -> Result<Vec<String>, TopologyError> {
    descriptor
        .private_run
        .model_hidden_tools
        .iter()
        .map(|tool| exact_tool(exact_tools, tool))
        .collect()
}

fn provider_client_projection_labels(
    provider: &InterfaceDescriptor,
) -> Result<Vec<String>, TopologyError> {
    let config = provider.interface_driver_config.as_ref().ok_or_else(|| {
        contract(format!(
            "provider interface '{}' has no interface_driver_config",
            provider.id.as_str()
        ))
    })?;
    let values = config
        .private_config
        .get("client_projection_output_labels")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            contract(format!(
                "provider interface '{}' has no client_projection_output_labels array",
                provider.id.as_str()
            ))
        })?;
    let mut labels = BTreeSet::new();
    for value in values {
        let label = value
            .as_str()
            .filter(|label| !label.is_empty() && label.trim() == *label)
            .ok_or_else(|| {
                contract(format!(
                    "provider interface '{}' has malformed client projection label",
                    provider.id.as_str()
                ))
            })?;
        if !labels.insert(label.to_string()) {
            return Err(contract(format!(
                "provider interface '{}' repeats client projection label '{}'",
                provider.id.as_str(),
                label
            )));
        }
    }
    if labels.is_empty() {
        return Err(contract(format!(
            "provider interface '{}' has no client projection labels",
            provider.id.as_str()
        )));
    }
    Ok(labels.into_iter().collect())
}

fn private_topology_interfaces<'a>(
    terminal: &InterfaceId,
    shell_terminal: &InterfaceId,
    provider: &InterfaceId,
    interface_authoring: &InterfaceId,
    authoring_environment: &InterfaceId,
    worker: &InterfaceId,
    capabilities: impl IntoIterator<Item = &'a InterfaceId>,
) -> Vec<InterfaceId> {
    let mut interfaces = BTreeSet::from([
        terminal.clone(),
        shell_terminal.clone(),
        provider.clone(),
        interface_authoring.clone(),
        authoring_environment.clone(),
        worker.clone(),
    ]);
    interfaces.extend(capabilities.into_iter().cloned());
    interfaces.into_iter().collect()
}

fn planned_private_interface_replacements(
    catalog: &RootCatalog,
    terminal: &InterfaceId,
    shell_terminal: &InterfaceId,
    provider: &InterfaceId,
    interface_authoring: &InterfaceId,
    authoring_environment: &InterfaceId,
    worker: &InterfaceId,
    capability_interfaces: &BTreeMap<String, InterfaceId>,
    operation_interfaces: &BTreeMap<String, InterfaceId>,
) -> Result<BTreeMap<String, BTreeSet<InterfaceId>>, TopologyError> {
    let mut replacements = BTreeMap::new();
    let mut insert = |base: &InterfaceId, exact: &InterfaceId| {
        replacements
            .entry(base.as_str().to_string())
            .or_insert_with(BTreeSet::new)
            .insert(exact.clone());
    };
    insert(&catalog.terminal.id, terminal);
    insert(&catalog.shell_terminal.id, shell_terminal);
    insert(&catalog.provider.id, provider);
    insert(&catalog.interface_authoring.id, interface_authoring);
    insert(&catalog.authoring_environment.id, authoring_environment);
    insert(&catalog.worker.id, worker);
    for (template, exact) in capability_interfaces {
        let capability = catalog.capabilities.get(template).ok_or_else(|| {
            contract(format!(
                "private capability interface '{}' has no formed catalog entry",
                template
            ))
        })?;
        insert(&capability.template.id, exact);
    }
    for (template, exact) in operation_interfaces {
        let capability = catalog
            .operation_capabilities
            .get(template)
            .ok_or_else(|| {
                contract(format!(
                    "private operation interface '{}' has no formed catalog entry",
                    template
                ))
            })?;
        insert(&capability.template.id, exact);
    }
    Ok(replacements)
}

fn replace_private_worker_interfaces(
    policy: &mut WorkerInterfacePolicy,
    replacements: &BTreeMap<String, BTreeSet<InterfaceId>>,
) -> Result<(), TopologyError> {
    rebase_private_worker_interface_policy(policy, replacements)?;
    let mut replaced = BTreeSet::new();
    let mut exact = BTreeSet::new();
    for interface in &policy.ceiling.interfaces {
        let base = interface.as_str();
        if let Some(next) = replacements.get(base) {
            replaced.insert(base.to_string());
            exact.extend(next.iter().cloned());
        } else {
            exact.insert(interface.clone());
        }
    }
    let missing = replacements
        .keys()
        .filter(|interface| !replaced.contains(*interface))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(contract(format!(
            "private worker ceiling does not contain formed interfaces [{}]",
            missing.join(", ")
        )));
    }
    policy.ceiling.interfaces = exact.into_iter().collect();
    for interface in policy.ceiling.interface_policy.keys() {
        if !policy.ceiling.interfaces.contains(interface) {
            return Err(contract(format!(
                "private worker '{}' carries interface policy for '{}' outside its rebased interface ceiling",
                policy.id.as_str(),
                interface.as_str()
            )));
        }
    }
    Ok(())
}

fn rebase_private_worker_interface_policy(
    policy: &mut WorkerInterfacePolicy,
    replacements: &BTreeMap<String, BTreeSet<InterfaceId>>,
) -> Result<(), TopologyError> {
    let mut rebased = octopus_policy::InterfacePolicyOverrideLayer::new();
    for (interface, values) in &policy.ceiling.interface_policy {
        let targets = replacements
            .get(interface.as_str())
            .cloned()
            .unwrap_or_else(|| BTreeSet::from([interface.clone()]));
        if targets.is_empty() {
            return Err(contract(format!(
                "private worker '{}' has no exact replacement for interface policy '{}'",
                policy.id.as_str(),
                interface.as_str()
            )));
        }
        for target in targets {
            if rebased.insert(target.clone(), values.clone()).is_some() {
                return Err(contract(format!(
                    "private worker '{}' interface-policy rebasing collides at exact interface '{}'",
                    policy.id.as_str(),
                    target.as_str()
                )));
            }
        }
    }
    policy.ceiling.interface_policy = rebased;
    Ok(())
}

fn replace_private_worker_tools(
    policy: &mut WorkerInterfacePolicy,
    replacements: &BTreeMap<String, String>,
) -> Result<(), TopologyError> {
    let mut replaced = BTreeSet::new();
    let mut exact = BTreeSet::new();
    for tool in &policy.ceiling.tools {
        let base = tool.as_str();
        let next = replacements
            .get(base)
            .cloned()
            .unwrap_or_else(|| base.to_string());
        if replacements.contains_key(base) {
            replaced.insert(base.to_string());
        }
        if !exact.insert(next.clone()) {
            return Err(contract(format!(
                "private worker tool replacement collapses to duplicate id '{}'",
                next
            )));
        }
    }
    let missing = replacements
        .keys()
        .filter(|tool| !replaced.contains(*tool))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(contract(format!(
            "private worker ceiling does not contain formed tools [{}]",
            missing.join(", ")
        )));
    }
    policy.ceiling.tools = exact
        .into_iter()
        .map(octopus_descriptors::ToolId::new)
        .collect();
    Ok(())
}

fn plan_private_tool_writes(
    roots: &TerminalHostRoots,
    descriptor: &TerminalClientDescriptor,
    catalog: &RootCatalog,
    authoring_environment: &InterfaceId,
    capability_interfaces: &BTreeMap<String, InterfaceId>,
    exact_tools: &BTreeMap<String, String>,
    writes: &mut Vec<PlannedWrite>,
) -> Result<(), TopologyError> {
    let tool_root = roots.registry_root.join("tool");
    let mut authored = BTreeSet::new();
    for (template_name, capability) in &catalog.capabilities {
        for base_tool in &capability.tools {
            let exact_tool = exact_tool(exact_tools, base_tool)?;
            let exact_door = if template_name == &descriptor.authoring.room_interface
                && base_tool == &descriptor.authoring.tool
            {
                authoring_environment.clone()
            } else {
                capability_interfaces
                    .get(template_name)
                    .cloned()
                    .ok_or_else(|| {
                        contract(format!(
                            "formed tool '{}' owner '{}' has no planned exact door",
                            base_tool, template_name
                        ))
                    })?
            };
            let mut tool = load_tool_descriptor_from_dir(&tool_root, base_tool)
                .map_err(|error| {
                    contract(format!(
                        "formed tool descriptor '{}' could not be loaded: {error}",
                        base_tool
                    ))
                })?
                .ok_or_else(|| contract(format!("formed tool '{}' is absent", base_tool)))?;
            if tool.interface != capability.template.id {
                return Err(contract(format!(
                    "formed tool '{}' belongs to interface '{}', expected '{}'",
                    base_tool,
                    tool.interface.as_str(),
                    capability.template.id.as_str()
                )));
            }
            tool.id = ToolDescriptorId::new(&exact_tool);
            tool.interface = exact_door.clone();
            validate_private_tool_operation(&tool, base_tool)?;
            let content = pretty_typed(&tool, "private tool descriptor")?;
            let parsed = octopus_descriptors::parse_strict::<ToolDescriptor>(content.as_bytes())
                .map_err(|error| {
                    contract(format!(
                        "private tool '{}' failed descriptor validation: {error}",
                        exact_tool
                    ))
                })?;
            if parsed.id.as_str() != exact_tool || parsed.interface != exact_door {
                return Err(contract(format!(
                    "private tool '{}' changed exact identity during encoding",
                    exact_tool
                )));
            }
            push_new_write(
                writes,
                tool_root.join(&exact_tool).join("tool.json"),
                content,
                PlannedWritePhase::ToolDescriptor,
            )?;
            authored.insert(base_tool.clone());
        }
    }
    if authored != exact_tools.keys().cloned().collect::<BTreeSet<_>>() {
        return Err(contract(
            "private tool authoring did not cover the complete formed tool surface",
        ));
    }

    Ok(())
}

fn validate_private_tool_operation(
    tool: &ToolDescriptor,
    base_tool: &str,
) -> Result<(), TopologyError> {
    let config = match &tool.handler_config {
        serde_json::Value::Null => return Ok(()),
        serde_json::Value::Object(config) => config,
        _ => {
            return Err(contract(format!(
                "tool '{}' handler_config must be null or an operation object",
                base_tool
            )));
        }
    };
    let operation = config
        .get("operation")
        .and_then(serde_json::Value::as_str)
        .filter(|operation| !operation.trim().is_empty())
        .ok_or_else(|| {
            contract(format!(
                "tool '{}' handler_config.operation is malformed",
                base_tool
            ))
        })?;
    if operation.trim() != operation {
        return Err(contract(format!(
            "tool '{}' handler_config.operation is not trimmed",
            base_tool
        )));
    }
    Ok(())
}

fn plan_ee_capability_allowances(
    roots: &TerminalHostRoots,
    catalog: &RootCatalog,
    interface_replacements: &BTreeMap<String, BTreeSet<InterfaceId>>,
    exact_tools: Vec<String>,
    writes: &mut Vec<PlannedWrite>,
) -> Result<(), TopologyError> {
    let exact_interfaces = interface_replacements
        .values()
        .flat_map(BTreeSet::iter)
        .cloned()
        .collect::<BTreeSet<_>>();
    let templates = BTreeSet::from([
        catalog.parent_ee_template.id.clone(),
        catalog.ee_template.id.clone(),
    ]);
    for template in templates {
        let path = roots
            .registry_root
            .join("ee_template")
            .join(template.as_str())
            .join("ee_template.json");
        let bytes = read_bytes("EE exact capability allowance", &path)?;
        let mut spec =
            octopus_descriptors::parse_strict::<EeTemplateDescriptor>(&bytes).map_err(|error| {
                TopologyError::Malformed {
                    surface: "EE exact capability allowance",
                    path: path.display().to_string(),
                    reason: error.to_string(),
                }
            })?;
        if spec.id != template {
            return Err(contract(format!(
                "EE template path '{}' declares '{}'",
                template.as_str(),
                spec.id.as_str()
            )));
        }
        if let Some(allowed) = spec.allowed_interfaces.as_mut() {
            let mut values = allowed.iter().cloned().collect::<BTreeSet<_>>();
            values.extend(
                exact_interfaces
                    .iter()
                    .map(|interface| interface.as_str().to_string()),
            );
            *allowed = values.into_iter().collect();
        }
        if let Some(allowed) = spec.allowed_tools.as_mut() {
            let mut values = allowed.iter().cloned().collect::<BTreeSet<_>>();
            values.extend(exact_tools.iter().cloned());
            *allowed = values.into_iter().collect();
        }
        copy_private_ee_interface_policy_overrides(&mut spec, interface_replacements)?;
        let content = pretty_typed(&spec, "EE exact capability allowance")?;
        octopus_descriptors::parse_strict::<EeTemplateDescriptor>(content.as_bytes()).map_err(
            |error| {
                contract(format!(
                    "EE template '{}' rejected exact capability allowance: {error}",
                    template.as_str()
                ))
            },
        )?;
        push_existing_write(writes, path, bytes, content)?;
    }
    Ok(())
}

fn copy_private_ee_interface_policy_overrides(
    template: &mut EeTemplateDescriptor,
    replacements: &BTreeMap<String, BTreeSet<InterfaceId>>,
) -> Result<(), TopologyError> {
    let mut additions = octopus_policy::InterfacePolicyOverrideLayer::new();
    for (base, exacts) in replacements {
        let base_id = InterfaceId::new(base);
        let Some(values) = template.interface_policy_overrides.get(&base_id) else {
            continue;
        };
        if exacts.is_empty() {
            return Err(contract(format!(
                "EE template '{}' has no exact replacement for interface policy '{}'",
                template.id.as_str(),
                base
            )));
        }
        for exact in exacts {
            if exact == &base_id {
                continue;
            }
            if template.interface_policy_overrides.contains_key(exact)
                || additions.insert(exact.clone(), values.clone()).is_some()
            {
                return Err(contract(format!(
                    "EE template '{}' interface-policy copy collides at exact interface '{}'",
                    template.id.as_str(),
                    exact.as_str()
                )));
            }
        }
    }
    template.interface_policy_overrides.extend(additions);
    Ok(())
}
fn plan_private_run(
    octopus_home: &Path,
    roots: &TerminalHostRoots,
    descriptor: &TerminalClientDescriptor,
    catalog: &RootCatalog,
    request: PrivateRunRequest,
    opaque_id: String,
) -> Result<(PrivateRunTopology, Vec<PlannedWrite>), TopologyError> {
    require_id_fragment("generated private run id", &opaque_id)?;
    validate_private_run_model_selection(catalog, &request)?;
    validate_private_door_host_slots(catalog, &request.door_host_slots)?;
    let prefixes = &descriptor.private_run.id_prefixes;
    let run = RunId::new(format!("{}{}", prefixes.run, opaque_id));
    let terminal = InterfaceId::new(format!("{}{}", prefixes.terminal, opaque_id));
    let shell_terminal = InterfaceId::new(format!("{}{}", prefixes.shell_terminal, opaque_id));
    let provider = InterfaceId::new(format!("{}{}", prefixes.provider, opaque_id));
    let interface_authoring =
        InterfaceId::new(format!("{}{}", prefixes.interface_authoring, opaque_id));
    let authoring_environment =
        InterfaceId::new(format!("{}{}", prefixes.authoring_environment, opaque_id));
    let worker = InterfaceId::new(format!("{}{}", prefixes.worker, opaque_id));

    let exact_tools = planned_exact_tools(catalog, &opaque_id)?;
    let mut capability_interfaces = BTreeMap::new();
    for (template_name, capability) in &catalog.capabilities {
        if exact_capability_tools(descriptor, template_name, capability, &exact_tools)?.is_empty() {
            continue;
        }
        let interface = InterfaceId::new(format!("{}_{}", template_name, opaque_id));
        if capability_interfaces
            .insert(template_name.clone(), interface)
            .is_some()
        {
            return Err(contract(format!(
                "formed capability plan repeats interface template '{}'",
                template_name
            )));
        }
    }
    let mut operation_interfaces = BTreeMap::new();
    for template_name in catalog.operation_capabilities.keys() {
        let interface = InterfaceId::new(format!("{}_{}", template_name, opaque_id));
        if operation_interfaces
            .insert(template_name.clone(), interface)
            .is_some()
        {
            return Err(contract(format!(
                "formed operation plan repeats interface template '{}'",
                template_name
            )));
        }
    }
    let shell_environment = capability_interfaces
        .get(catalog.shell_environment.id.as_str())
        .cloned()
        .ok_or_else(|| {
            contract(format!(
                "shell environment template '{}' has no planned exact capability door",
                catalog.shell_environment.id.as_str()
            ))
        })?;
    let interface_authoring_socket =
        interface_control_socket_path(octopus_home, &interface_authoring);
    let terminal_descriptor = child_interface_bytes(&catalog.terminal, &terminal, None)?;
    let shell_terminal_descriptor =
        child_interface_bytes(&catalog.shell_terminal, &shell_terminal, None)?;
    let provider_descriptor = child_interface_bytes(
        &catalog.provider,
        &provider,
        Some(serde_json::json!({
            "private_config": {
                "model": request.model,
                "reasoning_effort": request.reasoning_effort,
                "model_hidden_tools": exact_model_hidden_tools(descriptor, &exact_tools)?,
                "run_authoring_profile": {
                    "file_write_tool": exact_tool(&exact_tools, &descriptor.authoring.tool)?,
                    "file_write_label": descriptor.artifact_labels.tool_call,
                    "file_write_result_label": exact_tool_result_label(
                        &descriptor.authoring.tool,
                        &descriptor.artifact_labels.file_write_result,
                        &exact_tool(&exact_tools, &descriptor.authoring.tool)?,
                    )?,
                    "result_timeout_ms": descriptor.authoring.result_timeout_ms
                }
            }
        })),
    )?;
    let interface_authoring_descriptor =
        child_interface_bytes(&catalog.interface_authoring, &interface_authoring, None)?;
    let authoring_environment_descriptor =
        child_interface_bytes(&catalog.authoring_environment, &authoring_environment, None)?;
    let worker_descriptor = child_interface_bytes(&catalog.worker, &worker, None)?;

    let mut capability_descriptors = BTreeMap::new();
    for (template_name, interface) in &capability_interfaces {
        let capability = catalog.capabilities.get(template_name).ok_or_else(|| {
            contract(format!(
                "planned capability interface '{}' has no formed catalog entry",
                template_name
            ))
        })?;
        capability_descriptors.insert(
            template_name.clone(),
            child_interface_bytes(
                &capability.template,
                interface,
                request
                    .door_host_slots
                    .get(template_name)
                    .map(host_slot_overlay),
            )?,
        );
    }
    let mut operation_descriptors = BTreeMap::new();
    for (template_name, interface) in &operation_interfaces {
        let capability = catalog
            .operation_capabilities
            .get(template_name)
            .ok_or_else(|| {
                contract(format!(
                    "planned operation interface '{}' has no formed catalog entry",
                    template_name
                ))
            })?;
        operation_descriptors.insert(
            template_name.clone(),
            child_interface_bytes(&capability.template, interface, None)?,
        );
    }

    let mut private_worker_policy = catalog.worker_policy.clone();
    private_worker_policy.id = worker.clone();
    let interface_replacements = planned_private_interface_replacements(
        catalog,
        &terminal,
        &shell_terminal,
        &provider,
        &interface_authoring,
        &authoring_environment,
        &worker,
        &capability_interfaces,
        &operation_interfaces,
    )?;
    replace_private_worker_interfaces(&mut private_worker_policy, &interface_replacements)?;
    replace_private_worker_tools(&mut private_worker_policy, &exact_tools)?;
    let worker_policy = pretty_typed(&private_worker_policy, "private worker policy")?;
    let parsed_policy: WorkerInterfacePolicy = serde_json::from_slice(worker_policy.as_bytes())
        .map_err(|error| contract(format!("private worker policy failed validation: {error}")))?;
    if parsed_policy.id != worker {
        return Err(contract("private worker policy id changed during encoding"));
    }

    let terminal_listen_env = format!(
        "{}{}_LISTEN",
        descriptor.private_run.terminal_listen_env_prefix,
        opaque_id.to_ascii_uppercase()
    );
    let terminal_activation = pretty_json(
        &serde_json::json!({
            "schema_version": 1,
            "interface": terminal,
            "activations": [{
                "activation_driver_ref": descriptor.private_run.activation_driver_ref,
                "run": run,
                "config": {
                    "listen_env": terminal_listen_env,
                    "bind_interface_socket_when_env_unset": true
                }
            }]
        }),
        "private terminal activation",
    )?;
    let shell_terminal_listen_env = format!(
        "{}{}_LISTEN",
        descriptor.private_run.shell_terminal_listen_env_prefix,
        opaque_id.to_ascii_uppercase()
    );
    let shell_terminal_activation = pretty_json(
        &serde_json::json!({
            "schema_version": 1,
            "interface": shell_terminal,
            "activations": [{
                "activation_driver_ref": descriptor.private_run.activation_driver_ref,
                "run": run,
                "config": {
                    "listen_env": shell_terminal_listen_env,
                    "bind_interface_socket_when_env_unset": true
                }
            }]
        }),
        "private shell terminal activation",
    )?;
    let interface_authoring_listen_env = format!(
        "{}{}_LISTEN",
        descriptor.private_run.interface_authoring_listen_env_prefix,
        opaque_id.to_ascii_uppercase()
    );
    let interface_authoring_activation = pretty_json(
        &serde_json::json!({
            "schema_version": 1,
            "interface": interface_authoring,
            "activations": [{
                "activation_driver_ref": descriptor.private_run.activation_driver_ref,
                "run": run,
                "config": {
                    "listen_env": interface_authoring_listen_env,
                    "bind_interface_socket_when_env_unset": true
                }
            }]
        }),
        "private interface authoring activation",
    )?;

    let reuse_bootstrap_ee = catalog.ee_template.id == catalog.parent_ee_template.id;
    let requested_existing_ee = request
        .existing_ee
        .clone()
        .or_else(|| reuse_bootstrap_ee.then(|| EeId::new(&descriptor.private_run.parent_ee)));
    let (ee_instance, write_ee) = match requested_existing_ee.as_ref() {
        Some(existing_ee) => {
            let path = roots
                .ee_instances_root
                .join(format!("{}.json", existing_ee.as_str()));
            let bytes = read_bytes("existing private EE", &path)?;
            let instance: EeInstance =
                serde_json::from_slice(&bytes).map_err(|error| TopologyError::Malformed {
                    surface: "existing private EE",
                    path: path.display().to_string(),
                    reason: error.to_string(),
                })?;
            if instance.ee_id != *existing_ee
                || instance.template != catalog.ee_template.id
                || instance.goal != request.goal
            {
                return Err(contract(format!(
                    "existing EE '{}' does not match the requested private run scope",
                    existing_ee.as_str()
                )));
            }
            (instance, false)
        }
        None => (
            EeInstance {
                name: None,
                ee_id: EeId::new(format!("{}{}", prefixes.ee, opaque_id)),
                template: catalog.ee_template.id.clone(),
                parent_ee: Some(EeId::new(&descriptor.private_run.parent_ee)),
                goal: request.goal.clone(),
                config: Default::default(),
                policy_narrowing: Default::default(),
            },
            true,
        ),
    };
    if catalog.parent_ee_template.id.as_str() != descriptor.private_run.parent_ee_template {
        return Err(contract(
            "parent EE template identity changed during planning",
        ));
    }
    let ee = ee_instance.ee_id.clone();
    let ee_bytes = pretty_typed(&ee_instance, "private EE instance")?;
    let parsed_ee: EeInstance = serde_json::from_slice(ee_bytes.as_bytes())
        .map_err(|error| contract(format!("private EE instance failed validation: {error}")))?;
    if parsed_ee.ee_id != ee {
        return Err(contract(
            "private EE instance identity changed during encoding",
        ));
    }
    if write_ee
        && parsed_ee.parent_ee.as_ref().map(EeId::as_str)
            != Some(descriptor.private_run.parent_ee.as_str())
    {
        return Err(contract(
            "private EE parent identity changed during encoding",
        ));
    }

    let mut writes = Vec::new();
    let operation_catalog_artifacts =
        plan_private_operation_catalog_writes(roots, catalog, &operation_interfaces, &mut writes)?;
    let mut context_artifacts = catalog.mandatory_role_artifacts.clone();
    let mut binding_ids = context_artifacts.iter().cloned().collect::<BTreeSet<_>>();
    for artifact in &request.seed_artifacts {
        require_non_empty("private run seed artifact", artifact)?;
        let artifact = ArtifactId::new(artifact);
        if binding_ids.insert(artifact.clone()) {
            context_artifacts.push(artifact);
        }
    }
    for artifact in operation_catalog_artifacts.values() {
        if binding_ids.insert(artifact.clone()) {
            context_artifacts.push(artifact.clone());
        }
    }
    let mandatory_role_bindings = artifact_binding_values(&catalog.mandatory_role_artifacts);
    let context_bindings = artifact_binding_values(&context_artifacts);
    let checkpoint_prefix = format!("cp_{}", &opaque_id[..12]);
    let mut initial_frontier_ids = Vec::new();
    let terminal_input_binding = names_binding(
        &terminal,
        [descriptor.artifact_labels.operator_input.clone()],
    )?;
    let turn_control_binding =
        names_binding(&terminal, [descriptor.artifact_labels.turn_control.clone()])?;
    let mut checkpoints = vec![
        serde_json::json!({
            "id": format!("{checkpoint_prefix}_terminal_input"),
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_terminal_input", run.as_str()),
            "epoch": 0,
            "interface_binding": terminal_input_binding,
            "outbound_interface_bindings": [all_binding(&provider)],
            "artifact_bindings": context_bindings,
            "required_verified_artifacts": []
        }),
        serde_json::json!({
            "id": format!("{checkpoint_prefix}_turn_control_input"),
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_turn_control_input", run.as_str()),
            "epoch": 0,
            "interface_binding": turn_control_binding,
            "outbound_interface_bindings": [all_binding(&provider)],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": []
        }),
        serde_json::json!({
            "id": format!("{checkpoint_prefix}_worker_input"),
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_worker_input", run.as_str()),
            "epoch": 0,
            "interface_binding": all_binding(&worker),
            "outbound_interface_bindings": [all_binding(&provider)],
            "artifact_bindings": context_bindings,
            "required_verified_artifacts": []
        }),
    ];
    for (template_name, interface) in &capability_interfaces {
        let segment = worker_environment_segment(template_name);
        let capability = catalog.capabilities.get(template_name).ok_or_else(|| {
            contract(format!(
                "capability interface template '{}' has no formed catalog entry",
                template_name
            ))
        })?;
        let tools = exact_capability_tools(descriptor, template_name, capability, &exact_tools)?;
        let output_id = format!("{checkpoint_prefix}_capability_{segment}_output");
        initial_frontier_ids.push(output_id.clone());
        checkpoints.push(serde_json::json!({
            "id": format!("{checkpoint_prefix}_capability_{segment}_input"),
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_capability_{segment}_input", run.as_str()),
            "epoch": 0,
            "interface_binding": all_binding(interface),
            "outbound_interface_bindings": [all_binding(&provider)],
            "artifact_bindings": context_bindings,
            "required_verified_artifacts": []
        }));
        checkpoints.push(serde_json::json!({
            "id": output_id,
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_capability_{segment}_output", run.as_str()),
            "epoch": 0,
            "interface_binding": names_binding(
                &provider,
                [descriptor.artifact_labels.tool_call.clone()],
            )?,
            "outbound_interface_bindings": [names_binding(
                interface,
                [descriptor.artifact_labels.tool_call.clone()],
            )?],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": [{
                "mark": descriptor.private_run.tool_call_mark,
                "artifact_label": descriptor.artifact_labels.tool_call,
                "verifier": descriptor.private_run.tool_call_verifier,
                "params": { "tools": tools }
            }]
        }));
    }
    for (template_name, interface) in &operation_interfaces {
        let segment = worker_environment_segment(template_name);
        let capability = catalog
            .operation_capabilities
            .get(template_name)
            .ok_or_else(|| {
                contract(format!(
                    "operation interface template '{}' has no formed catalog entry",
                    template_name
                ))
            })?;
        let request_labels = capability
            .operations
            .iter()
            .map(|operation| operation.request_label.clone())
            .collect::<Vec<_>>();
        let output_id = format!("{checkpoint_prefix}_operation_{segment}_output");
        initial_frontier_ids.push(output_id.clone());
        checkpoints.push(serde_json::json!({
            "id": format!("{checkpoint_prefix}_operation_{segment}_input"),
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_operation_{segment}_input", run.as_str()),
            "epoch": 0,
            "interface_binding": all_binding(interface),
            "outbound_interface_bindings": [all_binding(&provider)],
            "artifact_bindings": context_bindings,
            "required_verified_artifacts": []
        }));
        checkpoints.push(serde_json::json!({
            "id": output_id,
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_operation_{segment}_output", run.as_str()),
            "epoch": 0,
            "interface_binding": names_binding(&provider, request_labels.clone())?,
            "outbound_interface_bindings": [names_binding(interface, request_labels)?],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": []
        }));
    }
    let shell_tool = exact_tool(&exact_tools, &descriptor.shell.tool)?;
    let terminal_output_id = format!("{checkpoint_prefix}_terminal_output");
    initial_frontier_ids.push(terminal_output_id.clone());
    let client_projection_labels = provider_client_projection_labels(&catalog.provider)?;
    let run_authoring_binding = private_run_authoring_binding(
        &run,
        &worker,
        &provider,
        &interface_authoring,
        private_run_authoring_checkpoints(
            descriptor,
            catalog,
            &terminal,
            &shell_terminal,
            &provider,
            &interface_authoring,
            &authoring_environment,
            &shell_environment,
            &worker,
            &capability_interfaces,
            &operation_interfaces,
            &exact_tools,
            &client_projection_labels,
        )?,
    )?;
    checkpoints.extend([
        serde_json::json!({
            "id": format!("{checkpoint_prefix}_shell_call"),
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_shell_call", run.as_str()),
            "epoch": 0,
            "interface_binding": all_binding(&shell_terminal),
            "outbound_interface_bindings": [all_binding(&shell_environment)],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": [{
                "mark": descriptor.private_run.tool_call_mark,
                "artifact_label": descriptor.artifact_labels.tool_call,
                "verifier": descriptor.private_run.tool_call_verifier,
                "params": { "tools": [shell_tool] }
            }]
        }),
        serde_json::json!({
            "id": format!("{checkpoint_prefix}_shell_result"),
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_shell_result", run.as_str()),
            "epoch": 0,
            "interface_binding": all_binding(&shell_environment),
            "outbound_interface_bindings": [all_binding(&shell_terminal)],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": []
        }),
        serde_json::json!({
            "id": terminal_output_id,
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_terminal_output", run.as_str()),
            "epoch": 0,
            "interface_binding": names_binding(&provider, client_projection_labels.clone())?,
            "outbound_interface_bindings": [names_binding(
                &terminal,
                client_projection_labels,
            )?],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": []
        }),
        serde_json::json!({
            "id": format!("{checkpoint_prefix}_authoring_call"),
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_authoring_call", run.as_str()),
            "epoch": 0,
            "interface_binding": all_binding(&interface_authoring),
            "outbound_interface_bindings": [all_binding(&authoring_environment)],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": []
        }),
        serde_json::json!({
            "id": format!("{checkpoint_prefix}_authoring_result"),
            "run": run,
            "trace": format!("trace:{}:{checkpoint_prefix}_authoring_result", run.as_str()),
            "epoch": 0,
            "interface_binding": all_binding(&authoring_environment),
            "outbound_interface_bindings": [all_binding(&provider)],
            "artifact_bindings": mandatory_role_bindings,
            "required_verified_artifacts": []
        }),
    ]);
    initial_frontier_ids.extend([
        format!("{checkpoint_prefix}_authoring_call"),
        format!("{checkpoint_prefix}_authoring_result"),
    ]);
    let active_checkpoints = checkpoints
        .iter()
        .map(|checkpoint| checkpoint["id"].clone())
        .collect::<Vec<_>>();
    let run_bytes = pretty_json(
        &serde_json::json!({
            "id": run,
            "ee": ee,
            "worker": worker,
            "trace": format!("trace:{}", run.as_str()),
            "status": "running",
            "interface_state": {
                (provider.as_str()): {
                    "context_generation": 0,
                    "authoring_generation": 0,
                    "frontier_checkpoint_ids": initial_frontier_ids,
                    "run_authoring_binding": run_authoring_binding
                }
            },
            "checkpoints": checkpoints,
            "active_checkpoints": active_checkpoints
        }),
        "private terminal run",
    )?;
    RunRecord::from_bytes(run_bytes.as_bytes())
        .map_err(|error| contract(format!("private terminal run failed validation: {error}")))?;

    plan_private_tool_writes(
        roots,
        descriptor,
        catalog,
        &authoring_environment,
        &capability_interfaces,
        &exact_tools,
        &mut writes,
    )?;
    plan_ee_capability_allowances(
        roots,
        catalog,
        &interface_replacements,
        exact_tools.values().cloned().collect(),
        &mut writes,
    )?;
    push_copied_interface_writes(
        &mut writes,
        roots,
        &catalog.terminal,
        &terminal,
        terminal_descriptor,
    )?;
    push_new_write(
        &mut writes,
        roots
            .registry_interface_root
            .join(terminal.as_str())
            .join("activation.json"),
        terminal_activation,
        PlannedWritePhase::InterfaceCompanion,
    )?;
    push_copied_interface_writes(
        &mut writes,
        roots,
        &catalog.shell_terminal,
        &shell_terminal,
        shell_terminal_descriptor,
    )?;
    push_new_write(
        &mut writes,
        roots
            .registry_interface_root
            .join(shell_terminal.as_str())
            .join("activation.json"),
        shell_terminal_activation,
        PlannedWritePhase::InterfaceCompanion,
    )?;
    push_copied_interface_writes(
        &mut writes,
        roots,
        &catalog.provider,
        &provider,
        provider_descriptor,
    )?;
    push_copied_interface_writes(
        &mut writes,
        roots,
        &catalog.interface_authoring,
        &interface_authoring,
        interface_authoring_descriptor,
    )?;
    push_new_write(
        &mut writes,
        roots
            .registry_interface_root
            .join(interface_authoring.as_str())
            .join("activation.json"),
        interface_authoring_activation,
        PlannedWritePhase::InterfaceCompanion,
    )?;
    push_copied_interface_writes(
        &mut writes,
        roots,
        &catalog.authoring_environment,
        &authoring_environment,
        authoring_environment_descriptor,
    )?;
    for (template, interface) in &capability_interfaces {
        let capability = catalog.capabilities.get(template).ok_or_else(|| {
            contract(format!(
                "planned capability template '{}' has no formed catalog entry",
                template
            ))
        })?;
        let interface_descriptor = capability_descriptors.get(template).ok_or_else(|| {
            contract(format!(
                "capability template '{}' has no planned descriptor",
                template
            ))
        })?;
        push_copied_interface_writes(
            &mut writes,
            roots,
            &capability.template,
            interface,
            interface_descriptor.clone(),
        )?;
        for (sidecar, content) in &capability.sidecars {
            push_new_write(
                &mut writes,
                roots
                    .registry_interface_root
                    .join(interface.as_str())
                    .join(sidecar),
                content.clone(),
                PlannedWritePhase::InterfaceCompanion,
            )?;
        }
    }
    for (template, interface) in &operation_interfaces {
        let capability = catalog
            .operation_capabilities
            .get(template)
            .ok_or_else(|| {
                contract(format!(
                    "planned operation template '{}' has no formed catalog entry",
                    template
                ))
            })?;
        let interface_descriptor = operation_descriptors.get(template).ok_or_else(|| {
            contract(format!(
                "operation template '{}' has no planned descriptor",
                template
            ))
        })?;
        push_copied_interface_writes(
            &mut writes,
            roots,
            &capability.template,
            interface,
            interface_descriptor.clone(),
        )?;
        push_private_operation_descriptor_writes(&mut writes, roots, capability, interface)?;
    }
    push_copied_interface_writes(
        &mut writes,
        roots,
        &catalog.worker,
        &worker,
        worker_descriptor,
    )?;
    push_new_write(
        &mut writes,
        roots
            .registry_interface_root
            .join(worker.as_str())
            .join("policy.json"),
        worker_policy,
        PlannedWritePhase::InterfaceCompanion,
    )?;
    if write_ee {
        push_new_write(
            &mut writes,
            roots
                .ee_instances_root
                .join(format!("{}.json", ee.as_str())),
            ee_bytes,
            PlannedWritePhase::EeInstance,
        )?;
    }
    push_new_write(
        &mut writes,
        roots
            .run_records_root
            .join(format!("{}.json", run.as_str())),
        run_bytes,
        PlannedWritePhase::RunRecord,
    )?;
    let mut unique_paths = BTreeSet::new();
    for write in &writes {
        if !unique_paths.insert(write.path.clone()) {
            return Err(contract(format!(
                "topology write plan repeats path '{}'",
                write.path.display()
            )));
        }
    }

    let mut worker_environment_interfaces = BTreeMap::new();
    let mut tool_interfaces = BTreeMap::new();
    for (template_name, interface) in &capability_interfaces {
        let capability = catalog.capabilities.get(template_name).ok_or_else(|| {
            contract(format!(
                "planned capability interface '{}' has no formed catalog entry",
                template_name
            ))
        })?;
        if capability.worker_environment_kinds.is_empty() {
            tool_interfaces.insert(template_name.clone(), interface.clone());
        } else {
            worker_environment_interfaces.insert(template_name.clone(), interface.clone());
        }
    }
    let topology = PrivateRunTopology {
        opaque_id,
        run,
        ee,
        terminal_interface: terminal.clone(),
        shell_terminal_interface: shell_terminal.clone(),
        provider_interface: provider,
        interface_authoring_interface: interface_authoring,
        authoring_environment_interface: authoring_environment,
        worker_environment_interfaces,
        tool_interfaces,
        operation_interfaces,
        exact_tools,
        shell_environment_interface: shell_environment,
        worker_interface: worker,
        model: request.model,
        reasoning_effort: request.reasoning_effort,
        operator_input_label: descriptor.artifact_labels.operator_input.clone(),
        turn_control_label: descriptor.artifact_labels.turn_control.clone(),
        terminal_socket: interface_control_socket_path(octopus_home, &terminal),
        shell_terminal_socket: interface_control_socket_path(octopus_home, &shell_terminal),
        interface_authoring_socket,
    };
    Ok((topology, writes))
}
fn child_interface_bytes(
    template: &InterfaceDescriptor,
    id: &InterfaceId,
    interface_driver_config: Option<serde_json::Value>,
) -> Result<String, TopologyError> {
    let mut descriptor = template.clone();
    descriptor.id = id.clone();
    if let Some(config) = interface_driver_config {
        let overlay: InterfaceDriverConfig =
            serde_json::from_value(config.clone()).map_err(|error| {
                contract(format!(
                    "private interface '{}' driver config is malformed: {error}",
                    id.as_str()
                ))
            })?;
        descriptor.interface_driver_config = Some(merge_interface_driver_config(
            template.interface_driver_config.as_ref(),
            overlay,
        ));
    }
    let bytes = pretty_typed(&descriptor, "private interface descriptor")?;
    let parsed = octopus_descriptors::parse_strict::<InterfaceDescriptor>(bytes.as_bytes())
        .map_err(|error| {
            contract(format!(
                "private interface '{}' failed descriptor validation: {error}",
                id.as_str()
            ))
        })?;
    if parsed.id != *id
        || parsed.interface_driver_ref != template.interface_driver_ref
        || parsed.interface_type != template.interface_type
        || parsed.role != template.role
        || parsed.direction != template.direction
    {
        return Err(contract(format!(
            "private interface '{}' changed its exact copied contract during encoding",
            id.as_str()
        )));
    }
    Ok(bytes)
}

fn validate_private_door_host_slots(
    catalog: &RootCatalog,
    overrides: &BTreeMap<String, BTreeMap<String, PathBuf>>,
) -> Result<(), TopologyError> {
    for (door, slots) in overrides {
        if !catalog.capabilities.contains_key(door) {
            return Err(contract(format!(
                "private run host-slot override names unknown capability door '{door}'"
            )));
        }
        if slots.is_empty() {
            return Err(contract(format!(
                "private run host-slot override for door '{door}' is empty"
            )));
        }
        for (slot, path) in slots {
            if slot.trim().is_empty() || slot != slot.trim() {
                return Err(contract(format!(
                    "private run host-slot override for door '{door}' has an invalid slot name"
                )));
            }
            if !path.is_absolute() || path.as_os_str().is_empty() {
                return Err(contract(format!(
                    "private run host-slot override '{door}.{slot}' is not an absolute path: '{}'",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn host_slot_overlay(slots: &BTreeMap<String, PathBuf>) -> serde_json::Value {
    serde_json::json!({
        "host_slots": slots
            .iter()
            .map(|(slot, path)| {
                (slot.clone(), serde_json::json!({ "path": path }))
            })
            .collect::<serde_json::Map<String, serde_json::Value>>()
    })
}

fn merge_interface_driver_config(
    template: Option<&InterfaceDriverConfig>,
    overlay: InterfaceDriverConfig,
) -> InterfaceDriverConfig {
    let Some(template) = template else {
        return overlay;
    };
    let mut private_config = template.private_config.clone();
    private_config.extend(overlay.private_config);
    let mut host_slots = template.host_slots.clone();
    host_slots.extend(overlay.host_slots);
    InterfaceDriverConfig {
        private_config,
        residual_ambient: overlay
            .residual_ambient
            .or_else(|| template.residual_ambient.clone()),
        host_slots,
        transport: overlay.transport.or_else(|| template.transport.clone()),
        supported_reasoning_efforts: if overlay.supported_reasoning_efforts.is_empty() {
            template.supported_reasoning_efforts.clone()
        } else {
            overlay.supported_reasoning_efforts
        },
    }
}

fn push_copied_interface_writes(
    writes: &mut Vec<PlannedWrite>,
    roots: &TerminalHostRoots,
    template: &InterfaceDescriptor,
    interface: &InterfaceId,
    descriptor: String,
) -> Result<(), TopologyError> {
    let target = roots.registry_interface_root.join(interface.as_str());
    push_new_write(
        writes,
        target.join("interface.json"),
        descriptor,
        PlannedWritePhase::InterfaceDescriptor,
    )?;
    let source_life = roots
        .registry_interface_root
        .join(template.id.as_str())
        .join("life.json");
    if source_life.is_file() {
        let bytes = read_bytes("interface template life", &source_life)?;
        let life: SpawnLifeConfig =
            serde_json::from_slice(&bytes).map_err(|error| TopologyError::Malformed {
                surface: "interface template life",
                path: source_life.display().to_string(),
                reason: error.to_string(),
            })?;
        push_new_write(
            writes,
            target.join("life.json"),
            pretty_typed(&life, "private interface life")?,
            PlannedWritePhase::InterfaceCompanion,
        )?;
    }
    Ok(())
}

fn push_private_operation_descriptor_writes(
    writes: &mut Vec<PlannedWrite>,
    roots: &TerminalHostRoots,
    capability: &OperationCapabilityCatalog,
    interface: &InterfaceId,
) -> Result<(), TopologyError> {
    let target = roots
        .registry_interface_root
        .join(interface.as_str())
        .join(&capability.catalog_dir);
    for operation in &capability.operations {
        push_new_write(
            writes,
            target.join(format!("{}.json", operation.id)),
            pretty_json(
                &operation.descriptor,
                "private interface operation descriptor",
            )?,
            PlannedWritePhase::InterfaceCompanion,
        )?;
    }
    Ok(())
}

fn push_new_write(
    writes: &mut Vec<PlannedWrite>,
    path: PathBuf,
    content: String,
    phase: PlannedWritePhase,
) -> Result<(), TopologyError> {
    if path.exists() {
        let existing = read_bytes("fresh topology target", &path)?;
        if existing == content.as_bytes() {
            return Ok(());
        }
        return Err(TopologyError::Conflict(format!(
            "fresh topology target '{}' already exists with different bytes",
            path.display()
        )));
    }
    let after_hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
    writes.push(PlannedWrite {
        path,
        content,
        before_hash: None,
        after_hash,
        phase,
    });
    Ok(())
}

fn push_existing_write(
    writes: &mut Vec<PlannedWrite>,
    path: PathBuf,
    before: Vec<u8>,
    content: String,
) -> Result<(), TopologyError> {
    if before == content.as_bytes() {
        return Ok(());
    }
    writes.push(PlannedWrite {
        path,
        content: content.clone(),
        before_hash: Some(ContentHash::of_bytes(&before).to_hex()),
        after_hash: ContentHash::of_bytes(content.as_bytes()).to_hex(),
        phase: PlannedWritePhase::SharedConsent,
    });
    Ok(())
}

fn commit_client_writes(writes: &[PlannedWrite]) -> Result<(), TopologyError> {
    let mut unique = BTreeSet::new();
    for write in writes {
        if !unique.insert(write.path.clone()) {
            return Err(contract(format!(
                "client authoring repeats path '{}'",
                write.path.display()
            )));
        }
        let observed = optional_content_hash(&write.path)?;
        if observed.as_deref() != write.before_hash.as_deref() {
            return Err(TopologyError::Conflict(format!(
                "client authoring conflict for '{}': expected {}, observed {}",
                write.path.display(),
                write.before_hash.as_deref().unwrap_or("<absent>"),
                observed.as_deref().unwrap_or("<absent>")
            )));
        }
    }
    let mut ordered = writes.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        left.phase
            .cmp(&right.phase)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut committed = Vec::new();
    for write in ordered {
        let parent = write.path.parent().ok_or_else(|| {
            contract(format!(
                "client authoring target '{}' has no parent",
                write.path.display()
            ))
        })?;
        std::fs::create_dir_all(parent).map_err(|source| TopologyError::Read {
            surface: "client authoring directory",
            path: parent.display().to_string(),
            source,
        })?;
        let staged = parent.join(format!(
            ".{}.octopus-client-{}.tmp",
            write
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| contract("client authoring target has no UTF-8 file name"))?,
            uuid::Uuid::new_v4().simple()
        ));
        let result = (|| -> Result<(), TopologyError> {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&staged)
                .map_err(|source| TopologyError::Read {
                    surface: "client authoring staging file",
                    path: staged.display().to_string(),
                    source,
                })?;
            std::io::Write::write_all(&mut file, write.content.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|source| TopologyError::Read {
                    surface: "client authoring staging file",
                    path: staged.display().to_string(),
                    source,
                })?;
            let observed = optional_content_hash(&write.path)?;
            if observed.as_deref() != write.before_hash.as_deref() {
                return Err(TopologyError::Conflict(format!(
                    "client authoring conflict for '{}' after staging: expected {}, observed {}",
                    write.path.display(),
                    write.before_hash.as_deref().unwrap_or("<absent>"),
                    observed.as_deref().unwrap_or("<absent>")
                )));
            }
            if write.before_hash.is_none() {
                std::fs::hard_link(&staged, &write.path).map_err(|source| {
                    if source.kind() == std::io::ErrorKind::AlreadyExists {
                        TopologyError::Conflict(format!(
                            "fresh client authoring target '{}' appeared during commit",
                            write.path.display()
                        ))
                    } else {
                        TopologyError::Read {
                            surface: "client-authored file",
                            path: write.path.display().to_string(),
                            source,
                        }
                    }
                })?;
                std::fs::remove_file(&staged).map_err(|source| TopologyError::Read {
                    surface: "client authoring staging file",
                    path: staged.display().to_string(),
                    source,
                })?;
            } else {
                std::fs::rename(&staged, &write.path).map_err(|source| TopologyError::Read {
                    surface: "client-authored file",
                    path: write.path.display().to_string(),
                    source,
                })?;
            }
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|source| TopologyError::Read {
                    surface: "client authoring directory",
                    path: parent.display().to_string(),
                    source,
                })?;
            let installed = optional_content_hash(&write.path)?;
            if installed.as_deref() != Some(write.after_hash.as_str()) {
                return Err(contract(format!(
                    "client authoring installed unexpected bytes at '{}'",
                    write.path.display()
                )));
            }
            Ok(())
        })();
        let _ = std::fs::remove_file(&staged);
        if let Err(error) = result {
            return match error {
                TopologyError::Conflict(message) => Err(TopologyError::Conflict(format!(
                    "{message}; client-authored files already installed [{}]",
                    committed.join(", ")
                ))),
                other => Err(contract(format!(
                    "{other}; client-authored files already installed [{}]",
                    committed.join(", ")
                ))),
            };
        }
        committed.push(write.path.display().to_string());
    }
    Ok(())
}

fn optional_content_hash(path: &Path) -> Result<Option<String>, TopologyError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(ContentHash::of_bytes(&bytes).to_hex())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(TopologyError::Read {
            surface: "client authoring target",
            path: path.display().to_string(),
            source,
        }),
    }
}

async fn wait_for_socket(socket: &Path, timeout_ms: u64) -> Result<(), TopologyError> {
    wait_for_path(
        socket,
        timeout_ms,
        format!("private terminal socket '{}'", socket.display()),
    )
    .await
}

async fn wait_for_reader_change(
    reader: &mut TerminalOutboundReader,
    deadline: Instant,
    timeout_ms: u64,
    waiting_for: String,
) -> Result<(), TopologyError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(TopologyError::Timeout {
            timeout_ms,
            waiting_for,
        });
    }
    match tokio::time::timeout(remaining, reader.wait_for_change()).await {
        Ok(result) => result.map_err(TopologyError::from),
        Err(_) => Err(TopologyError::Timeout {
            timeout_ms,
            waiting_for,
        }),
    }
}

async fn wait_for_path(
    path: &Path,
    timeout_ms: u64,
    waiting_for: String,
) -> Result<(), TopologyError> {
    if path.exists() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| contract(format!("watched path '{}' has no parent", path.display())))?;
    let (watch_root, mode) = if parent.is_dir() {
        (parent, RecursiveMode::NonRecursive)
    } else {
        let ancestor = parent
            .parent()
            .filter(|ancestor| ancestor.is_dir())
            .ok_or_else(|| {
                contract(format!(
                    "watched path '{}' has no existing interface directory",
                    path.display()
                ))
            })?;
        (ancestor, RecursiveMode::Recursive)
    };
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    })
    .map_err(|error| {
        contract(format!(
            "filesystem watcher for '{}' cannot be created: {error}",
            path.display()
        ))
    })?;
    watcher.watch(watch_root, mode).map_err(|error| {
        contract(format!(
            "interface directory '{}' cannot be watched: {error}",
            watch_root.display()
        ))
    })?;
    if path.exists() {
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(TopologyError::Timeout {
                timeout_ms,
                waiting_for,
            });
        }
        let _event = tokio::time::timeout(remaining, events.recv())
            .await
            .map_err(|_| TopologyError::Timeout {
                timeout_ms,
                waiting_for: waiting_for.clone(),
            })?
            .ok_or_else(|| contract("filesystem watcher channel closed"))?
            .map_err(|error| contract(format!("filesystem watcher failed: {error}")))?;
        if path.exists() {
            return Ok(());
        }
    }
}

fn load_template_interface(
    registry_root: &Path,
    id: &str,
) -> Result<InterfaceDescriptor, TopologyError> {
    load_interface(registry_root, &InterfaceId::new(id))
}

fn validate_private_run_model_selection(
    catalog: &RootCatalog,
    request: &PrivateRunRequest,
) -> Result<(), TopologyError> {
    let ee_chain = if catalog.ee_template.id == catalog.parent_ee_template.id {
        vec![&catalog.parent_ee_template]
    } else {
        vec![&catalog.parent_ee_template, &catalog.ee_template]
    };
    let formed_models =
        formed_provider_models(&catalog.provider, &catalog.worker_policy, &ee_chain)?;
    let config = catalog
        .provider
        .interface_driver_config
        .as_ref()
        .ok_or_else(|| {
            contract(format!(
                "provider interface '{}' carries no driver config",
                catalog.provider.id.as_str()
            ))
        })?;
    validate_provider_model_selection(
        config,
        &catalog.provider.id,
        &request.model,
        &request.reasoning_effort,
        &formed_models,
    )
    .map(|_| ())
}

fn load_live_formed_provider_models(
    registry_root: &Path,
    ee_instances_root: &Path,
    ee: &EeId,
    worker: &InterfaceId,
    provider: &InterfaceDescriptor,
) -> Result<BTreeSet<String>, TopologyError> {
    let worker_policy = load_worker_policy(registry_root, worker)?;
    let live_chain = resolve_live_ee_policy_chain(ee_instances_root, ee).map_err(|error| {
        contract(format!(
            "provider interface '{}' live EE chain failed: {error}",
            provider.id.as_str()
        ))
    })?;
    let templates = live_chain
        .iter()
        .map(|layer| load_ee_template(registry_root, layer.template.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let chain = templates.iter().collect::<Vec<_>>();
    let instance_policy_narrowing = live_chain
        .iter()
        .map(|layer| layer.policy_narrowing.clone())
        .collect::<Vec<_>>();
    formed_provider_models_with_instance_policy(
        provider,
        &worker_policy,
        &chain,
        &instance_policy_narrowing,
    )
}

fn formed_provider_models(
    provider: &InterfaceDescriptor,
    worker_policy: &WorkerInterfacePolicy,
    ee_chain: &[&EeTemplateDescriptor],
) -> Result<BTreeSet<String>, TopologyError> {
    formed_provider_models_with_instance_policy(provider, worker_policy, ee_chain, &[])
}

fn formed_provider_models_with_instance_policy(
    provider: &InterfaceDescriptor,
    worker_policy: &WorkerInterfacePolicy,
    ee_chain: &[&EeTemplateDescriptor],
    instance_policy_narrowing: &[EeInstancePolicyNarrowing],
) -> Result<BTreeSet<String>, TopologyError> {
    if ee_chain.is_empty() {
        return Err(contract(format!(
            "provider interface '{}' cannot form models without an EE chain",
            provider.id.as_str()
        )));
    }
    if !instance_policy_narrowing.is_empty() && instance_policy_narrowing.len() != ee_chain.len() {
        return Err(contract(format!(
            "provider interface '{}' EE chain has {} templates and {} instance-policy layers",
            provider.id.as_str(),
            ee_chain.len(),
            instance_policy_narrowing.len()
        )));
    }
    if !worker_policy.ceiling.interfaces.contains(&provider.id) {
        return Err(contract(format!(
            "worker '{}' does not admit provider interface '{}'",
            worker_policy.id.as_str(),
            provider.id.as_str()
        )));
    }
    for interface in worker_policy.ceiling.interface_policy.keys() {
        if !worker_policy.ceiling.interfaces.contains(interface) {
            return Err(contract(format!(
                "worker '{}' carries interface policy for '{}' outside its interface ceiling",
                worker_policy.id.as_str(),
                interface.as_str()
            )));
        }
    }
    for (index, template) in ee_chain.iter().enumerate() {
        if !optional_axis_allows(&template.allowed_interfaces, provider.id.as_str()) {
            return Err(contract(format!(
                "EE template '{}' does not admit provider interface '{}'",
                template.id.as_str(),
                provider.id.as_str()
            )));
        }
        if let Some(instance) = instance_policy_narrowing.get(index) {
            if !optional_axis_allows(&instance.allowed_interfaces, provider.id.as_str()) {
                return Err(contract(format!(
                    "EE instance of template '{}' does not admit provider interface '{}'",
                    template.id.as_str(),
                    provider.id.as_str()
                )));
            }
        }
    }
    let mut ee_layers = Vec::new();
    for (index, template) in ee_chain.iter().enumerate() {
        if let Some(layer) = template.interface_policy_overrides.get(&provider.id) {
            ee_layers.push(layer);
        }
        if let Some(layer) = instance_policy_narrowing
            .get(index)
            .and_then(|instance| instance.interface_policy_overrides.get(&provider.id))
        {
            ee_layers.push(layer);
        }
    }
    let formed = octopus_policy::form_interface_policy(
        &provider.interface_policy,
        worker_policy.ceiling.interface_policy.get(&provider.id),
        &ee_layers,
    )
    .map_err(|error| {
        contract(format!(
            "provider interface '{}' policy formation failed: {error}",
            provider.id.as_str()
        ))
    })?;
    let values = formed
        .context
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            contract(format!(
                "provider interface '{}' formed policy carries no models set",
                provider.id.as_str()
            ))
        })?;
    let mut models = BTreeSet::new();
    for value in values {
        let model = value
            .as_str()
            .filter(|model| !model.is_empty() && model.trim() == *model)
            .ok_or_else(|| {
                contract(format!(
                    "provider interface '{}' formed models contains a malformed entry",
                    provider.id.as_str()
                ))
            })?;
        if !models.insert(model.to_string()) {
            return Err(contract(format!(
                "provider interface '{}' formed models repeats '{}'",
                provider.id.as_str(),
                model
            )));
        }
    }
    if models.is_empty() {
        return Err(contract(format!(
            "provider interface '{}' formed models set is empty",
            provider.id.as_str()
        )));
    }
    Ok(models)
}

fn configured_reasoning_effort(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
) -> Result<String, TopologyError> {
    match config.private_config.get("reasoning_effort") {
        None => Ok(String::new()),
        Some(serde_json::Value::String(effort)) => Ok(effort.clone()),
        Some(_) => Err(contract(format!(
            "provider interface '{}' reasoning_effort must be a string",
            interface.as_str()
        ))),
    }
}

fn provider_model_catalog(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
    formed_models: &BTreeSet<String>,
) -> Result<Vec<ProviderModelPreset>, TopologyError> {
    let value = config
        .private_config
        .get("model_catalog")
        .cloned()
        .ok_or_else(|| {
            contract(format!(
                "provider interface '{}' declares no private_config.model_catalog",
                interface.as_str()
            ))
        })?;
    let catalog = serde_json::from_value::<Vec<ProviderModelPreset>>(value).map_err(|error| {
        contract(format!(
            "provider interface '{}' model catalog is malformed: {error}",
            interface.as_str()
        ))
    })?;
    if catalog.is_empty() {
        return Err(contract(format!(
            "provider interface '{}' model catalog is empty",
            interface.as_str()
        )));
    }
    let mut models = BTreeSet::new();
    for preset in &catalog {
        require_non_empty("provider model catalog model", &preset.model)?;
        require_non_empty("provider model catalog display name", &preset.display_name)?;
        if !models.insert(preset.model.clone()) {
            return Err(contract(format!(
                "provider interface '{}' model catalog repeats model '{}'",
                interface.as_str(),
                preset.model
            )));
        }
        let mut efforts = BTreeSet::new();
        for effort in &preset.supported_reasoning_efforts {
            require_non_empty("provider model reasoning effort", &effort.effort)?;
            if !efforts.insert(effort.effort.as_str()) {
                return Err(contract(format!(
                    "provider interface '{}' model '{}' repeats reasoning effort '{}'",
                    interface.as_str(),
                    preset.model,
                    effort.effort
                )));
            }
            if !config.supported_reasoning_efforts.is_empty()
                && !config
                    .supported_reasoning_efforts
                    .iter()
                    .any(|supported| supported == &effort.effort)
            {
                return Err(contract(format!(
                    "provider interface '{}' model '{}' declares reasoning effort '{}' outside its supported_reasoning_efforts ceiling",
                    interface.as_str(),
                    preset.model,
                    effort.effort
                )));
            }
        }
        if if efforts.is_empty() {
            !preset.default_reasoning_effort.is_empty()
        } else {
            !efforts.contains(preset.default_reasoning_effort.as_str())
        } {
            return Err(contract(format!(
                "provider interface '{}' model '{}' default reasoning effort '{}' is not in its supported list",
                interface.as_str(),
                preset.model,
                preset.default_reasoning_effort
            )));
        }
    }
    let missing = formed_models
        .difference(&models)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(contract(format!(
            "provider interface '{}' formed models are absent from its catalog [{}]",
            interface.as_str(),
            missing.join(", ")
        )));
    }
    let catalog = catalog
        .into_iter()
        .filter(|preset| formed_models.contains(&preset.model))
        .collect::<Vec<_>>();
    if catalog.is_empty() {
        return Err(contract(format!(
            "provider interface '{}' formed model catalog is empty",
            interface.as_str()
        )));
    }
    Ok(catalog)
}

fn validate_provider_model_selection(
    config: &InterfaceDriverConfig,
    interface: &InterfaceId,
    model: &str,
    reasoning_effort: &str,
    formed_models: &BTreeSet<String>,
) -> Result<Vec<ProviderModelPreset>, TopologyError> {
    let catalog = provider_model_catalog(config, interface, formed_models)?;
    let preset = catalog
        .iter()
        .find(|preset| preset.model == model)
        .ok_or_else(|| {
            contract(format!(
                "provider interface '{}' formed model catalog does not admit model '{}'",
                interface.as_str(),
                model
            ))
        })?;
    if if preset.supported_reasoning_efforts.is_empty() {
        !reasoning_effort.is_empty()
    } else {
        !preset.supported_reasoning_efforts.iter().any(|supported| supported.effort == reasoning_effort)
    } {
        return Err(contract(format!(
            "provider interface '{}' model '{}' does not support reasoning effort '{}'; supported values are [{}]",
            interface.as_str(),
            model,
            reasoning_effort,
            preset
                .supported_reasoning_efforts
                .iter()
                .map(|supported| supported.effort.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(catalog)
}

fn load_interface(
    registry_root: &Path,
    id: &InterfaceId,
) -> Result<InterfaceDescriptor, TopologyError> {
    let path = registry_root
        .join("interface")
        .join(id.as_str())
        .join("interface.json");
    let bytes = read_bytes("interface descriptor", &path)?;
    let descriptor =
        octopus_descriptors::parse_strict::<InterfaceDescriptor>(&bytes).map_err(|error| {
            TopologyError::Malformed {
                surface: "interface descriptor",
                path: path.display().to_string(),
                reason: error.to_string(),
            }
        })?;
    if descriptor.id != *id {
        return Err(contract(format!(
            "interface path '{}' declares id '{}'",
            path.display(),
            descriptor.id.as_str()
        )));
    }
    Ok(descriptor)
}

fn validate_exact_tool_descriptor(
    registry_root: &Path,
    base_tool: &str,
    exact_tool: &str,
    exact_door: &InterfaceId,
) -> Result<(), TopologyError> {
    let tool = load_tool_descriptor_from_dir(&registry_root.join("tool"), exact_tool)
        .map_err(|error| {
            contract(format!(
                "private tool descriptor '{}' could not be loaded: {error}",
                exact_tool
            ))
        })?
        .ok_or_else(|| contract(format!("private tool '{}' is absent", exact_tool)))?;
    if tool.id.as_str() != exact_tool || tool.interface != *exact_door {
        return Err(contract(format!(
            "private tool '{}' does not belong to exact door '{}'",
            exact_tool,
            exact_door.as_str()
        )));
    }
    let base = load_tool_descriptor_from_dir(&registry_root.join("tool"), base_tool)
        .map_err(|error| {
            contract(format!(
                "base tool descriptor '{}' could not be loaded: {error}",
                base_tool
            ))
        })?
        .ok_or_else(|| contract(format!("base tool '{}' is absent", base_tool)))?;
    let mut expected = base;
    expected.id = ToolDescriptorId::new(exact_tool);
    expected.interface = exact_door.clone();
    validate_private_tool_operation(&expected, base_tool)?;
    if tool != expected {
        return Err(contract(format!(
            "private tool '{}' changed behavior beyond exact identity and door ownership",
            exact_tool
        )));
    }
    Ok(())
}

fn capability_interfaces_from_topology(
    worker_environment_interfaces: &BTreeMap<String, InterfaceId>,
    tool_interfaces: &BTreeMap<String, InterfaceId>,
) -> BTreeMap<String, InterfaceId> {
    worker_environment_interfaces
        .iter()
        .chain(tool_interfaces.iter())
        .map(|(template, interface)| (template.clone(), interface.clone()))
        .collect()
}

fn validate_private_policy_and_consents(
    registry_root: &Path,
    catalog: &RootCatalog,
    worker: &InterfaceDescriptor,
    exact_tools: &BTreeMap<String, String>,
    interface_replacements: &BTreeMap<String, BTreeSet<InterfaceId>>,
    exact_interfaces: Vec<InterfaceId>,
) -> Result<(), TopologyError> {
    let private_policy = load_worker_policy(registry_root, &worker.id)?;
    let mut expected_policy = catalog.worker_policy.clone();
    expected_policy.id = worker.id.clone();
    replace_private_worker_interfaces(&mut expected_policy, interface_replacements)?;
    replace_private_worker_tools(&mut expected_policy, exact_tools)?;
    if private_policy != expected_policy {
        return Err(contract(format!(
            "private worker '{}' policy does not carry the exact topology ceiling",
            worker.id.as_str()
        )));
    }

    for template in BTreeSet::from([
        catalog.parent_ee_template.id.clone(),
        catalog.ee_template.id.clone(),
    ]) {
        let ee = load_ee_template(registry_root, template.as_str())?;
        if let Some(allowed) = &ee.allowed_interfaces {
            let allowed = allowed.iter().map(String::as_str).collect::<BTreeSet<_>>();
            for interface in &exact_interfaces {
                if !allowed.contains(interface.as_str()) {
                    return Err(contract(format!(
                        "EE template '{}' does not allow exact interface '{}'",
                        template.as_str(),
                        interface.as_str()
                    )));
                }
            }
        }
        if let Some(allowed) = &ee.allowed_tools {
            let allowed = allowed.iter().map(String::as_str).collect::<BTreeSet<_>>();
            for tool in exact_tools.values() {
                if !allowed.contains(tool.as_str()) {
                    return Err(contract(format!(
                        "EE template '{}' does not allow exact tool '{}'",
                        template.as_str(),
                        tool
                    )));
                }
            }
        }
    }

    let wek_registry = octopus_worker_env::WorkerEnvKindRegistry::load_from_dir(
        &registry_root.join("worker_environment_kind"),
    )
    .map_err(|error| contract(format!("worker-environment registry is invalid: {error}")))?;
    for capability in catalog.capabilities.values() {
        for (base_tool, kind) in &capability.tool_worker_environment_kinds {
            if !capability.tools.contains(base_tool) {
                return Err(contract(format!(
                    "worker-environment classification names unformed tool '{}'",
                    base_tool
                )));
            }
            wek_registry.get(kind).map_err(|error| {
                contract(format!(
                    "worker-environment kind '{}' is unavailable: {error}",
                    kind.as_str()
                ))
            })?;
        }
    }
    Ok(())
}

pub fn load_interface_host_io_limits(
    registry_root: &Path,
    interface: &InterfaceId,
) -> Result<InterfaceHostIoLimits, TopologyError> {
    let config = spawn_life_config_of(registry_root, interface)
        .map_err(contract)?
        .ok_or_else(|| {
            contract(format!(
                "interface '{}' resolves no life",
                interface.as_str()
            ))
        })?;
    let mut processes = config.processes.into_iter();
    let limits = processes
        .next()
        .ok_or_else(|| {
            contract(format!(
                "interface '{}' life has no process",
                interface.as_str()
            ))
        })?
        .io_limits;
    if processes.any(|process| process.io_limits != limits) {
        return Err(contract(format!(
            "interface '{}' life processes declare different host I/O limits",
            interface.as_str()
        )));
    }
    limits.validate().map_err(|error| {
        contract(format!(
            "interface '{}' resolves invalid host_io_limits: {error}",
            interface.as_str()
        ))
    })?;
    Ok(limits)
}

fn load_json_sidecar(
    registry_root: &Path,
    interface: &InterfaceId,
    name: &str,
    surface: &'static str,
) -> Result<String, TopologyError> {
    require_sidecar_name(surface, name)?;
    let path = registry_root
        .join("interface")
        .join(interface.as_str())
        .join(name);
    let bytes = read_bytes(surface, &path)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| TopologyError::Malformed {
            surface,
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    if !value.is_object() {
        return Err(TopologyError::Malformed {
            surface,
            path: path.display().to_string(),
            reason: "sidecar root must be a JSON object".to_string(),
        });
    }
    pretty_json(&value, surface)
}

fn load_worker_policy(
    registry_root: &Path,
    worker: &InterfaceId,
) -> Result<WorkerInterfacePolicy, TopologyError> {
    let path = registry_root
        .join("interface")
        .join(worker.as_str())
        .join("policy.json");
    let bytes = read_bytes("worker interface policy", &path)?;
    let policy: WorkerInterfacePolicy =
        serde_json::from_slice(&bytes).map_err(|error| TopologyError::Malformed {
            surface: "worker interface policy",
            path: path.display().to_string(),
            reason: error.to_string(),
        })?;
    if policy.id != *worker {
        return Err(contract(format!(
            "worker policy path '{}' declares id '{}'",
            path.display(),
            policy.id.as_str()
        )));
    }
    Ok(policy)
}

fn load_ee_template(registry_root: &Path, id: &str) -> Result<EeTemplateDescriptor, TopologyError> {
    let path = registry_root
        .join("ee_template")
        .join(id)
        .join("ee_template.json");
    let bytes = read_bytes("EE template", &path)?;
    let descriptor =
        octopus_descriptors::parse_strict::<EeTemplateDescriptor>(&bytes).map_err(|error| {
            TopologyError::Malformed {
                surface: "EE template",
                path: path.display().to_string(),
                reason: error.to_string(),
            }
        })?;
    if descriptor.id.as_str() != id {
        return Err(contract(format!(
            "EE template path '{}' declares id '{}'",
            path.display(),
            descriptor.id.as_str()
        )));
    }
    Ok(descriptor)
}

fn metadata_string(
    config: &InterfaceDriverConfig,
    key: &str,
    interface: &InterfaceId,
) -> Result<String, TopologyError> {
    config
        .private_config
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            contract(format!(
                "interface '{}' invocation metadata has no non-empty '{}'",
                interface.as_str(),
                key
            ))
        })
}

fn index_authored_checkpoint_templates(
    checkpoints: Vec<AuthoredCheckpointTemplate>,
) -> Result<BTreeMap<String, AuthoredCheckpointTemplate>, TopologyError> {
    if checkpoints.is_empty() {
        return Err(contract("terminal-owned authoring topology is empty"));
    }
    let mut indexed = BTreeMap::new();
    for checkpoint in checkpoints {
        require_id_fragment("terminal-owned checkpoint name", &checkpoint.name)?;
        checkpoint.interface_binding.validate().map_err(contract)?;
        if checkpoint.outbound_interface_bindings.is_empty() {
            return Err(contract(format!(
                "terminal-owned checkpoint '{}' has no outbound interfaces",
                checkpoint.name
            )));
        }
        let mut outbound = BTreeSet::new();
        for target in &checkpoint.outbound_interface_bindings {
            target.validate().map_err(contract)?;
            if !outbound.insert(target.interface.as_str()) {
                return Err(contract(format!(
                    "terminal-owned checkpoint '{}' repeats outbound interface '{}'",
                    checkpoint.name,
                    target.interface.as_str()
                )));
            }
        }
        let mut labels = BTreeSet::new();
        for required in &checkpoint.required_verified_artifacts {
            required
                .validate(&format!("terminal-owned checkpoint '{}'", checkpoint.name))
                .map_err(contract)?;
            if !labels.insert(required.artifact_label.as_str()) {
                return Err(contract(format!(
                    "terminal-owned checkpoint '{}' repeats required artifact label '{}'",
                    checkpoint.name, required.artifact_label
                )));
            }
        }
        let name = checkpoint.name.clone();
        if indexed.insert(name.clone(), checkpoint).is_some() {
            return Err(contract(format!(
                "terminal-owned checkpoints repeat name '{name}'"
            )));
        }
    }
    Ok(indexed)
}

fn validate_active_terminal_topology(
    record: &RunRecord,
    templates: &BTreeMap<String, AuthoredCheckpointTemplate>,
    provider: &InterfaceId,
    mandatory_role_artifacts: &[ArtifactId],
) -> Result<(), TopologyError> {
    let active = record
        .active_checkpoints
        .iter()
        .map(|checkpoint| checkpoint.as_str().to_string())
        .collect::<BTreeSet<_>>();
    let mut matched = BTreeSet::new();
    let mut expected_frontier = BTreeSet::new();
    for template in templates.values() {
        let matches = record
            .checkpoints
            .iter()
            .filter(|checkpoint| active.contains(checkpoint.id.as_str()))
            .filter(|checkpoint| {
                checkpoint.interface_binding.as_ref() == Some(&template.interface_binding)
                    && checkpoint.outbound_interface_bindings
                        == template.outbound_interface_bindings
                    && checkpoint.required_verified_artifacts
                        == template.required_verified_artifacts
                    && checkpoint_role_bindings_match(
                        checkpoint,
                        mandatory_role_artifacts,
                        template.include_context,
                    )
            })
            .collect::<Vec<_>>();
        let [checkpoint] = matches.as_slice() else {
            return Err(contract(format!(
                "private run '{}' has {} active checkpoints matching terminal-owned route '{}'",
                record.id.as_str(),
                matches.len(),
                template.name
            )));
        };
        if !matched.insert(checkpoint.id.as_str().to_string()) {
            return Err(contract(format!(
                "private run '{}' active checkpoint '{}' matches more than one terminal-owned route",
                record.id.as_str(),
                checkpoint.id.as_str()
            )));
        }
        if matches!(
            template.role,
            AuthoredCheckpointRole::ProviderClientOutput
                | AuthoredCheckpointRole::ProviderToolOutput
                | AuthoredCheckpointRole::ProviderInterfaceOutput
                | AuthoredCheckpointRole::AuthoringCall
                | AuthoredCheckpointRole::AuthoringResult
        ) {
            expected_frontier.insert(checkpoint.id.as_str().to_string());
        }
    }
    if matched != active {
        return Err(contract(format!(
            "private run '{}' active checkpoint set is not the terminal-owned topology",
            record.id.as_str()
        )));
    }
    let frontier = record
        .interface_state
        .get(provider)
        .and_then(|state| state.get("frontier_checkpoint_ids"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            contract(format!(
                "private run '{}' provider '{}' has no frontier_checkpoint_ids array",
                record.id.as_str(),
                provider.as_str()
            ))
        })?;
    let mut actual_frontier = BTreeSet::new();
    for value in frontier {
        let checkpoint = value
            .as_str()
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| {
                contract(format!(
                    "private run '{}' provider frontier contains a malformed checkpoint id",
                    record.id.as_str()
                ))
            })?;
        if !actual_frontier.insert(checkpoint.to_string()) {
            return Err(contract(format!(
                "private run '{}' provider frontier repeats checkpoint '{}'",
                record.id.as_str(),
                checkpoint
            )));
        }
    }
    if actual_frontier != expected_frontier {
        return Err(contract(format!(
            "private run '{}' provider frontier does not exactly own its output and authoring checkpoints",
            record.id.as_str()
        )));
    }
    Ok(())
}

fn checkpoint_role_bindings_match(
    checkpoint: &octopus_run_record::CheckpointRecord,
    mandatory_role_artifacts: &[ArtifactId],
    allow_additional_context: bool,
) -> bool {
    if checkpoint.artifact_bindings.len() < mandatory_role_artifacts.len()
        || (!allow_additional_context
            && checkpoint.artifact_bindings.len() != mandatory_role_artifacts.len())
    {
        return false;
    }
    checkpoint
        .artifact_bindings
        .iter()
        .zip(mandatory_role_artifacts)
        .all(|(binding, artifact)| binding.id == *artifact)
}

fn authored_checkpoint<'a>(
    checkpoints: &'a BTreeMap<String, AuthoredCheckpointTemplate>,
    name: &str,
    interface: &InterfaceId,
) -> Result<&'a AuthoredCheckpointTemplate, TopologyError> {
    checkpoints.get(name).ok_or_else(|| {
        contract(format!(
            "interface '{}' run_authoring checkpoints have no '{}'",
            interface.as_str(),
            name
        ))
    })
}

fn require_checkpoint_route(
    checkpoint: &AuthoredCheckpointTemplate,
    name: &str,
    interface: &InterfaceId,
    outbound: &[&InterfaceId],
    include_context: bool,
) -> Result<(), TopologyError> {
    let expected = outbound
        .iter()
        .map(|interface| interface.as_str())
        .collect::<BTreeSet<_>>();
    let actual = checkpoint
        .outbound_interfaces()
        .map(InterfaceId::as_str)
        .collect::<BTreeSet<_>>();
    if checkpoint.interface() != interface
        || expected != actual
        || checkpoint.outbound_interface_bindings.len() != outbound.len()
        || checkpoint.include_context != include_context
    {
        return Err(contract(format!(
            "run_authoring checkpoint '{}' route does not match the terminal client topology",
            name
        )));
    }
    Ok(())
}

fn require_checkpoint_without_verifier(
    checkpoint: &AuthoredCheckpointTemplate,
    name: &str,
) -> Result<(), TopologyError> {
    if !checkpoint.required_verified_artifacts.is_empty() {
        return Err(contract(format!(
            "run_authoring checkpoint '{}' unexpectedly declares verified-artifact requirements",
            name
        )));
    }
    Ok(())
}

fn require_checkpoint_role(
    checkpoint: &AuthoredCheckpointTemplate,
    name: &str,
    expected: AuthoredCheckpointRole,
) -> Result<(), TopologyError> {
    if checkpoint.role != expected {
        return Err(contract(format!(
            "run_authoring checkpoint '{}' has role {:?}, expected {:?}",
            name, checkpoint.role, expected
        )));
    }
    Ok(())
}

fn single_checkpoint_outbound(
    checkpoint: &AuthoredCheckpointTemplate,
    name: &str,
) -> Result<InterfaceId, TopologyError> {
    if checkpoint.outbound_interface_bindings.len() != 1 {
        return Err(contract(format!(
            "run_authoring checkpoint '{}' must declare exactly one outbound interface",
            name
        )));
    }
    Ok(checkpoint.outbound_interface_bindings[0].interface.clone())
}

fn checkpoint_tools(
    checkpoint: &AuthoredCheckpointTemplate,
    name: &str,
    mark: &str,
    artifact_label: &str,
    verifier: &str,
) -> Result<Vec<String>, TopologyError> {
    if checkpoint.required_verified_artifacts.len() != 1 {
        return Err(contract(format!(
            "run_authoring checkpoint '{}' must declare one tool-call requirement",
            name
        )));
    }
    let required = &checkpoint.required_verified_artifacts[0];
    if required.mark.as_str() != mark
        || required.artifact_label != artifact_label
        || required.verifier.as_str() != verifier
    {
        return Err(contract(format!(
            "run_authoring checkpoint '{}' tool-call requirement does not match the client descriptor",
            name
        )));
    }
    let params = required.params.as_object().ok_or_else(|| {
        contract(format!(
            "run_authoring checkpoint '{}' tool-call params must be an object",
            name
        ))
    })?;
    if params.len() != 1 || !params.contains_key("tools") {
        return Err(contract(format!(
            "run_authoring checkpoint '{}' tool-call params must contain only tools",
            name
        )));
    }
    let values = params["tools"].as_array().ok_or_else(|| {
        contract(format!(
            "run_authoring checkpoint '{}' tool-call tools must be an array",
            name
        ))
    })?;
    if values.is_empty() {
        return Err(contract(format!(
            "run_authoring checkpoint '{}' tool-call tools are empty",
            name
        )));
    }
    let mut tools = Vec::with_capacity(values.len());
    let mut unique = BTreeSet::new();
    for value in values {
        let tool = value.as_str().ok_or_else(|| {
            contract(format!(
                "run_authoring checkpoint '{}' tool id is not a string",
                name
            ))
        })?;
        require_non_empty("run_authoring checkpoint tool", tool)?;
        if !unique.insert(tool) {
            return Err(contract(format!(
                "run_authoring checkpoint '{}' repeats tool '{}'",
                name, tool
            )));
        }
        tools.push(tool.to_string());
    }
    Ok(tools)
}

fn optional_axis_allows(axis: &Option<Vec<String>>, value: &str) -> bool {
    axis.as_ref()
        .is_none_or(|allowed| allowed.iter().any(|candidate| candidate == value))
}

fn required_root(
    root_slots: &BTreeMap<String, PathBuf>,
    slot: &str,
) -> Result<PathBuf, TopologyError> {
    root_slots.get(slot).cloned().ok_or_else(|| {
        contract(format!(
            "host manifest does not bind required root slot '{slot}'"
        ))
    })
}

fn canonicalize_declared_root(path: &Path) -> Result<PathBuf, String> {
    require_absolute("declared root", path).map_err(|error| error.to_string())?;
    let mut existing = path.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(mut canonical) => {
                for component in suffix.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = existing
                    .file_name()
                    .ok_or_else(|| format!("path '{}' has no existing ancestor", path.display()))?;
                suffix.push(component.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| format!("path '{}' has no existing ancestor", path.display()))?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(format!(
                    "path '{}' cannot be canonicalized: {error}",
                    existing.display()
                ));
            }
        }
    }
}

fn read_bytes(surface: &'static str, path: &Path) -> Result<Vec<u8>, TopologyError> {
    std::fs::read(path).map_err(|source| TopologyError::Read {
        surface,
        path: path.display().to_string(),
        source,
    })
}

fn pretty_json(value: &serde_json::Value, surface: &'static str) -> Result<String, TopologyError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| contract(format!("{surface} did not encode: {error}")))?;
    bytes.push(b'\n');
    String::from_utf8(bytes)
        .map_err(|error| contract(format!("{surface} encoded non-UTF8 bytes: {error}")))
}

fn pretty_typed<T: serde::Serialize>(
    value: &T,
    surface: &'static str,
) -> Result<String, TopologyError> {
    let value = serde_json::to_value(value)
        .map_err(|error| contract(format!("{surface} did not encode: {error}")))?;
    pretty_json(&value, surface)
}

fn require_absolute(name: &str, path: &Path) -> Result<(), TopologyError> {
    if !path.is_absolute() {
        return Err(contract(format!(
            "{name} must be absolute, got '{}'",
            path.display()
        )));
    }
    Ok(())
}

fn require_non_empty(name: &str, value: &str) -> Result<(), TopologyError> {
    if value.trim() != value || value.is_empty() {
        return Err(contract(format!("{name} must be non-empty and trimmed")));
    }
    Ok(())
}

fn require_id_fragment(name: &str, value: &str) -> Result<(), TopologyError> {
    require_non_empty(name, value)?;
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
    {
        return Err(contract(format!(
            "{name} contains characters outside [A-Za-z0-9_.-]"
        )));
    }
    Ok(())
}

fn require_sidecar_name(name: &str, value: &str) -> Result<(), TopologyError> {
    require_non_empty(name, value)?;
    let path = Path::new(value);
    if path.components().count() != 1
        || path.file_name().and_then(|part| part.to_str()) != Some(value)
        || !value.ends_with(".json")
        || matches!(
            value,
            "interface.json" | "life.json" | "activation.json" | "policy.json" | "client.json"
        )
    {
        return Err(contract(format!(
            "{name} must be a non-reserved JSON filename without path components"
        )));
    }
    Ok(())
}

fn require_env_prefix(name: &str, value: &str) -> Result<(), TopologyError> {
    require_non_empty(name, value)?;
    if !value.chars().all(|character| {
        character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
    }) {
        return Err(contract(format!(
            "{name} contains characters outside [A-Z0-9_]"
        )));
    }
    Ok(())
}

fn worker_environment_segment(raw: &str) -> String {
    raw.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn contract(message: impl Into<String>) -> TopologyError {
    TopologyError::Contract(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mandatory_roles_preserve_policy_order_and_deduplicate_by_first_occurrence() {
        let relay = |id: &str, roles: &[&str]| WorkerRelayCapabilityBinding {
            interface: octopus_descriptors::parse_strict::<InterfaceDescriptor>(
                serde_json::to_vec(&serde_json::json!({
                    "id": id,
                    "interface_type": "system",
                    "role": "worker",
                    "direction": "bidirectional",
                    "source_names": [id]
                }))
                .unwrap()
                .as_slice(),
            )
            .unwrap(),
            role_artifacts: roles.iter().copied().map(ArtifactId::new).collect(),
        };
        let relays = BTreeMap::from([
            (
                InterfaceId::new("system_worker_target_z"),
                relay(
                    "system_worker_target_z",
                    &["artifact-target-z", "artifact-current-a"],
                ),
            ),
            (
                InterfaceId::new("system_worker_target_a"),
                relay(
                    "system_worker_target_a",
                    &["artifact-target-a", "artifact-target-z"],
                ),
            ),
        ]);
        let actual = ordered_mandatory_role_artifacts(
            &InterfaceId::new("system_worker_current"),
            &[
                ArtifactId::new("artifact-current-z"),
                ArtifactId::new("artifact-current-a"),
            ],
            &relays,
        );
        assert_eq!(
            actual,
            [
                "artifact-current-z",
                "artifact-current-a",
                "artifact-target-a",
                "artifact-target-z",
            ]
            .map(ArtifactId::new)
        );
    }

    #[test]
    fn terminal_run_prefix_requires_a_non_empty_instance_id() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .unwrap();
        let descriptor = TerminalClientDescriptor::load(&repository.join("registry")).unwrap();

        assert!(!descriptor.is_private_run(&RunId::new("run_terminal_")));
        assert!(
            descriptor.is_private_run(&RunId::new("run_terminal_4d5fd6eacf2a4563b055d4d879808411"))
        );
    }

    #[test]
    fn terminal_host_roots_use_the_home_registry_not_the_repository_catalog() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let active_registry = home.join("registry");
        let repo = temp.path().join("repo");
        let worktrees = home.join("state/worker-worktrees");
        let runs = home.join("state/runs");
        let ees = home.join("state/ee_instances");
        std::fs::create_dir_all(&active_registry).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("host.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "host_manifest_version": 1,
                "root_refs": {
                    "repo_root": repo,
                    "worktree_root": worktrees,
                    "run_records_root": runs,
                    "ee_instances_root": ees
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let roots = TerminalHostRoots::load_from_home(&home).unwrap();
        assert_eq!(
            roots.registry_root,
            std::fs::canonicalize(active_registry).unwrap()
        );
        assert_ne!(roots.registry_root, roots.repo_root.join("registry"));
    }

    #[test]
    fn private_run_catalog_forms_the_complete_capability_surface_from_registry_policy() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .unwrap();
        let registry = repository.join("registry");
        let descriptor = TerminalClientDescriptor::load(&registry).unwrap();
        let request = default_private_run_request(&registry).unwrap();
        let catalog = load_root_catalog(&registry, &descriptor, &request).unwrap();

        let routed_tools = catalog
            .capabilities
            .values()
            .flat_map(|capability| capability.tools.iter().cloned())
            .collect::<BTreeSet<_>>();
        let ceiling_tools = catalog
            .worker_policy
            .ceiling
            .tools
            .iter()
            .map(|tool| tool.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(routed_tools, ceiling_tools);

        let context = &catalog.capabilities["system_context"];
        assert!(context.tools.contains(&"artifact.read".to_string()));
        assert_eq!(
            context
                .worker_environment_kinds
                .iter()
                .map(octopus_descriptors::WorkerEnvironmentKindId::as_str)
                .collect::<Vec<_>>(),
            vec!["system_room"]
        );

        let room = &catalog.capabilities["system_room"];
        assert!(room.tools.contains(&"artifact.send".to_string()));
        assert!(room.tools.contains(&"file.write".to_string()));
        assert_eq!(
            room.worker_environment_kinds
                .iter()
                .map(octopus_descriptors::WorkerEnvironmentKindId::as_str)
                .collect::<Vec<_>>(),
            vec!["system_room"]
        );

        let browser = &catalog.capabilities["system_browser"];
        assert_eq!(
            browser
                .worker_environment_kinds
                .iter()
                .map(octopus_descriptors::WorkerEnvironmentKindId::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["browser_session"])
        );
        assert!(browser.tools.contains(&"browser.navigate".to_string()));

        let web_search = &catalog.capabilities["system_web_search"];
        assert_eq!(
            web_search
                .worker_environment_kinds
                .iter()
                .map(octopus_descriptors::WorkerEnvironmentKindId::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["web_search"])
        );
        assert!(web_search.tools.contains(&"web.search".to_string()));

        let process = &catalog.capabilities["system_exec_env"];
        assert_eq!(
            process
                .worker_environment_kinds
                .iter()
                .map(octopus_descriptors::WorkerEnvironmentKindId::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["process_session"])
        );
        assert!(process.tools.contains(&"process.start".to_string()));

        let remote_exec = &catalog.capabilities["system_remote_exec"];
        assert_eq!(
            remote_exec
                .worker_environment_kinds
                .iter()
                .map(octopus_descriptors::WorkerEnvironmentKindId::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["remote_exec_server"])
        );
        assert!(remote_exec.tools.contains(&"remote.exec".to_string()));
        assert!(remote_exec.sidecars.contains_key("remote_exec.json"));

        for interface in [
            "world_mcp_firecrawl",
            "world_mcp_github",
            "world_mcp_unity",
        ] {
            let operation = &catalog.operation_capabilities[interface];
            assert!(!operation.operations.is_empty());
            assert_eq!(operation.catalog_label, "mcp.operation_catalog");
            assert_eq!(operation.request_label, "mcp.operation.request");
            assert!(!catalog.capabilities.contains_key(interface));
        }

        let private_provider = child_interface_bytes(
            &catalog.provider,
            &InterfaceId::new("world_provider_codex_private_test"),
            None,
        )
        .unwrap();
        let private_provider =
            octopus_descriptors::parse_strict::<InterfaceDescriptor>(private_provider.as_bytes())
                .unwrap();
        assert_eq!(
            private_provider.interface_driver_ref,
            catalog.provider.interface_driver_ref
        );
        assert_eq!(
            private_provider.interface_driver_config,
            catalog.provider.interface_driver_config
        );
        assert_eq!(
            private_provider.interface_policy,
            catalog.provider.interface_policy
        );
    }

    #[test]
    fn default_private_run_reuses_existence_and_authors_every_formed_capability() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .unwrap();
        let registry = repository.join("registry");
        let descriptor = TerminalClientDescriptor::load(&registry).unwrap();
        let request = default_private_run_request(&registry).unwrap();
        let catalog = load_root_catalog(&registry, &descriptor, &request).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let run_records_root = temp.path().join("runs");
        let ee_instances_root = temp.path().join("ees");
        let registry_interface_root = temp.path().join("interfaces");
        let worktree_root = canonicalize_declared_root(&temp.path().join("worktrees")).unwrap();
        std::fs::create_dir_all(&run_records_root).unwrap();
        std::fs::create_dir_all(&ee_instances_root).unwrap();
        std::fs::create_dir_all(&registry_interface_root).unwrap();
        let root_ee = EeInstance {
            name: None,
            ee_id: EeId::new("ee_existence"),
            template: catalog.ee_template.id.clone(),
            parent_ee: None,
            goal: request.goal.clone(),
            config: Default::default(),
            policy_narrowing: Default::default(),
        };
        std::fs::write(
            ee_instances_root.join("ee_existence.json"),
            serde_json::to_vec_pretty(&root_ee).unwrap(),
        )
        .unwrap();
        let roots = TerminalHostRoots {
            repo_root: repository.clone(),
            worktree_root: worktree_root.clone(),
            root_slots: BTreeMap::from([
                ("repo_root".to_string(), repository.clone()),
                ("worktree_root".to_string(), worktree_root),
                ("run_records_root".to_string(), run_records_root.clone()),
                ("ee_instances_root".to_string(), ee_instances_root.clone()),
            ]),
            registry_root: registry.clone(),
            registry_artifact_root: temp.path().join("artifacts"),
            registry_interface_root,
            run_records_root,
            ee_instances_root,
            durable_ledger_root: temp.path().join("ledger"),
        };
        let (topology, writes) = plan_private_run(
            temp.path(),
            &roots,
            &descriptor,
            &catalog,
            request,
            "0123456789abcdef0123456789abcdef".to_string(),
        )
        .unwrap();

        assert_eq!(topology.ee.as_str(), "ee_existence");
        let expected_capability_count = catalog
            .capabilities
            .iter()
            .filter(|(template, capability)| {
                !exact_capability_tools(&descriptor, template, capability, &topology.exact_tools)
                    .unwrap()
                    .is_empty()
            })
            .count();
        assert_eq!(
            topology.worker_environment_interfaces.len() + topology.tool_interfaces.len(),
            expected_capability_count
        );
        assert_eq!(
            topology.operation_interfaces.len(),
            catalog.operation_capabilities.len()
        );
        assert!(
            topology
                .worker_environment_interfaces
                .contains_key("system_room")
        );
        assert!(!topology.tool_interfaces.contains_key("system_room"));
        assert!(
            !writes
                .iter()
                .any(|write| write.path.starts_with(&roots.ee_instances_root)),
            "a normal terminal chat reuses the existence EE instead of fabricating a child"
        );
        for interface in topology
            .worker_environment_interfaces
            .values()
            .chain(topology.tool_interfaces.values())
            .chain(topology.operation_interfaces.values())
        {
            assert!(writes.iter().any(|write| {
                write.path
                    == roots
                        .registry_interface_root
                        .join(interface.as_str())
                        .join("interface.json")
            }));
        }
        for (template, interface) in &topology.operation_interfaces {
            let capability = &catalog.operation_capabilities[template];
            for operation in &capability.operations {
                let write = writes
                    .iter()
                    .find(|write| {
                        write.path
                            == roots
                                .registry_interface_root
                                .join(interface.as_str())
                                .join(&capability.catalog_dir)
                                .join(format!("{}.json", operation.id))
                    })
                    .unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&write.content).unwrap(),
                    operation.descriptor
                );
            }
            let artifact = operation_catalog_artifact_id(interface);
            let write = writes
                .iter()
                .find(|write| {
                    write.path
                        == roots
                            .registry_artifact_root
                            .join(artifact.as_str())
                            .join("artifact.json")
                })
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&write.content).unwrap(),
                operation_catalog_artifact_value(interface, capability)
            );
        }
        let coder_interface = topology
            .worker_environment_interfaces
            .get("system_coder_env")
            .unwrap();
        assert_eq!(
            topology.shell_environment_interface,
            coder_interface.clone()
        );
        assert!(!writes.iter().any(|write| {
            write.path
                == roots
                    .registry_interface_root
                    .join(format!(
                        "{}{}",
                        descriptor.private_run.id_prefixes.shell_environment, topology.opaque_id
                    ))
                    .join("interface.json")
        }));
        let coder_descriptor = writes
            .iter()
            .find(|write| {
                write.path
                    == roots
                        .registry_interface_root
                        .join(coder_interface.as_str())
                        .join("interface.json")
            })
            .unwrap();
        let coder_descriptor = octopus_descriptors::parse_strict::<InterfaceDescriptor>(
            coder_descriptor.content.as_bytes(),
        )
        .unwrap();
        assert!(
            coder_descriptor
                .interface_driver_config
                .as_ref()
                .is_none_or(|config| !config.host_slots.contains_key("worktree_root"))
        );

        for (base_tool, exact_tool) in &topology.exact_tools {
            let write = writes
                .iter()
                .find(|write| {
                    write.path
                        == roots
                            .registry_root
                            .join("tool")
                            .join(exact_tool)
                            .join("tool.json")
                })
                .unwrap();
            let tool =
                octopus_descriptors::parse_strict::<ToolDescriptor>(write.content.as_bytes())
                    .unwrap();
            assert_eq!(tool.id.as_str(), exact_tool);
            let (template, _) = catalog
                .capabilities
                .iter()
                .find(|(_, capability)| capability.tools.contains(base_tool))
                .unwrap();
            let expected_door = if template == &descriptor.authoring.room_interface
                && base_tool == &descriptor.authoring.tool
            {
                topology.authoring_environment_interface.clone()
            } else {
                topology
                    .worker_environment_interfaces
                    .get(template)
                    .or_else(|| topology.tool_interfaces.get(template))
                    .unwrap()
                    .clone()
            };
            assert_eq!(tool.interface, expected_door);
            let base = load_tool_descriptor_from_dir(&roots.registry_root.join("tool"), base_tool)
                .unwrap()
                .unwrap();
            assert_eq!(tool.handler_config, base.handler_config);
        }

        let worker_policy_write = writes
            .iter()
            .find(|write| {
                write.path
                    == roots
                        .registry_interface_root
                        .join(topology.worker_interface.as_str())
                        .join("policy.json")
            })
            .unwrap();
        let worker_policy: WorkerInterfacePolicy =
            serde_json::from_str(&worker_policy_write.content).unwrap();
        let worker_tools = worker_policy
            .ceiling
            .tools
            .iter()
            .map(|tool| tool.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            worker_tools,
            topology
                .exact_tools
                .values()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        );
        assert!(
            worker_policy
                .ceiling
                .interfaces
                .contains(&topology.authoring_environment_interface)
        );
        let capability_interfaces = capability_interfaces_from_topology(
            &topology.worker_environment_interfaces,
            &topology.tool_interfaces,
        );
        let interface_replacements = planned_private_interface_replacements(
            &catalog,
            &topology.terminal_interface,
            &topology.shell_terminal_interface,
            &topology.provider_interface,
            &topology.interface_authoring_interface,
            &topology.authoring_environment_interface,
            &topology.worker_interface,
            &capability_interfaces,
            &topology.operation_interfaces,
        )
        .unwrap();
        for (base, exacts) in interface_replacements {
            for exact in &exacts {
                assert!(worker_policy.ceiling.interfaces.contains(exact));
            }
            assert!(
                exacts.iter().any(|exact| exact.as_str() == base)
                    || !worker_policy
                        .ceiling
                        .interfaces
                        .iter()
                        .any(|interface| interface.as_str() == base)
            );
        }

        let provider_write = writes
            .iter()
            .find(|write| {
                write.path
                    == roots
                        .registry_interface_root
                        .join(topology.provider_interface.as_str())
                        .join("interface.json")
            })
            .unwrap();
        let provider = octopus_descriptors::parse_strict::<InterfaceDescriptor>(
            provider_write.content.as_bytes(),
        )
        .unwrap();
        let mut provider_config = provider.interface_driver_config.unwrap().private_config;
        assert!(!provider_config.contains_key("run_authoring"));
        let run_authoring_profile = provider_config.remove("run_authoring_profile").unwrap();
        assert_eq!(
            run_authoring_profile["file_write_tool"],
            serde_json::json!(topology.exact_tools[&descriptor.authoring.tool])
        );
        assert_eq!(
            run_authoring_profile["file_write_result_label"],
            serde_json::json!(format!(
                "{}.result",
                topology.exact_tools[&descriptor.authoring.tool]
            ))
        );

        let run_write = writes
            .iter()
            .find(|write| {
                write.path
                    == roots
                        .run_records_root
                        .join(format!("{}.json", topology.run.as_str()))
            })
            .unwrap();
        let run = RunRecord::from_bytes(run_write.content.as_bytes()).unwrap();
        let run_authoring_binding = &run.interface_state[&topology.provider_interface]
            ["run_authoring_binding"];
        assert_eq!(run_authoring_binding["schema_version"], 2);
        assert_eq!(
            run_authoring_binding["provider_interface"],
            topology.provider_interface.as_str()
        );
        let terminal_input = run
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id.as_str().ends_with("_terminal_input"))
            .unwrap();
        assert_eq!(
            terminal_input
                .interface_binding
                .as_ref()
                .unwrap()
                .config
                .artifact_policy,
            BindingArtifactPolicy::Allow {
                names: vec![descriptor.artifact_labels.operator_input.clone()],
                namespaces: Vec::new(),
            },
            "generic terminal input cannot bypass a dynamically authored verified operator response edge"
        );
        let turn_control_input = run
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.id.as_str().ends_with("_turn_control_input"))
            .unwrap();
        assert_eq!(
            turn_control_input
                .interface_binding
                .as_ref()
                .unwrap()
                .config
                .artifact_policy,
            BindingArtifactPolicy::Allow {
                names: vec![descriptor.artifact_labels.turn_control.clone()],
                namespaces: Vec::new(),
            }
        );
        let operation_catalogs = topology
            .operation_interfaces
            .values()
            .map(operation_catalog_artifact_id)
            .collect::<BTreeSet<_>>();
        for checkpoint in run.checkpoints {
            assert!(checkpoint.artifact_bindings.len() >= catalog.mandatory_role_artifacts.len());
            assert!(
                checkpoint
                    .artifact_bindings
                    .iter()
                    .zip(&catalog.mandatory_role_artifacts)
                    .all(|(binding, artifact)| binding.id == *artifact)
            );
            if checkpoint.outbound_interfaces().next() == Some(&topology.provider_interface)
                && checkpoint.artifact_bindings.len() > catalog.mandatory_role_artifacts.len()
            {
                let bound = checkpoint
                    .artifact_bindings
                    .iter()
                    .map(|binding| binding.id.clone())
                    .collect::<BTreeSet<_>>();
                assert!(operation_catalogs.is_subset(&bound));
            }
            for requirement in checkpoint.required_verified_artifacts {
                let tools = requirement.params["tools"].as_array().unwrap();
                assert!(tools.iter().all(|tool| {
                    topology
                        .exact_tools
                        .values()
                        .any(|exact| tool.as_str() == Some(exact))
                }));
            }
        }

        assert!(writes.iter().all(|write| {
            !write
                .path
                .starts_with(roots.registry_root.join("worker_environment_kind"))
        }));

        let ee_write = writes
            .iter()
            .find(|write| {
                write.path
                    == roots
                        .registry_root
                        .join("ee_template/existence/ee_template.json")
            })
            .unwrap();
        let ee =
            octopus_descriptors::parse_strict::<EeTemplateDescriptor>(ee_write.content.as_bytes())
                .unwrap();
        assert!(ee.allowed_interfaces.is_none());
        assert!(ee.allowed_tools.is_none());
    }

    #[test]
    fn private_model_picker_and_selection_use_the_formed_policy_set() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .unwrap();
        let registry = repository.join("registry");
        let descriptor = TerminalClientDescriptor::load(&registry).unwrap();
        let request = default_private_run_request(&registry).unwrap();
        let catalog = load_root_catalog(&registry, &descriptor, &request).unwrap();
        let mut provider = catalog.provider.clone();
        let narrowed_model = "gpt-policy-narrowed";
        let config = provider.interface_driver_config.as_mut().unwrap();
        let presets = config
            .private_config
            .get_mut("model_catalog")
            .unwrap()
            .as_array_mut()
            .unwrap();
        let mut narrowed_preset = presets[0].clone();
        narrowed_preset["model"] = serde_json::json!(narrowed_model);
        narrowed_preset["display_name"] = serde_json::json!("Policy Narrowed");
        presets.push(narrowed_preset);
        provider
            .interface_policy
            .variables
            .get_mut("models")
            .unwrap()
            .default = serde_json::json!([request.model, narrowed_model]);

        let mut worker = catalog.worker_policy.clone();
        worker.ceiling.interface_policy.insert(
            provider.id.clone(),
            BTreeMap::from([(
                "models".to_string(),
                serde_json::json!([request.model, narrowed_model]),
            )]),
        );
        let mut parent = catalog.parent_ee_template.clone();
        parent.interface_policy_overrides.insert(
            provider.id.clone(),
            BTreeMap::from([(
                "models".to_string(),
                serde_json::json!([request.model, narrowed_model]),
            )]),
        );
        let mut child = catalog.ee_template.clone();
        child.interface_policy_overrides.insert(
            provider.id.clone(),
            BTreeMap::from([("models".to_string(), serde_json::json!([narrowed_model]))]),
        );
        let formed = formed_provider_models(&provider, &worker, &[&parent, &child]).unwrap();
        assert_eq!(formed, BTreeSet::from([narrowed_model.to_string()]));

        let config = provider.interface_driver_config.as_ref().unwrap();
        let picker = provider_model_catalog(config, &provider.id, &formed).unwrap();
        assert_eq!(
            picker
                .iter()
                .map(|preset| preset.model.as_str())
                .collect::<Vec<_>>(),
            vec![narrowed_model]
        );
        assert!(
            validate_provider_model_selection(
                config,
                &provider.id,
                &request.model,
                &request.reasoning_effort,
                &formed,
            )
            .is_err()
        );
        validate_provider_model_selection(
            config,
            &provider.id,
            narrowed_model,
            &request.reasoning_effort,
            &formed,
        )
        .unwrap();
    }

    #[test]
    fn every_registry_provider_catalog_admits_its_default_model_and_effort() {
        let registry = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .unwrap()
            .join("registry/interface");
        let mut checked = 0;
        for entry in std::fs::read_dir(&registry).unwrap() {
            let path = entry.unwrap().path().join("interface.json");
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let provider = octopus_descriptors::parse_strict::<InterfaceDescriptor>(&bytes).unwrap();
            let Some(config) = provider.interface_driver_config.as_ref() else { continue };
            if !config.private_config.contains_key("model_catalog") {
                continue;
            }
            let formed = provider.interface_policy.variables["models"]
                .default
                .as_array()
                .unwrap()
                .iter()
                .map(|model| model.as_str().unwrap().to_string())
                .collect::<BTreeSet<_>>();
            let model = config.private_config["model"].as_str().unwrap();
            let effort = configured_reasoning_effort(config, &provider.id).unwrap();
            validate_provider_model_selection(config, &provider.id, model, &effort, &formed)
                .unwrap_or_else(|error| panic!("{}: {error}", provider.id.as_str()));
            if effort.is_empty() {
                assert!(validate_provider_model_selection(config, &provider.id, model, "high", &formed).is_err());
                let mut malformed = config.clone();
                malformed.private_config.get_mut("model_catalog").unwrap().as_array_mut().unwrap()[0]["default_reasoning_effort"] = serde_json::json!("high");
                assert!(provider_model_catalog(&malformed, &provider.id, &formed).is_err());
            }
            checked += 1;
        }
        assert!(checked >= 30, "only {checked} provider catalogs were checked");
    }

    #[test]
    fn exact_clone_rebases_worker_policy_and_copies_ee_overrides() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .unwrap();
        let registry = repository.join("registry");
        let descriptor = TerminalClientDescriptor::load(&registry).unwrap();
        let request = default_private_run_request(&registry).unwrap();
        let catalog = load_root_catalog(&registry, &descriptor, &request).unwrap();
        let base = catalog.provider.id.clone();
        let exact = InterfaceId::new("world_provider_codex_exact_policy_test");
        let values = BTreeMap::from([("models".to_string(), serde_json::json!([request.model]))]);
        let replacements =
            BTreeMap::from([(base.as_str().to_string(), BTreeSet::from([exact.clone()]))]);

        let mut worker = catalog.worker_policy.clone();
        worker
            .ceiling
            .interface_policy
            .insert(base.clone(), values.clone());
        replace_private_worker_interfaces(&mut worker, &replacements).unwrap();
        assert!(!worker.ceiling.interface_policy.contains_key(&base));
        assert_eq!(worker.ceiling.interface_policy.get(&exact), Some(&values));

        let mut worker_collision = catalog.worker_policy.clone();
        worker_collision
            .ceiling
            .interface_policy
            .insert(base.clone(), values.clone());
        worker_collision
            .ceiling
            .interface_policy
            .insert(exact.clone(), values.clone());
        assert!(replace_private_worker_interfaces(&mut worker_collision, &replacements).is_err());

        let mut ee = catalog.ee_template.clone();
        ee.interface_policy_overrides
            .insert(base.clone(), values.clone());
        copy_private_ee_interface_policy_overrides(&mut ee, &replacements).unwrap();
        assert_eq!(ee.interface_policy_overrides.get(&base), Some(&values));
        assert_eq!(ee.interface_policy_overrides.get(&exact), Some(&values));
        assert!(copy_private_ee_interface_policy_overrides(&mut ee, &replacements).is_err());
    }

    #[test]
    fn client_write_conflict_refuses_before_installing_fresh_topology() {
        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared.json");
        let fresh = temp.path().join("fresh.json");
        let before = b"{\"value\":1}\n".to_vec();
        std::fs::write(&shared, &before).unwrap();
        let writes = vec![
            PlannedWrite {
                path: fresh.clone(),
                content: "{}\n".to_string(),
                before_hash: None,
                after_hash: ContentHash::of_bytes(b"{}\n").to_hex(),
                phase: PlannedWritePhase::InterfaceCompanion,
            },
            PlannedWrite {
                path: shared.clone(),
                content: "{\"value\":2}\n".to_string(),
                before_hash: Some(ContentHash::of_bytes(&before).to_hex()),
                after_hash: ContentHash::of_bytes(b"{\"value\":2}\n").to_hex(),
                phase: PlannedWritePhase::SharedConsent,
            },
        ];
        std::fs::write(&shared, b"{\"value\":3}\n").unwrap();

        assert!(matches!(
            commit_client_writes(&writes),
            Err(TopologyError::Conflict(_))
        ));
        assert!(!fresh.exists());
    }
}
