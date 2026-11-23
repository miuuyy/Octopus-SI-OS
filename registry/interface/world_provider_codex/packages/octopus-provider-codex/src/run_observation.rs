use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use octopus_descriptors::{ArtifactId, InterfaceId};
use octopus_interface_transport::InterfaceLiftContext;
use octopus_interfaces::{Direction, InterfaceDescriptor, InterfaceType};
use octopus_ledger::{ContentHash, Endpoint, Movement, MovementKind, MovementMetadata};
use octopus_ledger_index::LedgerSqliteReader;
use octopus_ledger_store::DurableMovementDecoder;
use octopus_operator_chat_client::run_composition::CompositionRoots;
use octopus_run_record::RunRecord;
use serde_json::{Value, json};

const MAX_TAIL_MOVEMENTS_PER_RUN: usize = 512;
const MAX_OTHER_RUNS: usize = 64;
const MAX_EE_INSTANCES: usize = 48;
const MAX_DESCRIPTOR_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ERROR_BLOB_BYTES: u64 = 256 * 1024;
const MAX_EXCERPT_CHARS: usize = 300;
const MAX_GOAL_CHARS: usize = 240;
const DEFAULT_MAX_ROW_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) fn inspect_interface_topology(roots: &CompositionRoots) -> Value {
    let bound_runs = collect_active_bound_runs(&roots.run_records_root);
    let interface_root = roots.registry_root.join("interface");
    let mut world_sensors = Vec::new();
    let mut world_services = Vec::new();
    let mut world_providers = Vec::new();
    let mut world_user = Vec::new();
    let mut world_other = Vec::new();
    let mut system_workers = Vec::new();
    let mut system_doors = Vec::new();
    let mut system_services = Vec::new();
    let mut run_private_copies_count: usize = 0;

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&interface_root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        let Some(descriptor) = read_interface_descriptor(&dir.join("interface.json")) else {
            continue;
        };
        let id = descriptor.id.as_str().to_string();
        let role = descriptor.role.0.as_str();
        let is_authored_world_copy = role == "sensor"
            || (descriptor.interface_type == InterfaceType::World
                && matches!(role, "service" | "transformer"));
        if descriptor.copy_of.is_some() && !is_authored_world_copy {
            run_private_copies_count += 1;
            continue;
        }
        let runs: Vec<String> = bound_runs
            .get(&id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();
        match descriptor.interface_type {
            InterfaceType::World => match role {
                "sensor" => world_sensors.push(sensor_or_service_row(&descriptor, runs, false)),
                "service" => world_services.push(sensor_or_service_row(&descriptor, runs, false)),
                "provider" => world_providers.push(id),
                "user" => world_user.push(id),
                _ => world_other.push(sensor_or_service_row(&descriptor, runs, true)),
            },
            InterfaceType::System => match role {
                "worker" => system_workers.push(id),
                "system" => system_doors.push(id),
                _ => system_services.push(id),
            },
        }
    }

    world_providers.sort();
    world_user.sort();
    system_workers.sort();
    system_doors.sort();
    system_services.sort();

    json!({
        "world": {
            "sensors": world_sensors,
            "services": world_services,
            "providers": world_providers,
            "user": world_user,
            "other": world_other,
        },
        "system": {
            "workers": system_workers,
            "doors": system_doors,
            "services": system_services,
        },
        "run_private_copies_count": run_private_copies_count,
    })
}

pub(crate) fn inspect_ee_tree(roots: &CompositionRoots, current_ee: &str) -> (Vec<Value>, usize, bool) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&roots.ee_instances_root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    entries.sort();

    let mut by_id: BTreeMap<String, (Option<String>, Value)> = BTreeMap::new();
    for path in entries {
        let Some(value) = read_json_bounded(&path, MAX_DESCRIPTOR_BYTES) else {
            continue;
        };
        let Some(ee_id) = value.get("ee_id").and_then(Value::as_str) else {
            continue;
        };
        let name = value.get("name").and_then(Value::as_str).unwrap_or("");
        let template_name = value
            .get("template_name")
            .and_then(Value::as_str)
            .unwrap_or("");
        let parent_ee_id = value.get("parent_ee_id").and_then(Value::as_str);
        let goal = value
            .get("goal")
            .and_then(Value::as_str)
            .map(|text| clip_single_line(text, MAX_GOAL_CHARS))
            .unwrap_or_default();
        by_id.insert(
            ee_id.to_string(),
            (
                parent_ee_id.map(ToOwned::to_owned),
                json!({
                    "ee_id": ee_id,
                    "name": name,
                    "template_name": template_name,
                    "parent_ee_id": parent_ee_id,
                    "goal": goal,
                }),
            ),
        );
    }

    let total = by_id.len();
    if total <= MAX_EE_INSTANCES {
        let all = by_id.into_values().map(|(_, row)| row).collect();
        return (all, total, false);
    }

    let mut selected = BTreeSet::new();
    let mut cursor = Some(current_ee.to_string());
    let mut hops = 0usize;
    while let Some(id) = cursor {
        if hops > 64 || !selected.insert(id.clone()) {
            break;
        }
        hops += 1;
        cursor = by_id.get(&id).and_then(|(parent, _)| parent.clone());
    }
    for (id, (parent, _)) in &by_id {
        if parent.as_deref() == Some(current_ee) {
            selected.insert(id.clone());
        }
    }
    for id in by_id.keys() {
        if selected.len() >= MAX_EE_INSTANCES {
            break;
        }
        selected.insert(id.clone());
    }

    let instances: Vec<Value> = selected
        .into_iter()
        .filter_map(|id| by_id.remove(&id).map(|(_, row)| row))
        .collect();
    let truncated = instances.len() < total;
    (instances, total, truncated)
}

pub(crate) struct ObservedRuns {
    pub source_activity: Value,
    pub source_provider_activity: String,
    pub source_title: Option<String>,
    pub source_provider_interface_id: Option<String>,
    pub source_model: Option<String>,
    pub source_reasoning_effort: Option<String>,
    pub source_external_inputs: Vec<String>,
    pub declared_peer_runs: Vec<Value>,
    pub other_runs: Option<Vec<Value>>,
    pub other_runs_total: Option<usize>,
    pub archived_runs_count: Option<usize>,
}

