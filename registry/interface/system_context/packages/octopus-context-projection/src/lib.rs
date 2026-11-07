use std::collections::BTreeSet;
use std::sync::Arc;

use octopus_interface_transport::{InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::InterfaceArtifactFile;
use octopus_ledger::ArtifactRef;
use octopus_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffOutcome, ProviderHandoffSession,
    ProviderSourceRecovery, RejectedFinalOutput, RunAuthoringResultIdentity, SessionContextArtifact,
    snapshot_projection::{
        SnapshotInputOrigin, SnapshotInputRoute, SnapshotSourceScope, SnapshotWindowBounds,
        resolve_session_snapshot_window, selected_source_input_binding, verify_selected_source_input, verify_source_bound_entry,
        verify_source_context_entry, verify_terminal_source,
    },
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DRIVER: &str = "bound_context_projection";
pub const OUTPUT_LABEL: &str = "memory.experience.input";
pub const OUTCOME_LABEL: &str = "memory.capture.outcome";
pub const OUTCOME_ERROR_LABEL: &str = "memory.capture.error";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeProjectionConfig {
    pub worker_interface: String,
    pub provider_interface: String,
    pub relay_interface: String,
    pub experience_interface: String,
    pub input_routes: Vec<SnapshotInputRoute>,
    pub tool_call_label: String,
    pub tool_result_labels: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionBounds {
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
    pub max_snapshot_entries: usize,
    pub max_projected_entries: usize,
    pub max_field_bytes: usize,
    pub ancestry: SnapshotWindowBounds,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionRule {
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pointers: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub whole_payload: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SnapshotInputOrigin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextProjectionConfig {
    pub schema_version: u32,
    pub observer_run: String,
    pub projection_run: String,
    pub source_run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ee: Option<String>,
    pub source_worker_interface: String,
    pub source_provider_interface: String,
    pub source_client_interface: String,
    pub source_relay_interface: String,
    pub terminal_label: String,
    pub snapshot_labels: BTreeSet<String>,
    pub input_rules: Vec<ProjectionRule>,
    pub input_routes: Vec<SnapshotInputRoute>,
    pub evidence_rules: Vec<ProjectionRule>,
    pub terminal_pointers: Vec<String>,
    pub collection_source: String,
    pub publication_tool: String,
    pub task: String,
    pub bounds: ProjectionBounds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OutcomeProjectionConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    provider_interface: String,
    #[serde(default)]
    parent: Option<ArtifactRef>,
    artifacts: Vec<SnapshotEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotEntry {
    label: String,
    payload: Value,
    source_artifact: ArtifactRef,
    source_movement_seq: Option<u64>,
}

fn exact(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

impl ContextProjectionConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 || self.terminal_label == OUTPUT_LABEL {
            return Err("projection requires schema 1 and distinct terminal/input labels".into());
        }
        let identities = [
            &self.observer_run,
            &self.projection_run,
            &self.source_run,
            &self.source_worker_interface,
            &self.source_provider_interface,
            &self.source_client_interface,
            &self.source_relay_interface,
            &self.terminal_label,
            &self.collection_source,
            &self.publication_tool,
        ];
        if identities.into_iter().any(|id| !exact(id))
            || self.source_ee.as_deref().is_some_and(|ee| !exact(ee))
            || self.task.trim().is_empty()
            || self.snapshot_labels.is_empty()
            || !self.snapshot_labels.contains(&self.terminal_label)
            || self.source_run == self.observer_run
            || self.source_run == self.projection_run
            || self.observer_run == self.projection_run
            || self.source_provider_interface == self.source_relay_interface
            || self.source_client_interface == self.source_relay_interface
        {
            return Err("projection requires distinct exact source and observer identities".into());
        }
        let b = &self.bounds;
        if b.max_input_bytes == 0
            || b.max_output_bytes == 0
            || b.max_input_bytes > 268_435_456
            || b.max_output_bytes > 1_048_576
            || b.max_snapshot_entries == 0
            || b.max_snapshot_entries > 65_536
            || b.max_projected_entries == 0
            || b.max_projected_entries > 1024
            || b.max_field_bytes == 0
            || b.max_field_bytes as u64 > b.max_output_bytes
        {
            return Err(
                "projection bounds must be finite, positive and internally consistent".into(),
            );
        }
        let mut labels = BTreeSet::new();
        if self.input_rules.is_empty() || self.input_routes.is_empty() {
            return Err("projection requires explicit source input rules and routes".into());
        }
        let input_rules = self.input_rules.iter().map(|rule| (rule, true));
        for (rule, input) in input_rules.chain(self.evidence_rules.iter().map(|rule| (rule, false))) {
            if rule.labels.is_empty()
                || rule.pointers.is_empty() != rule.whole_payload
                || rule.whole_payload && (!input || rule.origin.is_some())
                || rule
                    .labels
                    .iter()
                    .any(|label| !exact(label) || !labels.insert(label))
            {
                return Err(
                    "projection rules require disjoint exact labels and explicit fields; only an input without origin may select its whole payload".into(),
                );
            }
            if rule.whole_payload {
                continue;
            }
            validate_pointers(&rule.pointers)?;
            if let Some(origin) = &rule.origin {
                validate_pointers(&[
                    origin.artifact_pointer.clone(),
                    origin.sequence_pointer.clone(),
                ])?;
                validate_pointers(&origin.pointers)?;
            }
        }
        validate_pointers(&self.terminal_pointers)?;
        if let Some(outcome) = &self.outcome {
            if [
                &outcome.worker_interface,
                &outcome.provider_interface,
                &outcome.relay_interface,
                &outcome.experience_interface,
                &outcome.tool_call_label,
            ]
            .iter()
            .any(|id| !exact(id))
                || outcome.input_routes.is_empty()
                || outcome.relay_interface == self.source_relay_interface
            {
                return Err(
                    "outcome projection requires an exact independent observer boundary".into(),
                );
            }
        }
        Ok(())
    }
}

fn validate_pointers(pointers: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    if pointers.is_empty()
        || pointers.iter().any(|pointer| {
            !pointer.starts_with('/')
                || pointer.contains('*')
                || !exact(pointer)
                || pointer.split('/').any(|part| part == "session_context")
                || !seen.insert(pointer)
                || pointer
                    .as_bytes()
                    .windows(2)
                    .any(|pair| pair[0] == b'~' && !matches!(pair[1], b'0' | b'1'))
                || pointer.ends_with('~')
        })
    {
        return Err(
            "projection pointers must be explicit unique JSON pointers outside session_context"
                .into(),
        );
    }
    Ok(())
}

fn input_pointers(rule: &ProjectionRule, payload: &Value) -> Result<Vec<String>, String> {
    if !rule.whole_payload {
        return Ok(rule.pointers.clone());
    }
    let pointers = payload
        .as_object()
        .ok_or("a whole-payload input must be a JSON object")?
        .keys()
        .filter(|key| key.as_str() != "session_context")
        .map(|key| format!("/{}", key.replace('~', "~0").replace('/', "~1")))
        .collect::<Vec<_>>();
    if pointers.is_empty() {
        return Err("a whole-payload input carries no members".into());
    }
    Ok(pointers)
}

fn fields(
    payload: &Value,
    pointers: &[String],
    limit: usize,
    required: bool,
) -> Result<(Value, Vec<Value>), String> {
    let mut projected = serde_json::Map::new();
    let mut trimmed = Vec::new();
    for pointer in pointers {
        match payload.pointer(pointer) {
            Some(value) => {
                let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?.len();
                if bytes > limit {
                    projected.insert(pointer.clone(), truncated_middle(value, limit)?);
                    trimmed.push(json!({"pointer":pointer,"bytes":bytes}));
                } else {
                    projected.insert(pointer.clone(), value.clone());
                }
            }
            None if required => {
                return Err(format!("required projection field '{pointer}' is absent"));
            }
            None => {}
        }
    }
    Ok((Value::Object(projected), trimmed))
}

fn truncated_middle(value: &Value, limit: usize) -> Result<Value, String> {
    let text = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).map_err(|e| e.to_string())?,
    };
    let mut budget = limit;
    loop {
        let truncated = Value::String(octopus_util_string::truncate_middle_chars(&text, budget));
        if budget == 0 || serde_json::to_vec(&truncated).map_err(|e| e.to_string())?.len() <= limit {
            return Ok(truncated);
        }
        budget = budget * 3 / 4;
    }
}

pub fn project_context(
    config: &ContextProjectionConfig,
    source_ee: &str,
    label: &str,
    carrier: &ArtifactRef,
    source_movement_seq: u64,
    bytes: &[u8],
) -> Result<Value, String> {
    config.validate()?;
    if label != config.terminal_label || bytes.len() as u64 > config.bounds.max_input_bytes {
        return Err(
            "projection input does not match its declared terminal carrier or byte bound".into(),
        );
    }
    let payload: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let snapshot: Snapshot = serde_json::from_value(
        payload
            .get("session_context")
            .cloned()
            .ok_or("terminal carrier has no declared context snapshot")?,
    )
    .map_err(|e| e.to_string())?;
    if snapshot.schema_version != 3
        || snapshot.provider_interface != config.source_provider_interface
        || snapshot.artifacts.len() > config.bounds.max_snapshot_entries
    {
        return Err(
            "terminal context does not match the exact declared source provider and bounds".into(),
        );
    }
    project_window(
        config,
        source_ee,
        carrier,
        source_movement_seq,
        &payload,
        snapshot.parent,
        &snapshot
            .artifacts
            .into_iter()
            .map(|entry| SessionContextArtifact {
                label: entry.label,
                payload: entry.payload,
                source_artifact: entry.source_artifact,
                source_movement_seq: entry.source_movement_seq,
            })
            .collect::<Vec<_>>(),
    )
}

fn project_window(
    config: &ContextProjectionConfig,
    source_ee: &str,
    carrier: &ArtifactRef,
    source_movement_seq: u64,
    payload: &Value,
    parent: Option<ArtifactRef>,
    entries: &[SessionContextArtifact],
) -> Result<Value, String> {
    if entries.len() > config.bounds.max_snapshot_entries {
        return Err("resolved source context exceeds its declared snapshot entry bound".into());
    }
    let input_index = entries
        .iter()
        .rposition(|entry| {
            config
                .input_rules
                .iter()
                .any(|rule| rule.labels.contains(&entry.label))
        })
        .ok_or(
            "terminal snapshot has no selected source input; parent history is not authorized",
        )?;
    let input = &entries[input_index];
    let sequence = input
        .source_movement_seq
        .ok_or("source input has no movement evidence")?;
    let input_rule = config
        .input_rules
        .iter()
        .find(|rule| rule.labels.contains(&input.label))
        .ok_or("selected input has no declared fields")?;
    let selected_pointers = input_pointers(input_rule, &input.payload)?;
    let (input_fields, input_trimmed) = fields(
        &input.payload,
        &selected_pointers,
        config.bounds.max_field_bytes,
        true,
    )?;
    let mut evidence = Vec::new();
    let mut omitted = Vec::new();
    let mut refs = BTreeSet::new();
    for entry in &entries[input_index..] {
        let seq = entry
            .source_movement_seq
            .ok_or("selected source context has no movement evidence")?;
        if seq < sequence
            || !refs.insert((
                entry.source_artifact.id.clone(),
                entry.source_artifact.content,
            ))
        {
            return Err(
                "selected source context has evidence before its selected input or duplicate evidence".into(),
            );
        }
        if let Some(rule) = config
            .evidence_rules
            .iter()
            .find(|rule| rule.labels.contains(&entry.label))
        {
            let (projected, trimmed) = fields(&entry.payload, &rule.pointers, config.bounds.max_field_bytes, false)?;
            evidence.push(json!({"label":entry.label,"source_artifact":entry.source_artifact,
                "source_movement_seq":seq,"fields":projected,"trimmed_fields":trimmed,
                "absent_requested_pointers":rule.pointers.iter().filter(|pointer|entry.payload.pointer(pointer).is_none()).collect::<Vec<_>>()}));
        } else {
            omitted.push(json!({"label":entry.label,"source_movement_seq":seq,"source_artifact":entry.source_artifact}));
        }
    }
    let (terminal_fields, terminal_trimmed) = fields(&payload,&config.terminal_pointers,config.bounds.max_field_bytes,true)?;
    let trimmed_entries = evidence.len().saturating_sub(config.bounds.max_projected_entries);
    let mut output = json!({
            "schema_version":1,"label":OUTPUT_LABEL,
            "source":{"run":config.source_run,"ee":source_ee,"worker_interface":config.source_worker_interface,
                "provider_interface":config.source_provider_interface,"client_interface":config.source_client_interface,
                "terminal_artifact":carrier,"projection_source_movement_seq":source_movement_seq,
                "snapshot_parent":parent},
            "selected_input":{"label":input.label,"source_artifact":input.source_artifact,"source_movement_seq":sequence,"fields":input_fields},
            "terminal_fields":terminal_fields,
            "evidence":&evidence[trimmed_entries..],"collection_source":config.collection_source,"publication_tool":config.publication_tool,
            "omission":{"before_selected_input":true,"unselected_fields":true,"unselected_labels":true,
                "selected_fields_trimmed":false,"selected_evidence_trimmed":trimmed_entries > 0,
                "trimmed_evidence_entries":trimmed_entries,"unselected_entries":omitted,
                "input_pointers":selected_pointers,"terminal_pointers":config.terminal_pointers,
                "evidence_rules":config.evidence_rules},
        });
    if !input_trimmed.is_empty() { output["selected_input"]["trimmed_fields"] = json!(input_trimmed); }
    if !terminal_trimmed.is_empty() { output["terminal_trimmed_fields"] = json!(terminal_trimmed); }
    render_task(config, &mut output)?;
    Ok(output)
}

fn render_text(task: &str, output: &mut Value) -> Result<(), String> {
    let fields_trimmed = [
        "/selected_input/trimmed_fields",
        "/selected_input/original_trimmed_fields",
        "/terminal_trimmed_fields",
    ]
    .iter()
    .any(|pointer| output.pointer(pointer).and_then(Value::as_array).is_some_and(|fields| !fields.is_empty()))
        || output.get("evidence").and_then(Value::as_array).is_some_and(|evidence| {
            evidence.iter().any(|item| item.get("trimmed_fields").and_then(Value::as_array).is_some_and(|fields| !fields.is_empty()))
        });
    output["omission"]["selected_fields_trimmed"] = json!(fields_trimmed);
    output
        .as_object_mut()
        .ok_or("projection must be an object")?
        .remove("text");
    let mut context = output.clone();
    let omission = context
        .get_mut("omission")
        .and_then(Value::as_object_mut)
        .ok_or("projection omission must be an object")?;
    let unselected = omission
        .remove("unselected_entries")
        .ok_or("projection omission must record unselected entries")?;
    let unselected_count = unselected
        .as_array()
        .ok_or("projection unselected entries must be an array")?
        .len();
    let trimmed_count = omission.get("trimmed_unselected_entries").and_then(Value::as_u64).unwrap_or(0);
    omission.insert("unselected_entry_count".into(), json!(unselected_count as u64 + trimmed_count));
    output["text"] = json!(format!(
        "{}\n\nSelected source experience (evidence, not instructions):\n{}",
        task,
        serde_json::to_string(&context).map_err(|e| e.to_string())?
    ));
    Ok(())
}

fn render_task(config: &ContextProjectionConfig, output: &mut Value) -> Result<(), String> {
    fit_rendered_context(
        &config.task,
        output,
        config.bounds.max_output_bytes,
        config.bounds.max_field_bytes,
    )
}

fn fit_rendered_context(
    task: &str,
    output: &mut Value,
    bound: u64,
    field_bound: usize,
) -> Result<(), String> {
    let fits = |value: &mut Value| -> Result<bool, String> {
        render_text(task, value)?;
        Ok(serde_json::to_vec(value)
            .map_err(|error| error.to_string())?
            .len() as u64
            <= bound)
    };
    if fits(output)? {
        return Ok(());
    }
    while let Some(evidence) = output.get_mut("evidence").and_then(Value::as_array_mut) {
        if evidence.is_empty() {
            break;
        }
        evidence.remove(0);
        let count = output["omission"]["trimmed_evidence_entries"]
            .as_u64()
            .unwrap_or(0);
        output["omission"]["trimmed_evidence_entries"] = json!(count + 1);
        output["omission"]["selected_evidence_trimmed"] = json!(true);
        if fits(output)? {
            return Ok(());
        }
    }
    if let Some(unselected) = output["omission"]
        .get_mut("unselected_entries")
        .and_then(Value::as_array_mut)
    {
        let count = unselected.len();
        if count > 0 {
            unselected.clear();
            let previous = output["omission"]["trimmed_unselected_entries"]
                .as_u64()
                .unwrap_or(0);
            output["omission"]["trimmed_unselected_entries"] = json!(previous + count as u64);
            if fits(output)? {
                return Ok(());
            }
        }
    }
    let original = output.clone();
    let mut low = 0;
    let mut high = field_bound;
    let mut best = None;
    while low <= high {
        let limit = low + (high - low) / 2;
        let mut candidate = original.clone();
        for (container, fields_key, trimmed_key) in [
            ("selected_input", "fields", "trimmed_fields"),
            (
                "selected_input",
                "original_fields",
                "original_trimmed_fields",
            ),
            ("", "terminal_fields", "terminal_trimmed_fields"),
        ] {
            let target = if container.is_empty() {
                &mut candidate
            } else {
                &mut candidate[container]
            };
            let Some(fields) = target.get(fields_key).and_then(Value::as_object).cloned() else {
                continue;
            };
            let mut trimmed = target
                .get(trimmed_key)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for (pointer, value) in fields {
                let bytes = serde_json::to_vec(&value)
                    .map_err(|error| error.to_string())?
                    .len();
                if bytes <= limit {
                    continue;
                }
                target[fields_key][&pointer] = truncated_middle(&value, limit)?;
                if !trimmed
                    .iter()
                    .any(|entry| entry["pointer"].as_str() == Some(&pointer))
                {
                    trimmed.push(json!({"pointer":pointer,"bytes":bytes}));
                }
            }
            if !trimmed.is_empty() {
                target[trimmed_key] = json!(trimmed);
            }
        }
        if fits(&mut candidate)? {
            best = Some(candidate);
            low = limit + 1;
        } else if limit == 0 {
            break;
        } else {
            high = limit - 1;
        }
    }
    *output = best.ok_or("projection provenance exceeds its declared output bound")?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/rendering.rs"]
mod rendering_tests;
#[cfg(test)]
#[path = "../tests/unit/outcome.rs"]
mod outcome_tests;

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(run(ctx)))
}

async fn run(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    if descriptor.interface_driver_ref != DRIVER {
        return Err("context projection resolves an undeclared driver".into());
    }
    let config: ContextProjectionConfig = serde_json::from_value(
        descriptor
            .interface_driver_config
            .as_ref()
            .and_then(|config| config.private_config.get("context_projection"))
            .cloned()
            .ok_or("context projection declaration is absent")?,
    )
    .map_err(|e| e.to_string())?;
    config.validate()?;
    let mut reader = LedgerTailReader::open_named(&ctx, "context-projection")?;
    let session = ProviderHandoffSession::new(&ctx)?;
    let authoring_result = RunAuthoringResultIdentity::from_descriptor(&descriptor)?;
    ctx.signal_ready().await?;
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        let batch = reader.read_new()?;
        if batch.is_empty() {
            reader.wait_for_change(&ctx.cancel).await?;
            continue;
        }
        for own in batch {
            let envelope = reader.envelope(&own)?;
            let manifest = envelope.artifact_file.read_manifest().map_err(|e| e.to_string())?;
            if !authoring_result
                .consume_if_matches(&ctx, &session, &own.movement, &envelope, &manifest.label)
                .await?
            {
                settle_admitted_input(&ctx, &session, &config, &envelope, &own.movement).await?;
            }
            reader.commit_through(&own)?;
        }
    }
}

