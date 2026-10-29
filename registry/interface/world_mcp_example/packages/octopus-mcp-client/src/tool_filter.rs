use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{json, Value};

const MAX_FILTER_VALUES: usize = 64;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolMetaFilterDocument {
    meta_key: String,
    values: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolMetaFilter {
    meta_key: String,
    values: BTreeSet<String>,
}

impl ToolMetaFilter {
    pub(crate) fn parse(value: &Value) -> Result<Self, String> {
        let document = serde_json::from_value::<ToolMetaFilterDocument>(value.clone())
            .map_err(|error| format!("private_config.tool_filter is malformed: {error}"))?;
        if document.meta_key.is_empty() || document.meta_key.trim() != document.meta_key {
            return Err("private_config.tool_filter.meta_key must be non-empty and trimmed".into());
        }
        if document.values.is_empty() || document.values.len() > MAX_FILTER_VALUES {
            return Err(format!(
                "private_config.tool_filter.values must contain 1..={MAX_FILTER_VALUES} entries"
            ));
        }
        let mut values = BTreeSet::new();
        for value in document.values {
            if value.is_empty() || value.trim() != value || !values.insert(value.clone()) {
                return Err(format!(
                    "private_config.tool_filter.values entry '{value}' is empty, untrimmed or repeated"
                ));
            }
        }
        Ok(Self {
            meta_key: document.meta_key,
            values,
        })
    }

    pub(crate) fn admits(&self, tool: &Value) -> bool {
        tool.get("_meta")
            .and_then(|meta| meta.get(&self.meta_key))
            .and_then(Value::as_str)
            .is_some_and(|value| self.values.contains(value))
    }

    pub(crate) fn as_value(&self) -> Value {
        json!({ "meta_key": self.meta_key, "values": self.values })
    }
}
