use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use octopus_descriptors::{
    ArtifactId, CheckpointId, EeId, InterfaceId, RunId, ToolDescriptorId, ToolId,
};
use octopus_interfaces::{
    GrantProjection, InterfaceArtifactFile, InterfaceInvocationEnvelope, InterfaceToolAffordance,
    InvocationMetadata, RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactDelivery, ArtifactRef, ContentHash, MovementLog};
use octopus_tool_door_runtime::{
    DeliveryVerdict, SystemInterfaceEmissionArtifact, ToolProcessEnvironment, ToolProcessError,
    ToolProcessInvocation,
};
use octopus_tool_registry::{ToolDescriptor, load_tool_descriptor_from_dir};
use octopus_worker_env::{
    WorkerEnvKindRegistry, WorkerEnvKindSpec, WorkerEnvironmentFormation,
    WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId,
};
use serde_json::{Value, json};

use super::{CodeModeRoom, EmissionSink, code_mode_operation};

const DOOR: &str = "system_code_mode";
const WORKER: &str = "system_worker_coder_max";
const KIND: &str = "code_mode";
const NESTED_TOOL: &str = "process.list";
const NESTED_INTERFACE: &str = "system_exec_env_test";
const NESTED_CHECKPOINT: &str = "cp_code_mode_to_process";

#[test]
fn persistent_prelude_requires_the_exact_admitted_binding_and_payload() {
    let temporary = tempfile::tempdir().unwrap();
    let payload = serde_json::to_vec(&json!({
        "schema_version":1,"module_source":"export const value = 'λ你好';","exports":["value"]
    }))
    .unwrap();
    let file = InterfaceArtifactFile::write(
        temporary.path().join("prelude"),
        "code-mode.prelude",
        &payload,
    )
    .unwrap();
    let content = ContentHash::of_bytes(&std::fs::read(file.manifest_path()).unwrap());
    let artifact = ArtifactRef {
        id: ArtifactId::new(format!("code-mode.prelude:{}", content.to_hex())),
        content,
    };
    assert_ne!(artifact.content, ContentHash::of_bytes(&payload));
    let config: super::config::CodeModeConfig = serde_json::from_value(json!({
        "schema_version":1,"context":{"mode":"persistent","prelude_artifact":artifact}
    }))
    .unwrap();
    let mut input = envelope(
        temporary.path(),
        1,
        "code_mode.run",
        json!({"script":"text(prelude.value)"}),
    );
    assert!(config.resolve_context(&input).is_err());
    input
        .metadata
        .as_mut()
        .unwrap()
        .artifact_binding_refs
        .push(artifact.clone());
    assert!(config.resolve_context(&input).is_err());
    input
        .artifact_binding_payloads
        .push(octopus_interfaces::ArtifactBindingPayload::new(
            artifact.clone(),
            payload,
        ));
    let resolved = config.resolve_context(&input).unwrap();
    assert!(
        matches!(resolved, super::config::ResolvedContext::Persistent { engine, .. }
        if engine.module_source == "export const value = 'λ你好';" && engine.exports == ["value"])
    );
    let declared = json!({"id":"registry_prelude","run":"run_code_mode_test","owner_interface":DOOR,
        "schema_version":1,"module_source":"export const value = 1;","exports":["value"]});
    for field in [None, Some("id"), Some("run"), Some("owner_interface")] {
        let mut body = declared.clone();
        if let Some(field) = field { body.as_object_mut().unwrap().remove(field); }
        let bytes = octopus_descriptors::canonical_bytes(&body);
        let reference = ArtifactRef { id:ArtifactId::new("registry_prelude"), content:ContentHash::of_bytes(&bytes) };
        let declared_config:super::config::CodeModeConfig = serde_json::from_value(json!({
            "schema_version":1,"context":{"mode":"persistent","prelude_artifact":reference}
        })).unwrap();
        let mut declared_input = input.clone();
        declared_input.metadata.as_mut().unwrap().artifact_binding_refs = vec![reference.clone()];
        declared_input.artifact_binding_payloads = vec![octopus_interfaces::ArtifactBindingPayload::new(reference,bytes)];
        assert_eq!(declared_config.resolve_context(&declared_input).is_ok(),field.is_none());
    }
    input
        .metadata
        .as_mut()
        .unwrap()
        .artifact_binding_refs
        .push(artifact.clone());
    assert!(config.resolve_context(&input).is_err());
    input.metadata.as_mut().unwrap().artifact_binding_refs.pop();
    input.artifact_binding_payloads[0].artifact.content = ContentHash::of_bytes(b"other-manifest");
    assert!(config.resolve_context(&input).is_err());
    input.artifact_binding_payloads[0].artifact = artifact;
    input.artifact_binding_payloads[0] = octopus_interfaces::ArtifactBindingPayload::new(
        input.artifact_binding_payloads[0].artifact.clone(),
        vec![0xff],
    );
    assert!(config.resolve_context(&input).is_err());
    assert!(
        serde_json::from_value::<super::config::CodeModeConfig>(json!({
            "schema_version":1,"context":{"mode":"persistent"}
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<super::config::CodeModeConfig>(json!({
            "schema_version":1,"context":{"mode":"unknown"}
        }))
        .is_err()
    );
}

fn registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

fn tool_ids() -> Vec<String> {
    [
        "code_mode.run",
        "code_mode.resume",
        "code_mode.wait",
        "code_mode.cancel",
        NESTED_TOOL,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn raw_descriptor(id: &str) -> ToolDescriptor {
    if id == "file.write.provider" {
        let mut descriptor = raw_descriptor("file.write");
        descriptor.id = ToolDescriptorId::new(id);
        descriptor.interface = InterfaceId::new("system_provider_file_test");
        return descriptor;
    }
    load_tool_descriptor_from_dir(&registry_root().join("tool"), id)
        .unwrap()
        .unwrap()
}

fn effective_descriptor(id: &str) -> ToolDescriptor {
    let mut descriptor = raw_descriptor(id);
    descriptor.interface = InterfaceId::new(DOOR);
    descriptor
}

fn write_interface(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(
        path.join("interface.json"),
        serde_json::to_vec_pretty(&json!({
            "id": DOOR,
            "interface_type": "system",
            "role": "system",
            "direction": "bidirectional",
            "source_names": ["code_mode"]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        path.join("code_mode.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "max_source_bytes": 1048576,
            "max_output_bytes": 8388608,
            "max_cells": 4,
            "default_yield_time_ms": 1000,
            "max_yield_time_ms": 5000,
            "routes": [{
                "tool": NESTED_TOOL,
                "interface": NESTED_INTERFACE,
                "checkpoint": NESTED_CHECKPOINT
            }]
        }))
        .unwrap(),
    )
    .unwrap();
}

fn formation(door_instance_dir: &Path) -> (WorkerEnvironmentFormation, WorkerEnvKindSpec) {
    formation_with_tools(door_instance_dir, tool_ids())
}

fn formation_with_tools(
    door_instance_dir: &Path,
    tools: Vec<String>,
) -> (WorkerEnvironmentFormation, WorkerEnvKindSpec) {
    let registry =
        WorkerEnvKindRegistry::load_from_dir(&registry_root().join("worker_environment_kind"))
            .unwrap();
    let hashes = tools
        .iter()
        .map(|tool| {
            let value = serde_json::to_value(raw_descriptor(tool)).unwrap();
            let hash = blake3::hash(&octopus_descriptors::canonical_bytes(&value))
                .to_hex()
                .to_string();
            (tool.clone(), hash)
        })
        .collect::<BTreeMap<_, _>>();
    let process_bins = tools
        .iter()
        .map(|tool| (tool.clone(), raw_descriptor(tool).process.bin))
        .collect::<BTreeMap<_, _>>();
    let kind_id = WorkerEnvironmentKindId::new(KIND);
    let kinds = vec![kind_id.clone()];
    let door = InterfaceId::new(DOOR);
    let worker = InterfaceId::new(WORKER);
    let formed = registry
        .form(&WorkerEnvironmentFormationRequest {
            door_additional_root_bindings: &Default::default(),
            door_interface: &door,
            door_descriptor_hash: "code-mode-test-door-hash",
            door_driver_ref: "code_mode_env",
            tool: "code_mode.run",
            tool_descriptor_hashes: &hashes,
            tool_process_bins: &process_bins,
            tool_worker_environment_kinds: &kinds,
            worker: &worker,
            ee: "ee_code_mode_test",
            run: "run_code_mode_test",
            granted_worker_environment_kinds: &kinds,
            granted_tools: &tools,
            door_root_bindings: &BTreeMap::new(),
            door_instance_dir,
            ee_root_slot_allowance: None,
        })
        .unwrap();
    let (kind, _) = registry.get(&kind_id).unwrap();
    (formed, (*kind).clone())
}

fn envelope(
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
) -> InterfaceInvocationEnvelope {
    envelope_with_tools(root, sequence, tool, arguments, tool_ids())
}

fn envelope_with_tools(
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    tools: Vec<String>,
) -> InterfaceInvocationEnvelope {
    let payload = serde_json::to_vec(&json!({
        "tool": tool,
        "arguments": arguments,
        "correlation_id": format!("outer-{sequence}")
    }))
    .unwrap();
    let artifact_file = InterfaceArtifactFile::write(
        root.join(format!("inbound-{sequence}")),
        "tool.call",
        &payload,
    )
    .unwrap();
    let content = ContentHash::of_bytes(&payload);
    let metadata = InvocationMetadata::new(GrantProjection {
        tools,
        worker_environment_kinds: vec![KIND.to_string()],
        interface_affordance_names: vec![NESTED_TOOL.to_string()],
        interface_affordance_specs: vec![InterfaceToolAffordance {
            name: ToolId::new(NESTED_TOOL),
            description: "List processes in the persistent process room.".to_string(),
            args_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }],
        ..GrantProjection::default()
    })
    .with_runtime(RuntimeInvocationMetadata {
        run: RunId::new("run_code_mode_test"),
        ee: EeId::new("ee_code_mode_test"),
        worker: InterfaceId::new(WORKER),
        checkpoint: CheckpointId::new(format!("cp-{sequence}")),
        interface: InterfaceId::new(DOOR),
        epoch: 0,
        run_record_hash: None,
    });
    InterfaceInvocationEnvelope {
        artifact: ArtifactRef {
            id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
            content,
        },
        artifact_file,
        artifact_binding_payloads: Vec::new(),
        metadata: Some(metadata),
    }
}

async fn invoke(
    room: &CodeModeRoom,
    sink: &FakeSink,
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    formation: &WorkerEnvironmentFormation,
    kind: &WorkerEnvKindSpec,
) -> Value {
    let invocation = ToolProcessInvocation {
        request_id: sequence,
        descriptor: effective_descriptor(tool),
        envelope: envelope(root, sequence, tool, arguments),
        source_movement_seq: sequence,
        worker_environment: Some(formation.clone()),
        worker_environment_kind: Some(kind.clone()),
    };
    let outputs = room
        .receive_with_sink(invocation, sink)
        .await
        .unwrap_or_else(|error| match error {
            ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                panic!("code mode room invocation failed: {error}")
            }
        });
    serde_json::from_slice(&outputs[0].read_payload_bytes().unwrap()).unwrap()
}

#[derive(Clone)]
struct CapturedEmission {
    to_interface: String,
    payload: Value,
}

#[derive(Default)]
struct FakeSink {
    emissions: Mutex<Vec<CapturedEmission>>,
    live_state: Mutex<Option<Value>>,
    fail_live_state: AtomicBool,
}

impl FakeSink {
    fn take(&self) -> Vec<CapturedEmission> {
        std::mem::take(&mut *self.emissions.lock().unwrap())
    }

    fn fail_next_live_state(&self) {
        self.fail_live_state.store(true, Ordering::Release);
    }
}

impl EmissionSink for FakeSink {
    fn arrive<'a>(
        &'a self,
        _artifact: &'a InterfaceArtifactFile,
    ) -> Pin<Box<dyn Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>> {
        Box::pin(async { Err("legacy fake sink does not exercise admitted continuation".into()) })
    }

    fn hand<'a>(
        &'a self,
        artifact: &'a SystemInterfaceEmissionArtifact,
        to_interface: &'a InterfaceId,
    ) -> Pin<Box<dyn Future<Output = Result<DeliveryVerdict, String>> + Send + 'a>> {
        Box::pin(async move {
            let payload = match artifact {
                SystemInterfaceEmissionArtifact::Materialized(artifact) => {
                    serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap()
                }
                SystemInterfaceEmissionArtifact::Reference(artifact) => {
                    panic!("unexpected nested tool artifact reference: {}", artifact.id)
                }
            };
            self.emissions.lock().unwrap().push(CapturedEmission {
                to_interface: to_interface.as_str().to_string(),
                payload,
            });
            Ok(DeliveryVerdict::Admitted { seq: 77 })
        })
    }
}

impl octopus_tool_door_runtime::WorkerEnvironmentLifecycleRecorder for FakeSink {
    fn record<'a>(
        &'a self,
        _formation: &'a WorkerEnvironmentFormation,
        _lifecycle: octopus_worker_env::WorkerEnvironmentLifecycle,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

impl octopus_tool_door_runtime::WorkerEnvironmentLiveStateRecorder for FakeSink {
    fn record_live_state<'a>(
        &'a self,
        _formation: &'a WorkerEnvironmentFormation,
        state: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if self.fail_live_state.swap(false, Ordering::AcqRel) {
                return Err("injected live-state write failure".to_string());
            }
            *self.live_state.lock().unwrap() = Some(state.clone());
            Ok(())
        })
    }
}

#[tokio::test]
async fn formed_code_mode_room_yields_nested_artifact_and_resumes_same_v8_cell() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    write_interface(&interface_dir);
    let (formation, kind) = formation(&interface_dir);
    let room = CodeModeRoom::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: root.path().join("var"),
        durable_var_dir: None,
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let sink = FakeSink::default();

    let started = invoke(
        &room,
        &sink,
        root.path(),
        1,
        "code_mode.run",
        json!({
            "script": "const result = await tools.process_list({}); text(result.processes[0]);",
            "yield_time_ms": 1000
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(started["ok"], true, "{started}");
    assert_eq!(started["body"]["state"], "yielded", "{started}");
    let cell_id = started["body"]["cell_id"].as_str().unwrap().to_string();
    let emissions = sink.take();
    assert_eq!(emissions.len(), 1);
    let emission = &emissions[0];
    assert_eq!(emission.to_interface, NESTED_INTERFACE);
    assert_eq!(emission.payload["tool"], NESTED_TOOL);
    let nested_call_id = emission.payload["correlation_id"]
        .as_str()
        .unwrap()
        .to_string();

    let completed = invoke(
        &room,
        &sink,
        root.path(),
        2,
        "code_mode.resume",
        json!({
            "cell_id": cell_id,
            "tool_call_id": nested_call_id,
            "result": { "processes": ["alpha"] },
            "yield_time_ms": 1000
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(completed["ok"], true, "{completed}");
    assert_eq!(completed["body"]["state"], "completed", "{completed}");
    assert_eq!(completed["body"]["output"][0]["text"], "alpha");
    assert!(sink.take().is_empty());

    let live = sink.live_state.lock().unwrap().clone().unwrap();
    assert_eq!(live["status"], "ready");
    assert_eq!(live["active_cells"], json!([]));
    assert_eq!(live["pending_tool_calls"], json!([]));
}

#[tokio::test]
async fn completed_code_mode_effect_returns_artifact_when_live_state_write_fails() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    write_interface(&interface_dir);
    let (formation, kind) = formation(&interface_dir);
    let room = CodeModeRoom::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: root.path().join("var"),
        durable_var_dir: None,
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let sink = FakeSink::default();
    sink.fail_next_live_state();

    let completed = invoke(
        &room,
        &sink,
        root.path(),
        80,
        "code_mode.run",
        json!({
            "script": "text('effect-completed');",
            "yield_time_ms": 1000
        }),
        &formation,
        &kind,
    )
    .await;
    assert_eq!(completed["ok"], false, "{completed}");
    assert_eq!(
        completed["error"]["details"]["operation_body"]["state"],
        "completed"
    );
    assert_eq!(
        completed["error"]["details"]["operation_body"]["output"][0]["text"],
        "effect-completed"
    );
    assert_eq!(
        completed["error"]["details"]["evidence_write_errors"][0]["stage"],
        "live_state"
    );
}

#[tokio::test]
async fn exact_clone_id_executes_copied_run_operation_and_stays_exact_in_evidence() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("interface");
    write_interface(&interface_dir);
    let (mut formation, kind) = formation(&interface_dir);
    let exact_tool = "code_mode.run.run_42";
    let mut clone = effective_descriptor("code_mode.run");
    clone.id = ToolDescriptorId::new(exact_tool);
    formation.granted_tools.push(exact_tool.to_string());
    formation.granted_tools.sort();
    formation.tool_descriptor_hashes.insert(
        exact_tool.to_string(),
        blake3::hash(&octopus_descriptors::canonical_bytes(
            &serde_json::to_value(&clone).unwrap(),
        ))
        .to_hex()
        .to_string(),
    );
    let mut tools = tool_ids();
    tools.push(exact_tool.to_string());
    tools.sort();
    let room = CodeModeRoom::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: root.path().join("var"),
        durable_var_dir: None,
        host_io_limits: octopus_tool_door_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    });
    let sink = FakeSink::default();
    let invocation = ToolProcessInvocation {
        request_id: 42,
        descriptor: clone,
        envelope: envelope_with_tools(
            root.path(),
            42,
            exact_tool,
            json!({"script": "text('clone-operation');", "yield_time_ms": 1000}),
            tools,
        ),
        source_movement_seq: 42,
        worker_environment: Some(formation),
        worker_environment_kind: Some(kind),
    };
    let outputs = room
        .receive_with_sink(invocation, &sink)
        .await
        .unwrap_or_else(|error| match error {
            ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
                panic!("code mode clone invocation failed: {error}")
            }
        });
    let response: Value =
        serde_json::from_slice(&outputs[0].read_payload_bytes().unwrap()).unwrap();
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["tool"], exact_tool);
    assert_eq!(response["label"], format!("{exact_tool}.result"));
    assert_eq!(response["body"]["state"], "completed");
    assert_eq!(response["body"]["output"][0]["text"], "clone-operation");
    for handler_config in [
        json!({}),
        json!({"operation": 1}),
        json!({"operation": "unknown"}),
    ] {
        let error = code_mode_operation(&handler_config, exact_tool).unwrap_err();
        let message = match error {
            ToolProcessError::Capability(message) | ToolProcessError::Unavailable(message) => {
                message
            }
        };
        assert!(message.contains(exact_tool), "{message}");
        assert!(message.contains("handler_config"), "{message}");
    }
}

fn persistent_input(
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    prelude: &ArtifactRef,
    prelude_bytes: &[u8],
) -> InterfaceInvocationEnvelope {
    let mut envelope = envelope(root, sequence, tool, arguments);
    envelope.artifact = super::continuation::artifact_ref(&envelope.artifact_file).unwrap();
    let metadata = envelope.metadata.as_mut().unwrap();
    metadata.grant.interfaces = vec![
        DOOR.into(),
        NESTED_INTERFACE.into(),
        "world_operator_test".into(),
    ];
    metadata.runtime.as_mut().unwrap().run_record_hash = Some(format!("authored-run-{sequence}"));
    metadata.artifact_binding_refs = vec![prelude.clone()];
    envelope.artifact_binding_payloads = vec![octopus_interfaces::ArtifactBindingPayload::new(
        prelude.clone(),
        prelude_bytes.to_vec(),
    )];
    envelope
}

