use std::sync::Arc;

use octopus_process_session_tool_runtime::ToolProcessEnvironment;
mod process_room;
pub use process_room::ProcessRoom;

pub async fn run_process_room_tool_process() -> Result<(), String> {
    let env = ToolProcessEnvironment::from_env()?;
    let process = Arc::new(ProcessRoom::new(env));
    octopus_process_session_tool_runtime::run_tool_process(process).await
}
