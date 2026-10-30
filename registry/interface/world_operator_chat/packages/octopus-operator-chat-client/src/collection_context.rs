use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use octopus_context_collections::{
    BoundCollection, CatalogueLimits, CollectionCatalogue, MAX_DESCRIPTOR_BYTES, ScopeContext,
    UninspectedBinding,
};
use octopus_descriptors::{ArtifactId, EeId, InterfaceId};
use octopus_ledger::{ArtifactRef, ContentHash, InvocationMetadata};
use octopus_policy::{
    ArtifactAccess, ContextGrant, PolicyLayer, WorkerInterfacePolicy, form_policy,
};
use octopus_run_record::RunRecord;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ArtifactBindingSeed, PlannedClientWrite, PlannedIdentity};

const ADOPTION: &str = "collection_catalogue_adoption";
const INLINE_SELECTION: &str = "collection_catalogue_instruction_artifacts";

pub(crate) fn configure_inline_selection(
    config: &mut serde_json::Map<String, Value>,
    target: &[String],
) -> Result<(), String> {
    let previous: Vec<String> = config
        .get(INLINE_SELECTION)
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| format!("{INLINE_SELECTION} is malformed: {error}"))
        })
        .transpose()?
        .unwrap_or_default();
    if previous.is_empty() && target.is_empty() {
        return Ok(());
    }
    let mut selected: Vec<String> = config
        .get("instruction_artifacts")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| format!("instruction_artifacts is malformed: {error}"))
        })
        .transpose()?
        .unwrap_or_default();
    for ids in [&previous[..], &selected[..], target] {
        let mut seen = BTreeSet::new();
        for id in ids {
            crate::context_bindings::validate_artifact_id(id)?;
            if !seen.insert(id) {
                return Err("catalogue inline selection repeats an exact id".into());
            }
        }
    }
    if !previous.iter().all(|id| selected.contains(id)) {
        return Err("owned catalogue is absent from its explicit inline selection".into());
    }
    if previous == target {
        return Ok(());
    }
    selected.retain(|id| !previous.contains(id));
    for id in target {
        if !selected.contains(id) {
            selected.push(id.clone());
        }
    }
    config.insert("instruction_artifacts".into(), json!(selected));
    if target.is_empty() {
        config.remove(INLINE_SELECTION);
    } else {
        config.insert(INLINE_SELECTION.into(), json!(target));
    }
    Ok(())
}

pub enum CaptureAuthority<'a> {
    Operator,
    Native(&'a InvocationMetadata),
}

impl CaptureAuthority<'_> {
    fn admitted(&self, id: &str) -> Result<Option<&ArtifactRef>, String> {
        let Self::Native(metadata) = self else {
            return Ok(None);
        };
        let refs = metadata
            .artifact_binding_refs
            .iter()
            .filter(|reference| reference.id.as_str() == id)
            .collect::<Vec<_>>();
        if refs.len() > 1 {
            return Err(format!(
                "catalogue source '{id}' has ambiguous admitted refs"
            ));
        }
        Ok(refs.first().copied())
    }

    fn may_read(&self, id: &str) -> bool {
        match self {
            Self::Operator => true,
            Self::Native(metadata) => {
                metadata.runtime.is_some() && metadata.grant.may_read_artifact(id)
            }
        }
    }

    fn scope(&self) -> Option<ScopeContext> {
        match self {
            Self::Operator => None,
            Self::Native(metadata) => metadata.runtime.as_ref().map(|runtime| ScopeContext {
                ee_id: runtime.ee.to_string(),
                worker_interface: runtime.worker.to_string(),
            }),
        }
    }
}

