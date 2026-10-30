use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use octopus_descriptors::WorkerEnvironmentKindId;
use octopus_tool_handlers::{
    ToolCall, ToolHandler, ToolHandlerContext, ToolHandlerError, ToolResultArtifact,
};
use octopus_tool_registry::load_tool_descriptor_from_dir;
use octopus_worker_env::{
    AbsPath, ExecOutput, ExecRequest, ExecSurface, FileSurface, NetworkCeiling, PatchOutcome,
    ProjectedSandbox, WorkerEnvError,
};
use serde_json::json;

use super::WorkspaceFsHandler;

#[test]
fn workspace_tool_code_world_is_exactly_the_leased_kind() {
    let mut descriptor =
        load_tool_descriptor_from_dir(&registry_root().join("tool"), "workspace.write_file")
            .unwrap()
            .unwrap();
    let leased_kind = WorkerEnvironmentKindId::new("filesystem_workspace");
    super::validate_tool_code_world(&descriptor, &leased_kind).unwrap();

    descriptor.supported_worker_environment_kinds.clear();
    assert!(super::validate_tool_code_world(&descriptor, &leased_kind).is_err());
}

#[derive(Default)]
struct MemoryFs {
    files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
}

impl FileSurface for MemoryFs {
    fn read(
        &self,
        path: &AbsPath,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<u8>, WorkerEnvError>> + Send + '_>,
    > {
        let result = self
            .files
            .lock()
            .unwrap()
            .get(path.as_path())
            .cloned()
            .ok_or_else(|| WorkerEnvError::ReadOutsideRoots(path.as_str().to_string()));
        Box::pin(async move { result })
    }

    fn write(
        &self,
        path: &AbsPath,
        bytes: &[u8],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<octopus_worker_env::FileWriteOutput, WorkerEnvError>,
                > + Send
                + '_,
        >,
    > {
        let previous_content = self
            .files
            .lock()
            .unwrap()
            .insert(path.as_path().to_path_buf(), bytes.to_vec());
        Box::pin(async move {
            Ok(octopus_worker_env::FileWriteOutput {
                previous_content: Ok(previous_content),
            })
        })
    }

    fn list(
        &self,
        _dir: &AbsPath,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<AbsPath>, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn apply_patch(
        &self,
        _patch: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<PatchOutcome, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async {
            Ok(PatchOutcome {
                changed_paths: Vec::new(),
                is_exact: true,
            })
        })
    }
}

struct RefusingExec;

impl ExecSurface for RefusingExec {
    fn run(
        &self,
        _request: octopus_worker_env::ExecRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecOutput, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async { Err(WorkerEnvError::ExecFailed("unexpected exec".to_string())) })
    }
}

#[derive(Default)]
struct RecordingExec {
    request: Mutex<Option<ExecRequest>>,
}

impl ExecSurface for RecordingExec {
    fn run(
        &self,
        request: ExecRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecOutput, WorkerEnvError>> + Send + '_>,
    > {
        *self.request.lock().unwrap() = Some(request);
        Box::pin(async {
            Ok(ExecOutput {
                exit_code: 0,
                stdout: b"hello\xff".to_vec(),
                stderr: Vec::new(),
                stdout_total_bytes: 11,
                stderr_total_bytes: 0,
                duration: std::time::Duration::from_millis(17),
                timed_out: false,
                stdout_truncated: true,
                stderr_truncated: false,
            })
        })
    }
}

fn registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

fn workspace_sandbox(root: &Path) -> ProjectedSandbox {
    ProjectedSandbox::from_parts(
        vec![AbsPath::new(root).unwrap()],
        Vec::new(),
        Vec::new(),
        NetworkCeiling::Full,
    )
}

#[tokio::test]
async fn exact_clone_id_executes_descriptor_owned_write_operation_and_stays_exact_in_response() {
    let descriptor =
        load_tool_descriptor_from_dir(&registry_root().join("tool"), "workspace.write_file")
            .unwrap()
            .unwrap();
    let exact_tool = "workspace.write_file.run_42";
    let fs = Arc::new(MemoryFs::default());
    let root = std::env::current_dir().unwrap();
    let context = ToolHandlerContext {
        run: "run_42".to_string(),
        room_root: root.clone(),
        sandbox: workspace_sandbox(&root),
        fs: fs.clone(),
        exec: Arc::new(RefusingExec),
        artifacts: None,
    };
    let call = ToolCall {
        tool_id: exact_tool.to_string(),
        arguments: json!({"path": "clone-proof.txt", "content": "descriptor-operation"}),
        handler_config: descriptor.handler_config.clone(),
    };

    let result = WorkspaceFsHandler.execute(&context, &call).await.unwrap();
    assert_eq!(result.body["result"], "file_written");
    assert_eq!(result.body["changes"][0]["kind"], "add");
    assert_eq!(result.body["changes"][0]["added"], 1);
    assert!(result.body["changes"][0]["diff"]
        .as_str()
        .unwrap()
        .contains("+descriptor-operation"));
    assert_eq!(
        fs.files
            .lock()
            .unwrap()
            .get(&root.join("clone-proof.txt"))
            .unwrap(),
        b"descriptor-operation"
    );
    let response =
        serde_json::to_value(ToolResultArtifact::success(exact_tool, None, result.body)).unwrap();
    assert_eq!(response["tool"], exact_tool);
    assert_eq!(response["label"], format!("{exact_tool}.result"));

    for (before, after, expected) in [
        (b"old\n".to_vec(), "new\n".to_string(), Some((1, 1))),
        (b"same\n".to_vec(), "same\n".to_string(), Some((0, 0))),
        (vec![0xff], "text".to_string(), None),
        (b"binary\0".to_vec(), "text".to_string(), None),
        (vec![b'x'; 256 * 1024], "text".to_string(), None),
    ] {
        fs.files
            .lock()
            .unwrap()
            .insert(root.join("receipt.txt"), before);
        let result = WorkspaceFsHandler
            .execute(
                &context,
                &ToolCall {
                    tool_id: exact_tool.into(),
                    arguments: json!({"path": "receipt.txt", "content": after}),
                    handler_config: descriptor.handler_config.clone(),
                },
            )
            .await
            .unwrap();
        let change = &result.body["changes"][0];
        assert_eq!(change["kind"], "update");
        if let Some((added, deleted)) = expected {
            assert_eq!(change["added"], added);
            assert_eq!(change["deleted"], deleted);
            if added > 0 {
                assert!(change["diff"].as_str().unwrap().contains("-old\n+new\n"));
            } else {
                assert_eq!(change["diff"], "");
            }
        } else {
            assert!(change["diff_unavailable"].is_string());
            assert!(change.get("added").is_none());
        }
    }

    for handler_config in [
        json!({}),
        json!({"operation": 1}),
        json!({"operation": "unknown"}),
    ] {
        let refused = WorkspaceFsHandler
            .execute(
                &context,
                &ToolCall {
                    tool_id: exact_tool.to_string(),
                    arguments: json!({"path": "refused.txt", "content": "no"}),
                    handler_config,
                },
            )
            .await
            .unwrap_err();
        assert!(refused.to_string().contains(exact_tool));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn coder_apply_patch_preserves_line_endings_and_refuses_symbolic_escape() {
    use std::os::unix::fs::symlink;

    let descriptor =
        load_tool_descriptor_from_dir(&registry_root().join("tool"), "workspace.apply_patch")
            .unwrap()
            .unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::write(root.join("lines.txt"), b"one\r\ntwo\r\n").unwrap();
    let context = ToolHandlerContext {
        run: "run_patch_options".to_string(),
        room_root: root.clone(),
        sandbox: workspace_sandbox(&root),
        fs: Arc::new(MemoryFs::default()),
        exec: Arc::new(RefusingExec),
        artifacts: None,
    };
    let update = WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.apply_patch.run_patch_options".to_string(),
                arguments: json!({
                    "patch": "*** Begin Patch\n*** Update File: lines.txt\n@@\n-one\n+ONE\n*** End Patch"
                }),
                handler_config: descriptor.handler_config.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(update.body["result"], "patch_applied");
    assert_eq!(update.body["changes"][0]["added"], 1);
    assert_eq!(update.body["changes"][0]["deleted"], 1);
    assert!(update.body["changes"][0]["diff"]
        .as_str()
        .unwrap()
        .contains("+ONE\r\n"));
    assert_eq!(
        std::fs::read(root.join("lines.txt")).unwrap(),
        b"ONE\r\ntwo\r\n"
    );

    let outside = tempfile::tempdir().unwrap();
    let outside_root = outside.path().canonicalize().unwrap();
    symlink(&outside_root, root.join("linked")).unwrap();
    let refused = WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.apply_patch.run_patch_options".to_string(),
                arguments: json!({
                    "patch": "*** Begin Patch\n*** Add File: linked/new.txt\n+refused\n*** End Patch"
                }),
                handler_config: descriptor.handler_config,
            },
        )
        .await
        .expect_err("a symbolic target outside formed roots must be refused");
    assert!(refused
        .to_string()
        .contains("read outside projected readable roots"));
    assert!(!outside_root.join("new.txt").exists());
}

#[tokio::test]
async fn read_file_returns_bounded_utf8_content_with_exact_continuation_offset() {
    let descriptor =
        load_tool_descriptor_from_dir(&registry_root().join("tool"), "workspace.read_file")
            .unwrap()
            .unwrap();
    let fs = Arc::new(MemoryFs::default());
    let root = std::env::current_dir().unwrap();
    fs.files
        .lock()
        .unwrap()
        .insert(root.join("utf8.txt"), "ab😀cd".as_bytes().to_vec());
    let context = ToolHandlerContext {
        run: "run_utf8".to_string(),
        room_root: root.clone(),
        sandbox: workspace_sandbox(&root),
        fs,
        exec: Arc::new(RefusingExec),
        artifacts: None,
    };

    let first = WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.read_file.run_utf8".to_string(),
                arguments: json!({"path": "utf8.txt", "max_bytes": 4}),
                handler_config: descriptor.handler_config.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.body["content"], "ab");
    assert_eq!(first.body["bytes_read"], 2);
    assert_eq!(first.body["next_offset_bytes"], 2);
    assert_eq!(first.body["truncated"], true);
    assert!(first.body.get("bytes").is_none());

