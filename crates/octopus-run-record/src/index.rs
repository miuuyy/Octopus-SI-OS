use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use crate::{RunRecord, RunRecordSnapshot};
use octopus_descriptors::{CheckpointId, InterfaceId, RunId};
use octopus_ledger::ContentHash;

use octopus_file_storage::metadata::FileStamp;
use std::time::SystemTime;

struct RunSlot {
    fingerprint: FileStamp,
    settled: bool,
    record_hash: Option<ContentHash>,
    record_bytes: Option<Arc<[u8]>>,
    record: Option<Arc<RunRecord>>,
    liveness_interfaces: BTreeSet<InterfaceId>,
}

#[derive(Default)]
pub struct RunRecordsIndex {
    slots: BTreeMap<RunId, RunSlot>,
    by_interface: BTreeMap<InterfaceId, BTreeSet<(RunId, CheckpointId)>>,
    mentioned_interfaces: BTreeMap<InterfaceId, usize>,
}

impl RunRecordsIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn catch_up(&mut self, dir: &Path) -> std::io::Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.clear_cache();
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let mut present: BTreeSet<RunId> = BTreeSet::new();
        for entry in entries {
            let entry = entry?;
            let Some(run) = run_id_of_json(&entry.path()) else {
                continue;
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    self.remove(&run);
                    continue;
                }
            };
            if !file_type.is_file() {
                self.remove(&run);
                continue;
            }
            let observed_before_read = SystemTime::now();
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.remove(&run);
                    continue;
                }
            };
            present.insert(run.clone());
            let fingerprint = FileStamp::of(&metadata);
            let settled = fingerprint.is_settled(observed_before_read);
            if settled
                && self
                    .slots
                    .get(&run)
                    .is_some_and(|slot| slot.settled && slot.fingerprint == fingerprint)
            {
                continue;
            }
            let (record_hash, record_bytes, loaded) = match std::fs::read(entry.path()) {
                Ok(bytes) => {
                    if let Some(slot) = self
                        .slots
                        .get_mut(&run)
                        .filter(|slot| slot.record_bytes.as_deref() == Some(bytes.as_slice()))
                    {
                        slot.fingerprint = fingerprint;
                        slot.settled = settled;
                        continue;
                    }
                    (
                        Some(ContentHash::of_bytes(&bytes)),
                        Some(Arc::from(bytes.as_slice())),
                        RunRecord::from_bytes(&bytes).map(Some),
                    )
                }
                Err(error) => (None, None, Err(error)),
            };
            self.install(run, fingerprint, settled, record_hash, record_bytes, loaded);
        }
        let vanished: Vec<RunId> = self
            .slots
            .keys()
            .filter(|run| !present.contains(*run))
            .cloned()
            .collect();
        for run in vanished {
            self.remove(&run);
        }
        Ok(())
    }

    pub fn targets_for_interface(
        &self,
        interface: &InterfaceId,
    ) -> Vec<(Arc<RunRecord>, CheckpointId)> {
        let Some(bucket) = self.by_interface.get(interface) else {
            return Vec::new();
        };
        let mut targets = Vec::with_capacity(bucket.len());
        for (run, checkpoint) in bucket {
            if let Some(record) = self.slots.get(run).and_then(|slot| slot.record.clone()) {
                targets.push((record, checkpoint.clone()));
            }
        }
        targets
    }

    pub fn record_snapshot(&self, run: &RunId) -> Option<RunRecordSnapshot> {
        let slot = self.slots.get(run).filter(|slot| slot.record.is_some())?;
        Some(RunRecordSnapshot {
            hash: slot.record_hash?,
            bytes: slot.record_bytes.clone()?,
        })
    }

    pub fn interfaces_requiring_life(&self) -> BTreeSet<InterfaceId> {
        self.mentioned_interfaces.keys().cloned().collect()
    }

    fn install(
        &mut self,
        run: RunId,
        fingerprint: FileStamp,
        settled: bool,
        record_hash: Option<ContentHash>,
        record_bytes: Option<Arc<[u8]>>,
        loaded: std::io::Result<Option<RunRecord>>,
    ) {
        let previous_liveness = self
            .slots
            .get(&run)
            .map(|slot| slot.liveness_interfaces.clone())
            .unwrap_or_default();
        self.detach_routes(&run);
        let record = match loaded {
            Ok(Some(record)) if record.id == run => Some(Arc::new(record)),
            Ok(Some(_)) => None,
            Ok(None) => None,
            Err(_) => None,
        };
        let liveness_interfaces = record
            .as_ref()
            .map(|record| active_checkpoint_interfaces(record))
            .unwrap_or_default();
        self.replace_liveness(&previous_liveness, &liveness_interfaces);
        if let Some(record) = record.as_ref() {
            for (interface, checkpoint) in active_interface_checkpoints(record) {
                self.by_interface
                    .entry(interface)
                    .or_default()
                    .insert((run.clone(), checkpoint));
            }
        }
        self.slots.insert(
            run,
            RunSlot {
                fingerprint,
                settled,
                record_hash,
                record_bytes,
                record,
                liveness_interfaces,
            },
        );
    }

    fn remove(&mut self, run: &RunId) {
        self.detach_routes(run);
        if let Some(slot) = self.slots.remove(run) {
            self.replace_liveness(&slot.liveness_interfaces, &BTreeSet::new());
        }
    }

    fn detach_routes(&mut self, run: &RunId) {
        let record = match self.slots.get(run).and_then(|slot| slot.record.as_ref()) {
            Some(record) => Arc::clone(record),
            None => return,
        };
        for (interface, checkpoint) in active_interface_checkpoints(&record) {
            if let Some(bucket) = self.by_interface.get_mut(&interface) {
                bucket.remove(&(run.clone(), checkpoint));
                if bucket.is_empty() {
                    self.by_interface.remove(&interface);
                }
            }
        }
    }

    fn replace_liveness(&mut self, previous: &BTreeSet<InterfaceId>, next: &BTreeSet<InterfaceId>) {
        for interface in previous.difference(next) {
            let remove = match self.mentioned_interfaces.get_mut(interface) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => true,
                None => false,
            };
            if remove {
                self.mentioned_interfaces.remove(interface);
            }
        }
        for interface in next.difference(previous) {
            *self
                .mentioned_interfaces
                .entry(interface.clone())
                .or_default() += 1;
        }
    }

    fn clear_cache(&mut self) {
        self.slots.clear();
        self.by_interface.clear();
        self.mentioned_interfaces.clear();
    }
}

