use std::collections::{BTreeMap, BTreeSet};

use octopus_descriptors::{
    ArtifactId, DescriptorHash, DescriptorName, EeId, InterfaceId, ToolId, WorkerEnvironmentKindId,
};
use octopus_ee::EeTree;
use octopus_interface_projection::{InterfaceToolOwnerMap, admissible_interface_tools};
use octopus_policy::{
    ArtifactAccess, CapSet, ContextGrant, CopyLineage, FormationPin, FormedPolicy, PolicyLayer,
    WorkerInterfacePolicy, form_policy_with_lineage,
};

#[derive(Debug)]
pub enum WorkerPolicyError {
    UnknownEe(String),
    EeChain(String),
    WorkerNotAdmittedByEeChain { worker: String, ee: String },
}

impl std::fmt::Display for WorkerPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerPolicyError::UnknownEe(e) => write!(f, "unknown EE instance for worker run: {e}"),
            WorkerPolicyError::EeChain(e) => write!(f, "EE chain policy projection failed: {e}"),
            WorkerPolicyError::WorkerNotAdmittedByEeChain { worker, ee } => write!(
                f,
                "run claims worker interface '{worker}' but the EE chain of '{ee}' does not allow that interface; a run cannot lift itself above its place in the tree"
            ),
        }
    }
}
impl std::error::Error for WorkerPolicyError {}

pub fn worker_interface_descriptor_name(worker: &InterfaceId) -> DescriptorName {
    DescriptorName::new(format!(
        "registry_interface_root/{}/policy.json",
        worker.as_str()
    ))
}

pub fn interface_descriptor_source_name(interface: &InterfaceId) -> DescriptorName {
    DescriptorName::new(format!(
        "registry_interface_root/{}/interface.json",
        interface.as_str()
    ))
}

pub fn ee_template_source_name(template: &str) -> DescriptorName {
    DescriptorName::new(format!(
        "registry_ee_template_root/{template}/ee_template.json"
    ))
}

pub fn ee_instance_source_name(ee: &EeId) -> DescriptorName {
    DescriptorName::new(format!("ee_instances_root/{}.json", ee.as_str()))
}

pub fn tool_descriptor_source_name(tool: &ToolId) -> DescriptorName {
    DescriptorName::new(format!("registry_tool_root/{}/tool.json", tool.as_str()))
}

pub fn worker_environment_kind_source_name(kind: &WorkerEnvironmentKindId) -> DescriptorName {
    DescriptorName::new(format!(
        "registry_worker_environment_kind_root/{}/worker_environment_kind.json",
        kind.as_str()
    ))
}

