#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

pub use tokio_util::sync::CancellationToken;

use octopus_descriptors as desc;
use octopus_interfaces as iface;
use octopus_interfaces::RefusedReason;
use octopus_ledger as ledger;
use octopus_marks as marks;
use octopus_policy as policy;

pub use desc::{ArtifactId, CheckpointId, InterfaceId, RunId};
pub use ledger::{
    ArtifactRef, ContentHash, Endpoint, Lamport, Movement, MovementDraft, MovementKind, Seq,
    TraceId,
};
pub use marks::{
    FailClosedVerifiedArtifactAdmission, Mark, MarkClass, PreparedVerification, ReadyVerification,
    RequiredVerifiedArtifact, VerificationInput, VerifiedArtifactAdmission, VerifiedArtifactError,
};
pub use policy::{
    ArtifactAccess, ArtifactVerb, AuthoredInterfaceBinding, BindingArtifactPolicy, CapSet,
    ContextGrant, DenySet, FormationPin, FormedPolicy, InterfaceBindingConfig, PolicyLayer,
    form_policy,
};

pub use desc::MarkId;

use octopus_run_record::{
    ArtifactBinding, CheckpointRecord, GrantEpoch, RunRecord, RunRecordSnapshot, RunRecordsIndex,
};

#[derive(Debug)]
pub struct Observation {
    pub ok: bool,
    pub data: serde_json::Value,
    pub invocation_metadata: iface::InvocationMetadata,
    pub admitted_artifact_bindings: Vec<ArtifactRef>,
    pub outbound_artifact_transfers: Vec<InterfaceOutboundArtifactTransfer>,
    pub refused_outbound_artifact_transfers: Vec<InterfaceOutboundArtifactRefusal>,
    pub admitted_mark_outcomes: Vec<Mark>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceOutboundArtifactTransfer {
    pub run: RunId,
    pub checkpoint: CheckpointId,
    pub artifact: ArtifactRef,
    pub artifact_bindings: Vec<ArtifactRef>,
    pub from_interface: InterfaceId,
    pub to_interface: InterfaceId,
    pub invocation_metadata: iface::InvocationMetadata,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceOutboundArtifactRefusal {
    pub run: RunId,
    pub checkpoint: CheckpointId,
    pub artifact: ArtifactRef,
    pub from_interface: InterfaceId,
    pub to_interface: InterfaceId,
    pub invocation_metadata: iface::InvocationMetadata,
}

#[derive(Clone)]
pub struct InterfaceArtifactAdmission {
    pub run: RunId,
    pub checkpoint: Option<CheckpointId>,
    pub epoch: GrantEpoch,
    pub interface: InterfaceId,
    pub artifact: ArtifactRef,
    pub artifact_label: String,
    pub run_record_hash: Option<ContentHash>,
}

#[derive(Clone, Debug)]
pub struct InterfaceArrivalTarget {
    pub run: RunId,
    pub checkpoint: CheckpointId,
    pub ee: desc::EeId,
    pub worker: InterfaceId,
    pub epoch: GrantEpoch,
    pub run_record: RunRecordSnapshot,
    pub run_trace: TraceId,
    pub checkpoint_trace: TraceId,
    pub inbound_binding: AuthoredInterfaceBinding,
    pub outbound_bindings: Vec<AuthoredInterfaceBinding>,
    arriving_interface: InterfaceId,
    source_movement_seq: Option<u64>,
    pub delivery_scope_seq: Option<u64>,
}

impl InterfaceArrivalTarget {
    pub fn with_source_movement_seq(mut self, seq: u64) -> Self {
        self.source_movement_seq = Some(seq);
        self
    }

    pub fn from_retained_snapshot(coordinates: &octopus_ledger::RuntimeInvocationMetadata, snapshot: RunRecordSnapshot, scope_seq: u64) -> Result<Self, String> {
        if ContentHash::of_bytes(&snapshot.bytes) != snapshot.hash
            || coordinates.run_record_hash.as_deref() != Some(snapshot.hash.to_hex().as_str()) {
            return Err("retained arrival snapshot differs from its captured source".into());
        }
        let record = RunRecord::from_bytes(&snapshot.bytes).map_err(|error| error.to_string())?;
        let checkpoint = record.checkpoint(&coordinates.checkpoint).ok_or("retained arrival checkpoint is absent")?;
        if record.id != coordinates.run || record.ee != coordinates.ee || record.worker != coordinates.worker
            || !record.active_checkpoints.contains(&coordinates.checkpoint) || checkpoint.epoch.0 != coordinates.epoch
            || checkpoint.inbound_interface() != Some(&coordinates.interface) {
            return Err("retained arrival coordinates differ from the pinned active checkpoint".into());
        }
        Ok(Self {
            run: record.id.clone(), checkpoint: checkpoint.id.clone(), ee: record.ee.clone(), worker: record.worker.clone(),
            epoch: checkpoint.epoch, run_trace: record.trace.clone(), checkpoint_trace: checkpoint.trace.clone(),
            inbound_binding: checkpoint.interface_binding.clone().ok_or("retained arrival has no interface binding")?,
            outbound_bindings: checkpoint.outbound_interface_bindings.clone(), run_record: snapshot,
            arriving_interface: coordinates.interface.clone(), source_movement_seq: None, delivery_scope_seq: Some(scope_seq),
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SystemInterfaceEmitter<'a> {
    pub run: &'a RunId,
    pub checkpoint: &'a CheckpointId,
    pub epoch: GrantEpoch,
}

#[derive(Clone, Debug)]
pub struct SystemInterfaceEmissionAuthority {
    pub grant: Arc<FormedPolicy>,
    pub metadata_projection: iface::GrantProjection,
    pub ee: desc::EeId,
    pub worker: InterfaceId,
    pub epoch: GrantEpoch,
    pub checkpoint_binding_policy: iface::CheckpointBindingPolicySnapshot,
    pub run_record: RunRecordSnapshot,
}

pub struct RunGrantQuery<'a> {
    pub run: &'a RunId,
    pub checkpoint: &'a CheckpointId,
    pub ee: &'a desc::EeId,
    pub worker: &'a InterfaceId,
    pub epoch: GrantEpoch,
    pub authored_interfaces: &'a [InterfaceId],
    pub artifact_bindings: &'a [ArtifactId],
    /// POLICY.md §4/§5 — the run's authored object-level artifact rights, the
    /// bindings leg of `grant = ceiling ∩ EE chain ∩ authored bindings`. Formation
    /// masks each entry down to the composed artifact-verb ceiling.
    pub artifact_access: &'a BTreeMap<ArtifactId, policy::ArtifactAccess>,
    pub required_verified_artifacts: &'a [RequiredVerifiedArtifact],
}

#[derive(Clone, Debug)]
pub struct RunGrantResolution {
    grant: FormedPolicy,
    metadata_projection: iface::GrantProjection,
}

impl RunGrantResolution {
    pub fn new(
        grant: FormedPolicy,
        metadata_projection: iface::GrantProjection,
    ) -> Result<Self, String> {
        let formed_projection = iface::GrantProjection::from_formed(&grant);
        if metadata_projection.formation_pin != formed_projection.formation_pin
            || metadata_projection.tools != formed_projection.tools
            || metadata_projection.hard_blocked_tools != formed_projection.hard_blocked_tools
            || metadata_projection.interfaces != formed_projection.interfaces
            || metadata_projection.worker_environment_kinds
                != formed_projection.worker_environment_kinds
            || metadata_projection.mark_outcomes != formed_projection.mark_outcomes
            || metadata_projection.interface_policy != formed_projection.interface_policy
            || metadata_projection.artifact_verbs != formed_projection.artifact_verbs
            || metadata_projection.artifact_access != formed_projection.artifact_access
        {
            return Err(
                "run grant metadata projection does not match the formed enforcement axes"
                    .to_string(),
            );
        }
        Ok(Self {
            grant,
            metadata_projection,
        })
    }

    pub fn grant(&self) -> &FormedPolicy {
        &self.grant
    }

    pub fn metadata_projection(&self) -> &iface::GrantProjection {
        &self.metadata_projection
    }

    fn into_parts(self) -> (FormedPolicy, iface::GrantProjection) {
        (self.grant, self.metadata_projection)
    }
}

pub trait RunGrantResolver: Send + Sync {
    fn resolve_run_grant(&self, query: RunGrantQuery<'_>) -> Result<RunGrantResolution, String>;

    fn acquire_commit_guard(&self, _record: &RunRecord, _marks: &[Mark]) -> Result<Option<Box<dyn AdmissionSourceGuard>>, String> {
        Ok(None)
    }
}

pub trait AdmissionSourceGuard: Send {
    fn validate(&self) -> Result<(), String>;
}

pub trait ContextArtifactResolver: Send + Sync {
    fn resolve_artifact_content(&self, id: &ArtifactId) -> Result<ContentHash, String>;
}

pub use iface::{InterfaceRuntimeDisposition, InterfaceType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceClass {
    interface_type: InterfaceType,
    disposition: InterfaceRuntimeDisposition,
}

impl InterfaceClass {
    pub fn new(
        interface_type: InterfaceType,
        disposition: InterfaceRuntimeDisposition,
    ) -> Result<Self, String> {
        if disposition == InterfaceRuntimeDisposition::Inert {
            return Err("an inert interface cannot enter the runtime catalog".to_string());
        }
        if disposition == InterfaceRuntimeDisposition::RuntimeRelay
            && interface_type != InterfaceType::System
        {
            return Err("a runtime relay must be a System interface".to_string());
        }
        Ok(Self {
            interface_type,
            disposition,
        })
    }

    pub fn interface_type(self) -> InterfaceType {
        self.interface_type
    }

    pub fn disposition(self) -> InterfaceRuntimeDisposition {
        self.disposition
    }
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeInterfaceCatalog {
    classes: Arc<RwLock<BTreeMap<InterfaceId, InterfaceClass>>>,
}

impl RuntimeInterfaceCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_classes(entries: impl IntoIterator<Item = (InterfaceId, InterfaceClass)>) -> Self {
        Self {
            classes: Arc::new(RwLock::new(entries.into_iter().collect())),
        }
    }

    pub fn insert_class(&mut self, id: InterfaceId, class: InterfaceClass) {
        self.classes.write().unwrap().insert(id, class);
    }

    pub fn replace_classes(
        &self,
        entries: impl IntoIterator<Item = (InterfaceId, InterfaceClass)>,
    ) -> Result<(), String> {
        *self
            .classes
            .write()
            .map_err(|_| "runtime interface catalog lock is poisoned".to_string())? =
            entries.into_iter().collect();
        Ok(())
    }

    pub fn contains(&self, id: &InterfaceId) -> bool {
        self.classes
            .read()
            .is_ok_and(|classes| classes.contains_key(id))
    }

    pub fn interface_class(&self, id: &InterfaceId) -> Option<InterfaceClass> {
        self.classes
            .read()
            .ok()
            .and_then(|classes| classes.get(id).copied())
    }

    pub fn interface_type(&self, id: &InterfaceId) -> Option<InterfaceType> {
        self.interface_class(id).map(InterfaceClass::interface_type)
    }

    pub fn is_system(&self, id: &InterfaceId) -> bool {
        matches!(self.interface_type(id), Some(InterfaceType::System))
    }

    pub fn is_runtime_relay(&self, id: &InterfaceId) -> bool {
        self.interface_class(id)
            .is_some_and(|class| class.disposition() == InterfaceRuntimeDisposition::RuntimeRelay)
    }

    pub fn is_empty(&self) -> bool {
        self.classes.read().is_ok_and(|classes| classes.is_empty())
    }
}

#[derive(Debug)]
struct RunRecordAdmission {
    ee: desc::EeId,
    worker: InterfaceId,
    grant: Arc<FormedPolicy>,
    metadata_projection: iface::GrantProjection,
    artifact_bindings: Vec<ArtifactRef>,
    required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
    inbound_binding: AuthoredInterfaceBinding,
    outbound_bindings: Vec<AuthoredInterfaceBinding>,
}

struct ResolvedRunGrant {
    grant: Arc<FormedPolicy>,
    metadata_projection: iface::GrantProjection,
}

struct RunAuthority {
    grant: Arc<FormedPolicy>,
    epoch: GrantEpoch,
}

struct RuntimeGate {
    authorities: Mutex<HashMap<RunId, RunAuthority>>,
    admitted: Mutex<Vec<Mark>>,
    verified_artifact_admission: RwLock<Arc<dyn VerifiedArtifactAdmission + Send + Sync>>,
}

fn ensure_interface_in_grant(
    grant: &FormedPolicy,
    run: &RunId,
    checkpoint: Option<&CheckpointId>,
    interface: &InterfaceId,
    direction: &str,
) -> Result<(), RuntimeError> {
    if grant.context.interfaces.contains(interface) {
        return Ok(());
    }
    let checkpoint = checkpoint
        .map(|id| format!(" checkpoint '{}'", id.as_str()))
        .unwrap_or_default();
    Err(RuntimeError::DescriptorDrift {
        reason: RefusedReason::NoBinding,
        detail: format!(
            "run '{}'{} has no formed grant for {direction} interface '{}'; the interface folder is the only binding identity, and it is not in the run's formed interface cap",
            run.as_str(),
            checkpoint,
            interface.as_str()
        ),
    })
}

impl RuntimeGate {
    pub fn set_authority(&self, run: RunId, grant: Arc<FormedPolicy>, epoch: GrantEpoch) {
        self.authorities
            .lock()
            .unwrap()
            .insert(run, RunAuthority { grant, epoch });
    }

    pub fn run_authority_pin(&self, run: &RunId) -> Option<(Arc<FormedPolicy>, GrantEpoch)> {
        self.authorities
            .lock()
            .unwrap()
            .get(run)
            .map(|a| (a.grant.clone(), a.epoch))
    }

    pub fn admitted_mark_outcomes(&self) -> Vec<Mark> {
        self.admitted.lock().unwrap().clone()
    }

    fn set_verified_artifact_admission(
        &self,
        admission: Arc<dyn VerifiedArtifactAdmission + Send + Sync>,
    ) {
        *self.verified_artifact_admission.write().unwrap() = admission;
    }

    async fn admit_interface_boundary(
        &self,
        inv: InterfaceArtifactAdmission,
        ee: desc::EeId,
        worker: InterfaceId,
        grant: Arc<FormedPolicy>,
        metadata_projection: iface::GrantProjection,
        admitted_artifact_bindings: Vec<ArtifactRef>,
        required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
        inbound_interface_binding: AuthoredInterfaceBinding,
        outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
        checkpoint_admission: iface::CheckpointAdmissionMetadata,
        source_movement_seq: Option<u64>,
        delivery_scope_seq: Option<u64>,
        pending: Option<&Movement>,
    ) -> Result<Observation, RuntimeError> {
        let admitted_checkpoint = inv.checkpoint.clone().ok_or_else(|| {
            RuntimeError::InterfaceBoundaryFailed(format!(
                "runtime admitted interface '{}' artifact '{}' without a run-record checkpoint",
                inv.interface.as_str(),
                inv.artifact.id.as_str()
            ))
        })?;
        let run_record_hash = inv.run_record_hash.ok_or_else(|| {
            RuntimeError::InterfaceBoundaryFailed(format!(
                "runtime admitted interface '{}' artifact '{}' without pinning the exact run-record bytes",
                inv.interface.as_str(),
                inv.artifact.id.as_str()
            ))
        })?;

        ensure_interface_in_grant(
            grant.as_ref(),
            &inv.run,
            inv.checkpoint.as_ref(),
            &inv.interface,
            "inbound",
        )?;

        if inbound_interface_binding.interface != inv.interface {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::NoBinding,
                detail: format!(
                    "run '{}' checkpoint '{}' pins inbound interface '{}', but admission arrived from '{}'",
                    inv.run.as_str(),
                    admitted_checkpoint.as_str(),
                    inbound_interface_binding.interface.as_str(),
                    inv.interface.as_str()
                ),
            });
        }
        if !inbound_interface_binding.admits(&inv.artifact_label) {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: format!(
                    "policy.interface_binding_denied: run '{}' checkpoint '{}' inbound binding '{}' denies artifact name '{}'",
                    inv.run.as_str(),
                    admitted_checkpoint.as_str(),
                    inv.interface.as_str(),
                    inv.artifact_label
                ),
            });
        }

        let outbound_interface_ids = outbound_interface_bindings
            .iter()
            .map(|binding| binding.interface.clone())
            .collect::<Vec<_>>();

        let runtime_metadata = iface::RuntimeInvocationMetadata {
            run: inv.run.clone(),
            ee,
            worker,
            checkpoint: admitted_checkpoint.clone(),
            interface: inv.interface.clone(),
            epoch: inv.epoch.0,
            run_record_hash: Some(run_record_hash.to_hex()),
        };
        let mut invocation_metadata = iface::InvocationMetadata::new(metadata_projection)
            .with_runtime(runtime_metadata)
            .with_outbound_interface_bindings(outbound_interface_ids)
            .with_checkpoint_binding_policy(iface::CheckpointBindingPolicySnapshot {
                inbound: inbound_interface_binding,
                outbound: outbound_interface_bindings,
            })
            .with_required_verified_artifacts(required_verified_artifacts.clone())
            .with_artifact_binding_refs(admitted_artifact_bindings.clone())
            .with_checkpoint_admission(checkpoint_admission);
        invocation_metadata.source_movement_seq = source_movement_seq;
        invocation_metadata.delivery_scope_seq = delivery_scope_seq;
        if let Some(pending) = pending {
            let prior = pending.invocation_metadata.as_ref().and_then(ledger::MovementMetadata::invocation)
                .ok_or_else(|| RuntimeError::InterfaceBoundaryFailed("pending arrival has no invocation metadata".into()))?;
            if pending.kind != MovementKind::InterfaceArrival
                || pending.artifact.as_ref() != Some(&inv.artifact)
                || pending.from != Endpoint::Interface(inv.interface.clone())
                || pending.to != Endpoint::Interface(invocation_metadata.runtime.as_ref().unwrap().worker.clone())
                || !invocation_metadata.retains_required_admission_contract(prior)
            {
                return Err(RuntimeError::DescriptorDrift { reason: RefusedReason::StaleEpoch,
                    detail: "pending arrival changed its exact artifact or required checkpoint contract".into() });
            }
            invocation_metadata.replaces_arrival_seq = prior.replaces_arrival_seq;
            if invocation_metadata != *prior {
                invocation_metadata.replaces_arrival_seq = Some(pending.seq.0);
            }
        }
        let admitted_mark_outcomes = self
            .admit_required_verified_artifacts(
                &inv,
                &admitted_checkpoint,
                inv.epoch,
                grant.as_ref(),
                &required_verified_artifacts,
                invocation_metadata.clone(),
            )
            .await?;
        Ok(Observation {
            ok: true,
            data: serde_json::json!({
                "admitted": true,
                "metadata": invocation_metadata,
                "admitted_mark_outcomes": admitted_mark_outcomes,
            }),
            invocation_metadata,
            admitted_artifact_bindings,
            outbound_artifact_transfers: Vec::new(),
            refused_outbound_artifact_transfers: Vec::new(),
            admitted_mark_outcomes,
        })
    }

    async fn admit_required_verified_artifacts(
        &self,
        inv: &InterfaceArtifactAdmission,
        checkpoint: &CheckpointId,
        epoch: GrantEpoch,
        grant: &FormedPolicy,
        required_verified_artifacts: &[RequiredVerifiedArtifact],
        invocation_metadata: iface::InvocationMetadata,
    ) -> Result<Vec<Mark>, RuntimeError> {
        if required_verified_artifacts.is_empty() {
            return Ok(Vec::new());
        }
        let admission = self
            .verified_artifact_admission
            .read()
            .map_err(|_| RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: "required verified artifact admission lock is poisoned".to_string(),
            })?
            .clone();
        for required in required_verified_artifacts {
            if !grant.context.mark_outcomes.contains(&required.mark) {
                return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                    "run '{}' checkpoint '{}' requires verified artifact mark '{}', but the formed grant does not authorize that mark",
                    inv.run.as_str(),
                    checkpoint.as_str(),
                    required.mark.as_str()
                )));
            }
        }
        let matching_required: Vec<&RequiredVerifiedArtifact> = required_verified_artifacts
            .iter()
            .filter(|required| required.artifact_label == inv.artifact_label)
            .collect();
        if matching_required.is_empty() {
            return Err(RuntimeError::VerifiedArtifactRejected {
                error: VerifiedArtifactError::NoRequirementForLabel {
                    run: inv.run.as_str().to_string(),
                    checkpoint: checkpoint.as_str().to_string(),
                    artifact_label: inv.artifact_label.clone(),
                },
                invocation_metadata: Box::new(invocation_metadata),
            });
        }
        if matching_required.len() > 1 {
            return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                "run '{}' checkpoint '{}' has multiple required_verified_artifacts for artifact label '{}'; one artifact edge cannot close multiple verified-artifact requirements",
                inv.run.as_str(),
                checkpoint.as_str(),
                inv.artifact_label
            )));
        }
        let required = matching_required[0];
        let metadata = serde_json::to_value(&invocation_metadata).map_err(|error| {
            RuntimeError::InterfaceBoundaryFailed(format!(
                "invocation metadata failed to encode required_verified_artifacts on interface '{}': {error}",
                inv.interface.as_str()
            ))
        })?;
        let input = VerificationInput {
            run: inv.run.clone(),
            checkpoint: checkpoint.clone(),
            epoch: epoch.0,
            mark: required.mark.clone(),
            artifact: inv.artifact.clone(),
            artifact_label: inv.artifact_label.clone(),
            params: required.params.clone(),
            metadata,
        };
        let prepared = admission
            .prepare_required_verified_artifact(required, input)
            .map_err(|error| RuntimeError::VerifiedArtifactRejected {
                error,
                invocation_metadata: Box::new(invocation_metadata.clone()),
            })?;
        let mark = tokio::task::spawn_blocking(move || prepared.run())
            .await
            .map_err(|error| RuntimeError::DescriptorDrift {
                reason: RefusedReason::RegistryUnstable,
                detail: format!("verified-artifact verifier execution did not complete: {error}"),
            })?
            .map_err(|error| RuntimeError::VerifiedArtifactRejected {
                error,
                invocation_metadata: Box::new(invocation_metadata),
            })?;
        let admitted = vec![mark];
        Ok(admitted)
    }
}