fn persistent_result(
    root: &Path,
    sequence: u64,
    source_sequence: u64,
    emitted: &Value,
    prelude: &ArtifactRef,
    prelude_bytes: &[u8],
) -> InterfaceInvocationEnvelope {
    let result = octopus_tool_handlers::ToolResultArtifact::success(
        NESTED_TOOL,
        Some(emitted["correlation_id"].as_str().unwrap().into()),
        json!({"value": 41}),
    );
    let file = InterfaceArtifactFile::write(
        root.join(format!("nested-result-{sequence}")),
        &result.label,
        &serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
    let mut envelope = persistent_input(
        root,
        sequence,
        "code_mode.resume",
        json!({}),
        prelude,
        prelude_bytes,
    );
    envelope.artifact = super::continuation::artifact_ref(&file).unwrap();
    envelope.artifact_file = file;
    let metadata = envelope.metadata.as_mut().unwrap();
    metadata.runtime.as_mut().unwrap().run_record_hash =
        Some(format!("authored-run-{source_sequence}"));
    metadata.runtime.as_mut().unwrap().checkpoint = CheckpointId::new("cp_process_to_code_mode");
    metadata.outbound_interface_bindings = vec![InterfaceId::new(DOOR)];
    metadata.checkpoint_binding_policy = Some(
        serde_json::from_value(json!({
            "inbound":{"interface":NESTED_INTERFACE,"config":{"artifact_policy":{"mode":"all"}}},
            "outbound":[{"interface":DOOR,"config":{"artifact_policy":{"mode":"all"}}}]
        }))
        .unwrap(),
    );
    metadata.checkpoint_admission = Some(octopus_interfaces::CheckpointAdmissionMetadata {
        artifact_label: result.label,
        required_artifact_labels: vec![],
        complete: true,
    });
    envelope
}

async fn owner_frontier(
    room: &CodeModeRoom,
    input: (
        u64,
        octopus_tool_handlers::ToolCallArtifact,
        InterfaceInvocationEnvelope,
    ),
    formation: &WorkerEnvironmentFormation,
    kind: &WorkerEnvKindSpec,
    mut on_artifact: impl FnMut(&InterfaceArtifactFile) -> DeliveryVerdict,
) -> Result<Value, String> {
    let (seq, call, envelope) = input;
    let (io, mut peer) = octopus_tool_door_runtime::owner_contract_tests::io_pair(seq + 1);
    let invocation = ToolProcessInvocation {
        request_id: seq + 1,
        descriptor: effective_descriptor(&call.tool),
        envelope,
        source_movement_seq: seq,
        worker_environment: Some(formation.clone()),
        worker_environment_kind: Some(kind.clone()),
    };
    let execution = octopus_tool_door_runtime::owner_contract_tests::execute_formed(
        room,
        &room.environment,
        invocation,
        io,
    );
    tokio::pin!(execution);
    let outputs = loop {
        tokio::select! {
            output = &mut execution => break output?,
            output = peer.next_artifact() => {
                let (request_id, event_id, artifact) = output.unwrap();
                assert_eq!(request_id, seq + 1);
                let verdict = on_artifact(&artifact);
                peer.acknowledge(request_id, event_id, verdict).await.unwrap();
            }
        }
    };
    serde_json::from_slice(&outputs[0].read_payload_bytes().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

async fn run_local_fixture(fixture: Pin<Box<dyn Future<Output = ()>>>) {
    tokio::task::LocalSet::new().run_until(async move {
        tokio::task::spawn_local(fixture).await.unwrap();
    }).await;
}

#[tokio::test]
async fn persistent_frontiers_use_admitted_owner_results_and_preserve_wait_output() {
    run_local_fixture(persistent_frontiers_fixture()).await;
}

fn persistent_frontiers_fixture() -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(async move {
    use octopus_tool_door_runtime::{
        ToolDoorArtifactDecodePurpose as Purpose, owner_contract_tests::DoorFixture,
    };
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let interface_dir = root.join("interface");
    write_interface(&interface_dir);
    let prelude_bytes = serde_json::to_vec(&json!({"schema_version":1,"module_source":"export const seed = {value: 1};","exports":["seed"]})).unwrap();
    let prelude_file =
        InterfaceArtifactFile::write(root.join("prelude"), "code-mode.prelude", &prelude_bytes)
            .unwrap();
    let prelude = super::continuation::artifact_ref(&prelude_file).unwrap();
    let config_path = interface_dir.join("code_mode.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["context"] = json!({"mode":"persistent","prelude_artifact":prelude});
    config["continuation"] = json!({"mode":"admitted_artifact","resume_tool":"code_mode.resume","notification_interface":"world_operator_test"});
    config["routes"][0]["result_checkpoint"] = json!("cp_process_to_code_mode");
    config["default_yield_time_ms"] = json!(50);
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let (formation, kind) = formation(&interface_dir);
    let limits = octopus_tool_door_runtime::InterfaceHostIoLimits {
        max_control_frame_bytes: 1024 * 1024,
        max_stderr_bytes: 256 * 1024,
        max_manifest_bytes: 1024 * 1024,
        max_payload_bytes: 16 * 1024 * 1024,
    };
    let mut door =
        DoorFixture::new(&root, InterfaceId::new(DOOR), interface_dir.clone(), limits).unwrap();
    assert_eq!(door.cache(&prelude_file).unwrap(), prelude);
    let durable = door.durable_dir(&formation).unwrap();
    let make_room = || {
        CodeModeRoom::new(ToolProcessEnvironment {
            interface: InterfaceId::new(DOOR),
            interface_dir: interface_dir.clone(),
            descriptor_snapshot: None,
            var_dir: root.join("var"),
            durable_var_dir: Some(durable.clone()),
            host_io_limits: limits,
            private_reads: BTreeMap::new(),
            root_slots: BTreeMap::new(),
        })
    };
    let room = make_room();
    let decoder = super::decode_code_mode_door_artifact;
    let source = persistent_input(
        &root,
        1,
        "code_mode.run",
        json!({"script":"let retained = {n: prelude.seed.value}; let answered = await tools.process_list({}); retained.n += answered.body.value; notify('accepted exact result'); text(retained.n);","yield_time_ms":50}),
        &prelude,
        &prelude_bytes,
    );
    let input = door
        .admit(&source, InterfaceId::new("world_provider_test"), decoder)
        .unwrap();
    let mut queued = None;
    let mut emitted = None;
    let started = owner_frontier(&room, input, &formation, &kind, |artifact| {
        let call: Value = serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(call["tool"], NESTED_TOOL);
        let mut outbound = source.metadata().unwrap().clone();
        outbound.runtime.as_mut().unwrap().checkpoint = CheckpointId::new(NESTED_CHECKPOINT);
        let emitted_seq = door
            .record_delivery(
                artifact,
                InterfaceId::new(NESTED_INTERFACE),
                outbound,
                octopus_ledger::MovementKind::InterfaceOutbound,
            )
            .unwrap();
        let result = persistent_result(&root, 2, 1, &call, &prelude, &prelude_bytes);
        queued = Some(
            door.admit(&result, InterfaceId::new(NESTED_INTERFACE), decoder)
                .unwrap(),
        );
        emitted = Some(call);
        DeliveryVerdict::Admitted { seq: emitted_seq }
    })
    .await
    .unwrap();
    assert_eq!(started["body"]["state"], "yielded", "{started}");
    let cell_id = started["body"]["cell_id"].as_str().unwrap().to_string();
    let second = persistent_input(
        &root,
        3,
        "code_mode.run",
        json!({"script":"text('must not run')","yield_time_ms":50}),
        &prelude,
        &prelude_bytes,
    );
    let input = door
        .admit(&second, InterfaceId::new("world_provider_test"), decoder)
        .unwrap();
    let refused = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        owner_frontier(&room, input, &formation, &kind, |_| {
            panic!("busy Run emitted an effect")
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(refused["ok"], false);
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already has an executing cell")
    );
    let queued = queued.unwrap();
    let original_resume_id = queued.1.correlation_id.clone();
    assert_ne!(
        original_resume_id.as_deref(),
        emitted.as_ref().unwrap()["correlation_id"].as_str()
    );
    assert!(
        door.admit(&queued.2, InterfaceId::new("foreign_source"), decoder)
            .unwrap_err()
            .contains("emitted by another interface")
    );
    let mut foreign = queued.2.clone();
    foreign
        .metadata
        .as_mut()
        .unwrap()
        .runtime
        .as_mut()
        .unwrap()
        .checkpoint = CheckpointId::new("another-checkpoint");
    assert!(
        door.admit(&foreign, InterfaceId::new(NESTED_INTERFACE), decoder)
            .is_err()
    );
    let mut changed_generation = queued.2.clone();
    changed_generation
        .metadata
        .as_mut()
        .unwrap()
        .runtime
        .as_mut()
        .unwrap()
        .run_record_hash = Some("neighbor-topology-changed".into());
    assert!(
        door.admit(
            &changed_generation,
            InterfaceId::new(NESTED_INTERFACE),
            decoder
        )
        .is_err()
    );
    let config_bytes = std::fs::read(&config_path).unwrap();
    config["max_cells"] = json!(3);
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(
        door.decode_record(queued.0, &queued.2, decoder, Purpose::Dispatch)
            .is_err()
    );
    let identity = door
        .decode_record(queued.0, &queued.2, decoder, Purpose::HistoricalIdentity)
        .unwrap()
        .unwrap();
    assert_eq!(identity.correlation_id, original_resume_id);
    std::fs::write(&config_path, config_bytes).unwrap();
    let history_before: BTreeMap<_, _> = std::fs::read_dir(&durable)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.clone(), std::fs::read(path).unwrap())
        })
        .collect();
    let historical = door
        .decode_record(queued.0, &queued.2, decoder, Purpose::HistoricalIdentity)
        .unwrap()
        .unwrap();
    for (path, bytes) in &history_before {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
    assert_eq!(
        std::fs::read_dir(&durable).unwrap().count(),
        history_before.len()
    );
    let mut wrong_sequence = queued.clone();
    wrong_sequence.0 += 1;
    let wrong = owner_frontier(&room, wrong_sequence, &formation, &kind, |_| {
        panic!("changed result source sequence emitted an effect")
    })
    .await
    .unwrap_err();
    assert!(
        wrong.contains("original owner-admitted artifact"),
        "{wrong}"
    );
    let mut notifications = 0;
    let completed = owner_frontier(&room, queued.clone(), &formation, &kind, |artifact| {
        assert_eq!(
            artifact.read_manifest().unwrap().label,
            "code_mode.notification"
        );
        let notification: Value =
            serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(notification["text"], "accepted exact result");
        notifications += 1;
        let admitted_seq = door
            .record_delivery(
                artifact,
                InterfaceId::new("world_operator_test"),
                queued.2.metadata().unwrap().clone(),
                octopus_ledger::MovementKind::InterfaceOutbound,
            )
            .unwrap();
        DeliveryVerdict::Admitted { seq: admitted_seq }
    })
    .await
    .unwrap();
    assert_eq!(notifications, 1);
    assert_eq!(completed["tool"], "code_mode.resume");
    assert_eq!(
        completed["correlation_id"],
        json!(historical.correlation_id)
    );
    assert_eq!(completed["body"]["output_retained"], true, "{completed}");
    assert!(completed["body"].get("output").is_none());
    assert!(
        door.decode_record(queued.0, &queued.2, decoder, Purpose::Dispatch)
            .is_err()
    );
    assert_eq!(
        door.decode_record(queued.0, &queued.2, decoder, Purpose::HistoricalIdentity)
            .unwrap()
            .unwrap()
            .correlation_id,
        original_resume_id
    );
    let waiting = persistent_input(
        &root,
        4,
        "code_mode.wait",
        json!({"cell_id":cell_id,"yield_time_ms":50}),
        &prelude,
        &prelude_bytes,
    );
    let input = door
        .admit(&waiting, InterfaceId::new("world_provider_test"), decoder)
        .unwrap();
    let visible = owner_frontier(&room, input, &formation, &kind, |_| {
        panic!("buffered Wait emitted an effect")
    })
    .await
    .unwrap();
    assert_eq!(visible["body"]["state"], "completed", "{visible}");
    assert_eq!(visible["body"]["output"][0]["text"], "42");
    let input = door
        .admit(&waiting, InterfaceId::new("world_provider_test"), decoder)
        .unwrap();
    let consumed = owner_frontier(&room, input, &formation, &kind, |_| {
        panic!("consumed Wait emitted an effect")
    })
    .await
    .unwrap();
    assert_eq!(consumed["ok"], false);
    let rejection = persistent_input(
        &root,
        5,
        "code_mode.run",
        json!({"script":"try { await tools.process_list({}); } catch (error) { text(String(error)); }","yield_time_ms":50}),
        &prelude,
        &prelude_bytes,
    );
    let input = door
        .admit(&rejection, InterfaceId::new("world_provider_test"), decoder)
        .unwrap();
    let mut refused_return = None;
    let rejected = owner_frontier(&room, input, &formation, &kind, |artifact| {
        let emitted: Value =
            serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
        refused_return = Some(persistent_result(
            &root,
            6,
            5,
            &emitted,
            &prelude,
            &prelude_bytes,
        ));
        DeliveryVerdict::Refused {
            reason: octopus_tool_door_runtime::RefusedReason::Policy,
            detail: "fixture exact edge refused".into(),
        }
    })
    .await
    .unwrap();
    assert_eq!(
        rejected["body"]["emissions"][0]["delivery"]["refused"]["reason"],
        "policy"
    );
    assert!(
        door.admit(
            &refused_return.unwrap(),
            InterfaceId::new(NESTED_INTERFACE),
            decoder
        )
        .unwrap_err()
        .contains("refused or indeterminate")
    );
    let drain = persistent_input(
        &root,
        7,
        "code_mode.wait",
        json!({"cell_id":rejected["body"]["cell_id"],"yield_time_ms":100}),
        &prelude,
        &prelude_bytes,
    );
    let input = door
        .admit(&drain, InterfaceId::new("world_provider_test"), decoder)
        .unwrap();
    let caught = owner_frontier(&room, input, &formation, &kind, |_| {
        panic!("refused emission retried an effect")
    })
    .await
    .unwrap();
    assert!(
        caught["body"]["output"][0]["text"]
            .as_str()
            .unwrap()
            .contains("policy")
    );
    let lost = make_room();
    let input = door
        .admit(&second, InterfaceId::new("world_provider_test"), decoder)
        .unwrap();
    let (io, _) = octopus_tool_door_runtime::owner_contract_tests::io_pair(input.0 + 1);
    let error = lost
        .receive(
            ToolProcessInvocation {
                request_id: input.0 + 1,
                descriptor: effective_descriptor(&input.1.tool),
                envelope: input.2,
                source_movement_seq: input.0,
                worker_environment: Some(formation.clone()),
                worker_environment_kind: Some(kind.clone()),
            },
            io,
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(error, ToolProcessError::Capability(error) if error.contains("context_lost")));
    room.release_room(&formation).await.unwrap();
    assert_eq!(
        door.decode_record(queued.0, &queued.2, decoder, Purpose::HistoricalIdentity)
            .unwrap()
            .unwrap()
            .correlation_id,
        original_resume_id
    );
    tokio::task::spawn_local(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::Full)).await.unwrap();
    })
}
#[path = "../../../../system_room/packages/octopus-system-room-env/src/file_write.rs"]
mod actual_file_write;
#[path = "../../../../system_room/packages/octopus-system-room-env/src/bound_surface.rs"]
mod actual_bound_surface;
#[path = "../../octopus-code-mode-tool-runtime/src/owner_handoff.rs"]
mod actual_owner_handoff;
#[path = "failure_return_tests.rs"]
mod failure_return_tests;

#[derive(Clone)]
struct AdmittedDelivery {
    seq: u64,
    from: InterfaceId,
    to: InterfaceId,
    file: InterfaceArtifactFile,
    metadata: InvocationMetadata,
}

struct FrontierPolicy {
    descriptor_pin: octopus_policy::FormationPin,
    ceiling: octopus_policy::ContextGrant,
    affordances: Vec<InterfaceToolAffordance>,
    contents: BTreeMap<ArtifactId, ContentHash>,
    blobs: PathBuf,
    roles: Vec<ArtifactRef>,
}

impl octopus_runtime::RunGrantResolver for FrontierPolicy {
    fn resolve_run_grant(
        &self,
        query: octopus_runtime::RunGrantQuery<'_>,
    ) -> Result<octopus_runtime::RunGrantResolution, String> {
        if !["run_code_mode_test", "run_detached_provider_writer"].contains(&query.run.as_str())
            || query.ee.as_str() != "ee_code_mode_test"
            || query.worker.as_str() != WORKER
        {
            return Err("fixture does not own this run/EE/worker".into());
        }
        let grant = octopus_policy::form_policy(
            self.ceiling.clone(),
            &[],
            &[octopus_policy::PolicyLayer {
                interfaces: Some(octopus_policy::CapSet::from_allowed(
                    query.authored_interfaces.iter().cloned(),
                )),
                artifact_access: query.artifact_access.clone(),
                ..Default::default()
            }],
            self.descriptor_pin.clone(),
        );
        let mut projection = GrantProjection::from_formed(&grant);
        for role in &self.roles {
            if !query.artifact_access.get(&role.id).is_some_and(|access| access.read)
                || !grant.context.artifact_access.get(&role.id).is_some_and(|access| access.read) {
                return Err(format!("mandatory role artifact '{}' is not readable at '{}'", role.id, query.checkpoint));
            }
        }
        projection.role_artifacts = self.roles.iter().map(|role| role.id.to_string()).collect();
        projection.ee_root_slot_allowance = Some(vec!["run_records_root".into()]);
        projection.interface_affordance_names = self
            .affordances
            .iter()
            .map(|a| a.name.to_string())
            .collect();
        projection.interface_affordance_specs = self.affordances.clone();
        octopus_runtime::RunGrantResolution::new(grant, projection)
    }
}

impl octopus_runtime::ContextArtifactResolver for FrontierPolicy {
    fn resolve_artifact_content(&self, id: &ArtifactId) -> Result<ContentHash, String> {
        if let Some(content) = self.contents.get(id) { return Ok(*content); }
        let (label,hash) = id.as_str().rsplit_once(':').ok_or("fixture artifact has no content identity")?;
        let content = ContentHash::from_hex(hash).ok_or("fixture artifact has invalid hash")?;
        let bytes = std::fs::read(self.blobs.join(hash)).map_err(|e|e.to_string())?;
        let manifest: octopus_interfaces::InterfaceArtifactManifest = serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;
        if ContentHash::of_bytes(&bytes) != content || manifest.label != label { return Err("fixture artifact identity mismatch".into()); }
        Ok(content)
    }
}

struct FrontierAdmission {
    runtime: octopus_runtime::Runtime,
    provider: FrontierProvider,
    door: tokio::sync::Mutex<octopus_tool_door_runtime::owner_contract_tests::DoorFixture>,
    retained_arrivals: std::sync::Arc<Mutex<octopus_ledger_index::ArrivalDedupIndex>>,
    deliveries: Mutex<Vec<AdmittedDelivery>>,
    changed: tokio::sync::Notify,
    root: PathBuf,
    run_records: PathBuf,
    file_writes: std::sync::atomic::AtomicUsize,
    conflict_next: AtomicBool,
    reject_next_owner_arrival: AtomicBool,
    fail_after_nested_output: AtomicBool,
    interleave_next_write: AtomicBool,
    provider_wins_next_write: AtomicBool,
    provider_writes_before_owner_call: AtomicBool,
    probe_unsubmitted_output: AtomicBool,
    drop_owner_ready_ack: AtomicBool,
    pending_output_proof: Mutex<Option<(octopus_tool_door_runtime::ToolDoorOutputSource, PathBuf, super::config::CodeModeConfig)>>,
    output_prepared: tokio::sync::Notify,
    provider_result_release: tokio::sync::Notify,
    owner_write_prepared: tokio::sync::Notify,
    owner_write_admitted: tokio::sync::Notify,
    peer_write_admitted: tokio::sync::Notify,
    peer_write_applied: tokio::sync::Notify,
    owner_write_completed: tokio::sync::Notify,
}

impl FrontierAdmission {
    async fn record(
        &self,
        file: &InterfaceArtifactFile,
        from: InterfaceId,
        to: InterfaceId,
        metadata: InvocationMetadata,
    ) -> Result<u64, String> {
        let runtime = metadata.runtime.as_ref().ok_or("fixture movement lost runtime scope")?;
        let snapshot = std::fs::read(self.run_records.join(format!("{}.json",runtime.run))).map_err(|e| e.to_string())?;
        let hash = ContentHash::of_bytes(&snapshot).to_hex();
        if runtime.run_record_hash.as_deref() != Some(hash.as_str()) { return Err("fixture current bytes differ from actual runtime admission".into()); }
        std::fs::write(self.root.join("blobs").join(hash),snapshot).map_err(|e| e.to_string())?;
        let seq = self.door.lock().await.record_transfer(
            file,
            from.clone(),
            to.clone(),
            metadata.clone(),
            octopus_ledger::MovementKind::InterfaceOutbound,
        )?;
        self.deliveries.lock().unwrap().push(AdmittedDelivery {
            seq,
            from,
            to,
            file: file.clone(),
            metadata,
        });
        self.changed.notify_one();
        Ok(seq)
    }

    async fn arrive(&self, from: &str, file: &InterfaceArtifactFile) -> Result<u64, String> {
        let artifact = self.door.lock().await.cache(file)?;
        let label = file.read_manifest().map_err(|e| e.to_string())?.label;
        let targets = self
            .runtime
            .resolve_interface_arrival_targets(&InterfaceId::new(from), &label)
            .map_err(|e| e.to_string())?;
        if targets.is_empty() {
            return Err(format!("no exact authored target for {from}"));
        }
        let mut final_seq = None;
        let mut refusals = Vec::new();
        for target in targets {
            let observation = match self
                .runtime
                .admit_resolved_interface_checkpoint(&target, artifact.clone(), label.clone(), None)
                .await
            {
                Ok(observation) => observation.commit(),
                Err(error) => {
                    self.door.lock().await.record_refusal(file, RuntimeInvocationMetadata {
                        run:target.run.clone(),ee:target.ee.clone(),worker:target.worker.clone(),checkpoint:target.checkpoint.clone(),
                        interface:InterfaceId::new(from),epoch:target.epoch.0,run_record_hash:Some(target.run_record.hash.to_hex()),
                    }, DeliveryVerdict::refused(error.refused_reason(),error.refusal_detail()))?;
                    refusals.push(error.to_string());
                    continue;
                }
            };
            if !observation.ok {
                refusals.push(format!(
                    "actual runtime refused arrival: {:?}",
                    observation.data
                ));
                continue;
            }
            let admitted_seq = self.door.lock().await.record_admission(&artifact,&observation.invocation_metadata,&observation.admitted_mark_outcomes)?;
            final_seq.get_or_insert(admitted_seq);
            if label == "code_mode.owner_ready" {
                let ctx = self.door.lock().await.context.clone();
                if let Some((source, durable, config)) = self.pending_output_proof.lock().unwrap().take() {
                    let run_path = self.run_records.join("run_code_mode_test.json");
                    let run = std::fs::read(&run_path).unwrap();
                    let ledger = std::fs::read(ctx.owner_movements_path()).unwrap();
                    assert!(matches!(super::continuation::output_authority(&ctx, &durable, &config, &source)?, super::continuation::OutputAuthority::Pending));
                    assert_eq!(std::fs::read(&run_path).unwrap(), run);
                    assert_eq!(std::fs::read(ctx.owner_movements_path()).unwrap(), ledger);
                }
            }
            for transfer in observation.outbound_artifact_transfers {
                let seq = self.record(file,transfer.from_interface.clone(),transfer.to_interface.clone(),transfer.invocation_metadata.clone()).await?;

                if transfer.to_interface.as_str() == "system_provider_file_test" {
                    self.execute_provider_file_write(&AdmittedDelivery { seq,from:transfer.from_interface,to:transfer.to_interface,file:file.clone(),metadata:transfer.invocation_metadata }).await?;
                }
            }
        }
        final_seq.ok_or_else(|| format!("runtime admitted no output edge: {refusals:?}"))
    }

    async fn arrive_retained(
        &self, from: &str, file: &InterfaceArtifactFile, identity: &str,
    ) -> Result<ArtifactDelivery, String> {
        let artifact = super::continuation::artifact_ref(file)?;
        let (result, movements) = {
            let mut door = self.door.lock().await;
            let start = octopus_ledger::Seq(door.len() as u64);
            let mut blobs = octopus_ledger_store::DurableBlobs::open(self.root.join("blobs"))
                .map_err(|error| error.to_string())?;
            let mut owner = octopus_app_server::InterfaceArtifactArrivalFromInterface::new(
                &self.runtime, &mut *door, &mut blobs, self.retained_arrivals.clone(),
                octopus_ledger::TraceId("trace:code-mode-owner-contract".into()), InterfaceId::new(from),
            );
            let result = octopus_app_server::owner_contract_tests::arrive_retained_artifact(
                &mut owner, file, identity,
            ).await.map_err(|error| error.to_string());
            drop(owner);
            (result, door.read_from(start))
        };
        for movement in movements {
            if movement.kind != octopus_ledger::MovementKind::InterfaceOutbound { continue; }
            if movement.artifact.as_ref() != Some(&artifact) {
                return Err("retained owner emitted a different artifact".into());
            }
            let metadata = movement.invocation_metadata.as_ref()
                .and_then(|metadata| metadata.invocation()).cloned()
                .ok_or("retained owner outbound lost its exact invocation metadata")?;
            let (octopus_ledger::Endpoint::Interface(from), octopus_ledger::Endpoint::Interface(to)) =
                (movement.from, movement.to);
            let delivery = AdmittedDelivery { seq: movement.seq.0, from, to, file: file.clone(), metadata };
            self.deliveries.lock().unwrap().push(delivery.clone());
            self.changed.notify_one();
            if delivery.to.as_str() == "system_provider_file_test" {
                self.execute_provider_file_write(&delivery).await?;
            }
        }
        result
    }

    async fn transport(
        &self,
        output: octopus_tool_door_runtime::owner_contract_tests::OwnerTransportOutput,
    ) -> Result<(), String> {
        let seq = if let Some((source_seq, target)) = &output.directed {
            if target.as_str() != "system_file_test" {
                return Err("unexpected directed owner output".into());
            }
            let source = self
                .deliveries
                .lock()
                .unwrap()
                .iter()
                .find(|d| d.seq == *source_seq)
                .cloned()
                .ok_or("missing actual source movement")?;
            let source_runtime = source
                .metadata
                .runtime
                .as_ref()
                .ok_or("missing source metadata")?;
            let interleaved = self.interleave_next_write.load(Ordering::SeqCst);
            let provider_wins = self.provider_wins_next_write.load(Ordering::SeqCst);
            let provider_writes_first = self.provider_writes_before_owner_call.load(Ordering::SeqCst);
            if interleaved && provider_wins {
                self.owner_write_prepared.notify_one();
                if provider_writes_first { self.peer_write_applied.notified().await; }
                else { self.peer_write_admitted.notified().await; }
            }
            let authority = self
                .runtime
                .resolve_system_interface_emission_grant(
                    octopus_runtime::SystemInterfaceEmitter {
                        run: &source_runtime.run,
                        checkpoint: &source_runtime.checkpoint,
                        epoch: octopus_run_record::GrantEpoch(source_runtime.epoch),
                    },
                    &InterfaceId::new(DOOR),
                    target,
                    "tool.call",
                )
                .map_err(|e| e.to_string())?;
            let mut metadata = InvocationMetadata::new(authority.metadata_projection).with_runtime(
                RuntimeInvocationMetadata {
                    run: source_runtime.run.clone(),
                    ee: authority.ee,
                    worker: authority.worker,
                    checkpoint: source_runtime.checkpoint.clone(),
                    interface: target.clone(),
                    epoch: authority.epoch.0,
                    run_record_hash: Some(authority.run_record.hash.to_hex()),
                },
            );
            metadata.checkpoint_binding_policy = Some(authority.checkpoint_binding_policy);
            let call: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(
                &output
                    .artifact
                    .read_payload_bytes()
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if call.tool != "file.write" || !metadata.grant.tools.contains(&call.tool) {
                return Err("file writer was not granted exactly".into());
            }
            let seq = self
                .record(
                    &output.artifact,
                    InterfaceId::new(DOOR),
                    target.clone(),
                    metadata.clone(),
                )
                .await?;
            if interleaved && provider_wins && provider_writes_first {
                assert_ne!(metadata.runtime.as_ref().unwrap().run_record_hash.as_deref(),
                    call.arguments["expected_before_hash"].as_str());
            }
            let envelope = InterfaceInvocationEnvelope {
                artifact: self.door.lock().await.cache(&output.artifact)?,
                artifact_file: output.artifact.clone(),
                metadata: Some(metadata),
                artifact_binding_payloads: vec![],
            };
            if self.conflict_next.swap(false, Ordering::SeqCst) { self.provider_progress(777).await?; }
            if interleaved {
                self.owner_write_admitted.notify_one();
                if provider_wins && !provider_writes_first {
                    self.peer_write_applied.notified().await;
                } else if !provider_wins {
                    self.peer_write_admitted.notified().await;
                }
            }
            let effect = actual_file_write::execute_file_write(
                &envelope,
                &call.arguments,
                &["system_room".into()],
                &BTreeMap::from([("run_records_root".into(), self.run_records.clone())]),
                &self.root.join("file-effects"),
                8 * 1024 * 1024,
            );
            self.file_writes.fetch_add(1, Ordering::SeqCst);
            let result = match effect {
                Ok(body) => octopus_tool_handlers::ToolResultArtifact::success(&call.tool,call.correlation_id,body),
                Err(error) => octopus_tool_handlers::ToolResultArtifact::failure(&call.tool,call.correlation_id,octopus_tool_handlers::ToolResultError::new("tool_execution_failed",error)),
            };
            let file = InterfaceArtifactFile::write(
                self.root.join("file-results"),
                &result.label,
                &serde_json::to_vec(&result).unwrap(),
            )
            .map_err(|e| e.to_string())?;
            let delivered = self.arrive(target.as_str(), &file).await;
            delivered?;
            if interleaved && provider_wins && !provider_writes_first {
                let ctx = self.door.lock().await.context.clone();
                failure_return_tests::rejected_write_is_proven_by_its_admitted_failure(self, &ctx, &envelope, seq);
            }
            if interleaved { self.owner_write_completed.notify_one(); }
            seq
        } else {
            if output.artifact.read_manifest().map_err(|e| e.to_string())?.label != "code_mode.owner_ready"
                && self.reject_next_owner_arrival.swap(false, Ordering::SeqCst) {
                let private = self.door.lock().await.context.interface_var_dir().canonicalize().unwrap();
                assert!(output.artifact.manifest_path().starts_with(&private), "owner replay must cross the spawned private-output boundary");
                assert!(output.artifact.read_manifest().unwrap().payload_path.starts_with(&private));
                if output.retained_delivery_id.is_some() {
                    return output.acknowledge_retained(Err("fixture interrupted after checked owner installation".into()));
                }
                return output.acknowledge(DeliveryVerdict::refused(octopus_interfaces::RefusedReason::Policy,
                    "fixture interrupted after checked owner installation".into()));
            }
            if let Some(identity) = output.retained_delivery_id.as_deref() {
                let delivery = self.arrive_retained(DOOR, &output.artifact, identity).await;
                return output.acknowledge_retained(delivery);
            }
            let seq = self.arrive(DOOR, &output.artifact).await?;
            if output.artifact.read_manifest().map_err(|e| e.to_string())?.label == "code_mode.owner_ready"
                && self.drop_owner_ready_ack.swap(false, Ordering::SeqCst) {
                return Ok(());
            }
            seq
        };
        output.acknowledge(DeliveryVerdict::Admitted { seq })
    }

    async fn next(
        &self,
    ) -> Result<
        (
            u64,
            octopus_tool_handlers::ToolCallArtifact,
            InterfaceInvocationEnvelope,
        ),
        String,
    > {
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                let ready = self.changed.notified();
                tokio::pin!(ready);
                ready.as_mut().enable();
                if let Some(input) = self
                    .door
                    .lock()
                    .await
                    .next_formed(super::decode_code_mode_door_artifact)?
                {
                    return Ok(input);
                }
                ready.await;
            }
        })
        .await
        .map_err(|_| "owner fixture timed out waiting for an admitted invocation".to_string())?
    }

    fn run<'a>(
        &'a self,
        room: &'a CodeModeRoom,
        handoff: &'a std::sync::Arc<actual_owner_handoff::OwnerHandoff>,
        formation: &'a WorkerEnvironmentFormation,
        kind: &'a WorkerEnvKindSpec,
        input: (
            u64,
            octopus_tool_handlers::ToolCallArtifact,
            InterfaceInvocationEnvelope,
        ),
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>> {
        Box::pin(async move {
        use octopus_tool_door_runtime::owner_contract_tests::{
            execute_formed, hand_owned_output, io_pair,
        };
        let (seq, call, envelope) = input;
        let (ctx, source) = {
            let door = self.door.lock().await;
            (
                door.context.clone(),
                door.output_source(seq, &envelope, formation)?,
            )
        };
        let callback = handoff.callback();
        let (io, mut peer) = io_pair(seq + 1);
        let invocation = ToolProcessInvocation {
            request_id: seq + 1,
            source_movement_seq: seq,
            descriptor: effective_descriptor(&call.tool),
            envelope,
            worker_environment: Some(formation.clone()),
            worker_environment_kind: Some(kind.clone()),
        };
        let execution = execute_formed(room, &room.environment, invocation, io);
        tokio::pin!(execution);
        let outputs = loop {
            tokio::select! {
                result=&mut execution=>break result?,
                artifact=peer.next_artifact()=>{
                    let (request,event,file)=artifact?;
                    let nested = file.read_manifest().map_err(|error| error.to_string())?.label == "tool.call";
                    let verdict=hand_owned_output(&ctx,&source,&callback,file).await?;
                    if nested && self.fail_after_nested_output.swap(false, Ordering::SeqCst) {
                        return Err("fixture interrupted after admitted nested output".into());
                    }
                    peer.acknowledge(request,event,verdict).await?;
                }
            }
        };
        let mut result = None;
        for file in outputs {
            result = Some(
                serde_json::from_slice(&file.read_payload_bytes().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?,
            );
            if self.probe_unsubmitted_output.load(Ordering::SeqCst) {
                let config: super::config::CodeModeConfig = serde_json::from_slice(&std::fs::read(ctx.interface_registry_dir().join("code_mode.json")).unwrap()).unwrap();
                let durable = octopus_tool_door_runtime::tool_door_durable_state_dir(&ctx, &formation.worker_environment_id)?;
                assert!(super::continuation::first_output_is_unsubmitted(&ctx, &durable, &config, &source, &file)?);
                let mut other = source.clone(); other.movement.seq.0 += 1;
                assert!(!super::continuation::first_output_is_unsubmitted(&ctx, &durable, &config, &other, &file)?);
                let ledger = std::fs::read(ctx.owner_movements_path()).unwrap();
                let mut partial = ledger.clone(); partial.extend_from_slice(b"{\"seq\":");
                std::fs::write(ctx.owner_movements_path(), partial).unwrap();
                assert!(!super::continuation::first_output_is_unsubmitted(&ctx, &durable, &config, &source, &file)?);
                assert!(matches!(super::continuation::output_authority(&ctx, &durable, &config, &source)?, super::continuation::OutputAuthority::Pending));
                std::fs::write(ctx.owner_movements_path(), &ledger).unwrap();
                *self.pending_output_proof.lock().unwrap() = Some((source.clone(), durable.clone(), config.clone()));
                self.output_prepared.notify_one();
                self.peer_write_applied.notified().await;
                assert!(matches!(super::continuation::reconcile_checked_provider_advance(&ctx, &durable, &config, None),
                    Err(octopus_run_authoring::CheckedRunError::AwaitingResults { .. })));
                let delivery = hand_owned_output(&ctx, &source, &callback, file.clone());
                tokio::pin!(delivery);
                assert!(tokio::time::timeout(std::time::Duration::from_millis(50), &mut delivery).await.is_err());
                self.provider_result_release.notify_one();
                let error = delivery.await.unwrap_err();
                assert!(error.contains("verdict channel"), "{error}");
                let cold = actual_owner_handoff::OwnerHandoff::new();
                let callback = cold.callback();
                hand_owned_output(&ctx, &source, &callback, file.clone()).await?;
                self.probe_unsubmitted_output.store(false, Ordering::SeqCst);
                assert!(!super::continuation::first_output_is_unsubmitted(&ctx, &durable, &config, &source, &file)?);
                assert_eq!(self.deliveries.lock().unwrap().iter().filter(|row|
                    row.from.as_str() == DOOR && row.to.as_str() == DOOR
                    && row.file.read_manifest().is_ok_and(|manifest| manifest.label == "code_mode.owner_ready")).count(), 1);
                let run_path = self.run_records.join("run_code_mode_test.json");
                let run = std::fs::read(&run_path).unwrap();
                let ledger = std::fs::read(ctx.owner_movements_path()).unwrap();
                assert!(matches!(super::continuation::output_authority(&ctx, &durable, &config, &source)?, super::continuation::OutputAuthority::Admitted(_)));
                assert_eq!(std::fs::read(&run_path).unwrap(), run);
                assert_eq!(std::fs::read(ctx.owner_movements_path()).unwrap(), ledger);
            } else {
                hand_owned_output(&ctx, &source, &callback, file).await?;
            }
        }
        result.ok_or_else(|| "room emitted no terminal result".into())
        })
    }
}

