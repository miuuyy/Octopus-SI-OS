#[path = "../owner_handoff.rs"]
mod owner_handoff;

const IDLE_BEFORE_SLEEP: std::time::Duration = std::time::Duration::from_secs(120);

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let interface = std::env::var("OCTOPUS_INTERFACE_ID")
        .map(octopus_descriptors::InterfaceId::new)
        .expect("OCTOPUS_INTERFACE_ID is required by the code-mode door life");
    let handoff = owner_handoff::OwnerHandoff::new();
    let door = octopus_tool_door_runtime::sleeping_session_tool_door_interface_life_with_output(
        octopus_tool_door_runtime::ToolDoorOwner {
            worker_environment_kind: octopus_descriptors::WorkerEnvironmentKindId::new("code_mode"),
            door_interface: interface,
            max_in_flight: 16,
            private_read_max_bytes: 4096,
            root_slot_transform: None,
        },
        octopus_code_mode_room::decode_code_mode_door_artifact,
        Some(handoff.callback()),
        IDLE_BEFORE_SLEEP,
    )
    .expect("owned tool-door configuration is valid");
    let life: octopus_tool_door_runtime::InterfaceLiftFn = std::sync::Arc::new(move |ctx| {
        let door = door.clone();
        let handoff = handoff.clone();
        Box::pin(async move {
            let cancellation = ctx.owner_cancel();
            let door = door(ctx.clone());
            let observer = handoff.observe(ctx);
            tokio::pin!(door, observer);
            tokio::select! {
                result = &mut door => { cancellation.cancel(); let observed = observer.await; result.and(observed) },
                result = &mut observer => { cancellation.cancel(); let detached = door.await; result.and(detached) },
            }
        })
    });
    if let Err(error) = octopus_tool_door_runtime::run_spawned_interface_life(life).await {
        eprintln!("octopus-code-mode-tool-door-life failed: {error}");
        std::process::exit(1);
    }
}
