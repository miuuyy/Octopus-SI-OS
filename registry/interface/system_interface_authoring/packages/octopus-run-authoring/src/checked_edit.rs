use super::*;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedRunEdit {
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "octopus_file_write_contract::optional_json_format")]
    pub json_format: Option<JsonFormat>,
    pub path: PathBuf,
    pub before_hash: String,
    pub after_hash: String,
    pub correlation_id: String,
    pub content: String,
    pub patch: json_patch::Patch,
}

pub use crate::authoring_retry::checked_edit_verification_failure;
pub use crate::authoring_retry::checked_failed_run_write;

impl CheckedRunEdit {
    pub fn arguments(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(FileWriteCallArguments {
            path: self.path.clone(), patch: self.patch.clone(),
            expected_before_hash: self.before_hash.clone(), expected_after_hash: self.after_hash.clone(),
            json_format: self.json_format,
        }).map_err(|error| error.to_string())
    }
    pub fn prepare(
        path: PathBuf,
        current: &[u8],
        prospective: serde_json::Value,
        correlation_id: String,
    ) -> Result<Self, String> {
        Self::prepare_with_format(path, current, prospective, correlation_id, Some(JsonFormat::Compact))
    }

    pub fn prepare_with_format(
        path: PathBuf, current: &[u8], prospective: serde_json::Value,
        correlation_id: String, json_format: Option<JsonFormat>,
    ) -> Result<Self, String> {
        if !path.is_absolute() || correlation_id.is_empty() {
            return Err("checked run edit requires an absolute path and exact correlation".into());
        }
        let before = RunRecord::from_bytes(current).map_err(|e| e.to_string())?;
        let bytes = json_format.unwrap_or_default().encode(&prospective)?;
        let after = RunRecord::from_bytes(&bytes).map_err(|e| e.to_string())?;
        if before.id != after.id
            || before.ee != after.ee
            || before.worker != after.worker
            || before.trace != after.trace
        {
            return Err("checked run edit changed the original run identity".into());
        }
        let previous = serde_json::from_slice(current).map_err(|e| e.to_string())?;
        let patch = json_patch::diff(&previous, &prospective);
        let after_hash = ContentHash::of_bytes(&bytes).to_hex();
        let content = apply_run_json_patch(current, &patch, &after_hash, json_format)?;
        if content.as_bytes() != bytes {
            return Err("checked run edit did not reconstruct exact prospective bytes".into());
        }
        Ok(Self {
            json_format,
            path,
            before_hash: ContentHash::of_bytes(current).to_hex(),
            after_hash,
            correlation_id,
            content,
            patch,
        })
    }

    pub fn materialize(
        &self,
        root: &Path,
        profile: &RunAuthoringProfile,
    ) -> Result<InterfaceArtifactFile, String> {
        if profile.file_write_tool.is_empty()
            || profile.file_write_label.is_empty()
            || profile.file_write_result_label.is_empty()
        {
            return Err("checked file-write profile is incomplete".into());
        }
        let bytes = serde_json::to_vec(&serde_json::json!({
            "tool":profile.file_write_tool,"correlation_id":self.correlation_id,
            "arguments":self.arguments()?
        })).map_err(|e| e.to_string())?;
        InterfaceArtifactFile::write(root, &profile.file_write_label, &bytes)
            .map_err(|e| e.to_string())
    }

