use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use octopus_ledger::{GrantProjection, RefusedReason, VerifierPackagePin};
use octopus_run_record::GrantEpoch;
use octopus_runtime::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestRunner};
use serde_json::json;

const INTERFACES: [&str; 2] = ["world_test", "world_client"];
const MAX_BATCH: usize = 5;
const SEED: u64 = 0x0C70_9005;

fn check<S: Strategy>(
    cases: u32,
    strategy: S,
    property: impl Fn(S::Value) -> Result<(), TestCaseError>,
) {
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = cases;
    }
    if config.rng_seed == RngSeed::Random {
        config.rng_seed = RngSeed::Fixed(SEED);
    }
    config.failure_persistence = None;
    let seed = config.rng_seed;
    if let Err(failure) = TestRunner::new(config).run(&strategy, property) {
        panic!("{failure}\nseed {seed}; set PROPTEST_RNG_SEED to search from another one");
    }
}

struct FixtureGrant(CapSet<InterfaceId>);

impl RunGrantResolver for FixtureGrant {
    fn resolve_run_grant(&self, query: RunGrantQuery<'_>) -> Result<RunGrantResolution, String> {
        let grant = form_policy(
            ContextGrant {
                interfaces: CapSet::from_allowed(INTERFACES.map(InterfaceId::new)),
                mark_outcomes: CapSet::from_allowed((0..MAX_BATCH).map(mark)),
                ..Default::default()
            },
            &[PolicyLayer {
                interfaces: Some(self.0.clone()),
                ..Default::default()
            }],
            &[PolicyLayer {
                interfaces: Some(CapSet::from_allowed(
                    query.authored_interfaces.iter().cloned(),
                )),
                ..Default::default()
            }],
            FormationPin::default(),
        );
        let projection = GrantProjection::from_formed(&grant);
        RunGrantResolution::new(grant, projection)
    }
}

struct FixtureVerifier;

impl VerifiedArtifactAdmission for FixtureVerifier {
    fn prepare_required_verified_artifact(
        &self,
        required: &RequiredVerifiedArtifact,
        input: VerificationInput,
    ) -> Result<Box<dyn PreparedVerification>, VerifiedArtifactError> {
        assert_eq!(required.artifact_label, input.artifact_label);
        Ok(Box::new(ReadyVerification::passed(
            Mark::passed_with_class(
                required.mark.clone(),
                input.checkpoint,
                input.run,
                input.epoch,
                MarkClass::Success,
                input.artifact,
                required.verifier.clone(),
                VerifierPackagePin {
                    descriptor_content: ContentHash::of_bytes(b"fixture verifier descriptor"),
                    script_content: ContentHash::of_bytes(b"fixture verifier script"),
                },
                json!({"passed": true}),
            ),
        )))
    }
}

fn mark(index: usize) -> MarkId {
    MarkId::new(format!("result-{index}.ok"))
}

fn label(index: usize) -> String {
    format!("result-{index}")
}

fn gate(dir: &Path, client_granted: bool) -> Runtime {
    let catalog = RuntimeInterfaceCatalog::from_classes(INTERFACES.map(|name| {
        (
            InterfaceId::new(name),
            InterfaceClass::new(
                InterfaceType::World,
                InterfaceRuntimeDisposition::ExternalReader,
            )
            .unwrap(),
        )
    }));
    let runtime = Runtime::open(RuntimeServices {
        interface_catalog: Arc::new(catalog),
    })
    .unwrap();
    runtime.set_run_records_dir(dir.to_path_buf());
    let allowed = &INTERFACES[..if client_granted { 2 } else { 1 }];
    runtime.set_run_grant_resolver(Arc::new(FixtureGrant(CapSet::from_allowed(
        allowed.iter().map(|name| InterfaceId::new(*name)),
    ))));
    runtime.set_verified_artifact_admission(Arc::new(FixtureVerifier));
    runtime
}

