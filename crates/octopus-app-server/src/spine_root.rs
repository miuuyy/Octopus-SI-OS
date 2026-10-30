use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use octopus_descriptors::{
    ArtifactId, Descriptor, DescriptorHash, DescriptorName, EeId, InterfaceId, MarkId, RunId,
    ToolId, VerifierId, WorkerEnvironmentKindId, parse_strict,
};
use octopus_ee::{EeInstance, EeTemplateDescriptor, EeTree, bootstrap_ee_instance_id};
use octopus_interfaces::{InterfaceDescriptor, InterfaceRuntimeDisposition};
use octopus_ledger::{BlobStore, ContentHash, VerifierPackagePin};
use octopus_marks::{
    Mark, MarkClass, MarkDescriptor, PreparedVerification, RequiredVerifiedArtifact,
    VerificationInput, VerifiedArtifactAdmission, VerifiedArtifactError,
};
use octopus_policy::{
    ArtifactVerb, CapSet, ContextGrant, CopyLineage, DenySet, FormationPin, FormedPolicy,
    InterfacePolicyGrant, InterfacePolicyValues, PolicyLayer, WorkerInterfacePolicy,
    form_policy_with_lineage,
};
use octopus_runtime::{
    InterfaceClass, RunGrantQuery, RunGrantResolution, RunGrantResolver, Runtime,
    RuntimeInterfaceCatalog, RuntimeServices,
};
use octopus_run_record::GrantEpoch;
use octopus_tool_registry::ToolDescriptor;
use octopus_verifiers::{
    VerifierDescriptor, VerifierPackageContext, pin_verifier_package, run_pinned_verifier_process,
};
use octopus_worker_env::WorkerEnvKindSpec;

use octopus_interface_projection::{
    InterfaceToolAffordance, InterfaceToolDescriptionView, InterfaceToolOwnerMap,
    attached_bindings_from_grant, interface_map, interface_metadata_projection_body,
    project_interface_tool_affordances,
};
pub use octopus_ledger::SharedBlobs;

use crate::context_artifact_cache::RegistryContextArtifactCache;
use crate::registry_reference_graph::{
    ActivationReferenceSet, RegistryNode, RegistryPackages, RegistryReferenceGraph,
};

#[path = "spine_root/commit.rs"]
mod commit;

#[derive(Clone, Debug)]
pub(crate) struct LoadedRegistrySnapshot {
    interfaces_loaded: Vec<(DescriptorName, InterfaceDescriptor)>,
    interfaces_by_id: BTreeMap<InterfaceId, InterfaceDescriptor>,
    interface_dispositions: BTreeMap<InterfaceId, InterfaceRuntimeDisposition>,
    worker_interface_policies: BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    tool_descriptions: RegistryToolDescriptions,
    tool_ownership: InterfaceToolOwnerMap,
    tool_supported_weks: BTreeMap<ToolId, BTreeSet<WorkerEnvironmentKindId>>,
    worker_environment_kinds: BTreeSet<WorkerEnvironmentKindId>,
    reference_graph: RegistryReferenceGraph,
    ee_tree: EeTree,
    copy_lineage: CopyLineage,
    formation_source_hashes: BTreeMap<DescriptorName, DescriptorHash>,
    formation_source_bytes: BTreeMap<DescriptorName, Vec<u8>>,
    ee_instance_issues: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EePolicyProjection {
    pub allowed_interfaces: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub allowed_worker_environment_kinds: Vec<String>,
    pub allowed_root_slots: Option<Vec<String>>,
    pub allowed_artifact_verbs: Vec<String>,
    pub hard_blocked_tools: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EePolicyProjectionRow {
    pub ee_id: String,
    pub effective_policy: EePolicyProjection,
    pub policy_ceiling: EePolicyProjection,
    pub workers: Vec<EeWorkerInterfacePolicyProjection>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EeWorkerInterfacePolicyProjection {
    pub worker_interface_id: String,
    pub interface_policy: InterfacePolicyGrant,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct EePolicyProjectionSnapshot {
    pub environments: Vec<EePolicyProjectionRow>,
    pub issues: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegistryDescriptorFingerprint {
    file_count: u64,
    digest: u128,
}

#[derive(Default)]
struct RegistryFileCache(BTreeMap<PathBuf, RegistryFileObservation>);

struct RegistryFileObservation {
    stamp: octopus_file_storage::metadata::FileStamp,
    settled: bool,
    digest: u64,
    observed: bool,
}

const REGISTRY_OBSERVATION_ATTEMPTS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum RegistrySnapshotError {
    #[error("{0}")]
    Invalid(String),
    #[error("registry descriptors changed while forming the live snapshot; no stable observation after {attempts} attempts")]
    Unstable { attempts: usize },
}

impl From<String> for RegistrySnapshotError {
    fn from(error: String) -> Self { Self::Invalid(error) }
}

fn observe_stable_registry<T>(
    current: Option<RegistryDescriptorFingerprint>,
    mut fingerprint: impl FnMut() -> std::io::Result<RegistryDescriptorFingerprint>,
    mut load: impl FnMut() -> Result<T, String>,
) -> Result<Option<(RegistryDescriptorFingerprint, T)>, RegistrySnapshotError> {
    for _ in 0..REGISTRY_OBSERVATION_ATTEMPTS {
        let before = match fingerprint() {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("registry descriptor fingerprint before load failed: {error}").into()),
        };
        let candidate = if current == Some(before) {
            Ok(None)
        } else {
            load().map(Some)
        };
        let after = match fingerprint() {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("registry descriptor fingerprint after load failed: {error}").into()),
        };
        if before != after {
            continue;
        }
        return candidate.map(|snapshot| snapshot.map(|snapshot| (after, snapshot)))
            .map_err(RegistrySnapshotError::Invalid);
    }
    Err(RegistrySnapshotError::Unstable { attempts: REGISTRY_OBSERVATION_ATTEMPTS })
}

const REGISTRY_DESCRIPTOR_KINDS: [&str; 7] = [
    "interface",
    "artifact",
    "mark",
    "verifier",
    "worker_environment_kind",
    "tool",
    "ee_template",
];

fn registry_descriptor_fingerprint(
    registry_root: &Path,
    ee_instances_dir: &Path,
    files: &mut RegistryFileCache,
) -> std::io::Result<RegistryDescriptorFingerprint> {
    use std::hash::{Hash, Hasher};
    let mut file_count: u64 = 0;
    let mut digest: u128 = 0;
    let mut fold_file = |path: PathBuf| -> std::io::Result<()> {
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            return Ok(());
        }
        let observed_before_read = std::time::SystemTime::now();
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_file() {
            return Ok(());
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        let stamp = octopus_file_storage::metadata::FileStamp::of(&metadata);
        let settled = stamp.is_settled(observed_before_read);
        let content = if let Some(cached) = files.0.get_mut(&path)
            .filter(|cached| cached.stamp == stamp && cached.settled && settled)
        {
            cached.observed = true;
            cached.digest
        } else {
            let mut content = std::collections::hash_map::DefaultHasher::new();
            std::fs::read(&path)?.hash(&mut content);
            let digest = content.finish();
            files.0.insert(path.clone(), RegistryFileObservation {
                stamp, settled, digest, observed: true,
            });
            digest
        };
        content.hash(&mut hasher);
        metadata.len().hash(&mut hasher);
        metadata.modified()?.hash(&mut hasher);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            metadata.dev().hash(&mut hasher);
            metadata.ino().hash(&mut hasher);
            metadata.ctime().hash(&mut hasher);
            metadata.ctime_nsec().hash(&mut hasher);
        }
        digest = digest.wrapping_add((hasher.finish() as u128).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        file_count += 1;
        Ok(())
    };
    for kind in REGISTRY_DESCRIPTOR_KINDS {
        let kind_root = registry_root.join(kind);
        let packages = match std::fs::read_dir(&kind_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for package in packages {
            let package = package?;
            if package.file_name() == octopus_file_storage::publication::COORDINATION_DIRECTORY {
                continue;
            }
            if !package.file_type()?.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(package.path())? {
                let file = file?;
                if file.file_type()?.is_file() {
                    fold_file(file.path())?;
                }
            }
        }
    }
    match std::fs::read_dir(ee_instances_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    fold_file(entry.path())?;
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    files.0.retain(|_, entry| std::mem::take(&mut entry.observed));
    Ok(RegistryDescriptorFingerprint { file_count, digest })
}

#[derive(Default)]
struct PersistedEeInstances {
    valid: Vec<EeInstance>,
    issues: Vec<String>,
}

fn load_persisted_ee_instances(ee_instances_dir: &Path) -> Result<PersistedEeInstances, String> {
    let entries = match std::fs::read_dir(ee_instances_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PersistedEeInstances::default());
        }
        Err(error) => {
            return Err(format!(
                "EE instance directory '{}' cannot be read: {error}",
                ee_instances_dir.display()
            ));
        }
    };
    let mut loaded = PersistedEeInstances::default();
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("EE instance directory entry cannot be read: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                loaded.issues.push(format!(
                    "EE instance '{}' type failed: {error}",
                    path.display()
                ));
                continue;
            }
        };
        if !file_type.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            loaded.issues.push(format!(
                "EE instance '{}' has a non-UTF8 id",
                path.display()
            ));
            continue;
        };
        if stem.is_empty() || stem == "." || stem == ".." || stem.contains('/') {
            loaded
                .issues
                .push(format!("EE instance '{}' has an unsafe id", path.display()));
            continue;
        }
        let expected = EeId::new(stem);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                loaded.issues.push(format!(
                    "EE instance '{}' cannot be read: {error}",
                    path.display()
                ));
                continue;
            }
        };
        let instance: EeInstance = match serde_json::from_slice(&bytes) {
            Ok(instance) => instance,
            Err(error) => {
                loaded.issues.push(format!(
                    "EE instance '{}' is invalid: {error}",
                    path.display()
                ));
                continue;
            }
        };
        if instance.ee_id != expected {
            loaded.issues.push(format!(
                "EE instance '{}' declares id '{}' instead of filename id '{}'",
                path.display(),
                instance.ee_id.as_str(),
                expected.as_str()
            ));
            continue;
        }
        loaded.valid.push(instance);
    }
    Ok(loaded)
}

struct RegistryRunGrantResolver {
    live_registry: Arc<RwLock<LoadedRegistrySnapshot>>,
    blobs: SharedBlobs,
    registry_root: PathBuf,
    ee_instances_root: PathBuf,
    fingerprint: Arc<Mutex<Option<RegistryDescriptorFingerprint>>>,
    files: Arc<Mutex<RegistryFileCache>>,
}

/// The prospective run uses the same descriptor snapshot and policy formation
/// as an arrival. No artifact is admitted, verifier executed, mark recorded or
/// runtime state changed. Dependencies are for bounded candidate revalidation;
/// the public report contains faults, never unrelated run bodies or grants.
pub fn inspect_registry_run_grant(
    registry_root: &Path,
    ee_instances_root: &Path,
    record: &octopus_run_record::RunRecord,
    checkpoint: &octopus_descriptors::CheckpointId,
) -> Result<octopus_interfaces::GrantProjection, String> {
    prospective_run_grant(record, checkpoint, || LoadedRegistrySnapshot::load(registry_root, ee_instances_root))
}

