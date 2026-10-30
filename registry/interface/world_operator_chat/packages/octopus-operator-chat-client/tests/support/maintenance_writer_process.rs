use super::*;
use octopus_verifiers::process_authority as native_authority;
use native_authority::ProcessAuthoritySpecExecution;
use octopus_interfaces::{
    GrantProjection, InterfaceDescriptor, InvocationMetadata, RuntimeInvocationMetadata,
};
use octopus_ledger::ContentHash;
use octopus_tool_registry::ToolDescriptor;
use octopus_worker_env::{
    WorkerEnvKindRegistry, WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId,
};
use std::collections::{BTreeMap, BTreeSet};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[tokio::test]
async fn exact_file_authoring_tools_apply_under_sandbox_and_refuse_sibling() {
    let temp = tempfile::Builder::new().prefix("file-authority.[x]+(y)$\"\\-").tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let probe = compile_probe(&root);
    let request = capture_fixture(&root);
    let plan = plan_run_composition(&request).unwrap();
    let record: Value = serde_json::from_str(&plan.run_write.content).unwrap();
    let declaration =
        &record["interface_state"][&plan.provider_interface_id]["maintenance_observer"];
    install(
        plan.descriptor_writes
            .into_iter()
            .chain(plan.run_writes)
            .chain(std::iter::once(plan.run_write)),
    );
    for (record, owner) in [
        (
            &declaration["initial_record"],
            declaration["provider_interface"].as_str().unwrap(),
        ),
        (
            &declaration["projection_run"]["initial_record"],
            declaration["projection_interface"].as_str().unwrap(),
        ),
    ] {
        let binding = &record["interface_state"][owner]["run_authoring_binding"];
        let pair = binding["writer_run"]["initial_record"]["checkpoints"]
            .as_array()
            .unwrap();
        let door = pair[0]["outbound_interface_bindings"][0]["interface"]
            .as_str()
            .unwrap();
        let tool = pair[0]["required_verified_artifacts"][0]["params"]["tools"][0]
            .as_str()
            .unwrap();
        let interface_dir = root.join("registry/interface").join(door);
        let interface: InterfaceDescriptor =
            serde_json::from_value(read(&interface_dir.join("interface.json"))).unwrap();
        let descriptor: ToolDescriptor = serde_json::from_value(read(
            &root.join("registry/tool").join(tool).join("tool.json"),
        ))
        .unwrap();
        assert!(
            descriptor
                .process
                .authority
                .write_argument_paths
                .iter()
                .all(|path| !path.include_parent)
        );
        let roots: BTreeMap<String, PathBuf> = interface
            .interface_driver_config
            .as_ref()
            .unwrap()
            .host_slots
            .iter()
            .map(|(slot, binding)| (slot.clone(), binding.path.clone()))
            .collect();
        assert_eq!(roots.len(), 2);
        let paths = [
            roots["registry_artifact_root"].clone(),
            roots["run_records_root"].clone(),
        ];
        let before = paths
            .each_ref()
            .map(|path| std::fs::read_to_string(path).unwrap());
        let after = before.each_ref().map(|content| format!("{content}\n"));
        let arguments = json!({"ordered":true,"writes":(0..2).map(|i| json!({"path":paths[i],"content":after[i],
            "expected_before_content":before[i],"expected_before_hash":ContentHash::of_bytes(before[i].as_bytes()).to_hex()})).collect::<Vec<_>>()});
        let sibling = paths[0].parent().unwrap().join("unrelated.json");
        std::fs::write(&sibling, "untouched").unwrap();
        let denied = json!({"path":sibling,"content":"must not be written"});
        assert!(
            descriptor
                .process
                .authority
                .preflight_invocation_paths(&roots, &denied)
                .is_err()
        );
        let invocation_paths = descriptor
            .process
            .authority
            .resolve_invocation_paths(&roots, &arguments)
            .unwrap();
        assert_eq!(invocation_paths.writable, BTreeSet::from(paths.clone()));
        let private = tempfile::tempdir_in(&root).unwrap();
        let authority = descriptor
            .process
            .authority
            .resolve_for_invocation(
                &interface_dir,
                &roots,
                &BTreeMap::new(),
                &arguments,
                [interface_dir.clone(), private.path().to_path_buf()],
                [private.path().to_path_buf()],
                std::iter::empty(),
                std::iter::empty(),
            )
            .unwrap();
        let parents = paths
            .each_ref()
            .map(|path| path.parent().unwrap().to_path_buf());
        let aliases: BTreeSet<PathBuf> = parents
            .iter()
            .filter_map(|parent| {
                [("/private/var", "/var"), ("/private/tmp", "/tmp")]
                    .into_iter()
                    .find_map(|(canonical, alias)| {
                        parent
                            .strip_prefix(canonical)
                            .ok()
                            .map(|relative| Path::new(alias).join(relative))
                    })
            })
            .filter(|alias| !parents.contains(alias))
            .collect();
        assert!(!aliases.is_empty(), "no distinct supported parent alias");
        for parent in parents.iter().chain(aliases.iter()) {
            let proof = raw_probe(&authority.authority, &probe, &interface_dir, parent, "unrelated.json".as_ref());
            assert_eq!(proof["directory_open"], 0, "{proof}");
            assert_eq!(proof["directory_fsync"], 0, "{proof}");
            assert_eq!(std::fs::read_to_string(&sibling).unwrap(), "untouched");
            assert!(!parent.join("created.json").exists());
            assert!(!parent.join("created-at.json").exists());
            for operation in [
                "read", "write", "create", "delete", "readat", "writeat", "createat", "deleteat",
            ] {
                assert_eq!(proof[operation], 1, "{operation}: {proof}");
            }
        }
        eprintln!("{owner}: distinct aliases exercised: {aliases:?}");
        let result = invoke(
            &root,
            &interface_dir,
            &interface,
            &descriptor,
            &roots,
            &arguments,
            &arguments,
            record,
        )
        .await;
        assert_eq!(result["ok"], true, "{result}");
        for (i, receipt) in result["body"]["writes"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(receipt["outcome"], "applied", "{result}");
            assert_eq!(
                receipt["observed_hash"],
                ContentHash::of_bytes(after[i].as_bytes()).to_hex()
            );
            assert_eq!(std::fs::read_to_string(&paths[i]).unwrap(), after[i]);
        }
        assert_eq!(result["body"]["writes"].as_array().unwrap().len(), 2);
        use octopus_file_storage::publication::{COORDINATION_DIRECTORY, DigestAlgorithm, PublicationOutcome, PublicationReceipt, WriteGuard};
        let sibling_receipt = WriteGuard::acquire_operation(&sibling, false, "sibling-authority-probe")
            .unwrap().publish("sibling-authority-probe", Some(&ContentHash::of_bytes(b"untouched").to_hex()), b"untouched", DigestAlgorithm::Blake3).unwrap();
        assert_eq!(sibling_receipt.outcome, PublicationOutcome::Applied);
        let metadata_root = sibling.parent().unwrap().join(COORDINATION_DIRECTORY);
        let receipt_path = std::fs::read_dir(&metadata_root).unwrap().map(|entry| entry.unwrap().path())
            .find(|path| std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<PublicationReceipt>(&bytes).ok()).as_ref() == Some(&sibling_receipt)).unwrap();
        let receipt_bytes = std::fs::read(&receipt_path).unwrap();
        for parent in parents.iter().chain(aliases.iter()) {
            let metadata = parent.join(COORDINATION_DIRECTORY);
            let proof = raw_probe(&authority.authority, &probe, &interface_dir, &metadata, receipt_path.file_name().unwrap());
            assert_eq!(proof["directory_open"], 0, "{proof}");
            assert_eq!(proof["directory_fsync"], 0, "{proof}");
            for operation in ["read", "write", "create", "delete", "readat", "writeat", "createat", "deleteat"] {
                assert_eq!(proof[operation], 1, "sibling publication {operation}: {proof}");
            }
            assert!(!metadata.join("created.json").exists());
            assert!(!metadata.join("created-at.json").exists());
        }
        assert_eq!(std::fs::read(&receipt_path).unwrap(), receipt_bytes);
        let refused = invoke(
            &root,
            &interface_dir,
            &interface,
            &descriptor,
            &roots,
            &arguments,
            &denied,
            record,
        )
        .await;
        assert_eq!(refused["ok"], false, "{refused}");
        assert!(
            refused["error"]["message"]
                .as_str()
                .unwrap()
                .contains("outside this room's permission")
        );
        assert_eq!(std::fs::read_to_string(&sibling).unwrap(), "untouched");
        eprintln!(
            "{owner}: sandboxed applied2 with readback/fsync; raw sibling content read/write/create/delete and openat/unlinkat refused on canonical and alias parents"
        );
    }
}