fn projection_bytes(output: &Value, bound: u64) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(output).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > bound {
        return Err("immutable projection exceeds its declared output bound".into());
    }
    Ok(bytes)
}

fn outcome_publication_evidence(
    publication_tool: &str,
    outcome: &OutcomeProjectionConfig,
    max_entries: usize,
    entries: &[SessionContextArtifact],
) -> Result<(Vec<Value>, Vec<Value>), String> {
    let result_label = format!("{publication_tool}.result");
    let mut evidence = Vec::new();
    let mut omitted = Vec::new();
    for entry in entries {
        let publication = entry.payload.get("tool").and_then(Value::as_str) == Some(publication_tool)
            || entry.label == result_label;
        if publication {
            if entry.label != outcome.tool_call_label && !outcome.tool_result_labels.contains(&entry.label) {
                return Err("publication evidence is outside its declared outcome labels".into());
            }
            if evidence.len() >= max_entries {
                return Err("outcome publication evidence exceeds its declared entry bound".into());
            }
            evidence.push(json!(entry));
        } else {
            omitted.push(json!({"label":entry.label,"source_artifact":entry.source_artifact,"source_movement_seq":entry.source_movement_seq}));
        }
    }
    Ok((evidence, omitted))
}

fn project_outcome(
    ctx: &InterfaceLiftContext,
    config: &ContextProjectionConfig,
    outcome: &OutcomeProjectionConfig,
    artifact: &ArtifactRef,
    payload_bytes: &[u8],
    movement: &octopus_ledger::Movement,
    rejected: Option<&RejectedFinalOutput>,
) -> Result<(&'static str, Value), String> {
    let limits = ctx.required_host_io_limits()?;
    let scope = SnapshotSourceScope {
        run: config.observer_run.clone(),
        worker: outcome.worker_interface.clone(),
        provider: outcome.provider_interface.clone(),
        client: outcome.experience_interface.clone(),
        relay: outcome.relay_interface.clone(),
    };
    let (source_seq, _) = verify_terminal_source(
        ctx,
        movement,
        artifact,
        &config.terminal_label,
        &scope,
    )?;
    let entries = resolve_session_snapshot_window(
        &ctx.blobs_dir,
        limits,
        artifact,
        payload_bytes,
        source_seq,
        &config.snapshot_labels,
        &octopus_descriptors::InterfaceId::new(&outcome.provider_interface),
        &BTreeSet::from([OUTPUT_LABEL.to_owned()]),
        &config.bounds.ancestry,
    )?;
    if entries.len() > config.bounds.max_snapshot_entries {
        return Err(
            "resolved outcome context exceeds its declared snapshot entry bound".into(),
        );
    }
    let selected = entries
        .iter()
        .rfind(|entry| entry.label == OUTPUT_LABEL)
        .ok_or("outcome has no current admitted experience input")?;
    verify_selected_source_input(ctx, selected, &scope, &outcome.input_routes, None)?;
    let source = selected
        .payload
        .get("source")
        .ok_or("outcome experience input has no source scope")?;
    let observed_ee = source.get("ee").and_then(Value::as_str).filter(|ee| exact(ee))
        .ok_or("outcome experience input has no observed EE")?;
    if source.get("run").and_then(Value::as_str) != Some(config.source_run.as_str()) {
        return Err("outcome experience input differs from its exact source run".into());
    }
    for entry in &entries {
        if entry.label == outcome.tool_call_label || outcome.tool_result_labels.contains(&entry.label) {
            verify_source_bound_entry(ctx, entry, &scope)?;
        }
    }
    let (evidence, omitted) = outcome_publication_evidence(&config.publication_tool, outcome, config.bounds.max_projected_entries, &entries)?;
    let payload: Value =
        serde_json::from_slice(payload_bytes).map_err(|e| e.to_string())?;
    let decision = payload.get("text").and_then(Value::as_str)
        .ok_or_else(|| "outcome has no final JSON text".to_owned())
        .and_then(|text| serde_json::from_str::<Value>(text).map_err(|e| format!("outcome is not a JSON value: {e}")));
    let (decision, decision_error) = match decision {
        Ok(value) => (value, None),
        Err(error) => (Value::Null, Some(error)),
    };
    let label = if decision_error.is_some() || rejected.is_some() { OUTCOME_ERROR_LABEL } else { OUTCOME_LABEL };
    let mut output = json!({"schema_version":1,"label":label,
                "source":{"run":config.source_run,"ee":observed_ee,"observer_run":config.observer_run,
                    "provider_interface":outcome.provider_interface,"terminal_artifact":artifact,
                    "terminal_source_movement_seq":source_seq},
                "experience":{"source_artifact":selected.source_artifact,"source_movement_seq":selected.source_movement_seq},
                "outcome":decision,"outcome_parse_error":decision_error,"evidence":evidence,
                "omission":{"before_current_experience":true,"unselected_entries":omitted,"selected_evidence_trimmed":false}});
    if let Some(rejected) = rejected {
        output["outcome_refusal"] = json!({"refusal_movement_seqs":rejected.refusal_movement_seqs()});
    }
    Ok((label, output))
}