    let second = WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.read_file.run_utf8".to_string(),
                arguments: json!({"path": "utf8.txt", "offset_bytes": 2, "max_bytes": 4}),
                handler_config: descriptor.handler_config,
            },
        )
        .await
        .unwrap();
    assert_eq!(second.body["content"], "😀");
    assert_eq!(second.body["bytes_read"], 4);
    assert_eq!(second.body["next_offset_bytes"], 6);
    assert_eq!(second.body["truncated"], true);
}

#[tokio::test]
async fn build_returns_byte_exact_tagged_evidence_and_separate_lossy_preview() {
    let descriptor = load_tool_descriptor_from_dir(&registry_root().join("tool"), "workspace.build")
        .unwrap()
        .unwrap();
    let exec = Arc::new(RecordingExec::default());
    let root = std::env::current_dir().unwrap();
    let context = ToolHandlerContext {
        run: "run_exec".to_string(),
        room_root: root.clone(),
        sandbox: workspace_sandbox(&root),
        fs: Arc::new(MemoryFs::default()),
        exec: exec.clone(),
        artifacts: None,
    };
    let result = super::WorkspaceExecHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.build.run_exec".to_string(),
                arguments: json!({
                    "command": "printf hello",
                    "workdir": ".",
                    "timeout_ms": 1234,
                    "max_output_bytes": 99
                }),
                handler_config: descriptor.handler_config,
            },
        )
        .await
        .unwrap();

    let request = exec.request.lock().unwrap().clone().unwrap();
    assert_eq!(request.argv, ["sh", "-lc", "printf hello"]);
    assert_eq!(request.cwd.unwrap().as_path(), root);
    assert_eq!(request.timeout, std::time::Duration::from_millis(1234));
    assert_eq!(request.max_output_bytes, 99);
    assert_eq!(result.body["result"], "command_finished");
    let stdout = &result.body["output"]["stdout"];
    assert_eq!(stdout["encoding"], "base64");
    assert_eq!(stdout["retained_bytes"], 6);
    assert_eq!(
        stdout["retained_blake3"],
        blake3::hash(b"hello\xff").to_hex().to_string()
    );
    assert_eq!(
        BASE64_STANDARD
            .decode(stdout["base64"].as_str().unwrap())
            .unwrap(),
        b"hello\xff"
    );
    assert_eq!(result.body["output"]["stderr"]["encoding"], "utf8");
    assert_eq!(result.body["output"]["stderr"]["text"], "");
    assert_eq!(result.body["output"]["stdout_preview"]["text"], "hello�");
    assert_eq!(result.body["output"]["stdout_preview"]["lossy"], true);
    assert_eq!(result.body["output"]["stdout_truncated"], true);
    assert_eq!(result.body["output"]["stdout_total_bytes"], 11);
    assert_eq!(result.body["output"]["stdout_omitted_bytes"], 5);
    assert_eq!(result.body["output"]["stdout_retention"], "head_tail");
    assert_eq!(result.body["output"]["duration_ms"], 17);
    assert!(result.body["output"].get("stdout_base64").is_none());
}

