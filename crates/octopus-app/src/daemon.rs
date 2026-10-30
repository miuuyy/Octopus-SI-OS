use notify::Watcher;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use octopus_app_server::{
    EmbeddedRuntime, InterfaceArtifactArrivalAdmissionError, InterfaceArtifactFile,
    SharedBlobHandle, SharedBlobs, SpineRootConfig, admit_interface_artifact_arrival,
    admit_retained_interface_artifact_arrival,
    admit_system_interface_emission, admit_system_interface_reference_emission,
    build_embedded_spine, recover_durable_checkpoint_outbound_tail,
};
use octopus_descriptors::{ArtifactId, CheckpointId, InterfaceId, RunId};
use octopus_ee::EeTree;
pub use octopus_interface_transport::InterfaceActivationEvidence as ArrivalEvidence;
pub use octopus_interface_transport::{SystemInterfaceEmission, SystemInterfaceEmissionArtifact};
use octopus_interfaces::{DeliveryVerdict, RefusedReason};
#[cfg(test)]
use octopus_ledger::ArtifactRef;
use octopus_ledger::{Endpoint, InMemoryLog, Movement, MovementKind, MovementLog, Seq, TraceId};
#[cfg(test)]
use octopus_ledger_store::DurableBlobs;
use octopus_ledger_store::DurableLog;
use tokio_util::sync::CancellationToken;

use crate::{installed_source_registry_root, registry_install, runtime_installation_root};

#[path = "host_manifest.rs"]
mod host_manifest;
use host_manifest::{
    EE_INSTANCES_ROOT_SLOT, HostFactSeed, REPO_ROOT_SLOT, RUN_RECORDS_ROOT_SLOT, ResolvedHostRoots,
    WORKTREE_ROOT_SLOT, load_or_initialize_host_roots,
};

pub const DAEMON_BOOT_RUN_ID: &str = "run_daemon";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DaemonBootEvent {
    LedgerOpening {
        path: std::path::PathBuf,
    },
    LedgerProjected {
        movements: usize,
        unique_grants: usize,
    },
    SpineOpening,
    SpineReady,
    CheckpointsReconstructing,
    CheckpointsReconstructed,
    OutboundTailRecovering,
    OutboundTailRecovered {
        movements: usize,
        hydrated_rows: usize,
    },
    SpotlightOpening,
    SpotlightReady {
        indexed_movements: Option<usize>,
    },
    StateRebuilding,
    StateRebuilt {
        durable_runs: usize,
        ee_scopes: usize,
    },
}

pub struct Arrival {
    pub artifact: InterfaceArtifactFile,
    pub interface: InterfaceId,
    pub evidence: Vec<ArrivalEvidence>,
    pub verdict_tx: Option<tokio::sync::oneshot::Sender<DeliveryVerdict>>,
    pub retained: Option<octopus_interface_transport::RetainedInterfaceArrival>,
}

impl Arrival {
    #[cfg(test)]
    pub fn fire_and_forget(artifact: InterfaceArtifactFile) -> Self {
        Self {
            artifact,
            interface: InterfaceId::new("world_terminal"),
            evidence: Vec::new(),
            verdict_tx: None,
            retained: None,
        }
    }
}

#[derive(Debug)]
pub struct ResumedState {
    pub ee_tree: EeTree,
    pub durable_runs: Vec<RunId>,
}

#[cfg(test)]
pub fn rebuild_daemon_state_from_durable_root(
    durable_root: &Path,
    base_tree: EeTree,
) -> Result<ResumedState, Box<dyn std::error::Error>> {
    let log = DurableLog::open(durable_root.join("movements.jsonl"))?;
    rebuild_daemon_state_from_movements(log.try_movements_from(Seq(0))?, base_tree)
}

fn rebuild_daemon_state_from_movements(
    movements: impl IntoIterator<Item = octopus_ledger::Movement>,
    base_tree: EeTree,
) -> Result<ResumedState, Box<dyn std::error::Error>> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for m in movements {
        if let Some(run) = m.run() {
            seen.insert(run.as_str().to_string());
        }
    }
    let durable_runs: Vec<RunId> = seen.into_iter().map(|r| RunId::new(&r)).collect();

    Ok(ResumedState {
        ee_tree: base_tree,
        durable_runs,
    })
}

async fn admit_pair_emission_hand_in(
    embedded: &EmbeddedRuntime,
    log: &mut (dyn MovementLog + Send),
    blobs: &mut SharedBlobHandle,
    emission: SystemInterfaceEmission,
) {
    let (artifact, from_interface, source_movement_seq, to_interface, verdict_tx) =
        emission.into_parts();
    let outcome = match &artifact {
        SystemInterfaceEmissionArtifact::Materialized(artifact) => {
            admit_system_interface_emission(
                embedded,
                log,
                blobs,
                artifact,
                source_movement_seq,
                &from_interface,
                &to_interface,
            )
            .await
        }
        SystemInterfaceEmissionArtifact::Reference(artifact) => {
            admit_system_interface_reference_emission(
                embedded,
                log,
                blobs,
                artifact,
                source_movement_seq,
                &from_interface,
                &to_interface,
            )
            .await
        }
    };
    let verdict = match &outcome {
        Ok(outcome) => DeliveryVerdict::Admitted {
            seq: outcome.admitted_seq(),
        },
        Err(error) => {
            let verdict = error.verdict();
            let reason = match &verdict {
                DeliveryVerdict::Refused { reason, .. } => reason.as_str(),
                DeliveryVerdict::Admitted { .. } => "admitted",
            };
            eprintln!(
                "run_daemon: emission from interface '{}' at source movement {} toward interface '{}' refused \
                 (verdict={reason}; the refusal is a DeliveryRefused movement in the ledger; \
                 the spine stays live): {error}",
                from_interface.as_str(),
                source_movement_seq,
                to_interface.as_str()
            );
            verdict
        }
    };
    let _ = verdict_tx.send(verdict);
}

fn durable_arrival_evidence_duplicate(
    dedup: &std::sync::Mutex<octopus_ledger_index::ArrivalDedupIndex>,
    log: &(dyn MovementLog + Send),
    evidence: &[ArrivalEvidence],
) -> Result<Option<ArtifactId>, String> {
    let evidence_ids: Vec<&ArtifactId> = evidence.iter().map(|item| &item.artifact_id).collect();
    if evidence_ids.is_empty() {
        return Ok(None);
    }
    let mut index = dedup
        .lock()
        .map_err(|_| "arrival dedup index mutex poisoned".to_string())?;
    index.catch_up(log).map_err(|e| e.to_string())?;
    Ok(index.arrival_evidence_duplicate(&evidence_ids))
}

pub(crate) fn ensure_host_manifest(
    octopus_home: &Path,
    registry_root: &Path,
    source_root: &Path,
    run_records_root: &Path,
    ee_instances_root: &Path,
) -> Result<ResolvedHostRoots, Box<dyn std::error::Error>> {
    let repo_root = std::fs::canonicalize(source_root)?;
    let worktree_root = octopus_home.join("state").join("worker-worktrees");
    let roots = load_or_initialize_host_roots(octopus_home, registry_root, || {
        Ok(HostFactSeed {
            root_refs: std::collections::BTreeMap::from([
                (REPO_ROOT_SLOT.to_string(), repo_root),
                (WORKTREE_ROOT_SLOT.to_string(), worktree_root),
                (
                    RUN_RECORDS_ROOT_SLOT.to_string(),
                    run_records_root.to_path_buf(),
                ),
                (
                    EE_INSTANCES_ROOT_SLOT.to_string(),
                    ee_instances_root.to_path_buf(),
                ),
            ]),
        })
    })?;
    Ok(roots)
}

fn required_host_root<'a>(
    roots: &'a ResolvedHostRoots,
    slot: &str,
) -> Result<&'a Path, Box<dyn std::error::Error>> {
    roots
        .get(slot)
        .ok_or_else(|| format!("host manifest resolved no required root slot '{slot}'").into())
}

pub async fn run_daemon(
    durable_root: &Path,
    octopus_home: &Path,
    blobs: SharedBlobs,
    arrivals: tokio::sync::mpsc::Receiver<Arrival>,
    emissions: tokio::sync::mpsc::Receiver<SystemInterfaceEmission>,
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let installation_root = runtime_installation_root()?;
    let source_registry_root = installed_source_registry_root(&installation_root)?;
    let registry_root =
        registry_install::install_registry(&source_registry_root, &octopus_home.join("registry"))?
            .root;
    let source_root = source_registry_root
        .parent()
        .ok_or("source registry root has no parent")?;
    let state_root = octopus_home.join("state");
    let default_run_records_root = state_root.join("runs");
    let default_ee_instances_root = state_root.join("ee_instances");
    let interface_artifact_root = state_root.join("interface-artifacts");
    let roots = ensure_host_manifest(
        octopus_home,
        &registry_root,
        source_root,
        &default_run_records_root,
        &default_ee_instances_root,
    )?;
    let run_records_root = required_host_root(&roots, RUN_RECORDS_ROOT_SLOT)?.to_path_buf();
    let ee_instances_root = required_host_root(&roots, EE_INSTANCES_ROOT_SLOT)?.to_path_buf();
    run_daemon_on_dirs(
        durable_root,
        registry_root,
        interface_artifact_root,
        run_records_root,
        ee_instances_root,
        blobs,
        arrivals,
        emissions,
        cancel,
        None,
        None,
    )
    .await
}