pub fn capture(
    registry_root: &Path,
    scope: &ScopeContext,
    bindings: &[ArtifactBindingSeed],
    target_grant: &ContextGrant,
    authority: CaptureAuthority<'_>,
    limits: CatalogueLimits,
) -> Result<Option<Value>, String> {
    let canonical_root = canonical_registry_root(registry_root)?;
    let registry_root = canonical_root.as_path();
    let mut catalogue = CollectionCatalogue {
        schema_version: 1,
        scope: scope.clone(),
        author_scope: authority.scope(),
        limits,
        sources: Vec::new(),
        uninspected_bindings: Vec::new(),
    };
    let ids = bindings
        .iter()
        .map(|binding| binding.id.as_str())
        .collect::<BTreeSet<_>>();
    if ids.len() > 1024 {
        return Err("catalogue binding set exceeds 1024 exact ids".into());
    }
    let mut remaining = catalogue.limits.max_scan_bytes;
    for id in ids {
        crate::context_bindings::validate_artifact_id(id)?;
        if id
            .rsplit_once(':')
            .is_some_and(|(_, hash)| ContentHash::from_hex(hash).is_some())
        {
            continue;
        }
        if !target_grant
            .artifact_verbs
            .contains(&octopus_policy::ArtifactVerb::Read)
            || !target_grant
                .artifact_access
                .get(&ArtifactId::new(id))
                .is_some_and(|access| access.read)
        {
            return Err(format!("catalogue target formation denies read of '{id}'"));
        }
        let admitted = authority.admitted(id)?;
        if !authority.may_read(id)
            || matches!(authority, CaptureAuthority::Native(_)) && admitted.is_none()
        {
            catalogue.uninspected_bindings.push(UninspectedBinding {
                id: id.into(),
                reason: "not_readable_in_exact_author_input".into(),
            });
            continue;
        }
        let path = artifact_path(registry_root, id)?;
        let (value, bytes) = read_json(&path, remaining)?;
        remaining = remaining
            .checked_sub(bytes)
            .ok_or("catalogue aggregate scan budget exceeded")?;
        let source = ArtifactRef {
            id: ArtifactId::new(id),
            content: ContentHash::of_canonical_json(&value),
        };
        if admitted.is_some_and(|reference| reference != &source) {
            return Err(format!(
                "catalogue source '{id}' changed from its admitted ref"
            ));
        }
        if value.get("label").and_then(Value::as_str) != Some("context.collection") {
            continue;
        }
        if value.get("id").and_then(Value::as_str) != Some(id) {
            return Err(format!("catalogue binding '{id}' has a different identity"));
        }
        if bytes > MAX_DESCRIPTOR_BYTES {
            return Err("collection descriptor exceeds its byte bound".into());
        }
        if catalogue.sources.len() >= catalogue.limits.max_sources {
            return Err("catalogue source count exceeds its aggregate limit".into());
        }
        remaining = remaining
            .checked_sub(3 * MAX_DESCRIPTOR_BYTES)
            .ok_or("catalogue aggregate descriptor-check budget exceeded")?;
        let collection = BoundCollection::open(&registry_root.join("artifact"), &source, &value)
            .map_err(|error| error.to_string())?;
        let limit = collection.descriptor().limits.max_entries.min(32);
        let (page, scanned) = collection
            .catalogue_page(scope, catalogue.author_scope.as_ref(), limit, remaining)
            .map_err(|error| error.to_string())?;
        remaining = remaining
            .checked_sub(scanned)
            .ok_or("catalogue aggregate scan budget exceeded")?;
        catalogue.sources.push(page);
    }
    if catalogue.sources.is_empty() && catalogue.uninspected_bindings.is_empty() {
        return Ok(None);
    }
    catalogue
        .descriptor()
        .map(Some)
        .map_err(|error| error.to_string())
}

pub(crate) struct PlanContext<'a> {
    pub registry_root: &'a Path,
    pub ee_instances_root: &'a Path,
    pub ee_id: &'a str,
    pub identity: &'a PlannedIdentity,
    pub bindings: Vec<ArtifactBindingSeed>,
    pub native_metadata: Option<&'a InvocationMetadata>,
    pub existing: Option<&'a Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Adoption {
    schema_version: u32,
    scope: ScopeContext,
    snapshot: ArtifactRef,
}

pub(crate) struct CataloguePlan(Option<Adoption>);

impl CataloguePlan {
    pub(crate) fn inline_ids(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|adoption| adoption.snapshot.id.to_string())
            .collect()
    }

    pub(crate) fn wire(&self, provider: &str, content: String) -> Result<String, String> {
        let Some(adoption) = &self.0 else {
            return Ok(content);
        };
        let mut record: Value =
            serde_json::from_str(&content).map_err(|error| error.to_string())?;
        record["interface_state"][provider][ADOPTION] = json!(adoption);
        crate::pretty_json(&record)
    }
}

