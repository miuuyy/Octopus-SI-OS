use super::*;
use serde_json::{Value, json};

#[cfg(test)]
#[path = "../tests/support/capability_amendment.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/support/capability_home.rs"]
mod home_tests;

const AMENDMENT: &str = "capability_amendment";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCapabilityBridge {
    pub tool: String,
    pub correlation_id: String,
    pub arguments_hash: String,
    pub return_interface: InterfaceId,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCapabilityAmendment {
    pub amendment_key: String,
    pub provider_interface: InterfaceId,
    pub before: Value,
    pub after: Value,
    pub bridge: ProviderCapabilityBridge,
    pub authoring_profile: RunAuthoringProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityDelta {
    before: BoundRunAuthoringTopology,
    after: BoundRunAuthoringTopology,
    static_artifact_additions: Vec<ArtifactId>,
    checkpoints: Vec<Value>,
    interface_state: BTreeMap<InterfaceId, BTreeMap<String, Value>>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AppliedAmendment {
    amendment_key: String,
    bridge: ProviderCapabilityBridge,
    delta: CapabilityDelta,
    ordinary_patch: json_patch::Patch,
    effect: CapabilityEffectEvidence,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityEffectEvidence {
    manifest_path: PathBuf,
    manifest: String,
    payload: String,
}

fn topology(record: &RunRecord, provider: &InterfaceId) -> Result<BoundRunAuthoringTopology, String> {
    let value = record.interface_state.get(provider)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING))
        .ok_or("capability amendment requires the existing provider authoring topology")?;
    let value: BoundRunAuthoringTopology = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    value.validate_identity(record, provider)?;
    Ok(value)
}

fn record(value: &Value) -> Result<RunRecord, String> {
    RunRecord::from_bytes(&serde_json::to_vec(value).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

fn exact(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!("capability amendment {name} must be exact and nonempty"));
    }
    Ok(())
}

fn extends<T: PartialEq>(before: &[T], after: &[T]) -> bool {
    before.iter().all(|value| after.contains(value))
}

fn same_future_contract(before: &BoundRunAuthoringTopology, after: &BoundRunAuthoringTopology) -> bool {
    let mut before = before.clone();
    let mut after = after.clone();
    before.checkpoints.retain(|cp| cp.role != RunAuthoringCheckpointRole::ResultInput);
    after.checkpoints.retain(|cp| cp.role != RunAuthoringCheckpointRole::ResultInput);
    before == after
}

fn validate_templates(before: &BoundRunAuthoringTopology, after: &BoundRunAuthoringTopology) -> Result<(), String> {
    let mut old_header = before.clone();
    let mut new_header = after.clone();
    old_header.checkpoints.clear();
    new_header.checkpoints.clear();
    old_header.payload_artifact_bindings.clear();
    new_header.payload_artifact_bindings.clear();
    if old_header != new_header || !extends(&before.payload_artifact_bindings, &after.payload_artifact_bindings) {
        return Err("capability amendment cannot change run, worker, provider, writer or retained payload identities".into());
    }
    let mut names = BTreeSet::new();
    for next in &after.checkpoints {
        exact(&next.name, "checkpoint name")?;
        if !names.insert(&next.name) { return Err("capability amendment repeats a checkpoint template name".into()); }
    }
    for prior in &before.checkpoints {
        let matches = after.checkpoints.iter().filter(|next| next.name == prior.name
            || (prior.role == RunAuthoringCheckpointRole::ProviderToolOutput && next.role == prior.role
                && next.interface_binding == prior.interface_binding
                && next.outbound_interface_bindings == prior.outbound_interface_bindings)).collect::<Vec<_>>();
        let [next] = matches.as_slice() else { return Err(format!("capability amendment removes or duplicates retained template '{}' ({:?}: {} -> {:?})", prior.name,prior.role,prior.interface(),prior.outbound_interfaces().collect::<Vec<_>>())); };
        let next = *next;
        let mut old = prior.clone();
        let mut new = next.clone();
        if !extends(&old.static_artifact_bindings, &new.static_artifact_bindings) {
            return Err(format!("capability amendment removes static context from '{}'", prior.name));
        }
        old.static_artifact_bindings.clear();
        new.static_artifact_bindings.clear();
        if prior.role == RunAuthoringCheckpointRole::ProviderToolOutput {
            let old_tools = checkpoint_tool_ids(prior)?;
            let new_tools = checkpoint_tool_ids(next)?;
            if !old_tools.is_subset(&new_tools) {
                return Err(format!("capability amendment removes accepted tool rights from '{}'", prior.name));
            }
            old.required_verified_artifacts[0].params["tools"] = json!([]);
            new.required_verified_artifacts[0].params["tools"] = json!([]);
            if old.result_receiving.is_empty() != new.result_receiving.is_empty()
                || old.result_receiving.iter().any(|(tool, required)| new.result_receiving.get(tool) != Some(required)) {
                return Err("capability amendment changes retained result receiving requirements".into());
            }
            old.result_receiving.clear();
            new.result_receiving.clear();
            new.name = old.name.clone();
        }
        if old != new || (matches!(prior.role, RunAuthoringCheckpointRole::OwnerInput
            | RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ResultInput) && prior != next) {
            return Err(format!("capability amendment changes a retained return, writer or verification contract '{}'", prior.name));
        }
    }
    for added in after.checkpoints.iter().filter(|next| !before.checkpoints.iter().any(|old| old.name == next.name)) {
        if matches!(added.role, RunAuthoringCheckpointRole::AuthoringCall | RunAuthoringCheckpointRole::AuthoringResult
            | RunAuthoringCheckpointRole::ProviderTransportDispatch | RunAuthoringCheckpointRole::ProviderHandoffCompletion
            | RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ResultInput) {
            return Err("capability amendment cannot introduce another writer or replace a verified-input obligation".into());
        }
    }
    Ok(())
}

fn provider_owned(record: &RunRecord, topology: &BoundRunAuthoringTopology, cp: &octopus_run_record::CheckpointRecord) -> bool {
    record.interface_state.get(&topology.provider_interface)
        .and_then(|state| state.get(FRONTIER_CHECKPOINT_IDS)).and_then(Value::as_array)
        .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(cp.id.as_str())))
        || topology.checkpoints.iter().any(|template| template.owner_authoring.is_none()
            && !matches!(template.role, RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ResultInput)
            && checkpoint_record_matches_template(cp, template, &record.id))
}