pub(crate) async fn run_daemon_on_dirs(
    durable_root: &Path,
    registry_root: std::path::PathBuf,
    interface_artifact_root: std::path::PathBuf,
    run_records_dir: std::path::PathBuf,
    ee_instances_dir: std::path::PathBuf,
    blobs: SharedBlobs,
    arrivals: tokio::sync::mpsc::Receiver<Arrival>,
    emissions: tokio::sync::mpsc::Receiver<SystemInterfaceEmission>,
    cancel: CancellationToken,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    boot_events: Option<tokio::sync::mpsc::UnboundedSender<DaemonBootEvent>>,
) -> Result<(), Box<dyn std::error::Error>> {
    octopus_file_storage::publication::create_dir_all(durable_root)?;
    let durable_root = std::fs::canonicalize(durable_root)?;
    let root = std::fs::canonicalize(registry_root)?;

    let trace_id = TraceId("trace-daemon".into());
    let movements_path = durable_root.join("movements.jsonl");
    publish_boot_event(
        &boot_events,
        DaemonBootEvent::LedgerOpening {
            path: movements_path.clone(),
        },
    );
    tokio::task::yield_now().await;
    let mut log = DurableLog::open(&movements_path)?;
    let mut boot_movements = log.try_boot_movements_from(Seq(0))?;
    publish_boot_event(
        &boot_events,
        DaemonBootEvent::LedgerProjected {
            movements: boot_movements.len(),
            unique_grants: log.validated_grant_count()?,
        },
    );
    octopus_file_storage::publication::create_dir_all(&run_records_dir)?;
    let run_records_dir = std::fs::canonicalize(run_records_dir)?;
    crate::bootstrap_ees::ensure_bootstrap_ee_instances(&root, &ee_instances_dir)
        .map_err(std::io::Error::other)?;
    let ee_instances_dir = std::fs::canonicalize(ee_instances_dir)?;
    octopus_file_storage::publication::create_dir_all(&interface_artifact_root)?;
    let interface_artifact_root = std::fs::canonicalize(interface_artifact_root)?;
    publish_boot_event(&boot_events, DaemonBootEvent::SpineOpening);
    tokio::task::yield_now().await;
    let embedded = std::sync::Arc::new(
        build_embedded_spine(SpineRootConfig {
            registry_root: root.clone(),
            run: RunId::new(DAEMON_BOOT_RUN_ID),
            blobs: blobs.clone(),
            interface_artifact_root,
            run_records_root: run_records_dir.clone(),
            ee_instances_root: ee_instances_dir.clone(),
        })
        .await?,
    );
    publish_boot_event(&boot_events, DaemonBootEvent::SpineReady);
    let embedded_run_records_dir = std::fs::canonicalize(embedded.run_records_dir())?;
    if embedded_run_records_dir != run_records_dir {
        return Err(format!(
            "embedded runtime run directory '{}' differs from the host-resolved run records directory '{}'",
            embedded_run_records_dir.display(),
            run_records_dir.display()
        )
        .into());
    }
    let embedded_ee_instances_dir = std::fs::canonicalize(embedded.ee_instances_dir())?;
    if embedded_ee_instances_dir != ee_instances_dir {
        return Err(format!(
            "embedded runtime EE directory '{}' differs from the host-resolved EE instances directory '{}'",
            embedded_ee_instances_dir.display(),
            ee_instances_dir.display()
        )
        .into());
    }

    publish_boot_event(&boot_events, DaemonBootEvent::CheckpointsReconstructing);
    tokio::task::yield_now().await;
    hydrate_partial_checkpoint_rows(&log, &mut boot_movements)?;
    embedded
        .arrival_dedup
        .lock()
        .expect("arrival dedup index mutex is not poisoned")
        .catch_up_movements(boot_movements.clone())
        .map_err(|error| -> Box<dyn std::error::Error> {
            format!("arrival dedup index failed to fold the durable ledger: {error}").into()
        })?;

    embedded
        .runtime
        .reconstruct_spent_checkpoints_from_movements(&boot_movements)
        .map_err(|error| -> Box<dyn std::error::Error> {
            format!("runtime failed to reconstruct checkpoint admissions: {error}").into()
        })?;
    publish_boot_event(&boot_events, DaemonBootEvent::CheckpointsReconstructed);

    publish_boot_event(&boot_events, DaemonBootEvent::OutboundTailRecovering);
    tokio::task::yield_now().await;
    let hydrated_rows = hydrate_outbound_tail_recovery_rows(&log, &mut boot_movements)?;
    let recovered = recover_durable_checkpoint_outbound_tail(&mut log, &boot_movements)?;
    publish_boot_event(
        &boot_events,
        DaemonBootEvent::OutboundTailRecovered {
            movements: recovered.len(),
            hydrated_rows,
        },
    );
    if !recovered.is_empty() {
        embedded
            .arrival_dedup
            .lock()
            .expect("arrival dedup index mutex is not poisoned")
            .catch_up_movements(recovered.clone())
            .map_err(|error| -> Box<dyn std::error::Error> {
                format!("arrival dedup index failed to fold recovered outbound tail: {error}")
                    .into()
            })?;
        boot_movements.extend(recovered);
    }

    let (run_changes_tx, run_changes) = tokio::sync::mpsc::unbounded_channel();
    let mut run_watcher = notify::recommended_watcher(move |event| {
        let _ = run_changes_tx.send(event);
    })?;
    run_watcher.watch(&run_records_dir, notify::RecursiveMode::NonRecursive)?;

    let relay_start = Seq(log.len() as u64);
    let mut relay_blobs = SharedBlobHandle::new(blobs.clone());
    octopus_app_server::arrival::recover_runtime_relay_arrivals(
        &embedded.runtime,
        &mut log,
        &mut relay_blobs,
        embedded.arrival_dedup.clone(),
        &boot_movements,
    )
    .await?;
    boot_movements.extend(log.try_boot_movements_from(relay_start)?);

    publish_boot_event(&boot_events, DaemonBootEvent::SpotlightOpening);
    tokio::task::yield_now().await;
    let spotlight = match octopus_ledger_index::LedgerSqliteIndex::open_healthy_from_movements(
        &durable_root.join("ledger-index.sqlite"),
        &log,
        &boot_movements,
    ) {
        Ok(index) => Some(index),
        Err(error) => {
            eprintln!(
                "run_daemon: ledger spotlight index unavailable (readers fall back to the JSONL \
                 ledger): {error}"
            );
            None
        }
    };
    let indexed_movements = spotlight
        .as_ref()
        .and_then(|index| index.count().ok())
        .and_then(|count| usize::try_from(count).ok());
    publish_boot_event(
        &boot_events,
        DaemonBootEvent::SpotlightReady { indexed_movements },
    );
    let _ = &trace_id;

    publish_boot_event(&boot_events, DaemonBootEvent::StateRebuilding);
    tokio::task::yield_now().await;
    let resumed = rebuild_daemon_state_from_movements(boot_movements, embedded.live_ee_tree()?)?;
    if resumed.ee_tree.len() > 1 || !resumed.durable_runs.is_empty() {
        eprintln!(
            "run_daemon: RESUMED prior state from the durable ledger alone — {} EE scope(s), {} prior \
             durable run(s) (no in-memory carryover)",
            resumed.ee_tree.len(),
            resumed.durable_runs.len(),
        );
    }
    publish_boot_event(
        &boot_events,
        DaemonBootEvent::StateRebuilt {
            durable_runs: resumed.durable_runs.len(),
            ee_scopes: resumed.ee_tree.len(),
        },
    );
    let writer = crate::ledger_writer::LedgerWriter::new(log, spotlight);
    if let Some(ready) = ready {
        ready
            .send(())
            .map_err(|_| "daemon readiness receiver closed before initialization completed")?;
    }
    run_admission_loop(embedded, writer, arrivals, emissions, run_changes, cancel).await
}

async fn run_admission_loop(
    embedded: std::sync::Arc<EmbeddedRuntime>,
    writer: crate::ledger_writer::LedgerWriter,
    mut arrivals: tokio::sync::mpsc::Receiver<Arrival>,
    mut emissions: tokio::sync::mpsc::Receiver<SystemInterfaceEmission>,
    mut run_changes: tokio::sync::mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    enum Admission {
        Arrival(Arrival),
        Emission(SystemInterfaceEmission),
    }

    let mut inflight = tokio::task::JoinSet::new();
    let mut result = loop {
        tokio::select! {
            biased;
            Some(completed) = inflight.join_next(), if !inflight.is_empty() => {
                if let Err(error) = completed {
                    break Err(error);
                }
            }
            _ = cancel.cancelled() => break Ok(()),
            Some(change) = run_changes.recv() => {
                let mut paths = std::collections::BTreeSet::new();
                match change {
                    Ok(event) if !matches!(event.kind, notify::EventKind::Access(_)) => paths.extend(event.paths),
                    Ok(_) => {},
                    Err(error) => { eprintln!("run binding watcher failed: {error}"); continue; }
                }
                while let Ok(change) = run_changes.try_recv() {
                    match change {
                        Ok(event) if !matches!(event.kind, notify::EventKind::Access(_)) => paths.extend(event.paths),
                    Ok(_) => {},
                        Err(error) => eprintln!("run binding watcher failed: {error}"),
                    }
                }
                let embedded = embedded.clone();
                let writer = writer.clone();
                inflight.spawn_local(async move {
                    if let Err(error) = catch_up_changed_relay_bindings(&embedded, writer, paths).await {
                        eprintln!("runtime relay catch-up refused: {error}");
                    }
                });
            }
            next = async {
                tokio::select! {
                    arrival = arrivals.recv() => arrival.map(Admission::Arrival),
                    emission = emissions.recv() => emission.map(Admission::Emission),
                }
            } => match next {
                Some(Admission::Arrival(arrival)) => {
                    inflight.spawn_local(admit_arrival_concurrently(
                        embedded.clone(),
                        writer.clone(),
                        arrival,
                    ));
                }
                Some(Admission::Emission(emission)) => {
                    inflight.spawn_local(admit_emission_concurrently(
                        embedded.clone(),
                        writer.clone(),
                        emission,
                    ));
                }
                None => break Ok(()),
            },
        }
    };
    arrivals.close();
    emissions.close();
    inflight.abort_all();
    while let Some(completed) = inflight.join_next().await {
        if let Err(error) = completed {
            if !error.is_cancelled() && result.is_ok() {
                result = Err(error);
            }
        }
    }
    result.map_err(|error| {
        std::io::Error::other(format!("daemon admission task failed: {error}")).into()
    })
}

