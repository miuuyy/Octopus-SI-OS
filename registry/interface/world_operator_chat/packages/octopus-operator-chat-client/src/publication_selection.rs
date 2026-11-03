use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use octopus_context_collections::{
    BoundCollection, CollectionSelection, ScopeContext, normalized_selection, parse_entry,
};
use octopus_descriptors::{ArtifactId, Descriptor, InterfaceId};
use octopus_ledger::{ArtifactRef, ContentHash, InvocationMetadata};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    ArtifactBindingSeed, ExternalInputSeed, ExternalRelaySeed, ModelOutputSeed,
    OctopusOsAuthoringProfile, OperatorChatPlanRequest, PlannedClientWrite, SelectedToolSeed,
    WriteHashAlgorithm,
};

const MAX_DESCRIPTOR_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PublicationSelectionSeeds {
    pub selected_tools: Vec<SelectedToolSeed>,
    pub selected_worker_environment_kinds: Vec<String>,
    pub door_host_slots: BTreeMap<String, BTreeMap<String, PathBuf>>,
    pub external_inputs: Vec<ExternalInputSeed>,
    pub external_relays: Vec<ExternalRelaySeed>,
    pub model_outputs: Vec<ModelOutputSeed>,
    pub context_artifact_bindings: Vec<ArtifactBindingSeed>,
    pub payload_artifact_bindings: Vec<ArtifactRef>,
}

impl PublicationSelectionSeeds {
    pub fn from_operator(request: &OperatorChatPlanRequest) -> Self {
        Self {
            selected_tools: request.selected_tools.clone(),
            selected_worker_environment_kinds: request.selected_worker_environment_kinds.clone(),
            door_host_slots: request.door_host_slots.clone(),
            external_inputs: request.external_inputs.clone(),
            external_relays: request.external_relays.clone(),
            model_outputs: request.model_outputs.clone(),
            context_artifact_bindings: request.context_artifact_bindings.clone(),
            payload_artifact_bindings: request.payload_artifact_bindings.clone(),
        }
    }

    pub fn apply_operator(self, request: &mut OperatorChatPlanRequest) {
        request.selected_tools = self.selected_tools;
        request.selected_worker_environment_kinds = self.selected_worker_environment_kinds;
        request.door_host_slots = self.door_host_slots;
        request.external_inputs = self.external_inputs;
        request.external_relays = self.external_relays;
        request.model_outputs = self.model_outputs;
        request.context_artifact_bindings = self.context_artifact_bindings;
        request.payload_artifact_bindings = self.payload_artifact_bindings;
    }

    pub fn from_profile(profile: &OctopusOsAuthoringProfile) -> Self {
        Self {
            selected_tools: profile.selected_tools.clone(),
            selected_worker_environment_kinds: profile.selected_worker_environment_kinds.clone(),
            door_host_slots: profile.door_host_slots.clone(),
            external_inputs: profile.external_inputs.clone(),
            external_relays: profile.external_relays.clone(),
            model_outputs: profile.model_outputs.clone(),
            context_artifact_bindings: profile.context_artifact_bindings.clone(),
            payload_artifact_bindings: profile.payload_artifact_bindings.clone(),
        }
    }

    pub fn apply_profile(self, profile: &mut OctopusOsAuthoringProfile) {
        profile.selected_tools = self.selected_tools;
        profile.selected_worker_environment_kinds = self.selected_worker_environment_kinds;
        profile.door_host_slots = self.door_host_slots;
        profile.external_inputs = self.external_inputs;
        profile.external_relays = self.external_relays;
        profile.model_outputs = self.model_outputs;
        profile.context_artifact_bindings = self.context_artifact_bindings;
        profile.payload_artifact_bindings = self.payload_artifact_bindings;
    }