#[test]
fn downloaded_artifact_is_admitted_bound_and_read_in_the_same_cell() {
    std::thread::Builder::new().stack_size(16 * 1024 * 1024).spawn(|| {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(60),run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::Generated))).await.unwrap();
        });
    }).unwrap().join().unwrap();
}

#[test]
fn application_approval_rearms_the_return_before_either_delivery_order() {
    std::thread::Builder::new().stack_size(16 * 1024 * 1024).spawn(|| {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(120),run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::Application))).await.unwrap();
        });
    }).unwrap().join().unwrap();
}

async fn actual_owner_failure_return(
    admission: &std::sync::Arc<FrontierAdmission>, room: &CodeModeRoom,
    handoff: &std::sync::Arc<actual_owner_handoff::OwnerHandoff>,
    formation: &WorkerEnvironmentFormation, config: &super::config::CodeModeConfig,
    initial: &(u64, octopus_tool_handlers::ToolCallArtifact, InterfaceInvocationEnvelope), durable: &Path,
) {
    let ctx = admission.door.lock().await.context.clone();
    super::continuation::ContinuationContext::create(durable, formation, config,
        &config.resolve_context(&initial.2).unwrap(), &initial.2,
        super::continuation::OriginalRun::from_envelope(&initial.2, initial.0).unwrap()).unwrap();
    let records = octopus_worker_env::WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
    records.activate(formation, initial.0).unwrap();
    let result = octopus_tool_handlers::ToolResultArtifact::failure(&initial.1.tool, initial.1.correlation_id.clone(),
        octopus_tool_handlers::ToolResultError::new("tool_effect_indeterminate", "tool process outcome is unknown")
            .with_details(serde_json::Map::from_iter([("source_movement_seq".into(), json!(initial.0)),
                ("failure_details".into(), json!({"effect_state":"in_flight_outcome_unknown"}))])));
    let file = InterfaceArtifactFile::write(ctx.interface_var_dir().join("failures"), &result.label, &serde_json::to_vec(&result).unwrap()).unwrap();
    admission.arrive(DOOR, &file).await.unwrap();
    let own = admission.deliveries.lock().unwrap().iter().rev().find(|row| row.from.as_str() == DOOR && row.to.as_str() == DOOR).unwrap().clone();
    let limits = ctx.owner_io_limits().unwrap();
    let evidence = octopus_run_authoring::CheckedRunEvidence {
        movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
        max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
    };
    let movement = octopus_run_authoring::generated_artifacts::read_movement(&evidence, own.seq).unwrap();
    let mut reader = octopus_tool_door_runtime::LedgerTailReader::open_named(&ctx, "failure-contract").unwrap();
    let mut wrong = movement.clone(); wrong.artifact.as_mut().unwrap().id = ArtifactId::new("foreign");
    assert!(handoff.reconcile_failure(&ctx, &mut reader, &wrong).await.is_err());
    let context_path = durable.join("context.json");
    let saved = std::fs::read(&context_path).unwrap();
    let mut drift: Value = serde_json::from_slice(&saved).unwrap(); drift["identity"]["formation"]["ee"] = json!("ee_foreign");
    std::fs::write(&context_path, serde_json::to_vec(&drift).unwrap()).unwrap();
    assert!(handoff.reconcile_failure(&ctx, &mut reader, &movement).await.is_err());
    std::fs::write(&context_path, saved).unwrap();
    admission.provider_progress(4242).await.unwrap();
    let current: Value = serde_json::from_slice(&std::fs::read(admission.run_records.join("run_code_mode_test.json")).unwrap()).unwrap();
    assert!(!current["checkpoints"].as_array().unwrap().iter().any(|cp|
        cp["id"] == json!(initial.2.runtime().unwrap().checkpoint)));
    assert!(current["active_checkpoints"].as_array().unwrap().contains(&json!(own.metadata.runtime.as_ref().unwrap().checkpoint)));
    admission.reject_next_owner_arrival.store(true, Ordering::SeqCst);
    let error = handoff.reconcile_failure(&ctx, &mut reader, &movement).await.unwrap_err();
    assert!(error.contains("fixture interrupted after checked owner installation"), "{error}");
    let installed_writes = admission.file_writes.load(Ordering::SeqCst);
    let proposal = std::fs::read_dir(durable).unwrap().map(|entry| entry.unwrap().path())
        .find(|path| path.file_name().unwrap().to_str().unwrap().ends_with(".edit.json")).unwrap();
    let saved = std::fs::read(&proposal).unwrap();
    let mut edit: octopus_run_authoring::CheckedRunEdit = super::continuation::read_record(&proposal).unwrap();
    super::continuation::prepare_record(&proposal, &edit).unwrap();
    edit.correlation_id.push_str("-foreign");
    assert!(super::continuation::prepare_record(&proposal, &edit).is_err());
    assert_eq!(std::fs::read(&proposal).unwrap(), saved);
    std::fs::write(&proposal, serde_json::to_vec(&edit).unwrap()).unwrap();
    assert!(actual_owner_handoff::OwnerHandoff::new().reconcile_failure(&ctx, &mut reader, &movement).await.unwrap_err()
        .contains("exact prepared checked write"));
    std::fs::write(&proposal, saved).unwrap();
    actual_owner_handoff::OwnerHandoff::new().reconcile_failure(&ctx, &mut reader, &movement).await.unwrap();
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), installed_writes + 1);
    let writes = admission.file_writes.load(Ordering::SeqCst);
    actual_owner_handoff::OwnerHandoff::new().reconcile_failure(&ctx, &mut reader, &movement).await.unwrap();
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
    assert!(room.rooms.lock().await.is_empty());
    let delivered = admission.deliveries.lock().unwrap().iter().filter(|row|
        row.from.as_str() == DOOR && row.to.as_str() == "world_provider_test"
        && row.file.read_manifest().unwrap().label == result.label).cloned().collect::<Vec<_>>();
    assert_eq!(delivered.len(), 1);
    assert_eq!(super::continuation::artifact_ref(&delivered[0].file).unwrap(), movement.artifact.unwrap());
    assert_eq!(serde_json::from_slice::<octopus_tool_handlers::ToolResultArtifact>(&delivered[0].file.read_payload_bytes().unwrap()).unwrap(), result);
}

enum FrontierCase { Full, Generated, Application, Coauthor, DetachedResult, DetachedFailure, InterleavedResult, UnsubmittedResult, RefusedFailure, RefusedFailurePretty, NestedFailure, ReadinessFailure }

fn exact_install_does_not_claim_later_state(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    path: &Path, durable: &Path, call: &AdmittedDelivery, result: &AdmittedDelivery,
    peer: &octopus_run_authoring::CheckedRunWriter,
) {
    use octopus_run_authoring::{CheckedRunEvidence, CheckedRunEvidenceReader, CheckedRunInstall,
        CheckedRunWriter, ConsentEvidenceRef, FileCheckedRunReader, checked_run_install};
    let limits = ctx.owner_io_limits().unwrap();
    let evidence = CheckedRunEvidence { movements_path:ctx.owner_movements_path(), ledger_index_path:ctx.owner_ledger_index_path(),
        blobs_dir:ctx.owner_blobs_dir(), max_control_frame_bytes:limits.max_control_frame_bytes,
        max_manifest_bytes:limits.max_manifest_bytes, max_payload_bytes:limits.max_payload_bytes };
    let tool: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&call.file.read_payload_bytes().unwrap()).unwrap();
    let writer = CheckedRunWriter { owner:result.to.clone(), source:call.from.clone(), door:call.to.clone(), tool:tool.tool,
        call_label:call.file.read_manifest().unwrap().label, result_label:result.file.read_manifest().unwrap().label };
    let writers = [writer.clone(), peer.clone()];
    let install = CheckedRunInstall {
        call:ConsentEvidenceRef {movement_seq:call.seq,artifact:super::continuation::artifact_ref(&call.file).unwrap()},
        result:ConsentEvidenceRef {movement_seq:result.seq,artifact:super::continuation::artifact_ref(&result.file).unwrap()},
    };
    let fresh = || {
        let mut reader = FileCheckedRunReader::open(&evidence, Some(call.seq-1)).unwrap();
        while reader.next_sequence() < call.seq {
            reader.movement(reader.next_sequence()).unwrap().unwrap();
        }
        reader
    };
    let exact = checked_run_install(&mut fresh(), path, &install, &writers).unwrap();
    let recorded = super::continuation::InstalledHandoff {
        before_hash: ContentHash::of_bytes(&exact.before).to_hex(),
        after_hash: ContentHash::of_bytes(&exact.after).to_hex(),
        source_seq: call.seq,
        source_artifact: install.call.artifact.clone(),
        result_seq: result.seq,
        result_artifact: install.result.artifact.clone(),
    };
    let record_path = durable.join(format!("{}.installed.json", recorded.after_hash));
    assert_eq!(super::continuation::read_record::<super::continuation::InstalledHandoff>(&record_path).unwrap(), recorded);
    let stored = std::fs::read(&record_path).unwrap();
    super::continuation::record_install(durable, &recorded).unwrap();
    let concurrent = durable.join("concurrent-install");
    std::fs::create_dir(&concurrent).unwrap();
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&concurrent, std::fs::Permissions::from_mode(0o700)).unwrap(); }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    std::thread::scope(|scope| {
        let mut writers = Vec::new();
        for _ in 0..2 {
            let (root, value, barrier) = (&concurrent, &recorded, barrier.clone());
            writers.push(scope.spawn(move || {
                super::continuation::INSTALL_PUBLICATION_BARRIER.with(|slot| *slot.borrow_mut() = Some(barrier));
                super::continuation::record_install(root, value)
            }));
        }
        for writer in writers { writer.join().unwrap().unwrap(); }
    });
    let concurrent_path = concurrent.join(record_path.file_name().unwrap());
    assert_eq!(std::fs::read(&concurrent_path).unwrap(), stored);
    assert_eq!(super::continuation::read_record::<super::continuation::InstalledHandoff>(&concurrent_path).unwrap(), recorded);
    assert_eq!(std::fs::read_dir(&concurrent).unwrap().count(), 1);
    for field in ["before_hash", "source_seq", "source_artifact", "result_seq", "result_artifact"] {
        let mut different = recorded.clone();
        match field {
            "before_hash" => different.before_hash = ContentHash::of_bytes(b"foreign preimage").to_hex(),
            "source_seq" => different.source_seq -= 1,
            "source_artifact" => different.source_artifact.id = ArtifactId::new("foreign call"),
            "result_seq" => different.result_seq += 1,
            "result_artifact" => different.result_artifact.content = ContentHash::of_bytes(b"foreign result"),
            _ => unreachable!(),
        }
        assert!(super::continuation::record_install(durable, &different).unwrap_err().contains("conflicts with its immutable bytes"), "accepted changed {field}");
    }
    assert_eq!(std::fs::read(&record_path).unwrap(), stored);
    let mut alias = writer.clone(); alias.owner = InterfaceId::new("another_owner");
    assert!(checked_run_install(&mut fresh(), path, &install, &[writer, alias]).unwrap_err().contains("ambiguous writer"));
    struct LaterSnapshot<'a> { reader:FileCheckedRunReader<'a>, result:octopus_ledger::Movement, bytes:Vec<u8>, hash:ContentHash }
    impl CheckedRunEvidenceReader for LaterSnapshot<'_> {
        fn max_payload_bytes(&self) -> u64 { self.reader.max_payload_bytes() }
        fn admitted_movement(&mut self, seq:u64) -> Result<Option<octopus_ledger::Movement>,String> {
            if seq==self.result.seq.0 {Ok(Some(self.result.clone()))}else{self.reader.admitted_movement(seq)}
        }
        fn movement(&mut self, seq:u64) -> Result<Option<octopus_ledger::Movement>,String> {
            if seq==self.result.seq.0 {Ok(Some(self.result.clone()))}else{self.reader.movement(seq)}
        }
        fn snapshot(&mut self, hash:&ContentHash)->Result<Vec<u8>,String> {
            if hash==&self.hash {Ok(self.bytes.clone())}else{self.reader.snapshot(hash)}
        }
        fn artifact(&mut self, reference:&ArtifactRef)->Result<(octopus_interfaces::InterfaceArtifactManifest,Vec<u8>),String> {self.reader.artifact(reference)}
    }
    let overlay = |state:Value| {
        let bytes=serde_json::to_vec_pretty(&state).unwrap();let hash=ContentHash::of_bytes(&bytes);
        let mut result=exact.result.clone();
        let mut metadata=result.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
        metadata.runtime.as_mut().unwrap().run_record_hash=Some(hash.to_hex());
        result.invocation_metadata=Some(metadata.into());
        LaterSnapshot {reader:fresh(),result,bytes,hash}
    };
    let mut later:Value=serde_json::from_slice(&exact.after).unwrap();
    later["interface_state"][peer.owner.as_str()]["unproven_later_state"]=json!(true);
    let mut reader=overlay(later.clone());
    assert_eq!(checked_run_install(&mut reader,path,&install,&writers).unwrap().after,exact.after);
    let unproven=reader.bytes.clone();
    assert!(octopus_run_authoring::checked_run_file_chain_before(&mut overlay(later.clone()),path,
        &exact.before,&unproven,call.seq-1,result.seq+1,&writers).is_err());
    let cp=exact.result.runtime().unwrap().checkpoint.clone();
    later["checkpoints"].as_array_mut().unwrap().iter_mut().find(|row|row["id"]==json!(cp)).unwrap()["epoch"]=json!(1);
    assert!(checked_run_install(&mut overlay(later),path,&install,&writers).is_err());
}

#[tokio::test]
async fn completed_owner_write_survives_an_overlapping_provider_write() {
    tokio::time::timeout(std::time::Duration::from_secs(20),
        run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::InterleavedResult)))
        .await.unwrap();
}

#[tokio::test]
async fn completed_error_waits_for_the_checked_provider_result_before_dispatch() {
    tokio::time::timeout(std::time::Duration::from_secs(20),
        run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::UnsubmittedResult)))
        .await.unwrap();
}

#[tokio::test]
async fn detached_coauthor_result_recovers_target_bytes_from_checked_write() {
    run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::DetachedResult)).await;
}

#[tokio::test]
async fn admitted_owner_failure_returns_once_without_reexecuting_the_effect() {
    run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::DetachedFailure)).await;
}

#[tokio::test]
async fn detached_coauthor_uses_its_exact_admitted_writer_declaration() {
    run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::Coauthor)).await;
}

