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
