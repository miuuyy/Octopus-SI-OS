#[tokio::main(flavor = "current_thread")]
async fn main() {
    let interface = std::env::var("OCTOPUS_INTERFACE_ID")
        .map(octopus_descriptors::InterfaceId::new)
        .expect("OCTOPUS_INTERFACE_ID is required by the desktop-session door life");
    if let Err(error) = octopus_tool_door_runtime::run_spawned_interface_life(
        octopus_tool_door_runtime::owned_tool_door_interface_life_with_decoder(
            octopus_tool_door_runtime::ToolDoorOwner {
                worker_environment_kind: octopus_descriptors::WorkerEnvironmentKindId::new(
                    "desktop_session",
                ),
                door_interface: interface,
                max_in_flight: 16,
                private_read_max_bytes: 4096,
                root_slot_transform: Some(|ctx| {
                    octopus_system_gui_env::prepare_private_desktop_host(ctx.owner_root_slots())
                }),
            },
            octopus_system_gui_env::decode_gui_door_artifact,
        )
        .expect("owned tool-door configuration is valid"),
    )
    .await
    {
        eprintln!("octopus-desktop-session-tool-door-life failed: {error}");
        std::process::exit(1);
    }
}
