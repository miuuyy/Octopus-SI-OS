use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use octopus_descriptors::{ArtifactId, Descriptor, DescriptorHash, DescriptorName, InterfaceId};
use octopus_ee::EeTemplateDescriptor;
use octopus_interfaces::{
    InterfaceDescriptor, InterfaceRuntimeDisposition, form_interface_runtime_disposition,
    scan_worker_interface_policies,
};
use octopus_marks::MarkDescriptor;
use octopus_policy::WorkerInterfacePolicy;
use octopus_tool_registry::ToolDescriptor;
use octopus_verifiers::VerifierDescriptor;
use octopus_worker_env::{
    WorkerEnvKindSpec, WorkerEnvironmentCeilingView, WorkerEnvironmentRegistrySubject,
    WorkerEnvironmentToolView, worker_environment_registry_issues,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RegistryNodeKind {
    Interface,
    Tool,
    WorkerEnvironmentKind,
    EeTemplate,
    Mark,
    Verifier,
    Artifact,
}

impl RegistryNodeKind {
    fn label(self) -> &'static str {
        match self {
            Self::Interface => "interface",
            Self::Tool => "tool",
            Self::WorkerEnvironmentKind => "worker_environment_kind",
            Self::EeTemplate => "ee_template",
            Self::Mark => "mark",
            Self::Verifier => "verifier",
            Self::Artifact => "artifact",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RegistryNode {
    pub(crate) kind: RegistryNodeKind,
    pub(crate) id: String,
}

impl RegistryNode {
    pub(crate) fn interface(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Interface, id)
    }

    pub(crate) fn tool(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Tool, id)
    }

    pub(crate) fn wek(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::WorkerEnvironmentKind, id)
    }

    pub(crate) fn ee_template(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::EeTemplate, id)
    }

    pub(crate) fn mark(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Mark, id)
    }

    pub(crate) fn verifier(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Verifier, id)
    }

    pub(crate) fn artifact(id: impl Into<String>) -> Self {
        Self::new(RegistryNodeKind::Artifact, id)
    }

    fn new(kind: RegistryNodeKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

impl std::fmt::Display for RegistryNode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.kind.label(), self.id)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RegistryReference {
    pub(crate) source: RegistryNode,
    pub(crate) field: String,
    pub(crate) target: RegistryNode,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RegistryReferenceGraph {
    available: BTreeSet<RegistryNode>,
    unavailable: BTreeMap<RegistryNode, String>,
    edges: BTreeMap<RegistryNode, Vec<RegistryReference>>,
    artifact_descriptor_hashes: BTreeMap<ArtifactId, DescriptorHash>,
    artifact_descriptor_bytes: BTreeMap<ArtifactId, Vec<u8>>,
}

impl RegistryReferenceGraph {
    fn register(&mut self, node: RegistryNode) {
        if !self.unavailable.contains_key(&node) {
            self.available.insert(node);
        }
    }

    fn refuse(&mut self, node: RegistryNode, reason: impl Into<String>) {
        self.available.remove(&node);
        self.unavailable.insert(node, reason.into());
    }

    fn link(&mut self, source: RegistryNode, field: impl Into<String>, target: RegistryNode) {
        self.edges
            .entry(source.clone())
            .or_default()
            .push(RegistryReference {
                source,
                field: field.into(),
                target,
            });
    }

    pub(crate) fn is_available(&self, node: &RegistryNode) -> bool {
        self.available.contains(node) && !self.unavailable.contains_key(node)
    }

    pub(crate) fn dependency_nodes(
        &self,
        roots: impl IntoIterator<Item = RegistryNode>,
    ) -> BTreeSet<RegistryNode> {
        let mut pending = roots.into_iter().collect::<Vec<_>>();
        let mut found = BTreeSet::new();
        while let Some(node) = pending.pop() {
            if found.insert(node.clone()) {
                pending.extend(
                    self.edges
                        .get(&node)
                        .into_iter()
                        .flatten()
                        .map(|edge| edge.target.clone()),
                );
            }
        }
        found
    }

    pub(crate) fn artifact_descriptor_hash(
        &self,
        artifact: &ArtifactId,
    ) -> Result<DescriptorHash, String> {
        let node = RegistryNode::artifact(artifact.as_str());
        if let Some(reason) = self.unavailable.get(&node) {
            return Err(format!("unavailable {node}: {reason}"));
        }
        if !self.available.contains(&node) {
            return Err(format!("missing {node}"));
        }
        self.artifact_descriptor_hashes
            .get(artifact)
            .copied()
            .ok_or_else(|| format!("available {node} has no loaded descriptor hash"))
    }

    pub(crate) fn artifact_descriptor_bytes(&self, artifact: &ArtifactId) -> Result<&[u8], String> {
        self.artifact_descriptor_hash(artifact)?;
        self.artifact_descriptor_bytes
            .get(artifact)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("artifact '{}' has no loaded descriptor bytes", artifact.as_str()))
    }

    pub(crate) fn validate_active_references(
        &self,
        source: &str,
        references: impl IntoIterator<Item = (String, RegistryNode)>,
    ) -> Result<BTreeSet<RegistryNode>, String> {
        let mut closure = BTreeSet::new();
        for (field, root) in references {
            let mut pending = vec![ReferenceVisit {
                node: &root,
                via: None,
            }];
            let mut queued = BTreeSet::from([&root]);
            let mut cursor = 0;
            while cursor < pending.len() {
                let node = pending[cursor].node;
                if !closure.contains(node) {
                    let unavailable = self.unavailable.get(node);
                    if unavailable.is_some() || !self.available.contains(node) {
                        if node.kind != RegistryNodeKind::Interface {
                            let path =
                                reference_visit_path(source, &field, &root, &pending, cursor);
                            return Err(match unavailable {
                                Some(reason) => format!(
                                    "active registry reference closure reached unavailable {node}: {reason}; path: {path}"
                                ),
                                None => format!(
                                    "active registry reference closure reached missing {node}; path: {path}"
                                ),
                            });
                        }
                    } else {
                        closure.insert(node.clone());
                        for edge in self.edges.get(node).into_iter().flatten() {
                            if !closure.contains(&edge.target) && queued.insert(&edge.target) {
                                pending.push(ReferenceVisit {
                                    node: &edge.target,
                                    via: Some((cursor, edge)),
                                });
                            }
                        }
                    }
                }
                cursor += 1;
            }
        }
        Ok(closure)
    }
}

struct ReferenceVisit<'a> {
    node: &'a RegistryNode,
    via: Option<(usize, &'a RegistryReference)>,
}

