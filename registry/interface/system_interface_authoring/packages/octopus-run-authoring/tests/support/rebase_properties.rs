use super::*;
use crate::property_check::check;
use proptest::prelude::*;
use serde_json::json;

fn address(label: &str, body: &[u8]) -> ArtifactRef {
    let content = ContentHash::of_bytes(body);
    ArtifactRef { id: ArtifactId::new(format!("{label}:{}", content.to_hex())), content }
}

fn waiting_call() -> (RunAuthoringConfig, ArtifactRef) {
    let binding = |id: &str| json!(AuthoredInterfaceBinding::all(InterfaceId::new(id)));
    let checkpoint = |name: &str, role: &str, source: &str, target: &str| json!({
        "name":name,"role":role,"interface_binding":binding(source),"outbound_interface_bindings":[binding(target)],
        "include_context":role == "stable","required_verified_artifacts":[],
    });
    let call = address("tool.call", b"exec");
    let mut wait = checkpoint("wait", "result_input", "room", "provider");
    wait["include_context"] = json!(true);
    wait["required_verified_artifacts"] = json!([{"mark":"tool.result.corresponds","artifact_label":"exec.result",
        "verifier":"tool_result_correspondence","params":{"call_artifact":call,"tool":"exec","correlation_id":"c1","params":{}}}]);
    let config = serde_json::from_value(json!({"run":"run","worker":"worker","provider_interface":"provider",
        "authoring_source_interface":"writer","file_write_tool":"write","file_write_label":"tool.call","file_write_result_label":"write.result",
        "checkpoints":[checkpoint("input","stable","client","provider"), checkpoint("writer_call","authoring_call","writer","writer_room"),
            checkpoint("writer_result","authoring_result","writer_room","provider"), wait]})).unwrap();
    (config, call)
}

fn record(config: &RunAuthoringConfig, context: &[ArtifactRef], admitted: &[ArtifactRef]) -> RunRecord {
    let bindings = |artifacts: &[ArtifactRef]| artifacts.iter().map(|artifact| json!({"id":artifact.id})).collect::<Vec<_>>();
    let mut rows = config.checkpoints[..3].iter()
        .map(|cp| materialize_checkpoint(cp, &cp.name, &config.run, &[json!({"id":"role"})])).collect::<Vec<_>>();
    rows[0]["artifact_bindings"] = json!(bindings(context));
    rows.push(materialize_checkpoint(&config.checkpoints[3], "wait", &config.run, &bindings(admitted)));
    RunRecord::from_bytes(&serde_json::to_vec(&json!({"id":"run","ee":"ee","worker":"worker","trace":"run","status":"running",
        "checkpoints":rows,"active_checkpoints":["input","writer_call","writer_result","wait"],
        "interface_state":{"provider":{"context_generation":1,"authoring_generation":1,
            "frontier_checkpoint_ids":["writer_call","writer_result"]}}})).unwrap()).unwrap()
}

#[derive(Clone, Debug)]
enum Change {
    None,
    Lose(prop::sample::Index),
    Swap(prop::sample::Index, prop::sample::Index),
}

#[test]
fn a_result_rebase_is_exactly_what_was_bound_while_its_call_waited() {
    let (config, call) = waiting_call();
    let case = (0..5usize).prop_flat_map(|admitted| (
        Just(admitted),
        prop::collection::vec(0..3usize, admitted + 1),
        any::<bool>(),
        prop::option::of(any::<prop::sample::Index>()),
        prop_oneof![
            3 => Just(Change::None),
            1 => any::<prop::sample::Index>().prop_map(Change::Lose),
            1 => (any::<prop::sample::Index>(), any::<prop::sample::Index>()).prop_map(|(a, b)| Change::Swap(a, b)),
        ],
    ));
    check(256, case, |(admitted, added, evidence, published, change)| {
        let admitted = (0..admitted).map(|index| address("operator.message", &[b'a', index as u8])).collect::<Vec<_>>();
        let mut context = Vec::new();
        let mut rebase = Vec::new();
        for (gap, count) in added.iter().enumerate() {
            for index in 0..*count {
                let bound = address("turn.control.acknowledged", &[b'n', gap as u8, index as u8]);
                context.push(bound.clone());
                rebase.push(bound);
            }
            context.extend(admitted.get(gap).cloned());
        }
        if let Some(at) = published {
            let at = at.index(context.len() + 1);
            context.insert(at, call.clone());
            if evidence {
                let before = context[..at].iter().filter(|bound| rebase.contains(bound)).count();
                rebase.insert(before, call.clone());
            }
        }
        let mut template = config.checkpoints[3].clone();
        if evidence {
            template.static_artifact_bindings.push(call.id.clone());
        }
        let mut row = admitted.clone();
        row.push(call.clone());
        let rebased = |context: &[ArtifactRef]| {
            let record = record(&config, context, &row);
            result_rebase(&record, &config, &template, record.checkpoint(&CheckpointId::new("wait")).unwrap())
        };
        match change {
            Change::None => {
                prop_assert_eq!(rebased(&context), Ok(rebase.clone()));
                prop_assert_eq!(rebased(&context), Ok(rebase));
            }
            Change::Lose(_) if admitted.is_empty() => {}
            Change::Lose(lost) => {
                let lost = lost.index(admitted.len());
                context.retain(|bound| bound != &admitted[lost]);
                let outcome = rebased(&context);
                if lost + 1 < admitted.len() {
                    prop_assert!(outcome.as_ref().is_err_and(|error| error.contains("reordered")), "{:?}", outcome);
                } else if lost == 0 && !rebase.is_empty() {
                    prop_assert_eq!(outcome, Ok(rebase));
                } else {
                    prop_assert!(outcome.as_ref().is_err_and(|error| error.contains("left the run")), "{:?}", outcome);
                }
            }
            Change::Swap(..) if admitted.len() < 2 => {}
            Change::Swap(first, second) => {
                let first = first.index(admitted.len());
                let second = (first + 1 + second.index(admitted.len() - 1)) % admitted.len();
                let at = |artifact: &ArtifactRef| context.iter().position(|bound| bound == artifact).unwrap();
                let (first, second) = (at(&admitted[first]), at(&admitted[second]));
                context.swap(first, second);
                let outcome = rebased(&context);
                prop_assert!(outcome.as_ref().is_err_and(|error| error.contains("reordered")), "{:?}", outcome);
            }
        }
        Ok(())
    });
}
