use super::*;

#[test]
fn declared_compatibility_proves_release_and_feature_state() {
    let release: serde_json::Value =
        serde_json::from_str(include_str!("../../managed-cli/release.json")).unwrap();
    assert_eq!(release["tag"], CODEX_SCHEMA_BASELINE);
    let requirements: CodexCompatibilityRequirements = serde_json::from_value(serde_json::json!({
        "minimum_version": release["version"],
        "required_features": {"code_mode_host": true, "instant_interrupt": true},
    }))
    .unwrap();
    for version in [
        None,
        Some("0.165.0"),
        Some("0.166.0-alpha.1"),
        Some("1.166.0"),
        Some("00.166.0"),
    ] {
        assert!(requirements.check_version(version).is_err());
    }
    let version = requirements.check_version(Some("0.166.0")).unwrap();
    let feature = |name: &str, enabled| ExperimentalFeature {
        name: name.into(),
        enabled,
        default_enabled: true,
        stage: ExperimentalFeatureStage::UnderDevelopment,
        display_name: None,
        description: None,
        announcement: None,
    };
    let features = BTreeMap::from([
        ("code_mode_host".into(), feature("code_mode_host", true)),
        (
            "instant_interrupt".into(),
            feature("instant_interrupt", true),
        ),
    ]);
    let proof = requirements
        .evidence(version.clone(), features.clone())
        .unwrap();
    assert_eq!(proof.schema_baseline, "rust-v0.166.0");
    assert_eq!(proof.steering.method, "turn/steer");
    assert!(!proof.steering.consumption_proven_by_acknowledgement);
    assert!(!proof.steering.responses_api_steering);
    for missing in requirements.required_features.keys() {
        let mut invalid = features.clone();
        invalid.remove(missing);
        assert!(requirements.evidence(version.clone(), invalid).is_err());
    }
    for stage in [
        ExperimentalFeatureStage::Stable,
        ExperimentalFeatureStage::Removed,
    ] {
        let mut invalid = features.clone();
        let target = invalid.get_mut("instant_interrupt").unwrap();
        target.enabled = false;
        target.stage = stage;
        assert!(requirements.evidence(version.clone(), invalid).is_err());
    }
}

#[test]
fn codex_166_capabilities_and_partial_answers_decode_without_terminal_claims() {
    use octopus_appserver_proto::{
        JSONRPCNotification, ModelProviderCapabilitiesReadResponse, ServerNotification, ThreadItem,
    };
    use octopus_protocol::models::MessagePhase;
    let capabilities: ModelProviderCapabilitiesReadResponse =
        serde_json::from_value(serde_json::json!({"imageGeneration": false, "webSearch": true}))
            .unwrap();
    assert!(capabilities.web_search);
    for method in ["item/started", "item/completed"] {
        let mut params = serde_json::json!({
            "threadId":"thread-1", "turnId":"turn-1",
            "item":{"type":"agentMessage", "id":"partial-1", "text":"Answer so far", "phase":"partial_answer"},
        });
        params[if method == "item/started" {
            "startedAtMs"
        } else {
            "completedAtMs"
        }] = serde_json::json!(100);
        let notification = ServerNotification::try_from(JSONRPCNotification {
            method: method.into(),
            params: Some(params),
        })
        .unwrap();
        let item = match notification {
            ServerNotification::ItemStarted(started) => started.item,
            ServerNotification::ItemCompleted(completed) => completed.item,
            _ => panic!("expected item notification"),
        };
        assert!(matches!(
            item,
            ThreadItem::AgentMessage {
                phase: Some(MessagePhase::PartialAnswer),
                ..
            }
        ));
    }
    assert!(serde_json::from_value::<MessagePhase>(serde_json::json!("future_phase")).is_err());
}