fn reference_visit_path(
    source: &str,
    field: &str,
    root: &RegistryNode,
    visits: &[ReferenceVisit<'_>],
    mut cursor: usize,
) -> String {
    let mut path = Vec::new();
    while let Some((parent, edge)) = visits[cursor].via {
        path.push(format!("{}.{} -> {}", edge.source, edge.field, edge.target));
        cursor = parent;
    }
    path.push(format!("{source}.{field} -> {root}"));
    path.reverse();
    path.join(" -> ")
}

#[derive(Clone, Debug)]
pub(crate) struct ActivationReferenceSet {
    pub(crate) source: String,
    pub(crate) references: Vec<(String, RegistryNode)>,
}

#[derive(Clone, Debug)]
pub(crate) struct DanglingCeilingRef {
    pub(crate) worker: String,
    pub(crate) field: String,
    pub(crate) target: RegistryNode,
}

#[derive(Clone, Debug)]
pub(crate) struct RegistryPackages {
    pub(crate) interfaces: Vec<(DescriptorName, InterfaceDescriptor)>,
    pub(crate) interface_dispositions: BTreeMap<InterfaceId, InterfaceRuntimeDisposition>,
    pub(crate) worker_policies: BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    pub(crate) tools: Vec<(DescriptorName, ToolDescriptor)>,
    pub(crate) worker_environment_kinds: Vec<(DescriptorName, WorkerEnvKindSpec)>,
    pub(crate) ee_templates: Vec<(DescriptorName, EeTemplateDescriptor)>,
    pub(crate) graph: RegistryReferenceGraph,
    pub(crate) activations: Vec<ActivationReferenceSet>,
    pub(crate) dangling_ceiling_refs: Vec<DanglingCeilingRef>,
}

struct KindLoad<T> {
    loaded: Vec<(DescriptorName, T)>,
    unavailable: BTreeMap<RegistryNode, String>,
}

impl RegistryPackages {
    pub(crate) fn load(registry_root: &Path) -> Result<Self, String> {
        let mut interface_load = load_kind::<InterfaceDescriptor>(
            &registry_root.join("interface"),
            RegistryNodeKind::Interface,
            false,
        )?;
        let tool_load = load_kind::<ToolDescriptor>(
            &registry_root.join("tool"),
            RegistryNodeKind::Tool,
            false,
        )?;
        let wek_load = load_kind::<WorkerEnvKindSpec>(
            &registry_root.join("worker_environment_kind"),
            RegistryNodeKind::WorkerEnvironmentKind,
            false,
        )?;
        let ee_load = load_kind::<EeTemplateDescriptor>(
            &registry_root.join("ee_template"),
            RegistryNodeKind::EeTemplate,
            false,
        )?;
        let mark_load =
            load_kind::<MarkDescriptor>(&registry_root.join("mark"), RegistryNodeKind::Mark, true)?;
        let verifier_load = load_kind::<VerifierDescriptor>(
            &registry_root.join("verifier"),
            RegistryNodeKind::Verifier,
            true,
        )?;

        let mut graph = RegistryReferenceGraph::default();
        for unavailable in [
            &interface_load.unavailable,
            &tool_load.unavailable,
            &wek_load.unavailable,
            &ee_load.unavailable,
            &mark_load.unavailable,
            &verifier_load.unavailable,
        ] {
            for (node, reason) in unavailable {
                graph.refuse(node.clone(), reason.clone());
            }
        }

        let policy_scan = scan_worker_interface_policies(
            &registry_root.join("interface"),
            &interface_load.loaded,
        );
        for (interface, reason) in policy_scan.refusals {
            graph.refuse(RegistryNode::interface(interface.as_str()), reason);
        }
        let worker_policies = policy_scan.policies;

        for (_, interface) in &interface_load.loaded {
            graph.register(RegistryNode::interface(interface.id.as_str()));
        }
        for (_, tool) in &tool_load.loaded {
            graph.register(RegistryNode::tool(tool.id.as_str()));
        }
        for (_, wek) in &wek_load.loaded {
            graph.register(RegistryNode::wek(wek.worker_environment_kind_id.as_str()));
        }
        for (_, template) in &ee_load.loaded {
            graph.register(RegistryNode::ee_template(template.id.as_str()));
        }
        for (_, mark) in &mark_load.loaded {
            graph.register(RegistryNode::mark(mark.id.as_str()));
        }
        for (_, verifier) in &verifier_load.loaded {
            graph.register(RegistryNode::verifier(verifier.id.as_str()));
        }
        load_artifacts(registry_root, &mut graph)?;

        link_tools(&tool_load.loaded, &mut graph);
        let interface_dispositions = form_interface_dispositions(
            registry_root,
            &interface_load.loaded,
            &worker_policies,
            &mut graph,
        );
        refuse_cross_package_mismatches(
            &worker_policies,
            &tool_load.loaded,
            &wek_load.loaded,
            &interface_load.loaded,
            &interface_dispositions,
            &mut graph,
        );
        let dangling_ceiling_refs = link_worker_ceilings(&worker_policies, &mut graph);
        let activations =
            load_activation_references(registry_root, &interface_load.loaded, &mut graph)?;

        interface_load.loaded.retain(|(_, descriptor)| {
            graph.is_available(&RegistryNode::interface(descriptor.id.as_str()))
        });
        let worker_policies = worker_policies
            .into_iter()
            .filter(|(id, _)| graph.is_available(&RegistryNode::interface(id.as_str())))
            .collect();
        let interface_dispositions = interface_dispositions
            .into_iter()
            .filter(|(id, _)| graph.is_available(&RegistryNode::interface(id.as_str())))
            .collect();
        let tools = tool_load
            .loaded
            .into_iter()
            .filter(|(_, descriptor)| {
                graph.is_available(&RegistryNode::tool(descriptor.id.as_str()))
            })
            .collect();
        let worker_environment_kinds = wek_load
            .loaded
            .into_iter()
            .filter(|(_, descriptor)| {
                graph.is_available(&RegistryNode::wek(
                    descriptor.worker_environment_kind_id.as_str(),
                ))
            })
            .collect();
        let ee_templates = ee_load
            .loaded
            .into_iter()
            .filter(|(_, descriptor)| {
                graph.is_available(&RegistryNode::ee_template(descriptor.id.as_str()))
            })
            .collect();
        Ok(Self {
            interfaces: interface_load.loaded,
            interface_dispositions,
            worker_policies,
            tools,
            worker_environment_kinds,
            ee_templates,
            graph,
            activations,
            dangling_ceiling_refs,
        })
    }
}

fn form_interface_dispositions(
    registry_root: &Path,
    interfaces: &[(DescriptorName, InterfaceDescriptor)],
    policies: &BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    graph: &mut RegistryReferenceGraph,
) -> BTreeMap<InterfaceId, InterfaceRuntimeDisposition> {
    let interface_root = registry_root.join("interface");
    let mut dispositions = BTreeMap::new();
    for (_, interface) in interfaces {
        let node = RegistryNode::interface(interface.id.as_str());
        if !graph.is_available(&node) {
            continue;
        }
        match form_interface_runtime_disposition(
            interface,
            &interface_root,
            policies.contains_key(&interface.id),
        ) {
            Ok(InterfaceRuntimeDisposition::Inert) => {
                graph.refuse(
                    node,
                    "interface declares neither an external reader nor a worker policy relay",
                );
            }
            Ok(disposition) => {
                dispositions.insert(interface.id.clone(), disposition);
            }
            Err(error) => graph.refuse(node, error),
        }
    }
    dispositions
}

pub fn registry_tool_output_sources(
    registry_root: &Path,
    tool_id: &str,
) -> Result<BTreeSet<InterfaceId>, String> {
    let packages = RegistryPackages::load(registry_root)?;
    let tool_node = RegistryNode::tool(tool_id);
    packages.graph.validate_active_references(
        &format!("tool/{tool_id}"),
        [("descriptor".to_string(), tool_node)],
    )?;
    let owner = packages
        .tools
        .iter()
        .find_map(|(_, tool)| (tool.id.as_str() == tool_id).then(|| tool.interface.clone()))
        .ok_or_else(|| format!("registry has no available '{tool_id}' tool descriptor"))?;
    let owner_node = RegistryNode::interface(owner.as_str());
    packages.graph.validate_active_references(
        &format!("interface/{}", owner.as_str()),
        [("descriptor".to_string(), owner_node)],
    )?;
    Ok(BTreeSet::from([owner]))
}

pub fn registry_worker_policy_issues(
    registry_root: &Path,
    workers: &[InterfaceId],
) -> Result<Vec<RegistryValidationIssue>, String> {
    let packages = RegistryPackages::load(registry_root)?;
    let mut issues = Vec::new();
    for worker in workers.iter().collect::<BTreeSet<_>>() {
        let node = RegistryNode::interface(worker.as_str());
        let reason = if packages.worker_policies.contains_key(worker) {
            packages.graph.validate_active_references(
                &node.to_string(), [("worker".to_string(), node.clone())],
            ).err()
        } else {
            Some(packages.graph.unavailable.get(&node).cloned().unwrap_or_else(|| {
                format!("worker interface '{}' has no loaded policy pin in the live registry snapshot", worker)
            }))
        };
        if let Some(reason) = reason {
            issues.push(RegistryValidationIssue {
                subject: node.to_string(), checkpoint: None, reason,
            });
        }
    }
    Ok(issues)
}

/// Inputs to a prospective file view. No caller-owned file is changed by validation.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryValidationRoots {
    pub registry_root: PathBuf,
    pub ee_instances_root: PathBuf,
    pub run_records_root: PathBuf,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryCandidateWrite {
    pub path: PathBuf,
    pub content: String,
    pub expected_before_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryValidationIssue {
    pub subject: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryValidationStage {
    pub after_write: usize,
    pub issues: Vec<RegistryValidationIssue>,
    pub available: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryCandidateValidation {
    pub safe_to_apply: bool,
    pub checked_subjects: Vec<String>,
    pub stages: Vec<RegistryValidationStage>,
    pub regressions: Vec<RegistryValidationIssue>,
}

impl RegistryValidationRoots {
    fn relative_target(&self, path: &Path) -> Result<PathBuf, String> {
        normal_absolute_path(path)?;
        for (name, root) in [
            ("registry", &self.registry_root),
            ("ee", &self.ee_instances_root),
            ("runs", &self.run_records_root),
        ] {
            normal_absolute_path(root)?;
            if let Ok(relative) = path.strip_prefix(root) {
                let parts = relative.components().collect::<Vec<_>>();
                let allowed = if name == "registry" {
                    parts.len() >= 3
                        && parts[0].as_os_str().to_str().is_some_and(|kind| {
                            [
                                "interface",
                                "tool",
                                "worker_environment_kind",
                                "ee_template",
                                "artifact",
                                "mark",
                                "verifier",
                            ]
                            .contains(&kind)
                        })
                } else {
                    parts.len() == 1
                        && relative.extension().and_then(|value| value.to_str()) == Some("json")
                };
                if !allowed {
                    return Err(format!(
                        "candidate path '{}' is outside descriptor/EE/run file shapes",
                        path.display()
                    ));
                }
                return Ok(Path::new(name).join(relative));
            }
        }
        Err(format!(
            "candidate path '{}' is outside the declared registry, EE and run roots",
            path.display()
        ))
    }
}

fn normal_absolute_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(format!(
            "candidate path '{}' must be absolute without traversal",
            path.display()
        ));
    }
    Ok(())
}

/// Check the complete candidate and every publication prefix through the same
/// loaders and grant formers used on arrival. Temporary files are a private view
/// only; applying remains an ordinary granted file tool. Existing isolated faults
/// remain evidence and do not prevent an unrelated repair. New active run faults,
/// regressions of existing packages, and unresolved final writes refuse apply.
pub fn validate_registry_candidate(
    roots: &RegistryValidationRoots,
    writes: &[RegistryCandidateWrite],
) -> Result<RegistryCandidateValidation, String> {
    validate_registry_candidate_with_inspector(roots, writes, |_, _| Ok(()))
}

pub struct RegistryCandidateStage<'a> {
    pub roots: &'a RegistryValidationRoots,
    snapshot: &'a Result<crate::spine_root::LoadedRegistrySnapshot, String>,
}

impl RegistryCandidateStage<'_> {
    pub fn run_grant(
        &self,
        record: &octopus_run_record::RunRecord,
        checkpoint: &octopus_descriptors::CheckpointId,
    ) -> Result<octopus_interfaces::GrantProjection, String> {
        crate::spine_root::prospective_run_grant(record, checkpoint, || self.snapshot.as_ref().map_err(Clone::clone))
    }
}

