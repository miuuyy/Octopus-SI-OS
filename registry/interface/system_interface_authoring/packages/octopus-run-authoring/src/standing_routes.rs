use super::*;
use octopus_run_record::CheckpointRecord;
use std::io::Write;

pub(super) const STANDING_DISPATCH_NAME: &str = "transport_dispatch_standing";
const JOURNAL_SCHEMA_VERSION: u32 = 1;

pub(super) fn standing_candidate_labels(descriptor: &InterfaceDescriptor) -> Result<BTreeSet<String>, String> {
    let Some(values) = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.private_config.get("client_projection_output_labels"))
    else {
        return Ok(BTreeSet::new());
    };
    values
        .as_array()
        .ok_or("provider private_config.client_projection_output_labels must be an array")?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            non_empty_string(
                value.as_str().unwrap_or_default(),
                &format!("client_projection_output_labels[{index}]"),
            )
        })
        .collect()
}

fn standing_dispatch_template(provider: &InterfaceId) -> RunAuthoringCheckpoint {
    RunAuthoringCheckpoint {
        name: STANDING_DISPATCH_NAME.to_string(),
        role: RunAuthoringCheckpointRole::ProviderTransportDispatch,
        result_receiving: BTreeMap::new(),
        static_artifact_bindings: Vec::new(),
        owner_authoring: None,
        interface_binding: AuthoredInterfaceBinding {
            interface: provider.clone(),
            config: InterfaceBindingConfig {
                artifact_policy: BindingArtifactPolicy::Allow {
                    names: vec![PROVIDER_TRANSPORT_DISPATCH_LABEL.to_string()],
                    namespaces: Vec::new(),
                },
            },
        },
        outbound_interface_bindings: Vec::new(),
        include_context: false,
        required_verified_artifacts: Vec::new(),
    }
}

pub(super) fn client_projection_owner<'a>(
    templates: &'a [RunAuthoringCheckpoint],
    artifact_label: &str,
) -> Result<&'a RunAuthoringCheckpoint, String> {
    let owners = templates
        .iter()
        .filter(|checkpoint| {
            checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
                && checkpoint.interface_binding.admits(artifact_label)
                && checkpoint
                    .outbound_interface_bindings
                    .iter()
                    .all(|binding| binding.admits(artifact_label))
                && match checkpoint.required_verified_artifacts.as_slice() {
                    [] => true,
                    [required] => required.artifact_label == artifact_label,
                    _ => false,
                }
        })
        .collect::<Vec<_>>();
    match owners.as_slice() {
        [owner] => Ok(owner),
        [] => Err(format!(
            "provider emitted client artifact '{artifact_label}' with no descriptor-derived output owner"
        )),
        _ => Err(format!(
            "provider emitted client artifact '{artifact_label}' with {} descriptor-derived output owners",
            owners.len()
        )),
    }
}

fn policy_names(policy: &BindingArtifactPolicy) -> Option<BTreeSet<String>> {
    match policy {
        BindingArtifactPolicy::Allow { names, namespaces } if namespaces.is_empty() => {
            Some(names.iter().cloned().collect())
        }
        _ => None,
    }
}

fn template_delivers(template: &RunAuthoringCheckpoint, artifact_label: &str) -> bool {
    if !template.required_verified_artifacts.is_empty() {
        return template
            .required_verified_artifacts
            .iter()
            .any(|required| required.artifact_label == artifact_label);
    }
    template.interface_binding.admits(artifact_label)
}

fn standing_owner<'a>(
    templates: &'a [RunAuthoringCheckpoint],
    provider: &InterfaceId,
    artifact_label: &str,
) -> Option<&'a RunAuthoringCheckpoint> {
    let owner = client_projection_owner(templates, artifact_label).ok()?;
    let exclusive = templates.iter().all(|template| {
        template.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            || template.role.is_provider_input()
            || template.interface() != provider
            || !template_delivers(template, artifact_label)
    });
    (exclusive
        && owner.interface() == provider
        && owner.required_verified_artifacts.is_empty()
        && !owner.include_context)
        .then_some(owner)
}

