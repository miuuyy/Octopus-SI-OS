const IDLE_BEFORE_SLEEP: std::time::Duration = std::time::Duration::from_secs(120);

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let interface = std::env::var("OCTOPUS_INTERFACE_ID")
        .map(octopus_descriptors::InterfaceId::new)
        .expect("OCTOPUS_INTERFACE_ID is required by the system-room door life");
    if let Err(error) = octopus_tool_door_runtime::run_spawned_interface_life(
        octopus_tool_door_runtime::sleeping_tool_door_interface_life_with_output(
            octopus_tool_door_runtime::ToolDoorOwner {
                worker_environment_kind: octopus_descriptors::WorkerEnvironmentKindId::new(
                    "system_room",
                ),
                door_interface: interface,
                max_in_flight: 16,
                private_read_max_bytes: 4096,
                root_slot_transform: None,
            },
            octopus_tool_door_runtime::default_tool_door_artifact_decoder,
            Some(octopus_system_room_tool_runtime::client_result_callback()),
            IDLE_BEFORE_SLEEP,
        )
        .expect("owned tool-door configuration is valid"),
    )
    .await
    {
        eprintln!("octopus-system-room-tool-door-life failed: {error}");
        std::process::exit(1);
    }
}