pub fn validate_registry_candidate_with_inspector(
    roots: &RegistryValidationRoots,
    writes: &[RegistryCandidateWrite],
    mut inspect: impl FnMut(usize, &RegistryCandidateStage<'_>) -> Result<(), String>,
) -> Result<RegistryCandidateValidation, String> {
    let mut targets = BTreeSet::new();
    for write in writes {
        roots.relative_target(&write.path)?;
        if !targets.insert(write.path.clone()) {
            return Err(format!(
                "candidate repeats target '{}'",
                write.path.display()
            ));
        }
        let before = match std::fs::symlink_metadata(&write.path) {
            Ok(metadata) if metadata.is_file() => {
                Some(std::fs::read(&write.path).map_err(|error| error.to_string())?)
            }
            Ok(_) => {
                return Err(format!(
                    "candidate target '{}' must be a regular file",
                    write.path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        if before
            .as_deref()
            .map(octopus_ledger::ContentHash::of_bytes)
            .map(|hash| hash.to_hex())
            != write.expected_before_hash
        {
            return Err(format!(
                "candidate preimage changed at '{}'",
                write.path.display()
            ));
        }
    }
    let mut view = CandidateView::new(roots)?;
    let (stage, snapshot) = view.inspect(0, None)?;
    let mut stages = vec![stage];
    let prospective = RegistryValidationRoots {
        registry_root:view.root.join("registry"), ee_instances_root:view.root.join("ee"), run_records_root:view.root.join("runs"),
    };
    inspect(0, &RegistryCandidateStage { roots: &prospective, snapshot: &snapshot })?;
    for (index, write) in writes.iter().enumerate() {
        let path = view.root.join(roots.relative_target(&write.path)?);
        view.materialize_parent(&path)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                std::fs::remove_file(&path).map_err(|error| error.to_string())?
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        std::fs::write(&path, &write.content).map_err(|error| error.to_string())?;
        let relative = roots.relative_target(&write.path)?;
        if relative.starts_with("runs") {
            view.refresh_run_record(&path)?;
        }
        let (stage, snapshot) = view.inspect(index + 1, Some(&relative))?;
        stages.push(stage);
        inspect(index + 1, &RegistryCandidateStage { roots: &prospective, snapshot: &snapshot })?;
    }
    let baseline = stages[0].issues.iter().cloned().collect::<BTreeSet<_>>();
    let existing = stages[0].available.iter().cloned().collect::<BTreeSet<_>>();
    let mut regressions = BTreeSet::new();
    for stage in stages.iter().skip(1) {
        for issue in &stage.issues {
            if (!baseline.contains(issue)
                || (issue.subject == "snapshot" && stage.after_write == writes.len()))
                && (stage.after_write == writes.len()
                    || issue.subject.starts_with("run/")
                    || existing.contains(&issue.subject)
                    || issue.subject == "snapshot")
            {
                regressions.insert(issue.clone());
            }
        }
    }
    for write in writes {
        let relative = roots.relative_target(&write.path)?;
        if relative.starts_with("registry") || relative.starts_with("ee") {
            let parts = relative.components().collect::<Vec<_>>();
            let subject = if relative.starts_with("registry") {
                format!(
                    "{}/{}",
                    parts[1].as_os_str().to_string_lossy(),
                    parts[2].as_os_str().to_string_lossy()
                )
            } else {
                format!("ee/{}", relative.file_stem().unwrap_or_default().to_string_lossy())
            };
            let complete = stages.last().ok_or("candidate has no complete validation stage")?;
            if !complete.available.contains(&subject) {
                let issue = complete.issues.iter().find(|issue| issue.subject == subject)
                    .cloned().unwrap_or_else(|| RegistryValidationIssue {
                        subject,
                        checkpoint: None,
                        reason: "authored package or EE is unavailable in the complete candidate".into(),
                    });
                regressions.insert(issue);
            }
        }
    }
    let mut checked_subjects = BTreeSet::new();
    for write in writes {
        let relative = roots.relative_target(&write.path)?;
        let parts = relative.components().collect::<Vec<_>>();
        checked_subjects.insert(if relative.starts_with("registry") {
            format!(
                "{}/{}",
                parts[1].as_os_str().to_string_lossy(),
                parts[2].as_os_str().to_string_lossy()
            )
        } else {
            format!(
                "{}/{}",
                parts[0].as_os_str().to_string_lossy().trim_end_matches('s'),
                relative.file_stem().unwrap_or_default().to_string_lossy()
            )
        });
    }
    let changed_subjects = checked_subjects.clone();
    for (run, dependencies) in &view.dependencies {
        if !dependencies.is_disjoint(&changed_subjects) {
            checked_subjects.insert(run.clone());
            checked_subjects.extend(dependencies.iter().cloned());
        }
    }
    checked_subjects.extend(regressions.iter().map(|issue| issue.subject.clone()));
    if !writes.is_empty() {
        for stage in &mut stages {
            stage
                .available
                .retain(|subject| checked_subjects.contains(subject));
            stage.issues.retain(|issue| {
                issue.subject == "snapshot" || checked_subjects.contains(&issue.subject)
            });
        }
    }
    Ok(RegistryCandidateValidation {
        safe_to_apply: regressions.is_empty(),
        checked_subjects: checked_subjects.into_iter().collect(),
        stages,
        regressions: regressions.into_iter().collect(),
    })
}

struct CandidateView {
    root: PathBuf,
    roots: RegistryValidationRoots,
    records: BTreeMap<String, octopus_run_record::RunRecord>,
    record_issues: BTreeMap<String, RegistryValidationIssue>,
    run_issues: Vec<RegistryValidationIssue>,
    dependencies: BTreeMap<String, BTreeSet<String>>,
    snapshot_failed: bool,
}

impl Drop for CandidateView {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl CandidateView {
    fn new(roots: &RegistryValidationRoots) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = loop {
            let candidate = std::env::temp_dir().join(format!(
                "octopus-registry-candidate-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        };
        let mut view = Self {
            root,
            roots: roots.clone(),
            records: BTreeMap::new(),
            record_issues: BTreeMap::new(),
            run_issues: Vec::new(),
            dependencies: BTreeMap::new(),
            snapshot_failed: false,
        };
        normal_absolute_path(&roots.registry_root)?;
        normal_absolute_path(&roots.ee_instances_root)?;
        normal_absolute_path(&roots.run_records_root)?;
        std::fs::create_dir(view.root.join("registry")).map_err(|error| error.to_string())?;
        for kind in [
            "interface",
            "tool",
            "worker_environment_kind",
            "ee_template",
            "artifact",
            "mark",
            "verifier",
        ] {
            view.copy_directory(
                &roots.registry_root.join(kind),
                &view.root.join("registry").join(kind),
                if kind == "verifier" { usize::MAX } else { 1 },
                kind == "verifier",
            )?;
        }
        view.copy_directory(&roots.ee_instances_root, &view.root.join("ee"), 0, false)?;
        view.copy_directory(&roots.run_records_root, &view.root.join("runs"), 0, false)?;
        for entry in std::fs::read_dir(view.root.join("runs")).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            {
                view.refresh_run_record(&entry.path())?;
            }
        }
        Ok(view)
    }

    fn copy_directory(
        &self,
        source: &Path,
        target: &Path,
        depth: usize,
        copy_all_files: bool,
    ) -> Result<(), String> {
        std::fs::create_dir(target).map_err(|error| error.to_string())?;
        let entries = match std::fs::read_dir(source) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let target = target.join(entry.file_name());
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir() && depth > 0 {
                self.copy_directory(&path, &target, depth - 1, copy_all_files)?;
            } else if kind.is_file()
                && (copy_all_files
                    || path.extension().and_then(|value| value.to_str()) == Some("json"))
            {
                self.copy_observed_file(&path, &target)?;
            } else {
                retained_link(&path, &target)?;
            }
        }
        Ok(())
    }

    fn copy_observed_file(&self, source: &Path, target: &Path) -> Result<(), String> {
        use std::io::Read;
        let observed = std::fs::symlink_metadata(source).map_err(|error| error.to_string())?;
        if !observed.is_file() {
            return Err(format!("candidate input '{}' is not a regular file", source.display()));
        }
        let mut input = std::fs::File::open(source).map_err(|error| error.to_string())?;
        let opened = input.metadata().map_err(|error| error.to_string())?;
        if !opened.is_file() || opened.len() != observed.len()
            || opened.modified().map_err(|error| error.to_string())?
                != observed.modified().map_err(|error| error.to_string())?
        {
            return Err(format!("candidate input '{}' changed before copying", source.display()));
        }
        let limit = observed.len().checked_add(1).ok_or("candidate input length overflow")?;
        let mut output = std::fs::File::create(target).map_err(|error| error.to_string())?;
        let copied = std::io::copy(&mut (&mut input).take(limit), &mut output)
            .map_err(|error| error.to_string())?;
        let current = std::fs::symlink_metadata(source).map_err(|error| error.to_string())?;
        if copied != observed.len() || !current.is_file() || current.len() != observed.len()
            || current.modified().map_err(|error| error.to_string())?
                != observed.modified().map_err(|error| error.to_string())?
        {
            return Err(format!("candidate input '{}' changed while copying", source.display()));
        }
        output.set_permissions(observed.permissions()).map_err(|error| error.to_string())
    }

    fn materialize_parent(&self, path: &Path) -> Result<(), String> {
        let parent = path.parent().ok_or("candidate target has no parent")?;
        let relative = parent
            .strip_prefix(&self.root)
            .map_err(|error| error.to_string())?;
        let mut current = self.root.clone();
        for part in relative.components() {
            current.push(part);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let source = std::fs::read_link(&current).map_err(|error| error.to_string())?;
                    std::fs::remove_file(&current).map_err(|error| error.to_string())?;
                    self.copy_directory(&source, &current, 0, false)?;
                }
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err("candidate parent is not a directory".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&current).map_err(|error| error.to_string())?
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    fn refresh_run_record(&mut self, path: &Path) -> Result<(), String> {
        let id = path
            .file_stem()
            .ok_or("candidate run path has no id")?
            .to_string_lossy()
            .to_string();
        let subject = format!("run/{id}");
        self.records.remove(&subject);
        self.record_issues.remove(&subject);
        let result = std::fs::read(path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                octopus_run_record::RunRecord::from_bytes(&bytes)
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(record) if record.id.as_str() == id => {
                self.records.insert(subject, record);
            }
            Ok(_) => {
                self.record_issues.insert(
                    subject.clone(),
                    RegistryValidationIssue {
                        subject,
                        checkpoint: None,
                        reason: "run identity differs from its filename".into(),
                    },
                );
            }
            Err(reason) => {
                self.record_issues
                    .insert(subject.clone(), RegistryValidationIssue { subject, checkpoint: None, reason });
            }
        }
        Ok(())
    }

    fn inspect(
        &mut self,
        after_write: usize,
        changed: Option<&Path>,
    ) -> Result<(RegistryValidationStage, Result<crate::spine_root::LoadedRegistrySnapshot, String>), String> {
        let registry = self.root.join("registry");
        let packages = RegistryPackages::load(&registry)?;
        let mut issues = packages
            .graph
            .unavailable
            .iter()
            .map(|(node, reason)| RegistryValidationIssue {
                subject: node.to_string(),
                checkpoint: None,
                reason: self.original_paths(reason),
            })
            .collect::<Vec<_>>();
        let available_packages = packages
            .graph
            .available
            .iter()
            .filter(|node| packages.graph.is_available(node))
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let changed_subject = changed.map(|path| {
            let parts = path.components().collect::<Vec<_>>();
            if parts[0].as_os_str() == "registry" {
                format!(
                    "{}/{}",
                    parts[1].as_os_str().to_string_lossy(),
                    parts[2].as_os_str().to_string_lossy()
                )
            } else {
                format!(
                    "{}/{}",
                    parts[0].as_os_str().to_string_lossy().trim_end_matches('s'),
                    path.file_stem().unwrap_or_default().to_string_lossy()
                )
            }
        });
        let records = self
            .records
            .iter()
            .filter(|(subject, _)| {
                changed_subject.as_ref().is_none_or(|changed| {
                    self.snapshot_failed
                        || self
                            .run_issues
                            .iter()
                            .any(|issue| &issue.subject == *subject)
                        || self
                            .dependencies
                            .get(*subject)
                            .is_none_or(|inputs| inputs.contains(changed))
                })
            })
            .map(|(_, record)| record.clone())
            .collect::<Vec<_>>();
        let affected = records
            .iter()
            .map(|record| format!("run/{}", record.id))
            .collect::<BTreeSet<_>>();
        let mut available_ee = BTreeSet::new();
        let ee_root = self.root.join("ee");
        let snapshot = crate::spine_root::LoadedRegistrySnapshot::load_from_packages(packages, &ee_root);
        match snapshot.as_ref().map_err(Clone::clone).and_then(|snapshot| {
            crate::spine_root::validate_registry_run_contracts(snapshot, &registry, &ee_root, &records)
        }) {
            Ok((faults, dependencies, ee)) => {
                self.snapshot_failed = false;
                available_ee = ee;
                self.run_issues
                    .retain(|issue| issue.subject.starts_with("run/") && !affected.contains(&issue.subject));
                let faults = faults
                    .into_iter()
                    .map(|mut fault| {
                        fault.reason = self.original_paths(&fault.reason);
                        fault
                    })
                    .collect::<Vec<_>>();
                self.run_issues.extend(faults);
                self.dependencies.extend(dependencies);
            }
            Err(reason) => {
                self.snapshot_failed = true;
                issues.push(RegistryValidationIssue {
                    subject: "snapshot".into(),
                    checkpoint: None,
                    reason: self.original_paths(&reason),
                });
            }
        }
        issues.extend(self.record_issues.values().cloned());
        issues.extend(self.run_issues.iter().cloned());
        issues.sort();
        Ok((
            RegistryValidationStage {
                after_write,
                issues,
                available: available_packages.into_iter().chain(available_ee).collect(),
            },
            snapshot,
        ))
    }

    fn original_paths(&self, value: &str) -> String {
        value
            .replace(
                &self.root.join("registry").to_string_lossy().to_string(),
                &self.roots.registry_root.to_string_lossy(),
            )
            .replace(
                &self.root.join("ee").to_string_lossy().to_string(),
                &self.roots.ee_instances_root.to_string_lossy(),
            )
            .replace(
                &self.root.join("runs").to_string_lossy().to_string(),
                &self.roots.run_records_root.to_string_lossy(),
            )
    }
}

#[cfg(unix)]
fn retained_link(source: &Path, target: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(source, target).map_err(|error| error.to_string())
}

#[cfg(windows)]
fn retained_link(source: &Path, target: &Path) -> Result<(), String> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(source, target)
    } else {
        std::os::windows::fs::symlink_file(source, target)
    }
    .map_err(|error| error.to_string())
}

fn load_kind<T: Descriptor>(
    root: &Path,
    kind: RegistryNodeKind,
    optional_root: bool,
) -> Result<KindLoad<T>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if optional_root && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(KindLoad {
                loaded: Vec::new(),
                unavailable: BTreeMap::new(),
            });
        }
        Err(error) => {
            return Err(format!(
                "registry kind root '{}' cannot be read: {error}",
                root.display()
            ));
        }
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "registry kind root '{}' entry cannot be read: {error}",
                root.display()
            )
        })?;
        if entry.file_name() == octopus_file_storage::publication::COORDINATION_DIRECTORY {
            continue;
        }
        if entry
            .file_type()
            .map_err(|error| {
                format!(
                    "registry entry '{}' type failed: {error}",
                    entry.path().display()
                )
            })?
            .is_dir()
        {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    let mut loaded = Vec::new();
    let mut unavailable = BTreeMap::new();
    for dir in dirs {
        let Some(folder) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let node = RegistryNode::new(kind, folder);
        let path = dir.join(format!("{}.json", T::KIND));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                unavailable.insert(
                    node,
                    format!("descriptor '{}' cannot be read: {error}", path.display()),
                );
                continue;
            }
        };
        let descriptor = match octopus_descriptors::parse_strict::<T>(&bytes) {
            Ok(descriptor) => descriptor,
            Err(error) => {
                unavailable.insert(
                    node,
                    format!("descriptor '{}' is invalid: {error}", path.display()),
                );
                continue;
            }
        };
        if let Some(declared) = descriptor.declared_name() {
            if declared != folder {
                unavailable.insert(
                    node,
                    format!(
                        "descriptor '{}' declares '{}' instead of folder id '{}'",
                        path.display(),
                        declared,
                        folder
                    ),
                );
                continue;
            }
        }
        loaded.push((
            DescriptorName::new(format!("{}/{folder}", T::KIND)),
            descriptor,
        ));
    }
    Ok(KindLoad {
        loaded,
        unavailable,
    })
}

