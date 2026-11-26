use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AuthoredRun {
    pub(crate) json_format: Option<JsonFormat>,
    pub(crate) content: String,
    pub(crate) patch: json_patch::Patch,
    pub(crate) before_hash: String,
    pub(crate) after_hash: String,
    pub(crate) correlation_id: String,
    pub(crate) authoring_result_checkpoint: CheckpointId,
    pub(crate) authoring_result_interface: InterfaceId,
    pub(crate) pending_handoff: PendingHandoff,
}

pub(crate) type FileWriteResultEnvelope = ToolResultArtifact;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileWriteCallEnvelope {
    pub(crate) tool: String,
    pub(crate) arguments: FileWriteCallArguments,
    pub(crate) correlation_id: String,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileWriteCallArguments {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "octopus_file_write_contract::optional_json_format")]
    pub(crate) json_format: Option<JsonFormat>,
    pub(crate) path: PathBuf,
    pub(crate) patch: json_patch::Patch,
    pub(crate) expected_before_hash: String,
    pub(crate) expected_after_hash: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileWriteResultBody {
    pub(crate) result: String,
    pub(crate) writes: Vec<FileWriteEvidence>,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileWriteEvidence {
    pub(crate) path: PathBuf,
    pub(crate) before_hash: Option<String>,
    pub(crate) after_hash: String,
    pub(crate) bytes: u64,
}

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InterfaceFailureEnvelope {
    pub(crate) source_movement_seq: u64,
    pub(crate) failure_class: String,
    pub(crate) details: serde_json::Value,
}

pub(crate) struct FileWriteFailureResult {
    pub(crate) correlation_id: String,
    pub(crate) failure: InterfaceFailureEnvelope,
}

pub(crate) enum AuthoredRunInstallOutcome {
    Installed,
    TerminalFailure(ProviderHandoffOutcome),
}

pub(crate) enum AuthoringWriteOutcome {
    Written,
    Rejected(authoring_retry::RejectedWrite),
    TerminalFailure {
        failure_seq: u64,
        failure: InterfaceFailureEnvelope,
    },
}

pub use octopus_app_server_interface::application_consent_contract::{
    CheckedRunWriter, CheckedRunInstall, ConsentEvidenceRef, ConsentHumanEvidence, ConsentIntake,
    ConsentPolicyAuthorization, SelectedConsentReturn, SelectedConsentBinding,
};

pub(crate) fn refuse_selected_consent_state(record: &RunRecord, provider: &InterfaceId) -> Result<(), String> {
    if record.interface_state.get(provider).is_some_and(|state| state.contains_key("selected_consent")) {
        return Err("selected Home consent state cannot use ordinary provider authoring".into());
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct CheckedRunTransition {
    /// Captured by the evidence reader from an admitted blob or the declared provider owner root.
    pub capability_payload: Option<std::sync::Arc<[u8]>>,
    pub writer: CheckedRunWriter,
    pub before: Vec<u8>,
    pub after: Vec<u8>,
    pub call: Movement,
    pub result: Movement,
    pub foreign: bool,
}

#[derive(Debug)]
pub enum CheckedRunError {
    Invalid(String),
    AwaitingResults { call_sequences: Vec<u64> },
    ChangedDuringRead { captured: String, current: String },
}

impl From<String> for CheckedRunError {
    fn from(error: String) -> Self { Self::Invalid(error) }
}

impl From<&str> for CheckedRunError {
    fn from(error: &str) -> Self { Self::Invalid(error.to_string()) }
}

impl std::fmt::Display for CheckedRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(error) => formatter.write_str(error),
            Self::AwaitingResults { call_sequences } => write!(formatter, "run change awaits admitted checked file.write results for calls {call_sequences:?}"),
            Self::ChangedDuringRead { captured, current } => write!(formatter, "run changed during evidence collection from {captured} to {current}"),
        }
    }
}

impl From<CheckedRunError> for String {
    fn from(error: CheckedRunError) -> Self { error.to_string() }
}

pub(crate) fn validate_write_result_evidence(
    config: &RunAuthoringConfig,
    run_path: &Path,
    result: FileWriteResultEnvelope,
    authored: &AuthoredRun,
) -> Result<(), String> {
    validate_file_write_evidence(
        &RunAuthoringProfile {
            file_write_tool: config.file_write_tool.clone(),
            file_write_label: config.file_write_label.clone(),
            file_write_result_label: config.file_write_result_label.clone(),
        },
        run_path,
        result,
        &authored.before_hash,
        &authored.after_hash,
        authored.content.len(),
    )
}

pub(crate) fn preserves_provider_evidence(before: &[u8], after: &[u8], provider: &InterfaceId) -> Result<bool, String> {
    let before = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let after = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    if before.id != after.id || before.ee != after.ee { return Ok(false); }
    let execution = |record: &RunRecord| {
        record.interface_state.get(provider).map(|state| {
            let mut state = state.clone();
            state.remove(RUN_AUTHORING_BINDING);
            state.remove(FRONTIER_CHECKPOINT_IDS);
            state
        })
    };
    Ok(execution(&before) == execution(&after))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderedFileWriteReceipt {
    path: PathBuf,
    before_hash: Option<String>,
    after_hash: String,
    bytes: u64,
    outcome: String,
    observed_hash: Option<String>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderedFileWriteReceipts {
    result: String,
    writes: Vec<OrderedFileWriteReceipt>,
}

pub(crate) fn applied_file_write_receipts(result: &ToolResultArtifact) -> Result<Option<Vec<Option<FileWriteEvidence>>>, String> {
    let body = if result.ok && result.error.is_none() {
        result.body.as_ref()
    } else if !result.ok && result.body.is_none() {
        result.error.as_ref().filter(|error| error.code == "file_write_incomplete")
            .and_then(|error| error.details.as_ref()).and_then(|details| details.get("file_write"))
    } else { None };
    let Some(body) = body else { return Ok(None); };
    match body.get("result").and_then(serde_json::Value::as_str) {
        Some("files_written") if result.ok => {
            let body: FileWriteResultBody = serde_json::from_value(body.clone()).map_err(|error| error.to_string())?;
            Ok(Some(body.writes.into_iter().map(Some).collect()))
        }
        Some("ordered_file_writes") => {
            let body: OrderedFileWriteReceipts = serde_json::from_value(body.clone()).map_err(|error| error.to_string())?;
            if body.result != "ordered_file_writes" { return Err("ordered write receipt changed its operation".into()); }
            let mut stopped = false;
            let mut applied = Vec::with_capacity(body.writes.len());
            for receipt in body.writes {
                match receipt.outcome.as_str() {
                    "applied" if !stopped => {
                        if receipt.observed_hash.as_ref() != Some(&receipt.after_hash) || receipt.error.is_some() {
                            return Err("applied ordered write has no exact effect receipt".into());
                        }
                        applied.push(Some(FileWriteEvidence { path: receipt.path, before_hash: receipt.before_hash,
                            after_hash: receipt.after_hash, bytes: receipt.bytes }));
                    }
                    "refused" | "uncertain" if !stopped && !result.ok => { stopped = true; applied.push(None); }
                    "not_attempted" if stopped && !result.ok => applied.push(None),
                    _ => return Err("ordered write receipts do not describe an applied prefix".into()),
                }
            }
            if !result.ok && !stopped { return Err("incomplete ordered write has no stopped entry".into()); }
            Ok(Some(applied))
        }
        _ => Ok(None),
    }
}