async fn catch_up_changed_relay_bindings(
    embedded: &EmbeddedRuntime,
    mut writer: crate::ledger_writer::LedgerWriter,
    paths: std::collections::BTreeSet<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    embedded.reload_registry_snapshot_on_artifact_arrival()?;
    let mut interfaces = std::collections::BTreeSet::new();
    for path in paths {
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let Ok(record) = octopus_run_record::RunRecord::from_bytes(&bytes) else {
            continue;
        };
        for checkpoint in record
            .checkpoints
            .iter()
            .filter(|cp| record.active_checkpoints.contains(&cp.id))
        {
            if let Some(binding) = &checkpoint.interface_binding {
                if embedded
                    .runtime
                    .is_runtime_relay_interface(&binding.interface)
                {
                    interfaces.insert(binding.interface.clone());
                }
            }
        }
    }
    let sequences = {
        let mut index = embedded
            .arrival_dedup
            .lock()
            .map_err(|_| "arrival index poisoned")?;
        index.catch_up(&writer)?;
        index.relay_source_sequences(&interfaces)
    };
    let mut movements = Vec::with_capacity(sequences.len());
    for seq in sequences {
        let movement = writer.read_at(Seq(seq)).ok_or_else(|| {
            format!("runtime relay source movement {seq} disappeared from the ledger")
        })?;
        movements.push(movement);
    }
    let mut blobs = SharedBlobHandle::new(embedded.blobs.clone());
    octopus_app_server::arrival::recover_runtime_relay_arrivals(
        &embedded.runtime,
        &mut writer,
        &mut blobs,
        embedded.arrival_dedup.clone(),
        &movements,
    )
    .await?;
    Ok(())
}

fn publish_boot_event(
    tx: &Option<tokio::sync::mpsc::UnboundedSender<DaemonBootEvent>>,
    event: DaemonBootEvent,
) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TailSourceIdentity {
    trace: TraceId,
    artifact_id: ArtifactId,
    artifact_content: octopus_ledger::ContentHash,
    from_interface: InterfaceId,
    run: RunId,
    checkpoint: CheckpointId,
    artifact_label: String,
    epoch: u64,
    run_record_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TailHydrationKey {
    source: TailSourceIdentity,
    to_interface: InterfaceId,
}

fn movement_tail_source_identity(movement: &Movement) -> Option<TailSourceIdentity> {
    let metadata = movement
        .invocation_metadata
        .as_ref()
        .and_then(|metadata| metadata.invocation())?;
    let runtime = metadata.runtime.as_ref()?;
    let admission = metadata.checkpoint_admission.as_ref()?;
    let artifact = movement.artifact.as_ref()?;
    let Endpoint::Interface(from_interface) = &movement.from;
    match (&movement.kind, &movement.to) {
        (MovementKind::InterfaceArrival, Endpoint::Interface(worker))
            if worker == &runtime.worker && from_interface == &runtime.interface => {}
        (MovementKind::InterfaceOutbound, Endpoint::Interface(to_interface))
            if to_interface == &runtime.interface => {}
        _ => return None,
    }
    Some(TailSourceIdentity {
        trace: movement.trace.clone(),
        artifact_id: artifact.id.clone(),
        artifact_content: artifact.content,
        from_interface: from_interface.clone(),
        run: runtime.run.clone(),
        checkpoint: runtime.checkpoint.clone(),
        artifact_label: admission.artifact_label.clone(),
        epoch: runtime.epoch,
        run_record_hash: runtime.run_record_hash.clone(),
    })
}

fn outbound_tail_hydration_key(
    movement: &Movement,
) -> Result<Option<TailHydrationKey>, Box<dyn std::error::Error>> {
    if movement.kind != MovementKind::InterfaceOutbound {
        return Ok(None);
    }
    let Endpoint::Interface(to_interface) = &movement.to;
    let source = movement_tail_source_identity(movement)
        .ok_or("recovered InterfaceOutbound has incomplete or inconsistent source identity")?;
    Ok(Some(TailHydrationKey {
        source,
        to_interface: to_interface.clone(),
    }))
}

fn hydrate_partial_checkpoint_rows(
    log: &DurableLog,
    movements: &mut [Movement],
) -> Result<usize, Box<dyn std::error::Error>> {
    let key = |movement: &Movement| {
        let runtime = movement
            .invocation_metadata
            .as_ref()?
            .invocation()?
            .runtime
            .as_ref()?;
        Some((
            runtime.run.clone(),
            runtime.checkpoint.clone(),
            runtime.epoch,
        ))
    };
    let mut partial: std::collections::BTreeSet<_> = movements
        .iter()
        .filter(|movement| {
            movement.kind == MovementKind::InterfaceArrival
                && movement
                    .invocation_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.invocation())
                    .and_then(|metadata| metadata.checkpoint_admission.as_ref())
                    .is_some_and(|admission| !admission.complete)
        })
        .filter_map(key)
        .collect();
    let proofs = octopus_ledger::InterfaceArrivalMarkProofIndex::from_movements(movements.iter())?;
    for movement in movements
        .iter()
        .filter(|movement| movement.kind == MovementKind::InterfaceArrival)
    {
        let Some(metadata) = movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation())
        else {
            continue;
        };
        if metadata.verification_failure.is_none()
            && metadata
                .checkpoint_admission
                .as_ref()
                .is_some_and(|admission| {
                    admission.complete
                        && (admission.required_artifact_labels.is_empty()
                            || proofs.proof_lamport(movement).is_some())
                })
        {
            if let Some(key) = key(movement) {
                partial.remove(&key);
            }
        }
    }
    let mut hydrated = 0;
    for movement in movements {
        if key(movement).is_some_and(|key| partial.contains(&key)) {
            *movement = log
                .try_movement_at(movement.seq)?
                .ok_or("partial checkpoint movement disappeared during hydration")?;
            hydrated += 1;
        }
    }
    Ok(hydrated)
}

fn hydrate_outbound_tail_recovery_rows(
    log: &DurableLog,
    movements: &mut [Movement],
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut probe = InMemoryLog::new();
    let missing = recover_durable_checkpoint_outbound_tail(&mut probe, movements)?;
    let mut selected = std::collections::BTreeSet::new();
    let mut latest_sources = std::collections::HashMap::new();
    for movement in movements
        .iter()
        .filter(|movement| movement.kind == MovementKind::InterfaceArrival)
    {
        let Some(identity) = movement_tail_source_identity(movement) else {
            continue;
        };
        latest_sources
            .entry(identity)
            .and_modify(|seq: &mut Seq| *seq = (*seq).max(movement.seq))
            .or_insert(movement.seq);
    }
    for candidate in &missing {
        let Some(key) = outbound_tail_hydration_key(candidate)? else {
            if candidate.kind != MovementKind::DeliveryRefused {
                return Err(format!(
                    "durable tail probe produced unexpected movement kind {:?}",
                    candidate.kind
                )
                .into());
            }
            continue;
        };
        let Some(source_row) = latest_sources.get(&key.source).copied() else {
            return Err(format!(
                "missing durable outbound tail toward interface '{}' for run '{}' checkpoint '{}' label '{}' resolved no source InterfaceArrival row",
                key.to_interface.as_str(),
                key.source.run.as_str(),
                key.source.checkpoint.as_str(),
                key.source.artifact_label,
            )
            .into());
        };
        selected.insert(source_row);
        for movement in movements.iter().filter(|movement| {
            movement.kind == MovementKind::InterfaceOutbound
                && movement_tail_source_identity(movement).as_ref() == Some(&key.source)
        }) {
            selected.insert(movement.seq);
        }
    }
    for movement in movements
        .iter()
        .filter(|movement| movement.kind == MovementKind::InterfaceOutbound)
    {
        let Some(identity) = movement_tail_source_identity(movement) else {
            continue;
        };
        let Some(source_row) = latest_sources.get(&identity).copied() else {
            continue;
        };
        let source_grant = log.grant_content_at(source_row).ok_or_else(|| {
            format!(
                "durable source movement {} has invocation metadata without grant identity",
                source_row.0
            )
        })?;
        let outbound_grant = log.grant_content_at(movement.seq).ok_or_else(|| {
            format!(
                "durable outbound movement {} has invocation metadata without grant identity",
                movement.seq.0
            )
        })?;
        if source_grant != outbound_grant {
            selected.insert(source_row);
            selected.insert(movement.seq);
        }
    }
    for seq in &selected {
        let index = usize::try_from(seq.0)
            .map_err(|_| format!("durable movement seq {} exceeds usize", seq.0))?;
        let slot = movements
            .get_mut(index)
            .ok_or_else(|| format!("durable movement seq {} has no boot projection slot", seq.0))?;
        if slot.seq != *seq {
            return Err(format!(
                "durable boot projection slot {} contains seq {}",
                index, slot.seq.0
            )
            .into());
        }
        *slot = log
            .try_movement_at(*seq)?
            .ok_or_else(|| format!("durable movement seq {} disappeared", seq.0))?;
    }
    Ok(selected.len())
}