fn outbound_artifact_transfers_for_checkpoint(
    run: &RunId,
    checkpoint: &CheckpointId,
    artifacts: &[AdmittedCheckpointArtifact],
    outbound_interface_bindings: &[AuthoredInterfaceBinding],
) -> (
    Vec<InterfaceOutboundArtifactTransfer>,
    Vec<InterfaceOutboundArtifactRefusal>,
) {
    let mut seen = BTreeSet::new();
    let mut transfers = Vec::new();
    let mut refusals = Vec::new();
    for artifact in artifacts {
        for outbound in outbound_interface_bindings {
            if !seen.insert((
                artifact.artifact.id.clone(),
                outbound.interface.as_str().to_string(),
            )) {
                continue;
            }
            let invocation_metadata = invocation_metadata_for_outbound_binding(
                &artifact.invocation_metadata,
                &outbound.interface,
            );
            if !artifact
                .invocation_metadata
                .grant
                .interfaces
                .contains(&outbound.interface.as_str().to_string())
                || !outbound.admits(&artifact.artifact_label)
            {
                refusals.push(InterfaceOutboundArtifactRefusal {
                    run: run.clone(),
                    checkpoint: checkpoint.clone(),
                    artifact: artifact.artifact.clone(),
                    from_interface: artifact.from_interface.clone(),
                    to_interface: outbound.interface.clone(),
                    invocation_metadata,
                });
                continue;
            }
            transfers.push(InterfaceOutboundArtifactTransfer {
                run: run.clone(),
                checkpoint: checkpoint.clone(),
                artifact: artifact.artifact.clone(),
                artifact_bindings: artifact.invocation_metadata.artifact_binding_refs.clone(),
                from_interface: artifact.from_interface.clone(),
                to_interface: outbound.interface.clone(),
                invocation_metadata,
            });
        }
    }
    (transfers, refusals)
}

fn invocation_metadata_for_outbound_binding(
    invocation_metadata: &iface::InvocationMetadata,
    interface: &InterfaceId,
) -> iface::InvocationMetadata {
    let mut metadata = invocation_metadata.clone();
    if let Some(runtime) = metadata.runtime.as_mut() {
        runtime.interface = interface.clone();
    }
    metadata
}

pub struct RuntimeServices {
    pub interface_catalog: Arc<RuntimeInterfaceCatalog>,
}

struct RuntimeInner {
    gate: Arc<RuntimeGate>,
    interface_catalog: Arc<RuntimeInterfaceCatalog>,
    run_records_dir: Mutex<Option<std::path::PathBuf>>,
    run_index: Mutex<RunRecordsIndex>,
    checkpoint_admission: Mutex<CheckpointAdmissionState>,
    admitted_checkpoint_artifacts:
        Mutex<BTreeMap<CheckpointAdmissionKey, BTreeMap<String, AdmittedCheckpointArtifact>>>,
    registry_generation: AtomicU64,
    run_grant_resolver: RwLock<Option<Arc<dyn RunGrantResolver + Send + Sync>>>,
    context_artifact_resolver: RwLock<Option<Arc<dyn ContextArtifactResolver + Send + Sync>>>,
}

#[derive(Default)]
struct CheckpointAdmissionState {
    spent: BTreeSet<CheckpointAdmissionKey>,
    routed: BTreeSet<(CheckpointAdmissionKey, ArtifactId)>,
    admitting: BTreeSet<CheckpointAdmissionKey>,
    waiters: BTreeMap<CheckpointAdmissionKey, tokio::sync::watch::Sender<u64>>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CheckpointAdmissionKey {
    run: RunId,
    checkpoint: CheckpointId,
    epoch: u64,
}

#[derive(Clone, Debug)]
struct AdmittedCheckpointArtifact {
    artifact_label: String,
    artifact: ArtifactRef,
    from_interface: InterfaceId,
    invocation_metadata: iface::InvocationMetadata,
}

struct CheckpointAdmissionReservation {
    inner: Arc<RuntimeInner>,
    key: CheckpointAdmissionKey,
    committed: bool,
}

pub struct PreparedInterfaceCheckpointAdmission {
    observation: Observation,
    reservation: CheckpointAdmissionReservation,
    admitted: AdmittedCheckpointArtifact,
    checkpoint_complete: bool,
    standing_route: bool,
    source_guard: Option<Box<dyn AdmissionSourceGuard>>,
    run_guard: octopus_file_storage::publication::ReadGuard,
}

impl PreparedInterfaceCheckpointAdmission {
    pub fn observation(&self) -> &Observation {
        &self.observation
    }

    pub fn validate_commit(&self) -> Result<(), RuntimeError> {
        self.run_guard.verify().map_err(admission_sources_changed)?;
        if let Some(guard) = &self.source_guard {
            guard.validate().map_err(admission_sources_changed)?;
        }
        Ok(())
    }

    pub fn commit(self) -> Observation {
        let Self {
            observation,
            reservation,
            admitted,
            checkpoint_complete,
            standing_route,
            source_guard: _source_guard,
            run_guard: _run_guard,
        } = self;
        reservation.inner.gate.admitted.lock().unwrap()
            .extend(observation.admitted_mark_outcomes.iter().cloned());
        reservation.commit(admitted, checkpoint_complete, standing_route);
        observation
    }
}

fn admission_sources_changed(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::DescriptorDrift {
        reason: RefusedReason::RegistryUnstable,
        detail: format!("admission sources changed before durable admission: {error}"),
    }
}

impl CheckpointAdmissionReservation {
    fn commit(mut self, admitted: AdmittedCheckpointArtifact, complete: bool, standing_route: bool) {
        let artifact = admitted.artifact.id.clone();
        if !standing_route {
            let mut progress = self.inner.admitted_checkpoint_artifacts.lock().unwrap();
            progress
                .entry(self.key.clone())
                .or_default()
                .insert(admitted.artifact_label.clone(), admitted);
            if complete {
                progress.remove(&self.key);
            }
        }

        let mut state = self.inner.checkpoint_admission.lock().unwrap();
        if standing_route {
            state.routed.insert((self.key.clone(), artifact));
        } else if complete {
            state.spent.insert(self.key.clone());
        }
        let wake = release_checkpoint_reservation(&mut state, &self.key);
        drop(state);
        if let Some(wake) = wake {
            wake.send_replace(1);
        }
        self.committed = true;
    }
}

impl Drop for CheckpointAdmissionReservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut state = self.inner.checkpoint_admission.lock().unwrap();
        let wake = release_checkpoint_reservation(&mut state, &self.key);
        drop(state);
        if let Some(wake) = wake {
            wake.send_replace(1);
        }
    }
}

fn release_checkpoint_reservation(
    state: &mut CheckpointAdmissionState,
    key: &CheckpointAdmissionKey,
) -> Option<tokio::sync::watch::Sender<u64>> {
    state.admitting.remove(key);
    state.waiters.remove(key)
}

#[derive(Clone)]
pub struct Runtime {
    inner: Arc<RuntimeInner>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime").finish_non_exhaustive()
    }
}

impl Runtime {
    pub fn open(services: RuntimeServices) -> Result<Runtime, RuntimeError> {
        let gate = Arc::new(RuntimeGate {
            authorities: Mutex::new(HashMap::new()),
            admitted: Mutex::new(Vec::new()),
            verified_artifact_admission: RwLock::new(Arc::new(FailClosedVerifiedArtifactAdmission)),
        });
        Ok(Self::from_gate(services, gate))
    }

    fn from_gate(services: RuntimeServices, gate: Arc<RuntimeGate>) -> Runtime {
        Runtime {
            inner: Arc::new(RuntimeInner {
                gate,
                interface_catalog: services.interface_catalog,
                run_records_dir: Mutex::new(None),
                run_index: Mutex::new(RunRecordsIndex::new()),
                checkpoint_admission: Mutex::new(CheckpointAdmissionState::default()),
                admitted_checkpoint_artifacts: Mutex::new(BTreeMap::new()),
                registry_generation: AtomicU64::new(0),
                run_grant_resolver: RwLock::new(None),
                context_artifact_resolver: RwLock::new(None),
            }),
        }
    }

    pub fn set_run_grant_resolver(&self, resolver: Arc<dyn RunGrantResolver + Send + Sync>) {
        *self.inner.run_grant_resolver.write().unwrap() = Some(resolver);
        self.inner
            .registry_generation
            .fetch_add(1, Ordering::SeqCst);
    }

    pub fn set_context_artifact_resolver(
        &self,
        resolver: Arc<dyn ContextArtifactResolver + Send + Sync>,
    ) {
        *self.inner.context_artifact_resolver.write().unwrap() = Some(resolver);
    }

