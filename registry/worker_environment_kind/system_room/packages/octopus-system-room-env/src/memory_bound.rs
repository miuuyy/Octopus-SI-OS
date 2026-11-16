use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_context_collections::{
    BoundCollection, CollectionEntry, CollectionRecord, EntryContent, EntryScope,
    OrderedFileWritePlan, PublicationState, ScopeContext,
};
use octopus_interfaces::InterfaceInvocationEnvelope;
use octopus_ledger::ContentHash;
use octopus_tool_handlers::{ToolCall, ToolHandlerError, ToolHandlerResult};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const MAX_NOTE_BYTES: usize = 8192;
const MAX_TITLE_CHARS: usize = 120;
const MAX_DESCRIPTION_CHARS: usize = 280;
const MAX_SUPERSEDED: usize = 16;
const MAX_CORRECTED: usize = 16;
const MAX_CORRECTIONS: usize = 64;
const DEFAULT_SEARCH_RESULTS: usize = 8;
const MAX_SEARCH_RESULTS: usize = 20;
const DEFAULT_PAGE: usize = 20;
const MAX_PAGE: usize = 50;
const MAX_QUERY_TERMS: usize = 32;
const MAX_EE_DEPTH: usize = 64;
const MAX_USAGE_DAYS: usize = 4096;
const PREVIEW_CHARS: usize = 320;
const APPLICABILITY: &str = "Use when the current work relates to this remembered note.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedWrite {
    path: PathBuf,
    content: String,
    expected_before_hash: Option<String>,
    #[serde(rename = "expected_before_content")]
    _expected_before_content: Option<String>,
}

pub struct MemoryHost<'a> {
    pub collection: &'a str,
    pub artifact_root: &'a Path,
    pub ee_instances_root: &'a Path,
    pub effect_root: Option<&'a Path>,
    pub maximum_bytes: u64,
    pub observed_run: Option<&'a str>,
    pub artifact_blobs_root: Option<&'a Path>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveArguments {
    note: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    supersedes: Vec<String>,
    #[serde(default)]
    corrects: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    id: String,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    include_evidence: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceArguments {
    id: String,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArguments {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    include_withdrawn: bool,
}

struct Memory<'a> {
    input: &'a InterfaceInvocationEnvelope,
    call: &'a ToolCall,
    host: &'a MemoryHost<'a>,
    collection: BoundCollection,
    caller: ScopeContext,
    lineage: Vec<String>,
}

pub fn execute(
    input: &InterfaceInvocationEnvelope,
    call: &ToolCall,
    host: &MemoryHost<'_>,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let operation = call
        .handler_config
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let body = match operation {
        "memory_save" => {
            let arguments = parse(call)?;
            Memory::open(input, call, host)?.save(arguments)?
        }
        "memory_search" => {
            let arguments = parse(call)?;
            Memory::open(input, call, host)?.search(arguments)?
        }
        "memory_read" => {
            let arguments = parse(call)?;
            Memory::open(input, call, host)?.read(arguments)?
        }
        "memory_list" => {
            let arguments = parse(call)?;
            Memory::open(input, call, host)?.list(arguments)?
        }
        "memory_inspect_evidence" => {
            let arguments = parse(call)?;
            Memory::open(input, call, host)?.inspect_evidence(arguments)?
        }
        other => {
            return Err(ToolHandlerError::Execution {
                tool_id: call.tool_id.clone(),
                reason: format!("memory operation '{other}' is not declared"),
            });
        }
    };
    Ok(ToolHandlerResult::new(body))
}

fn parse<T: DeserializeOwned>(call: &ToolCall) -> Result<T, ToolHandlerError> {
    serde_json::from_value(call.arguments.clone()).map_err(|error| {
        ToolHandlerError::InvalidArguments {
            tool_id: call.tool_id.clone(),
            reason: error.to_string(),
        }
    })
}