pub(crate) fn plan(
    context: PlanContext<'_>,
    selected_context: &mut Vec<ArtifactBindingSeed>,
    writes: &mut Vec<PlannedClientWrite>,
) -> Result<CataloguePlan, String> {
    let canonical_root = canonical_registry_root(context.registry_root)?;
    let scope = ScopeContext {
        ee_id: context.ee_id.into(),
        worker_interface: context.identity.worker_interface_id.clone(),
    };
    let authority = context
        .native_metadata
        .map(CaptureAuthority::Native)
        .unwrap_or(CaptureAuthority::Operator);
    let adoption = if let Some(existing) = context.existing {
        let record = RunRecord::from_bytes(
            &serde_json::to_vec(existing).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let provider = InterfaceId::new(&context.identity.model_interface_id);
        let Some(value) = record
            .interface_state
            .get(&provider)
            .and_then(|state| state.get(ADOPTION))
        else {
            return Ok(CataloguePlan(None));
        };
        let adoption: Adoption =
            serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        if adoption.schema_version != 1 || adoption.scope.worker_interface != scope.worker_interface {
            return Err("accepted catalogue has a different worker identity".into());
        }
        let bindings =
            octopus_run_authoring::provider_context_bindings(&record, &provider)?;
        let bound = bindings
            .iter()
            .filter(|binding| binding.id == adoption.snapshot.id)
            .collect::<Vec<_>>();
        if !matches!(bound.as_slice(), [binding] if binding.access.read) {
            return Err("accepted catalogue is not uniquely bound for read".into());
        }
        let readable = match &authority {
            CaptureAuthority::Operator => true,
            CaptureAuthority::Native(_) => {
                authority.may_read(adoption.snapshot.id.as_str())
                    && authority.admitted(adoption.snapshot.id.as_str())?
                        == Some(&adoption.snapshot)
            }
        };
        if authority
            .admitted(adoption.snapshot.id.as_str())?
            .is_some_and(|reference| reference != &adoption.snapshot)
        {
            return Err(
                "accepted catalogue differs from the author's admitted snapshot ref".into(),
            );
        }
        // Retaining an admitted run's opaque snapshot is not a new source read.
        // Its registry bytes require the author's own exact read authority.
        if readable {
            let (value, _) = read_json(
                &artifact_path(&canonical_root, adoption.snapshot.id.as_str())?,
                MAX_DESCRIPTOR_BYTES,
            )?;
            if ContentHash::of_canonical_json(&value) != adoption.snapshot.content {
                return Err("accepted catalogue snapshot changed".into());
            }
            let catalogue = CollectionCatalogue::validate_descriptor(&value)
                .map_err(|error| error.to_string())?;
            if catalogue.scope != adoption.scope {
                return Err("accepted catalogue payload has a different capture scope".into());
            }
        }
        adoption
    } else {
        let staged = writes
            .iter()
            .filter_map(|write| {
                let value: Value = serde_json::from_str(&write.content).ok()?;
                (write.path.parent()?.parent()? == context.registry_root.join("artifact")
                    && value.get("label").and_then(Value::as_str) != Some("context.collection"))
                .then(|| value.get("id").and_then(Value::as_str).map(str::to_owned))
                .flatten()
            })
            .collect::<BTreeSet<_>>();
        let bindings = context
            .bindings
            .into_iter()
            .filter(|binding| !staged.contains(&binding.id))
            .collect::<Vec<_>>();
        let target_grant = target_grant(
            context.registry_root,
            context.ee_instances_root,
            &scope,
            &bindings,
            writes,
        )?;
        let Some(value) = capture(
            context.registry_root,
            &scope,
            &bindings,
            &target_grant,
            authority,
            CatalogueLimits::default(),
        )?
        else {
            return Ok(CataloguePlan(None));
        };
        let snapshot = ArtifactRef {
            id: ArtifactId::new(
                value["id"]
                    .as_str()
                    .ok_or("catalogue snapshot has no identity")?,
            ),
            content: ContentHash::of_canonical_json(&value),
        };
        let path = artifact_path(&canonical_root, snapshot.id.as_str())?;
        regular_path(&path)?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) if read_json(&path, MAX_DESCRIPTOR_BYTES)?.0 != value => {
                return Err("catalogue snapshot already has conflicting content".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                writes.push(PlannedClientWrite {
                    path: artifact_path(context.registry_root, snapshot.id.as_str())?,
                    content: String::from_utf8(octopus_descriptors::canonical_bytes(&value))
                        .map_err(|error| error.to_string())?,
                    tag: format!("artifact:{}", snapshot.id),
                    expected_before_hash: None,
                })
            }
            Err(error) => return Err(error.to_string()),
        }
        Adoption {
            schema_version: 1,
            scope: scope.clone(),
            snapshot,
        }
    };
    let snapshot_binding = ArtifactBindingSeed {
        id: adoption.snapshot.id.to_string(),
    };
    let grant = target_grant(
        context.registry_root,
        context.ee_instances_root,
        &scope,
        std::slice::from_ref(&snapshot_binding),
        writes,
    )?;
    if !grant
        .artifact_access
        .get(&adoption.snapshot.id)
        .is_some_and(|access| access.read)
    {
        return Err("catalogue snapshot is not readable by its target".into());
    }
    if !selected_context
        .iter()
        .any(|binding| binding.id == snapshot_binding.id)
    {
        selected_context.push(snapshot_binding);
    }
    Ok(CataloguePlan(Some(adoption)))
}

