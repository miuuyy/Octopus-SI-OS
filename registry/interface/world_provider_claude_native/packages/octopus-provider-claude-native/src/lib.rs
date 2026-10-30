mod child;
mod config;
mod context;
mod controls;
mod input;
mod journal;
mod life;
mod stream;
mod tools;

pub use child::{ChildError, Event, Item, ItemKind, ReadyInfo, SdkChild, SessionConfig, TextInput, ToolSpec, TurnStatus, Usage};
pub use config::{ClaudeNativeConfig, InputMessage, RequestContextKind, SDK_CHILD_PROTOCOL, verify_claude_grant, verify_sdk_child};
pub use input::{DecodedInput, artifact_turn_text, decode as decode_input};
pub use journal::{InflightTurn, Journal, PendingCallRecord, RunSession};
pub use controls::{ControlResult, TurnControl, interrupted_result_payload};
pub use life::interface_life;
pub use stream::{LiveTurn, identity_hash as stream_identity_hash, retire_open_streams};

use octopus_descriptors::InterfaceId;

pub fn doctor() -> Result<(), String> {
    let interface = std::env::var("OCTOPUS_INTERFACE_ID")
        .map(InterfaceId::new)
        .map_err(|_| "OCTOPUS_INTERFACE_ID is required by the doctor contract".to_string())?;
    let interface_dir = std::env::var_os("OCTOPUS_INTERFACE_DIR")
        .map(std::path::PathBuf::from)
        .ok_or("OCTOPUS_INTERFACE_DIR is required by the doctor contract")?;
    let descriptor = octopus_world_pyecho_life_runtime::interface_descriptor_from_process_env(&interface)?
        .ok_or_else(|| {
            format!(
                "{} is required by the doctor contract",
                octopus_interface_transport::RESOLVED_INTERFACE_DESCRIPTOR_ENV
            )
        })?;
    let config = ClaudeNativeConfig::from_descriptor(&descriptor)?;
    verify_claude_grant(&interface_dir, &config.sdk_child.claude_executable)?;
    let registry_interface_root = interface_dir.parent().ok_or("OCTOPUS_INTERFACE_DIR has no registry interface root")?;
    verify_sdk_child(&config.sdk_child_executable(registry_interface_root)?)
}
