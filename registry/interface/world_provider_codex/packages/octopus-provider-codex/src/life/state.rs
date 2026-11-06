use super::*;

#[derive(Clone)]
pub(crate) struct QueuedTurn {
    pub(crate) envelope: InterfaceInvocationEnvelope,
    pub(crate) seq: u64,
}

pub(crate) struct CodexRuntimeAuth {
    pub(crate) manager: Arc<AuthManager>,
    pub(crate) provider_home: PathBuf,
    pub(crate) auth_store_home: PathBuf,
    pub(crate) activation_secrets_home: PathBuf,
    pub(crate) connection_mode: ProviderConnectionMode,
}

pub(crate) struct CodexTurnScope<'a> {
    pub(crate) ctx: &'a InterfaceLiftContext,
    pub(crate) descriptor: &'a InterfaceDescriptor,
    pub(crate) handoff_session: &'a ProviderHandoffSession,
    pub(crate) runtime_auth: &'a CodexRuntimeAuth,
    pub(crate) run: &'a RunId,
    pub(crate) control: &'a Arc<CodexTurnControlBroker>,
    pub(crate) interrupted_effects: &'a InterruptedEffectArbiter,
}

#[derive(Default)]
pub(crate) struct ForwardedProviderEmissions {
    pub(crate) outputs: usize,
    pub(crate) dispatches: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InterruptedResultAdmission {
    Admitted(usize),
}

pub(crate) type PendingProviderEmission = (
    ProviderEmission,
    Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    Vec<octopus_run_authoring::ProviderOutgoingBinding>,
);

pub(crate) type ActiveTurnControls = Arc<tokio::sync::Mutex<BTreeMap<String, Arc<CodexTurnControlBroker>>>>;
pub(crate) type ControlSourceGates =
    Arc<tokio::sync::Mutex<BTreeMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>>;

#[derive(Clone, Copy)]
pub(crate) struct MainReaderProgress {
    pub(crate) next_seq: u64,
    pub(crate) revision: u64,
}

pub(crate) struct ControlReaderFence {
    pub(crate) run: RunId,
    pub(crate) acknowledged: Option<tokio::sync::oneshot::Sender<u64>>,
}

pub(crate) struct PendingControlReaderFence {
    pub(crate) fence: ControlReaderFence,
    pub(crate) watermark: Option<u64>,
}

pub(crate) struct PendingControlReaderSource {
    pub(crate) envelope: InterfaceInvocationEnvelope,
    pub(crate) label: String,
    pub(crate) run: RunId,
}

pub(crate) struct ControlSource<'a> {
    pub(crate) envelope: &'a InterfaceInvocationEnvelope,
    pub(crate) source_movement_seq: u64,
    pub(crate) broker: &'a Arc<CodexTurnControlBroker>,
    pub(crate) control_gates: &'a ControlSourceGates,
    pub(crate) control: CodexTurnControl,
    pub(crate) terminal_control_fenced: bool,
    pub(crate) independent_reader: bool,
}

pub(crate) struct MalformedControlSource<'a> {
    pub(crate) envelope: &'a InterfaceInvocationEnvelope,
    pub(crate) source_movement_seq: u64,
    pub(crate) broker: Option<&'a Arc<CodexTurnControlBroker>>,
    pub(crate) control_gates: &'a ControlSourceGates,
    pub(crate) error: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlSourceDisposition {
    Handled,
    DeferredTurn,
}

#[derive(Clone)]
pub(crate) struct DeferredProviderTurn {
    pub(crate) run: RunId,
    pub(crate) turn: QueuedTurn,
    pub(crate) release: Option<DeferredProviderTurnRelease>,
}

pub(crate) struct ControlResultDelivery<'a> {
    pub(crate) envelope: &'a InterfaceInvocationEnvelope,
    pub(crate) source_movement_seq: u64,
    pub(crate) output: Option<Arc<ProviderOutputStream>>,
    pub(crate) handoff_barrier: Option<Arc<ProviderHandoffBarrier>>,
    pub(crate) effect_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    pub(crate) result: &'a mut CodexTurnControlResult,
    pub(crate) recovery: bool,
    pub(crate) defer_message_receipt: bool,
}