fn context(record: &RunRecord, topology: &BoundRunAuthoringTopology) -> Result<Vec<ArtifactId>, String> {
    let mut result = None;
    for template in topology.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::Stable && cp.include_context) {
        let rows = record.checkpoints.iter().filter(|cp| record.active_checkpoints.contains(&cp.id)
            && checkpoint_record_matches_template(cp, template, &record.id)).collect::<Vec<_>>();
        let [row] = rows.as_slice() else { return Err(format!("capability context '{}' has no exact active checkpoint", template.name)); };
        if row.artifact_bindings.iter().any(|binding| binding.access != octopus_policy::ArtifactAccess::read_only()) {
            return Err("capability context must retain exact read-only bindings".into());
        }
        let ids = row.artifact_bindings.iter().map(|binding| binding.id.clone()).collect::<Vec<_>>();
        if result.as_ref().is_some_and(|prior| prior != &ids) {
            return Err("capability context differs between stable provider inputs".into());
        }
        result = Some(ids);
    }
    result.ok_or_else(|| "capability amendment has no stable provider context".into())
}

fn validate_bridge(bridge: &ProviderCapabilityBridge) -> Result<(), String> {
    exact(&bridge.tool, "bridge tool")?;
    exact(&bridge.correlation_id, "bridge correlation")?;
    if ContentHash::from_hex(&bridge.arguments_hash).is_none_or(|hash| hash.to_hex() != bridge.arguments_hash) {
        return Err("capability bridge requires an exact argument hash".into());
    }
    Ok(())
}