impl<'a> Memory<'a> {
    fn open(
        input: &'a InterfaceInvocationEnvelope,
        call: &'a ToolCall,
        host: &'a MemoryHost<'a>,
    ) -> Result<Self, ToolHandlerError> {
        let failure = |reason: String| ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason,
        };
        let runtime = input.runtime().map_err(|error| failure(error.to_string()))?;
        let source = crate::collection_tools::readable_source(input, host.collection)
            .map_err(|error| failure(format!("memory collection '{}': {error}", host.collection)))?;
        let collection = crate::collection_tools::open(host.artifact_root, source).map_err(failure)?;
        let ee_id = match host.observed_run {
            Some(run) => crate::memory_observation::scope(input, host, run).map_err(failure)?,
            None => runtime.ee.to_string(),
        };
        let caller = ScopeContext {
            ee_id,
            worker_interface: runtime.worker.to_string(),
        };
        let lineage = ee_lineage(host.ee_instances_root, &caller.ee_id).map_err(failure)?;
        Ok(Self {
            input,
            call,
            host,
            collection,
            caller,
            lineage,
        })
    }

    fn save(&self, arguments: SaveArguments) -> Result<Value, ToolHandlerError> {
        let note = arguments.note.trim();
        if note.is_empty()
            || note.len() > MAX_NOTE_BYTES
            || note.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
        {
            return Err(self.invalid(format!(
                "note must be nonblank text of at most {MAX_NOTE_BYTES} bytes"
            )));
        }
        let title = match arguments.title.as_deref().map(str::trim) {
            Some(title)
                if title.is_empty()
                    || title.chars().count() > MAX_TITLE_CHARS
                    || title.chars().any(char::is_control) =>
            {
                return Err(self.invalid(format!(
                    "title must be one nonblank line of at most {MAX_TITLE_CHARS} characters"
                )));
            }
            Some(title) => title.to_string(),
            None => headline(note, MAX_TITLE_CHARS),
        };
        let mut superseded = BTreeSet::new();
        if arguments.supersedes.len() > MAX_SUPERSEDED
            || !arguments
                .supersedes
                .iter()
                .all(|id| superseded.insert(id.trim()))
        {
            return Err(self.invalid(format!(
                "supersedes lists at most {MAX_SUPERSEDED} distinct memory ids"
            )));
        }
        let mut corrected = BTreeSet::new();
        if arguments.corrects.len() > MAX_CORRECTED
            || !arguments
                .corrects
                .iter()
                .all(|id| corrected.insert(id.trim()))
            || !corrected.is_disjoint(&superseded)
        {
            return Err(self.invalid(format!(
                "corrects lists at most {MAX_CORRECTED} distinct memory ids, none of them also superseded"
            )));
        }
        let grant = self
            .input
            .grant()
            .map_err(|error| self.execution(error.to_string()))?;
        if !grant.may_create_artifact() {
            return Err(self.execution(
                "memory.save is refused: the formed grant holds no create artifact verb",
            ));
        }
        if !superseded.is_empty() && !grant.artifact_verbs.iter().any(|verb| verb == "change") {
            return Err(self.execution(
                "superseding a memory is refused: the formed grant holds no change artifact verb",
            ));
        }
        let effect_root = self
            .host
            .effect_root
            .ok_or_else(|| self.execution("memory.save requires formation-scoped durable state"))?;
        let directory = effect_root
            .join("memory-save")
            .join(self.input.artifact.content.to_hex());
        let _lock = crate::file_write::lock_invocation(&directory).map_err(|error| self.execution(error))?;
        let plan_path = directory.join("plan.json");
        let plan = match self.retained_plan(&plan_path)? {
            Some(plan) => plan,
            None => {
                let records = self.records(false)?;
                let wanted = normalized(note);
                if let Some(existing) = records.iter().find(|record| {
                    note_text(&record.entry).is_some_and(|text| normalized(text) == wanted)
                }) {
                    return Ok(json!({
                        "result": "memory_already_saved",
                        "memory": self.pin(existing),
                    }));
                }
                let plan = self.plan_save(note, title, &superseded, &corrected, &records)?;
                let mut bytes = serde_json::to_vec(&plan)
                    .map_err(|error| self.execution(error.to_string()))?;
                bytes.push(b'\n');
                crate::file_write::install_effect_record(&plan_path, &bytes)
                    .map_err(|error| self.execution(error))?;
                plan
            }
        };
        let entries = self.collection.directory().join("entries");
        self.real_directory(&entries, true)?;
        let planned: Vec<PlannedWrite> = serde_json::from_value(plan["file_write"]["writes"].clone())
            .map_err(|error| self.execution(format!("memory save plan writes are malformed: {error}")))?;
        let mut writes = Vec::with_capacity(planned.len());
        for write in &planned {
            match self.apply(&entries, &directory, write) {
                Ok(receipt) => writes.push(receipt),
                Err(reason) => {
                    return Err(ToolHandlerError::ExecutionWithDetails {
                        tool_id: self.call.tool_id.clone(),
                        reason: format!("memory save stopped before it was complete; the same call completes it: {reason}"),
                        details: serde_json::Map::from_iter([("writes".to_string(), json!(writes))]),
                    });
                }
            }
        }
        let corrects = self.record_corrections(&plan)?;
        Ok(json!({
            "result": "memory_saved",
            "memory": plan["memory"],
            "withdrawn": plan["withdrawn"],
            "corrects": corrects,
            "writes": writes,
        }))
    }

    fn record_corrections(&self, plan: &Value) -> Result<Value, ToolHandlerError> {
        let Some(targets) = plan
            .get("corrects")
            .and_then(Value::as_array)
            .filter(|targets| !targets.is_empty())
        else {
            return Ok(json!([]));
        };
        let name = |value: &Value| {
            value
                .as_str()
                .filter(|id| !id.is_empty() && !id.starts_with('.') && !id.contains(['/', '\\', '\0']))
                .map(str::to_string)
                .ok_or_else(|| self.execution("memory save plan names a malformed correction"))
        };
        let by = name(&plan["memory"]["id"])?;
        let root = self.collection.directory().join("corrections");
        self.real_directory(&root, true)?;
        for target in targets {
            let directory = root.join(name(&target["id"])?);
            self.real_directory(&directory, true)?;
            let receipt = json!({"schema_version": 1, "corrected": target, "by": plan["memory"]});
            let mut bytes = octopus_descriptors::canonical_bytes(&receipt);
            bytes.push(b'\n');
            crate::file_write::install_effect_record(&directory.join(format!("{by}.json")), &bytes).map_err(
                |error| self.execution(format!("memory correction was not recorded; the same call completes it: {error}")),
            )?;
        }
        Ok(Value::Array(targets.clone()))
    }

    fn apply(&self, entries: &Path, staging: &Path, write: &PlannedWrite) -> Result<Value, String> {
        let name = write
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.ends_with(".json") && !name.starts_with('.'))
            .ok_or_else(|| format!("memory write '{}' names no entry file", write.path.display()))?;
        let target = entries.join(name);
        if write.path != target {
            return Err(format!("memory write '{}' leaves the collection entries", write.path.display()));
        }
        let bytes = write.content.as_bytes();
        if bytes.len() as u64 > self.host.maximum_bytes {
            return Err(format!("memory entry '{name}' exceeds the tool's byte bound"));
        }
        let after = ContentHash::of_bytes(bytes).to_hex();
        match &write.expected_before_hash {
            None => install_new(staging, &target, bytes, &after)?,
            Some(before) => replace_exact(staging, &target, bytes, before, &after)?,
        }
        let observed = std::fs::read(&target)
            .map(|current| ContentHash::of_bytes(&current).to_hex())
            .map_err(|error| format!("memory entry '{name}' cannot be read back: {error}"))?;
        if observed != after {
            return Err(format!("memory entry '{name}' changed while it was being saved"));
        }
        Ok(json!({
            "path": target,
            "before_hash": write.expected_before_hash,
            "after_hash": after,
            "bytes": bytes.len(),
            "outcome": "applied",
            "observed_hash": observed,
            "error": null,
        }))
    }

    fn plan_save(
        &self,
        note: &str,
        title: String,
        superseded: &BTreeSet<&str>,
        corrected: &BTreeSet<&str>,
        records: &[CollectionRecord],
    ) -> Result<Value, ToolHandlerError> {
        let now = unix_seconds().map_err(|error| self.execution(error))?;
        let hash = self.input.artifact.content.to_hex();
        let id = format!("memory-{}-{}-{}", compact_time(now), slug(&title), &hash[..8]);
        let entry = CollectionEntry {
            schema_version: 1,
            id: id.clone(),
            title: title.clone(),
            description: headline(note, MAX_DESCRIPTION_CHARS),
            applicability: APPLICABILITY.to_string(),
            state: PublicationState::Active,
            scope: EntryScope {
                ee_ids: Some(vec![self.caller.ee_id.clone()]),
                worker_interfaces: None,
            },
            content: Some(EntryContent {
                text: Some(note.to_string()),
                evidence: vec![self.input.artifact.clone()],
            }),
            selection: json!({}),
            requires: Vec::new(),
        };
        let created = self
            .collection
            .prepare_publication(&entry, None, &self.caller)
            .map_err(|error| self.execution(error.to_string()))?;
        let mut writes = created.file_write.writes;
        let mut withdrawn = Vec::new();
        for id in superseded {
            let record = records
                .iter()
                .find(|record| record.entry.id == *id)
                .ok_or_else(|| {
                    self.invalid(format!("memory '{id}' is not an active memory visible here"))
                })?;
            if record.entry.scope.ee_ids.as_deref() != Some(std::slice::from_ref(&self.caller.ee_id))
                || record.entry.scope.worker_interfaces.is_some()
            {
                return Err(self.invalid(format!(
                    "memory '{id}' belongs to a wider or different scope; save a correcting note instead"
                )));
            }
            let mut retired = record.entry.clone();
            retired.state = PublicationState::Withdrawn;
            let plan = self
                .collection
                .prepare_publication(&retired, Some(record.revision), &self.caller)
                .map_err(|error| self.execution(error.to_string()))?;
            withdrawn.push(json!({"id": id, "revision": plan.pin.revision.to_hex()}));
            writes.extend(plan.file_write.writes);
        }
        let mut corrects = Vec::new();
        for id in corrected {
            let record = records
                .iter()
                .find(|record| record.entry.id == *id)
                .ok_or_else(|| {
                    self.invalid(format!("memory '{id}' is not an active memory visible here"))
                })?;
            corrects.push(json!({
                "id": id,
                "revision": record.revision.to_hex(),
                "scope_ee_ids": record.entry.scope.ee_ids,
            }));
        }
        let batch = serde_json::to_value(OrderedFileWritePlan {
            ordered: true,
            writes,
        })
        .map_err(|error| self.execution(error.to_string()))?;
        Ok(json!({
            "schema_version": 1,
            "artifact": self.input.artifact,
            "memory": {
                "id": id,
                "revision": created.pin.revision.to_hex(),
                "title": title,
                "saved_at": iso_time(now),
                "scope_ee_ids": [self.caller.ee_id],
            },
            "withdrawn": withdrawn,
            "corrects": corrects,
            "file_write": batch,
        }))
    }

    fn retained_plan(&self, path: &Path) -> Result<Option<Value>, ToolHandlerError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(self.execution(format!("memory save plan cannot be read: {error}"))),
        };
        let plan: Value = serde_json::from_slice(&bytes)
            .map_err(|error| self.execution(format!("memory save plan is malformed: {error}")))?;
        let artifact = serde_json::to_value(&self.input.artifact)
            .map_err(|error| self.execution(error.to_string()))?;
        if plan.get("schema_version") != Some(&json!(1)) || plan.get("artifact") != Some(&artifact) {
            return Err(self.execution("memory save plan differs from its invocation"));
        }
        if plan["memory"]["scope_ee_ids"] != json!([self.caller.ee_id]) {
            return Err(self.execution("memory save plan differs from its admitted experience scope"));
        }
        Ok(Some(plan))
    }

    fn search(&self, arguments: SearchArguments) -> Result<Value, ToolHandlerError> {
        let terms = terms(&arguments.query);
        if terms.is_empty() {
            return Err(self.invalid("query must contain searchable words"));
        }
        let limit = self.bounded(arguments.max_results, DEFAULT_SEARCH_RESULTS, MAX_SEARCH_RESULTS)?;
        let records = self.records(false)?;
        let active = active_notes(&records);
        let documents = records.iter().map(|record| document(&record.entry)).collect::<Vec<_>>();
        let mut matches = crate::memory_rank::rank(&documents, &terms)
            .into_iter()
            .zip(&records)
            .filter(|(rank, _)| rank.matched())
            .collect::<Vec<_>>();
        matches.sort_by(|(left_rank, left), (right_rank, right)| {
            left_rank
                .best_first(right_rank)
                .then_with(|| right.entry.id.cmp(&left.entry.id))
        });
        matches.truncate(limit);
        let rows = matches
            .iter()
            .map(|(rank, record)| {
                let mut row = self.summary(record, &active)?;
                let whole_word = rank.exact > 0.0;
                let score = if whole_word { rank.exact } else { rank.partial };
                row["score"] = json!((score * 10_000.0).round() / 10_000.0);
                row["match"] = json!(if whole_word { "word" } else { "partial" });
                row["preview"] = json!(note_text(&record.entry).map(|text| preview(text, PREVIEW_CHARS)));
                Ok(row)
            })
            .collect::<Result<Vec<_>, ToolHandlerError>>()?;
        Ok(json!({
            "result": "memories_found",
            "query": arguments.query.trim(),
            "matches": rows,
        }))
    }

    fn read(&self, arguments: ReadArguments) -> Result<Value, ToolHandlerError> {
        let records = self.records(false)?;
        let record = self.visible(&records, &arguments.id, arguments.revision.as_deref())?;
        self.record_usage(record)?;
        let mut memory = self.summary(record, &active_notes(&records))?;
        memory["applicability"] = json!(record.entry.applicability);
        memory["text"] = json!(note_text(&record.entry));
        memory["evidence"] = json!(record.entry.content.as_ref().map(|content| &content.evidence));
        if arguments.include_evidence {
            memory["evidence_details"] = self.evidence(record, crate::memory_evidence::DEFAULT_PREVIEW_BYTES);
        }
        memory["selection"] = record.entry.selection.clone();
        memory["requires"] = json!(record.entry.requires);
        Ok(json!({"result": "memory_read", "memory": memory}))
    }

    fn inspect_evidence(&self, arguments: EvidenceArguments) -> Result<Value, ToolHandlerError> {
        let preview_bytes =
            crate::memory_evidence::preview_bound(arguments.max_bytes).map_err(|reason| self.invalid(reason))?;
        let records = self.records(false)?;
        let record = self.visible(&records, &arguments.id, arguments.revision.as_deref())?;
        Ok(json!({
            "result": "memory_evidence",
            "memory": self.summary(record, &active_notes(&records))?,
            "evidence": self.evidence(record, preview_bytes),
        }))
    }

    fn evidence(&self, record: &CollectionRecord, preview_bytes: usize) -> Value {
        let recorded = record
            .entry
            .content
            .as_ref()
            .map(|content| content.evidence.as_slice())
            .unwrap_or_default();
        crate::memory_evidence::describe(
            self.host.artifact_blobs_root,
            note_text(&record.entry),
            recorded,
            preview_bytes,
        )
    }

    fn visible<'r>(
        &self,
        records: &'r [CollectionRecord],
        id: &str,
        revision: Option<&str>,
    ) -> Result<&'r CollectionRecord, ToolHandlerError> {
        let id = id.trim();
        let record = records
            .iter()
            .find(|record| record.entry.id == id)
            .ok_or_else(|| self.execution(format!("memory '{id}' is not an active memory visible here")))?;
        if let Some(revision) = revision.map(str::trim) {
            if revision != record.revision.to_hex() {
                return Err(self.execution(format!(
                    "memory '{id}' changed; its current revision is {}",
                    record.revision.to_hex()
                )));
            }
        }
        Ok(record)
    }

    fn list(&self, arguments: ListArguments) -> Result<Value, ToolHandlerError> {
        let records = self.records(arguments.include_withdrawn)?;
        let active = active_notes(&records);
        let rows = records
            .iter()
            .map(|record| json!([record.entry.id, record.revision.to_hex()]))
            .collect::<Vec<_>>();
        let fingerprint =
            ContentHash::of_bytes(&octopus_descriptors::canonical_bytes(&Value::Array(rows))).to_hex();
        let start = match arguments.cursor.as_deref() {
            None => 0,
            Some(cursor) => {
                let offset = cursor
                    .strip_prefix(fingerprint.as_str())
                    .and_then(|rest| rest.strip_prefix(':'))
                    .ok_or_else(|| self.execution("memory list cursor is stale; list again without a cursor"))?;
                offset
                    .parse::<usize>()
                    .ok()
                    .filter(|offset| *offset <= records.len())
                    .ok_or_else(|| self.invalid("memory list cursor is malformed"))?
            }
        };
        let limit = self.bounded(arguments.limit, DEFAULT_PAGE, MAX_PAGE)?;
        let end = start.saturating_add(limit).min(records.len());
        let memories = records[start..end]
            .iter()
            .map(|record| self.summary(record, &active))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({
            "result": "memories_listed",
            "memories": memories,
            "total": records.len(),
            "next_cursor": (end < records.len()).then(|| format!("{fingerprint}:{end}")),
        }))
    }

    fn records(&self, include_withdrawn: bool) -> Result<Vec<CollectionRecord>, ToolHandlerError> {
        let mut records = self
            .collection
            .records()
            .map_err(|error| self.execution(error.to_string()))?
            .into_iter()
            .filter(|record| {
                (include_withdrawn || record.entry.state == PublicationState::Active)
                    && self.lineage.iter().any(|ee| {
                        record.entry.scope.permits(&ScopeContext {
                            ee_id: ee.clone(),
                            worker_interface: self.caller.worker_interface.clone(),
                        })
                    })
            })
            .collect::<Vec<_>>();
        records.sort_by(|left, right| left.entry.id.cmp(&right.entry.id));
        Ok(records)
    }

    fn pin(&self, record: &CollectionRecord) -> Value {
        json!({
            "id": record.entry.id,
            "revision": record.revision.to_hex(),
            "title": record.entry.title,
            "saved_at": saved_at(&record.entry.id),
            "scope_ee_ids": record.entry.scope.ee_ids,
        })
    }

    fn summary(
        &self,
        record: &CollectionRecord,
        active: &BTreeMap<&str, &CollectionRecord>,
    ) -> Result<Value, ToolHandlerError> {
        let mut summary = self.pin(record);
        summary["description"] = json!(record.entry.description);
        summary["state"] = json!(record.entry.state);
        summary["usage"] = self.usage(&record.entry.id)?;
        summary["corrected_by"] = json!(self.corrected_by(&record.entry.id, active)?);
        Ok(summary)
    }

    fn corrected_by(
        &self,
        id: &str,
        active: &BTreeMap<&str, &CollectionRecord>,
    ) -> Result<Vec<Value>, ToolHandlerError> {
        let root = self.collection.directory().join("corrections");
        let directory = root.join(id);
        if !self.real_directory(&root, false)? || !self.real_directory(&directory, false)? {
            return Ok(Vec::new());
        }
        let mut correcting = BTreeSet::new();
        for entry in std::fs::read_dir(&directory)
            .map_err(|error| self.execution(format!("memory corrections of '{id}' cannot be listed: {error}")))?
            .take(MAX_CORRECTIONS)
        {
            let name = entry.map_err(|error| self.execution(error.to_string()))?.file_name();
            let by = name
                .to_str()
                .filter(|name| !name.starts_with('.'))
                .and_then(|name| name.strip_suffix(".json"));
            if let Some((by, _)) = by.and_then(|by| active.get_key_value(by)) {
                correcting.insert(*by);
            }
        }
        Ok(correcting.into_iter().map(|by| self.pin(active[by])).collect())
    }

    fn usage(&self, id: &str) -> Result<Value, ToolHandlerError> {
        let usage = self.collection.directory().join("usage");
        let directory = usage.join(id);
        if !self.real_directory(&usage, false)? || !self.real_directory(&directory, false)? {
            return Ok(json!({"days_used": 0, "last_used": null}));
        }
        let mut days = Vec::new();
        for entry in std::fs::read_dir(&directory)
            .map_err(|error| self.execution(format!("memory usage of '{id}' cannot be listed: {error}")))?
        {
            let entry = entry.map_err(|error| self.execution(error.to_string()))?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| self.execution(format!("memory usage of '{id}' has a non-UTF-8 name")))?;
            if name.starts_with('.') {
                continue;
            }
            let day = name
                .strip_suffix(".json")
                .filter(|day| is_day(day))
                .ok_or_else(|| self.execution(format!("memory usage of '{id}' holds unexpected '{name}'")))?;
            days.push(day.to_string());
            if days.len() > MAX_USAGE_DAYS {
                return Err(self.execution(format!("memory usage of '{id}' exceeds its bound")));
            }
        }
        days.sort();
        Ok(json!({"days_used": days.len(), "last_used": days.last()}))
    }

    fn record_usage(&self, record: &CollectionRecord) -> Result<(), ToolHandlerError> {
        let now = unix_seconds().map_err(|error| self.execution(error))?;
        let usage = self.collection.directory().join("usage");
        let directory = usage.join(&record.entry.id);
        self.real_directory(&usage, true)?;
        self.real_directory(&directory, true)?;
        let path = directory.join(format!("{}.json", day(now)));
        if std::fs::symlink_metadata(&path).is_ok() {
            return Ok(());
        }
        let runtime = self
            .input
            .runtime()
            .map_err(|error| self.execution(error.to_string()))?;
        let receipt = json!({
            "schema_version": 1,
            "entry_id": record.entry.id,
            "revision": record.revision.to_hex(),
            "day": day(now),
            "run": runtime.run.as_str(),
        });
        let mut bytes = octopus_descriptors::canonical_bytes(&receipt);
        bytes.push(b'\n');
        match crate::file_write::install_effect_record(&path, &bytes) {
            Ok(()) => Ok(()),
            Err(_) if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) => Ok(()),
            Err(error) => Err(self.execution(format!("memory usage cannot be recorded: {error}"))),
        }
    }

    fn real_directory(&self, path: &Path, create: bool) -> Result<bool, ToolHandlerError> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
            Ok(_) => Err(self.execution(format!("memory path '{}' is not a real directory", path.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                match std::fs::create_dir(path) {
                    Ok(()) => Ok(true),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        self.real_directory(path, false)
                    }
                    Err(error) => Err(self.execution(format!(
                        "memory directory '{}' cannot be created: {error}",
                        path.display()
                    ))),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(self.execution(format!("memory path '{}': {error}", path.display()))),
        }
    }

    fn bounded(&self, value: Option<usize>, default: usize, maximum: usize) -> Result<usize, ToolHandlerError> {
        match value {
            None => Ok(default),
            Some(value) if (1..=maximum).contains(&value) => Ok(value),
            Some(_) => Err(self.invalid(format!("limit must be between 1 and {maximum}"))),
        }
    }

    fn invalid(&self, reason: impl Into<String>) -> ToolHandlerError {
        ToolHandlerError::InvalidArguments {
            tool_id: self.call.tool_id.clone(),
            reason: reason.into(),
        }
    }

    fn execution(&self, reason: impl Into<String>) -> ToolHandlerError {
        ToolHandlerError::Execution {
            tool_id: self.call.tool_id.clone(),
            reason: reason.into(),
        }
    }
}

