use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use octopus_interfaces::{InterfaceArtifactFile, InterfaceInvocationEnvelope};
use octopus_tool_door_runtime::room::{admitted_tool, decode_tool_call, emit_json_artifact};
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo,
};
use octopus_tool_handlers::{ToolResultArtifact, ToolResultError};
use octopus_tool_registry::ToolDescriptor;
use octopus_worker_env::WorkerEnvironmentLifecycle;

mod artifact_bound;
mod artifact_bound_process;
mod client_mcp;
mod collection_tools;
mod artifact_send;
mod bound_surface;
mod file_write;
mod file_read;
mod memory_bound;
mod memory_observation;
mod skill_bound;

pub use artifact_bound_process::run_artifact_bound_tool_process;
pub use file_read::run_file_read_tool_process;
pub use file_write::execute_file_write_result;
pub use memory_bound::{MemoryHost, execute as execute_memory_tool};

const ROOM: &str = "system-room door";
const SUBSTRATE_FAMILY: &str = "system_room";

struct FileWriteProcess {
    root_slots: BTreeMap<String, PathBuf>,
    effect_root: PathBuf,
    max_payload_bytes: u64,
}

struct ArtifactSendProcess;

impl ToolProcessHandler for FileWriteProcess {
    fn substrate_family(&self) -> &str {
        SUBSTRATE_FAMILY
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolProcessInvocation,
        io: ToolProcessIo,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<InterfaceArtifactFile>, ToolProcessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let call = decode_invocation(&invocation.descriptor, &invocation.envelope)
                .map_err(capability)?;
            let room_kinds = invocation
                .descriptor
                .supported_worker_environment_kinds
                .iter()
                .map(|kind| kind.as_str().to_string())
                .collect::<Vec<_>>();
            let formation = invocation.worker_environment.as_ref().ok_or_else(|| {
                capability("file.write invocation has no formed worker environment")
            })?;
            let response = match file_write::execute_file_write_result(
                &invocation.envelope,
                &call.arguments,
                &room_kinds,
                &self.root_slots,
                &self.effect_root,
                self.max_payload_bytes,
            ) {
                Ok(body) => {
                    io.record_worker_environment_lifecycle(
                        formation,
                        WorkerEnvironmentLifecycle::Active,
                    )
                    .await
                    .map_err(ToolProcessError::Unavailable)?;
                    ToolResultArtifact::success(&call.tool, call.correlation_id, body)
                }
                Err(error) => {
                    io.record_worker_environment_lifecycle(
                        formation,
                        WorkerEnvironmentLifecycle::Failed,
                    )
                    .await
                    .map_err(ToolProcessError::Unavailable)?;
                    ToolResultArtifact::failure(
                        &call.tool,
                        call.correlation_id,
                        error,
                    )
                }
            };
            materialize_response(&invocation.envelope, response)
                .map(|artifact| vec![artifact])
                .map_err(capability)
        })
    }
}

impl ToolProcessHandler for ArtifactSendProcess {
    fn substrate_family(&self) -> &str {
        SUBSTRATE_FAMILY
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolProcessInvocation,
        io: ToolProcessIo,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<InterfaceArtifactFile>, ToolProcessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let call = decode_invocation(&invocation.descriptor, &invocation.envelope)
                .map_err(capability)?;
            let formation = invocation.worker_environment.as_ref().ok_or_else(|| {
                capability("artifact.send invocation has no formed worker environment")
            })?;
            let send_call = match artifact_send::decode_artifact_send_call(
                &invocation.envelope,
                &call.arguments,
            ) {
                Ok(call) => call,
                Err(error) => {
                    io.record_worker_environment_lifecycle(
                        formation,
                        WorkerEnvironmentLifecycle::Failed,
                    )
                    .await
                    .map_err(ToolProcessError::Unavailable)?;
                    let response = ToolResultArtifact::failure(
                        &call.tool,
                        call.correlation_id,
                        ToolResultError::new("tool_call_invalid", error.to_string()),
                    );
                    return materialize_response(&invocation.envelope, response)
                        .map(|artifact| vec![artifact])
                        .map_err(capability);
                }
            };
            let response =
                match artifact_send::execute_artifact_send(&invocation.envelope, &io, send_call)
                    .await
                {
                    Ok(body) => {
                        io.record_worker_environment_lifecycle(
                            formation,
                            WorkerEnvironmentLifecycle::Active,
                        )
                        .await
                        .map_err(ToolProcessError::Unavailable)?;
                        ToolResultArtifact::success(&call.tool, call.correlation_id, body)
                    }
                    Err(artifact_send::ArtifactSendError::Capability(error)) => {
                        io.record_worker_environment_lifecycle(
                            formation,
                            WorkerEnvironmentLifecycle::Failed,
                        )
                        .await
                        .map_err(ToolProcessError::Unavailable)?;
                        ToolResultArtifact::failure(
                            &call.tool,
                            call.correlation_id,
                            ToolResultError::new("tool_execution_failed", error),
                        )
                    }
                    Err(artifact_send::ArtifactSendError::Unavailable(error)) => {
                        return Err(ToolProcessError::Unavailable(error));
                    }
                };
            materialize_response(&invocation.envelope, response)
                .map(|artifact| vec![artifact])
                .map_err(capability)
        })
    }
}