#[derive(Clone)]
pub(crate) struct InterruptedEffectArbiter {
    pub(crate) ctx: InterfaceLiftContext,
    pub(crate) cache: Arc<tokio::sync::Mutex<InterruptedEffectCache>>,
}

#[derive(Default)]
pub(crate) struct InterruptedEffectCache {
    pub(crate) journals: BTreeMap<String, InterruptedEffectJournal>,
    pub(crate) loaded_runs: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InterruptedEffectJournal {
    pub(crate) schema_version: u32,
    pub(crate) run_id: String,
    pub(crate) control_source_movement_seq: u64,
    pub(crate) control_id: String,
    pub(crate) acknowledgement: DurableArtifactAdmission,
    pub(crate) continuity: crate::handler::CodexContinuityState,
    pub(crate) phase: InterruptedEffectPhase,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DurableArtifactAdmission {
    pub(crate) artifact: ArtifactRef,
    pub(crate) movement_seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactAdmissionScan {
    pub(crate) movement_seq: Option<u64>,
    pub(crate) complete_boundary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum InterruptedEffectPhase {
    AwaitingResult,
    Resolving {
        result_source_movement_seq: u64,
        source_artifact: ArtifactRef,
    },
    Closing {
        result_source_movement_seq: u64,
        closure_artifact: ArtifactRef,
    },
    Reconciled {
        result_source_movement_seq: u64,
        closure: DurableArtifactAdmission,
    },
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnControlJournal {
    pub(crate) schema_version: u32,
    pub(crate) source_movement_seq: u64,
    pub(crate) run_id: String,
    pub(crate) control_id: String,
    pub(crate) action: String,
    pub(crate) phase: TurnControlJournalPhase,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TurnControlJournalPhase {
    Intent,
    RetargetIntent {
        expected_turn_id: String,
        actual_turn_id: String,
    },
    DeferredTurn {
        result: CodexTurnControlResult,
        cause: CodexDeferredTurnCause,
    },
    Result {
        result: CodexTurnControlResult,
    },
    Delivered {
        result: CodexTurnControlResult,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeferredProviderTurnJournal {
    pub(crate) schema_version: u32,
    pub(crate) source_movement_seq: u64,
    pub(crate) run_id: String,
    pub(crate) artifact: ArtifactRef,
    pub(crate) metadata_content_hash: String,
    pub(crate) payload_content_hash: String,
    pub(crate) payload_byte_count: u64,
    pub(crate) artifact_bindings: Vec<DeferredArtifactBindingRef>,
    pub(crate) release: Option<DeferredProviderTurnRelease>,
}


#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum DeferredProviderTurnRelease {
    SessionRebase {
        source_session_carrier: Option<ArtifactRef>,
        rebased_session_carrier: DurableArtifactAdmission,
    },
    OriginalEnvelope {
        control_id: String,
        action: String,
        observed_ledger_next_seq: u64,
    },
    SettledEnvelope {
        observed_ledger_next_seq: u64,
    },
    RecoveredResult {
        source_session_carrier: Option<ArtifactRef>,
        failed_source_movement_seq: u64,
    },
    InterruptedResult {
        source_session_carrier: Option<ArtifactRef>,
        result_source: DurableArtifactAdmission,
        closure: DurableArtifactAdmission,
    },
}

pub(crate) struct ProviderSessionCarrierScan {
    pub(crate) latest: Option<DurableArtifactAdmission>,
    pub(crate) complete_boundary: bool,
    pub(crate) next_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeferredArtifactBindingRef {
    pub(crate) artifact: ArtifactRef,
    pub(crate) bytes_content: ContentHash,
    pub(crate) byte_count: u64,
}
