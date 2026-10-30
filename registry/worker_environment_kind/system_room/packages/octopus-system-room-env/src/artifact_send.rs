use octopus_descriptors::{ArtifactId, InterfaceId};
use octopus_interfaces::{InterfaceBoundaryError as InterfaceDriverError, InterfaceInvocationEnvelope};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_tool_door_runtime::{SystemInterfaceEmissionArtifact, ToolProcessIo};

#[derive(Clone, Debug)]
pub enum ArtifactSendBody {
    Payload { label: String, bytes: Vec<u8> },
    Reference(ArtifactRef),
}

#[derive(Clone, Debug)]
pub struct ArtifactSendCall {
    pub to_interface: InterfaceId,
    pub body: ArtifactSendBody,
}

pub enum ArtifactSendError {
    Capability(String),
    Unavailable(String),
}

pub fn decode_artifact_send_call(
    input: &InterfaceInvocationEnvelope,
    arguments: &serde_json::Value,
) -> Result<ArtifactSendCall, InterfaceDriverError> {
    let to_interface = required_trimmed_string(arguments, "interface")?;
    let body = match (arguments.get("payload"), arguments.get("ref")) {
        (Some(payload), None) => ArtifactSendBody::Payload {
            label: required_trimmed_string(arguments, "label")?,
            bytes: serde_json::to_vec_pretty(payload).map_err(|error| {
                InterfaceDriverError::Protocol(format!(
                    "artifact.send payload did not serialize: {error}"
                ))
            })?,
        },
        (None, Some(reference)) => {
            if arguments.get("label").is_some() {
                return Err(InterfaceDriverError::Protocol(
                    "artifact.send ref carries its original label and must not be relabeled".into(),
                ));
            }
            ArtifactSendBody::Reference(bound_artifact_reference(input, reference)?)
        }
        (None, None) => {
            return Err(InterfaceDriverError::Protocol(
                "artifact.send requires exactly one of 'payload' or 'ref'".into(),
            ));
        }
        (Some(_), Some(_)) => {
            return Err(InterfaceDriverError::Protocol(
                "artifact.send must not carry both 'payload' and 'ref'".into(),
            ));
        }
    };
    Ok(ArtifactSendCall {
        to_interface: InterfaceId::new(to_interface),
        body,
    })
}

pub async fn execute_artifact_send(
    input: &InterfaceInvocationEnvelope,
    io: &ToolProcessIo,
    call: ArtifactSendCall,
) -> Result<serde_json::Value, ArtifactSendError> {
    execute_artifact_send_with_reference(input, io, call).await.map(|(verdict, _)| verdict)
}

pub async fn execute_artifact_send_with_reference(
    input: &InterfaceInvocationEnvelope,
    io: &ToolProcessIo,
    call: ArtifactSendCall,
) -> Result<(serde_json::Value, ArtifactRef), ArtifactSendError> {
    ensure_interface_in_formed_grant(input, &call.to_interface)
        .map_err(|error| ArtifactSendError::Capability(error.to_string()))?;
    let emitted = match call.body {
        ArtifactSendBody::Payload { label, bytes } => {
            SystemInterfaceEmissionArtifact::Materialized(
                octopus_tool_door_runtime::room::emit_artifact(input, &label, &bytes).map_err(
                    |error| {
                        ArtifactSendError::Capability(format!(
                            "artifact.send could not materialize the emitted artifact: {error}"
                        ))
                    },
                )?,
            )
        }
        ArtifactSendBody::Reference(artifact) => {
            SystemInterfaceEmissionArtifact::Reference(artifact)
        }
    };
    let reference = match &emitted {
        SystemInterfaceEmissionArtifact::Reference(reference) => reference.clone(),
        SystemInterfaceEmissionArtifact::Materialized(file) => {
            let manifest = file.read_manifest_bounded(1024 * 1024)
                .map_err(|e| ArtifactSendError::Capability(e.to_string()))?;
            let content = ContentHash::of_bytes(&manifest.manifest_bytes);
            ArtifactRef { id: ArtifactId::new(format!("{}:{}", manifest.manifest.label, content.to_hex())), content }
        }
    };
    let verdict = io
        .hand_system_emission(&emitted, &call.to_interface)
        .await
        .map_err(ArtifactSendError::Unavailable)?;
    serde_json::to_value(&verdict).map(|verdict| (verdict, reference)).map_err(|error| {
        ArtifactSendError::Capability(format!(
            "artifact.send delivery verdict did not serialize: {error}"
        ))
    })
}

fn required_trimmed_string(
    arguments: &serde_json::Value,
    name: &str,
) -> Result<String, InterfaceDriverError> {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(format!(
                "artifact.send requires non-empty string '{name}'"
            ))
        })
}

fn bound_artifact_reference(
    input: &InterfaceInvocationEnvelope,
    reference: &serde_json::Value,
) -> Result<ArtifactRef, InterfaceDriverError> {
    let id = reference
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            InterfaceDriverError::Protocol(
                "artifact.send 'ref' must be a non-empty artifact id or content hash".into(),
            )
        })?;
    let mut matches = input
        .artifact_binding_payloads
        .iter()
        .filter(|bound| bound.artifact.id.as_str() == id || bound.artifact.content.to_hex() == id);
    let first = matches.next().ok_or_else(|| {
        InterfaceDriverError::Protocol(format!(
            "artifact.send 'ref' '{id}' is not bound to this invocation"
        ))
    })?;
    if matches.next().is_some() {
        return Err(InterfaceDriverError::Protocol(format!(
            "artifact.send 'ref' '{id}' is ambiguous"
        )));
    }
    Ok(first.artifact.clone())
}

fn ensure_interface_in_formed_grant(
    input: &InterfaceInvocationEnvelope,
    to_interface: &InterfaceId,
) -> Result<(), InterfaceDriverError> {
    if input
        .allowed_interfaces()?
        .iter()
        .any(|interface| interface == to_interface.as_str())
    {
        return Ok(());
    }
    Err(InterfaceDriverError::Protocol(format!(
        "artifact.send interface '{}' is absent from the formed interface grant",
        to_interface.as_str()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::ArtifactId;
    use octopus_interfaces::{ArtifactBindingPayload, InterfaceArtifactFile};
    use octopus_ledger::ContentHash;

    #[test]
    fn bound_reference_is_selected_without_copy_or_relabel() {
        let dir = tempfile::tempdir().unwrap();
        let current = InterfaceArtifactFile::write(dir.path(), "tool.call", b"{}").unwrap();
        let original = ArtifactRef {
            id: ArtifactId::new("memory.fact:0123456789abcdef"),
            content: ContentHash::of_bytes(b"manifest"),
        };
        let input = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new("tool.call:current"),
                content: ContentHash::of_bytes(b"current"),
            },
            artifact_file: current,
            artifact_binding_payloads: vec![ArtifactBindingPayload::new(
                original.clone(),
                b"payload".to_vec(),
            )],
            metadata: None,
        };
        let call = decode_artifact_send_call(
            &input,
            &serde_json::json!({
                "interface": "system_worker_planner",
                "ref": original.id.as_str()
            }),
        )
        .unwrap();
        assert_eq!(call.to_interface.as_str(), "system_worker_planner");
        match call.body {
            ArtifactSendBody::Reference(selected) => assert_eq!(selected, original),
            ArtifactSendBody::Payload { .. } => panic!("reference was copied into payload mode"),
        }
        assert!(decode_artifact_send_call(
            &input,
            &serde_json::json!({
                "interface": "system_worker_planner",
                "label": "changed.label",
                "ref": "memory.fact:0123456789abcdef"
            })
        )
        .is_err());
    }
}