fn delivered_names(checkpoint: &CheckpointRecord) -> Option<BTreeSet<String>> {
    if !checkpoint.required_verified_artifacts.is_empty() {
        return Some(
            checkpoint
                .required_verified_artifacts
                .iter()
                .map(|required| required.artifact_label.clone())
                .collect(),
        );
    }
    match &checkpoint.interface_binding {
        Some(binding) => policy_names(&binding.config.artifact_policy),
        None => Some(BTreeSet::new()),
    }
}

fn delivers(checkpoint: &CheckpointRecord, artifact_label: &str) -> bool {
    delivered_names(checkpoint).is_none_or(|names| names.contains(artifact_label))
}

fn standing_checkpoint(
    template: &RunAuthoringCheckpoint,
    id: &str,
    names: Vec<String>,
    mandatory_role_artifacts: &[ArtifactId],
) -> PendingHandoffCheckpoint {
    let mut artifact_bindings = mandatory_role_artifacts.to_vec();
    for artifact in &template.static_artifact_bindings {
        append_artifact(&mut artifact_bindings, artifact.clone());
    }
    let mut interface_binding = template.interface_binding.clone();
    interface_binding.config.artifact_policy = BindingArtifactPolicy::Allow {
        names,
        namespaces: Vec::new(),
    };
    PendingHandoffCheckpoint {
        id: CheckpointId::new(id),
        epoch: 0,
        interface_binding,
        outbound_interface_bindings: template.outbound_interface_bindings.clone(),
        artifact_bindings,
        required_verified_artifacts: Vec::new(),
    }
}

pub(super) fn materialize_standing_routes(
    templates: &[RunAuthoringCheckpoint],
    provider: &InterfaceId,
    candidates: &BTreeSet<String>,
    frontier: &[serde_json::Value],
    prefix: &str,
    run: &RunId,
    mandatory_role_artifacts: &[ArtifactId],
) -> Result<Vec<(String, serde_json::Value)>, String> {
    let mut claimed = BTreeSet::new();
    for value in frontier {
        let checkpoint: CheckpointRecord = serde_json::from_value(value.clone())
            .map_err(|error| format!("provider frontier checkpoint is malformed: {error}"))?;
        if checkpoint.inbound_interface() != Some(provider) {
            continue;
        }
        match delivered_names(&checkpoint) {
            Some(names) => claimed.extend(names),
            None => return Ok(Vec::new()),
        }
    }
    let mut labels = candidates.clone();
    for template in templates
        .iter()
        .filter(|template| template.role == RunAuthoringCheckpointRole::ProviderArtifactOutput)
    {
        if let Some(names) = policy_names(&template.interface_binding.config.artifact_policy) {
            labels.extend(names);
        }
    }
    let mut owned = BTreeMap::<String, (RunAuthoringCheckpoint, Vec<String>)>::new();
    for label in labels.into_iter().filter(|label| !claimed.contains(label)) {
        if let Some(owner) = standing_owner(templates, provider, &label) {
            owned
                .entry(owner.name.clone())
                .or_insert_with(|| (owner.clone(), Vec::new()))
                .1
                .push(label);
        }
    }
    if !claimed.contains(PROVIDER_TRANSPORT_DISPATCH_LABEL) {
        owned.insert(
            STANDING_DISPATCH_NAME.to_string(),
            (standing_dispatch_template(provider), vec![PROVIDER_TRANSPORT_DISPATCH_LABEL.to_string()]),
        );
    }
    Ok(owned
        .into_values()
        .map(|(template, names)| {
            let id = format!("{prefix}_standing_{}", template.name);
            let checkpoint = standing_checkpoint(&template, &id, names, mandatory_role_artifacts);
            (id, materialize_pending_checkpoint(&checkpoint, run))
        })
        .collect())
}