struct DecodedToolCall {
    tool: String,
    arguments: serde_json::Value,
    correlation_id: Option<String>,
}

pub async fn run_file_write_tool_process() -> Result<(), String> {
    let environment = ToolProcessEnvironment::from_env()?;
    let effect_root = environment
        .durable_var_dir
        .as_ref()
        .ok_or_else(|| {
            "file.write requires formation-scoped durable tool-process state".to_string()
        })?
        .join("file-write-effects");
    octopus_tool_door_runtime::run_tool_process(Arc::new(FileWriteProcess {
        root_slots: environment.root_slots,
        effect_root,
        max_payload_bytes: environment.host_io_limits.max_payload_bytes,
    }))
    .await
}

pub async fn run_artifact_send_tool_process() -> Result<(), String> {
    octopus_tool_door_runtime::run_tool_process(Arc::new(ArtifactSendProcess)).await
}

fn decode_invocation(
    descriptor: &ToolDescriptor,
    input: &InterfaceInvocationEnvelope,
) -> Result<DecodedToolCall, String> {
    let artifact = decode_tool_call(ROOM, input).map_err(|error| error.to_string())?;
    let tool = admitted_tool(ROOM, input, &artifact).map_err(|error| error.to_string())?;
    if descriptor.id.as_str() != tool {
        return Err(format!(
            "tool descriptor '{}' does not match tool.call '{}'",
            descriptor.id, tool
        ));
    }
    Ok(DecodedToolCall {
        tool,
        arguments: artifact.arguments,
        correlation_id: artifact.correlation_id,
    })
}

fn materialize_response(
    input: &InterfaceInvocationEnvelope,
    response: ToolResultArtifact,
) -> Result<InterfaceArtifactFile, String> {
    emit_json_artifact(input, &response.label, &response)
}

