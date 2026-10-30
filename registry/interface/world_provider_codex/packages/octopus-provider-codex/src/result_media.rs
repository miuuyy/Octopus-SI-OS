use base64::Engine;
use octopus_appserver_proto::DynamicToolCallOutputContentItem;
use octopus_interfaces::{InterfaceArtifactManifest, InterfaceBoundaryError};
use octopus_util_image::{
    PromptImageMode, PromptImageResizeLimits, load_data_url_for_prompt, load_for_prompt_bytes,
};
use serde::Deserialize;
use serde_json::Value;

const MAX_MEDIA_ITEMS: usize = 16;
const MAX_SOURCE_MEDIA_BYTES: usize = 64 * 1024 * 1024;
const MAX_NATIVE_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_NATIVE_OUTPUT_BYTES: usize =
    MAX_SOURCE_MEDIA_BYTES.div_ceil(3) * 4 + MAX_MEDIA_ITEMS * 256 + MAX_NATIVE_TEXT_BYTES;
pub(crate) const RECOVERY_ROLLOUT_MAX_LINE_BYTES: usize = MAX_NATIVE_OUTPUT_BYTES * 2;
const HIGH_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 2048,
    max_patches: 2500,
};
const ORIGINAL_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 6000,
    max_patches: 10_000,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedMedia {
    mime_type: String,
    artifact: InterfaceArtifactManifest,
    payload_base64: String,
}

pub(crate) struct ProjectedMedia {
    pub value: Value,
    pub images: Vec<DynamicToolCallOutputContentItem>,
}

fn invalid(message: impl Into<String>) -> InterfaceBoundaryError {
    InterfaceBoundaryError::Protocol(format!("invalid result media: {}", message.into()))
}

pub(crate) fn project(value: &Value) -> Result<Option<ProjectedMedia>, InterfaceBoundaryError> {
    let Some(media) = value.get("media") else {
        return Ok(None);
    };
    let entries = media
        .as_array()
        .ok_or_else(|| invalid("media must be an array"))?;
    if entries.len() > MAX_MEDIA_ITEMS {
        return Err(invalid(format!(
            "at most {MAX_MEDIA_ITEMS} images are supported"
        )));
    }
    let mut total_bytes = 0usize;
    let mut image_url_bytes = 0usize;
    let mut summaries = Vec::with_capacity(entries.len());
    let mut images = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let encoded = entry
            .get("payload_base64")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(format!("entry {index} requires embedded payload_base64")))?;
        let remaining = MAX_SOURCE_MEDIA_BYTES - total_bytes;
        if encoded.len() > remaining.div_ceil(3) * 4 {
            return Err(invalid(format!(
                "total decoded images exceed {MAX_SOURCE_MEDIA_BYTES} bytes"
            )));
        }
        let media: EmbeddedMedia = serde_json::from_value(entry.clone())
            .map_err(|error| invalid(format!("entry {index}: {error}")))?;
        if !matches!(
            media.mime_type.as_str(),
            "image/png" | "image/jpeg" | "image/webp"
        ) {
            return Err(invalid(format!(
                "unsupported MIME type '{}'",
                media.mime_type
            )));
        }
        if media.artifact.label.is_empty()
            || media.artifact.label.trim() != media.artifact.label
            || !media.artifact.payload_path.is_absolute()
        {
            return Err(invalid(format!(
                "entry {index} requires a canonical artifact manifest"
            )));
        }
        let expected = media
            .artifact
            .payload_content_hash
            .as_deref()
            .ok_or_else(|| invalid(format!("entry {index} requires a payload_content_hash")))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&media.payload_base64)
            .map_err(|error| invalid(format!("entry {index} has invalid base64: {error}")))?;
        if bytes.is_empty() || bytes.len() > remaining {
            return Err(invalid(format!(
                "entry {index} has an empty or oversized payload"
            )));
        }
        let actual = blake3::hash(&bytes).to_hex();
        if expected != actual.as_str() {
            return Err(invalid(format!("entry {index} payload hash mismatch")));
        }
        let supported_header = match media.mime_type.as_str() {
            "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
            _ => false,
        };
        if !supported_header {
            return Err(invalid(format!(
                "entry {index} image header differs from its MIME type"
            )));
        }
        total_bytes += bytes.len();
        let payload_bytes = bytes.len();
        let image = load_for_prompt_bytes(
            &media.artifact.payload_path,
            bytes,
            PromptImageMode::Original,
        )
        .map_err(|error| invalid(format!("entry {index} cannot be decoded: {error}")))?;
        if image.mime != media.mime_type {
            return Err(invalid(format!(
                "entry {index} decoded format differs from its MIME type"
            )));
        }
        let source_image = serde_json::json!({
            "width": image.width,
            "height": image.height,
            "mime_type": image.mime,
        });
        let image_url = image.into_data_url();
        image_url_bytes = image_url_bytes
            .checked_add(image_url.len())
            .filter(|size| *size <= MAX_NATIVE_OUTPUT_BYTES)
            .ok_or_else(|| invalid("source images exceed the recoverable native output limit"))?;
        summaries.push(serde_json::json!({
            "mime_type": media.mime_type,
            "artifact": media.artifact,
            "payload_bytes": payload_bytes,
            "source_image": source_image,
        }));
        images.push(DynamicToolCallOutputContentItem::InputImage { image_url });
    }
    let mut value = value.clone();
    value["media"] = Value::Array(summaries);
    Ok(Some(ProjectedMedia { value, images }))
}