fn standing_instance_owner(
    checkpoint: &CheckpointRecord,
    templates: &[RunAuthoringCheckpoint],
    provider: &InterfaceId,
    roles: &[ArtifactBinding],
) -> Option<RunAuthoringCheckpoint> {
    if checkpoint.inbound_interface() != Some(provider) || !checkpoint.required_verified_artifacts.is_empty() {
        return None;
    }
    let names = policy_names(&checkpoint.interface_binding.as_ref()?.config.artifact_policy)?;
    let first = names.first()?;
    let owner = if first == PROVIDER_TRANSPORT_DISPATCH_LABEL {
        (names.len() == 1).then(|| standing_dispatch_template(provider))?
    } else {
        let owner = standing_owner(templates, provider, first)?;
        if !names.iter().all(|name| {
            standing_owner(templates, provider, name).is_some_and(|other| other.name == owner.name)
        }) {
            return None;
        }
        owner.clone()
    };
    (checkpoint.outbound_interface_bindings == owner.outbound_interface_bindings
        && checkpoint_role_bindings_match(checkpoint, roles, &owner))
    .then_some(owner)
}

pub(super) fn classify_standing_frontier(
    provider_bound: &[&CheckpointRecord],
    claimed: &BTreeSet<CheckpointId>,
    templates: &[RunAuthoringCheckpoint],
    provider: &InterfaceId,
    roles: &[ArtifactBinding],
) -> Result<BTreeSet<CheckpointId>, String> {
    let mut owners = BTreeSet::new();
    let mut standing = BTreeSet::new();
    for checkpoint in provider_bound.iter().filter(|checkpoint| !claimed.contains(&checkpoint.id)) {
        let Some(owner) = standing_instance_owner(checkpoint, templates, provider, roles) else {
            continue;
        };
        if !owners.insert(owner.name.clone()) {
            return Err(format!(
                "provider frontier carries two standing routes for output template '{}'",
                owner.name
            ));
        }
        let names = delivered_names(checkpoint).unwrap_or_default();
        if provider_bound
            .iter()
            .filter(|other| other.id != checkpoint.id)
            .any(|other| names.iter().any(|name| delivers(other, name)))
        {
            return Err(format!(
                "provider standing route '{}' shares an artifact name with another provider-bound checkpoint",
                checkpoint.id.as_str()
            ));
        }
        standing.insert(checkpoint.id.clone());
    }
    Ok(standing)
}

fn standing_route<'a>(
    record: &'a RunRecord,
    templates: &[RunAuthoringCheckpoint],
    provider: &InterfaceId,
    roles: &[ArtifactBinding],
    owner: &str,
    artifact_label: &str,
) -> Result<Option<&'a CheckpointRecord>, String> {
    let state = record
        .interface_state
        .get(provider)
        .ok_or("standing route has no provider state")?;
    let frontier = raw_frontier_checkpoint_ids(state)?;
    let deliverers = record
        .active_checkpoints
        .iter()
        .filter_map(|id| record.checkpoint(id))
        .filter(|checkpoint| checkpoint.inbound_interface() == Some(provider) && delivers(checkpoint, artifact_label))
        .collect::<Vec<_>>();
    let [checkpoint] = deliverers.as_slice() else {
        return Ok(None);
    };
    Ok((frontier.contains(checkpoint.id.as_str())
        && standing_instance_owner(checkpoint, templates, provider, roles)
            .is_some_and(|template| template.name == owner))
    .then_some(*checkpoint))
}