#[tokio::test]
async fn direct_workspace_keeps_project_cwd_while_using_an_additional_granted_root() {
    let host = tempfile::tempdir().unwrap();
    let host_root = host.path().canonicalize().unwrap();
    let workspace_root = host_root.join("workspace");
    let outside_root = host_root.join("outside");
    std::fs::create_dir_all(&workspace_root).unwrap();
    std::fs::create_dir_all(&outside_root).unwrap();
    let outside_file = outside_root.join("outside.txt");
    std::fs::write(&outside_file, "before\n").unwrap();
    let sandbox = ProjectedSandbox::from_parts(
        vec![
            AbsPath::new(&workspace_root).unwrap(),
            AbsPath::new(&host_root).unwrap(),
        ],
        Vec::new(),
        Vec::new(),
        NetworkCeiling::Full,
    );
    let fs = Arc::new(MemoryFs::default());
    let exec = Arc::new(RecordingExec::default());
    let context = ToolHandlerContext {
        run: "run_expanded_workspace".to_string(),
        room_root: workspace_root.clone(),
        sandbox,
        fs: fs.clone(),
        exec: exec.clone(),
        artifacts: None,
    };

    let resolved = super::workspace_fs::resolve_room_path(
        &context,
        "workspace.write_file",
        outside_file.to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(resolved.as_path(), outside_file);
    assert_eq!(
        super::workspace_fs::resolve_room_path(&context, "workspace.write_file", "relative.txt")
            .unwrap()
            .as_path(),
        workspace_root.join("relative.txt")
    );

    WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.write_file".to_string(),
                arguments: json!({
                    "path": outside_file.to_string_lossy(),
                    "content": "written"
                }),
                handler_config: json!({"operation": "write_file"}),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fs.files.lock().unwrap().get(&outside_file).unwrap(),
        b"written"
    );

    let result = super::WorkspaceExecHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.build".to_string(),
                arguments: json!({
                    "command": "pwd",
                    "workdir": outside_root.to_string_lossy(),
                    "timeout_ms": 1000,
                    "max_output_bytes": 1024
                }),
                handler_config: json!({
                    "operation": "run_command",
                    "default_timeout_ms": 1000,
                    "max_timeout_ms": 1000,
                    "default_max_output_bytes": 1024,
                    "max_output_bytes": 1024,
                    "default_shell": "sh",
                    "allowed_shells": ["sh"],
                    "allow_login_shell": false
                }),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result.body["workdir"],
        outside_root.to_string_lossy().as_ref()
    );
    assert_eq!(
        exec.request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .cwd
            .as_ref()
            .unwrap()
            .as_path(),
        outside_root
    );

    WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.apply_patch".to_string(),
                arguments: json!({
                    "patch": format!(
                        "*** Begin Patch\n*** Update File: {}\n@@\n-before\n+after\n*** End Patch",
                        outside_file.display()
                    )
                }),
                handler_config: json!({"operation": "apply_patch"}),
            },
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&outside_file).unwrap(), "after\n");
    for (mode, query) in [("content", "after"), ("path", "outside.txt")] {
        let call = ToolCall {
            tool_id: "workspace.grep".to_string(),
            arguments: json!({"path": outside_root, "mode": mode, "query": query}),
            handler_config: grep_config(10, 1024, 1000, 100),
        };
        let result = WorkspaceFsHandler.execute(&context, &call).await.unwrap();
        let matched = &result.body["matches"][0];
        assert_eq!(
            matched.get("path").unwrap_or(matched),
            outside_file.to_str().unwrap()
        );
        let restricted = ToolHandlerContext {
            sandbox: workspace_sandbox(&workspace_root),
            ..context.clone()
        };
        assert!(WorkspaceFsHandler.execute(&restricted, &call).await.is_err());
    }
}

