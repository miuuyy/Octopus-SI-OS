use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use octopus_descriptors::{ArtifactId, InterfaceId};
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_ledger::{
    ArtifactRef, ContentHash, DeliveryVerdict, InterfaceToolAffordance, InvocationMetadata,
};
use octopus_operator_chat_client::run_composition::{
    AuthoredRunComposition, AuthoredRunCompositionPlan, CollectionSelection, CompositionAuthoring,
    CompositionCommunication, CompositionRoots, CompositionSeed, RegistryCandidateValidation, RegistryValidationIssue,
    RegistryValidationRoots, RunCompositionPlan, RunCompositionRequest,
    composition_candidate_writes, plan_authored_run_composition, plan_run_composition,
    validate_registry_candidate, validate_run_authoring_source, validation_roots,
};
use octopus_operator_chat_client::{ExternalInputSeed, ExternalRelaySeed, ModelOutputSeed, PlannedClientWrite, SelectedToolSeed};
use octopus_operator_chat_client::run_composition::current_run::{CurrentRunRequest, current_selection, plan_current_run_amendment_for_native};
use octopus_operator_chat_client::publication_selection::{
    PublicationSelectionEvidence, validate_evidence, validate_publication_snapshot_refs,
};
use octopus_run_authoring::ProviderOutgoingBinding;
use octopus_run_authoring::capability_amendment::{ProviderCapabilityAmendment, ProviderCapabilityBridge, validate_installed};
use octopus_run_record::RunRecord;
use octopus_tool_handlers::ToolResultArtifact;
use octopus_tool_registry::ToolArgumentValidator;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PEER_MESSAGE_LABEL: &str = "provider.peer.message";

fn validation_report(validation: &RegistryCandidateValidation) -> Value {
    let issues = validation.stages.last().into_iter().flat_map(|stage| stage.issues.iter()).collect::<BTreeSet<_>>();
    json!({
        "safe_to_apply": validation.safe_to_apply,
        "checked_subject_count": validation.checked_subjects.len(),
        "publication_stage_count": validation.stages.len(),
        "regressions": validation.regressions,
        "issues": issues,
        "scope": "Candidate declarations and formation. This does not establish execution or delivery.",
    })
}

#[cfg(test)]
#[path = "../tests/support/native_run_composition.rs"]
mod composition_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerAction {
    Queue,
    Followup,
    Interrupt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "PeerMessageWire")]
pub struct PeerMessage {
    pub action: PeerAction,
    pub control_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerMessageWire {
    action: PeerAction,
    control_id: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    artifacts: Vec<ArtifactRef>,
}

impl TryFrom<PeerMessageWire> for PeerMessage {
    type Error = String;

