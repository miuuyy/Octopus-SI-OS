//! Operator turn controls, as the chat client sends them and the Codex
//! provider answers them: an `operator.turn.control` artifact names an
//! action and a control id; the provider answers with
//! `turn.control.acknowledged` or `turn.control.failed` carrying the same
//! control id, the control's own movement, and the native turn it reached.

use serde::Deserialize;
use serde_json::{Value, json};

pub const TURN_CONTROL_LABEL: &str = "operator.turn.control";
pub const CONTROL_ACKNOWLEDGED_LABEL: &str = "turn.control.acknowledged";
pub const CONTROL_FAILED_LABEL: &str = "turn.control.failed";
pub const INTERRUPTED_RESULT_LABEL: &str = "turn.interrupted.result";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TurnControl {
    Interrupt { control_id: String },
    Stop { control_id: String },
    Pause { control_id: String },
    Resume { control_id: String },
}

impl TurnControl {
    pub fn decode(payload: &Value) -> Result<Self, String> {
        let control: Self = serde_json::from_value(payload.clone())
            .map_err(|error| format!("operator turn control is malformed: {error}"))?;
        let id = control.control_id();
        if id.trim().is_empty() || id.trim() != id || id.len() > 256 {
            return Err("operator turn control has an empty, untrimmed, or oversized control_id".into());
        }
        Ok(control)
    }

    pub fn control_id(&self) -> &str {
        match self {
            Self::Interrupt { control_id } | Self::Stop { control_id } | Self::Pause { control_id } | Self::Resume { control_id } => control_id,
        }
    }

    pub fn action(&self) -> &'static str {
        match self {
            Self::Interrupt { .. } => "interrupt",
            Self::Stop { .. } => "stop",
            Self::Pause { .. } => "pause",
            Self::Resume { .. } => "resume",
        }
    }

    pub fn interrupts(&self) -> bool {
        matches!(self, Self::Interrupt { .. } | Self::Stop { .. })
    }
}

/// The payload of a control's answer. `acknowledged` selects the label.
pub struct ControlResult<'a> {
    pub control_id: &'a str,
    pub action: &'a str,
    pub acknowledged: bool,
    pub source_movement_seq: u64,
    pub thread_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
    pub detail: Value,
}

impl ControlResult<'_> {
    pub fn label(&self) -> &'static str {
        if self.acknowledged { CONTROL_ACKNOWLEDGED_LABEL } else { CONTROL_FAILED_LABEL }
    }

    pub fn payload(&self) -> Value {
        json!({
            "label": self.label(),
            "control_id": self.control_id,
            "action": self.action,
            "status": if self.acknowledged { "acknowledged" } else { "failed" },
            "source_movement_seq": self.source_movement_seq,
            "thread_id": self.thread_id,
            "turn_id": self.turn_id,
            "detail": self.detail,
        })
    }
}

/// The closure of a tool result that arrives after its turn was interrupted
/// or redirected: the result is recorded, but no model turn consumes it.
pub fn interrupted_result_payload(
    result: &octopus_tool_handlers::ToolResultArtifact,
    source_artifact: &Value,
    thread_id: Option<&str>,
    turn_id: &str,
) -> Value {
    json!({
        "label": INTERRUPTED_RESULT_LABEL,
        "result_kind": "tool_result",
        "correlation_id": result.correlation_id,
        "thread_id": thread_id,
        "turn_id": turn_id,
        "ok": result.ok,
        "turn_interrupted": true,
        "returned_to_model": false,
        "result_disposition": "durably_admitted_after_interrupt",
        "source_artifact": source_artifact,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_decode_strictly() {
        let stop = TurnControl::decode(&json!({"action": "stop", "control_id": "control_1"})).unwrap();
        assert_eq!(stop, TurnControl::Stop { control_id: "control_1".into() });
        assert!(stop.interrupts());
        assert!(!TurnControl::decode(&json!({"action": "resume", "control_id": "c"})).unwrap().interrupts());
        for payload in [
            json!({"action": "stop"}),
            json!({"action": "stop", "control_id": " c"}),
            json!({"action": "stop", "control_id": "c", "turn": "t"}),
            json!({"action": "cancel", "control_id": "c"}),
        ] {
            assert!(TurnControl::decode(&payload).is_err(), "{payload}");
        }
    }

    #[test]
    fn a_control_result_names_its_control_and_movement() {
        let result = ControlResult {
            control_id: "control_1",
            action: "stop",
            acknowledged: true,
            source_movement_seq: 40,
            thread_id: Some("claude-1"),
            turn_id: Some("source-7"),
            detail: json!({"interrupted": true, "exact_result_pending": false}),
        };
        assert_eq!(result.label(), CONTROL_ACKNOWLEDGED_LABEL);
        let payload = result.payload();
        assert_eq!(payload["status"], "acknowledged");
        assert_eq!(payload["source_movement_seq"], 40);
        assert_eq!(payload["turn_id"], "source-7");
        let failed = ControlResult { acknowledged: false, ..result };
        assert_eq!(failed.label(), CONTROL_FAILED_LABEL);
        assert_eq!(failed.payload()["status"], "failed");
    }
}