pub fn artifact_descriptor_source_name(artifact: &ArtifactId) -> DescriptorName {
    DescriptorName::new(format!(
        "registry_artifact_root/{}/artifact.json",
        artifact.as_str()
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RoleArtifactDependency {
    pub(crate) required_by: InterfaceId,
    pub(crate) artifact: ArtifactId,
}

pub(crate) fn mandatory_role_artifact_dependencies(
    worker: &InterfaceId,
    worker_policy: &WorkerInterfacePolicy,
    formed: &FormedPolicy,
    worker_policies: &BTreeMap<InterfaceId, WorkerInterfacePolicy>,
) -> Vec<RoleArtifactDependency> {
    let mut seen = BTreeSet::new();
    let mut dependencies = Vec::new();
    let mut append = |required_by: &InterfaceId, role_artifacts: &[ArtifactId]| {
        for artifact in role_artifacts {
            if seen.insert(artifact.clone()) {
                dependencies.push(RoleArtifactDependency {
                    required_by: required_by.clone(),
                    artifact: artifact.clone(),
                });
            }
        }
    };

    append(worker, &worker_policy.role_artifacts);
    for interface in formed.context.interfaces.iter() {
        if interface == worker {
            continue;
        }
        if let Some(target_policy) = worker_policies.get(interface) {
            append(interface, &target_policy.role_artifacts);
        }
    }
    dependencies
}

/// The authored-bindings leg of POLICY.md §5 `grant = worker-interface ceiling ∩
/// the EE chain the run lives in ∩ the run's authored bindings`: the interface
/// folders the run's active checkpoints pin (`interface_binding` + `outbound_interface_bindings`),
/// plus the interface that owns each tool and the worker-environment kinds those tools
/// reach. Context tools and WEK are derived from the run's active pinned doors, never
/// from the whole ceiling.
pub struct AuthoredBindings<'a> {
    pub interfaces: &'a [InterfaceId],
    pub tool_ownership: &'a InterfaceToolOwnerMap,
    pub tool_supported_weks: &'a BTreeMap<ToolId, BTreeSet<WorkerEnvironmentKindId>>,
    /// POLICY.md §4 — the run's authored object-level artifact rights (the artifact
    /// leg of §5). Masked down to the composed artifact-verb ceiling by formation.
    pub artifact_access: &'a BTreeMap<ArtifactId, ArtifactAccess>,
}

/// POLICY.md §5: `grant = worker-interface ceiling ∩ the EE chain the run lives in
/// ∩ the run's authored bindings`. The ceiling seed is the `policy.json` of the
/// worker System-interface the run names in `worker`; the EE chain narrows it; the
/// authored-bindings leg then cuts the context down to the doors the checkpoint
/// actually pins. The EE chain must itself allow the worker interface — a run.json
/// claiming a pin its EE does not permit fails closed here, before any grant exists.
pub fn form_worker_run_policy(
    worker: &InterfaceId,
    policy: &WorkerInterfacePolicy,
    ee_tree: &EeTree,
    ee_id: &EeId,
    formation_source_hashes: &BTreeMap<DescriptorName, DescriptorHash>,
    authored: &AuthoredBindings<'_>,
    lineage: &CopyLineage,
) -> Result<FormedPolicy, WorkerPolicyError> {
    let ceiling: ContextGrant = policy.ceiling_grant();

    if ee_tree.get(ee_id).is_none() {
        return Err(WorkerPolicyError::UnknownEe(ee_id.as_str().to_string()));
    }
    let ee_layers: Vec<PolicyLayer> = ee_tree
        .policy_layers(ee_id)
        .map_err(|e| WorkerPolicyError::EeChain(e.to_string()))?;

    for layer in &ee_layers {
        if let Some(interfaces) = &layer.interfaces {
            if !lineage.admits_interface(interfaces, worker) {
                return Err(WorkerPolicyError::WorkerNotAdmittedByEeChain {
                    worker: worker.as_str().to_string(),
                    ee: ee_id.as_str().to_string(),
                });
            }
        }
    }

    let mut inputs: BTreeMap<DescriptorName, DescriptorHash> = BTreeMap::new();
    let worker_policy_source = worker_interface_descriptor_name(worker);
    let worker_policy_hash = formation_source_hashes
        .get(&worker_policy_source)
        .ok_or_else(|| {
            WorkerPolicyError::EeChain(format!(
                "worker interface '{}' policy has no formation-source hash",
                worker.as_str()
            ))
        })?;
    inputs.insert(worker_policy_source, *worker_policy_hash);
    let worker_descriptor_source = interface_descriptor_source_name(worker);
    let worker_descriptor_hash = formation_source_hashes
        .get(&worker_descriptor_source)
        .ok_or_else(|| {
            WorkerPolicyError::EeChain(format!(
                "worker interface '{}' descriptor has no formation-source hash",
                worker.as_str()
            ))
        })?;
    inputs.insert(worker_descriptor_source, *worker_descriptor_hash);
    for ee in ee_tree
        .ancestors(ee_id)
        .map_err(|error| WorkerPolicyError::EeChain(error.to_string()))?
    {
        let instance = ee_tree.get(&ee).ok_or_else(|| {
            WorkerPolicyError::EeChain(format!(
                "EE chain lost instance '{}' while forming descriptor evidence",
                ee.as_str()
            ))
        })?;
        let name = ee_template_source_name(instance.template.as_str());
        let hash = formation_source_hashes.get(&name).ok_or_else(|| {
            WorkerPolicyError::EeChain(format!(
                "EE template '{}' has no content hash in the loaded registry snapshot",
                instance.template.as_str()
            ))
        })?;
        inputs.insert(name, *hash);
        let instance_name = ee_instance_source_name(&instance.ee_id);
        let instance_hash = formation_source_hashes.get(&instance_name).ok_or_else(|| {
            WorkerPolicyError::EeChain(format!(
                "EE instance '{}' has no formation-source hash",
                instance.ee_id.as_str()
            ))
        })?;
        inputs.insert(instance_name, *instance_hash);
    }
    let pin = FormationPin { inputs };

    let binding_layers: Vec<PolicyLayer> = if authored.artifact_access.is_empty() {
        Vec::new()
    } else {
        vec![PolicyLayer {
            artifact_access: authored.artifact_access.clone(),
            ..Default::default()
        }]
    };
    let mut formed = form_policy_with_lineage(ceiling, &ee_layers, &binding_layers, pin, lineage);
    for tool in formed.context.tools.iter() {
        let name = tool_descriptor_source_name(tool);
        if let Some(hash) = formation_source_hashes.get(&name) {
            formed.pin.inputs.insert(name, *hash);
        }
    }
    narrow_context_to_authored_doors(&mut formed, authored, formation_source_hashes)?;
    for interface in formed.context.interfaces.iter() {
        let name = interface_descriptor_source_name(interface);
        let hash = formation_source_hashes.get(&name).ok_or_else(|| {
            WorkerPolicyError::EeChain(format!(
                "formed interface '{}' has no formation-source hash",
                interface.as_str()
            ))
        })?;
        formed.pin.inputs.insert(name, *hash);
    }
    for tool in formed.context.tools.iter() {
        let name = tool_descriptor_source_name(tool);
        let hash = formation_source_hashes.get(&name).ok_or_else(|| {
            WorkerPolicyError::EeChain(format!(
                "formed tool '{}' has no formation-source hash",
                tool.as_str()
            ))
        })?;
        formed.pin.inputs.insert(name, *hash);
    }
    for kind in formed.context.wek.iter() {
        let name = worker_environment_kind_source_name(kind);
        let hash = formation_source_hashes.get(&name).ok_or_else(|| {
            WorkerPolicyError::EeChain(format!(
                "formed worker environment kind '{}' has no formation-source hash",
                kind.as_str()
            ))
        })?;
        formed.pin.inputs.insert(name, *hash);
    }
    Ok(formed)
}

/// The authored-bindings leg of §5. `boundary` stays the static ceiling (§3a);
/// only `context` (the per-run grant, §3b) is cut to active run doors. A pinned
/// folder is admitted only when that exact interface survived the ceiling∩EE reach
/// and its descriptor is in the loaded registry snapshot; a pin to a folder that is
/// gone drops here, and the runtime refuses that route at send time (§6).
/// Tools narrow to those an admitted door owns; WEK narrows to the kinds those
/// surviving tools reach.
fn narrow_context_to_authored_doors(
    formed: &mut FormedPolicy,
    authored: &AuthoredBindings<'_>,
    formation_source_hashes: &BTreeMap<DescriptorName, DescriptorHash>,
) -> Result<(), WorkerPolicyError> {
    let reach = &formed.context.interfaces;
    let doors: BTreeSet<InterfaceId> = authored
        .interfaces
        .iter()
        .filter(|folder| {
            reach.contains(folder)
                && formation_source_hashes.contains_key(&interface_descriptor_source_name(folder))
        })
        .cloned()
        .collect();
    formed.context.interfaces = CapSet::from_allowed(doors);

    formed.context.tools = admissible_interface_tools(&formed.context, authored.tool_ownership);

    let reachable_wek: BTreeSet<WorkerEnvironmentKindId> = formed
        .context
        .tools
        .iter()
        .filter_map(|tool| authored.tool_supported_weks.get(tool))
        .flatten()
        .cloned()
        .collect();
    formed.context.wek = formed
        .context
        .wek
        .intersect(&CapSet::from_allowed(reachable_wek));
    let tools = formed
        .context
        .tools
        .iter()
        .filter_map(|tool| {
            let supported = match authored.tool_supported_weks.get(tool) {
                Some(supported) => supported,
                None => {
                    return Some(Err(WorkerPolicyError::EeChain(format!(
                        "formed tool '{}' has no worker-environment compatibility descriptor",
                        tool.as_str()
                    ))));
                }
            };
            if supported.is_empty()
                || supported
                    .iter()
                    .any(|kind| formed.context.wek.contains(kind))
            {
                Some(Ok(tool.clone()))
            } else {
                None
            }
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    formed.context.tools = CapSet::from_allowed(tools);
    Ok(())
}
