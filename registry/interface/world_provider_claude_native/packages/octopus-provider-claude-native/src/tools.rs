use std::collections::{BTreeMap, BTreeSet};

use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_run_authoring::ProviderOutputHandoff;
use octopus_tool_handlers::ToolResultArtifact;
use serde_json::Value;

use crate::child::ToolSpec;

const MAX_MODEL_TOOL_NAME: usize = 64;
const MAX_RESULT_TEXT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCatalog {
    pub specs: Vec<ToolSpec>,
    pub tools_by_model_name: BTreeMap<String, String>,
}

impl ToolCatalog {
    pub fn fingerprint_material(&self) -> String {
        serde_json::to_string(&self.specs.iter().map(|spec| (&spec.name, &spec.description, &spec.input_schema)).collect::<Vec<_>>())
            .unwrap_or_default()
    }
}

pub fn admitted_catalog(
    handoff: &ProviderOutputHandoff,
    envelope: &InterfaceInvocationEnvelope,
    hidden: &[String],
) -> Result<ToolCatalog, String> {
    let callable = handoff.callable_tool_names(envelope)?;
    let affordances = envelope.interface_affordance_specs().map_err(|error| error.to_string())?;
    let mut visible = BTreeSet::new();
    for affordance in &affordances {
        let tool = affordance.name.as_str();
        if !callable.contains(tool) || hidden.iter().any(|name| name == tool) {
            continue;
        }
        if !visible.insert(tool.to_string()) {
            return Err(format!("formed model affordances repeat tool '{tool}'"));
        }
    }
    handoff.validate_model_tool_routes(&visible)?;
    let mut catalog = ToolCatalog::default();
    for row in octopus_model_context_render::model_visible_affordances(affordances, hidden)? {
        let tool = row.get("name").and_then(Value::as_str).ok_or("model affordance has no name")?;
        if !visible.contains(tool) {
            continue;
        }
        let model_name = encode_tool_name(tool)?;
        if catalog.tools_by_model_name.insert(model_name.clone(), tool.to_string()).is_some() {
            return Err(format!("model tool name '{model_name}' is not unique"));
        }
        catalog.specs.push(ToolSpec {
            name: model_name,
            description: row.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
            input_schema: row.get("input_schema").cloned().unwrap_or_else(|| serde_json::json!({"type": "object"})),
        });
    }
    Ok(catalog)
}

pub fn encode_tool_name(tool: &str) -> Result<String, String> {
    let mut encoded = String::with_capacity(tool.len());
    for character in tool.chars() {
        match character {
            '-' => encoded.push_str("-h"),
            '.' => encoded.push_str("-d"),
            character if character.is_ascii_alphanumeric() || character == '_' => encoded.push(character),
            other => return Err(format!("tool '{tool}' contains '{other}', which has no model-name encoding")),
        }
    }
    if encoded.is_empty() || encoded.len() > MAX_MODEL_TOOL_NAME {
        return Err(format!("tool '{tool}' does not fit a {MAX_MODEL_TOOL_NAME}-character model tool name"));
    }
    Ok(encoded)
}

pub fn decode_tool_result(payload: &Value) -> Result<Option<ToolResultArtifact>, String> {
    let Some(object) = payload.as_object() else {
        return Ok(None);
    };
    if !(object.contains_key("label") && object.contains_key("tool") && object.contains_key("ok")) {
        return Ok(None);
    }
    let result: ToolResultArtifact = serde_json::from_value(payload.clone())
        .map_err(|error| format!("tool result artifact is not a strict result envelope: {error}"))?;
    result.validate()?;
    Ok(Some(result))
}

pub fn model_result_text(result: &ToolResultArtifact) -> String {
    let value = if result.ok {
        result.body.clone().unwrap_or(Value::Null)
    } else {
        serde_json::to_value(&result.error).unwrap_or(Value::Null)
    };
    let text = match &value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    if text.len() <= MAX_RESULT_TEXT_BYTES {
        return if text.is_empty() { "(empty result)".into() } else { text };
    }
    let mut end = MAX_RESULT_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated: {} of {} bytes shown]", &text[..end], end, text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names_round_trip_and_reject_what_they_cannot_encode() {
        assert_eq!(encode_tool_name("file.read").unwrap(), "file-dread");
        assert_eq!(encode_tool_name("code-mode.run_js").unwrap(), "code-hmode-drun_js");
        assert!(encode_tool_name("tool with space").is_err());
        assert!(encode_tool_name(&"a".repeat(65)).is_err());
    }
}
