use octopus_interfaces::{InterfaceArtifactFile, InterfaceInvocationEnvelope};
use octopus_tool_door_runtime::room::{admitted_tool, decode_tool_call, emit_json_artifact, formed_scope};
use octopus_tool_door_runtime::{ToolProcessEnvironment, ToolProcessError, ToolProcessInvocation};
use octopus_tool_handlers::{ToolCall, ToolCallArtifact, ToolResultArtifact, ToolResultError};
use octopus_worker_env::WorkerEnvironmentFormation;
use serde_json::Value;

mod browser_session;
mod browser_consent;
pub(crate) use browser_session::history as browser_history;
mod browser_wait_completion;
pub use browser_wait_completion::run_browser_wait_completion_tool_process;
mod native_transport;
pub use native_transport::prepare_private_native_host;
pub use browser_session::decode_filechooser_door_artifact;
pub use browser_session::{bounded_optional_u64, required_string, url_is_loopback};
#[cfg(test)]
mod tests;

use browser_session::BrowserRoom;

const ROOM: &str = "browser door";
const SUBSTRATE_FAMILY: &str = "browser";

struct DecodedRoomInvocation {
    envelope: InterfaceInvocationEnvelope,
    formation: WorkerEnvironmentFormation,
    call: ToolCall,
    operation: BrowserOperation,
    correlation_id: Option<String>,
    source_movement_seq: u64,
    effective_allow_network: bool,
    descriptor_bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrowserOperation {
    Navigate,
    Snapshot,
    Click,
    Type,
    Key,
    Scroll,
    Back,
    Forward,
    Reload,
    ViewportSet,
    ViewportReset,
    Eval,
    EvaluateReadonly,
    LocatorQuery,
    LocatorFill,
    LocatorPress,
    LocatorSelect,
    LocatorCheck,
    LocatorWait,
    DomSnapshot,
    PlaywrightDomSnapshot,
    DomClick,
    DomScroll,
    PointerMove,
    Paste,
    ClipboardRead,
    ClipboardWrite,
    MediaDownload,
    VisibilityGet,
    VisibilitySet,
    Hover,
    Select,
    Drag,
    Wait,
    WaitComplete,
    History,
    HistoryResume,
    Images,
    Console,
    Dialog,
    Screenshot,
    DownloadArm,
    DownloadWait,
    DownloadCancel,
    FilechooserArm,
    FilechooserWait,
    FilechooserSetFiles,
    FilechooserCancel,
    FilechooserResume,
    Pdf,
    Cookies,
    CookieSet,
    CookieDelete,
    Tabs,
    TabNew,
    TabSelect,
    TabMark,
    TabClose,
    Close,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserHandlerConfig {
    operation: BrowserOperation,
}

pub fn browser_result(
    tool: &str,
    correlation_id: Option<String>,
    result: Result<Value, String>,
    evidence_write_errors: Vec<Value>,
) -> ToolResultArtifact {
    match result {
        Ok(body) if evidence_write_errors.is_empty() => {
            ToolResultArtifact::success(tool, correlation_id, body)
        }
        Ok(body) => {
            let mut details = serde_json::Map::new();
            details.insert("operation_body".to_string(), body);
            details.insert(
                "evidence_write_errors".to_string(),
                Value::Array(evidence_write_errors),
            );
            ToolResultArtifact::failure(
                tool,
                correlation_id,
                ToolResultError::new(
                    "worker_environment_evidence_failed",
                    format!(
                        "{tool} completed but its worker-environment evidence could not be fully written"
                    ),
                )
                .with_details(details),
            )
        }
        Err(error) => {
            let mut failure = ToolResultError::new("tool_execution_failed", error);
            if !evidence_write_errors.is_empty() {
                let mut details = serde_json::Map::new();
                details.insert(
                    "evidence_write_errors".to_string(),
                    Value::Array(evidence_write_errors),
                );
                failure = failure.with_details(details);
            }
            ToolResultArtifact::failure(tool, correlation_id, failure)
        }
    }
}

pub fn evidence_write_error(stage: &str, error: impl Into<String>) -> Value {
    serde_json::json!({
        "stage": stage,
        "message": error.into(),
    })
}

pub async fn run_browser_session_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    let room = std::sync::Arc::new(BrowserRoom::load(environment)?);
    octopus_tool_door_runtime::run_tool_process(room).await
}

fn decode_room_invocation(
    invocation: ToolProcessInvocation,
) -> Result<DecodedRoomInvocation, ToolProcessError> {
    let ToolProcessInvocation {
        descriptor,
        envelope,
        source_movement_seq,
        worker_environment,
        ..
    } = invocation;
    let formation = worker_environment.ok_or_else(|| {
        capability(format!(
            "tool '{}' invocation has no formed worker environment",
            descriptor.id
        ))
    })?;
    formed_scope(ROOM, &envelope, &formation)?;
    let operation = browser_operation(&descriptor.handler_config, descriptor.id.as_str())?;
    let artifact: ToolCallArtifact = match operation {
        BrowserOperation::FilechooserResume => {
            browser_session::decoded_response(&envelope, descriptor.id.as_str()).map_err(capability)?
        }
        BrowserOperation::HistoryResume => {
            browser_history::decoded_response(&envelope, descriptor.id.as_str()).map_err(capability)?
        }
        _ => decode_tool_call(ROOM, &envelope)?,
    };
    let tool = admitted_tool(ROOM, &envelope, &artifact)
        .map_err(|error| capability(error.to_string()))?;
    if descriptor.id.as_str() != tool {
        return Err(capability(format!(
            "tool descriptor '{}' does not match tool.call '{}'",
            descriptor.id, tool
        )));
    }
    let descriptor_bytes = octopus_descriptors::canonical_bytes(
        &serde_json::to_value(&descriptor).map_err(|error| capability(error.to_string()))?,
    );
    let effective_allow_network = descriptor.process.authority.allow_network
        && formation.network_ceiling != octopus_worker_env::NetworkCeiling::Denied;
    Ok(DecodedRoomInvocation {
        call: ToolCall {
            tool_id: tool,
            arguments: artifact.arguments,
            handler_config: descriptor.handler_config.clone(),
        },
        operation,
        correlation_id: artifact.correlation_id,
        envelope,
        formation,
        source_movement_seq,
        effective_allow_network,
        descriptor_bytes,
    })
}

fn browser_operation(
    handler_config: &Value,
    tool: &str,
) -> Result<BrowserOperation, ToolProcessError> {
    serde_json::from_value::<BrowserHandlerConfig>(handler_config.clone())
        .map(|config| config.operation)
        .map_err(|error| {
            capability(format!(
                "tool '{tool}' has malformed browser handler_config: {error}"
            ))
        })
}

fn response_artifact(
    envelope: &InterfaceInvocationEnvelope,
    response: ToolResultArtifact,
) -> Result<InterfaceArtifactFile, ToolProcessError> {
    emit_json_artifact(envelope, &response.label, &response).map_err(capability)
}

fn capability(error: impl Into<String>) -> ToolProcessError {
    ToolProcessError::Capability(error.into())
}
