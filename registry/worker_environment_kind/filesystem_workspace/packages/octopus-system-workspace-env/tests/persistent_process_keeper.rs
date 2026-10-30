use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use octopus_descriptors::ProcessSandboxMode;
use octopus_workspace_substrate::{WorkspaceProcessLimits, WorkspaceProcessManager, WorkspaceProcessStore};
use octopus_worker_env::{AbsPath, NetworkCeiling, ProjectedSandbox, WorkerEnvironmentId};

#[tokio::test(flavor = "multi_thread")]
async fn a_room_command_outlives_its_tool_process_and_the_next_one_receives_its_exact_outcome() {
    let temporary = tempfile::Builder::new().prefix("keeper").tempdir_in("/tmp").unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    for directory in ["durable", "ipc", "work"] {
        std::fs::create_dir(root.join(directory)).unwrap();
    }
    let store = WorkspaceProcessStore::new(
        &root.join("durable"),
        &root.join("ipc"),
        PathBuf::from(env!("CARGO_BIN_EXE_octopus-workspace-exec-tool")),
    )
    .unwrap();
    let cwd = AbsPath::new(&root.join("work")).unwrap();
    let sandbox = ProjectedSandbox::from_parts(
        vec![cwd.clone()],
        Vec::new(),
        vec![AbsPath::new(Path::new("/")).unwrap()],
        NetworkCeiling::Denied,
    );
    let room = WorkerEnvironmentId::derive(b"keeper-contract");
    let limits = WorkspaceProcessLimits {
        max_processes_per_worker_environment: 2,
        max_buffered_output_bytes: 4096,
        max_buffered_output_events: 64,
        max_input_bytes: 64,
        process_timeout: Duration::from_secs(30),
        cleanup_timeout: Duration::from_secs(5),
    };
    let text = |poll: &octopus_workspace_substrate::WorkspaceProcessPoll| {
        poll.chunks.iter().flat_map(|chunk| chunk.bytes.clone()).collect::<Vec<_>>()
    };

    let first_owner = WorkspaceProcessManager::new(Some(store.clone()));
    let concurrent_owner = WorkspaceProcessManager::new(Some(store.clone()));
    let (started, concurrent) = tokio::join!(
        start(&first_owner, &room, &sandbox, &cwd, &limits),
        start(&concurrent_owner, &room, &sandbox, &cwd, &limits),
    );
    let started = started.unwrap();
    assert_eq!(concurrent.unwrap(), started);
    assert!(started.running && !started.closed);
    assert_eq!(text(&started), b"started");
    assert_eq!(start(&first_owner, &room, &sandbox, &cwd, &limits).await.unwrap(), started, "a re-invoked start returns its exact result and starts nothing");
    let written = first_owner
        .write_and_poll(&room, &started.process_id, b"SURVIVED\n", 0, 1024, Some("movement:11"))
        .await
        .unwrap();
    assert!(written.running && written.chunks.is_empty());
    drop(first_owner);

    let next_owner = WorkspaceProcessManager::new(Some(store));
    assert_eq!(
        next_owner
            .write_and_poll(&room, &started.process_id, b"SURVIVED\n", 0, 1024, Some("movement:11"))
            .await
            .unwrap(),
        written,
        "a re-invoked write is answered exactly and writes nothing twice"
    );
    let finished = next_owner
        .write_and_poll(&room, &started.process_id, b"", 10_000, 1024, Some("movement:12"))
        .await
        .unwrap();
    assert_eq!(text(&finished), b" SURVIVED");
    assert_eq!((finished.exit_code, finished.closed, finished.continuation_available), (Some(7), true, false));
    assert_eq!(
        next_owner
            .write_and_poll(&room, &started.process_id, b"", 0, 1024, Some("movement:12"))
            .await
            .unwrap(),
        finished
    );
    assert!(next_owner.list(&room).await.unwrap().is_empty());
    assert_eq!(start(&next_owner, &room, &sandbox, &cwd, &limits).await.unwrap(), started);
    assert_eq!(next_owner.write_and_poll(&room, &started.process_id, b"SURVIVED\n", 0, 1024, Some("movement:11")).await.unwrap(), written);
    next_owner.release(&room, None).await.unwrap();
    assert_eq!(start(&next_owner, &room, &sandbox, &cwd, &limits).await.unwrap(), started);
    assert_eq!(next_owner.write_and_poll(&room, &started.process_id, b"", 0, 1024, Some("movement:12")).await.unwrap(), finished);
    assert!(next_owner
        .write_and_poll(&room, &started.process_id, b"", 0, 1024, Some("movement:13"))
        .await
        .unwrap_err()
        .contains("retired"));
    let responses = root.join("durable/persistent-processes").join(format!("{}.calls", started.process_id));
    for entry in std::fs::read_dir(responses).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "response") {
            std::fs::remove_file(path).unwrap();
        }
    }
    assert!(start(&next_owner, &room, &sandbox, &cwd, &limits).await.unwrap_err().contains("cannot be re-executed"));
}