fn pending_checkpoint_of(checkpoint: &CheckpointRecord) -> Result<PendingHandoffCheckpoint, String> {
    if checkpoint.artifact_bindings.iter().any(|binding| binding.access != Default::default()) {
        return Err("standing route carries a non-default artifact binding".into());
    }
    Ok(PendingHandoffCheckpoint {
        id: checkpoint.id.clone(),
        epoch: checkpoint.epoch.0,
        interface_binding: checkpoint
            .interface_binding
            .clone()
            .ok_or("standing route has no interface binding")?,
        outbound_interface_bindings: checkpoint.outbound_interface_bindings.clone(),
        artifact_bindings: checkpoint.artifact_bindings.iter().map(|binding| binding.id.clone()).collect(),
        required_verified_artifacts: checkpoint.required_verified_artifacts.clone(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct JournalLine {
    schema_version: u32,
    boundary: String,
    source_movement_seq: u64,
    context_generations: u64,
    items: Vec<PendingHandoffItem>,
}

fn journal_path(ctx: &InterfaceLiftContext, run: &RunId) -> PathBuf {
    ctx.interface_var_dir()
        .join("handoff-standing")
        .join(format!("{}.jsonl", ContentHash::of_bytes(run.as_str().as_bytes()).to_hex()))
}

fn journal_boundary(record: &RunRecord, provider: &InterfaceId) -> Result<String, String> {
    let pending = persisted_pending_handoff(record, provider)?
        .ok_or("standing delivery has no persisted handoff boundary")?;
    let bytes = serde_json::to_vec(&pending).map_err(|error| error.to_string())?;
    Ok(ContentHash::of_bytes(&bytes).to_hex())
}

fn journal_lines(
    ctx: &InterfaceLiftContext,
    record: &RunRecord,
    provider: &InterfaceId,
) -> Result<Vec<JournalLine>, String> {
    let bytes = match std::fs::read(journal_path(ctx, &record.id)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("standing journal cannot be read: {error}")),
    };
    let Some(pending) = persisted_pending_handoff(record, provider)? else {
        return Ok(Vec::new());
    };
    let boundary = ContentHash::of_bytes(&serde_json::to_vec(&pending).map_err(|error| error.to_string())?).to_hex();
    let complete = bytes.iter().rposition(|byte| *byte == b'\n').map_or(0, |end| end + 1);
    let mut lines = Vec::new();
    for raw in bytes[..complete].split(|byte| *byte == b'\n').filter(|line| !line.is_empty()) {
        let line: JournalLine = serde_json::from_slice(raw)
            .map_err(|error| format!("standing journal is corrupt: {error}"))?;
        if line.schema_version != JOURNAL_SCHEMA_VERSION {
            return Err("standing journal has an unsupported schema".into());
        }
        if line.boundary == boundary {
            lines.push(line);
        }
    }
    Ok(lines)
}

fn append_journal(ctx: &InterfaceLiftContext, record: &RunRecord, line: &JournalLine) -> Result<(), String> {
    let path = journal_path(ctx, &record.id);
    let parent = path.parent().ok_or("standing journal has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut bytes = serde_json::to_vec(line).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let current = match std::fs::read(&path) {
        Ok(existing) => existing
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|first| serde_json::from_slice::<JournalLine>(first).ok())
            .is_some_and(|first| first.boundary == line.boundary),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.to_string()),
    };
    if current {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        return file
            .write_all(&bytes)
            .and_then(|_| file.sync_data())
            .map_err(|error| format!("standing journal append failed: {error}"));
    }
    let staged = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|name| name.to_str()).ok_or("standing journal has no name")?,
        std::process::id()
    ));
    let mut file = std::fs::File::create(&staged).map_err(|error| error.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| error.to_string())?;
    std::fs::rename(&staged, &path).map_err(|error| error.to_string())?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

pub fn standing_route_admits_only(
    record: &RunRecord,
    provider: &InterfaceId,
    artifact_label: &str,
) -> Result<(), String> {
    let deliverers = record
        .active_checkpoints
        .iter()
        .filter_map(|id| record.checkpoint(id))
        .filter(|checkpoint| checkpoint.inbound_interface() == Some(provider) && delivers(checkpoint, artifact_label))
        .collect::<Vec<_>>();
    match deliverers.as_slice() {
        [checkpoint]
            if checkpoint.required_verified_artifacts.is_empty()
                && checkpoint
                    .interface_binding
                    .as_ref()
                    .and_then(|binding| policy_names(&binding.config.artifact_policy))
                    .is_some_and(|names| names.contains(artifact_label)) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "run '{}' has no single standing route admitting '{artifact_label}' from '{}'",
            record.id.as_str(),
            provider.as_str()
        )),
    }
}