pub(crate) fn observe_runs_pulse(
    source_record: &RunRecord,
    declared_peers: &BTreeMap<String, Option<String>>,
    roots: Option<&CompositionRoots>,
    life_ctx: Option<&InterfaceLiftContext>,
    include_other_runs: bool,
) -> ObservedRuns {
    let now_ms = current_unix_ms();
    let run_records_root = roots
        .map(|r| r.run_records_root.as_path())
        .or_else(|| {
            life_ctx
                .and_then(|ctx| ctx.root_slots.get("run_records_root"))
                .map(PathBuf::as_path)
        });
    let registry_interface_root = roots
        .map(|r| r.registry_root.join("interface"))
        .or_else(|| {
            life_ctx
                .and_then(|ctx| ctx.root_slots.get("registry_interface_root"))
                .cloned()
        });

    let mut descriptor_cache = DescriptorCache::new(registry_interface_root.as_deref());
    let mut records_by_id: BTreeMap<String, RunRecord> = BTreeMap::new();
    records_by_id.insert(source_record.id.as_str().to_string(), source_record.clone());

    let mut peer_run_ids = BTreeSet::new();
    for run_id in declared_peers.values().flatten() {
        peer_run_ids.insert(run_id.clone());
        if !records_by_id.contains_key(run_id) {
            if let Some(root) = run_records_root {
                if let Some(record) = load_run_record_file(&root.join(format!("{run_id}.json"))) {
                    records_by_id.insert(run_id.clone(), record);
                }
            }
        }
    }

    let mut candidate_other_ids: Vec<String> = Vec::new();
    let mut archived_count: usize = 0;
    if include_other_runs {
        if let Some(root) = run_records_root {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(root)
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
                .collect();
            paths.sort();
            for path in paths {
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if stem == source_record.id.as_str() || peer_run_ids.contains(stem) {
                    continue;
                }
                let Some(record) = load_run_record_file(&path) else {
                    continue;
                };
                let summary = inspect_run_bindings(&record, &mut descriptor_cache);
                if summary.archived {
                    archived_count += 1;
                    continue;
                }
                if summary.provider_interface_id.is_none() && summary.external_inputs.is_empty() {
                    continue;
                }
                let id = record.id.as_str().to_string();
                records_by_id.insert(id.clone(), record);
                candidate_other_ids.push(id);
            }
        }
    }

    let mut target_run_ids = BTreeSet::new();
    target_run_ids.insert(source_record.id.as_str().to_string());
    target_run_ids.extend(peer_run_ids.iter().cloned());
    target_run_ids.extend(candidate_other_ids.iter().cloned());

    let ledger_tails = life_ctx.and_then(|ctx| load_tail_movements_by_run(ctx, &target_run_ids));
    let blobs_dir = life_ctx.map(|ctx| ctx.blobs_dir.as_path());

    let source_bindings = inspect_run_bindings(source_record, &mut descriptor_cache);
    let (source_activity, source_phase) = build_run_activity(
        Some(source_record),
        &source_bindings,
        ledger_tails
            .as_ref()
            .and_then(|map| map.get(source_record.id.as_str())),
        ledger_tails.is_some(),
        blobs_dir,
        now_ms,
    );
    let source_provider_activity = if ledger_tails.is_some() {
        source_phase.to_string()
    } else {
        "not_observed".to_string()
    };

    let mut declared_peer_runs = Vec::new();
    for (interface_id, maybe_run_id) in declared_peers {
        let Some(run_id) = maybe_run_id else { continue };
        let record = records_by_id.get(run_id);
        let bindings = record.map(|rec| inspect_run_bindings(rec, &mut descriptor_cache));
        let (activity, _) = build_run_activity(
            record,
            bindings.as_ref().unwrap_or(&RunBindingsSummary::empty()),
            ledger_tails.as_ref().and_then(|map| map.get(run_id)),
            ledger_tails.is_some(),
            blobs_dir,
            now_ms,
        );
        let mut entry = json!({
            "interface_id": interface_id,
            "run_id": run_id,
            "activity": activity,
        });
        if let Some(rec) = record {
            attach_record_metadata(&mut entry, rec, bindings.as_ref().unwrap());
        }
        declared_peer_runs.push(entry);
    }

    let (other_runs, other_runs_total, archived_runs_count) = if include_other_runs {
        let mut rows: Vec<(u64, String, Value)> = Vec::new();
        for run_id in candidate_other_ids {
            let Some(record) = records_by_id.get(&run_id) else {
                continue;
            };
            let bindings = inspect_run_bindings(record, &mut descriptor_cache);
            let (activity, _) = build_run_activity(
                Some(record),
                &bindings,
                ledger_tails.as_ref().and_then(|map| map.get(&run_id)),
                ledger_tails.is_some(),
                blobs_dir,
                now_ms,
            );
            let sort_ts = activity
                .get("last_movement_at_unix_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let mut entry = json!({
                "run_id": run_id,
                "activity": activity,
            });
            attach_record_metadata(&mut entry, record, &bindings);
            rows.push((sort_ts, run_id, entry));
        }
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let total = rows.len();
        let trimmed: Vec<Value> = rows
            .into_iter()
            .take(MAX_OTHER_RUNS)
            .map(|(_, _, val)| val)
            .collect();
        (Some(trimmed), Some(total), Some(archived_count))
    } else {
        (None, None, None)
    };

    ObservedRuns {
        source_activity,
        source_provider_activity,
        source_title: source_bindings.title,
        source_provider_interface_id: source_bindings.provider_interface_id,
        source_model: source_bindings.model,
        source_reasoning_effort: source_bindings.reasoning_effort,
        source_external_inputs: source_bindings.external_inputs,
        declared_peer_runs,
        other_runs,
        other_runs_total,
        archived_runs_count,
    }
}

