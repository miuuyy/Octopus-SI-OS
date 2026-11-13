mod interface_life;
mod life_ownership;
mod life_process;
pub mod runtime_instance;

pub use life_ownership::InterfaceLifeOwnership;

pub use octopus_verifiers::process_authority;

pub use interface_life::{
    INTERFACE_HOST_IO_LIMITS_ENV, InterfaceHostIoLimits, InterfaceLifeFuture, InterfaceLiftContext,
    InterfaceLiftFn, InterfaceLifeReadyPublication, RESOLVED_INTERFACE_DESCRIPTOR_ENV, ResolvedSpawnLifeConfig,
    ResolvedSpawnLifeExecutable, SpawnLifeArgument, SpawnLifeCommand, SpawnLifeConfig,
    SpawnLifeDoctorCommand, SpawnLifeRepositoryFileArgument, ValidatedSpawnLifeConfig,
    INHERITED_LISTENERS_ENV, InterfaceSleepDeclaration,
    capture_spawned_artifact, interfaces_mentioned_in_live_runs, read_private_environment_document,
    resolve_interface_descriptor, resolve_spawn_life_config,
    resolve_spawn_life_executable, spawn_life_config_of, spawn_process_life,
    validate_spawn_life_config,
};
#[cfg(unix)]
pub use interface_life::take_inherited_listener;
use notify::{RecursiveMode, Watcher};
use octopus_descriptors::{ArtifactId, InterfaceId};
use octopus_interfaces::{DeliveryVerdict, Direction, InterfaceArtifactFile, InterfaceDescriptor};
use octopus_ledger::{ArtifactDelivery, ArtifactRef, ContentHash};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub(crate) const INTERFACE_LIFE_INSTALL_ROOT_SLOT: &str = "interface_life_install_root";

pub type InterfaceMovementWakes = tokio::sync::mpsc::UnboundedSender<(InterfaceId, u64)>;
pub type InterfaceMovementWakeReceiver = tokio::sync::mpsc::UnboundedReceiver<(InterfaceId, u64)>;

pub fn interface_movement_wakes() -> (InterfaceMovementWakes, InterfaceMovementWakeReceiver) {
    tokio::sync::mpsc::unbounded_channel()
}

pub fn movement_readers(movement: &octopus_ledger::Movement) -> Vec<InterfaceId> {
    if movement.kind != octopus_ledger::MovementKind::InterfaceOutbound
        || !matches!(movement.from, octopus_ledger::Endpoint::Interface(_))
    {
        return Vec::new();
    }
    let octopus_ledger::Endpoint::Interface(to) = &movement.to;
    let mut readers = vec![to.clone()];
    if let Some(runtime) = movement.runtime() {
        if runtime.interface != *to {
            readers.push(runtime.interface.clone());
        }
    }
    readers
}

pub(crate) const INTERFACE_LOCAL_IPC_ROOT_SLOT: &str = "interface_local_ipc_root";
const APP_SERVER_CONTROL_ROOT_SLOT: &str = "app_server_control_root";

#[derive(Clone, Debug)]
pub struct InterfaceActivationEvidence {
    pub interface_id: InterfaceId,
    pub artifact_id: ArtifactId,
    pub label: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub struct InterfaceActivationArrival {
    pub artifact: InterfaceArtifactFile,
    pub interface: InterfaceId,
    pub evidence: Vec<InterfaceActivationEvidence>,
    pub verdict_tx: Option<tokio::sync::oneshot::Sender<DeliveryVerdict>>,
    pub retained: Option<RetainedInterfaceArrival>,
}

#[derive(Debug)]
pub struct RetainedInterfaceArrival {
    pub id: String,
    pub reply: tokio::sync::oneshot::Sender<Result<ArtifactDelivery, String>>,
}

#[derive(Debug)]
pub enum SystemInterfaceEmissionArtifact {
    Materialized(InterfaceArtifactFile),
    Reference(ArtifactRef),
}

#[derive(Debug)]
pub struct SystemInterfaceEmission {
    artifact: SystemInterfaceEmissionArtifact,
    from_interface: InterfaceId,
    source_movement_seq: u64,
    to_interface: InterfaceId,
    verdict_tx: tokio::sync::oneshot::Sender<DeliveryVerdict>,
}

impl SystemInterfaceEmission {
    fn host_attested(
        artifact: SystemInterfaceEmissionArtifact,
        from_interface: InterfaceId,
        source_movement_seq: u64,
        to_interface: InterfaceId,
        verdict_tx: tokio::sync::oneshot::Sender<DeliveryVerdict>,
    ) -> Self {
        Self {
            artifact,
            from_interface,
            source_movement_seq,
            to_interface,
            verdict_tx,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        SystemInterfaceEmissionArtifact,
        InterfaceId,
        u64,
        InterfaceId,
        tokio::sync::oneshot::Sender<DeliveryVerdict>,
    ) {
        (
            self.artifact,
            self.from_interface,
            self.source_movement_seq,
            self.to_interface,
            self.verdict_tx,
        )
    }
}

#[derive(Clone)]
pub struct ActivationStart {
    pub interface_id: InterfaceId,
    pub config: serde_json::Value,
    pub host_io_limits: InterfaceHostIoLimits,
    pub artifact_root: PathBuf,
    pub tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    pub cancel: CancellationToken,
    pub interface_var_dir: PathBuf,
    pub root_slots: BTreeMap<String, PathBuf>,
    pub sleep: ActivationSleep,
}

#[derive(Clone, Default)]
pub struct ActivationSleep {
    offers: Option<tokio::sync::mpsc::UnboundedSender<(PathBuf, tokio::sync::oneshot::Sender<bool>)>>,
}

pub type ActivationSleepOffers =
    tokio::sync::mpsc::UnboundedReceiver<(PathBuf, tokio::sync::oneshot::Sender<bool>)>;

impl ActivationSleep {
    pub fn channel() -> (Self, ActivationSleepOffers) {
        let (offers, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self { offers: Some(offers) }, receiver)
    }

    pub fn enabled(&self) -> bool {
        self.offers.is_some()
    }