    fn try_from(wire: PeerMessageWire) -> Result<Self, String> {
        let value = Self {
            action: wire.action,
            control_id: wire.control_id,
            text: wire.text,
            artifacts: wire.artifacts,
        };
        value.validate()?;
        Ok(value)
    }
}

impl PeerMessage {
    pub fn validate(&self) -> Result<(), String> {
        exact_string(&self.control_id, "peer control_id")?;
        if self
            .text
            .as_ref()
            .is_some_and(|text| text.trim().is_empty())
        {
            return Err("peer text must be absent or nonempty".into());
        }
        if self.action != PeerAction::Interrupt && self.text.is_none() && self.artifacts.is_empty()
        {
            return Err("queue/followup requires text or explicit artifacts".into());
        }
        distinct_refs(&self.artifacts)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOperation {
    CreateRun,
    PreviewRun,
    ReviseRun,
    PreviewRevision,
    PreviewCurrentRun,
    AmendCurrentRun,
    InspectAuthoring,
    SendMessage,
    FollowupTask,
    ListRuns,
    Wait,
    Interrupt,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCatalogEntry {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub operation: RunOperation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCatalog {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub operations: Vec<RunCatalogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunAffordanceConfig {
    #[serde(deserialize_with = "deserialize_artifact_ref")]
    pub catalog_artifact: ArtifactRef,
    #[serde(default)]
    pub file_write_tool: Option<String>,
    #[serde(default)]
    pub file_read_tool: Option<String>,
    #[serde(default)]
    pub artifact_send_tool: Option<String>,
    #[serde(default)]
    pub roots: Option<CompositionRoots>,
    #[serde(default)]
    pub defaults: Option<RunDefaults>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDefaults {
    #[serde(default)]
    pub client_template_interface_id: Option<String>,
    #[serde(default)]
    pub output_selection: Option<octopus_operator_chat_client::run_composition::CompositionOutputSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ee_id: Option<String>,
    pub worker_template_interface_id: String,
    pub provider_interface_id: String,
    pub model: String,
    pub reasoning_effort: String,
    #[serde(default)]
    pub service_tier: Option<String>,
    pub authoring: CompositionAuthoring,
    #[serde(default)]
    pub worker_environment_kinds: Vec<String>,
    #[serde(default)]
    pub tools: Vec<SelectedToolSeed>,
    #[serde(default)]
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    #[serde(default)]
    pub external_inputs: Vec<ExternalInputSeed>,
    #[serde(default)]
    pub external_relays: Vec<ExternalRelaySeed>,
    #[serde(default)]
    pub model_outputs: Vec<ModelOutputSeed>,
    #[serde(default)]
    pub publications: Vec<CollectionSelection>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    pub context_artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub return_interface_ids: Vec<String>,
    #[serde(default)]
    pub return_to_source: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRunRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored: Option<AuthoredRunComposition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub output_selection: Option<octopus_operator_chat_client::run_composition::CompositionOutputSelection>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    pub context_artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub ee_id: Option<String>,
    #[serde(default)]
    pub worker_template_interface_id: Option<String>,
    #[serde(default)]
    pub provider_interface_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub service_tier: Option<String>,
    #[serde(default)]
    pub worker_environment_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub tools: Option<Vec<SelectedToolSeed>>,
    #[serde(default)]
    pub door_host_slots: Option<BTreeMap<String, BTreeMap<String, PathBuf>>>,
    #[serde(default)]
    pub external_inputs: Option<Vec<ExternalInputSeed>>,
    #[serde(default)]
    pub external_relays: Option<Vec<ExternalRelaySeed>>,
    #[serde(default)]
    pub model_outputs: Option<Vec<ModelOutputSeed>>,
    #[serde(default)]
    pub publications: Option<Vec<CollectionSelection>>,
    #[serde(default)]
    pub return_interface_ids: Option<Vec<String>>,
    #[serde(default)]
    pub client_interface_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRequest {
    pub interface: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_artifact_refs")]
    pub artifacts: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitRequest {
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEffect {
    pub tool: String,
    pub arguments: Value,
    pub correlation_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileProof {
    pub path: PathBuf,
    pub before_hash: Option<String>,
    pub after_hash: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatedRun {
    pub run_id: String,
    pub worker_interface_id: String,
    pub provider_interface_id: String,
    pub receive_interface_id: String,
    pub return_interface_ids: Vec<String>,
    pub model: String,
    pub reasoning_effort: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativePhase {
    CurrentRun {
        stage: CurrentRunStage,
        plan: Box<PendingCurrentRun>,
    },
    PinnedCurrentRun {
        stage: CurrentRunStage,
        plan: Box<PendingCurrentRun>,
        publication_read_refs: Vec<ArtifactRef>,
    },
    Applying {
        created: Value,
        files: Vec<FileProof>,
        validation_roots: RegistryValidationRoots,
        allowed_issues: Vec<RegistryValidationIssue>,
        checked_subjects: Vec<String>,
        seed: Option<NativeEffect>,
        outgoing_bindings: Vec<ProviderOutgoingBinding>,
    },
    PinnedApplying {
        created: Value,
        files: Vec<FileProof>,
        publication_read_refs: Vec<ArtifactRef>,
        validation_roots: RegistryValidationRoots,
        allowed_issues: Vec<RegistryValidationIssue>,
        checked_subjects: Vec<String>,
        seed: Option<NativeEffect>,
        outgoing_bindings: Vec<ProviderOutgoingBinding>,
    },
    AppliedSeeding {
        created: Value,
        files: Vec<FileProof>,
        write_receipt: Value,
        validation: Value,
    },
    PinnedAppliedSeeding {
        created: Value,
        files: Vec<FileProof>,
        publication_read_refs: Vec<ArtifactRef>,
        write_receipt: Value,
        validation: Value,
    },
    Writing {
        created: CreatedRun,
        files: Vec<FileProof>,
        seed: NativeEffect,
        outgoing_bindings: Vec<ProviderOutgoingBinding>,
    },
    Seeding {
        created: CreatedRun,
        files: Vec<FileProof>,
        write_receipt: Value,
    },
    Sending {
        interface: String,
        message: PeerMessage,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurrentRunStage { Dependencies, Activation, Readback }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingCurrentRun {
    pub amendment: ProviderCapabilityAmendment,
    pub dependency_files: Vec<FileProof>,
    pub activation_files: Vec<FileProof>,
    pub activation: Option<NativeEffect>,
    pub readback: NativeEffect,
    pub validation_roots: RegistryValidationRoots,
    pub checked_subjects: Vec<String>,
    pub allowed_issues: Vec<RegistryValidationIssue>,
    pub receipts: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePending {
    pub call_id: String,
    pub operation_name: String,
    pub source_run: String,
    pub effect: NativeEffect,
    pub state: NativePhase,
}

#[derive(Clone, Debug)]
pub enum NativeStep {
    AmendmentEffect {
        effect: NativeEffect,
        pending: NativePending,
        amendment: ProviderCapabilityAmendment,
    },
    Effect {
        effect: NativeEffect,
        pending: NativePending,
        outgoing_bindings: Vec<ProviderOutgoingBinding>,
    },
    Complete {
        result: Value,
    },
    Wait {
        request: WaitRequest,
    },
}

impl NativePending {
    fn publication_read_refs(&self) -> Option<&[ArtifactRef]> {
        match &self.state {
            NativePhase::PinnedApplying { publication_read_refs, .. }
            | NativePhase::PinnedAppliedSeeding { publication_read_refs, .. }
            | NativePhase::PinnedCurrentRun { publication_read_refs, .. } => Some(publication_read_refs),
            _ => None,
        }
    }

    fn validate_publication_reads(&self, registry_root: &Path) -> Result<(), String> {
        if let Some(refs) = self.publication_read_refs() {
            validate_publication_snapshot_refs(registry_root, refs)?;
        }
        Ok(())
    }

    fn successor_state(&self, state: NativePhase) -> Result<NativePhase, String> {
        match (self.publication_read_refs(), state) {
            (Some(refs), NativePhase::CurrentRun { stage, plan }) => Ok(NativePhase::PinnedCurrentRun {
                stage, plan, publication_read_refs:refs.to_vec(),
            }),
            (Some(refs), NativePhase::AppliedSeeding { created, files, write_receipt, validation }) => Ok(NativePhase::PinnedAppliedSeeding {
                created, files, write_receipt, validation, publication_read_refs:refs.to_vec(),
            }),
            (None, state) => Ok(state),
            _ => Err("pinned native operation has an unsupported successor phase".into()),
        }
    }

    fn advance_current_run(&self, stage: &CurrentRunStage, plan: &PendingCurrentRun, body: &Value, receipt: &Value) -> Result<NativeStep, String> {
        let mut next = plan.clone();
        next.receipts.push(receipt.clone());
        match stage {
            CurrentRunStage::Dependencies => {
                validate_file_receipt(&self.effect, body, &plan.dependency_files)?;
                readback_files(&plan.dependency_files)?;
                self.validate_publication_reads(&plan.validation_roots.registry_root)?;
                let (effect, stage) = match &plan.activation {
                    Some(effect) => (effect.clone(), CurrentRunStage::Activation),
                    None => (plan.readback.clone(), CurrentRunStage::Readback),
                };
                Ok(NativeStep::AmendmentEffect {
                    amendment: plan.amendment.clone(), effect: effect.clone(),
                    pending: Self { effect, state:self.successor_state(NativePhase::CurrentRun { stage, plan:Box::new(next) })?, ..self.clone() },
                })
            }
            CurrentRunStage::Activation => {
                validate_file_receipt(&self.effect, body, &plan.activation_files)?;
                readback_files(&plan.activation_files)?;
                self.validate_publication_reads(&plan.validation_roots.registry_root)?;
                let effect = plan.readback.clone();
                Ok(NativeStep::Effect {
                    effect: effect.clone(), outgoing_bindings:Vec::new(),
                    pending: Self { effect, state:self.successor_state(NativePhase::CurrentRun { stage:CurrentRunStage::Readback, plan:Box::new(next) })?, ..self.clone() },
                })
            }
            CurrentRunStage::Readback => {
                let path = self.effect.arguments.get("path").and_then(Value::as_str).ok_or("current-run readback lost its exact path")?;
                let content = body.get("content").and_then(Value::as_str).ok_or("current-run readback has no content")?;
                let hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
                if body.get("path").and_then(Value::as_str) != Some(path)
                    || body.get("content_hash").and_then(Value::as_str) != Some(hash.as_str())
                    || body.get("bytes").and_then(Value::as_u64) != Some(content.len() as u64) {
                    return Err("current-run readback differs from its exact file receipt".into());
                }
                let record = RunRecord::from_bytes(content.as_bytes()).map_err(|e| e.to_string())?;
                if record.id.as_str() != self.source_run { return Err("current-run readback names another run".into()); }
                validate_installed(&record, &plan.amendment)?;
                readback_files(&plan.dependency_files)?;
                readback_files(&plan.activation_files)?;
                let live = RunRecord::from_bytes(&std::fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                validate_installed(&live, &plan.amendment)?;
                self.validate_publication_reads(&plan.validation_roots.registry_root)?;
                let validation = validate_registry_candidate(&plan.validation_roots, &[])?;
                let issues = validation.stages.first().ok_or("current-run validation has no stage")?.issues.iter()
                    .filter(|issue| (issue.subject == "snapshot" || plan.checked_subjects.contains(&issue.subject)) && !plan.allowed_issues.contains(issue))
                    .cloned().collect::<Vec<_>>();
                Ok(NativeStep::Complete { result:json!({
                    "ok":issues.is_empty(), "call_id":self.call_id, "operation":self.operation_name,
                    "run_id":self.source_run, "amendment_key":plan.amendment.amendment_key,
                    "installed":true, "readback_hash":hash, "receipts":next.receipts,
                    "unmet_dependencies":issues, "work_completed":false,
                }) })
            }
        }
    }

    pub fn advance(&self, result: &Value) -> Result<NativeStep, String> {
        let receipt: ToolResultArtifact = serde_json::from_value(result.clone())
            .map_err(|error| format!("native operation result is malformed: {error}"))?;
        if receipt.tool != self.effect.tool
            || receipt.correlation_id.as_deref() != Some(self.effect.correlation_id.as_str())
        {
            return Err("native operation result belongs to another tool or correlation".into());
        }
        if !receipt.ok {
            return Ok(NativeStep::Complete {
                result: self.failure_result(result),
            });
        }
        let body = receipt.body.ok_or("native result has no body")?;
        match &self.state {
            NativePhase::CurrentRun { stage, plan } | NativePhase::PinnedCurrentRun { stage, plan, .. } => self.advance_current_run(stage, plan, &body, result),
            NativePhase::Applying {
                created,
                files,
                validation_roots,
                allowed_issues,
                checked_subjects,
                seed,
                outgoing_bindings,
            } | NativePhase::PinnedApplying {
                created,
                files,
                validation_roots,
                allowed_issues,
                checked_subjects,
                seed,
                outgoing_bindings,
                ..
            } => {
                validate_file_receipt(&self.effect, &body, files)?;
                readback_files(files)?;
                self.validate_publication_reads(&validation_roots.registry_root)?;
                let mut validation = validate_registry_candidate(validation_roots, &[])?;
                let unexpected = validation.stages[0]
                    .issues
                    .iter()
                    .filter(|issue| {
                        (issue.subject == "snapshot" || checked_subjects.contains(&issue.subject))
                            && !allowed_issues.contains(issue)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                validation.checked_subjects = checked_subjects.clone();
                for stage in &mut validation.stages {
                    stage
                        .available
                        .retain(|subject| checked_subjects.contains(subject));
                    stage.issues.retain(|issue| {
                        issue.subject == "snapshot" || checked_subjects.contains(&issue.subject)
                    });
                }
                if !unexpected.is_empty() {
                    return Ok(NativeStep::Complete {
                        result: json!({
                            "ok":false,"installed":true,"seed_admitted":false,"work_completed":false,
                            "created":created,"files":files,"write_receipt":result,
                            "validation":validation_report(&validation),"unmet_dependencies":unexpected,
                        }),
                    });
                }
                let Some(seed) = seed else {
                    return Ok(NativeStep::Complete {
                        result: json!({
                            "ok":true,"installed":true,"seed_admitted":false,"work_completed":false,
                            "created":created,"files":files,"write_receipt":result,"validation":validation_report(&validation),
                        }),
                    });
                };
                Ok(NativeStep::Effect {
                    effect: seed.clone(),
                    outgoing_bindings: outgoing_bindings.clone(),
                    pending: Self {
                        effect: seed.clone(),
                        state: self.successor_state(NativePhase::AppliedSeeding {
                            created: created.clone(),
                            files: files.clone(),
                            write_receipt: result.clone(),
                            validation: validation_report(&validation),
                        })?,
                        ..self.clone()
                    },
                })
            }
            NativePhase::AppliedSeeding {
                created,
                files,
                write_receipt,
                validation,
            } | NativePhase::PinnedAppliedSeeding {
                created,
                files,
                write_receipt,
                validation,
                ..
            } => {
                let verdict = parse_delivery_verdict(body)?;
                let admitted = matches!(verdict, DeliveryVerdict::Admitted { .. });
                let mut completed = json!({
                        "ok":admitted,"call_id":self.call_id,"operation":self.operation_name,
                        "created":created,"installed":true,"files":files,"write_receipt":write_receipt,
                        "validation":validation,"seed_receipt":result,"seed_admitted":admitted,"work_completed":false,
                    });
                if let Some(refs) = self.publication_read_refs() { completed["publication_read_refs"] = json!(refs); }
                Ok(NativeStep::Complete { result:completed })
            }
            NativePhase::Writing {
                created,
                files,
                seed,
                outgoing_bindings,
            } => {
                validate_file_receipt(&self.effect, &body, files)?;
                if self.effect.arguments.get("ordered") == Some(&Value::Bool(true)) {
                    readback_files(files)?;
                }
                let next = Self {
                    effect: seed.clone(),
                    state: NativePhase::Seeding {
                        created: created.clone(),
                        files: files.clone(),
                        write_receipt: result.clone(),
                    },
                    ..self.clone()
                };
                Ok(NativeStep::Effect {
                    effect: seed.clone(),
                    pending: next,
                    outgoing_bindings: outgoing_bindings.clone(),
                })
            }
            NativePhase::Seeding {
                created,
                files,
                write_receipt,
            } => {
                let verdict = parse_delivery_verdict(body)?;
                Ok(NativeStep::Complete {
                    result: json!({
                        "call_id": self.call_id, "operation": self.operation_name,
                        "created": created, "installed": true, "files": files,
                        "write_receipt": write_receipt, "seed_receipt": result,
                        "ok": matches!(verdict, DeliveryVerdict::Admitted { .. }),
                        "seed_admitted": matches!(verdict, DeliveryVerdict::Admitted { .. }),
                        "work_completed": false,
                    }),
                })
            }
            NativePhase::Sending { interface, message } => {
                let verdict = parse_delivery_verdict(body)?;
                Ok(NativeStep::Complete {
                    result: json!({
                        "call_id": self.call_id, "operation": self.operation_name,
                        "interface": interface, "control_id": message.control_id,
                        "action": message.action, "receipt": result,
                        "ok": matches!(verdict, DeliveryVerdict::Admitted { .. }),
                        "admitted": matches!(verdict, DeliveryVerdict::Admitted { .. }),
                        "recipient_action_confirmed": false, "work_completed": false,
                    }),
                })
            }
        }
    }

    fn failure_result(&self, receipt: &Value) -> Value {
        let mut result = json!({
            "call_id": self.call_id, "operation": self.operation_name,
            "ok": false, "receipt": receipt, "work_completed": false,
        });
        match &self.state {
            NativePhase::CurrentRun { stage, plan } | NativePhase::PinnedCurrentRun { stage, plan, .. } => {
                result["run_id"] = json!(self.source_run);
                result["amendment_key"] = json!(plan.amendment.amendment_key);
                result["stage"] = json!(stage);
                result["receipts"] = json!(plan.receipts);
                result["installed"] = Value::Null;
            }
            NativePhase::Applying { created, files, .. } | NativePhase::PinnedApplying { created, files, .. } => {
                result["created"] = created.clone();
                result["planned_files"] = json!(files);
                result["installed"] = Value::Null;
                result["seed_admitted"] = json!(false);
            }
            NativePhase::AppliedSeeding {
                created,
                files,
                write_receipt,
                validation,
            } | NativePhase::PinnedAppliedSeeding {
                created,
                files,
                write_receipt,
                validation,
                ..
            } => {
                result["created"] = created.clone();
                result["files"] = json!(files);
                result["write_receipt"] = write_receipt.clone();
                result["validation"] = validation.clone();
                result["installed"] = json!(true);
                result["seed_admitted"] = Value::Null;
            }
            NativePhase::Writing { created, files, .. } => {
                result["created"] = json!(created);
                result["planned_files"] = json!(files);
                result["installed"] = Value::Null;
                result["seed_admitted"] = json!(false);
            }
            NativePhase::Seeding {
                created,
                files,
                write_receipt,
            } => {
                result["created"] = json!(created);
                result["files"] = json!(files);
                result["write_receipt"] = write_receipt.clone();
                result["installed"] = json!(true);
                result["seed_admitted"] = Value::Null;
            }
            NativePhase::Sending { interface, message } => {
                result["interface"] = json!(interface);
                result["control_id"] = json!(message.control_id);
                result["admitted"] = Value::Null;
                result["recipient_action_confirmed"] = json!(false);
            }
        }
        if let Some(refs) = self.publication_read_refs() { result["publication_read_refs"] = json!(refs); }
        result
    }
}

#[derive(Clone, Debug)]
pub struct RunAffordances {
    config: RunAffordanceConfig,
    catalog: RunCatalog,
    enabled: BTreeSet<String>,
}

impl RunAffordances {
    fn current_run_step(&self, metadata: &InvocationMetadata, current: &RunRecord, call_id: &str, operation_name: &str,
        request: CurrentRunRequest, preview: bool) -> Result<NativeStep, String> {
        for publication in &request.publications {
            require_readable_refs(metadata, std::slice::from_ref(&publication.source))?;
        }
        let roots = self.config.roots.as_ref().ok_or("current-run authoring roots are absent")?;
        let writer = self.config.file_write_tool.as_ref().ok_or("current-run file_write_tool is absent")?;
        let reader = self.config.file_read_tool.as_ref().ok_or("current-run file_read_tool is absent")?;
        if writer == reader { return Err("current-run writer and reader must be distinct exact tools".into()); }
        let runtime = metadata.runtime.as_ref().ok_or("current-run operation has no runtime coordinate")?;
        let provider = &runtime.interface;
        let source_path = roots.run_records_root.join(format!("{}.json", current.id));
        let retained_tools = [writer, reader].into_iter().map(|tool| {
            let descriptor = octopus_tool_registry::load_tool_descriptor_from_dir(&roots.registry_root.join("tool"), tool).map_err(|e| e.to_string())?
                .ok_or_else(|| format!("current-run bridge descriptor '{tool}' is absent"))?;
            if descriptor.id.as_str() != tool || !metadata.grant.tools.contains(tool)
                || metadata.grant.hard_blocked_tools.contains(tool) { return Err("current-run bridge tool is not exactly granted".to_string()); }
            Ok((tool.clone(), descriptor.interface))
        }).collect::<Result<Vec<_>, String>>()?;
        let identity = call_identity(current.id.as_str(), &request.amendment_key)?;
        let readback = NativeEffect { tool:reader.clone(), arguments:json!({"path":source_path}), correlation_id:format!("{identity}:readback") };
        let read_bridge = current_run_bridge(&readback, retained_tools[1].1.clone())?;
        let mut plan = plan_current_run_amendment_for_native(roots, current, provider, request, read_bridge, &retained_tools, metadata)?;
        let batch = |writes: &[octopus_operator_chat_client::run_composition::RegistryCandidateWrite], suffix: &str| -> Result<Option<(NativeEffect, Vec<FileProof>)>, String> {
            if writes.is_empty() { return Ok(None); }
            let writes = writes.iter().map(|write| PlannedClientWrite { path:write.path.clone(), content:write.content.clone(),
                expected_before_hash:write.expected_before_hash.clone(), tag:format!("amendment:{}",write.path.display()) }).collect::<Vec<_>>();
            let (arguments, files) = file_write_batch(&writes, &source_path)?;
            Ok(Some((NativeEffect { tool:writer.clone(), arguments, correlation_id:format!("{identity}:{suffix}") }, files)))
        };
        let dependencies = batch(&plan.dependency_writes, "dependencies")?;
        let activation = batch(&plan.activation_writes, "activation")?;
        if let Some((effect, _)) = &activation { plan.amendment.bridge = current_run_bridge(effect, retained_tools[0].1.clone())?; }
        if preview || !plan.validation.safe_to_apply {
            return Ok(NativeStep::Complete { result:json!({
                "ok":plan.validation.safe_to_apply, "preview":preview, "installed":false, "run_id":current.id,
                "amendment_key":plan.amendment.amendment_key, "future_run_contract":plan.amendment.after,
                "dependency_writes":plan.dependency_writes, "activation_writes":plan.activation_writes,
                "bridge":plan.amendment.bridge, "validation":validation_report(&plan.validation), "awaiting_activation":plan.awaiting_activation, "work_completed":false,
            }) });
        }
        let publication_read_refs = publication_read_refs(&plan.publication_evidence)?;
        let pending_plan = PendingCurrentRun {
            amendment:plan.amendment.clone(), dependency_files:dependencies.as_ref().map(|(_, files)|files.clone()).unwrap_or_default(),
            activation_files:activation.as_ref().map(|(_, files)|files.clone()).unwrap_or_default(),
            activation:activation.as_ref().map(|(effect, _)|effect.clone()), readback:readback.clone(),
            validation_roots:validation_roots(roots), checked_subjects:plan.validation.checked_subjects,
            allowed_issues:plan.validation.stages.last().ok_or("amendment validation has no final stage")?.issues.clone(), receipts:Vec::new(),
        };
        if dependencies.is_none() {
            validate_publication_snapshot_refs(&pending_plan.validation_roots.registry_root, &publication_read_refs)?;
        }
        let (effect, stage, attach) = match dependencies {
            Some((effect, _)) => (effect, CurrentRunStage::Dependencies, false),
            None => match activation { Some((effect, _)) => (effect, CurrentRunStage::Activation, true), None => (readback, CurrentRunStage::Readback, true) },
        };
        let pending = NativePending { call_id:call_id.into(), operation_name:operation_name.into(), source_run:current.id.to_string(), effect:effect.clone(),
            state:NativePhase::PinnedCurrentRun { stage, plan:Box::new(pending_plan), publication_read_refs } };
        if attach { Ok(NativeStep::AmendmentEffect { effect, pending, amendment:plan.amendment }) }
        else { Ok(NativeStep::Effect { effect, pending, outgoing_bindings:Vec::new() }) }
    }

    pub fn from_invocation(
        input: &InterfaceInvocationEnvelope,
        private_config: &Value,
    ) -> Result<Option<Self>, String> {
        if private_config.is_null() {
            return Ok(None);
        }
        let private = private_config
            .as_object()
            .ok_or("provider private_config must be an object")?;
        let Some(raw) = private.get("run_affordances") else {
            return Ok(None);
        };
        let config: RunAffordanceConfig = serde_json::from_value(raw.clone())
            .map_err(|error| format!("run_affordances config is malformed: {error}"))?;
        let metadata = input.metadata().map_err(|error| error.to_string())?;
        require_readable_refs(metadata, std::slice::from_ref(&config.catalog_artifact))?;
        let mut bound = input
            .artifact_binding_payloads
            .iter()
            .filter(|bound| bound.artifact == config.catalog_artifact);
        let payload = bound
            .next()
            .ok_or("run affordance catalog payload is not exactly bound")?;
        if bound.next().is_some() {
            return Err("run affordance catalog is bound more than once".into());
        }
        let bytes = payload.read_bytes().map_err(|error| error.to_string())?;
        let catalog: RunCatalog = serde_json::from_slice(bytes.as_ref())
            .map_err(|error| format!("bound run affordance catalog is malformed: {error}"))?;
        if catalog.id != config.catalog_artifact.id.as_str() {
            return Err("run affordance catalog self-identity differs from its bound ref".into());
        }
        exact_string(&catalog.label, "catalog label")?;
        if catalog.operations.is_empty() {
            return Err("run affordance catalog is empty".into());
        }
        for id in config
            .file_write_tool
            .iter()
            .chain(config.file_read_tool.iter())
            .chain(config.artifact_send_tool.iter())
        {
            exact_string(id, "configured tool")?;
        }
        if config.file_write_tool.is_some() && config.file_write_tool == config.artifact_send_tool {
            return Err("file writer and artifact sender must be different exact tools".into());
        }
        if let Some(roots) = &config.roots {
            for path in [
                &roots.registry_root,
                &roots.ee_instances_root,
                &roots.run_records_root,
                &roots.interface_private_state_root,
            ] {
                absolute_path(path)?;
            }
        }
        if let Some(defaults) = &config.defaults {
            validate_defaults(defaults)?;

        }
        let mut names = BTreeSet::new();
        let mut enabled = BTreeSet::new();
        for row in &catalog.operations {
            exact_string(&row.name, "operation name")?;
            if !row
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            {
                return Err(
                    "operation name contains characters outside the native tool name contract"
                        .into(),
                );
            }
            if row.description.trim().is_empty() || row.description.trim() != row.description {
                return Err("operation description must be nonempty and trimmed".into());
            }
            if !names.insert(row.name.clone()) {
                return Err("run catalog repeats an operation name".into());
            }
            if metadata
                .grant
                .interface_affordance_specs
                .iter()
                .any(|tool| tool.name.as_str() == row.name)
            {
                return Err(format!(
                    "run operation '{}' collides with an ordinary tool",
                    row.name
                ));
            }
            if !row.input_schema.is_object()
                || row.input_schema.get("type").and_then(Value::as_str) != Some("object")
            {
                return Err(format!(
                    "run operation '{}' requires an object input schema",
                    row.name
                ));
            }
            ToolArgumentValidator::compile(&row.input_schema)?;
            if matches!(row.operation, RunOperation::PreviewCurrentRun | RunOperation::AmendCurrentRun)
                && (config.roots.is_none() || config.file_write_tool.is_none() || config.file_read_tool.is_none()) {
                return Err("current-run authoring requires explicit roots, file_write_tool and file_read_tool".into());
            }
            if matches!(
                row.operation,
                RunOperation::CreateRun
                    | RunOperation::PreviewRun
                    | RunOperation::ReviseRun
                    | RunOperation::PreviewRevision
            ) && (config.roots.is_none()
                || config.file_write_tool.is_none()
                || config.artifact_send_tool.is_none())
            {
                return Err("run authoring catalog row requires roots, file_write_tool and artifact_send_tool".into());
            }
            if matches!(
                row.operation,
                RunOperation::SendMessage | RunOperation::FollowupTask | RunOperation::Interrupt
            ) && config.artifact_send_tool.is_none()
            {
                return Err("peer operation requires explicit artifact_send_tool config".into());
            }
            if operation_granted(row.operation, &config, metadata)? {
                enabled.insert(row.name.clone());
            }
        }
        Ok(Some(Self {
            config,
            catalog,
            enabled,
        }))
    }

    pub fn affordances(&self) -> Vec<InterfaceToolAffordance> {
        self.catalog
            .operations
            .iter()
            .filter(|row| self.enabled.contains(&row.name))
            .map(|row| InterfaceToolAffordance {
                name: octopus_descriptors::ToolId::new(&row.name),
                description: row.description.clone(),
                args_schema: row.input_schema.clone(),
            })
            .collect()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.enabled.contains(name)
    }

    pub fn planning_baseline(&self, operation_name: &str, arguments: &Value) -> Result<Option<(String, Value)>, String> {
        let row = self.catalog.operations.iter().find(|row|row.name == operation_name).ok_or("native operation is absent from its declared catalog")?;
        if !self.contains(operation_name) { return Err("native operation is not granted".into()); }
        if !matches!(row.operation,RunOperation::PreviewCurrentRun | RunOperation::AmendCurrentRun) { return Ok(None); }
        let request: CurrentRunRequest = serde_json::from_value(arguments.clone()).map_err(|e|e.to_string())?;
        match (request.selection,request.authored) {
            (Some(_),None)=>Ok(None),
            (None,Some(authored))=>{
                if ContentHash::from_hex(&authored.expected_before_hash).is_none_or(|hash|hash.to_hex()!=authored.expected_before_hash) {
                    return Err("authored current-run baseline requires an exact content hash".into());
                }
                RunRecord::from_bytes(&serde_json::to_vec(&authored.expected_before).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
                Ok(Some((authored.expected_before_hash,authored.expected_before)))
            }
            _=>Err("current-run amendment requires exactly one complete selection or authored record".into()),
        }
    }

    pub fn plan_call(
        &self,
        metadata: &InvocationMetadata,
        admitted_parent: &RunRecord,
        parent_record: &RunRecord,
        call_id: &str,
        operation_name: &str,
        arguments: &Value,
    ) -> Result<NativeStep, String> {
        exact_string(call_id, "native call_id")?;
        require_readable_refs(
            metadata,
            std::slice::from_ref(&self.config.catalog_artifact),
        )?;
        let runtime = metadata
            .runtime
            .as_ref()
            .ok_or("native run operation requires runtime metadata")?;
        if runtime.run != parent_record.id
            || runtime.ee != admitted_parent.ee
            || runtime.worker != parent_record.worker
        {
            return Err("native run operation source record differs from runtime metadata".into());
        }
        let row = self
            .catalog
            .operations
            .iter()
            .find(|row| row.name == operation_name)
            .ok_or("native operation is absent from the bound catalog")?;
        if !self.contains(operation_name)
            || !operation_granted(row.operation, &self.config, metadata)?
        {
            return Err("native operation lacks its required ordinary tool grant".into());
        }
        ToolArgumentValidator::compile(&row.input_schema)?.validate(arguments)?;
        let control_id = call_identity(runtime.run.as_str(), call_id)?;
        match row.operation {
            RunOperation::PreviewCurrentRun | RunOperation::AmendCurrentRun => {
                validate_run_authoring_source(metadata, admitted_parent)?;
                self.current_run_step(metadata, parent_record, call_id, operation_name,
                    serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?,
                    row.operation == RunOperation::PreviewCurrentRun)
            }
            RunOperation::CreateRun
            | RunOperation::PreviewRun
            | RunOperation::ReviseRun
            | RunOperation::PreviewRevision => {
                validate_run_authoring_source(metadata, admitted_parent)?;
                let mut args: CreateRunRequest =
                    serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
                let creation_key = args.creation_key.as_deref().unwrap_or(call_id).to_string();
                exact_string(&creation_key, "creation_key")?;
                let identity = call_identity(runtime.run.as_str(), &creation_key)?;
                let preview = matches!(
                    row.operation,
                    RunOperation::PreviewRun | RunOperation::PreviewRevision
                );
                let revise = matches!(
                    row.operation,
                    RunOperation::ReviseRun | RunOperation::PreviewRevision
                );
                if let Some(authored) = args.authored.take() {
                    if arguments
                        .as_object()
                        .ok_or("native authoring requires an object")?
                        .keys()
                        .any(|key| key != "creation_key" && key != "authored")
                    {
                        return Err("exact authored files cannot be combined with template selection arguments".into());
                    }
                    let plan = plan_authored_run_composition(
                        self.config
                            .roots
                            .as_ref()
                            .ok_or("creation roots are absent")?,
                        parent_record,
                        authored,
                        revise,
                    )?;
                    return self.authored_step(
                        parent_record,
                        call_id,
                        operation_name,
                        &control_id,
                        creation_key,
                        plan,
                        preview,
                    );
                }
                if revise {
                    return Err(
                        "revise_run requires exact authored files and an existing run identity"
                            .into(),
                    );
                }
                let request =
                    self.composition_request(metadata, admitted_parent, &identity, args)?;
                let plan = plan_run_composition(&request)?;
                self.creation_step(
                    parent_record,
                    call_id,
                    operation_name,
                    &control_id,
                    creation_key,
                    plan,
                    preview,
                )
            }
            RunOperation::InspectAuthoring => {
                if arguments.as_object().is_none_or(|args| !args.is_empty()) {
                    return Err("inspect_authoring expects an empty object".into());
                }
                let roots = self
                    .config
                    .roots
                    .as_ref()
                    .ok_or("inspect_authoring requires declared roots")?;
                let inspected = validate_registry_candidate(&validation_roots(roots), &[])?;
                Ok(NativeStep::Complete {
                    result: json!({
                        "roots":roots,"defaults":self.config.defaults,
                        "current_selection":current_selection(roots, parent_record, &runtime.interface)?,
                        "installed":inspected.stages[0],
                        "selection_contract":"Installed entries are candidates. Each run is validated against its own worker ceiling, EE chain and exact authored bindings; creation does not inherit the initiator grant.",
                        "exact_contract":{"argument":"authored","run_id":"exact new run id (existing id for revise_run)",
                            "writes":"ordered exact path/content/expected_before_hash entries including complete run JSON",
                            "outgoing_bindings":"explicit interface/artifact_label/artifact_bindings for checked caller connections",
                            "seed":"optional exact interface_id/label/payload; omitted means publish waits without immediate emission"},
                    }),
                })
            }
            RunOperation::SendMessage | RunOperation::FollowupTask | RunOperation::Interrupt => {
                let args: PeerRequest =
                    serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
                require_peer_target(metadata, parent_record, &args.interface, self.config.roots.as_ref().map(|roots| roots.registry_root.as_path()))?;
                require_readable_refs(metadata, &args.artifacts)?;
                let message = PeerMessage {
                    action: match row.operation {
                        RunOperation::SendMessage => PeerAction::Queue,
                        RunOperation::FollowupTask => PeerAction::Followup,
                        RunOperation::Interrupt => PeerAction::Interrupt,
                        _ => unreachable!(),
                    },
                    control_id: control_id.clone(),
                    text: args.text,
                    artifacts: args.artifacts,
                };
                message.validate()?;
                let effect = peer_effect(
                    self.sender()?,
                    &args.interface,
                    &message,
                    &format!("{control_id}:send"),
                );
                let pending = NativePending {
                    call_id: call_id.into(),
                    operation_name: operation_name.into(),
                    source_run: runtime.run.as_str().into(),
                    effect: effect.clone(),
                    state: NativePhase::Sending {
                        interface: args.interface.clone(),
                        message,
                    },
                };
                Ok(NativeStep::Effect {
                    effect,
                    pending,
                    outgoing_bindings: vec![ProviderOutgoingBinding {
                        interface: InterfaceId::new(args.interface),
                        artifact_label: PEER_MESSAGE_LABEL.into(),
                        artifact_bindings: Vec::new(),
                    }],
                })
            }
            RunOperation::ListRuns => {
                if arguments
                    .as_object()
                    .is_none_or(|object| !object.is_empty())
                {
                    return Err("list_runs expects an empty object".into());
                }
                Ok(NativeStep::Complete {
                    result: json!({
                        "source_run": {"run_id": runtime.run, "ee_id": runtime.ee,
                            "worker_interface_id": runtime.worker, "recorded_status": parent_record.status,
                            "run_record_hash": runtime.run_record_hash, "provider_activity": "not_observed"},
                    "declared_peer_interfaces": declared_peers(metadata, parent_record, self.config.roots.as_ref().map(|roots| roots.registry_root.as_path())),
                    "receive_interface_id": declared_receive_interface(parent_record, &metadata.runtime.as_ref().ok_or("list requires runtime metadata")?.interface)?,
                        "source_movement_seq": metadata.source_movement_seq,
                    }),
                })
            }
            RunOperation::Wait => {
                let request: WaitRequest =
                    serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
                if request.timeout_ms == 0 {
                    return Err("wait timeout_ms must be positive".into());
                }
                Ok(NativeStep::Wait { request })
            }
        }
    }

    fn sender(&self) -> Result<&str, String> {
        self.config
            .artifact_send_tool
            .as_deref()
            .ok_or_else(|| "artifact_send_tool is not configured".into())
    }

    fn composition_request(
        &self,
        metadata: &InvocationMetadata,
        parent: &RunRecord,
        identity: &str,
        args: CreateRunRequest,
    ) -> Result<RunCompositionRequest, String> {
        let defaults = self
            .config
            .defaults
            .as_ref()
            .ok_or("creation defaults are absent")?;
        require_readable_refs(metadata, &args.artifacts)?;
        let contexts = merge_refs(&defaults.context_artifacts, &args.context_artifacts)?;
        require_readable_refs(metadata, &contexts)?;
        let publications = args.publications.unwrap_or_else(|| defaults.publications.clone());
        for publication in &publications {
            require_readable_refs(metadata, std::slice::from_ref(&publication.source))?;
        }
        let seed = PeerMessage {
            action: PeerAction::Followup,
            control_id: identity.into(),
            text: args.text,
            artifacts: args.artifacts.clone(),
        };
        seed.validate()?;
        let return_interfaces = match args.return_interface_ids {
            Some(ids) => ids,
            None => {
                let mut ids = defaults.return_interface_ids.clone();
                if defaults.return_to_source {
                    let endpoint = declared_receive_interface(parent, &metadata.runtime.as_ref().ok_or("return requires runtime metadata")?.interface)?;
                    if !ids.contains(&endpoint) { ids.push(endpoint); }
                }
                ids
            }
        };
        Ok(RunCompositionRequest {
            roots: self
                .config
                .roots
                .clone()
                .ok_or("creation roots are absent")?,
            metadata: metadata.clone(),
            admitted_parent_record: parent.clone(),
            identity_seed: identity.into(),
            ee_id: args.ee_id.unwrap_or_else(|| parent.ee.to_string()),
            worker_template_interface_id: args
                .worker_template_interface_id
                .unwrap_or_else(|| defaults.worker_template_interface_id.clone()),
            provider_interface_id: args
                .provider_interface_id
                .unwrap_or_else(|| defaults.provider_interface_id.clone()),
            model: args.model.unwrap_or_else(|| defaults.model.clone()),
            reasoning_effort: args
                .reasoning_effort
                .unwrap_or_else(|| defaults.reasoning_effort.clone()),
            service_tier: args.service_tier.or_else(|| defaults.service_tier.clone()),
            authoring: defaults.authoring.clone(),
            communication: CompositionCommunication {
                client_template_interface_id: args.client_interface_id.is_none()
                    .then(|| defaults.client_template_interface_id.clone()).flatten(),
                client_title: args.title,
                receive_interface_id: format!("system_worker_{}", identity),
                return_interface_ids: return_interfaces,
                client_interface_id: args.client_interface_id,
                output_selection: args.output_selection.or_else(|| defaults.output_selection.clone()),
            },
            selected_worker_environment_kinds: args
                .worker_environment_kinds
                .unwrap_or_else(|| defaults.worker_environment_kinds.clone()),
            selected_tools: args.tools.unwrap_or_else(|| defaults.tools.clone()),
            door_host_slots: args
                .door_host_slots
                .unwrap_or_else(|| defaults.door_host_slots.clone()),
            external_inputs: args.external_inputs.unwrap_or_else(|| defaults.external_inputs.clone()),
            external_relays: args.external_relays.unwrap_or_else(|| defaults.external_relays.clone()),
            model_outputs: args.model_outputs.unwrap_or_else(|| defaults.model_outputs.clone()),
            publications,
            context_artifact_bindings: Vec::new(),
            payload_artifact_bindings: merge_refs(&contexts, &args.artifacts)?,
            seed: CompositionSeed {
                label: PEER_MESSAGE_LABEL.into(),
                payload: json!(seed),
            },
        })
    }

    fn creation_step(
        &self,
        parent: &RunRecord,
        call_id: &str,
        operation_name: &str,
        identity: &str,
        creation_key: String,
        plan: RunCompositionPlan,
        preview: bool,
    ) -> Result<NativeStep, String> {
        let roots = self
            .config
            .roots
            .as_ref()
            .ok_or("creation roots are absent")?;
        let mut role_refs = BTreeSet::new();
        for update in &plan.required_binding_updates {
            if update.run_id != parent.id.as_str()
                || update.outbound_binding.interface.as_str() != plan.receive_interface_id
            {
                return Err("composition requested an unrelated parent binding edit".into());
            }
            role_refs.extend(
                update
                    .required_artifact_bindings
                    .iter()
                    .map(|binding| ArtifactId::new(&binding.id)),
            );
        }
        let authored = AuthoredRunComposition {
            run_id: plan.run_id.clone(),
            writes: composition_candidate_writes(&plan),
            publication_read_refs: publication_read_refs(&plan.publication_evidence)?,
            outgoing_bindings: vec![ProviderOutgoingBinding {
                interface: InterfaceId::new(&plan.receive_interface_id),
                artifact_label: plan.seed.label.clone(),
                artifact_bindings: role_refs.into_iter().collect(),
            }],
            seed: Some(plan.seed.clone()),
        };
        let exact = plan_authored_run_composition(roots, parent, authored, false)?;
        self.authored_step(
            parent,
            call_id,
            operation_name,
            identity,
            creation_key,
            exact,
            preview,
        )
    }

    fn authored_step(
        &self,
        parent: &RunRecord,
        call_id: &str,
        operation_name: &str,
        identity: &str,
        creation_key: String,
        plan: AuthoredRunCompositionPlan,
        preview: bool,
    ) -> Result<NativeStep, String> {
        let roots = self
            .config
            .roots
            .as_ref()
            .ok_or("creation roots are absent")?;
        if preview || !plan.validation.safe_to_apply {
            return Ok(NativeStep::Complete {
                result: json!({
                    "ok":plan.validation.safe_to_apply,"preview":preview,"installed":false,"seed_admitted":false,
                    "creation_key":creation_key,"authored":plan.authored,"run_contract":plan.run,
                    "validation":validation_report(&plan.validation),"work_completed":false,
                }),
            });
        }
        let writes = plan
            .authored
            .writes
            .iter()
            .map(|write| PlannedClientWrite {
                path: write.path.clone(),
                content: write.content.clone(),
                expected_before_hash: write.expected_before_hash.clone(),
                tag: format!("authored:{}", write.path.display()),
            })
            .collect::<Vec<_>>();
        let (arguments, files) = file_write_batch(
            &writes,
            &roots.run_records_root.join(format!("{}.json", parent.id)),
        )?;
        let seed = plan.authored.seed.as_ref().map(|seed| -> Result<NativeEffect, String> {
            Ok(NativeEffect { tool:self.sender()?.into(), correlation_id:format!("{identity}:seed"),
                arguments:json!({"interface":seed.interface_id,"label":seed.label,"payload":seed.payload}) })
        }).transpose()?;
        let effect = NativeEffect {
            tool: self
                .config
                .file_write_tool
                .clone()
                .ok_or("file_write_tool is absent")?,
            arguments,
            correlation_id: format!("{identity}:write"),
        };
        let created = json!({"run_id":plan.run.id,"ee_id":plan.run.ee,"worker_interface_id":plan.run.worker,
            "active_checkpoints":plan.run.active_checkpoints,"checkpoints":plan.run.checkpoints,
            "creation_key":creation_key});
        let pending = NativePending {
            call_id: call_id.into(),
            operation_name: operation_name.into(),
            source_run: parent.id.to_string(),
            effect: effect.clone(),
            state: NativePhase::PinnedApplying {
                created,
                files,
                publication_read_refs: plan.authored.publication_read_refs,
                validation_roots: validation_roots(roots),
                allowed_issues: plan
                    .validation
                    .stages
                    .last()
                    .ok_or("candidate has no complete validation stage")?
                    .issues
                    .clone(),
                checked_subjects: plan.validation.checked_subjects,
                seed,
                outgoing_bindings: plan.authored.outgoing_bindings,
            },
        };
        Ok(NativeStep::Effect {
            effect,
            pending,
            outgoing_bindings: Vec::new(),
        })
    }
}

pub fn call_identity(source_run: &str, call_id: &str) -> Result<String, String> {
    exact_string(source_run, "source run")?;
    exact_string(call_id, "native call_id")?;
    let bytes = serde_json::to_vec(&["octopus.run.affordance.v1", source_run, call_id])
        .map_err(|e| e.to_string())?;
    Ok(format!("native_{}", ContentHash::of_bytes(&bytes).to_hex()))
}

fn current_run_bridge(effect: &NativeEffect, return_interface: InterfaceId) -> Result<ProviderCapabilityBridge, String> {
    Ok(ProviderCapabilityBridge { tool:effect.tool.clone(), correlation_id:effect.correlation_id.clone(), return_interface,
        arguments_hash:ContentHash::of_bytes(&serde_json::to_vec(&effect.arguments).map_err(|e| e.to_string())?).to_hex() })
}

fn peer_effect(
    tool: &str,
    interface: &str,
    message: &PeerMessage,
    correlation_id: &str,
) -> NativeEffect {
    NativeEffect {
        tool: tool.into(),
        correlation_id: correlation_id.into(),
        arguments: json!({"interface": interface, "label": PEER_MESSAGE_LABEL, "payload": message}),
    }
}

fn file_write_batch(
    writes: &[PlannedClientWrite],
    parent_path: &Path,
) -> Result<(Value, Vec<FileProof>), String> {
    if writes.is_empty() {
        return Err("native creation has no planned writes".into());
    }
    let mut paths = BTreeSet::new();
    let mut proofs = Vec::with_capacity(writes.len());
    let mut entries = Vec::with_capacity(writes.len());
    for write in writes {
        absolute_path(&write.path)?;
        if write.path == parent_path || !paths.insert(write.path.clone()) {
            return Err(
                "native creation must not patch the parent run or repeat write paths".into(),
            );
        }
        if let Some(hash) = &write.expected_before_hash {
            if ContentHash::from_hex(hash).is_none_or(|parsed| parsed.to_hex() != *hash) {
                return Err("planned file preimage is not a canonical content hash".into());
            }
        }
        proofs.push(FileProof {
            path: write.path.clone(),
            before_hash: write.expected_before_hash.clone(),
            after_hash: ContentHash::of_bytes(write.content.as_bytes()).to_hex(),
            bytes: write.content.len() as u64,
        });
        entries.push(json!({"path": write.path, "content": write.content, "expected_before_hash": write.expected_before_hash}));
        let before = match std::fs::symlink_metadata(&write.path) {
            Ok(metadata) if metadata.is_file() => {
                let bytes = std::fs::read(&write.path).map_err(|error| error.to_string())?;
                if Some(ContentHash::of_bytes(&bytes).to_hex()) != write.expected_before_hash {
                    return Err(format!(
                        "native authoring preimage changed at '{}'",
                        write.path.display()
                    ));
                }
                Some(String::from_utf8(bytes).map_err(|error| error.to_string())?)
            }
            Ok(_) => {
                return Err(format!(
                    "native authoring target '{}' is not a regular file",
                    write.path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if write.expected_before_hash.is_some() {
                    return Err(format!(
                        "native authoring preimage disappeared at '{}'",
                        write.path.display()
                    ));
                }
                None
            }
            Err(error) => return Err(error.to_string()),
        };
        entries.last_mut().ok_or("native write entry is absent")?["expected_before_content"] =
            json!(before);
    }
    Ok((json!({"ordered": true, "writes": entries}), proofs))
}

fn validate_file_receipt(
    effect: &NativeEffect,
    body: &Value,
    files: &[FileProof],
) -> Result<(), String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Written {
        result: String,
        writes: Vec<Value>,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OrderedWrite {
        path: PathBuf,
        before_hash: Option<String>,
        after_hash: String,
        bytes: u64,
        outcome: String,
        observed_hash: Option<String>,
        error: Option<String>,
    }
    let written: Written = serde_json::from_value(body.clone())
        .map_err(|error| format!("native authoring file evidence is malformed: {error}"))?;
    let ordered = match effect.arguments.get("ordered") {
        Some(Value::Bool(true)) => true,
        None => false,
        Some(_) => return Err("native authoring call has an invalid ordered declaration".into()),
    };
    if written.result
        != if ordered {
            "ordered_file_writes"
        } else {
            "files_written"
        }
        || written.writes.len() != files.len()
    {
        return Err("native authoring file result does not prove the exact planned writes".into());
    }
    for (value, expected) in written.writes.into_iter().zip(files) {
        let observed = if ordered {
            let receipt: OrderedWrite =
                serde_json::from_value(value).map_err(|error| error.to_string())?;
            if receipt.outcome != "applied"
                || receipt.error.is_some()
                || receipt.observed_hash.as_ref() != Some(&receipt.after_hash)
            {
                return Err(
                    "native authoring success contains an unapplied or uncertain entry".into(),
                );
            }
            FileProof {
                path: receipt.path,
                before_hash: receipt.before_hash,
                after_hash: receipt.after_hash,
                bytes: receipt.bytes,
            }
        } else {
            serde_json::from_value(value).map_err(|error| error.to_string())?
        };
        if observed != *expected {
            return Err(
                "native authoring file receipt differs from its exact prospective bytes".into(),
            );
        }
    }
    Ok(())
}

fn publication_read_refs(evidence: &[PublicationSelectionEvidence]) -> Result<Vec<ArtifactRef>, String> {
    let mut refs = BTreeMap::new();
    for evidence in evidence {
        validate_evidence(evidence)?;
        for reference in evidence.snapshot.iter().chain(&evidence.context_sources) {
            if refs.insert(reference.id.clone(), reference.clone()).is_some_and(|prior| prior != *reference) {
                return Err("publication read evidence contains conflicting exact references".into());
            }
        }
    }
    Ok(refs.into_values().collect())
}

fn readback_files(files: &[FileProof]) -> Result<(), String> {
    for proof in files {
        let metadata = std::fs::symlink_metadata(&proof.path).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() != proof.bytes {
            return Err(format!(
                "native authoring readback has no exact regular file at '{}'",
                proof.path.display()
            ));
        }
        let bytes = std::fs::read(&proof.path).map_err(|error| error.to_string())?;
        if ContentHash::of_bytes(&bytes).to_hex() != proof.after_hash {
            return Err(format!(
                "native authoring readback changed at '{}'",
                proof.path.display()
            ));
        }
    }
    Ok(())
}

fn parse_delivery_verdict(body: Value) -> Result<DeliveryVerdict, String> {
    let verdict: DeliveryVerdict = serde_json::from_value(body.clone())
        .map_err(|e| format!("malformed artifact.send verdict: {e}"))?;
    if serde_json::to_value(&verdict).map_err(|e| e.to_string())? != body {
        return Err("artifact.send verdict contains undeclared fields".into());
    }
    Ok(verdict)
}

fn operation_granted(
    operation: RunOperation,
    config: &RunAffordanceConfig,
    metadata: &InvocationMetadata,
) -> Result<bool, String> {
    let granted = |id: &Option<String>| -> Result<bool, String> {
        let Some(id) = id else {
            return Ok(false);
        };
        if !metadata.grant.tools.contains(id) || metadata.grant.hard_blocked_tools.contains(id) {
            return Ok(false);
        }
        let mut specs = metadata
            .grant
            .interface_affordance_specs
            .iter()
            .filter(|spec| spec.name.as_str() == id);
        let spec = specs
            .next()
            .ok_or_else(|| format!("granted tool '{id}' has no formed descriptor affordance"))?;
        if specs.next().is_some() || !spec.args_schema.is_object() {
            return Err(format!(
                "granted tool '{id}' has ambiguous or malformed affordance metadata"
            ));
        }
        Ok(true)
    };
    match operation {
        RunOperation::PreviewCurrentRun | RunOperation::AmendCurrentRun => Ok(granted(&config.file_write_tool)?
            && granted(&config.file_read_tool)?),
        RunOperation::CreateRun
        | RunOperation::PreviewRun
        | RunOperation::ReviseRun
        | RunOperation::PreviewRevision => Ok(granted(&config.file_write_tool)?
            && granted(&config.artifact_send_tool)?),
        RunOperation::SendMessage | RunOperation::FollowupTask | RunOperation::Interrupt => {
            granted(&config.artifact_send_tool)
        }
        RunOperation::InspectAuthoring | RunOperation::ListRuns | RunOperation::Wait => Ok(true),
    }
}

fn declared_receive_interface(record: &RunRecord, provider: &InterfaceId) -> Result<String, String> {
    let mut endpoints = Vec::new();
    for (id, state) in &record.interface_state {
        if let Some(value) = state.get("input_intake") {
            let config: octopus_operator_chat_client::input_intake::InputIntakeConfig = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            config.validate(id)?;
            if config.run == record.id.as_str() && config.provider_interface == *provider { endpoints.push(id.to_string()); }
        }
    }
    match endpoints.as_slice() {
        [] => Ok(record.worker.to_string()),
        [endpoint] => Ok(endpoint.clone()),
        _ => Err("run has ambiguous declared input endpoints for this provider".into()),
    }
}

fn declared_peers(metadata: &InvocationMetadata, record: &RunRecord, registry: Option<&Path>) -> Vec<String> {
    metadata
        .grant
        .attached_interface_bindings
        .iter()
        .filter(|binding| binding.interface_type == "system")
        .map(|binding| binding.interface.as_str())
        .filter(|interface| require_peer_target(metadata, record, interface, registry).is_ok())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn require_peer_target(
    metadata: &InvocationMetadata,
    record: &RunRecord,
    interface: &str,
    registry: Option<&Path>,
) -> Result<(), String> {
    exact_string(interface, "peer interface")?;
    let service = metadata.grant.attached_interface_bindings.iter().any(|bound| bound.interface.as_str() == interface && bound.interface_type == "system" && bound.role == "service");
    if service {
        let root = registry.ok_or("exact intake descriptor validation requires declared registry roots")?;
        if interface.contains('/') || interface == "." || interface == ".." { return Err("invalid exact intake interface".into()); }
        let bytes = std::fs::read(root.join("interface").join(interface).join("interface.json")).map_err(|e| e.to_string())?;
        let descriptor: octopus_interfaces::InterfaceDescriptor = octopus_descriptors::parse_strict(&bytes).map_err(|e| e.to_string())?;
        if descriptor.id.as_str() != interface || descriptor.interface_type != octopus_interfaces::InterfaceType::System || descriptor.role.0 != "service" || descriptor.interface_driver_ref != octopus_operator_chat_client::input_intake::INPUT_INTAKE_DRIVER { return Err("peer target is not the declared exact input service".into()); }
        let config: octopus_operator_chat_client::input_intake::InputIntakeConfig = serde_json::from_value(descriptor.interface_driver_config.as_ref().and_then(|c| c.private_config.get("input_intake")).cloned().ok_or("input service has no concrete declaration")?).map_err(|e| e.to_string())?;
        config.validate(&descriptor.id)?;
    }
    if !metadata.grant.interfaces.iter().any(|id| id == interface)
        || !metadata
            .grant
            .attached_interface_bindings
            .iter()
            .any(|bound| {
                bound.interface.as_str() == interface
                    && bound.interface_type == "system"
                    && (bound.role == "worker" || service)
            })
    {
        return Err(format!(
            "peer interface '{interface}' is not an exact granted outgoing System input binding"
        ));
    }
    if !record
        .checkpoints
        .iter()
        .filter(|checkpoint| record.active_checkpoints.contains(&checkpoint.id))
        .flat_map(|checkpoint| &checkpoint.outbound_interface_bindings)
        .any(|binding| {
            binding.interface.as_str() == interface
                && binding.config.artifact_policy.admits(PEER_MESSAGE_LABEL)
        })
    {
        return Err("peer message label is not admitted by the exact outgoing binding".into());
    }
    Ok(())
}

fn require_readable_refs(
    metadata: &InvocationMetadata,
    refs: &[ArtifactRef],
) -> Result<(), String> {
    distinct_refs(refs)?;
    for reference in refs {
        let mut bound = metadata
            .artifact_binding_refs
            .iter()
            .filter(|bound| bound.id == reference.id);
        if !metadata.grant.may_read_artifact(reference.id.as_str())
            || bound.next() != Some(reference)
            || bound.next().is_some()
        {
            return Err(format!(
                "artifact '{}' is not an exact readable metadata binding",
                reference.id
            ));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactArtifactRef {
    id: ArtifactId,
    content: ContentHash,
}

impl From<ExactArtifactRef> for ArtifactRef {
    fn from(reference: ExactArtifactRef) -> Self {
        Self {
            id: reference.id,
            content: reference.content,
        }
    }
}

fn deserialize_artifact_ref<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<ArtifactRef, D::Error> {
    ExactArtifactRef::deserialize(deserializer).map(Into::into)
}

fn deserialize_artifact_refs<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ArtifactRef>, D::Error> {
    Vec::<ExactArtifactRef>::deserialize(deserializer)
        .map(|refs| refs.into_iter().map(Into::into).collect())
}

fn distinct_refs(refs: &[ArtifactRef]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for reference in refs {
        exact_string(reference.id.as_str(), "artifact id")?;
        if !ids.insert(reference.id.as_str()) {
            return Err("artifact refs repeat a logical identity".into());
        }
    }
    Ok(())
}

fn merge_refs(first: &[ArtifactRef], second: &[ArtifactRef]) -> Result<Vec<ArtifactRef>, String> {
    distinct_refs(first)?;
    distinct_refs(second)?;
    let mut refs = first.to_vec();
    for reference in second {
        if let Some(existing) = refs.iter().find(|item| item.id == reference.id) {
            if existing != reference {
                return Err("context artifact selected at conflicting hashes".into());
            }
        } else {
            refs.push(reference.clone());
        }
    }
    Ok(refs)
}

fn exact_string(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value || value.contains(['\r', '\n', '\0']) {
        return Err(format!("{field} must be a nonempty exact string"));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(format!(
            "native composition path '{}' must be absolute without traversal",
            path.display()
        ));
    }
    Ok(())
}

fn validate_defaults(defaults: &RunDefaults) -> Result<(), String> {
    distinct_refs(&defaults.context_artifacts)?;
    if let Some(tier) = &defaults.service_tier {
        exact_string(tier, "service_tier")?;
        if tier.len() > 80 || tier.chars().any(char::is_control) {
            return Err("service_tier must be a native tier id".into());
        }
    }
    if let Some(template) = &defaults.client_template_interface_id { exact_string(template, "client template")?; }
    if let Some(ee) = &defaults.ee_id { exact_string(ee, "ee_id")?; }
    for (field, value) in [
        ("worker template", &defaults.worker_template_interface_id),
        ("provider interface", &defaults.provider_interface_id),
        ("model", &defaults.model),
        ("reasoning_effort", &defaults.reasoning_effort),
        ("authoring source", &defaults.authoring.source_interface_id),
        ("authoring room", &defaults.authoring.room_interface_id),
        ("authoring tool", &defaults.authoring.tool_id),
        ("tool call label", &defaults.authoring.tool_call_label),
        ("tool call mark", &defaults.authoring.tool_call_mark),
        ("tool call verifier", &defaults.authoring.tool_call_verifier),
    ] {
        exact_string(value, field)?;
    }
    if defaults.authoring.result_timeout_ms == 0 {
        return Err("authoring timeout must be positive".into());
    }
    for paths in defaults.door_host_slots.values() {
        for path in paths.values() {
            absolute_path(path)?;
        }
    }
    for id in defaults
        .return_interface_ids
        .iter()
        .chain(&defaults.worker_environment_kinds)
    {
        exact_string(id, "default exact selection")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_interfaces::{ArtifactBindingPayload, InterfaceArtifactFile};

    fn writing() -> NativePending {
        let effect = NativeEffect {
            tool: "exact.writer".into(),
            arguments: json!({"writes":[]}),
            correlation_id: "write-1".into(),
        };
        NativePending {
            call_id: "native-call".into(),
            operation_name: "create_work".into(),
            source_run: "source".into(),
            effect,
            state: NativePhase::Writing {
                created: CreatedRun {
                    run_id: "created".into(),
                    worker_interface_id: "system_worker_created".into(),
                    provider_interface_id: "world_provider_created".into(),
                    receive_interface_id: "system_worker_created".into(),
                    return_interface_ids: vec![],
                    model: "chosen".into(),
                    reasoning_effort: "xhigh".into(),
                    service_tier: None,
                },
                files: vec![FileProof {
                    path: "/tmp/native-created.json".into(),
                    before_hash: None,
                    after_hash: ContentHash::of_bytes(b"{}").to_hex(),
                    bytes: 2,
                }],
                seed: NativeEffect {
                    tool: "exact.sender".into(),
                    arguments: json!({"interface":"system_worker_created","label":PEER_MESSAGE_LABEL,"payload":{"action":"followup","control_id":"work-1","text":"task"}}),
                    correlation_id: "seed-1".into(),
                },
                outgoing_bindings: vec![ProviderOutgoingBinding {
                    interface: "system_worker_created".into(),
                    artifact_label: PEER_MESSAGE_LABEL.into(),
                    artifact_bindings: vec!["role-created".into()],
                }],
            },
        }
    }

    #[test]
    fn native_creation_replay_requires_exact_receipts_before_seed_and_never_claims_work() {
        let pending = writing();
        let NativePhase::Writing { files, .. } = &pending.state else {
            unreachable!()
        };
        let receipt = json!(ToolResultArtifact::success(
            "exact.writer",
            Some("write-1".into()),
            json!({"result":"files_written","writes":files})
        ));
        let mut wrong = receipt.clone();
        wrong["body"]["writes"][0]["after_hash"] = json!(ContentHash::of_bytes(b"wrong").to_hex());
        assert!(pending.advance(&wrong).is_err());
        wrong = receipt.clone();
        wrong["correlation_id"] = json!("foreign");
        assert!(pending.advance(&wrong).is_err());
        wrong = receipt.clone();
        wrong["body"]["writes"] = json!([]);
        assert!(pending.advance(&wrong).is_err());
        let restored: NativePending = serde_json::from_value(json!(pending)).unwrap();
        let NativeStep::Effect {
            effect,
            pending: next,
            outgoing_bindings,
        } = restored.advance(&receipt).unwrap()
        else {
            panic!("expected seed")
        };
        assert_eq!(effect.tool, "exact.sender");
        assert_eq!(next.call_id, restored.call_id);
        assert_eq!(next.operation_name, "create_work");
        assert_eq!(
            outgoing_bindings[0].artifact_bindings[0].as_str(),
            "role-created"
        );
        let next: NativePending = serde_json::from_value(json!(next)).unwrap();
        let admitted = json!(ToolResultArtifact::success(
            "exact.sender",
            Some("seed-1".into()),
            json!({"admitted":{"seq":17}})
        ));
        let NativeStep::Complete { result } = next.advance(&admitted).unwrap() else {
            panic!("expected result")
        };
        assert_eq!(result["installed"], true);
        assert_eq!(result["seed_admitted"], true);
        assert_eq!(result["work_completed"], false);
        assert_eq!(result["operation"], "create_work");
        let mut corrupt = admitted;
        corrupt["body"]["admitted"]["other"] = json!(true);
        assert!(next.advance(&corrupt).is_err());
    }

    #[test]
    fn catalog_is_exact_bound_and_never_advertises_ungranted_effects() {
        let dir = tempfile::tempdir().unwrap();
        let reference = ArtifactRef {
            id: "catalog".into(),
            content: ContentHash::of_bytes(b"catalog manifest"),
        };
        let catalog = json!({"id":"catalog","label":"provider.run.affordances","operations":[
            {"name":"queue_work","description":"Queue a message","input_schema":{"type":"object"},"operation":"send_message"},
            {"name":"declared_runs","description":"List recorded bindings","input_schema":{"type":"object"},"operation":"list_runs"}
        ]});
        let metadata: InvocationMetadata = serde_json::from_value(json!({
            "grant":{"artifact_access":{"catalog":{"read":true}},"artifact_verbs":["read"]},"artifact_binding_refs":[reference]
        })).unwrap();
        let mut input = InterfaceInvocationEnvelope {
            artifact: reference.clone(),
            artifact_file: InterfaceArtifactFile::write(dir.path(), "input", b"{}").unwrap(),
            artifact_binding_payloads: vec![ArtifactBindingPayload::new(
                reference.clone(),
                serde_json::to_vec(&catalog).unwrap(),
            )],
            metadata: Some(metadata),
        };
        let config = json!({"run_affordances":{"catalog_artifact":reference,"artifact_send_tool":"selected.sender"}});
        assert!(
            RunAffordances::from_invocation(&input, &json!({}))
                .unwrap()
                .is_none()
        );
        let allowed = RunAffordances::from_invocation(&input, &config)
            .unwrap()
            .unwrap();
        assert!(!allowed.contains("queue_work"));
        assert!(allowed.contains("declared_runs"));
        input
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .tools
            .push("selected.sender".into());
        assert!(RunAffordances::from_invocation(&input, &config).is_err());
        input
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .interface_affordance_specs
            .push(InterfaceToolAffordance {
                name: "selected.sender".into(),
                description: "Send".into(),
                args_schema: json!({"type":"object"}),
            });
        assert!(
            RunAffordances::from_invocation(&input, &config)
                .unwrap()
                .unwrap()
                .contains("queue_work")
        );
        let all_effects: RunAffordanceConfig = serde_json::from_value(json!({
            "catalog_artifact": reference, "artifact_send_tool": "selected.sender",
            "file_write_tool": "selected.writer", "file_read_tool": "selected.reader"
        })).unwrap();
        let mut metadata = input.metadata.clone().unwrap();
        assert!(!metadata.grant.may_create_artifact());
        for id in ["selected.writer", "selected.reader"] {
            metadata.grant.tools.push(id.into());
            metadata.grant.interface_affordance_specs.push(InterfaceToolAffordance {
                name: id.into(), description: id.into(), args_schema: json!({"type":"object"}),
            });
        }
        for (operation, required) in [
            (RunOperation::PreviewCurrentRun, vec!["selected.writer", "selected.reader"]),
            (RunOperation::AmendCurrentRun, vec!["selected.writer", "selected.reader"]),
            (RunOperation::CreateRun, vec!["selected.writer", "selected.sender"]),
            (RunOperation::PreviewRun, vec!["selected.writer", "selected.sender"]),
            (RunOperation::ReviseRun, vec!["selected.writer", "selected.sender"]),
            (RunOperation::PreviewRevision, vec!["selected.writer", "selected.sender"]),
            (RunOperation::SendMessage, vec!["selected.sender"]),
            (RunOperation::FollowupTask, vec!["selected.sender"]),
            (RunOperation::Interrupt, vec!["selected.sender"]),
        ] {
            assert!(operation_granted(operation, &all_effects, &metadata).unwrap());
            for id in required {
                let mut missing = metadata.clone();
                missing.grant.tools.retain(|tool| tool != id);
                assert!(!operation_granted(operation, &all_effects, &missing).unwrap());
                let mut blocked = metadata.clone();
                blocked.grant.hard_blocked_tools.push(id.into());
                assert!(!operation_granted(operation, &all_effects, &blocked).unwrap());
            }
        }
        input.metadata.as_mut().unwrap().artifact_binding_refs[0].content =
            ContentHash::of_bytes(b"different");
        assert!(RunAffordances::from_invocation(&input, &config).is_err());
        assert!(RunAffordances::from_invocation(&input, &json!({"run_affordances":null})).is_err());
        input.metadata.as_mut().unwrap().artifact_binding_refs[0] = reference;
        let mut malformed = catalog;
        malformed["operations"][0]["input_schema"]["properties"] = json!(7);
        input.artifact_binding_payloads[0] = octopus_interfaces::ArtifactBindingPayload::new(
            input.artifact_binding_payloads[0].artifact.clone(),
            serde_json::to_vec(&malformed).unwrap(),
        );
        assert!(RunAffordances::from_invocation(&input, &config).is_err());
    }

    #[test]
    fn peer_contract_rejects_fake_origin_empty_work_and_unbound_targets() {
        assert!(
            serde_json::from_value::<PeerMessage>(json!({"action":"queue","control_id":"c"}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<PeerMessage>(
                json!({"action":"queue","control_id":"c","text":"hello","source_run":"forged"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<PeerMessage>(json!({"action":"interrupt","control_id":"c"}))
                .is_ok()
        );
        assert!(serde_json::from_value::<PeerMessage>(json!({"action":"queue","control_id":"c","artifacts":[{"id":"a","content":ContentHash::of_bytes(b"a"),"source_run":"forged"}]})).is_err());
        let mut metadata: InvocationMetadata = serde_json::from_value(json!({"grant":{
            "interfaces":["system_worker_peer"],"attached_interface_bindings":[{"interface":"system_worker_peer","interface_type":"system","role":"worker","direction":"outbound"}]},
            "outbound_interface_bindings":["world_provider"],
            "checkpoint_binding_policy":{"inbound":{"interface":"system_worker_source","config":{"artifact_policy":{"mode":"all"}}},"outbound":[{"interface":"world_provider","config":{"artifact_policy":{"mode":"all"}}}]}
        })).unwrap();
        let mut record: RunRecord = serde_json::from_value(json!({
            "id":"source","ee":"scope","worker":"system_worker_source","trace":"trace","status":"running",
            "checkpoints":[{"id":"send","run":"source","trace":"trace","epoch":0,"interface_binding":{"interface":"world_provider","config":{"artifact_policy":{"mode":"all"}}},"outbound_interface_bindings":[{"interface":"system_worker_peer","config":{"artifact_policy":{"mode":"all"}}}]}],
            "active_checkpoints":["send"]
        })).unwrap();
        assert!(require_peer_target(&metadata, &record, "system_worker_peer", None).is_ok());
        let reference = ArtifactRef { id: "catalog".into(), content: ContentHash::of_bytes(b"catalog") };
        metadata.artifact_binding_refs = vec![reference.clone()];
        metadata.grant.artifact_access.insert("catalog".into(), serde_json::from_value(json!({"read":true})).unwrap());
        metadata.grant.artifact_verbs = serde_json::from_value(json!(["create"])).unwrap();
        metadata.grant.tools = vec!["selected.sender".into()];
        metadata.grant.interface_affordance_specs = vec![InterfaceToolAffordance {
            name: "selected.sender".into(), description: "Send".into(), args_schema: json!({"type":"object"}),
        }];
        metadata.runtime = Some(serde_json::from_value(json!({"run":"source","ee":"scope","worker":"system_worker_source",
            "checkpoint":"input","interface":"world_provider","epoch":0})).unwrap());
        for operation in [RunOperation::SendMessage, RunOperation::FollowupTask, RunOperation::Interrupt] {
            let affordances = RunAffordances {
                config: RunAffordanceConfig { catalog_artifact: reference.clone(), file_write_tool: None, file_read_tool:None,
                    artifact_send_tool: Some("selected.sender".into()), roots: None, defaults: None },
                catalog: RunCatalog { id: "catalog".into(), label: "provider.run.affordances".into(), text: None,
                    operations: vec![RunCatalogEntry { name: "peer_action".into(), description: "Peer action".into(),
                        input_schema: json!({"type":"object"}), operation }] },
                enabled: BTreeSet::from(["peer_action".into()]),
            };
            let NativeStep::Effect { effect, outgoing_bindings, .. } = affordances.plan_call(
                &metadata, &record, &record, "call", "peer_action", &json!({"interface":"system_worker_peer","text":"hello"})
            ).unwrap() else { panic!("expected ordinary effect"); };
            assert_eq!(effect.tool, "selected.sender");
            assert_eq!(outgoing_bindings.len(), 1);
            assert_eq!(outgoing_bindings[0].interface.as_str(), "system_worker_peer");
            assert_eq!(outgoing_bindings[0].artifact_label, PEER_MESSAGE_LABEL);
            assert!(outgoing_bindings[0].artifact_bindings.is_empty());
            let mut unbound = record.clone(); unbound.active_checkpoints.clear();
            assert!(affordances.plan_call(&metadata, &record, &unbound, "call", "peer_action",
                &json!({"interface":"system_worker_peer","text":"hello"})).is_err());
        }
        record.active_checkpoints.clear();
        assert!(require_peer_target(&metadata, &record, "system_worker_peer", None).is_err());
        assert_ne!(
            call_identity("ab", "c").unwrap(),
            call_identity("a", "bc").unwrap()
        );
        assert_ne!(
            call_identity("run-a", "call").unwrap(),
            call_identity("run-b", "call").unwrap()
        );
        assert_eq!(
            call_identity("run-a", "call").unwrap(),
            call_identity("run-a", "call").unwrap()
        );
    }

    #[test]
    fn file_batch_keeps_new_file_preconditions_and_excludes_parent() {
        let write = PlannedClientWrite {
            path: "/tmp/new-run.json".into(),
            content: "{}\n".into(),
            tag: "run".into(),
            expected_before_hash: None,
        };
        let (batch, proofs) =
            file_write_batch(std::slice::from_ref(&write), Path::new("/tmp/parent.json")).unwrap();
        assert!(
            batch["writes"][0]
                .as_object()
                .unwrap()
                .contains_key("expected_before_hash")
        );
        assert!(batch["writes"][0]["expected_before_hash"].is_null());
        assert_eq!(proofs[0].bytes, 3);
        assert!(file_write_batch(std::slice::from_ref(&write), &write.path).is_err());
        assert!(file_write_batch(&[write.clone(), write], Path::new("/tmp/parent.json")).is_err());
    }
}