    pub fn validate_result(
        &self,
        profile: &RunAuthoringProfile,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        call_seq: u64,
        source: &InterfaceId,
        target: &InterfaceId,
        checkpoint: &CheckpointId,
        installed: &[u8],
    ) -> Result<(), String> {
        let record = RunRecord::from_bytes(self.content.as_bytes()).map_err(|e| e.to_string())?;
        validate_unchanged_admitted_checkpoint(self.content.as_bytes(), installed, checkpoint)?;
        let expected = record
            .checkpoint(checkpoint)
            .ok_or("checked authoring result checkpoint is absent")?;
        let runtime = envelope.runtime().map_err(|e| e.to_string())?;
        let metadata = envelope.metadata().map_err(|e| e.to_string())?;
        let snapshot = CheckpointBindingPolicySnapshot {
            inbound: expected
                .interface_binding
                .clone()
                .ok_or("authoring result has no inbound binding")?,
            outbound: expected.outbound_interface_bindings.clone(),
        };
        if movement.kind != MovementKind::InterfaceOutbound
            || movement.seq.0 <= call_seq
            || movement.from != Endpoint::Interface(source.clone())
            || movement.to != Endpoint::Interface(target.clone())
            || movement.artifact.as_ref() != Some(&envelope.artifact)
            || movement
                .invocation_metadata
                .as_ref()
                .and_then(|m| m.invocation())
                != Some(metadata)
            || runtime.run != record.id
            || runtime.ee != RunRecord::from_bytes(installed).map_err(|e| e.to_string())?.ee
            || runtime.worker != record.worker
            || runtime.interface != *target
            || runtime.checkpoint != *checkpoint
            || runtime.epoch != expected.epoch.0
            || runtime.run_record_hash.as_deref() != Some(ContentHash::of_bytes(installed).to_hex().as_str())
            || metadata.checkpoint_binding_policy.as_ref() != Some(&snapshot)
            || metadata.outbound_interface_bindings
                != expected
                    .outbound_interface_bindings
                    .iter()
                    .map(|b| b.interface.clone())
                    .collect::<Vec<_>>()
            || metadata.required_verified_artifacts != expected.required_verified_artifacts
            || !artifact_refs_match_bindings(
                &metadata.artifact_binding_refs,
                &expected.artifact_bindings,
            )
            || metadata.verification_failure.is_some()
            || !metadata
                .checkpoint_admission
                .as_ref()
                .is_some_and(|a| a.complete && a.artifact_label == profile.file_write_result_label)
        {
            return Err(
                "checked file-write result differs from its exact installed prospective snapshot"
                    .into(),
            );
        }
        let captured = envelope
            .read_artifact_bounded(1024 * 1024, 8 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let result: ToolResultArtifact =
            serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
        if captured.manifest.label != profile.file_write_result_label
            || result.correlation_id.as_deref() != Some(self.correlation_id.as_str())
        {
            return Err("checked file-write result has another label or correlation".into());
        }
        validate_file_write_evidence(
            profile,
            &self.path,
            result,
            &self.before_hash,
            &self.after_hash,
            self.content.len(),
        )
    }
}

pub(crate) fn validate_file_write_evidence(
    profile: &RunAuthoringProfile,
    path: &Path,
    result: ToolResultArtifact,
    before: &str,
    after: &str,
    bytes: usize,
) -> Result<(), String> {
    if result.label != profile.file_write_result_label || result.tool != profile.file_write_tool {
        return Err(format!("checked file.write returned another tool's result: {} / {}", result.tool, result.label));
    }
    if let Some(error) = result.error.as_ref() {
        return Err(format!("checked file.write failed: {}: {}", error.code, error.message));
    }
    if !result.ok {
        return Err("checked file.write returned a failed result without an error".into());
    }
    let body: FileWriteResultBody =
        serde_json::from_value(result.body.ok_or("checked file.write result has no body")?)
            .map_err(|e| e.to_string())?;
    if body.result != "files_written" || body.writes.len() != 1 {
        return Err("checked file.write did not report one file".into());
    }
    let evidence = &body.writes[0];
    if evidence.path != path
        || evidence.before_hash.as_deref() != Some(before)
        || evidence.after_hash != after
        || evidence.bytes != bytes as u64
    {
        return Err("checked file.write evidence differs from its exact patch".into());
    }
    Ok(())
}

pub(crate) fn apply_run_json_patch(
    current: &[u8],
    patch: &json_patch::Patch,
    expected_after_hash: &str,
    json_format: Option<JsonFormat>,
) -> Result<String, String> {
    if patch.is_empty() {
        return Err("provider run authoring patch must not be empty".to_string());
    }
    let expected_after = ContentHash::from_hex(expected_after_hash).ok_or_else(|| {
        "provider run authoring patch carries an invalid expected_after_hash".to_string()
    })?;
    if expected_after.to_hex() != expected_after_hash {
        return Err(
            "provider run authoring patch expected_after_hash must be lowercase canonical hex"
                .to_string(),
        );
    }
    let mut value: serde_json::Value = serde_json::from_slice(current)
        .map_err(|error| format!("provider run authoring source is not JSON: {error}"))?;
    json_patch::patch(&mut value, patch)
        .map_err(|error| format!("provider run authoring patch cannot be applied: {error}"))?;
    let bytes = json_format.unwrap_or_default().encode(&value)?;
    let actual_after = ContentHash::of_bytes(&bytes);
    if actual_after != expected_after {
        return Err(format!(
            "provider run authoring patch produced hash {}, expected {}",
            actual_after.to_hex(),
            expected_after_hash
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("provider patched run JSON is not UTF-8: {error}"))
}