async fn invoke(
    root: &Path,
    interface_dir: &Path,
    interface: &InterfaceDescriptor,
    descriptor: &ToolDescriptor,
    roots: &BTreeMap<String, PathBuf>,
    authority_arguments: &Value,
    arguments: &Value,
    record: &Value,
) -> Value {
    let private = tempfile::tempdir_in(root).unwrap();
    let input = private.path().join("input.json");
    std::fs::write(&input, serde_json::to_vec(&json!({"interface_dir":interface_dir,
        "interface":interface,"descriptor":descriptor,"roots":roots,
        "authority_arguments":authority_arguments,"arguments":arguments,"record":record})).unwrap()).unwrap();
    let executable = std::env::current_exe().unwrap();
    let tool = executable.parent().unwrap().parent().unwrap().join("octopus-file-write-tool");
    let authority = octopus_verifiers::process_authority::ProcessAuthority::resolve(
        roots.values().cloned().chain([source().join("registry"), interface_dir.to_path_buf()]),
        [private.path().to_path_buf()], [executable.clone(), tool], [], [], [],
        false, false, false, true, false, octopus_descriptors::ProcessSandboxMode::Enforced,
    ).unwrap();
    let profiles = tempfile::tempdir().unwrap();
    let prepared = authority.prepare(executable.to_str().unwrap(),
        &["--exact".into(), "maintenance_writer_process::sandboxed_writer_host".into(), "--nocapture".into()],
        interface_dir, None, profiles.path()).unwrap();
    let output = tokio::time::timeout(std::time::Duration::from_secs(30),
        tokio::process::Command::new(prepared.program).args(prepared.args)
            .env_clear().envs(prepared.environment).env("OCTOPUS_WRITER_TEST_INPUT", &input)
            .current_dir(interface_dir).kill_on_drop(true).output()).await.unwrap().unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    read(&private.path().join("result.json"))
}

