use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write};
use std::path::{Component, Path, PathBuf};

use super::{BoundCheckpointRole, BoundRunAuthoringTopology};
use octopus_descriptors::{EeId, InterfaceId, RunId};
use octopus_file_storage::publication::{
    DigestAlgorithm, DirectoryEntryKind, DirectoryMove, DirectoryMoveReceipt, DirectoryPreimage,
    DirectoryPublicationResult, FileIdentity, PublicationOutcome, publish_directory_moves, reclaim_directory, snapshot_directory,
};
use octopus_interface_transport::{InterfaceLifeOwnership, SpawnLifeConfig};
use octopus_process_session_tool_runtime::{ToolRetirementRequest, tool_retirement_quiescent};
use octopus_run_authoring::{
    CheckedRunEvidence, RunAuthoringProfile, declared_provider_run_writer,
    provider_input_payloads_ready,
};
use octopus_run_record::RunRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementSelection {
    pub schema_version: u32,
    pub source_run: RunId,
    pub interface_ids: Vec<InterfaceId>,
    pub records: Vec<TaskRetirementRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementRecord {
    pub run: RunId,
    pub file: PathBuf,
    pub expected_digest: String,
    pub worker: InterfaceId,
    pub ee: EeId,
    pub role: String,
    pub provider_interface: InterfaceId,
    pub target_run: RunId,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementRequest {
    pub registry_root: PathBuf,
    pub run_records_root: PathBuf,
    pub ledger_root: PathBuf,
    pub interface_private_state_root: PathBuf,
    pub selection: TaskRetirementSelection,
    pub original_records: BTreeMap<RunId, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementProof {
    pub schema_version: u32,
    pub selection_digest: String,
    pub preimages: Vec<TaskRetirementPreimage>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementPreimage {
    pub path: PathBuf,
    pub sha256: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementRoots {
    pub registry_root: PathBuf,
    pub run_records_root: PathBuf,
    pub ledger_root: PathBuf,
    pub interface_private_state_root: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementOriginal {
    pub file: PathBuf,
    pub expected_digest: String,
    pub content: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementFilePin {
    pub file: PathBuf,
    pub expected_digest: Option<String>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementLedgerBoundary {
    pub complete_offset: u64,
    pub sequence: u64,
    pub sha256: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldRetirementProof {
    pub schema_version: u32,
    pub phase: String,
    pub operation_id: String,
    pub selection_digest: String,
    pub directory_inventory_digest: String,
    pub interface_ids: Vec<InterfaceId>,
    pub originals: Vec<RetirementFilePin>,
    pub retired: Vec<RetirementFilePin>,
    pub ledger_boundary: RetirementLedgerBoundary,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetirementContext {
    pub operation_id: String,
    pub roots: TaskRetirementRoots,
    pub selection: TaskRetirementSelection,
    pub selection_digest: String,
    pub originals: Vec<RetirementOriginal>,
    pub retired: Vec<RetirementFilePin>,
    pub directory_inventory: RetirementDirectoryInventory,
    pub prior_proof: Option<HeldRetirementProof>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementDirectoryInventory {
    pub schema_version: u32,
    pub removals: Vec<RetirementDirectoryPin>,
    pub originals: Vec<RetirementOriginal>,
    pub retired: Vec<RetirementFilePin>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementDirectoryPin {
    pub path: PathBuf,
    pub identity: FileIdentity,
}

fn selection_digest(selection: &TaskRetirementSelection) -> Result<String, String> {
    Ok(digest(&octopus_descriptors::canonical_bytes(
        &serde_json::to_value(selection).map_err(|error| error.to_string())?,
    )))
}

impl TaskRetirementContext {
    fn request(&self) -> Result<TaskRetirementRequest, String> {
        component(&self.operation_id)?;
        if self.operation_id.is_empty()
            || self.selection_digest != selection_digest(&self.selection)?
            || self.originals.len() != self.selection.records.len()
            || self.retired.len() != self.originals.len()
        {
            return Err(
                "retirement context has a different operation, selection or write set".into(),
            );
        }
        let mut originals = BTreeMap::new();
        for original in &self.originals {
            let selected = self
                .selection
                .records
                .iter()
                .find(|record| record.file == original.file)
                .ok_or("retirement before image is outside its selection")?;
            if original.expected_digest != selected.expected_digest
                || digest(original.content.as_bytes()) != original.expected_digest
                || originals
                    .insert(selected.run.clone(), original.content.clone())
                    .is_some()
            {
                return Err("retirement before image changed or repeats a selected record".into());
            }
        }
        let retired = self
            .retired
            .iter()
            .map(|pin| &pin.file)
            .collect::<BTreeSet<_>>();
        if retired.len() != self.retired.len()
            || self.retired.iter().any(|pin| {
                pin.expected_digest.is_some()
                    || !self
                        .originals
                        .iter()
                        .any(|original| original.file == pin.file)
            })
        {
            return Err("retirement targets must be the exact removed run records".into());
        }
        Ok(TaskRetirementRequest {
            registry_root: self.roots.registry_root.clone(),
            run_records_root: self.roots.run_records_root.clone(),
            ledger_root: self.roots.ledger_root.clone(),
            interface_private_state_root: self.roots.interface_private_state_root.clone(),
            selection: self.selection.clone(),
            original_records: originals,
        })
    }

    fn proof(&self, ledger_boundary: RetirementLedgerBoundary) -> Result<HeldRetirementProof, String> {
        let originals = self
            .originals
            .iter()
            .map(|original| RetirementFilePin {
                file: original.file.clone(),
                expected_digest: Some(original.expected_digest.clone()),
            })
            .collect::<Vec<_>>();
        let retired = self.retired.clone();
        let interface_ids = self.selection.interface_ids.clone();
        Ok(HeldRetirementProof {
            schema_version: 2,
            phase: "held".into(),
            operation_id: self.operation_id.clone(),
            selection_digest: self.selection_digest.clone(),
            directory_inventory_digest: digest(&octopus_descriptors::canonical_bytes(
                &serde_json::to_value(&self.directory_inventory).map_err(|error| error.to_string())?,
            )),
            interface_ids,
            originals,
            retired,
            ledger_boundary,
        })
    }

    pub fn verify_before(&self) -> Result<(), String> {
        if self.prior_proof.is_some() {
            return Err("retirement preflight cannot substitute for a retained held proof".into());
        }
        self.request()?.inspect(false)?;
        RetirementDirectoryScope::new(self, false)?;
        Ok(())
    }
}

#[derive(Default)]
struct RetirementLedgerCache {
    boundary: Option<RetirementLedgerBoundary>,
    prefix: Vec<u8>,
    hasher: Sha256,
    metadata_lines: BTreeMap<octopus_ledger::ContentHash, (octopus_ledger::DurableMetadataContent, usize)>,
}

fn ledger_boundary(
    request: &TaskRetirementRequest,
    prior: Option<&RetirementLedgerBoundary>,
    cache: &mut RetirementLedgerCache,
) -> Result<RetirementLedgerBoundary, String> {
    let path = request.ledger_root.join("movements.jsonl");
    let bytes = regular(&path, None)?.unwrap_or_default();
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err("retirement ledger has an incomplete tail".into());
    }
    let reuse = match (&cache.boundary, prior) {
        (None, _) => false,
        (Some(checked), Some(prior)) if checked == prior => true,
        _ => return Err("retirement ledger cache differs from its held proof".into()),
    };
    let mut hasher = if reuse { cache.hasher.clone() } else { Sha256::new() };
    let prior_offset = match prior {
        Some(prior) => {
            let offset = usize::try_from(prior.complete_offset)
                .map_err(|_| "retirement prefix exceeds host range")?;
            let unchanged = offset <= bytes.len()
                && (offset == 0 || bytes[offset - 1] == b'\n')
                && if reuse {
                    bytes[..offset] == cache.prefix[..]
                } else {
                    hasher.update(&bytes[..offset]);
                    format!("{:x}", hasher.clone().finalize()) == prior.sha256
                };
            if !unchanged {
                return Err("retirement ledger prefix changed after the held proof".into());
            }
            offset
        }
        None => 0,
    };
    let interfaces = request
        .selection
        .interface_ids
        .iter()
        .collect::<BTreeSet<_>>();
    let runs = request
        .selection
        .records
        .iter()
        .map(|record| &record.run)
        .collect::<BTreeSet<_>>();
    let decoder = octopus_ledger_store::DurableMovementDecoder::for_log(&path);
    if reuse {
        for (content, (kind, line)) in &cache.metadata_lines {
            decoder.validate_metadata_content(*kind, *content, *line).map_err(|error| error.to_string())?;
        }
    }
    let mut sequence = if reuse { prior.unwrap().sequence } else { 0 };
    let mut offset = if reuse { prior_offset } else { 0 };
    for line in bytes[offset..].split_inclusive(|byte| *byte == b'\n') {
        let (movement, _) = decoder
            .decode_projection(line, sequence as usize)
            .map_err(|error| error.to_string())?;
        let row: Value = serde_json::from_slice(line).map_err(|error| error.to_string())?;
        for (field, kind) in [
            ("grant_content", octopus_ledger::DurableMetadataContent::Grant),
            ("artifact_binding_refs_content", octopus_ledger::DurableMetadataContent::ArtifactBindingRefs),
        ] {
            if let Some(content) = row.get("invocation_metadata").and_then(|metadata| metadata.get(field)) {
                let hash = content.as_str().and_then(octopus_ledger::ContentHash::from_hex)
                    .ok_or_else(|| format!("retirement metadata {field} is not a content address"))?;
                cache.metadata_lines.entry(hash).or_insert((kind, sequence as usize));
            }
        }
        if movement.seq.0 != sequence {
            return Err("retirement ledger is not contiguous".into());
        }
        if offset == prior_offset && prior.is_some_and(|prior| prior.sequence != sequence) {
            return Err("retirement ledger prefix has a different sequence boundary".into());
        }
        if prior.is_some() && offset >= prior_offset {
            let octopus_ledger::Endpoint::Interface(from) = &movement.from;
            let octopus_ledger::Endpoint::Interface(to) = &movement.to;
            if interfaces.contains(from)
                || interfaces.contains(to)
                || movement
                    .runtime()
                    .is_some_and(|runtime| runs.contains(&runtime.run))
            {
                return Err(
                    "retired task acquired new movement evidence after its held proof".into(),
                );
            }
        }
        offset += line.len();
        sequence += 1;
    }
    if offset == prior_offset && prior.is_some_and(|prior| prior.sequence != sequence) {
        return Err("retirement ledger ends at a different sequence boundary".into());
    }
    hasher.update(&bytes[prior_offset..]);
    let boundary = RetirementLedgerBoundary {
        complete_offset: bytes.len() as u64,
        sequence,
        sha256: format!("{:x}", hasher.clone().finalize()),
    };
    cache.boundary = Some(boundary.clone());
    cache.prefix = bytes;
    cache.hasher = hasher;
    Ok(boundary)
}

fn verify_held(
    context: &TaskRetirementContext,
    request: &TaskRetirementRequest,
    previous: Option<&HeldRetirementProof>,
    cache: &mut RetirementLedgerCache,
) -> Result<HeldRetirementProof, String> {
    request.validate(true)?;
    request.assert_unbound()?;
    let boundary = if let Some(previous) = previous {
        if context.proof(previous.ledger_boundary.clone())? != *previous {
            return Err("retirement held proof belongs to another exact transaction".into());
        }
        ledger_boundary(request, Some(&previous.ledger_boundary), cache)?
    } else {
        let proof = request.inspect(true)?;
        let boundary = ledger_boundary(request, None, cache)?;
        if proof
            .preimages
            .iter()
            .find(|pin| pin.path == request.ledger_root.join("movements.jsonl"))
            .and_then(|pin| pin.sha256.as_ref())
            .is_some_and(|hash| hash != &boundary.sha256)
        {
            return Err("retirement ledger advanced while capturing its held proof".into());
        }
        proof.assert_unchanged()?;
        boundary
    };
    request.validate(true)?;
    request.assert_unbound()?;
    context.proof(boundary)
}

const RETIREMENT_FRAME_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum RetirementCommand {
    AssertHeld {},
    Release {},
    SnapshotDirectory {
        transaction_id: String,
        path: PathBuf,
    },
    PublishDirectoryMoves {
        transaction_id: String,
        operation_id: String,
        moves: Vec<DirectoryMove>,
    },
    ReclaimDirectory {
        transaction_id: String,
        tombstone: PathBuf,
        receipt: DirectoryMoveReceipt,
        expected: DirectoryPreimage,
    },
}

#[derive(Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum RetirementDirectoryResponse {
    DirectorySnapshot {
        schema_version: u32,
        operation_id: String,
        proof: HeldRetirementProof,
        path: PathBuf,
        preimage: DirectoryPreimage,
    },
    DirectoryPublication {
        schema_version: u32,
        operation_id: String,
        proof: HeldRetirementProof,
        publication_operation_id: String,
        result: DirectoryPublicationResult,
    },
    DirectoryReclaimed {
        schema_version: u32,
        operation_id: String,
        proof: HeldRetirementProof,
        tombstone: PathBuf,
        receipt: DirectoryMoveReceipt,
    },
}

fn retirement_identity(metadata: &std::fs::Metadata) -> Result<FileIdentity, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(FileIdentity { device: metadata.dev(), inode: metadata.ino() })
    }
    #[cfg(not(unix))]
    { let _ = metadata; Err("retirement directory identity requires native filesystem support".into()) }
}

fn inventory_id(value: &Value) -> Result<&str, String> {
    let id = value.as_str().ok_or("retirement inventory identity must be a string")?;
    component(id)?;
    if id.contains('/') || id.trim() != id || id.chars().any(char::is_control) {
        return Err("retirement inventory identity must be one ordinary path component".into());
    }
    Ok(id)
}

fn inventory_ids(value: &Value) -> Result<BTreeSet<String>, String> {
    let values = value.as_array().ok_or("retirement inventory must be an exact array")?;
    let ids = values.iter().map(|id| inventory_id(id).map(str::to_owned)).collect::<Result<BTreeSet<_>, _>>()?;
    if ids.len() != values.len() { return Err("retirement inventory repeats an owner".into()); }
    Ok(ids)
}

fn prediction_interfaces(registry: &Path, record: &Value, source: &str, provider: &str) -> Result<BTreeSet<String>, String> {
    let config_path = registry.join("interface/world_operator_chat/composer-predictions.json");
    let config: Value = serde_json::from_slice(&regular(&config_path, Some(16384))?
        .ok_or("retirement suggestion contract is absent")?).map_err(|error| error.to_string())?;
    let client = inventory_id(&config["client_template"])?;
    let worker_template = inventory_id(&config["worker_template"])?;
    let suffix = digest(&serde_json::to_vec(&[source, provider]).map_err(|error| error.to_string())?)[..24].to_owned();
    let run = format!("run_client_maintenance_{suffix}");
    let input = format!("{client}_input_{suffix}");
    let result = format!("{client}_result_{suffix}");
    let worker = format!("{worker_template}_{suffix}");
    let state = &record["interface_state"][&input]["composer_prediction"];
    let request = &state["request"];
    let epoch = state["epoch"].as_u64().filter(|value| *value <= 9_007_199_254_740_991)
        .ok_or("retirement suggestion generation is malformed")?;
    let sequence = request["source_movement_seq"].as_u64().filter(|value| *value <= 9_007_199_254_740_991)
        .ok_or("retirement suggestion source sequence is malformed")?;
    let reference = &request["source"];
    if request.as_object().is_none_or(|object| object.len() != 4)
        || reference.as_object().is_none_or(|object| object.len() != 2)
        || reference["id"].as_str().is_none_or(str::is_empty)
        || reference["content"].as_array().is_none_or(|bytes| bytes.len() != 32 || bytes.iter().any(|byte| byte.as_u64().is_none_or(|byte| byte > 255)))
        || request["request_id"] != format!("prediction-{sequence}") || request["reply_to"] != result
    { return Err("retirement suggestion request ownership is malformed".into()); }
    let binding = |id: &str, label: &str| serde_json::json!({"interface":id,
        "config":{"artifact_policy":{"mode":"allow","names":[label],"namespaces":[]}}});
    let input_label = inventory_id(&config["request_label"])?;
    let result_label = inventory_id(&config["result_label"])?;
    let expected = serde_json::json!({"id":run,"ee":inventory_id(&record["ee"])?,"worker":worker,
        "trace":format!("trace:{run}"),"status":"running",
        "interface_state":{&input:{"composer_prediction":{"source_run":source,"request":request,"epoch":epoch}}},
        "checkpoints":[
            {"id":"cp_input","run":run,"trace":format!("trace:{run}:input:{epoch}"),"epoch":epoch,
                "interface_binding":binding(&input,input_label),"outbound_interface_bindings":[binding(provider,input_label)],
                "artifact_bindings":[{"id":reference["id"]}],"required_verified_artifacts":[]},
            {"id":"cp_result","run":run,"trace":format!("trace:{run}:result:{epoch}"),"epoch":epoch,
                "interface_binding":binding(&result,result_label),"outbound_interface_bindings":[],
                "artifact_bindings":[],"required_verified_artifacts":[]}],"active_checkpoints":["cp_input","cp_result"]});
    if *record != expected { return Err("retirement suggestion differs from its exact source ownership".into()); }
    Ok(BTreeSet::from([input, result, worker]))
}

fn title_interfaces(registry: &Path, record: &Value, source: &str, source_provider: &str, provider: &str) -> Result<BTreeSet<String>, String> {
    let config: Value = serde_json::from_slice(&regular(&registry.join("interface/world_operator_chat/task-titles.json"), Some(16384))?
        .ok_or("retirement title contract is absent")?).map_err(|error| error.to_string())?;
    let client = inventory_id(&config["client_template"])?;
    let worker_template = inventory_id(&config["worker_template"])?;
    let provider_template = inventory_id(&config["provider_template"])?;
    let suffix = digest(&serde_json::to_vec(&[source, source_provider]).map_err(|error| error.to_string())?)[..24].to_owned();
    let run = format!("run_client_title_{suffix}");
    let input = format!("{client}_title_input_{suffix}");
    let result = format!("{client}_title_result_{suffix}");
    let worker = format!("{worker_template}_title_{suffix}");
    if provider != format!("{provider_template}_title_{suffix}") {
        return Err("retirement title belongs to another source provider".into());
    }
    let state = &record["interface_state"][&input]["task_title"];
    let request = &state["request"];
    let sequence = request["source_movement_seq"].as_u64().filter(|value| *value <= 9_007_199_254_740_991)
        .ok_or("retirement title source sequence is malformed")?;
    let reference = &request["source"];
    if request.as_object().is_none_or(|object| object.len() != 7)
        || reference.as_object().is_none_or(|object| object.len() != 2)
        || reference["id"].as_str().is_none_or(str::is_empty)
        || reference["content"].as_array().is_none_or(|bytes| bytes.len() != 32 || bytes.iter().any(|byte| byte.as_u64().is_none_or(|byte| byte > 255)))
        || request["request_id"] != format!("title-{sequence}") || request["reply_to"] != result
        || request["text_pointer"].as_str().is_none_or(str::is_empty)
        || !request["instructions"].is_string() || !request["output_schema"].is_object()
    { return Err("retirement title request ownership is malformed".into()); }
    let expected_files = [(&input, vec!["interface.json", "activation.json", "life.json"]),
        (&result, vec!["interface.json", "activation.json", "life.json"]),
        (&worker, vec!["interface.json", "policy.json"]), (&provider.to_owned(), vec!["interface.json", "life.json"])]
        .into_iter().flat_map(|(id, names)| names.into_iter().map(move |name| format!("interface/{id}/{name}"))).collect::<BTreeSet<_>>();
    let files = state["files"].as_object().ok_or("retirement title has no ownership inventory")?;
    if files.keys().cloned().collect::<BTreeSet<_>>() != expected_files
        || files.values().any(|value| value.as_str().is_none_or(|value| value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())))
    { return Err("retirement title ownership inventory escapes its private interfaces".into()); }
    let binding = |id: &str, label: &str| serde_json::json!({"interface":id,
        "config":{"artifact_policy":{"mode":"allow","names":[label],"namespaces":[]}}});
    let input_label = inventory_id(&config["request_label"])?;
    let result_label = inventory_id(&config["result_label"])?;
    let completed = record["status"] == "idle";
    let expected = serde_json::json!({"id":run,"ee":inventory_id(&record["ee"])?,"worker":worker,
        "trace":format!("trace:{run}"),"status":if completed {"idle"} else {"running"},
        "interface_state":{&input:{"task_title":{"source_run":source,"source_provider":source_provider,"request":request,"files":files}}},
        "checkpoints":[
            {"id":"cp_input","run":run,"trace":format!("trace:{run}:input"),"epoch":0,
                "interface_binding":binding(&input,input_label),"outbound_interface_bindings":[binding(provider,input_label)],
                "artifact_bindings":[{"id":reference["id"]}],"required_verified_artifacts":[]},
            {"id":"cp_result","run":run,"trace":format!("trace:{run}:result"),"epoch":0,
                "interface_binding":binding(&result,result_label),"outbound_interface_bindings":[],
                "artifact_bindings":[],"required_verified_artifacts":[]}],
        "active_checkpoints":if completed {serde_json::json!([])} else {serde_json::json!(["cp_input","cp_result"])}});
    if *record != expected { return Err("retirement title differs from its exact source ownership".into()); }
    Ok(BTreeSet::from([input, result, worker, provider.to_owned()]))
}

fn client_companion_interfaces(registry: &Path, record: &Value, row: &TaskRetirementRecord, source: &TaskRetirementRecord) -> Result<BTreeSet<String>, String> {
    if row.target_run != source.run { return Err("retirement client companion belongs to another source".into()); }
    if row.role == "title" {
        title_interfaces(registry, record, source.run.as_str(), source.provider_interface.as_str(), row.provider_interface.as_str())
    } else if row.role == "prediction" && row.provider_interface == source.provider_interface {
        prediction_interfaces(registry, record, source.run.as_str(), row.provider_interface.as_str())
    } else { Err("retirement client companion belongs to another source provider".into()) }
}

fn record_interfaces(record: &Value, depth: usize) -> Result<BTreeSet<String>, String> {
    if depth > 2 { return Err("retirement private writer topology is recursively nested".into()); }
    let mut ids = BTreeSet::from([inventory_id(&record["worker"])?.to_owned()]);
    let states = record["interface_state"].as_object().ok_or("retirement record has no exact interface state")?;
    let mut checkpoints = record["checkpoints"].as_array().ok_or("retirement record has no checkpoints")?.iter().collect::<Vec<_>>();
    for (owner, state) in states {
        inventory_id(&Value::String(owner.clone()))?;
        ids.insert(owner.clone());
        if let Some(topology) = state.get("run_authoring_binding") {
            checkpoints.extend(topology["checkpoints"].as_array().ok_or("retirement private topology has no checkpoints")?);
        }
        if let Some(writer) = state.pointer("/run_authoring_binding/writer_run").or_else(|| state.get("writer_run")) {
            let initial = &writer["initial_record"];
            if initial["id"] != writer["run"] || initial["worker"] != record["worker"]
                || initial["interface_state"][owner]["file_writer_target"]["run"] != record["id"]
            { return Err("retirement private writer changed its declared target".into()); }
            ids.extend(record_interfaces(initial, depth + 1)?);
        }
    }
    for checkpoint in checkpoints {
        if let Some(binding) = checkpoint.get("interface_binding").filter(|value| !value.is_null()) {
            ids.insert(inventory_id(&binding["interface"])?.to_owned());
        }
        for binding in checkpoint["outbound_interface_bindings"].as_array().ok_or("retirement checkpoint has no exact returns")? {
            ids.insert(inventory_id(&binding["interface"])?.to_owned());
        }
        if let Some(owner) = checkpoint.get("owner_authoring").filter(|value| !value.is_null()) {
            for key in ["owner", "source", "door"] { ids.insert(inventory_id(&owner[key])?.to_owned()); }
        }
    }
    Ok(ids)
}

struct RetirementDirectoryEvidence {
    documents: BTreeMap<PathBuf, Value>,
    used: BTreeSet<PathBuf>,
    records: BTreeMap<String, Value>,
    source: String,
    client: Option<String>,
    companions: BTreeSet<String>,
    tools: BTreeSet<String>,
    collection: Option<String>,
}

impl RetirementDirectoryEvidence {
    fn new(context: &TaskRetirementContext) -> Result<Self, String> {
        let mut documents = BTreeMap::new();
        for original in &context.directory_inventory.originals {
            let value: Value = serde_json::from_str(&original.content).map_err(|error| error.to_string())?;
            if !value.is_object() || digest(original.content.as_bytes()) != original.expected_digest
                || documents.insert(original.file.clone(), value).is_some()
            { return Err("retirement directory evidence changed bytes or repeats a file".into()); }
        }
        let records = context.originals.iter().map(|original| {
            let value: Value = serde_json::from_str(&original.content).map_err(|error| error.to_string())?;
            Ok((inventory_id(&value["id"])?.to_owned(), value))
        }).collect::<Result<BTreeMap<_, _>, String>>()?;
        let source_id = context.selection.source_run.to_string();
        let source = records.get(&source_id).ok_or("retirement directory inventory lost its source")?;
        let selected = context.selection.records.iter().find(|record| record.role == "source").ok_or("retirement inventory has no source")?;
        let states = source["interface_state"].as_object().ok_or("retirement source has no state")?;
        let clients = states.iter().filter(|(_, state)| state["provider_runtime_interface"].as_str() == Some(selected.provider_interface.as_str()))
            .map(|(id, _)| id.clone()).collect::<Vec<_>>();
        let client = match clients.as_slice() { [client] => Some(client.clone()), [] => None,
            _ => return Err("retirement source has ambiguous client ownership".into()) };
        let declarations = states.iter().filter_map(|(owner, state)| state.get("maintenance_observer").map(|d| (owner, d))).collect::<Vec<_>>();
        let mut companions = BTreeSet::new();
        let mut tools = BTreeSet::new();
        let mut collection = None;
        if let [(owner, declaration)] = declarations.as_slice() {
            let d = *declaration;
            if owner.as_str() != selected.provider_interface.as_str() || d["schema_version"] != 1
                || d["source_run"] != source["id"] || d["source_worker_interface"] != source["worker"]
                || d["source_provider_interface"].as_str() != Some(owner.as_str()) || d["source_client_interface"].as_str() != client.as_deref()
            { return Err("retirement maintenance inventory changed its accepted source".into()); }
            let ownership = &d["lifecycle_ownership"];
            if ownership["schema_version"] != 1 || ownership.as_object().is_none_or(|object| object.len() != 3) {
                return Err("retirement maintenance inventory has a different schema".into());
            }
            companions = inventory_ids(&ownership["private_interface_ids"])?;
            tools = inventory_ids(&ownership["private_tool_ids"])?;
            collection = Some(inventory_id(&d["collection_source"])?.to_owned());
            let mut expected = BTreeSet::new();
            for key in ["source_relay_interface", "experience_interface", "outcome_interface", "capture_interface"] {
                expected.insert(inventory_id(&d[key])?.to_owned());
            }
            for (role, declaration) in [("observer", d), ("projection", &d["projection_run"])] {
                let id = inventory_id(&declaration["run"])?;
                let provider = inventory_id(&declaration["provider_interface"])?;
                let initial = &declaration["initial_record"];
                let bytes = declaration["initial_record_content"].as_str().ok_or("retirement companion has no accepted initial bytes")?;
                let current = records.get(id).ok_or("retirement companion is outside the saved original selection")?;
                let row = context.selection.records.iter().find(|row| row.run.as_str() == id).ok_or("retirement companion is not selected")?;
                if serde_json::from_str::<Value>(bytes).map_err(|error| error.to_string())? != *initial
                    || initial["id"] != declaration["run"] || initial["worker"] != declaration["worker_interface"]
                    || current["worker"] != initial["worker"] || row.role != role
                    || row.provider_interface.as_str() != provider || initial["interface_state"][provider]["run_authoring_binding"].is_null()
                    || current["interface_state"][provider]["run_authoring_binding"] != initial["interface_state"][provider]["run_authoring_binding"]
                    || !record_interfaces(current, 0)?.is_subset(&companions)
                { return Err("retirement companion changed its accepted ownership topology".into()); }
                expected.extend(record_interfaces(initial, 0)?);
            }
            if expected != companions || !tools.contains(inventory_id(&d["publication_tool"])?)
                || [source["worker"].as_str(), Some(owner.as_str()), client.as_deref()].into_iter().flatten().any(|id| companions.contains(id))
                || companions.iter().any(|id| !context.selection.interface_ids.iter().any(|selected| selected.as_str() == id))
            { return Err("retirement private inventory differs from accepted companion owners".into()); }
        } else if !declarations.is_empty() {
            return Err("retirement source has ambiguous maintenance ownership".into());
        }
        for row in context.selection.records.iter().filter(|row| matches!(row.role.as_str(), "prediction" | "title")) {
            let record = records.get(row.run.as_str()).ok_or("retirement client companion record is absent")?;
            let owned = client_companion_interfaces(&context.roots.registry_root, record, row, selected)?;
            if owned.iter().any(|id| companions.contains(id) || !context.selection.interface_ids.iter().any(|selected| selected.as_str() == id)) {
                return Err("retirement suggestion overlaps or escapes its exact private inventory".into());
            }
            companions.extend(owned);
        }
        Ok(Self { documents, used: BTreeSet::new(), records, source: source_id, client, companions, tools, collection })
    }

    fn read(&mut self, path: PathBuf) -> Result<Value, String> {
        let value = self.documents.get(&path).ok_or_else(|| format!("retirement inventory lacks ownership evidence '{}'", path.display()))?.clone();
        self.used.insert(path);
        Ok(value)
    }

    fn owner(&mut self, context: &TaskRetirementContext, owner: &str) -> Result<(), String> {
        if !context.selection.interface_ids.iter().any(|id| id.as_str() == owner) {
            return Err("retirement directory belongs to an unselected interface".into());
        }
        if self.companions.contains(owner) { return Ok(()); }
        let source = self.records[&self.source].clone();
        let intakes = source["interface_state"].as_object().ok_or("retirement source has no private state")?.iter()
            .filter(|(id, state)| state.get("input_intake").is_some()
                && (id.as_str() == owner || state["input_intake"]["writer"]["door"].as_str() == Some(owner)))
            .collect::<Vec<_>>();
        if intakes.len() > 1 { return Err("retirement input ownership is ambiguous".into()); }
        if let Some((id, state)) = intakes.first() {
            let interface = InterfaceId::from(id.as_str());
            let config: crate::input_intake::InputIntakeConfig = serde_json::from_value(state["input_intake"].clone())
                .map_err(|error| error.to_string())?;
            config.validate(&interface)?;
            let selected = context.selection.records.iter().find(|row| row.role == "source").ok_or("retirement source is absent")?;
            let intake = self.read(context.roots.registry_root.join("interface").join(id).join("interface.json"))?;
            let provider = self.read(context.roots.registry_root.join("interface").join(config.provider_interface.as_str()).join("interface.json"))?;
            let worker = self.read(context.roots.registry_root.join("interface").join(selected.worker.as_str()).join("policy.json"))?;
            if config.run != self.source || config.provider_interface != selected.provider_interface
                || intake["id"].as_str() != Some(id.as_str()) || intake["interface_driver_ref"] != "checked_input_intake"
                || !config.matches_declaration(&intake["interface_driver_config"]["private_config"]["input_intake"])?
                || provider["id"].as_str() != Some(config.provider_interface.as_str())
                || provider["interface_driver_config"]["private_config"]["input_intake"]["interface_id"].as_str() != Some(id.as_str())
                || worker["id"].as_str() != Some(selected.worker.as_str())
                || [id.as_str(), config.writer.door.as_str(), config.provider_interface.as_str()].iter().any(|pin|
                    worker["ceiling"]["interfaces"].as_array().is_none_or(|pins| !pins.iter().any(|value| value.as_str() == Some(*pin))))
                || worker["ceiling"]["tools"].as_array().is_none_or(|tools| !tools.iter().any(|tool| tool.as_str() == Some(config.writer.tool.as_str())))
            { return Err("retirement input writer differs from its exact source ownership".into()); }
            return Ok(());
        }
        let client = self.client.clone().ok_or("retirement source has no declared private client owner")?;
        let sidecar: super::OctopusOsInterfaceSidecar = serde_json::from_value(self.read(
            context.roots.registry_root.join("interface").join(owner).join("octopus_os.json"),
        )?).map_err(|error| error.to_string())?;
        if sidecar.schema_version != 1 || sidecar.workspace != "octopus_os" || sidecar.interface != owner || sidecar.owner_interface != client {
            return Err("retirement directory owner differs from the saved source client".into());
        }
        Ok(())
    }

    fn directory(&mut self, context: &TaskRetirementContext, path: &Path) -> Result<PathBuf, String> {
        let registry = &context.roots.registry_root;
        for kind in ["tool", "artifact"] {
            let Ok(relative) = path.strip_prefix(registry.join(kind)) else { continue; };
            let id = relative.to_str().ok_or("retirement directory id is not UTF-8")?;
            inventory_id(&Value::String(id.into()))?;
            let file = path.join(format!("{kind}.json"));
            let descriptor = self.read(file.clone())?;
            if descriptor["id"].as_str() != Some(id) { return Err("retirement descriptor changed its directory identity".into()); }
            if kind == "tool" {
                let owner = inventory_id(&descriptor["interface"])?;
                self.owner(context, owner)?;
                if self.companions.contains(owner) && !self.tools.contains(id) {
                    return Err("retirement tool is outside the accepted private maintenance inventory".into());
                }
            } else { self.artifact(context, &descriptor)?; }
            return Ok(file);
        }
        let relative = path.strip_prefix(&context.roots.interface_private_state_root).map_err(|_| "retirement additional directory has no declared owner")?;
        let parts = relative.iter().filter_map(|part| part.to_str()).collect::<Vec<_>>();
        let [door, "worker-environments", id] = parts.as_slice() else {
            return Err("retirement additional directory is not an exact worker environment".into());
        };
        let file = path.join("worker_environment.json");
        let record: octopus_worker_env::WorkerEnvironmentRecord = serde_json::from_value(self.read(file.clone())?).map_err(|error| error.to_string())?;
        record.validate().map_err(|error| error.to_string())?;
        let formation = &record.formation;
        let run = self.records.get(&formation.run).ok_or("retirement worker environment belongs to an unselected run")?;
        if record.lifecycle != octopus_worker_env::WorkerEnvironmentLifecycle::Released
            || formation.worker_environment_id.as_str() != *id || formation.door_interface.as_str() != *door
            || run["worker"].as_str() != Some(formation.worker.as_str())
        { return Err("retirement worker environment is unreleased or has a different run/worker/door".into()); }
        Ok(file)
    }

    fn artifact(&mut self, context: &TaskRetirementContext, descriptor: &Value) -> Result<(), String> {
        let id = inventory_id(&descriptor["id"])?;
        let run_id = inventory_id(&descriptor["run"])?;
        if self.collection.as_deref() == Some(id) || descriptor.get("collection_catalogue").is_some() {
            return Err("retirement must retain published collection and catalogue artifacts".into());
        }
        let run = self.records.get(run_id).ok_or("retirement artifact belongs to an unselected run")?.clone();
        let states = run["interface_state"].as_object().ok_or("retirement artifact run has no state")?;
        for (owner, state) in states {
            let Some(sources) = state.get("prelude_sources").and_then(Value::as_array) else { continue; };
            for source in sources {
                if source["snapshot"]["id"].as_str() != Some(id) { continue; }
                if source.as_object().is_none_or(|object| object.len() != 3) || source["source"]["id"] == source["snapshot"]["id"] {
                    return Err("retirement prelude snapshot has ambiguous ownership".into());
                }
                Self::artifact_ref(&source["snapshot"], descriptor)?;
                let bytes = descriptor["source_content"].as_str().ok_or("retirement snapshot has no source bytes")?;
                if descriptor["source"] != source["source"] || descriptor["source_sha256"] != source["source_sha256"]
                    || source["source_sha256"].as_str() != Some(digest(bytes.as_bytes()).as_str())
                { return Err("retirement prelude snapshot changed its accepted source".into()); }
                Self::artifact_ref(&source["source"], &serde_json::from_str(bytes).map_err(|error| error.to_string())?)?;
                self.artifact_owner(context, owner, run_id)?;
                return Ok(());
            }
        }
        let owner = inventory_id(&descriptor["owner_interface"])?;
        if !states.contains_key(owner) { return Err("retirement artifact has no saved interface owner".into()); }
        self.artifact_owner(context, owner, run_id)?;
        let sidecar = self.read(context.roots.registry_root.join("interface").join(owner).join("code_mode.json"))?;
        if sidecar["context"]["mode"] != "persistent" { return Err("retirement artifact has no persistent Code Mode owner".into()); }
        let prelude = &sidecar["context"]["prelude_artifact"];
        let topology = &sidecar["handoff"]["topology_artifact"];
        if topology["id"].as_str() == Some(id) {
            Self::artifact_ref(topology, descriptor)?;
            if descriptor["worker"] != run["worker"] || descriptor["prelude_artifact"] != *prelude {
                return Err("retirement topology artifact changed its worker or prelude".into());
            }
        } else { Self::artifact_ref(prelude, descriptor)?; }
        Ok(())
    }

    fn artifact_owner(&mut self, context: &TaskRetirementContext, owner: &str, run: &str) -> Result<(), String> {
        if self.companions.contains(owner) == (run == self.source) {
            return Err("retirement artifact crosses source and companion ownership".into());
        }
        self.owner(context, owner)
    }

    fn artifact_ref(reference: &Value, descriptor: &Value) -> Result<(), String> {
        if reference.as_object().is_none_or(|object| object.len() != 2) { return Err("retirement artifact reference has a different schema".into()); }
        let reference: octopus_ledger::ArtifactRef = serde_json::from_value(reference.clone()).map_err(|error| error.to_string())?;
        inventory_id(&Value::String(reference.id.to_string()))?;
        if descriptor["id"].as_str() != Some(reference.id.as_str())
            || reference.content != octopus_ledger::ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(descriptor))
        { return Err("retirement artifact changed its accepted content reference".into()); }
        Ok(())
    }
}

struct RetirementDirectoryScope {
    operation_id: String,
    sources: BTreeSet<PathBuf>,
    identities: BTreeMap<PathBuf, FileIdentity>,
    evidence: BTreeMap<PathBuf, Option<String>>,
    removed: BTreeSet<PathBuf>,
    recovering: bool,
    transaction: PathBuf,
    cleanup: PathBuf,
}

impl RetirementDirectoryScope {
    fn new(context: &TaskRetirementContext, retired: bool) -> Result<Self, String> {
        let roots = &context.roots;
        let home = roots.interface_private_state_root.parent().ok_or("retirement has no owning Home")?;
        if roots.interface_private_state_root != home.join("interfaces")
            || roots.run_records_root != home.join("state/runs")
            || roots.ledger_root != home.join("ledger")
        {
            return Err("retirement directory authority requires its exact owning roots".into());
        }
        for root in [&roots.registry_root, &roots.interface_private_state_root,
            &roots.run_records_root, &roots.ledger_root]
        {
            if std::fs::canonicalize(root).map_err(|error| error.to_string())?.as_os_str() != root.as_os_str() {
                return Err("retirement directory roots must be canonical, without aliases".into());
            }
        }
        let transactions = home.join("client-state/workspace/transactions");
        let sources = context.selection.interface_ids.iter().flat_map(|id| [
            roots.registry_root.join("interface").join(id.as_str()),
            roots.interface_private_state_root.join(id.as_str()),
            home.join("interface-hand-in").join(id.as_str()),
        ]).collect();
        let mut scope = Self {
            operation_id: context.operation_id.clone(), sources,
            identities: BTreeMap::new(), evidence: BTreeMap::new(), removed: BTreeSet::new(),
            recovering: context.prior_proof.is_some(),
            transaction: transactions.join(&context.operation_id),
            cleanup: transactions.join(format!(".cleanup-{}", context.operation_id)),
        };
        let inventory = &context.directory_inventory;
        if inventory.schema_version != 1 || inventory.removals.len() > 1024 {
            return Err("retirement directory inventory has an invalid version or size".into());
        }
        let mut ownership = RetirementDirectoryEvidence::new(context)?;
        let mut artifacts = BTreeSet::new();
        for pin in &inventory.removals {
            scope.path(&pin.path)?;
            if scope.sources.contains(&pin.path)
                || scope.identities.keys().any(|source| pin.path.starts_with(source) || source.starts_with(&pin.path))
                || scope.identities.insert(pin.path.clone(), pin.identity.clone()).is_some()
            {
                return Err("retirement additional directory overlaps another selected owner".into());
            }
            let evidence = ownership.directory(context, &pin.path)?;
            if pin.path.starts_with(roots.registry_root.join("artifact")) { artifacts.insert(evidence); }
            scope.sources.insert(pin.path.clone());
        }
        if ownership.used.len() != ownership.documents.len() {
            return Err("retirement inventory contains unrelated ownership evidence".into());
        }
        for original in &inventory.originals {
            scope.path(&original.file)?;
            scope.evidence.insert(original.file.clone(), Some(original.expected_digest.clone()));
        }
        let mut removed_files = BTreeSet::new();
        for pin in &inventory.retired {
            if pin.expected_digest.is_some() || !artifacts.contains(&pin.file) || !removed_files.insert(&pin.file) {
                return Err("retirement inventory may retire only exact owned artifact descriptors".into());
            }
            if retired { scope.evidence.insert(pin.file.clone(), None); }
        }
        scope.assert_evidence()?;
        Ok(scope)
    }

    fn assert_evidence(&self) -> Result<(), String> {
        for (path, expected) in &self.evidence {
            self.path(path)?;
            let observed = regular(path, Some(RETIREMENT_FRAME_BYTES))?;
            if observed.is_none() && (self.recovering || self.removed.iter().any(|root| path.starts_with(root))) { continue; }
            if observed.as_deref().map(digest) != *expected {
                return Err(format!("retirement directory ownership evidence '{}' changed", path.display()));
            }
        }
        for (path, identity) in &self.identities {
            self.path(path)?;
            match std::fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink()
                    && retirement_identity(&metadata)? == *identity => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
                    && (self.recovering || self.removed.contains(path)) => {}
                _ => return Err(format!("retirement directory '{}' changed its exact identity", path.display())),
            }
        }
        Ok(())
    }

    fn preimage(&self, path: &Path, expected: &DirectoryPreimage) -> Result<(), String> {
        if self.identities.get(path).is_some_and(|identity| identity != &expected.identity) {
            return Err("retirement directory preimage changed its declared root identity".into());
        }
        for (file, hash) in &self.evidence {
            let Ok(relative) = file.strip_prefix(path) else { continue; };
            let entry = expected.entries.iter().find(|entry| entry.relative_path == relative);
            if match (hash, entry) {
                (None, None) => false,
                (Some(hash), Some(entry)) => entry.kind != DirectoryEntryKind::File || entry.digest.as_ref() != Some(hash),
                _ => true,
            } {
                return Err("retirement directory preimage changed its exact ownership evidence".into());
            }
        }
        Ok(())
    }

    fn transaction(&self, id: &str) -> Result<(), String> {
        if id != self.operation_id {
            return Err("directory command belongs to a different held retirement transaction".into());
        }
        Ok(())
    }

    fn operation(&self, id: &str) -> Result<(), String> {
        let prefix = format!("{}:", self.operation_id);
        if !id.starts_with(&prefix) || id.len() <= prefix.len() || id.len() > 256
            || id.trim() != id || id.chars().any(char::is_control)
        {
            return Err("directory operation id must be bound to the held retirement transaction".into());
        }
        Ok(())
    }

    fn path(&self, path: &Path) -> Result<(), String> {
        let text = path.to_str().ok_or("retirement directory path must be UTF-8")?;
        if !path.is_absolute() || path.components().any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
            || text.split('/').skip(1).any(|part| part.is_empty() || part == "." || part == "..")
            || text.contains('\\') || text.chars().any(char::is_control)
            || path.components().any(|part| matches!(part.as_os_str().to_str(), Some(".octopus-publication" | ".life-ownership")))
        {
            return Err("retirement directory path must be an ordinary absolute normalized path".into());
        }
        for ancestor in path.ancestors().skip(1) {
            match std::fs::symlink_metadata(ancestor) {
                Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                    return Err("retirement directory path has an aliased or non-directory ancestor".into());
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    fn tombstone(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(self.transaction.join("removed")) else { return false; };
        let Some(name) = relative.to_str() else { return false; };
        name.len() == 4 && name.bytes().all(|byte| byte.is_ascii_digit())
    }

    fn snapshot(&self, path: &Path) -> Result<(), String> {
        self.path(path)?;
        if !self.sources.contains(path) && !self.tombstone(path) && path != self.transaction && path != self.cleanup {
            return Err("directory snapshot is outside the held retirement selection".into());
        }
        Ok(())
    }

    fn movement(&self, movement: &DirectoryMove) -> Result<(), String> {
        self.path(&movement.source)?;
        self.path(&movement.destination)?;
        if movement.expected.digest_algorithm != DigestAlgorithm::Sha256 {
            return Err("retirement directory preimages require explicit SHA256 evidence".into());
        }
        if self.sources.contains(&movement.source) && self.tombstone(&movement.destination) {
            self.preimage(&movement.source, &movement.expected)?;
            return Ok(());
        }
        if movement.source == self.transaction && movement.destination == self.cleanup {
            if movement.expected.entries.iter().any(|entry| {
                entry.relative_path.starts_with("removed") && entry.relative_path != Path::new("removed")
            }) {
                return Err("retirement tombstones must be reclaimed before moving their transaction root".into());
            }
            return Ok(());
        }
        Err("directory move is outside the exact held selection and transaction tombstones".into())
    }
}

fn retirement_line(input: &mut impl BufRead) -> Result<Option<String>, String> {
    let mut line = String::new();
    let count = Read::take(&mut *input, RETIREMENT_FRAME_BYTES + 1)
        .read_line(&mut line).map_err(|error| error.to_string())?;
    if count == 0 { return Ok(None); }
    if count as u64 > RETIREMENT_FRAME_BYTES || !line.ends_with('\n') {
        return Err("retirement command exceeds its bound or has an incomplete frame".into());
    }
    Ok(Some(line))
}

fn retirement_send(output: &mut impl Write, value: &impl Serialize) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() as u64 >= RETIREMENT_FRAME_BYTES {
        return Err("retirement response exceeds its frame bound".into());
    }
    bytes.push(b'\n');
    output.write_all(&bytes).and_then(|_| output.flush()).map_err(|error| error.to_string())
}

pub fn serve_task_retirement() -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let line = retirement_line(&mut input)?.ok_or("retirement context is absent")?;
    let context: TaskRetirementContext =
        serde_json::from_str(&line).map_err(|error| error.to_string())?;
    let request = context.request()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let _holds = loop {
        if let Some(holds) = request.try_hold()? {
            break holds;
        }
        if std::time::Instant::now() >= deadline {
            return Err("retirement is waiting for its interface processes to stop".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    let mut ledger_cache = RetirementLedgerCache::default();
    let mut proof = verify_held(&context, &request, context.prior_proof.as_ref(), &mut ledger_cache)?;
    let mut scope = RetirementDirectoryScope::new(&context, true)?;
    let mut output = std::io::stdout().lock();
    retirement_send(&mut output, &proof)?;
    while let Some(line) = retirement_line(&mut input)? {
        let command: RetirementCommand = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        match command {
            RetirementCommand::Release {} => return Ok(()),
            RetirementCommand::AssertHeld {} => {
                scope.assert_evidence()?;
                proof = verify_held(&context, &request, Some(&proof), &mut ledger_cache)?;
                retirement_send(&mut output, &proof)?;
            }
            RetirementCommand::SnapshotDirectory { transaction_id, path } => {
                scope.transaction(&transaction_id)?;
                scope.snapshot(&path)?;
                scope.assert_evidence()?;
                proof = verify_held(&context, &request, Some(&proof), &mut ledger_cache)?;
                let preimage = snapshot_directory(&path, DigestAlgorithm::Sha256).map_err(|error| error.to_string())?;
                scope.preimage(&path, &preimage)?;
                scope.assert_evidence()?;
                proof = verify_held(&context, &request, Some(&proof), &mut ledger_cache)?;
                retirement_send(&mut output, &RetirementDirectoryResponse::DirectorySnapshot {
                    schema_version: 1, operation_id: context.operation_id.clone(), proof: proof.clone(), path, preimage,
                })?;
            }
            RetirementCommand::PublishDirectoryMoves { transaction_id, operation_id, moves } => {
                scope.transaction(&transaction_id)?;
                scope.operation(&operation_id)?;
                for movement in &moves { scope.movement(movement)?; }
                scope.assert_evidence()?;
                proof = verify_held(&context, &request, Some(&proof), &mut ledger_cache)?;
                let result = publish_directory_moves(&operation_id, &moves).map_err(|error| error.to_string())?;
                for receipt in &result.receipts {
                    if receipt.outcome == PublicationOutcome::Applied { scope.removed.insert(receipt.movement.source.clone()); }
                }
                scope.assert_evidence()?;
                proof = verify_held(&context, &request, Some(&proof), &mut ledger_cache)?;
                retirement_send(&mut output, &RetirementDirectoryResponse::DirectoryPublication {
                    schema_version: 1, operation_id: context.operation_id.clone(), proof: proof.clone(), publication_operation_id: operation_id, result,
                })?;
            }
            RetirementCommand::ReclaimDirectory { transaction_id, tombstone, receipt, expected } => {
                scope.transaction(&transaction_id)?;
                scope.operation(&receipt.operation_id)?;
                scope.movement(&receipt.movement)?;
                if tombstone != receipt.movement.destination || expected != receipt.movement.expected {
                    return Err("retirement reclaim changed the original destination or exact preimage".into());
                }
                scope.assert_evidence()?;
                proof = verify_held(&context, &request, Some(&proof), &mut ledger_cache)?;
                reclaim_directory(&tombstone, &receipt, &expected).map_err(|error| error.to_string())?;
                scope.assert_evidence()?;
                proof = verify_held(&context, &request, Some(&proof), &mut ledger_cache)?;
                retirement_send(&mut output, &RetirementDirectoryResponse::DirectoryReclaimed {
                    schema_version: 1, operation_id: context.operation_id.clone(), proof: proof.clone(), tombstone, receipt,
                })?;
            }
        }
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn regular(path: &Path, maximum: Option<u64>) -> Result<Option<Vec<u8>>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || maximum.is_some_and(|maximum| metadata.len() > maximum)
    {
        return Err(format!(
            "retirement evidence '{}' is not a bounded regular file",
            path.display()
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if !opened.is_file() || opened.len() != metadata.len() {
        return Err("retirement evidence changed during open".into());
    }
    let mut bytes = Vec::new();
    file.take(metadata.len().saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != metadata.len() {
        return Err("retirement evidence changed during reading".into());
    }
    Ok(Some(bytes))
}

fn document(path: &Path, pins: &mut BTreeMap<PathBuf, Option<String>>) -> Result<Value, String> {
    let bytes = regular(path, Some(268_435_456))?
        .ok_or_else(|| format!("retirement evidence '{}' is absent", path.display()))?;
    pins.insert(path.to_path_buf(), Some(digest(&bytes)));
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

fn component(value: &str) -> Result<(), String> {
    let mut parts = Path::new(value).components();
    if !matches!(parts.next(), Some(Component::Normal(_)))
        || parts.next().is_some()
        || value.contains('\\')
    {
        return Err("retirement identity must be one exact path component".into());
    }
    Ok(())
}

impl TaskRetirementRequest {
    pub fn validate(&self, retired: bool) -> Result<BTreeMap<RunId, RunRecord>, String> {
        for root in [
            &self.registry_root,
            &self.run_records_root,
            &self.ledger_root,
            &self.interface_private_state_root,
        ] {
            if !root.is_absolute()
                || root
                    .components()
                    .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
            {
                return Err("retirement roots must be absolute normalized paths".into());
            }
            let metadata = std::fs::symlink_metadata(root).map_err(|error| error.to_string())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("retirement root is not a regular directory".into());
            }
        }
        let selection = &self.selection;
        let interfaces = selection.interface_ids.iter().collect::<BTreeSet<_>>();
        if selection.schema_version != 1
            || interfaces.is_empty()
            || interfaces.len() != selection.interface_ids.len()
            || selection.records.len() != self.original_records.len()
            || selection.records.is_empty()
        {
            return Err("retirement selection has invalid version or duplicate ownership".into());
        }
        for id in &interfaces {
            component(id.as_str())?;
        }
        let mut records = BTreeMap::new();
        for selected in &selection.records {
            component(selected.run.as_str())?;
            let original = self
                .original_records
                .get(&selected.run)
                .ok_or("retirement original record is absent")?;
            if selected.file != self.run_records_root.join(format!("{}.json", selected.run))
                || digest(original.as_bytes()) != selected.expected_digest
                || !interfaces.contains(&selected.worker)
                || !interfaces.contains(&selected.provider_interface)
                || !matches!(
                    selected.role.as_str(),
                    "source" | "observer" | "projection" | "writer" | "prediction" | "title"
                )
            {
                return Err("retirement record changed its exact path, preimage or owner".into());
            }
            let record =
                RunRecord::from_bytes(original.as_bytes()).map_err(|error| error.to_string())?;
            if record.id != selected.run
                || record.worker != selected.worker
                || record.ee != selected.ee
                || records.insert(record.id.clone(), record).is_some()
            {
                return Err("retirement record changed its identity or repeats a run".into());
            }
            let current = regular(&selected.file, Some(268_435_456))?;
            if retired && current.is_some()
                || !retired && current.as_deref() != Some(original.as_bytes())
            {
                return Err("retirement record differs from its expected publication state".into());
            }
        }
        let sources = selection
            .records
            .iter()
            .filter(|row| row.role == "source")
            .collect::<Vec<_>>();
        if sources.len() != 1 || sources[0].run != selection.source_run {
            return Err("retirement selection requires one exact source".into());
        }
        for selected in &selection.records {
            if matches!(selected.role.as_str(), "prediction" | "title") {
                let original: Value = serde_json::from_str(&self.original_records[&selected.run]).map_err(|error| error.to_string())?;
                for id in client_companion_interfaces(&self.registry_root, &original, selected, sources[0])? {
                    if !selection.interface_ids.iter().any(|selected| selected.as_str() == id) {
                        return Err("retirement suggestion interface is outside its selection".into());
                    }
                }
            } else if selected.role == "writer" {
                let target = records
                    .get(&selected.target_run)
                    .ok_or("retirement writer target is absent")?;
                let state = target
                    .interface_state
                    .get(&selected.provider_interface)
                    .ok_or("retirement writer owner is absent")?;
                if state
                    .get("run_authoring_binding").and_then(|binding| binding.pointer("/writer_run/run"))
                    .or_else(|| state.get("writer_run").and_then(|binding| binding.get("run")))
                    .and_then(Value::as_str)
                    != Some(selected.run.as_str())
                {
                    return Err(
                        "retirement writer differs from its exact target declaration".into(),
                    );
                }
            } else if selected.target_run != selected.run {
                return Err("retirement target changed identity".into());
            }
        }
        Ok(records)
    }

    pub fn assert_unbound(&self) -> Result<(), String> {
        let selected = self
            .selection
            .records
            .iter()
            .map(|record| &record.run)
            .collect::<BTreeSet<_>>();
        let interfaces = self.selection.interface_ids.iter().collect::<BTreeSet<_>>();
        for entry in std::fs::read_dir(&self.run_records_root).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("json")
            {
                continue;
            }
            let bytes = regular(&entry.path(), Some(268_435_456))?
                .ok_or("run disappeared during retirement ownership scan")?;
            let record = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
            if selected.contains(&record.id) {
                return Err("retired run is still published".into());
            }
            if interfaces.contains(&record.worker)
                || record
                    .interface_state
                    .keys()
                    .any(|id| interfaces.contains(id))
                || record.checkpoints.iter().any(|checkpoint| {
                    checkpoint
                        .interface_binding
                        .as_ref()
                        .is_some_and(|binding| interfaces.contains(&binding.interface))
                        || checkpoint
                            .outbound_interface_bindings
                            .iter()
                            .any(|binding| interfaces.contains(&binding.interface))
                })
            {
                return Err(format!(
                    "retired interface is still bound by run '{}'",
                    record.id
                ));
            }
        }
        Ok(())
    }

    pub fn inspect(&self, retired: bool) -> Result<TaskRetirementProof, String> {
        let records = self.validate(retired)?;
        if retired {
            self.assert_unbound()?;
        }
        let movements = self.ledger_root.join("movements.jsonl");
        let index = self.ledger_root.join("ledger-index.sqlite");
        let blobs = self.ledger_root.join("blobs");
        let mut pins = BTreeMap::new();
        pins.insert(
            movements.clone(),
            regular(&movements, None)?.as_deref().map(digest),
        );
        let mut limits = None;
        for selected in self.selection.records.iter().filter(|record| matches!(record.role.as_str(), "prediction" | "title")) {
            let policy = document(&self.registry_root.join("interface").join(selected.worker.as_str()).join("policy.json"), &mut pins)?;
            if policy["id"].as_str() != Some(selected.worker.as_str())
                || ["tools", "wek"].iter().any(|key| policy["ceiling"][key].as_array().is_none_or(|items| !items.is_empty()))
            { return Err("retirement client companion has an effect-capable worker ceiling".into()); }
        }
        for selected in self
            .selection
            .records
            .iter()
            .filter(|record| !matches!(record.role.as_str(), "writer" | "prediction" | "title"))
        {
            let record = &records[&selected.run];
            let provider = &selected.provider_interface;
            let root = self.registry_root.join("interface").join(provider.as_str());
            let descriptor = document(&root.join("interface.json"), &mut pins)?;
            if descriptor.get("id").and_then(Value::as_str) != Some(provider.as_str()) {
                return Err("retirement provider changed identity".into());
            }
            let profile: RunAuthoringProfile = serde_json::from_value(
                descriptor
                    .pointer("/interface_driver_config/private_config/run_authoring_profile")
                    .cloned()
                    .ok_or("retirement provider has no declared writer profile")?,
            )
            .map_err(|error| error.to_string())?;
            let config: SpawnLifeConfig =
                serde_json::from_value(document(&root.join("life.json"), &mut pins)?)
                    .map_err(|error| error.to_string())?;
            let [process] = config.processes.as_slice() else {
                return Err("retirement provider requires one declared life process".into());
            };
            process.io_limits.validate()?;
            limits = Some(process.io_limits);
            let evidence = CheckedRunEvidence {
                movements_path: &movements,
                ledger_index_path: &index,
                blobs_dir: &blobs,
                max_control_frame_bytes: process.io_limits.max_control_frame_bytes,
                max_manifest_bytes: process.io_limits.max_manifest_bytes,
                max_payload_bytes: process.io_limits.max_payload_bytes,
            };
            let writer = declared_provider_run_writer(record, provider, &profile)?;
            let topology: BoundRunAuthoringTopology = serde_json::from_value(
                record
                    .interface_state
                    .get(provider)
                    .and_then(|state| state.get("run_authoring_binding"))
                    .cloned()
                    .ok_or("retirement provider has no declared topology")?,
            )
            .map_err(|error| error.to_string())?;
            let sources = topology
                .checkpoints
                .iter()
                .filter(|checkpoint| {
                    checkpoint.role == BoundCheckpointRole::Stable && checkpoint.include_context
                })
                .map(|checkpoint| &checkpoint.interface_binding.interface)
                .collect::<BTreeSet<_>>();
            if sources.is_empty() {
                return Err("retirement provider has no declared context input".into());
            }
            for source in sources {
                if !provider_input_payloads_ready(
                    &evidence,
                    &selected.file,
                    self.original_records[&selected.run].as_bytes(),
                    &writer,
                    source,
                )? {
                    return Err(format!(
                        "run '{}' still has unhandled input or an unsettled handoff",
                        selected.run
                    ));
                }
            }
        }
        let limits = limits.ok_or("retirement selection has no provider")?;
        let limits = octopus_process_session_tool_runtime::InterfaceHostIoLimits {
            max_control_frame_bytes: limits.max_control_frame_bytes,
            max_stderr_bytes: limits.max_stderr_bytes,
            max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        };
        let mut drivers = BTreeSet::new();
        for (id, value) in registry_documents(&self.registry_root.join("worker_environment_kind"), "worker_environment_kind.json", &mut pins)? {
            let spec: octopus_worker_env::WorkerEnvKindSpec = serde_json::from_value(value).map_err(|error| error.to_string())?;
            octopus_descriptors::Descriptor::validate(&spec)?;
            if spec.worker_environment_kind_id.as_str() != id {
                return Err("retirement kind descriptor changed identity".into());
            }
            drivers.extend(spec.door_driver_refs);
        }
        let mut tool_doors = BTreeSet::new();
        for (id, value) in registry_documents(&self.registry_root.join("interface"), "interface.json", &mut pins)? {
            let descriptor: octopus_interfaces::InterfaceDescriptor = serde_json::from_value(value).map_err(|error| error.to_string())?;
            octopus_descriptors::Descriptor::validate(&descriptor)?;
            if descriptor.id.as_str() != id {
                return Err("retirement interface descriptor changed identity".into());
            }
            if drivers.contains(&descriptor.interface_driver_ref) { tool_doors.insert(descriptor.id); }
        }
        let proof = tool_retirement_quiescent(ToolRetirementRequest {
            movements_path: &movements,
            ledger_index_path: &index,
            blobs_dir: &blobs,
            interface_private_state_root: &self.interface_private_state_root,
            runs: &records.keys().cloned().collect(),
            tool_doors: &tool_doors,
            limits: &limits,
        })?;
        if !proof.quiescent {
            return Err("retirement has an accepted unresolved tool effect".into());
        }
        for (path, hash) in proof.preimages {
            if pins.get(&path).is_some_and(|before| before != &hash) {
                return Err("retirement evidence changed across inspection".into());
            }
            pins.insert(path, hash);
        }
        let proof = TaskRetirementProof {
            schema_version: 1,
            selection_digest: selection_digest(&self.selection)?,
            preimages: pins
                .into_iter()
                .map(|(path, sha256)| TaskRetirementPreimage { path, sha256 })
                .collect(),
        };
        proof.assert_unchanged()?;
        self.validate(retired)?;
        Ok(proof)
    }

    pub fn try_hold(&self) -> Result<Option<Vec<InterfaceLifeOwnership>>, String> {
        self.validate(true)?;
        let mut ids = self.selection.interface_ids.clone();
        ids.sort();
        let mut guards = Vec::new();
        for id in ids {
            let Some(guard) =
                InterfaceLifeOwnership::try_acquire(&self.interface_private_state_root, &id)?
            else {
                return Ok(None);
            };
            guards.push(guard);
        }
        self.assert_unbound()?;
        Ok(Some(guards))
    }
}

fn registry_documents(root: &Path, filename: &str, pins: &mut BTreeMap<PathBuf, Option<String>>) -> Result<Vec<(String, Value)>, String> {
    let mut documents = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_symlink() { return Err("retirement registry cannot follow an aliased owner".into()); }
        if !kind.is_dir() { continue; }
        let id = entry.file_name().into_string().map_err(|_| "retirement registry owner is not UTF-8")?;
        if id == ".octopus-publication" {
            #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                if entry.metadata().map_err(|error| error.to_string())?.permissions().mode() & 0o077 != 0 {
                    return Err("retirement publication metadata is not private".into());
                }
            }
            continue;
        }
        component(&id)?;
        documents.push((id, document(&entry.path().join(filename), pins)?));
    }
    Ok(documents)
}

impl TaskRetirementProof {
    pub fn assert_unchanged(&self) -> Result<(), String> {
        for pin in &self.preimages {
            if regular(&pin.path, None)?.as_deref().map(digest) != pin.sha256 {
                return Err(format!(
                    "retirement evidence '{}' changed",
                    pin.path.display()
                ));
            }
        }
        Ok(())
    }
}