fn attach_record_metadata(target: &mut Value, record: &RunRecord, bindings: &RunBindingsSummary) {
    if let Some(obj) = target.as_object_mut() {
        obj.insert("ee_id".into(), json!(record.ee));
        obj.insert("worker_interface_id".into(), json!(record.worker));
        obj.insert("recorded_status".into(), record.status.clone());
        if let Some(title) = &bindings.title {
            obj.insert("title".into(), json!(title));
        }
        if let Some(provider) = &bindings.provider_interface_id {
            obj.insert("provider_interface_id".into(), json!(provider));
        }
        if let Some(model) = &bindings.model {
            obj.insert("model".into(), json!(model));
        }
        if let Some(effort) = &bindings.reasoning_effort {
            obj.insert("reasoning_effort".into(), json!(effort));
        }
        if !bindings.external_inputs.is_empty() {
            obj.insert("external_inputs".into(), json!(bindings.external_inputs));
        }
    }
}

struct RunBindingsSummary {
    title: Option<String>,
    archived: bool,
    provider_interface_id: Option<String>,
    provider_interfaces: BTreeSet<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    external_inputs: Vec<String>,
    world_inbound_interfaces: BTreeSet<String>,
    door_interfaces: BTreeSet<String>,
}

impl RunBindingsSummary {
    fn empty() -> Self {
        Self {
            title: None,
            archived: false,
            provider_interface_id: None,
            provider_interfaces: BTreeSet::new(),
            model: None,
            reasoning_effort: None,
            external_inputs: Vec::new(),
            world_inbound_interfaces: BTreeSet::new(),
            door_interfaces: BTreeSet::new(),
        }
    }
}

struct DescriptorCache<'a> {
    interface_root: Option<&'a Path>,
    descriptors: BTreeMap<String, Option<InterfaceDescriptor>>,
    sidecars: BTreeMap<String, Option<Value>>,
}

impl<'a> DescriptorCache<'a> {
    fn new(interface_root: Option<&'a Path>) -> Self {
        Self {
            interface_root,
            descriptors: BTreeMap::new(),
            sidecars: BTreeMap::new(),
        }
    }

    fn descriptor(&mut self, interface_id: &str) -> Option<&InterfaceDescriptor> {
        if !self.descriptors.contains_key(interface_id) {
            let loaded = self.interface_root.and_then(|root| {
                read_interface_descriptor(&root.join(interface_id).join("interface.json"))
            });
            self.descriptors.insert(interface_id.to_string(), loaded);
        }
        self.descriptors.get(interface_id).and_then(Option::as_ref)
    }

    fn sidecar(&mut self, interface_id: &str) -> Option<&Value> {
        if !self.sidecars.contains_key(interface_id) {
            let loaded = self.interface_root.and_then(|root| {
                read_json_bounded(
                    &root.join(interface_id).join("octopus_os.json"),
                    MAX_DESCRIPTOR_BYTES,
                )
            });
            self.sidecars.insert(interface_id.to_string(), loaded);
        }
        self.sidecars.get(interface_id).and_then(Option::as_ref)
    }
}

fn inspect_run_bindings(record: &RunRecord, cache: &mut DescriptorCache<'_>) -> RunBindingsSummary {
    let active_ids: BTreeSet<&str> = record
        .active_checkpoints
        .iter()
        .map(|id| id.as_str())
        .collect();
    let mut all_interfaces = BTreeSet::new();
    let mut active_inbound = BTreeSet::new();
    for checkpoint in &record.checkpoints {
        let is_active = active_ids.contains(checkpoint.id.as_str());
        if let Some(iface) = checkpoint.inbound_interface() {
            all_interfaces.insert(iface.as_str().to_string());
            if is_active {
                active_inbound.insert(iface.as_str().to_string());
            }
        }
        for iface in checkpoint.outbound_interfaces() {
            all_interfaces.insert(iface.as_str().to_string());
        }
    }
    for iface in record.interface_state.keys() {
        all_interfaces.insert(iface.as_str().to_string());
    }

    let preferred_provider = record
        .interface_state
        .values()
        .find_map(|state| {
            state
                .get("provider_runtime_interface")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        });

    let mut summary = RunBindingsSummary::empty();
    let mut external_inputs = BTreeSet::new();

    for iface_id in &all_interfaces {
        let Some(descriptor) = cache.descriptor(iface_id).cloned() else {
            continue;
        };
        let role = descriptor.role.0.as_str();
        match (descriptor.interface_type, role) {
            (_, "provider") => {
                summary.provider_interfaces.insert(iface_id.clone());
                let is_preferred = preferred_provider.as_deref() == Some(iface_id.as_str());
                let (model, effort) = extract_provider_model_and_effort(&descriptor);
                if is_preferred
                    || summary.provider_interface_id.is_none()
                    || (summary.model.is_none() && model.is_some() && preferred_provider.is_none())
                {
                    summary.provider_interface_id = Some(iface_id.clone());
                    summary.model = model;
                    summary.reasoning_effort = effort;
                }
            }
            (InterfaceType::World, "user") => {
                summary.world_inbound_interfaces.insert(iface_id.clone());
                if let Some(sidecar) = cache.sidecar(iface_id) {
                    if summary.title.is_none() {
                        summary.title = sidecar
                            .pointer("/presentation/name")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(ToOwned::to_owned);
                    }
                    if sidecar.get("archived").and_then(Value::as_bool) == Some(true)
                        || sidecar.get("retired").and_then(Value::as_bool) == Some(true)
                    {
                        summary.archived = true;
                    }
                }
            }
            (InterfaceType::System, "system") => {
                summary.door_interfaces.insert(iface_id.clone());
            }
            (InterfaceType::World, _) => {
                summary.world_inbound_interfaces.insert(iface_id.clone());
                if active_inbound.contains(iface_id) {
                    external_inputs.insert(iface_id.clone());
                }
            }
            _ => {}
        }
    }

    summary.external_inputs = external_inputs.into_iter().collect();
    summary
}

fn extract_provider_model_and_effort(
    descriptor: &InterfaceDescriptor,
) -> (Option<String>, Option<String>) {
    let private_cfg = descriptor
        .interface_driver_config
        .as_ref()
        .map(|cfg| &cfg.private_config);
    let pick = |keys: &[&str]| -> Option<String> {
        for key in keys {
            if let Some(value) = private_cfg
                .and_then(|cfg| cfg.get(*key))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(value.to_string());
            }
        }
        None
    };
    let model = pick(&["model", "default_model"]);
    let effort = pick(&["reasoning_effort", "default_reasoning_effort"]);
    (model, effort)
}