#[test]
fn direct_workspace_operations_keep_kind_owned_command_routing() {
    let tool_root = registry_root().join("tool");
    let kind_id = WorkerEnvironmentKindId::new("filesystem_workspace");
    let (kind, _) = octopus_worker_env::WorkerEnvKindRegistry::load_kind_from_dir(
        &registry_root().join("worker_environment_kind"),
        &kind_id,
    )
    .unwrap()
    .get(&kind_id)
    .unwrap();
    let descriptors = [
        ("workspace.exec", super::WorkspaceOperation::StartCommand),
        ("workspace.write_stdin", super::WorkspaceOperation::WriteStdin),
        (
            "workspace.process.signal",
            super::WorkspaceOperation::ProcessSignal,
        ),
        (
            "workspace.process.terminate",
            super::WorkspaceOperation::ProcessTerminate,
        ),
        ("workspace.process.list", super::WorkspaceOperation::ProcessList),
    ]
    .map(|(id, operation)| {
        (
            load_tool_descriptor_from_dir(&tool_root, id)
                .unwrap()
                .unwrap(),
            operation,
        )
    });
    for (descriptor, expected) in &descriptors {
        super::validate_tool_code_world(descriptor, &kind_id).unwrap();
        assert_eq!(descriptor.interface, descriptors[0].0.interface);
        assert!(kind.tool_process_bins.contains(&descriptor.process.bin));
        let config = super::workspace_handler_config(&ToolCall {
            tool_id: descriptor.id.as_str().to_string(),
            arguments: json!({}),
            handler_config: descriptor.handler_config.clone(),
        })
        .unwrap();
        assert_eq!(&config.operation, expected);
    }
}