    pub async fn offer(&self, socket: PathBuf) -> bool {
        let Some(offers) = &self.offers else {
            return false;
        };
        let (answer, accepted) = tokio::sync::oneshot::channel();
        offers.send((socket, answer)).is_ok() && accepted.await.unwrap_or(false)
    }
}

pub async fn send_arrival(
    tx: &tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    interface: &InterfaceId,
    artifact: InterfaceArtifactFile,
    evidence: Vec<InterfaceActivationEvidence>,
    verdict_tx: Option<tokio::sync::oneshot::Sender<DeliveryVerdict>>,
) -> Result<(), SendArrivalError> {
    tx.send(InterfaceActivationArrival {
        artifact,
        interface: interface.clone(),
        evidence,
        verdict_tx,
        retained: None,
    })
    .await
    .map_err(|_| SendArrivalError)
}

#[derive(Debug, thiserror::Error)]
#[error("interface arrival intake receiver was dropped")]
pub struct SendArrivalError;

pub type ActivationStartFuture =
    Pin<Box<dyn Future<Output = Result<Vec<tokio::task::JoinHandle<()>>, String>> + Send>>;

pub type ActivationStartFn = fn(ActivationStart) -> ActivationStartFuture;

pub struct InterfaceActivationReconciler {
    registry_root: PathBuf,
    interface_life_install_root: PathBuf,
    tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    emissions_tx: tokio::sync::mpsc::Sender<SystemInterfaceEmission>,
    root_cancel: CancellationToken,
    octopus_home: PathBuf,
    durable_root: Option<PathBuf>,
    run_index: octopus_run_record::RunRecordsIndex,
    resolved_lives: BTreeMap<InterfaceId, CachedResolvedInterfaceLife>,
    imported_executables: interface_life::ImportedExecutableVerification,
    lifted: BTreeMap<InterfaceId, LiftedInterfaceLife>,
    life_exit_tx: tokio::sync::mpsc::UnboundedSender<InterfaceLifeEvent>,
    life_exit_rx: tokio::sync::mpsc::UnboundedReceiver<InterfaceLifeEvent>,
    next_lift_id: u64,
    failures: tokio::sync::watch::Sender<BTreeMap<InterfaceId, String>>,
    waking_limit: usize,
    deferred_lifts: BTreeSet<InterfaceId>,
    deferred_wake_scheduled: bool,
    movement_wakes: Option<InterfaceMovementWakeReceiver>,
    latest_movements: BTreeMap<InterfaceId, u64>,
    live: bool,
}

struct CachedResolvedInterfaceLife {
    source_fingerprint: ContentHash,
    resolved: ResolvedSpawnLifeConfig,
}

struct LiftedInterfaceLife {
    lift_id: u64,
    fingerprint: ContentHash,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
    started_at: Instant,
    restart_attempt: u32,
    died_at: Option<Instant>,
    retry_not_before: Option<Instant>,
    waking: bool,
    ended: bool,
    sleeping: bool,
    woken: bool,
    asleep_through: Option<u64>,
    socket_roots: Vec<PathBuf>,
    #[cfg(unix)]
    held_sockets: Option<HeldSockets>,
}

#[cfg(unix)]
struct HeldSockets {
    listeners: Vec<interface_life::HeldListener>,
    _watch: tokio_util::sync::DropGuard,
}

enum InterfaceLifeEvent {
    Exited {
        interface: InterfaceId,
        lift_id: u64,
        error: String,
    },
    Retry {
        interface: InterfaceId,
        lift_id: u64,
    },
    Readiness {
        interface: InterfaceId,
        lift_id: u64,
        outcome: Result<(), String>,
    },
    WakingSlotElapsed,
    Asleep {
        interface: InterfaceId,
        lift_id: u64,
        declaration: InterfaceSleepDeclaration,
    },
    Knocked {
        interface: InterfaceId,
        lift_id: u64,
    },
}

const LIFE_RESTART_BASE_DELAY: Duration = Duration::from_millis(250);
const LIFE_RESTART_MAX_DELAY: Duration = Duration::from_secs(300);
const LIFE_RESTART_MAX_DELAY_WITH_WORK: Duration = Duration::from_secs(30);
const LIFE_RESTART_STABLE_AFTER: Duration = Duration::from_secs(600);
const LIFE_WAKING_SLOT: Duration = Duration::from_secs(30);
const INTERFACE_WATCH_EVENT_BATCH_LIMIT: usize = 256;

fn interface_life_waking_limit() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .clamp(2, 8)
}

fn interface_life_restart_delay(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    LIFE_RESTART_BASE_DELAY
        .saturating_mul(1u32 << exponent)
        .min(LIFE_RESTART_MAX_DELAY)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InterfaceWatchRoots {
    interface_registry: PathBuf,
    run_records: PathBuf,
    host_directory: PathBuf,
    private_interfaces: Option<PathBuf>,
}

struct ActivationLifeScan {
    enabled: BTreeSet<InterfaceId>,
    errors: BTreeMap<InterfaceId, String>,
}

pub struct InterfaceActivationReconcilerHandle {
    pub reconciler: InterfaceActivationReconciler,
    pub initial_task_count: usize,
    pub initial_failures: BTreeMap<InterfaceId, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterfaceBootEvent {
    Discovered {
        total: usize,
    },
    Ready {
        interface: InterfaceId,
        completed: usize,
        total: usize,
    },
    Failed {
        interface: InterfaceId,
        completed: usize,
        total: usize,
        error: String,
    },
}

impl InterfaceActivationReconciler {
    fn new(
        registry_root: PathBuf,
        interface_life_install_root: PathBuf,
        tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
        emissions_tx: tokio::sync::mpsc::Sender<SystemInterfaceEmission>,
        root_cancel: CancellationToken,
        octopus_home: PathBuf,
        durable_root: Option<PathBuf>,
    ) -> Self {
        let (life_exit_tx, life_exit_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            registry_root,
            interface_life_install_root,
            tx,
            emissions_tx,
            root_cancel,
            octopus_home,
            durable_root,
            run_index: octopus_run_record::RunRecordsIndex::new(),
            resolved_lives: BTreeMap::new(),
            imported_executables: interface_life::ImportedExecutableVerification::default(),
            lifted: BTreeMap::new(),
            life_exit_tx,
            life_exit_rx,
            next_lift_id: 0,
            failures: tokio::sync::watch::channel(BTreeMap::new()).0,
            waking_limit: interface_life_waking_limit(),
            deferred_lifts: BTreeSet::new(),
            deferred_wake_scheduled: false,
            movement_wakes: None,
            latest_movements: BTreeMap::new(),
            live: false,
        }
    }

    fn record_failure(&mut self, interface: &InterfaceId, failure: &str) {
        if self.live && self.failures.borrow().get(interface).map(String::as_str) != Some(failure) {
            eprintln!("interface '{}' is in coma: {failure}", interface.as_str());
        }
        self.failures.send_modify(|failures| {
            failures.insert(interface.clone(), failure.to_string());
        });
    }

    pub fn observe_failures(&self) -> tokio::sync::watch::Receiver<BTreeMap<InterfaceId, String>> {
        self.failures.subscribe()
    }

    fn clear_failure(&self, interface: &InterfaceId) -> bool {
        let mut removed = false;
        self.failures.send_modify(|failures| {
            removed = failures.remove(interface).is_some();
        });
        removed
    }

    fn resolve_interface_life(
        &mut self,
        interface: &InterfaceId,
        verification: &mut interface_life::ImportedExecutableVerification,
    ) -> Result<Option<ResolvedSpawnLifeConfig>, String> {
        if let Some(cached) = self.resolved_lives.get(interface) {
            let fingerprint = interface_life::resolved_spawn_life_source_fingerprint(
                &self.registry_root,
                interface,
                &cached.resolved,
                verification,
            );
            if fingerprint.is_ok_and(|fingerprint| fingerprint == cached.source_fingerprint) {
                return Ok(Some(cached.resolved.clone()));
            }
        }
        self.resolved_lives.remove(interface);
        let resolved = interface_life::resolve_spawn_life_config_with_verification(
            &self.registry_root,
            interface,
            verification,
        )?;
        let Some(resolved) = resolved else {
            return Ok(None);
        };
        let source_fingerprint = interface_life::resolved_spawn_life_source_fingerprint(
            &self.registry_root,
            interface,
            &resolved,
            verification,
        )?;
        self.resolved_lives.insert(
            interface.clone(),
            CachedResolvedInterfaceLife {
                source_fingerprint,
                resolved: resolved.clone(),
            },
        );
        Ok(Some(resolved))
    }

    async fn reconcile_interface_lives(
        &mut self,
        scope: Option<&BTreeSet<InterfaceId>>,
    ) -> Result<(), String> {
        if self.root_cancel.is_cancelled() {
            self.stop_all_lives().await;
            return Err("interface lifecycle reconciliation cancelled".to_string());
        }
        let Some(durable_root) = self.durable_root.clone() else {
            self.stop_all_lives().await;
            return Ok(());
        };
        let root_slots = match life_root_slots(
            &self.registry_root,
            &self.octopus_home,
            &self.interface_life_install_root,
        ) {
            Ok(root_slots) => root_slots,
            Err(error) => {
                self.stop_all_lives().await;
                return Err(error);
            }
        };
        let Some(run_records_dir) = root_slots.get("run_records_root").cloned() else {
            self.stop_all_lives().await;
            return Err(
                "host manifest resolved no run_records_root slot for interface life reconciliation"
                    .to_string(),
            );
        };
        if let Err(error) = self.run_index.catch_up(&run_records_dir) {
            self.stop_all_lives().await;
            return Err(format!(
                "run records directory '{}' cannot be indexed for interface life reconciliation: {error}",
                run_records_dir.display()
            ));
        }
        let mentioned = self.run_index.interfaces_requiring_life();
        let considered = match scope {
            Some(scope) => mentioned.intersection(scope).cloned().collect::<BTreeSet<_>>(),
            None => mentioned.clone(),
        };
        let activation_scan = scan_activation_lives(&self.registry_root, &considered);
        self.failures.send_modify(|failures| {
            failures.retain(|interface, _| mentioned.contains(interface));
        });
        match scope {
            Some(scope) => self.deferred_lifts.retain(|interface| {
                !scope.contains(interface) && mentioned.contains(interface)
            }),
            None => self.deferred_lifts.clear(),
        }
        let mut failures = Vec::new();
        let stale = self
            .lifted
            .keys()
            .filter(|interface| !mentioned.contains(*interface))
            .cloned()
            .collect::<Vec<_>>();
        for interface in stale {
            self.stop_life(&interface).await;
            self.resolved_lives.remove(&interface);
            self.clear_failure(&interface);
        }
        let mut verification = std::mem::take(&mut self.imported_executables);
        for interface in considered {
            tokio::task::yield_now().await;
            if self.root_cancel.is_cancelled() {
                self.stop_all_lives().await;
                return Err("interface lifecycle reconciliation cancelled".to_string());
            }
            if let Some(error) = activation_scan.errors.get(&interface) {
                self.stop_life(&interface).await;
                tracing::warn!(
                    interface = interface.as_str(),
                    error = %error,
                    "bound interface activation is invalid"
                );
                let failure = format!("interface activation is invalid: {error}");
                self.record_failure(&interface, &failure);
                failures.push(format!("interface '{}': {failure}", interface.as_str()));
                continue;
            }
            let resolved = match self.resolve_interface_life(&interface, &mut verification) {
                Ok(Some(resolved)) => resolved,
                Ok(None) => {
                    self.stop_life(&interface).await;
                    if activation_scan.enabled.contains(&interface) {
                        let failure = "activated interface has no life descriptor".to_string();
                        self.record_failure(&interface, &failure);
                        failures.push(format!("interface '{}': {failure}", interface.as_str()));
                    } else {
                        self.clear_failure(&interface);
                    }
                    continue;
                }
                Err(error) => {
                    self.stop_life(&interface).await;
                    tracing::warn!(
                        interface = interface.as_str(),
                        error = %error,
                        "active interface life descriptor is unavailable"
                    );
                    let failure = format!("life descriptor is unavailable: {error}");
                    self.record_failure(&interface, &failure);
                    failures.push(format!("interface '{}': {failure}", interface.as_str()));
                    continue;
                }
            };
            let interface_dir = self
                .registry_root
                .join("interface")
                .join(interface.as_str());
            let interface_root_slots = match merge_interface_root_slots(
                &root_slots,
                &resolved.interface_descriptor,
                &self.octopus_home,
            ) {
                Ok(root_slots) => root_slots,
                Err(error) => {
                    self.stop_life(&interface).await;
                    tracing::warn!(
                        interface = interface.as_str(),
                        error = %error,
                        "active interface life root coordinates are unavailable"
                    );
                    let failure = format!("root coordinates are unavailable: {error}");
                    self.record_failure(&interface, &failure);
                    failures.push(format!("interface '{}': {failure}", interface.as_str()));
                    continue;
                }
            };
            let fingerprint =
                match interface_life::interface_private_environment_source_fingerprint(
                    &self
                        .octopus_home
                        .join("interfaces")
                        .join(interface.as_str()),
                )
                .and_then(|private_environment| {
                    interface_lift_fingerprint(
                        resolved.fingerprint,
                        private_environment,
                        &interface_root_slots,
                    )
                }) {
                    Ok(fingerprint) => fingerprint,
                    Err(error) => {
                        self.stop_life(&interface).await;
                        tracing::warn!(
                            interface = interface.as_str(),
                            error = %error,
                            "active interface life fingerprint is unavailable"
                        );
                        let failure = format!("life fingerprint is unavailable: {error}");
                        self.record_failure(&interface, &failure);
                        failures.push(format!("interface '{}': {failure}", interface.as_str()));
                        continue;
                    }
                };
            let existing = self.lifted.get(&interface);
            let fingerprint_changed =
                existing.is_some_and(|lifted| lifted.fingerprint != fingerprint);
            let restart = existing
                .map(|lifted| {
                    !lifted.sleeping
                        && (fingerprint_changed
                            || (lifted.ended || lifted.handle.is_finished())
                                && lifted
                                    .retry_not_before
                                    .is_none_or(|not_before| Instant::now() >= not_before))
                })
                .unwrap_or(true);
            if !restart {
                continue;
            }
            if self.free_waking_slots(Some(&interface)) == 0 {
                self.deferred_lifts.insert(interface);
                continue;
            }
            let restart_attempt = existing
                .filter(|_| !fingerprint_changed)
                .map(|lifted| lifted.restart_attempt)
                .unwrap_or(0);
            let after_coma = self.live.then(|| self.failures.borrow().get(&interface).cloned()).flatten();
            #[cfg(unix)]
            let inherited_listeners = self
                .lifted
                .get_mut(&interface)
                .and_then(|life| life.held_sockets.take())
                .map(|held| held.listeners)
                .unwrap_or_default();
            self.stop_life(&interface).await;
            let config = resolved.config;
            let descriptor_snapshot = resolved.interface_descriptor;
            tracing::info!(
                interface = interface.as_str(),
                process_count = config.processes.len(),
                "lifting interface life process"
            );
            let life: InterfaceLiftFn =
                Arc::new(move |ctx| interface_life::spawn_process_life(ctx, config.clone()));
            let lift_id = self.next_lift_id;
            self.next_lift_id = self
                .next_lift_id
                .checked_add(1)
                .ok_or_else(|| "interface life lift id overflowed".to_string())?;
            let cancel = self.root_cancel.child_token();
            let (readiness, readiness_rx) = interface_life::InterfaceLifeReadiness::channel();
            let ctx = InterfaceLiftContext {
                interface: interface.clone(),
                interface_dir,
                descriptor_snapshot: Some(descriptor_snapshot),
                var_dir: self
                    .octopus_home
                    .join("interfaces")
                    .join(interface.as_str()),
                movements_path: durable_root.join("movements.jsonl"),
                blobs_dir: durable_root.join("blobs"),
                ledger_index_path: durable_root.join("ledger-index.sqlite"),
                root_slots: interface_root_slots,
                host_io_limits: None,
                arrivals: self.tx.clone(),
                emissions: self.emissions_tx.clone(),
                cancel: cancel.clone(),
                readiness: readiness.clone(),
                asleep: Default::default(),
                #[cfg(unix)]
                inherited_listeners: interface_life::InheritedListeners::new(inherited_listeners),
                #[cfg(not(unix))]
                inherited_listeners: Default::default(),
            };
            let socket_roots = [APP_SERVER_CONTROL_ROOT_SLOT, INTERFACE_LOCAL_IPC_ROOT_SLOT]
                .into_iter()
                .filter_map(|slot| ctx.root_slots.get(slot).cloned())
                .collect::<Vec<_>>();
            let life_sleep = ctx.asleep.clone();
            let lifted_interface = interface.clone();
            let life_exit_tx = self.life_exit_tx.clone();
            let life_cancel = cancel.clone();
            let handle = tokio::spawn(async move {
                let result = life(ctx).await;
                if result.is_ok() && !life_cancel.is_cancelled() && readiness.signaled() {
                    if let Some(declaration) = life_sleep.declared() {
                        let _ = life_exit_tx.send(InterfaceLifeEvent::Asleep {
                            interface: lifted_interface,
                            lift_id,
                            declaration,
                        });
                        return;
                    }
                }
                let failure = match result {
                    Ok(()) => format!("interface life '{}' ended", lifted_interface.as_str()),
                    Err(error) => error,
                };
                let _ = readiness.signal(Err(failure.clone()));
                if !life_cancel.is_cancelled() {
                    eprintln!(
                        "interface life '{}' stopped: {failure}",
                        lifted_interface.as_str()
                    );
                    tracing::warn!(
                        interface = lifted_interface.as_str(),
                        error = %failure,
                        "interface life stopped"
                    );
                }
                let _ = life_exit_tx.send(InterfaceLifeEvent::Exited {
                    interface: lifted_interface,
                    lift_id,
                    error: failure,
                });
            });
            if let Some(previous) = after_coma {
                eprintln!(
                    "interface life '{}' is lifted again after coma (restart attempt {restart_attempt}): {previous}",
                    interface.as_str()
                );
            }
            {
                let events = self.life_exit_tx.clone();
                let interface = interface.clone();
                tokio::spawn(async move {
                    let outcome = readiness_rx
                        .await
                        .unwrap_or_else(|_| Err("readiness channel closed".to_string()));
                    let _ = events.send(InterfaceLifeEvent::Readiness { interface, lift_id, outcome });
                });
            }
            self.lifted.insert(
                interface,
                LiftedInterfaceLife {
                    lift_id,
                    fingerprint,
                    cancel,
                    handle,
                    started_at: Instant::now(),
                    restart_attempt,
                    died_at: None,
                    retry_not_before: None,
                    waking: true,
                    ended: false,
                    sleeping: false,
                    woken: false,
                    asleep_through: None,
                    socket_roots,
                    #[cfg(unix)]
                    held_sockets: None,
                },
            );
        }
        if scope.is_none() {
            verification.finish_pass();
            self.latest_movements
                .retain(|interface, _| mentioned.contains(interface));
        }
        self.imported_executables = verification;
        if !self.deferred_lifts.is_empty() && !self.deferred_wake_scheduled {
            self.deferred_wake_scheduled = true;
            let events = self.life_exit_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(LIFE_WAKING_SLOT).await;
                let _ = events.send(InterfaceLifeEvent::WakingSlotElapsed);
            });
        }
        if !self.deferred_lifts.is_empty() {
            tracing::info!(
                deferred = self.deferred_lifts.len(),
                waking_limit = self.waking_limit,
                "interface lives wait for waking siblings before they are lifted"
            );
        }
        if !failures.is_empty() {
            tracing::warn!(
                failures = failures.len(),
                details = %failures.join("; "),
                "interface-local lives remain in coma while healthy siblings continue"
            );
        }
        Ok(())
    }

    fn free_waking_slots(&self, lifting: Option<&InterfaceId>) -> usize {
        let waking = self
            .lifted
            .iter()
            .filter(|(interface, life)| {
                Some(*interface) != lifting
                    && life.waking
                    && life.started_at.elapsed() < LIFE_WAKING_SLOT
            })
            .count();
        self.waking_limit.saturating_sub(waking)
    }

    fn schedule_life_retry(&mut self, interface: &InterfaceId, lift_id: u64) {
        let Some(lifted) = self.lifted.get_mut(interface) else {
            return;
        };
        if lifted.lift_id != lift_id {
            return;
        }
        if lifted.retry_not_before.is_some() {
            return;
        }
        if lifted.started_at.elapsed() >= LIFE_RESTART_STABLE_AFTER {
            lifted.restart_attempt = 0;
        }
        lifted.restart_attempt = lifted.restart_attempt.saturating_add(1);
        let delay = interface_life_restart_delay(lifted.restart_attempt);
        let died_at = Instant::now();
        lifted.died_at = Some(died_at);
        lifted.retry_not_before = Some(died_at + delay);
        let event_tx = self.life_exit_tx.clone();
        let interface = interface.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = event_tx.send(InterfaceLifeEvent::Retry { interface, lift_id });
        });
    }

    async fn stop_life(&mut self, interface: &InterfaceId) {
        if let Some(lifted) = self.lifted.remove(interface) {
            lifted.cancel.cancel();
            if let Err(error) = lifted.handle.await {
                tracing::warn!(
                    interface = interface.as_str(),
                    error = %error,
                    "interface life task could not finish cleanup"
                );
            }
        }
    }

    async fn stop_all_lives(&mut self) {
        self.deferred_lifts.clear();
        self.imported_executables = interface_life::ImportedExecutableVerification::default();
        let lifted = std::mem::take(&mut self.lifted);
        for life in lifted.values() {
            life.cancel.cancel();
        }
        for (interface, life) in lifted {
            if let Err(error) = life.handle.await {
                tracing::warn!(
                    interface = interface.as_str(),
                    error = %error,
                    "interface life task could not finish cleanup"
                );
            }
        }
    }

    async fn await_initial_readiness(
        &mut self,
        total: usize,
        mut completed: usize,
        mut report: Option<&mut dyn FnMut(InterfaceBootEvent)>,
    ) -> Result<usize, String> {
        let mut awaiting = self
            .lifted
            .keys()
            .chain(self.deferred_lifts.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut ready = 0usize;
        while !awaiting.is_empty() {
            let event = tokio::select! {
                biased;
                _ = self.root_cancel.cancelled() => {
                    self.stop_all_lives().await;
                    return Err("interface lifecycle initialization cancelled".to_string());
                }
                event = self.life_exit_rx.recv() => event
                    .ok_or_else(|| "interface life event channel closed during initialization".to_string())?,
            };
            let mut outcomes = Vec::new();
            if let Some((interface, outcome)) = self.handle_life_event(event).await {
                if awaiting.remove(&interface) {
                    outcomes.push((interface, outcome.map_err(|error| format!("failed before readiness: {error}"))));
                }
            }
            let unbound = awaiting
                .iter()
                .filter(|interface| {
                    !self.lifted.contains_key(*interface) && !self.deferred_lifts.contains(*interface)
                })
                .cloned()
                .collect::<Vec<_>>();
            for interface in unbound {
                awaiting.remove(&interface);
                let failure = self
                    .failures
                    .borrow()
                    .get(&interface)
                    .cloned()
                    .unwrap_or_else(|| "interface life is no longer lifted".to_string());
                outcomes.push((interface, Err(failure)));
            }
            for (interface, outcome) in outcomes {
                completed = completed.saturating_add(1);
                let event = match outcome {
                    Ok(()) => {
                        ready = ready.saturating_add(1);
                        InterfaceBootEvent::Ready { interface, completed, total }
                    }
                    Err(error) => InterfaceBootEvent::Failed { interface, completed, total, error },
                };
                if let Some(report) = report.as_deref_mut() {
                    report(event);
                }
            }
        }
        Ok(ready)
    }

    async fn handle_life_event(
        &mut self,
        event: InterfaceLifeEvent,
    ) -> Option<(InterfaceId, Result<(), String>)> {
        match event {
            InterfaceLifeEvent::Exited { interface, lift_id, error } => {
                let current = self.lifted.get_mut(&interface).filter(|life| {
                    life.lift_id == lift_id && !life.cancel.is_cancelled()
                })?;
                let was_waking = std::mem::replace(&mut current.waking, false);
                current.ended = true;
                self.record_failure(&interface, &error);
                self.schedule_life_retry(&interface, lift_id);
                if was_waking {
                    self.lift_deferred().await;
                }
                None
            }
            InterfaceLifeEvent::Retry { interface, lift_id } => {
                let current = self
                    .lifted
                    .get(&interface)
                    .is_some_and(|lifted| lifted.lift_id == lift_id);
                if current {
                    let scope = BTreeSet::from([interface.clone()]);
                    if let Err(error) = self.reconcile_scope(Some(&scope)).await {
                        eprintln!(
                            "interface life '{}' could not be lifted again: {error}",
                            interface.as_str()
                        );
                    }
                }
                None
            }
            InterfaceLifeEvent::Readiness { interface, lift_id, outcome } => {
                let current = self
                    .lifted
                    .get_mut(&interface)
                    .filter(|lifted| lifted.lift_id == lift_id)?;
                let was_waking = std::mem::replace(&mut current.waking, false);
                match &outcome {
                    Ok(()) => {
                        if self.clear_failure(&interface) && self.live {
                            eprintln!("interface '{}' woke from coma", interface.as_str());
                        }
                    }
                    Err(error) => {
                        self.record_failure(&interface, &format!("failed before readiness: {error}"));
                        self.schedule_life_retry(&interface, lift_id);
                    }
                }
                if was_waking {
                    self.lift_deferred().await;
                }
                Some((interface, outcome))
            }
            InterfaceLifeEvent::WakingSlotElapsed => {
                self.deferred_wake_scheduled = false;
                self.lift_deferred().await;
                None
            }
            InterfaceLifeEvent::Asleep { interface, lift_id, declaration } => {
                let events = self.life_exit_tx.clone();
                let current = self
                    .lifted
                    .get_mut(&interface)
                    .filter(|life| life.lift_id == lift_id && !life.cancel.is_cancelled())?;
                let was_waking = std::mem::replace(&mut current.waking, false);
                current.ended = true;
                current.sleeping = true;
                current.asleep_through = declaration.through_seq;
                current.restart_attempt = 0;
                current.retry_not_before = None;
                #[cfg(unix)]
                let held = hold_sleeping_sockets(&interface, lift_id, &declaration.sockets, &current.socket_roots, events);
                #[cfg(not(unix))]
                let held: Result<(), String> = if declaration.sockets.is_empty() {
                    Ok(())
                } else {
                    Err("sleeping sockets are held only on unix".to_string())
                };
                #[cfg(unix)]
                let held = held.map(|held| current.held_sockets = held);
                if let Err(error) = held {
                    self.record_failure(&interface, &format!("sleeping life sockets cannot be held: {error}"));
                    self.wake_life(&interface).await;
                    return None;
                }
                self.clear_failure(&interface);
                tracing::debug!(interface = interface.as_str(), "interface life fell asleep");
                if declaration.through_seq.is_some_and(|through| {
                    self.latest_movements
                        .get(&interface)
                        .is_some_and(|seq| *seq >= through)
                }) {
                    self.wake_life(&interface).await;
                } else if was_waking {
                    self.lift_deferred().await;
                }
                None
            }
            InterfaceLifeEvent::Knocked { interface, lift_id } => {
                if self
                    .lifted
                    .get(&interface)
                    .is_some_and(|life| life.lift_id == lift_id && life.sleeping)
                {
                    self.wake_life(&interface).await;
                }
                None
            }
        }
    }

    pub fn attach_movement_wakes(&mut self, wakes: InterfaceMovementWakeReceiver) {
        self.movement_wakes = Some(wakes);
    }

    async fn movement_reached(&mut self, interface: InterfaceId, seq: u64) {
        let latest = self.latest_movements.entry(interface.clone()).or_insert(seq);
        *latest = (*latest).max(seq);
        let Some(life) = self.lifted.get_mut(&interface) else {
            return;
        };
        if life.sleeping {
            if life.asleep_through.is_some_and(|through| seq >= through) {
                self.wake_life(&interface).await;
            }
            return;
        }
        let (Some(died_at), Some(not_before)) = (life.died_at, life.retry_not_before) else {
            return;
        };
        let sooner = died_at + LIFE_RESTART_MAX_DELAY_WITH_WORK;
        if sooner >= not_before {
            return;
        }
        life.retry_not_before = Some(sooner);
        let lift_id = life.lift_id;
        let event_tx = self.life_exit_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(sooner.into()).await;
            let _ = event_tx.send(InterfaceLifeEvent::Retry { interface, lift_id });
        });
    }

    async fn wake_life(&mut self, interface: &InterfaceId) {
        let Some(life) = self.lifted.get_mut(interface) else {
            return;
        };
        life.sleeping = false;
        life.woken = true;
        life.asleep_through = None;
        let scope = BTreeSet::from([interface.clone()]);
        if let Err(error) = self.reconcile_scope(Some(&scope)).await {
            tracing::warn!(
                interface = interface.as_str(),
                error = %error,
                "interface life could not be woken by its movement"
            );
        }
    }

    async fn lift_deferred(&mut self) {
        loop {
            let free = self.free_waking_slots(None);
            if free == 0 || self.deferred_lifts.is_empty() {
                return;
            }
            let woken = |interface: &&InterfaceId| {
                self.lifted.get(*interface).is_some_and(|life| life.woken)
            };
            let scope = self
                .deferred_lifts
                .iter()
                .filter(woken)
                .chain(self.deferred_lifts.iter().filter(|interface| !woken(interface)))
                .take(free)
                .cloned()
                .collect::<BTreeSet<_>>();
            let deferred = self.deferred_lifts.len();
            if let Err(error) = self.reconcile_scope(Some(&scope)).await {
                tracing::warn!(
                    error = %error,
                    "interface life reconciliation failed closed while lifting deferred lives"
                );
                return;
            }
            if self.deferred_lifts.len() >= deferred {
                return;
            }
        }
    }

    pub async fn run(mut self) {
        self.live = true;
        let mut roots = match self.watch_roots() {
            Ok(roots) => roots,
            Err(error) => {
                self.stop_all_lives().await;
                tracing::error!(error = %error, "interface life watch roots are unavailable");
                return;
            }
        };
        let (mut watcher, mut events) = match interface_watcher(&roots) {
            Ok(watcher) => watcher,
            Err(error) => {
                self.stop_all_lives().await;
                tracing::error!(error = %error, "interface life filesystem watcher failed to start");
                return;
            }
        };
        if let Err(error) = self.reconcile_once().await {
            tracing::warn!(
                error = %error,
                "interface life reconciliation failed closed after filesystem watch activation"
            );
        }
        loop {
            tokio::select! {
                _ = self.root_cancel.cancelled() => break,
                event = events.recv() => {
                    let Some(event) = event else {
                        tracing::error!("interface life filesystem watcher channel closed");
                        break;
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            tracing::error!(error = %error, "interface life filesystem watcher failed");
                            break;
                        }
                    };
                    let mut event_error = None;
                    let mut reconcile = interface_watch_event_requires_reconcile(&event, &roots);
                    for _ in 1..INTERFACE_WATCH_EVENT_BATCH_LIMIT {
                        let Ok(event) = events.try_recv() else {
                            break;
                        };
                        match event {
                            Ok(event) => {
                                reconcile |= interface_watch_event_requires_reconcile(&event, &roots);
                            }
                            Err(error) => {
                                event_error = Some(error);
                                break;
                            }
                        }
                    }
                    if let Some(error) = event_error {
                        tracing::error!(error = %error, "interface life filesystem watcher failed");
                        break;
                    }
                    if !reconcile { continue; }
                    if let Err(error) = self.reconcile_once().await {
                        tracing::warn!(
                            error = %error,
                            "interface life reconciliation failed closed; lifted lives were stopped"
                        );
                    }
                    match self.watch_roots() {
                        Ok(next_roots) if next_roots != roots => {
                            match interface_watcher(&next_roots) {
                                Ok((next_watcher, next_events)) => {
                                    watcher = next_watcher;
                                    events = next_events;
                                    roots = next_roots;
                                    if let Err(error) = self.reconcile_once().await {
                                        tracing::warn!(
                                            error = %error,
                                            "interface life reconciliation failed closed after watch-root change"
                                        );
                                    }
                                }
                                Err(error) => {
                                    tracing::error!(error = %error, "interface life filesystem watcher failed to follow changed roots");
                                    break;
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                "interface life watch-root refresh failed closed; existing host-directory watch remains active"
                            );
                        }
                    }
                }
                event = self.life_exit_rx.recv() => {
                    let Some(event) = event else {
                        tracing::error!("interface life completion channel closed");
                        break;
                    };
                    self.handle_life_event(event).await;
                }
                wake = async { self.movement_wakes.as_mut().expect("guarded movement wakes").recv().await },
                    if self.movement_wakes.is_some() =>
                {
                    match wake {
                        Some((interface, seq)) => self.movement_reached(interface, seq).await,
                        None => {
                            tracing::error!("interface movement wake channel closed");
                            break;
                        }
                    }
                }
            }
        }
        drop(watcher);
        self.stop_all_lives().await;
    }

    fn watch_roots(&self) -> Result<InterfaceWatchRoots, String> {
        let root_slots = life_root_slots(
            &self.registry_root,
            &self.octopus_home,
            &self.interface_life_install_root,
        )?;
        let run_records = root_slots.get("run_records_root").cloned().ok_or_else(|| {
            "host manifest resolved no run_records_root slot for interface life watch".to_string()
        })?;
        let private_interfaces = self.octopus_home.join("interfaces");
        let private_interfaces = match std::fs::symlink_metadata(&private_interfaces) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                Some(private_interfaces)
            }
            Ok(_) => {
                return Err(format!(
                    "interface private root '{}' is not a plain directory",
                    private_interfaces.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    let mut builder = std::fs::DirBuilder::new();
                    builder.mode(0o700);
                    match builder.create(&private_interfaces) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => {
                            return Err(format!(
                                "interface private root '{}' cannot be created: {error}",
                                private_interfaces.display()
                            ));
                        }
                    }
                }
                #[cfg(not(unix))]
                match std::fs::create_dir(&private_interfaces) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(format!(
                            "interface private root '{}' cannot be created: {error}",
                            private_interfaces.display()
                        ));
                    }
                }
                let metadata = std::fs::symlink_metadata(&private_interfaces).map_err(|error| {
                    format!(
                        "interface private root '{}' cannot be inspected after creation: {error}",
                        private_interfaces.display()
                    )
                })?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(format!(
                        "interface private root '{}' is not a plain directory",
                        private_interfaces.display()
                    ));
                }
                Some(private_interfaces)
            }
            Err(error) => {
                return Err(format!(
                    "interface private root '{}' cannot be inspected: {error}",
                    private_interfaces.display()
                ));
            }
        };
        Ok(InterfaceWatchRoots {
            interface_registry: self.registry_root.join("interface"),
            run_records,
            host_directory: self.octopus_home.clone(),
            private_interfaces,
        })
    }

    async fn reconcile_once(&mut self) -> Result<usize, Box<dyn std::error::Error>> {
        self.reconcile_interface_lives(None).await?;
        Ok(self.lifted.len())
    }

    async fn reconcile_scope(&mut self, scope: Option<&BTreeSet<InterfaceId>>) -> Result<(), String> {
        self.reconcile_interface_lives(scope).await
    }
}

