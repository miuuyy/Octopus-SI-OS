#[test]
fn inbound_artifact_defers_against_current_admitted_pending_call() {
    let temp = tempfile::tempdir().unwrap();
    let descriptor = descriptor();
    let ctx = context(temp.path(), &descriptor);
    let run = RunId::new("run-a");
    let settled = artifact(&temp.path().join("settled"), "turn.agent_message", json!({
        "label":"turn.agent_message", "codex_continuity":continuity(8, None),
    }));
    let settled_ref = cache_artifact(&ctx, &settled);
    let pending = artifact(&temp.path().join("pending"), "tool.call", json!({
        "label":"tool.call", "codex_continuity":continuity(9, Some("pending")),
    }));
    let pending_ref = cache_artifact(&ctx, &pending);
    let inbound = artifact(&temp.path().join("input"), "tool.call", json!({
        "tool":"another.operation", "arguments":{}, "correlation_id":"peer-call",
    }));
    let inbound_ref = cache_artifact(&ctx, &inbound);
    let mut metadata = runtime_metadata();
    metadata.artifact_binding_refs = vec![settled_ref.clone()];
    let turn = QueuedTurn {
        envelope: InterfaceInvocationEnvelope {
            artifact:inbound_ref.clone(), artifact_file:inbound,
            artifact_binding_payloads:vec![octopus_interfaces::ArtifactBindingPayload::new(
                settled_ref.clone(), settled.read_payload_bytes().unwrap(),
            )],
            metadata:Some(metadata.clone()),
        },
        seq:2,
    };
    let mut source = movement(2, MovementKind::InterfaceOutbound,
        Endpoint::Interface(InterfaceId::new("world_operator")),
        Endpoint::Interface(descriptor.id.clone()), inbound_ref);
    source.invocation_metadata = Some(metadata.into());
    write_movements(&ctx.movements_path, &[
        movement(0, MovementKind::InterfaceArrival, Endpoint::Interface(descriptor.id.clone()),
            Endpoint::Interface(InterfaceId::new("system_worker_test")), settled_ref.clone()),
        movement(1, MovementKind::InterfaceArrival, Endpoint::Interface(descriptor.id.clone()),
            Endpoint::Interface(InterfaceId::new("system_worker_test")), pending_ref.clone()),
        source,
    ]);
    assert!(is_provider_turn_input(&descriptor, &turn.envelope).unwrap());
    assert!(!operator_turn_requires_deferral(&ctx, &descriptor, &run, &turn, &[]).unwrap());
    let mut current = vec![
        serde_json::from_value(session_entry("turn.agent_message",
            serde_json::from_slice(&settled.read_payload_bytes().unwrap()).unwrap(), settled_ref, 0)).unwrap(),
        serde_json::from_value(session_entry("tool.call",
            serde_json::from_slice(&pending.read_payload_bytes().unwrap()).unwrap(), pending_ref, 1)).unwrap(),
    ];
    assert!(operator_turn_requires_deferral(&ctx, &descriptor, &run, &turn, &current).unwrap());
    let ended = artifact(&temp.path().join("ended"), "turn.agent_message", json!({
        "label":"turn.agent_message", "codex_continuity":continuity(10, None),
    }));
    let ended_ref = cache_artifact(&ctx, &ended);
    let mut after_turn = current.clone();
    after_turn.push(serde_json::from_value(session_entry("turn.agent_message",
        serde_json::from_slice(&ended.read_payload_bytes().unwrap()).unwrap(), ended_ref, 3)).unwrap());
    assert!(!operator_turn_requires_deferral(&ctx, &descriptor, &run, &turn, &after_turn).unwrap(),
        "an input deferred behind a call no longer waits once that turn has ended in the session");
    current[1].payload["codex_continuity"]["run"] = json!("unrelated-run");
    assert!(operator_turn_requires_deferral(&ctx, &descriptor, &run, &turn, &current).is_err());
}

