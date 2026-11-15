use super::*;
use octopus_ledger::ContentHash;

#[test]
fn declared_input_messages_preserve_context_and_reject_malformed_requests() {
    for source in [include_str!("../../../../interface.json"), include_str!("../../../../../world_provider_chatgpt/interface.json")] {
        let descriptor: InterfaceDescriptor = serde_json::from_str(source).unwrap();
        let config = &descriptor.interface_driver_config.unwrap().private_config;
        let payload = serde_json::json!({"input_text":"Show the work", "delegation_id":"request-2",
            "call_id":"call-1", "correlation_id":"offer-1", "transcript":[{"role":"user","text":"Show the work"}],
            "client_context":[{"name":"client_state","text":"Widget: hidden; revision: current."}]});
        let decoded = decode_input_message(config, "realtime.delegation", &serde_json::to_vec(&payload).unwrap(), None).unwrap().unwrap();
        assert_eq!(decoded.control_id.as_deref(), Some("request-2"));
        let texts = decoded.input.iter().map(|item| match item { UserInput::Text { text, .. } => text.as_str(), _ => panic!("text expected") }).collect::<Vec<_>>();
        let context: serde_json::Value = serde_json::from_str(texts[0]).unwrap();
        for field in ["delegation_id", "call_id", "correlation_id", "transcript"] {
            assert_eq!(context["input_message_context"]["fields"][field], payload[field]);
        }
        assert!(texts[1].contains("Widget: hidden; revision: current."));
        assert_eq!(texts[2], "Show the work");
        let mut transcript_request = payload.clone();
        transcript_request["input_text"] = serde_json::Value::Null;
        transcript_request["provider_event_id"] = serde_json::json!("event-1");
        let decoded = decode_provider_input(&serde_json::to_vec(&transcript_request).unwrap(), "realtime.delegation", None,
            &BTreeMap::new(), None, config).unwrap().into_turn_input().unwrap();
        let [UserInput::Text { text: snapshot, .. }, UserInput::Text { text: request, .. }] = decoded.as_slice() else { panic!("snapshot and complete JSON request expected") };
        assert!(snapshot.contains("Widget: hidden; revision: current."));
        let request: serde_json::Value = serde_json::from_str(request).unwrap();
        assert_eq!(request["input_message_context"]["fields"], transcript_request);
        transcript_request["transcript"] = serde_json::json!([]);
        assert!(decode_provider_input(&serde_json::to_vec(&transcript_request).unwrap(), "realtime.delegation", None,
            &BTreeMap::new(), None, config).is_err());
        for (field, value) in [("input_text", serde_json::json!(false)),
            ("delegation_id", serde_json::Value::Null), ("delegation_id", serde_json::json!(" untrimmed")),
            ("client_context", serde_json::json!({})), ("client_context", serde_json::json!([])),
            ("transcript", serde_json::Value::Null), ("transcript", serde_json::json!({})),
            ("transcript", serde_json::json!(false))] {
            let mut malformed = payload.clone(); malformed[field] = value;
            assert!(decode_provider_input(&serde_json::to_vec(&malformed).unwrap(), "realtime.delegation", None,
                &BTreeMap::new(), None, config).is_err(), "{field}");
        }
        assert!(decode_provider_input(b"plain text", "realtime.delegation", None, &BTreeMap::new(), None, config).is_err());
        for payload in [serde_json::json!({"prompt":"Persisted request"}), serde_json::json!({"text":null,"prompt":"Persisted request"}),
            serde_json::json!({"text":"Persisted request","prompt":"Persisted request"})] {
            let input = decode_provider_input(&serde_json::to_vec(&payload).unwrap(), "operator.message", None,
                &BTreeMap::new(), None, config).unwrap().into_turn_input().unwrap();
            assert!(matches!(input.as_slice(), [UserInput::Text { text, .. }] if text == "Persisted request"));
        }
        assert!(decode_provider_input(br#"{"text":"One request","prompt":"Another request"}"#, "operator.message", None,
            &BTreeMap::new(), None, config).is_err());
    }
}

#[test]
fn arbitrary_json_is_artifact_data_but_operator_envelopes_remain_strict() {
    let source = InterfaceId::new("system_context");
    for payload in [serde_json::json!({"authority":"operator","constraints":[]}), serde_json::json!([1, 2])] {
        let input = decode_provider_input(&serde_json::to_vec(&payload).unwrap(), "document", Some(&source), &BTreeMap::new(), None, &serde_json::Map::new()).unwrap();
        let input = input.into_turn_input().unwrap();
        let [UserInput::Text { text, .. }] = input.as_slice() else { panic!("artifact text expected") };
        let rendered: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(rendered["artifact"]["payload"], payload);
        assert_eq!(rendered["artifact"]["source_interface"], "system_context");
    }
    for (label, payload) in [(OPERATOR_MESSAGE_LABEL, b"{}".as_slice()), ("request", b"{\"prompt\":\"\"}".as_slice()), (OPERATOR_TURN_CONTROL_LABEL, b"{}".as_slice())] {
        assert!(decode_provider_input(payload, label, Some(&source), &BTreeMap::new(), None, &serde_json::Map::new()).is_err());
    }
}

#[test]
fn input_template_does_not_claim_operator_messages_but_input_delivery_needs_exact_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let selection = serde_json::json!({"template_interface_id":"system_service_input"});
    let make = |label: &str, from: &str, payload: serde_json::Value| {
        let file =
            InterfaceArtifactFile::write(dir.path(), label, &serde_json::to_vec(&payload).unwrap())
                .unwrap();
        InterfaceInvocationEnvelope {
            artifact: ArtifactRef { id:"fixture".into(), content:ContentHash::of_bytes(b"fixture") },
            artifact_file:file,
            artifact_binding_payloads:Default::default(),
            metadata:Some(serde_json::from_value(serde_json::json!({"runtime":{"run":"run","ee":"ee","worker":"worker","checkpoint":"input","interface":"provider","epoch":0},"checkpoint_binding_policy":{"inbound":{"interface":from,"config":{"artifact_policy":{"mode":"all"}}},"outbound":[]}})).unwrap()),
        }
    };
    for label in ["operator.message", "operator.turn.control"] {
        assert!(
            admitted_peer_message(
                &make(label, "client", serde_json::json!({})),
                Some(&selection)
            )
            .unwrap()
            .is_none()
        );
    }
    let body = serde_json::json!({"schema_version":2,"source_movement_seq":1,"source_artifact":{"id":"source","content":ContentHash::of_bytes(b"source")},"source":{"interface":"world_provider_child","run":"run-child"},"message":{"action":"interrupt","control_id":"exact"}});
    let input = make(
        octopus_operator_chat_client::input_intake::INPUT_INTAKE_LABEL,
        "intake",
        body.clone(),
    );
    assert!(admitted_peer_message(&input, None).is_err());
    assert!(admitted_peer_message(&input, Some(&selection)).is_err());
    let exact =
        serde_json::json!({"template_interface_id":"system_service_input","interface_id":"intake"});
    assert_eq!(
        admitted_peer_message(&input, Some(&exact))
            .unwrap()
            .unwrap()
            .action,
        PeerAction::Interrupt
    );
    let wrong = make(
        octopus_operator_chat_client::input_intake::INPUT_INTAKE_LABEL,
        "worker",
        body,
    );
    assert!(admitted_peer_message(&wrong, Some(&exact)).is_err());
}
