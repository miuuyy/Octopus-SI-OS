use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const DECLARATION: &str = "file_writer_target";

#[cfg(test)]
#[path = "../tests/support/writer_indeterminate.rs"]
mod indeterminate_tests;

#[derive(Default)]
pub(super) struct WriterCallIndex {
    generations: BTreeMap<PathBuf, Option<u64>>,
    directories: BTreeMap<PathBuf, WriterCallDirectory>,
}

#[derive(Default)]
struct WriterCallDirectory {
    fingerprint: Option<(std::time::SystemTime, u64)>,
    by_generation: BTreeMap<u64, BTreeSet<PathBuf>>,
    unclassified: BTreeSet<PathBuf>,
}

impl WriterCallIndex {
    fn candidates(&mut self, root: &Path, generation: u64) -> Result<Vec<PathBuf>, String> {
        let fingerprint = match Self::fingerprint(root) {
            Ok(fingerprint) => fingerprint,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.directories.remove(root);
                self.generations
                    .retain(|path, _| path.parent() != Some(root));
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.to_string()),
        };
        let fresh = self
            .directories
            .get(root)
            .is_some_and(|directory| directory.fingerprint.as_ref() == Some(&fingerprint));
        if !fresh {
            let mut paths = BTreeSet::new();
            for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
                let path = entry.map_err(|error| error.to_string())?.path();
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".artifact.json"))
                {
                    paths.insert(path);
                }
            }
            self.generations
                .retain(|path, _| path.parent() != Some(root) || paths.contains(path));
            let mut directory = WriterCallDirectory::default();
            for path in paths {
                match self.generations.get(&path) {
                    Some(Some(generation)) => {
                        directory
                            .by_generation
                            .entry(*generation)
                            .or_default()
                            .insert(path);
                    }
                    Some(None) => {}
                    None => {
                        directory.unclassified.insert(path);
                    }
                }
            }
            let observed = Self::fingerprint(root).map_err(|error| error.to_string())?;
            if observed == fingerprint {
                directory.fingerprint = Some(fingerprint);
            }
            self.directories.insert(root.to_path_buf(), directory);
        }
        let directory = self
            .directories
            .get(root)
            .ok_or("writer directory index disappeared")?;
        Ok(directory
            .unclassified
            .iter()
            .chain(
                directory
                    .by_generation
                    .get(&generation)
                    .into_iter()
                    .flatten(),
            )
            .cloned()
            .collect())
    }

    fn fingerprint(root: &Path) -> Result<(std::time::SystemTime, u64), std::io::Error> {
        let metadata = std::fs::metadata(root)?;
        if !metadata.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "provider hand-in root is not a directory",
            ));
        }
        Ok((metadata.modified()?, metadata.len()))
    }

    fn record(&mut self, path: PathBuf, generation: Option<u64>) {
        let previous = self.generations.insert(path.clone(), generation);
        if let Some(directory) = path
            .parent()
            .and_then(|root| self.directories.get_mut(root))
        {
            directory.unclassified.remove(&path);
            if let Some(Some(previous)) = previous {
                if let Some(paths) = directory.by_generation.get_mut(&previous) {
                    paths.remove(&path);
                }
            }
            if let Some(generation) = generation {
                directory
                    .by_generation
                    .entry(generation)
                    .or_default()
                    .insert(path);
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/writer_call_index.rs"]
mod writer_call_index_tests;

#[cfg(test)]
#[path = "../tests/support/ordered_patch_storage.rs"]
mod ordered_patch_storage_tests;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriterRunBinding {
    pub run: RunId,
    pub initial_record: Value,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "octopus_file_write_contract::optional_json_format")]
    pub initial_record_format: Option<JsonFormat>,
    pub role_artifact_bindings: Vec<ArtifactBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Target {
    run: RunId,
    provider: InterfaceId,
    source: InterfaceId,
    door: InterfaceId,
    generation: u64,
}

impl WriterRunBinding {
    pub fn initial_bytes(&self) -> Result<Vec<u8>, String> {
        self.initial_record_format.unwrap_or_default().encode(&self.initial_record)
    }
    pub(super) fn roles(
        &self,
        record: &RunRecord,
        provider: &InterfaceId,
        templates: &[RunAuthoringCheckpoint],
    ) -> Result<Vec<ArtifactBinding>, String> {
        let initial =
            RunRecord::from_bytes(&self.initial_bytes()?).map_err(|e| e.to_string())?;
        let target = target(&initial, provider)?;
        if self.run == record.id
            || initial.id != self.run
            || initial.worker != record.worker
            || target.run != record.id
            || target.provider != *provider
            || target.generation != 0
            || initial.checkpoints.len() != 2
            || initial.active_checkpoints.len() != 2
        {
            return Err(
                "separate writer declaration changed its exact run, worker or initial boundary"
                    .into(),
            );
        }
        let role = |role| -> Result<&RunAuthoringCheckpoint, String> {
            let matches = templates
                .iter()
                .filter(|t| t.role == role)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [one] => Ok(*one),
                _ => Err("separate writer has ambiguous templates".into()),
            }
        };
        let call = role(RunAuthoringCheckpointRole::AuthoringCall)?;
        let result = role(RunAuthoringCheckpointRole::AuthoringResult)?;
        if call.interface() != &target.source || result.interface() != &target.door {
            return Err("separate writer declaration has a different exact source or door".into());
        }
        let frontier = initial.checkpoints.iter().collect::<Vec<_>>();
        canonical_role_bindings(&frontier, call, result, &initial.id)?;
        if self.role_artifact_bindings.iter().any(|b| !b.access.read) {
            return Err("writer target roles must be readable".into());
        }
        Ok(self.role_artifact_bindings.clone())
    }
}

fn bytes(value: &Value) -> Result<Vec<u8>, String> {
    JsonFormat::Compact.encode(value)
}

pub(super) fn binding(
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<Option<WriterRunBinding>, String> {
    record
        .interface_state
        .get(provider)
        .and_then(|s| s.get(RUN_AUTHORING_BINDING))
        .and_then(|v| v.get("writer_run"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| e.to_string())
}

pub fn owner_writer_binding(
    record: &RunRecord,
    owner: &InterfaceId,
) -> Result<Option<WriterRunBinding>, String> {
    record
        .interface_state
        .get(owner)
        .and_then(|s| s.get("writer_run"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| e.to_string())
}

fn owner_config(
    record: &RunRecord,
    writer: &CheckedRunWriter,
) -> Result<RunAuthoringConfig, String> {
    if record
        .interface_state
        .get(&writer.owner)
        .is_some_and(|s| s.contains_key(RUN_AUTHORING_BINDING))
    {
        return Ok(validate_declared_provider_writer(record, writer)?.config(
            &RunAuthoringProfile {
                file_write_tool: writer.tool.clone(),
                file_write_label: writer.call_label.clone(),
                file_write_result_label: writer.result_label.clone(),
                result_timeout_ms: 1,
            },
        ));
    }
    let bound =
        owner_writer_binding(record, &writer.owner)?.ok_or("owner has no declared writer Run")?;
    let initial =
        RunRecord::from_bytes(&bound.initial_bytes()?).map_err(|e| e.to_string())?;
    let mut templates = Vec::new();
    for (name, role, source, target, label) in [
        (
            "call",
            "authoring_call",
            &writer.source,
            &writer.door,
            &writer.call_label,
        ),
        (
            "result",
            "authoring_result",
            &writer.door,
            &writer.owner,
            &writer.result_label,
        ),
    ] {
        let rows = initial
            .checkpoints
            .iter()
            .filter(|cp| {
                cp.inbound_interface() == Some(source) && cp.outbound_interfaces().eq([target])
            })
            .collect::<Vec<_>>();
        let [cp] = rows.as_slice() else {
            return Err("owner writer Run has ambiguous exact edges".into());
        };
        if !cp
            .interface_binding
            .as_ref()
            .is_some_and(|b| b.config.artifact_policy.admits(label))
        {
            return Err("owner writer label is not admitted by its binding".into());
        }
        templates.push(serde_json::from_value(json!({"name":name,"role":role,"interface_binding":cp.interface_binding,
            "outbound_interface_bindings":cp.outbound_interface_bindings,"required_verified_artifacts":cp.required_verified_artifacts,"include_context":false})).map_err(|e|e.to_string())?);
    }
    bound.roles(record, &writer.owner, &templates)?;
    Ok(RunAuthoringConfig {
        run: record.id.clone(),
        worker: record.worker.clone(),
        provider_interface: writer.owner.clone(),
        authoring_source_interface: writer.source.clone(),
        file_write_tool: writer.tool.clone(),
        file_write_label: writer.call_label.clone(),
        file_write_result_label: writer.result_label.clone(),
        payload_artifact_bindings: vec![],
        writer_run: Some(bound),
        checkpoints: templates,
        result_timeout_ms: 1,
    })
}

pub fn separate_owner_writer_run(
    record: &mut Value,
    writer: &CheckedRunWriter,
    run: &RunId,
) -> Result<Value, String> {
    let parsed = RunRecord::from_bytes(&bytes(record)?).map_err(|e| e.to_string())?;
    if let Some(prior) = owner_writer_binding(&parsed, &writer.owner)? {
        if prior.run != *run {
            return Err("owner writer Run identity changed".into());
        }
        owner_config(&parsed, writer)?;
        return Ok(prior.initial_record);
    }
    let owned: BTreeSet<String> = serde_json::from_value(
        record["interface_state"][writer.owner.as_str()]["owned_checkpoint_ids"].clone(),
    )
    .map_err(|e| e.to_string())?;
    let mut ids = BTreeSet::new();
    let mut rows = Vec::new();
    for (source, target) in [
        (&writer.source, &writer.door),
        (&writer.door, &writer.owner),
    ] {
        let candidates = parsed
            .checkpoints
            .iter()
            .filter(|cp| {
                owned.contains(cp.id.as_str())
                    && parsed.active_checkpoints.contains(&cp.id)
                    && cp.inbound_interface() == Some(source)
                    && cp.outbound_interfaces().eq([target])
            })
            .collect::<Vec<_>>();
        let [cp] = candidates.as_slice() else {
            return Err("owner needs its exact active call/result pair".into());
        };
        let mut row = serde_json::to_value(cp).map_err(|e| e.to_string())?;
        row["run"] = json!(run);
        row["trace"] = json!(format!("trace:{run}:{}", cp.id));
        for requirement in row["required_verified_artifacts"]
            .as_array_mut()
            .ok_or("writer requirements absent")?
        {
            if let Some(matches) = requirement
                .pointer_mut("/params/metadata_matches")
                .and_then(Value::as_array_mut)
            {
                for item in matches {
                    if item["payload"] == "/arguments/expected_before_hash"
                        && item["metadata"] == "/runtime/run_record_hash"
                    {
                        item["payload"] = json!("/arguments/writes/0/expected_before_hash");
                    }
                }
            }
        }
        ids.insert(cp.id.to_string());
        rows.push(row);
    }
    let initial = json!({"id":run,"ee":parsed.ee,"worker":parsed.worker,"trace":format!("trace:{run}"),"status":"idle",
        "interface_state":{writer.owner.as_str():{DECLARATION:Target {run:parsed.id,provider:writer.owner.clone(),source:writer.source.clone(),door:writer.door.clone(),generation:0}}},
        "checkpoints":rows,"active_checkpoints":ids});
    let bound = WriterRunBinding {
        run: run.clone(),
        initial_record: initial.clone(),
        initial_record_format: Some(JsonFormat::Compact),
        role_artifact_bindings: serde_json::from_value(rows[0]["artifact_bindings"].clone())
            .map_err(|e| e.to_string())?,
    };
    for key in ["checkpoints", "active_checkpoints"] {
        record[key]
            .as_array_mut()
            .ok_or("owner checkpoints absent")?
            .retain(|row| {
                !ids.contains(if key == "checkpoints" {
                    row["id"].as_str().unwrap_or_default()
                } else {
                    row.as_str().unwrap_or_default()
                })
            });
    }
    let state = &mut record["interface_state"][writer.owner.as_str()];
    for key in ["owned_checkpoint_ids", FRONTIER_CHECKPOINT_IDS] {
        state[key]
            .as_array_mut()
            .ok_or("owner frontier absent")?
            .retain(|id| !ids.contains(id.as_str().unwrap_or_default()));
    }
    state["writer_run"] = serde_json::to_value(bound).map_err(|e| e.to_string())?;
    owner_config(
        &RunRecord::from_bytes(&bytes(record)?).map_err(|e| e.to_string())?,
        writer,
    )?;
    Ok(initial)
}

fn target(record: &RunRecord, provider: &InterfaceId) -> Result<Target, String> {
    serde_json::from_value(
        record
            .interface_state
            .get(provider)
            .and_then(|s| s.get(DECLARATION))
            .ok_or("writer Run has no exact target declaration")?
            .clone(),
    )
    .map_err(|e| e.to_string())
}

pub(super) fn consume_result(
    ctx: &InterfaceLiftContext,
    cache: &Mutex<BTreeMap<RunId, WriterHistory>>,
    profile: &RunAuthoringProfile,
    movement: &Movement,
    record: &RunRecord,
) -> Result<bool, String> {
    if !record
        .interface_state
        .get(&ctx.interface)
        .is_some_and(|s| s.contains_key(DECLARATION))
    {
        return Ok(false);
    }
    let target = target(record, &ctx.interface)?;
    let root = ctx
        .root_slot("run_records_root")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let bytes =
        std::fs::read(root.join(format!("{}.json", target.run))).map_err(|e| e.to_string())?;
    let original = RunRecord::from_bytes(&bytes).map_err(|e| e.to_string())?;
    let config = config_from_persisted_profile(&original, &ctx.interface, profile)?;
    if config
        .writer_run
        .as_ref()
        .is_none_or(|b| b.run != record.id)
    {
        return Err("writer result target no longer declares this exact writer Run".into());
    }
    let history = history(ctx, cache, &config)?;
    if history
        .completed
        .values()
        .any(|(seq, _)| *seq == movement.seq.0)
        || history.voided.contains_key(&movement.seq.0)
    {
        Ok(true)
    } else {
        Err("writer result has no exact checked ordered effect".into())
    }
}

pub fn separate_writer_run(
    record: &mut Value,
    provider: &InterfaceId,
    run: &RunId,
) -> Result<Value, String> {
    let original = RunRecord::from_bytes(&bytes(record)?).map_err(|e| e.to_string())?;
    let mut topology: BoundRunAuthoringTopology = serde_json::from_value(
        record["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING].clone(),
    )
    .map_err(|e| e.to_string())?;
    topology.validate_identity(&original, provider)?;
    let prior = topology.writer_run.clone();
    if prior.as_ref().is_some_and(|binding| &binding.run != run) {
        return Err("writer continuation changed its exact Run".into());
    }
    let mut ids = BTreeSet::new();
    let mut checkpoints = Vec::new();
    for template in topology.checkpoints.iter_mut().filter(|cp| {
        matches!(
            cp.role,
            RunAuthoringCheckpointRole::AuthoringCall | RunAuthoringCheckpointRole::AuthoringResult
        )
    }) {
        let matches = original
            .checkpoints
            .iter()
            .filter(|cp| {
                original.active_checkpoints.contains(&cp.id)
                    && checkpoint_record_matches_template(cp, template, &original.id)
            })
            .collect::<Vec<_>>();
        let cp = match matches.as_slice() {
            [cp] => Some(*cp),
            [] if prior.is_some() => None,
            _ => {
                return Err(
                    "separate writer needs one exact active checkpoint per authoring role".into(),
                );
            }
        };
        if template.role == RunAuthoringCheckpointRole::AuthoringCall {
            for requirement in &mut template.required_verified_artifacts {
                if let Some(matches) = requirement
                    .params
                    .get_mut("metadata_matches")
                    .and_then(Value::as_array_mut)
                {
                    for requirement in matches {
                        if requirement["payload"] == "/arguments/expected_before_hash"
                            && requirement["metadata"] == "/runtime/run_record_hash"
                        {
                            requirement["payload"] =
                                json!("/arguments/writes/0/expected_before_hash");
                        }
                    }
                }
            }
        }
        if let Some(cp) = cp {
            ids.insert(cp.id.to_string());
            let mut row = serde_json::to_value(cp).map_err(|e| e.to_string())?;
            row["run"] = json!(run);
            row["trace"] = json!(format!("trace:{run}:{}", cp.id));
            row["required_verified_artifacts"] = json!(template.required_verified_artifacts);
            checkpoints.push(row);
        }
    }
    if checkpoints.len() != 2 && !(checkpoints.is_empty() && prior.is_some()) {
        return Err("writer declaration has an incomplete authoring pair".into());
    }
    let roles = checkpoints
        .first()
        .map(|cp| {
            serde_json::from_value::<Vec<ArtifactBinding>>(cp["artifact_bindings"].clone())
                .map_err(|e| e.to_string())
        })
        .transpose()?;
    let mut detached = if let Some(mut prior) = prior {
        if let Some(roles) = roles {
            prior.role_artifact_bindings = roles;
        }
        prior
    } else {
        let door = topology
            .checkpoints
            .iter()
            .find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringResult)
            .ok_or("writer result absent")?
            .interface()
            .clone();
        let initial = json!({
            "id":run,"ee":original.ee,"worker":original.worker,"trace":format!("trace:{run}"),"status":"idle",
            "interface_state":{provider.as_str():{DECLARATION:Target {run:original.id.clone(), provider:provider.clone(), source:topology.authoring_source_interface.clone(), door, generation:0}}},
            "checkpoints":checkpoints,"active_checkpoints":ids,
        });
        WriterRunBinding {
            run: run.clone(),
            initial_record: initial,
            initial_record_format: Some(JsonFormat::Compact),
            role_artifact_bindings: roles.ok_or("writer roles absent")?,
        }
    };
    detached.roles(&original, provider, &topology.checkpoints)?;
    for key in ["checkpoints", "active_checkpoints"] {
        record[key]
            .as_array_mut()
            .ok_or("writer target lost its checkpoints")?
            .retain(|row| {
                !ids.contains(if key == "checkpoints" {
                    row["id"].as_str().unwrap_or_default()
                } else {
                    row.as_str().unwrap_or_default()
                })
            });
    }
    for state in record["interface_state"]
        .as_object_mut()
        .ok_or("run state absent")?
        .values_mut()
    {
        for key in ["cycle_checkpoint_ids", FRONTIER_CHECKPOINT_IDS] {
            if let Some(rows) = state.get_mut(key).and_then(Value::as_array_mut) {
                rows.retain(|id| !ids.contains(id.as_str().unwrap_or_default()));
            }
        }
    }
    topology.writer_run = Some(detached.clone());
    record["interface_state"][provider.as_str()][RUN_AUTHORING_BINDING] =
        serde_json::to_value(&topology).map_err(|e| e.to_string())?;
    let next = RunRecord::from_bytes(&bytes(record)?).map_err(|e| e.to_string())?;
    validate_checkpoint_topology(&next, provider, &topology.checkpoints, None)?;
    Ok(std::mem::take(&mut detached.initial_record))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OrderedArguments {
    ordered: bool,
    writes: Vec<OrderedEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerWriterEdit {
    arguments: OrderedArguments,
    writer: CheckedRunWriter,
    correlation_id: String,
}

#[derive(Default)]
pub struct OwnerWriterEvidence(Mutex<BTreeMap<RunId, WriterHistory>>);

impl OwnerWriterEdit {
    pub fn prepare(
        before: &[u8],
        edit: &CheckedRunEdit,
        writer: &CheckedRunWriter,
        max_payload_bytes: u64,
    ) -> Result<Option<Self>, String> {
        let record = RunRecord::from_bytes(before).map_err(|e| e.to_string())?;
        let declared = if record.interface_state.get(&writer.owner)
            .is_some_and(|state| state.contains_key(RUN_AUTHORING_BINDING)) {
            binding(&record, &writer.owner)?
        } else {
            owner_writer_binding(&record, &writer.owner)?
        };
        let Some(binding) = declared else {
            return Ok(None);
        };
        let config = owner_config(&record, writer)?;
        let path = edit
            .path
            .parent()
            .ok_or("writer edit target has no parent")?
            .join(format!("{}.json", binding.run));
        let current = std::fs::read(&path).map_err(|e| e.to_string())?;
        let next = next_writer(&current, &config, JsonFormat::Compact, 0)?;
        let arguments = OrderedArguments {
            ordered: true,
            writes: vec![
                OrderedEntry::prepare(path, &current, &next, Some(JsonFormat::Compact))?,
                OrderedEntry::prepare(edit.path.clone(), before, edit.content.as_bytes(), edit.json_format)?,
            ],
        };
        let result = Self {
            arguments,
            writer: writer.clone(),
            correlation_id: edit.correlation_id.clone(),
        };
        result.validate(before, edit, writer, max_payload_bytes)?;
        Ok(Some(result))
    }

    pub fn validate(
        &self,
        before: &[u8],
        edit: &CheckedRunEdit,
        writer: &CheckedRunWriter,
        max_payload_bytes: u64,
    ) -> Result<(), String> {
        if self.writer != *writer || self.correlation_id != edit.correlation_id {
            return Err("owner ordered edit changed its writer or correlation".into());
        }
        let first = self
            .arguments
            .writes
            .first()
            .ok_or("owner writer advance absent")?;
        let (pinned, args) = self.arguments.checked_call(
            writer,
            first.before(max_payload_bytes)?.as_bytes(),
            &edit.path,
            max_payload_bytes,
        )?;
        if pinned != before
            || args.expected_before_hash != edit.before_hash
            || args.expected_after_hash != edit.after_hash
            || args.patch != edit.patch || args.json_format != edit.json_format
        {
            return Err("owner ordered edit changed its exact target intent".into());
        }
        Ok(())
    }

    pub fn materialize(&self, root: &Path) -> Result<InterfaceArtifactFile, String> {
        InterfaceArtifactFile::write(root,&self.writer.call_label,&serde_json::to_vec(&json!({"tool":self.writer.tool,"arguments":self.arguments,"correlation_id":self.correlation_id})).map_err(|e|e.to_string())?).map_err(|e|e.to_string())
    }

    pub fn matches_call(&self, value: &Value) -> bool {
        *value
            == json!({"tool":self.writer.tool,"arguments":self.arguments,"correlation_id":self.correlation_id})
    }
}

impl OwnerWriterEvidence {
    pub fn result(
        &self,
        ctx: &InterfaceLiftContext,
        attempt: &OwnerWriterEdit,
    ) -> Result<Option<bool>, String> {
        let entry = attempt
            .arguments
            .writes
            .get(1)
            .ok_or("ordered owner target absent")?;
        let maximum = ctx.required_host_io_limits()?.max_payload_bytes;
        let before = RunRecord::from_bytes(entry.before(maximum)?.as_bytes())
            .map_err(|e| e.to_string())?;
        let config = owner_config(&before, &attempt.writer)?;
        let progress = history(ctx, &self.0, &config)?;
        if let Some(refusal) = progress.refused.get(&entry.after_hash()) {
            return Err(format!("owner ordered file write was refused before any effect: {refusal}"));
        }
        let result = progress
            .completed
            .get(&ContentHash::of_bytes(entry.content(maximum)?.as_bytes()).to_hex())
            .map(|(_, applied)| *applied);
        if result.is_none()
            && progress.current
                != attempt.arguments.writes[0]
                    .before(maximum)?
                    .as_bytes()
        {
            return Err("owner writer preimage has no exact checked history".into());
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OrderedPatchEntry {
    path: PathBuf,
    expected_before_content: Preimage,
    expected_before_hash: String,
    patch: json_patch::Patch,
    expected_after_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "octopus_file_write_contract::optional_json_format")]
    json_format: Option<JsonFormat>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OrderedContentEntry {
    path: PathBuf,
    content: String,
    expected_before_content: String,
    expected_before_hash: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum OrderedEntry {
    Patch(OrderedPatchEntry),
    Content(OrderedContentEntry),
}

impl OrderedEntry {
    fn prepare(path: PathBuf, before: &[u8], after: &[u8], json_format: Option<JsonFormat>) -> Result<Self, String> {
        let entry = OrderedPatchEntry {
            path,
            expected_before_content: Preimage::encode(before)?,
            expected_before_hash: ContentHash::of_bytes(before).to_hex(),
            patch: json_patch::diff(
                &serde_json::from_slice(before).map_err(|error| error.to_string())?,
                &serde_json::from_slice(after).map_err(|error| error.to_string())?,
            ),
            expected_after_hash: ContentHash::of_bytes(after).to_hex(),
            json_format,
        };
        if apply_run_json_patch(before, &entry.patch, &entry.expected_after_hash, json_format)?.as_bytes() != after {
            return Err("ordered write patch did not reconstruct its exact postimage".into());
        }
        Ok(Self::Patch(entry))
    }

    fn path(&self) -> &Path {
        match self { Self::Patch(entry) => &entry.path, Self::Content(entry) => &entry.path }
    }

    fn json_format(&self) -> Option<JsonFormat> {
        match self { Self::Patch(entry) => entry.json_format, Self::Content(_) => None }
    }

    fn after_hash(&self) -> String {
        match self { Self::Patch(entry) => entry.expected_after_hash.clone(),
            Self::Content(entry) => ContentHash::of_bytes(entry.content.as_bytes()).to_hex() }
    }

    fn before(&self, maximum: u64) -> Result<String, String> {
        self.decode_before(&mut DecodeBudget::new(maximum)?)
    }

    fn decode_before(&self, budget: &mut DecodeBudget) -> Result<String, String> {
        match self {
            Self::Patch(entry) => entry.expected_before_content.decode(&entry.expected_before_hash, budget),
            Self::Content(entry) => Preimage::Text(entry.expected_before_content.clone()).decode(&entry.expected_before_hash, budget),
        }
    }

    fn resolved(&self, budget: &mut DecodeBudget) -> Result<(String, String), String> {
        let before = self.decode_before(budget)?;
        let after = match self {
            Self::Patch(entry) => apply_run_json_patch(before.as_bytes(), &entry.patch, &entry.expected_after_hash, entry.json_format)?,
            Self::Content(entry) => entry.content.clone(),
        };
        budget.charge(after.len() as u64)?;
        Ok((before, after))
    }

    fn content(&self, maximum: u64) -> Result<String, String> {
        Ok(self.resolved(&mut DecodeBudget::new(maximum)?)?.1)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OrderedReceipt {
    path: PathBuf,
    before_hash: Option<String>,
    after_hash: String,
    bytes: u64,
    outcome: String,
    observed_hash: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderedResult {
    result: String,
    writes: Vec<OrderedReceipt>,
}

fn next_writer(before: &[u8], config: &RunAuthoringConfig, format: JsonFormat, minimum_generation: u64) -> Result<Vec<u8>, String> {
    let record = RunRecord::from_bytes(before).map_err(|e| e.to_string())?;
    let declaration = target(&record, &config.provider_interface)?;
    let binding = config
        .writer_run
        .as_ref()
        .ok_or("writer run not selected")?;
    if record.id != binding.run
        || record.worker != config.worker
        || declaration.run != config.run
        || declaration.provider != config.provider_interface
        || declaration.source != config.authoring_source_interface
    {
        return Err("writer continuation changed exact owner or target".into());
    }
    let generation = declaration
        .generation
        .checked_add(1)
        .ok_or("writer generation overflow")?
        .max(minimum_generation);
    let initial =
        RunRecord::from_bytes(&binding.initial_bytes()?).map_err(|e| e.to_string())?;
    if record.ee != initial.ee || record.trace != initial.trace || record.status != initial.status {
        return Err("writer continuation changed EE, trace or status".into());
    }
    if writer_generation(
        binding,
        &config.provider_interface,
        declaration.generation,
    )? != serde_json::from_slice::<Value>(before).map_err(|error| error.to_string())?
    {
        return Err("writer Run differs from its declared generation".into());
    }
    format.encode(&writer_generation(
        binding,
        &config.provider_interface,
        generation,
    )?)
}

fn writer_generation(
    binding: &WriterRunBinding,
    provider: &InterfaceId,
    generation: u64,
) -> Result<Value, String> {
    let mut next = binding.initial_record.clone();
    if generation == 0 {
        return Ok(next);
    }
    next["interface_state"][provider.as_str()][DECLARATION]["generation"] = json!(generation);
    let mut ids = Vec::new();
    for cp in next["checkpoints"]
        .as_array_mut()
        .ok_or("writer checkpoints absent")?
    {
        let id = format!(
            "{}_{}",
            cp["id"].as_str().ok_or("writer id absent")?,
            generation
        );
        cp["id"] = json!(id);
        cp["trace"] = json!(format!("trace:{}:{id}", binding.run));
        ids.push(id);
    }
    next["active_checkpoints"] = json!(ids);
    Ok(next)
}

impl OrderedArguments {
    pub fn targets(&self, path: &Path) -> bool {
        self.writes.get(1).is_some_and(|entry| entry.path() == path)
    }

    pub fn checked_call(
        &self,
        writer: &CheckedRunWriter,
        snapshot: &[u8],
        path: &Path,
        max_payload_bytes: u64,
    ) -> Result<(Vec<u8>, FileWriteCallArguments), String> {
        let [advance, edit] = self.writes.as_slice() else {
            return Err(
                "provider ordered write needs exactly its writer and target entries".into(),
            );
        };
        let mut budget = DecodeBudget::new(max_payload_bytes)?;
        let (advance_before, advance_content) = advance.resolved(&mut budget)?;
        let (edit_before, edit_content) = edit.resolved(&mut budget)?;
        if !self.ordered
            || edit.path() != path
            || !path.is_absolute()
            || advance.path().parent() != path.parent()
            || advance_before.as_bytes() != snapshot
        {
            return Err(
                "provider ordered write changed its exact paths or admitted writer preimage".into(),
            );
        }
        let before = edit_before.into_bytes();
        let record = RunRecord::from_bytes(&before).map_err(|e| e.to_string())?;
        let config = owner_config(&record, writer)?;
        let bound = config
            .writer_run
            .as_ref()
            .ok_or("ordered call target has no declared writer Run")?;
        bound.roles(&record, &writer.owner, &config.checkpoints)?;
        let advanced = RunRecord::from_bytes(advance_content.as_bytes()).map_err(|e| e.to_string())?;
        let generation = target(&advanced, &writer.owner)?.generation;
        if generation <= target(&RunRecord::from_bytes(snapshot).map_err(|e| e.to_string())?, &writer.owner)?.generation {
            return Err("ordered writer generation did not advance".into());
        }
        if advance.path().file_name().and_then(|s| s.to_str()) != Some(&format!("{}.json", bound.run))
            || edit.path().file_name().and_then(|s| s.to_str())
                != Some(&format!("{}.json", record.id))
            || next_writer(snapshot, &config, advance.json_format().unwrap_or_default(), generation)? != advance_content.as_bytes()
        {
            return Err("ordered call does not author exactly the next writer generation".into());
        }
        let prospective =
            RunRecord::from_bytes(edit_content.as_bytes()).map_err(|e| e.to_string())?;
        if record.id != prospective.id
            || record.ee != prospective.ee
            || record.worker != prospective.worker
        {
            return Err("ordered call changes its target identity".into());
        }
        let arguments = FileWriteCallArguments {
            path: edit.path().to_path_buf(),
            patch: match edit {
                OrderedEntry::Patch(entry) => entry.patch.clone(),
                OrderedEntry::Content(_) => json_patch::diff(
                    &serde_json::from_slice(&before).map_err(|error| error.to_string())?,
                    &serde_json::from_str(&edit_content).map_err(|error| error.to_string())?),
            },
            expected_before_hash: ContentHash::of_bytes(&before).to_hex(),
            expected_after_hash: edit.after_hash(),
            json_format: edit.json_format(),
        };
        if apply_run_json_patch(&before, &arguments.patch, &arguments.expected_after_hash, arguments.json_format)?
            != edit_content
        {
            return Err("ordered call target is not the exact canonical patched JSON".into());
        }
        Ok((before, arguments))
    }

    pub fn checked_result(
        &self,
        writer: &CheckedRunWriter,
        metadata: &InvocationMetadata,
        snapshot: &[u8],
        result: &ToolResultArtifact,
        max_payload_bytes: u64,
        call_seq: u64,
    ) -> Result<OrderedTargetEffect, String> {
        let [advance, edit] = self.writes.as_slice() else {
            return Err("ordered result lost its two exact writes".into());
        };
        let record = RunRecord::from_bytes(snapshot).map_err(|e| e.to_string())?;
        let runtime = metadata
            .runtime
            .as_ref()
            .ok_or("ordered result has no runtime coordinate")?;
        let cp = record
            .checkpoint(&runtime.checkpoint)
            .ok_or("ordered result checkpoint absent")?;
        let inbound = cp
            .interface_binding
            .clone()
            .ok_or("ordered result inbound absent")?;
        let mut budget = DecodeBudget::new(max_payload_bytes)?;
        let decoded = [advance.resolved(&mut budget)?, edit.resolved(&mut budget)?];
        let indeterminate = !result.ok && result.body.is_none() && result.error.as_ref().is_some_and(|error| {
            error.code == "tool_effect_indeterminate"
                && error.details.as_ref().is_some_and(|details| {
                    details["source_movement_seq"].as_u64() == Some(call_seq)
                        && details["failure_details"]["source_movement_seq"].as_u64() == Some(call_seq)
                        && details["failure_details"]["tool"].as_str() == Some(writer.tool.as_str())
                        && details["failure_details"]["effect_state"].as_str() == Some("in_flight_outcome_unknown")
                })
        });
        let unresolved = indeterminate || definite_refusal(result).is_some();
        let stopped = writer_stop(result);
        let writer_unadvanced = snapshot == decoded[0].0.as_bytes();
        let mismatches = [
            (snapshot != decoded[0].1.as_bytes() && !((unresolved || stopped.is_some()) && writer_unadvanced), "run record bytes"),
            (runtime.run != record.id, "run"),
            (runtime.ee != record.ee, "execution environment"),
            (runtime.worker != record.worker, "worker"),
            (runtime.interface != writer.owner, "owner interface"),
            (runtime.epoch != cp.epoch.0, "checkpoint epoch"),
            (!record.active_checkpoints.contains(&cp.id), "active checkpoint"),
            (inbound.interface != writer.door, "inbound door"),
            (!cp.outbound_interfaces().eq([&writer.owner]), "checkpoint outbound"),
            (metadata.checkpoint_binding_policy.as_ref()
                != Some(&CheckpointBindingPolicySnapshot {
                    inbound,
                    outbound: cp.outbound_interface_bindings.clone(),
                }), "binding policy"),
            (metadata.outbound_interface_bindings != [writer.owner.clone()], "invocation outbound"),
            (metadata.verification_failure.is_some(), "verification failure"),
            (!metadata.required_verified_artifacts.is_empty() || !cp.required_verified_artifacts.is_empty(), "required verified artifacts"),
            (!artifact_refs_match_bindings(&metadata.artifact_binding_refs, &cp.artifact_bindings), "artifact bindings"),
            (!metadata.checkpoint_admission.as_ref().is_some_and(|a| {
                a.complete
                    && a.artifact_label == writer.result_label
                    && a.required_artifact_labels.is_empty()
            }), "checkpoint admission"),
        ];
        let differing = mismatches.iter().filter(|(differs, _)| *differs).map(|(_, field)| *field).collect::<Vec<_>>();
        if !differing.is_empty() {
            let reported = match result.error.as_ref().filter(|_| !result.ok && !indeterminate) {
                Some(error) => format!("; {} reported {}: {}", writer.tool, error.code, error.message),
                None => String::new(),
            };
            return Err(format!(
                "ordered result differs from its newly admitted exact writer checkpoint in: {}{reported}",
                differing.join(", ")
            ));
        }
        if result.tool != writer.tool || result.label != writer.result_label {
            return Err("ordered result changed tool or label".into());
        }
        if unresolved {
            return Ok(if writer_unadvanced { OrderedTargetEffect::UncertainWriter } else { OrderedTargetEffect::Uncertain });
        }
        let body = if result.ok {
            if result.error.is_some() {
                return Err("successful ordered result carries an error".into());
            }
            result.body.as_ref().ok_or("ordered result body absent")?
        } else {
            let error = result.error.as_ref().ok_or("ordered result error absent")?;
            if result.body.is_some() || error.code != "file_write_incomplete" {
                return Err(format!(
                    "ordered file write failed without complete effect evidence: {}",
                    error.message
                ));
            }
            error
                .details
                .as_ref()
                .and_then(|d| d.get("file_write"))
                .ok_or("ordered failure has no partial effect evidence")?
        };
        let body: OrderedResult =
            serde_json::from_value(body.clone()).map_err(|e| e.to_string())?;
        if body.result != "ordered_file_writes" || body.writes.len() != 2 {
            return Err("ordered result has a different operation or entry count".into());
        }
        for ((entry, receipt), (before, content)) in [advance, edit].into_iter().zip(&body.writes).zip(&decoded) {
            if receipt.path != entry.path()
                || receipt.before_hash.as_deref()
                    != Some(
                        &ContentHash::of_bytes(before.as_bytes()).to_hex(),
                    )
                || receipt.after_hash != entry.after_hash()
                || receipt.bytes != content.len() as u64
            {
                return Err("ordered result changed a pinned entry".into());
            }
            if receipt.outcome == "applied"
                && (receipt.observed_hash.as_ref() != Some(&receipt.after_hash)
                    || receipt.error.is_some())
            {
                return Err("ordered applied result lacks exact readback evidence".into());
            }
        }
        if body.writes[0].outcome != "applied" {
            return match (stopped, writer_unadvanced) {
                (Some(_), true) => Ok(OrderedTargetEffect::UncertainWriter),
                (Some("uncertain"), false) => Ok(OrderedTargetEffect::Uncertain),
                _ => Err("ordered writer Run was not advanced".into()),
            };
        }
        match body.writes[1].outcome.as_str() {
            "applied" if result.ok => Ok(OrderedTargetEffect::Applied),
            "refused" if !result.ok && body.writes[1].error.is_some() => Ok(OrderedTargetEffect::Refused),
            "uncertain" if !result.ok && body.writes[1].error.is_some() => Ok(OrderedTargetEffect::Uncertain),
            _ => Err("ordered target write has no exact outcome".into()),
        }
    }

    fn target_hashes(&self, maximum: u64) -> Result<(String, String), String> {
        let edit = self.writes.get(1).ok_or("ordered call target absent")?;
        Ok((ContentHash::of_bytes(edit.before(maximum)?.as_bytes()).to_hex(), edit.after_hash()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OrderedTargetEffect {
    Applied,
    Refused,
    Uncertain,
    UncertainWriter,
}

pub(super) fn definite_refusal(result: &ToolResultArtifact) -> Option<String> {
    let error = result.error.as_ref().filter(|_| !result.ok && result.body.is_none())?;
    (!matches!(error.code.as_str(), "tool_effect_indeterminate" | "file_write_incomplete"))
        .then(|| format!("{}: {}", error.code, error.message))
}

fn writer_stop(result: &ToolResultArtifact) -> Option<&str> {
    let error = result.error.as_ref().filter(|error| !result.ok && result.body.is_none() && error.code == "file_write_incomplete")?;
    let writes = error.details.as_ref()?.get("file_write")?.get("writes")?.as_array()?;
    let [writer, target] = writes.as_slice() else { return None };
    (target["outcome"].as_str() == Some("not_attempted"))
        .then(|| writer["outcome"].as_str()).flatten().filter(|outcome| matches!(*outcome, "refused" | "uncertain"))
}

pub(super) fn target_untouched(result: &ToolResultArtifact, effect: OrderedTargetEffect) -> bool {
    writer_stop(result).is_some() || (effect == OrderedTargetEffect::UncertainWriter && definite_refusal(result).is_some())
}

pub(super) fn untouched_refusal(result: &ToolResultArtifact) -> Option<String> {
    definite_refusal(result).or_else(|| (writer_stop(result) == Some("refused")).then(|| result.error.as_ref()
        .map(|error| format!("{}: {}", error.code, error.message))).flatten())
}

#[derive(Clone)]
struct UnresolvedTarget {
    seq: u64,
    args: OrderedArguments,
    call: Movement,
    result: Movement,
    writer_advanced: bool,
}

#[derive(Clone)]
pub(super) struct WriterHistory {
    binding: WriterRunBinding,
    next_seq: u64,
    current: Vec<u8>,
    pending: Option<(String, OrderedArguments, Movement)>,
    completed: BTreeMap<String, (u64, bool)>,
    applied_calls: BTreeMap<String, u64>,
    last_applied_target: Option<(u64, Vec<u8>)>,
    unresolved: Option<UnresolvedTarget>,
    voided: BTreeMap<u64, u64>,
    refused: BTreeMap<String, String>,
    cursor: Option<(u64, u64, ContentHash)>,
}

fn writer(config: &RunAuthoringConfig) -> Result<CheckedRunWriter, String> {
    Ok(CheckedRunWriter {
        owner: config.provider_interface.clone(),
        source: config.authoring_source_interface.clone(),
        door: config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?
            .interface()
            .clone(),
        tool: config.file_write_tool.clone(),
        call_label: config.file_write_label.clone(),
        result_label: config.file_write_result_label.clone(),
    })
}

fn history(
    ctx: &InterfaceLiftContext,
    cache: &Mutex<BTreeMap<RunId, WriterHistory>>,
    config: &RunAuthoringConfig,
) -> Result<WriterHistory, String> {
    let binding = config
        .writer_run
        .as_ref()
        .ok_or("writer Run not selected")?;
    let mut cache = cache.lock().map_err(|_| "writer evidence cache poisoned")?;
    let saved = cache.entry(binding.run.clone()).or_insert(WriterHistory {
        binding: binding.clone(),
        next_seq: 0,
        current: binding.initial_bytes()?,
        pending: None,
        completed: BTreeMap::new(),
        applied_calls: BTreeMap::new(),
        last_applied_target: None,
        unresolved: None,
        voided: BTreeMap::new(),
        refused: BTreeMap::new(),
        cursor: None,
    });
    if saved.binding.run != binding.run || saved.binding.initial_record != binding.initial_record || saved.binding.initial_record_format != binding.initial_record_format {
        return Err("writer evidence cache declaration changed".into());
    }
    let mut current = saved.clone();
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence {
        movements_path: &ctx.movements_path,
        ledger_index_path: &ctx.ledger_index_path,
        blobs_dir: &ctx.blobs_dir,
        max_control_frame_bytes: limits.max_control_frame_bytes,
        max_manifest_bytes: limits.max_manifest_bytes,
        max_payload_bytes: limits.max_payload_bytes,
    };
    let mut reader = FileCheckedRunReader::open(&evidence, None)?.with_provider_context(ctx);
    if let Some((start, end, hash)) = current.cursor {
        if reader
            .reader
            .get_ref()
            .metadata()
            .map_err(|e| e.to_string())?
            .len()
            < end
        {
            return Err("writer evidence ledger was truncated".into());
        }
        reader
            .reader
            .seek(std::io::SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        let BoundedLedgerRow::Complete(row) =
            read_bounded_ledger_row(&mut reader.reader, limits.max_control_frame_bytes)?
        else {
            return Err("writer evidence cursor lost its durable boundary".into());
        };
        if ContentHash::of_bytes(&row) != hash
            || reader.reader.stream_position().map_err(|e| e.to_string())? != end
        {
            return Err("writer evidence boundary changed".into());
        }
        reader.next_seq = current.next_seq;
    }
    let mut last_start = None;
    let declared = writer(config)?;
    let mut indexed = reader.indexed_outbound_edges(&[
        (&declared.source, &declared.door), (&declared.door, &declared.owner),
    ])?;
    let target_path = ctx
        .root_slot("run_records_root")?
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join(format!("{}.json", config.run));
    loop {
        if let Some(row) = indexed.as_mut().and_then(|rows| rows.pop_front()) {
            reader.reader.seek(std::io::SeekFrom::Start(row.start_offset)).map_err(|error| error.to_string())?;
            reader.next_seq = row.seq;
        }
        let seq = reader.next_seq;
        let start = reader.reader.stream_position().map_err(|e| e.to_string())?;
        reader.snapshot_bytes = 0;
        let Some(movement) = reader.movement_with_grant_if(seq, |movement| {
            movement.kind == MovementKind::InterfaceOutbound
                && movement.runtime().is_some_and(|runtime| runtime.run == binding.run)
                && ((movement.from == Endpoint::Interface(declared.source.clone())
                    && movement.to == Endpoint::Interface(declared.door.clone()))
                    || (movement.from == Endpoint::Interface(declared.door.clone())
                        && movement.to == Endpoint::Interface(declared.owner.clone())))
        })? else {
            break;
        };
        last_start = Some(start);
        current.next_seq = seq
            .checked_add(1)
            .ok_or("writer evidence sequence overflow")?;
        if movement.kind != MovementKind::InterfaceOutbound {
            continue;
        }
        let call_edge = movement.from == Endpoint::Interface(declared.source.clone())
            && movement.to == Endpoint::Interface(declared.door.clone());
        let result_edge = movement.from == Endpoint::Interface(declared.door.clone())
            && movement.to == Endpoint::Interface(declared.owner.clone());
        if !call_edge && !result_edge {
            continue;
        }
        let metadata = movement
            .invocation_metadata
            .as_ref()
            .and_then(|m| m.invocation())
            .ok_or("writer movement metadata absent")?;
        let runtime = metadata
            .runtime
            .as_ref()
            .ok_or("writer movement coordinate absent")?;
        if runtime.run != binding.run {
            continue;
        }
        let reference = movement
            .artifact
            .as_ref()
            .ok_or("writer movement artifact absent")?;
        let (manifest, payload) = reader.artifact(reference)?;
        let hash = runtime
            .run_record_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .ok_or("writer snapshot hash absent")?;
        let snapshot = reader.snapshot(&hash)?;
        if ContentHash::of_bytes(&snapshot) != hash {
            return Err("writer snapshot integrity failed".into());
        }
        if call_edge {
            let call: octopus_tool_handlers::ToolCallArtifact =
                serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
            if manifest.label != declared.call_label
                || call.tool != declared.tool
                || current.pending.is_some()
            {
                return Err("writer call changed its declared route, sequential generation or proven preimage".into());
            }
            let args: OrderedArguments =
                serde_json::from_value(call.arguments).map_err(|e| e.to_string())?;
            let (before, _) = args.checked_call(&declared, &snapshot, &target_path, limits.max_payload_bytes)?;
            if current.unresolved.is_some() {
                let observed = ContentHash::of_bytes(&before).to_hex();
                resolve_target_attempt(&mut current, &mut reader, &declared, &target_path, &config.provider_interface, limits.max_payload_bytes, &observed, Some(&snapshot))?;
            }
            if snapshot != current.current {
                return Err("writer call changed its proven preimage".into());
            }
            let before = RunRecord::from_bytes(&before).map_err(|e| e.to_string())?;
            if owner_config(&before, &declared)?
                .writer_run
                .as_ref()
                .is_none_or(|bound| {
                    bound.run != current.binding.run
                        || bound.initial_record != current.binding.initial_record || bound.initial_record_format != current.binding.initial_record_format
                })
            {
                return Err("writer call target declared a different writer Run".into());
            }
            let pinned = RunRecord::from_bytes(&snapshot).map_err(|e| e.to_string())?;
            let cp = pinned
                .checkpoint(&runtime.checkpoint)
                .ok_or("writer call checkpoint absent")?;
            let template = config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)?;
            if runtime.ee != pinned.ee
                || runtime.worker != config.worker
                || runtime.interface != declared.door
                || !pinned.active_checkpoints.contains(&cp.id)
                || runtime.epoch != cp.epoch.0
                || !checkpoint_record_matches_template(cp, template, &pinned.id)
                || !metadata_matches_checkpoint(metadata, template)
                || !artifact_refs_match_bindings(
                    &metadata.artifact_binding_refs,
                    &cp.artifact_bindings,
                )
                || !metadata.grant.tools.contains(&declared.tool)
                || metadata.grant.hard_blocked_tools.contains(&declared.tool)
                || !metadata
                    .grant
                    .interfaces
                    .contains(&declared.door.to_string())
                || metadata.verification_failure.is_some()
                || !metadata
                    .checkpoint_admission
                    .as_ref()
                    .is_some_and(|a| a.complete && a.artifact_label == declared.call_label)
            {
                return Err(
                    "writer call differs from its exact admitted checkpoint or grant".into(),
                );
            }
            current.pending = Some((
                call.correlation_id
                    .ok_or("writer call correlation absent")?,
                args,
                movement,
            ));
        } else {
            let result: ToolResultArtifact =
                serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
            let (correlation, args, call) = current
                .pending
                .take()
                .ok_or("writer result has no preceding admitted call")?;
            if result.correlation_id.as_ref() != Some(&correlation)
                || manifest.label != declared.result_label
            {
                return Err("writer result correlation or label differs".into());
            }
            let effect = args.checked_result(&declared, metadata, &snapshot, &result, limits.max_payload_bytes, call.seq.0)?;
            current.current = snapshot;
            if target_untouched(&result, effect) {
                let writer = RunRecord::from_bytes(args.writes[0].content(limits.max_payload_bytes)?.as_bytes()).map_err(|e| e.to_string())?;
                current.voided.insert(seq, target(&writer, &config.provider_interface)?.generation);
                if let Some(refusal) = untouched_refusal(&result) { current.refused.insert(args.writes[1].after_hash(), refusal); }
                continue;
            }
            let attempt = UnresolvedTarget { seq, args, call, result: movement.clone(),
                writer_advanced: effect != OrderedTargetEffect::UncertainWriter };
            if matches!(effect, OrderedTargetEffect::Uncertain | OrderedTargetEffect::UncertainWriter) {
                current.unresolved = Some(attempt);
            } else {
                settle_target_attempt(&mut current, &mut reader, &declared, &target_path, limits.max_payload_bytes, attempt, effect)?;
            }
        }
    }
    if current.unresolved.is_some() {
        let observed = ContentHash::of_bytes(&std::fs::read(&target_path).map_err(|e| e.to_string())?).to_hex();
        resolve_target_attempt(&mut current, &mut reader, &declared, &target_path, &config.provider_interface, limits.max_payload_bytes, &observed, None)?;
    }
    if let Some(start) = last_start {
        reader
            .reader
            .seek(std::io::SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        let BoundedLedgerRow::Complete(row) =
            read_bounded_ledger_row(&mut reader.reader, limits.max_control_frame_bytes)?
        else {
            return Err("writer evidence boundary is incomplete".into());
        };
        current.cursor = Some((
            start,
            reader.reader.stream_position().map_err(|e| e.to_string())?,
            ContentHash::of_bytes(&row),
        ));
    }
    *saved = current.clone();
    Ok(current)
}

fn settle_target_attempt(
    current: &mut WriterHistory,
    reader: &mut impl CheckedRunEvidenceReader,
    declared: &CheckedRunWriter,
    target_path: &Path,
    maximum: u64,
    attempt: UnresolvedTarget,
    effect: OrderedTargetEffect,
) -> Result<(), String> {
    let UnresolvedTarget { seq, args, call, result, .. } = attempt;
    let call_seq = call.seq.0;
    let applied = match effect {
        OrderedTargetEffect::Applied => true,
        OrderedTargetEffect::Refused => false,
        OrderedTargetEffect::Uncertain | OrderedTargetEffect::UncertainWriter => return Err("ordered target attempt was settled while uncertain".into()),
    };
    let hash = args.writes[1].after_hash();
    if applied {
        let transition = CheckedRunTransition {
            writer: declared.clone(),
            before: args.writes[1].before(maximum)?.into_bytes(),
            after: args.writes[1].content(maximum)?.into_bytes(),
            call,
            result,
            placement: false,
        };
        validate_checked_owner_transition(&transition)?;
        generated_artifacts::validate_generated_transition(reader, &transition)?;
        validate_verified_input_transition(reader, &transition)?;
        input_revision::validate_rearm_evidence(reader, &transition, target_path)?;
        current.last_applied_target = Some((seq, transition.after));
        current.applied_calls.insert(hash.clone(), call_seq);
    }
    if current.completed.insert(hash, (seq, applied)).is_some() {
        return Err("writer repeated an already resolved target attempt".into());
    }
    Ok(())
}

fn resolve_target_attempt(
    current: &mut WriterHistory,
    reader: &mut impl CheckedRunEvidenceReader,
    declared: &CheckedRunWriter,
    target_path: &Path,
    provider: &InterfaceId,
    maximum: u64,
    observed: &str,
    observed_writer: Option<&[u8]>,
) -> Result<(), String> {
    let Some(mut attempt) = current.unresolved.take() else { return Ok(()) };
    let (before, after) = attempt.args.target_hashes(maximum)?;
    if !attempt.writer_advanced {
        let actual = match observed_writer {
            Some(bytes) => bytes.to_vec(),
            None => std::fs::read(attempt.args.writes[0].path()).map_err(|e| e.to_string())?,
        };
        let writer_after = attempt.args.writes[0].content(maximum)?.into_bytes();
        if actual == writer_after {
            current.current = actual;
            attempt.writer_advanced = true;
        } else if actual != attempt.args.writes[0].before(maximum)?.as_bytes() || observed != before {
            return Err("indeterminate ordered write has no exact writer and target readback".into());
        }
    }
    if observed == after {
        return settle_target_attempt(current, reader, declared, target_path, maximum, attempt, OrderedTargetEffect::Applied);
    }
    if observed != before {
        return Err("ordered target effect remains uncertain; its target moved to content neither attempt state proves".into());
    }
    let writer = RunRecord::from_bytes(attempt.args.writes[0].content(maximum)?.as_bytes()).map_err(|e| e.to_string())?;
    current.voided.insert(attempt.seq, target(&writer, provider)?.generation);
    Ok(())
}

pub(super) fn success_evidence(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    config: &RunAuthoringConfig,
    authored: &AuthoredRun,
    after_seq: Option<u64>,
) -> Result<Option<u64>, String> {
    Ok(history(ctx, &session.writer_history, config)?
        .completed
        .get(&authored.after_hash)
        .filter(|(seq, ok)| *ok && after_seq.is_none_or(|after| *seq > after))
        .map(|(seq, _)| *seq))
}

pub(super) fn last_applied_target(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    config: &RunAuthoringConfig,
) -> Result<Option<(u64, Vec<u8>)>, String> {
    let history = history(ctx, &session.writer_history, config)?;
    Ok(if history.pending.is_none() { history.last_applied_target } else { None })
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OrderedCall {
    tool: String,
    arguments: OrderedArguments,
    correlation_id: String,
}

struct Attempt {
    manifest: InterfaceArtifactFile,
    args: OrderedArguments,
    authored: AuthoredRun,
    generation: u64,
    source_movement_seq: u64,
}

impl ProviderOutputHandoff {
    fn writer_attempts(&self, authoring_generation: u64) -> Result<Vec<Attempt>, String> {
        let paths = self.session.writer_call_generations.lock()
            .map_err(|_| "provider writer call index is poisoned".to_string())?
            .candidates(&self.hand_in_root, authoring_generation)?;
        let declared = writer(&self.config)?;
        let prefix = format!("provider-writer-run:{}:", self.config.run);
        let limits = self.ctx.required_host_io_limits()?;
        let mut attempts = Vec::new();
        for path in paths {
            let manifest = InterfaceArtifactFile::new(path.clone()).map_err(|e| e.to_string())?;
            let snapshot = manifest
                .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                .map_err(|e| e.to_string())?;
            let value = (snapshot.manifest.label == self.config.file_write_label)
                .then(|| serde_json::from_slice::<Value>(&snapshot.payload_bytes).ok())
                .flatten()
                .filter(|value| {
                    value["correlation_id"]
                        .as_str()
                        .is_some_and(|s| s.starts_with(&prefix))
                });
            let declared_generation = value.as_ref().map(|value| {
                value["correlation_id"]
                    .as_str()
                    .and_then(|s| s.strip_prefix(&prefix))
                    .and_then(|s| s.split(':').next())
                    .and_then(|s| s.parse::<u64>().ok())
            });
            if !matches!(declared_generation, Some(None)) {
                self.session
                    .writer_call_generations
                    .lock()
                    .map_err(|_| "provider writer call index is poisoned".to_string())?
                    .record(path, declared_generation.flatten());
            }
            let Some(value) = value else {
                continue;
            };
            if declared_generation.flatten().is_some_and(|generation| generation != authoring_generation) {
                continue;
            }
            let call: OrderedCall = serde_json::from_value(value).map_err(|e| e.to_string())?;
            if call.tool != self.config.file_write_tool {
                return Err("private ordered call changed its tool".into());
            }
            let first = call
                .arguments
                .writes
                .first()
                .ok_or("private ordered call has no writer")?;
            let (before, args) = call.arguments.checked_call(
                &declared,
                first.before(limits.max_payload_bytes)?.as_bytes(),
                &self.run_path,
                limits.max_payload_bytes,
            )?;
            let prospective = call.arguments.writes[1].content(limits.max_payload_bytes)?;
            let record =
                RunRecord::from_bytes(prospective.as_bytes()).map_err(|e| e.to_string())?;
            let pending = persisted_pending_handoff(&record, &self.config.provider_interface)?
                .ok_or("private ordered call lost pending outputs")?;
            let generation = optional_u64(
                record
                    .interface_state
                    .get(&self.config.provider_interface)
                    .ok_or("private call provider state absent")?,
                "authoring_generation",
            )?;
            let writer_record =
                RunRecord::from_bytes(first.content(limits.max_payload_bytes)?.as_bytes()).map_err(|e| e.to_string())?;
            let writer_generation =
                target(&writer_record, &self.config.provider_interface)?.generation;
            if call.correlation_id != format!("{prefix}{generation}:{writer_generation}") {
                return Err(
                    "private ordered call correlation differs from its exact generations".into(),
                );
            }
            if generation != authoring_generation {
                continue;
            }
            let original = RunRecord::from_bytes(&before).map_err(|error| error.to_string())?;
            let original_config = attempt_config(&original, &self.config)?;
            if !result_inputs::saved_definition_transition(&original, &record, &original_config)? {
                source_settlement::validate_transition(&original, &record, &original_config)?;
                source_continuation::validate_transition(&original, &record, &original_config)?;
                source_withdrawal::validate_saved_attempt(&self.ctx, &before, prospective.as_bytes(), &original_config)?;
                rejected_final::validate_saved_attempt(&self.ctx, &before, prospective.as_bytes(), &original_config)?;
            }
            let candidate_config = result_inputs::saved_attempt_config(&record, &original_config)?;
            validate_bound_run_authoring_topology(&record, &candidate_config, None)?;
            result_inputs::validate_saved(&self.ctx, &original, &record, &original_config)?;
            let source_movement_seq = pending.source_movement_seq;
            let result_template = original_config
                .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
            let result = writer_record
                .checkpoints
                .iter()
                .find(|cp| {
                    checkpoint_record_matches_template(cp, result_template, &writer_record.id)
                })
                .ok_or("writer result checkpoint absent")?;
            let authored = AuthoredRun {
                json_format: args.json_format,
                content: prospective,
                patch: args.patch,
                before_hash: ContentHash::of_bytes(&before).to_hex(),
                after_hash: args.expected_after_hash,
                correlation_id: format!("provider-run-authoring:{}:{generation}", self.config.run),
                authoring_result_checkpoint: result.id.clone(),
                authoring_result_interface: declared.door.clone(),
                pending_handoff: pending,
            };
            attempts.push(Attempt {
                manifest,
                args: call.arguments,
                authored,
                generation: writer_generation,
                source_movement_seq,
            });
        }
        attempts.sort_by_key(|a| a.generation);
        if attempts
            .windows(2)
            .any(|pair| pair[0].generation == pair[1].generation)
        {
            return Err("private writer has competing attempts for one generation".into());
        }
        Ok(attempts)
    }

    pub(super) fn recover_writer_attempt(
        &self,
        record: &RunRecord,
        record_bytes: &[u8],
        recover_predecessors: bool,
    ) -> Result<AuthoringAttemptRecovery, String> {
        let generation = optional_u64(
            record
                .interface_state
                .get(&self.config.provider_interface)
                .ok_or("provider recovery state absent")?,
            "authoring_generation",
        )?;
        let expected = generation
            .checked_add(1)
            .ok_or("provider recovery generation overflow")?;
        let mut generations = if recover_predecessors {
            revision_attempt_generations(&self.ctx, &self.session, record, record_bytes, &self.config)?
                .into_keys().collect::<BTreeSet<_>>()
        } else { BTreeSet::new() };
        generations.insert(expected);
        let mut attempts = Vec::new();
        for generation in &generations { attempts.extend(self.writer_attempts(*generation)?); }
        attempts.sort_by_key(|attempt| attempt.generation);
        if attempts.windows(2).any(|pair| pair[0].generation == pair[1].generation) {
            return Err("writer recovery has competing attempts for one writer generation".into());
        }
        let mut selected = None;
        for attempt in attempts {
            let generation = correlation_generation(&record.id, &attempt.authored.correlation_id)?;
            if !generations.contains(&generation) { return Err("writer recovery selected an unproven generation".into()); }
            let before_bytes = attempt.args.writes[1].before(self.ctx.required_host_io_limits()?.max_payload_bytes)?;
            let before =
                RunRecord::from_bytes(before_bytes.as_bytes())
                    .map_err(|e| e.to_string())?;
            if generation != expected {
                validate_revision_attempt_predecessor(&self.ctx, before_bytes.as_bytes(), record_bytes,
                    attempt.source_movement_seq, &attempt_config(&before, &self.config)?)?;
            } else if before.interface_state.get(&self.config.provider_interface)
                != record.interface_state.get(&self.config.provider_interface)
            {
                let protected = output_connections::protected_record(before_bytes.as_bytes(), record_bytes, &self.config)?
                    .ok_or("private writer recovery changed provider-owned target state")?;
                let protected = RunRecord::from_bytes(&protected).map_err(|error| error.to_string())?;
                if before.interface_state.get(&self.config.provider_interface)
                    != protected.interface_state.get(&self.config.provider_interface) {
                    return Err("private writer recovery changed protected provider-owned target state".into());
                }
            }
            if let Some(previous) = &selected {
                let previous: &Attempt = previous;
                let progress = history(&self.ctx, &self.session.writer_history, &self.config)?;
                if !progress.voided.values().any(|generation| *generation == previous.generation)
                    && progress
                        .completed
                        .get(&previous.authored.after_hash)
                        .is_none_or(|(_, ok)| *ok)
                {
                    return Err("writer recovery has an unexplained overlapping attempt".into());
                }
                if previous.authored.correlation_id != attempt.authored.correlation_id {
                    let original = previous.args.writes[1].before(self.ctx.required_host_io_limits()?.max_payload_bytes)?;
                    let original_config = attempt_config(&RunRecord::from_bytes(original.as_bytes())
                        .map_err(|error| error.to_string())?, &self.config)?;
                    // The detached writer's fresh return is already proved by
                    // OrderedArguments::checked_call; the target intent is exact.
                    let mut target = attempt.authored.clone();
                    target.authoring_result_checkpoint = previous.authored.authoring_result_checkpoint.clone();
                    authoring_retry::validate_rebased_authored_run(original.as_bytes(), before_bytes.as_bytes(),
                        &previous.authored, &target, &original_config)?;
                }
            }
            selected = Some(attempt);
        }
        match selected {
            None => Ok(AuthoringAttemptRecovery::None),
            Some(attempt)
                if attempt.source_movement_seq
                    != self.source_movement_seq =>
            {
                Ok(AuthoringAttemptRecovery::OtherSource(
                    attempt.source_movement_seq,
                ))
            }
            Some(attempt) => Ok(AuthoringAttemptRecovery::InFlight(
                ValidatedAuthoringAttempt {
                    manifest_path: attempt.manifest.manifest_path().to_path_buf(),
                    provider_source_movement_seq: self.source_movement_seq,
                    authored: attempt.authored,
                },
            )),
        }
    }

    pub(super) async fn install_with_writer_run(
        &self,
        state: &mut HandoffState,
        authored: &AuthoredRun,
    ) -> Result<AuthoredRunInstallOutcome, String> {
        let maximum = self.ctx.required_host_io_limits()?.max_payload_bytes;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(self.config.result_timeout_ms))
            .ok_or("writer deadline overflow")?;
        let mut planned = authored.clone();
        let mut known: Option<(String, InterfaceArtifactFile, OrderedArguments, u64)> = None;
        loop {
            if Instant::now() >= deadline {
                return Err("provider ordered file write awaits its admitted result".into());
            }
            let progress = history(&self.ctx, &self.session.writer_history, &self.config)?;
            if let Some(refusal) = progress.refused.get(&planned.after_hash) {
                return Err(format!("provider ordered file write was refused before any effect: {refusal}"));
            }
            if known.as_ref().is_none_or(|(after_hash, _, _, generation)| {
                *after_hash != planned.after_hash || progress.voided.values().any(|voided| voided == generation)
            }) {
                known = self
                    .writer_attempts(correlation_generation(&self.config.run, &planned.correlation_id)?)?
                    .into_iter()
                    .find(|a| a.authored.after_hash == planned.after_hash
                        && !progress.voided.values().any(|voided| *voided == a.generation))
                    .map(|a| (planned.after_hash.clone(), a.manifest, a.args, a.generation));
            }
            if let Some((_, true)) = progress.completed.get(&planned.after_hash) {
                state.run_record_bytes = planned.content.as_bytes().to_vec();
                state.run_record_evidence_seq =
                    progress.applied_calls.get(&planned.after_hash).copied();
                return Ok(AuthoredRunInstallOutcome::Installed);
            }
            if let Some((_, false)) = progress.completed.get(&planned.after_hash) {
                let (_, _, args, _) = known.as_ref().ok_or("refused ordered write lost its private call")?;
                let before_content = args.writes[1].before(maximum)?;
                let before = before_content.as_bytes();
                let remaining = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .min(u64::MAX as u128) as u64;
                let (current, chain) = await_checked_evidence(&self.ctx, remaining, || {
                    checked_live_run_advance(
                        &self.ctx,
                        &self.run_path,
                        before,
                        self.source_movement_seq,
                        &self.config,
                    )
                })
                .await?;
                let routing_only = output_connections::protected_record(before, &current, &self.config)?
                    .is_some_and(|protected| ContentHash::of_bytes(&protected) == ContentHash::of_bytes(before));
                if (chain.is_empty() && !routing_only) || chain
                        .iter()
                        .any(|t| t.writer.owner == self.config.provider_interface && !input_revision::checked_revision(t))
                {
                    return Err(
                        "refused target write has no exact independent checked advance".into(),
                    );
                }
                planned =
                    authoring_retry::rebase_authored_run(before, &current, &planned, &self.config, Some(JsonFormat::Compact))?;
                state.run_record_bytes = current;
                if let Some(last) = chain.last() {
                    state.run_record_evidence_seq = Some(last.call.seq.0);
                }
                continue;
            }
            let manifest = if let Some((_, manifest, _, _)) = &known {
                manifest.clone()
            } else {
                if progress.pending.is_some() {
                    return Err("writer Run already has a different admitted attempt".into());
                }
                let binding = self
                    .config
                    .writer_run
                    .as_ref()
                    .ok_or("writer binding absent")?;
                let path = self
                    .run_path
                    .parent()
                    .ok_or("run parent absent")?
                    .join(format!("{}.json", binding.run));
                if std::fs::read(&path).map_err(|e| e.to_string())? != progress.current {
                    return Err("writer Run changed without admitted ordered-write evidence".into());
                }
                if ContentHash::of_bytes(&state.run_record_bytes).to_hex() != planned.before_hash {
                    return Err("writer lost its planned target preimage".into());
                }
                let minimum = progress.voided.values().max().copied().unwrap_or(0).checked_add(1).ok_or("writer attempt generation overflow")?;
                let next = next_writer(&progress.current, &self.config, JsonFormat::Compact, minimum)?;
                let next_record = RunRecord::from_bytes(&next).map_err(|e| e.to_string())?;
                let generation = target(&next_record, &self.config.provider_interface)?.generation;
                let call = OrderedCall {
                    tool: self.config.file_write_tool.clone(),
                    correlation_id: format!(
                        "provider-writer-run:{}:{}:{generation}",
                        self.config.run,
                        correlation_generation(&self.config.run, &planned.correlation_id)?
                    ),
                    arguments: OrderedArguments {
                        ordered: true,
                        writes: vec![
                            OrderedEntry::prepare(path, &progress.current, &next, Some(JsonFormat::Compact))?,
                            OrderedEntry::prepare(self.run_path.clone(), &state.run_record_bytes, planned.content.as_bytes(), planned.json_format)?,
                        ],
                    },
                };
                call.arguments.checked_call(&writer(&self.config)?, &progress.current, &self.run_path, maximum)?;
                let manifest = InterfaceArtifactFile::write(
                    &self.hand_in_root,
                    &self.config.file_write_label,
                    &serde_json::to_vec(&call).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                known = Some((planned.after_hash.clone(), manifest.clone(), call.arguments, generation));
                manifest
            };
            if progress.pending.is_none() {
                let verdict = self
                    .send_authoring_manifest(
                        &std::fs::read(manifest.manifest_path()).map_err(|e| e.to_string())?,
                    )
                    .await?;
                if let DeliveryVerdict::Refused { reason, detail } = verdict {
                    if reason != octopus_interfaces::RefusedReason::CheckpointSpent {
                        return Err(format!(
                            "writer hand-in refused {}: {detail}",
                            reason.as_str()
                        ));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