    fn resolve_context_artifact_bindings(
        &self,
        bindings: &[ArtifactBinding],
        artifact_access: &BTreeMap<ArtifactId, policy::ArtifactAccess>,
    ) -> Result<Vec<ArtifactRef>, RuntimeError> {
        let readable: Vec<&ArtifactBinding> = bindings
            .iter()
            .filter(|binding| {
                artifact_access
                    .get(&binding.id)
                    .is_some_and(|access| access.read)
            })
            .collect();
        if readable.is_empty() {
            return Ok(Vec::new());
        }
        let resolver = self
            .inner
            .context_artifact_resolver
            .read()
            .map_err(|_| RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: "context artifact resolver lock is poisoned while admitting artifact arrival"
                    .to_string(),
            })?
            .clone()
            .ok_or_else(|| RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: "artifact arrival binds context artifacts by id, but no context artifact resolver is installed to derive their content".to_string(),
            })?;
        let mut resolved = Vec::with_capacity(readable.len());
        for binding in readable {
            let content = resolver
                .resolve_artifact_content(&binding.id)
                .map_err(|e| RuntimeError::DescriptorDrift {
                    reason: RefusedReason::NoBinding,
                    detail: format!(
                        "artifact binding '{}' content could not be derived from its id: {e}",
                        binding.id.as_str()
                    ),
                })?;
            resolved.push(ArtifactRef {
                id: binding.id.clone(),
                content,
            });
        }
        Ok(resolved)
    }

    fn require_known_interface(&self, interface: &InterfaceId) -> Result<(), RuntimeError> {
        if self.inner.interface_catalog.contains(interface) {
            Ok(())
        } else {
            Err(RuntimeError::UnknownInterface(interface.clone()))
        }
    }

    pub fn is_system_interface(&self, interface: &InterfaceId) -> bool {
        self.inner.interface_catalog.is_system(interface)
    }

    pub fn is_runtime_relay_interface(&self, interface: &InterfaceId) -> bool {
        self.inner.interface_catalog.is_runtime_relay(interface)
    }

    fn require_known_interface_bindings(
        &self,
        bindings: &[InterfaceId],
    ) -> Result<(), RuntimeError> {
        for interface in bindings {
            self.require_known_interface(interface)?;
        }
        Ok(())
    }

    pub fn set_run_records_dir(&self, dir: std::path::PathBuf) {
        *self.inner.run_records_dir.lock().unwrap() = Some(dir);
    }

    fn reset_checkpoint_admission(&self) {
        let mut state = self.inner.checkpoint_admission.lock().unwrap();
        state.spent.clear();
        state.routed.clear();
        state.admitting.clear();
        state.waiters.clear();
        drop(state);
        self.inner
            .admitted_checkpoint_artifacts
            .lock()
            .unwrap()
            .clear();
    }

    pub fn reconstruct_spent_checkpoints_from_movements<'a>(
        &self,
        movements: impl IntoIterator<Item = &'a Movement>,
    ) -> Result<(), RuntimeError> {
        let movements: Vec<&Movement> = movements.into_iter().collect();
        let mark_proofs =
            ledger::InterfaceArrivalMarkProofIndex::from_movements(movements.iter().copied())
                .map_err(|error| RuntimeError::InterfaceBoundaryFailed(error.to_string()))?;
        self.reset_checkpoint_admission();
        for movement in &movements {
            if self.durable_arrival_is_committed(movement, &mark_proofs)? {
                self.note_checkpoint_admission_from_movement(movement)?;
            }
        }
        Ok(())
    }

    pub fn reconstruct_spent_checkpoints_from_log(
        &self,
        log: &(dyn octopus_ledger::MovementLog + Send),
    ) -> Result<(), RuntimeError> {
        let mut movements = Vec::new();
        log.scan_from(octopus_ledger::Seq(0), &mut |movement| {
            movements.push(movement);
            std::ops::ControlFlow::Continue(())
        });
        self.reconstruct_spent_checkpoints_from_movements(&movements)
    }

    fn durable_arrival_is_committed(
        &self,
        movement: &Movement,
        mark_proofs: &ledger::InterfaceArrivalMarkProofIndex,
    ) -> Result<bool, RuntimeError> {
        if movement.kind != MovementKind::InterfaceArrival {
            return Ok(true);
        }
        let run = movement.run().ok_or_else(|| {
            RuntimeError::InterfaceBoundaryFailed("durable arrival has no run provenance".into())
        })?;
        let metadata = movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation())
            .ok_or_else(|| {
                RuntimeError::InterfaceBoundaryFailed(format!(
                    "durable InterfaceArrival into run '{}' has no typed invocation metadata",
                    run.as_str()
                ))
            })?;
        let runtime = metadata.runtime.as_ref().ok_or_else(|| {
            RuntimeError::InterfaceBoundaryFailed(format!(
                "durable InterfaceArrival into run '{}' has no runtime provenance",
                run.as_str()
            ))
        })?;
        if movement.to != Endpoint::Interface(runtime.worker.clone())
            || !matches!(
                &movement.from,
                Endpoint::Interface(interface) if interface == &runtime.interface
            )
        {
            return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                "durable InterfaceArrival into run '{}' has mismatched runtime provenance",
                run.as_str()
            )));
        }
        if movement.artifact.is_none() {
            return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                "durable admission for run '{}' checkpoint '{}' has no artifact",
                run.as_str(),
                runtime.checkpoint.as_str()
            )));
        }
        if metadata.verification_failure.is_some() {
            return Ok(true);
        }
        let Some(admission) = metadata.checkpoint_admission.as_ref() else {
            return Ok(true);
        };
        if admission.required_artifact_labels.is_empty() {
            if !admission.complete || admission.artifact_label.trim().is_empty() {
                return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                    "durable admission for run '{}' checkpoint '{}' has malformed checkpoint obligation metadata",
                    run.as_str(),
                    runtime.checkpoint.as_str()
                )));
            }
            return Ok(true);
        }
        let required_labels: BTreeSet<&str> = admission
            .required_artifact_labels
            .iter()
            .map(String::as_str)
            .collect();
        let declared_labels: BTreeSet<&str> = metadata
            .required_verified_artifacts
            .iter()
            .map(|required| required.artifact_label.as_str())
            .collect();
        if admission.artifact_label.trim().is_empty()
            || required_labels.len() != admission.required_artifact_labels.len()
            || !required_labels.contains(admission.artifact_label.as_str())
            || declared_labels.len() != metadata.required_verified_artifacts.len()
            || required_labels != declared_labels
        {
            return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                "durable admission for run '{}' has inconsistent required verified artifact metadata",
                run.as_str()
            )));
        }
        Ok(mark_proofs.proof_lamport(movement).is_some())
    }

    fn load_run_record_with_hash(
        &self,
        run: &RunId,
    ) -> std::io::Result<Option<(RunRecord, RunRecordSnapshot)>> {
        let Some(dir) = self.inner.run_records_dir.lock().unwrap().clone() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "run-record directory is not configured",
            ));
        };
        let path = dir.join(format!("{}.json", run.as_str()));
        match std::fs::read(path) {
            Ok(bytes) => {
                let hash = ContentHash::of_bytes(&bytes);
                let record = RunRecord::from_bytes(&bytes)?;
                if record.id != *run {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "run record file '{}' contains id '{}'",
                            run.as_str(),
                            record.id.as_str()
                        ),
                    ));
                }
                Ok(Some((
                    record,
                    RunRecordSnapshot {
                        hash,
                        bytes: Arc::from(bytes),
                    },
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn active_run_record_checkpoints<'a>(
        record: &'a RunRecord,
        context: &str,
    ) -> Result<Vec<&'a CheckpointRecord>, RuntimeError> {
        if record.active_checkpoint_ids().is_empty() {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::Settled,
                detail: format!(
                    "run '{}' has an empty active_checkpoints[] while {}; a settled run admits no artifact",
                    record.id.as_str(),
                    context
                ),
            });
        }
        let mut checkpoints_by_id = HashMap::with_capacity(record.checkpoints.len());
        for checkpoint in &record.checkpoints {
            if checkpoints_by_id
                .insert(checkpoint.id.clone(), checkpoint)
                .is_some()
            {
                return Err(RuntimeError::DescriptorDrift {
                    reason: RefusedReason::NoBinding,
                    detail: format!(
                        "run '{}' JSON repeats checkpoint '{}' while {}",
                        record.id.as_str(),
                        checkpoint.id.as_str(),
                        context
                    ),
                });
            }
        }
        let mut seen = BTreeSet::new();
        let mut active = Vec::with_capacity(record.active_checkpoint_ids().len());
        for id in record.active_checkpoint_ids() {
            if !seen.insert(id.clone()) {
                return Err(RuntimeError::DescriptorDrift {
                    reason: RefusedReason::NoBinding,
                    detail: format!(
                        "run '{}' JSON repeats active checkpoint '{}' while {}",
                        record.id.as_str(),
                        id.as_str(),
                        context
                    ),
                });
            }
            let checkpoint = checkpoints_by_id.get(id).copied().ok_or_else(|| {
                RuntimeError::DescriptorDrift {
                    reason: RefusedReason::NoBinding,
                    detail: format!(
                        "run '{}' JSON active checkpoint '{}' is not present in checkpoints[] while {}",
                        record.id.as_str(),
                        id.as_str(),
                        context
                    ),
                }
            })?;
            if checkpoint.run != record.id {
                return Err(RuntimeError::DescriptorDrift {
                    reason: RefusedReason::NoBinding,
                    detail: format!(
                        "run '{}' JSON active checkpoint '{}' belongs to run '{}'",
                        record.id.as_str(),
                        checkpoint.id.as_str(),
                        checkpoint.run.as_str()
                    ),
                });
            }
            active.push(checkpoint);
        }
        Ok(active)
    }

    fn authored_interfaces_for_active_checkpoints(
        active_checkpoints: &[&CheckpointRecord],
    ) -> Vec<InterfaceId> {
        let mut authored = BTreeSet::new();
        for checkpoint in active_checkpoints {
            if let Some(bound) = checkpoint.interface_binding.as_ref() {
                authored.insert(bound.interface.clone());
            }
            authored.extend(
                checkpoint
                    .outbound_interface_bindings
                    .iter()
                    .map(|binding| binding.interface.clone()),
            );
        }
        authored.into_iter().collect()
    }

    fn installed_run_grant_resolver(
        &self,
    ) -> Result<Arc<dyn RunGrantResolver + Send + Sync>, RuntimeError> {
        self.inner
            .run_grant_resolver
            .read()
            .map_err(|_| RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: "run grant resolver lock is poisoned while forming run authority"
                    .to_string(),
            })?
            .clone()
            .ok_or_else(|| RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: "runtime has no installed run grant resolver; refusing to trust run JSON as authority"
                    .to_string(),
            })
    }

    fn resolve_run_record_checkpoint_grant_once(
        &self,
        resolver: &Arc<dyn RunGrantResolver + Send + Sync>,
        record: &RunRecord,
        checkpoint: &CheckpointRecord,
        authored_interfaces: &[InterfaceId],
    ) -> Result<ResolvedRunGrant, RuntimeError> {
        let artifact_bindings = checkpoint
            .artifact_bindings
            .iter()
            .map(|binding| binding.id.clone())
            .collect::<Vec<_>>();
        let artifact_access = checkpoint
            .artifact_bindings
            .iter()
            .map(|binding| (binding.id.clone(), binding.access))
            .collect::<BTreeMap<_, _>>();
        let resolution = resolver
            .resolve_run_grant(RunGrantQuery {
                run: &record.id,
                checkpoint: &checkpoint.id,
                ee: &record.ee,
                worker: &record.worker,
                epoch: checkpoint.epoch,
                authored_interfaces,
                artifact_bindings: &artifact_bindings,
                artifact_access: &artifact_access,
                required_verified_artifacts: &checkpoint.required_verified_artifacts,
            })
            .map_err(|e| RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: format!(
                    "run '{}' checkpoint '{}' grant re-formation failed closed: {e}",
                    record.id.as_str(),
                    checkpoint.id.as_str()
                ),
            })?;
        let (grant, metadata_projection) = resolution.into_parts();
        Ok(ResolvedRunGrant {
            grant: Arc::new(grant),
            metadata_projection,
        })
    }

    fn resolve_run_record_checkpoint_grant(
        &self,
        record: &RunRecord,
        checkpoint: &CheckpointRecord,
    ) -> Result<ResolvedRunGrant, RuntimeError> {
        let resolver = self.installed_run_grant_resolver()?;
        let active_checkpoints = Self::active_run_record_checkpoints(
            record,
            "forming the run-wide authored binding grant",
        )?;
        let authored_interfaces =
            Self::authored_interfaces_for_active_checkpoints(&active_checkpoints);
        loop {
            let registry_generation = self.inner.registry_generation.load(Ordering::SeqCst);
            let resolved = self.resolve_run_record_checkpoint_grant_once(
                &resolver,
                record,
                checkpoint,
                &authored_interfaces,
            )?;
            if self.inner.registry_generation.load(Ordering::SeqCst) == registry_generation {
                return Ok(resolved);
            }
        }
    }

    async fn reserve_checkpoint_admission(
        &self,
        key: CheckpointAdmissionKey,
        artifact: &ArtifactRef,
        interface: &InterfaceId,
    ) -> Result<CheckpointAdmissionReservation, RuntimeError> {
        loop {
            let wait = {
                let mut state = self.inner.checkpoint_admission.lock().unwrap();
                if state.spent.contains(&key) {
                    return Err(RuntimeError::DescriptorDrift {
                        reason: RefusedReason::CheckpointSpent,
                        detail: format!(
                            "artifact '{}' arrived for run '{}' on interface '{}', but checkpoint '{}' epoch {} already closed its required verified artifact set in the durable movement log; interface JSON must author a new checkpoint generation before another artifact can enter",
                            artifact.id.as_str(),
                            key.run.as_str(),
                            interface.as_str(),
                            key.checkpoint.as_str(),
                            key.epoch
                        ),
                    });
                }
                if state.routed.contains(&(key.clone(), artifact.id.clone())) {
                    return Err(RuntimeError::DescriptorDrift {
                        reason: RefusedReason::CheckpointSpent,
                        detail: format!(
                            "artifact '{}' arrived for run '{}' on interface '{}', but checkpoint '{}' epoch {} already admitted this exact artifact in the durable movement log; every arrival on a checkpoint is a distinct artifact",
                            artifact.id.as_str(),
                            key.run.as_str(),
                            interface.as_str(),
                            key.checkpoint.as_str(),
                            key.epoch
                        ),
                    });
                }
                if state.admitting.insert(key.clone()) {
                    let (wake, _) = tokio::sync::watch::channel(0);
                    state.waiters.insert(key.clone(), wake);
                    return Ok(CheckpointAdmissionReservation {
                        inner: self.inner.clone(),
                        key,
                        committed: false,
                    });
                }
                state
                    .waiters
                    .get(&key)
                    .expect("an admitting checkpoint owns a wake channel")
                    .subscribe()
            };
            let mut wait = wait;
            let _ = wait.changed().await;
        }
    }

    fn checkpoint_admission_plan(
        &self,
        inv: &InterfaceArtifactAdmission,
        checkpoint: &CheckpointId,
        ee: &desc::EeId,
        worker: &InterfaceId,
        metadata_projection: &iface::GrantProjection,
        artifact_binding_refs: &[ArtifactRef],
        required_verified_artifacts: &[RequiredVerifiedArtifact],
        inbound_interface_binding: &AuthoredInterfaceBinding,
        outbound_interface_bindings: &[AuthoredInterfaceBinding],
    ) -> Result<
        (
            iface::CheckpointAdmissionMetadata,
            Vec<AdmittedCheckpointArtifact>,
        ),
        RuntimeError,
    > {
        let required_artifact_labels: Vec<String> = required_verified_artifacts
            .iter()
            .map(|required| required.artifact_label.clone())
            .collect();
        let outbound_interface_ids = outbound_interface_bindings
            .iter()
            .map(|binding| binding.interface.clone())
            .collect::<Vec<_>>();
        let binding_policy_snapshot = iface::CheckpointBindingPolicySnapshot {
            inbound: inbound_interface_binding.clone(),
            outbound: outbound_interface_bindings.to_vec(),
        };
        let key = CheckpointAdmissionKey {
            run: inv.run.clone(),
            checkpoint: checkpoint.clone(),
            epoch: inv.epoch.0,
        };
        let progress = self.inner.admitted_checkpoint_artifacts.lock().unwrap();
        let admitted = progress.get(&key).cloned().unwrap_or_default();
        if admitted.contains_key(&inv.artifact_label) {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::CheckpointSpent,
                detail: format!(
                    "artifact label '{}' already satisfied its requirement for run '{}' checkpoint '{}'; author a new checkpoint generation instead of replaying one requirement edge",
                    inv.artifact_label,
                    inv.run.as_str(),
                    checkpoint.as_str()
                ),
            });
        }

        for prior in admitted.values() {
            let Some(runtime) = prior.invocation_metadata.runtime.as_ref() else {
                return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                    "durable partial admission '{}' for run '{}' checkpoint '{}' has no runtime coordinates",
                    prior.artifact_label,
                    inv.run.as_str(),
                    checkpoint.as_str()
                )));
            };
            let prior_required_labels = prior
                .invocation_metadata
                .checkpoint_admission
                .as_ref()
                .map(|admission| admission.required_artifact_labels.as_slice())
                .unwrap_or_default();
            if !runtime.same_checkpoint_coordinates(&octopus_ledger::RuntimeInvocationMetadata {
                run: inv.run.clone(),
                checkpoint: checkpoint.clone(),
                ee: ee.clone(),
                worker: worker.clone(),
                interface: inv.interface.clone(),
                epoch: inv.epoch.0,
                run_record_hash: None,
            })
                || prior_required_labels != required_artifact_labels.as_slice()
                || prior.invocation_metadata.grant != *metadata_projection
                || prior.invocation_metadata.artifact_binding_refs != artifact_binding_refs
                || prior.invocation_metadata.required_verified_artifacts
                    != required_verified_artifacts
                || prior.invocation_metadata.outbound_interface_bindings != outbound_interface_ids
                || prior.invocation_metadata.checkpoint_binding_policy.as_ref()
                    != Some(&binding_policy_snapshot)
                || prior.invocation_metadata.verification_failure.is_some()
            {
                return Err(RuntimeError::DescriptorDrift {
                    reason: RefusedReason::StaleEpoch,
                    detail: format!(
                        "run '{}' checkpoint '{}' changed its authored coordinates after partially satisfying required artifacts; author a new checkpoint generation instead of combining evidence from different checkpoint snapshots",
                        inv.run.as_str(),
                        checkpoint.as_str()
                    ),
                });
            }
        }

        let complete = required_artifact_labels.is_empty()
            || admitted.len().saturating_add(1) == required_artifact_labels.len();
        let ordered_prior = required_artifact_labels
            .iter()
            .filter_map(|label| admitted.get(label).cloned())
            .collect();
        Ok((
            iface::CheckpointAdmissionMetadata {
                artifact_label: inv.artifact_label.clone(),
                required_artifact_labels,
                complete,
            },
            ordered_prior,
        ))
    }

    fn note_checkpoint_admission_from_movement(
        &self,
        movement: &Movement,
    ) -> Result<(), RuntimeError> {
        if movement.kind != MovementKind::InterfaceArrival {
            return Ok(());
        }
        let run = movement.run().ok_or_else(|| {
            RuntimeError::InterfaceBoundaryFailed("durable arrival has no run provenance".into())
        })?;
        let metadata = movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation())
            .ok_or_else(|| {
                RuntimeError::InterfaceBoundaryFailed(format!(
                    "durable InterfaceArrival into run '{}' has no typed invocation metadata",
                    run.as_str()
                ))
            })?;
        let runtime = metadata.runtime.as_ref().ok_or_else(|| {
            RuntimeError::InterfaceBoundaryFailed(format!(
                "durable InterfaceArrival into run '{}' has no runtime provenance",
                run.as_str()
            ))
        })?;
        if movement.to != Endpoint::Interface(runtime.worker.clone())
            || !matches!(
                &movement.from,
                Endpoint::Interface(interface) if interface == &runtime.interface
            )
        {
            return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                "durable InterfaceArrival into run '{}' has mismatched runtime provenance",
                run.as_str()
            )));
        }
        let artifact = movement.artifact.clone().ok_or_else(|| {
            RuntimeError::InterfaceBoundaryFailed(format!(
                "durable admission for run '{}' checkpoint '{}' has no artifact",
                run.as_str(),
                runtime.checkpoint.as_str()
            ))
        })?;
        if metadata.verification_failure.is_some() {
            return Ok(());
        }
        let key = CheckpointAdmissionKey {
            run: run.clone(),
            checkpoint: runtime.checkpoint.clone(),
            epoch: runtime.epoch,
        };
        let Some(admission) = metadata.checkpoint_admission.as_ref() else {
            self.inner
                .checkpoint_admission
                .lock()
                .unwrap()
                .spent
                .insert(key);
            return Ok(());
        };
        let mut required = BTreeSet::new();
        if admission.artifact_label.trim().is_empty()
            || admission
                .required_artifact_labels
                .iter()
                .any(|label| label.trim().is_empty() || !required.insert(label.clone()))
            || (!admission.required_artifact_labels.is_empty()
                && !required.contains(&admission.artifact_label))
            || (admission.required_artifact_labels.is_empty() && !admission.complete)
        {
            return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                "durable admission for run '{}' checkpoint '{}' has malformed checkpoint obligation metadata",
                run.as_str(),
                runtime.checkpoint.as_str()
            )));
        }

        if admission.required_artifact_labels.is_empty() {
            let mut state = self.inner.checkpoint_admission.lock().unwrap();
            if state.spent.contains(&key) || !state.routed.insert((key, artifact.id.clone())) {
                return Err(RuntimeError::DescriptorDrift {
                    reason: RefusedReason::CheckpointSpent,
                    detail: format!(
                        "durable movement log admits artifact '{}' twice on run '{}' checkpoint '{}'",
                        artifact.id.as_str(),
                        run.as_str(),
                        runtime.checkpoint.as_str()
                    ),
                });
            }
            return Ok(());
        }

        if self
            .inner
            .checkpoint_admission
            .lock()
            .unwrap()
            .spent
            .contains(&key)
        {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::CheckpointSpent,
                detail: format!(
                    "durable movement log contains another admitted edge after run '{}' checkpoint '{}' was already complete",
                    run.as_str(),
                    runtime.checkpoint.as_str()
                ),
            });
        }

        let admitted = AdmittedCheckpointArtifact {
            artifact_label: admission.artifact_label.clone(),
            artifact,
            from_interface: runtime.interface.clone(),
            invocation_metadata: metadata.clone(),
        };
        let mut progress = self.inner.admitted_checkpoint_artifacts.lock().unwrap();
        let checkpoint_progress = progress.entry(key.clone()).or_default();
        if let Some(prior) = checkpoint_progress.values().next() {
            let prior_required_labels = prior
                .invocation_metadata
                .checkpoint_admission
                .as_ref()
                .map(|prior| prior.required_artifact_labels.as_slice())
                .unwrap_or_default();
            if !prior.invocation_metadata.runtime.as_ref()
                .is_some_and(|prior| prior.same_checkpoint_coordinates(runtime))
                || prior.invocation_metadata.grant != metadata.grant
                || prior.invocation_metadata.artifact_binding_refs != metadata.artifact_binding_refs
                || prior.invocation_metadata.required_verified_artifacts
                    != metadata.required_verified_artifacts
                || prior.invocation_metadata.outbound_interface_bindings
                    != metadata.outbound_interface_bindings
                || prior.invocation_metadata.checkpoint_binding_policy
                    != metadata.checkpoint_binding_policy
                || prior.invocation_metadata.verification_failure.is_some()
                || metadata.verification_failure.is_some()
                || prior_required_labels != admission.required_artifact_labels.as_slice()
            {
                return Err(RuntimeError::DescriptorDrift {
                    reason: RefusedReason::StaleEpoch,
                    detail: format!(
                        "durable movement log combines different run, policy, binding, or checkpoint snapshots for run '{}' checkpoint '{}'",
                        run.as_str(),
                        runtime.checkpoint.as_str()
                    ),
                });
            }
        }
        if checkpoint_progress
            .insert(admission.artifact_label.clone(), admitted)
            .is_some()
        {
            return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                "durable movement log repeats artifact label '{}' for run '{}' checkpoint '{}'",
                admission.artifact_label,
                run.as_str(),
                runtime.checkpoint.as_str()
            )));
        }
        let admitted_labels: BTreeSet<String> = checkpoint_progress.keys().cloned().collect();
        if admission.complete {
            if admitted_labels != required && !required.is_empty() {
                return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                    "durable movement log declares run '{}' checkpoint '{}' complete before all authored required artifact labels were admitted",
                    run.as_str(),
                    runtime.checkpoint.as_str()
                )));
            }
            progress.remove(&key);
            drop(progress);
            self.inner
                .checkpoint_admission
                .lock()
                .unwrap()
                .spent
                .insert(key);
        } else if admitted_labels == required {
            return Err(RuntimeError::InterfaceBoundaryFailed(format!(
                "durable movement log leaves run '{}' checkpoint '{}' partial after all required artifact labels were admitted",
                run.as_str(),
                runtime.checkpoint.as_str()
            )));
        }
        Ok(())
    }

    fn admit_run_record_artifact_arrival(
        &self,
        inv: &mut InterfaceArtifactAdmission,
    ) -> Result<RunRecordAdmission, RuntimeError> {
        let Some((record, run_record)) = self.load_run_record_with_hash(&inv.run).map_err(|error| {
            admission_sources_changed(format!(
                "run '{}' record could not be read for artifact arrival admission: {error}",
                inv.run.as_str()
            ))
        })?
        else {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::UnknownRun,
                detail: format!(
                    "run '{}' has no run JSON on artifact arrival; refusing to bypass checkpoint admission",
                    inv.run.as_str()
                ),
            });
        };
        if let Some(expected) = inv.run_record_hash {
            if expected != run_record.hash {
                return Err(RuntimeError::ArrivalTargetChanged {
                    run: inv.run.clone(),
                    expected,
                    observed: run_record.hash,
                });
            }
        }
        inv.run_record_hash = Some(run_record.hash);
        let active = Self::active_run_record_checkpoints(
            &record,
            "admitting an interface artifact arrival",
        )?;
        let current = match inv.checkpoint.as_ref() {
            Some(checkpoint) => active
                .iter()
                .copied()
                .find(|active| &active.id == checkpoint)
                .ok_or_else(|| RuntimeError::DescriptorDrift {
                    reason: RefusedReason::NoBinding,
                    detail: format!(
                        "artifact '{}' arrived for run '{}' on interface '{}' addressed at checkpoint '{}', but that checkpoint is not active in the run JSON",
                        inv.artifact.id.as_str(),
                        inv.run.as_str(),
                        inv.interface.as_str(),
                        checkpoint.as_str()
                    ),
                })?,
            None => {
                let mut fanned = active.iter().copied().filter(|checkpoint| {
                    checkpoint
                        .interface_binding
                        .as_ref()
                        .is_some_and(|binding| binding.interface == inv.interface)
                });
                let first = fanned.next().ok_or_else(|| RuntimeError::DescriptorDrift {
                    reason: RefusedReason::NoBinding,
                    detail: format!(
                        "artifact '{}' arrived for run '{}' on interface '{}', but no active checkpoint pins that interface",
                        inv.artifact.id.as_str(),
                        inv.run.as_str(),
                        inv.interface.as_str()
                    ),
                })?;
                if let Some(second) = fanned.next() {
                    return Err(RuntimeError::DescriptorDrift {
                        reason: RefusedReason::NoBinding,
                        detail: format!(
                            "artifact '{}' arrived for run '{}' on interface '{}' with no addressed checkpoint, but active checkpoints '{}' and '{}' both pin that interface; a single admission cannot fan out — resolve every edge via resolve_interface_arrival_targets and admit each addressed checkpoint (architecture.md §Execution Flow: the artifact enters EVERY active checkpoint on the arriving interface)",
                            inv.artifact.id.as_str(),
                            inv.run.as_str(),
                            inv.interface.as_str(),
                            first.id.as_str(),
                            second.id.as_str()
                        ),
                    });
                }
                inv.checkpoint = Some(first.id.clone());
                first
            }
        };
        let Some(bound) = current.interface_binding.as_ref() else {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::NoBinding,
                detail: format!(
                    "artifact '{}' arrived for run '{}' on interface '{}', but active checkpoint '{}' has no interface_binding; refusing to bypass checkpoint admission",
                    inv.artifact.id.as_str(),
                    inv.run.as_str(),
                    inv.interface.as_str(),
                    current.id.as_str()
                ),
            });
        };
        if bound.interface != inv.interface {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::NoBinding,
                detail: format!(
                    "artifact '{}' arrived for run '{}' on interface '{}', but active checkpoint '{}' pins interface '{}'; refusing to route outside the authored interface folder",
                    inv.artifact.id.as_str(),
                    inv.run.as_str(),
                    inv.interface.as_str(),
                    current.id.as_str(),
                    bound.interface.as_str()
                ),
            });
        }
        if !bound.admits(&inv.artifact_label) {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: format!(
                    "policy.interface_binding_denied: run '{}' checkpoint '{}' inbound binding '{}' denies artifact name '{}'",
                    inv.run.as_str(),
                    current.id.as_str(),
                    inv.interface.as_str(),
                    inv.artifact_label
                ),
            });
        }
        if current.epoch != inv.epoch {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::StaleEpoch,
                detail: format!(
                    "artifact '{}' arrived for run '{}' checkpoint '{}' at epoch {}, but run JSON checkpoint epoch is {}",
                    inv.artifact.id.as_str(),
                    inv.run.as_str(),
                    current.id.as_str(),
                    inv.epoch.0,
                    current.epoch.0
                ),
            });
        }
        let resolved_grant = self.resolve_run_record_checkpoint_grant(&record, current)?;
        let artifact_bindings = self.resolve_context_artifact_bindings(
            &current.artifact_bindings,
            &resolved_grant.grant.context.artifact_access,
        )?;
        Ok(RunRecordAdmission {
            ee: record.ee.clone(),
            worker: record.worker.clone(),
            grant: resolved_grant.grant,
            metadata_projection: resolved_grant.metadata_projection,
            artifact_bindings,
            required_verified_artifacts: current.required_verified_artifacts.clone(),
            inbound_binding: bound.clone(),
            outbound_bindings: current.outbound_interface_bindings.clone(),
        })
    }

    pub fn set_verified_artifact_admission(
        &self,
        admission: Arc<dyn VerifiedArtifactAdmission + Send + Sync>,
    ) {
        self.inner.gate.set_verified_artifact_admission(admission);
    }

    pub fn replace_interface_catalog_classes(
        &self,
        entries: impl IntoIterator<Item = (InterfaceId, InterfaceClass)>,
    ) -> Result<(), String> {
        self.inner.interface_catalog.replace_classes(entries)?;
        self.inner
            .registry_generation
            .fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn interface_arrival_targets(
        &self,
        interface: &InterfaceId,
        artifact_label: &str,
        skip_emitter: Option<SystemInterfaceEmitter<'_>>,
    ) -> Result<(Vec<InterfaceArrivalTarget>, usize), RuntimeError> {
        let Some(dir) = self.inner.run_records_dir.lock().unwrap().clone() else {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::UnknownRun,
                detail: format!(
                    "run-record directory is not configured while resolving interface '{}' artifact arrival targets",
                    interface.as_str()
                ),
            });
        };
        let matches = {
            let mut index = self.inner.run_index.lock().map_err(|_| {
                RuntimeError::DescriptorDrift {
                    reason: RefusedReason::UnknownRun,
                    detail: format!(
                        "run-records index lock is poisoned while resolving interface '{}' artifact arrival targets",
                        interface.as_str()
                    ),
                }
            })?;
            index
                .catch_up(&dir)
                .map_err(|e| RuntimeError::DescriptorDrift {
                    reason: RefusedReason::UnknownRun,
                    detail: format!(
                        "run records could not be scanned for interface '{}' artifact arrival: {e}",
                        interface.as_str()
                    ),
                })?;
            index
                .targets_for_interface(interface)
                .into_iter()
                .filter_map(|(record, checkpoint)| {
                    index
                        .record_snapshot(&record.id)
                        .map(|snapshot| (record, checkpoint, snapshot))
                })
                .collect::<Vec<_>>()
        };
        let mut targets = Vec::with_capacity(matches.len());
        let mut pinned = 0;
        for (record, checkpoint_id, run_record) in matches {
            let Some(checkpoint) = record.checkpoint(&checkpoint_id) else {
                continue;
            };
            if let Some(emitter) = skip_emitter {
                if record.id == *emitter.run && checkpoint.id == *emitter.checkpoint {
                    continue;
                }
            }
            pinned += 1;
            if !checkpoint
                .interface_binding
                .as_ref()
                .is_some_and(|binding| binding.admits(artifact_label))
                || (!checkpoint.required_verified_artifacts.is_empty()
                    && !checkpoint
                        .required_verified_artifacts
                        .iter()
                        .any(|required| required.artifact_label == artifact_label))
            {
                continue;
            }
            targets.push(InterfaceArrivalTarget {
                run: record.id.clone(),
                checkpoint: checkpoint.id.clone(),
                ee: record.ee.clone(),
                worker: record.worker.clone(),
                epoch: checkpoint.epoch,
                run_trace: record.trace.clone(),
                checkpoint_trace: checkpoint.trace.clone(),
                inbound_binding: checkpoint
                    .interface_binding
                    .clone()
                    .expect("run index returns only inbound-bound checkpoints"),
                run_record,
                outbound_bindings: checkpoint.outbound_interface_bindings.clone(),
                arriving_interface: interface.clone(),
                source_movement_seq: None,
                delivery_scope_seq: None,
            });
        }
        Ok((targets, pinned))
    }

    pub fn resolve_interface_arrival_targets(
        &self,
        interface: &InterfaceId,
        artifact_label: &str,
    ) -> Result<Vec<InterfaceArrivalTarget>, RuntimeError> {
        self.require_known_interface(interface)?;
        let (targets, pinned) = self.interface_arrival_targets(interface, artifact_label, None)?;
        if targets.is_empty() && pinned > 0 {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: format!(
                    "policy.interface_binding_denied: no active checkpoint pinning interface '{}' admits artifact name '{}'",
                    interface.as_str(),
                    artifact_label
                ),
            });
        }
        if targets.is_empty() {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::NoBinding,
                detail: format!(
                    "interface '{}' artifact arrival has no bound targets",
                    interface.as_str()
                ),
            });
        }
        Ok(targets)
    }

    pub fn refresh_retained_interface_arrival_target(&self, original: &InterfaceArrivalTarget) -> Result<InterfaceArrivalTarget, RuntimeError> {
        self.require_known_interface(&original.arriving_interface)?;
        let changed = |detail: String| RuntimeError::DescriptorDrift { reason: RefusedReason::StaleEpoch, detail };
        // A failed observation cannot prove that the original recipient was removed
        // or replaced. Preserve the edge until current state can be established.
        let (record, snapshot) = self.load_run_record_with_hash(&original.run)
            .map_err(|error| admission_sources_changed(format!(
                "retained arrival run '{}' could not be observed: {error}", original.run.as_str()
            )))?
            .ok_or_else(|| changed("retained arrival run was removed".into()))?;
        let previous = RunRecord::from_bytes(&original.run_record.bytes)
            .map_err(|error| changed(error.to_string()))?;
        if record.worker != original.worker
            || record.trace != previous.trace
            || !record.active_checkpoints.contains(&original.checkpoint)
            || record.checkpoint(&original.checkpoint) != previous.checkpoint(&original.checkpoint) {
            return Err(changed("the original receiving checkpoint was removed or its contract changed".into()));
        }
        let mut current = original.clone();
        current.ee = record.ee.clone();
        current.run_record = snapshot;
        Ok(current)
    }

    pub fn resolve_system_interface_emission_grant(
        &self,
        emitter: SystemInterfaceEmitter<'_>,
        source_interface: &InterfaceId,
        interface: &InterfaceId,
        artifact_name: &str,
    ) -> Result<SystemInterfaceEmissionAuthority, RuntimeError> {
        self.require_known_interface(source_interface)?;
        if !self.inner.interface_catalog.is_system(interface) {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::UnknownInterface,
                detail: format!(
                    "run '{}' checkpoint '{}' emits toward interface '{}', but that interface is not a System interface; pair-addressed emission is the run→run seam only",
                    emitter.run.as_str(),
                    emitter.checkpoint.as_str(),
                    interface.as_str()
                ),
            });
        }
        let Some((record, run_record)) = self.load_run_record_with_hash(emitter.run).map_err(|e| {
            RuntimeError::DescriptorDrift {
                reason: RefusedReason::UnknownRun,
                detail: format!(
                    "emitter run '{}' record could not be read for interface-addressed emission: {e}",
                    emitter.run.as_str()
                ),
            }
        })?
        else {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::UnknownRun,
                detail: format!(
                    "run '{}' emits toward interface '{}', but that run JSON does not exist",
                    emitter.run.as_str(),
                    interface.as_str()
                ),
            });
        };
        let Some(checkpoint) = record.checkpoint(emitter.checkpoint) else {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: format!(
                    "run '{}' emits from checkpoint '{}', but the run JSON no longer carries that checkpoint; refusing to form an emitter grant for a vanished position",
                    emitter.run.as_str(),
                    emitter.checkpoint.as_str()
                ),
            });
        };
        let active_checkpoints = Self::active_run_record_checkpoints(
            &record,
            "authorizing an interface-addressed system emission",
        )?;
        if checkpoint.epoch != emitter.epoch {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: format!(
                    "run '{}' checkpoint '{}' emission belongs to epoch {}, but the active checkpoint uses epoch {}",
                    emitter.run.as_str(),
                    emitter.checkpoint.as_str(),
                    emitter.epoch.0,
                    checkpoint.epoch.0,
                ),
            });
        }
        if !active_checkpoints
            .iter()
            .any(|active| active.id == *emitter.checkpoint)
        {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: format!(
                    "run '{}' emits from checkpoint '{}', but that checkpoint is not active",
                    emitter.run.as_str(),
                    emitter.checkpoint.as_str()
                ),
            });
        }
        if !checkpoint
            .interface_binding
            .as_ref()
            .is_some_and(|binding| binding.interface == *source_interface)
            && !checkpoint
                .outbound_interface_bindings
                .iter()
                .any(|binding| binding.interface == *source_interface)
        {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: format!(
                    "run '{}' checkpoint '{}' does not author host-attested source interface '{}'; refusing borrowed emitter authority",
                    emitter.run.as_str(),
                    emitter.checkpoint.as_str(),
                    source_interface.as_str()
                ),
            });
        }
        let target_bindings = active_checkpoints
            .iter()
            .flat_map(|active| {
                active
                    .interface_binding
                    .iter()
                    .chain(active.outbound_interface_bindings.iter())
            })
            .filter(|binding| binding.interface == *interface)
            .cloned()
            .collect::<Vec<_>>();
        if target_bindings.is_empty() {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::NoBinding,
                detail: format!(
                    "run '{}' checkpoint '{}' emits toward interface '{}', but no active checkpoint in the run authors that interface folder",
                    emitter.run.as_str(),
                    emitter.checkpoint.as_str(),
                    interface.as_str()
                ),
            });
        }
        if target_bindings
            .iter()
            .any(|binding| !binding.admits(artifact_name))
        {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::Policy,
                detail: format!(
                    "policy.interface_binding_denied: run '{}' checkpoint '{}' outbound binding '{}' denies artifact name '{}'",
                    emitter.run.as_str(),
                    emitter.checkpoint.as_str(),
                    interface.as_str(),
                    artifact_name
                ),
            });
        }
        let resolved_grant = self.resolve_run_record_checkpoint_grant(&record, checkpoint)?;
        ensure_interface_in_grant(
            resolved_grant.grant.as_ref(),
            emitter.run,
            Some(emitter.checkpoint),
            interface,
            "outbound",
        )?;
        Ok(SystemInterfaceEmissionAuthority {
            grant: resolved_grant.grant,
            metadata_projection: resolved_grant.metadata_projection,
            ee: record.ee.clone(),
            worker: record.worker.clone(),
            epoch: checkpoint.epoch,
            checkpoint_binding_policy: iface::CheckpointBindingPolicySnapshot {
                inbound: checkpoint
                    .interface_binding
                    .clone()
                    .expect("active checkpoint validation requires an inbound binding"),
                outbound: target_bindings,
            },
            run_record,
        })
    }

    pub fn resolve_runtime_relay_cross_run_arrival_targets(
        &self,
        interface: &InterfaceId,
        artifact: &ArtifactRef,
        artifact_label: &str,
        emitter: SystemInterfaceEmitter<'_>,
    ) -> Result<Vec<InterfaceArrivalTarget>, RuntimeError> {
        if !self.inner.interface_catalog.is_runtime_relay(interface) {
            return Err(RuntimeError::DescriptorDrift {
                reason: RefusedReason::UnknownInterface,
                detail: format!(
                    "artifact '{}' outbound on interface '{}' requests runtime-relay fan-out, but that interface is not formed as a runtime relay",
                    artifact.id.as_str(),
                    interface.as_str()
                ),
            });
        }
        self.interface_arrival_targets(interface, artifact_label, Some(emitter))
            .map(|(targets, _)| targets)
    }

    async fn admit_interface_checkpoint(
        &self,
        mut inv: InterfaceArtifactAdmission,
        source_movement_seq: Option<u64>,
        delivery_scope_seq: Option<u64>,
        pending: Option<&Movement>,
    ) -> Result<PreparedInterfaceCheckpointAdmission, RuntimeError> {
        let registry_generation = self.inner.registry_generation.load(Ordering::Acquire);
        let admitted_checkpoint = self.admit_run_record_artifact_arrival(&mut inv)?;
        let mut commit_inv = inv.clone();
        let admitted_artifact_bindings = admitted_checkpoint.artifact_bindings.clone();
        let required_verified_artifacts = admitted_checkpoint.required_verified_artifacts.clone();
        let inbound_interface_binding = admitted_checkpoint.inbound_binding.clone();
        let outbound_interface_bindings = admitted_checkpoint.outbound_bindings.clone();
        self.require_known_interface(&inv.interface)?;
        self.require_known_interface_bindings(
            &outbound_interface_bindings
                .iter()
                .map(|binding| binding.interface.clone())
                .collect::<Vec<_>>(),
        )?;
        let spent_run = inv.run.clone();
        let spent_checkpoint = inv.checkpoint.clone().ok_or_else(|| {
            RuntimeError::InterfaceBoundaryFailed(format!(
                "runtime resolved interface '{}' artifact '{}' arrival without a checkpoint before verifier-gated admission",
                inv.interface.as_str(),
                inv.artifact.id.as_str()
            ))
        })?;
        let spent_interface = inv.interface.clone();
        let spent_artifact = inv.artifact.clone();
        let spent_artifact_label = inv.artifact_label.clone();
        let checkpoint_admission_key = CheckpointAdmissionKey {
            run: spent_run.clone(),
            checkpoint: spent_checkpoint.clone(),
            epoch: inv.epoch.0,
        };
        let reservation = self
            .reserve_checkpoint_admission(
                checkpoint_admission_key,
                &spent_artifact,
                &spent_interface,
            )
            .await?;
        let (checkpoint_admission, prior_admissions) = self.checkpoint_admission_plan(
            &inv,
            &spent_checkpoint,
            &admitted_checkpoint.ee,
            &admitted_checkpoint.worker,
            &admitted_checkpoint.metadata_projection,
            &admitted_artifact_bindings,
            &required_verified_artifacts,
            &inbound_interface_binding,
            &outbound_interface_bindings,
        )?;
        let checkpoint_complete = checkpoint_admission.complete;
        let required_artifact_labels = checkpoint_admission.required_artifact_labels.clone();
        let standing_route = required_artifact_labels.is_empty();
        let mut observation = self
            .inner
            .gate
            .admit_interface_boundary(
                inv,
                admitted_checkpoint.ee,
                admitted_checkpoint.worker,
                admitted_checkpoint.grant,
                admitted_checkpoint.metadata_projection,
                admitted_artifact_bindings,
                required_verified_artifacts,
                inbound_interface_binding,
                outbound_interface_bindings.clone(),
                checkpoint_admission,
                source_movement_seq,
                delivery_scope_seq,
                pending,
            )
            .await?;
        let run_directory = self.inner.run_records_dir.lock().unwrap().clone()
            .ok_or_else(|| admission_sources_changed("run-record directory is not configured"))?;
        let run_guard = octopus_file_storage::publication::ReadGuard::try_acquire(
            &[run_directory.join(format!("{}.json", spent_run.as_str()))], &[],
        ).map_err(admission_sources_changed)?;
        let resolver = self.inner.run_grant_resolver.read().unwrap().clone();
        let (record, _) = self.load_run_record_with_hash(&spent_run)
            .map_err(admission_sources_changed)?
            .ok_or_else(|| admission_sources_changed("verified run was removed before admission"))?;
        let source_guard = resolver.as_ref()
            .map(|resolver| resolver.acquire_commit_guard(&record, &observation.admitted_mark_outcomes))
            .transpose().map_err(admission_sources_changed)?.flatten();
        let current = self.admit_run_record_artifact_arrival(&mut commit_inv)?;
        let metadata = &observation.invocation_metadata;
        let runtime = metadata.runtime.as_ref()
            .ok_or_else(|| admission_sources_changed("prepared admission lost its runtime coordinates"))?;
        if current.ee != runtime.ee || current.worker != runtime.worker
            || current.metadata_projection != metadata.grant
            || current.artifact_bindings != metadata.artifact_binding_refs
            || current.required_verified_artifacts != metadata.required_verified_artifacts
            || metadata.checkpoint_binding_policy.as_ref() != Some(&iface::CheckpointBindingPolicySnapshot {
                inbound: current.inbound_binding,
                outbound: current.outbound_bindings,
            })
            || self.inner.registry_generation.load(Ordering::Acquire) != registry_generation
        {
            return Err(admission_sources_changed("the verified checkpoint, bindings or current grant changed"));
        }
        let admitted = AdmittedCheckpointArtifact {
            artifact_label: spent_artifact_label,
            artifact: spent_artifact,
            from_interface: spent_interface,
            invocation_metadata: observation.invocation_metadata.clone(),
        };
        if checkpoint_complete {
            let mut by_label: BTreeMap<String, AdmittedCheckpointArtifact> = prior_admissions
                .into_iter()
                .map(|prior| (prior.artifact_label.clone(), prior))
                .collect();
            by_label.insert(admitted.artifact_label.clone(), admitted.clone());
            let released = if required_artifact_labels.is_empty() {
                vec![admitted.clone()]
            } else {
                required_artifact_labels
                    .iter()
                    .filter_map(|label| by_label.get(label).cloned())
                    .collect()
            };
            let (transfers, refusals) = outbound_artifact_transfers_for_checkpoint(
                &spent_run,
                &spent_checkpoint,
                &released,
                &outbound_interface_bindings,
            );
            observation.outbound_artifact_transfers = transfers;
            observation.refused_outbound_artifact_transfers = refusals;
        }
        Ok(PreparedInterfaceCheckpointAdmission {
            observation,
            reservation,
            admitted,
            checkpoint_complete,
            standing_route,
            source_guard,
            run_guard,
        })
    }

    pub async fn admit_bound_interface_checkpoint(
        &self,
        inv: InterfaceArtifactAdmission,
    ) -> Result<Observation, RuntimeError> {
        self.admit_interface_checkpoint(inv, None, None, None)
            .await
            .map(PreparedInterfaceCheckpointAdmission::commit)
    }

    pub async fn admit_resolved_interface_checkpoint(
        &self,
        target: &InterfaceArrivalTarget,
        artifact: ArtifactRef,
        artifact_label: String,
        pending: Option<&Movement>,
    ) -> Result<PreparedInterfaceCheckpointAdmission, RuntimeError> {
        self.admit_interface_checkpoint(
            InterfaceArtifactAdmission {
                run: target.run.clone(),
                checkpoint: Some(target.checkpoint.clone()),
                epoch: target.epoch,
                interface: target.arriving_interface.clone(),
                artifact,
                artifact_label,
                run_record_hash: Some(target.run_record.hash),
            },
            target.source_movement_seq,
            target.delivery_scope_seq,
            pending,
        )
        .await
    }

    pub fn set_run_authority(&self, run: RunId, grant: Arc<FormedPolicy>, epoch: GrantEpoch) {
        self.inner.gate.set_authority(run, grant, epoch);
    }

    pub fn run_authority_pin(&self, run: &RunId) -> Option<(Arc<FormedPolicy>, GrantEpoch)> {
        self.inner.gate.run_authority_pin(run)
    }

    pub fn admitted_mark_outcomes(&self) -> Vec<Mark> {
        self.inner.gate.admitted_mark_outcomes()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("interface binding required/denied: {0}")]
    InterfaceBindingDenied(InterfaceId),
    #[error("outward output mode denied")]
    OutwardOutputDenied,
    #[error("required verified artifact rejected at admission: {error}")]
    VerifiedArtifactRejected {
        error: VerifiedArtifactError,
        invocation_metadata: Box<iface::InvocationMetadata>,
    },
    #[error("unknown interface in runtime catalog: {0}")]
    UnknownInterface(InterfaceId),
    #[error("interface boundary failed closed: {0}")]
    InterfaceBoundaryFailed(String),
    #[error("run '{}' record changed after its arrival target was resolved: expected hash {}, found {}", run.as_str(), expected.to_hex(), observed.to_hex())]
    ArrivalTargetChanged {
        run: RunId,
        expected: ContentHash,
        observed: ContentHash,
    },
    #[error("delivery refused ({}): {detail}", reason.as_str())]
    DescriptorDrift {
        reason: RefusedReason,
        detail: String,
    },
}