fn target_grant(
    root: &Path,
    ee_root: &Path,
    scope: &ScopeContext,
    bindings: &[ArtifactBindingSeed],
    writes: &[PlannedClientWrite],
) -> Result<ContextGrant, String> {
    let canonical_root = canonical_registry_root(root)?;
    let path = root
        .join("interface")
        .join(&scope.worker_interface)
        .join("policy.json");
    let value = match writes.iter().find(|write| write.path == path) {
        Some(write) => serde_json::from_str(&write.content).map_err(|error| error.to_string())?,
        None => {
            read_json(
                &canonical_root
                    .join("interface")
                    .join(&scope.worker_interface)
                    .join("policy.json"),
                MAX_DESCRIPTOR_BYTES,
            )?
            .0
        }
    };
    let worker: WorkerInterfacePolicy =
        serde_json::from_value(value).map_err(|error| error.to_string())?;
    if worker.id.as_str() != scope.worker_interface {
        return Err("catalogue target worker policy identity differs".into());
    }
    let mut layers = Vec::new();
    for layer in octopus_world_terminal_client::resolve_live_ee_policy_chain(
        ee_root,
        &EeId::new(&scope.ee_id),
    )
    .map_err(|error| error.to_string())?
    {
        let (value, _) = read_json(
            &canonical_root
                .join("ee_template")
                .join(layer.template.as_str())
                .join("ee_template.json"),
            MAX_DESCRIPTOR_BYTES,
        )?;
        let template: octopus_ee::EeTemplateDescriptor =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        layers.push(template.as_policy_layer());
        layers.push(layer.policy_narrowing.as_policy_layer());
    }
    let binding_layer = PolicyLayer {
        artifact_access: bindings
            .iter()
            .map(|binding| (ArtifactId::new(&binding.id), ArtifactAccess::read_only()))
            .collect::<BTreeMap<_, _>>(),
        ..Default::default()
    };
    Ok(form_policy(
        worker.ceiling_grant(),
        &layers,
        &[binding_layer],
        Default::default(),
    )
    .context)
}

fn artifact_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    crate::context_bindings::validate_artifact_id(id)?;
    Ok(root.join("artifact").join(id).join("artifact.json"))
}

fn canonical_registry_root(root: &Path) -> Result<PathBuf, String> {
    crate::require_absolute("catalogue registry root", root)?;
    std::fs::canonicalize(root)
        .map_err(|error| format!("catalogue registry root '{}': {error}", root.display()))
}

fn read_json(path: &Path, limit: u64) -> Result<(Value, u64), String> {
    regular_path(path)?;
    let bytes = octopus_interfaces::read_artifact_carrier_file_bounded(path, limit)
        .map_err(|error| format!("catalogue descriptor '{}': {error}", path.display()))?;
    let value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("catalogue descriptor '{}': {error}", path.display()))?;
    Ok((value, bytes.len() as u64))
}

fn regular_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("catalogue registry path must be absolute".into());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            _ => return Err(format!("unsafe catalogue path '{}'", path.display())),
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "catalogue path '{}' contains a symlink",
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