fn run_id_of_json(path: &Path) -> Option<RunId> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
        return None;
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(RunId::new)
}

fn active_interface_checkpoints(record: &RunRecord) -> Vec<(InterfaceId, CheckpointId)> {
    let active = record
        .active_checkpoint_ids()
        .iter()
        .map(|checkpoint| checkpoint.as_str())
        .collect::<BTreeSet<_>>();
    let mut pairs = Vec::with_capacity(active.len());
    for checkpoint in &record.checkpoints {
        if !active.contains(checkpoint.id.as_str()) {
            continue;
        }
        let Some(interface) = checkpoint.interface_binding.as_ref() else {
            continue;
        };
        pairs.push((interface.interface.clone(), checkpoint.id.clone()));
    }
    pairs
}

fn active_checkpoint_interfaces(record: &RunRecord) -> BTreeSet<InterfaceId> {
    let active = record
        .active_checkpoint_ids()
        .iter()
        .map(|checkpoint| checkpoint.as_str())
        .collect::<BTreeSet<_>>();
    let mut interfaces = BTreeSet::new();
    for checkpoint in &record.checkpoints {
        if !active.contains(checkpoint.id.as_str()) {
            continue;
        }
        interfaces.extend(
            checkpoint
                .interface_binding
                .iter()
                .map(|binding| binding.interface.clone()),
        );
        interfaces.extend(
            checkpoint
                .outbound_interface_bindings
                .iter()
                .map(|binding| binding.interface.clone()),
        );
    }
    interfaces
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::EeId;
    use std::sync::atomic::{AtomicU64, Ordering};

    static RUN_INDEX_TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "octopus-run-index-{label}-{}-{}",
                std::process::id(),
                RUN_INDEX_TEST_DIR_SEQ.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).expect("test temp dir created");
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_run(dir: &Path, run: &str, active: &[(&str, &str)], settled: bool) {
        let checkpoints: Vec<serde_json::Value> = active
            .iter()
            .map(|(checkpoint, interface)| {
                serde_json::json!({
                    "id": checkpoint,
                    "run": run,
                    "trace": format!("trace:{run}:{checkpoint}"),
                    "epoch": 0,
                    "interface_binding": octopus_policy::AuthoredInterfaceBinding::all(InterfaceId::new(*interface)),
                })
            })
            .collect();
        let active_ids: Vec<&str> = if settled {
            Vec::new()
        } else {
            active.iter().map(|(checkpoint, _)| *checkpoint).collect()
        };
        let record = serde_json::json!({
            "id": run,
            "ee": EeId::new("ee_existence"),
            "worker": "system_worker_flash",
            "trace": format!("trace:{run}"),
            "status": "running",
            "interface_state": {},
            "checkpoints": checkpoints,
            "active_checkpoints": active_ids,
        });
        std::fs::write(
            dir.join(format!("{run}.json")),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
    }

    fn runs(targets: &[(Arc<RunRecord>, CheckpointId)]) -> BTreeSet<(String, String)> {
        targets
            .iter()
            .map(|(record, checkpoint)| {
                (
                    record.id.as_str().to_string(),
                    checkpoint.as_str().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn reverse_map_answers_fan_out_without_scanning_every_run() {
        let dir = TempDir::new("t");
        write_run(dir.path(), "run_a", &[("c1", "world_x")], false);
        write_run(dir.path(), "run_b", &[("c1", "world_x")], false);
        write_run(dir.path(), "run_c", &[("c1", "world_y")], false);

        let mut index = RunRecordsIndex::new();
        index.catch_up(dir.path()).unwrap();

        let x = index.targets_for_interface(&InterfaceId::new("world_x"));
        assert_eq!(
            runs(&x),
            [("run_a".into(), "c1".into()), ("run_b".into(), "c1".into())]
                .into_iter()
                .collect()
        );
        assert_eq!(
            index
                .targets_for_interface(&InterfaceId::new("world_y"))
                .len(),
            1
        );
        assert!(index
            .targets_for_interface(&InterfaceId::new("world_absent"))
            .is_empty());
    }

    #[test]
    fn one_malformed_run_is_isolated_and_never_poisons_a_sibling_interface() {
        let dir = TempDir::new("t");
        write_run(dir.path(), "run_client", &[("c1", "world_operator")], false);
        std::fs::write(dir.path().join("run_broken.json"), b"{ this is not json").unwrap();

        let mut index = RunRecordsIndex::new();
        index.catch_up(dir.path()).unwrap();

        assert_eq!(
            index
                .targets_for_interface(&InterfaceId::new("world_operator"))
                .len(),
            1,
            "a malformed sibling run must not fail-close the client interface's fan-out"
        );
    }

    #[test]
    fn malformed_rewrite_is_inert_for_routing_and_for_interface_life() {
        let dir = TempDir::new("liveness");
        write_run(dir.path(), "run_a", &[("c1", "world_x")], false);
        let mut index = RunRecordsIndex::new();
        index.catch_up(dir.path()).unwrap();
        assert_eq!(
            index.interfaces_requiring_life(),
            [InterfaceId::new("world_x")].into_iter().collect()
        );

        std::fs::write(dir.path().join("run_a.json"), b"{").unwrap();
        index.catch_up(dir.path()).unwrap();
        assert!(index
            .targets_for_interface(&InterfaceId::new("world_x"))
            .is_empty());
        assert!(
            index.interfaces_requiring_life().is_empty(),
            "a malformed run is inert on every projection: the on-disk JSON is the one \
             truth, so a record that no longer parses keeps no cached liveness behind it"
        );

        write_run(dir.path(), "run_a", &[("c2", "world_y")], false);
        index.catch_up(dir.path()).unwrap();
        assert_eq!(
            index.interfaces_requiring_life(),
            [InterfaceId::new("world_y")].into_iter().collect()
        );

        std::fs::remove_file(dir.path().join("run_a.json")).unwrap();
        index.catch_up(dir.path()).unwrap();
        assert!(index.interfaces_requiring_life().is_empty());
    }

    #[test]
    fn a_settled_run_contributes_no_target() {
        let dir = TempDir::new("t");
        write_run(dir.path(), "run_a", &[("c1", "world_x")], true);
        let mut index = RunRecordsIndex::new();
        index.catch_up(dir.path()).unwrap();
        assert!(index
            .targets_for_interface(&InterfaceId::new("world_x"))
            .is_empty());
    }

    #[test]
    fn catch_up_reflects_re_arm_and_removal_incrementally() {
        let dir = TempDir::new("t");
        write_run(dir.path(), "run_a", &[("c1", "world_x")], false);
        let mut index = RunRecordsIndex::new();
        index.catch_up(dir.path()).unwrap();
        assert_eq!(
            index
                .targets_for_interface(&InterfaceId::new("world_x"))
                .len(),
            1
        );

        let path = dir.path().join("run_a.json");
        let prior_time = std::fs::metadata(&path).unwrap().modified().unwrap();
        let replacement_dir = TempDir::new("replacement");
        write_run(replacement_dir.path(), "run_a", &[("c2", "world_y")], false);
        let replacement = replacement_dir.path().join("run_a.json");
        #[cfg(unix)]
        std::fs::File::open(&replacement)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(prior_time))
            .unwrap();
        #[cfg(not(unix))]
        let _ = prior_time;
        std::fs::rename(&replacement, &path).unwrap();
        index.catch_up(dir.path()).unwrap();
        assert!(
            index
                .targets_for_interface(&InterfaceId::new("world_x"))
                .is_empty(),
            "the stale checkpoint's reverse-map entry must be retracted on re-arm"
        );
        assert_eq!(
            index
                .targets_for_interface(&InterfaceId::new("world_y"))
                .len(),
            1
        );

        for (from, to) in [("world_y", "world_x"), ("world_x", "world_y")]
            .into_iter()
            .cycle()
            .take(32)
        {
            let bytes = std::fs::read_to_string(&path).unwrap().replace(from, to);
            std::fs::write(&path, bytes).unwrap();
            std::fs::File::open(&path)
                .unwrap()
                .set_modified(prior_time)
                .unwrap();
            index.catch_up(dir.path()).unwrap();
            assert!(index
                .targets_for_interface(&InterfaceId::new(from))
                .is_empty());
            let targets = index.targets_for_interface(&InterfaceId::new(to));
            assert_eq!(targets.len(), 1);
            index.catch_up(dir.path()).unwrap();
            assert!(Arc::ptr_eq(
                &targets[0].0,
                &index.targets_for_interface(&InterfaceId::new(to))[0].0,
            ));
        }

        std::fs::remove_file(dir.path().join("run_a.json")).unwrap();
        index.catch_up(dir.path()).unwrap();
        assert!(index
            .targets_for_interface(&InterfaceId::new("world_y"))
            .is_empty());
    }
}