impl RuntimeError {
    pub fn refused_reason(&self) -> RefusedReason {
        match self {
            RuntimeError::InterfaceBindingDenied(_) => RefusedReason::Policy,
            RuntimeError::OutwardOutputDenied => RefusedReason::Policy,
            RuntimeError::VerifiedArtifactRejected { .. } => RefusedReason::VerificationFailed,
            RuntimeError::UnknownInterface(_) => RefusedReason::UnknownInterface,
            RuntimeError::InterfaceBoundaryFailed(_) => RefusedReason::Policy,
            RuntimeError::ArrivalTargetChanged { .. } => RefusedReason::StaleEpoch,
            RuntimeError::DescriptorDrift { reason, .. } => *reason,
        }
    }

    pub fn refusal_detail(&self) -> serde_json::Value {
        match self {
            RuntimeError::InterfaceBindingDenied(interface) => serde_json::json!({
                "message": self.to_string(),
                "interface": interface.as_str(),
            }),
            RuntimeError::UnknownInterface(interface) => serde_json::json!({
                "message": self.to_string(),
                "interface": interface.as_str(),
            }),
            RuntimeError::VerifiedArtifactRejected {
                error,
                invocation_metadata,
            } => serde_json::json!({
                "message": self.to_string(),
                "verification": error.failure_metadata(),
                "invocation_metadata": invocation_metadata,
            }),
            RuntimeError::DescriptorDrift { reason, detail } => serde_json::json!({
                "message": detail,
                "reason_detail": reason.as_str(),
            }),
            RuntimeError::ArrivalTargetChanged { run, expected, observed } => serde_json::json!({
                "message": self.to_string(),
                "reason_detail": "arrival_target_changed",
                "run": run.as_str(),
                "expected_run_record_hash": expected.to_hex(),
                "observed_run_record_hash": observed.to_hex(),
            }),
            RuntimeError::InterfaceBoundaryFailed(message) => serde_json::json!({
                "message": message,
            }),
            RuntimeError::OutwardOutputDenied => serde_json::json!({
                "message": self.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    mod publication_admission {
        include!("../tests/support/publication_admission.rs");
    }

    fn test_verifier_package_pin() -> octopus_ledger::VerifierPackagePin {
        octopus_ledger::VerifierPackagePin {
            descriptor_content: octopus_ledger::ContentHash::of_bytes(b"test verifier descriptor"),
            script_content: octopus_ledger::ContentHash::of_bytes(b"test verifier script"),
        }
    }

    struct TestRunAuthorityResolver {
        gate: Arc<RuntimeGate>,
    }

    impl RunGrantResolver for TestRunAuthorityResolver {
        fn resolve_run_grant(
            &self,
            query: RunGrantQuery<'_>,
        ) -> Result<RunGrantResolution, String> {
            let (grant, epoch) = self
                .gate
                .run_authority_pin(query.run)
                .ok_or_else(|| format!("test run '{}' has no registered authority", query.run))?;
            if epoch != query.epoch {
                return Err(format!(
                    "test run '{}' epoch mismatch: authority={} checkpoint={}",
                    query.run.as_str(),
                    epoch.0,
                    query.epoch.0
                ));
            }
            let grant = grant.as_ref().clone();
            let metadata_projection = iface::GrantProjection::from_formed(&grant);
            RunGrantResolution::new(grant, metadata_projection)
        }
    }

    struct MutableRunGrantResolver {
        current: Arc<RwLock<RunGrantResolution>>,
    }

    impl RunGrantResolver for MutableRunGrantResolver {
        fn resolve_run_grant(
            &self,
            _query: RunGrantQuery<'_>,
        ) -> Result<RunGrantResolution, String> {
            self.current
                .read()
                .map_err(|_| "mutable test grant resolver lock is poisoned".to_string())
                .map(|resolution| resolution.clone())
        }
    }

    struct TestVerifiedArtifactAdmission;

    impl VerifiedArtifactAdmission for TestVerifiedArtifactAdmission {
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
            }
            Ok(Box::new(ReadyVerification::passed(
                Mark::passed_with_class(
                    required.mark.clone(),
                    input.checkpoint,
                    input.run,
                    input.epoch,
                    MarkClass::Success,
                    input.artifact,
                    required.verifier.clone(),
                    test_verifier_package_pin(),
                    serde_json::json!({
                        "verifier": required.verifier.as_str(),
                        "passed": true
                    }),
                ),
            )))
        }
    }

    struct PanickingVerification;

    impl VerifiedArtifactAdmission for PanickingVerification {
        fn prepare_required_verified_artifact(
            &self,
            _required: &RequiredVerifiedArtifact,
            _input: VerificationInput,
        ) -> Result<Box<dyn PreparedVerification>, VerifiedArtifactError> {
            Ok(Box::new(Self))
        }
    }

    impl PreparedVerification for PanickingVerification {
        fn run(self: Box<Self>) -> Result<Mark, VerifiedArtifactError> {
            panic!("fixture verifier execution panicked");
        }
    }

    struct FailFirstThenPassVerifiedArtifactAdmission {
        calls: AtomicU64,
    }

    impl VerifiedArtifactAdmission for FailFirstThenPassVerifiedArtifactAdmission {
        fn prepare_required_verified_artifact(
            &self,
            required: &RequiredVerifiedArtifact,
            input: VerificationInput,
        ) -> Result<Box<dyn PreparedVerification>, VerifiedArtifactError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(Box::new(ReadyVerification::rejected(VerifiedArtifactError::VerifierRejected {
                    mark: required.mark.as_str().to_string(),
                    verifier: required.verifier.as_str().to_string(),
                    artifact: input.artifact.id.as_str().to_string(),
                    verifier_package: None,
                    details: serde_json::json!({ "reason": "first build was red" }),
                })));
            }
            Ok(Box::new(ReadyVerification::passed(
                Mark::passed_with_class(
                    required.mark.clone(),
                    input.checkpoint,
                    input.run,
                    input.epoch,
                    MarkClass::Success,
                    input.artifact,
                    required.verifier.clone(),
                    test_verifier_package_pin(),
                    serde_json::json!({
                        "verifier": required.verifier.as_str(),
                        "passed": true
                    }),
                ),
            )))
        }
    }