fn validate_delta(before: &RunRecord, delta: &CapabilityDelta, profile: &RunAuthoringProfile) -> Result<(), String> {
    let provider = &delta.before.provider_interface;
    if !same_future_contract(&topology(before, provider)?, &delta.before) { return Err("capability amendment has stale baseline topology".into()); }
    delta.after.validate_identity(before, provider)?;
    validate_templates(&delta.before, &delta.after)?;
    for topology in [&delta.before, &delta.after] { topology.config(profile).validate(provider, &before.id)?; }
    let prior_context = context(before, &delta.before)?;
    let mut added = BTreeSet::new();
    for id in &delta.static_artifact_additions {
        exact(id.as_str(), "static artifact")?;
        if prior_context.contains(id) || !added.insert(id) { return Err("capability amendment repeats a static context addition".into()); }
    }
    if !added.is_empty() && delta.before.checkpoints.iter().any(|cp| cp.role == RunAuthoringCheckpointRole::Stable
        && cp.include_context && !cp.required_verified_artifacts.is_empty()) {
        return Err("capability context extension requires settling the existing verified stable input first".into());
    }
    let retained_owners = before.checkpoints.iter().flat_map(|cp|cp.inbound_interface().into_iter().chain(cp.outbound_interfaces()))
        .chain(before.interface_state.keys()).chain(std::iter::once(&before.worker)).collect::<BTreeSet<_>>();
    if delta.interface_state.keys().any(|owner|retained_owners.contains(owner)) {
        return Err("capability amendment changes an existing checkpoint owner".into());
    }
    let mut candidate = json!(before);
    for cp in &delta.checkpoints {
        let cp: octopus_run_record::CheckpointRecord = serde_json::from_value(cp.clone()).map_err(|e|e.to_string())?;
        if before.checkpoint(&cp.id).is_some() || cp.inbound_interface().is_none_or(|owner|
            retained_owners.contains(owner) || !delta.interface_state.contains_key(owner)) {
            return Err(format!("capability amendment added checkpoint '{}' outside a new declared owner", cp.id));
        }
        candidate["checkpoints"].as_array_mut().ok_or("run checkpoints absent")?.push(json!(cp));
        candidate["active_checkpoints"].as_array_mut().ok_or("run active checkpoints absent")?.push(json!(cp.id));
    }
    record(&candidate)?;
    Ok(())
}

fn delta(amendment: &ProviderCapabilityAmendment) -> Result<CapabilityDelta, String> {
    exact(&amendment.amendment_key, "key")?;
    validate_bridge(&amendment.bridge)?;
    let before = record(&amendment.before)?;
    let after = record(&amendment.after)?;
    let provider = &amendment.provider_interface;
    let old = topology(&before, provider)?;
    let new = topology(&after, provider)?;
    validate_templates(&old, &new)?;
    let mut old_root = amendment.before.clone();
    let mut new_root = amendment.after.clone();
    for key in ["checkpoints", "active_checkpoints", "interface_state", "status"] {
        old_root.as_object_mut().ok_or("run is not an object")?.remove(key);
        new_root.as_object_mut().ok_or("run is not an object")?.remove(key);
    }
    if old_root != new_root { return Err("capability amendment changed run identity or unrelated root fields".into()); }
    for (owner, state) in &before.interface_state {
        let next = after.interface_state.get(owner).ok_or("capability amendment removed interface state")?;
        let mut state = state.clone();
        let mut next = next.clone();
        if owner == provider {
            for key in [FRONTIER_CHECKPOINT_IDS, RUN_AUTHORING_BINDING] { state.remove(key); next.remove(key); }
        }
        if state != next { return Err(format!("capability amendment changed protected private state for '{owner}'")); }
    }
    for cp in before.checkpoints.iter().filter(|cp| !provider_owned(&before, &old, cp)) {
        if after.checkpoint(&cp.id) != Some(cp) || before.active_checkpoints.contains(&cp.id) != after.active_checkpoints.contains(&cp.id) {
            return Err(format!("capability amendment changed accepted neighboring checkpoint '{}'", cp.id));
        }
    }
    let prior_context = context(&before, &old)?;
    let future_context = context(&after, &new)?;
    if !extends(&prior_context, &future_context) { return Err("capability amendment removes retained context".into()); }
    let additions = future_context.into_iter().filter(|id| !prior_context.contains(id)).collect::<Vec<_>>();
    if !additions.is_empty() && old.checkpoints.iter().any(|cp| cp.role == RunAuthoringCheckpointRole::Stable
        && cp.include_context && !cp.required_verified_artifacts.is_empty()) {
        return Err("capability context extension requires settling the existing verified stable input first".into());
    }
    let mut checkpoints = Vec::new();
    for cp in after.checkpoints.iter().filter(|cp| before.checkpoint(&cp.id).is_none() && !provider_owned(&after, &new, cp)) {
        if !after.active_checkpoints.contains(&cp.id) || cp.inbound_interface().is_none_or(|owner| before.interface_state.contains_key(owner)) {
            return Err(format!("capability amendment added checkpoint '{}' outside a new declared owner", cp.id));
        }
        checkpoints.push(serde_json::to_value(cp).map_err(|e| e.to_string())?);
    }
    for record in [&before, &after] {
        validate_tool_continuation(record, provider, &amendment.bridge.tool, &amendment.bridge.return_interface)?;
    }
    let delta = CapabilityDelta {
        before: old, after: new, static_artifact_additions: additions, checkpoints,
        interface_state: after.interface_state.into_iter().filter(|(owner, _)| !before.interface_state.contains_key(owner)).collect(),
    };
    validate_delta(&before, &delta, &amendment.authoring_profile)?;
    Ok(delta)
}

