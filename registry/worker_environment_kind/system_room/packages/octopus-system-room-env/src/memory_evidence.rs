use std::io::Read;
use std::path::Path;

use octopus_ledger::{ArtifactRef, ContentHash};
use serde_json::{Value, json};

pub(crate) const DEFAULT_PREVIEW_BYTES: usize = 2048;
pub(crate) const MAX_PREVIEW_BYTES: usize = 8192;
const MIN_PREVIEW_BYTES: usize = 16;
const MAX_EVIDENCE: usize = 16;
const MAX_BLOB_BYTES: u64 = 1024 * 1024;
const MAX_FIELD_CHARS: usize = 256;
const MAX_ORIGIN_BYTES: usize = 1024;
const CALL_LABEL: &str = "tool.call";
const DESCRIBED_MEMBERS: [&str; 4] = ["arguments", "tool", "correlation_id", "label"];

pub(crate) fn preview_bound(requested: Option<usize>) -> Result<usize, String> {
    match requested {
        None => Ok(DEFAULT_PREVIEW_BYTES),
        Some(bytes) if (MIN_PREVIEW_BYTES..=MAX_PREVIEW_BYTES).contains(&bytes) => Ok(bytes),
        Some(_) => Err(format!(
            "max_bytes must be between {MIN_PREVIEW_BYTES} and {MAX_PREVIEW_BYTES}"
        )),
    }
}

pub(crate) fn describe(blobs: Option<&Path>, note: Option<&str>, evidence: &[ArtifactRef], preview_bytes: usize) -> Value {
    let artifacts = evidence
        .iter()
        .take(MAX_EVIDENCE)
        .map(|artifact| {
            let mut row = json!({
                "artifact": artifact,
                "label": artifact.id.as_str().rsplit_once(':').map(|(label, _)| label),
            });
            let outcome = match resolve(blobs, note, artifact, preview_bytes) {
                Ok(found) => found,
                Err((status, reason)) => json!({"status": status, "reason": reason}),
            };
            for (key, value) in outcome.as_object().into_iter().flatten() {
                row[key.as_str()] = value.clone();
            }
            row
        })
        .collect::<Vec<_>>();
    json!({
        "artifacts": artifacts,
        "omitted": evidence.len().saturating_sub(MAX_EVIDENCE),
    })
}

type Refusal = (&'static str, String);

fn unavailable(reason: impl Into<String>) -> Refusal {
    ("unavailable", reason.into())
}

fn withheld(reason: impl Into<String>) -> Refusal {
    ("withheld", reason.into())
}

fn resolve(blobs: Option<&Path>, note: Option<&str>, artifact: &ArtifactRef, preview_bytes: usize) -> Result<Value, Refusal> {
    let blobs = blobs.ok_or_else(|| unavailable("this call holds no read grant on the artifact blob store"))?;
    let note = note.ok_or_else(|| withheld("the entry carries no note text to match a saving call against"))?;
    let manifest_bytes = blob(blobs, &artifact.content)?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| withheld("the recorded artifact is not a call manifest"))?;
    let payload_hash = manifest
        .get("payload_content_hash")
        .and_then(Value::as_str)
        .and_then(ContentHash::from_hex);
    let (Some(CALL_LABEL), Some(payload_hash)) = (manifest.get("label").and_then(Value::as_str), payload_hash) else {
        return Err(withheld("the recorded artifact is not a call manifest"));
    };
    let payload = blob(blobs, &payload_hash)?;
    let call: Value = serde_json::from_slice(&payload)
        .map_err(|_| withheld("the recorded call carries no readable arguments"))?;
    if call["arguments"]["note"].as_str().map(str::trim) != Some(note) {
        return Err(withheld("the recorded artifact is not the call that saved this note"));
    }
    let preview = prefix(&payload, preview_bytes)
        .ok_or_else(|| unavailable("the recorded call is not UTF-8 text"))?;
    let field = |name: &str| {
        call.get(name)
            .and_then(Value::as_str)
            .map(|text| text.chars().take(MAX_FIELD_CHARS).collect::<String>())
    };
    let origin = call
        .as_object()
        .map(|members| {
            members
                .iter()
                .filter(|(name, _)| !DESCRIBED_MEMBERS.contains(&name.as_str()))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect::<serde_json::Map<_, _>>()
        })
        .filter(|members| !members.is_empty())
        .map(|members| Value::Object(members).to_string());
    let origin = origin.as_deref().and_then(|text| prefix(text.as_bytes(), MAX_ORIGIN_BYTES));
    Ok(json!({
        "status": "available",
        "reason": null,
        "tool": field("tool"),
        "correlation_id": field("correlation_id"),
        "origin": origin,
        "payload": {"content_hash": payload_hash.to_hex(), "bytes": payload.len()},
        "preview": preview,
        "preview_bytes": preview.len(),
        "truncated": preview.len() < payload.len(),
    }))
}

fn blob(blobs: &Path, hash: &ContentHash) -> Result<Vec<u8>, Refusal> {
    let path = blobs.join(hash.to_hex());
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(unavailable("the blob store does not hold the recorded artifact"));
        }
        Err(error) => return Err(unavailable(format!("the recorded artifact cannot be read: {}", error.kind()))),
    };
    if !metadata.is_file() {
        return Err(unavailable("the recorded artifact is not a regular blob"));
    }
    if metadata.len() > MAX_BLOB_BYTES {
        return Err(unavailable(format!("the recorded artifact exceeds {MAX_BLOB_BYTES} bytes")));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_BLOB_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| unavailable(format!("the recorded artifact cannot be read: {}", error.kind())))?;
    if bytes.len() as u64 > MAX_BLOB_BYTES || ContentHash::of_bytes(&bytes) != *hash {
        return Err(unavailable("the stored blob differs from the recorded content"));
    }
    Ok(bytes)
}

fn prefix(bytes: &[u8], limit: usize) -> Option<&str> {
    let cut = &bytes[..bytes.len().min(limit)];
    match std::str::from_utf8(cut) {
        Ok(text) => Some(text),
        Err(error) if error.error_len().is_none() => std::str::from_utf8(&cut[..error.valid_up_to()]).ok(),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_ends_on_a_character_boundary() {
        let text = "aż€𝄞";
        for limit in 0..=text.len() + 2 {
            let cut = prefix(text.as_bytes(), limit).unwrap();
            assert!(cut.len() <= limit && text.starts_with(cut), "{limit}: {cut:?}");
            assert!(limit < text.len() || cut == text);
        }
        assert_eq!(prefix(text.as_bytes(), 3), Some("aż"));
        assert_eq!(prefix(text.as_bytes(), 5), Some("aż"));
        assert_eq!(prefix(&[b'a', 0xff, b'b'], 3), None);
    }

    #[test]
    fn the_preview_bound_has_a_default_and_a_ceiling() {
        assert_eq!(preview_bound(None), Ok(DEFAULT_PREVIEW_BYTES));
        assert_eq!(preview_bound(Some(MAX_PREVIEW_BYTES)), Ok(MAX_PREVIEW_BYTES));
        assert!(preview_bound(Some(MAX_PREVIEW_BYTES + 1)).is_err());
        assert!(preview_bound(Some(0)).is_err());
    }
}
