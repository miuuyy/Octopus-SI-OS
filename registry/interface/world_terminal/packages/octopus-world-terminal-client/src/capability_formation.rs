use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_descriptors::{
    ArtifactId, DescriptorName, EeId, EeTemplateId, InterfaceId, ToolId, WorkerEnvironmentKindId,
};
use octopus_ee::{EeInstance, EeInstancePolicyNarrowing, EeTemplateDescriptor};
use octopus_interfaces::{
    InterfaceDescriptor, InterfaceRuntimeDisposition, WorkerInterfacePolicyScan,
    form_interface_runtime_disposition, scan_worker_interface_policies,
};
use octopus_policy::{
    CapSet, CopyLineage, FormationPin, FormedInterfacePolicy, WorkerInterfacePolicy,
    form_interface_policy, form_policy_with_lineage,
};
use octopus_tool_registry::ToolDescriptor;
use octopus_worker_env::{WorkerEnvKindRegistry, WorkerEnvKindSpec};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityFormationRequest {
    pub worker: InterfaceId,
    pub ee_template_chain: Vec<EeTemplateId>,
    pub ee_instance_policy_narrowing: Vec<EeInstancePolicyNarrowing>,
    pub planned_tools: Vec<ToolDescriptor>,
}

impl CapabilityFormationRequest {
    pub fn new(
        worker: impl Into<InterfaceId>,
        ee_template_chain: impl IntoIterator<Item = EeTemplateId>,
    ) -> Self {
        let ee_template_chain = ee_template_chain.into_iter().collect::<Vec<_>>();
        Self {
            worker: worker.into(),
            ee_instance_policy_narrowing: vec![
                EeInstancePolicyNarrowing::default();
                ee_template_chain.len()
            ],
            ee_template_chain,
            planned_tools: Vec::new(),
        }
    }

    pub fn from_live_chain(
        worker: impl Into<InterfaceId>,
        ee_chain: impl IntoIterator<Item = LiveEePolicyLayer>,
    ) -> Self {
        let ee_chain = ee_chain.into_iter().collect::<Vec<_>>();
        Self {
            worker: worker.into(),
            ee_template_chain: ee_chain
                .iter()
                .map(|layer| layer.template.clone())
                .collect(),
            ee_instance_policy_narrowing: ee_chain
                .into_iter()
                .map(|layer| layer.policy_narrowing)
                .collect(),
            planned_tools: Vec::new(),
        }
    }