pub fn validate_plan(amendment: &ProviderCapabilityAmendment) -> Result<(), String> { delta(amendment).map(|_| ()) }

pub fn prepare_plan(mut amendment: ProviderCapabilityAmendment) -> Result<ProviderCapabilityAmendment, String> {
    validate_plan(&amendment)?;
    for value in [&mut amendment.before, &mut amendment.after] {
        value.get_mut("interface_state").and_then(|states|states.get_mut(amendment.provider_interface.as_str()))
            .and_then(Value::as_object_mut).ok_or("capability provider state absent")?.remove(AMENDMENT);
    }
    Ok(amendment)
}

pub fn rebase_plan(current: &RunRecord, amendment: ProviderCapabilityAmendment) -> Result<ProviderCapabilityAmendment, String> {
    let change = delta(&amendment)?;
    validate_delta(current, &change, &amendment.authoring_profile)?;
    let before = json!(current);
    let mut after = before.clone();
    let config = change.before.config(&amendment.authoring_profile);
    apply_delta(current, &mut after, &change, &config, &amendment.amendment_key)?;
    let owner = amendment.provider_interface.as_str();
    let state = after["interface_state"][owner].as_object_mut().ok_or("capability provider state absent")?;
    for (key,value) in &current.interface_state[&amendment.provider_interface] {
        if key != RUN_AUTHORING_BINDING && key != FRONTIER_CHECKPOINT_IDS { state.insert(key.clone(),value.clone()); }
    }
    prepare_plan(ProviderCapabilityAmendment {before, after, ..amendment})
}

pub fn prospective_capability_record(amendment: &ProviderCapabilityAmendment) -> Result<RunRecord, String> {
    let mut future = record(&amendment.after)?;
    let topology = topology(&future, &amendment.provider_interface)?;
    let config = topology.config(&amendment.authoring_profile);
    let roles = current_mandatory_role_artifacts(&future, &config)?;
    let context = current_context_artifacts(&future, &config)?;
    for template in &topology.checkpoints {
        if template.owner_authoring.is_some() || matches!(template.role,
            RunAuthoringCheckpointRole::AuthoringCall | RunAuthoringCheckpointRole::AuthoringResult
            | RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ResultInput) {
            continue;
        }
        let id = format!("prospective_{}_{}", amendment.amendment_key, template.name);
        if future.checkpoints.iter().any(|cp| cp.id.as_str() == id) {
            return Err(format!("prospective capability checkpoint '{id}' already exists"));
        }
        let bindings = if template.include_context { &context } else { &roles }
            .iter().map(|id| json!({"id":id})).collect::<Vec<_>>();
        let checkpoint = materialize_checkpoint(template, &id, &future.id, &bindings);
        future.checkpoints.push(serde_json::from_value(checkpoint).map_err(|e| e.to_string())?);
        future.active_checkpoints.push(id.into());
    }
    Ok(future)
}

