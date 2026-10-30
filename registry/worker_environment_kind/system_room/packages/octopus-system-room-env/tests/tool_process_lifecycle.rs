use octopus_file_write_contract::{JsonFormat, Preimage};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interfaces::{
    GrantProjection, InterfaceArtifactFile, InterfaceDescriptor, InvocationMetadata,
    RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_tool_registry::{load_tool_descriptor_from_dir, ToolDescriptor};
use octopus_worker_env::{
    WorkerEnvKindRegistry, WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId,
};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../..")
        .canonicalize()
        .unwrap()
}

fn canonical_hash(value: &impl Serialize) -> String {
    let value = serde_json::to_value(value).unwrap();
    ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&value)).to_hex()
}

async fn write_message(stdin: &mut tokio::process::ChildStdin, value: &Value) {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    stdin.write_all(&bytes).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn read_message(
    stdout: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
) -> Value {
    let line = tokio::time::timeout(std::time::Duration::from_secs(10), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

fn context_carrier(var_dir: &Path, request_id: u64, bindings: &Value, metadata: &impl Serialize) -> Value {
    let bytes = serde_json::to_vec(&json!({"artifact_bindings": bindings, "metadata": metadata})).unwrap();
    let content_hash = ContentHash::of_bytes(&bytes);
    let directory = var_dir.join("invocations").canonicalize().unwrap().join(request_id.to_string()).join("artifact");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("invocation-context-{}.json", content_hash.to_hex()));
    std::fs::write(&path, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    json!({"path": path, "content_hash": content_hash, "byte_len": bytes.len()})
}

#[tokio::test]
async fn file_write_process_reports_active_before_completion_and_accepts_release() {
    file_write_process_case(false).await;
}

#[tokio::test]
async fn ordered_file_write_process_reports_durable_partial_effects() {
    file_write_process_case(true).await;
}

#[test]
fn ordered_patch_journal_preserves_replay_and_bounds_storage() {
    use octopus_interfaces::InterfaceInvocationEnvelope;
    use octopus_system_room_env::execute_file_write_result;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let target = root.join("target.json");
    let peer = root.join("peer.json");
    let effects = root.join("effects");
    let roots = BTreeMap::from([("run_records_root".into(), root.clone())]);
    let kinds = vec!["system_room".into()];
    let descriptor = load_tool_descriptor_from_dir(&repository_root().join("registry/tool"), "file.write").unwrap().unwrap();
    let schema = octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema).unwrap();
    let pretty = |value: &Value| format!("{}\n", serde_json::to_string_pretty(value).unwrap());
    let hash = |text: &str| ContentHash::of_bytes(text.as_bytes()).to_hex();
    let input = |args: &Value| {
        let payload = serde_json::to_vec(&json!({"tool":"file.write","arguments":args})).unwrap();
        let content = ContentHash::of_bytes(&payload);
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef { id: ArtifactId::new(format!("tool.call:{}", content.to_hex())), content },
            artifact_file: InterfaceArtifactFile::write(root.join("calls"), "tool.call", &payload).unwrap(),
            artifact_binding_payloads: vec![],
            metadata: Some(InvocationMetadata::new(GrantProjection { worker_environment_kinds:kinds.clone(), ..Default::default() })),
        }
    };
    let apply = |envelope: &InterfaceInvocationEnvelope, args: &Value| {
        execute_file_write_result(envelope, args, &kinds, &roots, &effects, 8 * 1024 * 1024)
    };
    let mut value = json!({"generation":0,"unchanged_context":"context ".repeat(4096)});
    let mut before = pretty(&value);
    std::fs::write(&target, &before).unwrap();
    let mut payload_bytes = 0;
    let mut full_payload_bytes = 0;
    let mut intent_bytes = 0;
    for generation in 1..=100 {
        value["generation"] = json!(generation);
        let after = String::from_utf8(if generation == 1 { JsonFormat::Pretty } else { JsonFormat::Compact }.encode(&value).unwrap()).unwrap();
        let mut args = json!({"ordered":true,"writes":[{"path":target,"expected_before_content":before,
            "expected_before_hash":hash(&before),"patch":[{"op":"replace","path":"/generation","value":generation}],"expected_after_hash":hash(&after)}]});
        if generation > 1 {
            args["writes"][0]["expected_before_content"] = serde_json::to_value(Preimage::encode(before.as_bytes()).unwrap()).unwrap();
            args["writes"][0]["json_format"] = json!("compact");
        }
        schema.validate(&args).unwrap();
        let envelope = input(&args);
        if generation == 2 {
            let too_small = (before.len() + after.len() - 1) as u64;
            assert!(execute_file_write_result(&envelope, &args, &kinds, &roots, &effects, too_small).is_err());
            assert_eq!(std::fs::read_to_string(&target).unwrap(), before);
            assert!(!effects.join("ordered").join(envelope.artifact.content.to_hex()).exists());
        }
        let receipt = apply(&envelope, &args).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), after);
        let dir = effects.join("ordered").join(envelope.artifact.content.to_hex());
        let intent = std::fs::read(dir.join("intent.json")).unwrap();
        let record: Value = serde_json::from_slice(&intent).unwrap();
        assert_eq!(record["schema_version"], 3);
        assert_eq!(record["batch_content_hash"], ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&args)).to_hex());
        assert_eq!(record["artifact"], serde_json::to_value(&envelope.artifact).unwrap());
        assert!(record.get("batch").is_none());
        assert!(!dir.join("0.before").exists());
        assert!(!dir.join("0.bytes").exists());
        if generation <= 2 {
            let retained = ["intent.json", "0.receipt.json"].map(|name| std::fs::read(dir.join(name)).unwrap());
            assert_eq!(apply(&envelope, &args).unwrap(), receipt);
            assert_eq!(["intent.json", "0.receipt.json"].map(|name| std::fs::read(dir.join(name)).unwrap()), retained);
        }
        payload_bytes += serde_json::to_vec(&args).unwrap().len();
        let mut full = args.clone();
        let entry = full["writes"][0].as_object_mut().unwrap();
        entry.remove("patch"); entry.remove("expected_after_hash"); entry.remove("json_format"); entry.insert("expected_before_content".into(), json!(before)); entry.insert("content".into(), json!(after));
        full_payload_bytes += serde_json::to_vec(&full).unwrap().len();
        intent_bytes += intent.len();
        before = after;
    }
    assert!(payload_bytes * 100 < full_payload_bytes * 55);
    assert!(intent_bytes < 100 * 1024);
    eprintln!("ordered storage benchmark: calls=100 full_payload_bytes={full_payload_bytes} patch_payload_bytes={payload_bytes} intent_bytes={intent_bytes} before_copy_bytes=0 retained_stage_bytes=0");

    value["generation"] = json!(101);
    let after = pretty(&value);
    std::fs::write(&peer, "independent edit").unwrap();
    let args = json!({"ordered":true,"writes":[
        {"path":target,"expected_before_content":before,"expected_before_hash":hash(&before),
            "patch":[{"op":"replace","path":"/generation","value":101}],"expected_after_hash":hash(&after)},
        {"path":peer,"content":"stale postimage","expected_before_content":"prior","expected_before_hash":hash("prior")},
        {"path":root.join("unattempted"),"content":"new","expected_before_content":null,"expected_before_hash":null}
    ]});
    schema.validate(&args).unwrap();
    let envelope = input(&args);
    let error = apply(&envelope, &args).unwrap_err();
    assert_eq!(error.code, "file_write_incomplete");
    let details = error.details.unwrap();
    assert_eq!(details["file_write"]["writes"][0]["outcome"], "applied");
    assert_eq!(details["file_write"]["writes"][1]["outcome"], "refused");
    assert_eq!(details["file_write"]["writes"][2]["outcome"], "not_attempted");
    std::fs::write(&target, "later independent edit").unwrap();
    assert_eq!(apply(&envelope, &args).unwrap_err().details.unwrap(), details);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "later independent edit");
    assert!(!root.join("unattempted").exists());
    let dir = effects.join("ordered").join(envelope.artifact.content.to_hex());
    for damaged in [json!({"artifact":envelope.artifact,"batch":args}), json!({"schema_version":2})] {
        std::fs::write(dir.join("intent.json"), serde_json::to_vec(&damaged).unwrap()).unwrap();
        assert_eq!(apply(&envelope, &args).unwrap_err().code, "file_write_evidence_failed");
    }
    for (field, bad) in [("expected_before_hash",json!(hash("wrong"))), ("expected_after_hash",json!(hash("wrong"))),
        ("expected_before_content",Value::Null), ("patch",json!([])), ("content",json!("ambiguous")), ("foreign",json!(true))] {
        let mut invalid = args.clone(); invalid["writes"][0][field] = bad;
        assert_eq!(apply(&input(&invalid), &invalid).unwrap_err().code, "file_write_evidence_failed", "{field}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "later independent edit");
    }
    let outside = tempfile::tempdir().unwrap();
    let mut denied = args.clone(); denied["writes"][0]["path"] = json!(outside.path().join("outside.json"));
    assert!(apply(&input(&denied), &denied).is_err());
    let mut ungranted = input(&args); ungranted.metadata.as_mut().unwrap().grant.worker_environment_kinds.clear();
    assert!(apply(&ungranted, &args).is_err());
    let mut unordered = args.clone(); unordered.as_object_mut().unwrap().remove("ordered");
    assert!(schema.validate(&unordered).is_err());
    let content = json!({"ordered":true,"writes":[{"path":root.join("text.txt"),"content":"ordinary text",
        "expected_before_content":null,"expected_before_hash":null}]});
    schema.validate(&content).unwrap();
    assert_eq!(apply(&input(&content), &content).unwrap()["writes"][0]["outcome"], "applied");
    assert_eq!(std::fs::read_to_string(root.join("text.txt")).unwrap(), "ordinary text");
}