fn install_new(staging: &Path, target: &Path, bytes: &[u8], after: &str) -> Result<(), String> {
    let staged = crate::file_write::stage_write(staging, target, bytes)?;
    let linked = std::fs::hard_link(&staged, target);
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => crate::file_write::sync_parent(target),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let current = std::fs::read(target).map_err(|error| error.to_string())?;
            if ContentHash::of_bytes(&current).to_hex() == after {
                Ok(())
            } else {
                Err(format!("memory entry '{}' already exists with other content", target.display()))
            }
        }
        Err(error) => Err(format!("memory entry '{}' cannot be installed: {error}", target.display())),
    }
}

fn replace_exact(staging: &Path, target: &Path, bytes: &[u8], before: &str, after: &str) -> Result<(), String> {
    let current = std::fs::read(target)
        .map_err(|error| format!("memory entry '{}' cannot be read: {error}", target.display()))?;
    let current = ContentHash::of_bytes(&current).to_hex();
    if current == after {
        return Ok(());
    }
    if current != before {
        return Err(format!("memory entry '{}' changed since this save was planned", target.display()));
    }
    let staged = crate::file_write::stage_write(staging, target, bytes)?;
    if let Err(error) = exchange(&staged, target) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("memory entry '{}' cannot be replaced: {error}", target.display()));
    }
    let displaced = std::fs::read(&staged).map(|old| ContentHash::of_bytes(&old).to_hex());
    if displaced.as_deref().ok() != Some(before) {
        let restored = exchange(&staged, target);
        let _ = std::fs::remove_file(&staged);
        return Err(match restored {
            Ok(()) => format!("memory entry '{}' changed while it was being replaced", target.display()),
            Err(error) => format!("memory entry '{}' changed while it was being replaced and could not be restored: {error}", target.display()),
        });
    }
    let _ = std::fs::remove_file(&staged);
    crate::file_write::sync_parent(target)
}