pub fn open_standing_handoff_before(
    ctx: &InterfaceLiftContext,
    record: &RunRecord,
    provider: &InterfaceId,
    source_movement_seq: u64,
) -> Result<bool, String> {
    if open_handoff_before(record, provider, source_movement_seq)? {
        return Ok(true);
    }
    Ok(journal_lines(ctx, record, provider)?
        .iter()
        .any(|line| line.source_movement_seq < source_movement_seq))
}

pub(super) struct StandingJournal {
    pending: PendingHandoff,
    context_generations: u64,
}

impl ProviderOutputHandoff {
    pub(super) fn standing_route_for(
        &self,
        record: &RunRecord,
        route: &ProviderHandoffRoute,
        template: &RunAuthoringCheckpoint,
        artifact_label: &str,
    ) -> Result<Option<PendingHandoffCheckpoint>, String> {
        let owner = match route {
            ProviderHandoffRoute::TransportDispatch { .. } => STANDING_DISPATCH_NAME,
            ProviderHandoffRoute::ClientProjection => template.name.as_str(),
            _ => return Ok(None),
        };
        let roles = current_mandatory_role_bindings(record, &self.config)?;
        standing_route(
            record,
            &self.config.checkpoints,
            &self.config.provider_interface,
            &roles,
            owner,
            artifact_label,
        )?
        .map(pending_checkpoint_of)
        .transpose()
    }

    fn standing_pending(&self, items: Vec<PendingHandoffItem>) -> PendingHandoff {
        PendingHandoff {
            source_movement_seq: self.source_movement_seq,
            items,
            terminal: false,
            context_publication: None,
            deferred_publication: false,
            source_without_context: self.failed_source,
            settlement_of: None,
            dispatch_fenced: true,
            withdrawal: None,
            replacement: None,
            standing: Vec::new(),
        }
    }

    pub(super) fn standing_items_since_write(&self, record: &RunRecord) -> Result<Vec<PendingHandoffItem>, String> {
        let lines = journal_lines(&self.ctx, record, &self.config.provider_interface)?;
        if let Some(other) = lines.iter().find(|line| line.source_movement_seq != self.source_movement_seq) {
            return Err(format!(
                "provider write for movement {} meets standing deliveries of movement {} without their terminal boundary",
                self.source_movement_seq, other.source_movement_seq
            ));
        }
        Ok(lines.into_iter().flat_map(|line| line.items).collect())
    }

    pub(super) async fn hand_standing_batch(
        &self,
        record: &RunRecord,
        items: Vec<PendingHandoffItem>,
        context_generations: u64,
    ) -> Result<(PendingHandoff, Vec<ProviderHandoffOutcome>), String> {
        if let Some(older) = journal_lines(&self.ctx, record, &self.config.provider_interface)?
            .iter()
            .find(|line| line.source_movement_seq != self.source_movement_seq)
        {
            return Err(format!(
                "provider standing handoff for movement {} follows movement {} without its terminal boundary",
                self.source_movement_seq, older.source_movement_seq
            ));
        }
        append_journal(
            &self.ctx,
            record,
            &JournalLine {
                schema_version: JOURNAL_SCHEMA_VERSION,
                boundary: journal_boundary(record, &self.config.provider_interface)?,
                source_movement_seq: self.source_movement_seq,
                context_generations,
                items: items.clone(),
            },
        )?;
        let pending = self.standing_pending(items);
        let outcomes = self.hand_standing_items(&pending).await?;
        Ok((pending, outcomes))
    }