#[cfg(unix)]
fn hold_sleeping_sockets(
    interface: &InterfaceId,
    lift_id: u64,
    sockets: &[PathBuf],
    socket_roots: &[PathBuf],
    events: tokio::sync::mpsc::UnboundedSender<InterfaceLifeEvent>,
) -> Result<Option<HeldSockets>, String> {
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if sockets.is_empty() {
        return Ok(None);
    }
    let watch = CancellationToken::new();
    let mut listeners = Vec::new();
    for socket in sockets {
        if !socket
            .parent()
            .is_some_and(|parent| socket_roots.iter().any(|root| root == parent))
        {
            return Err(format!(
                "socket '{}' is not in a socket root the host formed for this life",
                socket.display()
            ));
        }
        match std::fs::symlink_metadata(socket) {
            Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(socket)
                .map_err(|error| format!("stale socket '{}' cannot be removed: {error}", socket.display()))?,
            Ok(_) => return Err(format!("'{}' is not a socket", socket.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("socket '{}' cannot be inspected: {error}", socket.display())),
        }
        let listener = std::os::unix::net::UnixListener::bind(socket)
            .map_err(|error| format!("socket '{}' cannot be bound: {error}", socket.display()))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("socket '{}' permissions cannot be set: {error}", socket.display()))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("socket '{}' cannot be made nonblocking: {error}", socket.display()))?;
        let fd = Arc::new(OwnedFd::from(listener));
        let readable = unsafe {
            tokio::io::unix::AsyncFd::register_with_interest(
                SharedListenerFd(Arc::clone(&fd)),
                tokio::io::Interest::READABLE,
            )
        }
        .map_err(|error| format!("socket '{}' cannot be watched: {}", socket.display(), error.into_parts().1))?;
        let watch = watch.clone();
        let events = events.clone();
        let interface = interface.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = watch.cancelled() => {}
                ready = readable.readable() => {
                    if ready.is_ok() {
                        let _ = events.send(InterfaceLifeEvent::Knocked { interface, lift_id });
                    }
                }
            }
        });
        listeners.push((socket.clone(), fd));
    }
    Ok(Some(HeldSockets {
        listeners,
        _watch: watch.drop_guard(),
    }))
}

