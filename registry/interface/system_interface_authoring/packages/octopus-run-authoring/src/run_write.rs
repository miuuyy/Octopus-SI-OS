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
