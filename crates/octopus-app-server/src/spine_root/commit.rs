use super::*;
use octopus_file_storage::publication::{COORDINATION_DIRECTORY, ReadGuard};
use octopus_runtime::AdmissionSourceGuard;

pub(super) struct RegistryCommitGuard(ReadGuard);

impl AdmissionSourceGuard for RegistryCommitGuard {
    fn validate(&self) -> Result<(), String> {
        self.0.verify().map_err(|error| error.to_string())
    }
}

#[derive(Default, PartialEq, Eq)]
struct Inputs {
    files: BTreeSet<PathBuf>,
    directories: BTreeSet<PathBuf>,
}

impl Inputs {
    fn directory(&mut self, path: &Path) -> Result<bool, String> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                self.directories.insert(path.to_path_buf());
                Ok(true)
            }
            Ok(_) => Err(format!("admission dependency '{}' is not a directory", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = path.parent().ok_or_else(|| "admission dependency has no parent".to_string())?;
                self.directory(parent)?;
                Ok(false)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn files_in(&mut self, path: &Path, recursive: bool) -> Result<(), String> {
        if !self.directory(path)? { return Ok(()); }
        for entry in std::fs::read_dir(path).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            if entry.file_name() == COORDINATION_DIRECTORY { continue; }
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_file() && (recursive || entry.path().extension().is_some_and(|extension| extension == "json")) {
                self.files.insert(entry.path());
            } else if recursive && kind.is_dir() {
                self.files_in(&entry.path(), true)?;
            } else if recursive && kind.is_symlink() {
                return Err(format!("verifier dependency '{}' is a symbolic link", entry.path().display()));
            }
        }
        Ok(())
    }

    fn capture(owner: &RegistryRunGrantResolver, record: &octopus_run_record::RunRecord, marks: &[Mark]) -> Result<Self, String> {
        let mut inputs = Self::default();
        let snapshot = owner.live_registry.read().map_err(|_| "registry snapshot lock is poisoned")?;
        let mut roots = vec![RegistryNode::interface(record.worker.as_str())];
        for ee in snapshot.ee_tree.ancestors(&record.ee).map_err(|error| error.to_string())? {
            let instance = snapshot.ee_tree.get(&ee).ok_or("admission EE ancestry is incomplete")?;
            roots.push(RegistryNode::ee_template(instance.template.as_str()));
            inputs.files.insert(owner.ee_instances_root.join(format!("{ee}.json")));
        }
        for id in &record.active_checkpoints {
            let checkpoint = record.checkpoint(id).ok_or("active admission checkpoint is absent")?;
            roots.extend(checkpoint.inbound_interface().into_iter().chain(checkpoint.outbound_interfaces())
                .map(|interface| RegistryNode::interface(interface.as_str())));
            roots.extend(checkpoint.artifact_bindings.iter()
                .filter(|binding| !artifact_id_carries_content_hash(&binding.id))
                .map(|binding| RegistryNode::artifact(binding.id.as_str())));
            roots.extend(checkpoint.required_verified_artifacts.iter().flat_map(|required| [
                RegistryNode::mark(required.mark.as_str()), RegistryNode::verifier(required.verifier.as_str()),
            ]));
        }
        for node in snapshot.reference_graph.dependency_nodes(roots) {
            if node.id.is_empty() || node.id == "." || node.id == ".." || node.id.contains('/') || node.id.contains('\\') {
                return Err("admission dependency has invalid registry identity".into());
            }
            let package = owner.registry_root.join(node.to_string());
            inputs.directory(package.parent().ok_or("admission dependency has no registry kind")?)?;
            inputs.files_in(&package, false)?;
        }
        drop(snapshot);
        inputs.directory(&owner.ee_instances_root)?;
        for mark in marks {
            let id = mark.verifier.as_str();
            if id.is_empty() || id == "." || id == ".." || id.contains('/') || id.contains('\\') {
                return Err("admitted verifier has invalid registry identity".into());
            }
            inputs.files_in(&owner.registry_root.join("verifier").join(id), true)?;
        }
        Ok(inputs)
    }
}

pub(super) fn acquire(owner: &RegistryRunGrantResolver, record: &octopus_run_record::RunRecord, marks: &[Mark]) -> Result<RegistryCommitGuard, String> {
    let before = Inputs::capture(owner, record, marks)?;
    let guard = ReadGuard::try_acquire(
        &before.files.iter().cloned().collect::<Vec<_>>(),
        &before.directories.iter().cloned().collect::<Vec<_>>(),
    ).map_err(|error| error.to_string())?;
    if before != Inputs::capture(owner, record, marks)? {
        return Err("admission registry dependency set changed while acquiring ownership".into());
    }
    let observed = {
        let mut files = owner.files.lock().map_err(|_| "registry file cache lock is poisoned")?;
        registry_descriptor_fingerprint(&owner.registry_root, &owner.ee_instances_root, &mut files)
            .map_err(|error| error.to_string())?
    };
    if *owner.fingerprint.lock().map_err(|_| "registry fingerprint lock is poisoned")? != Some(observed) {
        return Err("admission registry snapshot no longer matches its source files".into());
    }
    for mark in marks {
        let current_mark = load_referenced_descriptor::<MarkDescriptor>(&owner.registry_root, mark.name.as_str())?;
        if current_mark.descriptor.class != mark.class || current_mark.descriptor.id != mark.name {
            return Err("mark descriptor changed after verification".into());
        }
        let verifier = load_referenced_descriptor::<VerifierDescriptor>(&owner.registry_root, mark.verifier.as_str())?;
        let package = pin_verifier_package(&VerifierPackageContext {
            package_dir: Some(owner.registry_root.join("verifier").join(mark.verifier.as_str())),
            descriptor: verifier.descriptor,
            repository_root: owner.registry_root.parent().ok_or("registry has no parent")?.to_path_buf(),
            scratch_root: owner.registry_root.clone(),
        }).map_err(|error| error.to_string())?;
        let pin = VerifierPackagePin {
            descriptor_content: ContentHash::of_bytes(&verifier.canonical_bytes),
            script_content: ContentHash::of_bytes(&package.closure_bytes()),
        };
        if mark.verifier_package.as_ref() != Some(&pin) {
            return Err("verifier package changed after verification".into());
        }
    }
    guard.verify().map_err(|error| error.to_string())?;
    Ok(RegistryCommitGuard(guard))
}
