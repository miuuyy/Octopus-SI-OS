use super::*;
use crate::property_check::check;
use proptest::prelude::*;

const CLOSING_LABELS: [&str; 3] = ["reply", "turn.error", "turn.interrupted.result"];

#[derive(Clone, Debug)]
struct Trace {
    calls: Vec<(bool, bool)>,
    arrival: Vec<usize>,
    upfront: bool,
    steers: Vec<usize>,
    closing: usize,
}

fn trace() -> impl Strategy<Value = Trace> {
    prop::collection::vec((any::<bool>(), any::<bool>()), 1..=5)
        .prop_flat_map(|mut calls| {
            calls.last_mut().unwrap().1 = true;
            let count = calls.len();
            (
                Just(calls),
                Just((0..count).collect::<Vec<_>>()).prop_shuffle(),
                any::<bool>(),
                prop::collection::vec(0..count, 0..=2),
                0..CLOSING_LABELS.len(),
            )
        })
        .prop_map(|(calls, arrival, upfront, steers, closing)| Trace { calls, arrival, upfront, steers, closing })
}

struct Called {
    call: ArtifactRef,
    tool: &'static str,
    correlation: String,
    held: usize,
    evidence: bool,
}

fn ids(artifacts: &[ArtifactRef]) -> Vec<ArtifactId> {
    artifacts.iter().map(|artifact| artifact.id.clone()).collect()
}

fn written(authored: &AuthoredRun) -> ArtifactRef {
    let item = &authored.pending_handoff.items[0];
    ArtifactRef { id: item.artifact_id.clone(), content: ContentHash::from_hex(&item.artifact_content_hash).unwrap() }
}

fn leave_on_a_non_terminal_write(fixture: &mut CompletedInputReplayFixture, door: &str, tool: &str, correlation: &str) -> ArtifactRef {
    let before = std::fs::read(&fixture.handoff.run_path).unwrap();
    let record = RunRecord::from_bytes(&before).unwrap();
    let mut context = effective_context_artifacts(&record, &fixture.handoff.config).unwrap();
    append_artifact(&mut context, fixture.input.id.clone());
    let file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join(format!("call-{correlation}")), "tool.call",
        &serde_json::to_vec(&serde_json::json!({"label":"tool.call","tool":tool,"arguments":{},"correlation_id":correlation})).unwrap()).unwrap();
    let identity = artifact_identity(&file, &fixture.handoff.var_dir).unwrap();
    let call = identity.artifact.clone();
    append_artifact(&mut context, call.id.clone());
    let authored = fixture.handoff.author_run_with_bindings(&before, &context, &[PreparedHandoff { identity, verified_inputs: Vec::new(),
        output_checkpoint: fixture.handoff.config.checkpoints.iter().find(|cp| cp.name == door).unwrap().clone() }],
        true, None, false, false, &[]).unwrap();
    fixture.install(&authored);
    fixture.edge(authored.content.as_bytes(), &authored.pending_handoff.items[0].checkpoint.id, &file, "worker".into(), MovementKind::InterfaceArrival);
    fixture.mark_item(&authored, 0);
    if authored.pending_handoff.context_publication.is_some() && !authored.pending_handoff.deferred_publication {
        fixture.publish(&authored);
    }
    fixture.reload_definition();
    call
}

fn steer(fixture: &mut CompletedInputReplayFixture, context: &mut Vec<ArtifactRef>, number: usize) {
    let file = InterfaceArtifactFile::write(fixture.handoff.var_dir.join(format!("steer-{number}")), "tool.result",
        &serde_json::to_vec(&serde_json::json!({"label":"tool.result","tool":"read","correlation_id":format!("steer-{number}"),"ok":true,"body":{"value":number}})).unwrap()).unwrap();
    let steer = artifact_identity(&file, &fixture.handoff.var_dir).unwrap().artifact;
    let current = std::fs::read(&fixture.handoff.run_path).unwrap();
    let record = RunRecord::from_bytes(&current).unwrap();
    let alternate = fixture.handoff.config.checkpoints.iter().find(|template| template.name == "alternate").unwrap().clone();
    let alternate = record.checkpoints.iter().find(|cp| record.active_checkpoints.contains(&cp.id)
        && checkpoint_record_matches_template(cp, &alternate, &record.id)).unwrap().id.clone();
    let source = fixture.rows.len() as u64;
    fixture.edge(&current, &alternate, &file, "provider".into(), MovementKind::InterfaceOutbound);
    fixture.select_source(source);
    let bound = fixture.consume_result(&steer, "interface.context.updated", false);
    context.push(steer);
    context.push(written(&bound));
}