#[tokio::test]
async fn sandboxed_writer_host() {
    let Some(input) = std::env::var_os("OCTOPUS_WRITER_TEST_INPUT") else { return; };
    let input = PathBuf::from(input);
    let value = read(&input);
    let result = invoke_tool(input.parent().unwrap(),
        Path::new(value["interface_dir"].as_str().unwrap()),
        &serde_json::from_value(value["interface"].clone()).unwrap(),
        &serde_json::from_value(value["descriptor"].clone()).unwrap(),
        &serde_json::from_value(value["roots"].clone()).unwrap(),
        &value["authority_arguments"], &value["arguments"], &value["record"]).await;
    std::fs::write(input.parent().unwrap().join("result.json"), serde_json::to_vec(&result).unwrap()).unwrap();
}

async fn invoke_tool(
    root: &Path,
    interface_dir: &Path,
    interface: &InterfaceDescriptor,
    descriptor: &ToolDescriptor,
    roots: &BTreeMap<String, PathBuf>,
    authority_arguments: &Value,
    arguments: &Value,
    record: &Value,
) -> Value {
    let kind_registry =
        WorkerEnvKindRegistry::load_from_dir(&source().join("registry/worker_environment_kind"))
            .unwrap();
    let kind_id = WorkerEnvironmentKindId::new("system_room");
    let (kind, _) = kind_registry.get(&kind_id).unwrap();
    let hash = |value: Value| {
        ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&value)).to_hex()
    };
    let tools = vec![descriptor.id.as_str().to_owned()];
    let tool_hashes = BTreeMap::from([(tools[0].clone(), hash(json!(descriptor)))]);
    let bins = BTreeMap::from([(tools[0].clone(), descriptor.process.bin.clone())]);
    let kinds = vec![kind_id.clone()];
    let slots = roots.keys().cloned().collect();
    let worker = record["worker"].as_str().unwrap().into();
    let formation = kind_registry
        .form(&WorkerEnvironmentFormationRequest {
            door_additional_root_bindings: &Default::default(),
            door_interface: &interface.id,
            door_descriptor_hash: &hash(json!(interface)),
            door_driver_ref: &interface.interface_driver_ref,
            tool: &tools[0],
            tool_descriptor_hashes: &tool_hashes,
            tool_process_bins: &bins,
            tool_worker_environment_kinds: &descriptor.supported_worker_environment_kinds,
            worker: &worker,
            ee: record["ee"].as_str().unwrap(),
            run: record["id"].as_str().unwrap(),
            granted_worker_environment_kinds: &kinds,
            granted_tools: &tools,
            door_root_bindings: roots,
            door_instance_dir: interface_dir,
            ee_root_slot_allowance: Some(&slots),
        })
        .unwrap();
    let private = tempfile::Builder::new()
        .prefix("writer-process-")
        .tempdir_in(root)
        .unwrap();
    let durable = private.path().join("durable");
    std::fs::create_dir(&durable).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&durable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let payload = serde_json::to_vec(
        &json!({"tool":descriptor.id,"arguments":arguments,"correlation_id":"writer-sandbox"}),
    )
    .unwrap();
    let file = octopus_interfaces::InterfaceArtifactFile::write(
        private.path().join("invocations/1/artifact"),
        "tool.call",
        &payload,
    )
    .unwrap();
    let manifest = std::fs::read(file.manifest_path()).unwrap();
    let content = ContentHash::of_bytes(&manifest);
    let authority = descriptor
        .process
        .authority
        .resolve_for_invocation(
            interface_dir,
            roots,
            &BTreeMap::new(),
            authority_arguments,
            [interface_dir.to_path_buf(), private.path().to_path_buf()],
            [private.path().to_path_buf()],
            std::iter::empty(),
            std::iter::empty(),
        )
        .unwrap();
    let bin = Path::new(env!("CARGO_BIN_EXE_octopus-operator-chat-topology"))
        .with_file_name("octopus-file-write-tool");
    let process = authority
        .authority
        .prepare(bin.to_str().unwrap(), &[], interface_dir, None, root)
        .unwrap();
    assert_eq!(process.program, Path::new("/usr/bin/sandbox-exec"));
    let mut child =
        tokio::process::Command::new(process.program)
            .args(process.args)
            .env_clear()
            .envs(process.environment)
            .current_dir(interface_dir)
            .env("OCTOPUS_INTERFACE_ID", interface.id.as_str())
            .env("OCTOPUS_INTERFACE_DIR", interface_dir)
            .env(
                "OCTOPUS_INTERFACE_DESCRIPTOR_JSON",
                serde_json::to_string(interface).unwrap(),
            )
            .env(
                "OCTOPUS_INTERFACE_HOST_IO_LIMITS",
                serde_json::to_string(
                    &read(&source().join("registry/interface/system_room/life.json"))["processes"]
                        [0]["io_limits"],
                )
                .unwrap(),
            )
            .env("OCTOPUS_VAR_DIR", private.path())
            .env("OCTOPUS_TOOL_PROCESS_DURABLE_VAR_DIR", &durable)
            .env(
                "OCTOPUS_ROOT_SLOTS_JSON",
                serde_json::to_string(roots).unwrap(),
            )
            .env("OCTOPUS_PRIVATE_READS_JSON", "{}")
            .env("TMPDIR", private.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let metadata = InvocationMetadata::new(GrantProjection {
        tools,
        worker_environment_kinds: vec![kind_id.to_string()],
        ee_root_slot_allowance: Some(slots.into_iter().collect()),
        ..Default::default()
    })
    .with_runtime(RuntimeInvocationMetadata {
        run: record["id"].as_str().unwrap().into(),
        ee: record["ee"].as_str().unwrap().into(),
        worker,
        checkpoint: "writer-process-proof".into(),
        interface: interface.id.clone(),
        epoch: 0,
        run_record_hash: None,
    });
    let context = serde_json::to_vec(&json!({"artifact_bindings":[],"metadata":metadata})).unwrap();
    let context_hash = ContentHash::of_bytes(&context);
    let context_path = private.path().join("invocations").canonicalize().unwrap().join("1/artifact")
        .join(format!("invocation-context-{}.json", context_hash.to_hex()));
    std::fs::write(&context_path, &context).unwrap();
    std::fs::set_permissions(&context_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let message = json!({"kind":"invoke","request_id":1,"descriptor":descriptor,"envelope":{"artifact":{
        "id":format!("tool.call:{}",content.to_hex()),"content":content},"label":"tool.call","manifest_path":file.manifest_path(),
        "context":{"path":context_path,"content_hash":context_hash,"byte_len":context.len()},
        "source_movement_seq":1,"worker_environment":formation,"worker_environment_kind":&*kind}});
    stdin
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
    let lifecycle: Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(10), stdout.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        lifecycle["kind"], "worker_environment_lifecycle",
        "{lifecycle}"
    );
    let acknowledgement = json!({"kind":"worker_environment_lifecycle_recorded","request_id":1,"event_id":lifecycle["event_id"],"error":null});
    stdin
        .write_all(format!("{acknowledgement}\n").as_bytes())
        .await
        .unwrap();
    let completed: Value =
        serde_json::from_str(&stdout.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(completed["kind"], "complete", "{completed}");
    let result = octopus_interfaces::InterfaceArtifactFile::new(
        completed["artifact_manifest_paths"][0].as_str().unwrap(),
    )
    .unwrap()
    .read_bounded(1 << 20, 1 << 26)
    .unwrap();
    drop(stdin);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    serde_json::from_slice(&result.payload_bytes).unwrap()
}

fn raw_probe(
    authority: &native_authority::ProcessAuthority,
    binary: &Path,
    cwd: &Path,
    parent: &Path,
    entry: &std::ffi::OsStr,
) -> Value {
    let args = [parent.as_os_str().to_owned(), entry.to_owned()];
    let profiles = tempfile::tempdir().unwrap();
    let process = authority
        .prepare(binary.to_str().unwrap(), &args, cwd, None, profiles.path())
        .unwrap();
    assert_eq!(process.program, Path::new("/usr/bin/sandbox-exec"));
    let output = std::process::Command::new(process.program)
        .args(process.args)
        .env_clear()
        .envs(process.environment)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn compile_probe(root: &Path) -> PathBuf {
    let source = root.join("raw-authority.c");
    let binary = root.join("raw-authority");
    std::fs::write(&source, r#"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
#include <string.h>
static int refused(int fd) { if (fd >= 0) { close(fd); return 0; } return errno == EPERM || errno == EACCES ? 1 : -errno; }
static int content(int fd) { if (fd < 0) return errno == EPERM || errno == EACCES ? 1 : -errno; char byte; int result = read(fd, &byte, 1); int error = errno; close(fd); return result < 0 && (error == EPERM || error == EACCES) ? 1 : 0; }
static int deleted(int result) { return result == 0 ? 0 : errno == EPERM || errno == EACCES ? 1 : -errno; }
int main(int argc, char **argv) {
    char existing[4096], create[4096];
    snprintf(existing, sizeof existing, "%s/%s", argv[1], argv[2]);
    snprintf(create, sizeof create, "%s/created.json", argv[1]);
    int parent = open(argv[1], O_RDONLY | O_DIRECTORY);
    int opening = parent < 0 ? errno : 0;
    int syncing = parent < 0 ? -1 : fsync(parent) == 0 ? 0 : errno;
    int reading = content(open(existing, O_RDONLY));
    int writing = refused(open(existing, O_WRONLY | O_TRUNC));
    int creating = refused(open(create, O_WRONLY | O_CREAT | O_EXCL, 0600));
    int deleting = deleted(unlink(existing));
    int readat = parent < 0 ? -1 : content(openat(parent, argv[2], O_RDONLY));
    int writeat = parent < 0 ? -1 : refused(openat(parent, argv[2], O_WRONLY | O_TRUNC));
    int createat = parent < 0 ? -1 : refused(openat(parent, "created-at.json", O_WRONLY | O_CREAT | O_EXCL, 0600));
    int deleteat = parent < 0 ? -1 : deleted(unlinkat(parent, argv[2], 0));
    printf("{\"directory_open\":%d,\"directory_fsync\":%d,\"read\":%d,\"write\":%d,\"create\":%d,\"delete\":%d,\"readat\":%d,\"writeat\":%d,\"createat\":%d,\"deleteat\":%d}\n", opening, syncing, reading, writing, creating, deleting, readat, writeat, createat, deleteat);
    if (parent >= 0) close(parent);
    return 0;
}
"#).unwrap();
    let output = std::process::Command::new("/usr/bin/clang")
        .args([source.as_os_str(), "-o".as_ref(), binary.as_os_str()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    binary
}
