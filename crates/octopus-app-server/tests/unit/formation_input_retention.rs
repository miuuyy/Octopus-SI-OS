use super::*;
use octopus_descriptors::CheckpointId;
use octopus_ledger::InMemoryBlobs;
use octopus_policy::ArtifactAccess;
use serde_json::json;

fn write(path: &Path, value: serde_json::Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn blob(blobs: &SharedBlobs, hash: &DescriptorHash) -> Vec<u8> {
    blobs
        .lock()
        .unwrap()
        .get(&ContentHash::from_hex(&hash.to_hex()).unwrap())
        .unwrap()
}

fn resolve(
    registry_root: &Path,
    ee_instances_root: &Path,
    snapshot: LoadedRegistrySnapshot,
    blobs: SharedBlobs,
) -> Result<RunGrantResolution, String> {
    RegistryRunGrantResolver {
        live_registry: Arc::new(RwLock::new(snapshot)),
        blobs,
        registry_root: registry_root.to_path_buf(),
        ee_instances_root: ee_instances_root.to_path_buf(),
    }
    .resolve_run_grant(RunGrantQuery {
        run: &RunId::new("run"),
        checkpoint: &CheckpointId::new("checkpoint"),
        ee: &EeId::new("ee_root"),
        worker: &InterfaceId::new("system_worker_test"),
        epoch: GrantEpoch(0),
        authored_interfaces: &[InterfaceId::new("system_worker_test")],
        artifact_bindings: &[ArtifactId::new("role")],
        artifact_access: &BTreeMap::from([(ArtifactId::new("role"), ArtifactAccess::read_only())]),
        required_verified_artifacts: &[],
    })
}

#[test]
fn normalized_formation_inputs_rebuild_the_historical_grant_after_source_changes() {
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("registry");
    let instances = dir.path().join("instances");
    for kind in "interface artifact tool worker_environment_kind verifier mark ee_template"
        .split_whitespace()
    {
        std::fs::create_dir_all(registry.join(kind)).unwrap();
    }
    let worker = registry.join("interface/system_worker_test");
    write(
        &worker.join("interface.json"),
        json!({"id":"system_worker_test", "interface_type":"system",
        "role":"worker", "direction":"bidirectional", "source_names":["test"]}),
    );
    write(
        &worker.join("policy.json"),
        json!({"id":"system_worker_test",
        "ceiling":{"interfaces":["system_worker_test"]}, "role_artifacts":["role"]}),
    );
    write(
        &registry.join("artifact/role/artifact.json"),
        json!({"id":"role", "content":"original role"}),
    );
    write(
        &registry.join("ee_template/root/ee_template.json"),
        json!({"id":"root", "goal_class":"original goal",
        "bootstrap_root":true, "allowed_interfaces":["system_worker_test"]}),
    );
    write(
        &instances.join("ee_root.json"),
        json!({"ee_id":"ee_root", "template_name":"root",
        "parent_ee_id":null, "goal":"original goal"}),
    );
    let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
    let first = resolve(
        &registry,
        &instances,
        LoadedRegistrySnapshot::load(&registry, &instances).unwrap(),
        blobs.clone(),
    )
    .unwrap();
    let policy_name = crate::worker_policy::worker_interface_descriptor_name(&InterfaceId::new(
        "system_worker_test",
    ));
    assert_ne!(
        blob(&blobs, &first.grant().pin.inputs[&policy_name]),
        std::fs::read(worker.join("policy.json")).unwrap()
    );
    write(
        &worker.join("policy.json"),
        json!({"id":"system_worker_test", "ceiling":{
        "interfaces":["system_worker_test"], "artifact_verbs":["read","change"]}, "role_artifacts":["role"]}),
    );
    write(
        &registry.join("artifact/role/artifact.json"),
        json!({"id":"role", "content":"changed role"}),
    );
    write(
        &instances.join("ee_root.json"),
        json!({"ee_id":"ee_root", "template_name":"root",
        "parent_ee_id":null, "goal":"original goal", "config":{"changed":true}}),
    );
    write(
        &registry.join("ee_template/root/ee_template.json"),
        json!({"id":"root", "goal_class":"changed goal",
        "bootstrap_root":true, "allowed_interfaces":["system_worker_test"]}),
    );
    let current = resolve(
        &registry,
        &instances,
        LoadedRegistrySnapshot::load(&registry, &instances).unwrap(),
        blobs.clone(),
    )
    .unwrap();
    assert_ne!(current.grant().pin, first.grant().pin);
    let roots = BTreeMap::from([
        ("registry_interface_root", registry.join("interface")),
        ("registry_artifact_root", registry.join("artifact")),
        ("registry_ee_template_root", registry.join("ee_template")),
        ("ee_instances_root", instances.clone()),
    ]);
    for (name, hash) in &first.grant().pin.inputs {
        let bytes = blob(&blobs, hash);
        assert_eq!(DescriptorHash::of_bytes(&bytes), *hash);
        let (slot, relative) = name.as_str().split_once('/').unwrap();
        let path = roots[slot].join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let mut historical = LoadedRegistrySnapshot::load(&registry, &instances).unwrap();
    let replayed = resolve(&registry, &instances, historical.clone(), blobs.clone()).unwrap();
    assert_eq!(replayed.grant(), first.grant());
    assert_eq!(replayed.metadata_projection(), first.metadata_projection());
    historical
        .formation_source_bytes
        .get_mut(&policy_name)
        .unwrap()
        .push(b' ');
    assert!(
        resolve(&registry, &instances, historical, blobs)
            .unwrap_err()
            .contains("differs from its pinned hash")
    );
}

#[test]
fn registry_fingerprint_tracks_same_size_rewrites_with_preserved_mtime() {
    let root = tempfile::tempdir().unwrap();
    let registry = root.path().join("registry");
    let instances = root.path().join("instances");
    let descriptor = registry.join("interface/world_probe/interface.json");
    write(&descriptor, json!({"id":"world_probe", "source_names":["alpha"]}));
    let modified = std::fs::metadata(&descriptor).unwrap().modified().unwrap();
    let mut files = RegistryFileCache::default();
    let mut previous = registry_descriptor_fingerprint(&registry, &instances, &mut files).unwrap();
    for text in ["omega", "alpha"].into_iter().cycle().take(32) {
        write(&descriptor, json!({"id":"world_probe", "source_names":[text]}));
        std::fs::File::open(&descriptor).unwrap().set_modified(modified).unwrap();
        let next = registry_descriptor_fingerprint(&registry, &instances, &mut files).unwrap();
        assert_ne!(previous, next);
        previous = next;
    }
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(previous, registry_descriptor_fingerprint(&registry, &instances, &mut files).unwrap());
}

#[tokio::test]
async fn registry_observation_requires_stability_and_propagates_read_failures() {
    for (reads, expected, loads) in [
        (vec![1, 2, 2, 2], "ok", 2),
        (vec![1, 1], "malformed", 1),
        ((1..=(2 * REGISTRY_OBSERVATION_ATTEMPTS as u128)).collect(), "unstable", REGISTRY_OBSERVATION_ATTEMPTS),
        (vec![0], "before load failed", 0),
        (vec![1, 0], "after load failed", 1),
        (vec![u128::MAX, 1, 2, 2, 2], "ok", 2),
        (vec![1, u128::MAX, 2, 2], "ok", 2),
        (vec![u128::MAX, 1, 1], "malformed", 1),
        (vec![u128::MAX; REGISTRY_OBSERVATION_ATTEMPTS], "unstable", 0),
        (vec![u128::MAX, u128::MAX, u128::MAX, u128::MAX, 1, 2, 2, 2], "ok", 2),
    ] {
        let mut reads = reads.into_iter().map(|digest| match digest {
            0 => Err(std::io::Error::other("unreadable")),
            u128::MAX => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "enumerated descriptor was removed")),
            digest => Ok(RegistryDescriptorFingerprint {
                file_count: 1,
                digest,
            }),
        });
        let mut count = 0;
        let mut waits = Vec::new();
        let result = observe_stable_registry(
            || {
                let observation = observe_registry_once(None, || reads.next().unwrap(), || {
                count += 1;
                (count != 1).then_some(count).ok_or("malformed".into())
                });
                std::future::ready(observation)
            },
            |attempt| { waits.push(attempt); std::future::ready(()) },
        ).await;
        assert_eq!(waits, (0..waits.len()).collect::<Vec<_>>());
        if expected == "unstable" { assert_eq!(waits.len(), REGISTRY_OBSERVATION_ATTEMPTS - 1); }
        assert_eq!(count, loads);
        assert!(reads.next().is_none());
        assert!(match result {
            Ok(Some((f, candidate))) => expected == "ok" && (f.digest, candidate) == (2, 2),
            Err(error) => {
                assert_eq!(matches!(error, RegistrySnapshotError::Unstable { .. }), expected == "unstable");
                if let RegistrySnapshotError::Unstable { attempts } = &error {
                    let verdict = crate::InterfaceArtifactArrivalAdmissionError::RegistryUnstable { attempts: *attempts }.verdict();
                    assert!(matches!(verdict, octopus_ledger::DeliveryVerdict::Refused {
                        reason: octopus_ledger::RefusedReason::RegistryUnstable, ..
                    }));
                    assert_eq!(serde_json::to_value(verdict).unwrap()["refused"]["reason"], "registry_unstable");
                }
                if expected == "unstable" {
                    error.to_string().contains(&format!("after {REGISTRY_OBSERVATION_ATTEMPTS} attempts"))
                } else { error.to_string().contains(expected) }
            },
            Ok(None) => false,
        });
    }
}