pub async fn process_admitted_input(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    config: &ContextProjectionConfig,
    envelope: &octopus_interfaces::InterfaceInvocationEnvelope,
    movement: &octopus_ledger::Movement,
) -> Result<(), String> {
    process(ctx, session, config, envelope, movement, false).await
}

pub async fn settle_admitted_input(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    config: &ContextProjectionConfig,
    envelope: &octopus_interfaces::InterfaceInvocationEnvelope,
    movement: &octopus_ledger::Movement,
) -> Result<(), String> {
    process(ctx, session, config, envelope, movement, true).await
}

async fn process(
    ctx: &InterfaceLiftContext,
    session: &ProviderHandoffSession,
    config: &ContextProjectionConfig,
    envelope: &octopus_interfaces::InterfaceInvocationEnvelope,
    movement: &octopus_ledger::Movement,
    settle_unprojectable: bool,
) -> Result<(), String> {
    config.validate()?;
    let descriptor = ctx.resolved_interface_descriptor()?;
    let limits = ctx.required_host_io_limits()?;
    let runtime = envelope
        .metadata
        .as_ref()
        .and_then(|m| m.runtime.as_ref())
        .ok_or("projection input has no admitted runtime evidence")?;
    if runtime.run.as_str() != config.projection_run {
        return Err("projection movement differs from its exact bound projection run".into());
    }
    let pinned = ctx.pinned_run_record_bytes(&envelope)?;
    let handoff = session
        .from_invocation_when_ready(
            ctx,
            &descriptor,
            &octopus_descriptors::RunId::new(&config.projection_run),
            &envelope,
            movement.seq.0,
            &pinned,
        )
        .await
        .map_err(|e| e.to_string())?;
    if let Some(outcome) = handoff.recover_replacement_final().await? {
        return if matches!(outcome, ProviderHandoffOutcome::Admitted { .. }) { Ok(()) }
            else { Err("projection retained replacement was not admitted".into()) };
    }
    let rejected = handoff.rejected_final_output().await?;
    if rejected.is_none() { match handoff.recover_source().await? {
        ProviderSourceRecovery::Completed => {
            return Ok(());
        }
        ProviderSourceRecovery::Process => {}
        ProviderSourceRecovery::Interrupted { .. } => {
            return Err(
                "projection has an interrupted ordinary handoff without a recoverable final output"
                    .into(),
            );
        }
    } }
    let (output_label, bytes) =
        match project_source(ctx, config, envelope, movement, rejected.as_ref(), limits) {
            Ok(projected) => projected,
            Err(error) if !settle_unprojectable => return Err(error),
            Err(error) => (
                OUTCOME_ERROR_LABEL,
                projection_bytes(
                    &json!({"schema_version":1,"label":OUTCOME_ERROR_LABEL,
                        "source":{"run":config.source_run,"projection_run":config.projection_run,
                            "terminal_artifact":envelope.artifact,"projection_source_movement_seq":movement.seq.0},
                        "projection_error":error}),
                    config.bounds.max_output_bytes,
                )?,
            ),
        };
    let artifact =
        InterfaceArtifactFile::write(ctx.var_dir.join("projected-context"), output_label, &bytes)
            .map_err(|e| e.to_string())?;
    let emission = ProviderEmission { artifact, route: ProviderEmissionRoute::ClientProjection };
    let outcome = match rejected {
        Some(rejected) => handoff.hand_replacement_final(&rejected, emission).await?,
        None => handoff.hand_final(emission).await?,
    };
    if !matches!(outcome, ProviderHandoffOutcome::Admitted { .. }) {
        return Err("projection final output was not admitted".into());
    }
    Ok(())
}