    struct BlockingVerifiedArtifactAdmission {
        started: Arc<tokio::sync::Notify>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
    }

    struct BlockingPreparedVerification {
        started: Arc<tokio::sync::Notify>,
        release: Arc<(Mutex<bool>, std::sync::Condvar)>,
        mark: Mark,
    }

    impl PreparedVerification for BlockingPreparedVerification {
        fn run(self: Box<Self>) -> Result<Mark, VerifiedArtifactError> {
            self.started.notify_one();
            let (released, wake) = self.release.as_ref();
            let mut released = released.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            Ok(self.mark)
        }
    }

    impl VerifiedArtifactAdmission for BlockingVerifiedArtifactAdmission {
        fn prepare_required_verified_artifact(
            &self,
            required: &RequiredVerifiedArtifact,
            input: VerificationInput,
        ) -> Result<Box<dyn PreparedVerification>, VerifiedArtifactError> {
            Ok(Box::new(BlockingPreparedVerification {
                started: self.started.clone(),
                release: self.release.clone(),
                mark: Mark::passed_with_class(
                    required.mark.clone(),
                    input.checkpoint,
                    input.run,
                    input.epoch,
                    MarkClass::Success,
                    input.artifact,
                    required.verifier.clone(),
                    test_verifier_package_pin(),
                    serde_json::json!({ "passed": true }),
                ),
            }))
        }
    }

