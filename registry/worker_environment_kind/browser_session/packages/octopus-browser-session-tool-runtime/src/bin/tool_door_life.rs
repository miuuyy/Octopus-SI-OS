#[tokio::main(flavor = "current_thread")]
async fn main() {
    let interface = std::env::var("OCTOPUS_INTERFACE_ID")
        .map(octopus_descriptors::InterfaceId::new)
        .expect("OCTOPUS_INTERFACE_ID is required by the browser-session door life");
    if let Err(error) = octopus_tool_door_runtime::run_spawned_interface_life(
        octopus_tool_door_runtime::owned_tool_door_interface_life_with_decoder(
            octopus_tool_door_runtime::ToolDoorOwner {
                worker_environment_kind: octopus_descriptors::WorkerEnvironmentKindId::new(
                    "browser_session",
                ),
                door_interface: interface,
                max_in_flight: 16,
                private_read_max_bytes: 4096,
                root_slot_transform: Some(octopus_browser_session_env::prepare_browser_door_roots),
            },
            octopus_browser_session_env::decode_filechooser_door_artifact,
        )
        .expect("owned tool-door configuration is valid"),
    )
    .await
    {
        eprintln!("octopus-browser-session-tool-door-life failed: {error}");
        std::process::exit(1);
    }
}