pub(crate) fn without_embedded_payloads(value: &Value) -> Value {
    let mut value = value.clone();
    if let Some(entries) = value.get_mut("media").and_then(Value::as_array_mut) {
        for entry in entries.iter_mut().filter_map(Value::as_object_mut) {
            entry.remove("payload_base64");
        }
    }
    value
}

pub(crate) fn text_exceeds_limit(text: &str) -> Result<bool, InterfaceBoundaryError> {
    let item = DynamicToolCallOutputContentItem::InputText { text: text.into() };
    let encoded = serde_json::to_vec(&item)
        .map_err(|error| invalid(format!("native text cannot be encoded: {error}")))?;
    Ok(encoded.len() > MAX_NATIVE_TEXT_BYTES)
}

pub(crate) fn validate_content(
    content: &[DynamicToolCallOutputContentItem],
) -> Result<(), InterfaceBoundaryError> {
    let mut text_bytes = 0usize;
    for item in content {
        if let DynamicToolCallOutputContentItem::InputText { .. } = item {
            let encoded = serde_json::to_vec(item)
                .map_err(|error| invalid(format!("native text cannot be encoded: {error}")))?;
            text_bytes = text_bytes
                .checked_add(encoded.len())
                .filter(|size| *size <= MAX_NATIVE_TEXT_BYTES)
                .ok_or_else(|| invalid("serialized native text exceeds its output limit"))?;
        }
    }
    let native = content
        .iter()
        .cloned()
        .map(|item| {
            let item: octopus_protocol::dynamic_tools::DynamicToolCallOutputContentItem =
                item.into();
            octopus_protocol::models::FunctionCallOutputContentItem::from(item)
        })
        .collect::<Vec<_>>();
    let encoded = serde_json::to_vec(&native)
        .map_err(|error| invalid(format!("native output cannot be encoded: {error}")))?;
    if encoded.len() > MAX_NATIVE_OUTPUT_BYTES {
        return Err(invalid(format!(
            "serialized native output exceeds {MAX_NATIVE_OUTPUT_BYTES} bytes"
        )));
    }
    Ok(())
}

pub(crate) fn matches_recorded_output(expected: &Value, recorded: &Value) -> bool {
    let (Some(expected_items), Some(recorded_items)) = (expected.as_array(), recorded.as_array())
    else {
        return expected == recorded;
    };
    expected_items.len() == recorded_items.len()
        && expected_items
            .iter()
            .zip(recorded_items)
            .all(|(expected, recorded)| {
                if expected.get("type").and_then(Value::as_str) != Some("input_image") {
                    return expected == recorded;
                }
                if expected.get("detail").and_then(Value::as_str) != Some("high")
                    || !matches!(
                        recorded.get("detail").and_then(Value::as_str),
                        Some("high" | "original")
                    )
                {
                    return false;
                }
                let Some(image_url) = expected.get("image_url").and_then(Value::as_str) else {
                    return false;
                };
                let limits = match recorded.get("detail").and_then(Value::as_str) {
                    Some("original") => ORIGINAL_DETAIL_LIMITS,
                    Some("high") => HIGH_DETAIL_LIMITS,
                    _ => return false,
                };
                let Ok(prepared) =
                    load_data_url_for_prompt(image_url, PromptImageMode::ResizeWithLimits(limits))
                else {
                    return false;
                };
                let mut prepared_item = expected.clone();
                prepared_item["detail"] = recorded["detail"].clone();
                prepared_item["image_url"] = Value::String(prepared.into_data_url());
                &prepared_item == recorded
            })
}