fn drive(trace: &Trace) {
    let executor = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    executor.block_on(async {
        let closing = CLOSING_LABELS[trace.closing];
        let mut fixture = parallel_call_fixture();
        fixture.handoff.context_output_labels.insert(closing.into());
        fixture.handoff.session_snapshot_labels.insert(closing.into());
        let limits = fixture.handoff.ctx.required_host_io_limits().unwrap();
        let mut context = vec![fixture.input.clone()];
        let mut called = Vec::new();
        for (index, (search, ends_batch)) in trace.calls.iter().enumerate() {
            let (door, tool) = if *search { ("search", "search") } else { ("tool", "read") };
            let correlation = format!("call-{index}");
            let held = context.len();
            let call = if *ends_batch {
                fixture.calls(&[(door, tool, &correlation)]).1.remove(0)
            } else {
                leave_on_a_non_terminal_write(&mut fixture, door, tool, &correlation)
            };
            context.push(call.clone());
            called.push(Called { call, tool, correlation, held, evidence: !*ends_batch });
        }
        let waiting = fixture.result_inputs();
        assert_eq!(waiting.len(), called.len(), "every call of every batch has its own result wait");
        for entry in &called {
            let template = waiting.iter().find(|template| template.required_verified_artifacts[0].params["call_artifact"]["id"]
                == serde_json::json!(entry.call.id)).expect("a call has a result wait");
            assert_eq!(template.static_artifact_bindings.contains(&entry.call.id), entry.evidence,
                "only a call that left on a non-terminal write is held as evidence");
        }
        let mut results = called.iter().map(|_| None).collect::<Vec<_>>();
        if trace.upfront {
            for &index in &trace.arrival {
                results[index] = Some(fixture.admit_result(&called[index].call, called[index].tool, &called[index].correlation));
            }
        }
        let mut consumed = Vec::<(usize, ArtifactRef)>::new();
        for (position, &index) in trace.arrival.iter().enumerate() {
            for number in trace.steers.iter().enumerate().filter(|(_, at)| **at == position).map(|(number, _)| number) {
                steer(&mut fixture, &mut context, number);
            }
            let entry = &called[index];
            let (source, result) = match results[index].take() {
                Some(admitted) => admitted,
                None => fixture.admit_result(&entry.call, entry.tool, &entry.correlation),
            };
            fixture.select_source(source);
            assert_eq!(fixture.handoff.state.try_lock().unwrap().context_artifacts, ids(&context),
                "the published context holds every call, result and bound input in the order it was bound");
            assert_eq!(fixture.handoff.recover_source().await.unwrap(), ProviderSourceRecovery::Process);
            let expected = context[entry.held..].iter().filter(|artifact| entry.evidence || **artifact != entry.call).cloned().collect::<Vec<_>>();
            let mut state = fixture.handoff.state.try_lock().unwrap().clone();
            assert_eq!(state.result_rebase.clone().unwrap_or_default(), expected, "result {index} consumed at position {position}");
            let record = RunRecord::from_bytes(&std::fs::read(&fixture.handoff.run_path).unwrap()).unwrap();
            let (session, head, _) = recover_source_session_context_from_cache(&fixture.handoff.ctx.blobs_dir, limits,
                &ProviderHandoffSession::new(&fixture.handoff.ctx).unwrap(), &record, &fixture.handoff.config, source, &expected,
                &fixture.handoff.context_output_labels, &fixture.handoff.session_snapshot_labels).unwrap();
            state.session_context = session;
            state.session_snapshot_head = head;
            if !state.session_context.artifacts.iter().any(|held| held.source_artifact == result) {
                let (_, payload) = read_pinned_artifact_from_cache(&fixture.handoff.ctx.blobs_dir, limits, &result).unwrap();
                append_session_payload(&mut state.session_context, &format!("{}.result", entry.tool), &payload, &result, Some(source), "exact result input").unwrap();
            }
            append_artifact(&mut state.context_artifacts, result.id.clone());
            let at = |artifact: &ArtifactRef| state.session_context.artifacts.iter().position(|held| &held.source_artifact == artifact);
            for bound in context[1..].iter().chain([&result]) {
                assert!(at(bound).is_some(), "the session a snapshot is made of lost '{}'", bound.id.as_str());
            }
            for (earlier, earlier_result) in consumed.iter().map(|(earlier, result)| (*earlier, result)).chain([(index, &result)]) {
                assert!(at(&called[earlier].call) < at(earlier_result), "a result precedes its call in the session");
            }
            assert_eq!(result_inputs::snapshot_carried_context(&fixture.handoff.config, &state.context_artifacts,
                &state.static_context_artifacts, &state.session_context, |_| Ok(false)), Ok(Vec::new()),
                "a snapshot taken on result {index} covers every binding");
            if position + 1 < trace.arrival.len() {
                let before = RunRecord::from_bytes(&std::fs::read(&fixture.handoff.run_path).unwrap()).unwrap();
                let write = fixture.consume_result(&result, "interface.context.updated", true);
                assert_eq!(write.pending_handoff.rebase, expected, "the consuming write records the rebase it consumed against");
                for forge in [true, false] {
                    let mut forged: serde_json::Value = serde_json::from_str(&write.content).unwrap();
                    let rebase = forged["interface_state"]["provider"]["pending_handoff"]["rebase"].as_array_mut();
                    match (forge, rebase) {
                        (true, Some(rebase)) => rebase.push(serde_json::to_value(&fixture.input).unwrap()),
                        (false, Some(rebase)) if !rebase.is_empty() => { rebase.remove(0); }
                        _ => continue,
                    }
                    let forged = RunRecord::from_bytes(&serde_json::to_vec(&forged).unwrap()).unwrap();
                    assert!(result_inputs::validate_saved(&fixture.handoff.ctx, &before, &forged, &fixture.handoff.config).is_err(),
                        "a recorded rebase that {} is refused", if forge { "names context outside the difference" } else { "drops a binding" });
                }
                context.push(result.clone());
                context.push(written(&write));
                consumed.push((index, result));
                continue;
            }
            let mut two_heads = state.context_artifacts.clone();
            let stray = ArtifactId::new(format!("{closing}:{}", ContentHash::of_bytes(b"another head").to_hex()));
            two_heads.push(stray.clone());
            let beside = result_inputs::snapshot_carried_context(&fixture.handoff.config, &two_heads,
                &state.static_context_artifacts, &state.session_context, |id| Ok(id == &stray));
            assert!(beside.unwrap_err().contains(stray.as_str()), "no closing output binds a second session snapshot beside itself");
            let current = std::fs::read(&fixture.handoff.run_path).unwrap();
            let before = RunRecord::from_bytes(&current).unwrap();
            let answer = InterfaceArtifactFile::write(fixture.handoff.var_dir.join("closing"), closing,
                &serde_json::to_vec(&serde_json::json!({"label":closing,"text":"done"})).unwrap()).unwrap();
            let snapshot = materialize_session_snapshot(&answer, &state.session_context, state.session_snapshot_head.as_ref()).unwrap();
            let identity = artifact_identity(&snapshot, &fixture.handoff.var_dir).unwrap();
            let carrier = identity.artifact.clone();
            let closed = fixture.handoff.author_run_with_bindings(&current, &[carrier.id.clone()], &[PreparedHandoff { identity, verified_inputs: Vec::new(),
                output_checkpoint: fixture.handoff.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone() }],
                true, None, false, true, &[]).unwrap();
            assert_eq!(closed.pending_handoff.rebase, expected, "the closing write records the rebase it consumed its result against");
            result_inputs::validate_saved(&fixture.handoff.ctx, &before, &RunRecord::from_bytes(closed.content.as_bytes()).unwrap(), &fixture.handoff.config)
                .expect("the write that closes a turn is a valid saved record");
        }
    });
}