#[test]
fn admission_guards_only_the_selected_ee_dependency_closure() {
    use octopus_file_storage::publication::{DigestAlgorithm, PublicationOutcome, WriteGuard};
    use octopus_runtime::AdmissionSourceGuard;
    let root = tempfile::tempdir().unwrap();
    let registry = root.path().join("registry");
    let instances = root.path().join("instances");
    for kind in REGISTRY_DESCRIPTOR_KINDS {
        std::fs::create_dir_all(registry.join(kind)).unwrap();
    }
    write(&registry.join("interface/worker/interface.json"), json!({
        "id":"worker", "interface_type":"system", "role":"worker", "direction":"bidirectional", "source_names":["test"]
    }));
    write(&registry.join("interface/worker/policy.json"), json!({"id":"worker", "ceiling":{"interfaces":["worker"]}}));
    write(&registry.join("ee_template/root/ee_template.json"), json!({"id":"root", "goal_class":"root", "bootstrap_root":true, "allowed_interfaces":["worker"], "allowed_child_ee_templates":["root"]}));
    write(&instances.join("ee_root.json"), json!({"ee_id":"ee_root", "template_name":"root", "parent_ee_id":null, "goal":"root"}));
    for id in 0..1000 {
        write(&instances.join(format!("ee_{id}.json")), json!({"ee_id":format!("ee_{id}"), "template_name":"root", "parent_ee_id":"ee_root", "goal":"test"}));
    }
    let owner = RegistryRunGrantResolver {
        live_registry: Arc::new(RwLock::new(LoadedRegistrySnapshot::load(&registry, &instances).unwrap())),
        blobs: Arc::new(Mutex::new(InMemoryBlobs::new())),
        registry_root: registry, ee_instances_root: instances.clone(),
    };
    let record = octopus_run_record::RunRecord::from_bytes(&serde_json::to_vec(&json!({
        "id":"run", "ee":"ee_0", "worker":"worker", "trace":"guard", "status":"running", "interface_state":{},
        "active_checkpoints":["input"], "checkpoints":[{"id":"input", "run":"run", "trace":"guard", "epoch":0,
        "interface_binding":octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new("worker")), "artifact_bindings":[], "outbound_interface_bindings":[]}]
    })).unwrap()).unwrap();
    let guard = commit::acquire(&owner, &record, &[]).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let publisher = std::thread::spawn(move || {
        let path = instances.join("ee_999.json");
        let before = std::fs::read(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&before).unwrap();
        value["goal"] = json!("independent update");
        let result = WriteGuard::acquire(&path, false).and_then(|guard| {
            guard.publish("unrelated-ee", Some(&ContentHash::of_bytes(&before).to_hex()), &serde_json::to_vec(&value).unwrap(), DigestAlgorithm::Blake3)
        });
        tx.send(result).unwrap();
    });
    let result = rx.recv_timeout(std::time::Duration::from_secs(5));
    guard.validate().unwrap();
    drop(guard);
    publisher.join().unwrap();
    assert_eq!(result.unwrap().unwrap().outcome, PublicationOutcome::Applied);
}