fn build_run_activity(
    record: Option<&RunRecord>,
    bindings: &RunBindingsSummary,
    movements: Option<&VecDeque<Movement>>,
    ledger_readable: bool,
    blobs_dir: Option<&Path>,
    now_ms: u64,
) -> (Value, &'static str) {
    if !ledger_readable {
        return (
            json!({
                "phase": "unreadable",
                "last_movement_seq": null,
                "last_movement_at_unix_ms": null,
                "seconds_since_last_movement": null,
                "last_movement_kind": null,
                "last_artifact_label": null,
                "last_provider_activity_at_unix_ms": null,
                "last_provider_artifact_label": null,
                "pending_tool_calls": [],
                "recent_refusal": null,
                "last_error": null,
            }),
            "unreadable",
        );
    }

    let empty = VecDeque::new();
    let rows = movements.unwrap_or(&empty);

    let mut last_movement_seq: Option<u64> = None;
    let mut last_movement_at_unix_ms: Option<u64> = None;
    let mut last_movement_kind: Option<&'static str> = None;
    let mut last_artifact_label: Option<String> = None;
    let mut last_provider_activity_at_unix_ms: Option<u64> = None;
    let mut last_provider_artifact_label: Option<String> = None;
    let mut turn_open = false;
    let mut pending_doors: BTreeMap<String, (String, u64, u64)> = BTreeMap::new();
    let mut recent_refusal: Option<Value> = None;
    let mut last_error: Option<(u64, u64, String, Option<ContentHash>)> = None;

    for movement in rows {
        let seq = movement.seq.0;
        let ts = movement.recorded_at_unix_ms;
        let label = movement
            .artifact
            .as_ref()
            .map(|artifact| artifact_label(&artifact.id));
        let from_id = endpoint_interface(&movement.from);
        let to_id = endpoint_interface(&movement.to);

        last_movement_seq = Some(seq);
        last_movement_at_unix_ms = Some(ts);
        last_movement_kind = Some(movement_kind_str(&movement.kind));
        if label.is_some() {
            last_artifact_label = label.clone();
        }

        let from_provider = bindings.provider_interfaces.contains(from_id);
        let to_provider = bindings.provider_interfaces.contains(to_id);

        if from_provider && movement.kind == MovementKind::InterfaceArrival {
            last_provider_activity_at_unix_ms = Some(ts);
            if let Some(lbl) = &label {
                last_provider_artifact_label = Some(lbl.clone());
            }
        }

        match &movement.kind {
            MovementKind::DeliveryRefused | MovementKind::VerificationFailed => {
                let reason = movement
                    .invocation_metadata
                    .as_ref()
                    .and_then(MovementMetadata::delivery_verdict)
                    .and_then(|verdict| match verdict {
                        octopus_ledger::DeliveryVerdict::Refused { reason, .. } => {
                            Some(reason.as_str())
                        }
                        _ => None,
                    });
                recent_refusal = Some(json!({
                    "seq": seq,
                    "at_unix_ms": ts,
                    "kind": movement_kind_str(&movement.kind),
                    "reason": reason,
                    "label": label,
                }));
            }
            MovementKind::InterfaceOutbound => {
                if bindings.door_interfaces.contains(to_id) {
                    let call_label = label.clone().unwrap_or_else(|| "tool.call".into());
                    pending_doors.insert(to_id.to_string(), (call_label, seq, ts));
                    turn_open = true;
                } else if to_provider {
                    turn_open = true;
                    last_error = None;
                    recent_refusal = None;
                }
            }
            MovementKind::InterfaceArrival => {
                if bindings.door_interfaces.contains(from_id) {
                    pending_doors.remove(from_id);
                } else if from_provider {
                    match label.as_deref() {
                        Some("turn.started" | "provider.transport.dispatch") => {
                            turn_open = true;
                            last_error = None;
                            recent_refusal = None;
                        }
                        Some("turn.completed" | "turn.interrupted.result") => {
                            turn_open = false;
                            pending_doors.clear();
                        }
                        Some("turn.error" | "turn.control.failed") => {
                            turn_open = false;
                            pending_doors.clear();
                            last_error = Some((
                                seq,
                                ts,
                                label.clone().unwrap_or_default(),
                                movement.artifact.as_ref().map(|a| a.content),
                            ));
                        }
                        _ => {}
                    }
                } else if bindings.world_inbound_interfaces.contains(from_id) {
                    turn_open = true;
                    last_error = None;
                    recent_refusal = None;
                }
            }
            _ => {}
        }
    }

    let mut pending_tool_calls: Vec<Value> = pending_doors
        .iter()
        .map(|(iface, (label, seq, ts))| {
            json!({
                "interface_id": iface,
                "label": label,
                "since_seq": seq,
                "since_at_unix_ms": ts,
                "waiting_seconds": now_ms.saturating_sub(*ts) / 1000,
            })
        })
        .collect();

    if let Some(rec) = record {
        let active_set: BTreeSet<&str> = rec.active_checkpoints.iter().map(|id| id.as_str()).collect();
        for checkpoint in &rec.checkpoints {
            if !active_set.contains(checkpoint.id.as_str()) {
                continue;
            }
            if checkpoint.required_verified_artifacts.is_empty() {
                continue;
            }
            let Some(iface) = checkpoint.inbound_interface() else {
                continue;
            };
            if bindings.door_interfaces.contains(iface.as_str())
                && !pending_doors.contains_key(iface.as_str())
            {
                pending_tool_calls.push(json!({
                    "interface_id": iface.as_str(),
                    "checkpoint_id": checkpoint.id.as_str(),
                    "label": "awaiting_verified_artifact",
                }));
            }
        }
    }

    let last_error_json = last_error.as_ref().map(|(seq, ts, label, content_hash)| {
        let excerpt = content_hash
            .as_ref()
            .and_then(|hash| blobs_dir.and_then(|dir| read_error_excerpt(dir, hash)));
        json!({
            "seq": seq,
            "at_unix_ms": ts,
            "label": label,
            "excerpt": excerpt,
        })
    });

    let record_running = record.is_some_and(|rec| rec.status.as_str() == Some("running"));
    let phase = if last_movement_seq.is_none() {
        if record_running {
            "turn_open"
        } else {
            "not_started"
        }
    } else if !pending_tool_calls.is_empty() {
        "awaiting_tool_result"
    } else if turn_open || record_running {
        "turn_open"
    } else if last_error.is_some() {
        "error"
    } else {
        "idle"
    };

    let seconds_since_last_movement =
        last_movement_at_unix_ms.map(|ts| now_ms.saturating_sub(ts) / 1000);

    (
        json!({
            "phase": phase,
            "last_movement_seq": last_movement_seq,
            "last_movement_at_unix_ms": last_movement_at_unix_ms,
            "seconds_since_last_movement": seconds_since_last_movement,
            "last_movement_kind": last_movement_kind,
            "last_artifact_label": last_artifact_label,
            "last_provider_activity_at_unix_ms": last_provider_activity_at_unix_ms,
            "last_provider_artifact_label": last_provider_artifact_label,
            "pending_tool_calls": pending_tool_calls,
            "recent_refusal": recent_refusal,
            "last_error": last_error_json,
        }),
        phase,
    )
}

