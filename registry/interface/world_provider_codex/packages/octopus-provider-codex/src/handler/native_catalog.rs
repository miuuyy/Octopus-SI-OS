use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InterfaceOperationAffordance {
    pub(crate) name: String,
    pub(crate) interface: InterfaceId,
    pub(crate) request_label: String,
    pub(crate) output_label: String,
    pub(crate) input_labels: BTreeSet<String>,
    pub(crate) operator_approval: bool,
}

pub(crate) struct NativeAffordanceCatalog {
    pub(crate) specs: Vec<DynamicToolSpec>,
    pub(crate) tool_names: BTreeMap<String, String>,
    pub(crate) interface_operations: BTreeMap<String, InterfaceOperationAffordance>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeToolNamespace {
    pub(crate) name: String,
    pub(crate) description: String,
}

impl NativeToolNamespace {
    pub(crate) fn validate(self) -> Result<Self, InterfaceDriverError> {
        if self.name.trim() != self.name
            || self.name.is_empty()
            || !self
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(InterfaceDriverError::Protocol(
                "interface_driver_config.private_config.native_tool_namespace.name must be a non-empty Codex namespace identifier"
                    .to_string(),
            ));
        }
        if self.description.trim() != self.description
            || self.description.is_empty()
            || self.description.chars().count() > 1024
        {
            return Err(InterfaceDriverError::Protocol(
                "interface_driver_config.private_config.native_tool_namespace.description must be trimmed and contain 1–1024 characters"
                    .to_string(),
            ));
        }
        Ok(self)
    }
}

pub(crate) struct InterfaceOperationResultArtifact {
    pub(crate) label: String,
    pub(crate) operation: String,
    pub(crate) correlation_id: String,
    pub(crate) ok: bool,
    pub(crate) value: serde_json::Value,
}

pub(crate) struct InterfaceContextArtifact {
    pub(crate) interface: InterfaceId,
    pub(crate) label: String,
    pub(crate) content: serde_json::Value,
}

#[derive(Clone)]
pub(crate) enum OperatorResponseArtifact {
    Approval {
        request_id: String,
        decision: serde_json::Value,
    },
    Elicitation {
        request_id: String,
        answer: serde_json::Value,
    },
    UserInput {
        request_id: String,
        answers: serde_json::Value,
    },
}

pub(crate) enum ProviderInput {
    Turn(Vec<UserInput>),
    Resume { control_id: String },
    ToolResult(ToolResultArtifact),
    InterfaceOperationResult(InterfaceOperationResultArtifact),
    InterfaceContext(InterfaceContextArtifact),
    OperatorResponse(OperatorResponseArtifact),
}

pub(crate) struct RecoveredToolContinuation {
    pub(crate) item: serde_json::Value,
    pub(crate) activity: CodexTurnOutput,
    pub(crate) activity_already_admitted: bool,
}

pub(crate) struct CutTurnContinuation {
    pub(crate) context: String,
    pub(crate) activity: CodexTurnOutput,
    pub(crate) activity_already_admitted: bool,
}

pub(crate) enum NativeTerminalCheck {
    Open,
    Closed,
    CutByProcessDeath(CodexTurnOutput),
}

impl NativeTerminalCheck {
    pub(crate) fn settled(&self) -> bool {
        !matches!(self, Self::Open)
    }
}

pub(crate) enum ThreadAction {
    Reuse { thread_id: String },
    Mint,
}

pub(crate) fn plan_thread_action(state: &TransportState, fingerprint: &str) -> ThreadAction {
    match (&state.thread_id, &state.thread_shape_fingerprint) {
        (Some(thread_id), Some(shape)) if shape == fingerprint => ThreadAction::Reuse {
            thread_id: thread_id.clone(),
        },
        _ => ThreadAction::Mint,
    }
}

pub(crate) fn thread_shape_fingerprint(
    model: Option<&str>,
    bound_instructions: &str,
    native_affordances: &[DynamicToolSpec],
    static_context_fingerprint: &str,
) -> Result<String, InterfaceDriverError> {
    let affordances = serde_json::to_string(native_affordances).map_err(|e| {
        InterfaceDriverError::Protocol(format!(
            "native affordances failed to serialize for the thread fingerprint: {e}"
        ))
    })?;
    let mut bytes = Vec::with_capacity(
        model.map(str::len).unwrap_or(0) + bound_instructions.len() + affordances.len() + 2,
    );
    bytes.extend_from_slice(model.unwrap_or("").as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(bound_instructions.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(affordances.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(static_context_fingerprint.as_bytes());
    Ok(sha256_hex(&bytes))
}

pub(crate) fn validate_recovered_thread_coordinates(
    recovered: &CodexContinuityState,
    fingerprint: &str,
    covered_context_generation: u64,
    static_context_fingerprint: &str,
) -> Result<(), InterfaceDriverError> {
    if recovered.thread_shape_fingerprint != fingerprint
        || recovered.context_generation != covered_context_generation
        || recovered.static_context_fingerprint != static_context_fingerprint
    {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex continuity is stale for the current context coordinates".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_recovered_reseed_coordinates(
    recovered: &CodexContinuityState,
    context_generation: u64,
    session_context_available: bool,
) -> Result<(), InterfaceDriverError> {
    if !session_context_available {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex private thread cold-start requires a descriptor-declared session context snapshot"
                .to_string(),
        ));
    }
    if recovered.context_generation > context_generation {
        return Err(InterfaceDriverError::FailedClosed(
            "Codex recovered continuity is ahead of the admitted session context".to_string(),
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct ThreadAcquisition<'a> {
    pub(crate) recovered: Option<&'a CodexContinuityState>,
    pub(crate) retired_history: Option<&'a NativeHistoryProof>,
    pub(crate) require_exact_resume: bool,
    pub(crate) context_generation: u64,
    pub(crate) input_changes_context: bool,
    pub(crate) model: Option<String>,
    pub(crate) bound_instructions: &'a str,
    pub(crate) native_affordances: &'a [DynamicToolSpec],
    pub(crate) static_context_fingerprint: &'a str,
    pub(crate) session_context_available: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrivateThreadColdStart {
    ResumeExact,
    ResumeCompatible,
    ReseedFromSessionContext,
}