#[test]
fn collection_entry_writes_do_not_change_kernel_registry_fingerprint() {
    let root = tempfile::tempdir().unwrap();
    let registry = root.path().join("registry");
    let instances = root.path().join("instances");
    let collection = registry.join("artifact/notes");
    let carrier = collection.join("artifact.json");
    write(&carrier, json!({"id":"notes","label":"context.collection"}));
    let mut files = RegistryFileCache::default();
    let fingerprint = registry_descriptor_fingerprint(&registry, &instances, &mut files).unwrap();
    let entry = collection.join("entries/note.json");
    write(&entry, json!({"revision":1}));
    write(&entry, json!({"revision":2}));
    assert_eq!(fingerprint, registry_descriptor_fingerprint(&registry, &instances, &mut files).unwrap());
    std::fs::remove_file(entry).unwrap();
    assert_eq!(fingerprint, registry_descriptor_fingerprint(&registry, &instances, &mut files).unwrap());
    write(&carrier, json!({"id":"notes","label":"context.collection","description":"changed"}));
    assert_ne!(fingerprint, registry_descriptor_fingerprint(&registry, &instances, &mut files).unwrap());
}

#[test]
fn commit_acquire_ignores_unrelated_registry_writes_and_fails_on_dependency_change() {
    use octopus_runtime::AdmissionSourceGuard;
    let root = tempfile::tempdir().unwrap();
    let (registry, instances) = (root.path().join("registry"), root.path().join("instances"));
    for kind in REGISTRY_DESCRIPTOR_KINDS { std::fs::create_dir_all(registry.join(kind)).unwrap(); }
    write(&registry.join("interface/worker/interface.json"), json!({"id":"worker","interface_type":"system","role":"worker","direction":"bidirectional","source_names":["test"]}));
    write(&registry.join("interface/worker/policy.json"), json!({"id":"worker","ceiling":{"interfaces":["worker"]}}));
    write(&registry.join("ee_template/root/ee_template.json"), json!({"id":"root","goal_class":"root","bootstrap_root":true,"allowed_interfaces":["worker"],"allowed_child_ee_templates":["root"]}));
    write(&instances.join("ee_root.json"), json!({"ee_id":"ee_root","template_name":"root","parent_ee_id":null,"goal":"root"}));
    write(&instances.join("ee_target.json"), json!({"ee_id":"ee_target","template_name":"root","parent_ee_id":"ee_root","goal":"target"}));
    write(&instances.join("ee_other.json"), json!({"ee_id":"ee_other","template_name":"root","parent_ee_id":"ee_root","goal":"other"}));
    let owner = RegistryRunGrantResolver {
        live_registry: Arc::new(RwLock::new(LoadedRegistrySnapshot::load(&registry, &instances).unwrap())),
        blobs: Arc::new(Mutex::new(InMemoryBlobs::new())),
        registry_root: registry.clone(), ee_instances_root: instances.clone(),
    };
    let record = octopus_run_record::RunRecord::from_bytes(&serde_json::to_vec(&json!({
        "id":"run","ee":"ee_target","worker":"worker","trace":"guard","status":"running","interface_state":{},
        "active_checkpoints":["input"],"checkpoints":[{"id":"input","run":"run","trace":"guard","epoch":0,
        "interface_binding":octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new("worker")),"artifact_bindings":[],"outbound_interface_bindings":[]}]
    })).unwrap()).unwrap();

    write(&instances.join("ee_other.json"), json!({"ee_id":"ee_other","template_name":"root","parent_ee_id":"ee_root","goal":"mutated unrelated"}));
    write(&registry.join("interface/world_unrelated/interface.json"), json!({"id":"world_unrelated","interface_type":"world","role":"probe","direction":"bidirectional","source_names":["u"],"interface_driver_ref":"probe"}));
    let guard = commit::acquire(&owner, &record, &[]).expect("unrelated registry mutations must not reject commit guard");
    guard.validate().unwrap();
    drop(guard);

    let policy_path = registry.join("interface/worker/policy.json");
    let original_policy = std::fs::read(&policy_path).unwrap();
    write(&policy_path, json!({"id":"worker","ceiling":{"interfaces":["worker"],"artifact_verbs":["read"]}}));
    assert!(commit::acquire(&owner, &record, &[]).err().unwrap().contains("no longer matches its source files"));
    std::fs::write(&policy_path, &original_policy).unwrap();
    commit::acquire(&owner, &record, &[]).unwrap();

    let extra_sidecar = registry.join("interface/worker/extra.json");
    write(&extra_sidecar, json!({"unexpected":true}));
    assert!(commit::acquire(&owner, &record, &[]).err().unwrap().contains("no longer matches its source files"));
    std::fs::remove_file(&extra_sidecar).unwrap();

    write(&instances.join("ee_target.json"), json!({"ee_id":"ee_target","template_name":"root","parent_ee_id":"ee_root","goal":"changed"}));
    assert!(commit::acquire(&owner, &record, &[]).err().unwrap().contains("no longer matches its source files"));
}