    pub fn with_planned_tools(mut self, tools: impl IntoIterator<Item = ToolDescriptor>) -> Self {
        self.planned_tools.extend(tools);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveEePolicyLayer {
    pub template: EeTemplateId,
    pub policy_narrowing: EeInstancePolicyNarrowing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormedToolCapability {
    pub tool: ToolId,
    pub worker_environment_kind: WorkerEnvironmentKindId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilitySidecarRequirement {
    pub file_name: String,
    pub worker_environment_kind: WorkerEnvironmentKindId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCapabilityBinding {
    pub interface: InterfaceDescriptor,
    pub tools: Vec<FormedToolCapability>,
    pub worker_environment_kinds: Vec<WorkerEnvironmentKindId>,
    pub sidecars: Vec<CapabilitySidecarRequirement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerRelayCapabilityBinding {
    pub interface: InterfaceDescriptor,
    pub role_artifacts: Vec<ArtifactId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormedInterfaceOperation {
    pub id: String,
    pub request_label: String,
    pub output_label: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub descriptor: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceOperationCapabilityBinding {
    pub interface: InterfaceDescriptor,
    pub catalog_dir: String,
    pub catalog_label: String,
    pub request_label: String,
    pub operations: Vec<FormedInterfaceOperation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormedCapabilityTopology {
    pub worker: InterfaceId,
    pub ee_template_chain: Vec<EeTemplateId>,
    pub ee_interface_allowance: Option<Vec<InterfaceId>>,
    pub copy_lineage: CopyLineage,
    pub granted_tools: Vec<ToolId>,
    pub granted_worker_environment_kinds: Vec<WorkerEnvironmentKindId>,
    pub interface_policies: BTreeMap<InterfaceId, FormedInterfacePolicy>,
    pub tool_bindings: BTreeMap<InterfaceId, ToolCapabilityBinding>,
    pub operation_bindings: BTreeMap<InterfaceId, InterfaceOperationCapabilityBinding>,
    pub worker_relays: BTreeMap<InterfaceId, WorkerRelayCapabilityBinding>,
}

impl FormedCapabilityTopology {
    pub fn ee_admits_interface(&self, interface: &InterfaceId) -> bool {
        self.ee_interface_allowance.as_ref().is_none_or(|allowance| {
            self.copy_lineage.admits_interface(
                &CapSet::from_allowed(allowance.iter().cloned()),
                interface,
            )
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CapabilityFormationError {
    #[error("capability registry surface '{surface}' could not be loaded: {reason}")]
    Registry {
        surface: &'static str,
        reason: String,
    },
    #[error("capability formation contract failed closed: {0}")]
    Contract(String),
}

pub fn form_capability_topology(
    registry_root: &Path,
    request: &CapabilityFormationRequest,
) -> Result<FormedCapabilityTopology, CapabilityFormationError> {
    CapabilityRegistry::load(registry_root).form(request)
}

pub struct CapabilityRegistry {
    registry_root: PathBuf,
    interfaces: Result<(Vec<(DescriptorName, InterfaceDescriptor)>, WorkerInterfacePolicyScan), String>,
    tools: Result<Vec<ToolDescriptor>, String>,
    ee_templates: Result<BTreeMap<EeTemplateId, EeTemplateDescriptor>, String>,
    worker_environment_kinds: Result<BTreeMap<WorkerEnvironmentKindId, WorkerEnvKindSpec>, String>,
}

impl CapabilityRegistry {
    pub fn load(registry_root: &Path) -> Self {
        let interface_root = registry_root.join("interface");
        let interfaces = octopus_descriptors::loader::load_kind::<InterfaceDescriptor>(&interface_root)
            .map(|loaded| {
                let scan = scan_worker_interface_policies(&interface_root, &loaded);
                (loaded, scan)
            })
            .map_err(|error| error.to_string());
        let tools = octopus_tool_registry::load_tool_descriptors(registry_root)
            .map(|loaded| loaded.into_iter().map(|(_, descriptor)| descriptor).collect())
            .map_err(|error| error.to_string());
        let ee_templates = octopus_descriptors::loader::load_kind::<EeTemplateDescriptor>(
            &registry_root.join("ee_template"),
        )
        .map(|loaded| {
            loaded
                .into_iter()
                .map(|(_, descriptor)| (descriptor.id.clone(), descriptor))
                .collect()
        })
        .map_err(|error| error.to_string());
        let worker_environment_kinds =
            WorkerEnvKindRegistry::load_from_dir(&registry_root.join("worker_environment_kind"))
                .map(|registry| {
                    registry
                        .head_specs()
                        .map(|kind| (kind.worker_environment_kind_id.clone(), (*kind).clone()))
                        .collect()
                })
                .map_err(|error| error.to_string());
        Self {
            registry_root: registry_root.to_path_buf(),
            interfaces,
            tools,
            ee_templates,
            worker_environment_kinds,
        }
    }

    pub fn registry_root(&self) -> &Path {
        &self.registry_root
    }

    pub fn form(
        &self,
        request: &CapabilityFormationRequest,
    ) -> Result<FormedCapabilityTopology, CapabilityFormationError> {
        form_from_registry(self, request)
    }
}

fn form_from_registry(
    registry: &CapabilityRegistry,
    request: &CapabilityFormationRequest,
) -> Result<FormedCapabilityTopology, CapabilityFormationError> {
    let interface_root = registry.registry_root.join("interface");
    let (loaded_interfaces, policy_scan) = registry
        .interfaces
        .as_ref()
        .map_err(|error| registry_error("interface", error))?;
    let interfaces = loaded_interfaces
        .iter()
        .map(|(_, descriptor)| (descriptor.id.clone(), descriptor))
        .collect::<BTreeMap<_, _>>();
    let worker = interfaces.get(&request.worker).ok_or_else(|| {
        contract(format!(
            "worker interface '{}' is absent from the registry",
            request.worker.as_str()
        ))
    })?;
    if let Some(reason) = policy_scan.refusals.get(&request.worker) {
        return Err(contract(format!(
            "worker interface '{}' policy is refused: {reason}",
            request.worker.as_str()
        )));
    }
    let worker_policy = policy_scan.policies.get(&request.worker).ok_or_else(|| {
        contract(format!(
            "worker interface '{}' has no valid policy ceiling",
            request.worker.as_str()
        ))
    })?;

    let tool_descriptors = registry
        .tools
        .as_ref()
        .map_err(|error| registry_error("tool", error))?;
    let tools = tool_descriptors
        .iter()
        .chain(&request.planned_tools)
        .map(|descriptor| (ToolId::new(descriptor.id.as_str()), descriptor))
        .collect::<BTreeMap<_, _>>();
    let mut copy_lineage = CopyLineage::default();
    for interface in interfaces.values() {
        if let Some(original) = interface.copy_of.as_ref().filter(|original| interfaces.contains_key(*original)) {
            copy_lineage.insert_interface(interface.id.clone(), original.clone());
        }
    }
    for (id, tool) in &tools {
        let Some(original) = &tool.copy_of else { continue };
        let original = ToolId::new(original.as_str());
        if tools.get(&original).is_some_and(|known| tool.never_wider_than(known)) {
            copy_lineage.insert_tool(id.clone(), original);
        } else {
            copy_lineage.insert_declared_tool(id.clone(), original);
        }
    }

    let ee_templates = registry
        .ee_templates
        .as_ref()
        .map_err(|error| registry_error("ee_template", error))?;
    let ee_chain = resolve_ee_chain(request, ee_templates)?;
    ensure_worker_admitted_by_ee_chain(
        worker,
        &ee_chain,
        &request.ee_instance_policy_narrowing,
        &copy_lineage,
    )?;
    let ee_layers = ee_chain
        .iter()
        .zip(&request.ee_instance_policy_narrowing)
        .flat_map(|(template, instance)| [template.as_policy_layer(), instance.as_policy_layer()])
        .collect::<Vec<_>>();
    let ee_interface_allowance = ee_layers
        .iter()
        .filter_map(|layer| layer.interfaces.as_ref())
        .cloned()
        .reduce(|left, right| left.intersect(&right))
        .map(|interfaces| interfaces.iter().cloned().collect());
    let original_worker = copy_lineage.interface_original(&request.worker).filter(|original| {
        ee_layers.iter().any(|layer| {
            layer.interfaces.as_ref().is_some_and(|allowed| !allowed.contains(&request.worker) && allowed.contains(*original))
        })
    });
    let narrowed = original_worker
        .map(|original| {
            policy_scan.policies.get(original).map(|held| worker_policy.narrowed_by(held, &copy_lineage)).ok_or_else(|| {
                contract(format!("worker '{}' is admitted by original '{}' that has no valid ceiling",
                    request.worker.as_str(), original.as_str()))
            })
        })
        .transpose()?;
    let worker_policy = narrowed.as_ref().unwrap_or(worker_policy);
    let formed = form_policy_with_lineage(
        worker_policy.ceiling_grant(),
        &ee_layers,
        &[],
        FormationPin::default(),
        &copy_lineage,
    );
    for interface in worker_policy.ceiling.interface_policy.keys() {
        if !worker_policy.ceiling.interfaces.contains(interface) {
            return Err(contract(format!(
                "worker interface '{}' declares interface policy for '{}' outside its interface ceiling",
                worker_policy.id.as_str(),
                interface.as_str()
            )));
        }
    }
    let mut interface_policies = BTreeMap::new();
    for interface in interfaces
        .values()
        .filter(|interface| formed.context.interfaces.contains(&interface.id))
    {
        let interface_id = &interface.id;
        let mut override_layers = Vec::new();
        for (template, instance) in ee_chain.iter().zip(&request.ee_instance_policy_narrowing) {
            for id in [Some(interface_id), interface.copy_of.as_ref()].into_iter().flatten() {
                if let Some(layer) = template.interface_policy_overrides.get(id) {
                    override_layers.push(layer);
                }
                if let Some(layer) = instance.interface_policy_overrides.get(id) {
                    override_layers.push(layer);
                }
            }
        }
        let policy = form_interface_policy(
            &interface.interface_policy,
            worker_policy.ceiling.interface_policy.get(interface_id),
            &override_layers,
        )
        .map_err(|error| {
            contract(format!(
                "interface '{}' policy formation failed: {error}",
                interface_id.as_str()
            ))
        })?;
        interface_policies.insert(interface_id.clone(), policy);
    }

    let worker_environment_kinds = registry
        .worker_environment_kinds
        .as_ref()
        .map_err(|error| CapabilityFormationError::Registry {
            surface: "worker_environment_kind",
            reason: error.clone(),
        })?;

    let mut tool_bindings = BTreeMap::<InterfaceId, ToolCapabilityBinding>::new();
    let mut routed_tools = BTreeSet::new();
    for tool_id in formed.context.tools.iter() {
        let tool = tools.get(tool_id).ok_or_else(|| {
            contract(format!(
                "formed tool '{}' has no descriptor",
                tool_id.as_str()
            ))
        })?;
        let owner = interfaces.get(&tool.interface).ok_or_else(|| {
            contract(format!(
                "tool '{}' owns missing interface '{}'",
                tool_id.as_str(),
                tool.interface.as_str()
            ))
        })?;
        if !formed.context.interfaces.contains(&owner.id) {
            return Err(contract(format!(
                "formed tool '{}' owner '{}' is outside the formed interface grant",
                tool_id.as_str(),
                owner.id.as_str()
            )));
        }
        ensure_tool_owner_reader(owner, &interface_root, &policy_scan.policies)?;
        let formed_kind = form_tool_worker_environment_kind(
            tool_id,
            tool,
            &formed.context.wek,
            worker_environment_kinds,
        )?;
        let binding =
            tool_bindings
                .entry(owner.id.clone())
                .or_insert_with(|| ToolCapabilityBinding {
                    interface: InterfaceDescriptor::clone(owner),
                    tools: Vec::new(),
                    worker_environment_kinds: Vec::new(),
                    sidecars: Vec::new(),
                });
        if !routed_tools.insert(tool_id.clone()) {
            return Err(contract(format!(
                "formed tool '{}' resolved more than once",
                tool_id.as_str()
            )));
        }
        binding.tools.push(FormedToolCapability {
            tool: tool_id.clone(),
            worker_environment_kind: formed_kind.clone(),
        });
        if !binding.worker_environment_kinds.contains(&formed_kind) {
            binding.worker_environment_kinds.push(formed_kind.clone());
        }
        let kind = worker_environment_kinds.get(&formed_kind).ok_or_else(|| {
            contract(format!(
                "formed worker-environment kind '{}' disappeared from the loaded projection",
                formed_kind.as_str()
            ))
        })?;
        append_sidecars(binding, kind)?;
    }

    for binding in tool_bindings.values_mut() {
        binding
            .tools
            .sort_by(|left, right| left.tool.cmp(&right.tool));
        binding.worker_environment_kinds.sort();
        binding.sidecars.sort_by(|left, right| {
            left.file_name.cmp(&right.file_name).then_with(|| {
                left.worker_environment_kind
                    .cmp(&right.worker_environment_kind)
            })
        });
    }

    let granted_tools = formed.context.tools.iter().cloned().collect::<Vec<_>>();
    if routed_tools != granted_tools.iter().cloned().collect::<BTreeSet<_>>() {
        return Err(contract(
            "formed capability topology did not account for every granted tool",
        ));
    }

    let mut worker_relays = BTreeMap::new();
    let mut operation_bindings = BTreeMap::new();
    for interface in interfaces
        .values()
        .filter(|interface| formed.context.interfaces.contains(&interface.id))
    {
        let interface_id = &interface.id;
        if let Some(reason) = policy_scan.refusals.get(interface_id) {
            return Err(contract(format!(
                "formed interface '{}' carries refused worker policy evidence: {reason}",
                interface_id.as_str()
            )));
        }
        let disposition = form_interface_runtime_disposition(
            interface,
            &interface_root,
            policy_scan.policies.contains_key(interface_id),
        )
        .map_err(|error| {
            contract(format!(
                "formed interface '{}' has invalid runtime disposition: {error}",
                interface_id.as_str()
            ))
        })?;
        if disposition == InterfaceRuntimeDisposition::RuntimeRelay {
            if tool_bindings.contains_key(&interface.id) {
                return Err(contract(format!(
                    "interface '{}' resolves both a tool owner and a worker relay",
                    interface.id.as_str()
                )));
            }
            worker_relays.insert(
                interface.id.clone(),
                WorkerRelayCapabilityBinding {
                    interface: InterfaceDescriptor::clone(interface),
                    role_artifacts: policy_scan
                        .policies
                        .get(&interface.id)
                        .ok_or_else(|| {
                            contract(format!(
                                "formed worker relay '{}' has no exact policy",
                                interface.id.as_str()
                            ))
                        })?
                        .role_artifacts
                        .clone(),
                },
            );
            continue;
        }
        if tool_bindings.contains_key(&interface.id) {
            continue;
        }
        if disposition == InterfaceRuntimeDisposition::ExternalReader
            && let Some(binding) = load_interface_operation_binding(&interface_root, interface)?
        {
            operation_bindings.insert(interface.id.clone(), binding);
        }
    }

    Ok(FormedCapabilityTopology {
        worker: request.worker.clone(),
        ee_template_chain: request.ee_template_chain.clone(),
        ee_interface_allowance,
        copy_lineage,
        granted_tools,
        granted_worker_environment_kinds: formed.context.wek.iter().cloned().collect(),
        interface_policies,
        tool_bindings,
        operation_bindings,
        worker_relays,
    })
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct InterfaceOperationFile {
    id: String,
    request_label: String,
    output_label: String,
    handler_config: serde_json::Value,
    description: String,
    input_schema: serde_json::Value,
}

pub(crate) fn load_interface_operation_binding(
    interface_root: &Path,
    interface: &InterfaceDescriptor,
) -> Result<Option<InterfaceOperationCapabilityBinding>, CapabilityFormationError> {
    let metadata = interface
        .interface_driver_config
        .as_ref()
        .map(|config| &config.private_config);
    let keys = [
        "operation_catalog_dir",
        "operation_catalog_label",
        "operation_request_label",
    ];
    let declared = keys
        .iter()
        .any(|key| metadata.is_some_and(|metadata| metadata.contains_key(*key)));
    if !declared {
        return Ok(None);
    }
    let metadata = metadata.ok_or_else(|| {
        contract(format!(
            "interface '{}' declares an operation catalog without driver metadata",
            interface.id.as_str()
        ))
    })?;
    let field = |key: &str| {
        metadata
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .map(str::to_string)
            .ok_or_else(|| {
                contract(format!(
                    "interface '{}' operation catalog field '{}' is absent or malformed",
                    interface.id.as_str(),
                    key
                ))
            })
    };
    let catalog_dir = field("operation_catalog_dir")?;
    let catalog_label = field("operation_catalog_label")?;
    let request_label = field("operation_request_label")?;
    let relative = Path::new(&catalog_dir);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(contract(format!(
            "interface '{}' operation_catalog_dir '{}' is not a normalized relative path",
            interface.id.as_str(),
            catalog_dir
        )));
    }
    let root = interface_root.join(interface.id.as_str()).join(relative);
    let metadata = std::fs::symlink_metadata(&root).map_err(|error| {
        contract(format!(
            "interface '{}' operation catalog '{}' cannot be inspected: {error}",
            interface.id.as_str(),
            root.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(contract(format!(
            "interface '{}' operation catalog '{}' is not a regular non-symlink directory",
            interface.id.as_str(),
            root.display()
        )));
    }
    let mut entries = std::fs::read_dir(&root)
        .map_err(|error| {
            contract(format!(
                "interface '{}' operation catalog '{}' cannot be read: {error}",
                interface.id.as_str(),
                root.display()
            ))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            contract(format!(
                "interface '{}' operation catalog '{}' cannot be enumerated: {error}",
                interface.id.as_str(),
                root.display()
            ))
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut operations = Vec::new();
    let mut ids = BTreeSet::new();
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            contract(format!(
                "interface '{}' operation entry '{}' cannot be inspected: {error}",
                interface.id.as_str(),
                path.display()
            ))
        })?;
        if file_type.is_symlink()
            || !file_type.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            return Err(contract(format!(
                "interface '{}' operation entry '{}' is not a regular JSON file",
                interface.id.as_str(),
                path.display()
            )));
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            contract(format!(
                "interface '{}' operation entry '{}' cannot be read: {error}",
                interface.id.as_str(),
                path.display()
            ))
        })?;
        let operation: InterfaceOperationFile =
            serde_json::from_slice(&bytes).map_err(|error| {
                contract(format!(
                    "interface '{}' operation entry '{}' is malformed: {error}",
                    interface.id.as_str(),
                    path.display()
                ))
            })?;
        for (field_name, value) in [
            ("id", operation.id.as_str()),
            ("request_label", operation.request_label.as_str()),
            ("output_label", operation.output_label.as_str()),
            ("description", operation.description.as_str()),
        ] {
            if value.is_empty() || value.trim() != value {
                return Err(contract(format!(
                    "interface '{}' operation '{}' has malformed {}",
                    interface.id.as_str(),
                    operation.id,
                    field_name
                )));
            }
        }
        if operation.id.contains('/') || operation.id.contains('\\') {
            return Err(contract(format!(
                "interface '{}' operation id '{}' is not a file-safe identity",
                interface.id.as_str(),
                operation.id
            )));
        }
        if operation.request_label != request_label {
            return Err(contract(format!(
                "interface '{}' operation '{}' request label '{}' differs from catalog label '{}'",
                interface.id.as_str(),
                operation.id,
                operation.request_label,
                request_label
            )));
        }
        if !operation.input_schema.is_object() {
            return Err(contract(format!(
                "interface '{}' operation '{}' input_schema is not an object",
                interface.id.as_str(),
                operation.id
            )));
        }
        let expected_file = format!("{}.json", operation.id);
        if path.file_name().and_then(|value| value.to_str()) != Some(expected_file.as_str()) {
            return Err(contract(format!(
                "interface '{}' operation '{}' must live in '{}', found '{}'",
                interface.id.as_str(),
                operation.id,
                expected_file,
                path.display()
            )));
        }
        if !ids.insert(operation.id.clone()) {
            return Err(contract(format!(
                "interface '{}' operation catalog repeats '{}'",
                interface.id.as_str(),
                operation.id
            )));
        }
        let descriptor = serde_json::to_value(&operation).map_err(|error| {
            contract(format!(
                "interface '{}' operation '{}' cannot be normalized: {error}",
                interface.id.as_str(),
                operation.id
            ))
        })?;
        operations.push(FormedInterfaceOperation {
            id: operation.id,
            request_label: operation.request_label,
            output_label: operation.output_label,
            description: operation.description,
            input_schema: operation.input_schema,
            descriptor,
        });
    }
    if operations.is_empty() {
        return Err(contract(format!(
            "interface '{}' operation catalog is empty",
            interface.id.as_str()
        )));
    }
    Ok(Some(InterfaceOperationCapabilityBinding {
        interface: interface.clone(),
        catalog_dir,
        catalog_label,
        request_label,
        operations,
    }))
}

/// POLICY.md §5 — the EE-chain leg of formation is the run's place in the LIVE
/// instance tree, not a client-declared template pair: the chain is read from
/// the persisted `ee_instances` rows by walking `parent_ee_id` to the root, at any
/// nesting depth, and every layer is fail-closed data.
pub fn resolve_live_ee_template_chain(
    ee_instances_root: &Path,
    ee: &EeId,
) -> Result<Vec<EeTemplateId>, CapabilityFormationError> {
    Ok(resolve_live_ee_policy_chain(ee_instances_root, ee)?
        .into_iter()
        .map(|layer| layer.template)
        .collect())
}

pub fn resolve_live_ee_policy_chain(
    ee_instances_root: &Path,
    ee: &EeId,
) -> Result<Vec<LiveEePolicyLayer>, CapabilityFormationError> {
    let mut chain = Vec::new();
    let mut visited = BTreeSet::new();
    let mut cursor = ee.clone();
    loop {
        if !visited.insert(cursor.clone()) {
            return Err(contract(format!(
                "EE instance chain of '{}' cycles at '{}'",
                ee.as_str(),
                cursor.as_str()
            )));
        }
        let path = ee_instances_root.join(format!("{}.json", cursor.as_str()));
        let bytes = std::fs::read(&path).map_err(|error| {
            contract(format!(
                "EE instance '{}' cannot be read at '{}': {error}",
                cursor.as_str(),
                path.display()
            ))
        })?;
        let instance: EeInstance = serde_json::from_slice(&bytes).map_err(|error| {
            contract(format!(
                "EE instance '{}' is malformed at '{}': {error}",
                cursor.as_str(),
                path.display()
            ))
        })?;
        if instance.ee_id != cursor {
            return Err(contract(format!(
                "EE instance file '{}' declares id '{}' instead of '{}'",
                path.display(),
                instance.ee_id.as_str(),
                cursor.as_str()
            )));
        }
        chain.push(LiveEePolicyLayer {
            template: instance.template.clone(),
            policy_narrowing: instance.policy_narrowing,
        });
        match instance.parent_ee {
            Some(parent) => cursor = parent,
            None => break,
        }
    }
    chain.reverse();
    Ok(chain)
}

fn resolve_ee_chain<'a>(
    request: &CapabilityFormationRequest,
    templates: &'a BTreeMap<EeTemplateId, EeTemplateDescriptor>,
) -> Result<Vec<&'a EeTemplateDescriptor>, CapabilityFormationError> {
    if request.ee_template_chain.is_empty() {
        return Err(contract("EE template chain must not be empty"));
    }
    if request.ee_template_chain.len() != request.ee_instance_policy_narrowing.len() {
        return Err(contract(format!(
            "EE template chain has {} layers while instance-policy chain has {}",
            request.ee_template_chain.len(),
            request.ee_instance_policy_narrowing.len()
        )));
    }
    let mut chain = Vec::with_capacity(request.ee_template_chain.len());
    for id in &request.ee_template_chain {
        let template = templates.get(id).ok_or_else(|| {
            contract(format!(
                "EE template chain references missing template '{}'",
                id.as_str()
            ))
        })?;
        chain.push(template);
    }
    for pair in chain.windows(2) {
        let parent = pair[0];
        let child = pair[1];
        if !parent
            .allowed_child_ee_templates
            .iter()
            .any(|allowed| allowed == child.id.as_str())
        {
            return Err(contract(format!(
                "EE template '{}' does not admit child template '{}'",
                parent.id.as_str(),
                child.id.as_str()
            )));
        }
    }
    Ok(chain)
}

fn ensure_worker_admitted_by_ee_chain(
    worker: &InterfaceDescriptor,
    chain: &[&EeTemplateDescriptor],
    instance_policy_narrowing: &[EeInstancePolicyNarrowing],
    lineage: &CopyLineage,
) -> Result<(), CapabilityFormationError> {
    let admits = |allowed: &[String]| {
        lineage.admits_interface(
            &CapSet::from_allowed(allowed.iter().map(InterfaceId::new)),
            &worker.id,
        )
    };
    for (template, instance) in chain.iter().zip(instance_policy_narrowing) {
        let Some(allowed) = &template.allowed_interfaces else {
            if let Some(allowed) = &instance.allowed_interfaces {
                if !admits(allowed) {
                    return Err(contract(format!(
                        "EE instance of template '{}' does not admit worker '{}'",
                        template.id.as_str(),
                        worker.id.as_str()
                    )));
                }
            }
            continue;
        };
        if !admits(allowed) {
            return Err(contract(format!(
                "EE template '{}' does not admit worker '{}'",
                template.id.as_str(),
                worker.id.as_str()
            )));
        }
        if let Some(allowed) = &instance.allowed_interfaces {
            if !admits(allowed) {
                return Err(contract(format!(
                    "EE instance of template '{}' does not admit worker '{}'",
                    template.id.as_str(),
                    worker.id.as_str()
                )));
            }
        }
    }
    Ok(())
}

fn ensure_tool_owner_reader(
    owner: &InterfaceDescriptor,
    interface_root: &Path,
    worker_policies: &BTreeMap<InterfaceId, WorkerInterfacePolicy>,
) -> Result<(), CapabilityFormationError> {
    let disposition = form_interface_runtime_disposition(
        owner,
        interface_root,
        worker_policies.contains_key(&owner.id),
    )
    .map_err(|error| {
        contract(format!(
            "tool owner interface '{}' has invalid runtime disposition: {error}",
            owner.id.as_str()
        ))
    })?;
    if disposition != InterfaceRuntimeDisposition::ExternalReader {
        return Err(contract(format!(
            "tool owner interface '{}' is not an external reader",
            owner.id.as_str()
        )));
    }
    Ok(())
}

fn form_tool_worker_environment_kind(
    tool_id: &ToolId,
    tool: &ToolDescriptor,
    formed_kinds: &octopus_policy::CapSet<WorkerEnvironmentKindId>,
    kinds: &BTreeMap<WorkerEnvironmentKindId, WorkerEnvKindSpec>,
) -> Result<WorkerEnvironmentKindId, CapabilityFormationError> {
    let [kind_id] = tool.supported_worker_environment_kinds.as_slice() else {
        return Err(contract(format!(
            "tool '{}' must name exactly one worker-environment code kind",
            tool_id.as_str()
        )));
    };
    kinds.get(kind_id).ok_or_else(|| {
        contract(format!(
            "tool '{}' references missing worker-environment kind '{}'",
            tool_id.as_str(),
            kind_id.as_str()
        ))
    })?;
    if !formed_kinds.contains(kind_id) {
        return Err(contract(format!(
            "formed tool '{}' has no compatible worker-environment kind",
            tool_id.as_str()
        )));
    }
    Ok(kind_id.clone())
}

fn append_sidecars(
    binding: &mut ToolCapabilityBinding,
    kind: &WorkerEnvKindSpec,
) -> Result<(), CapabilityFormationError> {
    for file_name in &kind.instance_config_files {
        if let Some(existing) = binding
            .sidecars
            .iter()
            .find(|sidecar| sidecar.file_name == *file_name)
        {
            if existing.worker_environment_kind != kind.worker_environment_kind_id {
                return Err(contract(format!(
                    "interface '{}' sidecar '{}' is ambiguously required by kinds '{}' and '{}'",
                    binding.interface.id.as_str(),
                    file_name,
                    existing.worker_environment_kind.as_str(),
                    kind.worker_environment_kind_id.as_str()
                )));
            }
            continue;
        }
        binding.sidecars.push(CapabilitySidecarRequirement {
            file_name: file_name.clone(),
            worker_environment_kind: kind.worker_environment_kind_id.clone(),
        });
    }
    Ok(())
}

fn registry_error(
    surface: &'static str,
    error: impl std::fmt::Display,
) -> CapabilityFormationError {
    CapabilityFormationError::Registry {
        surface,
        reason: error.to_string(),
    }
}

fn contract(message: impl Into<String>) -> CapabilityFormationError {
    CapabilityFormationError::Contract(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_ee_chain_resolves_from_persisted_instances_at_any_depth() {
        let dir = tempfile::tempdir().unwrap();
        let instance = |ee: &str, template: &str, parent: Option<&str>| {
            let row = octopus_ee::EeInstance {
                name: None,
                ee_id: EeId::new(ee),
                template: EeTemplateId::new(template),
                parent_ee: parent.map(EeId::new),
                goal: format!("goal of {template}"),
                config: Default::default(),
                policy_narrowing: Default::default(),
            };
            std::fs::write(
                dir.path().join(format!("{ee}.json")),
                serde_json::to_vec_pretty(&row).unwrap(),
            )
            .unwrap();
        };
        instance("ee_root", "existence", None);
        instance("ee_mid", "operator_interaction", Some("ee_root"));
        instance("ee_leaf", "goal_review", Some("ee_mid"));

        let chain = resolve_live_ee_template_chain(dir.path(), &EeId::new("ee_leaf")).unwrap();
        assert_eq!(
            chain,
            vec![
                EeTemplateId::new("existence"),
                EeTemplateId::new("operator_interaction"),
                EeTemplateId::new("goal_review"),
            ],
            "the chain is the live parent walk root→leaf, not a declared template pair"
        );

        instance("ee_loop_a", "existence", Some("ee_loop_b"));
        instance("ee_loop_b", "existence", Some("ee_loop_a"));
        assert!(
            resolve_live_ee_template_chain(dir.path(), &EeId::new("ee_loop_a")).is_err(),
            "a cyclic parent walk fails closed instead of spinning"
        );
        assert!(
            resolve_live_ee_template_chain(dir.path(), &EeId::new("ee_absent")).is_err(),
            "a missing instance row fails closed"
        );
    }

    #[test]
    fn tool_code_world_cannot_be_absent_during_capability_formation() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .unwrap();
        let registry = repository.join("registry");
        let tool_id = ToolId::new("artifact.read");
        let mut tool = octopus_tool_registry::load_tool_descriptor_from_dir(
            &registry.join("tool"),
            tool_id.as_str(),
        )
        .unwrap()
        .unwrap();
        tool.supported_worker_environment_kinds.clear();
        let error = form_tool_worker_environment_kind(
            &tool_id,
            &tool,
            &octopus_policy::CapSet::default(),
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("must name exactly one"));
    }

    #[test]
    fn absent_ceiling_interfaces_do_not_become_bindings_or_required_dependencies() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry").canonicalize().unwrap();
        let root = tempfile::tempdir().unwrap();
        let registry = root.path().join("registry");
        let write = |member: &str, value: serde_json::Value| {
            let path = registry.join(member);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        };
        for member in ["interface/system_gui/interface.json", "tool/gui.status/tool.json", "worker_environment_kind/desktop_session/worker_environment_kind.json", "ee_template/existence/ee_template.json"] {
            let path = registry.join(member);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::copy(source.join(member), path).unwrap();
        }
        write("interface/system_worker_fixture/interface.json", serde_json::json!({"id":"system_worker_fixture","interface_type":"system","role":"worker","direction":"bidirectional","source_names":["fixture"]}));
        write("interface/system_worker_fixture/policy.json", serde_json::json!({"id":"system_worker_fixture","ceiling":{"interfaces":["system_worker_fixture","system_gui","system_worker_future","world_provider_future"],"tools":["gui.status"],"wek":["desktop_session"]},"role_artifacts":[]}));
        let request = CapabilityFormationRequest::new(InterfaceId::new("system_worker_fixture"), [EeTemplateId::new("existence")]);
        let policy_path = registry.join("interface/system_worker_fixture/policy.json");
        let policy_before = std::fs::read(&policy_path).unwrap();
        let topology = form_capability_topology(&registry, &request).unwrap();
        assert_eq!(topology.granted_tools, vec![ToolId::new("gui.status")]);
        assert_eq!(topology.tool_bindings.len(), 1);
        assert_eq!(topology.worker_relays.len(), 1);
        for absent in ["system_worker_future", "world_provider_future"] {
            let id = InterfaceId::new(absent);
            assert!(!topology.interface_policies.contains_key(&id));
            assert!(!topology.worker_relays.contains_key(&id));
            assert!(!topology.operation_bindings.contains_key(&id));
            assert!(!topology.tool_bindings.contains_key(&id));
            assert!(!registry.join("interface").join(absent).exists());
        }
        for (member, expected) in [("interface/system_worker_fixture", "worker interface 'system_worker_fixture' is absent"), ("tool/gui.status", "formed tool 'gui.status' has no descriptor"), ("interface/system_gui", "owns missing interface 'system_gui'"), ("worker_environment_kind/desktop_session", "missing worker-environment kind 'desktop_session'")] {
            let path = registry.join(member);
            let removed = root.path().join("removed");
            std::fs::rename(&path, &removed).unwrap();
            let error = form_capability_topology(&registry, &request).unwrap_err().to_string();
            std::fs::rename(removed, path).unwrap();
            assert!(error.contains(expected), "{error}");
        }
        write("interface/system_worker_future/interface.json", serde_json::json!({"id":"system_worker_future","interface_type":"system","role":"worker","direction":"bidirectional","source_names":["fixture"]}));
        assert!(form_capability_topology(&registry, &request).unwrap_err().to_string().contains("refused worker policy evidence"));
        write("interface/system_worker_future/interface.json", serde_json::json!({"id":"system_worker_future","interface_type":42}));
        assert!(matches!(form_capability_topology(&registry, &request), Err(CapabilityFormationError::Registry { surface: "interface", .. })));
        assert_eq!(std::fs::read(policy_path).unwrap(), policy_before);
        assert_eq!(std::fs::read(registry.join("ee_template/existence/ee_template.json")).unwrap(), std::fs::read(source.join("ee_template/existence/ee_template.json")).unwrap());
    }

    #[test]
    fn independent_maintenance_root_forms_without_bootstrap_ancestry() {
        let registry = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../registry").canonicalize().unwrap();
        let request = CapabilityFormationRequest::new(InterfaceId::new("system_worker_client_maintenance"),
            [EeTemplateId::new("client_maintenance")]);
        let topology = form_capability_topology(&registry, &request).unwrap();
        assert!(topology.granted_tools.is_empty());
        assert!(topology.tool_bindings.is_empty());
        assert_eq!(topology.ee_template_chain, request.ee_template_chain);
        let denied = CapabilityFormationRequest::new(InterfaceId::new("system_worker_client_maintenance"),
            [EeTemplateId::new("client_maintenance"), EeTemplateId::new("existence")]);
        assert!(form_capability_topology(&registry, &denied).is_err());
    }

    #[test]
    fn real_registry_forms_the_full_permitted_surface_once_per_owner_interface() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .unwrap();
        let registry = repository.join("registry");
        let request = CapabilityFormationRequest::new(
            InterfaceId::new("system_worker_coder_max"),
            [
                EeTemplateId::new("existence"),
                EeTemplateId::new("operator_interaction"),
            ],
        );
        let topology = form_capability_topology(&registry, &request).unwrap();

        let worker_policy: WorkerInterfacePolicy = serde_json::from_slice(
            &std::fs::read(registry.join("interface/system_worker_coder_max/policy.json")).unwrap(),
        )
        .unwrap();
        let expected_tools = worker_policy
            .ceiling
            .tools
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let routed_tools = topology
            .tool_bindings
            .values()
            .flat_map(|binding| binding.tools.iter().map(|tool| tool.tool.clone()))
            .collect::<BTreeSet<_>>();
        assert_eq!(routed_tools, expected_tools);
        assert_eq!(
            topology
                .granted_tools
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            expected_tools
        );

        let context = &topology.tool_bindings[&InterfaceId::new("system_context")];
        assert!(
            context
                .tools
                .iter()
                .any(|tool| tool.tool == ToolId::new("artifact.read")
                    && tool.worker_environment_kind == WorkerEnvironmentKindId::new("system_room"))
        );
        let room = &topology.tool_bindings[&InterfaceId::new("system_room")];
        assert!(
            room.tools
                .iter()
                .any(|tool| tool.tool == ToolId::new("file.write")
                    && tool.worker_environment_kind == WorkerEnvironmentKindId::new("system_room"))
        );

        let browser = &topology.tool_bindings[&InterfaceId::new("system_browser")];
        assert_eq!(
            browser
                .worker_environment_kinds
                .iter()
                .map(WorkerEnvironmentKindId::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["browser_session"])
        );
        let web_search = &topology.tool_bindings[&InterfaceId::new("system_web_search")];
        assert_eq!(
            web_search
                .worker_environment_kinds
                .iter()
                .map(WorkerEnvironmentKindId::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["web_search"])
        );
        let exec = &topology.tool_bindings[&InterfaceId::new("system_exec_env")];
        assert_eq!(
            exec.worker_environment_kinds
                .iter()
                .map(WorkerEnvironmentKindId::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["process_session"])
        );
        let remote_exec = &topology.tool_bindings[&InterfaceId::new("system_remote_exec")];
        assert_eq!(
            remote_exec
                .worker_environment_kinds
                .iter()
                .map(WorkerEnvironmentKindId::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["remote_exec_server"])
        );
        assert!(
            remote_exec
                .sidecars
                .iter()
                .any(|sidecar| sidecar.file_name == "remote_exec.json"
                    && sidecar.worker_environment_kind
                        == WorkerEnvironmentKindId::new("remote_exec_server"))
        );

        for interface in [
            "world_mcp_firecrawl",
            "world_mcp_github",
            "world_mcp_unity",
        ] {
            let id = InterfaceId::new(interface);
            let binding = &topology.operation_bindings[&id];
            assert!(!binding.operations.is_empty());
            assert_eq!(binding.catalog_label, "mcp.operation_catalog");
            assert_eq!(binding.request_label, "mcp.operation.request");
            assert!(!topology.tool_bindings.contains_key(&id));
        }

        for relay in [
            "system_worker_coder_max",
            "system_worker_coder",
            "system_worker_planner",
            "system_worker_memory",
            "system_worker_web",
            "system_worker_flash",
        ] {
            assert!(
                topology
                    .worker_relays
                    .contains_key(&InterfaceId::new(relay))
            );
        }
        for unrelated in [
            "world_provider_codex",
            "world_terminal",
            "world_operator_chat",
        ] {
            let id = InterfaceId::new(unrelated);
            assert!(!topology.tool_bindings.contains_key(&id));
            assert!(!topology.operation_bindings.contains_key(&id));
            assert!(!topology.worker_relays.contains_key(&id));
        }
    }
}