#[test]
fn command_strings_use_only_descriptor_owned_shell_and_login_modes() {
    let descriptor = load_tool_descriptor_from_dir(&registry_root().join("tool"), "workspace.exec")
        .unwrap()
        .unwrap();
    let config = super::workspace_handler_config(&ToolCall {
        tool_id: descriptor.id.as_str().to_string(),
        arguments: json!({}),
        handler_config: descriptor.handler_config,
    })
    .unwrap();
    assert_eq!(
        super::workspace_exec::command_argv(
            "workspace.exec",
            &json!({"command": "printf ready", "shell": "zsh", "login": false}),
            &config,
        )
        .unwrap(),
        ["zsh", "-c", "printf ready"]
    );
    assert!(
        super::workspace_exec::command_argv(
            "workspace.exec",
            &json!({"command": "printf refused", "shell": "fish"}),
            &config,
        )
        .is_err()
    );
    assert!(
        super::workspace_exec::command_argv(
            "workspace.exec",
            &json!({"command": ["printf", "exact"], "login": true}),
            &config,
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn file_tool_path_resolution_refuses_symbolic_escape_outside_formed_roots() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), root.path().join("escape")).unwrap();
    let context = ToolHandlerContext {
        run: "symlink_path".to_string(),
        room_root: root.path().to_path_buf(),
        sandbox: workspace_sandbox(root.path()),
        fs: Arc::new(MemoryFs::default()),
        exec: Arc::new(RefusingExec),
        artifacts: None,
    };
    let error = super::workspace_fs::resolve_room_path(
        &context,
        "workspace.read_file",
        "escape/private.txt",
    )
    .expect_err("a room-relative symlink target outside formed roots must fail closed");
    assert!(
        error
            .to_string()
            .contains("read outside projected readable roots")
    );
}

