use std::path::PathBuf;

use octopus_interfaces::{InterfaceArtifactFile, InterfaceBoundaryError, InterfaceInvocationEnvelope};
use octopus_tool_handlers::ToolCallArtifact;
use octopus_worker_env::WorkerEnvironmentFormation;
use serde_json::{Map, Value};

use crate::ToolProcessError;

pub const TOOL_CALL_LABEL: &str = "tool.call";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationScope {
    pub run: String,
    pub ee: String,
}

pub fn invocation_scope(
    room: &str,
    input: &InterfaceInvocationEnvelope,
) -> Result<InvocationScope, InterfaceBoundaryError> {
    let runtime = input
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.runtime.as_ref())
        .ok_or_else(|| {
            InterfaceBoundaryError::Protocol(format!(
                "{room} requires runtime metadata beside the artifact"
            ))
        })?;
    Ok(InvocationScope {
        run: runtime.run.as_str().to_string(),
        ee: runtime.ee.as_str().to_string(),
    })
}

pub fn formed_scope(
    room: &str,
    input: &InterfaceInvocationEnvelope,
    formation: &WorkerEnvironmentFormation,
) -> Result<InvocationScope, ToolProcessError> {
    let scope = invocation_scope(room, input)
        .map_err(|error| ToolProcessError::Capability(error.to_string()))?;
    if scope.run != formation.run || scope.ee != formation.ee {
        return Err(ToolProcessError::Capability(format!(
            "{room} formation does not match invocation coordinates"
        )));
    }
    Ok(scope)
}

pub fn admitted_tool(
    room: &str,
    input: &InterfaceInvocationEnvelope,
    call: &ToolCallArtifact,
) -> Result<String, InterfaceBoundaryError> {
    let tool = call.tool.trim();
    if tool.is_empty() {
        return Err(InterfaceBoundaryError::Protocol(format!(
            "{room} tool.call has empty tool name"
        )));
    }
    if !input
        .granted_tool_names()?
        .iter()
        .any(|grant| grant == tool)
    {
        return Err(InterfaceBoundaryError::Protocol(format!(
            "{room} denied tool.call '{tool}' because formed metadata did not grant it"
        )));
    }
    if input
        .hard_blocked_tool_names()?
        .iter()
        .any(|blocked| blocked == tool)
    {
        return Err(InterfaceBoundaryError::Protocol(format!(
            "{room} denied hard-blocked tool.call '{tool}'"
        )));
    }
    Ok(tool.to_string())
}

pub fn decode_tool_call(
    room: &str,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<ToolCallArtifact, ToolProcessError> {
    let manifest = envelope
        .read_artifact_manifest()
        .map_err(|error| ToolProcessError::Capability(error.to_string()))?;
    if manifest.label != TOOL_CALL_LABEL {
        return Err(ToolProcessError::Capability(format!(
            "{room} expected '{TOOL_CALL_LABEL}', got '{}'",
            manifest.label
        )));
    }
    let payload = envelope
        .read_artifact_payload()
        .map_err(|error| ToolProcessError::Capability(error.to_string()))?;
    serde_json::from_slice(&payload).map_err(|error| {
        ToolProcessError::Capability(format!("{room} tool.call is malformed: {error}"))
    })
}

pub fn descriptor_operation<'a>(
    room: &str,
    tool_id: &str,
    handler_config: &'a Value,
    operations: &[&str],
) -> Result<&'a str, String> {
    let operation = handler_config
        .as_object()
        .ok_or_else(|| {
            format!("tool '{tool_id}' handler_config must be an object with an operation")
        })?
        .get("operation")
        .ok_or_else(|| format!("tool '{tool_id}' handler_config.operation is absent"))?
        .as_str()
        .ok_or_else(|| format!("tool '{tool_id}' handler_config.operation must be a string"))?;
    if operation.is_empty() || operation.trim() != operation {
        return Err(format!(
            "tool '{tool_id}' handler_config.operation must be a non-empty trimmed string"
        ));
    }
    if !operations.contains(&operation) {
        return Err(format!(
            "tool '{tool_id}' declares unknown {room} operation '{operation}'"
        ));
    }
    Ok(operation)
}

pub fn strict_object<'a>(
    tool: &str,
    value: &'a Value,
    allowed: &[&str],
) -> Result<&'a Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{tool} arguments must be a JSON object"))?;
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(format!("{tool} arguments contain unknown field '{field}'"));
    }
    Ok(object)
}

pub fn emission_root(input: &InterfaceInvocationEnvelope) -> Result<PathBuf, String> {
    input
        .artifact_file
        .manifest_path()
        .parent()
        .map(|parent| parent.join("emitted"))
        .ok_or_else(|| {
            format!(
                "tool input manifest '{}' has no parent directory",
                input.artifact_file.manifest_path().display()
            )
        })
}

pub fn emit_artifact(
    input: &InterfaceInvocationEnvelope,
    label: &str,
    payload: &[u8],
) -> Result<InterfaceArtifactFile, String> {
    InterfaceArtifactFile::write(emission_root(input)?, label, payload)
        .map_err(|error| error.to_string())
}

pub fn emit_json_artifact<T: serde::Serialize>(
    input: &InterfaceInvocationEnvelope,
    label: &str,
    body: &T,
) -> Result<InterfaceArtifactFile, String> {
    let payload = serde_json::to_vec(body)
        .map_err(|error| format!("emitted '{label}' artifact did not serialize: {error}"))?;
    emit_artifact(input, label, &payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_clone_selects_only_its_descriptor_operation() {
        let operations = ["start", "exec"];
        for tool_id in ["room.start", "room.custom_start"] {
            assert_eq!(
                descriptor_operation("example", tool_id, &json!({"operation": "start"}), &operations),
                Ok("start")
            );
        }
        for config in [
            Value::Null,
            json!({}),
            json!({"operation": 1}),
            json!({"operation": " start"}),
            json!({"operation": "unknown"}),
        ] {
            assert!(descriptor_operation("example", "room.clone", &config, &operations).is_err());
        }
    }
}