#[tokio::test]
async fn deferred_turn_ignores_an_older_error_carrier_but_a_persisted_rebase_must_match() {
    let temp = tempfile::tempdir().unwrap();
    let descriptor = descriptor();
    let ctx = context(temp.path(), &descriptor);
    let run = RunId::new("run-a");
    let provider = Endpoint::Interface(descriptor.id.clone());
    let worker = Endpoint::Interface(InterfaceId::new("system_worker_test"));
    let snapshot = |label: &str, generation, pending| {
        let mut body = json!({"label":label, "codex_continuity":continuity(generation, pending),
            "session_context":{"schema_version":3,"provider_interface":descriptor.id,"artifacts":[]}});
        if label == "turn.error" { body["will_retry"] = json!(false); }
        artifact(&temp.path().join("carriers"), label, body)
    };
    let bound = snapshot("interface.artifact.output", 7, Some("pending"));
    let bound_ref = cache_artifact(&ctx, &bound);
    let message = artifact(&temp.path().join("input"), "operator.message", json!({"prompt":"next task"}));
    let mut metadata = runtime_metadata();
    metadata.artifact_binding_refs = vec![bound_ref.clone()];
    let turn = QueuedTurn { seq:1, envelope:InterfaceInvocationEnvelope {
        artifact:cache_artifact(&ctx, &message), artifact_file:message, metadata:Some(metadata.clone()),
        artifact_binding_payloads:vec![octopus_interfaces::ArtifactBindingPayload::new(
            bound_ref.clone(), bound.read_payload_bytes().unwrap())],
    }};
    let mut source = movement(turn.seq, MovementKind::InterfaceOutbound,
        Endpoint::Interface(InterfaceId::new("world_operator")), provider.clone(), turn.envelope.artifact.clone());
    source.invocation_metadata = Some(metadata.into());
    let stale = cache_artifact(&ctx, &snapshot("turn.error", 6, None));
    write_movements(&ctx.movements_path, &[
        movement(0, MovementKind::InterfaceArrival, provider.clone(), worker.clone(), bound_ref.clone()),
        source, movement(2, MovementKind::InterfaceArrival, provider.clone(), worker.clone(), stale.clone()),
    ]);
    persist_deferred_provider_turn(&ctx, &run, &turn).unwrap();
    let deferred = DeferredProviderTurn {run:run.clone(), turn, release:None};
    let controls = ActiveTurnControls::default();
    let check = |deferred| prepare_deferred_provider_turn(&ctx, &descriptor, &run, deferred, &controls);
    let scan = latest_provider_session_carrier(&ctx, &run, 1, &codex_session_snapshot_labels(&descriptor).unwrap(), None).unwrap();
    let candidate = scan.latest.expect("the failed turn has an admitted session carrier");
    assert_eq!(candidate.artifact, stale);
    assert!(matches!(check(&deferred).await.unwrap(), DeferredTurnReadiness::AwaitingSessionSettlement));
    assert!(load_deferred_provider_turns(&ctx, &descriptor).unwrap()[&1].release.is_none());
    let invalid = DeferredProviderTurn {run:run.clone(),turn:deferred.turn.clone(),
        release:Some(DeferredProviderTurnRelease::SessionRebase {
            source_session_carrier:Some(bound_ref), rebased_session_carrier:candidate,
        })};
    assert!(check(&invalid).await.err().unwrap().contains("not a monotonic successor"));

    let current = cache_artifact(&ctx, &snapshot("interface.artifact.output", 8, None));
    append_movement(&ctx.movements_path, &movement(3, MovementKind::InterfaceArrival, provider, worker, current.clone()));
    let DeferredTurnReadiness::Ready(ready) = check(&deferred).await.unwrap() else { panic!("actual successor did not release the input") };
    assert_eq!(ready.seq, deferred.turn.seq);
    assert_eq!(ready.envelope.artifact, deferred.turn.envelope.artifact);
    assert_eq!(ready.envelope.artifact_binding_payloads[0].artifact, current);
}