async fn start(
    owner: &WorkspaceProcessManager,
    room: &WorkerEnvironmentId,
    sandbox: &ProjectedSandbox,
    cwd: &AbsPath,
    limits: &WorkspaceProcessLimits,
) -> Result<octopus_workspace_substrate::WorkspaceProcessPoll, String> {
    owner
        .start(
            room,
            sandbox,
            ProcessSandboxMode::Disabled,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf started; IFS= read line; sleep 1; printf ' %s' \"$line\"; exit 7".to_string(),
            ],
            cwd.as_path().to_path_buf(),
            BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
            false,
            300,
            1024,
            limits.clone(),
            Some("movement:10"),
        )
        .await
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn nested_sandbox_timeout_closes_owned_processes_without_signaling_a_neighbor() {
    const CHILD_ROOT: &str = "OCTOPUS_KEEPER_TIMEOUT_TEST_ROOT";
    const NEIGHBOR: &str = "OCTOPUS_KEEPER_TIMEOUT_TEST_NEIGHBOR";
    const PRIVATE_FILE: &str = "OCTOPUS_KEEPER_TIMEOUT_TEST_PRIVATE_FILE";
    const TTY: &str = "OCTOPUS_KEEPER_TIMEOUT_TEST_TTY";
    const ROOT_EXIT: &str = "OCTOPUS_KEEPER_TIMEOUT_TEST_ROOT_EXIT";
    if let Ok(root) = std::env::var(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let tty = std::env::var(TTY).as_deref() == Ok("true");
        let root_exit = std::env::var(ROOT_EXIT).unwrap();
        let cwd = AbsPath::new(&root.join("work")).unwrap();
        let store = WorkspaceProcessStore::new(
            &root.join("durable"), &root.join("ipc"),
            PathBuf::from(env!("CARGO_BIN_EXE_octopus-workspace-exec-tool")),
        ).unwrap();
        let manager = WorkspaceProcessManager::new(Some(store));
        let room = WorkerEnvironmentId::derive(format!("nested-timeout-contract-{tty}-{root_exit}").as_bytes());
        let sandbox = ProjectedSandbox::from_parts(
            vec![cwd.clone()], Vec::new(), vec![cwd.clone()], NetworkCeiling::Denied,
        );
        let limits = WorkspaceProcessLimits {
            max_processes_per_worker_environment: 2,
            max_buffered_output_bytes: 4096, max_buffered_output_events: 64,
            max_input_bytes: 64, process_timeout: Duration::from_millis(500),
            cleanup_timeout: Duration::from_secs(2),
        };
        let started = manager.start(
            &room, &sandbox, ProcessSandboxMode::Enforced,
            vec!["/bin/sh".into(), "-c".into(),
                "if /bin/kill -TERM \"$1\" 2>/dev/null; then exit 91; fi; if /bin/kill -TERM \"$PPID\" 2>/dev/null; then exit 93; fi; if /bin/cat \"$2\" 2>/dev/null; then exit 92; fi; trap '' HUP; /bin/sleep 8 & printf '%s %s\\n' \"$$\" \"$!\"; if [ \"$3\" = true ]; then exit 0; fi; wait".into(),
                "sh".into(), std::env::var(NEIGHBOR).unwrap(),
                std::env::var(PRIVATE_FILE).unwrap(), root_exit.clone()],
            cwd.as_path().to_path_buf(),
            BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            tty, 100, 4096, limits, Some("movement:timeout-start"),
        ).await.unwrap();
        let process_id = started.process_id.clone();
        let mut output: Vec<u8> = started.chunks.iter().flat_map(|c| c.bytes.clone()).collect();
        let finished = tokio::time::timeout(Duration::from_secs(5), async {
            let mut poll = started;
            let mut call = 0;
            while !poll.closed {
                call += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
                poll = manager.write_and_poll(
                    &room, &process_id, b"", 100, 4096, Some(&format!("movement:timeout-poll-{call}")),
                ).await.unwrap();
                output.extend(poll.chunks.iter().flat_map(|c| c.bytes.clone()));
                assert!(poll.failure.is_none(), "{:?}", poll.failure);
            }
            poll
        }).await.expect("owned command must close before its natural exit");
        assert!(finished.timed_out && finished.exited && finished.closed, "{finished:?}; output={}", String::from_utf8_lossy(&output));
        let pids: Vec<_> = std::str::from_utf8(&output).unwrap().split_whitespace().collect();
        assert_eq!(pids.len(), 2, "command must retain both its own and its child's PID");
        std::fs::write(root.join(format!("work/terminated-pids-{tty}-{root_exit}")), pids.join(" ")).unwrap();
        manager.release(&room, None).await.unwrap();
        return;
    }
    let temporary = tempfile::Builder::new().prefix("nested-keeper").tempdir_in("/tmp").unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    for directory in ["durable", "ipc", "work"] { std::fs::create_dir(root.join(directory)).unwrap(); }
    let private = tempfile::NamedTempFile::new_in(std::env::current_dir().unwrap()).unwrap();
    std::fs::write(private.path(), "not granted to the command").unwrap();
    let mut neighbor = std::process::Command::new("/bin/sleep").arg("30").spawn().unwrap();
    let executable = std::env::current_exe().unwrap();
    let authority = octopus_verifiers::process_authority::ProcessAuthority::resolve(
        [root.clone()], [root.clone()],
        [executable.clone(), PathBuf::from(env!("CARGO_BIN_EXE_octopus-workspace-exec-tool"))],
        [], [], [], false, true, false, true, true, ProcessSandboxMode::Enforced,
    ).unwrap();
    let prepared = authority.prepare(executable.to_str().unwrap(),
        &["--exact".into(), "nested_sandbox_timeout_closes_owned_processes_without_signaling_a_neighbor".into(), "--nocapture".into()],
        &root, None, &root).unwrap();
    let results = [(false, false), (false, true), (true, false), (true, true)].map(|(tty, root_exit)| (tty, root_exit, std::process::Command::new(&prepared.program)
        .args(&prepared.args).env_clear().envs(&prepared.environment)
        .current_dir(&root)
        .env(CHILD_ROOT, &root).env(NEIGHBOR, neighbor.id().to_string())
        .env(PRIVATE_FILE, private.path()).env(TTY, tty.to_string()).env(ROOT_EXIT, root_exit.to_string()).output().unwrap()));
    let neighbor_survived = neighbor.try_wait().unwrap().is_none();
    let _ = neighbor.kill();
    let _ = neighbor.wait();
    assert!(neighbor_survived, "the neighboring process must stay outside the command's control");
    for (tty, root_exit, result) in results {
    assert!(result.status.success(), "tty={tty}, root_exit={root_exit}: {}\n{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    for pid in std::fs::read_to_string(root.join(format!("work/terminated-pids-{tty}-{root_exit}"))).unwrap().split_whitespace() {
        let state = std::process::Command::new("/bin/ps")
            .args(["-p", pid, "-o", "stat="]).output().unwrap();
        let status = String::from_utf8(state.stdout).unwrap();
        assert!(status.trim().is_empty() || status.trim().starts_with('Z'), "process {pid}: {status}");
    }
    }
}
