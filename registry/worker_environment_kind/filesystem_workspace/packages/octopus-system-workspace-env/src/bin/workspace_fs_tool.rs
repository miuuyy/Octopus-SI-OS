#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = octopus_system_workspace_env::run_workspace_fs_tool_process().await {
        eprintln!("octopus-workspace-fs-tool failed: {error}");
        std::process::exit(1);
    }
}