fn arrival(label: &str) -> InterfaceArtifactAdmission {
    InterfaceArtifactAdmission {
        run: RunId::new("r1"),
        checkpoint: Some(CheckpointId::new("c1")),
        epoch: GrantEpoch(0),
        interface: InterfaceId::new("world_test"),
        artifact: ArtifactRef {
            id: ArtifactId::new(label),
            content: ContentHash::of_bytes(label.as_bytes()),
        },
        artifact_label: label.into(),
        run_record_hash: None,
    }
}

fn proof_rows(observation: &Observation, label: &str, seq: u64) -> [Movement; 2] {
    let runtime = observation.invocation_metadata.runtime.as_ref().unwrap();
    let mark = &observation.admitted_mark_outcomes[0];
    let arrival = Movement {
        seq: Seq(seq),
        lamport: Lamport(seq + 1),
        recorded_at_unix_ms: seq + 1,
        trace: TraceId("gate-trace".into()),
        kind: MovementKind::InterfaceArrival,
        from: Endpoint::Interface(runtime.interface.clone()),
        to: Endpoint::Interface(runtime.worker.clone()),
        artifact: Some(self::arrival(label).artifact),
        invocation_metadata: Some(observation.invocation_metadata.clone().into()),
    };
    let mark = Movement {
        seq: Seq(seq + 1),
        lamport: Lamport(seq + 2),
        recorded_at_unix_ms: seq + 2,
        kind: MovementKind::MarkOutcomeAdmitted(mark.name.clone()),
        invocation_metadata: Some(mark.clone().into()),
        ..arrival.clone()
    };
    [arrival, mark]
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "octopus-gate-trace-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn write_record(&self, batch: usize, contract: bool, presentation: u32) {
        let params = if contract { json!({"expected": "changed"}) } else { json!({}) };
        let record = json!({
            "id": "r1",
            "ee": "ee1",
            "worker": "system_worker_flash",
            "trace": "trace:r1",
            "status": "running",
            "interface_state": {"world_test": {"presentation": presentation}},
            "checkpoints": [{
                "id": "c1",
                "run": "r1",
                "trace": "trace:r1:c1",
                "epoch": 0,
                "interface_binding": AuthoredInterfaceBinding::all(InterfaceId::new("world_test")),
                "outbound_interface_bindings": [
                    AuthoredInterfaceBinding::all(InterfaceId::new("world_client"))
                ],
                "artifact_bindings": [],
                "required_verified_artifacts": (0..batch).map(|index| json!({
                    "mark": mark(index), "artifact_label": label(index),
                    "verifier": "fixture", "params": params,
                })).collect::<Vec<_>>(),
            }],
            "active_checkpoints": ["c1"]
        });
        std::fs::write(self.0.join("r1.json"), serde_json::to_vec(&record).unwrap()).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Debug)]
enum Step {
    Arrive(usize),
    Stray,
    Coma,
    EditState,
    EditContract,
}

#[derive(Clone, Debug)]
struct Trace {
    batch: usize,
    client_granted: bool,
    steps: Vec<Step>,
}

fn trace() -> impl Strategy<Value = Trace> {
    (1..=MAX_BATCH, any::<bool>()).prop_flat_map(|(batch, client_granted)| {
        let order = Just((0..batch).collect::<Vec<_>>()).prop_shuffle();
        let between = prop::collection::vec(
            (
                any::<prop::sample::Index>(),
                prop_oneof![
                    3 => (0..batch).prop_map(Step::Arrive),
                    1 => Just(Step::Stray),
                    2 => Just(Step::Coma),
                    2 => Just(Step::EditState),
                    1 => Just(Step::EditContract),
                ],
            ),
            0..8,
        );
        (order, between).prop_map(move |(order, between)| {
            let mut steps = order.into_iter().map(Step::Arrive).collect::<Vec<_>>();
            for (at, step) in between {
                steps.insert(at.index(steps.len() + 1), step);
            }
            Trace { batch, client_granted, steps }
        })
    })
}

