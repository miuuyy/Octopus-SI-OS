use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::descriptor::WorkerEnvKindSpec;
use crate::formation::formation_pin;
use crate::{
    WekDescriptorHash, WorkerConsent, WorkerEnvCaps, WorkerEnvError, WorkerEnvironmentFormation,
    WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId,
};

struct PinnedWek {
    spec: Arc<WorkerEnvKindSpec>,
    hash: WekDescriptorHash,
}

#[derive(Default)]
pub struct WorkerEnvKindRegistry {
    by_hash: BTreeMap<WekDescriptorHash, PinnedWek>,
    head: BTreeMap<WorkerEnvironmentKindId, WekDescriptorHash>,
}

impl WorkerEnvKindRegistry {
    pub fn load_kind_from_dir(
        root: &Path,
        id: &WorkerEnvironmentKindId,
    ) -> Result<Self, WorkerEnvError> {
        let mut components = Path::new(id.as_str()).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(name)) if name.to_str() == Some(id.as_str()))
            || components.next().is_some()
            || id.as_str().trim().is_empty()
        {
            return Err(WorkerEnvError::UnknownKind(id.as_str().to_string()));
        }
        let file = root.join(id.as_str()).join("worker_environment_kind.json");
        let registry_root =
            std::fs::canonicalize(root).map_err(|error| WorkerEnvError::MalformedDescriptor {
                path: root.display().to_string(),
                reason: error.to_string(),
            })?;
        let owner = std::fs::canonicalize(root.join(id.as_str())).map_err(|error| {
            WorkerEnvError::MalformedDescriptor {
                path: file.display().to_string(),
                reason: error.to_string(),
            }
        })?;
        let metadata = std::fs::symlink_metadata(&file).map_err(|error| {
            WorkerEnvError::MalformedDescriptor {
                path: file.display().to_string(),
                reason: error.to_string(),
            }
        })?;
        if owner != registry_root.join(id.as_str())
            || !metadata.is_file()
            || metadata.file_type().is_symlink()
        {
            return Err(WorkerEnvError::MalformedDescriptor {
                path: file.display().to_string(),
                reason: "kind descriptor is not a regular file in its exact registry owner".into(),
            });
        }
        let raw = std::fs::read(&file).map_err(|error| WorkerEnvError::MalformedDescriptor {
            path: file.display().to_string(),
            reason: error.to_string(),
        })?;
        let (spec, hash) = Self::parse_one(&raw, &file.display().to_string())?;
        if spec.worker_environment_kind_id != *id {
            return Err(WorkerEnvError::MalformedDescriptor {
                path: file.display().to_string(),
                reason: format!("directory/body identity mismatch for '{id}'"),
            });
        }
        let mut registry = Self::default();
        registry.head.insert(id.clone(), hash.clone());
        registry.by_hash.insert(
            hash.clone(),
            PinnedWek {
                spec: Arc::new(spec),
                hash,
            },
        );
        Ok(registry)
    }

    fn parse_one(
        raw_json: &[u8],
        path_for_err: &str,
    ) -> Result<(WorkerEnvKindSpec, WekDescriptorHash), WorkerEnvError> {
        let spec =
            octopus_descriptors::parse_strict::<WorkerEnvKindSpec>(raw_json).map_err(|e| {
                WorkerEnvError::MalformedDescriptor {
                    path: path_for_err.to_string(),
                    reason: e.to_string(),
                }
            })?;
        let value =
            serde_json::to_value(&spec).map_err(|e| WorkerEnvError::MalformedDescriptor {
                path: path_for_err.to_string(),
                reason: e.to_string(),
            })?;
        let canonical = octopus_descriptors::canonical_bytes(&value);
        let hash = WekDescriptorHash::of_bytes(&canonical);
        Ok((spec, hash))
    }

    pub fn load_from_dir(root: &Path) -> Result<Self, WorkerEnvError> {
        let mut reg = Self::default();
        let entries = std::fs::read_dir(root).map_err(|e| WorkerEnvError::MalformedDescriptor {
            path: root.display().to_string(),
            reason: format!("cannot read dir: {e}"),
        })?;
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| WorkerEnvError::MalformedDescriptor {
                path: root.display().to_string(),
                reason: format!("cannot read directory entry: {error}"),
            })?;
            dirs.push(entry.path());
        }
        dirs.sort();
        for dir in dirs {
            if !dir.is_dir() {
                continue;
            }
            let file = dir.join("worker_environment_kind.json");
            if !file.exists() {
                continue;
            }
            let raw = std::fs::read(&file).map_err(|e| WorkerEnvError::MalformedDescriptor {
                path: file.display().to_string(),
                reason: format!("cannot read: {e}"),
            })?;
            let (spec, hash) = Self::parse_one(&raw, &file.display().to_string())?;
            let dir_name = dir
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| WorkerEnvError::MalformedDescriptor {
                    path: dir.display().to_string(),
                    reason: "worker_environment_kind directory name is not valid UTF-8".into(),
                })?;
            if spec.worker_environment_kind_id.as_str() != dir_name {
                return Err(WorkerEnvError::MalformedDescriptor {
                    path: file.display().to_string(),
                    reason: format!(
                        "directory/body identity mismatch: directory '{}' contains worker_environment_kind_id '{}'",
                        dir_name,
                        spec.worker_environment_kind_id.as_str()
                    ),
                });
            }
            let id = spec.worker_environment_kind_id.clone();
            if reg.head.contains_key(&id) {
                return Err(WorkerEnvError::DuplicateKind(id.into_string()));
            }
            reg.by_hash.insert(
                hash.clone(),
                PinnedWek {
                    spec: Arc::new(spec),
                    hash: hash.clone(),
                },
            );
            reg.head.insert(id, hash);
        }
        Ok(reg)
    }

    pub fn upsert(&mut self, raw_json: &[u8]) -> Result<WekDescriptorHash, WorkerEnvError> {
        let (spec, hash) = Self::parse_one(raw_json, "<upsert>")?;
        let id = spec.worker_environment_kind_id.clone();
        self.by_hash
            .entry(hash.clone())
            .or_insert_with(|| PinnedWek {
                spec: Arc::new(spec),
                hash: hash.clone(),
            });
        self.head.insert(id, hash.clone());
        Ok(hash)
    }

    pub fn get(
        &self,
        id: &WorkerEnvironmentKindId,
    ) -> Result<(Arc<WorkerEnvKindSpec>, WekDescriptorHash), WorkerEnvError> {
        let hash = self
            .head
            .get(id)
            .cloned()
            .ok_or_else(|| WorkerEnvError::UnknownKind(id.as_str().to_string()))?;
        let pinned = self
            .by_hash
            .get(&hash)
            .ok_or_else(|| WorkerEnvError::UnknownDescriptorHash(hash.clone()))?;
        Ok((pinned.spec.clone(), pinned.hash.clone()))
    }

    pub fn head_specs(&self) -> impl Iterator<Item = Arc<WorkerEnvKindSpec>> + '_ {
        self.head
            .values()
            .filter_map(move |hash| self.by_hash.get(hash).map(|pinned| pinned.spec.clone()))
    }

    pub fn get_by_hash(
        &self,
        hash: &WekDescriptorHash,
    ) -> Result<Arc<WorkerEnvKindSpec>, WorkerEnvError> {
        self.by_hash
            .get(hash)
            .map(|p| p.spec.clone())
            .ok_or_else(|| WorkerEnvError::UnknownDescriptorHash(hash.clone()))
    }

    pub fn form(
        &self,
        request: &WorkerEnvironmentFormationRequest<'_>,
    ) -> Result<WorkerEnvironmentFormation, WorkerEnvError> {
        if request.tool.trim().is_empty()
            || request.ee.trim().is_empty()
            || request.run.trim().is_empty()
            || request.door_descriptor_hash.trim().is_empty()
            || request.door_driver_ref.trim().is_empty()
        {
            return Err(WorkerEnvError::NoCompatibleFormation(
                "formation request has an empty identity field".to_string(),
            ));
        }
        for (name, path) in request.door_root_bindings {
            if name.trim().is_empty() || !path.is_absolute() {
                return Err(WorkerEnvError::MalformedFormation(format!(
                    "door root binding '{name}' is not a named absolute path: {}",
                    path.display()
                )));
            }
        }

        let mut candidates = Vec::new();
        for hash in self.head.values() {
            let Some(pinned) = self.by_hash.get(hash) else {
                continue;
            };
            let spec = &pinned.spec;
            let granted = request
                .granted_worker_environment_kinds
                .iter()
                .any(|id| id == &spec.worker_environment_kind_id);
            let tool_declares_kind = request
                .tool_worker_environment_kinds
                .iter()
                .any(|id| id == &spec.worker_environment_kind_id);
            let door_belongs_to_kind = spec
                .door_driver_refs
                .iter()
                .any(|driver_ref| driver_ref == request.door_driver_ref);
            let tool_belongs_to_kind = request
                .tool_process_bins
                .get(request.tool)
                .is_some_and(|bin| spec.tool_process_bins.iter().any(|owned| owned == bin));
            let worker_consented =
                matches!(spec.worker_consent, WorkerConsent::ExplicitCeiling) && granted;
            if worker_consented
                && tool_declares_kind
                && door_belongs_to_kind
                && tool_belongs_to_kind
            {
                candidates.push(pinned);
            }
        }

        let pinned = match candidates.as_slice() {
            [] => {
                return Err(WorkerEnvError::NoCompatibleFormation(format!(
                    "door={} driver_ref={} tool={} process_bin={} worker={} granted=[{}] tool_kinds=[{}]",
                    request.door_interface,
                    request.door_driver_ref,
                    request.tool,
                    request
                        .tool_process_bins
                        .get(request.tool)
                        .map(String::as_str)
                        .unwrap_or("<missing>"),
                    request.worker,
                    request
                        .granted_worker_environment_kinds
                        .iter()
                        .map(WorkerEnvironmentKindId::as_str)
                        .collect::<Vec<_>>()
                        .join(", "),
                    request
                        .tool_worker_environment_kinds
                        .iter()
                        .map(WorkerEnvironmentKindId::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            [pinned] => *pinned,
            _ => {
                return Err(WorkerEnvError::NoCompatibleFormation(format!(
                    "door={} driver_ref={} tool={} worker={} resolves multiple descriptor-owned code kinds=[{}]",
                    request.door_interface,
                    request.door_driver_ref,
                    request.tool,
                    request.worker,
                    candidates
                        .iter()
                        .map(|candidate| candidate.spec.worker_environment_kind_id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        };

        let spec = &pinned.spec;
        let mut granted_tools = request
            .tool_descriptor_hashes
            .keys()
            .filter(|tool| request.granted_tools.iter().any(|granted| granted == *tool))
            .filter(|tool| {
                request
                    .tool_process_bins
                    .get(*tool)
                    .is_some_and(|bin| spec.tool_process_bins.iter().any(|owned| owned == bin))
            })
            .map(|tool| (*tool).clone())
            .collect::<Vec<_>>();
        granted_tools.sort();
        granted_tools.dedup();
        if !granted_tools.iter().any(|tool| tool == request.tool) {
            return Err(WorkerEnvError::NoCompatibleFormation(format!(
                "formed tool set does not contain requested tool '{}'",
                request.tool
            )));
        }

        let tool_descriptor_hashes = granted_tools
            .iter()
            .map(|tool| {
                request.tool_process_bins.get(tool).ok_or_else(|| {
                    WorkerEnvError::NoCompatibleFormation(format!(
                        "formed tool '{tool}' has no process bin"
                    ))
                })?;
                request
                    .tool_descriptor_hashes
                    .get(tool)
                    .filter(|hash| !hash.trim().is_empty())
                    .map(|hash| (tool.clone(), hash.clone()))
                    .ok_or_else(|| {
                        WorkerEnvError::NoCompatibleFormation(format!(
                            "formed tool '{}' has no descriptor hash",
                            tool
                        ))
                    })
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
        let instance_config_hashes = load_instance_config_hashes(spec, request.door_instance_dir)?;

        let caps = WorkerEnvCaps::form(
            spec,
            request.door_root_bindings,
            request.ee_root_slot_allowance,
        );
        let mut formed_root_names = caps.readable_root_names();
        formed_root_names.extend(caps.writable_root_names());
        formed_root_names.extend(
            spec.boundary_defaults
                .protected_read_only_subpaths
                .iter()
                .map(|root| root.path.clone())
                .filter(|name| request.door_root_bindings.contains_key(name)),
        );
        let door_root_bindings = formed_root_names
            .into_iter()
            .map(|name| {
                request
                    .door_root_bindings
                    .get(&name)
                    .cloned()
                    .map(|path| (name.clone(), path))
                    .ok_or_else(|| {
                        WorkerEnvError::MalformedFormation(format!(
                            "formed root slot '{name}' has no concrete door binding"
                        ))
                    })
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
        let door_additional_root_bindings = request.door_additional_root_bindings.iter()
            .filter(|(slot, paths)| door_root_bindings.contains_key(*slot) && !paths.is_empty())
            .map(|(slot, paths)| (slot.clone(), paths.clone())).collect();
        let (formation_pin, worker_environment_id) = formation_pin(
            request,
            &spec.worker_environment_kind_id,
            &pinned.hash,
            &granted_tools,
            &tool_descriptor_hashes,
            &instance_config_hashes,
            &door_root_bindings,
            &door_additional_root_bindings,
            &caps,
            spec.lifecycle.requires_release,
        )?;

        let formation = WorkerEnvironmentFormation {

            door_additional_root_bindings,
            worker_environment_id,
            run: request.run.to_string(),
            ee: request.ee.to_string(),
            worker: request.worker.clone(),
            door_interface: request.door_interface.clone(),
            door_descriptor_hash: request.door_descriptor_hash.to_string(),
            worker_environment_kind: spec.worker_environment_kind_id.clone(),
            worker_environment_kind_descriptor_hash: pinned.hash.to_hex(),
            tool_descriptor_hashes,
            instance_config_hashes,
            formation_pin,
            granted_tools,
            door_root_bindings,
            readable_root_slots: caps.readable_root_names().into_iter().collect(),
            writable_root_slots: caps.writable_root_names().into_iter().collect(),
            network_ceiling: caps.network(),
            requires_release: spec.lifecycle.requires_release,
        };
        formation.validate_against_kind(spec)?;
        Ok(formation)
    }
}

fn load_instance_config_hashes(
    spec: &WorkerEnvKindSpec,
    interface_dir: &Path,
) -> Result<BTreeMap<String, String>, WorkerEnvError> {
    let mut hashes = BTreeMap::new();
    for name in &spec.instance_config_files {
        let path = interface_dir.join(name);
        let bytes = std::fs::read(&path).map_err(|error| {
            WorkerEnvError::MalformedFormation(format!(
                "worker_environment_kind '{}' instance config '{}' cannot be read: {error}",
                spec.worker_environment_kind_id,
                path.display()
            ))
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            WorkerEnvError::MalformedFormation(format!(
                "worker_environment_kind '{}' instance config '{}' is malformed JSON: {error}",
                spec.worker_environment_kind_id,
                path.display()
            ))
        })?;
        if !value.is_object() {
            return Err(WorkerEnvError::MalformedFormation(format!(
                "worker_environment_kind '{}' instance config '{}' must be a JSON object",
                spec.worker_environment_kind_id,
                path.display()
            )));
        }
        hashes.insert(
            name.clone(),
            blake3::hash(&octopus_descriptors::canonical_bytes(&value))
                .to_hex()
                .to_string(),
        );
    }
    Ok(hashes)
}