fn load_tail_movements_by_run(
    ctx: &InterfaceLiftContext,
    target_runs: &BTreeSet<String>,
) -> Option<BTreeMap<String, VecDeque<Movement>>> {
    if target_runs.is_empty() {
        return Some(BTreeMap::new());
    }
    let mut file = match File::open(&ctx.movements_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(BTreeMap::new()),
        Err(_) => return None,
    };
    let snapshot_len = file.metadata().ok()?.len();
    let max_row_bytes = ctx
        .host_io_limits
        .map(|limits| limits.max_control_frame_bytes)
        .unwrap_or(DEFAULT_MAX_ROW_BYTES);
    let decoder = DurableMovementDecoder::for_log(&ctx.movements_path);

    if let Some(indexed) = try_load_via_sqlite_index(
        ctx,
        &mut file,
        snapshot_len,
        max_row_bytes,
        &decoder,
        target_runs,
    ) {
        return Some(indexed);
    }

    scan_movements_into_tails(
        &mut file,
        0,
        snapshot_len,
        max_row_bytes,
        &decoder,
        target_runs,
        BTreeMap::new(),
    )
}

fn try_load_via_sqlite_index(
    ctx: &InterfaceLiftContext,
    file: &mut File,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    target_runs: &BTreeSet<String>,
) -> Option<BTreeMap<String, VecDeque<Movement>>> {
    let reader = LedgerSqliteReader::open(&ctx.ledger_index_path).ok()?;
    let progress = reader.progress().ok()?;
    if progress.durable_end_offset > snapshot_len {
        return None;
    }

    let mut by_run: BTreeMap<String, VecDeque<Movement>> = BTreeMap::new();
    for run_id in target_runs {
        let rows = reader
            .movement_rows_touching_run(run_id, 0, progress.watermark)
            .ok()??;
        let skip = rows.len().saturating_sub(MAX_TAIL_MOVEMENTS_PER_RUN);
        let mut queue = VecDeque::with_capacity(rows.len().saturating_sub(skip));
        for range in rows.into_iter().skip(skip) {
            if range.end_offset > snapshot_len || range.end_offset <= range.start_offset {
                return None;
            }
            let len = range.end_offset - range.start_offset;
            if len > max_row_bytes {
                return None;
            }
            file.seek(SeekFrom::Start(range.start_offset)).ok()?;
            let mut buf = vec![0u8; len as usize];
            file.read_exact(&mut buf).ok()?;
            let (movement, _) = decoder
                .decode_projection(&buf, (range.seq + 1) as usize)
                .ok()?;
            if movement.seq.0 != range.seq {
                return None;
            }
            queue.push_back(movement);
        }
        if !queue.is_empty() {
            by_run.insert(run_id.clone(), queue);
        }
    }

    if progress.durable_end_offset < snapshot_len {
        return scan_movements_into_tails(
            file,
            progress.durable_end_offset,
            snapshot_len,
            max_row_bytes,
            decoder,
            target_runs,
            by_run,
        );
    }

    Some(by_run)
}

fn scan_movements_into_tails(
    file: &mut File,
    start_offset: u64,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    target_runs: &BTreeSet<String>,
    mut by_run: BTreeMap<String, VecDeque<Movement>>,
) -> Option<BTreeMap<String, VecDeque<Movement>>> {
    file.seek(SeekFrom::Start(start_offset)).ok()?;
    let remaining = snapshot_len.saturating_sub(start_offset);
    let mut reader = BufReader::new(file.take(remaining));
    let mut buf = Vec::new();
    let mut line_index = 1usize;

    loop {
        buf.clear();
        let read = reader.read_until(b'\n', &mut buf).ok()?;
        if read == 0 {
            break;
        }
        if !buf.ends_with(b"\n") {
            break;
        }
        if (buf.len() as u64) > max_row_bytes {
            return None;
        }
        let Ok((movement, _)) = decoder.decode_projection(&buf, line_index) else {
            line_index += 1;
            continue;
        };
        if let Some(run_id) = movement.run() {
            let key = run_id.as_str();
            if target_runs.contains(key) {
                let queue = by_run.entry(key.to_string()).or_default();
                if queue.len() == MAX_TAIL_MOVEMENTS_PER_RUN {
                    queue.pop_front();
                }
                queue.push_back(movement);
            }
        }
        line_index += 1;
    }

    Some(by_run)
}