fn run(trace: &Trace) -> Result<Vec<String>, TestCaseError> {
    let scratch = Scratch::new();
    let executor = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let (mut contract, mut presentation) = (false, 0);
    scratch.write_record(trace.batch, contract, presentation);
    let mut runtime = gate(&scratch.0, trace.client_granted);
    let mut rows = Vec::<Movement>::new();
    let mut admitted = BTreeMap::<usize, String>::new();
    let mut admitted_contract = None;
    let mut completions = 0;
    let mut observed = Vec::new();
    for step in &trace.steps {
        match step {
            Step::Arrive(index) => {
                let label = label(*index);
                let outcome = executor.block_on(runtime.admit_bound_interface_checkpoint(arrival(&label)));
                if admitted.contains_key(index) {
                    let Err(error) = outcome else {
                        return Err(TestCaseError::fail(format!("{label} was admitted twice")));
                    };
                    prop_assert_eq!(error.refused_reason(), RefusedReason::CheckpointSpent, "{}", error);
                    observed.push(format!("{label}: spent"));
                    continue;
                }
                if admitted_contract.is_some_and(|under| under != contract) {
                    let Err(error) = outcome else {
                        return Err(TestCaseError::fail(format!("{label} joined results admitted under another contract")));
                    };
                    prop_assert_eq!(error.refused_reason(), RefusedReason::StaleEpoch, "{}", error);
                    observed.push(format!("{label}: stale"));
                    continue;
                }
                let observation = match outcome {
                    Ok(observation) => observation,
                    Err(error) => return Err(TestCaseError::fail(format!("{label} was refused: {error}"))),
                };
                prop_assert_eq!(observation.admitted_mark_outcomes.len(), 1);
                prop_assert_eq!(&observation.admitted_mark_outcomes[0].name, &mark(*index));
                let snapshot = observation.invocation_metadata.runtime.as_ref()
                    .and_then(|runtime| runtime.run_record_hash.clone())
                    .ok_or_else(|| TestCaseError::fail("an admission names no run-record snapshot"))?;
                admitted.insert(*index, snapshot);
                admitted_contract = Some(contract);
                let complete = admitted.len() == trace.batch;
                prop_assert_eq!(
                    observation.invocation_metadata.checkpoint_admission.as_ref().map(|admission| admission.complete),
                    Some(complete)
                );
                let released = observation.outbound_artifact_transfers.iter().map(|transfer| {
                    (transfer.artifact.id.as_str().to_string(), transfer.to_interface.as_str().to_string(),
                        transfer.invocation_metadata.runtime.as_ref().and_then(|runtime| runtime.run_record_hash.clone()))
                }).collect::<Vec<_>>();
                let refused = observation.refused_outbound_artifact_transfers.iter()
                    .map(|refusal| (refusal.artifact.id.as_str().to_string(), refusal.to_interface.as_str().to_string()))
                    .collect::<Vec<_>>();
                let batch = || admitted.iter().map(|(index, snapshot)| (self::label(*index), "world_client".to_string(), Some(snapshot.clone())));
                if complete && trace.client_granted {
                    prop_assert_eq!(&released, &batch().collect::<Vec<_>>());
                    prop_assert!(refused.is_empty());
                } else if complete {
                    prop_assert!(released.is_empty(), "a result left for an interface outside the grant");
                    prop_assert_eq!(&refused, &batch().map(|(label, target, _)| (label, target)).collect::<Vec<_>>());
                } else {
                    prop_assert!(released.is_empty() && refused.is_empty(), "a result left before its batch was complete");
                }
                completions += usize::from(complete);
                rows.extend(proof_rows(&observation, &label, rows.len() as u64));
                observed.push(format!("{label}: admitted complete={complete} released={released:?} refused={refused:?}"));
            }
            Step::Stray => {
                let outcome = executor.block_on(runtime.admit_bound_interface_checkpoint(arrival("stray")));
                let Err(error) = outcome else {
                    return Err(TestCaseError::fail("an artifact no requirement names was admitted"));
                };
                observed.push(format!("stray: {:?}", error.refused_reason()));
            }
            Step::Coma => {
                runtime = gate(&scratch.0, trace.client_granted);
                for _ in 0..2 {
                    if let Err(error) = runtime.reconstruct_spent_checkpoints_from_movements(&rows) {
                        return Err(TestCaseError::fail(format!("recorded movements did not replay: {error}")));
                    }
                }
                observed.push("coma".into());
            }
            Step::EditState => {
                presentation += 1;
                scratch.write_record(trace.batch, contract, presentation);
                observed.push("state edited".into());
            }
            Step::EditContract => {
                contract = !contract;
                scratch.write_record(trace.batch, contract, presentation);
                observed.push("contract edited".into());
            }
        }
    }
    prop_assert_eq!(completions, usize::from(admitted.len() == trace.batch), "a batch closes exactly once");
    Ok(observed)
}

