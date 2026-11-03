use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use octopus_app_server::{
    InterfaceArtifactFile, SharedBlobHandle, SharedBlobs, SpineRootConfig,
    admit_interface_artifact_arrival,
};
use octopus_descriptors::{EeId, RunId};
use octopus_ledger::{BlobStore, ContentHash, Endpoint, MovementKind, Seq};
use octopus_ledger_store::{DurableBlobs, DurableLog};
use serde_json::{Value, json};

mod support;

const MODEL_REFERENCE_MS: u64 = 5_000;
const FRAMEWORK_BUDGET: Duration = Duration::from_millis(MODEL_REFERENCE_MS / 5);
const SAMPLES: usize = 20;
const INPUT: &str = "world_operator_chat";
const PROVIDER: &str = "world_provider_codex";
const WORKER: &str = "system_worker_coder_max";

struct MeasuredBlobs {
    inner: DurableBlobs,
    writes: Arc<Mutex<(usize, Duration)>>,
}

impl BlobStore for MeasuredBlobs {
    fn put_batch(&mut self, contents: &[&[u8]]) -> Vec<ContentHash> {
        let start = Instant::now();
        let hashes = self.inner.put_batch(contents);
        let mut writes = self.writes.lock().unwrap();
        writes.0 += contents.len();
        writes.1 += start.elapsed();
        hashes
    }
    fn put(&mut self, bytes: &[u8]) -> ContentHash {
        let start = Instant::now();
        let hash = self.inner.put(bytes);
        let mut writes = self.writes.lock().unwrap();
        writes.0 += 1;
        writes.1 += start.elapsed();
        hash
    }
    fn get(&self, hash: &ContentHash) -> Option<Vec<u8>> {
        self.inner.get(hash)
    }
    fn contains(&self, hash: &ContentHash) -> bool {
        self.inner.contains(hash)
    }
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    std::fs::File::open(path).unwrap().sync_all().unwrap();
}

fn run_record(run: &str, ee: &str, epoch: usize, inbound: &str, outbound: &str) -> Value {
    json!({
        "id": run, "ee": ee, "worker": WORKER,
        "trace": format!("trace:{run}"), "status": "running", "interface_state": {},
        "active_checkpoints": ["exchange"],
        "checkpoints": [{
            "id": "exchange", "run": run, "epoch": epoch,
            "trace": format!("trace:{run}:{epoch}"),
            "interface_binding": support::all_binding(inbound),
            "outbound_interface_bindings": [support::all_binding(outbound)],
            "artifact_bindings": support::worker_role_artifact_bindings(&[WORKER]),
        }],
    })
}

#[tokio::test]
#[ignore = "release-mode framework latency budget; run the manual doomsday workflow"]
async fn framework_round_trip_stays_within_one_second() {
    assert!(
        !cfg!(debug_assertions),
        "run this budget with cargo test --release"
    );
    let small = measure_round_trips(1, 1).await;
    let large = measure_round_trips(1_000, 30).await;
    assert!(
        small <= FRAMEWORK_BUDGET && large <= FRAMEWORK_BUDGET,
        "framework round trips exceeded {FRAMEWORK_BUDGET:?}: single EE {small:?}, 1000 EEs {large:?}"
    );
}