#[cfg(unix)]
struct SharedListenerFd(Arc<std::os::fd::OwnedFd>);

#[cfg(unix)]
impl std::os::fd::AsRawFd for SharedListenerFd {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}

fn scan_activation_lives(
    registry_root: &Path,
    bound_interfaces: &BTreeSet<InterfaceId>,
) -> ActivationLifeScan {
    let mut scan = ActivationLifeScan {
        enabled: BTreeSet::new(),
        errors: BTreeMap::new(),
    };
    for interface in bound_interfaces {
        let directory = registry_root.join("interface").join(interface.as_str());
        let activation_path = directory.join("activation.json");
        let metadata = match std::fs::symlink_metadata(&activation_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                scan.errors.insert(
                    interface.clone(),
                    format!(
                        "interface activation '{}' cannot be inspected: {error}",
                        activation_path.display()
                    ),
                );
                continue;
            }
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            scan.errors.insert(
                interface.clone(),
                format!(
                    "interface activation '{}' is not a regular file",
                    activation_path.display()
                ),
            );
            continue;
        }
        let loaded = match octopus_interface_projection::interface_activation::load_interface_activation_file_for_interface(
            registry_root,
            interface,
        ) {
            Ok(loaded) => loaded,
            Err(error) => {
                scan.errors.insert(interface.clone(), error);
                continue;
            }
        };
        if !loaded
            .descriptor
            .activations
            .iter()
            .any(|activation| activation.enabled)
        {
            continue;
        }
        match interface_life::resolve_interface_descriptor(registry_root, interface) {
            Ok(Some(descriptor))
                if matches!(
                    descriptor.direction,
                    Direction::Inbound | Direction::Bidirectional
                ) =>
            {
                scan.enabled.insert(interface.clone());
            }
            Ok(Some(_)) => {
                scan.errors.insert(
                    interface.clone(),
                    format!(
                        "interface activation '{}' points at non-inbound interface '{}'",
                        activation_path.display(),
                        interface.as_str()
                    ),
                );
            }
            Ok(None) => {
                scan.errors.insert(
                    interface.clone(),
                    format!(
                        "interface activation '{}' points at missing interface '{}'",
                        activation_path.display(),
                        interface.as_str()
                    ),
                );
            }
            Err(error) => {
                scan.errors.insert(interface.clone(), error);
            }
        }
    }
    scan
}

fn interface_watcher(
    roots: &InterfaceWatchRoots,
) -> Result<
    (
        notify::RecommendedWatcher,
        tokio::sync::mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    ),
    String,
> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let watched_roots = roots.clone();
    let mut watcher = notify::recommended_watcher(move |event| {
        enqueue_interface_watch_event(&tx, event, &watched_roots);
    })
    .map_err(|error| format!("filesystem watcher cannot be created: {error}"))?;
    watcher
        .watch(&roots.interface_registry, RecursiveMode::Recursive)
        .map_err(|error| {
            format!(
                "interface registry '{}' cannot be watched: {error}",
                roots.interface_registry.display()
            )
        })?;
    watcher
        .watch(&roots.run_records, RecursiveMode::NonRecursive)
        .map_err(|error| {
            format!(
                "run records '{}' cannot be watched: {error}",
                roots.run_records.display()
            )
        })?;
    watcher
        .watch(&roots.host_directory, RecursiveMode::NonRecursive)
        .map_err(|error| {
            format!(
                "host directory '{}' cannot be watched: {error}",
                roots.host_directory.display()
            )
        })?;
    if let Some(private_interfaces) = &roots.private_interfaces {
        watcher
            .watch(private_interfaces, RecursiveMode::Recursive)
            .map_err(|error| {
                format!(
                    "interface private root '{}' cannot be watched: {error}",
                    private_interfaces.display()
                )
            })?;
    }
    Ok((watcher, rx))
}