#[test]
fn a_batch_of_results_is_admitted_once_in_any_order_across_replays_restarts_and_edits() {
    check(128, trace(), |trace| {
        let observed = run(&trace)?;
        prop_assert_eq!(&observed, &run(&trace)?);
        Ok(())
    });
}

#[test]
fn a_formed_grant_is_the_same_for_any_order_of_its_inputs_and_never_wider_than_each() {
    let names = || prop::collection::vec(0u8..12, 0..12);
    check(256, (names(), names(), names(), names(), names(), any::<u64>()), |(ceiling, ee, bound, marks, ee_marks, turn)| {
        let interface = |id: &u8| InterfaceId::new(format!("world_{id}"));
        let outcome = |id: &u8| MarkId::new(format!("mark_{id}.ok"));
        let rotated = |ids: &[u8]| {
            let mut ids = ids.to_vec();
            if !ids.is_empty() {
                let by = (turn % ids.len() as u64) as usize;
                ids.rotate_left(by);
                ids.reverse();
            }
            ids
        };
        let form = |ceiling: &[u8], ee: &[u8], bound: &[u8], marks: &[u8], ee_marks: &[u8]| {
            GrantProjection::from_formed(&form_policy(
                ContextGrant {
                    interfaces: CapSet::from_allowed(ceiling.iter().map(interface)),
                    mark_outcomes: CapSet::from_allowed(marks.iter().map(outcome)),
                    ..Default::default()
                },
                &[PolicyLayer {
                    interfaces: Some(CapSet::from_allowed(ee.iter().map(interface))),
                    mark_outcomes: Some(CapSet::from_allowed(ee_marks.iter().map(outcome))),
                    ..Default::default()
                }],
                &[PolicyLayer {
                    interfaces: Some(CapSet::from_allowed(bound.iter().map(interface))),
                    ..Default::default()
                }],
                FormationPin::default(),
            ))
        };
        let formed = form(&ceiling, &ee, &bound, &marks, &ee_marks);
        let again = form(&rotated(&ceiling), &rotated(&ee), &rotated(&bound), &rotated(&marks), &rotated(&ee_marks));
        prop_assert_eq!(serde_json::to_vec(&formed).unwrap(), serde_json::to_vec(&again).unwrap());
        let set = |ids: &[u8]| ids.iter().copied().collect::<BTreeSet<_>>();
        let expected = set(&ceiling).intersection(&set(&ee)).copied().collect::<BTreeSet<_>>()
            .intersection(&set(&bound)).map(|id| interface(id).as_str().to_string()).collect::<BTreeSet<_>>();
        prop_assert_eq!(formed.interfaces.iter().cloned().collect::<BTreeSet<_>>(), expected);
        let expected = set(&marks).intersection(&set(&ee_marks)).map(|id| outcome(id).as_str().to_string()).collect::<BTreeSet<_>>();
        prop_assert_eq!(formed.mark_outcomes.iter().cloned().collect::<BTreeSet<_>>(), expected);
        Ok(())
    });
}