pub(crate) fn prospective_run_grant<S: std::borrow::Borrow<LoadedRegistrySnapshot>>(
    record: &octopus_run_record::RunRecord,
    checkpoint: &octopus_descriptors::CheckpointId,
    snapshot: impl FnOnce() -> Result<S, String>,
) -> Result<octopus_interfaces::GrantProjection, String> {
    if !record.active_checkpoints.contains(checkpoint) { return Err("prospective grant requires an active checkpoint".into()); }
    let checkpoint = record.checkpoint(checkpoint).ok_or("prospective checkpoint is absent")?;
    let interfaces = record.active_checkpoints.iter().filter_map(|id|record.checkpoint(id))
        .flat_map(|cp|cp.inbound_interface().into_iter().chain(cp.outbound_interfaces())).cloned().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
    let artifacts = checkpoint.artifact_bindings.iter().map(|binding|binding.id.clone()).collect::<Vec<_>>();
    let access = checkpoint.artifact_bindings.iter().map(|binding|(binding.id.clone(),binding.access)).collect();
    let query = RunGrantQuery {run:&record.id,checkpoint:&checkpoint.id,ee:&record.ee,worker:&record.worker,epoch:checkpoint.epoch,
        authored_interfaces:&interfaces,artifact_bindings:&artifacts,artifact_access:&access,required_verified_artifacts:&checkpoint.required_verified_artifacts};
    let snapshot = snapshot()?;
    snapshot.borrow().form_run_contract(&query).map(|(_,_,grant)|grant)
}

pub(crate) fn validate_registry_run_contracts(
    snapshot: &LoadedRegistrySnapshot,
    registry_root: &Path,
    ee_instances_root: &Path,
    records: &[octopus_run_record::RunRecord],
) -> Result<
    (
        Vec<crate::RegistryValidationIssue>,
        BTreeMap<String, BTreeSet<String>>,
        BTreeSet<String>,
    ),
    String,