#[cfg(target_vendor = "apple")]
fn exchange(left: &Path, right: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let left = std::ffi::CString::new(left.as_os_str().as_bytes())?;
    let right = std::ffi::CString::new(right.as_os_str().as_bytes())?;
    if unsafe { libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn exchange(left: &Path, right: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let left = std::ffi::CString::new(left.as_os_str().as_bytes())?;
    let right = std::ffi::CString::new(right.as_os_str().as_bytes())?;
    if unsafe {
        libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, left.as_ptr(), libc::AT_FDCWD, right.as_ptr(), libc::RENAME_EXCHANGE)
    } == 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn exchange(_left: &Path, _right: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "atomic exchange is unavailable on this platform"))
}

fn ee_lineage(root: &Path, ee: &str) -> Result<Vec<String>, String> {
    let mut lineage = Vec::new();
    let mut cursor = Some(ee.to_string());
    while let Some(id) = cursor {
        if lineage.contains(&id) {
            return Err(format!("EE lineage of '{ee}' cycles at '{id}'"));
        }
        if lineage.len() >= MAX_EE_DEPTH {
            return Err(format!("EE lineage of '{ee}' exceeds {MAX_EE_DEPTH} layers"));
        }
        if id.is_empty() || id.contains(['/', '\\', '\0']) || matches!(id.as_str(), "." | "..") {
            return Err(format!("EE id '{id}' is not a record name"));
        }
        let path: PathBuf = root.join(format!("{id}.json"));
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("EE instance '{id}' cannot be read: {error}"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("EE instance '{id}' is malformed: {error}"))?;
        if value.get("ee_id").and_then(Value::as_str) != Some(id.as_str()) {
            return Err(format!("EE instance '{id}' declares a different identity"));
        }
        cursor = match value.get("parent_ee_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(parent)) => Some(parent.clone()),
            Some(_) => return Err(format!("EE instance '{id}' has a malformed parent")),
        };
        lineage.push(id);
    }
    Ok(lineage)
}

