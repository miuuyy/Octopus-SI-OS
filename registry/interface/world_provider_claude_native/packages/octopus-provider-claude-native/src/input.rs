use serde_json::{Map, Value, json};

use crate::config::{InputMessage, RequestContextKind};

/// One declared input artifact, decoded into the text Claude receives.
///
/// The rules follow the shared `input_messages` contract that the Codex
/// provider also reads: text comes from the first declared text field that
/// carries text; a required identity must be present and well formed; every
/// declared context field must be present and not null; every request
/// context field must exist with its declared JSON type. When a contract
/// declares context, the whole payload is handed to the model as one
/// `input_message_context` block, so ids such as a voice delegation's
/// call_id, correlation_id and delegation_id reach the model exactly as
/// admitted.
#[derive(Debug, PartialEq, Eq)]
pub struct DecodedInput {
    pub identity: Option<String>,
    pub context_block: Option<String>,
    pub text: String,
}

impl DecodedInput {
    pub fn prompt_text(&self) -> String {
        match &self.context_block {
            Some(block) if block != &self.text => format!("{block}\n\n{}", self.text),
            _ => self.text.clone(),
        }
    }
}

pub fn decode(label: &str, contract: &InputMessage, payload: &Value) -> Result<DecodedInput, String> {
    let invalid = |detail: String| format!("declared input message '{label}' {detail}");
    let object = payload.as_object().ok_or_else(|| invalid("must be an object".into()))?;
    let text = declared_text(object, contract).map_err(invalid)?;
    let mut has_request_context = false;
    for (field, kind) in &contract.request_context_fields {
        let value = object
            .get(field)
            .ok_or_else(|| invalid(format!("has no request context field '{field}'")))?;
        let nonempty = match kind {
            RequestContextKind::Array => value.as_array().map(|items| !items.is_empty()),
            RequestContextKind::Object => value.as_object().map(|items| !items.is_empty()),
        }
        .ok_or_else(|| invalid(format!("request context field '{field}' has the wrong JSON type")))?;
        has_request_context |= nonempty;
    }
    if let Some(field) = &contract.attachments_field {
        if object.get(field).is_some_and(|value| !value.is_null() && !value.is_array()) {
            return Err(invalid(format!("field '{field}' must be an array")));
        }
    }
    let identity = match contract.identity_field.as_ref().and_then(|field| object.get(field)) {
        Some(Value::String(identity)) => {
            if identity.trim().is_empty() || identity.trim() != identity || identity.len() > 256 {
                return Err(invalid("has an empty, untrimmed or oversized identity".into()));
            }
            Some(identity.clone())
        }
        Some(Value::Null) | None if !contract.identity_required => None,
        Some(Value::Null) | None => return Err(invalid("has no declared identity".into())),
        Some(_) => return Err(invalid("identity must be text".into())),
    };
    for field in &contract.context_fields {
        if object.get(field).is_none_or(Value::is_null) {
            return Err(invalid(format!("has no context field '{field}'")));
        }
    }
    let context_block = (!contract.context_fields.is_empty() || !contract.request_context_fields.is_empty())
        .then(|| serde_json::to_string(&json!({"input_message_context": {"label": label, "fields": object}})))
        .transpose()
        .map_err(|error| invalid(error.to_string()))?;
    let text = match (text, &context_block) {
        (Some(text), _) => text,
        (None, Some(block)) if has_request_context => block.clone(),
        _ => return Err(invalid("carries no text in its declared text fields".into())),
    };
    Ok(DecodedInput { identity, context_block, text })
}

fn declared_text(object: &Map<String, Value>, contract: &InputMessage) -> Result<Option<String>, String> {
    let mut text: Option<&str> = None;
    for field in &contract.text_fields {
        match object.get(field) {
            None | Some(Value::Null) => {}
            Some(Value::String(value)) if value.trim().is_empty() => {}
            Some(Value::String(value)) => {
                if text.is_some_and(|previous| previous != value) {
                    return Err("has conflicting text fields".into());
                }
                text = Some(value);
            }
            Some(_) => return Err(format!("field '{field}' must be text")),
        }
    }
    Ok(text.map(str::to_owned))
}

/// The text of a turn started by an admitted artifact that no
/// `input_messages` entry declares, by the rule the Codex provider applies:
/// a payload that carries `text`, `prompt` or `task` text is that text;
/// any other payload reaches the model whole, under its label and source.
pub fn artifact_turn_text(label: &str, source_interface: Option<&str>, payload: &Value) -> Result<String, String> {
    if let Some(object) = payload.as_object() {
        for field in ["text", "prompt", "task"] {
            if let Some(text) = object.get(field).and_then(Value::as_str).filter(|text| !text.trim().is_empty()) {
                return Ok(text.to_string());
            }
        }
    }
    serde_json::to_string_pretty(&json!({"artifact": {"label": label, "source_interface": source_interface, "payload": payload}}))
        .map_err(|error| format!("input artifact '{label}' could not be rendered: {error}"))
}