fn load_artifacts(registry_root: &Path, graph: &mut RegistryReferenceGraph) -> Result<(), String> {
    let root = registry_root.join("artifact");
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "artifact registry '{}' cannot be read: {error}",
                root.display()
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("artifact registry entry failed: {error}"))?;
        if entry.file_name() == octopus_file_storage::publication::COORDINATION_DIRECTORY {
            continue;
        }
        if !entry
            .file_type()
            .map_err(|error| {
                format!(
                    "artifact entry '{}' type failed: {error}",
                    entry.path().display()
                )
            })?
            .is_dir()
        {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let node = RegistryNode::artifact(&id);
        let path = entry.path().join("artifact.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                graph.refuse(
                    node,
                    format!(
                        "artifact descriptor '{}' cannot be read: {error}",
                        path.display()
                    ),
                );
                continue;
            }
        };
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(error) => {
                graph.refuse(
                    node,
                    format!(
                        "artifact descriptor '{}' is malformed: {error}",
                        path.display()
                    ),
                );
                continue;
            }
        };
        if value.get("id").and_then(serde_json::Value::as_str) != Some(id.as_str()) {
            graph.refuse(
                node,
                format!(
                    "artifact descriptor '{}' does not declare folder id '{}'",
                    path.display(),
                    id
                ),
            );
            continue;
        }
        let bytes = octopus_descriptors::canonical_bytes(&value);
        let hash = DescriptorHash::of_bytes(&bytes);
        graph.artifact_descriptor_bytes.insert(ArtifactId::new(&id), bytes);
        graph.register(node);
        graph
            .artifact_descriptor_hashes
            .insert(ArtifactId::new(id), hash);
    }
    Ok(())
}

