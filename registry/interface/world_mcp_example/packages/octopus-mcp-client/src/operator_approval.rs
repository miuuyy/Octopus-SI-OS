use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_descriptors::InterfaceId;
use octopus_interfaces::InterfaceInvocationEnvelope;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) const OPERATOR_APPROVAL_RESPONSE_LABEL: &str = "operator.approval.response";
const APPROVAL_SCOPE: &str = "interface_operation";
const APPROVED_EDGE: (&str, &str) = ("operator.approval", "operator_approval");
const ANSWERED_EDGE: (&str, &str) = ("operator.input.provided", "operator_input");
const RECORD_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(default, rename = "readOnlyHint", skip_serializing_if = "Option::is_none")]
    pub(crate) read_only_hint: Option<bool>,
    #[serde(default, rename = "destructiveHint", skip_serializing_if = "Option::is_none")]
    pub(crate) destructive_hint: Option<bool>,
    #[serde(default, rename = "idempotentHint", skip_serializing_if = "Option::is_none")]
    pub(crate) idempotent_hint: Option<bool>,
    #[serde(default, rename = "openWorldHint", skip_serializing_if = "Option::is_none")]
    pub(crate) open_world_hint: Option<bool>,
}

impl McpToolAnnotations {
    fn read_only(&self) -> bool {
        self.read_only_hint == Some(true)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum CallApproval {
    Free,
    Fixed {
        tool: String,
        annotations: Option<McpToolAnnotations>,
    },
    Named(Arc<BTreeMap<String, Option<McpToolAnnotations>>>),
}

impl CallApproval {
    pub(crate) fn fixed(tool: &str, annotations: Option<&McpToolAnnotations>) -> Self {
        if annotations.is_some_and(McpToolAnnotations::read_only) {
            Self::Free
        } else {
            Self::Fixed {
                tool: tool.to_string(),
                annotations: annotations.cloned(),
            }
        }
    }

    pub(crate) fn declared_tools<'a>(
        fixed: impl Iterator<Item = (&'a str, Option<&'a McpToolAnnotations>)>,
    ) -> Arc<BTreeMap<String, Option<McpToolAnnotations>>> {
        let mut tools = BTreeMap::<String, Option<McpToolAnnotations>>::new();
        for (tool, annotations) in fixed {
            match tools.get(tool) {
                Some(existing) if existing.as_ref() != annotations => {
                    tools.insert(tool.to_string(), None);
                }
                Some(_) => {}
                None => {
                    tools.insert(tool.to_string(), annotations.cloned());
                }
            }
        }
        Arc::new(tools)
    }

    pub(crate) fn gated(&self, arguments: &Value) -> Option<(String, Option<McpToolAnnotations>)> {
        match self {
            Self::Free => None,
            Self::Fixed { tool, annotations } => Some((tool.clone(), annotations.clone())),
            Self::Named(tools) => {
                let name = arguments.get("name").and_then(Value::as_str)?;
                let annotations = tools.get(name).cloned().flatten();
                if annotations.as_ref().is_some_and(McpToolAnnotations::read_only) {
                    None
                } else {
                    Some((name.to_string(), annotations))
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperationApprovalRequest {
    pub(crate) schema_version: u32,
    pub(crate) approval_scope: String,
    pub(crate) request_id: String,
    pub(crate) call_id: String,
    pub(crate) interface: InterfaceId,
    pub(crate) operation: String,
    pub(crate) description: String,
    pub(crate) tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) annotations: Option<McpToolAnnotations>,
    pub(crate) arguments: Value,
    pub(crate) run: String,
    pub(crate) ee: String,
    pub(crate) worker: InterfaceId,
    pub(crate) source_artifact: Value,
}

impl OperationApprovalRequest {
    fn digest(&self) -> Result<String, String> {
        let mut commitment = serde_json::to_value(self).map_err(|error| error.to_string())?;
        commitment
            .as_object_mut()
            .ok_or("operator approval request did not encode as an object")?
            .remove("request_id");
        Ok(blake3::hash(&octopus_descriptors::canonical_bytes(&commitment))
            .to_hex()
            .to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingCall {
    pub(crate) request: OperationApprovalRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) correlation_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdmittedDecision {
    pub(crate) request_id: String,
    pub(crate) call_id: String,
    pub(crate) decision: String,
    pub(crate) operator_interface: InterfaceId,
    pub(crate) operator_artifact: Value,
}

impl AdmittedDecision {
    pub(crate) fn approved(&self) -> bool {
        self.decision == "accept"
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseBody {
    operator_approval_response: ResponseFields,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResponseFields {
    pub(crate) request_id: String,
    pub(crate) call_id: String,
    pub(crate) decision: String,
}

pub(crate) struct ApprovalStore {
    root: PathBuf,
}

impl ApprovalStore {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, request_id: &str, suffix: &str) -> Result<PathBuf, String> {
        let digest = request_id
            .strip_prefix("blake3:")
            .filter(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or("operator approval request has no exact BLAKE3 identity")?;
        Ok(self.root.join(format!("{digest}.{suffix}.json")))
    }

    pub(crate) fn open_request(
        &self,
        interface: &InterfaceId,
        envelope: &InterfaceInvocationEnvelope,
        operation: &str,
        description: &str,
        tool: String,
        annotations: Option<McpToolAnnotations>,
        arguments: &Value,
        correlation_id: Option<&str>,
    ) -> Result<OperationApprovalRequest, String> {
        let runtime = envelope.runtime().map_err(|error| {
            format!("operation '{operation}' needs operator approval, but its request carries no run provenance: {error}")
        })?;
        let mut request = OperationApprovalRequest {
            schema_version: 1,
            approval_scope: APPROVAL_SCOPE.to_string(),
            request_id: String::new(),
            call_id: correlation_id
                .map(str::to_string)
                .unwrap_or_else(|| envelope.artifact.id.as_str().to_string()),
            interface: interface.clone(),
            operation: operation.to_string(),
            description: description.to_string(),
            tool,
            annotations,
            arguments: arguments.clone(),
            run: runtime.run.as_str().to_string(),
            ee: runtime.ee.as_str().to_string(),
            worker: runtime.worker.clone(),
            source_artifact: serde_json::to_value(&envelope.artifact)
                .map_err(|error| error.to_string())?,
        };
        request.request_id = format!("blake3:{}", request.digest()?);
        let pending = PendingCall {
            request: request.clone(),
            correlation_id: correlation_id.map(str::to_string),
        };
        let path = self.path(&request.request_id, "request")?;
        if !create_record(&path, &pending)? && read_record::<PendingCall>(&path)? != pending {
            return Err("operator approval request already holds a different call".into());
        }
        Ok(request)
    }

    pub(crate) fn pending(&self, request_id: &str) -> Result<PendingCall, String> {
        let pending: PendingCall = read_record(&self.path(request_id, "request")?)?;
        if pending.request.request_id != request_id
            || pending.request.digest()? != request_id["blake3:".len()..]
        {
            return Err("operator approval request record changed its identity".into());
        }
        Ok(pending)
    }

    pub(crate) fn record_decision(&self, decision: &AdmittedDecision) -> Result<bool, String> {
        let path = self.path(&decision.request_id, "decision")?;
        if create_record(&path, decision)? {
            return Ok(true);
        }
        if read_record::<AdmittedDecision>(&path)? != *decision {
            return Err("operator approval request already has a different admitted decision".into());
        }
        Ok(false)
    }

    pub(crate) fn retained_result(&self, request_id: &str) -> Result<Option<Value>, String> {
        let path = self.path(request_id, "result")?;
        if !path.try_exists().map_err(|error| error.to_string())? {
            return Ok(None);
        }
        read_record(&path).map(Some)
    }

    pub(crate) fn retain_result(&self, request_id: &str, body: &Value) -> Result<(), String> {
        create_record(&self.path(request_id, "result")?, body).map(|_| ())
    }
}

pub(crate) fn approval_request_body(
    label: &str,
    operation: &str,
    correlation_id: Option<&str>,
    request: &OperationApprovalRequest,
) -> Value {
    json!({
        "label": label,
        "operation": operation,
        "correlation_id": correlation_id,
        "session_disposition": "awaiting_operator",
        "ok": true,
        "result": {
            "effect_performed": false,
            "operator_request": request,
        },
    })
}

pub(crate) fn read_response(envelope: &InterfaceInvocationEnvelope) -> Result<ResponseFields, String> {
    let payload = envelope
        .read_artifact_payload()
        .map_err(|error| error.to_string())?;
    let body: ResponseBody = serde_json::from_slice(&payload)
        .map_err(|error| format!("operator approval response is malformed: {error}"))?;
    Ok(body.operator_approval_response)
}

pub(crate) fn admit_decision(
    interface: &InterfaceId,
    envelope: &InterfaceInvocationEnvelope,
    pending: &PendingCall,
    response: ResponseFields,
) -> Result<AdmittedDecision, String> {
    let request = &pending.request;
    let (mark, verifier) = match response.decision.as_str() {
        "accept" => APPROVED_EDGE,
        "decline" | "cancel" => ANSWERED_EDGE,
        other => return Err(format!("operator decision '{other}' is not accept, decline or cancel")),
    };
    let metadata = envelope.metadata().map_err(|error| error.to_string())?;
    let runtime = envelope.runtime().map_err(|error| error.to_string())?;
    let bindings = metadata
        .checkpoint_binding_policy
        .as_ref()
        .ok_or("operator decision has no admitted binding snapshot")?;
    let admission = metadata
        .checkpoint_admission
        .as_ref()
        .ok_or("operator decision has no completed checkpoint admission")?;
    let [required] = metadata.required_verified_artifacts.as_slice() else {
        return Err("operator decision must close exactly one verified requirement".into());
    };
    let outbound = &metadata.outbound_interface_bindings;
    let label = OPERATOR_APPROVAL_RESPONSE_LABEL;
    if envelope
        .read_artifact_manifest()
        .map_err(|error| error.to_string())?
        .label
        != label
        || response.request_id != request.request_id
        || response.call_id != request.call_id
        || request.interface != *interface
        || runtime.interface != *interface
        || runtime.run.as_str() != request.run
        || runtime.ee.as_str() != request.ee
        || runtime.worker != request.worker
        || metadata.verification_failure.is_some()
        || !admission.complete
        || admission.artifact_label != label
        || admission.required_artifact_labels != [label]
        || required.mark.as_str() != mark
        || required.verifier.as_str() != verifier
        || required.artifact_label != label
        || required.params != json!({"request_id": request.request_id, "call_id": request.call_id})
        || !metadata.grant.mark_outcomes.iter().any(|value| value == mark)
        || !metadata.grant.interfaces.iter().any(|value| value == interface.as_str())
        || outbound.len() != 2
        || outbound[1] != *interface
        || outbound[0] == *interface
        || bindings.inbound.interface == *interface
        || outbound.contains(&bindings.inbound.interface)
        || bindings.outbound.iter().map(|binding| &binding.interface).ne(outbound.iter())
    {
        return Err("operator decision does not match the exact verifier-admitted request and scope".into());
    }
    Ok(AdmittedDecision {
        request_id: response.request_id,
        call_id: response.call_id,
        decision: response.decision,
        operator_interface: bindings.inbound.interface.clone(),
        operator_artifact: serde_json::to_value(&envelope.artifact)
            .map_err(|error| error.to_string())?,
    })
}

fn create_record<T: Serialize>(path: &Path, record: &T) -> Result<bool, String> {
    let parent = path.parent().ok_or("operator approval record has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let metadata = std::fs::symlink_metadata(parent).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("operator approval store is not a private directory".into());
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err("operator approval record exceeds its byte bound".into());
    }
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(format!("operator approval record cannot be created: {error}")),
    };
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(true)
}

fn read_record<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("operator approval record is unavailable: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > RECORD_LIMIT {
        return Err("operator approval record must be a bounded single-link file".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(RECORD_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("operator approval record is malformed: {error}"))
}
