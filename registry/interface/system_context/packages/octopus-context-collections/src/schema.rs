use std::collections::BTreeSet;

use octopus_ledger::{ArtifactRef, ContentHash};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{CollectionError, Result};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionDescriptor {
    pub label: String,
    pub id: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub schema_version: u32,
    pub directory: String,
    pub limits: CollectionLimits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionLimits {
    pub max_entries: u32,
    pub max_entry_bytes: u64,
    pub max_page_bytes: u64,
    pub max_scan_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionEntry {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub description: String,
    pub applicability: String,
    pub state: PublicationState,
    pub scope: EntryScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<EntryContent>,
    pub selection: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<Requirement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationState {
    Active,
    Withdrawn,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ee_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_interfaces: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub kind: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeContext {
    pub ee_id: String,
    pub worker_interface: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionSelection {
    pub source: ArtifactRef,
    pub entry_id: String,
    #[serde(with = "hex_hash")]
    pub revision: ContentHash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionCursor {
    #[serde(with = "hex_hash")]
    pub snapshot: ContentHash,
    pub after: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListRequest {
    #[serde(default)]
    pub query: Option<String>,
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<CollectionCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntrySummary {
    pub pin: CollectionSelection,
    pub title: String,
    pub description: String,
    pub applicability: String,
    pub scope: EntryScope,
    pub selection: Value,
    pub requires: Vec<Requirement>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionPage {
    pub source: ArtifactRef,
    #[serde(with = "hex_hash")]
    pub snapshot: ContentHash,
    pub entries: Vec<EntrySummary>,
    pub has_more: bool,
    pub cursor: Option<CollectionCursor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryRead {
    pub pin: CollectionSelection,
    pub entry: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlannedFileWrite {
    pub path: std::path::PathBuf,
    pub content: String,
    pub expected_before_hash: Option<String>,
    pub expected_before_content: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrderedFileWritePlan {
    pub ordered: bool,
    pub writes: Vec<PlannedFileWrite>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublicationPlan {
    pub pin: CollectionSelection,
    pub file_write: OrderedFileWritePlan,
}

pub fn parse_descriptor(value: &Value) -> Result<CollectionDescriptor> {
    let descriptor: CollectionDescriptor = serde_json::from_value(value.clone())?;
    descriptor.validate()?;
    Ok(descriptor)
}

pub fn parse_entry(value: &Value) -> Result<CollectionEntry> {
    if let Some(scope) = value.get("scope").and_then(Value::as_object) {
        for key in ["ee_ids", "worker_interfaces"] {
            if scope.get(key).is_some_and(|value| !value.is_array()) {
                return Err(invalid(format!(
                    "scope.{key} must be an array when present"
                )));
            }
        }
    }
    if let Some(evidence) = value.pointer("/content/evidence").and_then(Value::as_array) {
        for reference in evidence {
            let object = reference
                .as_object()
                .ok_or_else(|| invalid("evidence must contain ArtifactRef objects"))?;
            if object
                .keys()
                .any(|key| !["id", "content"].contains(&key.as_str()))
            {
                return Err(invalid("evidence references accept only id and content"));
            }
        }
    }
    let entry: CollectionEntry = serde_json::from_value(value.clone())?;
    entry.validate()?;
    Ok(entry)
}

pub fn entry_revision(value: &Value) -> Result<ContentHash> {
    parse_entry(value)?;
    Ok(ContentHash::of_canonical_json(value))
}

const ARRAY_SELECTION_FIELDS: &[&str] = &[
    "selected_tools",
    "selected_worker_environment_kinds",
    "external_inputs",
    "external_relays",
    "model_outputs",
    "context_artifact_bindings",
    "payload_artifact_bindings",
];

pub const MAX_SCAN_BYTES: u64 = 64 * 1024 * 1024;

pub fn normalized_selection(value: &Value) -> Result<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("selection must be an object"))?;
    for (key, value) in object {
        if ARRAY_SELECTION_FIELDS.contains(&key.as_str()) {
            if !value.is_array() {
                return Err(invalid(format!("selection.{key} must be an array")));
            }
        } else if key == "door_host_slots" {
            if !value.is_object() {
                return Err(invalid("selection.door_host_slots must be an object"));
            }
        } else {
            return Err(invalid(format!("unknown selection field '{key}'")));
        }
    }
    let mut result: Map<String, Value> = object.clone();
    for key in ARRAY_SELECTION_FIELDS {
        result
            .entry(key.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
    }
    result
        .entry("door_host_slots".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    Ok(Value::Object(result))
}

impl CollectionDescriptor {
    pub fn validate(&self) -> Result<()> {
        safe_id(&self.id)?;
        nonblank(&self.description, "collection.description")?;
        if let Some(text) = &self.text {
            nonblank(text, "collection.text")?;
        }
        if self.label != "context.collection"
            || self.schema_version != 1
            || self.directory != "entries"
        {
            return Err(invalid(
                "collection requires label=context.collection, schema_version=1, directory=entries",
            ));
        }
        if self.limits.max_entries == 0
            || self.limits.max_entry_bytes == 0
            || self.limits.max_page_bytes == 0
            || self.limits.max_scan_bytes == 0
            || self.limits.max_scan_bytes > MAX_SCAN_BYTES
            || self.limits.max_entry_bytes >= isize::MAX as u64
            || self.limits.max_page_bytes >= isize::MAX as u64
            || (self.limits.max_entries as u64)
                .checked_mul(self.limits.max_entry_bytes)
                .is_none()
        {
            return Err(invalid(
                "collection limits must be positive finite bounds, with max_scan_bytes <= 64 MiB",
            ));
        }
        Ok(())
    }
}

impl CollectionEntry {
    pub fn validate(&self) -> Result<()> {
        safe_id(&self.id)?;
        nonblank(&self.title, "entry.title")?;
        nonblank(&self.description, "entry.description")?;
        nonblank(&self.applicability, "entry.applicability")?;
        if self.schema_version != 1 {
            return Err(invalid("entry schema_version must be 1"));
        }
        for (axis, ids) in [("ee_ids", &self.scope.ee_ids), ("worker_interfaces", &self.scope.worker_interfaces)]
            .into_iter()
            .filter_map(|(axis, ids)| ids.as_ref().map(|ids| (axis, ids)))
        {
            if ids.is_empty() {
                return Err(invalid(format!(
                    "scope.{axis} is empty and would hide the entry from every run; omit it to leave that axis unrestricted"
                )));
            }
            let mut seen = BTreeSet::new();
            for id in ids {
                safe_id(id)?;
                if !seen.insert(id) {
                    return Err(invalid("scope repeats an exact id"));
                }
            }
        }
        normalized_selection(&self.selection)?;
        for requirement in &self.requires {
            if ![
                "interface",
                "tool",
                "worker_environment_kind",
                "ee_template",
                "artifact",
                "mark",
                "verifier",
            ]
            .contains(&requirement.kind.as_str())
            {
                return Err(invalid(format!(
                    "unknown registry requirement kind '{}'",
                    requirement.kind
                )));
            }
            safe_id(&requirement.id)?;
        }
        if let Some(content) = &self.content {
            if let Some(text) = &content.text {
                nonblank(text, "content.text")?;
            }
            for evidence in &content.evidence {
                safe_id(evidence.id.as_str())?;
            }
        }
        Ok(())
    }
}

impl EntryScope {
    pub fn permits(&self, scope: &ScopeContext) -> bool {
        self.ee_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&scope.ee_id))
            && self
                .worker_interfaces
                .as_ref()
                .is_none_or(|ids| ids.contains(&scope.worker_interface))
    }
}

pub(crate) fn safe_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.trim() != id
        || matches!(id, "." | "..")
        || id
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err(invalid(format!("invalid exact id '{id}'")));
    }
    Ok(())
}

pub(crate) fn invalid(message: impl Into<String>) -> CollectionError {
    CollectionError::InvalidSchema(message.into())
}

fn nonblank(text: &str, field: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(invalid(format!("{field} must be nonblank")));
    }
    Ok(())
}

mod hex_hash {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        hash: &ContentHash,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&hash.to_hex())
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<ContentHash, D::Error> {
        let text = String::deserialize(deserializer)?;
        ContentHash::from_hex(&text)
            .ok_or_else(|| serde::de::Error::custom("expected lowercase 64-character content hash"))
    }
}