#[test]
fn every_call_and_result_of_a_generated_turn_stays_covered_until_the_turn_closes() {
    check(64, trace(), |trace| {
        drive(&trace);
        Ok(())
    });
}

fn addressed(label: &str, body: &[u8]) -> ArtifactRef {
    let content = ContentHash::of_bytes(body);
    ArtifactRef { id: ArtifactId::new(format!("{label}:{}", content.to_hex())), content }
}

#[test]
fn a_snapshot_keeps_what_it_does_not_capture_and_refuses_only_a_second_head() {
    let fixture = parallel_call_fixture();
    let config = fixture.handoff.config.clone();
    let snapshot_labels = CLOSING_LABELS.iter().map(|label| label.to_string()).collect::<BTreeSet<_>>();
    let labels = ["tool.call", "tool.result", "operator.message", "reply", "turn.error", "turn.interrupted.result"];
    let universe = (0..12u8).map(|index| addressed(labels[index as usize % 6], &[index])).collect::<Vec<_>>();
    let declared = universe.iter().enumerate().map(|(index, artifact)| (artifact.id.clone(), labels[index % 6].to_string())).collect::<BTreeMap<_, _>>();
    let some = || prop::sample::subsequence(universe.clone(), 0..=universe.len());
    check(256, (some().prop_shuffle(), some(), some()), |(bound, statics, captured)| {
        let captured = ResolvedSessionContext { provider_interface: "provider".into(), artifacts: captured.into_iter().map(|artifact| SessionContextArtifact {
            label: declared[&artifact.id].clone(), payload: serde_json::json!({}), source_artifact: artifact, source_movement_seq: None,
        }).collect() };
        let outside = bound.iter().filter(|artifact| !statics.contains(artifact)
            && !captured.artifacts.iter().any(|held| &held.source_artifact == *artifact)).map(|artifact| artifact.id.clone()).collect::<Vec<_>>();
        let second_head = outside.iter().find(|id| snapshot_labels.contains(&declared[*id]));
        let carried = result_inputs::snapshot_carried_context(&config, &ids(&bound), &ids(&statics), &captured,
            |id| Ok(snapshot_labels.contains(&declared[id])));
        match second_head {
            Some(head) => prop_assert!(carried.as_ref().is_err_and(|error| error.contains(head.as_str())), "{:?}", carried),
            None => prop_assert_eq!(carried, Ok(outside)),
        }
        Ok(())
    });
}

