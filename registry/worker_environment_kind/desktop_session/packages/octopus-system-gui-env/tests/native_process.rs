#![cfg(unix)]
use octopus_system_gui_env::{backend::DesktopExecutor, native_client::NativeExecutor};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};

#[test]
fn cold_application_settings_child_uses_only_the_declared_private_store() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let invoke = |action: serde_json::Value| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_octopus-desktop-host"))
            .args(["--application-settings", root.to_str().unwrap()]).env_clear().current_dir(&root)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(&serde_json::to_vec(&action).unwrap()).unwrap();
        child.wait_with_output().unwrap()
    };
    let result = invoke(json!({"operation":"list"}));
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap(),
        json!({"schema_version":1,"applications":[],"removed":false}));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    assert!(root.join("application-permissions.lock").is_file());
    for action in [json!({"operation":"allow","bundle_id":"example.app"}),json!({"operation":"remove"}),json!({"operation":"list","run":"synthetic"})] {
        assert!(!invoke(action).status.success());
    }
    std::fs::write(root.join("app-permission-always-invalid.json"), b"{}").unwrap();
    assert!(!invoke(json!({"operation":"list"})).status.success());
    assert!(root.join("app-permission-always-invalid.json").is_file());
}

fn admitted(frame: &serde_json::Value, seq: u64) -> String {
    let verdict = json!({"admitted":{"seq":seq}});
    let reply = if frame["kind"] == "retained_arrival" {
        json!({"kind":"retained_delivery","request_id":frame["request_id"],"delivery":{"Ok":{"state":"complete","verdict":verdict}}})
    } else {
        json!({"kind":"delivery_verdict","request_id":frame["request_id"],"verdict":verdict})
    };
    format!("{reply}\n")
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn declared_host_signals_life_readiness_only_after_native_status_admission() {
    for valid_status in [true, false] {
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        let private = root.path().join("system_gui_fixture");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let private = private.canonicalize().unwrap();
        let program = root.path().join("native-fixture");
        let status = if valid_status {
            json!({"permissions":{"screen_capture":false,"accessibility":false},"screens":[]})
        } else {
            json!({"permissions":null,"screens":[]})
        };
        let policy: serde_json::Value = serde_json::from_slice::<serde_json::Value>(include_bytes!("../../../../../interface/system_gui/desktop.json")).unwrap()["host"]["application_policy"].clone();
        let policy_bytes=String::from_utf8(octopus_descriptors::canonical_bytes(&policy)).unwrap();
        assert!(!policy_bytes.contains('\''));
        std::fs::write(&program, format!("#!/bin/bash\n[[ $OCTOPUS_DESKTOP_APPLICATION_POLICY == '{policy_bytes}' ]] || exit 61\nwhile IFS= read -r line; do printf '%s\\n' '{{\"id\":1,\"ok\":true,\"result\":{status}}}'; done\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.path().join("fixture.sb"), r#"(version 1)
(deny default)
(import "dyld-support.sb")
(allow process-exec file-read-data file-map-executable (literal (param "HELPER")) (literal "/bin/bash"))
(allow file-read* file-map-executable (subpath "/System/Library") (subpath "/usr/lib"))
(allow file-read-metadata (apply path-ancestors (list (param "HELPER") "/bin/bash")))
(allow file-read* file-write* (literal "/dev/null") (subpath "/dev/fd"))
"#).unwrap();
        let mut behavior = serde_json::from_slice::<serde_json::Value>(include_bytes!("../../../../../interface/system_gui/desktop.json")).unwrap()["host"]["application_behavior"].clone();
        behavior["instruction_resource"]["path"] = json!("application-instructions.json");
        std::fs::write(root.path().join("application-instructions.json"), include_bytes!("../native/resources/application-instructions.json")).unwrap();
        std::fs::write(root.path().join("desktop.json"), serde_json::to_vec(&json!({
            "schema_version":1,"action_url":"http://127.0.0.1:0/v1/action",
            "viewer_url":"http://127.0.0.1:0/view",
            "host":{"native_program":"native-fixture","state_directory":"desktop-host",
                "native_sandbox_profile":"fixture.sb",
                "application_behavior":behavior,"application_policy":policy,
                "connection_file":"desktop-host/connection.json",
                "device_lock_path":root.path().join("lock")}
        })).unwrap()).unwrap();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_octopus-desktop-host"))
            .env_clear()
            .env("OCTOPUS_INTERFACE_DIR", root.path())
            .env("OCTOPUS_VAR_DIR", &private)
            .env("OCTOPUS_INTERFACE_ID", "system_gui_fixture")
            .env("OCTOPUS_ROOT_SLOTS_JSON", json!({"registry_worker_environment_kind_root":root.path()}).to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), output.read_line(&mut line))
            .await.expect("declared life never became ready or failed").unwrap();
        if valid_status && line.is_empty() {
            drop(child.stdin.take());
            let exited = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await.unwrap().unwrap();
            panic!("valid native fixture exited before readiness: {}: {}", exited.status, String::from_utf8_lossy(&exited.stderr));
        }
        if valid_status {
            assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap(), json!({"kind":"ready"}));
            let manifest = private.join("desktop-host/connection.json");
            let connection: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
            assert_eq!(connection["interface"], "system_gui_fixture");
            assert_eq!(connection["host_generation"].as_str().unwrap().len(), 32);
            assert_eq!(connection["token"].as_str().unwrap().len(), 64);
            assert!(url::Url::parse(connection["action_url"].as_str().unwrap()).unwrap().port().unwrap() > 0);
            assert_eq!(std::fs::metadata(manifest).unwrap().permissions().mode() & 0o777, 0o600);
        } else {
            assert!(line.is_empty(), "invalid native status must not announce readiness: {line}");
        }
        drop(child.stdin.take());
        let exited = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await.unwrap().unwrap();
        assert_eq!(exited.status.success(), valid_status);
        assert!(!private.join("desktop-host/connection.json").exists());
        if !valid_status {
            assert!(String::from_utf8_lossy(&exited.stderr).contains("native permission protocol"));
        }
    }
}

#[tokio::test]
async fn persistent_substrate_reuses_process_bounds_latest_frames_and_reaps_on_eof_or_oversize() {
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("native-fixture");
    let log = root.path().join("lifecycle.txt");
    std::fs::write(&program,format!(r##"#!/usr/bin/env python3
import json,sys,os,fcntl
reference=json.loads(os.environ["OCTOPUS_DESKTOP_DEVICE_REFERENCE"])
fd=reference["fd"]; metadata=os.fstat(fd)
assert (metadata.st_dev,metadata.st_ino,metadata.st_uid)==(reference["device"],reference["inode"],reference["owner"])
assert not fcntl.fcntl(fd,fcntl.F_GETFD)&fcntl.FD_CLOEXEC
fcntl.fcntl(fd,fcntl.F_SETFD,fcntl.FD_CLOEXEC)
assert os.read(fd,1)==b"d"
log={log:?}
def record(s):
 with open(log,'a') as f: f.write(s+'\n')
def emit(s): print(json.dumps(s),flush=True)
record('started')
for line in sys.stdin:
 p=json.loads(line); a=p['action']; t=a['type']
 if t=='oversize':
  print('x'*1000000,flush=True)
  continue
 emit({{'id':p['id'],'ok':True,'result':{{'ready':True}}}})
 if t=='view_start':
  record('view_start')
  for n in range(4): emit({{'event':'frame','screen_id':a['screen_id'],'observation':{{'id':'frame-'+str(n)}}}})
 if t=='view_stop': record('view_stop')
record('eof')
"##,log=log.to_string_lossy())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    use std::os::fd::AsRawFd;
    use std::io::{Seek, SeekFrom, Write};
    let mut device = octopus_system_gui_env::backend::open_device_file(&root.path().join("device.lock")).unwrap();
    device.write_all(b"device").unwrap();
    device.seek(SeekFrom::Start(0)).unwrap();
    let device = std::sync::Arc::new(device);
    let inherited_fd = device.as_raw_fd();
    let original_flags = unsafe { libc::fcntl(inherited_fd, libc::F_GETFD) };
    assert_ne!(original_flags & libc::FD_CLOEXEC, 0);
    let executor = NativeExecutor::new(program.clone(), 4096, Duration::from_secs(2)).with_device_file(device.clone()).unwrap();
    executor.execute(json!({"type":"status"})).await.unwrap();
    assert_eq!(unsafe { libc::lseek(inherited_fd, 0, libc::SEEK_CUR) }, 1, "helper and parent must share the same open file description");
    assert_eq!(unsafe { libc::fcntl(inherited_fd, libc::F_GETFD) }, original_flags, "pre_exec must not clear the parent CLOEXEC flag");
    executor.execute(json!({"type":"status"})).await.unwrap();
    let view = json!({"type":"view","view_id":"window-a","screen_id":"left"});
    executor.execute(view.clone()).await.unwrap();
    executor.execute(json!({"type":"status"})).await.unwrap();
    let latest = executor.execute(view.clone()).await.unwrap();
    assert_eq!(latest["observation"]["id"], "frame-3");
    executor
        .execute(json!({"type":"pause_view","view_id":"window-a"}))
        .await
        .unwrap();
    assert!(executor.execute(view).await.unwrap_err().contains("closed"));
    executor.shutdown().await.unwrap();
    let lifecycle = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        lifecycle.lines().collect::<Vec<_>>(),
        vec!["started", "view_start", "view_stop", "eof"]
    );
    assert_eq!(unsafe { libc::lseek(inherited_fd, 0, libc::SEEK_SET) }, 0);
    let executor = NativeExecutor::new(program, 4096, Duration::from_secs(2)).with_device_file(device).unwrap();
    let start = Instant::now();
    let error = executor
        .execute(json!({"type":"oversize"}))
        .await
        .unwrap_err();
    assert!(error.contains("bounded frame"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(executor
        .execute(json!({"type":"status"}))
        .await
        .unwrap_err()
        .contains("closed"));
    executor.shutdown().await.unwrap();
    eprintln!(
        "oversized native frame rejected and child reaped in {:?}",
        start.elapsed()
    );
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn remote_desktop_tool_crosses_the_actual_door_and_broker_without_native_credentials() {
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
    use octopus_interfaces::{GrantProjection, InterfaceArtifactFile, InvocationMetadata, RuntimeInvocationMetadata};
    use octopus_ledger::{ArtifactRef, ContentHash, Endpoint, Lamport, Movement, MovementKind, Seq, TraceId};
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..").canonicalize().unwrap();
    let door_binary = std::path::Path::new(env!("CARGO_BIN_EXE_octopus-desktop-host")).with_file_name("octopus-desktop-session-tool-door-life");
    assert!(door_binary.is_file(), "build octopus-desktop-session-tool-runtime --bin octopus-desktop-session-tool-door-life before this broker fixture");
    assert_eq!(source.parent().unwrap().join("target/debug/octopus-gui-session-tool").canonicalize().unwrap(), std::path::Path::new(env!("CARGO_BIN_EXE_octopus-gui-session-tool")).canonicalize().unwrap(), "broker fixture must execute this build's exact tool binary in the existing target");
    for (credential, mixed) in [(true, false), (false, false), (true, true)] {
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        let interfaces = root.path().join("registry/interface");
        let private = root.path().join("interfaces/system_gui_remote_fixture");
        let blobs = root.path().join("blobs");
        for directory in [&private, &blobs] { std::fs::create_dir_all(directory).unwrap(); }
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let write = |member: &str, value: &Value| {
            let file = root.path().join(member);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, serde_json::to_vec(value).unwrap()).unwrap();
        };
        let descriptor = json!({"id":"system_gui_remote_fixture","interface_type":"system","role":"system","direction":"bidirectional","source_names":["gui"],"interface_driver_ref":"system_gui"});
        write("registry/interface/system_gui_remote_fixture/interface.json", &descriptor);
        write("registry/interface/system_worker_computer_use/interface.json", &serde_json::from_slice(&std::fs::read(source.join("interface/system_worker_computer_use/interface.json")).unwrap()).unwrap());
        write("registry/worker_environment_kind/desktop_session/worker_environment_kind.json", &serde_json::from_slice(&std::fs::read(source.join("worker_environment_kind/desktop_session/worker_environment_kind.json")).unwrap()).unwrap());
        let mut tool: Value = serde_json::from_slice(&std::fs::read(source.join("tool/gui.status/tool.json")).unwrap()).unwrap();
        tool["interface"] = json!("system_gui_remote_fixture");
        tool["process"]["private_read_subpaths"] = if mixed { json!([{"path":"desktop-host/connection.json"}]) } else { json!([]) };
        tool["process"]["inherit_env"] = json!(["REMOTE_DESKTOP_FIXTURE_KEY"]);
        write("registry/tool/gui.status/tool.json", &tool);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        write("registry/interface/system_gui_remote_fixture/desktop.json", &json!({
            "schema_version":1,"host":null,"action_url":format!("http://{address}/v1/action"),"viewer_url":format!("http://{address}/view"),
            "auth_env_key":"REMOTE_DESKTOP_FIXTURE_KEY","request_timeout_ms":2000
        }));
        let payload = InterfaceArtifactFile::write(root.path().join("input"), "tool.call", br#"{"tool":"gui.status","arguments":{},"correlation_id":"remote-fixture"}"#).unwrap();
        let payload_bytes = payload.read_payload_bytes().unwrap();
        std::fs::write(blobs.join(ContentHash::of_bytes(&payload_bytes).to_hex()), &payload_bytes).unwrap();
        let manifest = std::fs::read(payload.manifest_path()).unwrap();
        let content = ContentHash::of_bytes(&manifest);
        std::fs::write(blobs.join(content.to_hex()), manifest).unwrap();
        let metadata = InvocationMetadata::new(GrantProjection {
            tools: vec!["gui.status".into()], worker_environment_kinds: vec!["desktop_session".into()], ..Default::default()
        }).with_runtime(RuntimeInvocationMetadata {
            run: RunId::new("run_remote_fixture"), ee: EeId::new("ee_remote_fixture"), worker: InterfaceId::new("system_worker_computer_use"),
            checkpoint: CheckpointId::new("remote_fixture"), interface: InterfaceId::new("system_gui_remote_fixture"), epoch:0, run_record_hash:None,
        });
        let movement = Movement {
            seq:Seq(0),lamport:Lamport(1),recorded_at_unix_ms:1,trace:TraceId("remote_fixture".into()),kind:MovementKind::InterfaceOutbound,
            from:Endpoint::Interface(InterfaceId::new("world_remote_fixture")),to:Endpoint::Interface(InterfaceId::new("system_gui_remote_fixture")),
            artifact:Some(ArtifactRef {id:ArtifactId::new("remote_fixture"),content}),invocation_metadata:Some(metadata.into()),
        };
        std::fs::write(root.path().join("movements.jsonl"), format!("{}\n", serde_json::to_string(&movement).unwrap())).unwrap();
        let mut command = tokio::process::Command::new(&door_binary);
        command.env_clear().env("OCTOPUS_INTERFACE_ID", "system_gui_remote_fixture")
            .env("OCTOPUS_INTERFACE_DIR", interfaces.join("system_gui_remote_fixture")).env("OCTOPUS_VAR_DIR", &private)
            .env("OCTOPUS_INTERFACE_DESCRIPTOR_JSON", descriptor.to_string())
            .env("OCTOPUS_INTERFACE_HOST_IO_LIMITS", json!({"max_control_frame_bytes":1048576,"max_stderr_bytes":262144,"max_manifest_bytes":1048576,"max_payload_bytes":8388608}).to_string())
            .env("OCTOPUS_MOVEMENTS_JSONL", root.path().join("movements.jsonl")).env("OCTOPUS_BLOBS_DIR", &blobs)
            .env("OCTOPUS_ROOT_SLOTS_JSON", json!({
                "interface_life_install_root": source.parent().unwrap(), "registry_interface_root": interfaces,
                "registry_tool_root":root.path().join("registry/tool"),"registry_worker_environment_kind_root":root.path().join("registry/worker_environment_kind"),
                "activation_secrets_home":private
            }).to_string())
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        if credential { command.env("REMOTE_DESKTOP_FIXTURE_KEY", "fixture-only-explicit-remote-credential"); }
        let mut child = command.spawn().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        let receive = async {
            loop {
                line.clear();
                assert!(output.read_line(&mut line).await.unwrap() > 0, "desktop door exited before a result");
                let frame: Value = serde_json::from_str(&line).unwrap();
                if frame["kind"] == "ready" { continue; }
                assert_eq!(frame["kind"], if mixed { "arrival" } else { "retained_arrival" }, "{frame}");
                let result = InterfaceArtifactFile::new(frame["artifact_manifest_path"].as_str().unwrap()).unwrap();
                let result: Value = serde_json::from_slice(&result.read_payload_bytes().unwrap()).unwrap();
                assert_eq!(result["ok"], credential && !mixed, "broker returned before its expected HTTP exchange: {result}");
                child.stdin.as_mut().unwrap().write_all(admitted(&frame, 1).as_bytes()).await.unwrap();
                return result;
            }
        };
        let serve = async {
            if !credential || mixed { return; }
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut length = None;
            let mut authenticated = false;
            for _ in 0..64 {
                let mut header = String::new();
                stream.read_line(&mut header).await.unwrap();
                assert!(header.len() < 4096);
                if header == "\r\n" { break; }
                if let Some((key, value)) = header.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") { length = Some(value.trim().parse::<usize>().unwrap()); }
                    if key.eq_ignore_ascii_case("authorization") { authenticated = value.trim() == "Bearer fixture-only-explicit-remote-credential"; }
                }
            }
            assert!(authenticated);
            let length = length.unwrap(); assert!(length < 65536);
            let mut body = vec![0; length]; stream.read_exact(&mut body).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["action"]["type"], "status");
            let reply = json!({"schema_version":2,"accepted":true,"worker_environment_id":body["worker_environment_id"],"request_id":body["request_id"],"operation":"status","error":null,
                "evidence":{"status":"ready","permissions":{"screen_capture":true,"accessibility":true},"control":{"mode":"idle","owner":null,"generation":1},"screens":[],"observation":null,"effect":null}}).to_string();
            stream.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(15), async { tokio::join!(receive, serve) }).await.expect("remote door waited for a native host or failed its finite broker contract");
        assert_eq!(result["ok"], credential && !mixed, "{result}");
        assert_eq!(result["tool"], "gui.status");
        if credential && !mixed { assert_eq!(result["body"]["evidence"]["status"], "ready"); }
        else { assert!(result.to_string().contains(if mixed { "desktop-host/connection.json" } else { "REMOTE_DESKTOP_FIXTURE_KEY" }), "{result}"); }
        assert!(!private.join("desktop-host").exists());
        if !credential || mixed { assert!(tokio::time::timeout(Duration::from_millis(50), listener.accept()).await.is_err()); }
        drop(child.stdin.take());
        tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn actual_gui_decoder_recovers_past_retired_host_without_reading_connection_or_publishing_decision() {
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
    use octopus_interfaces::{GrantProjection, InterfaceArtifactFile, InvocationMetadata, RuntimeInvocationMetadata};
    use octopus_ledger::{ArtifactRef, ContentHash, Endpoint, Lamport, Movement, MovementKind, Seq, TraceId};
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_octopus-desktop-host")).with_file_name("octopus-desktop-session-tool-door-life");
    assert!(binary.is_file(), "build the current desktop door before its actual callback recovery fixture");
    let install = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..").canonicalize().unwrap();
    for scenario in ["clean", "duplicate_old_approval", "corrupt_history", "unselected_proof"] {
        let duplicate = scenario == "duplicate_old_approval";
        let corrupt = scenario == "corrupt_history";
        let unselected_proof = scenario == "unselected_proof";
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        let door = "system_gui__recovery_fixture";
        let provider = "world_provider__recovery_fixture";
        let operator = "world_operator__recovery_fixture";
        let private = root.path().join(door);
        let host = private.join("desktop-host");
        let blobs = root.path().join("blobs");
        for path in [&private, &host, &blobs] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut request = json!({"schema_version":1,"approval_scope":"formed_app_target",
            "scope":{"worker_environment_id":"old-room","formation_pin":"old-pin","host_generation":"11111111111111111111111111111111"},
            "application":{"bundle_id":"example.app","bundle_path":"/Applications/Example.app","display_name":"Example","instance":null},
            "original":{"run":"run_gui_recovery","ee":"ee_gui_recovery","worker":"system_worker_computer_use","door_interface":door,
                "tool":"gui.app_state__recovery_fixture","correlation_id":"old-call","arguments":{"app":{"kind":"bundle_id","value":"example.app"}},
                "source_movement_seq":1,"source_artifact":{"id":"old-source","content":ContentHash::of_bytes(b"prior-application-call")}}});
        request["native_policy"] = json!({"decision":"allowed","allow_persistent_approval":false,"risk":"low","warning_subtitle":null,
            "target":{"bundle_id":"example.app","bundle_path":"/Applications/Example.app","display_name":"Example"}});
        let digest = format!("{:x}", Sha256::digest(octopus_descriptors::canonical_bytes(&request)));
        request["request_id"] = json!(format!("sha256:{digest}")); request["call_id"] = json!("old-call");
        request["resume"] = json!({"tool":"gui.app_resume__recovery_fixture","correlation_id":format!("app-resume:{digest}")});
        let pending = host.join(format!("app-{digest}.request.json"));
        std::fs::write(&pending, serde_json::to_vec(&json!({"request":request,"native_target_handle":"retired-target",
            "operator_interface":operator,"provider_interface":provider,
            "native_policy":{"decision":"allowed","allow_persistent_approval":false,"risk":"low","warning_subtitle":null,
                "target":{"bundle_id":"example.app","bundle_path":"/Applications/Example.app","display_name":"Example"}},
            "native_policy_digest":"0".repeat(64)})).unwrap()).unwrap();
        std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o600)).unwrap();
        if corrupt {
            let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&pending).unwrap()).unwrap();
            value["request"]["original"]["arguments"]["app"]["value"] = json!("replaced.app");
            std::fs::write(&pending, serde_json::to_vec(&value).unwrap()).unwrap();
        }
        let before = std::fs::read(&pending).unwrap();
        let store = |name: &str, label: &str, body: serde_json::Value| {
            let artifact = InterfaceArtifactFile::write(root.path().join(name), label, &serde_json::to_vec(&body).unwrap()).unwrap();
            let payload = artifact.read_payload_bytes().unwrap();
            std::fs::write(blobs.join(ContentHash::of_bytes(&payload).to_hex()), payload).unwrap();
            let manifest = std::fs::read(artifact.manifest_path()).unwrap();
            let content = ContentHash::of_bytes(&manifest);
            std::fs::write(blobs.join(content.to_hex()), manifest).unwrap();
            ArtifactRef { id: ArtifactId::new(format!("{label}:{}", content.to_hex())), content }
        };
        let metadata = |inbound: &str, checkpoint: &str| {
            let mut metadata = InvocationMetadata::new(GrantProjection { tools: vec!["gui.status__recovery_fixture".into(), "gui.app_resume__recovery_fixture".into()], ..Default::default() })
                .with_runtime(RuntimeInvocationMetadata { run: RunId::new("run_gui_recovery"), ee: EeId::new("ee_gui_recovery"),
                    worker: InterfaceId::new("system_worker_computer_use"), checkpoint: CheckpointId::new(checkpoint), interface: InterfaceId::new(door), epoch:0, run_record_hash:None });
            metadata.checkpoint_binding_policy = Some(serde_json::from_value(json!({"inbound":{"interface":inbound,"config":{"artifact_policy":{"mode":"all"}}},"outbound":[]})).unwrap());
            metadata
        };
        let mut answer=json!({"operator_approval_response":{"request_id":request["request_id"],"call_id":"old-call","decision":"accept"}});
        if unselected_proof {
            let evidence=json!({"artifact":request["original"]["source_artifact"],"movement_seq":1});
            answer["operator_approval_response"]["policy_authorization"]=json!({"authority":request["original"]["source_artifact"],
                "command":evidence,"writer_call":evidence,"writer_result":evidence,"committed_run_hash":"0".repeat(64),"record_id":"unproven"});
        }
        let old = store("old-approval", "operator.approval.response", answer);
        let current = store("new-call", "tool.call", json!({"tool":"gui.status__recovery_fixture","arguments":{},"correlation_id":"new-call"}));
        let result = store("new-result", "gui.status__recovery_fixture.result", json!({"label":"gui.status__recovery_fixture.result","tool":"gui.status__recovery_fixture","correlation_id":"new-call","ok":true,"body":{"done":true}}));
        let old_result = store("old-result", "gui.app_resume__recovery_fixture.result", json!({"label":"gui.app_resume__recovery_fixture.result",
            "tool":"gui.app_resume__recovery_fixture","correlation_id":request["resume"]["correlation_id"],"ok":true,"body":{"done":true}}));
        let source = |seq, artifact, inbound: &str, checkpoint: &str| Movement { seq:Seq(seq), lamport:Lamport(seq+1), recorded_at_unix_ms:seq+1,
            trace:TraceId("gui-recovery".into()), kind:MovementKind::InterfaceOutbound, from:Endpoint::Interface(InterfaceId::new(inbound)),
            to:Endpoint::Interface(InterfaceId::new(door)), artifact:Some(artifact), invocation_metadata:Some(metadata(inbound, checkpoint).into()) };
        let mut movements = vec![source(0,old.clone(),operator,"old"),source(1,current,provider,"new")];
        if duplicate { movements.push(source(2,old,operator,"duplicate-old-approval")); }
        movements.push(Movement {seq:Seq(movements.len() as u64),lamport:Lamport(movements.len() as u64+1),recorded_at_unix_ms:4,
            trace:TraceId("gui-recovery".into()),kind:MovementKind::InterfaceArrival,from:Endpoint::Interface(InterfaceId::new(door)),
            to:Endpoint::Interface(InterfaceId::new("system_worker_computer_use")),artifact:Some(old_result),invocation_metadata:Some(metadata(operator,"old-result").into())});
        movements.push(Movement {seq:Seq(movements.len() as u64),lamport:Lamport(movements.len() as u64+1),recorded_at_unix_ms:4,
            trace:TraceId("gui-recovery".into()),kind:MovementKind::InterfaceArrival,from:Endpoint::Interface(InterfaceId::new(door)),
            to:Endpoint::Interface(InterfaceId::new("system_worker_computer_use")),artifact:Some(result),invocation_metadata:Some(metadata(provider,"result").into())});
        let ledger = root.path().join("movements.jsonl");
        let mut file = std::fs::File::create(&ledger).unwrap();
        for movement in &movements { writeln!(file,"{}",serde_json::to_string(movement).unwrap()).unwrap(); }
        let mut journal = std::fs::File::create(private.join("tool-door-effect-frontier.jsonl")).unwrap();
        for (index, states) in [(0,["prepared",if duplicate {"in_flight"} else {"completed"}]),(1,["prepared",if duplicate {"completed"} else {"in_flight"}])] {
            let hash = blake3::hash(&serde_json::to_vec(&movements[index]).unwrap()).to_hex().to_string();
            for state in states { writeln!(journal,"{}",json!({"schema_version":2,"seq":index,"movement_digest":hash,"state":state})).unwrap(); }
        }
        std::fs::set_permissions(private.join("tool-door-effect-frontier.jsonl"),std::fs::Permissions::from_mode(0o600)).unwrap();
        let descriptor = json!({"id":door,"interface_type":"system","role":"system","direction":"bidirectional","source_names":["gui"],"interface_driver_ref":"system_gui"});
        std::fs::write(root.path().join("interface.json"),descriptor.to_string()).unwrap();
        let mut child = tokio::process::Command::new(&binary).env_clear()
            .env("OCTOPUS_INTERFACE_ID",door).env("OCTOPUS_INTERFACE_DIR",root.path()).env("OCTOPUS_VAR_DIR",&private)
            .env("OCTOPUS_INTERFACE_DESCRIPTOR_JSON",descriptor.to_string())
            .env("OCTOPUS_INTERFACE_HOST_IO_LIMITS",json!({"max_control_frame_bytes":1048576,"max_stderr_bytes":262144,"max_manifest_bytes":1048576,"max_payload_bytes":8388608}).to_string())
            .env("OCTOPUS_ROOT_SLOTS_JSON",json!({"interface_life_install_root":install}).to_string())
            .env("OCTOPUS_MOVEMENTS_JSONL",&ledger).env("OCTOPUS_BLOBS_DIR",&blobs)
            .env("OCTOPUS_LEDGER_INDEX_SQLITE",root.path().join("ledger-index.sqlite"))
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5),output.read_line(&mut line)).await.unwrap().unwrap();
        if line.is_empty() && !duplicate && !corrupt && !unselected_proof {
            drop(child.stdin.take());
            let exited = tokio::time::timeout(Duration::from_secs(5),child.wait_with_output()).await.unwrap().unwrap();
            panic!("GUI recovery fixture ended before readiness: {}: {}",exited.status,String::from_utf8_lossy(&exited.stderr));
        }
        if !line.is_empty() { assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap()["kind"],"ready"); }
        if !duplicate && !corrupt && !unselected_proof {
            tokio::time::timeout(Duration::from_secs(5),async {
                loop {
                    if child.try_wait().unwrap().is_some() { panic!("actual GUI recovery exited before committing its exact result"); }
                    if let Ok(bytes) = std::fs::read(private.join("ledger-cursor.tool-door.json")) {
                        let cursor: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                        if cursor["seq"].as_u64().is_some_and(|seq| seq >= 2) { break; }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("exact recovery did not finish");
            drop(child.stdin.take());
        }
        let input_owner = child.stdin.take();
        let exited = tokio::time::timeout(Duration::from_secs(5),child.wait_with_output()).await.unwrap().unwrap();
        drop(input_owner);
        assert!(!exited.status.success());
        if !duplicate && !corrupt && !unselected_proof {
            assert_eq!(String::from_utf8_lossy(&exited.stderr).trim(), "octopus-desktop-session-tool-door-life failed: spawned life input closed");
            assert!(!private.join("failure-artifacts").exists());
        }
        if duplicate { assert!(String::from_utf8_lossy(&exited.stderr).contains("duplicate source call identity"), "{}", String::from_utf8_lossy(&exited.stderr)); }
        if corrupt { assert!(String::from_utf8_lossy(&exited.stderr).contains("commit the exact request"), "{}", String::from_utf8_lossy(&exited.stderr)); }
        if unselected_proof { assert!(String::from_utf8_lossy(&exited.stderr).contains("unknown field `policy_authorization`"), "{}", String::from_utf8_lossy(&exited.stderr)); }
        assert_eq!(std::fs::read(&pending).unwrap(),before);
        assert!(!host.join("connection.json").exists());
        assert!(!host.join(format!("app-{digest}.decision.json")).exists());
    }
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn application_resume_crosses_actual_gui_door_and_broker_with_original_operator_manifest() {
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
    use octopus_interfaces::{GrantProjection, InterfaceArtifactFile, InvocationMetadata, RuntimeInvocationMetadata};
    use octopus_ledger::{ArtifactRef, ContentHash, Endpoint, Lamport, Movement, MovementKind, Seq, TraceId};
    use serde_json::Value;
    use sha2::{Digest, Sha256};
    use std::io::Write;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..").canonicalize().unwrap();
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_octopus-desktop-host")).with_file_name("octopus-desktop-session-tool-door-life");
    assert!(binary.is_file(), "build this source's desktop tool door before its broker fixture");
    assert_eq!(source.parent().unwrap().join("target/debug/octopus-gui-session-tool").canonicalize().unwrap(), std::path::Path::new(env!("CARGO_BIN_EXE_octopus-gui-session-tool")).canonicalize().unwrap());
    let root = tempfile::tempdir_in("/private/tmp").unwrap();
    let door = "system_gui__approval_fixture"; let provider = "world_provider__approval_fixture"; let operator = "world_operator__approval_fixture";
    let tools = ["gui.app_state__approval_fixture", "gui.app_resume__approval_fixture"];
    let interfaces = root.path().join("registry/interface"); let private = root.path().join("interfaces").join(door);
    let host = private.join("desktop-host"); let blobs = root.path().join("blobs");
    for path in [&private,&host,&blobs] { std::fs::create_dir_all(path).unwrap(); std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o700)).unwrap(); }
    let write = |path: &std::path::Path, value: &Value| { std::fs::create_dir_all(path.parent().unwrap()).unwrap(); std::fs::write(path,serde_json::to_vec(value).unwrap()).unwrap(); };
    let descriptor = json!({"id":door,"interface_type":"system","role":"system","direction":"bidirectional","source_names":["gui"],"interface_driver_ref":"system_gui"});
    write(&interfaces.join(door).join("interface.json"),&descriptor);
    write(&interfaces.join("system_worker_computer_use/interface.json"),&serde_json::from_slice(&std::fs::read(source.join("interface/system_worker_computer_use/interface.json")).unwrap()).unwrap());
    write(&root.path().join("registry/worker_environment_kind/desktop_session/worker_environment_kind.json"),&serde_json::from_slice(&std::fs::read(source.join("worker_environment_kind/desktop_session/worker_environment_kind.json")).unwrap()).unwrap());
    for (original, actual) in ["gui.app_state","gui.app_resume"].into_iter().zip(tools) {
        let mut tool: Value = serde_json::from_slice(&std::fs::read(source.join("tool").join(original).join("tool.json")).unwrap()).unwrap();
        tool["id"] = json!(actual); tool["interface"] = json!(door);
        write(&root.path().join("registry/tool").join(actual).join("tool.json"),&tool);
    }
    let mut config: Value = serde_json::from_slice(&std::fs::read(source.join("interface/system_gui/desktop.json")).unwrap()).unwrap();
    config["app_approval"] = json!({"operator_interface":operator,"resume_tool":tools[1]});
    config["request_timeout_ms"] = json!(5000);
    write(&interfaces.join(door).join("desktop.json"),&config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let address = listener.local_addr().unwrap();
    let generation = "11111111111111111111111111111111"; let token = "2222222222222222222222222222222222222222222222222222222222222222";
    write(&host.join("connection.json"),&json!({"schema_version":1,"interface":door,"host_generation":generation,"token":token,
        "action_url":format!("http://{address}/v1/action"),"viewer_url":format!("http://{address}/view")}));
    std::fs::set_permissions(host.join("connection.json"),std::fs::Permissions::from_mode(0o600)).unwrap();
    let store = |name: &str,label: &str,body: Value| {
        let artifact = InterfaceArtifactFile::write(root.path().join(name),label,&serde_json::to_vec(&body).unwrap()).unwrap();
        let payload = artifact.read_payload_bytes().unwrap(); std::fs::write(blobs.join(ContentHash::of_bytes(&payload).to_hex()),payload).unwrap();
        let bytes = std::fs::read(artifact.manifest_path()).unwrap(); let content = ContentHash::of_bytes(&bytes);
        std::fs::write(blobs.join(content.to_hex()),bytes).unwrap(); ArtifactRef {id:ArtifactId::new(format!("{label}:{}",content.to_hex())),content}
    };
    let mut metadata = InvocationMetadata::new(GrantProjection {tools:tools.iter().map(|tool|tool.to_string()).collect(),
        interfaces:vec![door.into(),operator.into(),provider.into()], worker_environment_kinds:vec!["desktop_session".into()],
        mark_outcomes:vec!["operator.input.provided".into()],..Default::default()}).with_runtime(RuntimeInvocationMetadata {
        run:RunId::new("run_approval_fixture"),ee:EeId::new("ee_approval_fixture"),worker:InterfaceId::new("system_worker_computer_use"),
        checkpoint:CheckpointId::new("initial"),interface:InterfaceId::new(door),epoch:0,run_record_hash:None});
    metadata.checkpoint_binding_policy = Some(serde_json::from_value(json!({"inbound":{"interface":provider,"config":{"artifact_policy":{"mode":"all"}}},"outbound":[{"interface":door,"config":{"artifact_policy":{"mode":"all"}}}]})).unwrap());
    let call = store("initial-call","tool.call",json!({"tool":tools[0],"correlation_id":"original-call","arguments":{"app":{"kind":"bundle_id","value":"example.app"}}}));
    let movement = |seq,artifact,from: &str,metadata: InvocationMetadata| Movement {seq:Seq(seq),lamport:Lamport(seq+1),recorded_at_unix_ms:seq+1,
        trace:TraceId("approval-fixture".into()),kind:MovementKind::InterfaceOutbound,from:Endpoint::Interface(InterfaceId::new(from)),to:Endpoint::Interface(InterfaceId::new(door)),
        artifact:Some(artifact),invocation_metadata:Some(metadata.into())};
    let first = movement(1,call.clone(),provider,metadata.clone());
    let mut unrelated = first.clone(); unrelated.seq=Seq(0); unrelated.to=Endpoint::Interface(InterfaceId::new("system_unrelated"));
    let ledger = root.path().join("movements.jsonl"); write_ledger_fixture(&ledger,&[unrelated,first]);
    let mut child = tokio::process::Command::new(binary).env_clear().env("OCTOPUS_INTERFACE_ID",door).env("OCTOPUS_INTERFACE_DIR",interfaces.join(door))
        .env("OCTOPUS_VAR_DIR",&private).env("OCTOPUS_INTERFACE_DESCRIPTOR_JSON",descriptor.to_string())
        .env("OCTOPUS_INTERFACE_HOST_IO_LIMITS",json!({"max_control_frame_bytes":1048576,"max_stderr_bytes":262144,"max_manifest_bytes":1048576,"max_payload_bytes":8388608}).to_string())
        .env("OCTOPUS_ROOT_SLOTS_JSON",json!({"interface_life_install_root":source.parent().unwrap(),"registry_interface_root":interfaces,
            "registry_tool_root":root.path().join("registry/tool"),"registry_worker_environment_kind_root":root.path().join("registry/worker_environment_kind"),"activation_secrets_home":private}).to_string())
        .env("OCTOPUS_MOVEMENTS_JSONL",&ledger).env("OCTOPUS_BLOBS_DIR",&blobs)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut operator_request = Value::Null;
    let mut response_artifact: Option<ArtifactRef> = None;
    for phase in 0..2 {
        let receive = async {
            loop {
                let mut line = String::new();
                if output.read_line(&mut line).await.map_err(|e|e.to_string())? == 0 { return Err("GUI door EOF before result".to_string()); }
                let frame: Value = serde_json::from_str(&line).map_err(|e|e.to_string())?;
                if frame["kind"] == "ready" { continue; }
                if frame["kind"] != "retained_arrival" { return Err(format!("unexpected GUI frame {frame}")); }
                let artifact = InterfaceArtifactFile::new(frame["artifact_manifest_path"].as_str().ok_or("arrival has no manifest")?).map_err(|e|e.to_string())?;
                let body: Value = serde_json::from_slice(&artifact.read_payload_bytes().map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
                if body["ok"] != true { return Err(format!("GUI tool failed before expected host response: {body}")); }
                child.stdin.as_mut().unwrap().write_all(admitted(&frame, 20+phase).as_bytes()).await.map_err(|e|e.to_string())?;
                return Ok(body);
            }
        };
        let serve = async {
            let (stream,_) = listener.accept().await.map_err(|e|e.to_string())?;
            let mut stream = BufReader::new(stream); let mut length=None; let mut auth=false;
            for _ in 0..64 {
                let mut line=String::new(); stream.read_line(&mut line).await.map_err(|e|e.to_string())?;
                if line=="\r\n" {break;}
                if let Some((key,value))=line.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") { length=Some(value.trim().parse::<usize>().map_err(|e|e.to_string())?); }
                    if key.eq_ignore_ascii_case("authorization") { auth=value.trim()==format!("Bearer {token}"); }
                }
            }
            if !auth {return Err("native fixture missing private bearer".to_string());}
            let length=length.ok_or("no body length")?; if length>65536 {return Err("oversized fixture request".into());}
            let mut bytes=vec![0;length];stream.read_exact(&mut bytes).await.map_err(|e|e.to_string())?;
            let body:Value=serde_json::from_slice(&bytes).map_err(|e|e.to_string())?;
            let context=&body["action"]["invocation"];
            let packet=if phase==0 {
                if context["call"]["tool"]!=tools[0] || context["call"]["source_artifact"]!=json!(call) {return Err("initial source coordinates changed".into());}
                let mut request=json!({"schema_version":1,"original":context["call"],"scope":context["scope"],"approval_scope":"formed_app_target",
                    "application":{"bundle_id":"example.app","bundle_path":"/Applications/Example.app","display_name":"Example","instance":null}});
                request["native_policy"]=json!({"decision":"allowed","allow_persistent_approval":true,"risk":"low","warning_subtitle":null,
                    "target":{"bundle_id":"example.app","bundle_path":"/Applications/Example.app","display_name":"Example"}});
                request["permission_offer"]=json!({"client_interface":operator,"offered_choices":[
                    {"decision":"accept","persistence":"once"},{"decision":"decline","persistence":"once"}]});
                let hash=format!("{:x}",Sha256::digest(octopus_descriptors::canonical_bytes(&request)));
                request["request_id"]=json!(format!("sha256:{hash}"));request["call_id"]=json!("original-call");request["resume"]=json!({"tool":tools[1],"correlation_id":format!("app-resume:{hash}")});
                let policy_digest=format!("{:x}",Sha256::digest(octopus_descriptors::canonical_bytes(&config["host"]["application_policy"])));
                let path=host.join(format!("app-{hash}.request.json"));write(&path,&json!({"request":request,"native_target_handle":"fixture-only-target","operator_interface":operator,"provider_interface":provider,
                    "native_policy":{"decision":"allowed","allow_persistent_approval":true,"risk":"low","warning_subtitle":null,
                        "target":{"bundle_id":"example.app","bundle_path":"/Applications/Example.app","display_name":"Example"}},
                    "native_policy_digest":policy_digest}));
                std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o600)).map_err(|e|e.to_string())?;
                json!({"type":"awaiting_approval","schema_version":3,"operator_request":request,"effect_performed":false})
            } else {
                if body["action"]["type"]!="app_resume" || context["call"]["tool"]!=tools[1]
                    || context["call"]["source_artifact"]!=json!(response_artifact.as_ref().unwrap()) {return Err("resume executable lost original operator artifact".into());}
                json!({"type":"resumed","schema_version":3,"operator_request":operator_request,"original_result":{
                    "label":format!("{}.result",tools[0]),"tool":tools[0],"correlation_id":"original-call","ok":false,
                    "error":{"code":"application_approval_declined","message":"No native effect performed"}}})
            };
            let reply=json!({"schema_version":2,"accepted":true,"worker_environment_id":body["worker_environment_id"],"request_id":body["request_id"],"operation":body["action"]["type"],"error":null,
                "evidence":{"status":"ready","permissions":{"screen_capture":true,"accessibility":true},"control":{"mode":"idle","owner":null,"generation":1},"screens":[],"observation":null,"effect":null},"application_result":packet}).to_string();
            stream.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await.map_err(|e|e.to_string())?;
            Ok::<(),String>(())
        };
        let (result,())=tokio::time::timeout(Duration::from_secs(10),async {tokio::try_join!(receive,serve)}).await.expect("GUI broker phase exceeded finite fixture deadline").unwrap();
        assert_eq!(result["tool"],tools[phase as usize]);
        if phase==0 {
            operator_request=result["body"]["operator_request"].clone();assert_eq!(result["body"]["effect_performed"],false);
            let response=store("operator-response","operator.approval.response",json!({"operator_approval_response":{"request_id":operator_request["request_id"],"call_id":"original-call","decision":"decline","selected_persistence":"once"}}));
            metadata.runtime.as_mut().unwrap().checkpoint=CheckpointId::new("approved-response");
            metadata.outbound_interface_bindings=vec![InterfaceId::new(provider),InterfaceId::new(door)];
            metadata.checkpoint_binding_policy=Some(serde_json::from_value(json!({"inbound":{"interface":operator,"config":{"artifact_policy":{"mode":"all"}}},"outbound":[{"interface":provider,"config":{"artifact_policy":{"mode":"all"}}},{"interface":door,"config":{"artifact_policy":{"mode":"all"}}}]})).unwrap());
            metadata.checkpoint_admission=Some(serde_json::from_value(json!({"artifact_label":"operator.approval.response","required_artifact_labels":["operator.approval.response"],"complete":true})).unwrap());
            metadata.required_verified_artifacts=vec![serde_json::from_value(json!({"mark":"operator.input.provided","artifact_label":"operator.approval.response","verifier":"operator_input","params":{"request_id":operator_request["request_id"],"call_id":"original-call","offered_choices":operator_request["permission_offer"]["offered_choices"]}})).unwrap()];
            writeln!(std::fs::OpenOptions::new().append(true).open(&ledger).unwrap(),"{}",serde_json::to_string(&movement(2,response.clone(),operator,metadata.clone())).unwrap()).unwrap();
            response_artifact=Some(response);
        } else {
            assert_eq!(result["correlation_id"],operator_request["resume"]["correlation_id"]);
            assert_eq!(result["body"]["original_result"]["ok"],false);
            assert_eq!(result["body"]["original_result"]["correlation_id"],"original-call");
            let hash=operator_request["request_id"].as_str().unwrap().strip_prefix("sha256:").unwrap();
            let decision:Value=serde_json::from_slice(&std::fs::read(host.join(format!("app-{hash}.decision.json"))).unwrap()).unwrap();
            assert_eq!(decision["operator_artifact"],json!(response_artifact.as_ref().unwrap()));
            assert_eq!(decision["selected_persistence"],"once");
        }
    }
    let forged = store("model-resume","tool.call",json!({"tool":tools[1],"correlation_id":"model-forgery",
        "arguments":{"request_id":operator_request["request_id"]}}));
    metadata.checkpoint_binding_policy = Some(serde_json::from_value(json!({"inbound":{"interface":provider,"config":{"artifact_policy":{"mode":"all"}}},"outbound":[{"interface":door,"config":{"artifact_policy":{"mode":"all"}}}]})).unwrap());
    metadata.required_verified_artifacts.clear(); metadata.checkpoint_admission = None;
    writeln!(std::fs::OpenOptions::new().append(true).open(&ledger).unwrap(),"{}",serde_json::to_string(&movement(3,forged,provider,metadata)).unwrap()).unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5),output.read_line(&mut line)).await.unwrap().unwrap();
    let frame: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(frame["kind"], "arrival");
    let artifact = InterfaceArtifactFile::new(frame["artifact_manifest_path"].as_str().unwrap()).unwrap();
    assert_eq!(artifact.read_manifest().unwrap().label, format!("{door}.failure"));
    let failure: Value = serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(failure["source_movement_seq"], 3);
    assert_eq!(failure["failure_class"], "invalid_tool_artifact");
    assert_eq!(failure["details"]["error"], "application resume requires the original operator.approval.response artifact at the GUI door");
    assert!(tokio::time::timeout(Duration::from_millis(50),listener.accept()).await.is_err());
    child.stdin.as_mut().unwrap().write_all(admitted(&frame, 22).as_bytes()).await.unwrap();
    drop(child.stdin.take());
    let exited=tokio::time::timeout(Duration::from_secs(5),child.wait_with_output()).await.unwrap().unwrap();
    assert!(!exited.status.success());
    assert_eq!(String::from_utf8_lossy(&exited.stderr).trim(), "octopus-desktop-session-tool-door-life failed: spawned life input closed");

    fn write_ledger_fixture(path:&std::path::Path,movements:&[Movement]) {
        let mut file=std::fs::File::create(path).unwrap();
        for movement in movements {writeln!(file,"{}",serde_json::to_string(movement).unwrap()).unwrap();}
    }
}