#[test]
fn persistent_poll_preserves_ordered_exact_chunks_and_fails_structurally_on_gaps() {
    let root = std::env::current_dir().unwrap();
    let context = ToolHandlerContext {
        run: "persistent_poll".to_string(),
        room_root: root.clone(),
        sandbox: workspace_sandbox(&root),
        fs: Arc::new(MemoryFs::default()),
        exec: Arc::new(RefusingExec),
        artifacts: None,
    };
    let poll = octopus_workspace_substrate::WorkspaceProcessPoll {
        process_id: "coder-process-opaque".to_string(),
        chunks: vec![octopus_workspace_substrate::WorkspaceProcessChunk {
            seq: 1,
            stream: octopus_workspace_substrate::ExecOutputStream::Stdout,
            byte_offset: 0,
            end_of_event: true,
            bytes: b"exact\xff".to_vec(),
        }],
        next_seq: 2,
        running: true,
        exited: false,
        exit_code: None,
        exit_seq: None,
        closed: false,
        close_seq: None,
        continuation_available: true,
        timed_out: false,
        failure: None,
        tty: false,
        cwd: AbsPath::new(&root).unwrap(),
    };
    let result =
        super::workspace_exec::persistent_poll_result(&context, "workspace.exec", poll.clone()).unwrap();
    assert_eq!(result.body["result"], "command_running");
    assert_eq!(result.body["output"]["chunks"][0]["source_seq"], 1);
    assert_eq!(
        result.body["output"]["chunks"][0]["content"]["encoding"],
        "base64"
    );

    let error = super::workspace_exec::persistent_poll_result(
        &context,
        "workspace.exec",
        octopus_workspace_substrate::WorkspaceProcessPoll {
            failure: Some("ordered event gap".to_string()),
            ..poll
        },
    )
    .expect_err("collector gaps must emit a failed tool result");
    let ToolHandlerError::ExecutionWithDetails { details, .. } = error else {
        panic!("collector failure must preserve structured process evidence");
    };
    assert_eq!(details["process_id"], "coder-process-opaque");
    assert_eq!(details["output"]["failure"], "ordered event gap");
}

fn grep_config(
    max_scanned_files: usize,
    max_scanned_bytes: u64,
    deadline_ms: u64,
    max_line_bytes: usize,
) -> serde_json::Value {
    json!({
        "operation": "grep",
        "max_scanned_files": max_scanned_files,
        "max_scanned_bytes": max_scanned_bytes,
        "deadline_ms": deadline_ms,
        "max_line_bytes": max_line_bytes,
    })
}

#[tokio::test]
async fn content_grep_bounds_line_memory_and_total_scanned_bytes() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::write(
        root_path.join("a-long.txt"),
        "0123456789needle-after-limit\n",
    )
    .unwrap();
    std::fs::write(root_path.join("b-match.txt"), "needle\n").unwrap();
    let context = ToolHandlerContext {
        run: "run_grep_line_limit".to_string(),
        room_root: root_path.clone(),
        sandbox: workspace_sandbox(&root_path),
        fs: Arc::new(MemoryFs::default()),
        exec: Arc::new(RefusingExec),
        artifacts: None,
    };
    let result = WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.grep.run_grep_line_limit".to_string(),
                arguments: json!({"query": "needle"}),
                handler_config: grep_config(10, 1024, 1000, 16),
            },
        )
        .await
        .unwrap();

    assert_eq!(result.body["matches"][0]["path"], "b-match.txt");
    assert_eq!(result.body["oversized_lines_skipped"], 1);
    assert_eq!(result.body["scanned_files"], 2);
    assert_eq!(result.body["truncated"], true);
    assert_eq!(result.body["truncation_reasons"], json!(["max_line_bytes"]));
    assert!(result.body["scanned_bytes"].as_u64().unwrap() <= 1024);

    std::fs::write(root_path.join("0-byte-limit.txt"), vec![b'x'; 64]).unwrap();
    let byte_limited = WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.grep.run_grep_byte_limit".to_string(),
                arguments: json!({"query": "absent"}),
                handler_config: grep_config(10, 8, 1000, 64),
            },
        )
        .await
        .unwrap();
    assert_eq!(byte_limited.body["scanned_bytes"], 8);
    assert_eq!(
        byte_limited.body["truncation_reasons"],
        json!(["max_scanned_bytes"])
    );
}