async fn measure_round_trips(ee_count: usize, depth: usize) -> Duration {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let runs = root.join("runs");
    let instances = root.join("ee_instances");
    let writes = Arc::new(Mutex::new((0, Duration::ZERO)));
    let blobs: SharedBlobs = Arc::new(Mutex::new(MeasuredBlobs {
        inner: DurableBlobs::open(root.join("blobs")).unwrap(),
        writes: writes.clone(),
    }));
    let run = RunId::new("latency_probe");
    let boot = Instant::now();
    let embedded = support::build_test_spine(SpineRootConfig {
        registry_root: support::registry_root(),
        run: run.clone(),
        blobs: blobs.clone(),
        interface_artifact_root: root.join("artifacts"),
        run_records_root: runs.clone(),
        ee_instances_root: instances.clone(),
    })
    .await
    .unwrap();
    let bootstrap = support::load_descriptor_bootstrap_ee_id();
    for index in 0..ee_count {
        let id = format!("probe_ee_{index:04}");
        let parent = if index % depth == 0 {
            bootstrap.as_str().to_owned()
        } else {
            format!("probe_ee_{:04}", index - 1)
        };
        write_json(
            &instances.join(format!("{id}.json")),
            &json!({
                "ee_id": id, "template_name": "project", "parent_ee_id": parent,
                "goal": "Measure framework artifact transfer",
            }),
        );
        let background = format!("background_{index:04}");
        write_json(
            &runs.join(format!("{background}.json")),
            &run_record(&background, &id, 0, "system_context", "world_operator_chat"),
        );
    }
    embedded
        .reload_registry_snapshot_on_artifact_arrival()
        .unwrap();
    embedded.runtime.set_run_records_dir(runs.clone());
    let startup_ms = boot.elapsed().as_secs_f64() * 1_000.0;
    let ee = EeId::new(format!("probe_ee_{:04}", depth - 1));
    let path = runs.join(format!("{}.json", run.as_str()));
    let mut log = DurableLog::open(root.join("ledger/movements.jsonl")).unwrap();
    let mut handle = SharedBlobHandle::new(blobs);
    let mut samples = Vec::new();
    let mut cold_legs = Vec::new();

    for sample in 0..SAMPLES {
        let started = Instant::now();
        for (leg, source, destination, label) in [
            (0, INPUT, PROVIDER, "benchmark.request"),
            (1, PROVIDER, INPUT, "benchmark.reply"),
        ] {
            let preparation = Instant::now();
            let mut record = run_record(
                run.as_str(),
                ee.as_str(),
                sample * 2 + leg,
                source,
                destination,
            );
            if leg == 1 {
                record["checkpoints"][0]["required_verified_artifacts"] = json!([{
                    "mark": "task.summary.received", "artifact_label": label,
                    "verifier": "artifact_exists", "params": {},
                }]);
            }
            write_json(&path, &record);
            let payload = serde_json::to_vec(&json!({
                "sample": sample, "leg": leg, "text": "Boundary fixture. ".repeat(256),
            }))
            .unwrap();
            let artifact = InterfaceArtifactFile::write(root.join("boundary"), label, &payload)
                .unwrap()
                .capture_bounded(1024 * 1024, 64 * 1024 * 1024)
                .unwrap();
            let preparation_ms = preparation.elapsed().as_secs_f64() * 1_000.0;
            let admission = Instant::now();
            let writes_before = *writes.lock().unwrap();
            let outcome = admit_interface_artifact_arrival(
                &embedded,
                &mut log,
                &mut handle,
                &artifact,
                &source.into(),
            )
            .await
            .unwrap();
            if sample == 0 {
                let writes_after = *writes.lock().unwrap();
                cold_legs.push(json!({"source": source, "preparation_ms": preparation_ms,
                    "admission_ms": admission.elapsed().as_secs_f64() * 1_000.0,
                    "blob_puts": writes_after.0 - writes_before.0,
                    "blob_put_ms": (writes_after.1 - writes_before.1).as_secs_f64() * 1_000.0}));
            }
            assert_eq!(outcome.admission.records.len(), 1);
            let admitted = &outcome.admission.records[0];
            assert_eq!(admitted.run, run);
            assert_eq!(admitted.outbound_transfer_movements.len(), 1);
            assert_eq!(
                admitted.outbound_transfer_movements[0].to,
                Endpoint::Interface(destination.into())
            );
            assert!(admitted.outbound_refusal_movements.is_empty());
            assert_eq!(admitted.admitted_mark_outcome_movements.len(), leg);
            let seq = admitted.outbound_transfer_movements[0].seq;
            let persisted = log.try_movement_at(seq).unwrap().unwrap();
            assert_eq!(
                persisted.artifact,
                admitted.outbound_transfer_movements[0].artifact
            );
        }
        samples.push(started.elapsed());
    }

    drop(log);
    let replay = DurableLog::replay(root.join("ledger/movements.jsonl")).unwrap();
    for (kind, expected) in [
        (MovementKind::InterfaceArrival, SAMPLES * 2),
        (MovementKind::InterfaceOutbound, SAMPLES * 2),
        (
            MovementKind::MarkOutcomeAdmitted("task.summary.received".into()),
            SAMPLES,
        ),
    ] {
        assert_eq!(
            replay.iter().filter(|row| row.kind == kind).count(),
            expected
        );
    }
    assert!(
        !replay
            .iter()
            .any(|row| row.kind == MovementKind::DeliveryRefused)
    );
    assert_eq!(replay.first().unwrap().seq, Seq(0));
    let cold = samples[0];
    samples.sort();
    let max = *samples.last().unwrap();
    println!(
        "{}",
        json!({
            "probe": "framework_round_trip", "provider": "boundary_fixture",
            "project_ee_count": ee_count, "project_depth": depth,
            "background_runs": ee_count, "samples": SAMPLES,
            "model_reference_ms": MODEL_REFERENCE_MS,
            "framework_budget_ms": FRAMEWORK_BUDGET.as_millis(),
        "setup_ms": startup_ms, "cold_ms": cold.as_secs_f64() * 1_000.0,
        "cold_legs": cold_legs,
            "p50_ms": samples[SAMPLES / 2 - 1].as_secs_f64() * 1_000.0,
            "p95_ms": samples[SAMPLES * 95 / 100 - 1].as_secs_f64() * 1_000.0,
            "max_ms": max.as_secs_f64() * 1_000.0, "durable_movements": replay.len(),
        })
    );
    max
}
