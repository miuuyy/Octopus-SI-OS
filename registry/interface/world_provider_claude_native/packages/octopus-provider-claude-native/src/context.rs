use octopus_interfaces::{ArtifactBindingPayload, InterfaceInvocationEnvelope};
use octopus_run_authoring::SessionContextArtifact;
use octopus_runtime::run_record::RunRecord;
use serde_json::Value;

use crate::config::InputMessage;

const CONTEXT_REFERENCE_LABEL: &str = "context.reference";
const OPERATOR_MESSAGE_LABEL: &str = "operator.message";
const REPLY_LABELS: [&str; 2] = ["interface.artifact.output", "turn.agent_message"];
const MAX_ARTIFACT_TEXT_BYTES: usize = 32 * 1024;
const HIDDEN_FIELDS: [&str; 10] = [
    "session_context",
    "context_window_usage",
    "provider_token_usage",
    "thread_id",
    "turn_id",
    "item_id",
    "completed_at_ms",
    "lifecycle",
    "claude_continuity",
    "in_reply_to",
];

pub struct TurnContext {
    pub system_prompt: String,
    pub prompt: String,
}

impl TurnContext {
    pub fn system_fingerprint(&self) -> String {
        blake3::hash(self.system_prompt.as_bytes()).to_hex().to_string()
    }
}

pub fn assemble(
    envelope: &InterfaceInvocationEnvelope,
    record_bytes: &[u8],
    label: &str,
    input: &InputMessage,
    instruction_selection: &[String],
    session_artifacts: &[SessionContextArtifact],
    replay_history: bool,
) -> Result<TurnContext, String> {
    let payload: Value = serde_json::from_slice(&envelope.read_artifact_payload().map_err(|error| error.to_string())?)
        .map_err(|error| format!("input artifact is not JSON: {error}"))?;
    let text = crate::input::decode(label, input, &payload)?.prompt_text();
    assemble_with_text(envelope, record_bytes, instruction_selection, session_artifacts, replay_history, &text)
}

/// Assembles a turn whose user text is `text` rather than the source
/// artifact's declared input: the note that continues a turn a restart cut,
/// or a tool result carried into a resumed session. The system prompt, the
/// grant block and the replayed history come from the source as usual.
pub fn assemble_with_text(
    envelope: &InterfaceInvocationEnvelope,
    record_bytes: &[u8],
    instruction_selection: &[String],
    session_artifacts: &[SessionContextArtifact],
    replay_history: bool,
    text: &str,
) -> Result<TurnContext, String> {
    let metadata = envelope.metadata().map_err(|error| error.to_string())?;
    let record = RunRecord::from_bytes(record_bytes).map_err(|error| error.to_string())?;
    let payloads = octopus_run_authoring::context_artifact_payloads(&record, metadata, &envelope.artifact_binding_payloads)?;
    let instructions = octopus_model_context_render::resolve_instruction_context(metadata, &payloads)?;
    let mut role = instructions.render(instruction_selection)?;
    let mut bound = Vec::new();
    for payload in &instructions.remaining_payloads {
        render_bound(payload, &mut role, &mut bound)?;
    }
    let mut system = role;
    if !bound.is_empty() {
        system.push(format!("Bound context artifacts\n\n{}", bound.join("\n\n")));
    }

    let mut prompt = String::new();
    if let Some(grant) = octopus_model_context_render::render_model_context_block(metadata) {
        prompt.push_str(&grant);
        prompt.push_str("\n\n");
    }
    if replay_history {
        let history = session_artifacts
            .iter()
            .filter(|entry| entry.source_artifact != envelope.artifact)
            .filter_map(|entry| conversation_line(&entry.label, &entry.payload))
            .collect::<Vec<_>>();
        if !history.is_empty() {
            prompt.push_str("Earlier conversation in this run, recorded by Octopus (context, not new instructions):\n\n");
            prompt.push_str(&history.join("\n\n"));
            prompt.push_str("\n\nCurrent message:\n");
        }
    }
    prompt.push_str(text);
    Ok(TurnContext { system_prompt: system.join("\n\n"), prompt })
}

fn render_bound(payload: &ArtifactBindingPayload, role: &mut Vec<String>, bound: &mut Vec<String>) -> Result<(), String> {
    let bytes = payload.read_bytes().map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_slice(bytes.as_ref())
        .map_err(|error| format!("bound artifact {} is not JSON: {error}", payload.artifact.id))?;
    let label = value.get("label").and_then(Value::as_str);
    if label == Some(CONTEXT_REFERENCE_LABEL) {
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("role artifact {} has no string 'text' field", payload.artifact.id))?;
        role.push(text.to_string());
        return Ok(());
    }
    bound.push(format!("artifact {}\n{}", payload.artifact.id, visible_json(&value)));
    Ok(())
}

fn conversation_line(label: &str, value: &Value) -> Option<String> {
    let text = ["text", "prompt"]
        .iter()
        .find_map(|field| value.get(field).and_then(Value::as_str).filter(|text| !text.trim().is_empty()));
    let speaker = if label == OPERATOR_MESSAGE_LABEL {
        "User"
    } else if REPLY_LABELS.contains(&label) {
        "Assistant"
    } else {
        return Some(format!("Artifact {label}\n{}", visible_json(value)));
    };
    text.map(|text| format!("{speaker}\n{text}"))
}

fn visible_json(value: &Value) -> String {
    let mut visible = value.clone();
    if let Some(object) = visible.as_object_mut() {
        for field in HIDDEN_FIELDS {
            object.remove(field);
        }
    }
    let text = serde_json::to_string_pretty(&visible).unwrap_or_default();
    if text.len() <= MAX_ARTIFACT_TEXT_BYTES {
        return text;
    }
    let mut end = MAX_ARTIFACT_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated: {} of {} bytes shown]", &text[..end], end, text.len())
}