fn read_error_excerpt(blobs_dir: &Path, hash: &ContentHash) -> Option<String> {
    let path = blobs_dir.join(hash.to_hex());
    let file = File::open(&path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > MAX_ERROR_BLOB_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(MAX_ERROR_BLOB_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if (bytes.len() as u64) > MAX_ERROR_BLOB_BYTES {
        return None;
    }
    if ContentHash::of_bytes(&bytes) != *hash {
        return None;
    }
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| value.pointer("/error/message").and_then(Value::as_str))
            .or_else(|| value.get("error").and_then(Value::as_str))
            .or_else(|| value.get("detail").and_then(Value::as_str))
            .or_else(|| value.get("reason").and_then(Value::as_str));
        if let Some(text) = message {
            return Some(clip_single_line(text, MAX_EXCERPT_CHARS));
        }
        if let Ok(rendered) = serde_json::to_string(&value) {
            return Some(clip_single_line(&rendered, MAX_EXCERPT_CHARS));
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    Some(clip_single_line(&text, MAX_EXCERPT_CHARS))
}

fn collect_active_bound_runs(run_records_root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(run_records_root) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(record) = load_run_record_file(&path) else {
            continue;
        };
        let active: BTreeSet<&str> = record
            .active_checkpoints
            .iter()
            .map(|id| id.as_str())
            .collect();
        for checkpoint in &record.checkpoints {
            if !active.contains(checkpoint.id.as_str()) {
                continue;
            }
            if let Some(iface) = checkpoint.inbound_interface() {
                map.entry(iface.as_str().to_string())
                    .or_default()
                    .insert(record.id.as_str().to_string());
            }
            for iface in checkpoint.outbound_interfaces() {
                map.entry(iface.as_str().to_string())
                    .or_default()
                    .insert(record.id.as_str().to_string());
            }
        }
    }
    map
}

fn sensor_or_service_row(
    descriptor: &InterfaceDescriptor,
    bound_runs: Vec<String>,
    include_role: bool,
) -> Value {
    let direction = match descriptor.direction {
        Direction::Inbound => "inbound",
        Direction::Outbound => "outbound",
        Direction::Bidirectional => "bidirectional",
    };
    let mut row = json!({
        "id": descriptor.id.as_str(),
        "copy_of": descriptor.copy_of.as_ref().map(InterfaceId::as_str),
        "driver": descriptor.interface_driver_ref,
        "direction": direction,
        "sources": descriptor.source_names,
        "bound_runs": bound_runs,
    });
    if include_role {
        row["role"] = json!(descriptor.role.0);
    }
    row
}

fn read_interface_descriptor(path: &Path) -> Option<InterfaceDescriptor> {
    let bytes = read_bytes_bounded(path, MAX_DESCRIPTOR_BYTES)?;
    serde_json::from_slice(&bytes).ok()
}

fn read_json_bounded(path: &Path, limit: u64) -> Option<Value> {
    let bytes = read_bytes_bounded(path, limit)?;
    serde_json::from_slice(&bytes).ok()
}

fn load_run_record_file(path: &Path) -> Option<RunRecord> {
    let bytes = read_bytes_bounded(path, MAX_DESCRIPTOR_BYTES)?;
    RunRecord::from_bytes(&bytes).ok()
}

fn read_bytes_bounded(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > limit {
        return None;
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    if (bytes.len() as u64) > limit {
        return None;
    }
    Some(bytes)
}

fn artifact_label(id: &ArtifactId) -> String {
    let raw = id.as_str();
    if let Some((label, suffix)) = raw.rsplit_once(':') {
        if suffix.len() == 64 && suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return label.to_string();
        }
    }
    raw.to_string()
}

fn endpoint_interface(endpoint: &Endpoint) -> &str {
    match endpoint {
        Endpoint::Interface(id) => id.as_str(),
    }
}

fn movement_kind_str(kind: &MovementKind) -> &'static str {
    match kind {
        MovementKind::ArtifactMoved => "artifact_moved",
        MovementKind::MarkOutcomeAdmitted(_) => "mark_outcome_admitted",
        MovementKind::VerificationFailed => "verification_failed",
        MovementKind::DeliveryRefused => "delivery_refused",
        MovementKind::InterfaceArrival => "interface_arrival",
        MovementKind::InterfaceOutbound => "interface_outbound",
    }
}