> {
    let mut issues = Vec::new();
    let available_ee = snapshot
        .ee_tree
        .instances()
        .map(|(id, _)| format!("ee/{id}"))
        .collect::<BTreeSet<_>>();
    for entry in std::fs::read_dir(ee_instances_root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json")
            || !entry.file_type().map_err(|error| error.to_string())?.is_file()
        {
            continue;
        }
        let id = path.file_stem().unwrap_or_default().to_string_lossy();
        let subject = format!("ee/{id}");
        if !available_ee.contains(&subject) {
            let reason = snapshot
                .ee_instance_issues
                .iter()
                .find(|reason| {
                    reason.contains(&path.display().to_string())
                        || reason.contains(&format!("instance '{id}'"))
                })
                .cloned()
                .unwrap_or_else(|| {
                    "EE instance is unavailable to the persisted EE loader and topology former".into()
                });
            issues.push(crate::RegistryValidationIssue { subject, checkpoint: None, reason });
        }
    }
    let mut dependencies = BTreeMap::new();
    for record in records {
        let subject = format!("run/{}", record.id);
        let active = record
            .active_checkpoints
            .iter()
            .filter_map(|id| record.checkpoint(id))
            .collect::<Vec<_>>();
        let interfaces = active
            .iter()
            .flat_map(|checkpoint| {
                checkpoint
                    .inbound_interface()
                    .into_iter()
                    .chain(checkpoint.outbound_interfaces())
                    .cloned()
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut roots = vec![RegistryNode::interface(record.worker.as_str())];
        roots.extend(
            interfaces
                .iter()
                .map(|interface| RegistryNode::interface(interface.as_str())),
        );
        let mut inputs = BTreeSet::from([format!("ee/{}", record.ee), subject.clone()]);
        if let Ok(chain) = snapshot.ee_tree.ancestors(&record.ee) {
            for ee in chain {
                inputs.insert(format!("ee/{ee}"));
                if let Some(instance) = snapshot.ee_tree.get(&ee) {
                    roots.push(RegistryNode::ee_template(instance.template.as_str()));
                }
            }
        }
        for checkpoint in &active {
            roots.extend(
                checkpoint
                    .artifact_bindings
                    .iter()
                    .filter(|binding| !artifact_id_carries_content_hash(&binding.id))
                    .map(|binding| RegistryNode::artifact(binding.id.as_str())),
            );
            roots.extend(
                checkpoint
                    .required_verified_artifacts
                    .iter()
                    .flat_map(|required| {
                        [
                            RegistryNode::mark(required.mark.as_str()),
                            RegistryNode::verifier(required.verifier.as_str()),
                        ]
                    }),
            );
        }
        inputs.extend(
            snapshot
                .reference_graph
                .dependency_nodes(roots)
                .into_iter()
                .map(|node| node.to_string()),
        );
        for checkpoint in active {
            let artifacts = checkpoint
                .artifact_bindings
                .iter()
                .map(|binding| binding.id.clone())
                .collect::<Vec<_>>();
            let access = checkpoint
                .artifact_bindings
                .iter()
                .map(|binding| (binding.id.clone(), binding.access))
                .collect::<BTreeMap<_, _>>();
            let query = RunGrantQuery {
                run: &record.id,
                checkpoint: &checkpoint.id,
                ee: &record.ee,
                worker: &record.worker,
                epoch: checkpoint.epoch,
                authored_interfaces: &interfaces,
                artifact_bindings: &artifacts,
                artifact_access: &access,
                required_verified_artifacts: &checkpoint.required_verified_artifacts,
            };
            let checked = (|| -> Result<(), String> {
                let (formed, _, _) = snapshot.form_run_contract(&query)?;
                for interface in checkpoint
                    .inbound_interface()
                    .into_iter()
                    .chain(checkpoint.outbound_interfaces())
                {
                    if !snapshot.interface_dispositions.contains_key(interface)
                        || !formed.context.interfaces.contains(interface)
                    {
                        return Err(format!(
                            "authored interface '{interface}' is unavailable or did not survive worker/EE/binding formation"
                        ));
                    }
                }
                for (artifact, rights) in &access {
                    if formed.context.artifact_access.get(artifact) != Some(rights) {
                        return Err(format!(
                            "authored artifact rights for '{artifact}' did not survive worker/EE/binding formation"
                        ));
                    }
                }
                for required in &checkpoint.required_verified_artifacts {
                    if !formed.context.mark_outcomes.contains(&required.mark) {
                        return Err(format!(
                            "required mark '{}' did not survive worker/EE formation",
                            required.mark
                        ));
                    }
                    let package_dir = registry_root
                        .join("verifier")
                        .join(required.verifier.as_str());
                    let descriptor = parse_strict::<VerifierDescriptor>(
                        &std::fs::read(package_dir.join("verifier.json"))
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                    pin_verifier_package(&VerifierPackageContext {
                        descriptor,
                        package_dir: Some(package_dir),
                        repository_root: registry_root
                            .parent()
                            .unwrap_or(registry_root)
                            .to_path_buf(),
                        scratch_root: registry_root.to_path_buf(),
                    })
                    .map_err(|details| {
                        format!(
                            "required verifier package '{}' is unavailable: {details}",
                            required.verifier
                        )
                    })?;
                }
                Ok(())
            })();
            if let Err(reason) = checked {
                issues.push(crate::RegistryValidationIssue {
                    subject: subject.clone(),
                    checkpoint: Some(checkpoint.id.to_string()),
                    reason: format!("checkpoint '{}': {reason}", checkpoint.id),
                });
            }
        }
        dependencies.insert(subject, inputs);
    }
    Ok((issues, dependencies, available_ee))
}

fn insert_formation_source(
    hashes: &mut BTreeMap<DescriptorName, DescriptorHash>,
    sources: &mut BTreeMap<DescriptorName, Vec<u8>>,
    name: DescriptorName,
    value: &impl serde::Serialize,
) -> Result<(), String> {
    let value = serde_json::to_value(value).map_err(|error| {
        format!(
            "formation source '{}' could not be encoded: {error}",
            name.as_str()
        )
    })?;
    let bytes = octopus_descriptors::canonical_bytes(&value);
    hashes.insert(name.clone(), DescriptorHash::of_bytes(&bytes));
    sources.insert(name, bytes);
    Ok(())
}

impl RunGrantResolver for RegistryRunGrantResolver {
    fn acquire_commit_guard(&self, record: &octopus_run_record::RunRecord, marks: &[Mark]) -> Result<Option<Box<dyn octopus_runtime::AdmissionSourceGuard>>, String> {
        commit::acquire(self, record, marks).map(|guard| Some(Box::new(guard) as Box<dyn octopus_runtime::AdmissionSourceGuard>))
    }

    fn resolve_run_grant(&self, query: RunGrantQuery<'_>) -> Result<RunGrantResolution, String> {
        let snapshot = self
            .live_registry
            .read()
            .map_err(|_| "live registry snapshot lock is poisoned".to_string())?;
        let (formed, role_artifacts, metadata_projection) = snapshot.form_run_contract(&query)?;
        let mut blobs = self
            .blobs
            .lock()
            .map_err(|_| "formation input blob store lock is poisoned".to_string())?;
        snapshot.retain_formation_inputs(&formed.pin, &role_artifacts, &mut *blobs)?;
        RunGrantResolution::new(formed, metadata_projection)
    }
}

impl LoadedRegistrySnapshot {
    fn form_run_contract(
        &self,
        query: &RunGrantQuery<'_>,
    ) -> Result<
        (
            FormedPolicy,
            Vec<ArtifactId>,
            octopus_interfaces::GrantProjection,
        ),
        String,
    > {
        self.validate_run_reference_closure(query)?;
        let declared = self.worker_interface_policy(query.worker)?;
        let ee_layers = self
            .ee_tree
            .policy_layers(query.ee)
            .map_err(|error| format!("run '{}' EE chain failed: {error}", query.run.as_str()))?;
        let original = self.admitted_original(query.worker, &ee_layers);
        let original_policy = original.as_ref().map(|original| self.worker_interface_policy(original)).transpose()?;
        let narrowed = original_policy.map(|original| declared.narrowed_by(original, &self.copy_lineage));
        let policy = narrowed.as_ref().unwrap_or(declared);
        let authored = crate::worker_policy::AuthoredBindings {
            interfaces: query.authored_interfaces,
            tool_ownership: &self.tool_ownership,
            tool_supported_weks: &self.tool_supported_weks,
            artifact_access: query.artifact_access,
        };
        let mut formed = crate::worker_policy::form_worker_run_policy(
            query.worker,
            policy,
            &self.ee_tree,
            query.ee,
            &self.formation_source_hashes,
            &authored,
            &self.copy_lineage,
        )
        .map_err(|e| {
            format!(
                "grant formation for run '{}' checkpoint '{}' worker interface '{}' failed: {e}",
                query.run.as_str(),
                query.checkpoint.as_str(),
                query.worker.as_str()
            )
        })?;
        if let Some(original) = &original {
            for source in [
                crate::worker_policy::worker_interface_descriptor_name(original),
                crate::worker_policy::interface_descriptor_source_name(original),
            ] {
                let hash = self.formation_source_hashes.get(&source).ok_or_else(|| {
                    format!("original '{}' of worker '{}' has no formation source", original.as_str(), query.worker.as_str())
                })?;
                formed.pin.inputs.insert(source, *hash);
            }
        }
        self.form_interface_policy(&mut formed, query.ee, policy, original_policy)?;
        let role_artifacts = self.pin_mandatory_role_artifacts(query, policy, &mut formed)?;
        let metadata_projection =
            self.metadata_projection_for_grant(query.worker, query.ee, &formed, &role_artifacts)?;
        Ok((formed, role_artifacts, metadata_projection))
    }

    fn from_loaded(
        interfaces_loaded: Vec<(DescriptorName, InterfaceDescriptor)>,
        interface_dispositions: BTreeMap<InterfaceId, InterfaceRuntimeDisposition>,
        worker_interface_policies: BTreeMap<InterfaceId, WorkerInterfacePolicy>,
        tool_descriptors_loaded: Vec<(DescriptorName, ToolDescriptor)>,
        worker_environment_kinds_loaded: Vec<(DescriptorName, WorkerEnvKindSpec)>,
        reference_graph: RegistryReferenceGraph,
        ee_tree: EeTree,
        ee_templates: &[(DescriptorName, EeTemplateDescriptor)],
        ee_instance_issues: Vec<String>,
    ) -> Result<Self, String> {
        let interfaces_by_id = interface_map(&interfaces_loaded);
        let tool_descriptions = RegistryToolDescriptions::from_loaded(&tool_descriptors_loaded);
        let tool_ownership = tool_descriptions.owner_map();
        let tool_supported_weks = tool_descriptors_loaded
            .iter()
            .map(|(_, descriptor)| {
                (
                    ToolId::new(descriptor.id.as_str()),
                    descriptor
                        .supported_worker_environment_kinds
                        .iter()
                        .cloned()
                        .collect(),
                )
            })
            .collect();
        let worker_environment_kinds = worker_environment_kinds_loaded
            .iter()
            .map(|(_, descriptor)| descriptor.worker_environment_kind_id.clone())
            .collect();
        let mut copy_lineage = CopyLineage::default();
        for (_, descriptor) in &interfaces_loaded {
            if let Some(original) = descriptor.copy_of.as_ref().filter(|original| interfaces_by_id.contains_key(*original)) {
                copy_lineage.insert_interface(descriptor.id.clone(), original.clone());
            }
        }
        for (_, descriptor) in &tool_descriptors_loaded {
            let Some(original) = &descriptor.copy_of else { continue };
            let original = ToolId::new(original.as_str());
            if tool_descriptions.by_tool.get(&original).is_some_and(|known| descriptor.never_wider_than(known)) {
                copy_lineage.insert_tool(ToolId::new(descriptor.id.as_str()), original);
            } else {
                copy_lineage.insert_declared_tool(ToolId::new(descriptor.id.as_str()), original);
            }
        }
        let mut formation_source_hashes = BTreeMap::new();
        let mut formation_source_bytes = BTreeMap::new();
        for (_, instance) in ee_tree.instances() {
            insert_formation_source(
                &mut formation_source_hashes,
                &mut formation_source_bytes,
                crate::worker_policy::ee_instance_source_name(&instance.ee_id),
                instance,
            )?;
        }
        for (_, descriptor) in &interfaces_loaded {
            insert_formation_source(
                &mut formation_source_hashes,
                &mut formation_source_bytes,
                crate::worker_policy::interface_descriptor_source_name(&descriptor.id),
                descriptor,
            )?;
        }
        for (worker, policy) in &worker_interface_policies {
            insert_formation_source(
                &mut formation_source_hashes,
                &mut formation_source_bytes,
                crate::worker_policy::worker_interface_descriptor_name(worker),
                policy,
            )?;
        }
        for (_, descriptor) in &tool_descriptors_loaded {
            insert_formation_source(
                &mut formation_source_hashes,
                &mut formation_source_bytes,
                crate::worker_policy::tool_descriptor_source_name(&ToolId::new(
                    descriptor.id.as_str(),
                )),
                descriptor,
            )?;
        }
        for (_, descriptor) in &worker_environment_kinds_loaded {
            insert_formation_source(
                &mut formation_source_hashes,
                &mut formation_source_bytes,
                crate::worker_policy::worker_environment_kind_source_name(
                    &descriptor.worker_environment_kind_id,
                ),
                descriptor,
            )?;
        }
        for (_, template) in ee_templates {
            insert_formation_source(
                &mut formation_source_hashes,
                &mut formation_source_bytes,
                crate::worker_policy::ee_template_source_name(template.id.as_str()),
                template,
            )?;
        }
        Ok(Self {
            interfaces_loaded,
            interfaces_by_id,
            interface_dispositions,
            worker_interface_policies,
            tool_descriptions,
            tool_ownership,
            tool_supported_weks,
            worker_environment_kinds,
            reference_graph,
            ee_tree,
            copy_lineage,
            formation_source_hashes,
            formation_source_bytes,
            ee_instance_issues,
        })
    }

    fn retain_formation_inputs(
        &self,
        pin: &FormationPin,
        role_artifacts: &[ArtifactId],
        blobs: &mut dyn BlobStore,
    ) -> Result<(), String> {
        let mut role_sources = BTreeMap::new();
        for artifact in role_artifacts {
            role_sources.insert(
                crate::worker_policy::artifact_descriptor_source_name(artifact),
                self.reference_graph.artifact_descriptor_bytes(artifact)?,
            );
        }
        let mut contents = Vec::with_capacity(pin.inputs.len());
        for (name, hash) in &pin.inputs {
            let bytes = self
                .formation_source_bytes
                .get(name)
                .map(Vec::as_slice)
                .or_else(|| role_sources.get(name).copied())
                .ok_or_else(|| {
                    format!(
                        "formation input '{}' has no normalized source bytes",
                        name.as_str()
                    )
                })?;
            if DescriptorHash::of_bytes(bytes) != *hash {
                return Err(format!(
                    "formation input '{}' differs from its pinned hash",
                    name.as_str()
                ));
            }
            contents.push(bytes);
        }
        let stored = blobs.put_batch(&contents);
        if stored.len() != contents.len() {
            return Err("formation input batch returned an incomplete set of hashes".into());
        }
        for (((name, _), bytes), actual) in pin.inputs.iter().zip(contents).zip(stored) {
            let expected = ContentHash::of_bytes(bytes);
            if actual != expected || blobs.get(&expected).as_deref() != Some(bytes) {
                return Err(format!(
                    "formation input '{}' was not retained at its pinned hash",
                    name.as_str()
                ));
            }
        }
        Ok(())
    }

    fn validate_run_reference_closure(&self, query: &RunGrantQuery<'_>) -> Result<(), String> {
        let mut references = Vec::new();
        for ee in self
            .ee_tree
            .ancestors(query.ee)
            .map_err(|error| format!("run '{}' EE chain failed: {error}", query.run.as_str()))?
        {
            let instance = self.ee_tree.get(&ee).ok_or_else(|| {
                format!(
                    "run '{}' EE chain contains missing instance '{}'",
                    query.run.as_str(),
                    ee.as_str()
                )
            })?;
            references.push((
                format!("ee_chain.{}", ee.as_str()),
                RegistryNode::ee_template(instance.template.as_str()),
            ));
        }
        references.push((
            "worker".to_string(),
            RegistryNode::interface(query.worker.as_str()),
        ));
        references.extend(query.authored_interfaces.iter().map(|interface| {
            (
                "checkpoint.interface".to_string(),
                RegistryNode::interface(interface.as_str()),
            )
        }));
        for required in query.required_verified_artifacts {
            references.push((
                "checkpoint.required_verified_artifacts.mark".to_string(),
                RegistryNode::mark(required.mark.as_str()),
            ));
            references.push((
                "checkpoint.required_verified_artifacts.verifier".to_string(),
                RegistryNode::verifier(required.verifier.as_str()),
            ));
        }
        references.extend(
            query
                .artifact_bindings
                .iter()
                .filter(|artifact| !artifact_id_carries_content_hash(artifact))
                .map(|artifact| {
                    (
                        "checkpoint.artifact_bindings".to_string(),
                        RegistryNode::artifact(artifact.as_str()),
                    )
                }),
        );
        self.reference_graph
            .validate_active_references(
                &format!(
                    "run/{}/checkpoint/{}",
                    query.run.as_str(),
                    query.checkpoint.as_str()
                ),
                references,
            )
            .map(|_| ())
    }

    /// A copy is admitted by its declared original when some layer of the run's EE
    /// chain lists the original but not the copy itself.
    fn admitted_original(&self, interface: &InterfaceId, layers: &[PolicyLayer]) -> Option<InterfaceId> {
        let original = self.copy_lineage.interface_original(interface)?;
        layers
            .iter()
            .any(|layer| layer.interfaces.as_ref().is_some_and(|allowed| !allowed.contains(interface) && allowed.contains(original)))
            .then(|| original.clone())
    }

    fn form_interface_policy(
        &self,
        formed: &mut FormedPolicy,
        ee: &EeId,
        worker_policy: &WorkerInterfacePolicy,
        original_worker: Option<&WorkerInterfacePolicy>,
    ) -> Result<(), String> {
        let override_layers = self
            .ee_tree
            .interface_policy_override_layers(ee)
            .map_err(|error| format!("interface policy EE chain failed: {error}"))?;
        let ee_layers = self
            .ee_tree
            .policy_layers(ee)
            .map_err(|error| format!("interface policy EE chain failed: {error}"))?;
        validate_worker_interface_policy_scope(worker_policy, formed)?;
        let worker_layers = std::iter::once(&worker_policy.ceiling.interface_policy)
            .chain(original_worker.map(|original| &original.ceiling.interface_policy))
            .collect::<Vec<_>>();
        let interfaces = formed.boundary.interfaces.iter().chain(formed.context.interfaces.iter()).cloned().collect::<BTreeSet<_>>();
        let mut originals = BTreeMap::new();
        for interface in &interfaces {
            if let Some(original) = self.admitted_original(interface, &ee_layers) {
                let source = crate::worker_policy::interface_descriptor_source_name(&original);
                let hash = self.formation_source_hashes.get(&source).ok_or_else(|| {
                    format!("original '{}' of interface '{}' has no formation source", original.as_str(), interface.as_str())
                })?;
                formed.pin.inputs.insert(source, *hash);
                originals.insert(interface.clone(), original);
            }
        }

        let mut boundary = InterfacePolicyGrant::new();
        for interface in formed.boundary.interfaces.iter() {
            if !self.interfaces_by_id.contains_key(interface) {
                continue;
            }
            let source = crate::worker_policy::interface_descriptor_source_name(interface);
            let hash = self.formation_source_hashes.get(&source).ok_or_else(|| {
                format!(
                    "interface policy '{}' has no formation source",
                    interface.as_str()
                )
            })?;
            formed.pin.inputs.insert(source, *hash);
            let (boundary_values, _) = form_interface_policy_for_interface(
                interface,
                &self.interfaces_by_id,
                &worker_layers,
                originals.get(interface),
                &override_layers,
                false,
            )?;
            if !boundary_values.is_empty() {
                boundary.insert(interface.clone(), boundary_values);
            }
        }

        let mut context = InterfacePolicyGrant::new();
        for interface in formed.context.interfaces.iter() {
            if !self.interfaces_by_id.contains_key(interface) {
                continue;
            }
            let (_, context_values) = form_interface_policy_for_interface(
                interface,
                &self.interfaces_by_id,
                &worker_layers,
                originals.get(interface),
                &override_layers,
                true,
            )?;
            if !context_values.is_empty() {
                context.insert(interface.clone(), context_values);
            }
        }
        formed.boundary.interface_policy = boundary;
        formed.context.interface_policy = context;
        Ok(())
    }

    fn metadata_projection_for_grant(
        &self,
        worker: &InterfaceId,
        ee: &EeId,
        formed: &FormedPolicy,
        role_artifacts: &[ArtifactId],
    ) -> Result<octopus_interfaces::GrantProjection, String> {
        let allowed_tools = formed.context.tools.clone();
        let affordances = project_interface_tool_affordances(
            &allowed_tools,
            &formed.context,
            &self.tool_ownership,
            &self.tool_descriptions,
        )
        .map_err(|e| {
            format!(
                "interface metadata tool affordance projection failed for worker interface '{}': {e}",
                worker.as_str()
            )
        })?;
        let attached = attached_bindings_from_grant(formed, &self.interfaces_by_id);
        let goal = self.ee_tree.get(ee).map(|instance| instance.goal.clone());
        let ee_root_slot_allowance = self
            .ee_tree
            .root_slot_allowance(ee)
            .map_err(|error| {
                format!(
                    "worker interface '{}' EE root-slot allowance projection failed: {error}",
                    worker.as_str()
                )
            })?
            .allowed()
            .map(|slots| slots.iter().cloned().collect::<Vec<_>>());
        interface_metadata_projection_body(
            goal,
            role_artifacts
                .iter()
                .map(|artifact| artifact.as_str().to_string())
                .collect(),
            &affordances,
            formed,
            attached,
        )
        .map(|projection| projection.with_ee_root_slot_allowance(ee_root_slot_allowance))
    }

    fn pin_mandatory_role_artifacts(
        &self,
        query: &RunGrantQuery<'_>,
        worker_policy: &WorkerInterfacePolicy,
        formed: &mut FormedPolicy,
    ) -> Result<Vec<ArtifactId>, String> {
        for interface in formed.context.interfaces.iter() {
            if self.worker_interface_policies.contains_key(interface) {
                let source = crate::worker_policy::worker_interface_descriptor_name(interface);
                let hash = self.formation_source_hashes.get(&source).ok_or_else(|| {
                    format!(
                        "role dependency worker '{}' has no policy source hash",
                        interface.as_str()
                    )
                })?;
                formed.pin.inputs.insert(source, *hash);
            }
        }
        let dependencies = crate::worker_policy::mandatory_role_artifact_dependencies(
            query.worker,
            worker_policy,
            formed,
            &self.worker_interface_policies,
        );
        for dependency in &dependencies {
            if !query
                .artifact_access
                .get(&dependency.artifact)
                .is_some_and(|access| access.read)
            {
                return Err(format!(
                    "mandatory role artifact dependency '{}' required by worker interface '{}' is not explicitly bound with read=true in run '{}' checkpoint '{}'",
                    dependency.artifact.as_str(),
                    dependency.required_by.as_str(),
                    query.run.as_str(),
                    query.checkpoint.as_str()
                ));
            }
            if !formed
                .context
                .artifact_access
                .get(&dependency.artifact)
                .is_some_and(|access| access.read)
            {
                return Err(format!(
                    "mandatory role artifact dependency '{}' required by worker interface '{}' has an explicit read binding that did not survive policy formation for run '{}' checkpoint '{}'",
                    dependency.artifact.as_str(),
                    dependency.required_by.as_str(),
                    query.run.as_str(),
                    query.checkpoint.as_str()
                ));
            }
            let hash = self
                .reference_graph
                .artifact_descriptor_hash(&dependency.artifact)
                .map_err(|error| {
                    format!(
                        "mandatory role artifact dependency '{}' required by worker interface '{}' is not available in the loaded registry: {error}",
                        dependency.artifact.as_str(),
                        dependency.required_by.as_str()
                    )
                })?;
            formed.pin.inputs.insert(
                crate::worker_policy::artifact_descriptor_source_name(&dependency.artifact),
                hash,
            );
        }
        Ok(dependencies
            .into_iter()
            .map(|dependency| dependency.artifact)
            .collect())
    }

    fn load(registry_root: &Path, ee_instances_dir: &Path) -> Result<Self, String> {
        let ee_instances = load_persisted_ee_instances(ee_instances_dir)?;
        let packages = RegistryPackages::load(registry_root)
            .map_err(|error| format!("artifact-arrival registry read failed: {error}"))?;
        Self::from_packages(packages, ee_instances)
    }

    pub(crate) fn load_from_packages(packages: RegistryPackages, ee_instances_dir: &Path) -> Result<Self, String> {
        let ee_instances = load_persisted_ee_instances(ee_instances_dir)?;
        Self::from_packages(packages, ee_instances)
    }

    fn from_packages(packages: RegistryPackages, mut ee_instances: PersistedEeInstances) -> Result<Self, String> {
        let (ee_tree, _, mut topology_issues) = registry_ee_state(
            &packages.ee_templates,
            &packages.graph,
            &packages.activations,
            &ee_instances.valid,
            "artifact-arrival registry read",
        )?;
        ee_instances.issues.append(&mut topology_issues);
        Self::from_loaded(
            packages.interfaces,
            packages.interface_dispositions,
            packages.worker_policies,
            packages.tools,
            packages.worker_environment_kinds,
            packages.graph,
            ee_tree,
            &packages.ee_templates,
            ee_instances.issues,
        )
    }

    fn worker_interface_policy(
        &self,
        worker: &InterfaceId,
    ) -> Result<&WorkerInterfacePolicy, String> {
        self.worker_interface_policies.get(worker).ok_or_else(|| {
            format!(
                "worker interface '{}' has no loaded policy pin in the live registry snapshot",
                worker.as_str()
            )
        })
    }

    fn ee_policy_ceiling(&self) -> ContextGrant {
        ContextGrant {
            tools: CapSet::from_allowed(self.tool_descriptions.by_tool.keys().cloned()),
            interfaces: CapSet::from_allowed(self.interfaces_by_id.keys().cloned()),
            wek: CapSet::from_allowed(self.worker_environment_kinds.iter().cloned()),
            mark_outcomes: CapSet::from_allowed(std::iter::empty()),
            hard_blocks_tools: DenySet::default(),
            interface_policy: Default::default(),
            artifact_verbs: CapSet::from_allowed(ArtifactVerb::ALL),
            artifact_access: Default::default(),
        }
    }

    fn ee_policy_projection(
        &self,
        ee: &EeId,
        omitted_instance_narrowing: Option<&EeId>,
    ) -> Result<EePolicyProjection, String> {
        let layers = self
            .ee_tree
            .policy_layers_omitting_instance_narrowing(ee, omitted_instance_narrowing)
            .map_err(|error| format!("EE '{}' policy chain failed: {error}", ee.as_str()))?;
        let formed = form_policy_with_lineage(
            self.ee_policy_ceiling(),
            &layers,
            &[],
            FormationPin::default(),
            &self.copy_lineage,
        );
        let allowed_root_slots = self
            .ee_tree
            .root_slot_allowance_omitting_instance_narrowing(ee, omitted_instance_narrowing)
            .map_err(|error| format!("EE '{}' root-slot chain failed: {error}", ee.as_str()))?
            .allowed()
            .map(|slots| slots.iter().cloned().collect());
        let mut allowed_artifact_verbs = formed
            .context
            .artifact_verbs
            .iter()
            .map(|verb| verb.as_str().to_string())
            .collect::<Vec<_>>();
        allowed_artifact_verbs.sort();
        Ok(EePolicyProjection {
            allowed_interfaces: formed
                .context
                .interfaces
                .iter()
                .map(|id| id.as_str().to_string())
                .collect(),
            allowed_tools: formed
                .context
                .tools
                .iter()
                .map(|id| id.as_str().to_string())
                .collect(),
            allowed_worker_environment_kinds: formed
                .context
                .wek
                .iter()
                .map(|id| id.as_str().to_string())
                .collect(),
            allowed_root_slots,
            allowed_artifact_verbs,
            hard_blocked_tools: formed
                .context
                .hard_blocks_tools
                .iter()
                .map(|id| id.as_str().to_string())
                .collect(),
        })
    }

    fn ee_worker_interface_policy_projection(
        &self,
        ee: &EeId,
        effective_policy: &EePolicyProjection,
    ) -> Result<Vec<EeWorkerInterfacePolicyProjection>, String> {
        let allowed_interfaces = effective_policy
            .allowed_interfaces
            .iter()
            .map(|id| InterfaceId::new(id.clone()))
            .collect::<BTreeSet<_>>();
        let ee_layers = self
            .ee_tree
            .policy_layers_omitting_instance_narrowing(ee, None)
            .map_err(|error| format!("EE '{}' policy chain failed: {error}", ee.as_str()))?;
        let mut workers = Vec::new();
        for (worker, worker_policy) in &self.worker_interface_policies {
            if !allowed_interfaces.contains(worker) {
                continue;
            }
            let mut formed = form_policy_with_lineage(
                worker_policy.ceiling_grant(),
                &ee_layers,
                &[],
                FormationPin::default(),
                &self.copy_lineage,
            );
            self.form_interface_policy(&mut formed, ee, worker_policy, None)?;
            workers.push(EeWorkerInterfacePolicyProjection {
                worker_interface_id: worker.as_str().to_string(),
                interface_policy: formed.context.interface_policy,
            });
        }
        workers.sort_by(|left, right| left.worker_interface_id.cmp(&right.worker_interface_id));
        Ok(workers)
    }
}

pub fn project_ee_policies(
    registry_root: &Path,
    ee_instances_dir: &Path,
) -> Result<EePolicyProjectionSnapshot, String> {
    let snapshot = LoadedRegistrySnapshot::load(registry_root, ee_instances_dir)?;
    let mut environments = snapshot
        .ee_tree
        .instances()
        .map(|(ee, _)| {
            let effective_policy = snapshot.ee_policy_projection(ee, None)?;
            Ok(EePolicyProjectionRow {
                ee_id: ee.as_str().to_string(),
                workers: snapshot.ee_worker_interface_policy_projection(ee, &effective_policy)?,
                effective_policy,
                policy_ceiling: snapshot.ee_policy_projection(ee, Some(ee))?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    environments.sort_by(|left, right| left.ee_id.cmp(&right.ee_id));
    Ok(EePolicyProjectionSnapshot {
        environments,
        issues: snapshot.ee_instance_issues,
    })
}

fn validate_worker_interface_policy_scope(
    worker_policy: &WorkerInterfacePolicy,
    formed: &FormedPolicy,
) -> Result<(), String> {
    for interface in worker_policy.ceiling.interface_policy.keys() {
        if !formed.boundary.interfaces.contains(interface) {
            return Err(format!(
                "worker interface '{}' declares interface policy for '{}' outside its interface ceiling",
                worker_policy.id.as_str(),
                interface.as_str()
            ));
        }
    }
    Ok(())
}

fn form_interface_policy_for_interface(
    interface: &InterfaceId,
    by_id: &BTreeMap<InterfaceId, InterfaceDescriptor>,
    worker_layers: &[&octopus_policy::InterfacePolicyOverrideLayer],
    original: Option<&InterfaceId>,
    override_layers: &[octopus_policy::InterfacePolicyOverrideLayer],
    apply_ee_overrides: bool,
) -> Result<(InterfacePolicyValues, InterfacePolicyValues), String> {
    let exact = by_id.get(interface).ok_or_else(|| {
        format!(
            "interface '{}' has no loaded descriptor during interface policy formation",
            interface.as_str()
        )
    })?;
    let original_values = original
        .map(|original| {
            let declared = by_id.get(original).ok_or_else(|| {
                format!("original '{}' of interface '{}' is not loaded", original.as_str(), interface.as_str())
            })?;
            octopus_policy::validate_interface_policy_copy(&exact.interface_policy, &declared.interface_policy)
                .map_err(|error| format!("interface '{}' admitted by '{}': {error}", interface.as_str(), original.as_str()))?;
            octopus_policy::form_interface_policy_values(&declared.interface_policy.variables, &[&declared.interface_policy.values])
        })
        .transpose()?;
    let boundary_extra = worker_layers
        .iter()
        .flat_map(|layer| {
            [Some(&exact.id), exact.copy_of.as_ref()]
                .into_iter()
                .flatten()
                .filter_map(|id| layer.get(id))
        })
        .chain(original_values.as_ref())
        .collect::<Vec<_>>();
    let ee_layers = if apply_ee_overrides {
        override_layers
            .iter()
            .flat_map(|layer| {
                [Some(&exact.id), exact.copy_of.as_ref()]
                    .into_iter()
                    .flatten()
                    .filter_map(|id| layer.get(id))
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let formed = octopus_policy::form_narrowed_interface_policy(
        &exact.interface_policy,
        &boundary_extra,
        &ee_layers,
    )
    .map_err(|error| {
        format!(
            "interface '{}' policy formation failed: {error}",
            interface.as_str()
        )
    })?;
    Ok((formed.boundary, formed.context))
}

#[derive(Clone, Debug, Default)]
struct RegistryToolDescriptions {
    by_tool: BTreeMap<ToolId, ToolDescriptor>,
}

impl RegistryToolDescriptions {
    fn from_loaded(loaded: &[(DescriptorName, ToolDescriptor)]) -> Self {
        Self {
            by_tool: loaded
                .iter()
                .map(|(_, descriptor)| (ToolId::new(descriptor.id.as_str()), descriptor.clone()))
                .collect(),
        }
    }

    fn owner_map(&self) -> InterfaceToolOwnerMap {
        InterfaceToolOwnerMap::from_pairs(
            self.by_tool
                .iter()
                .map(|(tool, descriptor)| (tool.clone(), descriptor.interface.clone())),
        )
    }
}

impl InterfaceToolDescriptionView for RegistryToolDescriptions {
    fn describe(&self, tool: &ToolId) -> Option<InterfaceToolAffordance> {
        self.by_tool
            .get(tool)
            .map(|descriptor| InterfaceToolAffordance {
                name: tool.clone(),
                description: descriptor.description.clone(),
                args_schema: descriptor.args_schema.clone(),
            })
    }
}

fn resolve_bootstrap_ee_template(
    loaded: &[(DescriptorName, EeTemplateDescriptor)],
) -> Result<(DescriptorName, &EeTemplateDescriptor, EeId), String> {
    let mut matches = loaded
        .iter()
        .filter(|(_, template)| template.bootstrap_root);
    let Some((name, template)) = matches.next() else {
        return Err(
            "registry/ee_template must declare exactly one bootstrap_root=true root".to_string(),
        );
    };
    if let Some((_other_name, other_template)) = matches.next() {
        return Err(format!(
            "registry/ee_template declares multiple bootstrap roots: '{}' and '{}'",
            template.id.as_str(),
            other_template.id.as_str()
        ));
    }
    Ok((name.clone(), template, bootstrap_ee_instance_id(template)))
}

fn artifact_id_carries_content_hash(artifact: &ArtifactId) -> bool {
    artifact
        .as_str()
        .rsplit_once(':')
        .and_then(|(_, hash)| ContentHash::from_hex(hash))
        .is_some()
}

fn registry_ee_state(
    templates: &[(DescriptorName, EeTemplateDescriptor)],
    graph: &RegistryReferenceGraph,
    activations: &[ActivationReferenceSet],
    dynamic_instances: &[EeInstance],
    source: &str,
) -> Result<(EeTree, EeId, Vec<String>), String> {
    let (_, bootstrap_template, bootstrap_ee) = resolve_bootstrap_ee_template(templates)?;
    let boot_references = vec![(
        "bootstrap_root".to_string(),
        RegistryNode::ee_template(bootstrap_template.id.as_str()),
    )];
    graph
        .validate_active_references(source, boot_references)
        .map_err(|error| format!("{source}: boot registry closure failed: {error}"))?;

    let mut tree = EeTree::new();
    let template_descriptors = templates
        .iter()
        .map(|(_, template)| template.clone())
        .collect::<Vec<_>>();
    for template in &template_descriptors {
        tree.insert_template(template.clone());
    }
    let root = dynamic_instances
        .iter()
        .find(|instance| instance.ee_id == bootstrap_ee)
        .ok_or_else(|| {
            format!(
                "{source}: persisted bootstrap EE instance '{}' is missing; normal runtime boot never recreates deleted EE records",
                bootstrap_ee.as_str()
            )
        })?;
    if root.template != bootstrap_template.id
        || root.parent_ee.is_some()
        || root.goal.trim().is_empty()
    {
        return Err(format!(
            "{source}: persisted bootstrap EE instance '{}' has invalid root identity/template/goal",
            bootstrap_ee.as_str()
        ));
    }
    tree.insert_instance(root.clone());

    let mut issues = Vec::new();
    let mut pending = BTreeMap::new();
    for instance in dynamic_instances.iter().cloned() {
        if instance.ee_id == bootstrap_ee {
            continue;
        }
        pending.insert(instance.ee_id.clone(), instance);
    }
    while !pending.is_empty() {
        let ready = pending
            .iter()
            .filter_map(|(id, instance)| {
                instance
                    .parent_ee
                    .as_ref()
                    .is_none_or(|parent| tree.get(parent).is_some())
                    .then_some(id.clone())
            })
            .collect::<Vec<_>>();
        if ready.is_empty() {
            for instance in pending.values() {
                issues.push(format!(
                    "{source}: persisted EE instance '{}' is isolated because parent '{}' is missing, invalid, or cyclic",
                    instance.ee_id.as_str(),
                    instance
                        .parent_ee
                        .as_ref()
                        .map(|parent| parent.as_str())
                        .unwrap_or("<none>")
                ));
            }
            break;
        }
        for id in ready {
            let instance = pending
                .remove(&id)
                .expect("ready EE instance remains pending");
            if let Err(error) = tree.insert_persisted_instance(instance) {
                issues.push(format!(
                    "{source}: persisted EE instance '{id}' materialization is isolated: {error}"
                ));
            }
        }
    }

    for activation in activations {
        graph
            .validate_active_references(&activation.source, activation.references.clone())
            .map_err(|error| format!("{source}: {error}"))?;
    }

    Ok((tree, bootstrap_ee, issues))
}

pub struct SharedBlobHandle {
    blobs: SharedBlobs,
}
impl SharedBlobHandle {
    pub fn new(blobs: SharedBlobs) -> Self {
        Self { blobs }
    }
}
impl BlobStore for SharedBlobHandle {
    fn put(&mut self, bytes: &[u8]) -> ContentHash {
        self.blobs
            .lock()
            .expect("shared blob store mutex is not poisoned")
            .put(bytes)
    }
    fn put_batch(&mut self, contents: &[&[u8]]) -> Vec<ContentHash> {
        self.blobs
            .lock()
            .expect("shared blob store mutex is not poisoned")
            .put_batch(contents)
    }
    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        self.blobs
            .lock()
            .expect("shared blob store mutex is not poisoned")
            .get(hash)
    }
    fn contains(&self, hash: &ContentHash) -> bool {
        self.blobs
            .lock()
            .expect("shared blob store mutex is not poisoned")
            .contains(hash)
    }
}

pub struct EmbeddedRuntime {
    pub runtime: Runtime,
    pub arrival_dedup: Arc<std::sync::Mutex<octopus_ledger_index::ArrivalDedupIndex>>,
    pub run: RunId,
    pub epoch: GrantEpoch,
    pub registry_root: PathBuf,
    pub interface_artifact_root: PathBuf,
    pub run_records_root: PathBuf,
    pub ee_instances_root: PathBuf,
    pub blobs: SharedBlobs,
    pub interfaces_loaded: Vec<(octopus_descriptors::DescriptorName, InterfaceDescriptor)>,
    pub worker_interface_policies: BTreeMap<InterfaceId, WorkerInterfacePolicy>,
    pub ee_templates_loaded: Vec<(octopus_descriptors::DescriptorName, EeTemplateDescriptor)>,
    pub tool_descriptors_loaded: Vec<(octopus_descriptors::DescriptorName, ToolDescriptor)>,
    live_registry: Arc<RwLock<LoadedRegistrySnapshot>>,
    registry_fingerprint: Arc<Mutex<Option<RegistryDescriptorFingerprint>>>,
    registry_files: Arc<Mutex<RegistryFileCache>>,
    pub ee_tree: EeTree,
    pub existence_ee_id: EeId,
    pub formation_source_hashes: BTreeMap<DescriptorName, DescriptorHash>,
}

impl EmbeddedRuntime {
    pub fn run_records_dir(&self) -> PathBuf {
        self.run_records_root.clone()
    }

    pub fn ee_instances_dir(&self) -> PathBuf {
        self.ee_instances_root.clone()
    }

    pub fn live_ee_tree(&self) -> Result<EeTree, String> {
        self.live_registry
            .read()
            .map_err(|_| "live registry snapshot lock is poisoned".to_string())
            .map(|snapshot| snapshot.ee_tree.clone())
    }

    fn install_registry_snapshot(&self, snapshot: LoadedRegistrySnapshot) -> Result<(), String> {
        for issue in &snapshot.ee_instance_issues {
            eprintln!("EE instance isolation warning: {issue}");
        }
        let interface_classes = snapshot
            .interfaces_loaded
            .iter()
            .map(|(_name, desc)| {
                let disposition = snapshot
                    .interface_dispositions
                    .get(&desc.id)
                    .copied()
                    .ok_or_else(|| {
                        format!(
                            "interface '{}' has no formed runtime disposition",
                            desc.id.as_str()
                        )
                    })?;
                Ok((
                    desc.id.clone(),
                    InterfaceClass::new(desc.interface_type, disposition)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut live = self
            .live_registry
            .write()
            .map_err(|_| "live registry snapshot lock is poisoned".to_string())?;
        self.runtime
            .replace_interface_catalog_classes(interface_classes)?;
        *live = snapshot;
        Ok(())
    }

    pub fn reload_registry_snapshot_on_artifact_arrival(&self) -> Result<(), RegistrySnapshotError> {
        let mut current = self
            .registry_fingerprint
            .lock()
            .map_err(|_| "registry fingerprint lock is poisoned".to_string())?;
        let mut files = self.registry_files.lock()
            .map_err(|_| "registry file cache lock is poisoned".to_string())?;
        if let Some((fingerprint, snapshot)) = observe_stable_registry(
            *current,
            || registry_descriptor_fingerprint(&self.registry_root, &self.ee_instances_root, &mut files),
            || LoadedRegistrySnapshot::load(&self.registry_root, &self.ee_instances_root),
        )? {
            self.install_registry_snapshot(snapshot)?;
            *current = Some(fingerprint);
        }
        Ok(())
    }
}

pub struct SpineRootConfig {
    pub registry_root: PathBuf,
    pub run: RunId,
    pub blobs: SharedBlobs,
    pub interface_artifact_root: PathBuf,
    pub run_records_root: PathBuf,
    pub ee_instances_root: PathBuf,
}

struct RegistryVerifiedArtifactAdmission {
    registry_root: PathBuf,
    repository_root: PathBuf,
    verifier_scratch_root: PathBuf,
    blobs: SharedBlobs,
}

impl RegistryVerifiedArtifactAdmission {
    fn load_mark_descriptor(&self, mark: &MarkId) -> Result<MarkDescriptor, VerifiedArtifactError> {
        load_referenced_descriptor::<MarkDescriptor>(&self.registry_root, mark.as_str())
            .map(|loaded| loaded.descriptor)
            .map_err(|_| VerifiedArtifactError::MarkUnresolved {
                mark: mark.as_str().to_string(),
            })
    }

    fn load_verifier_descriptor(
        &self,
        required: &RequiredVerifiedArtifact,
    ) -> Result<LoadedReferencedDescriptor<VerifierDescriptor>, VerifiedArtifactError> {
        let verifier_id = &required.verifier;
        load_referenced_descriptor::<VerifierDescriptor>(&self.registry_root, verifier_id.as_str())
            .map_err(|_| VerifiedArtifactError::VerifierUnresolved {
                mark: required.mark.as_str().to_string(),
                verifier: verifier_id.as_str().to_string(),
            })
    }
}

struct LoadedReferencedDescriptor<T> {
    descriptor: T,
    canonical_bytes: Vec<u8>,
}

fn load_referenced_descriptor<T: Descriptor>(
    registry_root: &Path,
    name: &str,
) -> Result<LoadedReferencedDescriptor<T>, String> {
    if name.trim().is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(format!("invalid {} registry identity: {name:?}", T::KIND));
    }
    let file = registry_root
        .join(T::KIND)
        .join(name)
        .join(format!("{}.json", T::KIND));
    let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    let descriptor: T = parse_strict(&bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    if let Some(declared) = descriptor.declared_name() {
        if declared != name {
            return Err(format!(
                "{}: dir/body identity mismatch — directory is {:?} but descriptor declares {:?}",
                file.display(),
                name,
                declared
            ));
        }
    }
    let value = serde_json::to_value(&descriptor)
        .map_err(|error| format!("{} cannot be canonicalized: {error}", file.display()))?;
    Ok(LoadedReferencedDescriptor {
        descriptor,
        canonical_bytes: octopus_descriptors::canonical_bytes(&value),
    })
}

impl VerifiedArtifactAdmission for RegistryVerifiedArtifactAdmission {
    fn prepare_required_verified_artifact(
        &self,
        required: &RequiredVerifiedArtifact,
        input: VerificationInput,
    ) -> Result<Box<dyn PreparedVerification>, VerifiedArtifactError> {
        if required.artifact_label != input.artifact_label {
            return Err(VerifiedArtifactError::ArtifactLabelMismatch(
                required.mark.as_str().to_string(),
                input.artifact_label,
            ));
        };
        let mark_descriptor = self.load_mark_descriptor(&required.mark)?;
        let verifier_id = &required.verifier;
        let mut verifier_input = input;
        let loaded_verifier = self.load_verifier_descriptor(required)?;
        let verifier_descriptor = loaded_verifier.descriptor;
        if verifier_descriptor.default_params.is_object() {
            let default_params = verifier_descriptor.default_params.clone();
            if verifier_input.params.is_null() {
                verifier_input.params = default_params;
            } else if verifier_input.params.is_object() {
                let mut merged = default_params;
                if let (Some(target), Some(source)) =
                    (merged.as_object_mut(), verifier_input.params.as_object())
                {
                    for (key, value) in source {
                        target.insert(key.clone(), value.clone());
                    }
                }
                verifier_input.params = merged;
            }
        }
        let package_context = VerifierPackageContext {
            package_dir: Some(
                self.registry_root
                    .join(VerifierDescriptor::KIND)
                    .join(verifier_id.as_str()),
            ),
            descriptor: verifier_descriptor,
            repository_root: self.repository_root.clone(),
            scratch_root: self.verifier_scratch_root.clone(),
        };
        let pinned_package = pin_verifier_package(&package_context).map_err(|details| {
            VerifiedArtifactError::VerifierUnavailable {
                mark: required.mark.as_str().to_string(),
                verifier: verifier_id.as_str().to_string(),
                verifier_package: None,
                details,
            }
        })?;
        let (payload_bytes, verifier_package) = {
            let mut guard =
                self.blobs
                    .lock()
                    .map_err(|e| VerifiedArtifactError::VerifierUnavailable {
                        mark: required.mark.as_str().to_string(),
                        verifier: verifier_id.as_str().to_string(),
                        verifier_package: None,
                        details: serde_json::json!({
                            "error": "artifact blob store lock is poisoned during verified-artifact admission",
                            "source": e.to_string()
                        }),
                    })?;
            let verifier_package = VerifierPackagePin {
                descriptor_content: guard.put(&loaded_verifier.canonical_bytes),
                script_content: guard.put(&pinned_package.closure_bytes()),
            };
            let payload_bytes = octopus_marks::verified_artifact_payload_bytes(
                &verifier_input,
                &*guard,
                "package_process",
            )
            .map_err(|details| VerifiedArtifactError::VerifierRejected {
                mark: required.mark.as_str().to_string(),
                verifier: verifier_id.as_str().to_string(),
                artifact: verifier_input.artifact.id.as_str().to_string(),
                verifier_package: Some(verifier_package.clone()),
                details,
            })?;
            (payload_bytes, verifier_package)
        };
        Ok(Box::new(PreparedRegistryVerification {
            pinned_package,
            input: verifier_input,
            payload_bytes,
            mark: required.mark.clone(),
            mark_class: mark_descriptor.class,
            verifier: verifier_id.clone(),
            verifier_package,
        }))
    }
}

struct PreparedRegistryVerification {
    pinned_package: octopus_verifiers::PinnedVerifierPackage,
    input: VerificationInput,
    payload_bytes: Vec<u8>,
    mark: MarkId,
    mark_class: MarkClass,
    verifier: VerifierId,
    verifier_package: VerifierPackagePin,
}

impl PreparedVerification for PreparedRegistryVerification {
    fn run(self: Box<Self>) -> Result<Mark, VerifiedArtifactError> {
        let PreparedRegistryVerification {
            pinned_package,
            input,
            payload_bytes,
            mark,
            mark_class,
            verifier,
            verifier_package,
        } = *self;
        let verdict = run_pinned_verifier_process(&pinned_package, &input, payload_bytes);
        if !verdict.passed {
            return Err(VerifiedArtifactError::VerifierRejected {
                mark: mark.as_str().to_string(),
                verifier: verifier.as_str().to_string(),
                artifact: input.artifact.id.as_str().to_string(),
                verifier_package: Some(verifier_package),
                details: verdict.details,
            });
        }
        Ok(Mark::passed_with_class(
            mark,
            input.checkpoint,
            input.run,
            input.epoch,
            mark_class,
            input.artifact,
            verifier,
            verifier_package,
            verdict.details,
        ))
    }
}

pub async fn build_embedded_spine(
    mut config: SpineRootConfig,
) -> Result<EmbeddedRuntime, Box<dyn std::error::Error>> {
    config.registry_root = std::fs::canonicalize(&config.registry_root)?;
    let root = &config.registry_root;
    let packages = RegistryPackages::load(root)
        .map_err(|error| format!("loading registry packages failed: {error}"))?;
    if !packages.dangling_ceiling_refs.is_empty() {
        let workers = packages
            .dangling_ceiling_refs
            .iter()
            .map(|reference| &reference.worker)
            .collect::<BTreeSet<_>>();
        let interfaces = packages
            .dangling_ceiling_refs
            .iter()
            .map(|reference| (&reference.field, &reference.target))
            .collect::<BTreeSet<_>>();
        eprintln!(
            "policy     {} interface targets across {} worker ceilings are unavailable; requests to them will be refused. References: {}/interface/<worker>/policy.json (ceiling.interfaces)",
            interfaces.len(), workers.len(), root.display()
        );
    }
    let mut persisted_ee_instances = load_persisted_ee_instances(&config.ee_instances_root)?;
    let mut registry_files = RegistryFileCache::default();
    let boot_fingerprint =
        registry_descriptor_fingerprint(&config.registry_root, &config.ee_instances_root, &mut registry_files).ok();
    let (ee_tree, bootstrap_ee, mut topology_issues) = registry_ee_state(
        &packages.ee_templates,
        &packages.graph,
        &packages.activations,
        &persisted_ee_instances.valid,
        "embedded spine boot",
    )?;
    persisted_ee_instances.issues.append(&mut topology_issues);
    for issue in &persisted_ee_instances.issues {
        eprintln!("EE instance isolation warning: {issue}");
    }
    let interfaces_loaded = packages.interfaces.clone();
    let interface_dispositions = packages.interface_dispositions.clone();
    let worker_interface_policies = packages.worker_policies.clone();
    let tool_descriptors_loaded = packages.tools.clone();
    let ee_templates_loaded = packages.ee_templates.clone();
    let interface_catalog = RuntimeInterfaceCatalog::from_classes(
        interfaces_loaded
            .iter()
            .map(|(_name, desc)| {
                let disposition =
                    interface_dispositions
                        .get(&desc.id)
                        .copied()
                        .ok_or_else(|| {
                            format!(
                                "interface '{}' has no formed runtime disposition",
                                desc.id.as_str()
                            )
                        })?;
                Ok((
                    desc.id.clone(),
                    InterfaceClass::new(desc.interface_type, disposition)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?,
    );

    let loaded_registry_snapshot = LoadedRegistrySnapshot::from_loaded(
        interfaces_loaded.clone(),
        interface_dispositions,
        worker_interface_policies.clone(),
        tool_descriptors_loaded.clone(),
        packages.worker_environment_kinds,
        packages.graph,
        ee_tree.clone(),
        &ee_templates_loaded,
        persisted_ee_instances.issues,
    )?;
    let formation_source_hashes = loaded_registry_snapshot.formation_source_hashes.clone();
    let live_registry = Arc::new(RwLock::new(loaded_registry_snapshot));

    let run_id = config.run.clone();
    let epoch = GrantEpoch(0);

    let interface_catalog = Arc::new(interface_catalog);
    let services = RuntimeServices { interface_catalog };

    let runtime = Runtime::open(services)?;
    let registry_fingerprint = Arc::new(Mutex::new(boot_fingerprint));
    let registry_files = Arc::new(Mutex::new(registry_files));
    runtime.set_run_grant_resolver(Arc::new(RegistryRunGrantResolver {
        live_registry: live_registry.clone(),
        blobs: config.blobs.clone(),
        registry_root: config.registry_root.clone(),
        ee_instances_root: config.ee_instances_root.clone(),
        fingerprint: registry_fingerprint.clone(),
        files: registry_files.clone(),
    }));
    let repository_root = config
        .registry_root
        .parent()
        .ok_or_else(|| {
            format!(
                "registry root '{}' has no repository parent",
                config.registry_root.display()
            )
        })?
        .to_path_buf();
    runtime.set_verified_artifact_admission(Arc::new(RegistryVerifiedArtifactAdmission {
        registry_root: config.registry_root.clone(),
        repository_root,
        verifier_scratch_root: config.interface_artifact_root.join("verifier-processes"),
        blobs: config.blobs.clone(),
    }));
    runtime.set_context_artifact_resolver(Arc::new(RegistryContextArtifactCache::new(
        config.registry_root.clone(),
        config.blobs.clone(),
    )?));
    runtime.set_run_records_dir(config.run_records_root.clone());

    Ok(EmbeddedRuntime {
        runtime,
        arrival_dedup: Arc::new(std::sync::Mutex::new(
            octopus_ledger_index::ArrivalDedupIndex::new(),
        )),
        run: run_id,
        epoch,
        registry_root: config.registry_root.clone(),
        interface_artifact_root: config.interface_artifact_root.clone(),
        run_records_root: config.run_records_root,
        ee_instances_root: config.ee_instances_root,
        blobs: config.blobs.clone(),
        interfaces_loaded,
        worker_interface_policies,
        ee_templates_loaded: ee_templates_loaded.clone(),
        tool_descriptors_loaded,
        live_registry,
        registry_fingerprint,
        registry_files,
        ee_tree,
        existence_ee_id: bootstrap_ee,
        formation_source_hashes,
    })
}

pub fn context_artifact_content(artifact_json: &serde_json::Value) -> (ContentHash, Vec<u8>) {
    let bytes = octopus_descriptors::canonical_bytes(artifact_json);
    (ContentHash::of_bytes(&bytes), bytes)
}

pub fn registry_root_from_manifest(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir).join("../../registry")
}

#[cfg(test)]
#[path = "../tests/unit/formation_input_retention.rs"]
mod formation_input_retention;

#[cfg(test)]
mod required_artifact_verification_tests {
    use super::*;
    use octopus_ledger::ArtifactRef;
    use octopus_ledger::InMemoryBlobs;
    use std::sync::Mutex;

    #[tokio::test]
    async fn clean_system_baseline_boots_without_optional_worker_packages() {
        let root = tempfile::tempdir().unwrap();
        let registry = root.path().join("registry");
        for kind in [
            "artifact",
            "interface",
            "mark",
            "tool",
            "verifier",
            "worker_environment_kind",
        ] {
            std::fs::create_dir_all(registry.join(kind)).unwrap();
        }
        let template_root = registry.join("ee_template/existence");
        std::fs::create_dir_all(&template_root).unwrap();
        std::fs::write(
            template_root.join("ee_template.json"),
            br#"{"id":"existence","goal_class":"existence","allowed_child_ee_templates":[],"bootstrap_root":true}"#,
        )
        .unwrap();
        let ee_instances_root = root.path().join("state/ee_instances");
        std::fs::create_dir_all(&ee_instances_root).unwrap();
        std::fs::write(
            ee_instances_root.join("ee_existence.json"),
            serde_json::to_vec_pretty(&EeInstance {
                name: None,
                ee_id: EeId::new("ee_existence"),
                template: octopus_descriptors::EeTemplateId::new("existence"),
                parent_ee: None,
                goal: "existence".to_string(),
                config: Default::default(),
                policy_narrowing: Default::default(),
            })
            .unwrap(),
        )
        .unwrap();
        let service_template = registry.join("ee_template/service");
        std::fs::create_dir_all(&service_template).unwrap();
        std::fs::write(service_template.join("ee_template.json"),
            br#"{"id":"service","goal_class":"service","allowed_tools":[],"allowed_child_ee_templates":[]}"#).unwrap();
        std::fs::write(ee_instances_root.join("ee_service.json"),
            br#"{"ee_id":"ee_service","template_name":"service","parent_ee_id":null,"goal":"Independent service"}"#).unwrap();
        let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));

        let embedded = build_embedded_spine(SpineRootConfig {
            registry_root: registry,
            run: RunId::new("run_bootstrap"),
            blobs,
            interface_artifact_root: root.path().join("state/interface-artifacts"),
            run_records_root: root.path().join("state/runs"),
            ee_instances_root,
        })
        .await
        .unwrap();

        assert!(embedded.interfaces_loaded.is_empty());
        assert!(embedded.worker_interface_policies.is_empty());
        assert_eq!(embedded.existence_ee_id, EeId::new("ee_existence"));
        let snapshot = embedded.live_registry.read().unwrap();
        assert_eq!(snapshot.ee_tree.ancestors(&EeId::new("ee_service")).unwrap(), vec![EeId::new("ee_service")]);
        drop(snapshot);
        assert!(
            embedded
                .live_registry
                .read()
                .unwrap()
                .worker_interface_policy(&InterfaceId::new("system_worker_missing"))
                .is_err()
        );
    }

    #[test]
    fn malformed_ee_file_is_an_isolated_slot() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("ee_valid.json"),
            br#"{"ee_id":"ee_valid","template_name":"operator_interaction","parent_ee_id":"ee_existence","goal":"valid"}"#,
        )
        .unwrap();
        std::fs::write(root.path().join("ee_broken.json"), b"{").unwrap();
        let loaded = load_persisted_ee_instances(root.path()).unwrap();
        assert_eq!(loaded.valid.len(), 1);
        assert_eq!(loaded.valid[0].ee_id, EeId::new("ee_valid"));
        assert_eq!(loaded.issues.len(), 1);
        assert!(loaded.issues[0].contains("ee_broken.json"));
    }

    fn manifest_evidence(blobs: &SharedBlobs, label: &str, body: &[u8]) -> ArtifactRef {
        let root = std::env::temp_dir().join(format!(
            "octopus-verifier-evidence-{}-{}",
            std::process::id(),
            label.replace('.', "_")
        ));
        std::fs::create_dir_all(&root).expect("verifier evidence temp dir writes");
        let artifact = octopus_interfaces::InterfaceArtifactFile::write(&root, label, body)
            .expect("interface artifact manifest writes")
            .load_into_blob(&mut *blobs.lock().unwrap())
            .expect("interface artifact manifest and payload pin into blobs")
            .artifact;
        artifact
    }

    fn admission(blobs: SharedBlobs) -> RegistryVerifiedArtifactAdmission {
        let registry_root = registry_root_from_manifest(env!("CARGO_MANIFEST_DIR"));
        RegistryVerifiedArtifactAdmission {
            repository_root: registry_root
                .parent()
                .expect("registry root has a repository parent")
                .to_path_buf(),
            verifier_scratch_root: std::env::temp_dir()
                .join(format!("octopus-verifier-processes-{}", std::process::id())),
            registry_root,
            blobs,
        }
    }

    #[test]
    fn context_artifact_resolver_derives_content_from_id_alone() {
        let registry_root = registry_root_from_manifest(env!("CARGO_MANIFEST_DIR"));
        let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
        let resolver = RegistryContextArtifactCache::new(registry_root.clone(), blobs.clone())
            .expect("context artifact cache opens");

        let role_json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                registry_root
                    .join("artifact")
                    .join("artifact-worker-role-template")
                    .join("artifact.json"),
            )
            .expect("the role context artifact is on disk"),
        )
        .expect("the role context artifact is JSON");
        let (expected, _bytes) = context_artifact_content(&role_json);
        assert_eq!(
            resolver
                .resolve(&ArtifactId::new("artifact-worker-role-template"))
                .expect("a registry context artifact resolves by id"),
            expected,
            "the resolver derives the canonical registry content hash from the id alone"
        );
        assert!(blobs.lock().unwrap().contains(&expected));

        let content = blobs.lock().unwrap().put(
            br#"{"label":"interface.output","payload_path":"/var/octopus/interface-artifacts/interface.output.payload"}"#,
        );
        let id = ArtifactId::new(format!("interface.output:{}", content.to_hex()));
        assert_eq!(
            resolver
                .resolve(&id)
                .expect("a content-addressed id resolves from its hex suffix"),
            content,
            "the resolver recovers the exact content hash encoded in the id"
        );

        assert!(
            resolver
                .resolve(&ArtifactId::new("artifact-does-not-exist"))
                .is_err(),
            "an unresolvable binding fails closed rather than inventing a hash"
        );
    }

    #[test]
    fn required_verified_artifact_runs_build_success_verifier() {
        let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
        let pass = manifest_evidence(
            &blobs,
            "coder.build.result",
            br#"{"label":"coder.build.result","tool":"coder.build","ok":true,"body":{"result":"command_finished","output":{"exit_code":0,"stdout":[],"stderr":[]}}}"#,
        );
        let wrong_tool = manifest_evidence(
            &blobs,
            "coder.build.result",
            br#"{"label":"coder.exec.result","tool":"coder.exec","ok":true,"body":{"result":"command_finished","output":{"exit_code":0,"stdout":[],"stderr":[]}}}"#,
        );
        let fail = manifest_evidence(
            &blobs,
            "coder.build.result",
            br#"{"label":"coder.build.result","tool":"coder.build","ok":false,"error":{"code":"tool_execution_failed","message":"build exited unsuccessfully","details":{"exit_code":1}}}"#,
        );

        let admission = admission(blobs.clone());
        let required = RequiredVerifiedArtifact {
            mark: MarkId::new("coder.build.ok"),
            artifact_label: "coder.build.result".into(),
            verifier: octopus_descriptors::VerifierId::new("build_success"),
            params: serde_json::json!({}),
        };
        let input = |artifact: ArtifactRef| VerificationInput {
            run: RunId::new("run"),
            checkpoint: octopus_descriptors::CheckpointId::new("checkpoint"),
            epoch: 7,
            mark: required.mark.clone(),
            artifact,
            artifact_label: "coder.build.result".into(),
            params: serde_json::json!({}),
            metadata: serde_json::json!({}),
        };
        let mark = admission
            .admit_required_verified_artifact(&required, input(pass))
            .expect("exit 0 build evidence passes");
        assert_eq!(mark.name, MarkId::new("coder.build.ok"));
        assert_eq!(mark.run_epoch, 7);
        assert!(matches!(
            admission.admit_required_verified_artifact(&required, input(fail)),
            Err(VerifiedArtifactError::VerifierRejected { .. })
        ));
        assert!(matches!(
            admission.admit_required_verified_artifact(&required, input(wrong_tool)),
            Err(VerifiedArtifactError::VerifierRejected { .. })
        ));
    }

    #[test]
    fn required_verified_artifact_passes_ocr_params_from_run_json() {
        let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
        let artifact = manifest_evidence(
            &blobs,
            "ocr.text.result",
            br#"{"ocr_text":"alpha Needle omega"}"#,
        );
        let admission = admission(blobs.clone());
        let required = RequiredVerifiedArtifact {
            mark: MarkId::new("ocr.text.contains"),
            artifact_label: "ocr.text.result".into(),
            verifier: octopus_descriptors::VerifierId::new("ocr_text_contains"),
            params: serde_json::json!({"expected_text":"needle"}),
        };
        let input = VerificationInput {
            run: RunId::new("run"),
            checkpoint: octopus_descriptors::CheckpointId::new("checkpoint"),
            epoch: 9,
            mark: required.mark.clone(),
            artifact,
            artifact_label: "ocr.text.result".into(),
            params: required.params.clone(),
            metadata: serde_json::json!({}),
        };
        let mark = admission
            .admit_required_verified_artifact(&required, input)
            .expect("ocr text containing expected_text passes");
        assert_eq!(mark.name, MarkId::new("ocr.text.contains"));
        assert!(
            mark.details
                .get("matched")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        );
    }

    #[test]
    fn required_verified_artifact_runs_context_recipe_verifier_over_manifest_payload() {
        let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
        let recipe = manifest_evidence(
            &blobs,
            "context.recipe",
            br#"---
name: coder-grounding
description: Grounding context for a coder run.
---

# Body
"#,
        );
        let malformed = manifest_evidence(
            &blobs,
            "context.recipe",
            br#"---
name: ""
description: ""
---

# Body
"#,
        );
        let admission = admission(blobs.clone());
        let required = RequiredVerifiedArtifact {
            mark: MarkId::new("context.recipe.valid"),
            artifact_label: "context.recipe".into(),
            verifier: octopus_descriptors::VerifierId::new("context_recipe"),
            params: serde_json::json!({}),
        };
        let input = |artifact: ArtifactRef| VerificationInput {
            run: RunId::new("run"),
            checkpoint: octopus_descriptors::CheckpointId::new("checkpoint"),
            epoch: 11,
            mark: required.mark.clone(),
            artifact,
            artifact_label: "context.recipe".into(),
            params: serde_json::json!({}),
            metadata: serde_json::json!({}),
        };
        let mark = admission
            .admit_required_verified_artifact(&required, input(recipe))
            .expect("context recipe manifest payload passes verifier");
        assert_eq!(mark.name, MarkId::new("context.recipe.valid"));
        assert_eq!(mark.run_epoch, 11);
        assert!(matches!(
            admission.admit_required_verified_artifact(&required, input(malformed)),
            Err(VerifiedArtifactError::VerifierRejected { .. })
        ));
    }

    fn program_runs(program: &str, args: &[&str]) -> bool {
        std::process::Command::new(program)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn package_script_verifier_validates_payloads_without_a_rust_branch() {
        if !program_runs("sh", &["-c", "exit 0"]) || !program_runs("python3", &["--version"]) {
            eprintln!("skipping: sh/python3 unavailable for package_script admission proof");
            return;
        }
        let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
        let admission = admission(blobs.clone());

        let valid_json = manifest_evidence(&blobs, "payload.candidate", br#"{"ok":true}"#);
        let not_json = manifest_evidence(&blobs, "payload.candidate", b"this is not json at all");
        let json_required = RequiredVerifiedArtifact {
            mark: MarkId::new("context.recipe.valid"),
            artifact_label: "payload.candidate".into(),
            verifier: octopus_descriptors::VerifierId::new("payload_json_valid"),
            params: serde_json::json!({}),
        };
        let json_input = |artifact: ArtifactRef| VerificationInput {
            run: RunId::new("run"),
            checkpoint: octopus_descriptors::CheckpointId::new("checkpoint"),
            epoch: 12,
            mark: json_required.mark.clone(),
            artifact,
            artifact_label: "payload.candidate".into(),
            params: serde_json::json!({}),
            metadata: serde_json::json!({}),
        };
        let mark = admission
            .admit_required_verified_artifact(&json_required, json_input(valid_json))
            .expect("valid-JSON payload passes the package script");
        assert_eq!(mark.name, MarkId::new("context.recipe.valid"));
        assert_eq!(
            mark.details
                .get("handler")
                .and_then(serde_json::Value::as_str),
            Some("payload_json_valid")
        );
        assert!(matches!(
            admission.admit_required_verified_artifact(&json_required, json_input(not_json)),
            Err(VerifiedArtifactError::VerifierRejected { .. })
        ));
    }

    #[test]
    fn package_script_verifier_fails_closed_on_missing_package_and_malformed_output() {
        if !program_runs("sh", &["-c", "exit 0"]) {
            eprintln!("skipping: sh unavailable for package_script fail-closed proof");
            return;
        }
        let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));

        let missing_required = RequiredVerifiedArtifact {
            mark: MarkId::new("context.recipe.valid"),
            artifact_label: "payload.candidate".into(),
            verifier: octopus_descriptors::VerifierId::new("payload_json_valid_absent"),
            params: serde_json::json!({}),
        };
        let candidate = manifest_evidence(&blobs, "payload.candidate", br#"{"ok":true}"#);
        let missing_input = VerificationInput {
            run: RunId::new("run"),
            checkpoint: octopus_descriptors::CheckpointId::new("checkpoint"),
            epoch: 14,
            mark: missing_required.mark.clone(),
            artifact: candidate,
            artifact_label: "payload.candidate".into(),
            params: serde_json::json!({}),
            metadata: serde_json::json!({}),
        };
        assert!(matches!(
            admission(blobs.clone())
                .admit_required_verified_artifact(&missing_required, missing_input),
            Err(VerifiedArtifactError::VerifierUnresolved { .. })
        ));

        let scratch_registry = std::env::temp_dir().join(format!(
            "octopus-package-script-registry-{}-{}",
            std::process::id(),
            line!()
        ));
        let verifier_dir = scratch_registry.join("verifier").join("malformed_probe");
        std::fs::create_dir_all(&verifier_dir).expect("scratch verifier package dir writes");
        std::fs::write(
            verifier_dir.join("verifier.json"),
            br#"{"id":"malformed_probe","package":"malformed_probe","script":"verify.sh","authority":{"read_root_slots":[],"write_root_slots":[],"executable_root_slots":[],"read_paths":[],"write_paths":[],"executable_paths":[],"executable_commands":["/bin/sh"],"read_env_paths":[],"write_env_paths":[],"executable_env_paths":[],"allow_network":false,"allow_local_ipc":false},"default_params":{}}"#,
        )
        .expect("scratch verifier descriptor writes");
        let script_path = verifier_dir.join("verify.sh");
        std::fs::write(&script_path, b"#!/bin/sh\nprintf 'not a verdict at all'\n")
            .expect("scratch verifier script writes");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&script_path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script_path, permissions).unwrap();
        }
        std::fs::create_dir_all(scratch_registry.join("mark").join("context.recipe.valid"))
            .expect("scratch mark dir writes");
        std::fs::write(
            scratch_registry
                .join("mark")
                .join("context.recipe.valid")
                .join("mark.json"),
            br#"{"id":"context.recipe.valid","package":"context.recipe.valid","class":"success"}"#,
        )
        .expect("scratch mark descriptor writes");

        let scratch_admission = RegistryVerifiedArtifactAdmission {
            registry_root: scratch_registry.clone(),
            repository_root: scratch_registry
                .parent()
                .expect("scratch registry has a parent")
                .to_path_buf(),
            verifier_scratch_root: scratch_registry.join("verifier-processes"),
            blobs: blobs.clone(),
        };
        let malformed_required = RequiredVerifiedArtifact {
            mark: MarkId::new("context.recipe.valid"),
            artifact_label: "payload.candidate".into(),
            verifier: octopus_descriptors::VerifierId::new("malformed_probe"),
            params: serde_json::json!({}),
        };
        let malformed_candidate = manifest_evidence(&blobs, "payload.candidate", b"anything");
        let malformed_input = VerificationInput {
            run: RunId::new("run"),
            checkpoint: octopus_descriptors::CheckpointId::new("checkpoint"),
            epoch: 15,
            mark: malformed_required.mark.clone(),
            artifact: malformed_candidate,
            artifact_label: "payload.candidate".into(),
            params: serde_json::json!({}),
            metadata: serde_json::json!({}),
        };
        let verdict = scratch_admission
            .admit_required_verified_artifact(&malformed_required, malformed_input);
        let _ = std::fs::remove_dir_all(&scratch_registry);
        assert!(matches!(
            verdict,
            Err(VerifiedArtifactError::VerifierRejected { .. })
        ));
    }

    #[test]
    fn context_artifact_resolver_materializes_activation_role_artifacts_on_demand() {
        let scratch = tempfile::tempdir().expect("scratch registry opens");
        let registry_root = scratch.path().to_path_buf();
        let interface = InterfaceId::new("world_probe_trigger");
        let role_id = "artifact-probe-role";
        let interface_dir = registry_root.join("interface").join(interface.as_str());
        std::fs::create_dir_all(&interface_dir).expect("probe interface folder writes");
        let checkpoint = serde_json::json!({
            "id": "cp_probe_in",
            "run": "run_probe",
            "epoch": 0,
            "interface_binding": {
                "interface": interface.as_str(),
                "config": {"artifact_policy": {"mode": "all"}}
            },
            "outbound_interface_bindings": [],
            "artifact_bindings": [{"id": role_id}],
            "required_verified_artifacts": [],
            "trace": "trace:run_probe:cp_probe_in"
        });
        std::fs::write(
            interface_dir.join("activation.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "interface": interface.as_str(),
                "activations": [{
                    "enabled": false,
                    "activation_driver_ref": "probe_life",
                    "config": {
                        "run_template": {
                            "id": "run_probe",
                            "ee": "ee_probe",
                            "worker": "system_worker_probe",
                            "trace": "trace:run_probe",
                            "status": "idle",
                            "checkpoints": [checkpoint],
                            "active_checkpoints": ["cp_probe_in"]
                        }
                    }
                }]
            }))
            .expect("probe activation encodes"),
        )
        .expect("probe activation writes");
        let role_dir = registry_root.join("artifact").join(role_id);
        std::fs::create_dir_all(&role_dir).expect("probe role folder writes");
        std::fs::write(
            role_dir.join("artifact.json"),
            serde_json::to_vec(&serde_json::json!({
                "label": "context.reference",
                "id": role_id,
                "text": "Probe role."
            }))
            .expect("probe role encodes"),
        )
        .expect("probe role writes");
        let activation = octopus_interface_projection::interface_activation::load_interface_activation_file_for_interface(
            &registry_root,
            &interface,
        )
        .expect("probe activation loads");
        let mut ids: BTreeSet<String> = BTreeSet::new();
        for entry in &activation.descriptor.activations {
            let Some(value) = entry.config.get("run_template") else {
                continue;
            };
            let bytes = serde_json::to_vec(value).expect("interface-owned run template encodes");
            let record = octopus_run_record::RunRecord::from_bytes(&bytes)
                .expect("interface-owned run template parses through the canonical run contract");
            for checkpoint in &record.checkpoints {
                for binding in &checkpoint.artifact_bindings {
                    ids.insert(binding.id.as_str().to_string());
                }
            }
        }
        assert!(
            ids.contains(role_id),
            "the activation run template must bind its role artifact; got {ids:?}"
        );

        let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
        let resolver = RegistryContextArtifactCache::new(registry_root.clone(), blobs.clone())
            .expect("context artifact cache opens");
        let artifact_json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(role_dir.join("artifact.json")).expect("probe role reads"),
        )
        .expect("probe role parses");
        let (hash, _bytes) = context_artifact_content(&artifact_json);
        assert_eq!(
            resolver
                .resolve(&ArtifactId::new(role_id))
                .expect("activation role artifact resolves on demand"),
            hash
        );
        assert!(
            blobs.lock().unwrap().get(&hash).is_some(),
            "the seeded blob is retrievable at the pinned content hash"
        );
    }
}