#[test]
fn a_tool_call_gets_an_output_route_only_inside_the_formed_grant() {
    let fixture = parallel_call_fixture();
    let config = fixture.handoff.config.clone();
    let tools = ["read", "search", "write", "delete"];
    check(256, (prop::sample::subsequence(tools.to_vec(), 0..=tools.len()), 0..tools.len()), |(granted, tool)| {
        let granted = granted.into_iter().map(str::to_string).collect::<BTreeSet<_>>();
        let tool = tools[tool];
        let route = config.output_checkpoint(&ProviderHandoffRoute::Tool { tool: tool.into() }, &granted, &BTreeSet::new(), "tool.call")
            .map(|owner| owner.name);
        match (granted.contains(tool), tool) {
            (false, _) => prop_assert!(route.as_ref().is_err_and(|error| error.contains("outside the formed invocation grant")), "{:?}", route),
            (true, "read") => prop_assert_eq!(route, Ok("tool".to_string())),
            (true, "search") => prop_assert_eq!(route, Ok("search".to_string())),
            (true, _) => prop_assert!(route.as_ref().is_err_and(|error| error.contains("no descriptor-derived output owner")), "{:?}", route),
        }
        Ok(())
    });
}

#[test]
fn a_session_prefix_fingerprint_ignores_key_order_and_ledger_coordinates_and_nothing_else() {
    let entry = (0..3usize, prop::collection::btree_map(0u8..8, any::<u8>(), 0..6), prop::option::of(0u64..64));
    check(256, (prop::collection::vec(entry, 1..6), any::<prop::sample::Index>()), |(entries, altered)| {
        let session = |reverse: bool, ground: bool, altered: Option<usize>| entries.iter().enumerate().map(|(index, (label, fields, seq))| {
            let mut fields = fields.iter().collect::<Vec<_>>();
            if reverse { fields.reverse(); }
            let mut payload = serde_json::Map::new();
            for (key, value) in fields { payload.insert(format!("field-{key}"), serde_json::json!(value)); }
            if altered == Some(index) { payload.insert("altered".into(), serde_json::json!(true)); }
            let label = ["tool.call", "tool.result", "operator.message"][*label];
            SessionContextArtifact { label: label.into(), source_artifact: addressed(label, &serde_json::to_vec(&payload).unwrap()),
                payload: payload.into(), source_movement_seq: if ground { Some(seq.unwrap_or(index as u64) + 1) } else { *seq } }
        }).collect::<Vec<_>>();
        let recorded = session(false, false, None);
        let prefix = session_snapshot_prefix_fingerprint(&recorded).unwrap();
        prop_assert_eq!(&prefix, &session_snapshot_prefix_fingerprint(&recorded).unwrap());
        let grounded = session(true, true, None);
        prop_assert_eq!(&prefix, &session_snapshot_prefix_fingerprint(&grounded).unwrap());
        prop_assert_ne!(session_artifacts_fingerprint(&recorded).unwrap(), session_artifacts_fingerprint(&grounded).unwrap());
        let changed = session(false, false, Some(altered.index(entries.len())));
        prop_assert_ne!(&prefix, &session_snapshot_prefix_fingerprint(&changed).unwrap());
        prop_assert_ne!(&prefix, &session_snapshot_prefix_fingerprint(&recorded[..recorded.len() - 1]).unwrap());
        Ok(())
    });
}