fn capability(error: impl ToString) -> ToolProcessError {
    ToolProcessError::Capability(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use octopus_descriptors::{
        ArtifactId, Descriptor, DescriptorHash, DescriptorName, ToolDescriptorId,
    };
    use octopus_interfaces::{GrantProjection, InvocationMetadata};
    use octopus_ledger::{ArtifactRef, ContentHash};
    use octopus_tool_door_runtime::room::TOOL_CALL_LABEL;
    use octopus_tool_registry::{ToolArgumentValidator, load_tool_descriptor_from_dir};

    use super::*;

    fn repository_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../..")
            .canonicalize()
            .unwrap()
    }

    #[test]
    fn registered_exact_clone_executes_the_single_file_write_operation() {
        let descriptor = load_tool_descriptor_from_dir(
            &repository_root().join("registry/tool"),
            "file.write.cron_authoring",
        )
        .unwrap()
        .unwrap();
        assert!(descriptor.handler_config.is_null());

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("writes/result.json");
        let payload = serde_json::to_vec(&serde_json::json!({
            "tool": descriptor.id.as_str(),
            "arguments": {
                "path": target,
                "content": "{\"exact\":true}\n",
                "expected_before_hash": null
            }
        }))
        .unwrap();
        let content = ContentHash::of_bytes(&payload);
        let artifact_file =
            InterfaceArtifactFile::write(root.path().join("inbound"), TOOL_CALL_LABEL, &payload)
                .unwrap();
        let mut envelope = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
                content,
            },
            artifact_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(InvocationMetadata::new(GrantProjection {
                tools: vec![descriptor.id.as_str().to_string()],
                worker_environment_kinds: vec!["system_room".to_string()],
                ..GrantProjection::default()
            })),
        };
        let call = decode_invocation(&descriptor, &envelope).unwrap();
        assert_eq!(call.tool, "file.write.cron_authoring");
        let room_kinds = descriptor
            .supported_worker_environment_kinds
            .iter()
            .map(|kind| kind.as_str().to_string())
            .collect::<Vec<_>>();
        let root_slots = BTreeMap::from([("test_root".to_string(), root.path().to_path_buf())]);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "{\"exact\":true}\n").unwrap();
        assert!(file_write::execute_file_write(
            &envelope, &call.arguments, &room_kinds, &root_slots,
            &root.path().join("effects"), 8 * 1024 * 1024,
        ).unwrap_err().contains("optimistic hash mismatch"));
        std::fs::remove_file(&target).unwrap();
        let body = file_write::execute_file_write(
            &envelope,
            &call.arguments,
            &room_kinds,
            &root_slots,
            &root.path().join("effects"),
            8 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(body["result"], "files_written");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "{\"exact\":true}\n"
        );

        let patch_descriptor =
            load_tool_descriptor_from_dir(&repository_root().join("registry/tool"), "file.write")
                .unwrap()
                .unwrap();
        patch_descriptor.validate().unwrap();
        let patched = "{\n  \"exact\": true,\n  \"patched\": true\n}\n";
        let patch_payload = serde_json::to_vec(&serde_json::json!({
            "tool": patch_descriptor.id.as_str(),
            "arguments": {
                "path": target,
                "patch": [
                    {
                        "op": "add",
                        "path": "/patched",
                        "value": true
                    }
                ],
                "expected_before_hash": ContentHash::of_bytes(b"{\"exact\":true}\n").to_hex(),
                "expected_after_hash": ContentHash::of_bytes(patched.as_bytes()).to_hex()
            }
        }))
        .unwrap();
        let patch_content = ContentHash::of_bytes(&patch_payload);
        let patch_artifact_file = InterfaceArtifactFile::write(
            root.path().join("patch-inbound"),
            TOOL_CALL_LABEL,
            &patch_payload,
        )
        .unwrap();
        let patch_envelope = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new(format!("tool.call:{}", patch_content.to_hex())),
                content: patch_content,
            },
            artifact_file: patch_artifact_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(InvocationMetadata::new(GrantProjection {
                tools: vec![patch_descriptor.id.as_str().to_string()],
                worker_environment_kinds: vec!["system_room".to_string()],
                ..GrantProjection::default()
            })),
        };
        let patch_call = decode_invocation(&patch_descriptor, &patch_envelope).unwrap();
        ToolArgumentValidator::compile(&patch_descriptor.args_schema)
            .unwrap()
            .validate(&patch_call.arguments)
            .unwrap();
        let patch_room_kinds = patch_descriptor
            .supported_worker_environment_kinds
            .iter()
            .map(|kind| kind.as_str().to_string())
            .collect::<Vec<_>>();
        let patch_body = file_write::execute_file_write(
            &patch_envelope,
            &patch_call.arguments,
            &patch_room_kinds,
            &root_slots,
            &root.path().join("effects"),
            8 * 1024 * 1024,
        )
        .unwrap();
        assert_eq!(patch_body["result"], "files_written");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), patched);

        let mut pinned_grant = GrantProjection {
            tools: vec![descriptor.id.as_str().to_string()],
            worker_environment_kinds: vec!["system_room".to_string()],
            ..GrantProjection::default()
        };
        pinned_grant.formation_pin.inputs.insert(
            DescriptorName::new("test_root/writes/result.json"),
            DescriptorHash::of_canonical_json(&serde_json::from_str(patched).unwrap()),
        );
        envelope.metadata = Some(InvocationMetadata::new(pinned_grant));
        let narrowed = "{\"config\":{},\"policy_narrowing\":{\"allowed_tools\":[]}}\n";
        let arguments = serde_json::json!({
            "path": target,
            "content": narrowed,
            "expected_before_hash": ContentHash::of_bytes(patched.as_bytes()).to_hex()
        });
        let payload = serde_json::to_vec(&arguments).unwrap();
        envelope.artifact.content = ContentHash::of_bytes(&payload);
        envelope.artifact.id =
            ArtifactId::new(format!("tool.call:{}", envelope.artifact.content.to_hex()));
        let apply = |arguments: &serde_json::Value| {
            file_write::execute_file_write(
                &envelope,
                arguments,
                &room_kinds,
                &root_slots,
                &root.path().join("effects"),
            8 * 1024 * 1024,
            )
        };
        let receipt = apply(&arguments).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), narrowed);
        let evidence = root.path().join("effects/blobs");
        assert_eq!(
            std::fs::read(evidence.join(receipt["writes"][0]["before_hash"].as_str().unwrap()))
                .unwrap(),
            patched.as_bytes()
        );
        assert_eq!(
            std::fs::read(evidence.join(receipt["writes"][0]["after_hash"].as_str().unwrap()))
                .unwrap(),
            narrowed.as_bytes()
        );
        assert_eq!(apply(&arguments).unwrap(), receipt);
        let later = octopus_file_storage::publication::WriteGuard::acquire(&target, false).unwrap()
            .publish("client-after-native", Some(&ContentHash::of_bytes(narrowed.as_bytes()).to_hex()), b"later client edit",
                octopus_file_storage::publication::DigestAlgorithm::Blake3).unwrap();
        assert_eq!(later.outcome, octopus_file_storage::publication::PublicationOutcome::Applied);
        assert_eq!(apply(&arguments).unwrap(), receipt);
        assert_eq!(std::fs::read(&target).unwrap(), b"later client edit");
        assert_eq!(file_write::execute_file_write(
            &patch_envelope, &patch_call.arguments, &patch_room_kinds, &root_slots,
            &root.path().join("effects"), 8 * 1024 * 1024,
        ).unwrap(), patch_body);
        assert_eq!(std::fs::read(&target).unwrap(), b"later client edit");
        let outside = tempfile::tempdir().unwrap();
        let denied =
            serde_json::json!({"path": outside.path().join("ungranted.json"), "content": "denied"});
        assert!(
            apply(&denied)
                .unwrap_err()
                .contains("outside this room's permission")
        );
        assert!(!outside.path().join("ungranted.json").exists());
        std::fs::write(
            evidence.join(receipt["writes"][0]["before_hash"].as_str().unwrap()),
            b"corrupt",
        )
        .unwrap();
        assert!(
            apply(&arguments)
                .unwrap_err()
                .contains("differs from its pinned hash")
        );
    }

    #[test]
    fn exact_send_clone_keeps_the_single_operation_without_an_alias() {
        let mut descriptor = load_tool_descriptor_from_dir(
            &repository_root().join("registry/tool"),
            "artifact.send",
        )
        .unwrap()
        .unwrap();
        descriptor.id = ToolDescriptorId::new("artifact.custom_send");
        assert!(descriptor.handler_config.is_null());

        let root = tempfile::tempdir().unwrap();
        let payload = serde_json::to_vec(&serde_json::json!({
            "tool": descriptor.id.as_str(),
            "arguments": {
                "interface": "system_worker_planner",
                "label": "message",
                "payload": {"text": "exact clone"}
            }
        }))
        .unwrap();
        let content = ContentHash::of_bytes(&payload);
        let artifact_file =
            InterfaceArtifactFile::write(root.path().join("inbound"), TOOL_CALL_LABEL, &payload)
                .unwrap();
        let envelope = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
                content,
            },
            artifact_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(InvocationMetadata::new(GrantProjection {
                tools: vec![descriptor.id.as_str().to_string()],
                worker_environment_kinds: vec!["system_room".to_string()],
                ..GrantProjection::default()
            })),
        };
        let call = decode_invocation(&descriptor, &envelope).unwrap();
        assert_eq!(call.tool, "artifact.custom_send");
        let send = artifact_send::decode_artifact_send_call(&envelope, &call.arguments).unwrap();
        assert_eq!(send.to_interface.as_str(), "system_worker_planner");
        assert!(matches!(
            send.body,
            artifact_send::ArtifactSendBody::Payload { .. }
        ));
    }
}