fn enqueue_interface_watch_event(
    tx: &tokio::sync::mpsc::UnboundedSender<notify::Result<notify::Event>>,
    event: notify::Result<notify::Event>,
    roots: &InterfaceWatchRoots,
) {
    let should_enqueue = event.as_ref().map_or(true, |event| {
        interface_watch_event_requires_reconcile(event, roots)
    });
    if should_enqueue {
        let _ = tx.send(event);
    }
}

fn interface_watch_event_requires_reconcile(
    event: &notify::Event,
    roots: &InterfaceWatchRoots,
) -> bool {
    if event.need_rescan() {
        return true;
    }
    use notify::event::{AccessKind, AccessMode};
    if matches!(
        event.kind,
        notify::EventKind::Access(
            AccessKind::Read | AccessKind::Open(_) | AccessKind::Close(AccessMode::Read)
        )
    ) {
        return false;
    }
    event.paths.iter().any(|path| {
        if path.starts_with(&roots.interface_registry) || path.starts_with(&roots.run_records) {
            return true;
        }
        if path.parent() == Some(roots.host_directory.as_path()) {
            return true;
        }
        let Some(private_interfaces) = &roots.private_interfaces else {
            return false;
        };
        let Ok(relative) = path.strip_prefix(private_interfaces) else {
            return false;
        };
        let parts = relative.components().collect::<Vec<_>>();
        parts.len() == 2
            && parts[1].as_os_str() == "environment.json"
            && matches!(parts[0], std::path::Component::Normal(_))
    })
}

fn life_root_slots(
    registry_root: &Path,
    octopus_home: &Path,
    interface_life_install_root: &Path,
) -> Result<BTreeMap<String, PathBuf>, String> {
    let path = octopus_home.join("host.json");
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("host manifest '{}' cannot be read: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("host manifest '{}' is malformed: {error}", path.display()))?;
    let refs = value
        .get("root_refs")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("host manifest '{}' has no root_refs object", path.display()))?;
    let mut slots = BTreeMap::new();
    for (name, value) in refs {
        let raw = value.as_str().ok_or_else(|| {
            format!(
                "host manifest '{}' root_refs.{name} is not a string",
                path.display()
            )
        })?;
        let mut root = PathBuf::from(raw);
        if !root.is_absolute() {
            return Err(format!(
                "host manifest '{}' root_refs.{name} is not absolute: {raw}",
                path.display()
            ));
        }
        if root.exists() {
            root = std::fs::canonicalize(&root).map_err(|error| {
                format!(
                    "host manifest '{}' root_refs.{name} path '{}' cannot be canonicalized: {error}",
                    path.display(),
                    root.display()
                )
            })?;
        }
        slots.insert(name.clone(), root);
    }
    slots.insert(
        "registry_interface_root".to_string(),
        registry_root.join("interface"),
    );
    slots.insert(
        "registry_ee_template_root".to_string(),
        registry_root.join("ee_template"),
    );
    slots.insert(
        "registry_artifact_root".to_string(),
        registry_root.join("artifact"),
    );
    slots.insert(
        "artifact_blobs_root".to_string(),
        octopus_home.join("ledger/blobs"),
    );
    slots.insert("registry_mark_root".to_string(), registry_root.join("mark"));
    slots.insert(
        "registry_verifier_root".to_string(),
        registry_root.join("verifier"),
    );
    slots.insert("registry_tool_root".to_string(), registry_root.join("tool"));
    slots.insert(
        "registry_worker_environment_kind_root".to_string(),
        registry_root.join("worker_environment_kind"),
    );
    slots.insert(
        APP_SERVER_CONTROL_ROOT_SLOT.to_string(),
        octopus_home.join("app-server-control"),
    );
    slots.insert(
        "interface_hand_in_root".to_string(),
        octopus_home.join("interface-hand-in"),
    );
    let interface_life_install_root =
        std::fs::canonicalize(interface_life_install_root).map_err(|error| {
            format!(
                "interface life installation root '{}' cannot be canonicalized: {error}",
                interface_life_install_root.display()
            )
        })?;
    slots.insert(
        INTERFACE_LIFE_INSTALL_ROOT_SLOT.to_string(),
        interface_life_install_root,
    );
    Ok(slots)
}

fn merge_interface_root_slots(
    base: &BTreeMap<String, PathBuf>,
    descriptor: &InterfaceDescriptor,
    octopus_home: &Path,
) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut slots = base.clone();
    if !slots.contains_key(INTERFACE_LIFE_INSTALL_ROOT_SLOT) {
        return Err("host formed no interface life installation root".to_string());
    }
    slots.insert(
        "activation_secrets_home".to_string(),
        octopus_home.join("interfaces").join(descriptor.id.as_str()),
    );
    slots.insert(
        INTERFACE_LOCAL_IPC_ROOT_SLOT.to_string(),
        prepare_interface_local_ipc_root(octopus_home, &descriptor.id)?,
    );
    let Some(config) = descriptor.interface_driver_config.as_ref() else {
        return Ok(slots);
    };
    for (name, override_slot) in &config.host_slots {
        if name == "activation_secrets_home"
            || name == INTERFACE_LIFE_INSTALL_ROOT_SLOT
            || name == INTERFACE_LOCAL_IPC_ROOT_SLOT
        {
            return Err(format!(
                "interface '{}' cannot override host-formed root slot '{}'",
                descriptor.id, name
            ));
        }
        let path = &override_slot.path;
        if path.as_os_str().is_empty() {
            return Err(format!(
                "interface '{}' host_slots override '{}' is an empty path",
                descriptor.id, name
            ));
        }
        if !path.is_absolute() {
            return Err(format!(
                "interface '{}' host_slots override '{}' is not absolute: {}",
                descriptor.id,
                name,
                path.display()
            ));
        }
        slots.insert(name.clone(), path.clone());
    }
    Ok(slots)
}

fn prepare_interface_local_ipc_root(
    octopus_home: &Path,
    interface: &InterfaceId,
) -> Result<PathBuf, String> {
    let base = interface_local_ipc_base(octopus_home);
    ensure_owner_only_directory(&base, "interface IPC root")?;
    let digest = blake3::hash(interface.as_str().as_bytes())
        .to_hex()
        .to_string();
    let root = base.join(&digest[..16]);
    ensure_owner_only_directory(&root, "interface-local IPC root")?;
    let canonical_base = std::fs::canonicalize(&base).map_err(|error| {
        format!(
            "interface IPC root '{}' cannot be canonicalized: {error}",
            base.display()
        )
    })?;
    let canonical_root = std::fs::canonicalize(&root).map_err(|error| {
        format!(
            "interface-local IPC root '{}' cannot be canonicalized: {error}",
            root.display()
        )
    })?;
    if canonical_root.parent() != Some(canonical_base.as_path()) {
        return Err(format!(
            "interface-local IPC root '{}' escaped its host root",
            canonical_root.display()
        ));
    }
    Ok(canonical_root)
}

#[cfg(unix)]
fn interface_local_ipc_base(octopus_home: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;

    let home_digest = blake3::hash(octopus_home.as_os_str().as_bytes())
        .to_hex()
        .to_string();
    PathBuf::from("/tmp").join(format!(
        "octopus-ipc-{}-{}",
        unsafe { libc::geteuid() },
        &home_digest[..12]
    ))
}

#[cfg(not(unix))]
fn interface_local_ipc_base(octopus_home: &Path) -> PathBuf {
    octopus_home.join("ipc")
}

fn ensure_owner_only_directory(path: &Path, label: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(format!(
                "{label} '{}' is not a real directory",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(|error| {
                format!("{label} '{}' cannot be created: {error}", path.display())
            })?;
        }
        Err(error) => {
            return Err(format!(
                "{label} '{}' cannot be inspected: {error}",
                path.display()
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            format!(
                "{label} '{}' cannot be reinspected: {error}",
                path.display()
            )
        })?;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(format!(
                "{label} '{}' is not owned by the current user",
                path.display()
            ));
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                format!(
                    "{label} '{}' cannot be made owner-only: {error}",
                    path.display()
                )
            },
        )?;
    }
    Ok(())
}

fn interface_lift_fingerprint(
    life: ContentHash,
    private_environment: ContentHash,
    root_slots: &BTreeMap<String, PathBuf>,
) -> Result<ContentHash, String> {
    let bytes = serde_json::to_vec(&(life.to_hex(), private_environment.to_hex(), root_slots))
        .map_err(|error| format!("interface life fingerprint did not encode: {error}"))?;
    Ok(ContentHash::of_bytes(&bytes))
}

pub async fn prepare_interface_activation_reconciler(
    registry_root: PathBuf,
    interface_life_install_root: PathBuf,
    tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    emissions_tx: tokio::sync::mpsc::Sender<SystemInterfaceEmission>,
    cancel: CancellationToken,
    octopus_home: PathBuf,
    durable_root: Option<PathBuf>,
    report: Option<&mut dyn FnMut(InterfaceBootEvent)>,
) -> Result<InterfaceActivationReconcilerHandle, Box<dyn std::error::Error>> {
    for directory in [
        octopus_home.join("app-server-control"),
        octopus_home.join("interface-hand-in"),
    ] {
        std::fs::create_dir_all(&directory).map_err(|error| {
            format!(
                "interface runtime root '{}' cannot be prepared before activation: {error}",
                directory.display()
            )
        })?;
    }
    let mut reconciler = InterfaceActivationReconciler::new(
        registry_root,
        interface_life_install_root,
        tx,
        emissions_tx,
        cancel,
        octopus_home,
        durable_root,
    );
    let _initial_lift_count = match reconciler.reconcile_once().await {
        Ok(count) => count,
        Err(error) => {
            reconciler.stop_all_lives().await;
            return Err(error.into());
        }
    };
    let preflight_failures = reconciler
        .failures
        .borrow()
        .iter()
        .filter(|(interface, _)| {
            !reconciler.lifted.contains_key(*interface) && !reconciler.deferred_lifts.contains(*interface)
        })
        .map(|(interface, error)| (interface.clone(), error.clone()))
        .collect::<Vec<_>>();
    let total = reconciler.lifted.len() + reconciler.deferred_lifts.len() + preflight_failures.len();
    let initial_task_count = match report {
        Some(report) => {
            report(InterfaceBootEvent::Discovered { total });
            let mut completed = 0usize;
            for (interface, error) in preflight_failures {
                completed = completed.saturating_add(1);
                report(InterfaceBootEvent::Failed {
                    interface,
                    completed,
                    total,
                    error,
                });
            }
            reconciler
                .await_initial_readiness(total, completed, Some(report))
                .await?
        }
        None => reconciler.await_initial_readiness(total, 0, None).await?,
    };
    let initial_failures = reconciler.failures.borrow().clone();
    Ok(InterfaceActivationReconcilerHandle {
        reconciler,
        initial_task_count,
        initial_failures,
    })
}

#[cfg(test)]
mod interface_life_switch_tests {
    use super::*;

    fn watch_roots(root: &Path) -> InterfaceWatchRoots {
        InterfaceWatchRoots {
            interface_registry: root.join("registry/interface"),
            run_records: root.join("state/runs"),
            host_directory: root.to_path_buf(),
            private_interfaces: Some(root.join("interfaces")),
        }
    }

    fn watch_event(path: PathBuf) -> notify::Event {
        notify::Event::new(notify::EventKind::Any).add_path(path)
    }