fn note_text(entry: &CollectionEntry) -> Option<&str> {
    entry.content.as_ref().and_then(|content| content.text.as_deref())
}

fn active_notes(records: &[CollectionRecord]) -> BTreeMap<&str, &CollectionRecord> {
    records
        .iter()
        .filter(|record| record.entry.state == PublicationState::Active)
        .map(|record| (record.entry.id.as_str(), record))
        .collect()
}

fn document(entry: &CollectionEntry) -> crate::memory_rank::Document {
    let text = note_text(entry).unwrap_or_default();
    let lead = entry.description.trim_end_matches('…');
    let mut body = vec![text];
    if lead.is_empty() || !text.trim_start().starts_with(lead) {
        body.push(entry.description.as_str());
    }
    if entry.applicability != APPLICABILITY {
        body.push(entry.applicability.as_str());
    }
    crate::memory_rank::Document::new(&entry.title, &body)
}

fn normalized(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(['.', '!'])
        .to_string()
}

fn headline(text: &str, maximum: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    preview(line, maximum)
}

fn preview(text: &str, maximum: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= maximum {
        return collapsed;
    }
    let mut cut = collapsed.chars().take(maximum - 1).collect::<String>();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

fn slug(text: &str) -> String {
    let mut slug = String::new();
    for word in text
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if slug.len() + word.len() + 1 > 40 {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(word);
    }
    if slug.is_empty() { "note".to_string() } else { slug }
}

fn terms(query: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    crate::memory_rank::words(query)
        .into_iter()
        .filter(|term| term.chars().count() >= 2)
        .filter(|term| seen.insert(term.clone()))
        .take(MAX_QUERY_TERMS)
        .collect()
}

fn unix_seconds() -> Result<u64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|error| format!("system clock precedes the Unix epoch: {error}"))
}

