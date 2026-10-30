#![forbid(unsafe_code)]

use octopus_policy::{ArtifactAccess, AuthoredInterfaceBinding};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_ledger::{ContentHash, RequiredVerifiedArtifact, TraceId};

mod index;
pub use index::RunRecordsIndex;

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
pub struct GrantEpoch(pub u64);

#[derive(Clone, PartialEq, Eq)]
pub struct RunRecordSnapshot {
    pub hash: ContentHash,
    pub bytes: Arc<[u8]>,
}

impl std::fmt::Debug for RunRecordSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunRecordSnapshot")
            .field("hash", &self.hash)
            .field("bytes_len", &self.bytes.len())
            .finish()
    }
}

/// POLICY.md §4 — an artifact binding is the run's authored declaration of its
/// object-level rights on a specific artifact. The `id` names the artifact; the
/// `access` states which of read/change/create/delete the run claims on it
/// (default: read only — a bound artifact is readable context). Formation masks
/// this down to the worker-interface + EE verb ceiling; the interface-binding
/// (the default flow) is a separate axis on the checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBinding {
    pub id: ArtifactId,
    #[serde(default)]
    pub access: ArtifactAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRecord {
    pub id: CheckpointId,
    pub run: RunId,
    pub trace: TraceId,
    pub epoch: GrantEpoch,
    #[serde(default)]
    pub interface_binding: Option<AuthoredInterfaceBinding>,
    #[serde(default)]
    pub outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    #[serde(default)]
    pub artifact_bindings: Vec<ArtifactBinding>,
    #[serde(default)]
    pub required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

impl CheckpointRecord {
    pub fn inbound_interface(&self) -> Option<&InterfaceId> {
        self.interface_binding
            .as_ref()
            .map(|binding| &binding.interface)
    }

    pub fn outbound_interfaces(&self) -> impl Iterator<Item = &InterfaceId> {
        self.outbound_interface_bindings
            .iter()
            .map(|binding| &binding.interface)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub id: RunId,
    pub ee: EeId,
    pub worker: InterfaceId,
    pub trace: TraceId,
    pub status: Value,
    #[serde(default)]
    pub interface_state: BTreeMap<InterfaceId, BTreeMap<String, Value>>,
    pub checkpoints: Vec<CheckpointRecord>,
    pub active_checkpoints: Vec<CheckpointId>,
}

impl RunRecord {
    pub fn checkpoint(&self, id: &CheckpointId) -> Option<&CheckpointRecord> {
        self.checkpoints.iter().find(|c| &c.id == id)
    }

    pub fn active_checkpoint_ids(&self) -> &[CheckpointId] {
        &self.active_checkpoints
    }

    pub fn load(run: &RunId, dir: &Path) -> std::io::Result<Option<Self>> {
        let path = dir.join(format!("{}.json", run.as_str()));
        match std::fs::read(&path) {
            Ok(bytes) => Self::from_bytes(&bytes).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> std::io::Result<Self> {
        let record: Self = serde_json::from_slice(bytes).map_err(std::io::Error::other)?;
        record
            .validate_artifact_arrival_shape()
            .map_err(std::io::Error::other)?;
        Ok(record)
    }

    pub fn validate_artifact_arrival_shape(&self) -> Result<(), String> {
        match self.status.as_str() {
            Some("running" | "idle") => {}
            _ => {
                return Err(format!(
                    "run '{}' JSON status must be either 'running' or 'idle'",
                    self.id.as_str()
                ));
            }
        }
        if self.trace.0.trim().is_empty() {
            return Err(format!(
                "run '{}' JSON has an empty trace",
                self.id.as_str()
            ));
        }
        if self.worker.as_str().trim().is_empty() {
            return Err(format!(
                "run '{}' JSON names no worker interface; a run resolves its ceiling only through the worker System-interface pinned in `worker`",
                self.id.as_str()
            ));
        }
        let mut checkpoints_by_id = BTreeMap::new();
        for checkpoint in &self.checkpoints {
            let checkpoint_id = checkpoint.id.as_str();
            if checkpoint_id.trim().is_empty() {
                return Err(format!(
                    "run '{}' JSON contains a checkpoint with an empty id",
                    self.id.as_str()
                ));
            }
            if checkpoints_by_id
                .insert(checkpoint_id, checkpoint)
                .is_some()
            {
                return Err(format!(
                    "run '{}' JSON repeats checkpoint '{}'",
                    self.id.as_str(),
                    checkpoint_id
                ));
            }
            if checkpoint.run != self.id {
                return Err(format!(
                    "run '{}' JSON checkpoint '{}' belongs to run '{}'",
                    self.id.as_str(),
                    checkpoint_id,
                    checkpoint.run.as_str()
                ));
            }
            if checkpoint.trace.0.trim().is_empty() {
                return Err(format!(
                    "run '{}' JSON checkpoint '{}' has an empty trace",
                    self.id.as_str(),
                    checkpoint_id
                ));
            }
            if let Some(binding) = checkpoint.interface_binding.as_ref() {
                binding.validate().map_err(|error| {
                    format!(
                        "run '{}' JSON checkpoint '{}' interface_binding is invalid: {error}",
                        self.id.as_str(),
                        checkpoint_id
                    )
                })?;
            }
            let mut outbound_interfaces = BTreeSet::new();
            for (binding_index, binding) in
                checkpoint.outbound_interface_bindings.iter().enumerate()
            {
                binding.validate().map_err(|error| {
                    format!(
                        "run '{}' JSON checkpoint '{}' outbound_interface_bindings[{binding_index}] is invalid: {error}",
                        self.id.as_str(),
                        checkpoint_id
                    )
                })?;
                if !outbound_interfaces.insert(binding.interface.clone()) {
                    return Err(format!(
                        "run '{}' JSON checkpoint '{}' repeats outbound interface '{}'; the interface folder is the only binding identity",
                        self.id.as_str(),
                        checkpoint_id,
                        binding.interface.as_str()
                    ));
                }
            }
            for (artifact_index, required) in
                checkpoint.required_verified_artifacts.iter().enumerate()
            {
                required.validate(&format!(
                    "run '{}' JSON checkpoint '{}' required_verified_artifacts[{artifact_index}]",
                    self.id.as_str(),
                    checkpoint_id
                ))?;
            }
            let mut required_artifact_labels = BTreeSet::new();
            for required in &checkpoint.required_verified_artifacts {
                if !required_artifact_labels.insert(required.artifact_label.clone()) {
                    return Err(format!(
                        "run '{}' JSON checkpoint '{}' repeats required_verified_artifacts artifact_label '{}'; one artifact edge cannot close multiple verified-artifact requirements",
                        self.id.as_str(),
                        checkpoint_id,
                        required.artifact_label
                    ));
                }
            }
            let mut bound_artifact_ids = BTreeSet::new();
            for binding in &checkpoint.artifact_bindings {
                if binding.id.as_str().trim().is_empty() {
                    return Err(format!(
                        "run '{}' JSON checkpoint '{}' contains an artifact binding with an empty id",
                        self.id.as_str(),
                        checkpoint_id
                    ));
                }
                if !bound_artifact_ids.insert(binding.id.clone()) {
                    return Err(format!(
                        "run '{}' JSON checkpoint '{}' repeats artifact binding '{}'; object-level rights are declared once per artifact",
                        self.id.as_str(),
                        checkpoint_id,
                        binding.id.as_str()
                    ));
                }
                if !binding.access.grants_any() {
                    return Err(format!(
                        "run '{}' JSON checkpoint '{}' artifact binding '{}' grants no object-level right (read/change/create/delete all false); a binding that grants nothing is authoring drift",
                        self.id.as_str(),
                        checkpoint_id,
                        binding.id.as_str()
                    ));
                }
            }
        }

        let mut active_ids = BTreeSet::new();
        for active in &self.active_checkpoints {
            let active_id = active.as_str();
            if active_id.trim().is_empty() {
                return Err(format!(
                    "run '{}' JSON active_checkpoints[] contains an empty id",
                    self.id.as_str()
                ));
            }
            if !active_ids.insert(active_id.to_string()) {
                return Err(format!(
                    "run '{}' JSON repeats active checkpoint '{}'",
                    self.id.as_str(),
                    active_id
                ));
            }
            let Some(checkpoint) = checkpoints_by_id.get(active_id).copied() else {
                return Err(format!(
                    "run '{}' JSON active checkpoint '{}' is not present in checkpoints[]",
                    self.id.as_str(),
                    active_id
                ));
            };
            if checkpoint.interface_binding.is_none() {
                return Err(format!(
                    "run '{}' JSON active checkpoint '{}' has no interface_binding; runtime admits only artifact arrivals",
                    self.id.as_str(),
                    active_id
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_schema_accepts_required_verified_artifacts_with_verifier() {
        let checkpoint: CheckpointRecord = serde_json::from_value(serde_json::json!({
            "id": "c1",
            "run": "run1",
            "trace": "trace:run1:c1",
            "epoch": 0,
            "required_verified_artifacts": [{
                "mark": "coder.build.ok",
                "artifact_label": "coder.build.result",
                "verifier": "build_success",
                "params": {}
            }]
        }))
        .expect("required_verified_artifacts is the checkpoint schema name");

        assert_eq!(checkpoint.required_verified_artifacts.len(), 1);
        assert_eq!(
            checkpoint.required_verified_artifacts[0].artifact_label,
            "coder.build.result"
        );
    }

    #[test]
    fn checkpoint_schema_rejects_unknown_checkpoint_contract_fields() {
        let err = serde_json::from_value::<CheckpointRecord>(serde_json::json!({
            "id": "c1",
            "run": "run1",
            "trace": "trace:run1:c1",
            "epoch": 0,
            "required_artifacts": []
        }))
        .expect_err("checkpoint schema must fail closed on non-canonical contract fields");

        assert!(
            err.to_string().contains("required_artifacts"),
            "unexpected serde error: {err}"
        );
    }

    #[test]
    fn run_record_rejects_duplicate_required_verified_artifact_labels() {
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "id": "run1",
            "ee": "ee1",
            "worker": "system_worker_flash",
            "trace": "trace:run1",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "c1",
                "run": "run1",
                "trace": "trace:run1:c1",
                "epoch": 0,
                "interface_binding": AuthoredInterfaceBinding::all(InterfaceId::new("world_test")),
                "required_verified_artifacts": [
                    {
                        "mark": "coder.build.ok",
                        "artifact_label": "same.artifact",
                        "verifier": "build_success",
                        "params": {}
                    },
                    {
                        "mark": "ocr.text.contains",
                        "artifact_label": "same.artifact",
                        "verifier": "ocr_text_contains",
                        "params": {"expected_text": "ready"}
                    }
                ]
            }],
            "active_checkpoints": ["c1"]
        }))
        .expect("JSON shape parses before semantic validation");

        let err = record
            .validate_artifact_arrival_shape()
            .expect_err("one artifact label cannot target multiple required verified artifacts");
        assert!(
            err.contains("repeats required_verified_artifacts artifact_label"),
            "unexpected validation error: {err}"
        );
    }

    #[test]
    fn artifact_binding_access_defaults_to_read_only_and_rejects_empty_grant() {
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "id": "run1",
            "ee": "ee1",
            "worker": "system_worker_flash",
            "trace": "trace:run1",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "c1",
                "run": "run1",
                "trace": "trace:run1:c1",
                "epoch": 0,
                "interface_binding": AuthoredInterfaceBinding::all(InterfaceId::new("world_test")),
                "artifact_bindings": [{ "id": "artifact-role" }]
            }],
            "active_checkpoints": ["c1"]
        }))
        .expect("a bare {id} artifact binding parses");
        let access = &record.checkpoints[0].artifact_bindings[0].access;
        assert!(
            access.read && !access.change && !access.create && !access.delete,
            "a bare binding defaults to read-only object-level access"
        );
        record
            .validate_artifact_arrival_shape()
            .expect("a read-only binding validates");

        let empty: RunRecord = serde_json::from_value(serde_json::json!({
            "id": "run1",
            "ee": "ee1",
            "worker": "system_worker_flash",
            "trace": "trace:run1",
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "c1",
                "run": "run1",
                "trace": "trace:run1:c1",
                "epoch": 0,
                "interface_binding": AuthoredInterfaceBinding::all(InterfaceId::new("world_test")),
                "artifact_bindings": [{ "id": "artifact-role", "access": { "read": false } }]
            }],
            "active_checkpoints": ["c1"]
        }))
        .expect("the JSON shape parses before semantic validation");
        assert!(
            empty.validate_artifact_arrival_shape().is_err(),
            "a binding granting no object-level right fails closed"
        );
    }
}
