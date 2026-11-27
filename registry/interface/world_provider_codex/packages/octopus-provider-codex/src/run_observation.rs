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
use octopus_operator_chat_client::input_intake::InputIntakeConfig;
use octopus_operator_chat_client::run_composition::CompositionRoots;
use octopus_run_record::RunRecord;
use serde_json::{Value, json};

const MAX_TAIL_MOVEMENTS_PER_RUN: usize = 512;
const MAX_DECLARED_PEER_RUNS: usize = 24;
const MAX_OTHER_RUNS: usize = 24;
const MAX_EE_INSTANCES: usize = 48;
pub(crate) const MAX_LIST_RUNS_BYTES: usize = 16 * 1024;
const MAX_PEER_INTERFACES_BYTES: usize = 4 * 1024;
const MAX_DESCRIPTOR_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ERROR_BLOB_BYTES: u64 = 256 * 1024;
const MAX_EXCERPT_CHARS: usize = 300;
const MAX_TITLE_CHARS: usize = 150;
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
    pub declared_peer_interfaces: Vec<String>,
    pub declared_peer_interfaces_total: usize,
    pub declared_peer_runs: Vec<Value>,
    pub declared_peer_tiers: Vec<u8>,
    pub declared_peer_runs_total: usize,
    pub other_runs: Option<Vec<Value>>,
    pub other_tiers: Option<Vec<u8>>,
    pub other_runs_total: Option<usize>,
    pub archived_runs_count: Option<usize>,
    pub counts_by_phase: BTreeMap<String, usize>,
}

fn run_priority_tier(phase: &str, has_refusal: bool, has_external_inputs: bool, standing_writer: bool) -> u8 {
    match phase {
        "awaiting_tool_result" | "turn_open" => 0,
        "error" => 1,
        _ if has_refusal => 1,
        _ if standing_writer => 4,
        _ if has_external_inputs => 2,
        _ => 3,
    }
}