fn link_tools(tools: &[(DescriptorName, ToolDescriptor)], graph: &mut RegistryReferenceGraph) {
    for (_, tool) in tools {
        let source = RegistryNode::tool(tool.id.as_str());
        graph.link(
            source.clone(),
            "interface",
            RegistryNode::interface(tool.interface.as_str()),
        );
        for (index, kind) in tool.supported_worker_environment_kinds.iter().enumerate() {
            graph.link(
                source.clone(),
                format!("supported_worker_environment_kinds[{index}]"),
                RegistryNode::wek(kind.as_str()),
            );
        }
    }
}

/// Edge every worker pin to each member of its ceiling — interfaces, tools,
/// worker-environment kinds, mark outcomes, role artifacts — so the active
/// reference closure walks a pin's whole granted surface and validates it
/// transitively, and run the dangling detector over those edges. The two kinds of
/// absence are NOT the same fact:
/// - a ceiling INTERFACE reference absent from the live registry is a typed
///   dangling FACT, never boot poison — the pin still forms and emission toward
///   the absent interface refuses at send time through the delivery protocol
///   (owner decision, notes2 2026-07-06); it stays a warning, no closure edge;
/// - a ceiling tool / wek / mark outcome / role artifact absent from the live
///   registry is an unsatisfiable grant — fail-closed: the pin is refused and
///   forms no grant (a worker referencing a mark id absent from `registry/mark`
///   fail-closes at boot, notes2 2026-07-05; a ceiling tool must resolve to a
///   tool descriptor, WORKER_TYPES.md §Tools).
///
/// Pass 1 applies the fail-closed refusals first so a pin refused for its own
/// unsatisfiable member reads as unavailable in pass 2 — a sibling granting it as
/// a ceiling interface then folds to a warning, never a fail-closed closure
/// dependency.
fn link_worker_ceilings(
    policies: &BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    graph: &mut RegistryReferenceGraph,
) -> Vec<DanglingCeilingRef> {
    for (worker, policy) in policies {
        let source = RegistryNode::interface(worker.as_str());
        if !graph.is_available(&source) {
            continue;
        }
        let unsatisfiable = policy
            .ceiling
            .tools
            .iter()
            .map(|tool| ("ceiling.tools", RegistryNode::tool(tool.as_str())))
            .chain(
                policy
                    .ceiling
                    .wek
                    .iter()
                    .map(|wek| ("ceiling.wek", RegistryNode::wek(wek.as_str()))),
            )
            .chain(
                policy
                    .ceiling
                    .mark_outcomes
                    .iter()
                    .map(|mark| ("ceiling.mark_outcomes", RegistryNode::mark(mark.as_str()))),
            )
            .chain(
                policy
                    .role_artifacts
                    .iter()
                    .map(|artifact| ("role_artifacts", RegistryNode::artifact(artifact.as_str()))),
            )
            .find(|(_, target)| !graph.is_available(target));
        if let Some((field, target)) = unsatisfiable {
            graph.refuse(
                source,
                format!(
                    "worker ceiling {field} grants '{target}', absent from the live registry — an unsatisfiable grant, fail-closed"
                ),
            );
        }
    }

    let mut dangling = Vec::new();
    for (worker, policy) in policies {
        let source = RegistryNode::interface(worker.as_str());
        if !graph.is_available(&source) {
            continue;
        }
        link_ceiling_members(
            &source,
            worker,
            "ceiling.interfaces",
            policy
                .ceiling
                .interfaces
                .iter()
                .map(|interface| RegistryNode::interface(interface.as_str())),
            true,
            graph,
            &mut dangling,
        );
        link_ceiling_members(
            &source,
            worker,
            "ceiling.tools",
            policy
                .ceiling
                .tools
                .iter()
                .map(|tool| RegistryNode::tool(tool.as_str())),
            false,
            graph,
            &mut dangling,
        );
        link_ceiling_members(
            &source,
            worker,
            "ceiling.wek",
            policy
                .ceiling
                .wek
                .iter()
                .map(|wek| RegistryNode::wek(wek.as_str())),
            false,
            graph,
            &mut dangling,
        );
        link_ceiling_members(
            &source,
            worker,
            "ceiling.mark_outcomes",
            policy
                .ceiling
                .mark_outcomes
                .iter()
                .map(|mark| RegistryNode::mark(mark.as_str())),
            false,
            graph,
            &mut dangling,
        );
        link_ceiling_members(
            &source,
            worker,
            "role_artifacts",
            policy
                .role_artifacts
                .iter()
                .map(|artifact| RegistryNode::artifact(artifact.as_str())),
            false,
            graph,
            &mut dangling,
        );
    }
    dangling
}