    #[test]
    fn reconciliation_reads_do_not_wake_the_watcher_but_writes_and_rescans_do() {
        use notify::event::{AccessKind, AccessMode, Flag};
        let roots = watch_roots(Path::new("/octopus-home"));
        for (kind, expected) in [
            (AccessKind::Open(AccessMode::Read), false),
            (AccessKind::Open(AccessMode::Any), false),
            (AccessKind::Read, false),
            (AccessKind::Close(AccessMode::Read), false),
            (AccessKind::Close(AccessMode::Write), true),
            (AccessKind::Close(AccessMode::Any), true),
            (AccessKind::Any, true),
        ] {
            let event = notify::Event::new(notify::EventKind::Access(kind))
                .add_path(roots.interface_registry.join("world_probe/life.json"));
            assert_eq!(interface_watch_event_requires_reconcile(&event, &roots), expected);
        }
        let event = notify::Event::new(notify::EventKind::Other).set_flag(Flag::Rescan);
        assert!(interface_watch_event_requires_reconcile(&event, &roots));
    }

    #[test]
    fn private_interface_churn_never_enters_the_reconciliation_queue() {
        let root = Path::new("/octopus-home");
        let roots = watch_roots(root);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        enqueue_interface_watch_event(
            &tx,
            Ok(watch_event(root.join(
                "interfaces/world_provider_codex/ledger-cursor.json",
            ))),
            &roots,
        );
        enqueue_interface_watch_event(
            &tx,
            Ok(watch_event(
                root.join("interfaces/world_provider_codex/emitted/42.json"),
            )),
            &roots,
        );
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn descriptor_run_and_private_environment_changes_enter_the_reconciliation_queue() {
        let root = Path::new("/octopus-home");
        let roots = watch_roots(root);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let relevant = [
            root.join("registry/interface/world_provider_codex/life.json"),
            root.join("state/runs/run_1.json"),
            root.join("interfaces/world_provider_codex/environment.json"),
        ];

        for path in &relevant {
            enqueue_interface_watch_event(&tx, Ok(watch_event(path.clone())), &roots);
        }

        for path in relevant {
            assert_eq!(rx.try_recv().unwrap().unwrap().paths, vec![path]);
        }
    }

    #[test]
    fn repeated_interface_life_failure_uses_bounded_backoff() {
        assert_eq!(interface_life_restart_delay(1), Duration::from_millis(250));
        assert_eq!(interface_life_restart_delay(2), Duration::from_millis(500));
        assert_eq!(interface_life_restart_delay(32), Duration::from_secs(300));
    }

    #[test]
    fn interface_life_install_root_is_independent_from_worker_repo_root() {
        let dir = tempfile::tempdir().unwrap();
        let registry_root = dir.path().join("home/registry");
        let octopus_home = dir.path().join("home");
        let worker_repo_root = dir.path().join("worker-repository");
        let install_root = dir.path().join("octopus-installation");
        std::fs::create_dir_all(&registry_root).unwrap();
        std::fs::create_dir_all(&worker_repo_root).unwrap();
        std::fs::create_dir_all(&install_root).unwrap();
        std::fs::write(
            octopus_home.join("host.json"),
            serde_json::json!({
                "host_manifest_version": 1,
                "root_refs": {
                    "repo_root": worker_repo_root,
                    "run_records_root": octopus_home.join("state/runs")
                }
            })
            .to_string(),
        )
        .unwrap();

        let slots = life_root_slots(&registry_root, &octopus_home, &install_root).unwrap();
        assert_eq!(
            slots.get("repo_root"),
            Some(&std::fs::canonicalize(worker_repo_root).unwrap())
        );
        assert_eq!(
            slots.get(INTERFACE_LIFE_INSTALL_ROOT_SLOT),
            Some(&std::fs::canonicalize(install_root).unwrap())
        );
        assert_eq!(
            slots.get("registry_mark_root"),
            Some(&registry_root.join("mark"))
        );
        assert_eq!(
            slots.get("registry_verifier_root"),
            Some(&registry_root.join("verifier"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn interface_local_ipc_root_stays_private_and_below_unix_socket_path_limit() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let home = PathBuf::from(
            "/an/intentionally/very/long/octopus/home/path/that/must/not/expand/the/local/socket/address",
        );
        let interface =
            InterfaceId::new("system_exec_env_with_an_intentionally_long_exact_interface_identity");
        let root = prepare_interface_local_ipc_root(&home, &interface).unwrap();
        let socket = root.join("p-0123456789abcdef0123.sock");
        let address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
        assert!(socket.as_os_str().as_bytes().len() < address.sun_path.len());
        let metadata = std::fs::symlink_metadata(&root).unwrap();
        assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
        let base = root.parent().unwrap().to_path_buf();
        std::fs::remove_dir(root).unwrap();
        std::fs::remove_dir(base).unwrap();
    }

    #[tokio::test]
    async fn cancellation_interrupts_initial_readiness_without_reporting_life_failures() {
        let dir = tempfile::tempdir().unwrap();
        let (arrivals, _) = tokio::sync::mpsc::channel(1);
        let (emissions, _) = tokio::sync::mpsc::channel(1);
        let cancel = CancellationToken::new();
        let mut reconciler = InterfaceActivationReconciler::new(
            dir.path().join("registry"),
            dir.path().join("installation"),
            arrivals,
            emissions,
            cancel.clone(),
            dir.path().join("home"),
            None,
        );
        for (index, name) in ["life_a", "life_b"].into_iter().enumerate() {
            let interface = InterfaceId::new(name);
            let life_cancel = cancel.child_token();
            let life_wait = life_cancel.clone();
            let handle = tokio::spawn(async move {
                life_wait.cancelled().await;
            });
            reconciler.lifted.insert(
                interface,
                LiftedInterfaceLife {
                    lift_id: index as u64,
                    fingerprint: ContentHash::of_bytes(name.as_bytes()),
                    cancel: life_cancel,
                    handle,
                    started_at: Instant::now(),
                    restart_attempt: 0,
                    died_at: None,
                    retry_not_before: None,
                    waking: true,
                    ended: false,
                    sleeping: false,
                    woken: false,
                    asleep_through: None,
                    socket_roots: Vec::new(),
                    #[cfg(unix)]
                    held_sockets: None,
                },
            );
        }
        let cancel_after_wait_begins = cancel.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            cancel_after_wait_begins.cancel();
        });
        let mut events = Vec::new();
        let mut report = |event| events.push(event);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            reconciler.await_initial_readiness(2, 0, Some(&mut report)),
        )
        .await
        .expect("cancellation must not leave initial readiness blocked");
        assert_eq!(
            result.unwrap_err(),
            "interface lifecycle initialization cancelled"
        );
        assert!(events.is_empty());
        assert!(reconciler.lifted.is_empty());
    }

    #[tokio::test]
    async fn life_failed_before_initial_readiness_is_lifted_again_by_its_retry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let iface_dir = root.join("registry").join("interface").join("test_flaky");
        let durable_root = root.join("durable");
        std::fs::create_dir_all(&iface_dir).unwrap();
        std::fs::create_dir_all(durable_root.join("blobs")).unwrap();
        let var_dir = root.join("home").join("interfaces").join("test_flaky");
        std::fs::create_dir_all(&var_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&var_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(iface_dir.join("interface.json"), serde_json::json!({
            "id": "test_flaky", "interface_type": "system", "role": "system",
            "direction": "bidirectional", "source_names": ["test_flaky"], "interface_driver_ref": "test_flaky"
        }).to_string()).unwrap();
        std::fs::write(iface_dir.join("activation.json"), serde_json::json!({
            "schema_version": 1, "interface": "test_flaky",
            "activations": [{"activation_driver_ref": "test_flaky", "enabled": true, "config": {}}]
        }).to_string()).unwrap();
        let marker = var_dir.join("failed-once");
        let payload = var_dir.join("payload.json");
        std::fs::write(&payload, br#"{"text":"lifted again"}"#).unwrap();
        let manifest = var_dir.join("emission.manifest.json");
        std::fs::write(&manifest, serde_json::json!({"label": "interface.output", "payload_path": payload}).to_string()).unwrap();
        let ready = serde_json::json!({"kind": "ready"}).to_string();
        let arrival = serde_json::json!({
            "kind": "arrival", "request_id": null, "artifact_manifest_path": manifest, "evidence": []
        }).to_string();
        std::fs::write(iface_dir.join("life.json"), serde_json::json!({
            "processes": [{
                "bin": "/bin/sh",
                "io_limits": {"max_control_frame_bytes": 65536, "max_stderr_bytes": 65536,
                    "max_manifest_bytes": 65536, "max_payload_bytes": 1048576},
                "args": ["-c", format!(
                    "if [ -e '{0}' ]; then printf '%s\\n' \"$1\" \"$2\"; IFS= read -r ignored; else : > '{0}'; exit 3; fi",
                    marker.display()), "octopus-test", ready, arrival],
                "authority": {
                    "read_root_slots": [], "write_root_slots": [], "executable_root_slots": [],
                    "read_paths": [], "write_paths": [marker], "executable_paths": ["/bin/sh"],
                    "executable_commands": [], "read_env_paths": [], "write_env_paths": [],
                    "executable_env_paths": [], "allow_network": false, "allow_local_ipc": false
                },
                "read_handles": []
            }]
        }).to_string()).unwrap();
        let runs_dir = durable_root.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::write(root.join("home").join("host.json"), serde_json::json!({
            "host_manifest_version": 1,
            "root_refs": {"repo_root": root.to_str().unwrap(), "run_records_root": runs_dir.to_str().unwrap()}
        }).to_string()).unwrap();
        std::fs::write(runs_dir.join("run_flaky.json"), serde_json::json!({
            "id": "run_flaky", "ee": "ee_test", "worker": "system_worker_test",
            "trace": "trace:run_flaky", "status": "idle",
            "checkpoints": [{
                "id": "c1", "run": "run_flaky", "trace": "trace:run_flaky:c1", "epoch": 0,
                "interface_binding": octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new("test_flaky")),
                "outbound_interface_bindings": []
            }],
            "active_checkpoints": ["c1"]
        }).to_string()).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<InterfaceActivationArrival>(4);
        let cancel = CancellationToken::new();
        let handle = prepare_interface_activation_reconciler(
            root.join("registry"), root.to_path_buf(), tx, tokio::sync::mpsc::channel(4).0,
            cancel.clone(), root.join("home"), Some(durable_root), None,
        ).await.unwrap();
        assert!(handle.initial_failures.contains_key(&InterfaceId::new("test_flaky")));
        assert!(marker.exists());
        let reconciler_task = tokio::spawn(handle.reconciler.run());
        let arrival = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a life that failed before initial readiness must be lifted again by its retry")
            .expect("intake open");
        assert_eq!(arrival.interface.as_str(), "test_flaky");
        assert_eq!(arrival.artifact.read_payload_bytes().unwrap(), br#"{"text":"lifted again"}"#);
        cancel.cancel();
        reconciler_task.await.unwrap();
    }

    struct ShLives {
        _dir: tempfile::TempDir,
        root: PathBuf,
        home: PathBuf,
        durable: PathBuf,
        cancel: CancellationToken,
    }

    impl ShLives {
        fn new() -> Self {
            let dir = tempfile::Builder::new().prefix("oct").tempdir_in("/tmp").unwrap();
            let root = dir.path().canonicalize().unwrap();
            let home = root.join("h");
            let durable = root.join("d");
            std::fs::create_dir_all(durable.join("blobs")).unwrap();
            std::fs::create_dir_all(durable.join("runs")).unwrap();
            std::fs::create_dir_all(home.join("app-server-control")).unwrap();
            std::fs::write(home.join("host.json"), serde_json::json!({
                "host_manifest_version": 1,
                "root_refs": {"repo_root": root.to_str().unwrap(), "run_records_root": durable.join("runs").to_str().unwrap()}
            }).to_string()).unwrap();
            Self { _dir: dir, root, home, durable, cancel: CancellationToken::new() }
        }

        fn var_dir(&self, name: &str) -> PathBuf {
            self.home.join("interfaces").join(name)
        }

        fn life(&self, name: &str, script: &str, frames: &[serde_json::Value], write_paths: &[&Path]) {
            let iface_dir = self.root.join("registry").join("interface").join(name);
            std::fs::create_dir_all(&iface_dir).unwrap();
            let var_dir = self.var_dir(name);
            std::fs::create_dir_all(&var_dir).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&var_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            std::fs::write(iface_dir.join("interface.json"), serde_json::json!({
                "id": name, "interface_type": "system", "role": "system",
                "direction": "bidirectional", "source_names": [name], "interface_driver_ref": name
            }).to_string()).unwrap();
            let mut args = vec![
                serde_json::json!("-c"),
                serde_json::json!(script),
                serde_json::json!("octopus-test"),
            ];
            args.extend(frames.iter().map(|frame| serde_json::json!(frame.to_string())));
            std::fs::write(iface_dir.join("life.json"), serde_json::json!({
                "processes": [{
                    "bin": "/bin/sh",
                    "io_limits": {"max_control_frame_bytes": 65536, "max_stderr_bytes": 65536,
                        "max_manifest_bytes": 65536, "max_payload_bytes": 1048576},
                    "args": args,
                    "authority": {
                        "read_root_slots": [], "write_root_slots": [], "executable_root_slots": [],
                        "read_paths": [], "write_paths": write_paths, "executable_paths": ["/bin/sh"],
                        "executable_commands": [], "read_env_paths": [], "write_env_paths": [],
                        "executable_env_paths": [], "allow_network": false, "allow_local_ipc": false
                    },
                    "read_handles": []
                }]
            }).to_string()).unwrap();
        }

        fn bind(&self, run: &str, names: &[&str]) {
            let checkpoints = names.iter().map(|name| serde_json::json!({
                "id": format!("c_{name}"), "run": run, "trace": format!("trace:{run}:c_{name}"), "epoch": 0,
                "interface_binding": octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new(*name)),
                "outbound_interface_bindings": []
            })).collect::<Vec<_>>();
            let active = names.iter().map(|name| format!("c_{name}")).collect::<Vec<_>>();
            std::fs::write(self.durable.join("runs").join(format!("{run}.json")), serde_json::json!({
                "id": run, "ee": "ee_test", "worker": "system_worker_test",
                "trace": format!("trace:{run}"), "status": "idle",
                "checkpoints": checkpoints, "active_checkpoints": active
            }).to_string()).unwrap();
        }

        fn reconciler(
            &self,
            arrivals: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
        ) -> InterfaceActivationReconciler {
            InterfaceActivationReconciler::new(
                self.root.join("registry"),
                self.root.clone(),
                arrivals,
                tokio::sync::mpsc::channel(4).0,
                self.cancel.clone(),
                self.home.clone(),
                Some(self.durable.clone()),
            )
        }

        async fn stop(&self, mut reconciler: InterfaceActivationReconciler) {
            self.cancel.cancel();
            reconciler.stop_all_lives().await;
        }
    }

    fn ready_frame() -> serde_json::Value {
        serde_json::json!({"kind": "ready"})
    }

    async fn pump_life_events(
        reconciler: &mut InterfaceActivationReconciler,
        expectation: &str,
        done: impl Fn(&InterfaceActivationReconciler) -> bool,
    ) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !done(reconciler) {
                if let Ok(Some(event)) =
                    tokio::time::timeout(Duration::from_millis(20), reconciler.life_exit_rx.recv()).await
                {
                    reconciler.handle_life_event(event).await;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{expectation}"));
    }

    #[tokio::test]
    async fn lives_wake_in_stages_bounded_by_the_waking_limit() {
        let lives = ShLives::new();
        let names = ["test_stage_a", "test_stage_b", "test_stage_c"];
        for name in names {
            lives.life(name, "printf '%s\\n' \"$1\"; IFS= read -r ignored", &[ready_frame()], &[]);
        }
        lives.bind("run_stages", &names);
        let mut reconciler = lives.reconciler(tokio::sync::mpsc::channel(4).0);
        reconciler.waking_limit = 1;
        reconciler.reconcile_once().await.unwrap();
        assert_eq!(reconciler.lifted.len(), 1, "only the waking limit is lifted at once");
        assert_eq!(reconciler.deferred_lifts.len(), 2, "the other lives wait for their turn");
        let mut events = Vec::new();
        let mut report = |event| events.push(event);
        let ready = tokio::time::timeout(
            Duration::from_secs(10),
            reconciler.await_initial_readiness(3, 0, Some(&mut report)),
        )
        .await
        .expect("deferred lives are lifted as their waking siblings become ready")
        .unwrap();
        assert_eq!(ready, 3);
        assert_eq!(reconciler.lifted.len(), 3);
        assert!(reconciler.deferred_lifts.is_empty());
        assert!(events.iter().all(|event| matches!(event, InterfaceBootEvent::Ready { .. })));
        lives.stop(reconciler).await;
    }

    #[tokio::test]
    async fn a_life_that_falls_asleep_is_woken_only_by_a_movement_it_has_not_read() {
        let lives = ShLives::new();
        let name = "test_sleeper";
        let lifts = lives.var_dir(name).join("lifts");
        lives.life(
            name,
            &format!("echo lift >> '{}'; printf '%s\\n' \"$1\" \"$2\"", lifts.display()),
            &[ready_frame(), serde_json::json!({"kind": "asleep", "through_seq": 5})],
            &[&lifts],
        );
        lives.bind("run_sleeper", &[name]);
        let mut reconciler = lives.reconciler(tokio::sync::mpsc::channel(4).0);
        let interface = InterfaceId::new(name);
        reconciler.reconcile_once().await.unwrap();
        assert_eq!(reconciler.await_initial_readiness(1, 0, None).await.unwrap(), 1);
        let asleep = |reconciler: &InterfaceActivationReconciler| {
            reconciler.lifted.get(&interface).and_then(|life| life.asleep_through)
        };
        pump_life_events(
            &mut reconciler,
            "a life that declares sleep and exits cleanly is asleep",
            |reconciler| asleep(reconciler).is_some(),
        )
        .await;
        assert_eq!(asleep(&reconciler), Some(5));
        assert!(reconciler.failures.borrow().is_empty(), "sleep is neither coma nor failure");
        reconciler.reconcile_once().await.unwrap();
        reconciler.movement_reached(interface.clone(), 4).await;
        assert_eq!(asleep(&reconciler), Some(5), "a movement the life already read does not wake it");
        assert_eq!(std::fs::read_to_string(&lifts).unwrap().lines().count(), 1);
        reconciler.movement_reached(interface.clone(), 5).await;
        assert_eq!(asleep(&reconciler), None);
        pump_life_events(
            &mut reconciler,
            "a movement past the sleep boundary lifts the life again",
            |_| std::fs::read_to_string(&lifts).unwrap().lines().count() >= 2,
        )
        .await;
        lives.stop(reconciler).await;
    }

    #[tokio::test]
    async fn a_movement_brings_a_comatose_life_back_sooner_than_its_backoff() {
        let lives = ShLives::new();
        let name = "test_coma";
        let lifts = lives.var_dir(name).join("lifts");
        lives.life(name, &format!("echo lift >> '{}'; exit 3", lifts.display()), &[], &[&lifts]);
        lives.bind("run_coma", &[name]);
        let mut reconciler = lives.reconciler(tokio::sync::mpsc::channel(4).0);
        let interface = InterfaceId::new(name);
        let lifted = || std::fs::read_to_string(&lifts).map_or(0, |text| text.lines().count());
        reconciler.reconcile_once().await.unwrap();
        pump_life_events(&mut reconciler, "a life that dies is in coma with a retry ahead", |reconciler| {
            reconciler.lifted.get(&interface).is_some_and(|life| life.retry_not_before.is_some())
        })
        .await;
        let life = reconciler.lifted.get_mut(&interface).unwrap();
        life.died_at = Some(Instant::now() - LIFE_RESTART_MAX_DELAY_WITH_WORK);
        life.retry_not_before = Some(Instant::now() + LIFE_RESTART_MAX_DELAY);
        let quiet_until = Instant::now() + Duration::from_millis(400);
        pump_life_events(&mut reconciler, "the backoff holds", |_| Instant::now() >= quiet_until).await;
        assert_eq!(lifted(), 1, "nothing lifts a comatose life before its backoff ends");
        reconciler.movement_reached(interface.clone(), 1).await;
        pump_life_events(&mut reconciler, "a movement to a comatose life lifts it once thirty seconds have passed since its death", |_| {
            lifted() >= 2
        })
        .await;
        lives.stop(reconciler).await;
    }

    #[tokio::test]
    async fn a_life_whose_exit_is_seen_before_its_asleep_frame_is_still_asleep() {
        let lives = ShLives::new();
        let name = "test_last_word";
        let gone = lives.var_dir(name).join("gone");
        let manifest = lives.var_dir(name).join("emission.manifest.json");
        let payload = lives.var_dir(name).join("payload.json");
        lives.life(
            name,
            &format!("printf '%s\\n' \"$1\" \"$2\" \"$2\" \"$3\"; : > '{}'", gone.display()),
            &[
                ready_frame(),
                serde_json::json!({"kind": "arrival", "request_id": null, "artifact_manifest_path": manifest, "evidence": []}),
                serde_json::json!({"kind": "asleep", "through_seq": 5}),
            ],
            &[&gone],
        );
        std::fs::write(&payload, br#"{"text":"said before sleep"}"#).unwrap();
        std::fs::write(&manifest, serde_json::json!({"label": "interface.output", "payload_path": payload}).to_string()).unwrap();
        lives.bind("run_last_word", &[name]);
        let (arrivals, mut arrived) = tokio::sync::mpsc::channel(1);
        let mut reconciler = lives.reconciler(arrivals);
        let interface = InterfaceId::new(name);
        reconciler.reconcile_once().await.unwrap();
        assert_eq!(reconciler.await_initial_readiness(1, 0, None).await.unwrap(), 1);
        for _ in 0..8 {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !gone.exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("the life prints its frames and exits while the host still hands in its first arrival");
            tokio::time::sleep(Duration::from_millis(30)).await;
            for _ in 0..2 {
                arrived.recv().await.expect("both arrivals the life printed are handed in");
            }
            pump_life_events(&mut reconciler, "the life's exit is settled", |reconciler| {
                reconciler.lifted.get(&interface).is_some_and(|life| life.sleeping)
                    || !reconciler.failures.borrow().is_empty()
            })
            .await;
            assert_eq!(
                reconciler.failures.borrow().get(&interface),
                None,
                "an exit seen before the asleep frame was read is still sleep"
            );
            std::fs::remove_file(&gone).unwrap();
            reconciler.wake_life(&interface).await;
        }
        lives.stop(reconciler).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_sleeping_life_socket_is_held_and_a_connection_lifts_it_with_the_listener() {
        let lives = ShLives::new();
        let name = "test_knock";
        let socket = lives.home.join("app-server-control").join("k.sock");
        let marker = lives.var_dir(name).join("slept");
        let inherited = lives.var_dir(name).join("inherited");
        lives.life(
            name,
            &format!(
                "if [ -e '{marker}' ]; then printf '%s' \"$OCTOPUS_INHERITED_LISTENERS\" > '{inherited}'; printf '%s\\n' \"$1\"; IFS= read -r ignored; else : > '{marker}'; printf '%s\\n' \"$1\" \"$2\"; fi",
                marker = marker.display(),
                inherited = inherited.display(),
            ),
            &[ready_frame(), serde_json::json!({"kind": "asleep", "sockets": [socket]})],
            &[&marker, &inherited],
        );
        lives.bind("run_knock", &[name]);
        let mut reconciler = lives.reconciler(tokio::sync::mpsc::channel(4).0);
        let interface = InterfaceId::new(name);
        reconciler.reconcile_once().await.unwrap();
        assert_eq!(reconciler.await_initial_readiness(1, 0, None).await.unwrap(), 1);
        pump_life_events(&mut reconciler, "a life that names its socket falls asleep", |reconciler| {
            reconciler.lifted.get(&interface).is_some_and(|life| life.sleeping)
        })
        .await;
        assert!(reconciler.failures.borrow().is_empty());
        let _client = std::os::unix::net::UnixStream::connect(&socket)
            .expect("the runtime holds a sleeping life's socket");
        pump_life_events(&mut reconciler, "a connection to the held socket lifts the life", |_| {
            std::fs::read_to_string(&inherited).is_ok_and(|text| !text.is_empty())
        })
        .await;
        let listeners: BTreeMap<PathBuf, i32> =
            serde_json::from_str(&std::fs::read_to_string(&inherited).unwrap()).unwrap();
        assert_eq!(listeners.keys().collect::<Vec<_>>(), vec![&socket], "the woken life inherits the listener");
        lives.stop(reconciler).await;
    }

    #[tokio::test]
    async fn activation_life_requires_live_bindings_and_emits_through_the_intake() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let iface_dir = root.join("registry").join("interface").join("test_shell");
        let durable_root = root.join("durable");
        std::fs::create_dir_all(&iface_dir).unwrap();
        std::fs::create_dir_all(durable_root.join("blobs")).unwrap();
        let interface_var_dir = root.join("home").join("interfaces").join("test_shell");
        std::fs::create_dir_all(&interface_var_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&interface_var_dir, std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }

        std::fs::write(
            iface_dir.join("interface.json"),
            serde_json::json!({
                "id": "test_shell",
                "interface_type": "system",
                "role": "system",
                "direction": "bidirectional",
                "source_names": ["test_shell"],
                "interface_driver_ref": "test_shell"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            iface_dir.join("activation.json"),
            serde_json::json!({
                "schema_version": 1,
                "interface": "test_shell",
                "activations": [{
                    "activation_driver_ref": "test_shell",
                    "config": {}
                }]
            })
            .to_string(),
        )
        .unwrap();
        let broken_dir = root.join("registry").join("interface").join("test_broken");
        std::fs::create_dir_all(&broken_dir).unwrap();
        std::fs::write(
            broken_dir.join("interface.json"),
            serde_json::json!({
                "id": "test_broken",
                "interface_type": "system",
                "role": "system",
                "direction": "bidirectional",
                "source_names": ["test_broken"],
                "interface_driver_ref": "test_broken"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            broken_dir.join("activation.json"),
            serde_json::json!({
                "schema_version": 1,
                "interface": "test_broken",
                "activations": [{
                    "activation_driver_ref": "test_broken",
                    "enabled": true,
                    "config": {}
                }]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            broken_dir.join("life.json"),
            serde_json::json!({
                "processes": [{
                    "bin": "/bin/sh",
                    "args": ["-c", "exit 9"],
                    "authority": {
                        "read_root_slots": [],
                        "write_root_slots": [],
                        "executable_root_slots": [],
                        "read_paths": [],
                        "write_paths": [],
                        "executable_paths": ["/bin/sh"],
                        "executable_commands": [],
                        "read_env_paths": [],
                        "write_env_paths": [],
                        "executable_env_paths": [],
                        "allow_network": false,
                        "allow_local_ipc": false
                    }
                }]
            })
            .to_string(),
        )
        .unwrap();
        let runs_dir = durable_root.join("runs");
        std::fs::create_dir_all(&runs_dir).unwrap();
        std::fs::write(
            root.join("home").join("host.json"),
            serde_json::json!({
                "host_manifest_version": 1,
                "root_refs": {
                    "repo_root": root.to_str().unwrap(),
                    "run_records_root": runs_dir.to_str().unwrap()
                }
            })
            .to_string(),
        )
        .unwrap();
        let payload = interface_var_dir.join("payload.json");
        std::fs::write(&payload, br#"{"text":"from-shell"}"#).unwrap();
        let manifest = interface_var_dir.join("emission.manifest.json");
        std::fs::write(
            &manifest,
            serde_json::json!({"label": "interface.output", "payload_path": payload}).to_string(),
        )
        .unwrap();
        let verdict_path = interface_var_dir.join("system-emission-verdict.json");
        let ready_frame = serde_json::json!({"kind": "ready"}).to_string();
        let arrival_frame = serde_json::json!({
            "kind": "arrival",
            "request_id": null,
            "artifact_manifest_path": manifest,
            "evidence": []
        })
        .to_string();
        let system_emission_frame = serde_json::json!({
            "kind": "system_emission",
            "request_id": 7,
            "artifact": {
                "carrier": "materialized",
                "artifact_manifest_path": manifest
            },
            "source_movement_seq": 0,
            "to_interface": "system_worker_planner"
        })
        .to_string();

        std::fs::write(
            iface_dir.join("life.json"),
            serde_json::json!({
                "processes": [{
                    "bin": "/bin/sh",
                    "io_limits": {
                        "max_control_frame_bytes": 65536,
                        "max_stderr_bytes": 65536,
                        "max_manifest_bytes": 65536,
                        "max_payload_bytes": 1048576
                    },
                    "args": ["-c",
                        format!(
                            "printf '%s\\n' \"$1\" \"$2\" \"$3\"; IFS= read -r verdict; printf '%s' \"$verdict\" > '{}'",
                            verdict_path.display()
                        ),
                        "octopus-test",
                        ready_frame,
                        arrival_frame,
                        system_emission_frame
                    ],
                    "authority": {
                        "read_root_slots": [],
                        "write_root_slots": [],
                        "executable_root_slots": [],
                        "read_paths": [],
                        "write_paths": [verdict_path],
                        "executable_paths": ["/bin/sh"],
                        "executable_commands": [],
                        "read_env_paths": [],
                        "write_env_paths": [],
                        "executable_env_paths": [],
                        "allow_network": false,
                        "allow_local_ipc": false
                    },
                    "read_handles": []
                }]
            })
            .to_string(),
        )
        .unwrap();

        let mut probe = InterfaceActivationReconciler::new(
            root.join("registry"),
            root.to_path_buf(),
            tokio::sync::mpsc::channel(4).0,
            tokio::sync::mpsc::channel(4).0,
            CancellationToken::new(),
            root.join("home"),
            Some(durable_root.clone()),
        );
        assert_eq!(probe.reconcile_once().await.unwrap(), 0);
        assert!(probe.failures.borrow().is_empty());
        let run_path = runs_dir.join("run_shell.json");
        let run = serde_json::json!({
            "id": "run_shell", "ee": "ee_test", "worker": "system_worker_test",
            "trace": "trace:run_shell", "status": "idle",
            "checkpoints": [{
                "id": "c1", "run": "run_shell", "trace": "trace:run_shell:c1", "epoch": 0,
                "interface_binding": octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new("test_shell")),
                "outbound_interface_bindings": [octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new("test_broken"))]
            }],
            "active_checkpoints": ["c1"]
        });
        std::fs::write(&run_path, run.to_string()).unwrap();
        assert_eq!(probe.reconcile_once().await.unwrap(), 1);
        let activation_path = iface_dir.join("activation.json");
        let activation = std::fs::read(&activation_path).unwrap();
        std::fs::write(&activation_path, b"{}").unwrap();
        assert_eq!(probe.reconcile_once().await.unwrap(), 0);
        assert!(probe.failures.borrow()[&InterfaceId::new("test_shell")].contains("activation is invalid"));
        std::fs::write(&activation_path, activation).unwrap();
        assert_eq!(probe.reconcile_once().await.unwrap(), 1);
        let mut settled = run.clone();
        settled["active_checkpoints"] = serde_json::json!([]);
        std::fs::write(&run_path, settled.to_string()).unwrap();
        assert_eq!(probe.reconcile_once().await.unwrap(), 0);
        assert!(probe.failures.borrow().is_empty());
        std::fs::write(&run_path, run.to_string()).unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<InterfaceActivationArrival>(4);
        let cancel = CancellationToken::new();
        let (emissions_tx, mut emissions_rx) = tokio::sync::mpsc::channel(4);
        let mut boot_events = Vec::new();
        let mut report_boot_event = |event| boot_events.push(event);
        let handle = prepare_interface_activation_reconciler(
            root.join("registry"),
            root.to_path_buf(),
            tx,
            emissions_tx,
            cancel.clone(),
            root.join("home"),
            Some(durable_root),
            Some(&mut report_boot_event),
        )
        .await
        .unwrap();
        drop(report_boot_event);
        assert_eq!(handle.initial_task_count, 1);
        assert!(
            handle
                .initial_failures
                .contains_key(&InterfaceId::new("test_broken"))
        );
        assert_eq!(
            boot_events.first(),
            Some(&InterfaceBootEvent::Discovered { total: 2 })
        );
        assert_eq!(boot_events.len(), 3);
        assert_eq!(
            boot_events
                .iter()
                .skip(1)
                .map(|event| match event {
                    InterfaceBootEvent::Ready {
                        completed, total, ..
                    }
                    | InterfaceBootEvent::Failed {
                        completed, total, ..
                    } => (*completed, *total),
                    InterfaceBootEvent::Discovered { .. } =>
                        panic!("discovery is emitted once before readiness"),
                })
                .collect::<Vec<_>>(),
            vec![(1, 2), (2, 2)]
        );
        assert!(boot_events.iter().any(|event| matches!(
            event,
            InterfaceBootEvent::Ready { interface, total: 2, .. }
                if interface == &InterfaceId::new("test_shell")
        )));
        assert!(boot_events.iter().any(|event| matches!(
            event,
            InterfaceBootEvent::Failed { interface, total: 2, error, .. }
                if interface == &InterfaceId::new("test_broken")
                    && handle.initial_failures.get(interface) == Some(error)
        )));
        let mut observed_failures = handle.reconciler.observe_failures();
        let reconciler_task = tokio::spawn(handle.reconciler.run());

        let arrival = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("the spawned life must emit within the timeout")
            .expect("intake open");
        assert_eq!(arrival.interface.as_str(), "test_shell");
        assert_eq!(
            arrival.artifact.read_payload_bytes().unwrap(),
            br#"{"text":"from-shell"}"#
        );
        let emission = tokio::time::timeout(std::time::Duration::from_secs(5), emissions_rx.recv())
            .await
            .expect("the spawned life must hand its system emission through the host")
            .expect("system emission intake open");
        let (artifact, from, source_movement_seq, to, verdict_tx) = emission.into_parts();
        assert_eq!(from.as_str(), "test_shell");
        assert_eq!(source_movement_seq, 0);
        assert_eq!(to.as_str(), "system_worker_planner");
        let SystemInterfaceEmissionArtifact::Materialized(artifact) = artifact else {
            panic!("expected materialized system emission")
        };
        assert_eq!(
            artifact.read_payload_bytes().unwrap(),
            br#"{"text":"from-shell"}"#
        );
        verdict_tx
            .send(DeliveryVerdict::Admitted { seq: 73 })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), observed_failures.wait_for(|failures| {
            failures.contains_key(&InterfaceId::new("test_shell"))
        })).await.expect("a life that exits after readiness must be observable").unwrap();
        let mut verdict = None;
        for _ in 0..100 {
            if let Ok(value) = std::fs::read_to_string(&verdict_path) {
                verdict = Some(value);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let verdict =
            verdict.expect("spawned life did not receive its correlated delivery verdict");
        assert!(verdict.contains("\"request_id\":7"));
        assert!(verdict.contains("\"seq\":73"));
        let repeated = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("the completed life must lift again from its own completion event")
            .expect("intake open");
        assert_eq!(
            repeated.artifact.read_payload_bytes().unwrap(),
            br#"{"text":"from-shell"}"#
        );
        tokio::time::timeout(Duration::from_secs(5), observed_failures.wait_for(|failures| {
            !failures.contains_key(&InterfaceId::new("test_shell"))
        })).await.expect("successful readiness clears the observed life failure").unwrap();
        cancel.cancel();
        reconciler_task.await.unwrap();
    }
}