fn civil(seconds: u64) -> (i64, u32, u32, u64) {
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day, seconds % 86_400)
}

fn day(seconds: u64) -> String {
    let (year, month, day, _) = civil(seconds);
    format!("{year:04}-{month:02}-{day:02}")
}

fn compact_time(seconds: u64) -> String {
    let (year, month, day, time) = civil(seconds);
    format!(
        "{year:04}{month:02}{day:02}t{:02}{:02}{:02}z",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

fn iso_time(seconds: u64) -> String {
    let (year, month, day, time) = civil(seconds);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

fn saved_at(id: &str) -> Value {
    let Some(stamp) = id.strip_prefix("memory-").and_then(|rest| rest.get(..16)) else {
        return Value::Null;
    };
    let bytes = stamp.as_bytes();
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(0..8) && bytes[8] == b't' && digits(9..15) && bytes[15] == b'z') {
        return Value::Null;
    }
    json!(format!(
        "{}-{}-{}T{}:{}:{}Z",
        &stamp[0..4],
        &stamp[4..6],
        &stamp[6..8],
        &stamp[9..11],
        &stamp[11..13],
        &stamp[13..15]
    ))
}

fn is_day(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

#[cfg(test)]
#[path = "../tests/support/memory_tools.rs"]
mod tests;