#[tokio::test]
async fn concurrent_unrelated_registry_writes_do_not_drop_arrivals_as_registry_unstable() {
    use octopus_file_storage::publication::{DigestAlgorithm, WriteGuard};
    use octopus_interfaces::InterfaceArtifactFile;
    use octopus_ledger::{InMemoryLog, MovementKind, MovementLog};
    use std::sync::atomic::{AtomicBool, Ordering};

    let root = tempfile::tempdir().unwrap();
    let (registry, state) = (root.path().join("registry"), root.path().join("state"));
    let (instances, runs, artifacts) = (state.join("ee_instances"), state.join("runs"), state.join("interface-artifacts"));
    for kind in REGISTRY_DESCRIPTOR_KINDS { std::fs::create_dir_all(registry.join(kind)).unwrap(); }
    for dir in [&instances, &runs, &artifacts] { std::fs::create_dir_all(dir).unwrap(); }
    write(&registry.join("interface/world_probe/interface.json"), json!({"id":"world_probe","interface_type":"world","role":"probe","direction":"bidirectional","source_names":["probe"],"interface_driver_ref":"probe"}));
    write(&registry.join("interface/system_worker_probe/interface.json"), json!({"id":"system_worker_probe","interface_type":"system","role":"worker","direction":"bidirectional","source_names":["worker"]}));
    write(&registry.join("interface/system_worker_probe/policy.json"), json!({"id":"system_worker_probe","ceiling":{"interfaces":["world_probe","system_worker_probe"]}}));
    write(&registry.join("ee_template/root/ee_template.json"), json!({"id":"root","goal_class":"root","bootstrap_root":true,"allowed_interfaces":["world_probe","system_worker_probe"],"allowed_child_ee_templates":["root"]}));
    write(&instances.join("ee_root.json"), json!({"ee_id":"ee_root","template_name":"root","parent_ee_id":null,"goal":"root"}));
    write(&instances.join("ee_other.json"), json!({"ee_id":"ee_other","template_name":"root","parent_ee_id":"ee_root","goal":"other"}));
    write(&runs.join("run_probe.json"), json!({
        "id":"run_probe","ee":"ee_root","worker":"system_worker_probe","trace":"trace:probe","status":"running","interface_state":{},
        "active_checkpoints":["wait"],"checkpoints":[{"id":"wait","run":"run_probe","trace":"trace:probe","epoch":0,
        "interface_binding":octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new("world_probe")),"artifact_bindings":[],"outbound_interface_bindings":[]}]
    }));

    let blobs: SharedBlobs = Arc::new(Mutex::new(InMemoryBlobs::new()));
    let embedded = Arc::new(build_embedded_spine(SpineRootConfig {
        registry_root: registry.clone(), run: RunId::new("run_probe"), blobs: blobs.clone(),
        interface_artifact_root: artifacts.clone(), run_records_root: runs.clone(), ee_instances_root: instances.clone(),
    }).await.unwrap());

    let stop = Arc::new(AtomicBool::new(false));
    let (writer_stop, writer_registry, writer_instances) = (stop.clone(), registry.clone(), instances.clone());
    let writer = std::thread::spawn(move || {
        for seq in 0usize..40 {
            if writer_stop.load(Ordering::Relaxed) { break; }
            let iface_id = format!("world_peer_{seq}");
            let iface_path = writer_registry.join("interface").join(&iface_id).join("interface.json");
            let body = serde_json::to_vec(&json!({"id":iface_id,"interface_type":"world","role":"probe","direction":"bidirectional","source_names":["peer"],"interface_driver_ref":"probe"})).unwrap();
            let op_id = format!("create-peer-{seq}");
            WriteGuard::acquire_creating_operation(&iface_path, &op_id).unwrap().publish(&op_id, None, &body, DigestAlgorithm::Blake3).unwrap();
            let ee_path = writer_instances.join("ee_other.json");
            let before = std::fs::read(&ee_path).unwrap();
            let updated = serde_json::to_vec(&json!({"ee_id":"ee_other","template_name":"root","parent_ee_id":"ee_root","goal":format!("other-{seq}")})).unwrap();
            let ee_op = format!("update-ee-{seq}");
            WriteGuard::acquire_operation(&ee_path, false, &ee_op).unwrap().publish(&ee_op, Some(&ContentHash::of_bytes(&before).to_hex()), &updated, DigestAlgorithm::Blake3).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    let mut log = InMemoryLog::new();
    let mut blob_handle = SharedBlobHandle::new(blobs.clone());
    let arriving = InterfaceId::new("world_probe");
    for i in 0..12 {
        let file = InterfaceArtifactFile::write(&artifacts.join(format!("in-{i}")), "probe.input", format!("{{\"n\":{i}}}").as_bytes())
            .unwrap().capture_bounded(1024 * 1024, 1024 * 1024).unwrap();
        let outcome = crate::admit_interface_artifact_arrival(&embedded, &mut log, &mut blob_handle, &file, &arriving).await
            .unwrap_or_else(|error| panic!("arrival {i} must not fail under concurrent unrelated writes: {error}"));
        assert_eq!(outcome.admission.records.len(), 1);
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();

    assert_eq!(log.read_from(octopus_ledger::Seq(0)).into_iter().filter(|m| m.kind == MovementKind::InterfaceArrival).count(), 12);
    assert_eq!(log.read_from(octopus_ledger::Seq(0)).into_iter().filter(|m| m.kind == MovementKind::DeliveryRefused).count(), 0);
}