    fn merge(&mut self, next: Self) -> Result<(), String> {
        merge_exact(
            &mut self.selected_tools,
            next.selected_tools,
            |value| &value.tool,
            "tool",
        )?;
        merge_exact(
            &mut self.selected_worker_environment_kinds,
            next.selected_worker_environment_kinds,
            |value| value,
            "worker environment kind",
        )?;
        merge_exact(
            &mut self.external_inputs,
            next.external_inputs,
            |value| &value.checkpoint_id,
            "external input",
        )?;
        merge_exact(
            &mut self.external_relays,
            next.external_relays,
            |value| &value.checkpoint_id,
            "external relay",
        )?;
        merge_exact(
            &mut self.model_outputs,
            next.model_outputs,
            |value| &value.checkpoint_id,
            "model output",
        )?;
        merge_exact(
            &mut self.context_artifact_bindings,
            next.context_artifact_bindings,
            |value| &value.id,
            "context artifact",
        )?;
        merge_exact(
            &mut self.payload_artifact_bindings,
            next.payload_artifact_bindings,
            |value| value.id.as_str(),
            "payload artifact",
        )?;
        for (door, slots) in next.door_host_slots {
            let target = self.door_host_slots.entry(door.clone()).or_default();
            for (slot, path) in slots {
                if target.get(&slot).is_some_and(|prior| prior != &path) {
                    return Err(format!(
                        "publication selections conflict on door '{door}' slot '{slot}'"
                    ));
                }
                target.insert(slot, path);
            }
        }
        self.validate()
    }

    fn validate(&self) -> Result<(), String> {
        let mut checkpoints = BTreeSet::new();
        for id in self
            .external_inputs
            .iter()
            .map(|value| &value.checkpoint_id)
            .chain(
                self.external_relays
                    .iter()
                    .map(|value| &value.checkpoint_id),
            )
            .chain(self.model_outputs.iter().map(|value| &value.checkpoint_id))
        {
            crate::safe_segment(id, "publication checkpoint")?;
            if !checkpoints.insert(id) {
                return Err(format!(
                    "publication selections conflict on checkpoint '{id}'"
                ));
            }
        }
        let contexts = self
            .context_artifact_bindings
            .iter()
            .map(|value| value.id.as_str())
            .collect::<BTreeSet<_>>();
        for reference in &self.payload_artifact_bindings {
            if contexts.contains(reference.id.as_str()) {
                return Err(format!(
                    "publication payload '{}' collides with an instruction context binding",
                    reference.id
                ));
            }
        }
        octopus_run_authoring::validate_payload_artifact_selection(
            &self.payload_artifact_bindings,
        )?;
        for (door, slots) in &self.door_host_slots {
            crate::safe_segment(door, "publication door")?;
            for (slot, path) in slots {
                crate::safe_segment(slot, "publication slot")?;
                crate::require_absolute("publication root path", path)?;
            }
        }
        Ok(())
    }
}

