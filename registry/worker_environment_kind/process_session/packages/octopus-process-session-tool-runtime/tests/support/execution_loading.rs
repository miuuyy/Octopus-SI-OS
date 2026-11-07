use super::*;
use crate::{ToolProcessInput, ToolProcessOutput};
use octopus_descriptors::{CheckpointId, EeId, RunId};
use octopus_interfaces::{GrantProjection, RuntimeInvocationMetadata};
use octopus_tool_handlers::ToolResultArtifact;
use serde_json::json;

fn stderr(ctx: &InterfaceLiftContext) -> String {
    let directory = tool_process_host_private_root(ctx).unwrap().join("stderr");
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => return format!("tool stderr directory unavailable: {error}"),
    };
    entries.map(|entry| {
        std::fs::read_to_string(entry.unwrap().path()).unwrap()
    }).collect::<Vec<_>>().join("\n")
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn authored_interpreted_tool_loads_hot_and_repairs_in_the_same_run() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let registry = root.join("registry");
    let door = registry.join("interface/system_test_room");
    let kind = registry.join("worker_environment_kind/test.room");
    let tool = registry.join("tool/authored.echo");
    for directory in [&door, &tool, &kind.join("packages"), &root.join("var"), &root.join("scope")] {
        std::fs::create_dir_all(directory).unwrap();
    }
    for (path, id, role) in [(&door, "system_test_room", "system"), (&registry.join("interface/system_worker_test"), "system_worker_test", "worker")] {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("interface.json"), serde_json::to_vec(&json!({
            "id": id, "interface_type": "system", "role": role, "direction": "bidirectional",
            "source_names": ["test"], "interface_driver_ref": "test_door"
        })).unwrap()).unwrap();
    }
    let (arrivals, mut outputs) = tokio::sync::mpsc::channel::<InterfaceActivationArrival>(4);
    let mut ctx = InterfaceLiftContext::unprivileged(
        InterfaceId::new("system_test_room"), door, None, root.join("var"), root.join("movements.jsonl"),
        root.join("blobs"), root.join("ledger-index.sqlite"), BTreeMap::from([
            ("interface_life_install_root".into(), root.clone()),
            ("registry_interface_root".into(), registry.join("interface")),
            ("registry_tool_root".into(), registry.join("tool")),
            ("registry_worker_environment_kind_root".into(), registry.join("worker_environment_kind")),
            ("scope_root".into(), root.join("scope")),
        ]), arrivals, tokio_util::sync::CancellationToken::new(),
    );
    ctx.host_io_limits = Some(InterfaceHostIoLimits {
        max_control_frame_bytes: 65536, max_stderr_bytes: 65536,
        max_manifest_bytes: 65536, max_payload_bytes: 65536,
    });
    std::fs::write(&ctx.movements_path, b"").unwrap();
    let host = ToolProcessBrokerHost::default();
    let arguments = |sequence| if sequence == 8 { json!({"value":"artifact example"}) } else { json!({}) };
    let tool_id = |sequence| match sequence {
        3 | 13 => "authored.alias",
        12 => "authored.scoped",
        _ => "authored.echo",
    };
    let envelope = |sequence| {
        let payload = serde_json::to_vec(&json!({"tool":tool_id(sequence), "arguments":arguments(sequence), "correlation_id":format!("call-{sequence}")})).unwrap();
        ToolProcessEnvelope {
            artifact: ArtifactRef { id: ArtifactId::new(format!("input:{sequence}")), content: ContentHash::of_bytes(&payload) },
            label: "tool.call".into(), payload, artifact_bindings: Vec::new(),
            metadata: Some(InvocationMetadata::new(GrantProjection {
                tools: vec!["authored.echo".into(), "authored.alias".into(), "authored.scoped".into()], worker_environment_kinds: vec!["test.room".into()], ..Default::default()
            }).with_runtime(RuntimeInvocationMetadata {
                run: RunId::new("run_authoring"), ee: EeId::new("ee_authoring"), worker: InterfaceId::new("system_worker_test"),
                checkpoint: CheckpointId::new(format!("call_{sequence}")), interface: ctx.interface.clone(), epoch: 0, run_record_hash: None,
            })), source_movement_seq: match sequence {
                12 => 4,
                13 => 5,
                4.. => sequence + 2,
                _ => sequence,
            }, worker_environment: None, worker_environment_kind: None, output_source: None,
        }
    };
    assert!(matches!(host.invoke(&ctx, "authored.echo".into(), json!({}), envelope(1), false).await,
        ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::InvocationRefused { .. })));
    std::fs::write(kind.join("worker_environment_kind.json"), serde_json::to_vec(&json!({
        "worker_environment_kind_id": "test.room", "substrate_family": "process_exec", "door_driver_refs": ["test_door"],
        "tool_process_bins": ["node"], "worker_consent": "explicit_ceiling", "supported_executor_kinds": ["exec"],
        "capabilities": ["process_exec"], "boundary_defaults": {"read_root_refs": [{"path":"scope_root", "access":["read"]}], "write_root_refs": [],
            "protected_read_only_subpaths": [], "network_ceiling": "denied"}, "lifecycle": {"requires_release": false}
    })).unwrap()).unwrap();
    let runtime_config = kind.join("packages/runtime.cnf");
    std::fs::write(&runtime_config, "openssl_conf = init\n[init]\n").unwrap();
    std::fs::write(tool.join("tool.json"), serde_json::to_vec(&json!({
        "id": "authored.echo", "interface": "system_test_room", "supported_worker_environment_kinds": ["test.room"],
        "process": {"bin": "node", "args": ["owner://packages/tool.js", "owner://packages/value.txt"],
            "required_files": ["owner://packages/value.txt", "owner://packages/runtime.cnf"],
            "env": {"OPENSSL_CONF": runtime_config}, "inherit_env": ["PATH"], "authority": ProcessAuthoritySpec {
                read_env_paths: BTreeSet::from(["OPENSSL_CONF".into()]), ..Default::default()
            }},
        "args_schema": {"type": "object", "additionalProperties": false}, "is_world_effect": false
    })).unwrap()).unwrap();
    for (id, slot) in [("authored.alias", "absent_root"), ("authored.scoped", "scope_root")] {
        let mut descriptor: serde_json::Value = serde_json::from_slice(&std::fs::read(tool.join("tool.json")).unwrap()).unwrap();
        descriptor["id"] = json!(id);
        descriptor["process"]["authority"]["read_root_slots"] = json!([slot]);
        let directory = registry.join("tool").join(id);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("tool.json"), serde_json::to_vec(&descriptor).unwrap()).unwrap();
    }
    let script = r#"const fs = require('fs'), path = require('path');