impl ProviderOutputHandoff {
    pub async fn checked_capability_baseline(&self, hash: &str, expected_before: &Value) -> Result<Vec<u8>, String> {
        let hash = ContentHash::from_hex(hash).filter(|value|value.to_hex()==hash).ok_or("capability baseline needs an exact content hash")?;
        let limits = self.ctx.required_host_io_limits()?;
        let observed = octopus_interfaces::read_artifact_carrier_file_bounded(&self.ctx.blobs_dir.join(hash.to_hex()),limits.max_payload_bytes).map_err(|e|e.to_string())?;
        if ContentHash::of_bytes(&observed) != hash || serde_json::from_slice::<Value>(&observed).map_err(|e|e.to_string())? != *expected_before {
            return Err("capability baseline differs from its exact pinned snapshot".into());
        }
        let before = RunRecord::from_bytes(&observed).map_err(|e|e.to_string())?;
        let topology = topology(&before,&self.config.provider_interface)?;
        if before.id != self.config.run { return Err("capability baseline belongs to another run".into()); }
        let mut state = self.state.lock().await;
        self.reconcile_checked_run_state(&mut state).await?;
        if observed != state.run_record_bytes {
            let writer = CheckedRunWriter {owner:self.config.provider_interface.clone(),source:topology.authoring_source_interface.clone(),
                door:self.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface().clone(),
                tool:self.config.file_write_tool.clone(),call_label:self.config.file_write_label.clone(),result_label:self.config.file_write_result_label.clone()};
            let evidence = CheckedRunEvidence {movements_path:&self.ctx.movements_path,ledger_index_path:&self.ctx.ledger_index_path,
                blobs_dir:&self.ctx.blobs_dir,max_control_frame_bytes:limits.max_control_frame_bytes,
                max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
            let start = pending_handoff(&before,&self.config.provider_interface)?.map(|pending|pending.source_movement_seq).unwrap_or(0);
            checked_declared_run_advance(&evidence,&self.run_path,&observed,&state.run_record_bytes,start,vec![writer])?;
        }
        Ok(state.run_record_bytes.clone())
    }
}

pub fn validate_tool_continuation(record: &RunRecord, provider: &InterfaceId, tool: &str, door: &InterfaceId) -> Result<(), String> {
    let topology = topology(record, provider)?;
    let outputs = topology.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::ProviderToolOutput)
        .map(|cp| checkpoint_tool_ids(cp).map(|tools| (cp, tools))).collect::<Result<Vec<_>, _>>()?;
    let outputs = outputs.iter().filter(|(_, tools)| tools.contains(tool)).collect::<Vec<_>>();
    let [(output, tools)] = outputs.as_slice() else { return Err(format!("capability bridge tool '{tool}' has no unique output contract")); };
    if output.outbound_interface_bindings.len() != 1 || output.outbound_interface_bindings[0].interface != *door {
        return Err("capability bridge tool changed its exact door".into());
    }
    let returns = topology.checkpoints.iter().filter(|cp| cp.role.is_provider_input()
        && cp.role != RunAuthoringCheckpointRole::ResultInput && cp.interface() == door
        && cp.outbound_interfaces().any(|target| target == provider)).count();
    if output.result_receiving.is_empty() {
        if returns != 1 || topology.checkpoints.iter().any(|cp| cp.role == RunAuthoringCheckpointRole::ResultInput
            && cp.interface() == door && cp.outbound_interfaces().any(|target| target == provider)) {
            return Err("capability bridge has no unique retained return contract".into());
        }
    } else {
        if returns != 0 || output.result_receiving.keys().ne(tools.iter()) {
            return Err("capability bridge result receiving conflicts with its exact tool or return contract".into());
        }
        for (tool, required) in &output.result_receiving {
            required.validate("capability result receiving")?;
            if required.artifact_label != format!("{tool}.result") || !required.params.is_object() {
                return Err("capability bridge requires exact native result labels and object params".into());
            }
        }
    }
    Ok(())
}

pub(super) fn validate_emission(amendment: &ProviderCapabilityAmendment, emission: &ProviderHandoffEmission) -> Result<(), String> {
    validate_plan(amendment)?;
    let ProviderHandoffRoute::Tool { tool } = &emission.route else { return Err("capability amendment requires an ordinary tool effect".into()); };
    if tool != &amendment.bridge.tool { return Err("capability amendment changed its effect tool".into()); }
    validate_effect_payload(&amendment.bridge, &emission.artifact.read_payload_bytes().map_err(|e| e.to_string())?)
}

fn validate_effect_payload(bridge: &ProviderCapabilityBridge, bytes: &[u8]) -> Result<(), String> {
    validate_bridge(bridge)?;
    let call: octopus_tool_handlers::ToolCallArtifact = serde_json::from_slice(bytes)
        .map_err(|e| e.to_string())?;
    if call.tool != bridge.tool || call.correlation_id.as_deref() != Some(bridge.correlation_id.as_str())
        || ContentHash::of_bytes(&serde_json::to_vec(&call.arguments).map_err(|e| e.to_string())?).to_hex() != bridge.arguments_hash {
        return Err("capability amendment is detached from its exact ordinary effect".into());
    }
    Ok(())
}

fn validate_effect(record: &RunRecord, bridge: &ProviderCapabilityBridge, effect: &CapabilityEffectEvidence, provider: &InterfaceId) -> Result<(), String> {
    let pending = pending_handoff(record, provider)?.ok_or("capability amendment lost its pending effect")?;
    let [item] = pending.items.as_slice() else { return Err("capability amendment requires one exact pending effect".into()); };
    let artifact = InterfaceArtifactFile::from_captured_snapshot(&effect.manifest_path, effect.manifest.as_bytes().to_vec(), effect.payload.as_bytes().to_vec()).map_err(|e|e.to_string())?;
    let manifest = artifact.read_manifest().map_err(|e|e.to_string())?;
    let payload_hash = ContentHash::of_bytes(effect.payload.as_bytes()).to_hex();
    if manifest.payload_content_hash.as_deref() != Some(payload_hash.as_str()) {
        return Err("capability amendment evidence requires a canonical payload_content_hash matching its captured payload".into());
    }
    let hash = ContentHash::of_bytes(effect.manifest.as_bytes()).to_hex();
    if hash != item.artifact_content_hash || manifest.label != item.artifact_label
        || item.artifact_id.as_str() != format!("{}:{hash}",manifest.label)
        || !effect.manifest_path.ends_with(&item.manifest_relative_path)
        || item.checkpoint.interface_binding.interface != *provider
        || item.checkpoint.outbound_interface_bindings.len() != 1
        || item.checkpoint.outbound_interface_bindings[0].interface != bridge.return_interface {
        return Err("capability amendment evidence differs from its persisted pending effect".into());
    }
    validate_effect_payload(bridge, effect.payload.as_bytes())
}