async fn admit_arrival_concurrently(
    embedded: std::sync::Arc<EmbeddedRuntime>,
    mut writer: crate::ledger_writer::LedgerWriter,
    arrival: Arrival,
) {
    let Arrival {
        artifact: arrival_artifact,
        interface: arriving_interface,
        evidence,
        verdict_tx,
        retained,
    } = arrival;
    let mut arrival_blobs = SharedBlobHandle::new(embedded.blobs.clone());
    let retained_exists = retained.as_ref().map(|retained| {
        let mut index = embedded.arrival_dedup.lock().map_err(|_| "arrival index is poisoned".to_string())?;
        index.catch_up(&writer).map_err(|error| error.to_string())?;
        index.retained_arrival_scope(&arriving_interface, &retained.id).map(|scope| scope.is_some()).map_err(|error| error.to_string())
    }).transpose();
    let early_refusal = match retained_exists {
        Err(message) => Some(DeliveryVerdict::refused(RefusedReason::Malformed, serde_json::json!({"message":message}))),
        Ok(_) if retained.is_some() && verdict_tx.is_some() => Some(DeliveryVerdict::refused(RefusedReason::Malformed,
            serde_json::json!({"message":"arrival has two response owners"}))),
        Ok(Some(true)) => None,
        Ok(_) => match durable_arrival_evidence_duplicate(
        &embedded.arrival_dedup,
        &writer,
        &evidence,
    ) {
        Ok(None) => None,
        Ok(Some(id)) => Some(DeliveryVerdict::refused(
            RefusedReason::Malformed,
            serde_json::json!({"message": "arrival evidence already exists in the movement ledger", "artifact_id": id}),
        )),
        Err(error) => Some(DeliveryVerdict::refused(
            RefusedReason::Malformed,
            serde_json::json!({"message": error}),
        )),
        },
    };
    if let Some(verdict) = early_refusal {
        writer.append(octopus_ledger::MovementDraft {
            trace: TraceId(format!("interface-arrival:{}", arriving_interface.as_str())),
            kind: MovementKind::DeliveryRefused,
            from: Endpoint::Interface(arriving_interface.clone()),
            to: Endpoint::Interface(arriving_interface.clone()),
            artifact: None,
            invocation_metadata: Some(verdict.clone().into()),
            causal_floor: None,
        });
        if let Some(tx) = verdict_tx {
            let _ = tx.send(verdict.clone());
        }
        if let Some(retained) = retained {
            let _ = retained.reply.send(Err(format!("retained arrival intake refused: {}", serde_json::to_string(&verdict).unwrap_or_default())));
        }
        return;
    }

    if let Some(retained) = retained {
        let outcome = admit_retained_interface_artifact_arrival(
            &embedded, &mut writer, &mut arrival_blobs, &arrival_artifact, &arriving_interface, &retained.id,
        ).await.map_err(|error| error.to_string());
        let _ = retained.reply.send(outcome);
        return;
    }

    let admission: Result<_, InterfaceArtifactArrivalAdmissionError> =
        admit_interface_artifact_arrival(
            &embedded,
            &mut writer,
            &mut arrival_blobs,
            &arrival_artifact,
            &arriving_interface,
        )
        .await;
    let verdict = match admission {
        Ok(outcome) => {
            let seq = outcome
                .admission
                .records
                .first()
                .expect("successful arrival admission has at least one record")
                .arrival_movement
                .seq
                .0;
            DeliveryVerdict::Admitted { seq }
        }
        Err(e) => {
            let verdict = e.verdict();
            let reason = match &verdict {
                DeliveryVerdict::Refused { reason, .. } => reason.as_str(),
                DeliveryVerdict::Admitted { .. } => "admitted",
            };
            eprintln!(
                "run_daemon: artifact arrival from interface '{}' refused (verdict={reason}; the spine stays live for the next arrival): {e}",
                arriving_interface.as_str()
            );
            verdict
        }
    };
    if let Some(verdict_tx) = verdict_tx {
        let _ = verdict_tx.send(verdict);
    }
}