fn merge_exact<T: PartialEq>(
    target: &mut Vec<T>,
    next: Vec<T>,
    key: impl Fn(&T) -> &str,
    field: &str,
) -> Result<(), String> {
    let mut indexes = BTreeMap::new();
    for (index, value) in target.iter().enumerate() {
        let id = key(value).to_string();
        if indexes.insert(id.clone(), index).is_some() {
            return Err(format!("selection repeats exact {field} '{id}'"));
        }
    }
    for value in next {
        let id = key(&value).to_string();
        if let Some(index) = indexes.get(&id) {
            if target[*index] != value {
                return Err(format!("publication selections conflict on {field} '{id}'"));
            }
        } else {
            indexes.insert(id, target.len());
            target.push(value);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationSelectionEvidence {
    pub pin: CollectionSelection,
    pub entry: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_sources: Vec<ArtifactRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationDefaultDeclaration {
    pub interface: InterfaceId,
    pub descriptor_hash: String,
    pub publications: Vec<CollectionSelection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationExpansionReceipt {
    pub requested: OctopusOsAuthoringProfile,
    pub publications: Vec<CollectionSelection>,
    pub defaults: Vec<PublicationDefaultDeclaration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reused_defaults: Vec<CollectionSelection>,
    pub selected: Vec<PublicationSelectionEvidence>,
    pub expanded: OctopusOsAuthoringProfile,
}

pub fn validate_publication_expansion_receipt(
    registry_root: &Path,
    receipt: &PublicationExpansionReceipt,
) -> Result<(), String> {
    let mut declarations = BTreeSet::new();
    let mut pins = receipt.publications.clone();
    for declaration in &receipt.defaults {
        if !declarations.insert(&declaration.interface) {
            return Err("publication expansion repeats a default owner".into());
        }
        let current =
            declared_publication_defaults(registry_root, [declaration.interface.clone()])?;
        if current.as_slice() != [declaration.clone()] {
            return Err("publication defaults changed during expansion planning".into());
        }
        pins.extend(declaration.publications.iter().cloned());
    }
    let mut exact = BTreeMap::new();
    for pin in pins {
        let key = (pin.source.id.clone(), pin.entry_id.clone());
        if exact.get(&key).is_some_and(|prior| prior != &pin) {
            return Err("publication expansion pins conflicting entries".into());
        }
        exact.insert(key, pin);
    }
    let mut selection = PublicationSelectionSeeds::from_profile(&receipt.requested);
    for pin in &receipt.reused_defaults {
        if receipt.publications.contains(pin)
            || exact
                .remove(&(pin.source.id.clone(), pin.entry_id.clone()))
                .as_ref()
                != Some(pin)
        {
            return Err("retained default is not one implicit exact default selection".into());
        }
        let retained = receipt
            .requested
            .publication_evidence
            .iter()
            .find(|evidence| evidence.pin == *pin)
            .ok_or("retained default has no accepted profile evidence")?;
        validate_evidence(retained)?;
        let before = selection.clone();
        expand_evidence(&mut selection, retained)?;
        if selection != before {
            return Err("retained default would add new selections to its accepted profile".into());
        }
    }
    for evidence in &receipt.selected {
        validate_evidence(evidence)?;
        if exact
            .remove(&(
                evidence.pin.source.id.clone(),
                evidence.pin.entry_id.clone(),
            ))
            .as_ref()
            != Some(&evidence.pin)
        {
            return Err(
                "publication expansion evidence is not one exact requested/default selection"
                    .into(),
            );
        }
        expand_evidence(&mut selection, evidence)?;
    }
    if !exact.is_empty() {
        return Err("publication expansion omits requested/default entry evidence".into());
    }
    let mut expected = receipt.requested.clone();
    selection.apply_profile(&mut expected);
    expected.publication_evidence =
        retain_publication_evidence(&receipt.requested.publication_evidence, &receipt.selected)?;
    if expected != receipt.expanded {
        return Err(
            "publication expansion result differs from its checked ordinary selections".into(),
        );
    }
    Ok(())
}

fn expand_evidence(
    selection: &mut PublicationSelectionSeeds,
    evidence: &PublicationSelectionEvidence,
) -> Result<(), String> {
    let entry = parse_entry(&evidence.entry).map_err(|error| error.to_string())?;
    let addition = serde_json::from_value(
        normalized_selection(&entry.selection).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    selection.merge(addition)?;
    if let Some(snapshot) = &evidence.snapshot {
        selection.merge(PublicationSelectionSeeds {
            context_artifact_bindings: vec![ArtifactBindingSeed {
                id: snapshot.id.to_string(),
            }],
            ..Default::default()
        })?;
    }
    Ok(())
}

pub fn validate_retained_snapshots(
    registry_root: &Path,
    evidence: &[PublicationSelectionEvidence],
) -> Result<(), String> {
    let mut snapshots = Vec::new();
    for evidence in evidence {
        validate_evidence(evidence)?;
        if let Some(snapshot) = &evidence.snapshot {
            snapshots.push(snapshot.clone());
        }
    }
    validate_publication_snapshot_refs(registry_root, &snapshots)
}

pub fn validate_publication_snapshot_refs(
    registry_root: &Path,
    snapshots: &[ArtifactRef],
) -> Result<(), String> {
    let registry_root = canonical_registry_root(registry_root)?;
    let mut pins = BTreeMap::new();
    for snapshot in snapshots {
        if let Some(content) = pins.insert(&snapshot.id, snapshot.content) {
            if content != snapshot.content {
                return Err(format!(
                    "publication snapshot '{}' has conflicting pins",
                    snapshot.id
                ));
            }
            continue;
        }
        if let Some(content) = content_addressed_hash(snapshot.id.as_str()) {
            if content != snapshot.content {
                return Err(format!("publication content-addressed reference '{}' has a different pin", snapshot.id));
            }
            continue;
        }
        let value = read_descriptor_value(&registry_root, "artifact", snapshot.id.as_str())?;
        if ContentHash::of_canonical_json(&value) != snapshot.content {
            return Err(format!("publication snapshot '{}' changed", snapshot.id));
        }
    }
    Ok(())
}

pub enum PublicationSourceAuthority<'a> {
    Operator {
        context_bindings: &'a [ArtifactBindingSeed],
        payload_bindings: &'a [ArtifactRef],
    },
    Native {
        metadata: &'a InvocationMetadata,
    },
}

impl PublicationSourceAuthority<'_> {
    fn authorize(&self, source: &ArtifactRef) -> Result<(), String> {
        match self {
            Self::Operator {
                context_bindings,
                payload_bindings,
            } => {
                let explicit = context_bindings
                    .iter()
                    .filter(|binding| binding.id == source.id.as_str())
                    .count();
                let payloads = payload_bindings
                    .iter()
                    .filter(|reference| reference.id == source.id)
                    .collect::<Vec<_>>();
                if explicit > 1 || payloads.len() > 1 || (explicit > 0 && !payloads.is_empty()) {
                    return Err("publication source has ambiguous explicit bindings".into());
                }
                if explicit != 1 && payloads.as_slice() != [source] {
                    return Err(format!(
                        "publication source '{}' requires an explicit or descriptor-dependent context binding",
                        source.id
                    ));
                }
            }
            Self::Native { metadata } => {
                if metadata.runtime.is_none()
                    || !metadata.grant.may_read_artifact(source.id.as_str())
                {
                    return Err(
                        "publication source is not readable in the admitted native input".into(),
                    );
                }
                let admitted = metadata
                    .artifact_binding_refs
                    .iter()
                    .filter(|reference| reference.id == source.id)
                    .collect::<Vec<_>>();
                if admitted.as_slice() != [source] {
                    return Err(
                        "publication source requires one exact admitted native binding ref".into(),
                    );
                }
            }
        }
        Ok(())
    }

    fn caller_scope(&self) -> Option<ScopeContext> {
        match self {
            Self::Native { metadata } => metadata.runtime.as_ref().map(|runtime| ScopeContext {
                ee_id: runtime.ee.to_string(),
                worker_interface: runtime.worker.to_string(),
            }),
            Self::Operator { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct ResolvedPublicationSelection {
    pub selection: PublicationSelectionSeeds,
    pub evidence: Vec<PublicationSelectionEvidence>,
    pub artifact_writes: Vec<PlannedClientWrite>,
    pub staged_artifacts: BTreeMap<String, Value>,
}

pub fn resolve_publication_selection(
    registry_root: &Path,
    target: &ScopeContext,
    publications: &[CollectionSelection],
    mut seeds: PublicationSelectionSeeds,
    authority: PublicationSourceAuthority<'_>,
    _write_hash_algorithm: WriteHashAlgorithm,
) -> Result<ResolvedPublicationSelection, String> {
    let requested_root = registry_root;
    let canonical_root = canonical_registry_root(registry_root)?;
    let registry_root = canonical_root.as_path();
    let mut evidence = Vec::new();
    let mut artifact_writes = Vec::new();
    let mut staged_artifacts = BTreeMap::new();
    let mut selections: BTreeMap<(ArtifactId, String), CollectionSelection> = BTreeMap::new();
    let mut collections = BTreeMap::new();
    for pin in publications {
        authority.authorize(&pin.source)?;
        let key = (pin.source.id.clone(), pin.entry_id.clone());
        if let Some(prior) = selections.get(&key) {
            if prior != pin {
                return Err(format!(
                    "publication selections conflict on entry '{}' in source '{}'",
                    pin.entry_id, pin.source.id
                ));
            }
            continue;
        }
        selections.insert(key, pin.clone());
        if let Some((source, _)) = collections.get(&pin.source.id) {
            if source != &pin.source {
                return Err("publication selections pin conflicting source revisions".into());
            }
        } else {
            let payload = octopus_context_collections::read_descriptor(
                &registry_root.join("artifact"),
                &pin.source,
            )
            .map_err(|error| error.to_string())?;
            let collection =
                BoundCollection::open(&registry_root.join("artifact"), &pin.source, &payload)
                    .map_err(|error| error.to_string())?;
            collections.insert(pin.source.id.clone(), (pin.source.clone(), collection));
        }
        let collection = &collections[&pin.source.id].1;
        if let Some(caller) = authority.caller_scope() {
            collection
                .resolve_metadata(pin, &caller)
                .map_err(|error| error.to_string())?;
        }
        let read = collection
            .read(pin, target)
            .map_err(|error| error.to_string())?;
        let entry = parse_entry(&read.entry).map_err(|error| error.to_string())?;
        for requirement in &entry.requires {
            validate_requirement(registry_root, &requirement.kind, &requirement.id)?;
        }
        let selected: PublicationSelectionSeeds = serde_json::from_value(
            normalized_selection(&entry.selection).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("publication '{}' selection: {error}", pin.entry_id))?;
        validate_selected_tools(registry_root, &selected.selected_tools)?;
        let context_sources =
            pin_context_sources(registry_root, &selected.context_artifact_bindings)?;
        seeds.merge(selected)?;
        let snapshot = if let Some(content) = entry
            .content
            .as_ref()
            .filter(|content| content.text.as_ref().is_some_and(|text| !text.is_empty()))
        {
            let value = snapshot_descriptor(
                &read.pin,
                &read.entry,
                &entry.description,
                content,
                &context_sources,
            );
            let id = value["id"]
                .as_str()
                .ok_or("publication snapshot has no identity")?
                .to_string();
            let reference = ArtifactRef {
                id: ArtifactId::new(&id),
                content: ContentHash::of_canonical_json(&value),
            };
            plan_snapshot(
                registry_root,
                requested_root,
                &reference,
                &value,
                &mut artifact_writes,
            )?;
            staged_artifacts.insert(id.clone(), value);
            seeds.merge(PublicationSelectionSeeds {
                context_artifact_bindings: vec![ArtifactBindingSeed { id }],
                ..Default::default()
            })?;
            Some(reference)
        } else {
            None
        };
        evidence.push(PublicationSelectionEvidence {
            pin: read.pin,
            entry: read.entry,
            snapshot,
            context_sources,
        });
    }
    Ok(ResolvedPublicationSelection {
        selection: seeds,
        evidence,
        artifact_writes,
        staged_artifacts,
    })
}

pub fn retain_publication_evidence(
    retained: &[PublicationSelectionEvidence],
    additions: &[PublicationSelectionEvidence],
) -> Result<Vec<PublicationSelectionEvidence>, String> {
    let mut result = retained.to_vec();
    for evidence in retained.iter().chain(additions) {
        validate_evidence(evidence)?;
    }
    for evidence in additions {
        if let Some(prior) = result.iter().find(|prior| {
            prior.pin.source.id == evidence.pin.source.id
                && prior.pin.entry_id == evidence.pin.entry_id
        }) {
            if prior != evidence {
                return Err("additive publication selection cannot replace a retained entry revision or its evidence".into());
            }
        } else {
            result.push(evidence.clone());
        }
    }
    Ok(result)
}

pub fn validate_evidence(evidence: &PublicationSelectionEvidence) -> Result<(), String> {
    let entry = parse_entry(&evidence.entry).map_err(|error| error.to_string())?;
    if entry.id != evidence.pin.entry_id
        || ContentHash::of_canonical_json(&evidence.entry) != evidence.pin.revision
    {
        return Err("publication evidence differs from its selected exact revision".into());
    }
    crate::context_bindings::validate_artifact_id(evidence.pin.source.id.as_str())?;
    let mut sources = BTreeSet::new();
    for source in &evidence.context_sources {
        crate::context_bindings::validate_artifact_id(source.id.as_str())?;
        if !sources.insert(&source.id) {
            return Err("publication evidence repeats a context source".into());
        }
    }
    match entry
        .content
        .as_ref()
        .filter(|content| content.text.as_ref().is_some_and(|text| !text.is_empty()))
    {
        Some(content) => {
            let expected = snapshot_descriptor(
                &evidence.pin,
                &evidence.entry,
                &entry.description,
                content,
                &evidence.context_sources,
            );
            if evidence.snapshot.as_ref().is_none_or(|reference| {
                reference.id.as_str() != expected["id"].as_str().unwrap_or("")
                    || reference.content != ContentHash::of_canonical_json(&expected)
            }) {
                return Err("publication evidence has no exact instruction snapshot pin".into());
            }
        }
        None if evidence.snapshot.is_some() => {
            return Err(
                "publication evidence claims an instruction snapshot without inline text".into(),
            );
        }
        None => {}
    }
    Ok(())
}

fn snapshot_descriptor(
    pin: &CollectionSelection,
    entry: &Value,
    description: &str,
    content: &octopus_context_collections::EntryContent,
    context_sources: &[ArtifactRef],
) -> Value {
    let id = format!(
        "artifact-publication-{}",
        ContentHash::of_canonical_json(&json!({"pin":pin,"context_sources":context_sources}))
            .to_hex()
    );
    json!({"id":id,"label":"context.reference","description":description,"text":content.text,"evidence":content.evidence,
        "publication_selection":{"pin":pin,"entry":entry,"context_sources":context_sources}})
}

pub fn declared_publications(
    registry_root: &Path,
    selected_interfaces: impl IntoIterator<Item = InterfaceId>,
    explicit: &[CollectionSelection],
) -> Result<Vec<CollectionSelection>, String> {
    let mut result = explicit.to_vec();
    result.extend(
        declared_publication_defaults(registry_root, selected_interfaces)?
            .into_iter()
            .flat_map(|default| default.publications),
    );
    Ok(result)
}

pub fn declared_publication_defaults(
    registry_root: &Path,
    selected_interfaces: impl IntoIterator<Item = InterfaceId>,
) -> Result<Vec<PublicationDefaultDeclaration>, String> {
    let registry_root = canonical_registry_root(registry_root)?;
    let mut result = Vec::new();
    for interface in selected_interfaces.into_iter().collect::<BTreeSet<_>>() {
        let value = read_descriptor_value(&registry_root, "interface", interface.as_str())?;
        let descriptor_hash = ContentHash::of_canonical_json(&value).to_hex();
        let descriptor: octopus_interfaces::InterfaceDescriptor =
            octopus_descriptors::parse_strict(&octopus_descriptors::canonical_bytes(&value))
                .map_err(|error| error.to_string())?;
        if descriptor.declared_name() != Some(interface.as_str()) {
            return Err("publication default owner has a different typed identity".into());
        }
        if let Some(value) = descriptor
            .interface_driver_config
            .as_ref()
            .and_then(|config| config.private_config.get("publications"))
        {
            let defaults: Vec<CollectionSelection> = serde_json::from_value(value.clone())
                .map_err(|error| format!("interface '{interface}' publications: {error}"))?;
            if !defaults.is_empty() {
                result.push(PublicationDefaultDeclaration {
                    interface,
                    descriptor_hash,
                    publications: defaults,
                });
            }
        }
    }
    Ok(result)
}

pub fn validate_target_read_formation(
    registry_root: &Path,
    ee_instances_root: &Path,
    ee_id: &str,
    worker_interface: &str,
) -> Result<(), String> {
    let registry_root = canonical_registry_root(registry_root)?;
    let chain = crate::capability_formation::resolve_live_ee_policy_chain(
        ee_instances_root,
        &octopus_descriptors::EeId::new(ee_id),
    )
    .map_err(|error| error.to_string())?;
    let path = descriptor_path(&registry_root, "interface", worker_interface)?
        .with_file_name("policy.json");
    regular_path(&path)?;
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(path, MAX_DESCRIPTOR_BYTES)
        .map_err(|error| error.to_string())?;
    let worker: octopus_policy::WorkerInterfacePolicy =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if worker.id.as_str() != worker_interface {
        return Err("publication target worker policy has a different identity".into());
    }
    let mut layers = Vec::new();
    for layer in chain {
        let template: octopus_ee::EeTemplateDescriptor =
            read_descriptor(&registry_root, "ee_template", layer.template.as_str())?;
        layers.push(template.as_policy_layer());
        layers.push(layer.policy_narrowing.as_policy_layer());
    }
    let formed =
        octopus_policy::form_policy(worker.ceiling_grant(), &layers, &[], Default::default());
    if !formed
        .context
        .artifact_verbs
        .contains(&octopus_policy::ArtifactVerb::Read)
    {
        return Err("publication target formation denies artifact read".into());
    }
    Ok(())
}

fn content_addressed_hash(id: &str) -> Option<ContentHash> {
    id.rsplit_once(':').and_then(|(label, hash)| {
        (!label.trim().is_empty()).then(|| ContentHash::from_hex(hash)).flatten()
    })
}

fn pin_context_sources(
    registry_root: &Path,
    selected: &[ArtifactBindingSeed],
) -> Result<Vec<ArtifactRef>, String> {
    let mut sources: BTreeMap<ArtifactId, ArtifactRef> = BTreeMap::new();
    let mut pending = selected
        .iter()
        .map(|binding| binding.id.clone())
        .collect::<Vec<_>>();
    let mut inspected = BTreeSet::new();
    let mut offset = 0;
    while offset < pending.len() {
        let id = pending[offset].clone();
        offset += 1;
        if !inspected.insert(id.clone()) {
            continue;
        }
        if inspected.len() > 1024 {
            return Err("publication context-source closure exceeds 1024 exact refs".into());
        }
        if let Some(content) = content_addressed_hash(&id) {
            insert_source(
                &mut sources,
                ArtifactRef {
                    id: ArtifactId::new(&id),
                    content,
                },
            )?;
            continue;
        }
        let value = read_descriptor_value(registry_root, "artifact", &id)?;
        let reference = ArtifactRef {
            id: ArtifactId::new(&id),
            content: ContentHash::of_canonical_json(&value),
        };
        insert_source(&mut sources, reference)?;
        if value.get("label").and_then(Value::as_str) != Some("context.reference") {
            continue;
        }
        let Some(value) = value.get("sources") else {
            continue;
        };
        let refs: Vec<ArtifactRef> =
            serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        for reference in refs {
            if reference.id.as_str() == id {
                return Err("publication instruction source references itself".into());
            }
            pending.push(reference.id.to_string());
            insert_source(&mut sources, reference)?;
        }
    }
    Ok(sources.into_values().collect())
}

fn insert_source(
    sources: &mut BTreeMap<ArtifactId, ArtifactRef>,
    reference: ArtifactRef,
) -> Result<(), String> {
    if sources
        .get(&reference.id)
        .is_some_and(|prior| prior != &reference)
    {
        return Err(format!(
            "publication instruction source '{}' has conflicting canonical pins",
            reference.id
        ));
    }
    sources.insert(reference.id.clone(), reference);
    Ok(())
}

fn validate_selected_tools(registry_root: &Path, tools: &[SelectedToolSeed]) -> Result<(), String> {
    for selected in tools {
        let tool: octopus_tool_registry::ToolDescriptor =
            read_descriptor(registry_root, "tool", &selected.tool)?;
        if tool.interface.as_str() != selected.interface
            || tool.supported_worker_environment_kinds.len() != 1
            || tool.supported_worker_environment_kinds[0].as_str()
                != selected.worker_environment_kind
        {
            return Err(format!(
                "publication tool '{}' does not match its ordinary descriptor owner/code-world",
                selected.tool
            ));
        }
    }
    Ok(())
}

fn validate_requirement(registry_root: &Path, kind: &str, id: &str) -> Result<(), String> {
    match kind {
        "interface" => {
            read_descriptor::<octopus_interfaces::InterfaceDescriptor>(registry_root, kind, id)?;
        }
        "tool" => {
            read_descriptor::<octopus_tool_registry::ToolDescriptor>(registry_root, kind, id)?;
        }
        "worker_environment_kind" => {
            read_descriptor::<octopus_worker_env::WorkerEnvKindSpec>(registry_root, kind, id)?;
        }
        "ee_template" => {
            read_descriptor::<octopus_ee::EeTemplateDescriptor>(registry_root, kind, id)?;
        }
        "mark" => {
            read_descriptor::<octopus_marks::MarkDescriptor>(registry_root, kind, id)?;
        }
        "verifier" => {
            read_descriptor::<octopus_verifiers::VerifierDescriptor>(registry_root, kind, id)?;
        }
        "artifact" => {
            let path = descriptor_path(registry_root, kind, id)?;
            regular_path(&path)?;
            if !std::fs::symlink_metadata(&path)
                .map_err(|error| error.to_string())?
                .is_file()
            {
                return Err(format!(
                    "publication requires regular artifact descriptor '{id}'"
                ));
            }
        }
        _ => {
            return Err(format!(
                "unsupported publication descriptor requirement '{kind}'"
            ));
        }
    }
    Ok(())
}

fn read_descriptor<T: Descriptor>(root: &Path, kind: &str, id: &str) -> Result<T, String> {
    let value = read_descriptor_value(root, kind, id)?;
    let descriptor: T =
        octopus_descriptors::parse_strict(&octopus_descriptors::canonical_bytes(&value))
            .map_err(|error| error.to_string())?;
    if T::KIND != kind || descriptor.declared_name() != Some(id) {
        return Err(format!(
            "publication descriptor '{kind}/{id}' has a different typed owner identity"
        ));
    }
    Ok(descriptor)
}

fn canonical_registry_root(root: &Path) -> Result<PathBuf, String> {
    crate::require_absolute("publication registry root", root)?;
    std::fs::canonicalize(root)
        .map_err(|error| format!("publication registry root '{}': {error}", root.display()))
}

fn descriptor_path(root: &Path, kind: &str, id: &str) -> Result<PathBuf, String> {
    crate::require_absolute("publication registry root", root)?;
    crate::context_bindings::validate_artifact_id(kind)?;
    crate::context_bindings::validate_artifact_id(id)?;
    Ok(root.join(kind).join(id).join(format!("{kind}.json")))
}

fn read_descriptor_value(root: &Path, kind: &str, id: &str) -> Result<Value, String> {
    let path = descriptor_path(root, kind, id)?;
    regular_path(&path)?;
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(&path, MAX_DESCRIPTOR_BYTES)
        .map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if !value.is_object()
        || (kind == "artifact" && value.get("id").and_then(Value::as_str) != Some(id))
    {
        return Err(format!(
            "publication descriptor '{kind}/{id}' has a different identity"
        ));
    }
    Ok(value)
}

fn regular_path(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            _ => return Err(format!("unsafe publication path '{}'", path.display())),
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "publication path '{}' contains a symlink",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn plan_snapshot(
    root: &Path,
    requested_root: &Path,
    reference: &ArtifactRef,
    value: &Value,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<(), String> {
    let path = descriptor_path(root, "artifact", reference.id.as_str())?;
    regular_path(&path)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let existing = read_descriptor_value(root, "artifact", reference.id.as_str())?;
            if existing != *value {
                return Err(format!(
                    "publication snapshot '{}' already has conflicting content",
                    reference.id
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            writes.push(PlannedClientWrite {
                path: descriptor_path(requested_root, "artifact", reference.id.as_str())?,
                content: String::from_utf8(octopus_descriptors::canonical_bytes(value))
                    .map_err(|error| error.to_string())?,
                tag: format!("artifact:{}", reference.id),
                expected_before_hash: None,
            })
        }
        Err(error) => return Err(error.to_string()),
    }
    Ok(())
}