    fn grant_with_interfaces(interface_names: &[&str], mark_names: &[&str]) -> Arc<FormedPolicy> {
        let interfaces = CapSet::from_allowed(interface_names.iter().map(|s| InterfaceId::new(*s)));
        let mark_outcomes = CapSet::from_allowed(mark_names.iter().map(|s| MarkId::new(*s)));
        let ctx = ContextGrant {
            tools: CapSet::from_allowed(std::iter::empty()),
            interfaces,
            wek: CapSet::from_allowed(std::iter::empty()),
            mark_outcomes,
            hard_blocks_tools: DenySet::default(),
            interface_policy: Default::default(),
            ..Default::default()
        };
        Arc::new(FormedPolicy {
            boundary: ctx.clone(),
            context: ctx,
            pin: FormationPin::default(),
        })
    }

    static RUN_RECORD_TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_test_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "octopus-runtime-{label}-{}-{}",
            std::process::id(),
            RUN_RECORD_TEST_DIR_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).expect("test temp dir created");
        dir
    }

    fn runtime_with_interface_catalog() -> Runtime {
        let services = RuntimeServices {
            interface_catalog: Arc::new(RuntimeInterfaceCatalog::from_classes([
                (
                    InterfaceId::new("world_test"),
                    InterfaceClass::new(
                        InterfaceType::World,
                        InterfaceRuntimeDisposition::ExternalReader,
                    )
                    .expect("world test interface class forms"),
                ),
                (
                    InterfaceId::new("world_client"),
                    InterfaceClass::new(
                        InterfaceType::World,
                        InterfaceRuntimeDisposition::ExternalReader,
                    )
                    .expect("world client interface class forms"),
                ),
            ])),
        };
        let runtime = Runtime::open(services).unwrap();
        runtime.set_run_grant_resolver(Arc::new(TestRunAuthorityResolver {
            gate: runtime.inner.gate.clone(),
        }));
        runtime.set_verified_artifact_admission(Arc::new(TestVerifiedArtifactAdmission));
        runtime
    }

    fn literal_boundary_run_record_fixture(
        dir: &std::path::Path,
        run: &RunId,
        interface: &InterfaceId,
        grant: &Arc<FormedPolicy>,
    ) -> std::path::PathBuf {
        literal_boundary_run_record_fixture_at_checkpoint(
            dir,
            run,
            interface,
            &CheckpointId::new("c1"),
            grant,
        )
    }

    fn literal_boundary_run_record_fixture_at_checkpoint(
        dir: &std::path::Path,
        run: &RunId,
        interface: &InterfaceId,
        checkpoint: &CheckpointId,
        grant: &Arc<FormedPolicy>,
    ) -> std::path::PathBuf {
        literal_boundary_run_record_fixture_at_checkpoint_with_outbound(
            dir,
            run,
            interface,
            checkpoint,
            grant,
            &[],
        )
    }

    fn literal_boundary_run_record_fixture_at_checkpoint_with_outbound(
        dir: &std::path::Path,
        run: &RunId,
        interface: &InterfaceId,
        checkpoint: &CheckpointId,
        _grant: &Arc<FormedPolicy>,
        outbound_bindings: &[InterfaceId],
    ) -> std::path::PathBuf {
        let path = dir.join(format!("{}.json", run.as_str()));
        let inbound_binding = AuthoredInterfaceBinding::all(interface.clone());
        let outbound_bindings = outbound_bindings
            .iter()
            .cloned()
            .map(AuthoredInterfaceBinding::all)
            .collect::<Vec<_>>();
        let record = serde_json::json!({
            "id": run,
            "ee": desc::EeId::new("ee1"),
            "worker": InterfaceId::new("system_worker_flash"),
            "trace": format!("trace:{}", run.as_str()),
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": checkpoint,
                "run": run,
                "trace": format!("trace:{}:{}", run.as_str(), checkpoint.as_str()),
                "epoch": 0,
                "interface_binding": inbound_binding,
                "outbound_interface_bindings": outbound_bindings,
                "artifact_bindings": [],
            }],
            "active_checkpoints": [checkpoint],
        });
        write_literal_run_record_fixture(&path, &record).expect("test run record fixture writes");
        path
    }

    fn write_literal_run_record_fixture(
        path: &std::path::Path,
        record: &serde_json::Value,
    ) -> std::io::Result<()> {
        let bytes =
            serde_json::to_vec_pretty(record).expect("test-authored run record JSON must encode");
        std::fs::write(path, bytes)
    }

    fn add_pre_existing_checkpoint(path: &std::path::Path, checkpoint_id: &str) {
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("run record read"))
                .expect("run record parses");
        let object = record.as_object_mut().expect("run record is an object");
        let checkpoints = object
            .get_mut("checkpoints")
            .and_then(serde_json::Value::as_array_mut)
            .expect("run record has checkpoints[]");
        let mut checkpoint = checkpoints
            .first()
            .cloned()
            .expect("run record fixture has a checkpoint to clone");
        checkpoint
            .as_object_mut()
            .expect("checkpoint is object")
            .insert(
                "id".into(),
                serde_json::Value::String(checkpoint_id.to_string()),
            );
        checkpoints.insert(0, checkpoint);
        write_literal_run_record_fixture(path, &record)
            .expect("test run record with pre-existing checkpoint writes");
    }

    fn add_second_active_checkpoint(path: &std::path::Path, checkpoint_id: &str) {
        add_second_active_checkpoint_for_interface(
            path,
            checkpoint_id,
            "world_test",
            serde_json::Value::Null,
        );
    }

    fn add_second_active_checkpoint_for_interface(
        path: &std::path::Path,
        checkpoint_id: &str,
        interface_id: &str,
        required_verified_artifacts: serde_json::Value,
    ) {
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("run record read"))
                .expect("run record parses");
        let object = record.as_object_mut().expect("run record is an object");
        let checkpoints = object
            .get_mut("checkpoints")
            .and_then(serde_json::Value::as_array_mut)
            .expect("run record has checkpoints[]");
        let mut checkpoint = checkpoints
            .first()
            .cloned()
            .expect("run record fixture has a checkpoint to clone");
        let checkpoint_object = checkpoint.as_object_mut().expect("checkpoint is object");
        checkpoint_object.insert(
            "id".into(),
            serde_json::Value::String(checkpoint_id.to_string()),
        );
        checkpoint_object.insert(
            "interface_binding".into(),
            serde_json::to_value(AuthoredInterfaceBinding::all(InterfaceId::new(
                interface_id,
            )))
            .expect("test interface binding encodes"),
        );
        if !required_verified_artifacts.is_null() {
            checkpoint_object.insert(
                "required_verified_artifacts".into(),
                required_verified_artifacts,
            );
        }
        checkpoints.push(checkpoint);
        object.insert(
            "active_checkpoints".into(),
            serde_json::json!(["c1", checkpoint_id]),
        );
        write_literal_run_record_fixture(path, &record)
            .expect("test run record with multiple active checkpoints writes");
    }

    fn set_required_verified_artifacts(path: &std::path::Path, artifacts: serde_json::Value) {
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("run record read"))
                .expect("run record parses");
        let checkpoints = record
            .as_object_mut()
            .expect("run record is an object")
            .get_mut("checkpoints")
            .and_then(serde_json::Value::as_array_mut)
            .expect("run record has checkpoints[]");
        checkpoints
            .first_mut()
            .expect("run record fixture has a checkpoint")
            .as_object_mut()
            .expect("checkpoint is object")
            .insert("required_verified_artifacts".into(), artifacts);
        write_literal_run_record_fixture(path, &record)
            .expect("test run record with required_verified_artifacts writes");
    }

    fn test_artifact_ref(label: &str, bytes: &[u8]) -> ArtifactRef {
        ArtifactRef {
            id: ArtifactId::new(label),
            content: ContentHash::of_bytes(bytes),
        }
    }

    fn interface_artifact_admission() -> InterfaceArtifactAdmission {
        InterfaceArtifactAdmission {
            run: RunId::new("r1"),
            checkpoint: None,
            epoch: GrantEpoch(0),
            interface: InterfaceId::new("world_test"),
            artifact: test_artifact_ref("turn.input", br#"{"hello":true}"#),
            artifact_label: "turn.input".into(),
            run_record_hash: None,
        }
    }

    #[tokio::test]
    async fn checkpoint_required_verified_artifact_admits_mark_from_arrival() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("required-verified-artifact");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &["coder.build.ok"]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        set_required_verified_artifacts(
            &record_path,
            serde_json::json!([{
                "mark": "coder.build.ok",
                "artifact_label": "turn.input",
                "verifier": "test.required",
                "params": {}
            }]),
        );

        let obs = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect("arrival closes the required verified artifact");

        assert_eq!(obs.admitted_mark_outcomes.len(), 1);
        assert_eq!(
            obs.admitted_mark_outcomes[0].name,
            MarkId::new("coder.build.ok")
        );
        assert_eq!(
            obs.data
                .pointer("/metadata/required_verified_artifacts/0/mark")
                .and_then(serde_json::Value::as_str),
            Some("coder.build.ok")
        );
    }

    #[tokio::test]
    async fn checkpoint_admits_only_matching_required_verified_artifact() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("matching-required-verified-artifact");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(
            &["world_test", "world_client"],
            &["coder.build.ok", "ocr.text.contains"],
        );
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture_at_checkpoint_with_outbound(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &CheckpointId::new("c1"),
            &grant,
            &[InterfaceId::new("world_client")],
        );
        set_required_verified_artifacts(
            &record_path,
            serde_json::json!([
                {
                    "mark": "coder.build.ok",
                    "artifact_label": "turn.input",
                    "verifier": "test.required",
                    "params": {}
                },
                {
                    "mark": "ocr.text.contains",
                    "artifact_label": "screenshot.ocr",
                    "verifier": "test.required",
                    "params": {"expected_text": "READY"}
                }
            ]),
        );

        let obs = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect("arrival admits the matching required verified artifact only");

        assert_eq!(obs.admitted_mark_outcomes.len(), 1);
        assert_eq!(
            obs.admitted_mark_outcomes[0].name,
            MarkId::new("coder.build.ok")
        );
        assert!(
            obs.outbound_artifact_transfers.is_empty(),
            "a partial checkpoint obligation cannot release one artifact early"
        );
        assert_eq!(
            obs.invocation_metadata
                .checkpoint_admission
                .as_ref()
                .map(|admission| admission.complete),
            Some(false)
        );

        let first_arrival = Movement {
            seq: Seq(0),
            lamport: Lamport(1),
            recorded_at_unix_ms: 1,
            trace: TraceId("checkpoint-obligations".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_test")),
            to: Endpoint::Interface(
                obs.invocation_metadata
                    .runtime
                    .as_ref()
                    .unwrap()
                    .worker
                    .clone(),
            ),
            artifact: Some(test_artifact_ref("turn.input", br#"{"hello":true}"#)),
            invocation_metadata: Some(obs.invocation_metadata.clone().into()),
        };
        let first_mark = Movement {
            seq: Seq(1),
            lamport: Lamport(2),
            recorded_at_unix_ms: 2,
            trace: first_arrival.trace.clone(),
            kind: MovementKind::MarkOutcomeAdmitted(obs.admitted_mark_outcomes[0].name.clone()),
            from: first_arrival.from.clone(),
            to: first_arrival.to.clone(),
            artifact: first_arrival.artifact.clone(),
            invocation_metadata: Some(obs.admitted_mark_outcomes[0].clone().into()),
        };
        let unmarked_reboot = runtime_with_interface_catalog();
        unmarked_reboot.set_run_records_dir(dir.clone());
        unmarked_reboot.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        unmarked_reboot
            .reconstruct_spent_checkpoints_from_movements([&first_arrival])
            .expect("an unmarked crash tail reconstructs as an open requirement");
        unmarked_reboot
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect("the exact required artifact remains retryable until its mark is durable");
        let rebooted = runtime_with_interface_catalog();
        rebooted.set_run_records_dir(dir.clone());
        rebooted.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        rebooted
            .reconstruct_spent_checkpoints_from_movements([&first_arrival, &first_mark])
            .expect("partial checkpoint obligation reconstructs from ledger evidence");

        let mut edited: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
        edited["interface_state"] =
            serde_json::json!({"world_test": {"presentation": "changed independently"}});
        std::fs::write(&record_path, serde_json::to_vec_pretty(&edited).unwrap()).unwrap();
        let final_obligation = rebooted
            .admit_bound_interface_checkpoint(InterfaceArtifactAdmission {
                run: RunId::new("r1"),
                checkpoint: None,
                epoch: GrantEpoch(0),
                interface: InterfaceId::new("world_test"),
                artifact: test_artifact_ref("screenshot.ocr", b"READY"),
                artifact_label: "screenshot.ocr".into(),
                run_record_hash: None,
            })
            .await
            .expect("the second exact requirement completes the checkpoint");
        let released_ids: BTreeSet<ArtifactId> = final_obligation
            .outbound_artifact_transfers
            .iter()
            .map(|transfer| transfer.artifact.id.clone())
            .collect();
        assert_eq!(
            released_ids,
            BTreeSet::from([
                ArtifactId::new("turn.input"),
                ArtifactId::new("screenshot.ocr"),
            ]),
            "completion releases the full checkpoint-scoped artifact set"
        );
    }

    struct FixedContextArtifactResolver;

    impl ContextArtifactResolver for FixedContextArtifactResolver {
        fn resolve_artifact_content(&self, id: &ArtifactId) -> Result<ContentHash, String> {
            Ok(ContentHash::of_bytes(id.as_str().as_bytes()))
        }
    }

    fn grant_with_artifact_access(
        interface_names: &[&str],
        access: &[(&str, ArtifactAccess)],
    ) -> Arc<FormedPolicy> {
        let interfaces = CapSet::from_allowed(interface_names.iter().map(|s| InterfaceId::new(*s)));
        let ctx = ContextGrant {
            interfaces,
            artifact_verbs: CapSet::from_allowed(ArtifactVerb::ALL),
            artifact_access: access
                .iter()
                .map(|(id, access)| (ArtifactId::new(*id), *access))
                .collect(),
            ..Default::default()
        };
        Arc::new(FormedPolicy {
            boundary: ctx.clone(),
            context: ctx,
            pin: FormationPin::default(),
        })
    }

    fn run_record_fixture_with_artifact_bindings(
        dir: &std::path::Path,
        run: &RunId,
        interface: &InterfaceId,
        _grant: &Arc<FormedPolicy>,
        bindings: serde_json::Value,
    ) -> std::path::PathBuf {
        let path = dir.join(format!("{}.json", run.as_str()));
        let checkpoint = CheckpointId::new("c1");
        let inbound_binding = AuthoredInterfaceBinding::all(interface.clone());
        let record = serde_json::json!({
            "id": run,
            "ee": desc::EeId::new("ee1"),
            "worker": InterfaceId::new("system_worker_flash"),
            "trace": format!("trace:{}", run.as_str()),
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": checkpoint,
                "run": run,
                "trace": format!("trace:{}:{}", run.as_str(), checkpoint.as_str()),
                "epoch": 0,
                "interface_binding": inbound_binding,
                "outbound_interface_bindings": [],
                "artifact_bindings": bindings,
            }],
            "active_checkpoints": [checkpoint],
        });
        write_literal_run_record_fixture(&path, &record).expect("test run record fixture writes");
        path
    }

    #[tokio::test]
    async fn arrival_materializes_only_read_granted_bindings_fail_closed() {
        let runtime = runtime_with_interface_catalog();
        runtime.set_context_artifact_resolver(Arc::new(FixedContextArtifactResolver));
        let dir = temp_test_dir("readable-binding-materialization");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_artifact_access(
            &["world_test"],
            &[
                ("artifact-readable", ArtifactAccess::read_only()),
                (
                    "artifact-writeonly",
                    ArtifactAccess {
                        read: false,
                        change: false,
                        create: true,
                        delete: false,
                    },
                ),
            ],
        );
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        run_record_fixture_with_artifact_bindings(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
            serde_json::json!([
                { "id": "artifact-readable" },
                { "id": "artifact-writeonly", "access": { "read": false, "create": true } }
            ]),
        );

        let obs = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect("arrival admits");

        let materialized: Vec<String> = obs
            .admitted_artifact_bindings
            .iter()
            .map(|artifact| artifact.id.as_str().to_string())
            .collect();
        assert_eq!(
            materialized,
            vec!["artifact-readable".to_string()],
            "POLICY.md §4: only a read-granted binding is materialized into context; a create-only binding surfaces no content (fail-closed)"
        );
    }

    #[tokio::test]
    async fn checkpoint_rejects_arrival_that_matches_no_required_verified_artifact() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("no-matching-required-verified-artifact");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &["coder.build.ok"]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        set_required_verified_artifacts(
            &record_path,
            serde_json::json!([{
                "mark": "coder.build.ok",
                "artifact_label": "coder.build.result",
                "verifier": "test.required",
                "params": {}
            }]),
        );

        let err = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect_err("non-matching artifact cannot pass a required verified artifact gate");

        assert!(
            err.to_string().contains("does not match any requirement"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn failed_verifier_leaves_checkpoint_open_for_retry() {
        let runtime = runtime_with_interface_catalog();
        runtime.set_verified_artifact_admission(Arc::new(PanickingVerification));
        let dir = temp_test_dir("verifier-fail-keeps-checkpoint-open");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &["coder.build.ok"]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        set_required_verified_artifacts(
            &record_path,
            serde_json::json!([{
                "mark": "coder.build.ok",
                "artifact_label": "coder.build.result",
                "verifier": "build_success",
                "params": {}
            }]),
        );

        let build_result = || InterfaceArtifactAdmission {
            run: RunId::new("r1"),
            checkpoint: None,
            epoch: GrantEpoch(0),
            interface: InterfaceId::new("world_test"),
            artifact: test_artifact_ref("coder.build.result", br#"{"build":"result"}"#),
            artifact_label: "coder.build.result".into(),
            run_record_hash: None,
        };

        let interrupted = runtime.admit_bound_interface_checkpoint(build_result()).await.unwrap_err();
        assert_eq!(interrupted.refused_reason(), RefusedReason::RegistryUnstable);
        assert!(ledger::DeliveryVerdict::refused(interrupted.refused_reason(), interrupted.refusal_detail()).awaits_current_source());
        assert!(runtime.admitted_mark_outcomes().is_empty());
        runtime.set_verified_artifact_admission(Arc::new(FailFirstThenPassVerifiedArtifactAdmission {
            calls: AtomicU64::new(0),
        }));
        let err = runtime
            .admit_bound_interface_checkpoint(build_result())
            .await
            .expect_err("a failed verifier must refuse the arrival");
        assert_eq!(
            err.refused_reason(),
            RefusedReason::VerificationFailed,
            "a failed verifier surfaces as verification_failed, got {err:?}"
        );
        assert!(!ledger::DeliveryVerdict::refused(err.refused_reason(), err.refusal_detail()).awaits_current_source());
        assert!(
            runtime.admitted_mark_outcomes().is_empty(),
            "a failed verifier must not admit a mark"
        );

        let obs = runtime
            .admit_bound_interface_checkpoint(build_result())
            .await
            .expect("a verifier failure must not spend the checkpoint; retry stays possible");
        assert!(obs.ok);
        assert_eq!(
            obs.admitted_mark_outcomes.len(),
            1,
            "the retry closes the required verified artifact with its mark"
        );

        let spent = runtime
            .admit_bound_interface_checkpoint(build_result())
            .await
            .expect_err("only a verified admission spends the checkpoint");
        assert_eq!(
            spent.refused_reason(),
            RefusedReason::CheckpointSpent,
            "after the verifier passes the checkpoint is spent, got {spent:?}"
        );
    }

    #[tokio::test]
    async fn concurrent_arrival_cannot_verify_or_mark_one_checkpoint_twice() {
        let runtime = runtime_with_interface_catalog();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        runtime.set_verified_artifact_admission(Arc::new(BlockingVerifiedArtifactAdmission {
            started: started.clone(),
            release: release.clone(),
        }));
        let dir = temp_test_dir("concurrent-checkpoint-admission");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &["coder.build.ok"]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        set_required_verified_artifacts(
            &record_path,
            serde_json::json!([{
                "mark": "coder.build.ok",
                "artifact_label": "coder.build.result",
                "verifier": "build_success",
                "params": {}
            }]),
        );
        let build_result = || InterfaceArtifactAdmission {
            run: RunId::new("r1"),
            checkpoint: None,
            epoch: GrantEpoch(0),
            interface: InterfaceId::new("world_test"),
            artifact: test_artifact_ref("coder.build.result", br#"{"build":"result"}"#),
            artifact_label: "coder.build.result".into(),
            run_record_hash: None,
        };

        let first_runtime = runtime.clone();
        let first = tokio::spawn(async move {
            first_runtime
                .admit_bound_interface_checkpoint(build_result())
                .await
        });
        started.notified().await;

        let second_runtime = runtime.clone();
        let second = tokio::spawn(async move {
            second_runtime
                .admit_bound_interface_checkpoint(build_result())
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !second.is_finished(),
            "a concurrent edge waits for the exact checkpoint reservation instead of being lost"
        );
        assert!(runtime.admitted_mark_outcomes().is_empty());

        let (released, wake) = release.as_ref();
        *released.lock().unwrap() = true;
        wake.notify_all();
        let admitted = first
            .await
            .expect("the first admission task joins")
            .expect("the reserved artifact admits after its verifier completes");
        assert_eq!(admitted.admitted_mark_outcomes.len(), 1);
        let second = second
            .await
            .expect("the waiting admission task joins")
            .expect_err("the repeated label observes the completed checkpoint after waking");
        assert_eq!(second.refused_reason(), RefusedReason::CheckpointSpent);
        assert_eq!(runtime.admitted_mark_outcomes().len(), 1);
    }

    #[tokio::test]
    async fn concurrent_distinct_requirements_wait_and_complete_one_checkpoint_together() {
        let runtime = runtime_with_interface_catalog();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        runtime.set_verified_artifact_admission(Arc::new(BlockingVerifiedArtifactAdmission {
            started: started.clone(),
            release: release.clone(),
        }));
        let dir = temp_test_dir("concurrent-distinct-checkpoint-requirements");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(
            &["world_test", "world_client"],
            &["coder.build.ok", "ocr.text.contains"],
        );
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture_at_checkpoint_with_outbound(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &CheckpointId::new("c1"),
            &grant,
            &[InterfaceId::new("world_client")],
        );
        set_required_verified_artifacts(
            &record_path,
            serde_json::json!([
                {
                    "mark": "coder.build.ok",
                    "artifact_label": "coder.build.result",
                    "verifier": "build_success",
                    "params": {}
                },
                {
                    "mark": "ocr.text.contains",
                    "artifact_label": "screenshot.ocr",
                    "verifier": "ocr_text_contains",
                    "params": {"expected_text": "READY"}
                }
            ]),
        );

        let first_runtime = runtime.clone();
        let first = tokio::spawn(async move {
            first_runtime
                .admit_bound_interface_checkpoint(InterfaceArtifactAdmission {
                    run: RunId::new("r1"),
                    checkpoint: None,
                    epoch: GrantEpoch(0),
                    interface: InterfaceId::new("world_test"),
                    artifact: test_artifact_ref("coder.build.result", b"green"),
                    artifact_label: "coder.build.result".into(),
                    run_record_hash: None,
                })
                .await
        });
        started.notified().await;

        let second_runtime = runtime.clone();
        let second = tokio::spawn(async move {
            second_runtime
                .admit_bound_interface_checkpoint(InterfaceArtifactAdmission {
                    run: RunId::new("r1"),
                    checkpoint: None,
                    epoch: GrantEpoch(0),
                    interface: InterfaceId::new("world_test"),
                    artifact: test_artifact_ref("screenshot.ocr", b"READY"),
                    artifact_label: "screenshot.ocr".into(),
                    run_record_hash: None,
                })
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !second.is_finished(),
            "the second requirement waits instead of being refused or racing ahead"
        );

        let (released, wake) = release.as_ref();
        *released.lock().unwrap() = true;
        wake.notify_all();
        let first = first
            .await
            .expect("the first requirement task joins")
            .expect("the first requirement admits");
        let second = second
            .await
            .expect("the second requirement task joins")
            .expect("the waiting distinct requirement admits after the first commits");

        assert!(first.outbound_artifact_transfers.is_empty());
        assert_eq!(second.outbound_artifact_transfers.len(), 2);
        assert_eq!(runtime.admitted_mark_outcomes().len(), 2);
    }

    #[tokio::test]
    async fn required_verified_artifact_on_other_interface_checkpoint_does_not_intercept_arrival() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("required-artifact-other-interface-checkpoint");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test", "world_client"], &["coder.build.ok"]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        add_second_active_checkpoint_for_interface(
            &record_path,
            "c2",
            "world_client",
            serde_json::json!([{
                "mark": "coder.build.ok",
                "artifact_label": "coder.build.result",
                "verifier": "test.required",
                "params": {}
            }]),
        );

        let obs = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect("world_test arrival uses only the world_test checkpoint");

        assert!(
            obs.admitted_mark_outcomes.is_empty(),
            "requirement on the world_client checkpoint must not run for world_test arrival"
        );
    }

    #[tokio::test]
    async fn interface_artifact_arrival_reaches_only_the_folder_it_addressed() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("interface-arrival-fanout");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test", "world_client"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        runtime.set_run_authority(RunId::new("r2"), grant.clone(), GrantEpoch(0));
        literal_boundary_run_record_fixture_at_checkpoint(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &CheckpointId::new("c1"),
            &grant,
        );
        literal_boundary_run_record_fixture_at_checkpoint(
            &dir,
            &RunId::new("r2"),
            &InterfaceId::new("world_client"),
            &CheckpointId::new("c2"),
            &grant,
        );

        let admitted_artifact = test_artifact_ref("turn.input", br#"{"hello":true}"#);

        let targets = runtime
            .resolve_interface_arrival_targets(&InterfaceId::new("world_test"), "turn.input")
            .expect("run JSON resolves the checkpoint pinning the arriving interface folder");
        let mut admitted = Vec::new();
        for target in targets {
            runtime
                .admit_bound_interface_checkpoint(InterfaceArtifactAdmission {
                    run: target.run.clone(),
                    checkpoint: Some(target.checkpoint.clone()),
                    epoch: target.epoch,
                    interface: InterfaceId::new("world_test"),
                    artifact: admitted_artifact.clone(),
                    artifact_label: "turn.input".into(),
                    run_record_hash: Some(target.run_record.hash),
                })
                .await
                .expect("resolved checkpoint admits through the runtime gate");
            admitted.push(target.run.as_str().to_string());
        }

        assert_eq!(
            admitted,
            vec!["r1".to_string()],
            "the artifact reaches only the checkpoint pinning the arriving interface folder, never an unrelated folder in another run"
        );
        assert!(
            runtime.admitted_mark_outcomes().is_empty(),
            "runtime delivery does not admit marks from synchronous interface return state"
        );
        let r1_record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("r1.json")).expect("r1 record reads"))
                .expect("r1 record parses");
        assert_eq!(
            r1_record.get("active_checkpoints"),
            Some(&serde_json::json!(["c1"])),
            "the addressed checkpoint stays active until interface-authored JSON changes it"
        );
        let r2_record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("r2.json")).expect("r2 record reads"))
                .expect("r2 record parses");
        assert_eq!(
            r2_record.get("active_checkpoints"),
            Some(&serde_json::json!(["c2"])),
            "a checkpoint on a different interface folder is never touched by this arrival"
        );
    }

    #[tokio::test]
    async fn stale_target_reforms_enforcement_and_metadata_as_one_resolution() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("stale-target-metadata");
        runtime.set_run_records_dir(dir.clone());

        let mut broad_grant = grant_with_interfaces(&["world_test"], &[]).as_ref().clone();
        let broad_tools = CapSet::from_allowed([desc::ToolId::new("old.write")]);
        broad_grant.boundary.tools = broad_tools.clone();
        broad_grant.context.tools = broad_tools;
        let broad_projection = iface::GrantProjection::from_formed(&broad_grant)
            .with_role_artifacts(vec!["role.old".to_string()]);
        let current = Arc::new(RwLock::new(
            RunGrantResolution::new(broad_grant.clone(), broad_projection)
                .expect("broad grant resolution is internally consistent"),
        ));
        runtime.set_run_grant_resolver(Arc::new(MutableRunGrantResolver {
            current: current.clone(),
        }));

        let broad_grant = Arc::new(broad_grant);
        literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &broad_grant,
        );
        let artifact = test_artifact_ref("turn.input", br#"{"hello":true}"#);
        let target = runtime
            .resolve_interface_arrival_targets(&InterfaceId::new("world_test"), "turn.input")
            .expect("broad target resolves")
            .into_iter()
            .next()
            .expect("world_test target exists");

        let narrow_grant = grant_with_interfaces(&["world_test"], &[]).as_ref().clone();
        let narrow_projection = iface::GrantProjection::from_formed(&narrow_grant)
            .with_role_artifacts(vec!["role.new".to_string()]);
        *current
            .write()
            .expect("mutable test grant resolver write lock") =
            RunGrantResolution::new(narrow_grant, narrow_projection)
                .expect("narrow grant resolution is internally consistent");
        runtime
            .replace_interface_catalog_classes([
                (
                    InterfaceId::new("world_test"),
                    InterfaceClass::new(
                        InterfaceType::World,
                        InterfaceRuntimeDisposition::ExternalReader,
                    )
                    .expect("world test interface class forms"),
                ),
                (
                    InterfaceId::new("world_client"),
                    InterfaceClass::new(
                        InterfaceType::World,
                        InterfaceRuntimeDisposition::ExternalReader,
                    )
                    .expect("world client interface class forms"),
                ),
            ])
            .expect("registry generation advances");

        let path = dir.join("r1.json");
        let original_bytes = std::fs::read(&path).unwrap();
        let mut retained = target.clone();
        retained.delivery_scope_seq = Some(41);
        let assert_reason = |error: RuntimeError, reason: RefusedReason| {
            assert_eq!(error.refused_reason(), reason);
            assert_eq!(ledger::DeliveryVerdict::refused(reason, error.refusal_detail()).awaits_current_source(), reason == RefusedReason::RegistryUnstable);
        };
        for read_failure in [true, false] {
            if read_failure {
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(&path).unwrap();
            } else {
                std::fs::write(&path, b"{\"incomplete\":").unwrap();
            }
            assert_reason(runtime.refresh_retained_interface_arrival_target(&retained).unwrap_err(), RefusedReason::RegistryUnstable);
            assert_reason(runtime.admit_resolved_interface_checkpoint(&retained, artifact.clone(), "turn.input".into(), None).await.err().unwrap(), RefusedReason::RegistryUnstable);
            if read_failure { std::fs::remove_dir(&path).unwrap(); }
            std::fs::write(&path, &original_bytes).unwrap();
            let resumed = runtime.refresh_retained_interface_arrival_target(&retained).unwrap();
            assert_eq!(resumed.delivery_scope_seq, retained.delivery_scope_seq);
            assert_eq!(resumed.run_record.hash, retained.run_record.hash);
            let prepared = runtime.admit_resolved_interface_checkpoint(&resumed, artifact.clone(), "turn.input".into(), None).await.unwrap();
            prepared.validate_commit().unwrap();
            drop(prepared);
        }
        std::fs::remove_file(&path).unwrap();
        assert_reason(runtime.refresh_retained_interface_arrival_target(&retained).unwrap_err(), RefusedReason::StaleEpoch);
        std::fs::write(&path, &original_bytes).unwrap();
        let mut record: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record["checkpoints"][0]["epoch"] = serde_json::json!(1);
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert_reason(runtime.refresh_retained_interface_arrival_target(&retained).unwrap_err(), RefusedReason::StaleEpoch);
        record["checkpoints"][0]["epoch"] = serde_json::json!(0);
        record["active_checkpoints"] = serde_json::json!(["c2"]);
        record["checkpoints"][0]["id"] = serde_json::json!("c2");
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert_reason(runtime.refresh_retained_interface_arrival_target(&retained).unwrap_err(), RefusedReason::StaleEpoch);
        let changed = runtime.admit_resolved_interface_checkpoint(&target, artifact.clone(), "turn.input".into(), None).await.err().unwrap();
        assert_eq!(changed.refused_reason(), RefusedReason::StaleEpoch);
        assert_eq!(changed.refusal_detail()["reason_detail"], "arrival_target_changed");
        let target = runtime.resolve_interface_arrival_targets(&InterfaceId::new("world_test"), "turn.input").unwrap().remove(0);
        assert_eq!(target.checkpoint.as_str(), "c2");

        let observation = runtime
            .admit_resolved_interface_checkpoint(&target, artifact, "turn.input".to_string(), None)
            .await
            .expect("stale target is re-formed before admission")
            .commit();
        assert_eq!(
            observation.data.pointer("/metadata/grant/tools"),
            Some(&serde_json::json!([]))
        );
        assert_eq!(
            observation.data.pointer("/metadata/grant/role_artifacts"),
            Some(&serde_json::json!(["role.new"]))
        );
    }

    #[tokio::test]
    async fn many_active_checkpoints_on_one_interface_fan_out_to_every_edge() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("interface-arrival-one-run-many-checkpoints");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        add_second_active_checkpoint(&record_path, "c2");

        let admitted_artifact = test_artifact_ref("turn.input", br#"{"hello":true}"#);

        let targets = runtime
            .resolve_interface_arrival_targets(&InterfaceId::new("world_test"), "turn.input")
            .expect("every active checkpoint on the arriving interface is a target");
        assert_eq!(
            targets.len(),
            2,
            "the artifact fans out into both active checkpoints on world_test"
        );

        let mut admitted: Vec<String> = Vec::new();
        for target in &targets {
            runtime
                .admit_bound_interface_checkpoint(InterfaceArtifactAdmission {
                    run: target.run.clone(),
                    checkpoint: Some(target.checkpoint.clone()),
                    epoch: target.epoch,
                    interface: InterfaceId::new("world_test"),
                    artifact: admitted_artifact.clone(),
                    artifact_label: "turn.input".into(),
                    run_record_hash: Some(target.run_record.hash),
                })
                .await
                .expect("each fanned-out checkpoint admits its own edge");
            admitted.push(target.checkpoint.as_str().to_string());
        }
        admitted.sort();
        assert_eq!(
            admitted,
            vec!["c1".to_string(), "c2".to_string()],
            "one edge lands per active checkpoint; no label picks a single winner"
        );
    }

    #[tokio::test]
    async fn arrival_on_a_folder_the_checkpoint_does_not_pin_fails_before_boundary_admission() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("interface-folder-mismatch");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test", "world_client"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        let inv = InterfaceArtifactAdmission {
            run: RunId::new("r1"),
            checkpoint: None,
            epoch: GrantEpoch(0),
            interface: InterfaceId::new("world_client"),
            artifact: test_artifact_ref("turn.input", br#"{"hello":true}"#),
            artifact_label: "turn.input".into(),
            run_record_hash: None,
        };

        let err = runtime
            .admit_bound_interface_checkpoint(inv)
            .await
            .expect_err(
            "an arrival on a folder no active checkpoint pins must fail before boundary admission",
        );
        assert_eq!(
            err.refused_reason(),
            RefusedReason::NoBinding,
            "routing outside the authored interface folder must be a fail-closed run JSON admission error, got {err:?}"
        );
        assert!(
            runtime.admitted_mark_outcomes().is_empty(),
            "no mark may be admitted when checkpoint admission failed before boundary admission"
        );
    }

    #[tokio::test]
    async fn standing_checkpoint_admits_each_distinct_artifact_once() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("checkpoint-standing-route");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );

        let first = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect("the first artifact is delivered to the JSON-authored checkpoint");
        assert!(first.ok);

        let err = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect_err("one checkpoint generation admits one exact artifact once");
        assert_eq!(err.refused_reason(), RefusedReason::CheckpointSpent, "got {err:?}");

        let mut next = interface_artifact_admission();
        next.artifact = test_artifact_ref("turn.input:second", br#"{"hello":"again"}"#);
        let second = runtime
            .admit_bound_interface_checkpoint(next.clone())
            .await
            .expect("a checkpoint without required verified artifacts admits the next distinct artifact");
        assert!(second.ok);
        assert_eq!(
            second.invocation_metadata.runtime.as_ref().map(|runtime| runtime.epoch),
            Some(0),
            "the next arrival keeps the same checkpoint generation"
        );

        assert!(runtime.admitted_mark_outcomes().is_empty());
    }

    #[tokio::test]
    async fn runtime_does_not_author_checkpoint_history_reuse() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("checkpoint-reuses-history");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        add_pre_existing_checkpoint(&record_path, "c0");
        let inv = interface_artifact_admission();

        let obs = runtime.admit_bound_interface_checkpoint(inv).await.expect(
            "runtime does not run a synchronous post-admission checkpoint-history validator",
        );
        assert!(obs.ok);
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).expect("run record still present"))
                .expect("run record still parses");
        assert_eq!(
            record.get("active_checkpoints"),
            Some(&serde_json::json!(["c1"])),
            "runtime admitted the movement but did not promote inactive checkpoint history into active JSON"
        );
        assert!(
            runtime.admitted_mark_outcomes().is_empty(),
            "history reuse does not admit a runtime mark"
        );
    }

    #[tokio::test]
    async fn interface_authored_run_json_transition_is_the_only_success_path() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("checkpoint-consumed");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        let inv = interface_artifact_admission();

        let obs = runtime
            .admit_bound_interface_checkpoint(inv)
            .await
            .expect("JSON-authored checkpoint admission is admissible");
        assert!(obs.ok);
        let mut authored_record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).expect("run record still present"))
                .expect("run record still parses");
        let object = authored_record
            .as_object_mut()
            .expect("run record is an object");
        object.insert("active_checkpoints".into(), serde_json::json!([]));
        write_literal_run_record_fixture(&record_path, &authored_record)
            .expect("test-authored interface JSON transition writes");
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).expect("run record still present"))
                .expect("run record still parses");
        assert_eq!(
            record.get("active_checkpoints"),
            Some(&serde_json::json!([])),
            "the consumed checkpoint is no longer active"
        );
        let marks = runtime.admitted_mark_outcomes();
        assert!(
            marks.is_empty(),
            "runtime delivery does not admit a mark after JSON progression"
        );

        let second_inv = interface_artifact_admission();
        let err = runtime
            .admit_bound_interface_checkpoint(second_inv)
            .await
            .expect_err("a consumed checkpoint cannot admit another artifact until JSON authors a new checkpoint generation");
        assert_eq!(
            err.refused_reason(),
            RefusedReason::Settled,
            "a second arrival after consuming the only checkpoint (empty active_checkpoints = settled) must fail closed before boundary admission, got {err:?}"
        );
        assert_eq!(
            runtime.admitted_mark_outcomes().len(),
            0,
            "the rejected second arrival must not admit a mark"
        );
    }

    #[tokio::test]
    async fn checkpoint_authored_outbound_bindings_are_movement_metadata_not_runtime_outputs() {
        let runtime = runtime_with_interface_catalog();
        runtime
            .replace_interface_catalog_classes(["world_test", "world_client", "world_denied"].map(
                |id| {
                    (
                        InterfaceId::new(id),
                        InterfaceClass::new(
                            InterfaceType::World,
                            InterfaceRuntimeDisposition::ExternalReader,
                        )
                        .unwrap(),
                    )
                },
            ))
            .unwrap();
        let dir = temp_test_dir("checkpoint-outbound-interface-artifacts");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test", "world_client"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let _record_path = literal_boundary_run_record_fixture_at_checkpoint_with_outbound(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &CheckpointId::new("c1"),
            &grant,
            &[
                InterfaceId::new("world_client"),
                InterfaceId::new("world_denied"),
            ],
        );
        let inv = interface_artifact_admission();

        let obs = runtime
            .admit_bound_interface_checkpoint(inv)
            .await
            .expect("checkpoint-authored outbound binding is admissible movement metadata");

        assert_eq!(
            obs.data.get("admitted"),
            Some(&serde_json::Value::Bool(true)),
            "runtime observation reports admission, not a runtime output"
        );
        assert!(
            obs.data.get("metadata").is_some(),
            "runtime forms metadata beside the artifact movement"
        );
        assert_eq!(
            obs.outbound_artifact_transfers.len(),
            1,
            "one authored outbound folder produces one movement fact"
        );
        assert_eq!(obs.refused_outbound_artifact_transfers.len(), 1);
        assert_eq!(
            obs.refused_outbound_artifact_transfers[0].to_interface,
            InterfaceId::new("world_denied")
        );
        let transfer = &obs.outbound_artifact_transfers[0];
        assert_eq!(transfer.run, RunId::new("r1"));
        assert_eq!(transfer.checkpoint, CheckpointId::new("c1"));
        assert_eq!(transfer.from_interface, InterfaceId::new("world_test"));
        assert_eq!(transfer.to_interface, InterfaceId::new("world_client"));
        assert_eq!(transfer.artifact.id, ArtifactId::new("turn.input"));
        let delivered_runtime = transfer
            .invocation_metadata
            .runtime
            .as_ref()
            .expect("outbound delivery carries runtime metadata");
        assert_eq!(
            delivered_runtime.interface,
            InterfaceId::new("world_client"),
            "receiving interface sees its own destination interface folder in the invocation envelope"
        );
        let admitted_metadata = obs
            .data
            .get("metadata")
            .and_then(|metadata| metadata.get("runtime"))
            .expect("observation keeps inbound admission metadata");
        assert_eq!(
            admitted_metadata
                .get("interface")
                .and_then(serde_json::Value::as_str),
            Some("world_test"),
            "observation metadata remains the admitted inbound interface"
        );
    }

    #[tokio::test]
    async fn checkpoint_outbound_interface_must_exist_in_runtime_catalog() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("checkpoint-outbound-interface-catalog");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test", "world_not_loaded"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        literal_boundary_run_record_fixture_at_checkpoint_with_outbound(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &CheckpointId::new("c1"),
            &grant,
            &[InterfaceId::new("world_not_loaded")],
        );

        let err = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect_err("runtime must not form an outbound transfer to an unknown interface id");

        assert!(
            matches!(err, RuntimeError::UnknownInterface(ref interface) if interface == &InterfaceId::new("world_not_loaded")),
            "unknown outbound interface must fail closed before movement formation, got {err:?}"
        );
    }

    #[tokio::test]
    async fn runtime_admission_does_not_synthesize_output_artifact() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("checkpoint-consumed-no-output");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        let inv = interface_artifact_admission();

        let obs = runtime
            .admit_bound_interface_checkpoint(inv)
            .await
            .expect("runtime may admit a checkpoint without synthesizing an output artifact");
        assert!(obs.ok);
        assert_eq!(
            obs.data.get("artifact"),
            None,
            "runtime admission must not synthesize a fake output artifact"
        );
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).expect("run record still present"))
                .expect("run record still parses");
        assert_eq!(
            record.get("active_checkpoints"),
            Some(&serde_json::json!(["c1"])),
            "runtime admission left the active checkpoint list exactly as JSON authored it"
        );
        let marks = runtime.admitted_mark_outcomes();
        assert_eq!(
            marks.len(),
            0,
            "no-output handling does not admit a descriptor-selected mark"
        );
    }

    fn interface_arrival_movement(run: &str, checkpoint: &str, artifact_label: &str) -> Movement {
        let bytes = artifact_label.as_bytes();
        Movement {
            seq: Seq(0),
            lamport: Lamport(1),
            recorded_at_unix_ms: 1,
            trace: TraceId("t".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_test")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_test")),
            artifact: Some(ArtifactRef {
                id: ArtifactId::new(artifact_label),
                content: ContentHash::of_bytes(bytes),
            }),
            invocation_metadata: Some(
                iface::InvocationMetadata::new(iface::GrantProjection::default())
                    .with_runtime(iface::RuntimeInvocationMetadata {
                        run: RunId::new(run),
                        ee: desc::EeId::new("ee_test"),
                        worker: InterfaceId::new("system_worker_test"),
                        checkpoint: CheckpointId::new(checkpoint),
                        interface: InterfaceId::new("world_test"),
                        epoch: 0,
                        run_record_hash: None,
                    })
                    .into(),
            ),
        }
    }

    #[tokio::test]
    async fn self_resolving_arrival_refuses_to_silently_drop_fan_out_tail() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("self-resolve-fanout-tail");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        add_second_active_checkpoint(&record_path, "c2");

        let err = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect_err("a checkpoint-less arrival matching two active checkpoints must not silently pick the first");
        assert_eq!(
            err.refused_reason(),
            RefusedReason::NoBinding,
            "self-resolution with >1 matching checkpoint fails closed instead of dropping the tail, got {err:?}"
        );
        assert!(
            err.to_string().contains("cannot fan out"),
            "the refusal directs the caller to the fan-out resolver, got {err}"
        );

        let admitted_artifact = test_artifact_ref("turn.input", br#"{"hello":true}"#);
        let targets = runtime
            .resolve_interface_arrival_targets(&InterfaceId::new("world_test"), "turn.input")
            .expect("the fan-out resolver enumerates every matching active checkpoint");
        let mut admitted: Vec<String> = Vec::new();
        for target in &targets {
            runtime
                .admit_bound_interface_checkpoint(InterfaceArtifactAdmission {
                    run: target.run.clone(),
                    checkpoint: Some(target.checkpoint.clone()),
                    epoch: target.epoch,
                    interface: InterfaceId::new("world_test"),
                    artifact: admitted_artifact.clone(),
                    artifact_label: "turn.input".into(),
                    run_record_hash: Some(target.run_record.hash),
                })
                .await
                .expect("each fanned-out checkpoint admits its own edge");
            admitted.push(target.checkpoint.as_str().to_string());
        }
        admitted.sort();
        assert_eq!(
            admitted,
            vec!["c1".to_string(), "c2".to_string()],
            "the artifact reaches EVERY matching active checkpoint, not only the first"
        );
    }

    #[tokio::test]
    async fn spent_exactly_once_survives_a_simulated_coma_via_movement_reconstruction() {
        let dir = temp_test_dir("spent-survives-coma");
        let grant = grant_with_interfaces(&["world_test"], &[]);
        literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );

        let pre_coma = runtime_with_interface_catalog();
        pre_coma.set_run_records_dir(dir.clone());
        pre_coma.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let obs = pre_coma
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect("the first artifact is admitted at checkpoint c1 before the coma");
        assert!(obs.ok);

        let mut durable_arrival = interface_arrival_movement("r1", "c1", "turn.input");
        durable_arrival.to = Endpoint::Interface(
            obs.invocation_metadata
                .runtime
                .as_ref()
                .unwrap()
                .worker
                .clone(),
        );
        durable_arrival.invocation_metadata = Some(obs.invocation_metadata.clone().into());
        let durable_arrivals = vec![durable_arrival];

        let post_coma = runtime_with_interface_catalog();
        post_coma.set_run_records_dir(dir.clone());
        post_coma.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let replay_before_reconstruction = post_coma
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await;
        assert!(
            replay_before_reconstruction.is_ok(),
            "a fresh in-RAM runtime with no reconstructed spent set would double-admit the still-active checkpoint — proving the RAM-only map cannot survive a coma without reconstruction"
        );

        let rebooted = runtime_with_interface_catalog();
        rebooted.set_run_records_dir(dir.clone());
        rebooted.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        rebooted
            .reconstruct_spent_checkpoints_from_movements(&durable_arrivals)
            .expect("durable checkpoint admissions reconstruct");
        let err = rebooted
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect_err("after reconstruction from durable movements, the replayed artifact is refused exactly-once");
        assert_eq!(
            err.refused_reason(),
            RefusedReason::CheckpointSpent,
            "the spent checkpoint reconstructed from the append-only movement log rejects the replayed arrival, got {err:?}"
        );
    }

    #[test]
    fn replay_refuses_an_arrival_without_runtime_provenance() {
        let runtime = runtime_with_interface_catalog();
        let mut movement = interface_arrival_movement("r1", "c1", "turn.input");
        movement.invocation_metadata = None;
        let error = runtime
            .reconstruct_spent_checkpoints_from_movements([&movement])
            .expect_err("an ungrounded durable arrival cannot reconstruct checkpoint state");
        assert!(error.to_string().contains("no run provenance"));
    }

    #[tokio::test]
    async fn settled_run_with_empty_active_refuses_fail_closed() {
        let runtime = runtime_with_interface_catalog();
        let dir = temp_test_dir("settled-empty-active");
        runtime.set_run_records_dir(dir.clone());
        let grant = grant_with_interfaces(&["world_test"], &[]);
        runtime.set_run_authority(RunId::new("r1"), grant.clone(), GrantEpoch(0));
        let record_path = literal_boundary_run_record_fixture(
            &dir,
            &RunId::new("r1"),
            &InterfaceId::new("world_test"),
            &grant,
        );
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).expect("run record reads"))
                .expect("run record parses");
        record
            .as_object_mut()
            .expect("run record is an object")
            .insert("active_checkpoints".into(), serde_json::json!([]));
        write_literal_run_record_fixture(&record_path, &record).expect("settled run record writes");

        let err = runtime
            .admit_bound_interface_checkpoint(interface_artifact_admission())
            .await
            .expect_err("a settled run (empty active_checkpoints) admits no artifact");
        assert_eq!(
            err.refused_reason(),
            RefusedReason::Settled,
            "settled = empty active_checkpoints must refuse fail-closed as a named invariant, got {err:?}"
        );

        let err = runtime
            .resolve_interface_arrival_targets(&InterfaceId::new("world_test"), "turn.input")
            .expect_err("an arrival whose folder has no bound target is refused fail-closed");
        assert_eq!(
            err.refused_reason(),
            RefusedReason::NoBinding,
            "the settled run admits no artifact on the resolver path either: empty targets refuse fail-closed, got {err:?}"
        );
    }
}