    async fn hand_standing_items(&self, pending: &PendingHandoff) -> Result<Vec<ProviderHandoffOutcome>, String> {
        let _ownership = self.dispatch_ownership().await?;
        let mut outcomes = Vec::with_capacity(pending.items.len());
        for item in &pending.items {
            let (artifact, expected_ref) = load_pending_artifact(&self.var_dir, item)?;
            let coordinate = PendingHandoffLedgerCoordinate {
                interface: &self.ctx.interface,
                run: &self.config.run,
                worker: &self.config.worker,
                artifact: &expected_ref,
                required_evidence: None,
                checkpoint: &item.checkpoint.id,
                epoch: item.checkpoint.epoch,
            };
            let mut admitted = self.session.index()?.admitted_handoff_seq(coordinate)?;
            if admitted.is_some() && !self.pending_delivery_complete(pending, item)? {
                admitted = None;
            }
            if admitted.is_none() {
                let verdict = self.hand_retained_pending(pending, item, artifact).await?;
                admitted = self.session.index()?.admitted_handoff_seq(coordinate)?;
                if let (None, DeliveryVerdict::Refused { reason, detail }) = (&admitted, verdict) {
                    return Err(format!(
                        "provider standing output '{}' for movement {} was refused as '{}': {}",
                        item.artifact_id.as_str(),
                        pending.source_movement_seq,
                        reason.as_str(),
                        detail
                    ));
                }
            }
            let seq = admitted.ok_or_else(|| {
                format!(
                    "provider standing output '{}' was admitted without exact checkpoint evidence",
                    item.artifact_id.as_str()
                )
            })?;
            outcomes.push(ProviderHandoffOutcome::Admitted { seq, artifact: expected_ref });
        }
        Ok(outcomes)
    }

    pub(super) fn standing_journal_for_source(&self, record: &RunRecord) -> Result<Option<StandingJournal>, String> {
        let lines = journal_lines(&self.ctx, record, &self.config.provider_interface)?;
        if let Some(older) = lines.iter().find(|line| line.source_movement_seq < self.source_movement_seq) {
            return Err(format!(
                "provider reader advanced to movement {} while movement {} retains a non-terminal standing handoff",
                self.source_movement_seq, older.source_movement_seq
            ));
        }
        let mut items = Vec::new();
        let mut context_generations = 0u64;
        for line in lines.into_iter().filter(|line| line.source_movement_seq == self.source_movement_seq) {
            items.extend(line.items);
            context_generations = context_generations
                .checked_add(line.context_generations)
                .ok_or("standing journal context generation overflowed")?;
        }
        Ok((!items.is_empty()).then(|| StandingJournal {
            pending: self.standing_pending(items),
            context_generations,
        }))
    }

    pub(super) async fn recover_standing_journal(
        &self,
        state: &mut HandoffState,
        journal: StandingJournal,
    ) -> Result<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>), String> {
        let StandingJournal { pending, context_generations } = journal;
        let outcomes = self.hand_standing_items(&pending).await?;
        let contributed = self.contributed_context_output_labels();
        for item in pending.items.iter().filter(|item| contributed.contains(&item.artifact_label)) {
            let (artifact, source) = load_pending_artifact(&self.var_dir, item)?;
            let payload = artifact
                .read_payload_bytes()
                .map_err(|error| format!("provider standing context payload cannot be read: {error}"))?;
            append_session_payload(
                &mut state.session_context,
                &item.artifact_label,
                &payload,
                &source,
                None,
                "recovered standing output",
            )?;
            append_artifact(&mut state.context_artifacts, item.artifact_id.clone());
        }
        Self::ground_admitted_context_outputs(&mut state.session_context, &pending, &outcomes, &contributed)?;
        state.deferred_context_generations = context_generations;
        state.consumed_input = false;
        let last_artifact_label = pending
            .items
            .last()
            .map(|item| item.artifact_label.clone())
            .ok_or("provider standing journal is empty")?;
        Ok((ProviderSourceRecovery::Interrupted { last_artifact_label }, outcomes))
    }
}