async fn admit_emission_concurrently(
    embedded: std::sync::Arc<EmbeddedRuntime>,
    mut writer: crate::ledger_writer::LedgerWriter,
    emission: SystemInterfaceEmission,
) {
    let mut arrival_blobs = SharedBlobHandle::new(embedded.blobs.clone());
    admit_pair_emission_hand_in(&embedded, &mut writer, &mut arrival_blobs, emission).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_app_server::build_embedded_spine;
    use octopus_descriptors::EeId;
    use octopus_interfaces::{
        CheckpointAdmissionMetadata, CheckpointBindingPolicySnapshot, GrantProjection,
        InvocationMetadata, RuntimeInvocationMetadata,
    };
    use octopus_ledger::{ContentHash, MovementLog};
    use std::sync::Arc;

    fn test_verifier_package_pin(verifier: &octopus_descriptors::VerifierId) -> octopus_ledger::VerifierPackagePin {
        let registry = repository_registry_root();
        let package_dir = registry.join("verifier").join(verifier.as_str());
        let descriptor: octopus_verifiers::VerifierDescriptor = octopus_descriptors::parse_strict(
            &std::fs::read(package_dir.join("verifier.json")).unwrap(),
        ).unwrap();
        let descriptor_bytes = octopus_descriptors::canonical_bytes(&serde_json::to_value(&descriptor).unwrap());
        let package = octopus_verifiers::pin_verifier_package(&octopus_verifiers::VerifierPackageContext {
            descriptor,
            package_dir: Some(package_dir),
            repository_root: registry.parent().unwrap().to_path_buf(),
            scratch_root: registry,
        }).unwrap();
        octopus_ledger::VerifierPackagePin {
            descriptor_content: ContentHash::of_bytes(&descriptor_bytes),
            script_content: ContentHash::of_bytes(&package.closure_bytes()),
        }
    }

    fn repository_registry_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry")
    }

    fn durable_test_blobs(durable_root: &Path) -> SharedBlobs {
        Arc::new(std::sync::Mutex::new(
            DurableBlobs::open(durable_root.join("blobs")).expect("test durable blob store opens"),
        ))
    }

    fn tail_recovery_metadata(
        run: &RunId,
        checkpoint: &CheckpointId,
        source: &InterfaceId,
        targets: &[InterfaceId],
        artifact_label: &str,
    ) -> InvocationMetadata {
        InvocationMetadata::new(GrantProjection {
            tools: vec!["tool.tail_recovery".to_string()],
            interfaces: targets.iter().map(|id| id.as_str().to_string()).collect(),
            ..GrantProjection::default()
        })
        .with_runtime(RuntimeInvocationMetadata {
            run: run.clone(),
            ee: EeId::new("ee_tail_recovery"),
            worker: InterfaceId::new("system_worker_tail_recovery"),
            checkpoint: checkpoint.clone(),
            interface: source.clone(),
            epoch: 7,
            run_record_hash: Some(ContentHash::of_bytes(b"tail run record").to_hex()),
        })
        .with_outbound_interface_bindings(targets.to_vec())
        .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {
            inbound: octopus_runtime::AuthoredInterfaceBinding::all(source.clone()),
            outbound: targets
                .iter()
                .cloned()
                .map(octopus_runtime::AuthoredInterfaceBinding::all)
                .collect(),
        })
        .with_checkpoint_admission(CheckpointAdmissionMetadata {
            artifact_label: artifact_label.to_string(),
            required_artifact_labels: Vec::new(),
            complete: true,
        })
    }

    fn append_tail_arrival(
        log: &mut DurableLog,
        trace: &TraceId,
        source: &InterfaceId,
        _run: &RunId,
        artifact: &ArtifactRef,
        metadata: &InvocationMetadata,
    ) -> Movement {
        log.append(octopus_ledger::MovementDraft {
            trace: trace.clone(),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(source.clone()),
            to: Endpoint::Interface(metadata.runtime.as_ref().unwrap().worker.clone()),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(metadata.clone().into()),
            causal_floor: None,
        })
    }

    fn append_tail_outbound(
        log: &mut DurableLog,
        trace: &TraceId,
        source: &InterfaceId,
        target: &InterfaceId,
        artifact: &ArtifactRef,
        metadata: &InvocationMetadata,
        causal_floor: octopus_ledger::Lamport,
    ) -> Movement {
        let mut metadata = metadata.clone();
        metadata.runtime.as_mut().unwrap().interface = target.clone();
        log.append(octopus_ledger::MovementDraft {
            trace: trace.clone(),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(source.clone()),
            to: Endpoint::Interface(target.clone()),
            artifact: Some(artifact.clone()),
            invocation_metadata: Some(metadata.into()),
            causal_floor: Some(causal_floor),
        })
    }

    fn projected_grant_tools(movement: &Movement) -> &[String] {
        &movement
            .invocation_metadata
            .as_ref()
            .and_then(|metadata| metadata.invocation())
            .unwrap()
            .grant
            .tools
    }

    fn projected_tail_log(
        path: &Path,
        trace: &TraceId,
        source: &InterfaceId,
        run: &RunId,
        artifact: &ArtifactRef,
        arrival_metadata: &InvocationMetadata,
        arrival_count: usize,
        outbounds: &[(&InterfaceId, &InvocationMetadata)],
    ) -> (DurableLog, Vec<Movement>) {
        let mut log = DurableLog::open(path).unwrap();
        let mut floor = None;
        for _ in 0..arrival_count {
            floor = Some(
                append_tail_arrival(&mut log, trace, source, run, artifact, arrival_metadata)
                    .lamport,
            );
        }
        for (target, metadata) in outbounds {
            append_tail_outbound(
                &mut log,
                trace,
                source,
                target,
                artifact,
                metadata,
                floor.unwrap(),
            );
        }
        drop(log);
        let log = DurableLog::open(path).unwrap();
        let movements = log.try_boot_movements_from(Seq(0)).unwrap();
        (log, movements)
    }

    #[test]
    fn selective_tail_hydration_preserves_exact_recovery_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let run = RunId::new("run_tail");
        let checkpoint = CheckpointId::new("checkpoint_tail");
        let source = InterfaceId::new("world_source");
        let target_a = InterfaceId::new("world_target_a");
        let target_b = InterfaceId::new("world_target_b");
        let targets = vec![target_a.clone(), target_b.clone()];
        let artifact = ArtifactRef {
            id: ArtifactId::new("tail.result:exact"),
            content: ContentHash::of_bytes(b"tail result"),
        };
        let trace = TraceId("trace-tail".to_string());
        let metadata = tail_recovery_metadata(&run, &checkpoint, &source, &targets, "tail.result");

        let missing_b_path = dir.path().join("missing-b/movements.jsonl");
        let (mut log, mut movements) = projected_tail_log(
            &missing_b_path,
            &trace,
            &source,
            &run,
            &artifact,
            &metadata,
            1,
            &[(&target_a, &metadata)],
        );
        assert_eq!(
            hydrate_outbound_tail_recovery_rows(&log, &mut movements).unwrap(),
            2
        );
        let recovered = recover_durable_checkpoint_outbound_tail(&mut log, &movements).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].to, Endpoint::Interface(target_b.clone()));
        assert_eq!(log.len(), 3);

        let duplicates_path = dir.path().join("duplicates/movements.jsonl");
        let (mut log, mut movements) = projected_tail_log(
            &duplicates_path,
            &trace,
            &source,
            &run,
            &artifact,
            &metadata,
            2,
            &[],
        );
        assert_eq!(
            hydrate_outbound_tail_recovery_rows(&log, &mut movements).unwrap(),
            1
        );
        assert!(projected_grant_tools(&movements[0]).is_empty());
        assert_eq!(projected_grant_tools(&movements[1]), ["tool.tail_recovery"]);
        assert_eq!(
            recover_durable_checkpoint_outbound_tail(&mut log, &movements)
                .unwrap()
                .len(),
            2
        );

        let mismatched_path = dir.path().join("mismatched-grant/movements.jsonl");
        let single_target_metadata = tail_recovery_metadata(
            &run,
            &checkpoint,
            &source,
            std::slice::from_ref(&target_a),
            "tail.result",
        );
        let mut mismatched_outbound_metadata = single_target_metadata.clone();
        mismatched_outbound_metadata.grant.tools = vec!["tool.different_grant".to_string()];
        let (mut log, mut movements) = projected_tail_log(
            &mismatched_path,
            &trace,
            &source,
            &run,
            &artifact,
            &single_target_metadata,
            1,
            &[(&target_a, &mismatched_outbound_metadata)],
        );
        assert_eq!(
            hydrate_outbound_tail_recovery_rows(&log, &mut movements).unwrap(),
            2
        );
        let recovered = recover_durable_checkpoint_outbound_tail(&mut log, &movements).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].to, Endpoint::Interface(target_a.clone()));
        assert_eq!(projected_grant_tools(&recovered[0]), ["tool.tail_recovery"]);
        assert_eq!(log.len(), 3);

        let complete_path = dir.path().join("complete/movements.jsonl");
        let (mut log, mut movements) = projected_tail_log(
            &complete_path,
            &trace,
            &source,
            &run,
            &artifact,
            &metadata,
            1,
            &[(&target_a, &metadata), (&target_b, &metadata)],
        );
        assert_eq!(
            hydrate_outbound_tail_recovery_rows(&log, &mut movements).unwrap(),
            0
        );
        assert!(
            movements
                .iter()
                .all(|movement| projected_grant_tools(movement).is_empty())
        );
        assert!(
            recover_durable_checkpoint_outbound_tail(&mut log, &movements)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn live_repair_and_boot_catch_up_release_each_required_artifact_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let state = root.join("state");
        let blobs = durable_test_blobs(root);
        seed_test_ee_root(&state.join("ee_instances"));
        let binding = |id| octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new(id));
        let mut roles = std::collections::BTreeSet::new();
        for worker in ["system_worker_coder_max", "system_worker_planner"] {
            let policy: serde_json::Value = serde_json::from_slice(
                &std::fs::read(
                    repository_registry_root()
                        .join("interface")
                        .join(worker)
                        .join("policy.json"),
                )
                .unwrap(),
            )
            .unwrap();
            roles.extend(
                policy["role_artifacts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|id| id.as_str().unwrap().to_string()),
            );
        }
        let mut record = serde_json::json!({
            "id":"emitter", "ee":"ee_existence", "worker":"system_worker_coder_max", "trace":"boot-emitter",
            "status":"running", "checkpoints":[{"id":"send", "run":"emitter", "trace":"boot-send", "epoch":0,
                "interface_binding":binding("system_context"), "outbound_interface_bindings":[binding("system_worker_planner")],
                "artifact_bindings":roles.iter().map(|id| serde_json::json!({"id":id})).collect::<Vec<_>>() }],
            "active_checkpoints":["send"]
        });
        write_literal_run_record_fixture(&state.join("runs/emitter.json"), &record).unwrap();
        let embedded = build_embedded_spine(SpineRootConfig {
            registry_root: repository_registry_root(),
            run: RunId::new("emitter"),
            blobs: blobs.clone(),
            interface_artifact_root: state.join("interface-artifacts"),
            run_records_root: state.join("runs"),
            ee_instances_root: state.join("ee_instances"),
        })
        .await
        .unwrap();
        let mut log = DurableLog::open(root.join("movements.jsonl")).unwrap();
        let mut handle = SharedBlobHandle::new(blobs.clone());
        for label in ["first", "second"] {
            let file = InterfaceArtifactFile::write(state.join(label), label, label.as_bytes())
                .unwrap()
                .capture_bounded(1024 * 1024, 1024 * 1024)
                .unwrap();
            let outcome = octopus_app_server::InterfaceArtifactArrivalFromInterface::new(
                &embedded.runtime,
                &mut log,
                &mut handle,
                embedded.arrival_dedup.clone(),
                TraceId("boot-source".into()),
                InterfaceId::new("system_context"),
            )
            .emit_artifact_to_interface(
                &file,
                &RunId::new("emitter"),
                &CheckpointId::new("send"),
                octopus_run_record::GrantEpoch(0),
                octopus_ledger::Lamport(0),
                &InterfaceId::new("system_worker_planner"),
            )
            .await
            .unwrap();
            assert!(outcome.delivered.is_empty());
        }
        assert_eq!(log.len(), 2);
        drop(log);
        drop(embedded);
        record["id"] = serde_json::json!("receiver");
        record["trace"] = serde_json::json!("boot-receiver");
        let checkpoint = &mut record["checkpoints"][0];
        checkpoint["run"] = serde_json::json!("receiver");
        checkpoint["trace"] = serde_json::json!("boot-receive");
        checkpoint["interface_binding"] = serde_json::json!(binding("system_worker_planner"));
        checkpoint["outbound_interface_bindings"] = serde_json::json!([binding("world_terminal")]);
        checkpoint["required_verified_artifacts"] = serde_json::json!(["first","second"].map(|label|
            serde_json::json!({"mark":"coder.build.ok", "artifact_label":label, "verifier":"artifact_exists", "params":{}})));
        std::fs::write(state.join("runs/receiver.json"), b"{").unwrap();
        let mut length = None;
        for restart in 0..3 {
            let (arrivals, arrival_rx) = tokio::sync::mpsc::channel(1);
            let (emissions, emission_rx) = tokio::sync::mpsc::channel(1);
            if restart != 0 {
                drop(arrivals);
                drop(emissions);
            }
            if restart == 0 {
                let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
                let cancel = CancellationToken::new();
                let task_cancel = cancel.clone();
                let task_root = root.to_path_buf();
                let task_state = state.clone();
                let task_blobs = blobs.clone();
                let local = tokio::task::LocalSet::new();
                local.run_until(async {
                    let daemon = tokio::task::spawn_local(async move {
                        run_daemon_on_dirs(&task_root, repository_registry_root(),
                            task_state.join("interface-artifacts"), task_state.join("runs"),
                            task_state.join("ee_instances"), task_blobs, arrival_rx, emission_rx,
                            task_cancel, Some(ready_tx), None).await.map_err(|error| error.to_string())
                    });
                    ready_rx.await.unwrap();
                    write_literal_run_record_fixture(&state.join("runs/receiver.json"), &record).unwrap();
                    let caught_up = tokio::time::timeout(std::time::Duration::from_secs(15), async {
                        loop {
                            let rows = DurableLog::open(root.join("movements.jsonl")).unwrap().read_from(Seq(0));
                            if rows.iter().filter(|row| row.kind == MovementKind::InterfaceOutbound
                                && row.to == Endpoint::Interface(InterfaceId::new("world_terminal"))).count() == 2 {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    }).await;
                    cancel.cancel();
                    daemon.await.unwrap().unwrap();
                    caught_up.expect("repairing run JSON must catch up the relay without restarting or resending");
                }).await;
            } else {
                run_daemon_on_dirs(
                    root,
                    repository_registry_root(),
                    state.join("interface-artifacts"),
                    state.join("runs"),
                    state.join("ee_instances"),
                    blobs.clone(),
                    arrival_rx,
                    emission_rx,
                    CancellationToken::new(),
                    None,
                    None,
                )
                .await
                .unwrap();
            }
            let rows = DurableLog::open(root.join("movements.jsonl"))
                .unwrap()
                .read_from(Seq(0));
            let outbound: Vec<_> = rows
                .iter()
                .filter(|movement| {
                    movement.kind == MovementKind::InterfaceOutbound
                        && movement.to == Endpoint::Interface(InterfaceId::new("world_terminal"))
                })
                .collect();
            assert_eq!(outbound.len(), 2);
            assert_ne!(outbound[0].artifact, outbound[1].artifact);
            assert_eq!(
                rows.iter()
                    .filter(|movement| matches!(
                        movement.kind,
                        MovementKind::MarkOutcomeAdmitted(_)
                    ))
                    .count(),
                2
            );
            if let Some(length) = length {
                assert_eq!(rows.len(), length);
            }
            length = Some(rows.len());
            if restart == 0 {
                let first_mark = rows
                    .iter()
                    .find(|movement| matches!(movement.kind, MovementKind::MarkOutcomeAdmitted(_)))
                    .unwrap();
                let log = DurableLog::open(root.join("movements.jsonl")).unwrap();
                let (_, end) = log.durable_row_byte_range(first_mark.seq).unwrap();
                drop(log);
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(root.join("movements.jsonl"))
                    .unwrap();
                file.set_len(end).unwrap();
                file.sync_all().unwrap();
            }
        }
    }

    fn seed_test_ee_root(ee_instances_root: &Path) {
        crate::bootstrap_ees::ensure_bootstrap_ee_instances(
            &repository_registry_root(),
            ee_instances_root,
        )
        .expect("test installation seeds its bootstrap EE root");
    }

    fn flash_team_payload(artifact_root: &Path, token: &str) -> Vec<u8> {
        let body = serde_json::json!({
            "prompt": format!(
                "you are the flash worker; the user needs planning, emit a run.init artifact through the formed route ({token})"
            )
        });
        write_test_input_artifact_reference(
            artifact_root.join("interface-request-artifacts"),
            "interface.input",
            &body,
        )
    }

    fn flash_team_submit(
        _blobs: &SharedBlobs,
        artifact_root: &Path,
        token: &str,
    ) -> InterfaceArtifactFile {
        let payload = flash_team_payload(artifact_root, token);
        InterfaceArtifactFile::write(artifact_root, "operator.message", &payload)
            .and_then(|artifact| artifact.capture_bounded(1024 * 1024, 64 * 1024 * 1024))
            .expect("flash team submit captures an interface artifact before runtime hand-in")
    }

    fn write_test_input_artifact_reference(
        root: PathBuf,
        label: &str,
        body: &serde_json::Value,
    ) -> Vec<u8> {
        let bytes = serde_json::to_vec(body).expect("fixture input body encodes");
        let artifact = InterfaceArtifactFile::write(root, label, &bytes)
            .expect("fixture input artifact reference writes");
        std::fs::read(artifact.manifest_path()).expect("fixture input artifact reference reads")
    }

    fn probe_role_bindings() -> Vec<serde_json::Value> {
        let policy: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                repository_registry_root().join("interface/system_worker_coder_max/policy.json"),
            )
            .unwrap(),
        )
        .unwrap();
        policy["role_artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| serde_json::json!({"id": id, "access": {"read": true}}))
            .collect()
    }

    fn literal_terminal_run_record_fixture_for_run(
        interface_artifact_root: &Path,
        run: &RunId,
        checkpoint: &str,
    ) {
        let path = interface_artifact_root
            .join("runs")
            .join(format!("{}.json", run.as_str()));
        write_literal_run_record_fixture(
            &path,
            &serde_json::json!({
                "id": run,
                "ee": EeId::new("ee_existence"),
                "worker": "system_worker_coder_max",
                "trace": format!("trace:{}", run.as_str()),
                "status": "running",
                "interface_state": {},
                "checkpoints": [{
                    "id": checkpoint,
                    "run": run,
                    "trace": format!("trace:{}:{checkpoint}", run.as_str()),
                    "epoch": 0,
                    "interface_binding": octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_terminal")),
                    "outbound_interface_bindings": [octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("world_terminal"))],
                    "artifact_bindings": probe_role_bindings(),
                }],
                "active_checkpoints": [checkpoint],
            }),
        )
        .expect("test run JSON writes");
    }

    fn write_literal_run_record_fixture(
        path: &Path,
        record: &serde_json::Value,
    ) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes =
            serde_json::to_vec_pretty(record).expect("test-authored run record JSON must encode");
        std::fs::write(path, bytes)
    }

    fn literal_daemon_terminal_run_record_fixture(durable_root: &Path, checkpoint: &str) {
        literal_terminal_run_record_fixture_for_run(
            &durable_root.join("octopus-home/state"),
            &RunId::new("run_daemon"),
            checkpoint,
        );
    }

    struct TestDaemon {
        handle: tokio::task::JoinHandle<Result<(), String>>,
        ready: tokio::sync::oneshot::Receiver<()>,
        emissions: Option<tokio::sync::mpsc::Sender<SystemInterfaceEmission>>,
    }

    impl TestDaemon {
        async fn expect_ready(&mut self) {
            match tokio::time::timeout(std::time::Duration::from_secs(20), &mut self.ready).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    let result =
                        tokio::time::timeout(std::time::Duration::from_secs(1), &mut self.handle)
                            .await;
                    panic!("daemon exited before reporting readiness: {result:?}");
                }
                Err(_) => {
                    self.handle.abort();
                    panic!("daemon did not report readiness within 20 seconds");
                }
            }
        }
    }

    fn spawn_test_daemon(
        durable_root: PathBuf,
        blobs: SharedBlobs,
        arrivals: tokio::sync::mpsc::Receiver<Arrival>,
        cancel: CancellationToken,
    ) -> TestDaemon {
        let state_root = durable_root.join("octopus-home/state");
        let (emissions, emission_rx) = tokio::sync::mpsc::channel::<SystemInterfaceEmission>(4);
        let (ready_tx, ready) = tokio::sync::oneshot::channel();
        let handle = tokio::task::spawn_local(async move {
            run_daemon_on_dirs(
                &durable_root,
                repository_registry_root(),
                state_root.join("interface-artifacts"),
                state_root.join("runs"),
                state_root.join("ee_instances"),
                blobs,
                arrivals,
                emission_rx,
                cancel,
                Some(ready_tx),
                None,
            )
            .await
            .map_err(|error| error.to_string())
        });
        TestDaemon {
            handle,
            ready,
            emissions: Some(emissions),
        }
    }

    #[tokio::test]
    async fn run_daemon_hosts_artifact_arrivals_on_one_live_spine() {
        let dir = tempfile::tempdir().expect("temp durable root");
        let durable_root = dir.path().to_path_buf();

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let (tx, rx) = tokio::sync::mpsc::channel::<Arrival>(8);
                let cancel = CancellationToken::new();
                let cancel_for_daemon = cancel.clone();
                let root_for_daemon = durable_root.clone();
                let blobs = durable_test_blobs(&durable_root);
                let blobs_for_daemon = blobs.clone();
                let artifact_root = durable_root.join("interface-artifacts");

                let movements_path = durable_root.join("movements.jsonl");
                literal_daemon_terminal_run_record_fixture(&durable_root, "c1");
                let mut daemon =
                    spawn_test_daemon(root_for_daemon, blobs_for_daemon, rx, cancel_for_daemon);
                daemon.expect_ready().await;
                tx.send(Arrival::fire_and_forget(flash_team_submit(&blobs, &artifact_root, "ONE"))).await.expect("send arrival 1");
                let len_after_one = wait_for_ledger_growth(&movements_path, 0).await;
                assert!(
                    len_after_one > 0,
                    "arrival 1 must have been admitted on the live spine and grown the \
                     durable ledger"
                );

                literal_daemon_terminal_run_record_fixture(&durable_root, "c2");
                tx.send(Arrival::fire_and_forget(flash_team_submit(&blobs, &artifact_root, "TWO"))).await.expect("send arrival 2");
                let len_after_two = wait_for_ledger_growth(&movements_path, len_after_one).await;
                assert!(
                    len_after_two > len_after_one,
                    "arrival 2 must have been admitted on the SAME live spine (not a one-shot exit) — the \
                     durable ledger must grow a SECOND time (was {len_after_one}, now {len_after_two})"
                );

                assert!(
                    !daemon.handle.is_finished(),
                    "the daemon must still be live after >=2 artifact arrivals (always-on, not one-shot)"
                );
                cancel.cancel();
                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(20), daemon.handle)
                    .await
                    .expect("the daemon must shut down cleanly within the timeout on cancel")
                    .expect("the daemon task must not panic");
                result.expect("run_daemon must return Ok(()) on a clean cancel shutdown");

                let final_movements = {
                    let reopened = DurableLog::open(durable_root.join("movements.jsonl"))
                        .expect("reopen the durable ledger");
                    reopened.read_from(octopus_ledger::Seq(0))
                };
                assert!(
                    final_movements.len() >= 2,
                    "the durable ledger must hold the movements from BOTH artifact arrivals driven on the \
                     one always-on spine, got {}",
                    final_movements.len()
                );

                let reopened = DurableLog::open(durable_root.join("movements.jsonl"))
                    .expect("reopen the durable ledger");
                let spotlight = octopus_ledger_index::LedgerSqliteIndex::open(
                    &durable_root.join("ledger-index.sqlite"),
                )
                .expect("the daemon created the spotlight index beside the ledger");
                assert_eq!(
                    spotlight.count().expect("spotlight row count"),
                    final_movements.len() as u64,
                    "every durable movement is indexed in the spotlight cache"
                );
                assert!(
                    spotlight
                        .verify_against(&reopened)
                        .expect("spotlight verify runs"),
                    "the spotlight cache verifies against the reopened durable ledger"
                );
            })
            .await;
    }

    #[tokio::test]
    async fn run_daemon_resumes_prior_state_from_the_durable_ledger_after_a_crash() {
        let dir = tempfile::tempdir().expect("temp durable root");
        let durable_root = dir.path().to_path_buf();
        let movements_path = durable_root.join("movements.jsonl");
        let blobs = durable_test_blobs(&durable_root);

        {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let (tx, rx) = tokio::sync::mpsc::channel::<Arrival>(8);
                    let cancel = CancellationToken::new();
                    let root_for_daemon = durable_root.clone();
                    let blobs_for_daemon = blobs.clone();
                    let artifact_root = durable_root.join("interface-artifacts");

                    literal_daemon_terminal_run_record_fixture(&durable_root, "c1");
                    let mut daemon =
                        spawn_test_daemon(root_for_daemon, blobs_for_daemon, rx, cancel);
                    daemon.expect_ready().await;
                    tx.send(Arrival::fire_and_forget(flash_team_submit(
                        &blobs,
                        &artifact_root,
                        "CRASH",
                    )))
                    .await
                    .expect("send the pre-crash arrival");
                    let grown = wait_for_ledger_growth(&movements_path, 0).await;
                    assert!(
                        grown > 0,
                        "the pre-crash arrival must have grown the durable ledger"
                    );
                    let runs_landed = wait_for_run_movement(&movements_path).await;
                    assert!(
                        runs_landed,
                        "the durable ledger must carry the arrival run movements before the crash"
                    );

                    daemon.handle.abort();
                    let _ = daemon.handle.await;
                })
                .await;
        }

        let base_tree = boot_base_ee_tree().await;
        let base_tree_len = base_tree.len();
        let resumed = rebuild_daemon_state_from_durable_root(&durable_root, base_tree)
            .expect("the durable ledger rebuilds the prior state (fail-closed if corrupt)");
        assert_eq!(
            resumed.ee_tree.len(),
            base_tree_len,
            "artifact arrival replay must not fabricate an EE child scope; interface-authored JSON/scope \
             creation has to arrive as its own explicit artifact/tool effect"
        );
        assert!(
            resumed
                .durable_runs
                .iter()
                .any(|r| r == &RunId::new(DAEMON_BOOT_RUN_ID)),
            "the run fold over the durable ledger must recover the addressed run movement evidence — got {:?}",
            resumed
                .durable_runs
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
        );

        let resumed_again =
            rebuild_daemon_state_from_durable_root(&durable_root, boot_base_ee_tree().await)
                .expect("a second rebuild from the same ledger also succeeds");
        assert_eq!(
            resumed_again.ee_tree.len(),
            resumed.ee_tree.len(),
            "two rebuilds from the same durable ledger produce the same EE tree size (deterministic replay)"
        );
        assert_eq!(
            resumed_again.durable_runs, resumed.durable_runs,
            "two rebuilds from the same durable ledger produce the same run set (deterministic replay)"
        );

        let len_before_resume = std::fs::metadata(&movements_path)
            .map(|m| m.len())
            .unwrap_or(0);
        {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let (tx, rx) = tokio::sync::mpsc::channel::<Arrival>(8);
                    let cancel = CancellationToken::new();
                    let cancel_for_daemon = cancel.clone();
                    let root_for_daemon = durable_root.clone();
                    let blobs_for_daemon = blobs.clone();
                    let artifact_root = durable_root.join("interface-artifacts");

                    literal_daemon_terminal_run_record_fixture(&durable_root, "c2");
                    let mut daemon = spawn_test_daemon(
                        root_for_daemon,
                        blobs_for_daemon,
                        rx,
                        cancel_for_daemon,
                    );
                    daemon.expect_ready().await;
                    tx.send(Arrival::fire_and_forget(flash_team_submit(&blobs, &artifact_root, "RESUMED"))).await.expect("send the post-resume arrival");
                    let grown = wait_for_ledger_growth(&movements_path, len_before_resume).await;
                    assert!(
                        grown > len_before_resume,
                        "the RESUMED daemon must serve a new arrival on top of the recovered durable truth \
                         (the ledger grows past where the crash left it: was {len_before_resume}, now {grown})"
                    );
                    cancel.cancel();
                    let result =
                        tokio::time::timeout(std::time::Duration::from_secs(20), daemon.handle)
                            .await
                            .expect("the resumed daemon shuts down cleanly within the timeout")
                            .expect("the resumed daemon task must not panic");
                    result.expect("the resumed run_daemon returns Ok(()) on clean shutdown");
                })
                .await;
        }
    }

    #[tokio::test]
    async fn rebuild_daemon_state_from_durable_root_fails_closed_on_a_corrupt_row() {
        let dir = tempfile::tempdir().expect("temp durable root (corrupt)");
        let durable_root = dir.path().to_path_buf();
        std::fs::create_dir_all(&durable_root).expect("mkdir durable root");
        std::fs::write(
            durable_root.join("movements.jsonl"),
            "{\"seq\":0,\"lamport\":1,\"trace\":\"t\",\"kind\":\"ArtifactMoved\",\"from\":{\"Run\":\"r1\"},\"to\":{\"Run\":\"r1\"},\"artifact\":null,\"surprise\":true}\n",
        )
        .expect("write the corrupt durable row");

        let base_tree = boot_base_ee_tree().await;
        let result = rebuild_daemon_state_from_durable_root(&durable_root, base_tree);
        assert!(
            result.is_err(),
            "a corrupt/partial durable row must make the resume fail closed (no fabricated state) — \
             DurableLog::open re-validates every row and does NOT skip a malformed line"
        );
        assert!(
            DurableLog::open(durable_root.join("movements.jsonl")).is_err(),
            "DurableLog::open over the corrupt file returns the typed LedgerError the resume propagates"
        );
    }

    async fn boot_base_ee_tree() -> EeTree {
        use octopus_app_server::SharedBlobs;
        use octopus_app_server::{SpineRootConfig, build_embedded_spine};
        use octopus_ledger::InMemoryBlobs;
        let dir = tempfile::tempdir().expect("temp interface artifact root");
        let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
        let interface_artifact_root = dir.path().join("handler-interface-artifacts");
        seed_test_ee_root(&interface_artifact_root.join("ee_instances"));
        let embedded = build_embedded_spine(SpineRootConfig {
            registry_root: repository_registry_root(),
            run: RunId::new("run_base_tree_probe"),
            blobs,
            interface_artifact_root: interface_artifact_root.clone(),
            run_records_root: interface_artifact_root.join("runs"),
            ee_instances_root: interface_artifact_root.join("ee_instances"),
        })
        .await
        .expect("the spine boots; the base EE tree (templates + existence root) is built");
        embedded.ee_tree.clone()
    }

    async fn wait_for_run_movement(path: &Path) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            if let Ok(log) = DurableLog::open(path) {
                let has_run = log
                    .read_from(octopus_ledger::Seq(0))
                    .iter()
                    .any(|m| m.run().is_some());
                if has_run {
                    return true;
                }
            }
            if std::time::Instant::now() > deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn run_daemon_shuts_down_on_closed_intake() {
        let dir = tempfile::tempdir().expect("temp durable root");
        let durable_root = dir.path().to_path_buf();

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let (tx, rx) = tokio::sync::mpsc::channel::<Arrival>(8);
                let cancel = CancellationToken::new();
                let root_for_daemon = durable_root.clone();
                let blobs = durable_test_blobs(&durable_root);
                let blobs_for_daemon = blobs.clone();
                let artifact_root = durable_root.join("interface-artifacts");

                let movements_path = durable_root.join("movements.jsonl");
                literal_daemon_terminal_run_record_fixture(&durable_root, "c1");
                let mut daemon = spawn_test_daemon(root_for_daemon, blobs_for_daemon, rx, cancel);
                daemon.expect_ready().await;
                tx.send(Arrival::fire_and_forget(flash_team_submit(
                    &blobs,
                    &artifact_root,
                    "ONLY",
                )))
                .await
                .expect("send the only arrival");
                let _ = wait_for_ledger_growth(&movements_path, 0).await;
                drop(tx);
                daemon.emissions.take();
                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(20), daemon.handle)
                        .await
                        .expect(
                            "a closed intake must shut the daemon down cleanly within the timeout",
                        )
                        .expect("the daemon task must not panic");
                result.expect("run_daemon must return Ok(()) when the intake closes");
            })
            .await;
    }

    #[tokio::test]
    async fn admit_interface_artifact_arrival_returns_interface_authored_admission() {
        use octopus_ledger::{InMemoryBlobs, InMemoryLog, LiveMovementLog};

        let dir = tempfile::tempdir().expect("temp interface artifact root");
        let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
        let run_flash = RunId::new("run_flash_daemon_unit");
        let interface_artifact_root = dir.path().join("handler-interface-artifacts");
        seed_test_ee_root(&interface_artifact_root.join("ee_instances"));
        let embedded = build_embedded_spine(SpineRootConfig {
            registry_root: repository_registry_root(),
            run: run_flash.clone(),
            blobs: blobs.clone(),
            interface_artifact_root: interface_artifact_root.clone(),
            run_records_root: interface_artifact_root.join("runs"),
            ee_instances_root: interface_artifact_root.join("ee_instances"),
        })
        .await
        .expect("the spine boots; the team tools resolve from the registry");

        literal_terminal_run_record_fixture_for_run(
            &embedded.interface_artifact_root,
            &embedded.run,
            "c1",
        );
        let mut log = LiveMovementLog::new(InMemoryLog::new());
        let mut arrival_blobs = SharedBlobHandle::new(blobs.clone());
        let outcome = admit_interface_artifact_arrival(
            &embedded,
            &mut log,
            &mut arrival_blobs,
            &flash_team_submit(&blobs, &dir.path().join("interface-artifacts"), "UNIT"),
            &InterfaceId::new("world_terminal"),
        )
        .await
        .expect("one interface artifact admission is admitted through the runtime gate");

        assert!(
            outcome.admission.first_outbound_artifact().is_some(),
            "the runtime should record an outbound artifact transfer for the target interface"
        );
    }

    async fn wait_for_ledger_growth(path: &Path, min_len: u64) -> u64 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            if let Ok(meta) = std::fs::metadata(path) {
                let len = meta.len();
                if len > min_len {
                    return len;
                }
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timed out waiting for the durable ledger at {} to grow past {min_len} bytes — the \
                     always-on spine did not admit the artifact arrival",
                    path.display()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    struct BarrierVerifiedArtifactAdmission {
        channels: std::sync::Mutex<
            Option<(
                tokio::sync::oneshot::Sender<()>,
                tokio::sync::oneshot::Receiver<()>,
            )>,
        >,
    }

    impl octopus_marks::VerifiedArtifactAdmission for BarrierVerifiedArtifactAdmission {
        fn prepare_required_verified_artifact(
            &self,
            required: &octopus_marks::RequiredVerifiedArtifact,
            input: octopus_marks::VerificationInput,
        ) -> Result<
            Box<dyn octopus_marks::PreparedVerification>,
            octopus_marks::VerifiedArtifactError,
        > {
            let (reached_tx, release_rx) = self
                .channels
                .lock()
                .expect("barrier channels mutex is not poisoned")
                .take()
                .expect("the barrier admission prepares exactly one verification");
            Ok(Box::new(BarrierPreparedVerification {
                reached_tx,
                release_rx,
                verifier: required.verifier.clone(),
                verifier_package: test_verifier_package_pin(&required.verifier),
                input,
            }))
        }
    }

    struct BarrierPreparedVerification {
        reached_tx: tokio::sync::oneshot::Sender<()>,
        release_rx: tokio::sync::oneshot::Receiver<()>,
        verifier: octopus_descriptors::VerifierId,
        verifier_package: octopus_ledger::VerifierPackagePin,
        input: octopus_marks::VerificationInput,
    }

    impl octopus_marks::PreparedVerification for BarrierPreparedVerification {
        fn run(
            self: Box<Self>,
        ) -> Result<octopus_marks::Mark, octopus_marks::VerifiedArtifactError> {
            let _ = self.reached_tx.send(());
            let _ = self.release_rx.blocking_recv();
            Ok(octopus_marks::Mark::passed(
                self.input.mark,
                self.input.checkpoint,
                self.input.run,
                self.input.epoch,
                self.input.artifact,
                self.verifier,
                self.verifier_package,
                serde_json::json!({ "barrier": true }),
            ))
        }
    }

    fn write_probe_run_record(
        interface_artifact_root: &Path,
        run: &RunId,
        interface: &str,
        required_verified_artifacts: Option<serde_json::Value>,
    ) {
        let mut checkpoint = serde_json::json!({
            "id": "cp",
            "run": run,
            "trace": format!("trace:{}:cp", run.as_str()),
            "epoch": 0,
            "interface_binding": octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new(interface)),
            "outbound_interface_bindings": [],
            "artifact_bindings": probe_role_bindings(),
        });
        if let Some(required) = required_verified_artifacts {
            checkpoint["required_verified_artifacts"] = required;
        }
        let path = interface_artifact_root
            .join("runs")
            .join(format!("{}.json", run.as_str()));
        write_literal_run_record_fixture(
            &path,
            &serde_json::json!({
                "id": run,
                "ee": EeId::new("ee_existence"),
                "worker": "system_worker_coder_max",
                "trace": format!("trace:{}", run.as_str()),
                "status": "running",
                "interface_state": {},
                "checkpoints": [checkpoint],
                "active_checkpoints": ["cp"],
            }),
        )
        .expect("probe run record writes");
    }

    fn plain_worker_artifact(root: &Path, token: &str) -> InterfaceArtifactFile {
        let payload = serde_json::to_vec(&serde_json::json!({
            "label": "coder.build.result",
            "tool": "coder.build",
            "ok": true,
            "body": {
                "result": "command_finished",
                "output": {
                    "exit_code": 0,
                    "stdout": token,
                    "stderr": "",
                }
            },
        }))
        .expect("probe artifact payload encodes");
        InterfaceArtifactFile::write(root, "coder.build.result", &payload)
            .and_then(|artifact| artifact.capture_bounded(1024 * 1024, 64 * 1024 * 1024))
            .expect("probe worker artifact captures before runtime hand-in")
    }

    #[tokio::test]
    async fn a_slow_verifier_does_not_block_a_sibling_of_the_same_arrival() {
        let dir = tempfile::tempdir().expect("temp durable root (concurrency)");
        let blobs: SharedBlobs =
            Arc::new(std::sync::Mutex::new(octopus_ledger::InMemoryBlobs::new()));
        let interface_artifact_root = dir.path().join("handler-interface-artifacts");
        seed_test_ee_root(&interface_artifact_root.join("ee_instances"));
        let embedded = Arc::new(
            build_embedded_spine(SpineRootConfig {
                registry_root: repository_registry_root(),
                run: RunId::new("run_concurrency_probe"),
                blobs: blobs.clone(),
                interface_artifact_root: interface_artifact_root.clone(),
                run_records_root: interface_artifact_root.join("runs"),
                ee_instances_root: interface_artifact_root.join("ee_instances"),
            })
            .await
            .expect("the spine boots for the concurrency contract test"),
        );

        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        embedded.runtime.set_verified_artifact_admission(Arc::new(
            BarrierVerifiedArtifactAdmission {
                channels: std::sync::Mutex::new(Some((reached_tx, release_rx))),
            },
        ));

        write_probe_run_record(
            &embedded.interface_artifact_root,
            &RunId::new("run_slow_a"),
            "system_context",
            Some(serde_json::json!([{
                "mark": "coder.build.ok",
                "artifact_label": "coder.build.result",
                "verifier": "build_success",
                "params": {}
            }])),
        );
        write_probe_run_record(
            &embedded.interface_artifact_root,
            &RunId::new("run_fast_b"),
            "system_context",
            None,
        );

        let writer =
            crate::ledger_writer::LedgerWriter::new(octopus_ledger::InMemoryLog::new(), None);
        let arrivals_root = dir.path().join("concurrency-arrivals");

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let submit_a = plain_worker_artifact(&arrivals_root, "SLOW-A");
                let embedded_a = embedded.clone();
                let mut writer_a = writer.clone();
                let blobs_a = blobs.clone();
                let mut task_a = tokio::task::spawn_local(async move {
                    let mut blob_handle = SharedBlobHandle::new(blobs_a);
                    admit_interface_artifact_arrival(
                        &embedded_a,
                        &mut writer_a,
                        &mut blob_handle,
                        &submit_a,
                        &InterfaceId::new("system_context"),
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                });

                tokio::select! {
                    reached = tokio::time::timeout(std::time::Duration::from_secs(5), reached_rx) => { reached.expect("verifier must start").expect("barrier sender remains alive"); }
                    result = &mut task_a => panic!("admission ended before verifier started: {result:?}"),
                }

                let fast_rows = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let rows = writer.read_from(Seq(0));
                        if rows.iter().any(|movement| movement.kind == MovementKind::InterfaceArrival
                            && movement.run().is_some_and(|run| run.as_str() == "run_fast_b")) {
                            break rows;
                        }
                        tokio::task::yield_now().await;
                    }
                }).await;
                release_tx.send(()).expect("release the barrier verifier even if the sibling stalled");
                let rows = fast_rows.expect("same-artifact sibling admitted while verifier remained blocked");
                let arrival_to = |run: &str| {
                    rows.iter().any(|movement| {
                        matches!(movement.kind, octopus_ledger::MovementKind::InterfaceArrival)
                            && movement.run().is_some_and(|r| r.as_str() == run)
                    })
                };
                assert!(
                    arrival_to("run_fast_b"),
                    "arrival B committed on the shared spine WHILE A's slow verifier ran — admission is concurrent, not queued"
                );
                assert!(
                    !arrival_to("run_slow_a"),
                    "arrival A has NOT committed yet — its verifier is still blocked, proving the runtime does not serialize the whole admission of one EE behind another's verifier"
                );
                let len_while_a_blocked = rows.len();

                task_a
                    .await
                    .expect("task A joins")
                    .expect("arrival A is admitted once its verifier is released");

                let mut refusal_writer = writer.clone();
                let mut refusal_blobs = SharedBlobHandle::new(blobs.clone());
                let before_refusal = refusal_writer.len();
                let missing = InterfaceArtifactFile::new(arrivals_root.join("missing.json")).unwrap();
                assert!(admit_interface_artifact_arrival(&embedded, &mut refusal_writer, &mut refusal_blobs,
                    &missing, &InterfaceId::new("system_context")).await.is_err());
                let missing_reference = ArtifactRef { id: ArtifactId::new("missing:manifest"), content: ContentHash::of_bytes(b"missing manifest") };
                let mut emitter = octopus_app_server::InterfaceArtifactArrivalFromInterface::new(&embedded.runtime,
                    &mut refusal_writer, &mut refusal_blobs, embedded.arrival_dedup.clone(), TraceId("missing-reference".into()),
                    InterfaceId::new("system_context"));
                assert!(emitter.emit_artifact_reference_to_interface(&missing_reference, &RunId::new("run_fast_b"),
                    &CheckpointId::new("cp"), octopus_run_record::GrantEpoch(0), octopus_ledger::Lamport(0), &InterfaceId::new("system_worker_planner")).await.is_err());
                let refused = refusal_writer.read_from(Seq(before_refusal as u64));
                assert_eq!(refused.len(), 2);
                assert!(refused.iter().all(|row| row.kind == MovementKind::DeliveryRefused));
                assert_eq!(refused[1].artifact.as_ref(), Some(&missing_reference));

                let final_rows = writer.read_from(Seq(0));
                assert!(
                    final_rows.iter().any(|movement| {
                        matches!(movement.kind, octopus_ledger::MovementKind::InterfaceArrival)
                            && movement.run().is_some_and(|r| r.as_str() == "run_slow_a")
                    }),
                    "arrival A commits its InterfaceArrival movement after the verifier is released"
                );
                assert!(
                    final_rows.len() > len_while_a_blocked,
                    "the shared ledger grew again with A's movements after release"
                );
                assert!(
                    final_rows.windows(2).all(|w| w[0].seq.0 + 1 == w[1].seq.0),
                    "the single dedicated writer preserved a strict append-only seq order across both concurrent admissions (movement is holy)"
                );
            })
            .await;
    }
}
