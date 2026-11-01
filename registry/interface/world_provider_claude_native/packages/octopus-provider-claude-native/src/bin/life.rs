#[tokio::main(flavor = "current_thread")]
async fn main() {
    let doctor = std::env::args().skip(1).eq(["--doctor".to_string()]);
    let result = if doctor {
        octopus_provider_claude_native::doctor()
    } else {
        octopus_world_pyecho_life_runtime::run_spawned_interface_life(
            octopus_provider_claude_native::interface_life(),
        )
        .await
    };
    if let Err(error) = result {
        eprintln!("octopus-provider-claude-native-life failed: {error}");
        std::process::exit(1);
    }
}