fn clip_single_line(text: &str, max_chars: usize) -> String {
    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let collected: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{collected}…")
    } else {
        collected
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{CheckpointId, EeId, RunId};
    use octopus_interface_transport::InterfaceHostIoLimits;
    use octopus_ledger::{
        ArtifactRef, DeliveryMetadata, DeliveryVerdict, MovementDraft, RefusedReason,
        RuntimeInvocationMetadata, TraceId,
    };
    use octopus_ledger_index::LedgerSqliteIndex;
    use octopus_ledger_store::{DurableBlobs, DurableLog};

    fn write_json(path: &Path, value: &Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    }

    fn write_iface(root: &Path, value: &Value) {
        let id = value["id"].as_str().unwrap();
        write_json(&root.join("interface").join(id).join("interface.json"), value);
    }

    fn write_run(root: &Path, run_id: &str, status: &str, bindings: &[(&str, &str)], outbound: &[(&str, &str)]) -> RunRecord {
        let mut checkpoints = Vec::new();
        let mut active = Vec::new();
        for (idx, (cp_id, iface)) in bindings.iter().enumerate() {
            active.push(json!(cp_id));
            let outs: Vec<Value> = outbound
                .iter()
                .filter(|(c, _)| c == cp_id)
                .map(|(_, o)| json!({"interface": o, "config": {"artifact_policy": {"mode": "all"}}}))
                .collect();
            checkpoints.push(json!({
                "id": cp_id,
                "run": run_id,
                "trace": format!("trace_{run_id}_{idx}"),
                "epoch": 0,
                "interface_binding": {"interface": iface, "config": {"artifact_policy": {"mode": "all"}}},
                "outbound_interface_bindings": outs,
            }));
        }
        let value = json!({
            "id": run_id,
            "ee": "ee_root",
            "worker": "system_worker_operator_task",
            "trace": format!("trace_{run_id}"),
            "status": status,
            "checkpoints": checkpoints,
            "active_checkpoints": active,
        });
        write_json(&root.join(format!("{run_id}.json")), &value);
        RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn runtime_meta(run: &str, iface: &str, verdict: DeliveryVerdict) -> MovementMetadata {
        MovementMetadata::Delivery(DeliveryMetadata {
            delivery_verdict: verdict,
            runtime: Some(RuntimeInvocationMetadata {
                run: RunId::new(run),
                ee: EeId::new("ee_root"),
                worker: InterfaceId::new("system_worker_operator_task"),
                checkpoint: CheckpointId::new("cp_prov"),
                interface: InterfaceId::new(iface),
                epoch: 0,
                run_record_hash: None,
            }),
            source_movement_seq: None,
            delivery_scope_seq: None,
        })
    }

    fn artifact_with_hash(label: &str, hash: ContentHash) -> ArtifactRef {
        ArtifactRef {
            id: ArtifactId::new(format!("{label}:{}", hash.to_hex())),
            content: hash,
        }
    }

    #[test]
    fn topology_separates_system_world_and_private_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = tmp.path().join("registry");
        let runs = tmp.path().join("runs");
        let ees = tmp.path().join("ees");
        let roots = CompositionRoots {
            registry_root: reg.clone(),
            ee_instances_root: ees.clone(),
            run_records_root: runs.clone(),
            interface_private_state_root: tmp.path().join("var"),
        };
        write_iface(&reg, &json!({"id":"world_watch","interface_type":"world","role":"sensor","direction":"inbound","source_names":["watch.event"],"interface_driver_ref":"world_watch_life"}));
        write_iface(&reg, &json!({"id":"world_watch_status","copy_of":"world_watch","interface_type":"world","role":"sensor","direction":"inbound","source_names":["watch.event"],"interface_driver_ref":"world_watch_life"}));
        write_iface(&reg, &json!({"id":"world_provider_antigravity","interface_type":"world","role":"provider","direction":"bidirectional","source_names":["antigravity"],"interface_driver_ref":"codex"}));
        write_iface(&reg, &json!({"id":"world_provider_antigravity_task1","copy_of":"world_provider_antigravity","interface_type":"world","role":"provider","direction":"bidirectional","source_names":["antigravity"],"interface_driver_ref":"codex"}));
        write_iface(&reg, &json!({"id":"world_operator_chat","interface_type":"world","role":"user","direction":"bidirectional","source_names":["chat"],"interface_driver_ref":"app_server"}));
        write_iface(&reg, &json!({"id":"world_operator_chat_task1","copy_of":"world_operator_chat","interface_type":"world","role":"user","direction":"bidirectional","source_names":["chat"],"interface_driver_ref":"app_server"}));
        write_iface(&reg, &json!({"id":"system_worker_operator_task","interface_type":"system","role":"worker","direction":"inbound","source_names":["task"],"interface_driver_ref":""}));
        write_iface(&reg, &json!({"id":"system_workspace_env","interface_type":"system","role":"system","direction":"bidirectional","source_names":["tool"],"interface_driver_ref":""}));
        write_iface(&reg, &json!({"id":"system_workspace_env_task1","copy_of":"system_workspace_env","interface_type":"system","role":"system","direction":"bidirectional","source_names":["tool"],"interface_driver_ref":""}));
        write_iface(&reg, &json!({"id":"system_memory","interface_type":"system","role":"service","direction":"bidirectional","source_names":["memory"],"interface_driver_ref":"memory"}));
        write_iface(&reg, &json!({"id":"system_service_input_task1","copy_of":"system_memory","interface_type":"system","role":"service","direction":"inbound","source_names":["peer"],"interface_driver_ref":"intake"}));
        write_run(&runs, "run_task1", "idle", &[("cp_w", "world_watch_status")], &[]);
        write_json(&ees.join("ee_root.json"), &json!({"ee_id":"ee_root","name":"Home","template_name":"existence","parent_ee_id":null,"goal":"Root existence goal"}));

        let topology = inspect_interface_topology(&roots);
        assert_eq!(topology["run_private_copies_count"], json!(4));
        assert_eq!(topology["world"]["providers"], json!(["world_provider_antigravity"]));
        assert_eq!(topology["world"]["user"], json!(["world_operator_chat"]));
        assert_eq!(topology["system"]["workers"], json!(["system_worker_operator_task"]));
        assert_eq!(topology["system"]["doors"], json!(["system_workspace_env"]));
        assert_eq!(topology["system"]["services"], json!(["system_memory"]));
        let sensors = topology["world"]["sensors"].as_array().unwrap();
        assert_eq!(sensors.len(), 2);
        let custom = sensors.iter().find(|s| s["id"] == "world_watch_status").unwrap();
        assert_eq!(custom["copy_of"], json!("world_watch"));
        assert_eq!(custom["bound_runs"], json!(["run_task1"]));

        let (ees_out, ee_total, ee_truncated) = inspect_ee_tree(&roots, "ee_root");
        assert_eq!(ees_out.len(), 1);
        assert_eq!(ee_total, 1);
        assert!(!ee_truncated);
        assert_eq!(ees_out[0]["ee_id"], json!("ee_root"));

        write_json(&ees.join("ee_mid.json"), &json!({"ee_id":"ee_mid","name":"Mid","template_name":"project","parent_ee_id":"ee_root","goal":"Mid goal"}));
        write_json(&ees.join("ee_child.json"), &json!({"ee_id":"ee_child","name":"Child","template_name":"project","parent_ee_id":"ee_mid","goal":"Child goal"}));
        for idx in 0..60 {
            let id = format!("ee_other_{idx:03}");
            write_json(&ees.join(format!("{id}.json")), &json!({"ee_id":id,"name":id,"template_name":"project","parent_ee_id":"ee_root","goal":"Other"}));
        }
        let (capped, total, truncated) = inspect_ee_tree(&roots, "ee_mid");
        assert_eq!(total, 63);
        assert!(truncated);
        assert_eq!(capped.len(), MAX_EE_INSTANCES);
        let ids: BTreeSet<&str> = capped.iter().filter_map(|row| row["ee_id"].as_str()).collect();
        assert!(ids.contains("ee_root"));
        assert!(ids.contains("ee_mid"));
        assert!(ids.contains("ee_child"));
    }

    #[test]
    fn observe_runs_pulse_folds_phases_errors_index_and_gating() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = tmp.path().join("registry");
        let runs = tmp.path().join("runs");
        let ledger = tmp.path().join("ledger");
        std::fs::create_dir_all(&ledger).unwrap();
        let roots = CompositionRoots {
            registry_root: reg.clone(),
            ee_instances_root: tmp.path().join("ees"),
            run_records_root: runs.clone(),
            interface_private_state_root: tmp.path().join("var"),
        };

        write_iface(&reg, &json!({
            "id":"world_provider_p1","interface_type":"world","role":"provider","direction":"bidirectional",
            "source_names":["p"],"interface_driver_ref":"codex",
            "interface_driver_config":{"private_config":{"model":"gemini-4-neon","reasoning_effort":"low"}}
        }));
        write_iface(&reg, &json!({"id":"world_chat_peer","interface_type":"world","role":"user","direction":"bidirectional","source_names":["c"],"interface_driver_ref":"app_server"}));
        write_json(&reg.join("interface/world_chat_peer/octopus_os.json"), &json!({"presentation":{"name":"Monitor API"}}));
        write_iface(&reg, &json!({"id":"world_chat_arch","interface_type":"world","role":"user","direction":"bidirectional","source_names":["c"],"interface_driver_ref":"app_server"}));
        write_json(&reg.join("interface/world_chat_arch/octopus_os.json"), &json!({"archived":true,"presentation":{"name":"Old Task"}}));
        write_iface(&reg, &json!({"id":"world_watch_api","interface_type":"world","role":"sensor","direction":"inbound","source_names":["watch.event"],"interface_driver_ref":"world_watch_life"}));
        write_iface(&reg, &json!({"id":"system_door_ws","interface_type":"system","role":"system","direction":"bidirectional","source_names":["tool"],"interface_driver_ref":""}));

        let home = write_run(&runs, "run_home", "idle", &[("cp_prov", "world_provider_p1")], &[]);
        write_run(&runs, "run_tool_wait", "running", &[("cp_prov", "world_provider_p1"), ("cp_chat", "world_chat_peer"), ("cp_watch", "world_watch_api")], &[("cp_prov", "system_door_ws")]);
        write_run(&runs, "run_errored", "idle", &[("cp_prov", "world_provider_p1")], &[]);
        write_run(&runs, "run_archived", "idle", &[("cp_prov", "world_provider_p1"), ("cp_chat", "world_chat_arch")], &[]);
        write_run(&runs, "run_internal_writer", "idle", &[("cp_door", "system_door_ws")], &[]);

        let mut log = DurableLog::open(ledger.join("movements.jsonl")).unwrap();
        let mut blobs = DurableBlobs::open(ledger.join("blobs")).unwrap();
        let err_hash = blobs.try_put(br#"{"message":"upstream quota exceeded"}"#).unwrap();
        let ok_hash = blobs.try_put(b"{}").unwrap();

        log.try_append(MovementDraft {
            trace: TraceId("t1".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_p1")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_operator_task")),
            artifact: Some(artifact_with_hash("turn.completed", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_home", "world_provider_p1", DeliveryVerdict::Admitted { seq: 0 })),
            causal_floor: None,
        }).unwrap();
        log.try_append(MovementDraft {
            trace: TraceId("t2".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_p1")),
            to: Endpoint::Interface(InterfaceId::new("system_door_ws")),
            artifact: Some(artifact_with_hash("tool.call.request", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_tool_wait", "system_door_ws", DeliveryVerdict::Admitted { seq: 1 })),
            causal_floor: None,
        }).unwrap();
        log.try_append(MovementDraft {
            trace: TraceId("t3".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_p1")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_operator_task")),
            artifact: Some(artifact_with_hash("turn.error", err_hash)),
            invocation_metadata: Some(runtime_meta("run_errored", "world_provider_p1", DeliveryVerdict::Admitted { seq: 2 })),
            causal_floor: None,
        }).unwrap();

        let mut index = LedgerSqliteIndex::open(&ledger.join("index.sqlite")).unwrap();
        index.ingest_from(&log).unwrap();

        log.try_append(MovementDraft {
            trace: TraceId("t3b".into()),
            kind: MovementKind::DeliveryRefused,
            from: Endpoint::Interface(InterfaceId::new("world_provider_p1")),
            to: Endpoint::Interface(InterfaceId::new("system_door_ws")),
            artifact: Some(artifact_with_hash("tool.call.request", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_errored", "system_door_ws", DeliveryVerdict::Refused { reason: RefusedReason::Policy, detail: json!({}) })),
            causal_floor: None,
        }).unwrap();

        let (arrivals, _rx) = tokio::sync::mpsc::channel(1);
        let mut ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_p1"),
            tmp.path().into(),
            None,
            tmp.path().into(),
            ledger.join("movements.jsonl"),
            ledger.join("blobs"),
            ledger.join("index.sqlite"),
            Default::default(),
            arrivals,
            tokio_util::sync::CancellationToken::new(),
        );
        ctx.host_io_limits = Some(InterfaceHostIoLimits {
            max_control_frame_bytes: 1 << 20,
            max_manifest_bytes: 1 << 20,
            max_payload_bytes: 1 << 24,
            max_stderr_bytes: 1024,
        });

        let peers = BTreeMap::from([("system_service_input_peer".to_string(), Some("run_tool_wait".to_string()))]);
        let gated = observe_runs_pulse(&home, &peers, Some(&roots), Some(&ctx), false);
        assert!(gated.other_runs.is_none());
        assert_eq!(gated.source_activity["phase"], json!("idle"));
        assert_eq!(gated.source_model.as_deref(), Some("gemini-4-neon"));
        assert_eq!(gated.declared_peer_runs.len(), 1);
        let peer = &gated.declared_peer_runs[0];
        assert_eq!(peer["title"], json!("Monitor API"));
        assert_eq!(peer["external_inputs"], json!(["world_watch_api"]));
        assert_eq!(peer["activity"]["phase"], json!("awaiting_tool_result"));
        assert_eq!(peer["activity"]["pending_tool_calls"].as_array().unwrap().len(), 1);

        let full = observe_runs_pulse(&home, &peers, Some(&roots), Some(&ctx), true);
        assert_eq!(full.archived_runs_count, Some(1));
        let others = full.other_runs.unwrap();
        assert_eq!(others.len(), 1);
        assert_eq!(others[0]["run_id"], json!("run_errored"));
        assert_eq!(others[0]["activity"]["phase"], json!("error"));
        assert_eq!(others[0]["activity"]["last_error"]["excerpt"], json!("upstream quota exceeded"));
        assert_eq!(others[0]["activity"]["recent_refusal"]["reason"], json!("policy"));

        let unreadable = observe_runs_pulse(&home, &peers, Some(&roots), None, false);
        assert_eq!(unreadable.source_provider_activity, "not_observed");
        assert_eq!(unreadable.source_activity["phase"], json!("unreadable"));
        assert_eq!(unreadable.declared_peer_runs[0]["title"], json!("Monitor API"));
    }
}