/// Draw a closure edge from `source` to every available ceiling member. An absent
/// member is a warning only for `warn_when_absent` fields (ceiling interfaces —
/// the send-time-validated reach axis); on any other field the pin was already
/// refused in pass 1 (fail-closed), so a still-available pin never reaches the
/// skip branch there.
fn link_ceiling_members(
    source: &RegistryNode,
    worker: &InterfaceId,
    field: &str,
    targets: impl Iterator<Item = RegistryNode>,
    warn_when_absent: bool,
    graph: &mut RegistryReferenceGraph,
    dangling: &mut Vec<DanglingCeilingRef>,
) {
    for target in targets {
        if graph.is_available(&target) {
            graph.link(source.clone(), field, target);
        } else if warn_when_absent {
            dangling.push(DanglingCeilingRef {
                worker: worker.as_str().to_string(),
                field: field.to_string(),
                target,
            });
        }
    }
}

fn refuse_cross_package_mismatches(
    policies: &BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    tools: &[(DescriptorName, ToolDescriptor)],
    kinds: &[(DescriptorName, WorkerEnvKindSpec)],
    interfaces: &[(DescriptorName, InterfaceDescriptor)],
    interface_dispositions: &BTreeMap<InterfaceId, InterfaceRuntimeDisposition>,
    graph: &mut RegistryReferenceGraph,
) {
    let worker_views = policies
        .iter()
        .map(|(worker, policy)| WorkerEnvironmentCeilingView {
            worker,
            wek: policy.ceiling.wek.iter().map(|wek| wek.as_str()).collect(),
            tools: policy
                .ceiling
                .tools
                .iter()
                .map(|tool| tool.as_str())
                .collect(),
        })
        .collect::<Vec<_>>();
    let tool_views = tools
        .iter()
        .map(|(_, tool)| WorkerEnvironmentToolView {
            tool: tool.id.as_str(),
            interface: &tool.interface,
            process_bin: &tool.process.bin,
            worker_environment_kinds: tool
                .supported_worker_environment_kinds
                .iter()
                .map(|kind| kind.as_str())
                .collect(),
        })
        .collect::<Vec<_>>();
    let mut registry_issues = BTreeMap::<RegistryNode, BTreeSet<String>>::new();
    for issue in worker_environment_registry_issues(
        &worker_views,
        kinds,
        &tool_views,
        interfaces,
        interface_dispositions,
    ) {
        let node = match issue.subject {
            WorkerEnvironmentRegistrySubject::Worker(worker) => {
                RegistryNode::interface(worker.as_str())
            }
            WorkerEnvironmentRegistrySubject::Tool(tool) => RegistryNode::tool(tool),
            WorkerEnvironmentRegistrySubject::Kind(kind) => RegistryNode::wek(kind),
            WorkerEnvironmentRegistrySubject::Interface(interface) => {
                RegistryNode::interface(interface.as_str())
            }
        };
        registry_issues
            .entry(node)
            .or_default()
            .insert(issue.reason);
    }
    for (node, reasons) in registry_issues {
        graph.refuse(node, reasons.into_iter().collect::<Vec<_>>().join("; "));
    }
}

