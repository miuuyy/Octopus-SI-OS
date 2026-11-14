use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CodexPendingContinuity {
    DynamicTool {
        turn_id: String,
        call_id: String,
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_operation: Option<serde_json::Value>,
    },
    InterfaceOperation {
        turn_id: String,
        call_id: String,
        operation: String,
        interface: InterfaceId,
        output_label: String,
    },
    Operator {
        turn_id: String,
        request_key: String,
        method: String,
    },
}

#[derive(Clone)]
pub(crate) struct PendingServerRequest {
    pub(crate) request_id: RequestId,
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
    pub(crate) key: String,
    pub(crate) kind: PendingServerRequestKind,
    pub(crate) effect_gate: Arc<tokio::sync::Mutex<()>>,
    pub(crate) turn_interrupted: Arc<std::sync::atomic::AtomicBool>,
}

pub(crate) type PendingServerRequests =
    Arc<Mutex<BTreeMap<(TransportStateScope, String), PendingServerRequest>>>;

#[derive(Clone)]
pub(crate) enum PendingServerRequestKind {
    DynamicTool {
        call_id: String,
        tool: String,
    },
    InterfaceOperation {
        call_id: String,
        operation: String,
        interface: InterfaceId,
        output_label: String,
    },
    Operator {
        method: String,
    },
}

pub(crate) enum TurnDrainOutcome {
    Complete,
    Suspended(PendingServerRequest),
    AwaitingResults,
}

pub(crate) enum ProviderStreamEmission {
    Turn(ProviderEmission),
    Progress(ProviderEmission),
    WithOutgoingBindings {
        emission: ProviderEmission,
        bindings: Vec<octopus_run_authoring::ProviderOutgoingBinding>,
    },
    WithCapabilityAmendment {
        emission: ProviderEmission,
        amendment: octopus_run_authoring::ProviderCapabilityAmendment,
    },
    TerminalGroup {
        stream: ProviderEmission,
        terminal: ProviderEmission,
    },
    InterruptedResult {
        activity: Option<ProviderEmission>,
        stream: Option<ProviderEmission>,
        retained_outputs: Vec<ProviderEmission>,
        closure: ProviderEmission,
    },
    Control {
        emission: ProviderEmission,
        source_movement_seq: u64,
        admitted: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    TransportDispatch {
        artifact: InterfaceArtifactFile,
        admitted: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    ContextBoundary {
        emissions: Vec<ProviderEmission>,
        admitted: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    CurrentRunRecord {
        expected_before_hash: Option<String>,
        reply: tokio::sync::oneshot::Sender<Result<(Vec<u8>, Option<Vec<u8>>), String>>,
    },
}

pub(crate) struct ProviderOutputStream {
    pub(crate) sender: tokio::sync::mpsc::UnboundedSender<ProviderStreamEmission>,
    pub(crate) continuity: Arc<std::sync::Mutex<ContinuityEmitterState>>,
}

pub(crate) struct ProviderHandoffBarrier {
    pub(crate) outcome: std::sync::Mutex<Option<Result<(), String>>>,
    pub(crate) notify: tokio::sync::Notify,
}

impl ProviderHandoffBarrier {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            outcome: std::sync::Mutex::new(None),
            notify: tokio::sync::Notify::new(),
        })
    }

    pub(crate) fn finish(&self, outcome: Result<(), String>) {
        *self.outcome.lock().expect("handoff barrier lock") = Some(outcome);
        self.notify.notify_waiters();
    }

    pub(crate) async fn wait(&self) -> Result<(), String> {
        loop {
            let notified = self.notify.notified();
            if let Some(outcome) = self.outcome.lock().expect("handoff barrier lock").clone() {
                return outcome;
            }
            notified.await;
        }
    }
}

pub(crate) enum ControlOutputError {
    Closed,
    Failed(String),
}

