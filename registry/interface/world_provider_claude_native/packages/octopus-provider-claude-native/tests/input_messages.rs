use std::path::Path;

use octopus_interfaces::InterfaceDescriptor;
use octopus_provider_claude_native::{ClaudeNativeConfig, decode_input};
use serde_json::{Value, json};

fn declared() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn config(value: &Value) -> Result<ClaudeNativeConfig, String> {
    let descriptor: InterfaceDescriptor = octopus_descriptors::parse_strict(&serde_json::to_vec(value).unwrap()).unwrap();
    ClaudeNativeConfig::from_descriptor(&descriptor)
}

fn delegation() -> Value {
    json!({
        "input_text": "What is on my calendar today?",
        "delegation_id": "request-2",
        "call_id": "call-1",
        "correlation_id": "offer-1",
        "transcript": [{"role": "user", "text": "What is on my calendar today?"}],
        "client_context": [{"name": "client_state", "text": "Widget: hidden."}]
    })
}

#[test]
fn the_declared_voice_delegation_matches_the_codex_contract() {
    let ours = declared();
    let codex: Value = serde_json::from_slice(
        &std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../world_provider_codex/interface.json")).unwrap(),
    )
    .unwrap();
    let pointer = "/interface_driver_config/private_config/input_messages/realtime.delegation";
    assert_eq!(ours.pointer(pointer), codex.pointer(pointer));
    assert!(ours.pointer(pointer).is_some());
}

#[test]
fn a_voice_delegation_reaches_claude_with_its_exact_ids() {
    let config = config(&declared()).unwrap();
    let contract = &config.input_messages["realtime.delegation"];
    let payload = delegation();
    let decoded = decode_input("realtime.delegation", contract, &payload).unwrap();
    assert_eq!(decoded.identity.as_deref(), Some("request-2"));
    assert_eq!(decoded.text, "What is on my calendar today?");
    let block: Value = serde_json::from_str(decoded.context_block.as_deref().unwrap()).unwrap();
    assert_eq!(block["input_message_context"]["label"], "realtime.delegation");
    assert_eq!(block["input_message_context"]["fields"], payload, "the model sees the admitted payload unchanged");
    let prompt = decoded.prompt_text();
    for id in ["call-1", "offer-1", "request-2", "Widget: hidden."] {
        assert!(prompt.contains(id), "prompt lacks {id}: {prompt}");
    }
    assert!(prompt.ends_with("What is on my calendar today?"));
}

#[test]
fn a_delegation_without_spoken_text_is_answered_from_its_transcript() {
    let config = config(&declared()).unwrap();
    let contract = &config.input_messages["realtime.delegation"];
    let mut payload = delegation();
    payload["input_text"] = Value::Null;
    let decoded = decode_input("realtime.delegation", contract, &payload).unwrap();
    assert_eq!(Some(decoded.text.as_str()), decoded.context_block.as_deref());
    assert_eq!(decoded.prompt_text(), decoded.text, "the context block is not repeated");
    payload["transcript"] = json!([]);
    let error = decode_input("realtime.delegation", contract, &payload).unwrap_err();
    assert!(error.contains("no text"), "{error}");
}

#[test]
fn a_delegation_missing_its_ids_or_shape_fails_closed() {
    let config = config(&declared()).unwrap();
    let contract = &config.input_messages["realtime.delegation"];
    for (field, value, expected) in [
        ("delegation_id", Value::Null, "no declared identity"),
        ("delegation_id", json!(" request-2"), "untrimmed"),
        ("delegation_id", json!(7), "identity must be text"),
        ("call_id", Value::Null, "no context field 'call_id'"),
        ("correlation_id", Value::Null, "no context field 'correlation_id'"),
        ("transcript", json!({"role": "user"}), "wrong JSON type"),
        ("input_text", json!(3), "must be text"),
    ] {
        let mut payload = delegation();
        payload[field] = value;
        let error = decode_input("realtime.delegation", contract, &payload).unwrap_err();
        assert!(error.contains(expected), "{field}: {error}");
    }
    let mut payload = delegation();
    payload.as_object_mut().unwrap().remove("transcript");
    assert!(decode_input("realtime.delegation", contract, &payload).unwrap_err().contains("no request context field"));
}

#[test]
fn an_operator_message_is_still_plain_text() {
    let config = config(&declared()).unwrap();
    let contract = &config.input_messages["operator.message"];
    let decoded = decode_input("operator.message", contract, &json!({"text": "hello", "message_id": "m-1"})).unwrap();
    assert_eq!(decoded.context_block, None);
    assert_eq!(decoded.prompt_text(), "hello");
    assert_eq!(decoded.identity.as_deref(), Some("m-1"));
    let decoded = decode_input("operator.message", contract, &json!({"prompt": "hi"})).unwrap();
    assert_eq!(decoded.identity, None);
    let error = decode_input("operator.message", contract, &json!({"text": "a", "prompt": "b"})).unwrap_err();
    assert!(error.contains("conflicting"), "{error}");
}

#[test]
fn input_contracts_with_invalid_fields_fail_closed() {
    let pointer = "/interface_driver_config/private_config/input_messages/realtime.delegation";
    for (field, value, expected) in [
        ("identity_field", Value::Null, "declares no identity_field"),
        ("context_fields", json!(["call_id", "call_id"]), "invalid fields"),
        ("context_fields", json!(["input_text"]), "invalid fields"),
        ("request_context_fields", json!({"transcript": "string"}), "unknown variant"),
    ] {
        let mut drifted = declared();
        let contract = drifted.pointer_mut(pointer).unwrap();
        if value.is_null() {
            contract.as_object_mut().unwrap().remove(field);
        } else {
            contract[field] = value;
        }
        let error = config(&drifted).unwrap_err();
        assert!(error.contains(expected), "{field}: {error}");
    }
}

#[test]
fn an_undeclared_artifact_reaches_claude_by_the_codex_rule() {
    use octopus_provider_claude_native::artifact_turn_text;
    assert_eq!(artifact_turn_text("task.request", Some("world_x"), &json!({"task": "Summarize the run", "extra": 1})).unwrap(), "Summarize the run");
    assert_eq!(artifact_turn_text("x", None, &json!({"text": "  ", "prompt": "use the prompt"})).unwrap(), "use the prompt");
    let experience = json!({"source": {"run": "run_home"}, "experience": [{"role": "user", "text": "I prefer Zig"}]});
    let rendered: Value = serde_json::from_str(&artifact_turn_text("memory.experience.input", Some("system_context_projection_m"), &experience).unwrap()).unwrap();
    assert_eq!(rendered["artifact"]["label"], "memory.experience.input");
    assert_eq!(rendered["artifact"]["source_interface"], "system_context_projection_m");
    assert_eq!(rendered["artifact"]["payload"], experience, "the payload reaches the model whole");
}