fn load_activation_references(
    registry_root: &Path,
    interfaces: &[(DescriptorName, InterfaceDescriptor)],
    graph: &mut RegistryReferenceGraph,
) -> Result<Vec<ActivationReferenceSet>, String> {
    let mut activations = Vec::new();
    for (_, interface) in interfaces {
        if !graph.is_available(&RegistryNode::interface(interface.id.as_str())) {
            continue;
        }
        let interface_dir = registry_root.join("interface").join(interface.id.as_str());
        let path = interface_dir.join("activation.json");
        if !path.exists() {
            continue;
        }
        let loaded = match octopus_interface_projection::load_interface_activation_file_in_dir(
            &interface_dir,
            &interface.id,
        ) {
            Ok(loaded) => loaded,
            Err(error) => {
                graph.refuse(
                    RegistryNode::interface(interface.id.as_str()),
                    format!(
                        "activation sidecar '{}' is invalid: {error}",
                        path.display()
                    ),
                );
                continue;
            }
        };
        for (index, activation) in loaded.descriptor.activations.iter().enumerate() {
            if !activation.enabled {
                continue;
            }
            let source = format!("activation/{}#{index}", interface.id.as_str());
            let references = vec![(
                "interface".to_string(),
                RegistryNode::interface(interface.id.as_str()),
            )];
            activations.push(ActivationReferenceSet { source, references });
        }
    }
    Ok(activations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_preserves_first_breadth_first_refusal_through_diamonds_and_cycles() {
        let mut graph = RegistryReferenceGraph::default();
        let nodes = ["root", "left", "right", "shared"].map(RegistryNode::tool);
        for node in &nodes {
            graph.register(node.clone());
        }
        for (from, field, to) in [
            (0, "left", 1),
            (0, "right", 2),
            (1, "shared", 3),
            (2, "shared", 3),
            (3, "cycle", 0),
        ] {
            graph.link(nodes[from].clone(), field, nodes[to].clone());
        }
        let roots = [
            ("first".into(), nodes[0].clone()),
            ("second".into(), nodes[2].clone()),
        ];
        assert_eq!(
            graph
                .validate_active_references("run", roots.clone())
                .unwrap(),
            nodes.into_iter().collect()
        );
        graph.link(
            RegistryNode::tool("shared"),
            "broken",
            RegistryNode::mark("absent"),
        );
        assert_eq!(
            graph.validate_active_references("run", roots).unwrap_err(),
            "active registry reference closure reached missing mark/absent; path: run.first -> tool/root -> tool/root.left -> tool/left -> tool/left.shared -> tool/shared -> tool/shared.broken -> mark/absent"
        );
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().expect("scratch path has a parent"))
            .expect("scratch dir writes");
        std::fs::write(path, body).expect("scratch file writes");
    }

    #[test]
    fn registry_inventory_excludes_publication_metadata_but_reports_missing_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        let roots = RegistryValidationRoots {
            registry_root: temp.path().join("registry"),
            ee_instances_root: temp.path().join("ee"),
            run_records_root: temp.path().join("runs"),
        };
        let kinds = ["interface", "tool", "worker_environment_kind", "ee_template", "mark", "verifier", "artifact"];
        for kind in kinds {
            let root = roots.registry_root.join(kind);
            std::fs::create_dir_all(root.join(".octopus-publication-other")).unwrap();
        }
        let baseline = validate_registry_candidate(&roots, &[]).unwrap().stages.remove(0);
        let expected = kinds.map(|kind| format!("{kind}/.octopus-publication-other"))
            .into_iter().collect::<BTreeSet<_>>();
        for kind in kinds {
            std::fs::create_dir(roots.registry_root.join(kind)
                .join(octopus_file_storage::publication::COORDINATION_DIRECTORY)).unwrap();
        }
        let packages = RegistryPackages::load(&roots.registry_root).unwrap();
        assert_eq!(packages.graph.unavailable.keys().map(ToString::to_string).collect::<BTreeSet<_>>(), expected);
        let installed = validate_registry_candidate(&roots, &[]).unwrap().stages.remove(0);
        assert_eq!(installed.issues, baseline.issues);
        assert_eq!(installed.available, baseline.available);
    }

    #[test]
    fn candidate_checks_publication_prefixes_without_exposing_or_changing_other_runs() {
        use serde_json::json;
        let temp = tempfile::tempdir().unwrap();
        let roots = RegistryValidationRoots {
            registry_root: temp.path().join("registry"),
            ee_instances_root: temp.path().join("ee"),
            run_records_root: temp.path().join("runs"),
        };
        for kind in [
            "interface",
            "tool",
            "worker_environment_kind",
            "ee_template",
        ] {
            std::fs::create_dir_all(roots.registry_root.join(kind)).unwrap();
        }
        write(
            &roots
                .registry_root
                .join("interface/world_present/interface.json"),
            r#"{"id":"world_present","interface_type":"world","role":"probe","direction":"bidirectional","source_names":["p"],"interface_driver_ref":"probe"}"#,
        );
        worker_pin(
            &roots.registry_root,
            "system_worker_present",
            r#"{"interfaces":["world_present"],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]}"#,
        );
        write(
            &roots
                .registry_root
                .join("ee_template/existence/ee_template.json"),
            r#"{"id":"existence","goal_class":"existence","bootstrap_root":true,"allowed_child_ee_templates":["existence"]}"#,
        );
        write(
            &roots.ee_instances_root.join("ee_existence.json"),
            r#"{"ee_id":"ee_existence","template_name":"existence","parent_ee_id":null,"goal":"existence"}"#,
        );
        let run = |id: &str, worker: &str| {
            json!({"id":id,"worker":worker,"ee":"ee_existence","trace":"trace","status":"idle",
            "interface_state":{"world_present":{"private":"unrelated-private-payload"}},
            "checkpoints":[{"id":"wait","run":id,"trace":"trace","epoch":0,"interface_binding":octopus_runtime::AuthoredInterfaceBinding::all("world_present".into())}],"active_checkpoints":["wait"]})
        };
        let original = serde_json::to_string(&run("existing", "system_worker_present")).unwrap();
        write(&roots.run_records_root.join("existing.json"), &original);
        let new_interface = RegistryCandidateWrite {path:roots.registry_root.join("interface/system_worker_new/interface.json"),
            content:r#"{"id":"system_worker_new","interface_type":"system","role":"worker","direction":"bidirectional","source_names":["new"]}"#.into(),expected_before_hash:None};
        let new_policy = RegistryCandidateWrite {path:roots.registry_root.join("interface/system_worker_new/policy.json"),
            content:r#"{"id":"system_worker_new","ceiling":{"interfaces":["world_present"],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]},"role_artifacts":[]}"#.into(),expected_before_hash:None};
        let new_run = RegistryCandidateWrite {
            path: roots.run_records_root.join("new.json"),
            content: serde_json::to_string(&run("new", "system_worker_new")).unwrap(),
            expected_before_hash: None,
        };
        let report = validate_registry_candidate(
            &roots,
            &[new_interface.clone(), new_policy.clone(), new_run.clone()],
        )
        .unwrap();
        assert!(report.safe_to_apply, "{:?}", report.regressions);
        assert!(
            report.stages[1]
                .issues
                .iter()
                .any(|issue| issue.subject == "interface/system_worker_new")
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("unrelated-private-payload")
        );
        assert_eq!(
            std::fs::read_to_string(roots.run_records_root.join("existing.json")).unwrap(),
            original
        );
        assert!(!new_run.path.exists());
        let report =
            validate_registry_candidate(&roots, &[new_interface, new_run, new_policy]).unwrap();
        assert!(!report.safe_to_apply);
        assert!(
            report
                .regressions
                .iter()
                .any(|issue| issue.subject == "run/new")
        );
        let path = roots
            .registry_root
            .join("interface/system_worker_present/policy.json");
        let before = std::fs::read(&path).unwrap();
        let mut policy: serde_json::Value = serde_json::from_slice(&before).unwrap();
        policy["ceiling"]["mark_outcomes"] = json!(["future"]);
        let change = RegistryCandidateWrite {
            path,
            content: serde_json::to_string(&policy).unwrap(),
            expected_before_hash: Some(octopus_ledger::ContentHash::of_bytes(&before).to_hex()),
        };
        let mark = RegistryCandidateWrite {
            path: roots.registry_root.join("mark/future/mark.json"),
            content: r#"{"id":"future","package":"future","class":"success"}"#.into(),
            expected_before_hash: None,
        };
        assert!(
            !validate_registry_candidate(&roots, &[change.clone(), mark.clone()])
                .unwrap()
                .safe_to_apply
        );
        assert!(
            validate_registry_candidate(&roots, &[mark, change])
                .unwrap()
                .safe_to_apply
        );
        let malformed_ee = RegistryCandidateWrite {
            path: roots.ee_instances_root.join("ee_authored.json"),
            content: "{\"unexpected\":true}".into(),
            expected_before_hash: None,
        };
        let report = validate_registry_candidate(&roots, &[malformed_ee.clone()]).unwrap();
        assert!(!report.safe_to_apply);
        assert!(report.regressions.iter().any(|issue| issue.subject == "ee/ee_authored"));
        write(&malformed_ee.path, &malformed_ee.content);
        let repair = RegistryCandidateWrite {
            path: malformed_ee.path.clone(),
            content: r#"{"ee_id":"ee_authored","template_name":"existence","parent_ee_id":"ee_existence","goal":"authored goal"}"#.into(),
            expected_before_hash: Some(octopus_ledger::ContentHash::of_bytes(malformed_ee.content.as_bytes()).to_hex()),
        };
        let report = validate_registry_candidate(&roots, &[repair]).unwrap();
        assert!(report.safe_to_apply, "{:?}", report.regressions);
        assert_eq!(std::fs::read_to_string(&malformed_ee.path).unwrap(), malformed_ee.content);
        std::fs::remove_file(&malformed_ee.path).unwrap();
        let scale_context = "x".repeat(71_000);
        for index in 0..999 {
            let id = format!("ee_scale_{index}");
            write(&roots.ee_instances_root.join(format!("{id}.json")), &serde_json::to_string(&json!({
                "ee_id":id,"template_name":"existence","parent_ee_id":"ee_existence","goal":"scale contract",
                "config":{"context":scale_context},
            })).unwrap());
        }
        let report = validate_registry_candidate(&roots, &[]).unwrap();
        assert!(report.stages[0].issues.is_empty(), "{:?}", report.stages[0].issues);
        assert_eq!(report.stages[0].available.iter().filter(|subject| subject.starts_with("ee/")).count(), 1000);
        let scale_write = RegistryCandidateWrite {
            path: roots.registry_root.join("mark/scale/mark.json"),
            content: r#"{"id":"scale","package":"scale","class":"success"}"#.into(),
            expected_before_hash: None,
        };
        let report = validate_registry_candidate(&roots, &[scale_write]).unwrap();
        assert!(report.safe_to_apply, "{:?}", report.regressions);
    }

    fn worker_pin(root: &Path, id: &str, ceiling_json: &str) {
        let dir = root.join("interface").join(id);
        write(
            &dir.join("interface.json"),
            &format!(
                r#"{{"id":"{id}","interface_type":"system","role":"worker","direction":"bidirectional","source_names":["{id}"]}}"#
            ),
        );
        write(
            &dir.join("policy.json"),
            &format!(r#"{{"id":"{id}","ceiling":{ceiling_json},"role_artifacts":[]}}"#),
        );
    }

    /// The live `RegistryPackages::load` path proves the ceiling reference closure:
    /// an absent ceiling mark / tool refuses the pin (fail-closed, WORKER_TYPES.md +
    /// notes2 2026-07-05), an absent ceiling interface stays a typed dangling
    /// warning (owner, notes2 2026-07-06), and every present ceiling member is
    /// edged so the active closure walks the pin's whole granted surface.
    #[test]
    fn worker_ceiling_closure_fails_closed_on_marks_and_tools_and_warns_on_interfaces() {
        let root = std::env::temp_dir().join(format!(
            "octopus-ceiling-closure-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for kind in [
            "interface",
            "tool",
            "worker_environment_kind",
            "ee_template",
        ] {
            std::fs::create_dir_all(root.join(kind)).expect("registry kind root writes");
        }
        write(
            &root
                .join("interface")
                .join("world_present")
                .join("interface.json"),
            r#"{"id":"world_present","interface_type":"world","role":"probe","direction":"outbound","source_names":["p"],"interface_driver_ref":"probe_reader"}"#,
        );
        worker_pin(
            &root,
            "system_worker_missing_mark",
            r#"{"interfaces":[],"tools":[],"wek":[],"mark_outcomes":["ghost_mark"],"hard_blocked_tools":[]}"#,
        );
        worker_pin(
            &root,
            "system_worker_missing_tool",
            r#"{"interfaces":[],"tools":["ghost_tool"],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]}"#,
        );
        worker_pin(
            &root,
            "system_worker_missing_iface",
            r#"{"interfaces":["ghost_iface"],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]}"#,
        );
        worker_pin(
            &root,
            "system_worker_reaches",
            r#"{"interfaces":["world_present"],"tools":[],"wek":[],"mark_outcomes":[],"hard_blocked_tools":[]}"#,
        );

        let packages = RegistryPackages::load(&root).expect("the crafted registry loads");

        let missing_mark = InterfaceId::new("system_worker_missing_mark");
        let missing_tool = InterfaceId::new("system_worker_missing_tool");
        let missing_iface = InterfaceId::new("system_worker_missing_iface");
        let reaches = InterfaceId::new("system_worker_reaches");

        // Fail-closed: an absent ceiling mark / tool refuses the pin, dropping it
        // from the loaded worker policies — it forms no grant.
        for refused in [&missing_mark, &missing_tool] {
            assert!(
                !packages
                    .graph
                    .is_available(&RegistryNode::interface(refused.as_str())),
                "{} must be refused for its unsatisfiable ceiling member",
                refused.as_str()
            );
            assert!(!packages.worker_policies.contains_key(refused));
        }
        // The fail-closed kinds are never demoted to boot warnings.
        assert!(
            !packages
                .dangling_ceiling_refs
                .iter()
                .any(|d| d.field == "ceiling.mark_outcomes" || d.field == "ceiling.tools"),
            "absent marks/tools fail closed, they do not masquerade as warnings: {:?}",
            packages.dangling_ceiling_refs
        );

        // Warning: an absent ceiling interface keeps the pin available and surfaces
        // a typed dangling fact instead of poisoning the boot.
        assert!(
            packages
                .graph
                .is_available(&RegistryNode::interface(missing_iface.as_str()))
        );
        assert!(packages.worker_policies.contains_key(&missing_iface));
        assert!(
            packages
                .dangling_ceiling_refs
                .iter()
                .any(|d| d.worker == "system_worker_missing_iface"
                    && d.field == "ceiling.interfaces"
                    && d.target == RegistryNode::interface("ghost_iface")),
            "the absent ceiling interface is a dangling warning: {:?}",
            packages.dangling_ceiling_refs
        );

        // Closure completeness: the reaching pin edges to its present ceiling
        // interface and the active reference closure walks that edge — the reach the
        // pre-fix graph (no ceiling.interfaces edges) could never validate.
        let closure = packages
            .graph
            .validate_active_references(
                "test",
                [(
                    "root".to_string(),
                    RegistryNode::interface(reaches.as_str()),
                )],
            )
            .expect("the reaching pin's closure is satisfiable");
        assert!(
            closure.contains(&RegistryNode::interface("world_present")),
            "the active closure reaches the granted ceiling interface: {closure:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
