#[test]
fn completed_message_identity_preserves_interleaved_native_items() {
    let mut turn: octopus_appserver_proto::Turn = serde_json::from_value(serde_json::json!({
        "id": "turn-a", "status": "completed", "itemsView": "full",
        "items": [
            {"type":"agentMessage", "id":"update", "text":"same", "phase":"final_answer", "delivery":"async"},
            {"type":"agentMessage", "id":"before", "text":"same", "phase":"commentary"},
            {"type":"agentMessage", "id":"final", "text":"same", "phase":"final_answer"},
            {"type":"agentMessage", "id":"unknown", "text":"same", "phase":null},
            {"type":"agentMessage", "id":"after", "text":"same", "phase":"commentary"}
        ]
    })).unwrap();
    for item in &turn.items {
        let ThreadItem::AgentMessage { id, text, phase, delivery } = item else { unreachable!() };
        let snapshot = completed_agent_message_output(id, text, phase.clone(), *delivery, "thread-a", &turn.id, 123);
        assert_eq!(snapshot.value["item_id"], *id);
        assert_eq!(snapshot.value["delivery"], serde_json::to_value(delivery).unwrap());
        assert_eq!(snapshot.value["phase"], serde_json::to_value(phase).unwrap());
        assert_eq!(snapshot.value["lifecycle"], "completed");
    }
    let (id, text) = completed_turn_final_message(&turn).unwrap();
    assert_eq!((id, text), ("final", "same"));
    let output = CodexTurnOutput::final_message("thread-a", &turn.id, id, text, None);
    assert_eq!(output.value["item_id"], "final");
    assert_eq!(output.value["phase"], "final_answer");
    let other = CodexTurnOutput::final_message("thread-b", "turn-b", id, text, None);
    assert_ne!(output.value, other.value);
    turn.items.retain(|item| item.final_answer().is_some());
    turn.items_view = TurnItemsView::Summary;
    assert_eq!(completed_turn_final_message(&turn).unwrap(), ("final", "same"));
    let notification = ServerNotification::TurnCompleted(octopus_appserver_proto::TurnCompletedNotification {
        thread_id: "thread-b".into(), turn: turn.clone(),
    });
    assert!(!server_notification_matches_turn(&notification, "thread-a", "turn-a").unwrap());
    assert!(!server_notification_matches_turn(&notification, "thread-b", "turn-b").unwrap());
}

#[test]
fn completed_message_identity_refuses_unknown_or_ambiguous_terminal_ownership() {
    for items in [
        serde_json::json!([]),
        serde_json::json!([{"type":"agentMessage", "id":"update", "text":"working", "phase":"final_answer", "delivery":"async"}]),
        serde_json::json!([{"type":"agentMessage", "id":"unknown", "text":"answer", "phase":null}]),
        serde_json::json!([{"type":"agentMessage", "id":"commentary", "text":"answer", "phase":"commentary"}]),
        serde_json::json!([
            {"type":"agentMessage", "id":"first", "text":"answer", "phase":"final_answer"},
            {"type":"agentMessage", "id":"second", "text":"answer", "phase":"final_answer"}
        ]),
        serde_json::json!([{"type":"agentMessage", "id":"", "text":"answer", "phase":"final_answer"}]),
    ] {
        let turn = serde_json::from_value(serde_json::json!({"id":"turn-a", "status":"completed", "items":items})).unwrap();
        assert!(matches!(completed_turn_final_message(&turn), Err(InterfaceDriverError::FailedClosed(_))));
    }
    let silent: octopus_appserver_proto::Turn = serde_json::from_value(serde_json::json!({
        "id":"turn-a", "status":"completed", "itemsView":"full",
        "items":[{"type":"agentMessage", "id":"empty", "text":"", "phase":"final_answer"}]
    })).unwrap();
    assert_eq!(completed_turn_final_message(&silent).unwrap(), ("empty", ""));
}