pub(crate) fn format_list_runs_response(
    source_run: Value,
    receive_interface_id: String,
    source_movement_seq: Option<u64>,
    observed: &ObservedRuns,
) -> Value {
    let mut interfaces: Vec<String> = Vec::new();
    let mut iface_bytes: usize = 2;
    for iface in &observed.declared_peer_interfaces {
        let cost = iface.len() + 3;
        if !interfaces.is_empty() && iface_bytes + cost > MAX_PEER_INTERFACES_BYTES {
            break;
        }
        interfaces.push(iface.clone());
        iface_bytes += cost;
    }

    let has_other_runs = observed.other_runs.is_some();
    let mut base = json!({
        "source_run": source_run,
        "declared_peer_interfaces": interfaces,
        "declared_peer_runs": [],
        "declared_peer_runs_total": observed.declared_peer_runs_total,
        "receive_interface_id": receive_interface_id,
        "source_movement_seq": source_movement_seq,
    });
    if !observed.counts_by_phase.is_empty() {
        base["counts_by_phase"] = json!(&observed.counts_by_phase);
    }
    if has_other_runs {
        base["other_runs"] = json!([]);
        if let Some(total) = observed.other_runs_total {
            base["other_runs_total"] = json!(total);
        }
    }
    if let Some(archived) = observed.archived_runs_count {
        base["archived_runs_count"] = json!(archived);
    }

    const RESERVED_OVERHEAD_BYTES: usize = 256;
    let base_bytes = serde_json::to_vec(&base).map(|b| b.len()).unwrap_or(MAX_LIST_RUNS_BYTES);
    let mut remaining = MAX_LIST_RUNS_BYTES.saturating_sub(base_bytes.saturating_add(RESERVED_OVERHEAD_BYTES));

    let mut selected_peers: Vec<(usize, Value)> = Vec::new();
    let mut selected_others: Vec<(usize, Value)> = Vec::new();
    let other_slice = observed.other_runs.as_deref().unwrap_or(&[]);
    let other_tiers = observed.other_tiers.as_deref().unwrap_or(&[]);

    // Direct connections own the budget before the discovery-only inventory.
    // Preserve activity priority within each group, not across their boundary.
    for (rows, tiers, selected) in [
        (observed.declared_peer_runs.as_slice(), observed.declared_peer_tiers.as_slice(), &mut selected_peers),
        (other_slice, other_tiers, &mut selected_others),
    ] {
        for tier in 0..=4u8 {
            for (idx, row) in rows.iter().enumerate() {
                if tiers.get(idx).copied().unwrap_or(3) != tier {
                    continue;
                }
                let cost = serde_json::to_vec(row).map(|b| b.len() + 1).unwrap_or(usize::MAX);
                if cost <= remaining {
                    remaining -= cost;
                    selected.push((idx, row.clone()));
                }
            }
        }
    }

    selected_peers.sort_by_key(|(idx, _)| *idx);
    selected_others.sort_by_key(|(idx, _)| *idx);

    let mut final_peers: Vec<Value> = selected_peers.into_iter().map(|(_, v)| v).collect();
    let mut final_others: Vec<Value> = selected_others.into_iter().map(|(_, v)| v).collect();

    loop {
        let mut candidate = base.clone();
        candidate["declared_peer_interfaces"] = json!(&interfaces);
        candidate["declared_peer_runs"] = json!(&final_peers);
        let peer_omitted = observed.declared_peer_runs_total.saturating_sub(final_peers.len());
        let iface_omitted = observed.declared_peer_interfaces_total.saturating_sub(interfaces.len());
        let mut other_omitted = 0usize;
        if has_other_runs {
            candidate["other_runs"] = json!(&final_others);
            other_omitted = observed
                .other_runs_total
                .unwrap_or(0)
                .saturating_sub(final_others.len());
        }
        if peer_omitted > 0 {
            candidate["declared_peer_runs_omitted"] = json!(peer_omitted);
        }
        if other_omitted > 0 {
            candidate["other_runs_omitted"] = json!(other_omitted);
        }
        if iface_omitted > 0 {
            candidate["declared_peer_interfaces_total"] = json!(observed.declared_peer_interfaces_total);
            candidate["declared_peer_interfaces_omitted"] = json!(iface_omitted);
        }
        if peer_omitted > 0 || other_omitted > 0 || iface_omitted > 0 {
            candidate["truncated"] = json!(true);
        }
        let encoded_len = serde_json::to_vec(&candidate).map(|b| b.len()).unwrap_or(0);
        if encoded_len <= MAX_LIST_RUNS_BYTES {
            return candidate;
        }
        if !final_others.is_empty() {
            final_others.pop();
        } else if !final_peers.is_empty() {
            final_peers.pop();
        } else if !interfaces.is_empty() {
            interfaces.pop();
        } else {
            return candidate;
        }
    }
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
    let mut bindings_by_id: BTreeMap<String, RunBindingsSummary> = BTreeMap::new();
    records_by_id.insert(source_record.id.as_str().to_string(), source_record.clone());
    let source_bindings = inspect_run_bindings(source_record, &mut descriptor_cache);

    let mut all_peer_run_ids = BTreeSet::new();
    let mut active_peer_entries: Vec<(String, String)> = Vec::new();
    let mut unresolved_peer_interfaces: Vec<String> = Vec::new();
    let mut archived_peer_count: usize = 0;

    for (interface_id, maybe_run_id) in declared_peers {
        let Some(run_id) = maybe_run_id else {
            unresolved_peer_interfaces.push(interface_id.clone());
            continue;
        };
        all_peer_run_ids.insert(run_id.clone());
        if let Some(root) = run_records_root.filter(|p| p.is_dir()) {
            if !records_by_id.contains_key(run_id) {
                if let Some(record) = load_run_record_file(&root.join(format!("{run_id}.json"))) {
                    let summary = inspect_run_bindings(&record, &mut descriptor_cache);
                    bindings_by_id.insert(run_id.clone(), summary);
                    records_by_id.insert(run_id.clone(), record);
                }
            }
            if !records_by_id.contains_key(run_id) {
                archived_peer_count += 1;
                continue;
            }
            let is_archived = if run_id == source_record.id.as_str() {
                source_bindings.archived
            } else {
                bindings_by_id.get(run_id).is_some_and(|summary| summary.archived)
            };
            if is_archived {
                archived_peer_count += 1;
                continue;
            }
        }
        active_peer_entries.push((interface_id.clone(), run_id.clone()));
    }

    let mut candidate_other_ids: Vec<String> = Vec::new();
    let mut archived_other_count: usize = 0;
    if include_other_runs {
        if let Some(root) = run_records_root.filter(|p| p.is_dir()) {
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
                if stem == source_record.id.as_str() || all_peer_run_ids.contains(stem) {
                    continue;
                }
                let Some(record) = load_run_record_file(&path) else {
                    continue;
                };
                let summary = inspect_run_bindings(&record, &mut descriptor_cache);
                if summary.archived {
                    archived_other_count += 1;
                    continue;
                }
                if summary.provider_interface_id.is_none() && summary.external_inputs.is_empty() {
                    continue;
                }
                let id = record.id.as_str().to_string();
                bindings_by_id.insert(id.clone(), summary);
                records_by_id.insert(id.clone(), record);
                candidate_other_ids.push(id);
            }
        }
    }

    let mut target_run_ids = BTreeSet::new();
    target_run_ids.insert(source_record.id.as_str().to_string());
    for (_, run_id) in &active_peer_entries {
        target_run_ids.insert(run_id.clone());
    }
    target_run_ids.extend(candidate_other_ids.iter().cloned());

    let mut intake_targets: BTreeMap<String, (String, IntakeRoute)> = BTreeMap::new();
    for (intake_iface, route) in &source_bindings.intake_interfaces {
        intake_targets.insert(
            intake_iface.clone(),
            (source_record.id.as_str().to_string(), route.clone()),
        );
    }
    for run_id in &target_run_ids {
        if let Some(bindings) = bindings_by_id.get(run_id) {
            for (intake_iface, route) in &bindings.intake_interfaces {
                intake_targets.insert(intake_iface.clone(), (run_id.clone(), route.clone()));
            }
        }
    }

    let loaded_movements = life_ctx
        .and_then(|ctx| load_tail_movements_by_run(ctx, &target_run_ids, &intake_targets));
    let blobs_dir = life_ctx.map(|ctx| ctx.blobs_dir.as_path());

    let (source_activity, source_phase) = build_run_activity_with_intake(
        Some(source_record),
        &source_bindings,
        loaded_movements
            .as_ref()
            .and_then(|loaded| loaded.by_run.get(source_record.id.as_str())),
        loaded_movements
            .as_ref()
            .and_then(|loaded| loaded.latest_intake_by_run.get(source_record.id.as_str())),
        loaded_movements.is_some(),
        blobs_dir,
        now_ms,
    );
    let source_provider_activity = if loaded_movements.is_some() {
        source_phase.to_string()
    } else {
        "not_observed".to_string()
    };

    let mut counts_by_phase: BTreeMap<String, usize> = BTreeMap::new();
    let mut counted_runs: BTreeSet<String> = BTreeSet::new();
    let empty_bindings = RunBindingsSummary::empty();

    let mut peer_rows: Vec<(u8, u64, String, String, Value)> = Vec::new();
    for (interface_id, run_id) in active_peer_entries {
        let record = records_by_id.get(&run_id);
        let bindings = if run_id == source_record.id.as_str() {
            &source_bindings
        } else {
            bindings_by_id.get(&run_id).unwrap_or(&empty_bindings)
        };
        let (activity, phase) = build_run_activity_with_intake(
            record,
            bindings,
            loaded_movements
                .as_ref()
                .and_then(|loaded| loaded.by_run.get(&run_id)),
            loaded_movements
                .as_ref()
                .and_then(|loaded| loaded.latest_intake_by_run.get(&run_id)),
            loaded_movements.is_some(),
            blobs_dir,
            now_ms,
        );
        if counted_runs.insert(run_id.clone()) {
            *counts_by_phase.entry(phase.to_string()).or_insert(0) += 1;
        }
        let sort_ts = activity
            .get("last_movement_at_unix_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let tier = run_priority_tier(
            phase,
            activity.get("recent_refusal").is_some(),
            !bindings.external_inputs.is_empty(),
            bindings.standing_writer,
        );
        let mut entry = json!({
            "interface_id": interface_id,
            "run_id": run_id,
            "activity": activity,
        });
        if let Some(rec) = record {
            attach_record_metadata(&mut entry, rec, bindings);
        }
        peer_rows.push((tier, sort_ts, run_id, interface_id, entry));
    }
    peer_rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| b.1.cmp(&a.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });

    let mut declared_peer_interfaces: Vec<String> = Vec::new();
    let mut seen_interfaces: BTreeSet<String> = BTreeSet::new();
    for (_, _, _, iface_id, _) in &peer_rows {
        if seen_interfaces.insert(iface_id.clone()) {
            declared_peer_interfaces.push(iface_id.clone());
        }
    }
    for iface_id in unresolved_peer_interfaces {
        if seen_interfaces.insert(iface_id.clone()) {
            declared_peer_interfaces.push(iface_id);
        }
    }
    let declared_peer_interfaces_total = declared_peer_interfaces.len();
    let declared_peer_runs_total = peer_rows.len();
    let mut declared_peer_runs = Vec::with_capacity(declared_peer_runs_total.min(MAX_DECLARED_PEER_RUNS));
    let mut declared_peer_tiers = Vec::with_capacity(declared_peer_runs_total.min(MAX_DECLARED_PEER_RUNS));
    for (tier, _, _, _, entry) in peer_rows.into_iter().take(MAX_DECLARED_PEER_RUNS) {
        declared_peer_tiers.push(tier);
        declared_peer_runs.push(entry);
    }

    let (other_runs, other_tiers, other_runs_total, archived_runs_count) = if include_other_runs {
        let mut rows: Vec<(u8, u64, String, Value)> = Vec::new();
        for run_id in candidate_other_ids {
            let Some(record) = records_by_id.get(&run_id) else {
                continue;
            };
            let bindings = bindings_by_id.get(&run_id).unwrap_or(&empty_bindings);
            let (activity, phase) = build_run_activity_with_intake(
                Some(record),
                bindings,
                loaded_movements
                    .as_ref()
                    .and_then(|loaded| loaded.by_run.get(&run_id)),
                loaded_movements
                    .as_ref()
                    .and_then(|loaded| loaded.latest_intake_by_run.get(&run_id)),
                loaded_movements.is_some(),
                blobs_dir,
                now_ms,
            );
            if counted_runs.insert(run_id.clone()) {
                *counts_by_phase.entry(phase.to_string()).or_insert(0) += 1;
            }
            let sort_ts = activity
                .get("last_movement_at_unix_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let tier = run_priority_tier(
                phase,
                activity.get("recent_refusal").is_some(),
                !bindings.external_inputs.is_empty(),
                bindings.standing_writer,
            );
            let mut entry = json!({
                "run_id": run_id,
                "activity": activity,
            });
            attach_record_metadata(&mut entry, record, bindings);
            rows.push((tier, sort_ts, run_id, entry));
        }
        rows.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        let total = rows.len();
        let mut trimmed = Vec::with_capacity(total.min(MAX_OTHER_RUNS));
        let mut tiers = Vec::with_capacity(total.min(MAX_OTHER_RUNS));
        for (tier, _, _, val) in rows.into_iter().take(MAX_OTHER_RUNS) {
            tiers.push(tier);
            trimmed.push(val);
        }
        (
            Some(trimmed),
            Some(tiers),
            Some(total),
            Some(archived_peer_count + archived_other_count),
        )
    } else {
        (
            None,
            None,
            None,
            (archived_peer_count > 0).then_some(archived_peer_count),
        )
    };

    ObservedRuns {
        source_activity,
        source_provider_activity,
        source_title: source_bindings.title,
        source_provider_interface_id: source_bindings.provider_interface_id,
        source_model: source_bindings.model,
        source_reasoning_effort: source_bindings.reasoning_effort,
        source_external_inputs: source_bindings.external_inputs,
        declared_peer_interfaces,
        declared_peer_interfaces_total,
        declared_peer_runs,
        declared_peer_tiers,
        declared_peer_runs_total,
        other_runs,
        other_tiers,
        other_runs_total,
        archived_runs_count,
        counts_by_phase,
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

#[derive(Clone)]
struct IntakeRoute {
    writer_door: String,
    writer_result_label: String,
    input_label: String,
}

#[derive(Clone)]
struct IntakeWakeup {
    seq: u64,
    at_unix_ms: u64,
    label: Option<String>,
}

struct LoadedRunMovements {
    by_run: BTreeMap<String, VecDeque<Movement>>,
    latest_intake_by_run: BTreeMap<String, BTreeMap<String, IntakeWakeup>>,
}

struct RunBindingsSummary {
    standing_writer: bool,
    title: Option<String>,
    archived: bool,
    provider_interface_id: Option<String>,
    provider_interfaces: BTreeSet<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    external_inputs: Vec<String>,
    world_inbound_interfaces: BTreeSet<String>,
    door_interfaces: BTreeSet<String>,
    intake_interfaces: BTreeMap<String, IntakeRoute>,
}

impl RunBindingsSummary {
    fn empty() -> Self {
        Self {
            standing_writer: false,
            title: None,
            archived: false,
            provider_interface_id: None,
            provider_interfaces: BTreeSet::new(),
            model: None,
            reasoning_effort: None,
            external_inputs: Vec::new(),
            world_inbound_interfaces: BTreeSet::new(),
            door_interfaces: BTreeSet::new(),
            intake_interfaces: BTreeMap::new(),
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
    summary.standing_writer = !octopus_run_authoring::standing_writer_checkpoints(record).is_empty();
    if record.active_checkpoints.is_empty() {
        summary.archived = true;
    }
    for (iface, state) in &record.interface_state {
        if let Some(declaration) = state.get("input_intake")
            && let Ok(config) = serde_json::from_value::<InputIntakeConfig>(declaration.clone())
            && config.validate(iface).is_ok()
            && config.run == record.id.as_str()
        {
            summary.intake_interfaces.insert(
                iface.as_str().to_string(),
                IntakeRoute {
                    writer_door: config.writer.door.as_str().to_string(),
                    writer_result_label: config.writer.result_label,
                    input_label: config.input_label,
                },
            );
        }
    }
    let mut external_inputs = BTreeSet::new();

    for iface_id in &all_interfaces {
        let Some(descriptor) = cache.descriptor(iface_id) else {
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
                            .map(|s| clip_single_line(s, MAX_TITLE_CHARS));
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

#[cfg(test)]
fn build_run_activity(
    record: Option<&RunRecord>,
    bindings: &RunBindingsSummary,
    movements: Option<&VecDeque<Movement>>,
    ledger_readable: bool,
    blobs_dir: Option<&Path>,
    now_ms: u64,
) -> (Value, &'static str) {
    build_run_activity_with_intake(record, bindings, movements, None, ledger_readable, blobs_dir, now_ms)
}

fn build_run_activity_with_intake(
    record: Option<&RunRecord>,
    bindings: &RunBindingsSummary,
    movements: Option<&VecDeque<Movement>>,
    latest_intakes: Option<&BTreeMap<String, IntakeWakeup>>,
    ledger_readable: bool,
    blobs_dir: Option<&Path>,
    now_ms: u64,
) -> (Value, &'static str) {
    if !ledger_readable {
        return (json!({ "phase": "unreadable" }), "unreadable");
    }

    let standing_writer_waits = record
        .map(octopus_run_authoring::standing_writer_checkpoints)
        .unwrap_or_default();
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
    let mut last_intake_delivery_seq: BTreeMap<String, u64> = BTreeMap::new();

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

        if let Some(route) = bindings.intake_interfaces.get(from_id)
            && label.as_deref() == Some(route.input_label.as_str())
            && matches!(
                movement.kind,
                MovementKind::InterfaceArrival | MovementKind::InterfaceOutbound
            )
        {
            last_intake_delivery_seq.insert(from_id.to_string(), seq);
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
                    if standing_writer_waits.is_empty() {
                        turn_open = true;
                    }
                } else if to_provider
                    && standing_writer_waits.is_empty()
                    && label.as_deref() != Some("interface.context.updated")
                {
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
                } else if bindings.world_inbound_interfaces.contains(from_id)
                    && label.as_deref() != Some("interface.context.updated")
                {
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
            if checkpoint.required_verified_artifacts.is_empty()
                || standing_writer_waits.contains(&checkpoint.id)
            {
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

    // A tail-derived open flag can outlive the provider's recorded idle
    // transition. Queued context is not liveness evidence; preserve actual
    // pending calls below and let a running record keep its open phase.
    if record.is_some_and(|rec| rec.status.as_str() == Some("idle"))
        && last_artifact_label.as_deref() == Some("interface.context.updated")
    {
        turn_open = false;
    }
    let tail_full = rows.len() == MAX_TAIL_MOVEMENTS_PER_RUN;
    let tail_min_seq = rows.front().map(|m| m.seq.0);
    let pending_intake = latest_intakes.and_then(|intakes| {
        intakes
            .iter()
            .filter(|(intake_iface, wakeup)| {
                last_intake_delivery_seq
                    .get(intake_iface.as_str())
                    .is_none_or(|delivered| *delivered <= wakeup.seq)
                    && !(tail_full && tail_min_seq.is_some_and(|min| wakeup.seq < min))
            })
            .max_by_key(|(_, wakeup)| wakeup.seq)
            .map(|(_, wakeup)| wakeup)
    });
    if let Some(wakeup) = pending_intake {
        turn_open = true;
        if last_error.as_ref().is_some_and(|(seq, ..)| *seq < wakeup.seq) {
            last_error = None;
        }
        if last_movement_seq.is_none_or(|seq| wakeup.seq > seq) {
            last_movement_seq = Some(wakeup.seq);
            last_movement_at_unix_ms = Some(wakeup.at_unix_ms);
            last_movement_kind = Some("interface_outbound");
            if wakeup.label.is_some() {
                last_artifact_label = wakeup.label.clone();
            }
        }
    }
    let record_running = record.is_some_and(|rec| rec.status.as_str() == Some("running"));
    let phase = if last_movement_seq.is_none() {
        if record_running {
            "turn_open"
        } else if !standing_writer_waits.is_empty()
            && record.is_some_and(|rec| rec.status.as_str() == Some("idle"))
        {
            "idle"
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

    let mut activity = serde_json::Map::new();
    activity.insert("phase".into(), json!(phase));
    if let Some(seq) = last_movement_seq {
        activity.insert("last_movement_seq".into(), json!(seq));
    }
    if let Some(ts) = last_movement_at_unix_ms {
        activity.insert("last_movement_at_unix_ms".into(), json!(ts));
    }
    if let Some(secs) = seconds_since_last_movement {
        activity.insert("seconds_since_last_movement".into(), json!(secs));
    }
    if let Some(kind) = last_movement_kind {
        activity.insert("last_movement_kind".into(), json!(kind));
    }
    if let Some(label) = last_artifact_label {
        activity.insert("last_artifact_label".into(), json!(label));
    }
    if let Some(wakeup) = pending_intake {
        activity.insert("pending_intake_seq".into(), json!(wakeup.seq));
        activity.insert("pending_intake_at_unix_ms".into(), json!(wakeup.at_unix_ms));
    }
    if let Some(ts) = last_provider_activity_at_unix_ms {
        activity.insert("last_provider_activity_at_unix_ms".into(), json!(ts));
    }
    if let Some(label) = last_provider_artifact_label {
        activity.insert("last_provider_artifact_label".into(), json!(label));
    }
    if !pending_tool_calls.is_empty() {
        activity.insert("pending_tool_calls".into(), json!(pending_tool_calls));
    }
    if let Some(refusal) = recent_refusal {
        activity.insert("recent_refusal".into(), refusal);
    }
    if let Some(err) = last_error_json {
        activity.insert("last_error".into(), err);
    }

    (Value::Object(activity), phase)
}

fn is_admitted_intake_wakeup(movement: &Movement, intake_iface: &str, route: &IntakeRoute) -> bool {
    if movement.kind != MovementKind::InterfaceOutbound
        || endpoint_interface(&movement.to) != intake_iface
    {
        return false;
    }
    let label = movement.artifact.as_ref().map(|a| artifact_label(&a.id));
    if label.as_deref() == Some("interface.context.updated") {
        return false;
    }
    if endpoint_interface(&movement.from) == route.writer_door
        && label.as_deref() == Some(route.writer_result_label.as_str())
    {
        return false;
    }
    !matches!(
        movement
            .invocation_metadata
            .as_ref()
            .and_then(MovementMetadata::delivery_verdict),
        Some(octopus_ledger::DeliveryVerdict::Refused { .. })
    )
}

fn record_intake_wakeup(
    latest_intake_by_run: &mut BTreeMap<String, BTreeMap<String, IntakeWakeup>>,
    run_id: &str,
    intake_iface: &str,
    movement: &Movement,
) {
    let entry = latest_intake_by_run
        .entry(run_id.to_string())
        .or_default();
    if entry
        .get(intake_iface)
        .is_some_and(|existing| existing.seq >= movement.seq.0)
    {
        return;
    }
    entry.insert(
        intake_iface.to_string(),
        IntakeWakeup {
            seq: movement.seq.0,
            at_unix_ms: movement.recorded_at_unix_ms,
            label: movement.artifact.as_ref().map(|a| artifact_label(&a.id)),
        },
    );
}

fn load_tail_movements_by_run(
    ctx: &InterfaceLiftContext,
    target_runs: &BTreeSet<String>,
    intake_targets: &BTreeMap<String, (String, IntakeRoute)>,
) -> Option<LoadedRunMovements> {
    if target_runs.is_empty() && intake_targets.is_empty() {
        return Some(LoadedRunMovements {
            by_run: BTreeMap::new(),
            latest_intake_by_run: BTreeMap::new(),
        });
    }
    let mut file = match File::open(&ctx.movements_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Some(LoadedRunMovements {
                by_run: BTreeMap::new(),
                latest_intake_by_run: BTreeMap::new(),
            });
        }
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
        intake_targets,
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
        intake_targets,
        LoadedRunMovements {
            by_run: BTreeMap::new(),
            latest_intake_by_run: BTreeMap::new(),
        },
    )
}

fn try_load_via_sqlite_index(
    ctx: &InterfaceLiftContext,
    file: &mut File,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    target_runs: &BTreeSet<String>,
    intake_targets: &BTreeMap<String, (String, IntakeRoute)>,
) -> Option<LoadedRunMovements> {
    let reader = LedgerSqliteReader::open(&ctx.ledger_index_path).ok()?;
    let progress = reader.progress().ok()?;
    if progress.durable_end_offset > snapshot_len {
        return None;
    }

    let mut by_run: BTreeMap<String, VecDeque<Movement>> = BTreeMap::new();
    for run_id in target_runs {
        let rows = reader
            .tail_movement_rows_touching_run(run_id, 0, progress.watermark, MAX_TAIL_MOVEMENTS_PER_RUN)
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
            if movement.seq.0 != range.seq || movement.run().map(|r| r.as_str()) != Some(run_id.as_str()) {
                return None;
            }
            queue.push_back(movement);
        }
        if !queue.is_empty() {
            by_run.insert(run_id.clone(), queue);
        }
    }

    let mut latest_intake_by_run: BTreeMap<String, BTreeMap<String, IntakeWakeup>> = BTreeMap::new();
    for (intake_iface, (run_id, route)) in intake_targets {
        let rows = reader
            .movement_rows_for_runtime_interface(intake_iface, 0, progress.watermark)
            .ok()??;
        for range in rows.into_iter().rev() {
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
            if is_admitted_intake_wakeup(&movement, intake_iface, route) {
                record_intake_wakeup(&mut latest_intake_by_run, run_id, intake_iface, &movement);
                break;
            }
        }
    }

    let loaded = LoadedRunMovements {
        by_run,
        latest_intake_by_run,
    };
    if progress.durable_end_offset < snapshot_len {
        return scan_movements_into_tails(
            file,
            progress.durable_end_offset,
            snapshot_len,
            max_row_bytes,
            decoder,
            target_runs,
            intake_targets,
            loaded,
        );
    }

    Some(loaded)
}

fn scan_movements_into_tails(
    file: &mut File,
    start_offset: u64,
    snapshot_len: u64,
    max_row_bytes: u64,
    decoder: &DurableMovementDecoder,
    target_runs: &BTreeSet<String>,
    intake_targets: &BTreeMap<String, (String, IntakeRoute)>,
    mut loaded: LoadedRunMovements,
) -> Option<LoadedRunMovements> {
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
        let to_id = endpoint_interface(&movement.to);
        if let Some((run_id, route)) = intake_targets.get(to_id)
            && is_admitted_intake_wakeup(&movement, to_id, route)
        {
            record_intake_wakeup(&mut loaded.latest_intake_by_run, run_id, to_id, &movement);
        }
        if let Some(run_id) = movement.run() {
            let key = run_id.as_str();
            if target_runs.contains(key) {
                let queue = loaded.by_run.entry(key.to_string()).or_default();
                if queue.len() == MAX_TAIL_MOVEMENTS_PER_RUN {
                    queue.pop_front();
                }
                queue.push_back(movement);
            }
        }
        line_index += 1;
    }

    Some(loaded)
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
    fn queued_context_is_not_a_provider_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let record = write_run(tmp.path(), "run_idle", "idle", &[], &[]);
        let mut bindings = RunBindingsSummary::empty();
        bindings.provider_interfaces.insert("provider".into());
        bindings.world_inbound_interfaces.insert("peer".into());
        let row = |kind, from: &str, to: &str, label: &str| Movement {
            seq: octopus_ledger::Seq(1), lamport: octopus_ledger::Lamport(2), recorded_at_unix_ms: 1,
            trace: TraceId("context".into()), kind,
            from: Endpoint::Interface(from.into()), to: Endpoint::Interface(to.into()),
            artifact: Some(artifact_with_hash(label, ContentHash::of_bytes(b"{}"))),
            invocation_metadata: None,
        };
        for movement in [
            row(MovementKind::InterfaceOutbound, "peer", "provider", "interface.context.updated"),
            row(MovementKind::InterfaceArrival, "peer", "worker", "interface.context.updated"),
        ] {
            let rows = VecDeque::from([movement]);
            let (activity, phase) = build_run_activity(Some(&record), &bindings, Some(&rows), true, None, 5_189_000);
            assert_eq!(phase, "idle");
            assert_eq!(activity["last_artifact_label"], "interface.context.updated");
        }
        let rows = VecDeque::from([
            row(MovementKind::InterfaceArrival, "provider", "worker", "turn.started"),
            row(MovementKind::InterfaceOutbound, "peer", "provider", "interface.context.updated"),
        ]);
        assert_eq!(build_run_activity(Some(&record), &bindings, Some(&rows), true, None, 2).1, "idle");
        let queued = VecDeque::from([
            row(MovementKind::InterfaceArrival, "provider", "worker", "turn.started"),
            row(MovementKind::InterfaceArrival, "provider", "worker", "interface.context.updated"),
        ]);
        assert_eq!(build_run_activity(Some(&record), &bindings, Some(&queued), true, None, 5_189_000).1, "idle");
        let mut running = record.clone();
        running.status = json!("running");
        assert_eq!(build_run_activity(Some(&running), &bindings, Some(&queued), true, None, 5_189_000).1, "turn_open");
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
        let reader = LedgerSqliteReader::open(&ledger.join("index.sqlite")).unwrap();
        let all = reader.movement_rows_touching_run("run_errored", 0, 3).unwrap().unwrap();
        let tail = reader.tail_movement_rows_touching_run("run_errored", 0, 3, 1).unwrap().unwrap();
        assert_eq!(all.last().map(|r| r.seq), tail.last().map(|r| r.seq));
        assert!(reader.tail_movement_rows_touching_run("run_errored", 0, 3, 0).unwrap().unwrap().is_empty());

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

        let indexed_activity = gated.source_activity.clone();
        let original_index = ctx.ledger_index_path.clone();
        ctx.ledger_index_path = ledger.join("absent.sqlite");
        let fallback = observe_runs_pulse(&home, &peers, Some(&roots), Some(&ctx), false);
        assert_eq!(fallback.source_activity["phase"], indexed_activity["phase"]);
        assert_eq!(fallback.source_activity["last_movement_seq"], indexed_activity["last_movement_seq"]);
        assert_eq!(fallback.declared_peer_runs[0]["activity"]["phase"], gated.declared_peer_runs[0]["activity"]["phase"]);
        ctx.ledger_index_path = original_index;
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

    #[test]
    fn list_runs_stays_bounded_valid_json_and_prioritizes_active_runs() {
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
            "interface_driver_config":{"private_config":{"model":"gemini-4-neon","reasoning_effort":"medium"}}
        }));
        write_iface(&reg, &json!({"id":"world_watch_s1","interface_type":"world","role":"sensor","direction":"inbound","source_names":["watch.event"],"interface_driver_ref":"world_watch_life"}));
        write_iface(&reg, &json!({"id":"system_door_ws","interface_type":"system","role":"system","direction":"bidirectional","source_names":["tool"],"interface_driver_ref":""}));
        write_iface(&reg, &json!({"id":"world_chat_arch","interface_type":"world","role":"user","direction":"bidirectional","source_names":["c"],"interface_driver_ref":"app_server"}));
        write_json(&reg.join("interface/world_chat_arch/octopus_os.json"), &json!({"archived":true,"presentation":{"name":"Archived Peer"}}));

        let home = write_run(&runs, "run_home", "idle", &[("cp_prov", "world_provider_p1")], &[]);
        let mut peers = BTreeMap::new();

        for i in 0..170 {
            let run_id = format!("run_peer_idle_{i:03}");
            let iface_id = format!("system_service_input_{run_id}");
            write_run(&runs, &run_id, "idle", &[("cp_prov", "world_provider_p1")], &[]);
            peers.insert(iface_id, Some(run_id));
        }
        write_run(&runs, "run_peer_archived", "idle", &[("cp_prov", "world_provider_p1"), ("cp_chat", "world_chat_arch")], &[]);
        peers.insert("system_service_input_archived".into(), Some("run_peer_archived".into()));
        write_run(&runs, "run_peer_retired", "idle", &[], &[]);
        peers.insert("system_service_input_retired".into(), Some("run_peer_retired".into()));
        write_run(&runs, "run_peer_active", "running", &[("cp_prov", "world_provider_p1")], &[("cp_prov", "system_door_ws")]);
        peers.insert("system_service_input_zzz_active".into(), Some("run_peer_active".into()));
        write_run(&runs, "run_peer_error", "idle", &[("cp_prov", "world_provider_p1")], &[]);
        peers.insert("system_service_input_zzz_error".into(), Some("run_peer_error".into()));
        write_run(&runs, "run_peer_standing", "idle", &[("cp_prov", "world_provider_p1"), ("cp_w", "world_watch_s1")], &[]);
        peers.insert("system_service_input_zzz_standing".into(), Some("run_peer_standing".into()));

        for i in 0..170 {
            let run_id = format!("run_other_idle_{i:03}");
            write_run(&runs, &run_id, "idle", &[("cp_prov", "world_provider_p1")], &[]);
        }
        write_run(&runs, "run_other_active", "running", &[("cp_prov", "world_provider_p1")], &[]);

        let mut log = DurableLog::open(ledger.join("movements.jsonl")).unwrap();
        let mut blobs = DurableBlobs::open(ledger.join("blobs")).unwrap();
        let ok_hash = blobs.try_put(b"{}").unwrap();
        let err_hash = blobs.try_put(br#"{"message":"tool failed"}"#).unwrap();

        log.try_append(MovementDraft {
            trace: TraceId("t_act".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_p1")),
            to: Endpoint::Interface(InterfaceId::new("system_door_ws")),
            artifact: Some(artifact_with_hash("tool.call.request", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_peer_active", "system_door_ws", DeliveryVerdict::Admitted { seq: 0 })),
            causal_floor: None,
        }).unwrap();
        log.try_append(MovementDraft {
            trace: TraceId("t_err".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_p1")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_operator_task")),
            artifact: Some(artifact_with_hash("turn.error", err_hash)),
            invocation_metadata: Some(runtime_meta("run_peer_error", "world_provider_p1", DeliveryVerdict::Admitted { seq: 1 })),
            causal_floor: None,
        }).unwrap();
        log.try_append(MovementDraft {
            trace: TraceId("t_oth".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("world_provider_p1")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_operator_task")),
            artifact: Some(artifact_with_hash("turn.started", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_other_active", "world_provider_p1", DeliveryVerdict::Admitted { seq: 2 })),
            causal_floor: None,
        }).unwrap();

        let (arrivals, _rx) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
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

        let observed = observe_runs_pulse(&home, &peers, Some(&roots), Some(&ctx), true);
        let response = format_list_runs_response(
            json!({"run_id": "run_home", "recorded_status": "idle", "activity": observed.source_activity}),
            "system_worker_operator_task".into(),
            Some(9),
            &observed,
        );
        let encoded = serde_json::to_vec(&response).unwrap();
        assert!(encoded.len() <= MAX_LIST_RUNS_BYTES, "encoded size {} exceeded {}", encoded.len(), MAX_LIST_RUNS_BYTES);
        let decoded: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded["truncated"], json!(true));
        assert_eq!(decoded["archived_runs_count"], json!(2));
        assert_eq!(decoded["declared_peer_runs_total"], json!(173));
        assert_eq!(decoded["other_runs_total"], json!(171));
        assert_eq!(decoded["counts_by_phase"]["awaiting_tool_result"], json!(1));
        assert_eq!(decoded["counts_by_phase"]["turn_open"], json!(1));
        assert_eq!(decoded["counts_by_phase"]["error"], json!(1));

        let peer_rows = decoded["declared_peer_runs"].as_array().unwrap();
        assert_eq!(peer_rows[0]["run_id"], json!("run_peer_active"));
        assert_eq!(peer_rows[1]["run_id"], json!("run_peer_error"));
        assert_eq!(peer_rows[2]["run_id"], json!("run_peer_standing"));
        assert!(!peer_rows.iter().any(|r| r["run_id"] == "run_peer_archived" || r["run_id"] == "run_peer_retired"));

        let other_rows = decoded["other_runs"].as_array().unwrap();
        assert_eq!(other_rows[0]["run_id"], json!("run_other_active"));
    }

    fn write_separate_writer(runs: &Path, id: &str) -> RunRecord {
        let record = write_run(runs, id, "idle",
            &[("call", "system_authoring_source"), ("result", "system_door_ws")],
            &[("call", "system_door_ws"), ("result", "world_provider_p1")]);
        let mut value = serde_json::to_value(record).unwrap();
        value["interface_state"] = json!({"world_provider_p1": {"file_writer_target": {
            "run": "run_owner", "provider": "world_provider_p1",
            "source": "system_authoring_source", "door": "system_door_ws", "generation": 0
        }}});
        value["checkpoints"][0]["required_verified_artifacts"] = json!([{
            "artifact_label": "tool.call", "verifier": "tool_call_allowed",
            "mark": "tool.call.allowed", "params": {}
        }]);
        write_json(&runs.join(format!("{id}.json")), &value);
        RunRecord::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    #[test]
    fn separate_writer_standing_routes_do_not_imply_a_pending_call() {
        let tmp = tempfile::tempdir().unwrap();
        let record = write_separate_writer(tmp.path(), "arbitrary_companion");
        assert_eq!(run_priority_tier("idle", false, true, true), 4);
        assert_eq!(run_priority_tier("turn_open", false, true, true), 0);
        assert_eq!(run_priority_tier("idle", true, true, true), 1);
        let mut bindings = RunBindingsSummary::empty();
        bindings.door_interfaces = BTreeSet::from(["system_authoring_source".into(), "system_door_ws".into()]);
        bindings.provider_interfaces.insert("world_provider_p1".into());
        let row = |kind, from: &str, to: &str, label: &str| Movement {
            seq: octopus_ledger::Seq(1), lamport: octopus_ledger::Lamport(2), recorded_at_unix_ms: 1,
            trace: TraceId("t".into()), kind,
            from: Endpoint::Interface(InterfaceId::new(from)), to: Endpoint::Interface(InterfaceId::new(to)),
            artifact: Some(artifact_with_hash(label, ContentHash::of_bytes(b"{}"))),
            invocation_metadata: None,
        };
        assert_eq!(build_run_activity(Some(&record), &bindings, None, true, None, 1000).1, "idle");
        let done = row(MovementKind::InterfaceArrival, "system_door_ws", "world_provider_p1", "tool.result");
        let (activity, phase) = build_run_activity(Some(&record), &bindings, Some(&VecDeque::from([done.clone()])), true, None, 1000);
        assert_eq!(phase, "idle");
        assert!(activity.get("pending_tool_calls").is_none());
        let emitted = row(MovementKind::InterfaceOutbound, "system_authoring_source", "system_door_ws", "tool.call");
        let forwarded = row(MovementKind::InterfaceOutbound, "system_door_ws", "world_provider_p1", "tool.result");
        assert_eq!(build_run_activity(Some(&record), &bindings, Some(&VecDeque::from([emitted.clone(), done.clone(), forwarded])), true, None, 1000).1, "idle");
        let started = row(MovementKind::InterfaceArrival, "world_provider_p1", "worker", "turn.started");
        assert_eq!(build_run_activity(Some(&record), &bindings, Some(&VecDeque::from([started])), true, None, 1000).1, "turn_open");
        let (activity, phase) = build_run_activity(Some(&record), &bindings, Some(&VecDeque::from([emitted])), true, None, 1000);
        assert_eq!(phase, "awaiting_tool_result");
        assert_eq!(activity["pending_tool_calls"].as_array().unwrap().len(), 1);
        let mut running = record.clone();
        running.status = json!("running");
        assert_eq!(build_run_activity(Some(&running), &bindings, Some(&VecDeque::from([done.clone()])), true, None, 1000).1, "turn_open");
        let err = row(MovementKind::InterfaceArrival, "world_provider_p1", "worker", "turn.error");
        assert_eq!(build_run_activity(Some(&record), &bindings, Some(&VecDeque::from([err])), true, None, 1000).1, "error");
        let refused = row(MovementKind::DeliveryRefused, "system_authoring_source", "system_door_ws", "tool.call");
        let (activity, phase) = build_run_activity(Some(&record), &bindings, Some(&VecDeque::from([refused])), true, None, 1000);
        assert_eq!(run_priority_tier(phase, activity.get("recent_refusal").is_some(), false, true), 1);
        let mut ordinary = record.clone();
        ordinary.interface_state.clear();
        assert_eq!(build_run_activity(Some(&ordinary), &bindings, Some(&VecDeque::from([done])), true, None, 1000).1, "awaiting_tool_result");
        let mut malformed = serde_json::to_value(&record).unwrap();
        malformed["interface_state"]["world_provider_p1"]["file_writer_target"]["run"] = json!(record.id);
        let malformed = RunRecord::from_bytes(&serde_json::to_vec(&malformed).unwrap()).unwrap();
        assert!(octopus_run_authoring::standing_writer_checkpoints(&malformed).is_empty());
        let mut incomplete = record.clone();
        incomplete.active_checkpoints.pop();
        assert!(octopus_run_authoring::standing_writer_checkpoints(&incomplete).is_empty());
    }

    #[test]
    fn peer_budget_is_protected_from_busy_discovery_inventory() {
        let tmp = tempfile::tempdir().unwrap();
        let (reg, runs, ledger) = (tmp.path().join("registry"), tmp.path().join("runs"), tmp.path().join("ledger"));
        std::fs::create_dir_all(&ledger).unwrap();
        write_iface(&reg, &json!({"id":"world_provider_p1","interface_type":"world","role":"provider","direction":"bidirectional","source_names":["p"],"interface_driver_ref":"codex"}));
        for id in ["system_door_ws", "system_authoring_source"] {
            write_iface(&reg, &json!({"id":id,"interface_type":"system","role":"system","direction":"bidirectional","source_names":["tool"],"interface_driver_ref":""}));
        }
        let home = write_run(&runs, "home", "idle", &[("provider", "world_provider_p1")], &[]);
        let mut peers = BTreeMap::new();
        for i in 0..22 {
            let id = format!("peer_{i:02}");
            write_run(&runs, &id, "idle", &[("provider", "world_provider_p1")], &[]);
            peers.insert(format!("input_{i:02}"), Some(id));
        }
        write_run(&runs, "ordinary_unconnected", "running", &[("provider", "world_provider_p1")], &[]);
        let mut log = DurableLog::open(ledger.join("movements.jsonl")).unwrap();
        for i in 0..55 {
            let id = format!("companion_{i:02}");
            write_separate_writer(&runs, &id);
            log.try_append(MovementDraft {
                trace: TraceId("t".into()), kind: MovementKind::InterfaceArrival,
                from: Endpoint::Interface(InterfaceId::new("system_door_ws")),
                to: Endpoint::Interface(InterfaceId::new("world_provider_p1")),
                artifact: Some(artifact_with_hash("tool.result", ContentHash::of_bytes(b"{}"))),
                invocation_metadata: Some(runtime_meta(&id, "world_provider_p1", DeliveryVerdict::Admitted { seq: i })),
                causal_floor: None,
            }).unwrap();
        }
        let (arrivals, _rx) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_p1"), tmp.path().into(), None, tmp.path().into(),
            ledger.join("movements.jsonl"), ledger.join("blobs"), ledger.join("index.sqlite"),
            Default::default(), arrivals, tokio_util::sync::CancellationToken::new(),
        );
        let roots = CompositionRoots {
            registry_root: reg, run_records_root: runs,
            ee_instances_root: tmp.path().join("ees"), interface_private_state_root: tmp.path().join("var"),
        };
        let observed = observe_runs_pulse(&home, &peers, Some(&roots), Some(&ctx), true);
        assert_eq!(observed.declared_peer_runs_total, 22);
        assert_eq!(observed.other_runs_total, Some(56));
        assert_eq!(observed.other_runs.as_ref().unwrap()[0]["run_id"], "ordinary_unconnected");
        assert_eq!(observed.other_tiers.as_ref().unwrap()[0], 0);
        assert!(observed.other_tiers.as_ref().unwrap()[1..].iter().all(|t| *t == 4));
        assert_eq!(observed.counts_by_phase.get("awaiting_tool_result"), None);
        assert_eq!(observed.counts_by_phase.get("idle"), Some(&55));
        let response = format_list_runs_response(json!({"run_id":"home"}), "worker".into(), None, &observed);
        assert_eq!(response["declared_peer_runs"].as_array().unwrap().len(), 22);
        assert!(response.get("declared_peer_runs_omitted").is_none());
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_LIST_RUNS_BYTES);
        assert_eq!(response["other_runs_total"], json!(56));
        assert_eq!(response["other_runs_omitted"].as_u64().unwrap() as usize + response["other_runs"].as_array().unwrap().len(), 56);
        let mut busy = observed;
        busy.other_tiers = Some(vec![0; busy.other_runs.as_ref().unwrap().len()]);
        for row in busy.other_runs.as_mut().unwrap() { row["padding"] = json!("x".repeat(1600)); }
        let response = format_list_runs_response(json!({"run_id":"home"}), "worker".into(), None, &busy);
        assert_eq!(response["declared_peer_runs"].as_array().unwrap().len(), 22);
        assert_eq!(response["truncated"], json!(true));
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_LIST_RUNS_BYTES);
        busy.declared_peer_tiers[20] = 0;
        busy.declared_peer_tiers[21] = 1;
        for row in &mut busy.declared_peer_runs { row["padding"] = json!("p".repeat(1800)); }
        let response = format_list_runs_response(json!({"run_id":"home"}), "worker".into(), None, &busy);
        let peer_rows = response["declared_peer_runs"].as_array().unwrap();
        assert!(peer_rows.iter().any(|r| r["run_id"] == "peer_20"));
        assert!(peer_rows.iter().any(|r| r["run_id"] == "peer_21"));
        assert!(response["other_runs"].as_array().unwrap().is_empty());
        assert_eq!(response["declared_peer_runs_omitted"].as_u64().unwrap() as usize + peer_rows.len(), 22);
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_LIST_RUNS_BYTES);
    }

    #[test]
    fn observe_runs_pulse_marks_pending_intake_wakeup_as_turn_open() {
        let tmp = tempfile::tempdir().unwrap();
        let (reg, runs, ledger) = (tmp.path().join("registry"), tmp.path().join("runs"), tmp.path().join("ledger"));
        std::fs::create_dir_all(&ledger).unwrap();
        let long_title = "T".repeat(150);
        write_iface(&reg, &json!({"id":"world_provider_p1","interface_type":"world","role":"provider","direction":"bidirectional","source_names":["p"],"interface_driver_ref":"codex"}));
        write_iface(&reg, &json!({"id":"world_chat_peer","interface_type":"world","role":"user","direction":"bidirectional","source_names":["c"],"interface_driver_ref":"app_server"}));
        write_json(&reg.join("interface/world_chat_peer/octopus_os.json"), &json!({"presentation":{"name": long_title}}));

        let home = write_run(&runs, "run_home", "idle", &[("cp_prov", "world_provider_p1")], &[]);
        let peer_base = write_run(&runs, "run_peer", "idle", &[("cp_prov", "world_provider_p1"), ("cp_chat", "world_chat_peer")], &[]);
        let mut peer_val = serde_json::to_value(peer_base).unwrap();
        peer_val["interface_state"] = json!({"system_service_input_peer": {"input_intake": {
            "schema_version": 1, "run": "run_peer", "provider_interface": "world_provider_p1",
            "writer": {"owner":"system_service_input_peer","source":"system_service_input_peer","door":"system_room_peer","tool":"file.write","call_label":"tool.call","result_label":"file.write.result"},
            "writer_required_verified_artifacts": [{
                "artifact_label": "tool.call", "verifier": "tool_call_allowed", "mark": "tool.call.allowed",
                "params": {"tools": ["file.write"], "metadata_matches": [{"payload": "/arguments/expected_before_hash", "metadata": "/runtime/run_record_hash"}]}
            }],
            "profile": {"file_write_tool":"file.write","file_write_label":"tool.call","file_write_result_label":"file.write.result"},
            "provider_profile": {"file_write_tool":"file.write","file_write_label":"tool.call","file_write_result_label":"file.write.result"},
            "input_label": "provider.peer.input", "artifact_bindings": []
        }}});
        write_json(&runs.join("run_peer.json"), &peer_val);

        let mut log = DurableLog::open(ledger.join("movements.jsonl")).unwrap();
        let mut blobs = DurableBlobs::open(ledger.join("blobs")).unwrap();
        let ok_hash = blobs.try_put(b"{}").unwrap();
        log.try_append(MovementDraft {
            trace: TraceId("t0".into()), kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface("world_provider_p1".into()), to: Endpoint::Interface("system_worker_operator_task".into()),
            artifact: Some(artifact_with_hash("turn.completed", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_peer", "world_provider_p1", DeliveryVerdict::Admitted { seq: 0 })),
            causal_floor: None,
        }).unwrap();
        log.try_append(MovementDraft {
            trace: TraceId("t1".into()), kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface("system_room_home".into()), to: Endpoint::Interface("system_service_input_peer".into()),
            artifact: Some(artifact_with_hash("provider.peer.message", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_home", "system_service_input_peer", DeliveryVerdict::Admitted { seq: 1 })),
            causal_floor: None,
        }).unwrap();
        log.try_append(MovementDraft {
            trace: TraceId("t2".into()), kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface("system_room_peer".into()), to: Endpoint::Interface("system_service_input_peer".into()),
            artifact: Some(artifact_with_hash("file.write.result", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_writer_peer", "system_service_input_peer", DeliveryVerdict::Admitted { seq: 2 })),
            causal_floor: None,
        }).unwrap();
        let mut index = LedgerSqliteIndex::open(&ledger.join("index.sqlite")).unwrap();
        index.ingest_from(&log).unwrap();

        let (arrivals, _rx) = tokio::sync::mpsc::channel(1);
        let mut ctx = InterfaceLiftContext::unprivileged(
            "world_provider_p1".into(), tmp.path().into(), None, tmp.path().into(),
            ledger.join("movements.jsonl"), ledger.join("blobs"), ledger.join("index.sqlite"),
            Default::default(), arrivals, tokio_util::sync::CancellationToken::new(),
        );
        let roots = CompositionRoots {
            registry_root: reg, run_records_root: runs,
            ee_instances_root: tmp.path().join("ees"), interface_private_state_root: tmp.path().join("var"),
        };
        let peers = BTreeMap::from([("system_service_input_peer".to_string(), Some("run_peer".to_string()))]);

        for use_index in [true, false] {
            ctx.ledger_index_path = if use_index { ledger.join("index.sqlite") } else { ledger.join("absent.sqlite") };
            let observed = observe_runs_pulse(&home, &peers, Some(&roots), Some(&ctx), false);
            let row = &observed.declared_peer_runs[0];
            assert_eq!(row["title"], json!(long_title));
            assert_eq!(row["activity"]["phase"], json!("turn_open"));
            assert_eq!(row["activity"]["pending_intake_seq"], json!(1));
            assert_eq!(row["activity"]["last_movement_seq"], json!(1));
        }

        log.try_append(MovementDraft {
            trace: TraceId("t3".into()), kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface("system_service_input_peer".into()), to: Endpoint::Interface("world_provider_p1".into()),
            artifact: Some(artifact_with_hash("provider.peer.input", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_peer", "world_provider_p1", DeliveryVerdict::Admitted { seq: 3 })),
            causal_floor: None,
        }).unwrap();
        log.try_append(MovementDraft {
            trace: TraceId("t4".into()), kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface("world_provider_p1".into()), to: Endpoint::Interface("system_worker_operator_task".into()),
            artifact: Some(artifact_with_hash("turn.completed", ok_hash)),
            invocation_metadata: Some(runtime_meta("run_peer", "world_provider_p1", DeliveryVerdict::Admitted { seq: 4 })),
            causal_floor: None,
        }).unwrap();
        index.ingest_from(&log).unwrap();
        ctx.ledger_index_path = ledger.join("index.sqlite");
        let settled = observe_runs_pulse(&home, &peers, Some(&roots), Some(&ctx), false);
        assert_eq!(settled.declared_peer_runs[0]["activity"]["phase"], json!("idle"));
        assert!(settled.declared_peer_runs[0]["activity"].get("pending_intake_seq").is_none());
    }
}