async fn file_write_process_case(ordered: bool) {
    let repository_root = repository_root();
    let interface_dir = repository_root.join("registry/interface/system_room");
    let interface: InterfaceDescriptor = octopus_descriptors::parse_strict(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    )
    .unwrap();
    let life: Value =
        serde_json::from_slice(&std::fs::read(interface_dir.join("life.json")).unwrap()).unwrap();
    let host_io_limits_json = serde_json::to_string(&life["processes"][0]["io_limits"]).unwrap();
    let tool_root = repository_root.join("registry/tool");
    let descriptor: ToolDescriptor = load_tool_descriptor_from_dir(&tool_root, "file.write")
        .unwrap()
        .unwrap();
    let kind_root = repository_root.join("registry/worker_environment_kind");
    let registry = WorkerEnvKindRegistry::load_from_dir(&kind_root).unwrap();
    let kind_id = WorkerEnvironmentKindId::new("system_room");
    let (kind, _) = registry.get(&kind_id).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let run_records_root = temp.path().join("runs");
    let ee_instances_root = temp.path().join("ees");
    let process_var_dir = temp.path().join("process-var");
    let durable_var_dir = temp.path().join("durable-var");
    std::fs::create_dir_all(&run_records_root).unwrap();
    std::fs::create_dir_all(&ee_instances_root).unwrap();
    std::fs::create_dir_all(&process_var_dir).unwrap();
    std::fs::create_dir_all(&durable_var_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&durable_var_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let durable_var_dir = std::fs::canonicalize(durable_var_dir).unwrap();
    let root_slots = BTreeMap::from([
        ("run_records_root".to_string(), run_records_root.clone()),
        (
            "registry_interface_root".to_string(),
            repository_root.join("registry/interface"),
        ),
        ("ee_instances_root".to_string(), ee_instances_root),
    ]);
    let tool_hashes = BTreeMap::from([("file.write".to_string(), canonical_hash(&descriptor))]);
    let tool_process_bins =
        BTreeMap::from([("file.write".to_string(), descriptor.process.bin.clone())]);
    let granted_tools = vec!["file.write".to_string()];
    let granted_kinds = vec![kind_id.clone()];
    let allowance = root_slots.keys().cloned().collect::<BTreeSet<_>>();
    let door = InterfaceId::new("system_room");
    let worker = InterfaceId::new("system_worker_flash");
    let formation = registry
        .form(&WorkerEnvironmentFormationRequest {
            door_additional_root_bindings: &Default::default(),
            door_interface: &door,
            door_descriptor_hash: &canonical_hash(&interface),
            door_driver_ref: &interface.interface_driver_ref,
            tool: "file.write",
            tool_descriptor_hashes: &tool_hashes,
            tool_process_bins: &tool_process_bins,
            tool_worker_environment_kinds: &descriptor.supported_worker_environment_kinds,
            worker: &worker,
            ee: "ee_test",
            run: "run_test",
            granted_worker_environment_kinds: &granted_kinds,
            granted_tools: &granted_tools,
            door_root_bindings: &root_slots,
            door_instance_dir: &interface_dir,
            ee_root_slot_allowance: Some(&allowance),
        })
        .unwrap();
    let target = run_records_root.join("created.json");
    let peer = run_records_root.join("peer.json");
    std::fs::write(&peer, "peer generation 2").unwrap();
    let expected = if ordered { "{\n  \"created\": true\n}\n" } else { "{\"created\":true}\n" };
    if ordered { std::fs::write(&target, "{}\n").unwrap(); }
    let arguments = if ordered { json!({"ordered":true,"writes":[
        {"path":target,"patch":[{"op":"add","path":"/created","value":true}],"expected_before_content":"{}\n","expected_before_hash":ContentHash::of_bytes(b"{}\n").to_hex(),"expected_after_hash":ContentHash::of_bytes(expected.as_bytes()).to_hex()},
        {"path":peer,"content":"stale edit","expected_before_content":"peer generation 1","expected_before_hash":ContentHash::of_bytes(b"peer generation 1").to_hex()}
    ]}) } else { json!({"path":target,"content":"{\"created\":true}\n","expected_before_hash":null}) };
    octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema).unwrap().validate(&arguments).unwrap();
    let payload = serde_json::to_vec(&json!({
        "tool": "file.write",
        "arguments": arguments,
        "correlation_id": "write-1"
    }))
    .unwrap();
    let content = ContentHash::of_bytes(&payload);
    let artifact = ArtifactRef {
        id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
        content,
    };
    let input_artifact = InterfaceArtifactFile::write(
        process_var_dir.join("invocations/1/artifact"),
        "tool.call",
        &payload,
    )
    .unwrap();
    let metadata = InvocationMetadata::new(GrantProjection {
        tools: granted_tools,
        worker_environment_kinds: vec![kind_id.as_str().to_string()],
        ee_root_slot_allowance: Some(allowance.into_iter().collect()),
        ..GrantProjection::default()
    })
    .with_runtime(RuntimeInvocationMetadata {
        run: RunId::new("run_test"),
        ee: EeId::new("ee_test"),
        worker,
        checkpoint: CheckpointId::new("checkpoint_test"),
        interface: door.clone(),
        epoch: 0,
        run_record_hash: None,
    });
    let descriptor_json = serde_json::to_string(&interface).unwrap();
    let root_slots_json = serde_json::to_string(&root_slots).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_octopus-file-write-tool"))
        .env("OCTOPUS_INTERFACE_ID", door.as_str())
        .env("OCTOPUS_INTERFACE_DIR", &interface_dir)
        .env("OCTOPUS_INTERFACE_DESCRIPTOR_JSON", descriptor_json)
        .env("OCTOPUS_INTERFACE_HOST_IO_LIMITS", host_io_limits_json)
        .env("OCTOPUS_VAR_DIR", &process_var_dir)
        .env("OCTOPUS_TOOL_PROCESS_DURABLE_VAR_DIR", &durable_var_dir)
        .env("OCTOPUS_PRIVATE_READS_JSON", "{}")
        .env("OCTOPUS_ROOT_SLOTS_JSON", root_slots_json)
        .env("TMPDIR", &process_var_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut stdout = tokio::io::BufReader::new(stdout).lines();
    write_message(
        &mut stdin,
        &json!({
            "kind": "invoke",
            "request_id": 1,
            "descriptor": descriptor,
            "envelope": {
                "artifact": artifact,
                "label": "tool.call",
                "manifest_path": input_artifact.manifest_path(),
                "context": context_carrier(&process_var_dir, 1, &json!([]), &metadata),
                "source_movement_seq": 41,
                "worker_environment": formation,
                "worker_environment_kind": &*kind
            }
        }),
    )
    .await;
    let lifecycle = read_message(&mut stdout).await;
    assert_eq!(lifecycle["kind"], "worker_environment_lifecycle");
    assert_eq!(lifecycle["lifecycle"], if ordered { "failed" } else { "active" }, "{lifecycle}");
    assert_eq!(
        lifecycle["worker_environment_id"],
        formation.worker_environment_id.as_str()
    );
    write_message(
        &mut stdin,
        &json!({
            "kind": "worker_environment_lifecycle_recorded",
            "request_id": 1,
            "event_id": lifecycle["event_id"],
            "error": null
        }),
    )
    .await;
    let completed = read_message(&mut stdout).await;
    assert_eq!(completed["kind"], "complete");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        expected
    );
    if ordered {
        assert_eq!(std::fs::read_to_string(&peer).unwrap(), "peer generation 2");
        let effects = durable_var_dir.join("file-write-effects/ordered").join(content.to_hex());
        let mut applied: Value = serde_json::from_slice(&std::fs::read(effects.join("0.receipt.json")).unwrap()).unwrap();
        let mut refused: Value = serde_json::from_slice(&std::fs::read(effects.join("1.receipt.json")).unwrap()).unwrap();
        assert_eq!(applied.as_object_mut().unwrap().remove("publication").unwrap()["digest_algorithm"], "blake3");
        assert_eq!(refused.as_object_mut().unwrap().remove("publication").unwrap()["outcome"], "not_applied");
        assert_eq!(applied["outcome"], "applied");
        assert_eq!(refused["outcome"], "refused");
        assert_eq!(refused["observed_hash"], ContentHash::of_bytes(b"peer generation 2").to_hex());
        let returned = InterfaceArtifactFile::new(completed["artifact_manifest_paths"][0].as_str().unwrap()).unwrap()
            .read_bounded(1024 * 1024, 8 * 1024 * 1024).unwrap();
        let result: Value = serde_json::from_slice(&returned.payload_bytes).unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], "file_write_incomplete");
        assert_eq!(result["error"]["details"]["file_write"]["writes"], json!([applied,refused]));
    } else { assert_eq!(
        std::fs::read_dir(durable_var_dir.join("file-write-effects"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(
                |entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            )
            .count(),
        1
    ); }
    assert!(!process_var_dir.join("file-write-effects").exists());
    write_message(
        &mut stdin,
        &json!({
            "kind": "release_worker_environment",
            "request_id": 2,
            "formation": formation,
            "worker_environment_kind": &*kind,
            "mode": "live_owner"
        }),
    )
    .await;
    let released = read_message(&mut stdout).await;
    assert_eq!(released["kind"], "worker_environment_released");
    drop(stdin);
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn client_mcp_process_preserves_bound_scope_emission_verdict_and_lifecycle() {
    let repository_root = repository_root();
    let interface_dir = repository_root.join("registry/interface/system_context");
    let interface: InterfaceDescriptor = octopus_descriptors::parse_strict(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let life: Value = serde_json::from_slice(&std::fs::read(interface_dir.join("life.json")).unwrap()).unwrap();
    let mut descriptor = load_tool_descriptor_from_dir(&repository_root.join("registry/tool"), "client.mcp").unwrap().unwrap();
    descriptor.id = octopus_descriptors::ToolDescriptorId::new(format!("{}__fixture", descriptor.id));
    let tool = descriptor.id.as_str().to_string();
    assert!(!descriptor.process.authority.allow_network && !descriptor.process.authority.allow_local_ipc);
    assert!(descriptor.process.authority.read_root_slots.is_empty() && descriptor.process.authority.write_root_slots.is_empty());
    let registry = WorkerEnvKindRegistry::load_from_dir(&repository_root.join("registry/worker_environment_kind")).unwrap();
    let kind_id = WorkerEnvironmentKindId::new("system_room");
    let (kind, _) = registry.get(&kind_id).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let process_var_dir = root.join("process-var");
    std::fs::create_dir_all(process_var_dir.join("invocations")).unwrap();
    let root_slots = BTreeMap::new();
    let allowance = BTreeSet::new();
    let tool_hashes = BTreeMap::from([(tool.clone(), canonical_hash(&descriptor))]);
    let tool_bins = BTreeMap::from([(tool.clone(), descriptor.process.bin.clone())]);
    let granted_tools = vec![tool.clone()];
    let granted_kinds = vec![kind_id.clone()];
    let door = interface.id.clone();
    let worker = InterfaceId::new("system_worker_fixture");
    let formation = registry.form(&WorkerEnvironmentFormationRequest {
        door_additional_root_bindings: &Default::default(),
        door_interface: &door, door_descriptor_hash: &canonical_hash(&interface),
        door_driver_ref: &interface.interface_driver_ref, tool: &tool,
        tool_descriptor_hashes: &tool_hashes, tool_process_bins: &tool_bins,
        tool_worker_environment_kinds: &descriptor.supported_worker_environment_kinds,
        worker: &worker, ee: "ee_mcp_fixture", run: "run_mcp_fixture",
        granted_worker_environment_kinds: &granted_kinds, granted_tools: &granted_tools,
        door_root_bindings: &root_slots, door_instance_dir: &interface_dir,
        ee_root_slot_allowance: Some(&allowance),
    }).unwrap();
    assert!(formation.door_root_bindings.is_empty());
    let mut bindings = Vec::new();
    for (id, bytes) in [("context:visible", b"bound context".as_slice()), ("context:hidden", b"private context".as_slice())] {
        let original = InterfaceArtifactFile::write(root.join(id.replace(':', "-")), "context", bytes).unwrap();
        let artifact = ArtifactRef { id: ArtifactId::new(id), content: ContentHash::of_bytes(&std::fs::read(original.manifest_path()).unwrap()) };
        let payload_hash = ContentHash::of_bytes(bytes);
        let reference_hash = ContentHash::of_bytes(&serde_json::to_vec(&artifact).unwrap());
        let carrier = process_var_dir.join("invocations/bound-artifacts").join(format!("{}-{}", reference_hash.to_hex(), payload_hash.to_hex()));
        std::fs::create_dir_all(&carrier).unwrap();
        std::fs::write(carrier.join("payload"), bytes).unwrap();
        std::fs::write(carrier.join("artifact.json"), serde_json::to_vec(&json!({
            "label":"tool-process.bound-artifact", "payload_path":carrier.join("payload"),
            "payload_content_hash":payload_hash.to_hex(),
        })).unwrap()).unwrap();
        bindings.push(json!({"artifact":artifact, "manifest_path":carrier.join("artifact.json"), "payload_hash":payload_hash}));
    }
    let target = "system_present_fixture";
    let metadata = InvocationMetadata::new(serde_json::from_value(json!({
        "tools":granted_tools, "interfaces":[target], "worker_environment_kinds":[kind_id.as_str()],
        "ee_root_slot_allowance":[], "artifact_access":{"context:visible":{"read":true},"context:hidden":{"read":false}},
    })).unwrap()).with_runtime(RuntimeInvocationMetadata {
        run: RunId::new("run_mcp_fixture"), ee: EeId::new("ee_mcp_fixture"), worker,
        checkpoint: CheckpointId::new("checkpoint_mcp_fixture"), interface: door.clone(), epoch:0, run_record_hash:None,
    });
    let ambient = root.join("ambient.txt");
    std::fs::write(&ambient, "ambient data must stay unread").unwrap();
    let authoring: Value = serde_json::from_slice(&std::fs::read(
        repository_root.join("registry/artifact/artifact-home-widget-authoring/artifact.json"),
    ).unwrap()).unwrap();
    let mut document = authoring["contract"]["example"].clone();
    document["id"]=json!("home.weather-fixture");document["glyph"]=json!("weather");document["revision"]=json!(2);
    document["data"]=json!({"summary":"Publication fixture: 10 °C"});document["render"]["density"]=json!("comfortable");
    let mut invalid=document.clone();invalid["glyph"]=json!("cloud");invalid["revision"]=json!(1);
    let publish = json!({"method":"tools/call","params":{"name":"publish_widget","arguments":{"interface":target,"document":document}}});
    let widget_requests = [json!({"schema":"octopus.client_action","schema_version":1,"action":"list_widgets","request_id":"inventory"}),
        json!({"schema":"octopus.client_action","schema_version":1,"action":"set_widget","request_id":"hide","changes":[{"widget_id":"hello","source_digest":"sha256:exact","state":"hidden"}]})];
    let requests = [
        json!({"method":"tools/list"}),
        json!({"method":"tools/call","params":{"name":"read_artifact","arguments":{"id":"context:visible"}}}),
        json!({"method":"tools/call","params":{"name":"read_artifact","arguments":{"id":"context:hidden"}}}),
        json!({"method":"tools/call","params":{"name":"read_artifact","arguments":{"id":ambient}}}),
        json!({"method":"tools/call","params":{"name":"publish_widget","arguments":{"interface":target,"document":invalid}}}),
        publish.clone(), publish,
        json!({"method":"tools/call","params":{"name":"publish_widget","arguments":{"interface":"system_foreign","document":document}}}),
        json!({"method":"tools/call","params":{"name":"list_widgets","arguments":{"interface":target,"action":widget_requests[0]}}}),
        json!({"method":"tools/call","params":{"name":"set_widget","arguments":{"interface":target,"action":widget_requests[1]}}}),
    ];
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_octopus-artifact-bound-tool"))
        .env_clear().current_dir(&root)
        .env("OCTOPUS_INTERFACE_ID", door.as_str()).env("OCTOPUS_INTERFACE_DIR", &interface_dir)
        .env("OCTOPUS_INTERFACE_DESCRIPTOR_JSON", serde_json::to_string(&interface).unwrap())
        .env("OCTOPUS_INTERFACE_HOST_IO_LIMITS", serde_json::to_string(&life["processes"][0]["io_limits"]).unwrap())
        .env("OCTOPUS_VAR_DIR", &process_var_dir).env("OCTOPUS_PRIVATE_READS_JSON", "{}")
        .env("OCTOPUS_ROOT_SLOTS_JSON", "{}").env("TMPDIR", &process_var_dir)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit()).kill_on_drop(true).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let refused = json!({"refused":{"reason":"policy","detail":{"message":"fixture gate refusal"}}});
    for (index, arguments) in requests.into_iter().enumerate() {
        let request_id = index as u64 + 1;
        octopus_tool_registry::ToolArgumentValidator::compile(&descriptor.args_schema).unwrap().validate(&arguments).unwrap();
        let correlation = format!("mcp-{request_id}");
        let payload = serde_json::to_vec(&json!({"tool":tool,"arguments":arguments,"correlation_id":correlation})).unwrap();
        let input = InterfaceArtifactFile::write(process_var_dir.join(format!("invocations/{request_id}/artifact")), "tool.call", &payload).unwrap();
        let content = ContentHash::of_bytes(&std::fs::read(input.manifest_path()).unwrap());
        write_message(&mut stdin, &json!({"kind":"invoke","request_id":request_id,"descriptor":descriptor,"envelope":{
            "artifact":{"id":format!("tool.call:{}",content.to_hex()),"content":content},"label":"tool.call",
            "manifest_path":input.manifest_path(),"context":context_carrier(&process_var_dir, request_id, &json!(bindings), &metadata),
            "source_movement_seq":41+request_id,"worker_environment":formation,"worker_environment_kind":&*kind,
        }})).await;
        if matches!(request_id, 6 | 7 | 9 | 10) {
            let emission = read_message(&mut stdout).await;
            assert_eq!(emission["kind"], "system_emission", "{emission}");
            assert_eq!(emission["request_id"], request_id);
            assert_eq!(emission["to_interface"], target);
            assert_eq!(emission["artifact"]["carrier"], "materialized");
            let emitted_path = PathBuf::from(emission["artifact"]["manifest_path"].as_str().unwrap());
            assert!(emitted_path.starts_with(process_var_dir.join(format!("invocations/{request_id}/artifact/emitted"))));
            let emitted = InterfaceArtifactFile::new(emitted_path).unwrap().read_bounded(1024*1024, 1024*1024).unwrap();
            assert_eq!(emitted.manifest.label, if request_id >= 9 { "client.action" } else { "client.surface.document" });
            let emitted_body: Value = serde_json::from_slice(&emitted.payload_bytes).unwrap();
            if request_id >= 9 {
                assert_eq!(emitted_body["schema"], "octopus.client_action_request");
                assert_eq!(emitted_body["tool_call"]["id"], format!("tool.call:{}",content.to_hex()));
                assert_eq!(emitted_body["action"], widget_requests[request_id as usize - 9]);
            } else { assert_eq!(emitted_body, document); }
            write_message(&mut stdin, &json!({"kind":"delivery_verdict","request_id":request_id,
                "event_id":emission["event_id"],"verdict":if request_id != 7 {json!({"admitted":{"seq":991}})} else {refused.clone()},
            })).await;
        }
        let lifecycle = read_message(&mut stdout).await;
        assert_eq!(lifecycle["kind"], "worker_environment_lifecycle", "{lifecycle}");
        assert_eq!(lifecycle["request_id"], request_id);
        assert_eq!(lifecycle["lifecycle"], "active");
        assert_eq!(lifecycle["worker_environment_id"], formation.worker_environment_id.as_str());
        write_message(&mut stdin, &json!({"kind":"worker_environment_lifecycle_recorded","request_id":request_id,
            "event_id":lifecycle["event_id"],"error":null,
        })).await;
        let completed = read_message(&mut stdout).await;
        assert_eq!(completed["kind"], "complete", "{completed}");
        assert_eq!(completed["request_id"], request_id);
        let returned = InterfaceArtifactFile::new(completed["artifact_manifest_paths"][0].as_str().unwrap()).unwrap()
            .read_bounded(1024*1024, 1024*1024).unwrap();
        let result: Value = serde_json::from_slice(&returned.payload_bytes).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["tool"], tool);
        assert_eq!(result["correlation_id"], correlation);
        let rpc = &result["body"];
        assert_eq!(rpc["jsonrpc"], "2.0");
        assert_eq!(rpc["id"], 1);
        if request_id == 5 {
            assert_eq!(rpc["error"]["code"],-32602);
            assert!(rpc["error"]["message"].as_str().unwrap().contains("/document/glyph"));
            assert!(rpc.get("result").is_none());
        } else if request_id == 1 {
            assert!(rpc.get("error").is_none(), "{rpc}");
            let names = rpc["result"]["tools"].as_array().unwrap().iter().map(|entry| entry["name"].clone()).collect::<Vec<_>>();
            let declared = descriptor.handler_config["mcp"]["tools"].as_array().unwrap().iter().map(|entry| entry["name"].clone()).collect::<Vec<_>>();
            assert_eq!(names, declared);
        } else {
            assert!(rpc.get("error").is_none(), "{rpc}");
            assert_eq!(rpc["result"]["isError"], matches!(request_id, 3 | 4 | 7 | 8));
            let body = &rpc["result"]["structuredContent"];
            match request_id {
                2 => { assert_eq!(body["id"], "context:visible"); assert_eq!(body["text"], "bound context"); }
                3 | 4 | 8 => assert!(body["error"].as_str().is_some(), "{body}"),
                6 => assert_eq!(body, &json!({"admitted":{"seq":991}})),
                9 | 10 => {
                    assert_eq!(body["schema"], "octopus.client_action_pending");
                    assert_eq!(body["admission"], json!({"admitted":{"seq":991}}));
                    assert_eq!(body["tool_call"]["id"], format!("tool.call:{}",content.to_hex()));
                },
                7 => assert_eq!(body, &refused),
                _ => unreachable!(),
            }
        }
    }
    assert_eq!(std::fs::read_to_string(&ambient).unwrap(), "ambient data must stay unread");
    write_message(&mut stdin, &json!({"kind":"release_worker_environment","request_id":11,
        "formation":formation,"worker_environment_kind":&*kind,"mode":"live_owner",
    })).await;
    let released = read_message(&mut stdout).await;
    assert_eq!(released["kind"], "worker_environment_released");
    drop(stdin);
    assert!(tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await.unwrap().unwrap().success());
}