fn with_ordinary_results(planned: &BoundRunAuthoringTopology, ordinary: &RunRecord) -> Result<BoundRunAuthoringTopology, String> {
    let mut topology = planned.clone();
    topology.checkpoints.retain(|cp| cp.role != RunAuthoringCheckpointRole::ResultInput);
    topology.checkpoints.extend(self::topology(ordinary, &planned.provider_interface)?.checkpoints.into_iter()
        .filter(|cp| cp.role == RunAuthoringCheckpointRole::ResultInput));
    Ok(topology)
}

fn apply_delta(current: &RunRecord, ordinary: &mut Value, delta: &CapabilityDelta, config: &RunAuthoringConfig, key: &str) -> Result<(), String> {
    let provider = &config.provider_interface;
    if !same_future_contract(&topology(current, provider)?, &delta.before) { return Err("capability amendment baseline topology changed before its checked write".into()); }
    let initial = record(ordinary)?;
    let amended = with_ordinary_results(&delta.after, &initial)?;
    let mut amended_config = config.clone();
    amended_config.checkpoints = amended.checkpoints.clone();
    amended_config.payload_artifact_bindings = amended.payload_artifact_bindings.clone();
    amended_config.validate(provider, &initial.id)?;
    let mut live_context = current_context_artifacts(&initial, config)?;
    for id in &delta.static_artifact_additions { append_artifact(&mut live_context, id.clone()); }
    let roles = current_mandatory_role_artifacts(&initial, config)?;
    let bindings = live_context.iter().map(|id| json!({"id":id})).collect::<Vec<_>>();
    let role_bindings = roles.iter().map(|id| json!({"id":id})).collect::<Vec<_>>();
    let state = initial.interface_state.get(provider).ok_or("capability provider state absent")?;
    let generation = optional_u64(state, "authoring_generation")?;
    let prefix = format!("cp_amendment_{}_{}", ContentHash::of_bytes(key.as_bytes()).to_hex(), generation);
    let root = ordinary.as_object_mut().ok_or("capability candidate is not an object")?;
    if !delta.static_artifact_additions.is_empty() {
        let mut context_config = config.clone();
        for template in &mut context_config.checkpoints {
            if let Some(next) = delta.after.checkpoints.iter().find(|next| next.name == template.name) {
                *template = next.clone();
            }
        }
        replace_context_checkpoints(root, &context_config, &initial.id, &prefix, &bindings)?;
    }
    let mut frontier = raw_frontier_checkpoint_ids(state)?.into_iter().collect::<Vec<_>>();
    let mut rows = Vec::new();
    for template in delta.after.checkpoints.iter().filter(|cp| !delta.before.checkpoints.iter().any(|old| old.name == cp.name)) {
        let id = format!("{prefix}_{}", template.name);
        match template.role {
            RunAuthoringCheckpointRole::Stable | RunAuthoringCheckpointRole::IndependentInput => {
                rows.push(materialize_checkpoint(template, &id, &initial.id, if template.include_context { &bindings } else { &role_bindings }));
            }
            _ if retains_future_output(template) => {
                rows.push(materialize_checkpoint(template, &id, &initial.id, &role_bindings));
                frontier.push(id);
            }
            RunAuthoringCheckpointRole::OwnerInput | RunAuthoringCheckpointRole::ProviderToolOutput | RunAuthoringCheckpointRole::ProviderArtifactOutput => {}
            _ => return Err("capability amendment contains an unsupported new authoring role".into()),
        }
    }
    rows.extend(delta.checkpoints.iter().cloned());
    for row in rows {
        let id = checkpoint_value_id(&row)?.to_string();
        let checkpoints = root.get_mut("checkpoints").and_then(Value::as_array_mut).ok_or("capability checkpoints absent")?;
        if checkpoints.iter().any(|cp| cp.get("id").and_then(Value::as_str) == Some(id.as_str())) {
            return Err(format!("capability amendment checkpoint '{id}' already exists"));
        }
        checkpoints.push(row);
        root.get_mut("active_checkpoints").and_then(Value::as_array_mut).ok_or("capability active checkpoints absent")?.push(json!(id));
    }
    let states = root.get_mut("interface_state").and_then(Value::as_object_mut).ok_or("capability state absent")?;
    for (owner, value) in &delta.interface_state {
        if states.contains_key(owner.as_str()) { return Err(format!("capability owner '{owner}' appeared during publication")); }
        states.insert(owner.to_string(), json!(value));
    }
    let state = states.get_mut(provider.as_str()).and_then(Value::as_object_mut).ok_or("capability provider state absent")?;
    state.insert(RUN_AUTHORING_BINDING.into(), json!(amended));
    state.insert(FRONTIER_CHECKPOINT_IDS.into(), json!(frontier));
    if !delta.static_artifact_additions.is_empty() {
        let before_generation = optional_u64(current.interface_state.get(provider).ok_or("current provider state absent")?, "context_generation")?;
        let next_generation = before_generation.checked_add(1).ok_or("context generation overflow")?;
        if state.get("context_generation").and_then(Value::as_u64).is_none_or(|authored| authored < next_generation) {
            state.insert("context_generation".into(), json!(next_generation));
        }
        if let Some(values) = state.get_mut("pending_handoff").and_then(|pending| pending.get_mut("context_publication")).and_then(Value::as_array_mut) {
            for id in &delta.static_artifact_additions { if !values.contains(&json!(id)) { values.push(json!(id)); } }
        }
    }
    let after = record(ordinary)?;
    validate_checkpoint_topology(&after, provider, &amended.checkpoints, None)?;
    if let Some(pending) = pending_handoff(&after, provider)? { validate_pending_handoff_delivery_isolation(&after, &pending)?; }
    Ok(())
}