fn project_source(
    ctx: &InterfaceLiftContext,
    config: &ContextProjectionConfig,
    envelope: &octopus_interfaces::InterfaceInvocationEnvelope,
    movement: &octopus_ledger::Movement,
    rejected: Option<&RejectedFinalOutput>,
    limits: octopus_interface_transport::InterfaceHostIoLimits,
) -> Result<(&'static str, Vec<u8>), String> {
    let captured = envelope
        .artifact_file
        .read_bounded(limits.max_manifest_bytes, config.bounds.max_input_bytes)
        .map_err(|e| e.to_string())?;
    if captured.manifest.label != config.terminal_label {
        return Err("projection arrived outside its declared terminal label".into());
    }
    let (output_label, output) = if let Some(outcome) = config.outcome.as_ref().filter(|outcome| {
        movement.from
            == octopus_ledger::Endpoint::Interface(octopus_descriptors::InterfaceId::new(
                &outcome.relay_interface,
            ))
    }) {
        project_outcome(ctx, config, outcome, &envelope.artifact, &captured.payload_bytes, movement, rejected)?
    } else {
        if movement.from
            != octopus_ledger::Endpoint::Interface(octopus_descriptors::InterfaceId::new(
                &config.source_relay_interface,
            ))
        {
            return Err(
                "projection input differs from both declared exact relay boundaries".into(),
            );
        }
        let scope = SnapshotSourceScope {
            run: config.source_run.clone(),
            worker: config.source_worker_interface.clone(),
            provider: config.source_provider_interface.clone(),
            client: config.source_client_interface.clone(),
            relay: config.source_relay_interface.clone(),
        };
        let (source_seq, source_ee) = verify_terminal_source(
            ctx,
            movement,
            &envelope.artifact,
            &config.terminal_label,
            &scope,
        )?;
        let entries = resolve_session_snapshot_window(
            &ctx.blobs_dir,
            limits,
            &envelope.artifact,
            &captured.payload_bytes,
            source_seq,
            &config.snapshot_labels,
            &octopus_descriptors::InterfaceId::new(&config.source_provider_interface),
            &config
                .input_rules
                .iter()
                .flat_map(|rule| rule.labels.iter().cloned())
                .collect(),
            &config.bounds.ancestry,
        )?;
        let selected = entries
            .iter()
            .rfind(|entry| {
                config
                    .input_rules
                    .iter()
                    .any(|rule| rule.labels.contains(&entry.label))
            })
            .ok_or("projection window has no selected source input")?;
        let rule = config
            .input_rules
            .iter()
            .find(|rule| rule.labels.contains(&selected.label))
            .ok_or("selected source input rule is absent")?;
        let inbound = selected_source_input_binding(ctx, selected, &scope)?;
        if !config.input_routes.iter().any(|route| {
            route.interface_binding == inbound && route.interface_binding.admits(&selected.label)
        }) {
            return Err(format!(
                "selected input arrived through '{}', outside the declared source input routes",
                inbound.interface
            ));
        }
        let input_proof = verify_selected_source_input(
            ctx,
            selected,
            &scope,
            &config.input_routes,
            rule.origin.as_ref(),
        )?;
        for entry in entries.iter().filter(|entry| {
            config
                .evidence_rules
                .iter()
                .any(|rule| rule.labels.contains(&entry.label))
        }) {
            verify_source_context_entry(ctx, entry, &scope)?;
        }
        let payload: Value =
            serde_json::from_slice(&captured.payload_bytes).map_err(|e| e.to_string())?;
        let parent = serde_json::from_value(payload["session_context"]["parent"].clone())
            .map_err(|e| e.to_string())?;
        let mut output = project_window(
            &config,
            &source_ee,
            &envelope.artifact,
            movement.seq.0,
            &payload,
            parent,
            &entries,
        )?;
        output["source"]["terminal_source_movement_seq"] = json!(source_seq);
        output["selected_input"]["provenance"] = input_proof;
        if let Some(origin) = &rule.origin {
            let reference: ArtifactRef = serde_json::from_value(
                selected
                    .payload
                    .pointer(&origin.artifact_pointer)
                    .cloned()
                    .ok_or("selected source input has no declared original artifact")?,
            )
            .map_err(|e| e.to_string())?;
            let captured =
                octopus_run_authoring::snapshot_projection::read_source_artifact(
                    ctx,
                    &reference,
                    u64::MAX,
                )?;
            let (original, trimmed) =
                fields(&captured, &origin.pointers, config.bounds.max_field_bytes, true)?;
            output["selected_input"]["original_fields"] = original;
            if !trimmed.is_empty() {
                output["selected_input"]["original_trimmed_fields"] = json!(trimmed);
            }
        }
        render_task(config, &mut output)?;
        (OUTPUT_LABEL, output)
    };
    Ok((output_label, projection_bytes(&output, config.bounds.max_output_bytes)?))
}