fn routed_frontiers_use_actual_runtime_and_file_write(case: FrontierCase) -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(async move {
    let generated_only = matches!(case, FrontierCase::Generated);
    use octopus_policy::{ArtifactVerb, CapSet, ContextGrant};
    use octopus_runtime::{
        InterfaceClass, InterfaceRuntimeDisposition, InterfaceType, Runtime,
        RuntimeInterfaceCatalog, RuntimeServices,
    };
    let root_guard = tempfile::Builder::new().prefix("oct-cm-").tempdir_in("/tmp").unwrap();
    let root = root_guard.path().canonicalize().unwrap();
    eprintln!("owner fixture artifacts: {}",root.display());
    bounded_room_media_preserves_the_actual_image_carrier(&root);
    let interface_dir = root.join("interface");
    write_interface(&interface_dir);
    let records = root.join("run-records");
    std::fs::create_dir(&records).unwrap();
    let prelude_source: Value = serde_json::from_slice(
        &std::fs::read(
            registry_root().join("artifact/artifact-code-mode-cua-prelude/artifact.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let browser_source: Value = serde_json::from_slice(
        &std::fs::read(registry_root().join("artifact/artifact-code-mode-browser-prelude/artifact.json")).unwrap(),
    ).unwrap();
    let browser_sdk_tools = ["browser.locator_query","browser.locator_fill","browser.locator_press","browser.locator_select","browser.locator_check","browser.locator_wait","browser.dom_snapshot","browser.dom_click","browser.dom_scroll","browser.pointer_move","browser.paste","browser.clipboard_read","browser.clipboard_write","browser.evaluate_readonly","browser.media_download","browser.visibility_get","browser.visibility_set","browser.click","browser.key","browser.drag","browser.scroll","browser.screenshot","browser.filechooser_arm","browser.filechooser_wait","browser.filechooser_set_files","browser.filechooser_cancel","browser.wait","browser.playwright_dom_snapshot"];
    let browser_bindings: serde_json::Map<String,Value> = browser_source["tool_roles"].as_object().unwrap().iter().filter_map(|(role, declaration)| {
        let tool = declaration["tool"].as_str().unwrap();
        (tool == "browser.tabs" || tool == "browser.wait_complete" || browser_sdk_tools.contains(&tool)).then(|| (role.clone(),json!({"name":tool.replace('.',"_"),"tool":tool})))
    }).collect();
    assert_eq!(browser_bindings.len(),browser_sdk_tools.len()+2);
    let module = format!(
        "{}\n{}\nexport const seed = {{value:1}}; export const cua = createCua({{app_state:{{name:'gui_app_state',tool:'gui.app_state'}}}}); export const browser = createBrowser({});",
        prelude_source["module_source"].as_str().unwrap(),
        browser_source["module_source"].as_str().unwrap(),
        Value::Object(browser_bindings)
    );
    let prelude_bytes = serde_json::to_vec(
        &json!({"schema_version":1,"module_source":module,"exports":["seed","cua","browser"]}),
    )
    .unwrap();
    let prelude_file =
        InterfaceArtifactFile::write(root.join("prelude"), "code-mode.prelude", &prelude_bytes)
            .unwrap();
    let prelude = super::continuation::artifact_ref(&prelude_file).unwrap();
    let binding = |from: &str, to: &str| json!({"inbound":{"interface":from,"config":{"artifact_policy":{"mode":"all"}}},"outbound":[{"interface":to,"config":{"artifact_policy":{"mode":"all"}}}]});
    let (topology, descriptor_pins, returns) = (|| {
        let edge = |name: &str, from: &str, to: &str| json!({"name":name,"binding":binding(from,to)});
        let mut topology = json!({"schema_version":1,"run":"run_code_mode_test","worker":WORKER,"owner_interface":DOOR,"prelude_artifact":prelude,"file_write_tool":"file.write",
            "authoring_result":edge("authoring","system_file_test",DOOR),"model_output":edge("model",DOOR,"world_provider_test"),"notification":edge("notification",DOOR,"world_operator_test"),"receipt":edge("receipt",DOOR,DOOR),"readiness":edge("readiness",DOOR,DOOR),
            "routes":[{"tool":NESTED_TOOL,"call":edge(NESTED_CHECKPOINT,DOOR,NESTED_INTERFACE),"result":edge("cp_process_to_code_mode",NESTED_INTERFACE,DOOR)},
                      {"tool":"browser.tabs","call":edge("cp_code_mode_to_browser",DOOR,"system_browser_test"),"result":edge("cp_browser_to_code_mode","system_browser_test",DOOR)}]});
        topology["routes"].as_array_mut().unwrap().push(json!({"tool":"gui.app_state","call":edge("cp_code_mode_to_gui",DOOR,"system_gui_test"),"result":edge("cp_gui_to_code_mode","system_gui_test",DOOR),"app_approval":{"operator_interface":"world_operator_test","resume_tool":"gui.app_resume"}}));
        for tool in browser_sdk_tools {
            topology["routes"].as_array_mut().unwrap().push(json!({"tool":tool,"call":edge(&format!("cp_sdk_{}",tool.replace('.',"_")),DOOR,"system_browser_test"),"result":edge("cp_browser_to_code_mode","system_browser_test",DOOR)}));
        }
        topology["routes"].as_array_mut().unwrap().push(json!({"tool":"browser.wait_complete","call":edge("cp_wait_complete",DOOR,"system_browser_test"),"result":edge("cp_wait_complete_return","system_browser_test",DOOR)}));
        for (tool,door) in [("browser.download_wait","system_browser_test"),("artifact.read","system_context_test")] {
            topology["routes"].as_array_mut().unwrap().push(json!({"tool":tool,"call":edge(&format!("cp_{}",tool.replace('.',"_")),DOOR,door),"result":edge("generated_return",door,DOOR)}));
        }
        let mut descriptor_pins = BTreeMap::new();
        let mut returns: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for route in topology["routes"].as_array_mut().unwrap() {
            let mut descriptor = raw_descriptor(route["tool"].as_str().unwrap());
            descriptor.interface = InterfaceId::new(route["call"]["binding"]["outbound"][0]["interface"].as_str().unwrap());
            let owner = super::config::DeclaredToolOwner::from_descriptor(&descriptor).unwrap();
            let name = owner.return_checkpoint().unwrap();
            let labels = returns.entry(name.clone()).or_default();
            labels.insert(format!("{}.result",descriptor.id));
            let value = serde_json::to_value(&descriptor).unwrap();
            descriptor_pins.insert(octopus_descriptors::DescriptorName::new(format!("registry_tool_root/{}/tool.json",descriptor.id)),octopus_descriptors::DescriptorHash::of_canonical_json(&value));
            route["process_descriptor"] = value;
            route["result"]["name"] = json!(name);
            if let Some(approval) = route.get_mut("app_approval") {
                let mut resume = raw_descriptor(approval["resume_tool"].as_str().unwrap());
                resume.interface = descriptor.interface;
                assert_eq!(super::config::DeclaredToolOwner::from_descriptor(&resume).unwrap(),owner);
                labels.insert(format!("{}.result",resume.id));
                let value = serde_json::to_value(&resume).unwrap();
                descriptor_pins.insert(octopus_descriptors::DescriptorName::new(format!("registry_tool_root/{}/tool.json",resume.id)),octopus_descriptors::DescriptorHash::of_canonical_json(&value));
                approval["resume_descriptor"] = value;
            }
        }
        for route in topology["routes"].as_array_mut().unwrap() {
            let labels = &returns[route["result"]["name"].as_str().unwrap()];
            let policy = json!({"mode":"allow","names":labels,"namespaces":[]});
            route["result"]["binding"]["inbound"]["config"]["artifact_policy"] = policy.clone();
            route["result"]["binding"]["outbound"][0]["config"]["artifact_policy"] = policy;
        }
        topology["coauthor"] = json!({"owner":"world_provider_test","source":"system_provider_author_test","door":"system_provider_file_test","tool":"file.write.provider","call_label":"tool.call","result_label":"file.write.provider.result"});
        (topology, descriptor_pins, returns)
    })();
    let topology_bytes = serde_json::to_vec(&topology).unwrap();
    let topology_file =
        InterfaceArtifactFile::write(root.join("topology"), "code-mode.topology", &topology_bytes)
            .unwrap();
    let topology_ref = super::continuation::artifact_ref(&topology_file).unwrap();
    let config_path = interface_dir.join("code_mode.json");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["context"] = json!({"mode":"persistent","prelude_artifact":prelude});
    config["continuation"] = json!({"mode":"admitted_artifact","resume_tool":"code_mode.resume","notification_interface":"world_operator_test"});
    config["handoff"] = json!({"topology_artifact":topology_ref,"wait_tool":"code_mode.wait"});
    config["routes"] = json!(topology["routes"].as_array().unwrap().iter().map(|route| json!({"tool":route["tool"],"interface":route["call"]["binding"]["outbound"][0]["interface"],"checkpoint":route["call"]["name"],"result_checkpoint":route["result"]["name"]})).collect::<Vec<_>>());
    config["default_yield_time_ms"] = json!(20);
    std::fs::write(config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut tools = tool_ids();
    tools.extend([
        "browser.tabs".into(),
        "file.write".into(),
        "file.write.provider".into(),
        "gui.app_state".into(),
        "gui.app_resume".into(),
    ]);
    tools.extend(browser_sdk_tools.iter().map(|tool| tool.to_string()));
    tools.extend(["browser.wait_complete".into(),"browser.download_wait".into(),"artifact.read".into()]);
    let (formation, kind) = formation_with_tools(&interface_dir, tools.clone());
    let limits = octopus_tool_door_runtime::InterfaceHostIoLimits {
        max_control_frame_bytes: 1024 * 1024,
        max_stderr_bytes: 256 * 1024,
        max_manifest_bytes: 1024 * 1024,
        max_payload_bytes: 16 * 1024 * 1024,
    };
    let mut door = octopus_tool_door_runtime::owner_contract_tests::DoorFixture::new(
        &root,
        InterfaceId::new(DOOR),
        interface_dir.clone(),
        limits,
    )
    .unwrap();
    door.cache(&prelude_file).unwrap();
    door.cache(&topology_file).unwrap();
    let input_bytes = [0xff, 0x00, 0x81, b'X'];
    let input_file = InterfaceArtifactFile::write(root.join("input-payload"), "fixture.input.payload", &input_bytes).unwrap();
    let input_ref = super::continuation::artifact_ref(&input_file).unwrap();
    door.cache(&input_file).unwrap();
    let mut roles = Vec::new();
    if generated_only {
        for index in 0..4 {
            let role = InterfaceArtifactFile::write(root.join("roles"), "context.reference", &serde_json::to_vec(&json!({"label":"context.reference","id":format!("fixture-role-{index}"),"text":"Mandatory worker role"})).unwrap()).unwrap();
            roles.push(door.cache(&role).unwrap());
        }
    }
    let durable = door.durable_dir(&formation).unwrap();
    let descriptor:octopus_interfaces::InterfaceDescriptor=serde_json::from_value(json!({"id":DOOR,"interface_type":"system","role":"system","direction":"bidirectional","source_names":["code_mode"],"interface_driver_ref":"code_mode_env","interface_driver_config":{"private_config":{"run_authoring_profile":{"file_write_tool":"file.write","file_write_label":"tool.call","file_write_result_label":"file.write.result","result_timeout_ms":30000}}}})).unwrap();
    let mut transport = door.configure_owner_transport(
        BTreeMap::from([("run_records_root".into(), records.clone())]),
        descriptor,
    );
    let provider_directory = root.join("provider-interface");
    std::fs::create_dir_all(&provider_directory).unwrap();
    let mut provider_descriptor:octopus_interfaces::InterfaceDescriptor=serde_json::from_value(json!({"id":"world_provider_test","interface_type":"world","role":"provider","direction":"bidirectional","source_names":["fixture"],"interface_driver_ref":"fixture","interface_driver_config":{"private_config":{"model_hidden_tools":["file.write.provider"],"run_authoring_profile":{"file_write_tool":"file.write.provider","file_write_label":"tool.call","file_write_result_label":"file.write.provider.result","result_timeout_ms":30000},"context_output_labels":["provider.fixture.snapshot"],"session_snapshot_labels":["provider.fixture.snapshot"]}}})).unwrap();
    if matches!(case, FrontierCase::DetachedResult | FrontierCase::DetachedFailure | FrontierCase::InterleavedResult | FrontierCase::UnsubmittedResult | FrontierCase::RefusedFailure | FrontierCase::RefusedFailurePretty | FrontierCase::NestedFailure) {
        provider_descriptor.interface_driver_config.as_mut().unwrap().private_config["context_output_labels"] = json!(["provider.fixture.snapshot", "tool.call"]);
    }
    let control = root.join("control");std::fs::create_dir_all(&control).unwrap();
    let listener = tokio::net::UnixListener::bind(octopus_interface_activation_app_server_transport::interface_control_socket_path_from_root(&control,&InterfaceId::new("system_provider_author_test"))).unwrap();
    let (provider_arrivals,mut provider_transport) = tokio::sync::mpsc::channel(8);
    let mut provider_context = octopus_interface_transport::InterfaceLiftContext::unprivileged(InterfaceId::new("world_provider_test"),provider_directory,Some(provider_descriptor.clone()),root.join("provider-var"),root.join("movements.jsonl"),root.join("blobs"),root.join("ledger-index.sqlite"),BTreeMap::from([("run_records_root".into(),records.clone()),("interface_hand_in_root".into(),root.join("provider-hand-in")),("app_server_control_root".into(),control)]),provider_arrivals,tokio_util::sync::CancellationToken::new());
    std::fs::create_dir_all(&provider_context.var_dir).unwrap();
    provider_context.host_io_limits = Some(octopus_interface_transport::InterfaceHostIoLimits { max_control_frame_bytes:limits.max_control_frame_bytes,max_stderr_bytes:limits.max_stderr_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes });
    let provider = FrontierProvider { context:provider_context,descriptor:provider_descriptor };
    let source_provenance = (|| {
        let bindings = json!([{"id":prelude.id},{"id":topology_ref.id},{"id":input_ref.id}]);
        let checkpoint = |id: &str, from: &str, to: &str| json!({"id":id,"run":"run_code_mode_test","trace":"trace:code-mode-owner-contract","epoch":0,"interface_binding":binding(from,to)["inbound"],"outbound_interface_bindings":binding(from,to)["outbound"],"artifact_bindings":bindings,"required_verified_artifacts":[]});
        let mut run = json!({"id":"run_code_mode_test","ee":"ee_code_mode_test","worker":WORKER,"trace":"trace:code-mode-owner-contract","status":"running","interface_state":{DOOR:{"frontier_checkpoint_ids":["initial-owner","initial-authoring"]}},
            "checkpoints":[checkpoint("model-input","world_provider_test",DOOR),checkpoint("operator-input","world_operator_test",DOOR),checkpoint("initial-owner",DOOR,DOOR),checkpoint("initial-authoring","system_file_test",DOOR)],
            "active_checkpoints":["model-input","operator-input","initial-owner","initial-authoring"]});
        for (name, _) in &returns {
            let route = topology["routes"].as_array().unwrap().iter().find(|route| route["result"]["name"] == *name).unwrap();
            let mut row = checkpoint(name,route["result"]["binding"]["inbound"]["interface"].as_str().unwrap(),DOOR);
            row["interface_binding"] = route["result"]["binding"]["inbound"].clone();
            row["outbound_interface_bindings"] = route["result"]["binding"]["outbound"].clone();
            run["checkpoints"].as_array_mut().unwrap().push(row);
            run["active_checkpoints"].as_array_mut().unwrap().push(json!(name));
        }
        run["checkpoints"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|cp| cp["id"] == "operator-input")
            .unwrap()["interface_binding"]["config"]["artifact_policy"] =
            json!({"mode":"allow","names":["operator.message"],"namespaces":[]});
        let provider_tool_requirement = json!([{"mark":"tool.call.allowed","artifact_label":"tool.call","verifier":"tool_call_allowed","params":{"tools":["code_mode.run","code_mode.wait","code_mode.cancel"]}}]);
        let templates = vec![
            ("model-input","provider_tool_output","world_provider_test",DOOR,false),
            ("provider-output","provider_artifact_output","world_provider_test","world_provider_projection_test",false),
            ("provider-input","stable","world_provider_input_test","world_provider_test",true),
            ("provider-authoring-call","authoring_call","system_provider_author_test","system_provider_file_test",false),
            ("provider-authoring-result","authoring_result","system_provider_file_test","world_provider_test",false),
        ];
        let mut bound = Vec::new();
        for (id,role,from,to,context) in templates {
            let mut row = checkpoint(id,from,to);
            row["artifact_bindings"] = if id == "model-input" { bindings.clone() } else if context { json!([{"id":input_ref.id}]) } else { json!([]) };
            if id == "model-input" { row["required_verified_artifacts"] = provider_tool_requirement.clone(); }
            if id == "provider-authoring-call" { row["required_verified_artifacts"] = json!([{"mark":"tool.call.allowed","artifact_label":"tool.call","verifier":"tool_call_allowed","params":{"tools":["file.write.provider"]}}]); }
            if id == "provider-output" { row["interface_binding"]["config"]["artifact_policy"] = json!({"mode":"allow","names":["provider.fixture.progress"],"namespaces":[]}); }
            let mut template = json!({"name":id,"role":role,"interface_binding":row["interface_binding"],"outbound_interface_bindings":row["outbound_interface_bindings"],"include_context":context,"required_verified_artifacts":row["required_verified_artifacts"]});
            if id == "model-input" { template["static_artifact_bindings"] = json!([prelude.id,topology_ref.id,input_ref.id]); }
            bound.push(template);
            if id == "model-input" { *run["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp| cp["id"]==id).unwrap()=row; }
            else { run["checkpoints"].as_array_mut().unwrap().push(row);run["active_checkpoints"].as_array_mut().unwrap().push(json!(id)); }
        }
        let mut owner_input = json!({"name":"owner-output","role":"owner_input","interface_binding":binding(DOOR,"world_provider_test")["inbound"],"outbound_interface_bindings":binding(DOOR,"world_provider_test")["outbound"],"owner_authoring":{"owner":DOOR,"source":DOOR,"door":"system_file_test","tool":"file.write","call_label":"tool.call","result_label":"file.write.result"},"include_context":true,"required_verified_artifacts":[]});
        if matches!(case, FrontierCase::NestedFailure) {
            owner_input["include_context"] = json!(false);
            owner_input["static_artifact_bindings"] = json!([prelude.id,topology_ref.id,input_ref.id]);
        }
        bound.push(owner_input);
        run["interface_state"]["world_provider_test"] = json!({"context_generation":0,"authoring_generation":0,"frontier_checkpoint_ids":["model-input","provider-output","provider-authoring-call","provider-authoring-result"],"run_authoring_binding":{"schema_version":2,"run":"run_code_mode_test","worker":WORKER,"provider_interface":"world_provider_test","authoring_source_interface":"system_provider_author_test","payload_artifact_bindings":[input_ref],"checkpoints":bound}});
        run["interface_state"][DOOR]["owned_checkpoint_ids"] = json!(["initial-owner".to_string(),"initial-authoring".to_string()].into_iter().chain(returns.keys().cloned()).collect::<Vec<_>>());
        let source_provenance = Value::Array([
            ("artifact-code-mode-cua-prelude", &prelude_source),
            ("artifact-code-mode-browser-prelude", &browser_source),
        ].into_iter().map(|(id, body)| {
            use sha2::{Digest, Sha256};
            let bytes = std::fs::read(registry_root().join("artifact").join(id).join("artifact.json")).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), *body);
            let content = octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(body));
            let source = octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new(id),content };
            let snapshot = octopus_ledger::ArtifactRef { id:octopus_descriptors::ArtifactId::new(format!("{id}__fixture_source")),content };
            std::fs::write(root.join(format!("{}.json",snapshot.id)),&bytes).unwrap();
            json!({"source":source,"snapshot":snapshot,"source_sha256":format!("{:x}",Sha256::digest(&bytes))})
        }).collect());
        run["interface_state"][DOOR]["prelude_sources"] = source_provenance.clone();
        if generated_only {
            let mut observer=checkpoint("00-observer","system_browser_test",DOOR);
            observer["outbound_interface_bindings"]=json!([]);observer["artifact_bindings"]=json!([]);
            run["checkpoints"].as_array_mut().unwrap().insert(0,observer);
            run["active_checkpoints"].as_array_mut().unwrap().insert(0,json!("00-observer"));
        }
        for checkpoint in run["checkpoints"].as_array_mut().unwrap() {
            let bindings = checkpoint["artifact_bindings"].as_array_mut().unwrap();
            bindings.splice(0..0, roles.iter().map(|role| json!({"id":role.id})));
        }
        if matches!(case, FrontierCase::DetachedResult | FrontierCase::DetachedFailure | FrontierCase::InterleavedResult | FrontierCase::UnsubmittedResult | FrontierCase::RefusedFailure | FrontierCase::RefusedFailurePretty) {
            let writer = octopus_run_authoring::separate_writer_run(
                &mut run, &InterfaceId::new("world_provider_test"), &RunId::new("run_detached_provider_writer"),
            ).unwrap();
            let bytes = octopus_run_authoring::JsonFormat::Compact.encode(&writer).unwrap();
            std::fs::write(records.join("run_detached_provider_writer.json"), bytes).unwrap();
        }
        std::fs::write(
            records.join("run_code_mode_test.json"),
            serde_json::to_vec_pretty(&run).unwrap(),
        )
        .unwrap();
        source_provenance
    })();
    let interfaces = [
        DOOR,
        NESTED_INTERFACE,
        "system_browser_test",
        "system_context_test",
        "system_file_test",
        "world_provider_test",
        "world_operator_test",
        "system_gui_test",
        "system_provider_author_test","system_provider_file_test","world_provider_input_test","world_provider_projection_test",
    ];
    let policy = std::sync::Arc::new(FrontierPolicy {
        descriptor_pin: octopus_policy::FormationPin { inputs: descriptor_pins },
        ceiling: ContextGrant {
            tools: CapSet::from_allowed(tools.iter().map(ToolId::new)),
            interfaces: CapSet::from_allowed(interfaces.iter().copied().map(InterfaceId::new)),
            wek: CapSet::from_allowed(
                [
                    KIND,
                    "system_room",
                    "process_session",
                    "browser_session",
                    "desktop_session",
                ]
                .map(WorkerEnvironmentKindId::new),
            ),
            artifact_verbs: CapSet::from_allowed([ArtifactVerb::Read]),
            mark_outcomes: CapSet::from_allowed(
                ["operator.approval", "operator.input.provided", "tool.call.allowed"]
                    .map(octopus_descriptors::MarkId::new),
            ),
            ..Default::default()
        },
        affordances: [NESTED_TOOL, "browser.tabs", "gui.app_state", "browser.wait_complete", "browser.download_wait", "artifact.read"].into_iter().chain(browser_sdk_tools)
            .map(|id| InterfaceToolAffordance {
                name: ToolId::new(id),
                description: raw_descriptor(id).description,
                args_schema: raw_descriptor(id).args_schema,
            })
            .collect(),
        blobs: root.join("blobs"),
        contents: [
            (prelude.id.clone(), prelude.content),
            (topology_ref.id.clone(), topology_ref.content),
            (input_ref.id.clone(), input_ref.content),
        ].into_iter().chain(roles.iter().map(|role| (role.id.clone(), role.content))).collect(),
        roles,
    });
    let runtime = Runtime::open(RuntimeServices {
        interface_catalog: std::sync::Arc::new(RuntimeInterfaceCatalog::from_classes(
            interfaces.map(|id| {
                (
                    InterfaceId::new(id),
                    InterfaceClass::new(
                        if id == "world_provider_test" || id == "world_operator_test" {
                            InterfaceType::World
                        } else {
                            InterfaceType::System
                        },
                        InterfaceRuntimeDisposition::ExternalReader,
                    )
                    .unwrap(),
                )
            }),
        )),
    })
    .unwrap();
    runtime.set_run_records_dir(records.clone());
    runtime.set_run_grant_resolver(policy.clone());
    runtime.set_context_artifact_resolver(policy);
    runtime.set_verified_artifact_admission(std::sync::Arc::new(FrontierOperatorVerifier {
        blobs: root.join("blobs"),
        scratch: root.join("verifier-scratch"),
    }));
    let admission = std::sync::Arc::new(FrontierAdmission {
        runtime,
        provider,
        door: tokio::sync::Mutex::new(door),
        retained_arrivals: std::sync::Arc::new(Mutex::new(octopus_ledger_index::ArrivalDedupIndex::new())),
        deliveries: Mutex::new(vec![]),
        changed: tokio::sync::Notify::new(),
        root: root.clone(),
        run_records: records,
        file_writes: std::sync::atomic::AtomicUsize::new(0),
        conflict_next: AtomicBool::new(false),
        reject_next_owner_arrival: AtomicBool::new(false),
        fail_after_nested_output: AtomicBool::new(false),
        interleave_next_write: AtomicBool::new(false),
        provider_wins_next_write: AtomicBool::new(false),
        provider_writes_before_owner_call: AtomicBool::new(false),
        probe_unsubmitted_output: AtomicBool::new(false),
        drop_owner_ready_ack: AtomicBool::new(false),
        pending_output_proof: Mutex::new(None),
        output_prepared: tokio::sync::Notify::new(),
        provider_result_release: tokio::sync::Notify::new(),
        owner_write_prepared: tokio::sync::Notify::new(),
        owner_write_admitted: tokio::sync::Notify::new(),
        peer_write_admitted: tokio::sync::Notify::new(),
        peer_write_applied: tokio::sync::Notify::new(),
        owner_write_completed: tokio::sync::Notify::new(),
    });
    let transport_owner = admission.clone();
    let pump = tokio::task::spawn_local(async move {
        loop {
            transport_owner.transport(transport.next().await?).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    });
    let provider_socket = tokio::spawn(actual_provider_authoring_socket(admission.clone(),listener));
    let provider_sink = admission.clone();
    let provider_pump = tokio::task::spawn_local(async move {
        loop {
            let output = provider_transport.recv().await.ok_or("provider fixture arrival channel closed")?;
            if output.interface.as_str() != "world_provider_test" || !output.evidence.is_empty() { return Err("fixture provider attempted an unowned arrival".to_string()); }
            match (output.verdict_tx, output.retained) {
                (None, Some(retained)) => {
                    let delivery = provider_sink.arrive_retained("world_provider_test", &output.artifact, &retained.id).await;
                    retained.reply.send(delivery).map_err(|_| "provider fixture dropped its retained hand-in receipt")?;
                }
                (Some(reply), None) => {
                    let seq = provider_sink.arrive("world_provider_test", &output.artifact).await?;
                    reply.send(DeliveryVerdict::Admitted { seq }).map_err(|_| "provider fixture dropped its hand-in receipt")?;
                }
                _ => return Err("provider fixture must select one hand-in receipt contract".into()),
            }
        }
        #[allow(unreachable_code)] Ok::<(),String>(())
    });
    let handoff = if matches!(case, FrontierCase::RefusedFailurePretty) {
        actual_owner_handoff::OwnerHandoff::with_json_format(None)
    } else { actual_owner_handoff::OwnerHandoff::new() };
    let room = std::sync::Arc::new(CodeModeRoom::new(ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        var_dir: root.join("room-var"),
        durable_var_dir: Some(durable.clone()),
        host_io_limits: limits,
        descriptor_snapshot: None,
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    }));
    if matches!(case, FrontierCase::Generated | FrontierCase::Application) {
        if generated_only {
            Box::pin(actual_generated_artifact_read(&admission,&room,&handoff,&formation,&kind,&root)).await;
        } else {
            let input = InterfaceArtifactFile::write(root.join("approval-input"), "operator.message", br#"{"text":"Use the declared application"}"#).unwrap();
            admission.arrive("world_provider_input_test", &input).await.unwrap();
            for (sequence, result_before_decision) in [(304, true), (310, false)] {
                Box::pin(actual_gui_decision_preserves_the_original_promise(&admission,&room,&handoff,&formation,&kind,&root,sequence,result_before_decision,false)).await;
            }
        }
        room.release_room(&formation).await.unwrap();
        admission.door.lock().await.context.owner_cancel().cancel();
        provider_socket.abort(); provider_pump.abort(); pump.abort();
        let _ = tokio::join!(provider_socket,provider_pump,pump);
        return;
    }
    let run_path_before_input = admission.run_records.join("run_code_mode_test.json");
    let selected_source = std::fs::read(&run_path_before_input).unwrap();
    let mut nonselected: Value = serde_json::from_slice(&selected_source).unwrap();
    nonselected["interface_state"]["world_provider_test"]["run_authoring_binding"].as_object_mut().unwrap().remove("payload_artifact_bindings");
    std::fs::write(&run_path_before_input, serde_json::to_vec_pretty(&nonselected).unwrap()).unwrap();
    let malformed_input = InterfaceArtifactFile::write(root.join("provider-nonselected-input"), "operator.message", br#"{"text":"Unselected binary must remain a parser error"}"#).unwrap();
    admission.arrive("world_provider_input_test", &malformed_input).await.unwrap();
    let error = match admission.provider_handoff() { Err(error) => error, Ok(_) => panic!("unselected binary context was silently skipped") };
    assert!(error.contains("not JSON"), "{error}");
    let mut selected: Value = serde_json::from_slice(&selected_source).unwrap();
    selected["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp|cp["id"] == "provider-input").unwrap()["epoch"] = json!(1);
    std::fs::write(&run_path_before_input, serde_json::to_vec_pretty(&selected).unwrap()).unwrap();
    let provider_input = InterfaceArtifactFile::write(root.join("provider-initial-input"), "operator.message", br#"{"text":"Use the declared immutable input"}"#).unwrap();
    admission.arrive("world_provider_input_test", &provider_input).await.unwrap();
    let stable = admission.deliveries.lock().unwrap().iter().rev().find(|delivery| delivery.from.as_str() == "world_provider_input_test" && delivery.to.as_str() == "world_provider_test").cloned().unwrap();
    let pinned = octopus_run_record::RunRecord::from_bytes(&std::fs::read(admission.run_records.join("run_code_mode_test.json")).unwrap()).unwrap();
    let payloads = vec![octopus_interfaces::ArtifactBindingPayload::new(input_ref.clone(), input_bytes.to_vec())];
    assert!(octopus_run_authoring::context_artifact_payloads(&pinned, &stable.metadata, &payloads).unwrap().is_empty());
    let mut changed = payloads.clone(); changed[0].artifact.content = ContentHash::of_bytes(b"different manifest");
    assert!(octopus_run_authoring::context_artifact_payloads(&pinned, &stable.metadata, &changed).is_err());
    let mut denied = stable.metadata.clone(); denied.grant.artifact_access.get_mut(input_ref.id.as_str()).unwrap().read = false;
    assert!(octopus_run_authoring::context_artifact_payloads(&pinned, &denied, &payloads).is_err());
    let mut role_collision = stable.metadata.clone(); role_collision.grant.role_artifacts.push(input_ref.id.as_str().to_string());
    assert!(octopus_run_authoring::context_artifact_payloads(&pinned, &role_collision, &payloads).is_err());
    let source = envelope(
        &root,
        300,
        "code_mode.run",
        json!({"script":if matches!(case, FrontierCase::InterleavedResult | FrontierCase::UnsubmittedResult) { "const docs = await prelude.cua.documentation(); return docs;" } else { "let sum = prelude.seed; let both = await Promise.all([tools.process_list({}), prelude.browser.tabs.selected()]); if (both[1].id !== 't' || typeof both[1].info !== 'function') throw new Error('browser facade lost its admitted Tab closure'); sum.value += both[0].body.value + both[1].id.length; notify('two exact doors'); text(sum.value);" },"yield_time_ms":if matches!(case, FrontierCase::InterleavedResult | FrontierCase::UnsubmittedResult) { 1000 } else { 20 }}),
    );
    if matches!(case, FrontierCase::DetachedResult | FrontierCase::DetachedFailure | FrontierCase::InterleavedResult | FrontierCase::UnsubmittedResult | FrontierCase::RefusedFailure | FrontierCase::RefusedFailurePretty) {
        let handoff = admission.provider_handoff().unwrap().unwrap();
        handoff.hand(octopus_run_authoring::ProviderEmission {
            artifact: InterfaceArtifactFile::write(admission.provider.context.var_dir.join("model-call"), "tool.call", &source.artifact_file.read_payload_bytes().unwrap()).unwrap(),
            route: octopus_run_authoring::ProviderEmissionRoute::Tool { tool: "code_mode.run".into() },
        }).await.unwrap();
    } else {
        admission.model_call(&source.artifact_file).await.unwrap();
    }
    let initial = admission.next().await.unwrap();
    let parsed_config: super::config::CodeModeConfig = serde_json::from_value(config.clone()).unwrap();
    let original_topology: super::config::CodeModeOwnerTopology = serde_json::from_value(topology.clone()).unwrap();
    parsed_config.validate_owner_topology(&original_topology,&initial.2).unwrap();
    let coauthor = original_topology.coauthor.as_ref().unwrap();
    let metadata = initial.2.metadata().unwrap();
    let snapshot = std::fs::read(root.join("blobs").join(metadata.runtime.as_ref().unwrap().run_record_hash.as_ref().unwrap())).unwrap();
    octopus_run_authoring::validate_admitted_run_writer(&snapshot, metadata, coauthor).unwrap();
    if matches!(case, FrontierCase::DetachedResult | FrontierCase::DetachedFailure | FrontierCase::InterleavedResult | FrontierCase::UnsubmittedResult | FrontierCase::RefusedFailure | FrontierCase::RefusedFailurePretty) {
        admission.provider_handoff().unwrap().unwrap().recover_source().await.unwrap();
        let target_path = admission.run_records.join("run_code_mode_test.json");
        let target = std::fs::read(&target_path).unwrap();
        let target_hash = ContentHash::of_bytes(&target).to_hex();
        if matches!(case, FrontierCase::UnsubmittedResult) {
            admission.probe_unsubmitted_output.store(true, Ordering::SeqCst);
            admission.drop_owner_ready_ack.store(true, Ordering::SeqCst);
            let peer = admission.clone();
            let progress = tokio::spawn(async move {
                peer.output_prepared.notified().await;
                peer.provider_progress(776).await
            });
            let completed = match Box::pin(admission.run(&room, &handoff, &formation, &kind, initial)).await {
                Ok(completed) => completed,
                Err(error) => {
                    if pump.is_finished() { panic!("{error}; owner pump: {:?}", pump.await); }
                    if provider_socket.is_finished() { panic!("{error}; provider socket: {:?}", provider_socket.await); }
                    panic!("{error}");
                }
            };
            progress.await.unwrap().unwrap();
            assert!(completed["body"]["error_text"].as_str().unwrap().contains("Illegal return"), "{completed}");
            room.release_room(&formation).await.unwrap();
            admission.door.lock().await.context.owner_cancel().cancel();
            provider_socket.abort(); provider_pump.abort(); pump.abort();
            let _ = tokio::join!(provider_socket, provider_pump, pump);
            return;
        }
        if !matches!(case, FrontierCase::InterleavedResult) {
            assert!(!root.join("blobs").join(&target_hash).exists());
        }
        if matches!(case, FrontierCase::InterleavedResult) {
            admission.interleave_next_write.store(true, Ordering::SeqCst);
            let peer = admission.clone();
            let progress = tokio::spawn(async move {
                peer.owner_write_admitted.notified().await;
                peer.provider_progress(777).await
            });
            let first = Box::pin(admission.run(&room, &handoff, &formation, &kind, initial)).await.unwrap();
            assert_eq!(first["body"]["state"], "completed", "{first}");
            assert!(first["body"]["error_text"].as_str().unwrap().contains("Illegal return"), "{first}");
            if let Err(error) = progress.await.unwrap() {
                if provider_socket.is_finished() { panic!("{error}; socket: {:?}", provider_socket.await); }
                panic!("{error}");
            }
            let ctx = admission.door.lock().await.context.clone();
            {
                let rows = admission.deliveries.lock().unwrap();
                let call = rows.iter().find(|row| row.from.as_str() == DOOR && row.to.as_str() == "system_file_test").unwrap();
                let result = rows.iter().find(|row| row.seq > call.seq && row.from.as_str() == "system_file_test").unwrap();
                let peer_call = rows.iter().find(|row| row.seq > call.seq && row.to.as_str() == "system_provider_file_test").unwrap();
                let peer_result = rows.iter().find(|row| row.seq > peer_call.seq && row.from.as_str() == "system_provider_file_test").unwrap();
                assert!(call.seq < peer_call.seq && peer_call.seq < result.seq && result.seq < peer_result.seq);
                let refused: Value = serde_json::from_slice(&peer_result.file.read_payload_bytes().unwrap()).unwrap();
                assert_eq!(refused["ok"], false);
                assert_eq!(refused["error"]["code"], "file_write_incomplete");
                exact_install_does_not_claim_later_state(&ctx, &target_path, &durable, call, result, coauthor);
            }
            let next = envelope(&root, 301, "code_mode.run", json!({"script":"globalThis.cuaDocs = await prelude.cua.unknownFixtureMethod(); text(typeof cuaDocs);", "yield_time_ms":1000}));
            Box::pin(admission.model_call(&next.artifact_file)).await.unwrap();
            let input = admission.next().await.unwrap();
            admission.provider_wins_next_write.store(true, Ordering::SeqCst);
            admission.interleave_next_write.store(true, Ordering::SeqCst);
            let peer = admission.clone();
            let progress = tokio::spawn(async move {
                peer.owner_write_prepared.notified().await;
                peer.provider_progress(778).await
            });
            let second = Box::pin(admission.run(&room, &handoff, &formation, &kind, input)).await.unwrap();
            progress.await.unwrap().unwrap();
            admission.provider_wins_next_write.store(false, Ordering::SeqCst);
            assert_eq!(second["body"]["state"], "completed", "{second}");
            assert!(second["body"]["error_text"].as_str().unwrap().contains("unknownFixtureMethod is not a function"), "{second}");
            let next = envelope(&root, 302, "code_mode.run", json!({"script":"text('NEXT_CODE_OK');", "yield_time_ms":1000}));
            Box::pin(admission.model_call(&next.artifact_file)).await.unwrap();
            let input = admission.next().await.unwrap();
            admission.provider_wins_next_write.store(true, Ordering::SeqCst);
            admission.provider_writes_before_owner_call.store(true, Ordering::SeqCst);
            admission.interleave_next_write.store(true, Ordering::SeqCst);
            let peer = admission.clone();
            let progress = tokio::spawn(async move {
                peer.owner_write_prepared.notified().await;
                peer.provider_progress(779).await
            });
            let third = Box::pin(admission.run(&room, &handoff, &formation, &kind, input)).await.unwrap();
            progress.await.unwrap().unwrap();
            admission.provider_wins_next_write.store(false, Ordering::SeqCst);
            admission.provider_writes_before_owner_call.store(false, Ordering::SeqCst);
            assert_eq!(third["body"]["state"], "completed", "{third}");
            assert_eq!(third["body"]["output"][0]["text"], "NEXT_CODE_OK", "{third}");
            room.release_room(&formation).await.unwrap();
            admission.door.lock().await.context.owner_cancel().cancel();
            provider_socket.abort(); provider_pump.abort(); pump.abort();
            let _ = tokio::join!(provider_socket, provider_pump, pump);
            return;
        }
        if matches!(case, FrontierCase::DetachedFailure | FrontierCase::RefusedFailure | FrontierCase::RefusedFailurePretty) {
            if matches!(case, FrontierCase::RefusedFailure | FrontierCase::RefusedFailurePretty) {
                Box::pin(failure_return_tests::actual_refused_failure_return(&admission, &room, &handoff, &formation, &kind, &parsed_config, &initial, &durable)).await;
            } else {
                Box::pin(actual_owner_failure_return(&admission, &room, &handoff, &formation, &parsed_config, &initial, &durable)).await;
            }
            admission.door.lock().await.context.owner_cancel().cancel();
            provider_socket.abort(); provider_pump.abort(); pump.abort();
            let _ = tokio::join!(provider_socket, provider_pump, pump);
            return;
        }
        let started = Box::pin(admission.run(&room, &handoff, &formation, &kind, initial)).await.unwrap();
        assert_eq!(started["body"]["state"], "yielded", "{started}");
        let installed: super::continuation::InstalledHandoff = super::continuation::read_record(&durable.join(format!("{target_hash}.installed.json"))).unwrap();
        let install = octopus_run_authoring::CheckedRunInstall {
            call: octopus_run_authoring::ConsentEvidenceRef { artifact: installed.source_artifact.clone(), movement_seq: installed.source_seq },
            result: octopus_run_authoring::ConsentEvidenceRef { artifact: installed.result_artifact.clone(), movement_seq: installed.result_seq },
        };
        let ctx = admission.door.lock().await.context.clone();
        let evidence = octopus_run_authoring::CheckedRunEvidence {
            movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
            max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
        };
        let checked = |path: &Path, proof: &octopus_run_authoring::CheckedRunInstall| {
            octopus_run_authoring::checked_declared_run_install(&evidence, path, &snapshot, proof, vec![coauthor.clone()])
        };
        assert_eq!(checked(&target_path, &install).unwrap().after, target);
        assert!(checked(&admission.run_records.join("foreign.json"), &install).is_err());
        let mut wrong = install.clone(); wrong.result.artifact.content = ContentHash::of_bytes(b"wrong result");
        assert!(checked(&target_path, &wrong).is_err());
        wrong = install.clone(); wrong.result.movement_seq = wrong.call.movement_seq;
        assert!(checked(&target_path, &wrong).is_err());
        let latest = std::fs::read_dir(&durable).unwrap().filter_map(|entry| {
            let path = entry.unwrap().path();
            path.file_name().unwrap().to_str().unwrap().ends_with(".installed.json").then(|| {
                let record: super::continuation::InstalledHandoff = super::continuation::read_record(&path).unwrap(); (path, record)
            })
        }).max_by_key(|(_, record)| record.result_seq).unwrap();
        let saved = std::fs::read(&latest.0).unwrap();
        let mut bad = latest.1; bad.after_hash = ContentHash::of_bytes(b"wrong target hash").to_hex();
        std::fs::write(&latest.0, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(super::continuation::reconcile_checked_provider_advance(&ctx, &durable, &parsed_config, None).is_err());
        std::fs::write(&latest.0, saved).unwrap();
        room.release_room(&formation).await.unwrap();
        ctx.owner_cancel().cancel();
        provider_socket.abort(); provider_pump.abort(); pump.abort();
        let _ = tokio::join!(provider_socket, provider_pump, pump);
        return;
    }
    let mut detached: Value = serde_json::from_slice(&snapshot).unwrap();
    octopus_run_authoring::separate_writer_run(&mut detached, &coauthor.owner, &RunId::new("run_separate_provider_writer")).unwrap();
    let detached_bytes = serde_json::to_vec_pretty(&detached).unwrap();
    let mut detached_input = initial.2.clone();
    let detached_metadata = detached_input.metadata.as_mut().unwrap();
    detached_metadata.runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(&detached_bytes).to_hex());
    detached_metadata.grant.interfaces.retain(|id| id != coauthor.source.as_str() && id != coauthor.door.as_str());
    detached_metadata.grant.tools.retain(|tool| tool != &coauthor.tool);
    parsed_config.validate_owner_topology(&original_topology, &detached_input).unwrap();
    octopus_run_authoring::validate_admitted_run_writer(&detached_bytes, detached_input.metadata().unwrap(), coauthor).unwrap();
    detached_input.metadata.as_mut().unwrap().runtime.as_mut().unwrap().epoch += 1;
    assert!(octopus_run_authoring::validate_admitted_run_writer(&detached_bytes, detached_input.metadata().unwrap(), coauthor).is_err());
    detached_input.metadata.as_mut().unwrap().runtime.as_mut().unwrap().epoch -= 1;
    assert!(octopus_run_authoring::validate_admitted_run_writer(&snapshot, detached_input.metadata().unwrap(), coauthor).is_err());
    let mut foreign_writer = coauthor.clone(); foreign_writer.source = InterfaceId::new("system_foreign_writer");
    assert!(octopus_run_authoring::validate_admitted_run_writer(&detached_bytes, detached_input.metadata().unwrap(), &foreign_writer).is_err());
    detached["interface_state"][coauthor.owner.as_str()]["run_authoring_binding"]["writer_run"]["initial_record"]["ee"] = json!("ee_writer_elsewhere");
    let elsewhere_bytes = serde_json::to_vec_pretty(&detached).unwrap();
    detached_input.metadata.as_mut().unwrap().runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(&elsewhere_bytes).to_hex());
    octopus_run_authoring::validate_admitted_run_writer(&elsewhere_bytes, detached_input.metadata().unwrap(), coauthor).unwrap();
    if matches!(case, FrontierCase::Coauthor) {
        room.release_room(&formation).await.unwrap();
        admission.door.lock().await.context.owner_cancel().cancel();
        provider_socket.abort(); provider_pump.abort(); pump.abort();
        let _ = tokio::join!(provider_socket, provider_pump, pump);
        return;
    }
    let mut wrong = original_topology.clone(); wrong.routes[0].process_descriptor.process.bin.push_str("-foreign");
    assert!(parsed_config.validate_owner_topology(&wrong,&initial.2).is_err());
    let mut overlap = topology.clone(); overlap["routes"][0]["result"]["binding"]["inbound"]["config"]["artifact_policy"] = topology["routes"][1]["result"]["binding"]["inbound"]["config"]["artifact_policy"].clone();
    assert!(parsed_config.validate_owner_topology(&serde_json::from_value(overlap).unwrap(),&initial.2).is_err());
    let initial_seq = initial.0;
    let started = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        admission.run(&room, &handoff, &formation, &kind, initial),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(started["body"]["state"], "yielded", "{started}");
    if matches!(case, FrontierCase::NestedFailure | FrontierCase::ReadinessFailure) {
        let ctx = admission.door.lock().await.context.clone();
        octopus_worker_env::WorkerEnvironmentRecordStore::new(ctx.interface_var_dir()).activate(&formation, initial_seq).unwrap();
    }
    let cell = started["body"]["cell_id"].as_str().unwrap();
    let nested: Vec<_> = admission
        .deliveries
        .lock()
        .unwrap()
        .iter()
        .filter(|d| d.to.as_str() == NESTED_INTERFACE || d.to.as_str() == "system_browser_test")
        .cloned()
        .collect();
    assert_eq!(nested.len(), 2);
    let journal_ctx = admission.door.lock().await.context.clone();
    let first_call: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&nested[0].file.read_payload_bytes().unwrap()).unwrap();
    let addressed = super::continuation::prepared_call_for_output(&durable, &first_call, &formation).unwrap();
    assert_eq!(super::continuation::prepared_calls(&durable).unwrap().len(), 2);
    let corrupt_history = durable.join(format!("{}.pending.json", ContentHash::of_bytes(b"unrelated-corrupt-history").to_hex()));
    super::continuation::create_record(&corrupt_history, &json!({"corrupt":true})).unwrap();
    assert_eq!(super::continuation::prepared_call_for_output(&durable, &first_call, &formation).unwrap().pending_digest(), addressed.pending_digest());
    let corrupt_error = super::continuation::prepared_calls(&durable).err().unwrap();
    assert!(super::continuation::reconcile_checked_provider_advance(&journal_ctx, &durable, &parsed_config, None).unwrap().is_some());
    std::fs::remove_file(&corrupt_history).unwrap();
    let mut changed_call = first_call.clone(); changed_call.arguments = json!({"changed":true});
    assert!(super::continuation::prepared_call_for_output(&durable, &changed_call, &formation).is_err());
    let mut foreign_formation = formation.clone(); foreign_formation.ee.push_str("-foreign");
    assert!(super::continuation::prepared_call_for_output(&durable, &first_call, &foreign_formation).is_err());
    changed_call = first_call.clone(); changed_call.correlation_id = Some("code-mode-tool:foreign:invalid-digest".into());
    assert!(super::continuation::prepared_call_for_output(&durable, &changed_call, &formation).is_err());
    let addressed_path = durable.join(format!("{}.pending.json", addressed.pending_digest()));
    let addressed_bytes = std::fs::read(&addressed_path).unwrap(); std::fs::write(&addressed_path, b"{}").unwrap();
    assert!(super::continuation::prepared_call_for_output(&durable, &first_call, &formation).is_err());
    std::fs::write(&addressed_path, &addressed_bytes).unwrap();
    let completion_path = durable.join(format!("{}.accepted.json", addressed.pending_digest()));
    super::continuation::create_record(&completion_path, &json!({"corrupt":true})).unwrap();
    assert!(super::continuation::prepared_call_for_output(&durable, &first_call, &formation).is_err()); std::fs::remove_file(&completion_path).unwrap();
    for (index, delivery) in nested.iter().enumerate() {
        assert!(delivery.metadata.artifact_binding_refs.contains(&input_ref));
        let call: octopus_tool_handlers::ToolCallArtifact =
            serde_json::from_slice(&delivery.file.read_payload_bytes().unwrap()).unwrap();
        let result = octopus_tool_handlers::ToolResultArtifact::success(
            &call.tool,
            call.correlation_id,
            if call.tool == NESTED_TOOL {
                json!({"value":40})
            } else {
                assert_eq!(call.tool,"browser.tabs");
                assert_eq!(call.arguments,json!({}));
                json!({"result":"browser.tabs","worker_environment_id":"fixture-browser-room","current_target_id":"t","tabs":[{"target_id":"t","title":"Fixture","url":"about:blank","opener_id":null,"browser_context_id":"fixture-browser-context","lifetime":"temporary","model_controlled":true,"attached":true}]})
            },
        );
        let file = InterfaceArtifactFile::write(
            root.join("nested-results"),
            &result.label,
            &serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
        admission.arrive(delivery.to.as_str(), &file).await.unwrap();
        if index == 0 {
            let before = std::fs::read(admission.run_records.join("run_code_mode_test.json")).unwrap();
            admission.provider_progress(1).await.unwrap();
            let after = std::fs::read(admission.run_records.join("run_code_mode_test.json")).unwrap();
            super::continuation::create_record(&corrupt_history, &json!({"corrupt":true})).unwrap();
            assert_eq!(super::continuation::reconcile_checked_provider_advance(&journal_ctx, &durable, &parsed_config, None).err().unwrap().to_string(), corrupt_error);
            std::fs::remove_file(&corrupt_history).unwrap();
            let ctx = admission.door.lock().await.context.clone();
            let writer:octopus_run_authoring::CheckedRunWriter = serde_json::from_value(topology["coauthor"].clone()).unwrap();
            let limits = ctx.owner_io_limits().unwrap();
            let evidence = octopus_run_authoring::CheckedRunEvidence { movements_path:ctx.owner_movements_path(),ledger_index_path:ctx.owner_ledger_index_path(),blobs_dir:ctx.owner_blobs_dir(),max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes };
            let chain = octopus_run_authoring::checked_run_file_chain(&evidence,&admission.run_records.join("run_code_mode_test.json"),&before,&after,0,std::slice::from_ref(&writer)).unwrap();
            assert_eq!(chain.len(),1);
            octopus_run_authoring::validate_checked_provider_transition(&chain[0]).unwrap();
            let mut wrong = writer.clone();wrong.source = InterfaceId::new("unrelated_writer");
            assert!(octopus_run_authoring::checked_run_file_chain(&evidence,&admission.run_records.join("run_code_mode_test.json"),&before,&after,0,&[wrong]).is_err());
            assert!(octopus_run_authoring::checked_run_file_chain(&evidence,&admission.run_records.join("run_code_mode_test.json"),&before,&after,chain[0].call.seq.0,&[writer]).is_err());
        }
    }
    let first_return = admission.next().await.unwrap();
    let second_return = admission.next().await.unwrap();
    if matches!(case, FrontierCase::NestedFailure) {
        let phase_admission = admission.clone();
        tokio::spawn(async move {
            Box::pin(failure_return_tests::actual_nested_failure_return(&phase_admission, &handoff, &formation, &second_return, &first_return, &durable, &room, &kind, &parsed_config)).await;
            room.release_room(&formation).await.unwrap();
        }).await.unwrap();
        admission.door.lock().await.context.owner_cancel().cancel();
        provider_socket.abort(); provider_pump.abort(); pump.abort();
        let _ = tokio::join!(provider_socket, provider_pump, pump);
        return;
    }
    let run_path = admission.run_records.join("run_code_mode_test.json");
    let exact_before = std::fs::read(&run_path).unwrap();
    let mut neighbor:Value = serde_json::from_slice(&exact_before).unwrap();
    neighbor["interface_state"][DOOR]["unrelated_change"] = json!(true);
    std::fs::write(&run_path,serde_json::to_vec_pretty(&neighbor).unwrap()).unwrap();
    let effects = admission.file_writes.load(Ordering::SeqCst);
    assert!(admission.door.lock().await.decode_record(first_return.0,&first_return.2,super::decode_code_mode_door_artifact,octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch).is_err());
    assert!(admission.provider_handoff().is_err());
    assert_eq!(admission.file_writes.load(Ordering::SeqCst),effects);
    std::fs::write(&run_path,&exact_before).unwrap();
    let first = admission
        .run(&room, &handoff, &formation, &kind, first_return.clone())
        .await
        .unwrap();
    assert_eq!(first["body"]["output_retained"], true);
    let second = admission
        .run(&room, &handoff, &formation, &kind, second_return.clone())
        .await
        .unwrap();
    assert_eq!(second["body"]["output_retained"], true);
    assert!(super::continuation::prepared_call_for_output(&durable, &first_call, &formation).is_err());
    let rows = admission.deliveries.lock().unwrap().clone();
    assert_eq!(
        rows.iter()
            .filter(|d| d.to.as_str() == "world_operator_test")
            .count(),
        1
    );
    assert!(
        rows.iter()
            .filter(|d| d.from.as_str() == DOOR && d.to.as_str() == "world_provider_test")
            .all(
                |d| serde_json::from_slice::<Value>(&d.file.read_payload_bytes().unwrap()).unwrap()
                    ["tool"]
                    == "code_mode.run"
            )
    );
    assert!(
        rows.iter()
            .filter(|d| d.file.read_manifest().unwrap().label == "code_mode.resume.result")
            .all(|d| d.from.as_str() == DOOR && d.to.as_str() == DOOR)
    );
    assert!(
        admission
            .door
            .lock()
            .await
            .decode_record(
                first_return.0,
                &first_return.2,
                super::decode_code_mode_door_artifact,
                octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch
            )
            .is_err()
    );
    let recovered = admission.provider_handoff().unwrap().unwrap();
    let boundary = admission.deliveries.lock().unwrap().iter().rev()
        .find(|d| d.from.as_str() == "world_provider_test" && d.to.as_str() == "world_provider_projection_test")
        .map(|d| super::continuation::artifact_ref(&d.file).unwrap()).unwrap();
    let ledger = admission.door.lock().await.len();
    let (recovery, replayed) = recovered.recover_source_with_outcomes().await.unwrap();
    assert_eq!(recovery, octopus_run_authoring::ProviderSourceRecovery::Completed);
    assert!(matches!(replayed.as_slice(), [octopus_run_authoring::ProviderHandoffOutcome::Admitted { artifact, .. }] if *artifact == boundary), "{replayed:?}");
    assert_eq!(admission.door.lock().await.len(), ledger);
    let retained = octopus_run_record::RunRecord::from_bytes(&std::fs::read(&run_path).unwrap()).unwrap();
    assert_eq!(octopus_run_authoring::payload_artifact_bindings(&retained, &InterfaceId::new("world_provider_test")).unwrap(), vec![input_ref.clone()]);
    assert!(retained.checkpoints.iter().filter(|cp| retained.active_checkpoints.contains(&cp.id) && cp.inbound_interface() == Some(&InterfaceId::new("world_provider_input_test"))).all(|cp| cp.artifact_bindings.iter().any(|binding| binding.id == input_ref.id && binding.access.read)));
    let wait = envelope(
        &root,
        301,
        "code_mode.wait",
        json!({"cell_id":cell,"yield_time_ms":20}),
    );
    admission
        .model_call(&wait.artifact_file)
        .await
        .unwrap();
    let waiting = admission.next().await.unwrap();
    let visible = admission
        .run(&room, &handoff, &formation, &kind, waiting)
        .await
        .unwrap();
    assert_eq!(visible["body"]["state"], "yielded", "{visible}");
    let wait = envelope(&root, 401, "code_mode.wait", json!({"cell_id":cell,"yield_time_ms":20}));
    admission
        .model_call(&wait.artifact_file)
        .await
        .unwrap();
    let waiting = admission.next().await.unwrap();
    let visible = admission
        .run(&room, &handoff, &formation, &kind, waiting)
        .await
        .unwrap();
    assert_eq!(visible["body"]["state"], "completed", "{visible}");
    assert_eq!(visible["body"]["output"][0]["text"], "42");
    for order in 0..2 {
    let source = envelope(
        &root,
        302 + order * 20,
        "code_mode.run",
        json!({"script":if matches!(case, FrontierCase::ReadinessFailure) { "await new Promise(resolve => setTimeout(resolve, 70)); await tools.process_list({}); text(sum.value + 1);" } else { "await new Promise(resolve => setTimeout(resolve, 70)); if (both[1].id !== 't' || typeof both[1].info !== 'function' || !Object.isFrozen(both[1])) throw new Error('browser Tab closure did not survive the next cell'); text(sum.value + 1);" },"yield_time_ms":5}),
    );
    admission
        .model_call(&source.artifact_file)
        .await
        .unwrap();
    let input = admission.next().await.unwrap();
    let delayed = admission
        .run(&room, &handoff, &formation, &kind, input)
        .await
        .unwrap();
    assert_eq!(delayed["body"]["state"], "yielded");
    let ctx = admission.door.lock().await.context.clone();
    let before_wake = admission.deliveries.lock().unwrap().last().unwrap().seq;
    if order == 0 { admission.provider_progress(10).await.unwrap(); }
    let observer = tokio::spawn(handoff.clone().observe(ctx));
    if order == 1 {
        admission.wait_for_readiness_delivery(before_wake).await.unwrap();
        admission.provider_progress(11).await.unwrap();
    }
    let wake = admission.next().await.unwrap();
    assert_eq!(
        wake.2.read_artifact_manifest().unwrap().label,
        "code_mode.frontier_ready"
    );
    if matches!(case, FrontierCase::Full) && order == 1 {
        use octopus_tool_door_runtime::{ToolDoorArtifactDecodeError as DecodeError, ToolDoorArtifactDecodePurpose as Purpose};
        admission.probe_unsubmitted_output.store(true, Ordering::SeqCst);
        let peer = admission.clone();
        let progress = tokio::spawn(async move { peer.provider_progress(12).await });
        admission.peer_write_applied.notified().await;
        let expected = admission.deliveries.lock().unwrap().iter().rev().find(|row| row.to.as_str() == "system_provider_file_test").unwrap().seq;
        let pending = admission.door.lock().await.decode_record(wake.0, &wake.2, super::decode_code_mode_door_artifact, Purpose::Dispatch);
        assert!(matches!(pending, Err(DecodeError::AwaitingResults { call_sequences }) if call_sequences == vec![expected]));
        admission.provider_result_release.notify_one();
        progress.await.unwrap().unwrap();
        admission.probe_unsubmitted_output.store(false, Ordering::SeqCst);
        assert!(admission.door.lock().await.decode_record(wake.0, &wake.2, super::decode_code_mode_door_artifact, Purpose::Dispatch).unwrap().is_some());
    }
    if matches!(case, FrontierCase::ReadinessFailure) {
        Box::pin(failure_return_tests::actual_readiness_failure_return(&admission, &handoff, &formation, &wake, &durable, &room, &kind)).await;
        observer.abort();
        let _ = observer.await;
        room.release_room(&formation).await.unwrap();
        admission.door.lock().await.context.owner_cancel().cancel();
        provider_socket.abort(); provider_pump.abort(); pump.abort();
        let _ = tokio::join!(provider_socket, provider_pump, pump);
        return;
    }
    let receipt = admission
        .run(&room, &handoff, &formation, &kind, wake.clone())
        .await
        .unwrap();
    assert_eq!(receipt["body"]["output_retained"], true);
    assert!(
        admission
            .door
            .lock()
            .await
            .decode_record(
                wake.0,
                &wake.2,
                super::decode_code_mode_door_artifact,
                octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch
            )
            .is_err()
    );
    let wait = envelope(
        &root,
        303 + order * 20,
        "code_mode.wait",
        json!({"cell_id":delayed["body"]["cell_id"],"yield_time_ms":20}),
    );
    admission
        .model_call(&wait.artifact_file)
        .await
        .unwrap();
    let input = admission.next().await.unwrap();
    let complete = admission
        .run(&room, &handoff, &formation, &kind, input)
        .await
        .unwrap();
    assert_eq!(complete["body"]["output"][0]["text"], "43");
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    }
    tokio::spawn(owner_registration_order_uses_disjoint_returns(
        admission.clone(), room.clone(), handoff.clone(), formation.clone(), kind.clone(), root.clone(), durable.clone(),
    )).await.unwrap();
    let scenarios = {
        let admission = admission.clone();
        let room = room.clone();
        let handoff = handoff.clone();
        let formation = formation.clone();
        let kind = kind.clone();
        let root = root.clone();
        tokio::spawn(async move {
            for (sequence, result_before_decision) in [(304, true), (310, false)] {
                actual_gui_decision_preserves_the_original_promise(
                    &admission,
                    &room,
                    &handoff,
                    &formation,
                    &kind,
                    &root,
                    sequence,
                    result_before_decision,
                    true,
                )
                .await;
            }
            actual_browser_sdk_uses_admitted_results(&admission,&room,&handoff,&formation,&kind,&root).await;
        })
    };
    scenarios.await.unwrap();
    assert!(admission.file_writes.load(Ordering::SeqCst) >= 10);
    let final_run:Value = serde_json::from_slice(&std::fs::read(admission.run_records.join("run_code_mode_test.json")).unwrap()).unwrap();
    assert_eq!(final_run["interface_state"][DOOR]["prelude_sources"],source_provenance);
    room.release_room(&formation).await.unwrap();
    admission.door.lock().await.context.owner_cancel().cancel();
    provider_socket.abort();provider_pump.abort();
    assert!(provider_socket.await.unwrap_err().is_cancelled());assert!(provider_pump.await.unwrap_err().is_cancelled());
    pump.abort();
    assert!(pump.await.unwrap_err().is_cancelled());
    })
}

async fn actual_generated_artifact_read(admission: &std::sync::Arc<FrontierAdmission>, room:&CodeModeRoom, handoff:&std::sync::Arc<actual_owner_handoff::OwnerHandoff>, formation:&WorkerEnvironmentFormation, kind:&WorkerEnvKindSpec, root:&Path) {
    let input = InterfaceArtifactFile::write(root.join("generated-input"),"operator.message",br#"{"text":"Read the downloaded file"}"#).unwrap();
    admission.arrive("world_provider_input_test",&input).await.unwrap();
    let original = envelope(root,901,"code_mode.run",json!({"script":"let downloadSettled=0; let downloaded=await tools.browser_download_wait({download_id:'aabbccddeeff00112233445566778899',timeout_ms:100}); downloadSettled++; let downloadedRead=await tools.artifact_read({id:downloaded.body.artifact.id}); text(downloadedRead.body.text); text(downloadSettled);","yield_time_ms":20}));
    admission.model_call(&original.artifact_file).await.unwrap();
    let started = admission.run(room,handoff,formation,kind,admission.next().await.unwrap()).await.unwrap();
    assert_eq!(started["body"]["state"],"yielded","{started}");
    let download = admission.deliveries.lock().unwrap().iter().find(|d|d.to.as_str()=="system_browser_test").unwrap().clone();
    let call:octopus_tool_handlers::ToolCallArtifact=serde_json::from_slice(&download.file.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(call.tool,"browser.download_wait");
    let payload = b"original downloaded bytes: the model can read this";
    let raw = InterfaceArtifactFile::write(root.join("generated-raw"),&octopus_run_authoring::generated_artifacts::output_artifact_label(call.correlation_id.as_deref().unwrap()).unwrap(),payload).unwrap();
    let artifact = super::continuation::artifact_ref(&raw).unwrap();
    let admission_seq = admission.arrive("system_browser_test",&raw).await.unwrap();
    assert!(!admission.deliveries.lock().unwrap().iter().any(|d|super::continuation::artifact_ref(&d.file).unwrap()==artifact));
    let payload_budget = admission.door.lock().await.context.owner_io_limits().unwrap().max_payload_bytes;
    let unbound = actual_bound_surface::InvocationArtifactSurface::new(&[],&root.join("unbound"),download.metadata.grant.clone(), None, payload_budget).unwrap();
    assert_eq!(unbound.read_impl(artifact.id.as_str()).unwrap(),None);
    let result = octopus_tool_handlers::ToolResultArtifact::success(&call.tool,call.correlation_id,json!({"status":"completed","artifact":artifact,"generated_artifacts":[{"artifact":artifact,"source_artifact":super::continuation::artifact_ref(&download.file).unwrap(),"source_movement_seq":download.seq,"admission_seq":admission_seq,"payload_content_hash":blake3::hash(payload).to_hex().to_string(),"byte_count":payload.len()}]}));
    let result_file = InterfaceArtifactFile::write(root.join("generated-result"),&result.label,&serde_json::to_vec(&result).unwrap()).unwrap();
    admission.arrive("system_browser_test",&result_file).await.unwrap();
    let resume = admission.next().await.unwrap();
    admission.conflict_next.store(true, Ordering::SeqCst);
    let before = std::fs::read(admission.run_records.join("run_code_mode_test.json")).unwrap();
    let retained = admission.run(room,handoff,formation,kind,resume.clone()).await.unwrap();
    assert_eq!(retained["body"]["output_retained"],true,"{retained}");
    let read = admission.deliveries.lock().unwrap().iter().find(|d|d.to.as_str()=="system_context_test").unwrap().clone();
    let read_call:octopus_tool_handlers::ToolCallArtifact=serde_json::from_slice(&read.file.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(read_call.tool,"artifact.read"); assert_eq!(read_call.arguments["id"],artifact.id.as_str());
    assert!(read.metadata.artifact_binding_refs.contains(&artifact));
    let bound = vec![octopus_interfaces::ArtifactBindingPayload::new(artifact.clone(),raw.read_payload_bytes().unwrap())];
    let surface = actual_bound_surface::InvocationArtifactSurface::new(&bound,&root.join("read-surface"),read.metadata.grant.clone(), None, payload_budget).unwrap();
    let bytes = surface.read_impl(artifact.id.as_str()).unwrap().unwrap(); assert_eq!(bytes,payload);
    let mut denied = read.metadata.grant.clone(); denied.artifact_access.get_mut(artifact.id.as_str()).unwrap().read=false;
    assert!(actual_bound_surface::InvocationArtifactSurface::new(&bound,&root.join("denied"),denied, None, payload_budget).unwrap().read_impl(artifact.id.as_str()).is_err());
    let after = std::fs::read(admission.run_records.join("run_code_mode_test.json")).unwrap();
    let record:Value=serde_json::from_slice(&after).unwrap();
    assert!(record["interface_state"][DOOR]["generated_artifacts"][0]["admission_seq"].as_u64().unwrap()>admission_seq);
    let ctx = admission.door.lock().await.context.clone(); let limits=ctx.owner_io_limits().unwrap();
    let evidence=octopus_run_authoring::CheckedRunEvidence{movements_path:ctx.owner_movements_path(),ledger_index_path:ctx.owner_ledger_index_path(),blobs_dir:ctx.owner_blobs_dir(),max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
    let writer=octopus_run_authoring::CheckedRunWriter{owner:InterfaceId::new(DOOR),source:InterfaceId::new(DOOR),door:InterfaceId::new("system_file_test"),tool:"file.write".into(),call_label:"tool.call".into(),result_label:"file.write.result".into()};
    let peer=octopus_run_authoring::CheckedRunWriter{owner:InterfaceId::new("world_provider_test"),source:InterfaceId::new("system_provider_author_test"),door:InterfaceId::new("system_provider_file_test"),tool:"file.write.provider".into(),call_label:"tool.call".into(),result_label:"file.write.provider.result".into()};
    let chain=octopus_run_authoring::checked_declared_run_advance(&evidence,&admission.run_records.join("run_code_mode_test.json"),&before,&after,resume.0,vec![writer,peer]).unwrap();
    assert!(!chain.is_empty());
    assert!(chain.iter().any(|change|change.writer.owner.as_str() == "world_provider_test"));
    for change in chain.iter().filter(|change|change.writer.owner.as_str() == DOOR) {
        octopus_run_authoring::validate_checked_owner_transition(change).unwrap();
        let mut invalid: Value = serde_json::from_slice(&change.after).unwrap();
        let raw = invalid["interface_state"][DOOR]["output_artifact_checkpoints"][0].clone();
        let row = invalid["checkpoints"].as_array_mut().unwrap().iter_mut().find(|row| row["id"] == raw).unwrap();
        assert_eq!(row["artifact_bindings"].as_array().unwrap().len(), 4);
        row["artifact_bindings"].as_array_mut().unwrap().remove(0);
        let mut omitted = change.clone(); omitted.after = serde_json::to_vec(&invalid).unwrap();
        assert!(octopus_run_authoring::validate_checked_owner_transition(&omitted).is_err());
    }
    let payload_cache=ctx.owner_blobs_dir().join(blake3::hash(payload).to_hex().as_str());
    let admitted_bytes=std::fs::read(&payload_cache).unwrap(); assert_eq!(admitted_bytes,payload);
    std::fs::write(&payload_cache,b"changed after admission").unwrap();
    assert!(octopus_run_authoring::checked_run_file_chain(&evidence,&admission.run_records.join("run_code_mode_test.json"),&before,&after,resume.0,&[chain[0].writer.clone()]).is_err());
    std::fs::write(&payload_cache,&admitted_bytes).unwrap();
    let read_result=octopus_tool_handlers::ToolResultArtifact::success(&read_call.tool,read_call.correlation_id,json!({"text":String::from_utf8(bytes).unwrap()}));
    let file=InterfaceArtifactFile::write(root.join("generated-read-result"),&read_result.label,&serde_json::to_vec(&read_result).unwrap()).unwrap();
    admission.arrive("system_context_test",&file).await.unwrap();
    admission.run(room,handoff,formation,kind,admission.next().await.unwrap()).await.unwrap();
    let wait=envelope(root,902,"code_mode.wait",json!({"cell_id":started["body"]["cell_id"],"yield_time_ms":20}));
    admission.model_call(&wait.artifact_file).await.unwrap();
    let retained=admission.run(room,handoff,formation,kind,admission.next().await.unwrap()).await.unwrap();
    assert_eq!(retained["body"]["state"],"yielded","{retained}");
    let wait=envelope(root,903,"code_mode.wait",json!({"cell_id":started["body"]["cell_id"],"yield_time_ms":20}));
    admission.model_call(&wait.artifact_file).await.unwrap();
    let completed=admission.run(room,handoff,formation,kind,admission.next().await.unwrap()).await.unwrap();
    assert_eq!(completed["body"]["state"],"completed","{completed}");
    assert_eq!(completed["body"]["output"][0]["text"],String::from_utf8(payload.to_vec()).unwrap()); assert_eq!(completed["body"]["output"][1]["text"],"1");
    assert_eq!(admission.deliveries.lock().unwrap().iter().filter(|d|d.to.as_str()=="system_browser_test").count(),1);
    assert!(admission.door.lock().await.decode_record(resume.0,&resume.2,super::decode_code_mode_door_artifact,octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch).is_err());
    println!("generated file admitted at {admission_seq}; checked file writes {}; original download settled once",admission.file_writes.load(Ordering::SeqCst));
}

async fn actual_browser_sdk_uses_admitted_results(
    admission: &std::sync::Arc<FrontierAdmission>,
    room: &CodeModeRoom,
    handoff: &std::sync::Arc<actual_owner_handoff::OwnerHandoff>,
    formation: &WorkerEnvironmentFormation,
    kind: &WorkerEnvKindSpec,
    root: &Path,
) {
    use base64::Engine;
    let script = r#"
var sdkTab = both[1];
var sdkLocator = sdkTab.playwright.frameLocator('iframe.outer').frameLocator('iframe.inner').getByRole('button',{name:/Save\s+draft/i}).filter({hasText:'Save "x"',hasNot:sdkTab.playwright.getByText('hidden')}).and(sdkTab.playwright.locator('button.primary')).first();
if (!Object.isFrozen(sdkLocator)) throw new Error('mutable locator');
try { sdkLocator.and({}); throw new Error('forged locator accepted'); } catch(error) { if (!error.message.includes('same retained Tab')) throw error; }
if (await sdkLocator.count() !== 2) throw new Error('count result lost');
var sdkRows = await sdkTab.playwright.locator('.row').all();
var sdkTexts = await Promise.all(sdkRows.map(row => row.getByText('cell').innerText()));
if (sdkTexts.join('|') !== 'alpha|beta' || await sdkRows[0].getByText('cell').getAttribute('data-value') !== 'a') throw new Error('collection read cache lost');
await sdkLocator.click({modifiers:['Meta'],timeoutMs:0});
var sdkRaw = await sdkLocator.evaluateAll((elements,arg) => ({length:elements.length,arg}),{needle:'🧭'},{timeoutMs:5});
if (sdkRaw.text.length !== 18000) throw new Error('raw readonly body was shortened');
if (await sdkTab.playwright.domSnapshot() !== '- iframe \"login\":\n  - button \"Save\"') throw new Error('Playwright frame snapshot text changed');
if (await sdkTab.dom_cua.get_visible_dom() !== 'button node_id=41 Save') throw new Error('DOM text changed');
await sdkTab.dom_cua.click({node_id:'41'});
await sdkTab.cua.type({text:'**literal plain text**'});
await sdkTab.clipboard.write([{entries:[{mimeType:'text/plain',text:'payload'},{mimeType:'application/octet-stream',base64:'AQID'}],presentationStyle:'attachment'}]);
var sdkClipboard = await sdkTab.clipboard.read();
if (sdkClipboard[0].entries[1].base64 !== 'AQID') throw new Error('clipboard bytes changed');
await sdkTab.cua.move({x:3,y:4});
await sdkTab.cua.drag({path:[{x:3,y:4}],keys:['Meta']});
await sdkTab.cua.keypress({keys:['Meta','A']});
await sdkTab.cua.scroll({x:3,y:4,scrollX:0,scrollY:12});
await sdkLocator.downloadMedia({timeoutMs:7});
var sdkChooser = await sdkTab.fileChoosers.arm();
if (!Object.isFrozen(sdkChooser) || sdkChooser.id !== 'a'.repeat(32)) throw new Error('chooser identity lost');
if ((await sdkChooser.wait({timeout_ms:0})).file_chooser_id !== 'b'.repeat(32)) throw new Error('chooser event lost');
await sdkChooser.cancel();
if (sdkChooser.receipt.status !== 'cancelled') throw new Error('chooser cancellation result lost');
if (await prelude.browser.visibility.get() !== false) throw new Error('visibility fabricated');
await prelude.browser.visibility.set(true);
try { await sdkTab.playwright.waitForLoadState({state:'networkidle'}); throw new Error('unsupported load state accepted'); } catch(error) { if (!error.message.includes('not supported')) throw error; }
if (await sdkTab.playwright.waitForURL('https://example.test/**?literal',{timeoutMs:0}) !== undefined) throw new Error('URL wait changed its void return');
if (await sdkTab.playwright.waitForLoadState({timeoutMs:7}) !== undefined) throw new Error('load wait changed its void return');
if (await sdkTab.playwright.waitForTimeout(1) !== undefined) throw new Error('duration wait changed its void return');
var sdkPng = await sdkTab.screenshot({format:'png',clip:{x:3,y:4,width:2,height:3},fullPage:false});
if (!(sdkPng instanceof Uint8Array)) throw new Error('PNG screenshot did not return bytes');
sdkTab.emitScreenshot();
var sdkJpeg = await sdkTab.screenshot({fullPage:true});
if (!(sdkJpeg instanceof Uint8Array)) throw new Error('default screenshot did not return bytes');
try { await sdkTab.screenshot({format:'png'}); throw new Error('invalid image byte count accepted'); } catch(error) { if (!error.message.includes('byte length differs')) throw error; }
sdkTab.emitScreenshot();
text({sdk_png:Array.from(sdkPng),sdk_jpeg:Array.from(sdkJpeg)});
text('sdk raw '+sdkRaw.text.length);
"#;
    let selector = r#"iframe.outer >> internal:control=enter-frame >> iframe.inner >> internal:control=enter-frame >> internal:role=button[name=/Save\s+draft/i] >> internal:has-text="Save \"x\""i >> internal:has-not="internal:text=\"hidden\"i" >> internal:and="button.primary" >> nth=0"#;
    let clipboard = json!({"items":[{"entries":[{"mime_type":"text/plain","text":"payload"},{"mime_type":"application/octet-stream","base64":"AQID"}],"presentation_style":"attachment"}]});
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2,3).write_to(&mut png,image::ImageFormat::Png).unwrap();
    let png = png.into_inner();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
    let media = InterfaceArtifactFile::write(root.join("sdk-image"),"browser.screenshot.image",&png).unwrap().read_manifest().unwrap();
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(3,2).write_to(&mut jpeg,image::ImageFormat::Jpeg).unwrap();
    let jpeg = jpeg.into_inner();
    let jpeg_encoded = base64::engine::general_purpose::STANDARD.encode(&jpeg);
    let jpeg_media = InterfaceArtifactFile::write(root.join("sdk-image"),"browser.screenshot.image",&jpeg).unwrap().read_manifest().unwrap();
    let cases = vec![
        ("browser.locator_query",json!({"selector":selector,"query":"count"}),json!(2)),
        ("browser.locator_query",json!({"selector":".row","query":"count"}),json!(2)),
        ("browser.locator_query",json!({"selector":".row","query":"read_all","relative_selector":"internal:text=\"cell\"i"}),json!([{"attributes":{"data-value":"a"},"inner_text":"alpha","text_content":"alpha"},{"attributes":{},"inner_text":"beta","text_content":"beta"}])),
        ("browser.click",json!({"selector":selector,"modifiers":["Meta"],"timeout_ms":0,"click_count":1}),json!({"clicked":true})),
        ("browser.evaluate_readonly",json!({"selector":selector,"selector_mode":"all","timeout_ms":5}),json!({"text":"x".repeat(18000)})),
        ("browser.playwright_dom_snapshot",json!({}),json!("- iframe \"login\":\n  - button \"Save\"")),
        ("browser.dom_snapshot",json!({}),json!("button node_id=41 Save")),
        ("browser.dom_click",json!({"node_id":"41"}),json!({"clicked":true})),
        ("browser.paste",json!({"text":"**literal plain text**"}),json!({"pasted":true})),
        ("browser.clipboard_write",clipboard.clone(),json!({"written":true})),
        ("browser.clipboard_read",json!({}),clipboard),
        ("browser.pointer_move",json!({"x":3,"y":4}),json!({"moved":true})),
        ("browser.drag",json!({"path":[{"x":3,"y":4}],"modifiers":["Meta"]}),json!({"dragged":true})),
        ("browser.key",json!({"keys":["Meta","A"]}),json!({"pressed":true})),
        ("browser.scroll",json!({"mode":"gesture","x":3,"y":4,"scroll_x":0,"scroll_y":12}),json!({"scrolled":true})),
        ("browser.media_download",json!({"selector":selector,"timeout_ms":7}),json!({"triggered":true})),
        ("browser.filechooser_arm",json!({}),json!({"reservation_id":"a".repeat(32),"file_chooser_id":null,"status":"armed","target_id":"t","browser_context_id":"fixture-browser-context","host_generation":"c".repeat(32),"is_multiple":null})),
        ("browser.filechooser_wait",json!({"reservation_id":"a".repeat(32),"timeout_ms":0}),json!({"reservation_id":"a".repeat(32),"file_chooser_id":"b".repeat(32),"status":"opened","target_id":"t","browser_context_id":"fixture-browser-context","host_generation":"c".repeat(32),"is_multiple":false})),
        ("browser.filechooser_cancel",json!({"reservation_id":"a".repeat(32)}),json!({"reservation_id":"a".repeat(32),"file_chooser_id":"b".repeat(32),"status":"cancelled","target_id":"t","browser_context_id":"fixture-browser-context","host_generation":"c".repeat(32),"is_multiple":false})),
        ("browser.visibility_get",json!({}),json!({"visible":false,"intent":null})),
        ("browser.visibility_set",json!({"visible":true}),json!({"visible":true,"intent":{"visible":true}})),
        ("browser.wait",json!({"url_pattern":"https://example.test/**?literal","wait_until":"commit","timeout_ms":0}),json!({"url":"https://example.test/path?literal","match_event":"Page.frameNavigated","wait_until":"commit","observation_errors":[]})),
        ("browser.wait",json!({"load_state":"load","timeout_ms":7}),json!({"state":"load","match_event":"Page.loadEventFired","observation_errors":[]})),
        ("browser.wait",json!({"duration_ms":1}),Value::Null),
        ("browser.screenshot",json!({"format":"png","clip":{"x":3,"y":4,"width":2,"height":3},"full_page":false,"pixel_mode":"css"}),Value::Null),
        ("browser.screenshot",json!({"format":"jpeg","quality":80,"full_page":true,"pixel_mode":"css"}),Value::Null),
        ("browser.screenshot",json!({"format":"png","pixel_mode":"css"}),Value::Null),
    ];
    let source = envelope(root,1000,"code_mode.run",json!({"script":script,"yield_time_ms":20}));
    let mut last_seq = admission.deliveries.lock().unwrap().last().unwrap().seq;
    admission.model_call(&source.artifact_file).await.unwrap();
    let input = admission.next().await.unwrap();
    let started = admission.run(room,handoff,formation,kind,input).await.unwrap();
    assert_eq!(started["body"]["state"],"yielded","{started}");
    let cell = started["body"]["cell_id"].clone();
    let frontier_count = cases.len();
    let mut nested_frontiers = Vec::new();
    for (tool,mut arguments,value) in cases {
        let calls:Vec<_> = admission.deliveries.lock().unwrap().iter().filter(|row|row.seq>last_seq && row.from.as_str()==DOOR && row.to.as_str()=="system_browser_test").cloned().collect();
        assert_eq!(calls.len(),1,"expected one frontier for {tool}");
        let delivery = &calls[0]; last_seq = delivery.seq;
        let call:octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&delivery.file.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(call.tool,tool);
        let artifact = serde_json::to_value(super::continuation::artifact_ref(&delivery.file).unwrap()).unwrap();
        let runtime = delivery.metadata.runtime.as_ref().unwrap();
        let admissions: Vec<Value> = std::fs::read_to_string(root.join("movements.jsonl")).unwrap()
            .lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["kind"] == "InterfaceArrival"
                && row["seq"].as_u64().unwrap() < delivery.seq
                && row["artifact"] == artifact
                && row["from"]["Interface"] == delivery.from.as_str()
                && row["to"]["Interface"] == runtime.worker.as_str()
                && row["invocation_metadata"]["runtime"]["run"] == runtime.run.as_str()
                && row["invocation_metadata"]["runtime"]["checkpoint"] == runtime.checkpoint.as_str()
                && row["invocation_metadata"]["runtime"]["run_record_hash"] == json!(runtime.run_record_hash)
                && row["invocation_metadata"]["checkpoint_admission"]["complete"] == true)
            .collect();
        assert_eq!(admissions.len(), 1, "nested call must have one exact completed admission");
        nested_frontiers.push(json!({"tool":call.tool,"correlation_id":call.correlation_id,"delivery":{"admitted":{"seq":admissions[0]["seq"]}},"interface":"system_browser_test"}));
        if !["browser.visibility_get","browser.visibility_set","browser.filechooser_wait","browser.filechooser_cancel"].contains(&tool) { arguments["target_id"]=json!("t"); }
        let mut actual = call.arguments.clone();
        if tool=="browser.evaluate_readonly" {
            let script=actual.as_object_mut().unwrap().remove("script").unwrap();
            let script=script.as_str().unwrap();
            assert!(script.contains("const arg = {\"needle\":\"🧭\"}"));
            assert!(script.contains("__playwrightEvaluate(elements, arg)"));
        }
        assert_eq!(actual,arguments,"{tool}");
        let body=if tool=="browser.screenshot" {
            let (payload, mime, artifact, byte_count, width, height) = if arguments["format"] == "jpeg" {
                (&jpeg_encoded,"image/jpeg",&jpeg_media,jpeg.len(),3,2)
            } else { (&encoded,"image/png",&media,png.len() + usize::from(arguments.get("clip").is_none()),2,3) };
            json!({"result":tool,"worker_environment_id":"fixture-browser-room","browser_context_id":"fixture-browser-context","target_id":"t","source_movement_seq":delivery.seq,"url":"about:blank","title":"Fixture","full_page":arguments.get("full_page").and_then(Value::as_bool).unwrap_or(false),"bytes":byte_count,"media_type":mime,"content_hash":artifact.payload_content_hash,"geometry":{"image":{"width":width,"height":height},"capture":{"x":3,"y":4,"width":width,"height":height}},"media":[{"mime_type":mime,"payload_base64":payload,"artifact":artifact}]})
        } else if tool=="browser.playwright_dom_snapshot" {
            json!({"result":tool,"worker_environment_id":"fixture-browser-room","browser_context_id":"fixture-browser-context","target_id":"t","value":value,"snapshot_observation":{"iframe_budget_ms":1000,"per_frame_timeout_ms":500,"frames":[{"selector":"iframe[name=login]","outcome":"expanded"}]}})
        } else if tool=="browser.wait" && arguments.get("duration_ms").is_some() {
            json!({"result":tool,"target_id":"t","condition":"duration","snapshot":null})
        } else if ["browser.filechooser_arm","browser.filechooser_wait","browser.filechooser_cancel"].contains(&tool) { value }
        else { json!({"result":tool,"worker_environment_id":"fixture-browser-room","browser_context_id":"fixture-browser-context","target_id":"t","value":value,"effective_timeout_ms":0,"load_waits":[],"url":"about:blank","title":"Fixture","frame_resolution_may_scroll":arguments.get("selector").is_some()}) };
        let result=octopus_tool_handlers::ToolResultArtifact::success(&call.tool,call.correlation_id,body);
        let file=InterfaceArtifactFile::write(root.join("sdk-results"),&result.label,&serde_json::to_vec(&result).unwrap()).unwrap();
        admission.arrive("system_browser_test",&file).await.unwrap();
        let input=admission.next().await.unwrap();
        let receipt=admission.run(room,handoff,formation,kind,input.clone()).await.unwrap();
        assert_eq!(receipt["body"]["output_retained"],true,"{receipt}");
        assert!(admission.door.lock().await.decode_record(input.0,&input.2,super::decode_code_mode_door_artifact,octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose::Dispatch).is_err());
    }
    let mut final_output=None;
    let mut returned_media = Vec::new();
    for offset in 0..frontier_count {
        let wait=envelope(root,1001 + offset as u64,"code_mode.wait",json!({"cell_id":cell,"yield_time_ms":20}));
        admission.model_call(&wait.artifact_file).await.unwrap();
        let input=admission.next().await.unwrap();
        let value=admission.run(room,handoff,formation,kind,input).await.unwrap();
        if let Some(media) = value["body"]["media"].as_array() {
            returned_media.extend(media.iter().cloned());
        }
        if value["body"]["state"]=="completed" {
            assert_eq!(offset + 1, frontier_count, "retained frontiers were skipped");
            final_output=Some(value); break;
        }
        assert_eq!(value["body"]["state"],"yielded","{value}");
        let emissions = value["body"]["emissions"].as_array().unwrap();
        assert_eq!(emissions.len(), 1);
        let expected = &nested_frontiers[offset + 1];
        for field in ["tool", "correlation_id", "delivery", "interface"] {
            assert_eq!(emissions[0][field], expected[field], "retained {field} changed");
        }
        assert_eq!(value["body"]["pending_tool_calls"], json!([emissions[0]["tool_call_id"]]));
    }
    let final_output=final_output.expect("SDK cell did not complete through its actual Wait");
    assert!(final_output["body"]["output"].as_array().unwrap().iter().any(|value|value["text"]=="sdk raw 18000"));
    assert_eq!(returned_media.len(),2);
    assert_eq!(returned_media[0]["payload_base64"],encoded);
    let returned:octopus_interfaces::InterfaceArtifactManifest=serde_json::from_value(returned_media[0]["artifact"].clone()).unwrap();
    assert_eq!(std::fs::read(returned.payload_path).unwrap(),png);
    assert_eq!(returned_media[1]["mime_type"],"image/jpeg");
    assert_eq!(returned_media[1]["payload_base64"],jpeg_encoded,
        "invalid later carrier does not overwrite or recapture the retained original JPEG");
    let returned:octopus_interfaces::InterfaceArtifactManifest=serde_json::from_value(returned_media[1]["artifact"].clone()).unwrap();
    assert_eq!(std::fs::read(returned.payload_path).unwrap(),jpeg);
    let arrays = final_output["body"]["output"].as_array().unwrap().iter().filter_map(|row|row["text"].as_str())
        .filter_map(|text|serde_json::from_str::<Value>(text).ok()).find(|value|value.get("sdk_png").is_some()).unwrap();
    assert_eq!(arrays["sdk_png"],json!(png)); assert_eq!(arrays["sdk_jpeg"],json!(jpeg));
    let source=envelope(root,2000,"code_mode.run",json!({"script":"try { await sdkLocator.count(); throw new Error('stale context accepted'); } catch(error) { if (!error.message.includes('retained context identity')) throw error; text('stale context refused'); }","yield_time_ms":20}));
    admission.model_call(&source.artifact_file).await.unwrap();
    let input=admission.next().await.unwrap();
    let started=admission.run(room,handoff,formation,kind,input).await.unwrap();
    let delivery=admission.deliveries.lock().unwrap().iter().rev().find(|row|row.from.as_str()==DOOR && row.to.as_str()=="system_browser_test").cloned().unwrap();
    let call:octopus_tool_handlers::ToolCallArtifact=serde_json::from_slice(&delivery.file.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(call.tool,"browser.locator_query");
    let result=octopus_tool_handlers::ToolResultArtifact::success(&call.tool,call.correlation_id,json!({"result":call.tool,"target_id":"t","worker_environment_id":"fixture-browser-room","browser_context_id":"other-context","value":2}));
    let file=InterfaceArtifactFile::write(root.join("sdk-stale-result"),&result.label,&serde_json::to_vec(&result).unwrap()).unwrap();
    admission.arrive("system_browser_test",&file).await.unwrap();
    let input=admission.next().await.unwrap();
    admission.run(room,handoff,formation,kind,input).await.unwrap();
    let mut rejected=false;
    for sequence in 2001..2004 {
        let wait=envelope(root,sequence,"code_mode.wait",json!({"cell_id":started["body"]["cell_id"],"yield_time_ms":20}));
        admission.model_call(&wait.artifact_file).await.unwrap();
        let input=admission.next().await.unwrap();
        let value=admission.run(room,handoff,formation,kind,input).await.unwrap();
        if value["body"]["state"]=="completed" {
            assert_eq!(value["body"]["output"][0]["text"],"stale context refused"); rejected=true; break;
        }
        assert_eq!(value["body"]["state"],"yielded","{value}");
    }
    assert!(rejected);
    assert!(!admission.deliveries.lock().unwrap().iter().any(|row|row.seq>delivery.seq && row.from.as_str()==DOOR && row.to.as_str()=="system_browser_test"));
}

struct FrontierOperatorVerifier {
    blobs: PathBuf,
    scratch: PathBuf,
}
impl octopus_marks::VerifiedArtifactAdmission for FrontierOperatorVerifier {
    fn prepare_required_verified_artifact(
        &self,
        required: &octopus_ledger::RequiredVerifiedArtifact,
        input: octopus_marks::VerificationInput,
    ) -> Result<Box<dyn octopus_marks::PreparedVerification>, octopus_marks::VerifiedArtifactError>
    {
        use octopus_ledger::BlobStore;
        let registry = registry_root();
        let path = registry.join("verifier").join(required.verifier.as_str());
        let descriptor_bytes = std::fs::read(path.join("verifier.json")).unwrap();
        let descriptor: octopus_verifiers::VerifierDescriptor =
            serde_json::from_slice(&descriptor_bytes).unwrap();
        let package =
            octopus_verifiers::pin_verifier_package(&octopus_verifiers::VerifierPackageContext {
                descriptor,
                package_dir: Some(path),
                repository_root: registry.parent().unwrap().to_path_buf(),
                scratch_root: self.scratch.clone(),
            })
            .unwrap();
        let mut blobs = octopus_ledger::InMemoryBlobs::new();
        let manifest = std::fs::read(self.blobs.join(input.artifact.content.to_hex())).unwrap();
        assert_eq!(blobs.put(&manifest), input.artifact.content);
        let carrier: octopus_interfaces::InterfaceArtifactManifest =
            serde_json::from_slice(&manifest).unwrap();
        let payload_hash =
            ContentHash::from_hex(carrier.payload_content_hash.as_deref().unwrap()).unwrap();
        assert_eq!(
            blobs.put(&std::fs::read(self.blobs.join(payload_hash.to_hex())).unwrap()),
            payload_hash
        );
        let payload =
            octopus_marks::verified_artifact_payload_bytes(&input, &blobs, "package_process")
                .unwrap();
        let pin = octopus_ledger::VerifierPackagePin {
            descriptor_content: blobs.put(&descriptor_bytes),
            script_content: blobs.put(&package.closure_bytes()),
        };
        let verdict = octopus_verifiers::run_pinned_verifier_process(&package, &input, payload);
        if !verdict.passed {
            return Err(octopus_marks::VerifiedArtifactError::VerifierRejected {
                mark: required.mark.to_string(),
                verifier: required.verifier.to_string(),
                artifact: input.artifact.id.to_string(),
                verifier_package: Some(pin),
                details: verdict.details,
            });
        }
        let mark: octopus_marks::MarkDescriptor = serde_json::from_slice(
            &std::fs::read(
                registry
                    .join("mark")
                    .join(required.mark.as_str())
                    .join("mark.json"),
            )
            .unwrap(),
        )
        .unwrap();
        Ok(Box::new(octopus_marks::ReadyVerification::passed(
            octopus_ledger::Mark::passed_with_class(
                required.mark.clone(),
                input.checkpoint,
                input.run,
                input.epoch,
                mark.class,
                input.artifact,
                required.verifier.clone(),
                pin,
                verdict.details,
            ),
        )))
    }
}

async fn actual_gui_decision_preserves_the_original_promise(
    admission: &std::sync::Arc<FrontierAdmission>,
    room: &CodeModeRoom,
    handoff: &std::sync::Arc<actual_owner_handoff::OwnerHandoff>,
    formation: &WorkerEnvironmentFormation,
    kind: &WorkerEnvKindSpec,
    root: &Path,
    sequence: u64,
    result_before_decision: bool,
    capture_followup: bool,
) {
    use octopus_tool_door_runtime::ToolDoorArtifactDecodePurpose as Purpose;
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let source = envelope(
        root,
        sequence,
        "code_mode.run",
        json!({"script":"{ const approvedApp = await prelude.cua.getApp({kind:'bundle_id',value:'example.fixture'},{emit:false}); globalThis.fixtureApp = approvedApp; text(approvedApp.state.application.bundle_id); approvedApp.emitState({screenshots:false}); text(approvedApp.getNode('fixture-app-root').title); }","yield_time_ms":20}),
    );
    admission
        .model_call(&source.artifact_file)
        .await
        .unwrap();
    let input = admission.next().await.unwrap();
    let started = admission
        .run(room, handoff, formation, kind, input)
        .await
        .unwrap();
    assert_eq!(started["body"]["state"], "yielded");
    let delivery = admission
        .deliveries
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|d| {
            d.to.as_str() == "system_gui_test"
                && d.file.read_manifest().unwrap().label == "tool.call"
        })
        .cloned()
        .unwrap();
    let call: octopus_tool_handlers::ToolCallArtifact =
        serde_json::from_slice(&delivery.file.read_payload_bytes().unwrap()).unwrap();
    let source_ref = admission.door.lock().await.cache(&delivery.file).unwrap();
    let generation = "11111111111111111111111111111111";
    let original = json!({"run":formation.run,"ee":formation.ee,"worker":formation.worker,"door_interface":"system_gui_test","tool":call.tool,"correlation_id":call.correlation_id,"arguments":call.arguments,"source_movement_seq":delivery.seq,"source_artifact":source_ref});
    let scope = json!({"worker_environment_id":"we_gui_fixture","formation_pin":"gui-formation-fixture","host_generation":generation});
    let application = json!({"bundle_id":"example.fixture","bundle_path":"/Applications/Fixture.app","display_name":"Fixture","instance":null});
    let commitment = json!({"schema_version":1,"original":original,"scope":scope,"application":application,"approval_scope":"formed_app_target"});
    let digest = format!(
        "{:x}",
        Sha256::digest(octopus_descriptors::canonical_bytes(&commitment))
    );
    let request = json!({"schema_version":1,"request_id":format!("sha256:{digest}"),"call_id":call.correlation_id,"original":original,"scope":scope,"application":application,"approval_scope":"formed_app_target","resume":{"tool":"gui.app_resume","correlation_id":format!("app-resume:{digest}")}});
    let intermediate = octopus_tool_handlers::ToolResultArtifact::success(
        &call.tool,
        call.correlation_id.clone(),
        json!({"operator_request":request,"effect_performed":false}),
    );
    let file = InterfaceArtifactFile::write(
        root.join("gui-intermediate"),
        &intermediate.label,
        &serde_json::to_vec(&intermediate).unwrap(),
    )
    .unwrap();
    admission.arrive("system_gui_test", &file).await.unwrap();
    let input = admission.next().await.unwrap();
    let pending = admission
        .run(room, handoff, formation, kind, input)
        .await
        .unwrap();
    assert_eq!(pending["body"]["state"], "application_approval_pending");
    admission.provider_progress(sequence).await.unwrap();
    let private = root.join(format!("gui-private/{digest}/system_gui_test"));
    let host = private.join("desktop-host");
    std::fs::create_dir_all(&host).unwrap();
    for path in [&private, &host] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let write_private = |name: &str, value: Value| {
        let p = host.join(name);
        std::fs::write(&p, serde_json::to_vec(&value).unwrap()).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        p
    };
    let request_path = write_private(
        &format!("app-{digest}.request.json"),
        json!({"request":request,"native_target_handle":"fixture-native-target","operator_interface":"world_operator_test","provider_interface":DOOR}),
    );
    let connection = write_private(
        "connection.json",
        json!({"schema_version":1,"interface":"system_gui_test","host_generation":generation,"token":"2222222222222222222222222222222222222222222222222222222222222222","action_url":"http://127.0.0.1:9/v1/action","viewer_url":"http://127.0.0.1:9/view"}),
    );
    let response=InterfaceArtifactFile::write(root.join("operator-decision"),"operator.approval.response",&serde_json::to_vec(&json!({"operator_approval_response":{"request_id":request["request_id"],"call_id":request["call_id"],"decision":"accept"}})).unwrap()).unwrap();
    admission
        .arrive("world_operator_test", &response)
        .await
        .unwrap();
    let gui = admission
        .deliveries
        .lock()
        .unwrap()
        .iter()
        .find(|d| {
            d.to.as_str() == "system_gui_test" && d.file.manifest_path() == response.manifest_path()
        })
        .cloned()
        .unwrap();
    let mut gui_envelope = InterfaceInvocationEnvelope {
        artifact: admission.door.lock().await.cache(&response).unwrap(),
        artifact_file: response.clone(),
        metadata: Some(gui.metadata),
        artifact_binding_payloads: vec![],
    };
    let gui_interface = root.join(format!("gui-interface/{digest}/system_gui_test"));
    std::fs::create_dir_all(&gui_interface).unwrap();
    let mut gui_config: Value = serde_json::from_slice(
        &std::fs::read(registry_root().join("interface/system_gui/desktop.json")).unwrap(),
    ).unwrap();
    gui_config["host"]["application_policy"] = Value::Null;
    gui_config["app_approval"] = json!({"operator_interface":"world_operator_test","resume_tool":"gui.app_resume"});
    let config_path = gui_interface.join("desktop.json");
    std::fs::write(&config_path, serde_json::to_vec(&gui_config).unwrap()).unwrap();
    let ctx = admission.door.lock().await.context.clone();
    let invoke_gui = |input: &InterfaceInvocationEnvelope, purpose| {
        let old_private = std::env::var_os("OCTOPUS_VAR_DIR");
        let old_door = std::env::var_os("OCTOPUS_INTERFACE_ID");
        let old_interface = std::env::var_os("OCTOPUS_INTERFACE_DIR");
        unsafe {
            std::env::set_var("OCTOPUS_VAR_DIR", &private);
            std::env::set_var("OCTOPUS_INTERFACE_ID", "system_gui_test");
            std::env::set_var("OCTOPUS_INTERFACE_DIR", &gui_interface);
        }
        let result = octopus_system_gui_env::decode_gui_door_artifact(&ctx, input, purpose);
        unsafe {
            match old_private {
                Some(v) => std::env::set_var("OCTOPUS_VAR_DIR", v),
                None => std::env::remove_var("OCTOPUS_VAR_DIR"),
            };
            match old_door {
                Some(v) => std::env::set_var("OCTOPUS_INTERFACE_ID", v),
                None => std::env::remove_var("OCTOPUS_INTERFACE_ID"),
            };
            match old_interface {
                Some(v) => std::env::set_var("OCTOPUS_INTERFACE_DIR", v),
                None => std::env::remove_var("OCTOPUS_INTERFACE_DIR"),
            };
        }
        result
    };
    let requirements = gui_envelope
        .metadata
        .as_mut()
        .unwrap()
        .required_verified_artifacts
        .clone();
    gui_envelope
        .metadata
        .as_mut()
        .unwrap()
        .required_verified_artifacts
        .clear();
    assert!(invoke_gui(&gui_envelope, Purpose::Dispatch).is_err());
    assert!(!host.join(format!("app-{digest}.decision.json")).exists());
    gui_envelope
        .metadata
        .as_mut()
        .unwrap()
        .required_verified_artifacts = requirements;
    let resumed = invoke_gui(&gui_envelope, Purpose::Dispatch)
        .unwrap()
        .unwrap();
    assert_eq!(resumed.tool, "gui.app_resume");
    assert_eq!(
        resumed.correlation_id.as_deref(),
        Some(format!("app-resume:{digest}").as_str())
    );
    let decision_bytes = std::fs::read(host.join(format!("app-{digest}.decision.json"))).unwrap();
    let original_bytes = std::fs::read(&request_path).unwrap();
    let app_ref = json!({"host_generation":generation,"app_instance_id":"fixture-app"});
    let root_node = json!({"element_id":"fixture-app-root","parent_id":null,"window_id":null,"window_unavailable_reason":"ax_window_attribute_unavailable","role":"AXApplication","subrole":null,"title":"Fixture","description":null,"value":null,"value_type":null,"value_input":null,"enabled":null,"focused":null,"frame_screen_points":null,"settable_attributes":[],"actions":[],"unavailable_attributes":[]});
    let mut state = json!({"scope":scope,"application":{"app":app_ref,"bundle_id":"example.fixture","bundle_path":"/Applications/Fixture.app","display_name":"Fixture","process_id":4242,"launch_time_unix_ms":null,"active":false,"hidden":false},"control_generation":0,"accessibility":{"mode":"full","observation_id":"fixture-observation","app":app_ref,"revision":1,"captured_at_unix_ms":1,"base_revision":null,"context":{"root_kind":"application","root_element_id":"fixture-app-root","menu_bar_element_id":null,"display_omissions":[]},"complete":true,"nodes":[root_node],"removed_ids":[],"omissions":[]},"windows":[],"captures":[],"primary_capture":null,"capture_omissions":[],"preparation_effects":[],"instructions":null});
    let native_presentation_text = format!("fixture-app-root AXApplication Fixture\n{}end of native presentation", "controlled native fixture text 🧭\n".repeat(160));
    state["accessibility"]["presentation"] = json!({"schema_version":1,"format":"macos_ax","context_type":"full_ui","mode":"full","base_revision":null,"text":native_presentation_text,"rendered_element_ids":["fixture-app-root"],"focused_element_id":null,"selected_text":null,"selected_element_ids":[]});
    let (media, _, _) = fixture_app_capture(root, &mut state, 0, true);
    let original_result = octopus_tool_handlers::ToolResultArtifact::success(
        &call.tool,
        call.correlation_id,
        json!({"app_grant_request_id":request["request_id"],"evidence":{"application":state},"media":media}),
    );
    let final_result = octopus_tool_handlers::ToolResultArtifact::success(
        &resumed.tool,
        resumed.correlation_id.clone(),
        json!({"request_id":request["request_id"],"original_result":original_result}),
    );
    let final_file = InterfaceArtifactFile::write(
        root.join("gui-final"),
        &final_result.label,
        &serde_json::to_vec(&final_result).unwrap(),
    )
    .unwrap();
    admission
        .arrive("system_gui_test", &final_file)
        .await
        .unwrap();
    let owner_decision = admission.next().await.unwrap();
    let early_result = admission.next().await.unwrap();
    if result_before_decision {
        let early = admission
            .run(room, handoff, formation, kind, early_result.clone())
            .await
            .unwrap();
        assert_eq!(early["body"]["state"], "application_continuation_pending");
    }
    let settled = admission
        .run(room, handoff, formation, kind, owner_decision.clone())
        .await
        .unwrap();
    assert_eq!(settled["body"]["output_retained"], true);
    if !result_before_decision {
        let continued = admission
            .run(room, handoff, formation, kind, early_result.clone())
            .await
            .unwrap();
        assert_eq!(continued["body"]["output_retained"], true);
    }
    let current: Value = serde_json::from_slice(
        &std::fs::read(admission.run_records.join("run_code_mode_test.json")).unwrap(),
    )
    .unwrap();
    assert!(current["checkpoints"].as_array().unwrap().iter().all(|cp| {
        cp["required_verified_artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["params"]["request_id"] != request["request_id"])
    }));
    assert!(
        admission
            .door
            .lock()
            .await
            .decode_record(
                early_result.0,
                &early_result.2,
                super::decode_code_mode_door_artifact,
                Purpose::Dispatch
            )
            .is_err()
    );
    std::fs::rename(&connection, host.join("retired-connection.json")).unwrap();
    std::fs::rename(&config_path, gui_interface.join("retired-desktop.json")).unwrap();
    let historical = invoke_gui(&gui_envelope, Purpose::HistoricalIdentity)
        .unwrap()
        .unwrap();
    assert_eq!(historical.correlation_id, resumed.correlation_id);
    assert_eq!(std::fs::read(&request_path).unwrap(), original_bytes);
    assert_eq!(
        std::fs::read(host.join(format!("app-{digest}.decision.json"))).unwrap(),
        decision_bytes
    );
    assert!(invoke_gui(&gui_envelope, Purpose::Dispatch).is_err());
    let mut completed = false;
    for sequence in sequence + 1..sequence + 4 {
        let wait = envelope(
            root,
            sequence,
            "code_mode.wait",
            json!({"cell_id":started["body"]["cell_id"],"yield_time_ms":20}),
        );
        admission
            .model_call(&wait.artifact_file)
            .await
            .unwrap();
        let input = admission.next().await.unwrap();
        let result = admission
            .run(room, handoff, formation, kind, input)
            .await
            .unwrap();
        if result["body"]["state"] == "completed" {
            assert_eq!(result["body"]["output"][0]["text"], "example.fixture");
            let rendered = result["body"]["output"][1]["text"].as_str().unwrap();
            let (identity, native_text) = rendered.split_once('\n').unwrap();
            let identity:Value = serde_json::from_str(identity).unwrap();
            assert_eq!(identity["observation_id"],"fixture-observation");
            assert_eq!(identity["revision"],1);
            assert_eq!(identity["context"],state["accessibility"]["context"]);
            assert_eq!(identity["presentation"]["rendered_element_ids"],json!(["fixture-app-root"]));
            assert!(identity.get("nodes").is_none());
            assert_eq!(native_text,native_presentation_text);
            assert_eq!(result["body"]["output"][2]["text"],"Fixture");
            assert_eq!(result["body"]["output"].as_array().unwrap().len(),3);
            assert!(result["body"].get("media").is_none());
            completed = true;
            break;
        }
    }
    assert!(
        completed,
        "real Wait did not retain the admitted App result"
    );
    if result_before_decision && capture_followup {
        actual_app_capture_facade(admission,room,handoff,formation,kind,root,&state,request["request_id"].as_str().unwrap()).await;
    }
}

async fn owner_registration_order_uses_disjoint_returns(
    admission: std::sync::Arc<FrontierAdmission>,
    room: std::sync::Arc<CodeModeRoom>,
    handoff: std::sync::Arc<actual_owner_handoff::OwnerHandoff>,
    formation: WorkerEnvironmentFormation,
    kind: WorkerEnvKindSpec,
    root: PathBuf,
    durable: PathBuf,
) {
    use sha2::{Digest, Sha256};
    for order in 0..2u64 {
        let delegate = room.rooms.lock().await.get(&formation.worker_environment_id).unwrap().delegate.clone();
        let (release_ninth, ninth) = tokio::sync::oneshot::channel();
        *delegate.registration_hold.lock().unwrap() = Some((9, ninth));
        let script = r#"{
            const tab = await prelude.browser.tabs.get('t');
            await Promise.all(Array.from({length:7}, () => tools.process_list({})));
            let actionCalls = 0, original;
            const navigation = tab.playwright.expectNavigation(async () => {
                if (++actionCalls !== 1) throw new Error('action was replayed');
                original = await tools.process_list({});
                await tab.playwright.locator('#next').click();
                return original;
            }, {waitUntil:'load',timeoutMs:3000});
            if (actionCalls !== 1) throw new Error('callback waited for start result');
            const settled = await navigation;
            if (settled !== original || actionCalls !== 1) throw new Error('original action result identity was lost');
            text(settled.body.value);
        }"#;
        let initial_seq = admission.deliveries.lock().unwrap().last().unwrap().seq;
        let source = envelope(&root, 7000 + order * 100, "code_mode.run", json!({"script":script,"yield_time_ms":20}));
        admission.model_call(&source.artifact_file).await.unwrap();
        let input = admission.next().await.unwrap();
        let started = admission.run(&room,&handoff,&formation,&kind,input).await.unwrap();
        let scheduling_cell = started["body"]["cell_id"].as_str().unwrap().to_string();
        let tabs = admission.deliveries.lock().unwrap().iter().find(|d| d.seq > initial_seq && d.from.as_str() == DOOR && d.to.as_str() == "system_browser_test").cloned().unwrap();
        let call: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&tabs.file.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(call.tool, "browser.tabs");
        assert_eq!(call.arguments, json!({}));
        let pending = super::continuation::prepared_calls(&durable).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].enqueue_ordinal(), 1);
        let result = octopus_tool_handlers::ToolResultArtifact::success(&call.tool, call.correlation_id, json!({"worker_environment_id":"fixture-browser-room","browser_context_id":"fixture-browser-context","current_target_id":"t","tabs":[{"target_id":"t","browser_context_id":"fixture-browser-context","url":"about:blank","title":"Fixture"}]}));
        let file = InterfaceArtifactFile::write(root.join("navigation-tabs-result"), &result.label, &serde_json::to_vec(&result).unwrap()).unwrap();
        admission.arrive("system_browser_test", &file).await.unwrap();
        let input = admission.next().await.unwrap();
        admission.run(&room, &handoff, &formation, &kind, input).await.unwrap();
        for _ in 0..7 {
            let call = admission.deliveries.lock().unwrap().iter().rev().find(|d| d.from.as_str()==DOOR && d.to.as_str()==NESTED_INTERFACE).cloned().unwrap();
            let body:octopus_tool_handlers::ToolCallArtifact=serde_json::from_slice(&call.file.read_payload_bytes().unwrap()).unwrap();
            assert_eq!(body.tool,NESTED_TOOL);
            let result=octopus_tool_handlers::ToolResultArtifact::success(&body.tool,body.correlation_id,json!({"value":1}));
            let file=InterfaceArtifactFile::write(root.join("ordinal-warm-results"),&result.label,&serde_json::to_vec(&result).unwrap()).unwrap();
            admission.arrive(NESTED_INTERFACE,&file).await.unwrap();
            let input=admission.next().await.unwrap();
            admission.run(&room,&handoff,&formation,&kind,input).await.unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(8),async {
            loop {
                let changed=delegate.registration_changed.notified();tokio::pin!(changed);changed.as_mut().enable();
                let ready={ let state=delegate.state.lock().unwrap();let registration=&state.registrations[&scheduling_cell];registration.prefix==8 && registration.waiting.contains(&10) };
                if ready { break; } changed.await;
            }
        }).await.unwrap();
        let before = admission.deliveries.lock().unwrap().last().unwrap().seq;
        let observer=tokio::spawn(handoff.clone().observe(admission.door.lock().await.context.clone()));
        let wake=admission.next().await.unwrap();
        assert_eq!(wake.2.read_artifact_manifest().unwrap().label,"code_mode.frontier_ready");
        admission.run(&room,&handoff,&formation,&kind,wake).await.unwrap();
        assert!(!admission.deliveries.lock().unwrap().iter().any(|d|d.seq>before && d.from.as_str()==DOOR && [NESTED_INTERFACE,"system_browser_test"].contains(&d.to.as_str())),"ordinal ten crossed admission before missing ordinal nine registered");
        release_ninth.send(()).unwrap();
        let wake=admission.next().await.unwrap();
        assert_eq!(wake.2.read_artifact_manifest().unwrap().label,"code_mode.frontier_ready");
        admission.run(&room,&handoff,&formation,&kind,wake).await.unwrap();
        observer.abort();assert!(observer.await.unwrap_err().is_cancelled());
        let calls=admission.deliveries.lock().unwrap().iter().filter(|d| d.seq>before && d.from.as_str()==DOOR && [NESTED_INTERFACE,"system_browser_test"].contains(&d.to.as_str())).cloned().collect::<Vec<_>>();
        assert_eq!(calls.len(),2);
        let start:octopus_tool_handlers::ToolCallArtifact=serde_json::from_slice(&calls[0].file.read_payload_bytes().unwrap()).unwrap();
        let read:octopus_tool_handlers::ToolCallArtifact=serde_json::from_slice(&calls[1].file.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(start.tool,"browser.wait");assert_eq!(read.tool,NESTED_TOOL);
        assert_eq!(start.arguments, json!({"phase":"start","target_id":"t","load_state":"load","timeout_ms":3000}));
        let pending=super::continuation::prepared_calls(&durable).unwrap();
        let start_pending = pending.iter().find(|p|p.correlation()==start.correlation_id.as_ref().unwrap().as_str()).unwrap();
        assert_eq!(start_pending.enqueue_ordinal(),9);
        let start_owner = start_pending.process_owner().unwrap().clone();
        assert_eq!(pending.iter().find(|p|p.correlation()==read.correlation_id.as_ref().unwrap().as_str()).unwrap().enqueue_ordinal(),10);
        let source_ref=super::continuation::artifact_ref(&calls[0].file).unwrap();
        let mut condition = start.arguments.clone();
        condition.as_object_mut().unwrap().remove("phase");
        condition.as_object_mut().unwrap().remove("target_id");
        let waiter=json!({"wait_id":"11111111111111111111111111111111","host_generation":"22222222222222222222222222222222","worker_environment_id":"fixture-browser-room","formation_pin":ContentHash::of_bytes(&serde_json::to_vec(&calls[0].metadata.grant.formation_pin).unwrap()).to_hex(),"browser_context_id":"fixture-browser-context","target_id":"t","debugger_epoch":0,"start_source_movement_seq":calls[0].seq,"start_artifact_content_hash":source_ref.content.to_hex(),"condition_digest":format!("{:x}",Sha256::digest(octopus_descriptors::canonical_bytes(&condition)))});
        for (call,from,result_body) in [(start,"system_browser_test",json!({"waiter":waiter,"status":if order==0 {"pending"} else {"satisfied"},"effective_timeout_ms":3000})),(read,NESTED_INTERFACE,json!({"value":42}))] {
            octopus_tool_registry::ToolArgumentValidator::compile(&raw_descriptor(&call.tool).args_schema).unwrap().validate(&call.arguments).unwrap();
            let result=octopus_tool_handlers::ToolResultArtifact::success(&call.tool,call.correlation_id,result_body);
            let file=InterfaceArtifactFile::write(root.join("ordinal-start-read-results"),&result.label,&serde_json::to_vec(&result).unwrap()).unwrap();
            admission.arrive(from,&file).await.unwrap();
            let input=admission.next().await.unwrap();admission.run(&room,&handoff,&formation,&kind,input).await.unwrap();
        }
        let pending=super::continuation::prepared_calls(&durable).unwrap();assert_eq!(pending.len(),2);
        let completion=pending.iter().find(|p|p.tool_call().tool=="browser.wait_complete").unwrap();
        let action=pending.iter().find(|p|p.tool_call().tool=="browser.click").unwrap();
        assert_eq!(completion.enqueue_ordinal(),11);assert_eq!(action.enqueue_ordinal(),12);
        assert_ne!(completion.process_owner(),action.process_owner());
        assert_eq!(action.process_owner(), Some(&start_owner));
        assert_eq!(completion.process_owner().unwrap().interface, start_owner.interface);
        assert_eq!(completion.process_owner().unwrap().kind, start_owner.kind);
        assert_eq!(completion.tool_call().arguments, json!({"waiter":waiter}));
        assert_eq!(action.tool_call().arguments, json!({"target_id":"t","selector":"#next","click_count":1}));
        let mut pair=vec![completion.tool_call(),action.tool_call()];if order==1 { pair.reverse(); }
        let mut admitted_hashes=Vec::new();let mut admitted_checkpoints=std::collections::BTreeSet::new();
        for call in pair {
            octopus_tool_registry::ToolArgumentValidator::compile(&raw_descriptor(&call.tool).args_schema).unwrap().validate(&call.arguments).unwrap();
            let invoked = admission.deliveries.lock().unwrap().iter().find(|delivery| {
                if delivery.seq <= before || delivery.from.as_str() != DOOR || delivery.to.as_str() != "system_browser_test" { return false; }
                let emitted: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&delivery.file.read_payload_bytes().unwrap()).unwrap();
                emitted.tool == call.tool && emitted.correlation_id == call.correlation_id
            }).cloned().expect("each independent process crossed its own admitted call edge");
            let body = if call.tool == "browser.wait_complete" {
                json!({"waiter":waiter,"status":"satisfied","observation":{"state":"load","match_event":"ready_state"},"completion_source_movement_seq":invoked.seq})
            } else {
                json!({"worker_environment_id":"fixture-browser-room","browser_context_id":"fixture-browser-context","target_id":"t","value":{"clicked":true}})
            };
            let result=octopus_tool_handlers::ToolResultArtifact::success(&call.tool,call.correlation_id,body);
            let file=InterfaceArtifactFile::write(root.join("simultaneous-owner-results"),&result.label,&serde_json::to_vec(&result).unwrap()).unwrap();
            let seq=admission.arrive("system_browser_test",&file).await.unwrap();
            let delivery=admission.deliveries.lock().unwrap().iter().find(|d|d.seq>seq && d.from.as_str()=="system_browser_test" && d.to.as_str()==DOOR && d.file.manifest_path()==file.manifest_path()).cloned().expect("admitted result has its own later outbound movement");
            let runtime=delivery.metadata.runtime.unwrap();admitted_hashes.push(runtime.run_record_hash);admitted_checkpoints.insert(runtime.checkpoint);
            assert!(admission.arrive("system_browser_test",&file).await.is_err());
        }
        assert_eq!(admitted_hashes[0],admitted_hashes[1]);assert_eq!(admitted_checkpoints.len(),2);
        admission.provider_progress(70 + order).await.unwrap();
        let first=admission.next().await.unwrap();let second=admission.next().await.unwrap();
        admission.run(&room,&handoff,&formation,&kind,first).await.unwrap();
        admission.run(&room,&handoff,&formation,&kind,second).await.unwrap();
        let mut visible_text=Vec::new();
        let mut completed=false;
        for index in 0..16u64 {
            let wait=envelope(&root,7010 + order*100 + index,"code_mode.wait",json!({"cell_id":scheduling_cell,"yield_time_ms":20}));
            admission.model_call(&wait.artifact_file).await.unwrap();let input=admission.next().await.unwrap();
            let visible=admission.run(&room,&handoff,&formation,&kind,input).await.unwrap();
            visible_text.extend(visible["body"]["output"].as_array().unwrap().iter().filter_map(|item|item["text"].as_str().map(str::to_string)));
            if visible["body"]["state"]=="completed" { completed=true;break; }
        }
        assert!(completed,"real Wait did not receive the retained terminal result");
        assert_eq!(visible_text,vec!["42"]);
        let calls = admission.deliveries.lock().unwrap().iter().filter(|delivery| delivery.seq > before && delivery.from.as_str() == DOOR && [NESTED_INTERFACE,"system_browser_test"].contains(&delivery.to.as_str())).map(|delivery| {
            serde_json::from_slice::<octopus_tool_handlers::ToolCallArtifact>(&delivery.file.read_payload_bytes().unwrap()).unwrap()
        }).collect::<Vec<_>>();
        assert_eq!(calls.iter().map(|call| call.tool.as_str()).collect::<Vec<_>>(), vec!["browser.wait",NESTED_TOOL,"browser.wait_complete","browser.click"]);
        assert_eq!(calls.iter().map(|call| call.correlation_id.as_ref().unwrap()).collect::<std::collections::BTreeSet<_>>().len(), 4);
    }
}

fn fixture_app_capture(root: &Path, state: &mut Value, ordinal: usize, include_primary: bool) -> (Value, Vec<u8>, &'static str) {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let primary_mime = if ordinal % 2 == 1 { "image/jpeg" } else { "image/png" };
    let mut media = Vec::new();
    let mut captures = Vec::new();
    let mut windows = Vec::new();
    let mut primary_bytes = Vec::new();
    for (index, id) in ["other-window", "primary-window"].into_iter().enumerate() {
        let window = json!({"window_id":id,"cg_window_id":index+11,"app":state["application"]["app"],"title":null,"frame_screen_points":{"x":10.0,"y":20.0,"width":2.0,"height":3.0},"on_screen":true,"window_layer":0});
        windows.push(window.clone());
        if index == 1 && !include_primary { continue; }
        let jpeg = (index == 1) == (primary_mime == "image/jpeg");
        let mime = if jpeg { "image/jpeg" } else { "image/png" };
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2,3).write_to(&mut bytes,if jpeg { image::ImageFormat::Jpeg } else { image::ImageFormat::Png }).unwrap();
        let bytes = bytes.into_inner();
        if index == 1 { primary_bytes = bytes.clone(); }
        captures.push(json!({"source":{"primary_window_incarnation":format!("{}",index+1),"contributors":[{"kind":"application_window","window_id":id,"window_incarnation":format!("{}",index+1)}],"crop_screen_points":window["frame_screen_points"],"filter_mode":"desktop_independent_window"},"app":state["application"]["app"],"window":window,"geometry":{"pixel_width":2,"pixel_height":3,"pixel_to_screen":[1.0,0.0,0.0,1.0,10.0,20.0],"filter_content_rect":{"x":0.0,"y":0.0,"width":2.0,"height":3.0},"point_pixel_scale":1.0},"image":{"image_id":format!("{id}-{ordinal}"),"media_type":mime,"data_base64":base64::engine::general_purpose::STANDARD.encode(&bytes),"sha256":format!("{:x}",Sha256::digest(&bytes)),"byte_length":bytes.len(),"width":2,"height":3},"captured_at_unix_ms":1}));
    }
    if ordinal % 2 == 1 { captures.reverse(); }
    state["windows"] = json!(windows);
    state["captures"] = json!(captures);
    state["primary_capture"] = json!({"observation_id":state["accessibility"]["observation_id"],"revision":state["accessibility"]["revision"],"source_window_id":"primary-window","source_window_incarnation":"2","selection":"ax_focused_window","image_id":if include_primary { json!(format!("primary-window-{ordinal}")) } else {Value::Null}});
    state["capture_omissions"] = if include_primary { json!([]) } else {json!([{"window_id":"primary-window","reason":"fixture_capture_unavailable"}])};
    let typed:octopus_system_gui_env::protocol::application::State=serde_json::from_value(state.clone()).unwrap();
    typed.validate(&typed.scope,&typed.application.app).unwrap();
    for capture in state["captures"].as_array_mut().unwrap() {
        let image=capture["image"].as_object_mut().unwrap();
        let encoded=image.remove("data_base64").unwrap();
        let bytes=base64::engine::general_purpose::STANDARD.decode(encoded.as_str().unwrap()).unwrap();
        let manifest=InterfaceArtifactFile::write(root.join("app-source-images"),"screen.captured",&bytes).unwrap().read_manifest().unwrap();
        image.insert("artifact".into(),json!(manifest));
        media.push(json!({"mime_type":image["media_type"],"artifact":manifest,"payload_base64":encoded}));
    }
    (json!(media),primary_bytes,primary_mime)
}

async fn actual_app_capture_facade(
    admission:&std::sync::Arc<FrontierAdmission>,room:&CodeModeRoom,
    handoff:&std::sync::Arc<actual_owner_handoff::OwnerHandoff>,formation:&WorkerEnvironmentFormation,
    kind:&WorkerEnvKindSpec,root:&Path,base_state:&Value,grant:&str,
) {
    use base64::Engine;
    use sha2::{Digest,Sha256};
    let instruction="Use the controlled fixture's exact observed elements.";
    let instructions=json!({"bundle_id":"example.fixture","bundle_path":"/Applications/Fixture.app","content_sha256":format!("{:x}",Sha256::digest(instruction.as_bytes())),"text":instruction});
    let mut previous_observation=base_state["accessibility"]["observation_id"].clone();
    let mut previous_revision=base_state["accessibility"]["revision"].clone();
    for case in 0..6usize {
        let mut state=base_state.clone();
        state["accessibility"]["observation_id"]=json!(format!("capture-observation-{case}"));
        state["accessibility"]["revision"]=json!(case+2);
        state["instructions"]=instructions.clone();
        let (mut media,bytes,mime)=fixture_app_capture(root,&mut state,case+2,case<3 || case==5);
        let expected=json!(bytes).to_string();
        let script=match case {
            0=>"var axText = await fixtureApp.getAXState({emit:false,disableDiffing:true}); if(typeof axText!=='string'||!axText.includes('<app_specific_instructions>')||!axText.endsWith('end of native presentation')) throw new Error('AX string/instructions changed'); text('AX string retained without emission');".into(),
            1=>format!("var shot = await fixtureApp.getScreenshot(); if(!(shot instanceof Uint8Array)||JSON.stringify(Array.from(shot))!=={expected:?}) throw new Error('Screenshot original bytes changed'); text('primary bytes retained');"),
            2=>format!("var captured = await fixtureApp.getAXStateAndScreenshot(); if(typeof captured.state!=='string'||captured.state.includes('<app_specific_instructions>')||!(captured.screenshot instanceof Uint8Array)||JSON.stringify(Array.from(captured.screenshot))!=={expected:?}) throw new Error('Same observation/string/bytes changed'); text('same observation retained');"),
            3=>"var missing = await fixtureApp.getAXStateAndScreenshot({emit:false}); if(typeof missing.state!=='string'||Object.hasOwn(missing,'screenshot')) throw new Error('Absent primary was replaced'); text('missing primary preserved');".into(),
            4=>"try { await fixtureApp.getScreenshot({emit:false}); throw new Error('Missing primary accepted'); } catch(error) { if(!error.message.includes('Screenshot unavailable')) throw error; text('missing primary refused'); }".into(),
            _=>"try { await fixtureApp.getScreenshot({emit:false}); throw new Error('Ambiguous carrier accepted'); } catch(error) { if(!error.message.includes('exact embedded MIME/carrier')) throw error; text('ambiguous carrier refused'); }".into(),
        };
        if case==5 {
            let artifact = &state["captures"].as_array().unwrap().iter().find(|capture| capture["image"]["image_id"] == state["primary_capture"]["image_id"]).unwrap()["image"]["artifact"];
            let duplicate = media.as_array().unwrap().iter().find(|carrier| &carrier["artifact"] == artifact).unwrap().clone();
            media.as_array_mut().unwrap().push(duplicate);
        }
        let watermark=admission.deliveries.lock().unwrap().last().unwrap().seq;
        let source=envelope(root,500+case as u64*4,"code_mode.run",json!({"script":script,"yield_time_ms":20}));
        admission.model_call(&source.artifact_file).await.unwrap();
        let input=admission.next().await.unwrap();
        let started=admission.run(room,handoff,formation,kind,input).await.unwrap();
        assert_eq!(started["body"]["state"],"yielded","{started}");
        let deliveries:Vec<_>=admission.deliveries.lock().unwrap().iter().filter(|d|d.seq>watermark && d.from.as_str()==DOOR && d.to.as_str()=="system_gui_test").cloned().collect();
        assert_eq!(deliveries.len(),1,"capture method emitted another observation");
        let call:octopus_tool_handlers::ToolCallArtifact=serde_json::from_slice(&deliveries[0].file.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(call.tool,"gui.app_state");
        assert_eq!(call.arguments["include_screenshots"],true);
        assert_eq!(call.arguments["capture_selection"], "primary");
        assert_eq!(call.arguments["app"],json!({"kind":"bundle_path","value":"/Applications/Fixture.app"}));
        if case==0 { assert!(call.arguments.get("presentation_base").is_none()); }
        else { assert_eq!(call.arguments["presentation_base"],json!({"app":state["application"]["app"],"observation_id":previous_observation,"revision":previous_revision})); }
        previous_observation=state["accessibility"]["observation_id"].clone(); previous_revision=state["accessibility"]["revision"].clone();
        let result=octopus_tool_handlers::ToolResultArtifact::success(&call.tool,call.correlation_id,json!({"app_grant_request_id":grant,"evidence":{"application":state},"media":media}));
        let file=InterfaceArtifactFile::write(root.join("app-capture-results"),&result.label,&serde_json::to_vec(&result).unwrap()).unwrap();
        admission.arrive("system_gui_test",&file).await.unwrap();
        let input=admission.next().await.unwrap();
        let resumed=admission.run(room,handoff,formation,kind,input).await.unwrap();
        assert_eq!(resumed["body"]["output_retained"],true,"{resumed}");
        let wait=envelope(root,501+case as u64*4,"code_mode.wait",json!({"cell_id":started["body"]["cell_id"],"yield_time_ms":20}));
        admission.model_call(&wait.artifact_file).await.unwrap();
        let input=admission.next().await.unwrap();
        let completed=admission.run(room,handoff,formation,kind,input).await.unwrap();
        assert_eq!(completed["body"]["state"],"completed","{completed}");
        let output=completed["body"]["output"].as_array().unwrap();
        let images:Vec<_>=output.iter().filter(|v|v["type"]=="image").collect();
        if case==1 || case==2 {
            assert_eq!(images.len(),1,"unrelated captured windows leaked into model output");
            let output_media=completed["body"]["media"].as_array().unwrap(); assert_eq!(output_media.len(),1);
            assert_eq!(output_media[0]["mime_type"],mime);
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(output_media[0]["payload_base64"].as_str().unwrap()).unwrap(),bytes);
            let identities:Vec<Value>=output.iter().filter_map(|v|v["text"].as_str().and_then(|s|serde_json::from_str(s).ok())).collect();
            let matching = identities.iter().filter(|identity| identity["image_id"] == state["primary_capture"]["image_id"]).collect::<Vec<_>>();
            assert_eq!(matching.len(), 1);
            let emitted: octopus_system_gui_env::protocol::application::CaptureSource = serde_json::from_value(matching[0]["source"].clone()).unwrap();
            let original: octopus_system_gui_env::protocol::application::CaptureSource = serde_json::from_value(state["captures"].as_array().unwrap().iter().find(|capture| capture["image"]["image_id"] == state["primary_capture"]["image_id"]).unwrap()["source"].clone()).unwrap();
            assert_eq!(serde_json::to_value(emitted).unwrap(), serde_json::to_value(original).unwrap());
        } else { assert!(images.is_empty()); assert_eq!(output.len(),1,"emit:false leaked state, instructions or another image"); }
        assert_eq!(admission.deliveries.lock().unwrap().iter().filter(|d|d.seq>watermark && d.from.as_str()==DOOR && d.to.as_str()=="system_gui_test").count(),1,"failure or emission secretly reobserved");
    }
}

fn bounded_room_media_preserves_the_actual_image_carrier(root: &Path) {
    use base64::Engine;
    let source = envelope(root, 299, "code_mode.run", json!({"script":""}));
    let config: super::config::CodeModeConfig =
        serde_json::from_value(json!({"schema_version":1})).unwrap();
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 3)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let bytes = bytes.into_inner();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let original = json!({"output":[{"type":"text","text":"window exact"},{"type":"image","image_url":format!("data:image/png;base64,{encoded}"),"detail":"original"}]});
    let mut body = original.clone();
    super::materialize_output_media(&source, &config, &mut body).unwrap();
    assert_eq!(body["output"][0], original["output"][0]);
    assert!(body["output"][1].get("image_url").is_none());
    assert_eq!(body["output"][1]["detail"], "original");
    assert_eq!(
        (
            body["output"][1]["width"].as_u64(),
            body["output"][1]["height"].as_u64()
        ),
        (Some(2), Some(3))
    );
    let manifest: octopus_interfaces::InterfaceArtifactManifest =
        serde_json::from_value(body["media"][0]["artifact"].clone()).unwrap();
    assert_eq!(std::fs::read(manifest.payload_path).unwrap(), bytes);
    assert_eq!(body["media"][0]["payload_base64"], encoded);
    assert_eq!(body["output"][1]["artifact"], body["media"][0]["artifact"]);
    let mut wrong_format = original.clone();
    wrong_format["output"][1]["image_url"] = json!(format!("data:image/jpeg;base64,{encoded}"));
    assert!(super::materialize_output_media(&source, &config, &mut wrong_format).is_err());
    assert!(wrong_format.get("media").is_none());
    let mut small = config.clone();
    small.max_output_bytes = bytes.len() - 1;
    assert!(super::materialize_output_media(&source, &small, &mut original.clone()).is_err());
    assert!(super::materialize_output_media(&source, &config, &mut body).is_err());
}

struct FrontierProvider {
    context: octopus_interface_transport::InterfaceLiftContext,
    descriptor: octopus_interfaces::InterfaceDescriptor,
}

impl FrontierAdmission {
    fn provider_handoff(&self) -> Result<Option<octopus_run_authoring::ProviderOutputHandoff>,String> {
        let delivery = self.deliveries.lock().unwrap().iter().rev().find(|d| (d.from.as_str() == DOOR || d.from.as_str() == "world_provider_input_test") && d.to.as_str() == "world_provider_test").cloned();
        let Some(delivery) = delivery else { return Ok(None); };
        let mut payloads = Vec::new();
        for artifact in &delivery.metadata.artifact_binding_refs {
            let manifest = std::fs::read(self.provider.context.blobs_dir.join(artifact.content.to_hex())).map_err(|e| e.to_string())?;
            if ContentHash::of_bytes(&manifest) != artifact.content { return Err("provider fixture manifest integrity changed".into()); }
            let manifest: octopus_interfaces::InterfaceArtifactManifest = serde_json::from_slice(&manifest).map_err(|e| e.to_string())?;
            let hash = manifest.payload_content_hash.as_deref().and_then(ContentHash::from_hex).ok_or("provider fixture bound payload lost its pin")?;
            let bytes = std::fs::read(self.provider.context.blobs_dir.join(hash.to_hex())).map_err(|e| e.to_string())?;
            if ContentHash::of_bytes(&bytes) != hash { return Err("provider fixture payload integrity changed".into()); }
            payloads.push(octopus_interfaces::ArtifactBindingPayload::new(artifact.clone(),bytes));
        }
        let input = InterfaceInvocationEnvelope { artifact:super::continuation::artifact_ref(&delivery.file)?,artifact_file:delivery.file,metadata:Some(delivery.metadata),artifact_binding_payloads:payloads };
        let pinned = self.provider.context.pinned_run_record_bytes(&input)?;
        let session = octopus_run_authoring::ProviderHandoffSession::new(&self.provider.context)?;
        session.from_invocation(&self.provider.context,&self.provider.descriptor,&input.runtime().map_err(|e| e.to_string())?.run,&input,delivery.seq,&pinned).map(Some)
    }

    async fn model_call(&self, file: &InterfaceArtifactFile) -> Result<(),String> {
        let Some(handoff) = self.provider_handoff()? else { self.arrive("world_provider_test",file).await?; return Ok(()); };
        let call:octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&file.read_payload_bytes().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let artifact = InterfaceArtifactFile::write(self.provider.context.var_dir.join("model-call"),&file.read_manifest().map_err(|e| e.to_string())?.label,&file.read_payload_bytes().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let outcome = handoff.hand_final(octopus_run_authoring::ProviderEmission { artifact,route:octopus_run_authoring::ProviderEmissionRoute::Tool{tool:call.tool} }).await?;
        if !matches!(outcome,octopus_run_authoring::ProviderHandoffOutcome::Admitted{..}) { return Err(format!("actual provider model call was not admitted: {outcome:?}")); }
        Ok(())
    }

    async fn provider_progress(&self, sequence:u64) -> Result<(),String> {
        let handoff = self.provider_handoff()?.ok_or("provider has no actual original owner output")?;
        let artifact = InterfaceArtifactFile::write(self.provider.context.var_dir.join("progress"),"provider.fixture.progress",&serde_json::to_vec(&json!({"sequence":sequence})).unwrap()).map_err(|e| e.to_string())?;
        let outcome = handoff.hand_final(octopus_run_authoring::ProviderEmission { artifact,route:octopus_run_authoring::ProviderEmissionRoute::ClientProjection }).await?;
        if !matches!(outcome,octopus_run_authoring::ProviderHandoffOutcome::Admitted{..}) { return Err(format!("actual provider rearm was not admitted: {outcome:?}")); }
        Ok(())
    }

    async fn execute_provider_file_write(&self, delivery: &AdmittedDelivery) -> Result<(),String> {
        if delivery.from.as_str() != "system_provider_author_test" || delivery.to.as_str() != "system_provider_file_test" { return Err("provider writer has foreign endpoints".into()); }
        let call:octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(&delivery.file.read_payload_bytes().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if call.tool != "file.write.provider" || !delivery.metadata.grant.tools.contains(&call.tool) { return Err("provider private file tool is not exactly granted".into()); }
        let input = InterfaceInvocationEnvelope { artifact:super::continuation::artifact_ref(&delivery.file)?,artifact_file:delivery.file.clone(),metadata:Some(delivery.metadata.clone()),artifact_binding_payloads:Vec::new() };
        let interleaved = self.interleave_next_write.swap(false, Ordering::SeqCst);
        let provider_wins = self.provider_wins_next_write.load(Ordering::SeqCst);
        if interleaved {
            self.peer_write_admitted.notify_one();
            if provider_wins {
                if !self.provider_writes_before_owner_call.load(Ordering::SeqCst) { self.owner_write_admitted.notified().await; }
            } else { self.owner_write_completed.notified().await; }
        }
        let body = actual_file_write::execute_file_write_result(&input,&call.arguments,&["system_room".into()],&BTreeMap::from([("run_records_root".into(),self.run_records.clone())]),&self.root.join("provider-file-effects"), 8 * 1024 * 1024);
        self.file_writes.fetch_add(1,Ordering::SeqCst);
        let result = match body {
            Ok(body) => octopus_tool_handlers::ToolResultArtifact::success(&call.tool,call.correlation_id,body),
            Err(error) => octopus_tool_handlers::ToolResultArtifact::failure(&call.tool,call.correlation_id,error),
        };
        let file = InterfaceArtifactFile::write(self.root.join("provider-file-results"),&result.label,&serde_json::to_vec(&result).unwrap()).map_err(|e| e.to_string())?;
        if self.probe_unsubmitted_output.load(Ordering::SeqCst) {
            assert!(result.ok, "{result:?}");
            self.peer_write_applied.notify_one();
            self.provider_result_release.notified().await;
        }
        if interleaved && provider_wins {
            assert!(result.ok, "{result:?}");
            self.peer_write_applied.notify_one();
            self.owner_write_completed.notified().await;
        }
        Box::pin(self.arrive("system_provider_file_test",&file)).await?;
        Ok(())
    }

    async fn wait_for_readiness_delivery(&self, after:u64) -> Result<(),String> {
        tokio::time::timeout(std::time::Duration::from_secs(8),async {
            loop {
                let changed = self.changed.notified();tokio::pin!(changed);changed.as_mut().enable();
                if self.deliveries.lock().unwrap().iter().any(|d| d.seq>after && d.from.as_str()==DOOR && d.to.as_str()==DOOR && d.file.read_manifest().is_ok_and(|m| m.label=="code_mode.frontier_ready")) { return Ok::<(),String>(()); }
                changed.await;
            }
        }).await.map_err(|_| "readiness was not admitted".to_string())?
    }
}

async fn actual_provider_authoring_socket(
    admission: std::sync::Arc<FrontierAdmission>,
    listener: tokio::net::UnixListener,
) -> Result<(),String> {
    use futures::{SinkExt,StreamExt};
    loop {
        let (stream,_) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut socket = tokio_tungstenite::accept_async(stream).await.map_err(|e| e.to_string())?;
        let bytes = socket.next().await.ok_or("provider socket closed before its manifest")?.map_err(|e| e.to_string())?.into_text().map_err(|e| e.to_string())?;
        let directory=admission.root.join("provider-call-carriers");std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let path=directory.join(format!("{}.json",ContentHash::of_bytes(bytes.as_bytes()).to_hex()));
        std::fs::write(&path,bytes.as_bytes()).map_err(|e| e.to_string())?;
        let file=InterfaceArtifactFile::new(path).map_err(|e| e.to_string())?;
        let seq=admission.arrive("system_provider_author_test",&file).await?;
        socket.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::to_string(&DeliveryVerdict::Admitted{seq}).unwrap().into())).await.map_err(|e| e.to_string())?;
        let _=socket.close(None).await;
    }
}