pub(super) fn apply_to_authored_run(current: &[u8], authored: &mut AuthoredRun, amendment: &ProviderCapabilityAmendment, config: &RunAuthoringConfig, var_dir: &Path) -> Result<BoundRunAuthoringTopology, String> {
    if config.provider_interface != amendment.provider_interface { return Err("capability amendment addressed another provider".into()); }
    let delta = delta(amendment)?;
    let profile = &amendment.authoring_profile;
    if profile.file_write_tool != config.file_write_tool || profile.file_write_label != config.file_write_label
        || profile.file_write_result_label != config.file_write_result_label || profile.result_timeout_ms != config.result_timeout_ms {
        return Err("capability amendment changed the active authoring profile".into());
    }
    let before = RunRecord::from_bytes(current).map_err(|e| e.to_string())?;
    validate_delta(&before, &delta, profile)?;
    let [item] = authored.pending_handoff.items.as_slice() else { return Err("capability amendment requires one pending effect".into()); };
    let (artifact, _) = load_pending_artifact(var_dir, item)?;
    let snapshot = artifact.read_bounded(u64::MAX, u64::MAX).map_err(|e|e.to_string())?;
    let effect = CapabilityEffectEvidence { manifest_path: artifact.manifest_path().to_path_buf(),
        manifest:String::from_utf8(snapshot.manifest_bytes).map_err(|e|e.to_string())?,payload:String::from_utf8(snapshot.payload_bytes).map_err(|e|e.to_string())? };
    let mut after: Value = serde_json::from_str(&authored.content).map_err(|e| e.to_string())?;
    let ordinary_patch = authored.patch.clone();
    apply_delta(&before, &mut after, &delta, config, &amendment.amendment_key)?;
    let pending = pending_handoff(&record(&after)?, &config.provider_interface)?.ok_or("capability amendment lost pending effect")?;
    if pending.items != authored.pending_handoff.items || pending.terminal != authored.pending_handoff.terminal || pending.source_movement_seq != authored.pending_handoff.source_movement_seq {
        return Err("capability amendment changed accepted effect or source coordinates".into());
    }
    validate_effect(&record(&after)?, &amendment.bridge, &effect, &config.provider_interface)?;
    let proof = AppliedAmendment { amendment_key: amendment.amendment_key.clone(), bridge: amendment.bridge.clone(), delta: delta.clone(), ordinary_patch, effect };
    after["interface_state"][config.provider_interface.as_str()][AMENDMENT] = json!(proof);
    let bytes = JsonFormat::Compact.encode(&after)?;
    authored.content = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    authored.after_hash = ContentHash::of_bytes(authored.content.as_bytes()).to_hex();
    authored.patch = json_patch::diff(&serde_json::from_slice(current).map_err(|e| e.to_string())?, &after);
    authored.pending_handoff = pending;
    topology(&record(&after)?, &config.provider_interface)
}