#[tokio::test]
async fn content_grep_enforces_descriptor_file_limit_and_complete_limit_config() {
    let descriptor = load_tool_descriptor_from_dir(&registry_root().join("tool"), "workspace.grep")
        .unwrap()
        .unwrap();
    let declared = super::workspace_handler_config(&ToolCall {
        tool_id: "workspace.grep".to_string(),
        arguments: json!({}),
        handler_config: descriptor.handler_config,
    })
    .unwrap();
    assert_eq!(declared.max_scanned_files, Some(50000));
    assert_eq!(declared.max_scanned_bytes, Some(268435456));
    assert_eq!(declared.deadline_ms, Some(30000));
    assert_eq!(declared.max_line_bytes, Some(1048576));

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    std::fs::write(root_path.join("a-first.txt"), "none\n").unwrap();
    std::fs::write(root_path.join("b-second.txt"), "needle\n").unwrap();
    let context = ToolHandlerContext {
        run: "run_grep_file_limit".to_string(),
        room_root: root_path.clone(),
        sandbox: workspace_sandbox(&root_path),
        fs: Arc::new(MemoryFs::default()),
        exec: Arc::new(RefusingExec),
        artifacts: None,
    };
    let limited = WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.grep.run_grep_file_limit".to_string(),
                arguments: json!({"query": "needle"}),
                handler_config: grep_config(1, 1024, 1000, 64),
            },
        )
        .await
        .unwrap();
    assert_eq!(limited.body["returned_match_count"], 0);
    assert_eq!(limited.body["scanned_files"], 1);
    assert_eq!(
        limited.body["truncation_reasons"],
        json!(["max_scanned_files"])
    );

    let incomplete = WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.grep.run_incomplete_config".to_string(),
                arguments: json!({"query": "needle"}),
                handler_config: json!({
                    "operation": "grep",
                    "max_scanned_files": 1,
                    "max_scanned_bytes": 1024,
                    "max_line_bytes": 64
                }),
            },
        )
        .await
        .expect_err("grep config without a deadline must fail closed");
    assert!(
        incomplete
            .to_string()
            .contains("grep operation requires complete scan limits")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn apply_patch_failure_reports_the_partial_room_effect() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let locked = root_path.join("locked");
    let out = root_path.join("out");
    std::fs::create_dir(&locked).unwrap();
    std::fs::create_dir(&out).unwrap();
    std::fs::write(locked.join("src.txt"), "line\n").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

    let context = ToolHandlerContext {
        run: "run_partial_patch".to_string(),
        room_root: root_path.clone(),
        sandbox: workspace_sandbox(&root_path),
        fs: Arc::new(MemoryFs::default()),
        exec: Arc::new(RefusingExec),
        artifacts: None,
    };
    let error = WorkspaceFsHandler
        .execute(
            &context,
            &ToolCall {
                tool_id: "workspace.apply_patch".to_string(),
                arguments: json!({
                    "patch": "*** Begin Patch\n*** Update File: locked/src.txt\n*** Move to: out/dst.txt\n@@\n-line\n+line2\n*** End Patch"
                }),
                handler_config: json!({"operation": "apply_patch"}),
            },
        )
        .await
        .expect_err("source removal fails after destination materialization");

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ToolHandlerError::ExecutionWithDetails { details, .. } = error else {
        panic!("partial effect must be a structured tool failure");
    };
    assert_eq!(details["partial_effect"], true);
    assert_eq!(details["changed_paths"], json!(["out/dst.txt"]));
    assert_eq!(details["changes"][0]["kind"], "add");
    assert_eq!(
        std::fs::read_to_string(out.join("dst.txt")).unwrap(),
        "line2\n"
    );
}