impl ProviderOutputStream {
    pub(crate) fn channel() -> (
        Arc<Self>,
        tokio::sync::mpsc::UnboundedReceiver<ProviderStreamEmission>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Arc::new(Self {
            sender,
            continuity: Arc::new(std::sync::Mutex::new(ContinuityEmitterState::default())),
        }), receiver)
    }

    pub(crate) fn continuity_snapshot(&self) -> Option<CodexContinuityState> {
        self.continuity.lock().expect("continuity lock").continuity.clone()
    }

    pub(crate) fn record_control_continuity(&self, result: &CodexTurnControlResult) {
        if let Some(continuity) = &result.continuity {
            let mut state = self.continuity.lock().expect("continuity lock");
            state.continuity = Some(continuity.clone());
            state.pending_input_context = false;
        }
    }

    pub(crate) fn send_turn(&self, emission: ProviderEmission) -> Result<(), InterfaceDriverError> {
        self.sender
            .send(ProviderStreamEmission::Turn(emission))
            .map_err(|_| {
                InterfaceDriverError::Transport(
                    "turn emission sink closed (the interface life is gone); aborting the turn"
                        .into(),
                )
            })
    }

    pub(crate) fn send_progress(&self, emission: ProviderEmission) -> Result<(), InterfaceDriverError> {
        self.sender
            .send(ProviderStreamEmission::Progress(emission))
            .map_err(|_| {
                InterfaceDriverError::Transport(
                    "turn progress sink closed before the observation could be handed".into(),
                )
            })
    }

    pub(crate) fn send_interrupted_result(
        &self,
        activity: Option<ProviderEmission>,
        stream: Option<ProviderEmission>,
        retained_outputs: Vec<ProviderEmission>,
        closure: ProviderEmission,
    ) -> Result<(), InterfaceDriverError> {
        self.sender
            .send(ProviderStreamEmission::InterruptedResult {
                activity,
                stream,
                retained_outputs,
                closure,
            })
            .map_err(|_| {
                InterfaceDriverError::Transport(
                    "interrupted-result emission sink closed before its exact closure could be handed"
                        .to_string(),
                )
            })
    }

    pub(crate) fn send_terminal_group(
        &self,
        stream: ProviderEmission,
        terminal: ProviderEmission,
    ) -> Result<(), InterfaceDriverError> {
        self.sender
            .send(ProviderStreamEmission::TerminalGroup { stream, terminal })
            .map_err(|_| {
                InterfaceDriverError::Transport(
                    "turn emission sink closed before its sealed terminal group could be handed"
                        .to_string(),
                )
            })
    }

    pub(crate) async fn hand_control(
        self: Arc<Self>,
        emission: ProviderEmission,
        source_movement_seq: u64,
    ) -> Result<(), ControlOutputError> {
        let (admitted, receipt) = tokio::sync::oneshot::channel();
        self.sender
            .send(ProviderStreamEmission::Control { emission, source_movement_seq, admitted })
            .map_err(|_| ControlOutputError::Closed)?;
        drop(self);
        receipt
            .await
            .map_err(|_| {
                ControlOutputError::Failed(
                    "provider output stream dropped the control-result receipt".to_string(),
                )
            })?
            .map_err(ControlOutputError::Failed)
    }

    pub(crate) async fn current_run_record(&self, expected_before_hash: Option<String>) -> Result<(Vec<u8>, Option<Vec<u8>>), InterfaceDriverError> {
        let (reply, receipt) = tokio::sync::oneshot::channel();
        self.sender.send(ProviderStreamEmission::CurrentRunRecord { expected_before_hash, reply })
            .map_err(|_| InterfaceDriverError::Transport("provider output stream closed before the checked run read".into()))?;
        receipt.await
            .map_err(|_| InterfaceDriverError::Transport("provider output stream dropped the checked run read".into()))?
            .map_err(InterfaceDriverError::FailedClosed)
    }

    pub(crate) async fn hand_transport_dispatch(
        self: Arc<Self>,
        artifact: InterfaceArtifactFile,
    ) -> Result<(), InterfaceDriverError> {
        let (admitted, receipt) = tokio::sync::oneshot::channel();
        self.sender
            .send(ProviderStreamEmission::TransportDispatch { artifact, admitted })
            .map_err(|_| {
                InterfaceDriverError::Transport(
                    "provider output stream closed before transport dispatch admission".to_string(),
                )
            })?;
        drop(self);
        receipt
            .await
            .map_err(|_| {
                InterfaceDriverError::Transport(
                    "provider output stream dropped transport dispatch receipt".to_string(),
                )
            })?
            .map_err(InterfaceDriverError::FailedClosed)
    }
}