const value = 'VERSION:' + fs.readFileSync(process.argv[2], 'utf8') + (process.env.MARK ? ':' + process.env.MARK : '');
const requests = new Map();
const send = message => process.stdout.write(JSON.stringify(message) + '\n');
require('readline').createInterface({input:process.stdin}).on('line', line => {
 const message = JSON.parse(line);
 if (message.kind === 'invoke') {
  requests.set(message.request_id, message);
  send({kind:'worker_environment_lifecycle', request_id:message.request_id, event_id:1,
        worker_environment_id:message.envelope.worker_environment.worker_environment_id, lifecycle:'active'});
 } else if (message.kind === 'worker_environment_lifecycle_recorded') {
  if (message.error) throw Error(message.error);
  const output = path.join(process.env.OCTOPUS_VAR_DIR, 'output-' + message.request_id);
  fs.writeFileSync(output, JSON.stringify({value, pid:process.pid}));
  fs.writeFileSync(output + '.json', JSON.stringify({label:'authored.echo.result', payload_path:output}));
  send({kind:'complete', request_id:message.request_id, artifact_manifest_paths:[output + '.json']});
 } else if (message.kind === 'release_worker_environment') {
  send({kind:'worker_environment_released', request_id:message.request_id});
 }
});"#;
    let entry = kind.join("packages/tool.js");
    std::fs::write(&entry, script.replace("VERSION", "alpha")).unwrap();
    std::fs::write(kind.join("packages/value.txt"), "one").unwrap();
    let malformed = registry.join("worker_environment_kind/unrelated");
    std::fs::create_dir_all(&malformed).unwrap();
    std::fs::write(malformed.join("worker_environment_kind.json"), "{").unwrap();
    let authoring: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../artifact/artifact-codebase-layout/artifact.json"))).unwrap();
    let contract = &authoring["executable_authoring"];
    for event in contract["tool_protocol"]["lifecycle_exchange"].as_array().unwrap() {
        if event["direction"] == "door_to_process" {
            serde_json::from_value::<ToolProcessInput>(event["frame"].clone()).unwrap();
        } else {
            serde_json::from_value::<ToolProcessOutput>(event["frame"].clone()).unwrap();
        }
    }
    let example = &contract["examples"]["python_tool"];
    assert!(matches!(host.invoke(&ctx, "authored.echo".into(), json!({}), envelope(1), true).await,
        ToolProcessBrokerInvocation::InterruptedEffectUnknown));
    assert!(host.shards.lock().await.by_key.is_empty());
    let mut observed = Vec::new();
    let executable = kind.join("packages/bin/tool");
    for sequence in [2, 3, 12, 13, 4, 5, 6, 7, 8] {
        if sequence == 4 { std::fs::write(&entry, script.replace("VERSION", "omega")).unwrap(); }
        if sequence == 5 { std::fs::write(kind.join("packages/value.txt"), "two").unwrap(); }
        if (6..=7).contains(&sequence) {
            std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
            std::fs::write(&executable, format!("#!/bin/sh\nexport MARK={}\nexec node \"$@\"\n", if sequence == 6 {"before"} else {"after"})).unwrap();
            if sequence == 6 {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
                let kind_path = kind.join("worker_environment_kind.json");
                let mut spec: serde_json::Value = serde_json::from_slice(&std::fs::read(&kind_path).unwrap()).unwrap();
                spec["tool_process_bins"] = json!(["node", "owner://packages/bin/tool"]);
                std::fs::write(&kind_path, serde_json::to_vec(&spec).unwrap()).unwrap();
                let tool_path = tool.join("tool.json");
                let mut descriptor: serde_json::Value = serde_json::from_slice(&std::fs::read(&tool_path).unwrap()).unwrap();
                descriptor["process"]["bin"] = json!("owner://packages/bin/tool");
                descriptor["process"]["authority"]["executable_commands"] = json!(["node"]);
                std::fs::write(&tool_path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
            }
        }
        if sequence == 8 {
            let kind_path = kind.join("worker_environment_kind.json");
            let mut spec: serde_json::Value = serde_json::from_slice(&std::fs::read(&kind_path).unwrap()).unwrap();
            spec["tool_process_bins"] = json!([example["descriptor"]["process"]["bin"], "node"]);
            std::fs::write(&kind_path, serde_json::to_vec(&spec).unwrap()).unwrap();
            let mut descriptor = example["descriptor"].clone();
            descriptor["id"] = json!("authored.echo");
            descriptor["interface"] = json!("system_test_room");
            descriptor["supported_worker_environment_kinds"] = json!(["test.room"]);
            std::fs::write(tool.join("tool.json"), serde_json::to_vec(&descriptor).unwrap()).unwrap();
            std::fs::create_dir_all(kind.join("packages/example")).unwrap();
            std::fs::write(kind.join("packages/example/tool.py"), example["source"].as_str().unwrap()).unwrap();
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let invoked = host.invoke(&ctx, tool_id(sequence).into(), arguments(sequence), envelope(sequence), false);
            tokio::pin!(invoked);
            let output = tokio::select! {
                output = outputs.recv() => output.unwrap(),
                result = &mut invoked => match result {
                    ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Unavailable { error } | ToolProcessBrokerResult::CapabilityFailed { error } | ToolProcessBrokerResult::OutputPending { error }) => panic!("{error}\n{}", stderr(&ctx)),
                    _ => panic!("invocation finished before its artifact arrived"),
                },
            };
            let body: serde_json::Value = serde_json::from_slice(&output.artifact.read_payload_bytes().unwrap()).unwrap();
            output.retained.unwrap().reply.send(Ok(octopus_ledger::ArtifactDelivery::Complete {
                verdict: DeliveryVerdict::Admitted {seq:sequence + 100},
            })).unwrap();
            (invoked.await, body)
        }).await.unwrap_or_else(|_| panic!("call {sequence} timed out\n{}", stderr(&ctx)));
        match result.0 {
            ToolProcessBrokerInvocation::Completed(_) => observed.push(result.1),
            ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Unavailable { error } | ToolProcessBrokerResult::CapabilityFailed { error } | ToolProcessBrokerResult::OutputPending { error }) => panic!("{error}"),
            _ => panic!("authored invocation did not complete"),
        }
        if [2, 3, 12, 13].contains(&sequence) {
            let shards = host.shards.lock().await;
            let expected = if sequence < 12 { 1 } else { 2 };
            assert_eq!(shards.by_key.len(), expected);
            let mut generations = BTreeSet::new();
            for (key, state) in &shards.by_key {
                let state = state.lock().await;
                assert_eq!(state.processes.len(), 1);
                let process = state.processes.get(key).unwrap();
                let generation = tool_process_generation_key_from_process_key(key).unwrap();
                assert_eq!(process.generation_dir.file_name().unwrap().to_str().unwrap(), generation);
                assert!(generations.insert(generation));
                let owner = read_tool_process_owner_record(&process.generation_dir).unwrap().unwrap();
                assert_eq!(Some(owner.process_id), process.child.id());
                assert!(try_acquire_tool_process_owner_lease(&process.generation_dir).unwrap().is_none());
                if let Some((original, admitted_key)) = state.declarations_by_tool.get("authored.alias") {
                    assert!(original.authority.read_root_slots.contains("absent_root"));
                    assert!(admitted_key.declaration.authority.read_root_slots.is_empty());
                    assert_eq!(admitted_key, key);
                }
            }
        }
    }
    assert_eq!(observed[..8].iter().map(|body| body["value"].as_str().unwrap()).collect::<Vec<_>>(), ["alpha:one", "alpha:one", "alpha:one", "alpha:one", "omega:one", "omega:two", "omega:two:before", "omega:two:after"]);
    assert_eq!(observed[0]["pid"], observed[1]["pid"]);
    assert_eq!(observed[1]["pid"], observed[3]["pid"]);
    assert_ne!(observed[1]["pid"], observed[2]["pid"]);
    assert_ne!(observed[2]["pid"], observed[3]["pid"]);
    assert_ne!(observed[3]["pid"], observed[4]["pid"]);
    assert_ne!(observed[4]["pid"], observed[5]["pid"]);
    assert_ne!(observed[6]["pid"], observed[7]["pid"]);
    let result: ToolResultArtifact = serde_json::from_value(observed[8].clone()).unwrap();
    assert!(result.ok);
    assert_eq!(result.tool, "authored.echo");
    assert_eq!(result.correlation_id.as_deref(), Some("call-8"));
    assert_eq!(result.body.unwrap(), arguments(8));
    let entry = kind.join("packages/example/tool.py");
    std::fs::remove_file(&entry).unwrap();
    std::os::unix::fs::symlink(root.join("outside.py"), &entry).unwrap();
    std::fs::write(root.join("outside.py"), example["source"].as_str().unwrap()).unwrap();
    assert!(matches!(host.invoke(&ctx, "authored.echo".into(), arguments(8), envelope(9), false).await,
        ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Unavailable { .. })));
    let sibling = registry.join("worker_environment_kind/test.sibling");
    std::fs::rename(&kind, &sibling).unwrap();
    std::os::unix::fs::symlink(&sibling, &kind).unwrap();
    assert!(matches!(host.invoke(&ctx, "authored.echo".into(), arguments(8), envelope(10), false).await,
        ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Unavailable { .. })));
    std::fs::remove_file(&kind).unwrap();
    std::fs::rename(&sibling, &kind).unwrap();
    std::fs::write(kind.join("worker_environment_kind.json"), "{").unwrap();
    assert!(matches!(host.invoke(&ctx, "authored.echo".into(), arguments(8), envelope(11), false).await,
        ToolProcessBrokerInvocation::Outcome(ToolProcessBrokerResult::Unavailable { .. })));
    let shards = host.shards.lock().await;
    for state in shards.by_key.values() { state.lock().await.shutdown(&ctx, false).await.unwrap(); }
}