pub fn validate_installed(record: &RunRecord, amendment: &ProviderCapabilityAmendment) -> Result<(), String> {
    let expected = delta(amendment)?;
    let proof: AppliedAmendment = serde_json::from_value(record.interface_state.get(&amendment.provider_interface)
        .and_then(|state| state.get(AMENDMENT)).cloned().ok_or("capability amendment has no installed checked transition")?).map_err(|e| e.to_string())?;
    let installed = topology(record, &amendment.provider_interface)?;
    if proof.amendment_key != amendment.amendment_key || proof.bridge != amendment.bridge || proof.delta != expected
        || !same_future_contract(&installed, &expected.after) {
        return Err("capability readback differs from its exact planned transition".into());
    }
    installed.config(&amendment.authoring_profile).validate(&amendment.provider_interface, &record.id)?;
    validate_checkpoint_topology(record, &amendment.provider_interface, &installed.checkpoints, None)?;
    if !extends(&expected.static_artifact_additions, &context(record,&installed)?) {
        return Err("capability readback lost its declared static context additions".into());
    }
    Ok(())
}

pub(super) fn checked_topology_advance(
    provider: &InterfaceId,
    initial: &BoundRunAuthoringTopology,
    current: &BoundRunAuthoringTopology,
    transitions: &[CheckedRunTransition],
) -> Result<bool, String> {
    let mut expected = initial.clone();
    let mut amended = false;
    for transition in transitions {
        let before = RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
        let after = RunRecord::from_bytes(&transition.after).map_err(|error| error.to_string())?;
        if topology(&before, provider)? != expected { return Ok(false); }
        let next = topology(&after, provider)?;
        if next == expected { continue; }
        if same_future_contract(&expected, &next) {
            if transition.writer.owner != *provider { return Ok(false); }
            validate_checked_provider_transition(transition)?;
            expected = next;
            continue;
        }
        if transition.writer.owner != *provider || !validate_transition(transition)? { return Ok(false); }
        expected = next;
        amended = true;
    }
    Ok(amended && expected == *current)
}

pub(super) fn validate_transition(transition: &CheckedRunTransition) -> Result<bool, String> {
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
    let provider = &transition.writer.owner;
    let prior = before.interface_state.get(provider).and_then(|state| state.get(AMENDMENT));
    let next = after.interface_state.get(provider).and_then(|state| state.get(AMENDMENT));
    if prior == next { return Ok(false); }
    let proof: AppliedAmendment = serde_json::from_value(next.cloned().ok_or("checked provider removed its capability transition evidence")?).map_err(|e| e.to_string())?;
    exact(&proof.amendment_key, "key")?;
    let profile = RunAuthoringProfile {
        file_write_tool: transition.writer.tool.clone(), file_write_label: transition.writer.call_label.clone(),
        file_write_result_label: transition.writer.result_label.clone(), result_timeout_ms: 1,
    };
    validate_delta(&before, &proof.delta, &profile)?;
    validate_effect(&after, &proof.bridge, &proof.effect, provider)?;
    if !same_future_contract(&topology(&before, provider)?, &proof.delta.before)
        || !same_future_contract(&topology(&after, provider)?, &proof.delta.after) {
        return Err("checked capability transition has stale topology evidence".into());
    }
    let mut ordinary: Value = serde_json::from_slice(&transition.before).map_err(|e| e.to_string())?;
    json_patch::patch(&mut ordinary, &proof.ordinary_patch).map_err(|e| e.to_string())?;
    if ordinary.get("interface_state").and_then(|state| state.get(provider.as_str())).and_then(|state| state.get(AMENDMENT)) != prior {
        return Err("ordinary provider step changed its amendment evidence".into());
    }
    let mut base = transition.clone();
    base.after = JsonFormat::Compact.encode(&ordinary)?;
    validate_checked_provider_transition(&base)?;
    let config = proof.delta.before.config(&profile);
    apply_delta(&before, &mut ordinary, &proof.delta, &config, &proof.amendment_key)?;
    ordinary["interface_state"][provider.as_str()][AMENDMENT] = json!(proof);
    if JsonFormat::Compact.encode(&ordinary)? != transition.after {
        return Err("checked capability transition changed fields outside its exact future contract".into());
    }
    validate_tool_continuation(&before, provider, &proof.bridge.tool, &proof.bridge.return_interface)?;
    validate_tool_continuation(&after, provider, &proof.bridge.tool, &proof.bridge.return_interface)?;
    Ok(true)
}
