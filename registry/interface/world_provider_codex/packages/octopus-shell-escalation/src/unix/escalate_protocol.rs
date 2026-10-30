use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::PathBuf;

use octopus_substrate_contract::approvals_substrate::EscalationPermissions;
use octopus_util_abspath::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;

pub const ESCALATE_SOCKET_ENV_VAR: &str = "CODEX_ESCALATE_SOCKET";

pub const EXEC_WRAPPER_ENV_VAR: &str = "EXEC_WRAPPER";

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct EscalateRequest {
    pub file: PathBuf,
    pub argv: Vec<String>,
    pub workdir: AbsolutePathBuf,
    pub env: HashMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct EscalateResponse {
    pub action: EscalateAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EscalationDecision {
    Run,
    Escalate(EscalationExecution),
    Deny { reason: Option<String> },
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EscalationExecution {
    Unsandboxed,
    TurnDefault,
    Permissions(EscalationPermissions),
}

impl EscalationDecision {
    pub fn run() -> Self {
        Self::Run
    }

    pub fn escalate(execution: EscalationExecution) -> Self {
        Self::Escalate(execution)
    }

    pub fn deny(reason: Option<String>) -> Self {
        Self::Deny { reason }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub enum EscalateAction {
    Run,
    Escalate,
    Deny { reason: Option<String> },
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SuperExecMessage {
    pub fds: Vec<RawFd>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SuperExecResult {
    pub exit_code: i32,
}
