use octopus_descriptors::WorkerEnvironmentKindId;
use octopus_system_workspace_env::{WorkspaceOperation, workspace_handler_config};
use octopus_tool_handlers::ToolCall;
use octopus_tool_registry::load_tool_descriptor_from_dir;
use serde_json::json;

use super::{WorktreeOperation, worktree_handler_config};

fn registry_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn worktree_path_is_scoped_to_one_formation() {
    let base = tempfile::tempdir().unwrap();
    let worker_environment_id =
        octopus_worker_env::WorkerEnvironmentId::parse("we_0123456789abcdef0123456789abcdef")
            .unwrap();
    assert_eq!(
        octopus_git_sandbox::instance_worktree_path(base.path(), &worker_environment_id).unwrap(),
        base.path().join(worker_environment_id.as_str())
    );
}

#[test]
fn persistent_coder_operations_keep_kind_owned_command_routing() {
    let tool_root = registry_root().join("tool");
    let kind_id = WorkerEnvironmentKindId::new("git_worktree");
    let (kind, _) = octopus_worker_env::WorkerEnvKindRegistry::load_kind_from_dir(
        &registry_root().join("worker_environment_kind"),
        &kind_id,
    )
    .unwrap()
    .get(&kind_id)
    .unwrap();
    let load = |id: &str| load_tool_descriptor_from_dir(&tool_root, id).unwrap().unwrap();
    let exec = load("coder.exec");
    let abandon = load("coder.worktree.abandon");
    let config = worktree_handler_config(abandon.id.as_str(), &abandon.handler_config)
        .unwrap()
        .expect("worktree abandon is a layout operation");
    assert_eq!(config.operation, WorktreeOperation::WorktreeAbandon);
    for (id, expected) in [
        ("coder.exec", WorkspaceOperation::StartCommand),
        ("coder.write_stdin", WorkspaceOperation::WriteStdin),
        ("coder.process.signal", WorkspaceOperation::ProcessSignal),
        ("coder.process.terminate", WorkspaceOperation::ProcessTerminate),
        ("coder.process.list", WorkspaceOperation::ProcessList),
    ] {
        let descriptor = load(id);
        assert_eq!(descriptor.supported_worker_environment_kinds, vec![kind_id.clone()]);
        assert_eq!(descriptor.interface, exec.interface);
        assert!(kind.tool_process_bins.contains(&descriptor.process.bin));
        assert!(worktree_handler_config(id, &descriptor.handler_config).unwrap().is_none());
        let config = workspace_handler_config(&ToolCall {
            tool_id: descriptor.id.as_str().to_string(),
            arguments: json!({}),
            handler_config: descriptor.handler_config.clone(),
        })
        .unwrap();
        assert_eq!(config.operation, expected);
    }
    assert_eq!(abandon.supported_worker_environment_kinds, vec![kind_id]);
    assert!(kind.tool_process_bins.contains(&abandon.process.bin));
}
