    use super::*;
    include!("context_refresh.rs");
    include!("cut_turn_continuation.rs");
    include!("steering_compatibility.rs");
    include!("pending_history.rs");
    include!("recovery_call_ownership.rs");
    include!("completed_messages.rs");
    include!("result_continuity.rs");
    include!("native_planning_read.rs");
    include!("native_catalog.rs");

    fn metadata_input(
        interface_affordance_specs: serde_json::Value,
    ) -> InterfaceInvocationEnvelope {
        let request_bytes = br#"{}"#;
        let artifact_file = octopus_interfaces::InterfaceArtifactFile::write(
            std::env::temp_dir().join("octopus-codex-handler-test"),
            "test.request",
            request_bytes,
        )
        .expect("test input artifact file writes");
        let specs = interface_affordance_specs
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|value| serde_json::from_value(value).expect("test interface affordance is typed"))
            .collect();
        InterfaceInvocationEnvelope {
            artifact: octopus_interfaces::ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("request-test"),
                content: octopus_ledger::ContentHash::of_bytes(request_bytes),
            },
            artifact_file,
            artifact_binding_payloads: Vec::new(),
            metadata: Some(octopus_interfaces::InvocationMetadata::new(
                octopus_interfaces::GrantProjection {
                    interface_affordance_specs: specs,
                    ..octopus_interfaces::GrantProjection::default()
                },
            )),
        }
    }

    fn model_policy_input(models: serde_json::Value) -> InterfaceInvocationEnvelope {
        let mut input = metadata_input(serde_json::json!([]));
        let mut grant = octopus_interfaces::GrantProjection::default();
        grant.interface_policy.insert(
            "world_provider_codex".to_string(),
            BTreeMap::from([("models".to_string(), models)]),
        );
        input.metadata = Some(
            octopus_interfaces::InvocationMetadata::new(grant).with_runtime(
                RuntimeInvocationMetadata {
                    run: octopus_interfaces::RunId::new("run-model-policy"),
                    ee: octopus_descriptors::EeId::new("ee-model-policy"),
                    worker: InterfaceId::new("system_worker_test"),
                    checkpoint: octopus_descriptors::CheckpointId::new("checkpoint-model-policy"),
                    interface: InterfaceId::new("world_provider_codex"),
                    epoch: 1,
                    run_record_hash: None,
                },
            ),
        );
        input
    }

    #[test]
    fn model_catalog_policy_is_strict_and_exact() {
        let formed = required_formed_models(&model_policy_input(serde_json::json!([
            "gpt-5.4", "gpt-5.5"
        ])))
        .expect("formed models");
        assert!(catalog_model_allowed("gpt-5.4", &formed));
        assert!(!catalog_model_allowed("gpt-5.3", &formed));
        assert!(matches!(
            required_formed_models(&model_policy_input(serde_json::json!([]))),
            Err(InterfaceDriverError::FailedClosed(error)) if error.contains("models is empty")
        ));
        assert!(matches!(
            required_formed_models(&model_policy_input(serde_json::json!(["gpt-5.4", "gpt-5.4"]))),
            Err(InterfaceDriverError::FailedClosed(error)) if error.contains("repeats")
        ));
    }

    #[test]
    fn model_reroute_cannot_substitute_the_descriptor_selected_model() {
        validate_model_reroute("gpt-selected", "gpt-selected", "gpt-selected")
            .expect("a no-op notification retains the exact selected model");
        assert!(matches!(
            validate_model_reroute("gpt-selected", "gpt-selected", "gpt-fallback"),
            Err(InterfaceDriverError::FailedClosed(error))
                if error.contains("hidden provider fallback is forbidden")
        ));
        assert!(matches!(
            validate_model_reroute("gpt-selected", "gpt-unknown", "gpt-selected"),
            Err(InterfaceDriverError::FailedClosed(error))
                if error.contains("started from 'gpt-unknown'")
        ));
    }

    #[test]
    fn model_catalog_retains_current_provider_metadata() {
        let catalog: ModelListResponse = serde_json::from_value(serde_json::json!({
            "data": [{
                "id": "gpt-current",
                "model": "gpt-current",
                "upgrade": null,
                "upgradeInfo": {
                    "model": "gpt-next",
                    "upgradeCopy": null,
                    "modelLink": null,
                    "migrationMarkdown": null,
                    "retirementAt": 1800000000
                },
                "availabilityNux": null,
                "displayName": "Current",
                "description": "Current provider model",
                "modelSpecialty": "cyber",
                "hidden": false,
                "supportedReasoningEfforts": [],
                "defaultReasoningEffort": "ultra",
                "multiAgentVersion": "v2",
                "isDefault": true
            }],
            "nextCursor": null
        }))
        .expect("current model catalog decodes");
        let wire = serde_json::to_value(catalog).expect("current model catalog re-encodes");
        assert_eq!(wire["data"][0]["modelSpecialty"], "cyber");
        assert_eq!(wire["data"][0]["multiAgentVersion"], "v2");
        assert_eq!(wire["data"][0]["upgradeInfo"]["retirementAt"], 1800000000);
    }

    #[test]
    fn safety_buffering_projects_an_actionable_warning() {
        let output = model_safety_buffering_output(&ModelSafetyBufferingUpdatedNotification {
            thread_id: "thread-1".into(),
            turn_id: "turn-1".into(),
            model: "gpt-current".into(),
            show_buffering_ui: true,
            reasons: vec!["additional_review".into()],
            use_cases: vec!["cyber".into()],
            faster_model: Some("gpt-fast".into()),
        })
        .expect("safety buffering warning encodes");
        assert_eq!(output.label, TURN_WARNING_LABEL);
        assert_eq!(output.value["source"], "model_safety_buffering");
        assert_eq!(output.value["showBufferingUi"], true);
        assert_eq!(output.value["fasterModel"], "gpt-fast");
        assert!(
            output.value["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
        );
    }

    #[test]
    fn interface_driver_ref_is_the_codex_app_server_family() {
        assert_eq!(CODEX_APP_SERVER_INTERFACE_DRIVER_REF, "codex_app_server");
    }

    #[test]
    fn every_source_codex_app_server_descriptor_satisfies_the_projection_contract() {
        let interface_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let native_owners = BTreeSet::from([
            "world_provider_chatgpt".to_string(),
            "world_provider_codex".to_string(),
        ]);
        let mut owners = BTreeSet::new();
        let mut opencodex_owners = BTreeSet::new();
        for entry in std::fs::read_dir(&interface_root).expect("source interface registry reads") {
            let path = entry
                .expect("source interface owner reads")
                .path()
                .join("interface.json");
            if !path.is_file() {
                continue;
            }
            let descriptor: InterfaceDescriptor = serde_json::from_slice(
                &std::fs::read(&path).expect("source interface descriptor reads"),
            )
            .expect("source interface descriptor parses");
            let owns_opencodex = descriptor
                .interface_driver_config
                .as_ref()
                .is_some_and(|config| config.private_config.contains_key("opencodex"));
            if owns_opencodex {
                assert_eq!(
                    descriptor.interface_driver_ref,
                    CODEX_APP_SERVER_INTERFACE_DRIVER_REF,
                    "OpenCodex owner '{}' must use the declared Codex harness",
                    descriptor.id.as_str()
                );
                opencodex_owners.insert(descriptor.id.as_str().to_string());
            }
            if descriptor.interface_driver_ref != CODEX_APP_SERVER_INTERFACE_DRIVER_REF {
                continue;
            }
            validate_codex_projection_descriptor_contract(&descriptor)
                .expect("source codex_app_server projection contract validates");
            if descriptor.id.as_str() == "world_provider_codex" {
                let config = descriptor.interface_driver_config.as_ref().unwrap();
                assert!(
                    LiveStreamRules::from_private_config(&config.private_config)
                        .unwrap()
                        .enabled()
                );
            }
            owners.insert(descriptor.id.as_str().to_string());
        }
        assert!(
            native_owners.is_subset(&owners),
            "the native Codex harness owners must remain explicit and self-contained"
        );
        assert!(
            !opencodex_owners.is_empty() && opencodex_owners.is_subset(&owners),
            "every concrete OpenCodex owner must remain explicit and self-contained"
        );
    }

    fn provider_operations_value() -> serde_json::Value {
        serde_json::json!({
            "error_output_label": PROVIDER_OPERATION_ERROR_LABEL,
            "account_login_completed_output_label": PROVIDER_ACCOUNT_LOGIN_COMPLETED_LABEL,
            "account_updated_output_label": PROVIDER_ACCOUNT_UPDATED_LABEL,
            "account_rate_limits_updated_output_label": PROVIDER_ACCOUNT_RATE_LIMITS_UPDATED_LABEL,
            "account_login_start": {
                "request_label": PROVIDER_ACCOUNT_LOGIN_START_LABEL,
                "output_label": PROVIDER_ACCOUNT_LOGIN_STARTED_LABEL,
            },
            "account_login_cancel": {
                "request_label": PROVIDER_ACCOUNT_LOGIN_CANCEL_LABEL,
                "output_label": PROVIDER_ACCOUNT_LOGIN_CANCELLED_LABEL,
            },
            "account_login_status_read": {
                "request_label": PROVIDER_ACCOUNT_LOGIN_STATUS_READ_LABEL,
                "output_label": PROVIDER_ACCOUNT_LOGIN_STATUS_LABEL,
            },
            "account_logout": {
                "request_label": PROVIDER_ACCOUNT_LOGOUT_LABEL,
                "output_label": PROVIDER_ACCOUNT_LOGGED_OUT_LABEL,
            },
            "account_read": {
                "request_label": PROVIDER_ACCOUNT_READ_LABEL,
                "output_label": PROVIDER_ACCOUNT_SNAPSHOT_LABEL,
            },
            "account_rate_limits_read": {
                "request_label": PROVIDER_ACCOUNT_RATE_LIMITS_READ_LABEL,
                "output_label": PROVIDER_ACCOUNT_RATE_LIMITS_LABEL,
            },
            "account_rate_limit_reset_consume": {
                "request_label": PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUME_LABEL,
                "output_label": PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUMED_LABEL,
            },
            "account_usage_read": {
                "request_label": PROVIDER_ACCOUNT_USAGE_READ_LABEL,
                "output_label": PROVIDER_ACCOUNT_USAGE_LABEL,
            },
            "account_workspace_messages_read": {
                "request_label": PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_READ_LABEL,
                "output_label": PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_LABEL,
            },
            "capabilities_read": {
                "request_label": PROVIDER_CAPABILITIES_READ_LABEL,
                "output_label": PROVIDER_CAPABILITIES_LABEL,
            },
            "thread_compact_start": {
                "request_label": PROVIDER_THREAD_COMPACT_START_LABEL,
                "output_label": TURN_CONTEXT_COMPACTION_LABEL,
            },
        })
    }

    #[test]
    fn provider_operation_vocabulary_is_strict_and_secret_free() {
        let operations: ProviderOperations =
            serde_json::from_value(provider_operations_value()).unwrap();
        validate_provider_operations(&operations).unwrap();
        assert!(operations.thread_compact_start.is_some());
        assert!(!is_undeclared_provider_operation(
            PROVIDER_THREAD_COMPACT_START_LABEL,
            true,
        ));
        assert!(is_undeclared_provider_operation(
            PROVIDER_THREAD_COMPACT_START_LABEL,
            false,
        ));
        let mut drifted = provider_operations_value();
        drifted["account_logout"]["output_label"] = serde_json::json!("provider.account.done");
        let drifted: ProviderOperations = serde_json::from_value(drifted).unwrap();
        assert!(validate_provider_operations(&drifted).is_err());
        let mut missing_reset = provider_operations_value();
        missing_reset
            .as_object_mut()
            .unwrap()
            .remove("account_rate_limit_reset_consume");
        assert!(serde_json::from_value::<ProviderOperations>(missing_reset).is_err());
        assert!(
            serde_json::from_value::<AccountLoginStartRequest>(serde_json::json!({
                "method": "api_key",
                "api_key": "must-not-be-an-artifact",
            }))
            .is_err()
        );
        let output = scoped_secret_operation_error(ScopedSecretError::InvalidHandle(
            "must-not-be-an-artifact".to_string(),
        ));
        let encoded = serde_json::to_string(&output.value).unwrap();
        assert!(!encoded.contains("must-not-be-an-artifact"));
        assert!(encoded.contains("secret_handle_invalid"));
        let reset: AccountRateLimitResetConsumeRequest = serde_json::from_value(
            serde_json::json!({"idempotency_key":"reset-1","credit_id":"credit-1"}),
        )
        .unwrap();
        assert_eq!(reset.idempotency_key, "reset-1");
        assert_eq!(reset.credit_id.as_deref(), Some("credit-1"));
        let usage: AccountUsageReadRequest =
            serde_json::from_value(serde_json::json!({"thread_id":"thread-1"})).unwrap();
        assert_eq!(usage.thread_id.as_deref(), Some("thread-1"));
    }

    #[test]
    fn extract_prompt_fails_closed_without_a_real_prompt() {
        assert_eq!(
            extract_prompt_from_value(&serde_json::json!({"prompt":"hello"})).unwrap(),
            "hello"
        );
        assert!(matches!(
            extract_prompt_from_value(&serde_json::json!({"text":"hi"})),
            Err(InterfaceDriverError::Protocol(_))
        ));
        assert!(matches!(
            extract_prompt_from_value(&serde_json::json!({})),
            Err(InterfaceDriverError::Protocol(_))
        ));
        assert!(matches!(
            extract_prompt_from_value(&serde_json::json!({"prompt":""})),
            Err(InterfaceDriverError::Protocol(_))
        ));
    }

    #[test]
    fn bound_instructions_must_be_bound_artifact_context() {
        let role = "You are the flash worker. Reply only from the bound artifacts.";
        assert_eq!(resolve_bound_instructions(Some(role)).unwrap(), role);

        assert!(matches!(
            resolve_bound_instructions(None),
            Err(InterfaceDriverError::FailedClosed(_))
        ));
        assert!(matches!(
            resolve_bound_instructions(Some("   ")),
            Err(InterfaceDriverError::FailedClosed(_))
        ));
        assert!(matches!(
            resolve_bound_instructions(Some("")),
            Err(InterfaceDriverError::FailedClosed(_))
        ));
    }

    #[test]
    fn terminal_usage_uses_current_context_not_cumulative_thread_usage() {
        let usage: ThreadTokenUsage = serde_json::from_value(serde_json::json!({
            "total": {
                "totalTokens": 3_218_100,
                "inputTokens": 3_000_000,
                "cachedInputTokens": 2_750_000,
                "outputTokens": 218_100,
                "reasoningOutputTokens": 0
            },
            "last": {
                "totalTokens": 42_000,
                "inputTokens": 40_000,
                "cachedInputTokens": 32_000,
                "outputTokens": 2_000,
                "reasoningOutputTokens": 0
            },
            "modelContextWindow": 258_400
        }))
        .unwrap();

        assert_eq!(
            terminal_usage_evidence(&usage).unwrap().0,
            ContextWindowUsage {
                used_tokens: 42_000,
                context_window: 258_400,
            }
        );
        let (_, provider_usage) = terminal_usage_evidence(&usage).unwrap();
        assert_eq!(provider_usage.cumulative.cached_input_tokens, 2_750_000);
        assert_eq!(provider_usage.cumulative.uncached_input_tokens, 250_000);
        assert_eq!(provider_usage.current_context.cached_input_tokens, 32_000);
        assert_eq!(provider_usage.current_context.uncached_input_tokens, 8_000);
        let output = CodexTurnOutput::final_message(
            "thread-a", "turn-a", "final-a", "done",
            Some(ContextWindowUsage {
                used_tokens: 42_000,
                context_window: 258_400,
            }),
        )
        .with_provider_token_usage(provider_usage);
        assert_eq!(
            output
                .value
                .pointer("/provider_token_usage/cumulative/uncached_input_tokens")
                .and_then(serde_json::Value::as_u64),
            Some(250_000)
        );
    }

    #[test]
    fn provider_token_usage_rejects_impossible_cache_accounting() {
        let usage: ThreadTokenUsage = serde_json::from_value(serde_json::json!({
            "total": {
                "totalTokens": 10,
                "inputTokens": 8,
                "cachedInputTokens": 9,
                "outputTokens": 2,
                "reasoningOutputTokens": 0
            },
            "last": {
                "totalTokens": 10,
                "inputTokens": 8,
                "cachedInputTokens": 0,
                "outputTokens": 2,
                "reasoningOutputTokens": 0
            },
            "modelContextWindow": 258_400
        }))
        .unwrap();
        assert!(matches!(
            terminal_usage_evidence(&usage),
            Err(InterfaceDriverError::Protocol(error))
                if error.contains("more cached input than total input")
        ));
    }

    #[test]
    fn terminal_usage_refuses_missing_model_window_telemetry() {
        let usage: ThreadTokenUsage = serde_json::from_value(serde_json::json!({
            "total": {
                "totalTokens": 42_000,
                "inputTokens": 40_000,
                "cachedInputTokens": 0,
                "outputTokens": 2_000,
                "reasoningOutputTokens": 0
            },
            "last": {
                "totalTokens": 42_000,
                "inputTokens": 40_000,
                "cachedInputTokens": 0,
                "outputTokens": 2_000,
                "reasoningOutputTokens": 0
            }
        }))
        .unwrap();

        assert!(matches!(
            terminal_usage_evidence(&usage),
            Err(InterfaceDriverError::FailedClosed(_))
        ));
    }

    fn bound(id: &str, payload: serde_json::Value) -> octopus_interfaces::ArtifactBindingPayload {
        let bytes = serde_json::to_vec(&payload).unwrap();
        octopus_interfaces::ArtifactBindingPayload::new(
            octopus_interfaces::ArtifactRef {
                id: octopus_descriptors::ArtifactId::new(id),
                content: octopus_ledger::ContentHash::of_bytes(&bytes),
            },
            bytes,
        )
    }

    fn session_artifact(
        id: &str,
        label: &str,
        payload: serde_json::Value,
        source_movement_seq: u64,
    ) -> serde_json::Value {
        let bytes = serde_json::to_vec(&payload).unwrap();
        serde_json::json!({
            "label": label,
            "payload": payload,
            "source_artifact": octopus_interfaces::ArtifactRef {
                id: octopus_descriptors::ArtifactId::new(id),
                content: octopus_ledger::ContentHash::of_bytes(&bytes),
            },
            "source_movement_seq": source_movement_seq,
        })
    }

    fn continuity(
        provider: &str,
        run: &str,
        thread_id: &str,
        context_generation: u64,
        pending: Option<CodexPendingContinuity>,
    ) -> CodexContinuityState {
        CodexContinuityState {
            schema_version: 1,
            provider_interface: InterfaceId::new(provider),
            run: run.to_string(),
            thread_id: thread_id.to_string(),
            thread_shape_fingerprint: "a".repeat(64),
            context_generation,
            static_context_fingerprint: "b".repeat(64),
            turn_input: None,
            pending,
            interrupted_pending: None,
            tool_operator_request: None,
            native_terminal: None,
            context_refresh: None,
            native_history_retirement: None,
        }
    }

    fn codex_context_labels() -> BTreeSet<String> {
        [
            TURN_AGENT_MESSAGE_LABEL,
            TURN_REASONING_LABEL,
            TURN_PLAN_UPDATE_LABEL,
            INTERFACE_ARTIFACT_OUTPUT_LABEL,
            INTERFACE_CONTEXT_UPDATED_LABEL,
            TOOL_CALL_LABEL,
            TURN_ERROR_LABEL,
            OPERATOR_APPROVAL_REQUEST_LABEL,
            OPERATOR_ELICITATION_REQUEST_LABEL,
            TURN_CONTROL_ACKNOWLEDGED_LABEL,
            TURN_INTERRUPTED_RESULT_LABEL,
            TURN_CONTEXT_COMPACTION_LABEL,
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn codex_snapshot_labels() -> BTreeSet<String> {
        [
            INTERFACE_ARTIFACT_OUTPUT_LABEL,
            TURN_INTERRUPTED_RESULT_LABEL,
            TURN_ERROR_LABEL,
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn codex_client_projection_labels() -> BTreeSet<String> {
        let mut labels = [
            "turn.stream",
            INTERFACE_ARTIFACT_OUTPUT_LABEL,
            INTERFACE_CONTEXT_UPDATED_LABEL,
            TOOL_CALL_LABEL,
            TURN_ERROR_LABEL,
            OPERATOR_APPROVAL_REQUEST_LABEL,
            OPERATOR_ELICITATION_REQUEST_LABEL,
            OPERATOR_USER_INPUT_REQUEST_LABEL,
            PROVIDER_MODELS_CATALOG_LABEL,
            PROVIDER_OPERATION_ERROR_LABEL,
            PROVIDER_ACCOUNT_LOGIN_COMPLETED_LABEL,
            PROVIDER_ACCOUNT_UPDATED_LABEL,
            PROVIDER_ACCOUNT_RATE_LIMITS_UPDATED_LABEL,
            PROVIDER_ACCOUNT_LOGIN_STARTED_LABEL,
            PROVIDER_ACCOUNT_LOGIN_CANCELLED_LABEL,
            PROVIDER_ACCOUNT_LOGIN_STATUS_LABEL,
            PROVIDER_ACCOUNT_LOGGED_OUT_LABEL,
            PROVIDER_ACCOUNT_SNAPSHOT_LABEL,
            PROVIDER_ACCOUNT_RATE_LIMITS_LABEL,
            PROVIDER_ACCOUNT_RATE_LIMIT_RESET_CONSUMED_LABEL,
            PROVIDER_ACCOUNT_USAGE_LABEL,
            PROVIDER_ACCOUNT_WORKSPACE_MESSAGES_LABEL,
            PROVIDER_CAPABILITIES_LABEL,
            TURN_CONTROL_ACKNOWLEDGED_LABEL,
            TURN_CONTROL_FAILED_LABEL,
            TURN_INTERRUPTED_RESULT_LABEL,
            LIFECYCLE_EVENT_LABEL,
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
        labels.extend(
            CLIENT_ACTIVITY_OUTPUT_LABELS
                .iter()
                .map(|label| (*label).to_string()),
        );
        labels
    }

    fn live_stream_config_value() -> serde_json::Value {
        serde_json::json!({
            "artifact_label": "turn.stream",
            "always_stream_labels": [
                AGENT_MESSAGE_DELTA_LABEL,
                TURN_REASONING_DELTA_LABEL,
                TURN_PLAN_DELTA_LABEL,
                TURN_EXEC_OUTPUT_DELTA_LABEL,
                TURN_FILE_CHANGE_DELTA_LABEL,
                TURN_DYNAMIC_TOOL_CALL_LABEL,
                TURN_STARTED_LABEL
            ],
            "active_turn_stream_labels": [
                PROVIDER_ACCOUNT_RATE_LIMITS_UPDATED_LABEL,
                ACCOUNT_RATE_LIMITS_LABEL
            ],
            "phase_rules": [{
                "label": TURN_REASONING_LABEL,
                "field": "lifecycle",
                "stream_values": ["started"],
                "artifact_values": ["completed"]
            }]
        })
    }

    fn live_stream_rules() -> LiveStreamRules {
        let config = serde_json::json!({"live_stream": live_stream_config_value()});
        LiveStreamRules::from_private_config(config.as_object().unwrap()).unwrap()
    }

    #[test]
    fn descriptor_requires_durable_interruption_outputs_in_context_and_snapshots() {
        let context = codex_context_labels();
        let snapshots = codex_snapshot_labels();
        validate_durable_interruption_output_labels(&context, &snapshots).unwrap();

        let mut stale_context = context.clone();
        stale_context.remove(TURN_CONTROL_ACKNOWLEDGED_LABEL);
        assert!(matches!(
            validate_durable_interruption_output_labels(&stale_context, &snapshots),
            Err(InterfaceDriverError::FailedClosed(message))
                if message.contains(TURN_CONTROL_ACKNOWLEDGED_LABEL)
        ));

        let mut stale_snapshots = snapshots;
        stale_snapshots.remove(TURN_INTERRUPTED_RESULT_LABEL);
        assert!(matches!(
            validate_durable_interruption_output_labels(&context, &stale_snapshots),
            Err(InterfaceDriverError::FailedClosed(message))
                if message.contains(TURN_INTERRUPTED_RESULT_LABEL)
        ));
    }

    #[test]
    fn interrupt_moves_exact_effect_into_durable_interrupted_continuity() {
        let pending = CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-1".to_string(),
            call_id: "call-1".to_string(),
            tool: "coder.exec".to_string(),
        };
        let state = continuity(
            "world_provider_codex",
            "run-a",
            "thread-a",
            7,
            Some(pending.clone()),
        );
        let (interrupted, retained) = interrupted_continuity(Some(state)).unwrap();
        let interrupted = interrupted.expect("continuity remains durable");
        assert!(retained);
        assert_eq!(interrupted.context_generation, 8);
        assert!(interrupted.pending.is_none());
        assert_eq!(interrupted.interrupted_pending, Some(pending));

        let parsed = parse_codex_continuity(
            &serde_json::to_value(&interrupted).unwrap(),
            &InterfaceId::new("world_provider_codex"),
            "run-a",
        )
        .unwrap();
        assert_eq!(parsed, interrupted);

        let mut repeated = interrupted.clone(); repeated.context_generation += 1;
        let (selected, overridden) = reconcile_durable_interrupted_continuity(Some(repeated.clone()), Some(interrupted.clone())).unwrap();
        assert_eq!(selected, Some(repeated.clone())); assert!(!overridden);
        for field in ["thread", "generation", "pending"] {
            let mut wrong = repeated.clone();
            match field { "thread" => wrong.thread_id = "foreign".into(), "generation" => wrong.context_generation = 0,
                _ => wrong.interrupted_pending = None }
            assert!(reconcile_durable_interrupted_continuity(Some(wrong), Some(interrupted.clone())).is_err());
        }

        let operator = continuity(
            "world_provider_codex",
            "run-a",
            "thread-a",
            8,
            Some(CodexPendingContinuity::Operator {
                turn_id: "turn-2".to_string(),
                request_key: "operator:request-1".to_string(),
                method: "item/commandExecution/requestApproval".to_string(),
            }),
        );
        let (operator, retained) = interrupted_continuity(Some(operator)).unwrap();
        assert!(!retained);
        let operator = operator.unwrap();
        assert!(operator.pending.is_none());
        assert!(operator.interrupted_pending.is_none());
    }

    #[tokio::test]
    async fn interrupt_retains_effect_correlation_but_drops_operator_waits() {
        let requests = Arc::new(Mutex::new(BTreeMap::new()));
        let scope = TransportStateScope::Run("run-a".to_string());
        let effect_gate = Arc::new(tokio::sync::Mutex::new(()));
        let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        requests.lock().await.insert(
            (scope.clone(), "tool:call-1".to_string()),
            PendingServerRequest {
                request_id: RequestId::Integer(1),
                thread_id: "thread-a".to_string(),
                turn_id: "turn-a".to_string(),
                key: "tool:call-1".to_string(),
                kind: PendingServerRequestKind::DynamicTool {
                    call_id: "call-1".to_string(),
                    tool: "coder.exec".to_string(),
                },
                effect_gate: effect_gate.clone(),
                turn_interrupted: interrupted.clone(),
            },
        );
        requests.lock().await.insert(
            (scope.clone(), "operator:request-1".to_string()),
            PendingServerRequest {
                request_id: RequestId::Integer(2),
                thread_id: "thread-a".to_string(),
                turn_id: "turn-a".to_string(),
                key: "operator:request-1".to_string(),
                kind: PendingServerRequestKind::Operator {
                    method: "item/commandExecution/requestApproval".to_string(),
                },
                effect_gate: effect_gate.clone(),
                turn_interrupted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        );

        let ack_guard = effect_gate.clone().lock_owned().await;
        interrupted.store(true, std::sync::atomic::Ordering::Release);
        assert!(
            retain_interrupted_effect_request(&requests, &scope, Some("thread-a"), Some("turn-a"),)
                .await
        );
        drop(ack_guard);
        assert!(interrupted.load(std::sync::atomic::Ordering::Acquire));
        let requests = requests.lock().await;
        assert!(requests.contains_key(&(scope.clone(), "tool:call-1".to_string())));
        assert!(!requests.contains_key(&(scope, "operator:request-1".to_string())));
    }

    #[tokio::test]
    async fn exact_result_winning_gate_clears_stale_interrupt_continuity() {
        let requests = Arc::new(Mutex::new(BTreeMap::new()));
        let scope = TransportStateScope::Run("run-a".to_string());
        let effect_gate = Arc::new(tokio::sync::Mutex::new(()));
        requests.lock().await.insert(
            (scope.clone(), "tool:call-1".to_string()),
            PendingServerRequest {
                request_id: RequestId::Integer(1),
                thread_id: "thread-a".to_string(),
                turn_id: "turn-a".to_string(),
                key: "tool:call-1".to_string(),
                kind: PendingServerRequestKind::DynamicTool {
                    call_id: "call-1".to_string(),
                    tool: "coder.exec".to_string(),
                },
                effect_gate: effect_gate.clone(),
                turn_interrupted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        );

        let result_guard = effect_gate.clone().lock_owned().await;
        requests
            .lock()
            .await
            .remove(&(scope.clone(), "tool:call-1".to_string()))
            .expect("the exact result claims its pending request first");
        drop(result_guard);

        let ack_guard = effect_gate.lock_owned().await;
        let retained =
            retain_interrupted_effect_request(&requests, &scope, Some("thread-a"), Some("turn-a"))
                .await;
        assert!(!retained);
        let pending = CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-a".to_string(),
            call_id: "call-1".to_string(),
            tool: "coder.exec".to_string(),
        };
        let mut state = continuity("world_provider_codex", "run-a", "thread-a", 8, None);
        state.interrupted_pending = Some(pending);
        let mut result = CodexTurnControlResult {
            control_id: "control-1".to_string(),
            action: "stop".to_string(),
            acknowledged: true,
            thread_id: Some("thread-a".to_string()),
            turn_id: Some("turn-a".to_string()),
            detail: serde_json::json!({
                "interrupted": true,
                "exact_result_pending": true,
            }),
            continuity: Some(state),
        };
        reconcile_interrupted_control_result(&mut result, retained);
        drop(ack_guard);
        assert_eq!(result.detail["exact_result_pending"], false);
        assert!(
            result
                .continuity
                .as_ref()
                .is_some_and(|state| state.interrupted_pending.is_none())
        );
    }

    #[tokio::test]
    async fn handoff_barrier_does_not_lose_early_completion() {
        let barrier = ProviderHandoffBarrier::new();
        barrier.finish(Ok(()));
        tokio::time::timeout(std::time::Duration::from_millis(50), barrier.wait())
            .await
            .expect("a completed handoff barrier returns immediately")
            .expect("completed handoff remains successful");

        let failed = ProviderHandoffBarrier::new();
        failed.finish(Err("authoring failed".to_string()));
        let error = tokio::time::timeout(std::time::Duration::from_millis(50), failed.wait())
            .await
            .expect("a failed handoff barrier returns immediately")
            .expect_err("a failed prior handoff cannot open the next authoring lane");
        assert_eq!(error, "authoring failed");
    }

    #[test]
    fn control_ack_materializes_durable_interrupted_correlation() {
        let temp = tempfile::tempdir().unwrap();
        let pending = CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-1".to_string(),
            call_id: "call-1".to_string(),
            tool: "coder.exec".to_string(),
        };
        let mut state = continuity("world_provider_codex", "run-a", "thread-a", 8, None);
        state.interrupted_pending = Some(pending);
        let artifact = materialize_turn_control_result(
            temp.path(),
            42,
            CodexTurnControlResult {
                control_id: "control-1".to_string(),
                action: "stop".to_string(),
                acknowledged: true,
                thread_id: Some("thread-a".to_string()),
                turn_id: Some("turn-1".to_string()),
                detail: serde_json::json!({
                    "interrupted": true,
                    "exact_result_pending": true,
                }),
                continuity: Some(state),
            },
        )
        .unwrap();
        let payload: serde_json::Value =
            serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(payload["label"], TURN_CONTROL_ACKNOWLEDGED_LABEL);
        assert_eq!(payload["detail"]["exact_result_pending"], true);
        assert_eq!(
            payload[CODEX_CONTINUITY_FIELD]["interrupted_pending"]["call_id"],
            "call-1"
        );
        assert!(payload[CODEX_CONTINUITY_FIELD].get("pending").is_none());

        let mut queued = CodexTurnControlResult {
            control_id: "queue-1".into(),
            action: "queue_message".into(),
            acknowledged: true,
            thread_id: Some("thread-a".into()),
            turn_id: Some("turn-1".into()),
            detail: serde_json::json!({"queued": true, "turn_triggered": false}),
            continuity: Some(continuity("world_provider_codex", "run-a", "thread-a", 8, None)),
        };
        assert!(materialize_turn_control_result(temp.path(), 43, queued.clone()).is_err());
        queued.continuity = None;
        let artifact = materialize_turn_control_result(temp.path(), 43, queued).unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(payload["label"], TURN_INPUT_ACKNOWLEDGED_LABEL);
        assert!(!codex_context_labels().contains(TURN_INPUT_ACKNOWLEDGED_LABEL));
        assert!(payload.get(CODEX_CONTINUITY_FIELD).is_none());
        assert_eq!(payload["detail"]["turn_triggered"], false);
    }

    #[test]
    fn native_continuity_closes_on_the_final_checked_effect_only() {
        use crate::run_affordances::{CreatedRun, FileProof, NativeEffect, NativePhase};
        let file = FileProof {
            path: PathBuf::from("/fixture/record.json"), before_hash: None,
            after_hash: octopus_ledger::ContentHash::of_bytes(b"{}").to_hex(), bytes: 2,
        };
        let writing = NativePending {
            call_id: "native-call".into(), operation_name: "run_create".into(), source_run: "run-a".into(),
            effect: NativeEffect { tool: "file.write".into(), correlation_id: "native:write".into(), arguments: serde_json::json!({}) },
            state: NativePhase::Writing {
                created: CreatedRun {
                    run_id: "run-b".into(), worker_interface_id: "system_worker_b".into(),
                    provider_interface_id: "world_provider_b".into(), receive_interface_id: "system_worker_b".into(),
                    return_interface_ids: vec!["system_worker_a".into()], model: "model".into(), reasoning_effort: "high".into(),
                    service_tier: None,
                },
                files: vec![file.clone()], outgoing_bindings: vec![],
                seed: NativeEffect { tool: "artifact.send".into(), correlation_id: "native:seed".into(), arguments: serde_json::json!({}) },
            },
        };
        let pending = |operation: &NativePending| CodexPendingContinuity::DynamicTool {
            turn_id: "turn-1".into(), call_id: operation.effect.correlation_id.clone(),
            tool: operation.effect.tool.clone(), native_operation: Some(serde_json::to_value(operation).unwrap()),
        };
        let mut evidence = SessionContinuityEvidence {
            label: "file.write.result".into(), source_key: "write-receipt".into(), source_movement_seq: Some(11),
            payload: serde_json::json!({"label":"file.write.result","tool":"file.write","correlation_id":"native:write","ok":true,"body":{"result":"files_written","writes":[file]}}),
        };
        assert!(!session_evidence_targets_pending(&evidence, &pending(&writing)).unwrap());
        let NativeStep::Effect { pending: seeding, .. } = writing.advance(&evidence.payload).unwrap() else { panic!("write must precede seed"); };
        evidence.label = "artifact.send.result".into();
        evidence.payload = serde_json::json!({"label":"artifact.send.result","tool":"artifact.send","correlation_id":"native:seed","ok":true,"body":{"admitted":{"seq":12}}});
        assert!(session_evidence_targets_pending(&evidence, &pending(&seeding)).unwrap());
        assert!(!session_evidence_targets_pending(&evidence, &pending(&writing)).unwrap());
        evidence.payload["body"] = serde_json::json!({"admitted":{"seq":12},"invented":true});
        assert!(session_evidence_targets_pending(&evidence, &pending(&seeding)).is_err());
        assert_eq!(peer_notification_text("turn.stream", b"{\"seq\":1}\n{\"seq\":2}\n").unwrap(), "Connected execution recorded turn.stream.");
        assert!(peer_notification_text(INTERFACE_ARTIFACT_OUTPUT_LABEL, b"{\"seq\":1}\n{\"seq\":2}\n").is_err());
    }

    #[test]
    fn continuity_recovery_is_exact_provider_scoped_and_unambiguous() {
        let old = continuity("world_provider_codex", "run-a", "thr-exact", 4, None);
        let latest = continuity("world_provider_codex", "run-a", "thr-exact", 7, None);
        let history_payload = serde_json::json!({
            "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
            CODEX_CONTINUITY_FIELD: old.clone(),
        });
        let current = bound(
            "current",
            serde_json::json!({
                "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                CODEX_CONTINUITY_FIELD: latest.clone(),
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "artifacts": [session_artifact(
                        "provider-output-old",
                        INTERFACE_ARTIFACT_OUTPUT_LABEL,
                        history_payload.clone(),
                        41,
                    )]
                }
            }),
        );
        let labels = codex_context_labels();
        let snapshots = codex_snapshot_labels();
        assert_eq!(
            recover_codex_continuity(
                std::slice::from_ref(&current),
                &InterfaceId::new("world_provider_codex"),
                "run-a",
                &labels,
                &snapshots,
            )
            .unwrap(),
            Some(latest.clone())
        );

        let next_pending = continuity(
            "world_provider_codex",
            "run-a",
            "thr-exact",
            8,
            Some(CodexPendingContinuity::DynamicTool {
                native_operation: None,
                turn_id: "turn-2".to_string(),
                call_id: "call-2".to_string(),
                tool: "coder.exec".to_string(),
            }),
        );
        let pending_output = bound(
            "pending-output",
            serde_json::json!({
                "label": TOOL_CALL_LABEL,
                CODEX_CONTINUITY_FIELD: next_pending.clone(),
            }),
        );
        assert_eq!(
            recover_codex_continuity(
                &[current.clone(), pending_output],
                &InterfaceId::new("world_provider_codex"),
                "run-a",
                &labels,
                &snapshots,
            )
            .unwrap(),
            Some(next_pending.clone())
        );

        let mut stale_outer = next_pending.clone();
        stale_outer.context_generation += 1;
        let resolved_session = bound(
            "resolved-session",
            serde_json::json!({
                "label": TURN_ERROR_LABEL,
                "message": "the connection-local turn was interrupted",
                CODEX_CONTINUITY_FIELD: stale_outer.clone(),
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "artifacts": [
                        session_artifact(
                            "pending-tool-call",
                            TOOL_CALL_LABEL,
                            serde_json::json!({
                                "label": TOOL_CALL_LABEL,
                                "tool": "coder.exec",
                                "correlation_id": "call-2",
                                "arguments": {},
                                CODEX_CONTINUITY_FIELD: next_pending.clone(),
                            }),
                            50,
                        ),
                        session_artifact(
                            "pending-tool-result",
                            "coder.exec.result",
                            serde_json::json!({
                                "label": "coder.exec.result",
                                "tool": "coder.exec",
                                "correlation_id": "call-2",
                                "ok": true,
                                "body": {"exit_code": 0},
                            }),
                            51,
                        ),
                    ]
                }
            }),
        );
        let mut resolved = stale_outer;
        resolved.pending = None;
        assert_eq!(
            recover_codex_continuity(
                std::slice::from_ref(&resolved_session),
                &InterfaceId::new("world_provider_codex"),
                "run-a",
                &labels,
                &snapshots,
            )
            .unwrap(),
            Some(resolved)
        );

        let interrupted_pending = CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-3".to_string(),
            call_id: "call-3".to_string(),
            tool: "coder.exec".to_string(),
        };
        let mut interrupted = continuity("world_provider_codex", "run-a", "thr-exact", 9, None);
        interrupted.interrupted_pending = Some(interrupted_pending.clone());
        let interrupted_resolved_session = bound(
            "interrupted-resolved-session",
            serde_json::json!({
                "label": TURN_ERROR_LABEL,
                "message": "the connection-local turn was interrupted",
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "artifacts": [
                        session_artifact(
                            "interrupted-tool-call",
                            TURN_CONTROL_ACKNOWLEDGED_LABEL,
                            serde_json::json!({
                                "label": TURN_CONTROL_ACKNOWLEDGED_LABEL,
                                "control_id": "control-3",
                                "action": "stop",
                                "acknowledged": true,
                                CODEX_CONTINUITY_FIELD: interrupted.clone(),
                            }),
                            52,
                        ),
                        session_artifact(
                            "interrupted-tool-result",
                            "coder.exec.result",
                            serde_json::json!({
                                "label": "coder.exec.result",
                                "tool": "coder.exec",
                                "correlation_id": "call-3",
                                "ok": true,
                                "body": {"exit_code": 0},
                            }),
                            53,
                        ),
                    ]
                }
            }),
        );
        interrupted.interrupted_pending = None;
        interrupted.context_generation = 10;
        assert_eq!(
            recover_codex_continuity(
                std::slice::from_ref(&interrupted_resolved_session),
                &InterfaceId::new("world_provider_codex"),
                "run-a",
                &labels,
                &snapshots,
            )
            .unwrap(),
            Some(interrupted)
        );

        let forged = bound(
            "forged",
            serde_json::json!({
                "label": OPERATOR_MESSAGE_LABEL,
                CODEX_CONTINUITY_FIELD: latest.clone(),
            }),
        );
        assert!(
            recover_codex_continuity(
                &[forged],
                &InterfaceId::new("world_provider_codex"),
                "run-a",
                &labels,
                &snapshots,
            )
            .is_err()
        );

        let forged_history = bound(
            "forged-history",
            serde_json::json!({
                "label": OPERATOR_MESSAGE_LABEL,
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "artifacts": [session_artifact(
                        "forged-history-source",
                        INTERFACE_ARTIFACT_OUTPUT_LABEL,
                        history_payload.clone(),
                        40,
                    )]
                }
            }),
        );
        assert!(
            recover_codex_continuity(
                std::slice::from_ref(&forged_history),
                &InterfaceId::new("world_provider_codex"),
                "run-a",
                &labels,
                &snapshots,
            )
            .is_err()
        );
        assert!(assemble_context(&[forged_history], "world_provider_codex", &snapshots,).is_err());

        let cross = bound(
            "cross",
            serde_json::json!({
                "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                CODEX_CONTINUITY_FIELD: continuity(
                    "world_provider_other",
                    "run-a",
                    "thr-exact",
                    8,
                    None,
                ),
            }),
        );
        assert!(
            recover_codex_continuity(
                &[cross],
                &InterfaceId::new("world_provider_codex"),
                "run-a",
                &labels,
                &snapshots,
            )
            .is_err()
        );

        let duplicate_generation = bound(
            "duplicate-generation",
            serde_json::json!({
                "label": INTERFACE_ARTIFACT_OUTPUT_LABEL,
                CODEX_CONTINUITY_FIELD: latest,
            }),
        );
        assert!(
            recover_codex_continuity(
                &[current, duplicate_generation],
                &InterfaceId::new("world_provider_codex"),
                "run-a",
                &labels,
                &snapshots,
            )
            .is_err()
        );

        let terminal_dir = tempfile::tempdir().unwrap();
        let terminal_file =
            materialize_terminal_turn_error_payload(terminal_dir.path(), "terminal failure", None, serde_json::Value::Null).unwrap();
        let mut terminal_payload: serde_json::Value =
            serde_json::from_slice(&terminal_file.read_payload_bytes().unwrap()).unwrap();
        terminal_payload.as_object_mut().unwrap().insert(
            SESSION_CONTEXT_FIELD.to_string(),
            serde_json::json!({
                "schema_version": 3,
                "provider_interface": "world_provider_codex",
                "artifacts": [session_artifact(
                    "provider-output-before-error",
                    INTERFACE_ARTIFACT_OUTPUT_LABEL,
                    history_payload,
                    42,
                )]
            }),
        );
        let terminal = bound("terminal", terminal_payload);
        assert_eq!(
            recover_codex_continuity(
                &[terminal],
                &InterfaceId::new("world_provider_codex"),
                "run-a",
                &labels,
                &snapshots,
            )
            .unwrap(),
            Some(old)
        );
    }

    #[test]
    fn recovered_code_mode_result_requires_the_exact_native_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("rollout.jsonl");
        let expected = recovered_function_call_output("exec-call", "missing directory");
        let activity = CodexTurnOutput::dynamic_tool_completed(
            "formed_interfaces".into(), "workspace.list".into(), "exec-call".into(),
            "thread-a".into(), "turn-a".into(), false, None, None,
        );
        let receipt = serde_json::json!({"type":"event_msg", "payload":{
            "type":"item_completed", "thread_id":"thread-a", "turn_id":"turn-a",
            "item":{"type":"DynamicToolCall", "id":"exec-call", "namespace":"formed_interfaces",
                "tool":"workspace-dlist", "status":"failed", "success":false,
                "content_items":[{"type":"inputText", "text":"missing directory"}]}
        }});
        std::fs::write(&rollout, format!("{receipt}\n")).unwrap();
        assert!(recovered_tool_result_rollout_evidence(&rollout, &expected, Some(&activity.value)).unwrap().output_persisted);
        assert!(!recovered_tool_result_rollout_evidence(&rollout, &expected, None).unwrap().output_persisted);
        for (pointer, value) in [
            ("/payload/thread_id", serde_json::json!("other")),
            ("/payload/turn_id", serde_json::json!("other")),
            ("/payload/item/namespace", serde_json::json!("other")),
            ("/payload/item/tool", serde_json::json!("other")),
            ("/payload/item/success", serde_json::json!(true)),
            ("/payload/item/content_items/0/text", serde_json::json!("invented")),
        ] {
            let mut bad = receipt.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            std::fs::write(&rollout, format!("{bad}\n")).unwrap();
            assert!(recovered_tool_result_rollout_evidence(&rollout, &expected, Some(&activity.value)).is_err(), "{pointer}");
        }
        std::fs::write(&rollout, format!("{receipt}\n{receipt}\n")).unwrap();
        assert!(recovered_tool_result_rollout_evidence(&rollout, &expected, Some(&activity.value)).is_err());
    }

    #[test]
    fn recovered_function_output_reconciles_exact_rollout_history() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("rollout.jsonl");
        let expected = recovered_function_call_output("call-7", "result".to_string());
        std::fs::write(
            &rollout,
            format!(
                "{}\n",
                serde_json::json!({
                    "type": "response_item",
                    "payload": {"type": "message", "role": "assistant", "content": []}
                })
            ),
        )
        .unwrap();
        assert!(!recovered_function_call_output_is_persisted(&rollout, &expected).unwrap());
        let exact = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "function_call_output", "call_id": "call-7", "output": "result"}
        });
        std::fs::write(&rollout, format!("{exact}\n")).unwrap();
        assert!(recovered_function_call_output_is_persisted(&rollout, &expected).unwrap());
        let conflict = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "function_call_output", "call_id": "call-7", "output": "other"}
        });
        std::fs::write(&rollout, format!("{exact}\n{conflict}\n")).unwrap();
        assert!(recovered_function_call_output_is_persisted(&rollout, &expected).is_err());
        std::fs::write(&rollout, format!("{exact}\n{exact}\n")).unwrap();
        assert!(recovered_function_call_output_is_persisted(&rollout, &expected).is_err());
        std::fs::write(&rollout, exact.to_string()).unwrap();
        assert!(recovered_function_call_output_is_persisted(&rollout, &expected).is_err());
    }

    #[test]
    fn recovered_successor_dispatch_fails_closed_before_duplicate_turn_start() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir.path().join("rollout.jsonl");
        let expected = recovered_function_call_output("call-7", "result".to_string());
        let exact = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "function_call_output", "call_id": "call-7", "output": "result"}
        });
        let started = serde_json::json!({
            "type": "event_msg",
            "payload": {
                "type": "task_started",
                "turn_id": "turn-successor",
                "model_context_window": 128000,
                "collaboration_mode_kind": "default"
            }
        });
        std::fs::write(&rollout, format!("{exact}\n{started}\n")).unwrap();
        let rollout_evidence =
            recovered_tool_result_rollout_evidence(&rollout, &expected, None).unwrap();
        assert_eq!(
            rollout_evidence,
            RecoveredToolResultRolloutEvidence {
                output_persisted: true,
                successor_turn_id: Some("turn-successor".to_string()),
                native_function_calls: 0,
            }
        );
        assert!(reconcile_recovered_successor_dispatch(&rollout_evidence, &[]).is_err());

        std::fs::write(&rollout, format!("{exact}\n")).unwrap();
        let rollout_evidence =
            recovered_tool_result_rollout_evidence(&rollout, &expected, None).unwrap();
        let request_frame = br#"{"id":7,"method":"turn/start","params":{}}
"#;
        let dispatch_file = materialize_codex_transport_dispatch(
            dir.path(),
            &InterfaceId::new("world_provider_codex"),
            70,
            2,
            "turn/start",
            request_frame,
            None,
        )
        .unwrap();
        let manifest_bytes = std::fs::read(dispatch_file.manifest_path()).unwrap();
        let content = octopus_ledger::ContentHash::of_bytes(&manifest_bytes);
        let artifact = ArtifactRef {
            id: octopus_descriptors::ArtifactId::new(format!(
                "{PROVIDER_TRANSPORT_DISPATCH_LABEL}:{}",
                content.to_hex()
            )),
            content,
        };
        let recovered = CodexRecoveredHandoffArtifact {
            movement_seq: 71,
            artifact,
            payload: dispatch_file.read_payload_bytes().unwrap(),
        };
        let dispatches = recovered_turn_start_dispatches(
            &[recovered],
            &InterfaceId::new("world_provider_codex"),
            70,
        )
        .unwrap();
        assert_eq!(dispatches.len(), 1);
        let error =
            reconcile_recovered_successor_dispatch(&rollout_evidence, &dispatches).unwrap_err();
        assert!(matches!(
            error,
            InterfaceDriverError::FailedClosed(message)
                if message.contains("no exact successor TurnStarted receipt")
                    && message.contains("second successor turn is forbidden")
        ));
    }

    #[test]
    fn current_session_recovery_keeps_the_newest_outstanding_effect() {
        let old_pending = CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-7".to_string(),
            call_id: "call-7".to_string(),
            tool: "coder.exec".to_string(),
        };
        let old = continuity(
            "world_provider_codex",
            "run-a",
            "thr-exact",
            7,
            Some(old_pending),
        );
        let source: SessionContextArtifact = serde_json::from_value(session_artifact(
            "source-result",
            "coder.exec.result",
            serde_json::json!({
                "label": "coder.exec.result",
                "tool": "coder.exec",
                "correlation_id": "call-7",
                "ok": true,
                "body": {"exit_code": 0},
            }),
            71,
        ))
        .unwrap();
        let new_pending = CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-8".to_string(),
            call_id: "call-8".to_string(),
            tool: "coder.exec".to_string(),
        };
        let newest = continuity(
            "world_provider_codex",
            "run-a",
            "thr-exact",
            8,
            Some(new_pending),
        );
        let artifacts = vec![
            serde_json::from_value(session_artifact(
                "old-call",
                TOOL_CALL_LABEL,
                serde_json::json!({"label": TOOL_CALL_LABEL, CODEX_CONTINUITY_FIELD: old.clone()}),
                70,
            ))
            .unwrap(),
            source.clone(),
            serde_json::from_value(session_artifact(
                "new-call",
                TOOL_CALL_LABEL,
                serde_json::json!({"label": TOOL_CALL_LABEL, CODEX_CONTINUITY_FIELD: newest.clone()}),
                72,
            ))
            .unwrap(),
        ];
        let current = recover_codex_continuity_from_current_session(
            &artifacts,
            std::slice::from_ref(&source.source_artifact),
            &InterfaceId::new("world_provider_codex"),
            "run-a",
            &codex_context_labels(),
        )
        .unwrap();
        assert_eq!(current, Some(newest.clone()));
        assert_eq!(
            reconcile_current_session_continuity(Some(old), current).unwrap(),
            Some(newest)
        );

        let mut interrupted = continuity("world_provider_codex", "run-a", "thr-exact", 7, None);
        interrupted.interrupted_pending = Some(CodexPendingContinuity::DynamicTool {
            native_operation: None, turn_id: "turn-7".into(), call_id: "call-7".into(),
            tool: "coder.exec".into(),
        });
        let ack: SessionContextArtifact = serde_json::from_value(session_artifact(
            "stop-ack", TURN_CONTROL_ACKNOWLEDGED_LABEL,
            serde_json::json!({"label": TURN_CONTROL_ACKNOWLEDGED_LABEL, CODEX_CONTINUITY_FIELD: interrupted}), 70,
        )).unwrap();
        let next_input = ArtifactRef {
            id: octopus_descriptors::ArtifactId::new("next-input"),
            content: octopus_ledger::ContentHash::of_bytes(b"next-input"),
        };
        let recover = |entries: &[SessionContextArtifact]| recover_codex_continuity_from_current_session(
            entries, std::slice::from_ref(&next_input), &InterfaceId::new("world_provider_codex"), "run-a", &codex_context_labels(),
        );
        let closed = recover(&[ack.clone(), source.clone()]).unwrap().unwrap();
        let forwarded_sources = [next_input.clone(), source.source_artifact.clone()];
        let awaiting = recover_codex_continuity_from_current_session(&[ack.clone(), source.clone()],
            &forwarded_sources, &InterfaceId::new("world_provider_codex"), "run-a", &codex_context_labels()).unwrap().unwrap();
        assert_eq!(awaiting.context_generation, 7);
        assert!(awaiting.interrupted_pending.is_some());
        assert_eq!(closed.context_generation, 8);
        assert!(closed.interrupted_pending.is_none());
        validate_recovered_thread_coordinates(
            &closed, &closed.thread_shape_fingerprint, 8, &closed.static_context_fingerprint,
        ).unwrap();
        assert!(validate_recovered_thread_coordinates(
            &closed, &closed.thread_shape_fingerprint, 9, &closed.static_context_fingerprint,
        ).is_err());
        let mut later_ack = ack.clone();
        later_ack.source_artifact.id = octopus_descriptors::ArtifactId::new("later-stop-ack");
        later_ack.source_movement_seq = Some(72);
        later_ack.payload[CODEX_CONTINUITY_FIELD]["context_generation"] = serde_json::json!(8);
        assert_eq!(recover(&[ack.clone(), source.clone(), later_ack]).unwrap(), Some(closed.clone()));
        let mut wrong_result = source.clone();
        wrong_result.payload["correlation_id"] = serde_json::json!("other-call");
        let unchanged = recover(&[ack.clone(), wrong_result.clone()]).unwrap().unwrap();
        assert_eq!(unchanged.context_generation, 7);
        assert!(unchanged.interrupted_pending.is_some());
        wrong_result.payload["correlation_id"] = serde_json::json!("call-7");
        wrong_result.payload["tool"] = serde_json::json!("other-tool");
        assert!(recover(&[ack.clone(), wrong_result]).is_err());
        let mut duplicate = source.clone();
        duplicate.source_artifact.id = octopus_descriptors::ArtifactId::new("duplicate-result");
        duplicate.source_movement_seq = Some(73);
        assert!(recover(&[ack.clone(), source.clone(), duplicate]).is_err());
        let mut replay = ack;
        replay.source_movement_seq = Some(74);
        replay.payload[CODEX_CONTINUITY_FIELD] = serde_json::to_value(&closed).unwrap();
        assert_eq!(recover(&[source, replay]).unwrap(), Some(closed));
    }

    #[test]
    fn terminal_failure_settles_only_the_result_owned_by_its_source() {
        let descriptor: InterfaceDescriptor = serde_json::from_slice(
            &std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../interface.json")
                    .canonicalize()
                    .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let pending = CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-7".to_string(),
            call_id: "call-7".to_string(),
            tool: "coder.exec".to_string(),
        };
        let old = continuity(
            "world_provider_codex",
            "run-a",
            "thr-exact",
            7,
            Some(pending),
        );
        let mut old_artifact: SessionContextArtifact = serde_json::from_value(session_artifact(
            "old-call",
            TOOL_CALL_LABEL,
            serde_json::json!({"label": TOOL_CALL_LABEL, CODEX_CONTINUITY_FIELD: old}),
            70,
        ))
        .unwrap();
        old_artifact.payload["correlation_id"] = serde_json::json!("call-7");
        old_artifact.payload["tool"] = serde_json::json!("coder.exec");
        let result = serde_json::json!({
            "label": "coder.exec.result",
            "tool": "coder.exec",
            "correlation_id": "call-7",
            "ok": true,
            "body": {"exit_code": 0},
        });
        let result_bytes = serde_json::to_vec(&result).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let input = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("source-result"),
                content: octopus_ledger::ContentHash::of_bytes(&result_bytes),
            },
            artifact_file: InterfaceArtifactFile::write(
                dir.path(),
                "coder.exec.result",
                &result_bytes,
            )
            .unwrap(),
            artifact_binding_payloads: Vec::new(),
            metadata: Some(
                octopus_interfaces::InvocationMetadata::new(
                    octopus_interfaces::GrantProjection::default(),
                )
                .with_runtime(RuntimeInvocationMetadata {
                    run: octopus_interfaces::RunId::new("run-a"),
                    ee: octopus_descriptors::EeId::new("ee-a"),
                    worker: InterfaceId::new("system_worker_test"),
                    checkpoint: octopus_descriptors::CheckpointId::new("checkpoint-a"),
                    interface: InterfaceId::new("world_provider_codex"),
                    epoch: 1,
                    run_record_hash: None,
                }),
            ),
        };
        let settled = terminal_output_continuity(
            &descriptor,
            &input,
            std::slice::from_ref(&old_artifact),
            &octopus_descriptors::RunId::new("run-a"),
            br#"{"id":"run-a","interface_state":{"world_provider_codex":{"context_generation":7}}}"#,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(settled.context_generation, 8);
        assert!(settled.pending.is_none());

        let bound = parse_codex_continuity(
            &old_artifact.payload[CODEX_CONTINUITY_FIELD], &descriptor.id, "run-a",
        ).unwrap();
        let mut operator = input.clone();
        operator.artifact.id = octopus_descriptors::ArtifactId::new("operator-message");
        let completed_result: SessionContextArtifact = serde_json::from_value(session_artifact(
            "completed-result", "coder.exec.result", result, 71,
        )).unwrap();
        assert!(reconcile_operator_session_continuity(
            &descriptor, &operator, Some(bound.clone()), std::slice::from_ref(&old_artifact),
        ).unwrap().unwrap().pending.is_some());
        assert!(reconcile_operator_session_continuity(
            &descriptor, &operator, Some(bound.clone()), &[old_artifact.clone(), completed_result.clone()],
        ).unwrap().unwrap().pending.is_none());
        let recovered_context = [old_artifact.clone(), completed_result.clone()];
        assert_eq!(recovered_failed_result_source(&descriptor, &operator, &bound, &recovered_context, &[]).unwrap(), None);
        assert_eq!(recovered_failed_result_source(&descriptor, &operator, &bound, &recovered_context, &[69]).unwrap(), None);
        assert_eq!(recovered_failed_result_source(&descriptor, &operator, &bound, &recovered_context, &[71]).unwrap(), Some(71));
        let restored_model_context = resumed_model_context(&recovered_context, &operator.artifact, &settled, ModelResultProjection { max_tokens: 1000 }).unwrap().unwrap();
        assert!(restored_model_context.contains("completed-result") && restored_model_context.contains("call-7"));
        let native_context = native_turn_context(Some(restored_model_context), None);
        assert!(matches!(native_context["octopus_bound_context"].kind, AdditionalContextKind::Untrusted));
        let settled_error: SessionContextArtifact = serde_json::from_value(session_artifact(
            "settled-error", TURN_ERROR_LABEL, serde_json::json!({"label":TURN_ERROR_LABEL,"message":"provider stopped",CODEX_CONTINUITY_FIELD:settled}), 72,
        )).unwrap();
        let foreign_output: SessionContextArtifact = serde_json::from_value(session_artifact(
            "foreign-result", INTERFACE_ARTIFACT_OUTPUT_LABEL, serde_json::json!({"label":INTERFACE_ARTIFACT_OUTPUT_LABEL,"text":"peer result",CODEX_CONTINUITY_FIELD:continuity("world_provider_peer","run-peer","thr-peer",1,None)}), 72,
        )).unwrap();
        let after_error = [old_artifact.clone(), completed_result.clone(), foreign_output, settled_error];
        let after_error_context = resumed_model_context(&after_error, &operator.artifact, &settled, ModelResultProjection { max_tokens: 1000 }).unwrap().unwrap();
        assert!(after_error_context.contains("completed-result") && after_error_context.contains("peer result"));
        assert!(resumed_model_context(&recovered_context, &completed_result.source_artifact, &settled, ModelResultProjection { max_tokens: 1000 }).unwrap().is_none());
        let mut wrong_result = completed_result.clone();
        wrong_result.payload["tool"] = serde_json::json!("coder.write_file");
        assert!(recovered_failed_result_source(&descriptor, &operator, &bound, &[old_artifact.clone(), wrong_result], &[71]).is_err());
        assert!(recovered_failed_result_source(&descriptor, &operator, &bound, &[old_artifact.clone(), completed_result.clone(), completed_result], &[71]).is_err());

        let new_pending = CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-8".to_string(),
            call_id: "call-8".to_string(),
            tool: "coder.exec".to_string(),
        };
        let newest = continuity(
            "world_provider_codex",
            "run-a",
            "thr-exact",
            8,
            Some(new_pending.clone()),
        );
        let newest_artifact: SessionContextArtifact = serde_json::from_value(session_artifact(
            "new-call",
            TOOL_CALL_LABEL,
            serde_json::json!({"label": TOOL_CALL_LABEL, CODEX_CONTINUITY_FIELD: newest}),
            72,
        ))
        .unwrap();
        let retained = terminal_output_continuity(
            &descriptor,
            &input,
            &[old_artifact, newest_artifact],
            &octopus_descriptors::RunId::new("run-a"),
            br#"{"id":"run-a","interface_state":{"world_provider_codex":{"context_generation":9}}}"#,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(retained.context_generation, 10);
        assert_eq!(retained.pending, Some(new_pending));
    }

    #[test]
    fn stale_or_unstructured_continuation_fails_closed() {
        let recovered = continuity("world_provider_codex", "run-a", "thr-exact", 7, None);
        assert!(
            validate_recovered_thread_coordinates(&recovered, &"a".repeat(64), 6, &"b".repeat(64),)
                .is_err()
        );
        assert!(
            validate_recovered_thread_coordinates(&recovered, &"a".repeat(64), 7, &"b".repeat(64),)
                .is_ok()
        );
        assert!(
            validate_recovered_thread_coordinates(&recovered, &"a".repeat(64), 8, &"b".repeat(64),)
                .is_err()
        );
        assert!(validate_recovered_reseed_coordinates(&recovered, 8, true).is_ok());
        assert!(validate_recovered_reseed_coordinates(&recovered, 8, false).is_err());
        let mut future = recovered.clone();
        future.context_generation = 9;
        assert!(validate_recovered_reseed_coordinates(&future, 8, true).is_err());
        let input = ProviderInput::ToolResult(ToolResultArtifact {
            label: "coder.exec.result".to_string(),
            tool: "coder.exec".to_string(),
            correlation_id: Some("call-1".to_string()),
            ok: true,
            body: Some(serde_json::json!({"exit_code": 0})),
            error: None,
        });
        assert!(matches!(
            input.into_turn_input(),
            Err(InterfaceDriverError::FailedClosed(_))
        ));

        let pending = PendingServerRequest {
            request_id: RequestId::Integer(1),
            thread_id: "thr-exact".to_string(),
            turn_id: "turn-1".to_string(),
            key: "tool:call-1".to_string(),
            kind: PendingServerRequestKind::DynamicTool {
                call_id: "call-1".to_string(),
                tool: "coder.exec".to_string(),
            },
            effect_gate: Arc::new(tokio::sync::Mutex::new(())),
            turn_interrupted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let live = TransportState {
            context_generation: 8,
            hooks_catalog_emitted: true,
            thread_id: Some("thr-exact".to_string()),
            thread_shape_fingerprint: Some("a".repeat(64)),
            static_context_fingerprint: Some("b".repeat(64)),
            rollout_path: None,
        };
        let mut pending_continuity = recovered.clone();
        pending_continuity.context_generation = 8;
        pending_continuity.pending = Some(CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-1".to_string(),
            call_id: "call-1".to_string(),
            tool: "coder.exec".to_string(),
        });
        assert!(
            validate_live_pending_continuity(Some(&pending_continuity), &pending, 8, &live,)
                .is_ok()
        );
        let mut forged = pending_continuity.clone();
        forged.static_context_fingerprint = "c".repeat(64);
        assert!(validate_live_pending_continuity(Some(&forged), &pending, 8, &live).is_err());
        assert!(
            validate_live_pending_continuity(Some(&pending_continuity), &pending, 7, &live)
                .is_err()
        );
        assert!(validate_live_pending_continuity(None, &pending, 8, &live).is_err());
    }

    fn native_result_test_source(value: &impl serde::Serialize) -> ArtifactRef {
        bound("native.result", serde_json::to_value(value).unwrap()).artifact
    }

    #[test]
    fn structured_tool_failure_reaches_the_codex_continuation_intact() {
        let mut details = serde_json::Map::new();
        details.insert("engine".to_string(), serde_json::json!("docker"));
        let result = ToolResultArtifact::failure(
            "container.status",
            Some("call-2".to_string()),
            octopus_tool_handlers::ToolResultError::new(
                "container_engine_unavailable",
                "container engine is unavailable",
            )
            .with_details(details),
        );
        let response = dynamic_tool_response_value(&result, &native_result_test_source(&result), ModelResultProjection { max_tokens: 512 }).unwrap();
        assert_eq!(response["success"], false);
        let text = response["contentItems"][0]["text"].as_str().unwrap();
        let wire: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(wire["tool"], "container.status");
        assert_eq!(wire["error"]["code"], "container_engine_unavailable");
        assert_eq!(wire["error"]["details"]["engine"], "docker");

        let full_text = format!("{}middle-observation-ref{}", "界🙂".repeat(4_000), "é".repeat(8_000));
        let large = ToolResultArtifact::success(
            "coder.worktree.snapshot",
            Some("call-3".to_string()),
            serde_json::json!({"unified_diff": full_text}),
        );
        let raw = dynamic_tool_response_value(&large, &native_result_test_source(&large), ModelResultProjection { max_tokens: 512 }).unwrap();
        let raw_text = raw["contentItems"][0]["text"].as_str().unwrap();
        let raw_value: serde_json::Value = serde_json::from_str(raw_text).unwrap();
        assert_eq!(raw_value["body"], large.body.as_ref().unwrap().clone());
        assert!(raw_text.contains("middle-observation-ref"));
        assert!(raw_value.get("model_projection").is_none());

        let bound_result = bound("coder.result:source", serde_json::to_value(&large).unwrap());
        let original_payload = bound_result.read_bytes().unwrap().into_owned();
        let assembled = assemble_context_resolved(
            std::slice::from_ref(&bound_result),
            &BTreeMap::new(),
            &InterfaceId::new("world_provider_test"),
            &codex_snapshot_labels(),
            ModelResultProjection { max_tokens: 512 },
        ).unwrap();
        let projected_text = assembled.conversation[0].split_once('\n').unwrap().1;
        assert!(octopus_util_string::approx_token_count(projected_text) <= 512);
        let projection: serde_json::Value = serde_json::from_str(projected_text).unwrap();
        assert_eq!(projection["model_projection"], "head_tail");
        assert_eq!(projection["source_artifact"], serde_json::to_value(&bound_result.artifact).unwrap());
        assert_eq!(bound_result.read_bytes().unwrap().as_ref(), original_payload.as_slice());
        assert!(projection["omitted_approx_tokens"].as_u64().unwrap() > 0);
        assert!(
            projection["preview"]
                .as_str()
                .unwrap()
                .contains("tokens truncated")
        );

        let recovered = recovered_function_call_output(
            "call-3",
            recovered_result_output(dynamic_tool_response_content(&large, &native_result_test_source(&large), ModelResultProjection { max_tokens: 512 }).unwrap()).unwrap(),
        );
        assert_eq!(recovered["output"], raw_text);
        let rollout = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(rollout.path(), format!("{}\n", serde_json::json!({
            "type": "response_item", "payload": recovered,
        }))).unwrap();
        assert!(recovered_function_call_output_is_persisted(rollout.path(), &recovered).unwrap());
        let projected_output = recovered_function_call_output("call-3", projected_text);
        assert!(recovered_function_call_output_is_persisted(rollout.path(), &projected_output).is_err());

        let mut operation = InterfaceOperationResultArtifact {
            label: "room.result".into(), operation: "observe".into(),
            correlation_id: "call-4".into(), ok: true, value: large.body.clone().unwrap(),
        };
        let operation_response = interface_operation_response_value(&operation, &native_result_test_source(&operation.value), ModelResultProjection { max_tokens: 512 }).unwrap();
        let operation_text = operation_response["contentItems"][0]["text"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(operation_text).unwrap(), operation.value);
        operation.value = serde_json::json!({"text": "x".repeat(8 * 1024 * 1024)});
        let source = native_result_test_source(&operation.value);
        let operation_output = interface_operation_response_value(&operation, &source, ModelResultProjection { max_tokens: 512 }).unwrap();
        let projected: serde_json::Value = serde_json::from_str(operation_output["contentItems"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(projected["truncated"], true);
        assert_eq!(projected["source_artifact"], serde_json::to_value(&source).unwrap());
        assert_eq!(projected["identity"]["correlation_id"], "call-4");
        assert_eq!(operation_output["success"], true);
        let oversized = ToolResultArtifact::success("room.observe", Some("call-5".into()), operation.value);
        let original = serde_json::to_vec(&oversized).unwrap();
        let source = native_result_test_source(&oversized);
        for max_tokens in [512, 8 * 1024 * 1024] {
            let projection = ModelResultProjection { max_tokens };
            let live = dynamic_tool_response_value(&oversized, &source, projection).unwrap();
            let text = live["contentItems"][0]["text"].as_str().unwrap();
            let projected: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(projected["truncated"], true);
            assert_eq!(projected["source_artifact"], serde_json::to_value(&source).unwrap());
            assert_eq!(projected["identity"]["correlation_id"], "call-5");
            assert!(!crate::result_media::text_exceeds_limit(text).unwrap());
            let recovered = recovered_result_output(dynamic_tool_response_content(&oversized, &source, projection).unwrap()).unwrap();
            assert_eq!(recovered, text);
            assert_eq!(serde_json::to_vec(&oversized).unwrap(), original);
            let rollout = tempfile::NamedTempFile::new().unwrap();
            let expected = recovered_function_call_output("call-5", recovered);
            std::fs::write(rollout.path(), format!("{}\n", serde_json::json!({"type":"response_item", "payload":expected}))).unwrap();
            assert!(recovered_function_call_output_is_persisted(rollout.path(), &expected).unwrap());
        }
    }

    #[test]
    fn interface_input_media_reaches_the_model_as_images() {
        use base64::Engine;
        let encoded = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGNwaDgARAwQCgAoDgYBQzpj2gAAAABJRU5ErkJggg==";
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let event = serde_json::json!({"event": "message", "chat": {"id": 7}, "media": [{
            "mime_type": "image/png", "payload_base64": encoded,
            "artifact": {"label": "chat.file", "payload_path": "/private/life/media/photo.png",
                "payload_content_hash": blake3::hash(&bytes).to_hex().to_string()}
        }]});
        let decode = |value: &serde_json::Value| decode_provider_input(&serde_json::to_vec(value).unwrap(), "chat.event",
            None, &BTreeMap::new(), None, &serde_json::Map::new());
        let ProviderInput::Turn(turn) = decode(&event).unwrap() else { panic!("an interface event is a turn") };
        let [UserInput::Text { text, .. }, UserInput::Image { url, .. }] = turn.as_slice() else { panic!("event lost its image: {turn:?}") };
        assert_eq!(url, &format!("data:image/png;base64,{encoded}"));
        assert!(text.contains("\"chat\"") && text.contains("chat.file") && !text.contains(encoded));
        assert!(!crate::result_media::without_embedded_payloads(&event).to_string().contains(encoded));
        let mut forged = event.clone();
        forged["media"][0]["mime_type"] = serde_json::json!("image/jpeg");
        assert!(decode(&forged).is_err());
    }

    #[test]
    fn embedded_result_media_survives_live_and_recovered_tool_outputs() {
        use base64::Engine;
        let encoded = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGNwaDgARAwQCgAoDgYBQzpj2gAAAABJRU5ErkJggg==";
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let body = serde_json::json!({"media": [{
            "mime_type": "image/png", "payload_base64": encoded,
            "artifact": {"label": "screen.captured", "payload_path": "/unreadable/room/screen.payload",
                "payload_content_hash": blake3::hash(&bytes).to_hex().to_string()}
        }], "observation_id": "exact-observation"});
        let result = ToolResultArtifact::success("room.observe", Some("call-image".into()), body.clone());
        let content = dynamic_tool_response_content(&result, &native_result_test_source(&result), ModelResultProjection { max_tokens: 512 }).unwrap();
        let live = dynamic_tool_response_value(&result, &native_result_test_source(&result), ModelResultProjection { max_tokens: 512 }).unwrap();
        assert_eq!(live["contentItems"][1]["imageUrl"], format!("data:image/png;base64,{encoded}"));
        assert!(!live["contentItems"][0]["text"].as_str().unwrap().contains(encoded));
        assert!(live["contentItems"][0]["text"].as_str().unwrap().contains("exact-observation"));
        assert_eq!(result.body.as_ref().unwrap(), &body);
        let recovered = recovered_function_call_output("call-image", recovered_result_output(content).unwrap());
        let typed: octopus_protocol::models::ResponseInputItem = serde_json::from_value(recovered.clone()).unwrap();
        assert!(matches!(typed, octopus_protocol::models::ResponseInputItem::FunctionCallOutput { .. }));
        assert_eq!(recovered["output"][1]["image_url"], live["contentItems"][1]["imageUrl"]);
        let operation = InterfaceOperationResultArtifact { label: "room.result".into(), operation: "observe".into(),
            correlation_id: "call-image".into(), ok: true, value: body.clone() };
        let operation_live = interface_operation_response_value(&operation, &native_result_test_source(&operation.value), ModelResultProjection { max_tokens: 512 }).unwrap();
        assert_eq!(operation_live["contentItems"][1], live["contentItems"][1]);
        let operation_recovered = recovered_result_output(interface_operation_response_content(&operation, &native_result_test_source(&operation.value), ModelResultProjection { max_tokens: 512 }).unwrap()).unwrap();
        assert_eq!(operation_recovered[1], recovered["output"][1]);
        let rollout = tempfile::NamedTempFile::new().unwrap();
        for detail in ["high", "original", "low", "auto"] {
            let mut recorded = recovered.clone();
            recorded["output"][1]["detail"] = serde_json::json!(detail);
            std::fs::write(rollout.path(), format!("{}\n", serde_json::json!({
                "type": "response_item", "payload": recorded,
            }))).unwrap();
            let persisted = recovered_function_call_output_is_persisted(rollout.path(), &recovered);
            if matches!(detail, "high" | "original") {
                assert!(persisted.unwrap());
            } else {
                assert!(persisted.is_err());
            }
        }
        let mut changed = recovered["output"].clone();
        changed[1]["image_url"] = serde_json::json!("data:image/png;base64,changed");
        assert!(!crate::result_media::matches_recorded_output(&recovered["output"], &changed));
        for (fixture, valid) in [
            ("iVBORw0KGgoAAAANSUhEUgAADIAAAAABCAIAAACTqqMmAAAAJUlEQVR4nO3CQREAAAgDIKMtmtGMZoe94ZjsqaqqqqqqqqqqWnzgSsEP43FopgAAAABJRU5ErkJggg==", true),
            ("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6ZQAAAABJRU5ErkJggg==", false),
        ] {
            let mut candidate = body.clone();
            candidate["media"][0]["payload_base64"] = serde_json::json!(fixture);
            candidate["media"][0]["artifact"]["payload_content_hash"] = serde_json::json!(
                blake3::hash(&base64::engine::general_purpose::STANDARD.decode(fixture).unwrap()).to_hex().to_string());
            let normalized = crate::result_media::project(&candidate);
            assert_eq!(normalized.is_ok(), valid);
            if let Ok(Some(normalized)) = normalized {
                assert_eq!(normalized.value["media"][0]["source_image"]["width"], 3200);
                let DynamicToolCallOutputContentItem::InputImage { image_url } = &normalized.images[0] else { panic!() };
                assert_eq!(image_url, &format!("data:image/png;base64,{fixture}"));
                let expected = recovered_result_output(normalized.images.clone()).unwrap();
                for (detail, max_dimension, max_patches) in [("high", 2048, 2500), ("original", 6000, 10000)] {
                    let prepared = octopus_util_image::load_data_url_for_prompt(image_url,
                        octopus_util_image::PromptImageMode::ResizeWithLimits(octopus_util_image::PromptImageResizeLimits {
                            max_dimension, max_patches,
                        })).unwrap();
                    assert_eq!(prepared.width, if detail == "high" { 2048 } else { 3200 });
                    let mut recorded = expected.clone();
                    recorded[0]["detail"] = serde_json::json!(detail);
                    recorded[0]["image_url"] = serde_json::json!(prepared.into_data_url());
                    assert!(crate::result_media::matches_recorded_output(&expected, &recorded));
                    recorded[0]["image_url"] = serde_json::json!(format!("data:image/png;base64,{encoded}"));
                    assert!(!crate::result_media::matches_recorded_output(&expected, &recorded));
                }
            }
        }
        assert!(crate::result_media::validate_content(&[
            DynamicToolCallOutputContentItem::InputText { text: "\"".repeat(4 * 1024 * 1024) },
        ]).is_err());
        for (field, value) in [("payload_base64", serde_json::json!("AAAA")),
            ("mime_type", serde_json::json!("image/jpeg")), ("artifact", serde_json::json!({}))] {
            let mut invalid = body.clone();
            invalid["media"][0][field] = value;
            assert!(crate::result_media::project(&invalid).is_err());
        }
        let mut missing = body.clone();
        missing["media"][0].as_object_mut().unwrap().remove("payload_base64");
        assert!(crate::result_media::project(&missing).is_err());
        let mut missing_hash = body.clone();
        missing_hash["media"][0]["artifact"].as_object_mut().unwrap().remove("payload_content_hash");
        assert!(crate::result_media::project(&missing_hash).is_err());
        assert!(crate::result_media::project(&serde_json::json!({"media": vec![body["media"][0].clone(); 17]})).is_err());
        assert_eq!(recovered_result_output(vec![DynamicToolCallOutputContentItem::InputText { text: "exact old text".into() }]).unwrap(), "exact old text");
    }

    #[test]
    fn ten_thousand_transient_events_never_reach_run_authoring() {
        let labels = codex_context_labels();
        let projected = codex_client_projection_labels();
        let rules = live_stream_rules();
        let (emit, mut receiver) = ProviderOutputStream::channel();
        let output = tempfile::tempdir().unwrap();
        let live_stream_root = output.path().join("streams");
        let provider_interface = InterfaceId::new("world_provider_codex");
        let sink = TurnSink {
            emit: &emit,
            output_root: output.path(),
            live_stream_root: &live_stream_root,
            provider_interface: &provider_interface,
            run: "run-a",
            source_movement_seq: 7,
            attempt: 1,
            context_output_labels: &labels,
            client_projection_output_labels: &projected,
            live_stream_rules: &rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: Arc::new(std::sync::Mutex::new(ContinuityEmitterState::default())),
        };
        sink.bind_live_stream("thread-a", "turn-a").unwrap();
        let user = |client_id: Option<&str>| ThreadItem::UserMessage {
            id: "user-a".to_string(), client_id: client_id.map(str::to_string), content: Vec::new(),
        };
        sink.record_input_item(&user(None), "thread-a", "turn-a", InputRecordedSource::ItemStarted, Some(1)).unwrap();
        sink.record_input_item(&user(Some("control-a")), "thread-a", "turn-a", InputRecordedSource::ItemStarted, Some(1)).unwrap();
        sink.record_input_item(&user(Some("control-a")), "thread-a", "turn-a", InputRecordedSource::ItemCompleted, Some(2)).unwrap();
        for seq in 0..10_000u64 {
            sink.send(CodexTurnOutput::client(
                AGENT_MESSAGE_DELTA_LABEL,
                serde_json::json!({"seq": seq, "text": "x"}),
            ))
            .unwrap();
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        let stream_guard = sink.live_stream.lock().unwrap();
        let stream = stream_guard.as_ref().unwrap();
        let manifest = InterfaceArtifactFile::new(stream.manifest_path().to_path_buf())
            .unwrap()
            .read_manifest()
            .unwrap();
        assert!(manifest.payload_content_hash.is_none());
        assert_eq!(
            std::fs::read_to_string(&manifest.payload_path)
                .unwrap()
                .lines()
                .count(),
            10_002
        );
        let receipts = std::fs::read_to_string(&manifest.payload_path).unwrap().lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|row| row["type"] == "input_recorded").collect::<Vec<_>>();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0]["client_id"], "control-a");
        drop(stream_guard);
        sink.send(CodexTurnOutput::client(
            TURN_COMPLETED_LABEL,
            serde_json::json!({"status": "completed"}),
        ))
        .unwrap();
        let ProviderStreamEmission::Turn(completed) = receiver.try_recv().unwrap() else {
            panic!("turn.completed must cross ordinary provider run-authoring")
        };
        assert_eq!(
            completed.artifact.read_manifest().unwrap().label,
            TURN_COMPLETED_LABEL
        );
    }

    #[test]
    fn reasoning_started_streams_but_reasoning_completed_is_an_artifact() {
        let labels = codex_context_labels();
        let projected = codex_client_projection_labels();
        let rules = live_stream_rules();
        let (emit, mut receiver) = ProviderOutputStream::channel();
        let output = tempfile::tempdir().unwrap();
        let live_stream_root = output.path().join("streams");
        let provider_interface = InterfaceId::new("world_provider_codex");
        let sink = TurnSink {
            emit: &emit,
            output_root: output.path(),
            live_stream_root: &live_stream_root,
            provider_interface: &provider_interface,
            run: "run-a",
            source_movement_seq: 8,
            attempt: 1,
            context_output_labels: &labels,
            client_projection_output_labels: &projected,
            live_stream_rules: &rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: Arc::new(std::sync::Mutex::new(ContinuityEmitterState::default())),
        };
        sink.bind_live_stream("thread-a", "turn-a").unwrap();
        let reasoning = || ThreadItem::Reasoning {
            id: "reason-a".to_string(),
            summary: vec!["summary".to_string()],
            content: vec!["content".to_string()],
        };
        sink.send(started_item_output(reasoning(), "thread-a", "turn-a", 1).unwrap())
            .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        sink.send(completed_item_output(reasoning(), "thread-a", "turn-a", 2).unwrap())
            .unwrap();
        let ProviderStreamEmission::Turn(completed) = receiver.try_recv().unwrap() else {
            panic!("completed reasoning must cross the ordinary artifact seam")
        };
        assert_eq!(
            completed.artifact.read_manifest().unwrap().label,
            TURN_REASONING_LABEL
        );
        let completed_payload: serde_json::Value =
            serde_json::from_slice(&completed.artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(completed_payload["lifecycle"], "completed");
        let stream = sink.live_stream.lock().unwrap();
        let body = std::fs::read_to_string(stream.as_ref().unwrap().body_path()).unwrap();
        let rows = body
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let event = rows.iter().find(|row| row["type"] == "event").unwrap();
        let boundary = rows
            .iter()
            .find(|row| row["type"] == "semantic_boundary")
            .unwrap();
        assert_eq!(event["payload"]["lifecycle"], "started");
        assert_eq!(boundary["label"], TURN_REASONING_LABEL);
        assert!(
            boundary["artifact_payload_blake3"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
        );
        assert!(boundary.get("payload").is_none());
    }

    #[tokio::test]
    async fn continuity_generation_tracks_input_and_provider_context_handoffs() {
        let labels = codex_context_labels();
        let projected = codex_client_projection_labels();
        let (emit, mut receiver) = ProviderOutputStream::channel();
        let output = tempfile::tempdir().unwrap();
        let provider_interface = InterfaceId::new("world_provider_codex");
        let live_stream_root = output.path().join("streams");
        let live_stream_rules = live_stream_rules();
        let sink = TurnSink {
            emit: &emit,
            output_root: output.path(),
            live_stream_root: &live_stream_root,
            provider_interface: &provider_interface,
            run: "run-a",
            source_movement_seq: 7,
            attempt: 1,
            context_output_labels: &labels,
            client_projection_output_labels: &projected,
            live_stream_rules: &live_stream_rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: emit.continuity.clone(),
        };
        sink.continuity.lock().unwrap().pending_input_context = true;
        sink.set_continuity(continuity(
            "world_provider_codex",
            "run-a",
            "thr-exact",
            3,
            None,
        ));
        let source = serde_json::json!({"id": "realtime.delegation:original", "content": vec![3u8; 32]});
        sink.continuity.lock().unwrap().continuity.as_mut().unwrap().turn_input = Some(serde_json::from_value(source.clone()).unwrap());
        sink.bind_live_stream("thr-exact", "turn-exact").unwrap();
        for _ in 0..3 {
            sink.send(CodexTurnOutput::client(
                AGENT_MESSAGE_DELTA_LABEL,
                serde_json::json!({"text": "before any handoff"}),
            ))
            .unwrap();
        }
        assert_eq!(sink.continuity_snapshot().unwrap().context_generation, 3);
        assert!(sink.continuity.lock().unwrap().pending_input_context);
        sink.send(CodexTurnOutput::client(
            HOOK_CATALOG_LABEL,
            serde_json::json!({"hooks": []}),
        ))
        .unwrap();
        sink.send(CodexTurnOutput::client(
            AGENT_MESSAGE_DELTA_LABEL,
            serde_json::json!({"text": "x"}),
        ))
        .unwrap();
        sink.send_terminal(CodexTurnOutput::final_message("thr-exact", "turn-exact", "final-exact", "done", None))
            .unwrap();
        assert_eq!(
            sink.continuity
                .lock()
                .unwrap()
                .continuity
                .as_ref()
                .unwrap()
                .context_generation,
            5
        );
        let ProviderStreamEmission::Turn(catalog) = receiver.try_recv().unwrap() else {
            panic!("expected one projected hook catalog")
        };
        assert_eq!(
            catalog.artifact.read_manifest().unwrap().label,
            HOOK_CATALOG_LABEL
        );
        let ProviderStreamEmission::TerminalGroup { stream, terminal } = receiver.try_recv().unwrap()
        else {
            panic!("expected one sealed stream and terminal output")
        };
        assert_eq!(
            stream.artifact.read_manifest().unwrap().label,
            "turn.stream"
        );
        let stream_payload = String::from_utf8(stream.artifact.read_payload_bytes().unwrap()).unwrap();
        let transient = stream_payload
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|row| row["type"] == "event" && row["label"] == AGENT_MESSAGE_DELTA_LABEL)
            .unwrap();
        assert!(transient["payload"].get("codex_continuity").is_none());
        assert_eq!(
            terminal.artifact.read_manifest().unwrap().label,
            INTERFACE_ARTIFACT_OUTPUT_LABEL
        );
        let terminal_payload: serde_json::Value =
            serde_json::from_slice(&terminal.artifact.read_payload_bytes().unwrap()).unwrap();
        for (key, expected) in [
            ("thread_id", "thr-exact"), ("turn_id", "turn-exact"),
            ("item_id", "final-exact"), ("phase", "final_answer"),
        ] {
            assert_eq!(terminal_payload[key], expected);
        }
        assert_eq!(transient["payload"]["in_reply_to"], source);
        assert_eq!(terminal_payload["in_reply_to"], source);
        let recovered: CodexContinuityState = serde_json::from_value(terminal_payload[CODEX_CONTINUITY_FIELD].clone()).unwrap();
        assert_eq!(serde_json::to_value(recovered.turn_input).unwrap(), source);
        assert!(sink.send(CodexTurnOutput::client(AGENT_MESSAGE_DELTA_LABEL,
            serde_json::json!({"text": "crossed", "in_reply_to": {"id": "another", "content": "d".repeat(64)}}))).is_err());
        let seal: serde_json::Value = serde_json::from_str(stream_payload.lines().last().unwrap()).unwrap();
        assert_eq!(seal["terminal_artifact_payload_blake3"], artifact_payload_blake3(&terminal.artifact).unwrap());
        assert_eq!(
            terminal_payload.pointer("/codex_continuity/context_generation"),
            Some(&serde_json::json!(5))
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        emit.continuity.lock().unwrap().pending_input_context = true;
        let warm_sink = TurnSink {
            source_movement_seq: 8,
            live_stream: std::sync::Mutex::new(None),
            continuity: emit.continuity.clone(),
            ..sink
        };
        let request = ClientRequest::TurnStart {
            request_id: octopus_appserver_proto::RequestId::Integer(1),
            params: TurnStartParams {
                thread_id: "thr-exact".to_string(),
                ..TurnStartParams::default()
            },
        };
        for accepted in [false, true] {
            let dispatch = warm_sink.admit_app_server_dispatch(&request, None);
            let receipt = async {
                let ProviderStreamEmission::TransportDispatch { artifact, admitted } =
                    receiver.recv().await.unwrap()
                else {
                    panic!("expected the next turn dispatch")
                };
                let payload: serde_json::Value =
                    serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
                assert_eq!(payload["method"], "turn/start");
                assert_eq!(payload["source_movement_seq"], 8);
                admitted
                    .send(if accepted {
                        Ok(())
                    } else {
                        Err("refused".into())
                    })
                    .unwrap();
            };
            let (result, ()) = tokio::join!(dispatch, receipt);
            assert_eq!(result.is_ok(), accepted);
            assert_eq!(
                warm_sink.continuity_snapshot().unwrap().context_generation,
                if accepted { 6 } else { 5 }
            );
            assert_eq!(
                warm_sink.continuity.lock().unwrap().pending_input_context,
                !accepted
            );
        }
        let next = warm_sink
            .materialize(CodexTurnOutput::final_message("thr-exact", "turn-next", "final-next", "next", None))
            .unwrap()
            .unwrap();
        let payload: serde_json::Value =
            serde_json::from_slice(&next.artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(payload[CODEX_CONTINUITY_FIELD]["context_generation"], 7);
        let (stopped, retained) = interrupted_continuity(emit.continuity_snapshot()).unwrap();
        assert!(!retained);
        let result = CodexTurnControlResult {
            control_id: "stop".into(), action: "stop".into(), acknowledged: true,
            thread_id: Some("thr-exact".into()), turn_id: Some("turn-exact".into()),
            detail: serde_json::json!({"interrupted":true}), continuity: stopped,
        };
        emit.record_control_continuity(&result);
        let ack = materialize_turn_control_result(output.path(), 9, result).unwrap();
        let terminal = warm_sink.materialize(CodexTurnOutput::client(TURN_COMPLETED_LABEL, serde_json::json!({"status":"interrupted"}))).unwrap().unwrap();
        for artifact in [&ack, &terminal.artifact] {
            let value: serde_json::Value = serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
            assert_eq!(value[CODEX_CONTINUITY_FIELD]["context_generation"], 8);
        }
        let recovered = emit.continuity_snapshot().unwrap();
        validate_recovered_thread_coordinates(&recovered, &recovered.thread_shape_fingerprint, 8, &recovered.static_context_fingerprint).unwrap();

    }

    #[test]
    fn terminal_is_not_emitted_when_live_stream_seal_fails() {
        let labels = codex_context_labels();
        let projected = codex_client_projection_labels();
        let rules = live_stream_rules();
        let (emit, mut receiver) = ProviderOutputStream::channel();
        let output = tempfile::tempdir().unwrap();
        let live_stream_root = output.path().join("streams");
        let provider_interface = InterfaceId::new("world_provider_codex");
        let sink = TurnSink {
            emit: &emit,
            output_root: output.path(),
            live_stream_root: &live_stream_root,
            provider_interface: &provider_interface,
            run: "run-a",
            source_movement_seq: 9,
            attempt: 1,
            context_output_labels: &labels,
            client_projection_output_labels: &projected,
            live_stream_rules: &rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: Arc::new(std::sync::Mutex::new(ContinuityEmitterState::default())),
        };
        sink.bind_live_stream("thread-a", "turn-a").unwrap();
        let manifest_path = sink
            .live_stream
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .manifest_path()
            .to_path_buf();
        std::fs::remove_file(&manifest_path).unwrap();
        std::fs::create_dir(&manifest_path).unwrap();

        let error = sink
            .send_terminal(CodexTurnOutput::final_message("thread-a", "turn-a", "final-a", "done", None))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("could not be atomically installed")
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    async fn next_test_rpc(
        socket: &mut tokio_tungstenite::WebSocketStream<crate::uds::UnixStream>,
    ) -> octopus_appserver_proto::JSONRPCMessage {
        use futures::StreamExt;

        let message = socket.next().await.unwrap().unwrap();
        serde_json::from_str(&message.into_text().unwrap()).unwrap()
    }

    async fn send_test_result(
        socket: &mut tokio_tungstenite::WebSocketStream<crate::uds::UnixStream>,
        id: RequestId,
        result: serde_json::Value,
    ) {
        use futures::SinkExt;

        let response = octopus_appserver_proto::JSONRPCMessage::Response(
            octopus_appserver_proto::JSONRPCResponse { id, result },
        );
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&response).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    async fn send_test_error(
        socket: &mut tokio_tungstenite::WebSocketStream<crate::uds::UnixStream>,
        id: RequestId,
        message: impl Into<String>,
    ) {
        use futures::SinkExt;

        let response =
            octopus_appserver_proto::JSONRPCMessage::Error(octopus_appserver_proto::JSONRPCError {
                id,
                error: octopus_appserver_proto::JSONRPCErrorError {
                    code: -32600,
                    message: message.into(),
                    data: None,
                },
            });
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&response).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    struct FixedSecretStore;

    impl ScopedSecretStore for FixedSecretStore {
        fn resolve(
            &self,
            exact_interface: &InterfaceId,
            handle: &str,
        ) -> Result<crate::auth::ScopedSecretValue, ScopedSecretError> {
            assert_eq!(exact_interface.as_str(), "world_provider_codex");
            assert_eq!(handle, "secret://world_provider_codex/OPENAI_API_KEY");
            Ok(crate::auth::ScopedSecretValue::from_test_value(
                "upstream-only-secret",
            ))
        }
    }

    #[tokio::test]
    async fn api_key_login_resolves_exact_secret_without_emitting_its_value() {
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("account-app-server.sock");
        let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
        let server = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let init = next_test_rpc(&mut socket).await;
            let octopus_appserver_proto::JSONRPCMessage::Request(init) = init else {
                panic!("expected initialize request")
            };
            send_test_result(
                &mut socket,
                init.id,
                serde_json::json!({"userAgent": "codex/test", "codexHome": "/tmp"}),
            )
            .await;
            assert!(matches!(
                next_test_rpc(&mut socket).await,
                octopus_appserver_proto::JSONRPCMessage::Notification(_)
            ));
            let login = next_test_rpc(&mut socket).await;
            let octopus_appserver_proto::JSONRPCMessage::Request(login) = login else {
                panic!("expected account/login/start request")
            };
            assert_eq!(login.method, "account/login/start");
            assert_eq!(login.params.as_ref().unwrap()["type"], "apiKey");
            assert_eq!(
                login.params.as_ref().unwrap()["apiKey"],
                "upstream-only-secret"
            );
            send_test_result(&mut socket, login.id, serde_json::json!({"type": "apiKey"})).await;
        });
        let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
            endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
                socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(
                    &socket_path,
                )
                .unwrap(),
            },
            client_name: "octopus-test".to_string(),
            client_version: "0".to_string(),
            experimental_api: false,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 8,
        })
        .await
        .unwrap();
        let config = InterfaceDriverConfig {
            private_config: serde_json::json!({
                "provider_operations": provider_operations_value(),
                "instruction_artifacts": [],
                "worker_instruction_artifacts": [],
                "context_output_labels": codex_context_labels(),
                "session_snapshot_labels": codex_snapshot_labels(),
                "client_projection_output_labels": codex_client_projection_labels(),
                "live_stream": live_stream_config_value(),
            })
            .as_object()
            .unwrap()
            .clone(),
            ..InterfaceDriverConfig::default()
        };
        let driver = CodexAppServerInterfaceDriver::new_with_config_and_secret_store(
            AppServerClient::Remote(client),
            Some(&config),
            Arc::new(FixedSecretStore),
        );
        let (output, mut output_rx) = ProviderOutputStream::channel();
        let labels = BTreeSet::new();
        let projected = codex_client_projection_labels();
        let output_root = temp.path().join("emitted");
        let live_stream_root = temp.path().join("streams");
        let live_stream_rules = live_stream_rules();
        let provider_interface = InterfaceId::new("world_provider_codex");
        let sink = TurnSink {
            emit: &output,
            output_root: &output_root,
            live_stream_root: &live_stream_root,
            provider_interface: &provider_interface,
            run: "run-a",
            source_movement_seq: 7,
            attempt: 1,
            context_output_labels: &labels,
            client_projection_output_labels: &projected,
            live_stream_rules: &live_stream_rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: Arc::new(std::sync::Mutex::new(ContinuityEmitterState::default())),
        };
        let operation = driver
            .handle_account_login_start(
                &provider_interface,
                br#"{"method":"api_key","secret_handle":"secret://world_provider_codex/OPENAI_API_KEY"}"#,
                &sink,
            );
        let receive = async {
            let ProviderStreamEmission::TransportDispatch { artifact, admitted } =
                output_rx.recv().await.unwrap()
            else {
                panic!("expected transport dispatch emission")
            };
            assert_eq!(
                artifact.read_manifest().unwrap().label,
                PROVIDER_TRANSPORT_DISPATCH_LABEL
            );
            let dispatch: serde_json::Value =
                serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
            assert_eq!(dispatch["provider_interface"], "world_provider_codex");
            assert_eq!(dispatch["method"], "account/login/start");
            assert_eq!(dispatch["source_movement_seq"], 7);
            assert_eq!(
                dispatch["credential_handle_fingerprint"]
                    .as_str()
                    .map(str::len),
                Some(64)
            );
            admitted.send(Ok(())).unwrap();
            let ProviderStreamEmission::Turn(emission) = output_rx.recv().await.unwrap() else {
                panic!("expected provider turn emission")
            };
            emission
        };
        let (result, emission) = tokio::join!(operation, receive);
        result.unwrap();
        assert_eq!(
            emission.artifact.read_manifest().unwrap().label,
            PROVIDER_ACCOUNT_LOGIN_STARTED_LABEL
        );
        let payload = emission.artifact.read_payload_bytes().unwrap();
        let payload = String::from_utf8(payload).unwrap();
        assert!(payload.contains("secret://world_provider_codex/OPENAI_API_KEY"));
        assert!(!payload.contains("upstream-only-secret"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cold_continuity_resumes_and_reads_the_exact_native_thread() {
        use octopus_appserver_proto::{
            ApprovalsReviewer, AskForApproval, SandboxPolicy, SessionSource, Thread, ThreadStatus,
        };

        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("app-server.sock");
        let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
        let cwd = octopus_util_abspath::AbsolutePathBuf::from_absolute_path(temp.path()).unwrap();
        let (method_tx, mut method_rx) = tokio::sync::mpsc::unbounded_channel();
        let thread = Thread {
            id: "thr-exact".to_string(),
            session_id: "thr-exact".to_string(),
            forked_from_id: None,
            parent_thread_id: None,
            preview: String::new(),
            ephemeral: false,
            model_provider: "openai".to_string(),
            created_at: 1,
            updated_at: 1,
            recency_at: Some(1),
            status: ThreadStatus::Idle,
            path: None,
            cwd: cwd.clone(),
            cli_version: "test".to_string(),
            source: SessionSource::Exec,
            thread_source: None,
            agent_nickname: None,
            agent_role: None,
            git_info: None,
            name: None,
            turns: Vec::new(),
        };
        let server = tokio::spawn({
            let cwd = cwd.clone();
            let thread = thread.clone();
            async move {
                use futures::StreamExt;

                let stream = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let init = next_test_rpc(&mut socket).await;
                let octopus_appserver_proto::JSONRPCMessage::Request(init) = init else {
                    panic!("expected initialize request")
                };
                assert_eq!(init.method, "initialize");
                send_test_result(
                    &mut socket,
                    init.id,
                    serde_json::json!({"userAgent": "codex/test", "codexHome": cwd}),
                )
                .await;
                assert!(matches!(
                    next_test_rpc(&mut socket).await,
                    octopus_appserver_proto::JSONRPCMessage::Notification(_)
                ));
                let resume = next_test_rpc(&mut socket).await;
                let octopus_appserver_proto::JSONRPCMessage::Request(resume) = resume else {
                    panic!("expected thread/resume request")
                };
                method_tx.send(resume.method.clone()).unwrap();
                assert_eq!(resume.method, "thread/resume");
                assert_eq!(resume.params.as_ref().unwrap()["threadId"], "thr-exact");
                assert_eq!(resume.params.as_ref().unwrap()["excludeTurns"], true);
                assert_eq!(
                    resume.params.as_ref().unwrap()["config"]["features.code_mode.direct_only_tool_namespaces"],
                    serde_json::json!([])
                );
                assert_eq!(resume.params.as_ref().unwrap()["config"]["web_search"], "live");
                assert_eq!(resume.params.as_ref().unwrap()["config"]["features.view_image"], false);
                assert_eq!(resume.params.as_ref().unwrap()["sandbox"], "read-only");
                assert_eq!(resume.params.as_ref().unwrap()["model"], "gpt-5.5");
                send_test_result(
                    &mut socket,
                    resume.id,
                    serde_json::to_value(ThreadResumeResponse {
                        thread: thread.clone(),
                        model: "gpt-5.5".to_string(),
                        model_provider: "openai".to_string(),
                        service_tier: None,
                        cwd: cwd.clone(),
                        runtime_workspace_roots: Vec::new(),
                        instruction_sources: Vec::new(),
                        approval_policy: AskForApproval::OnFailure,
                        approvals_reviewer: ApprovalsReviewer::User,
                        sandbox: SandboxPolicy::DangerFullAccess,
                        active_permission_profile: None,
                        reasoning_effort: None,
                        multi_agent_mode: None,
                        initial_turns_page: None,
                    })
                    .unwrap(),
                )
                .await;
                let read = next_test_rpc(&mut socket).await;
                let octopus_appserver_proto::JSONRPCMessage::Request(read) = read else {
                    panic!("expected thread/read request")
                };
                method_tx.send(read.method.clone()).unwrap();
                assert_eq!(read.method, "thread/read");
                assert_eq!(read.params.as_ref().unwrap()["threadId"], "thr-exact");
                assert!(read.params.as_ref().unwrap().get("includeTurns").is_none());
                send_test_result(
                    &mut socket,
                    read.id,
                    serde_json::to_value(ThreadReadResponse { thread }).unwrap(),
                )
                .await;
                while socket.next().await.is_some() {}
            }
        });
        let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
            endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
                socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(
                    &socket_path,
                )
                .unwrap(),
            },
            client_name: "octopus-test".to_string(),
            client_version: "0".to_string(),
            experimental_api: false,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 8,
        })
        .await
        .unwrap();
        let driver_config = InterfaceDriverConfig {
            private_config: serde_json::json!({
                "model": "gpt-5.5",
                "reasoning_summary": "none",
                "web_search": "live",
                "instruction_artifacts": [],
                "worker_instruction_artifacts": ["role-instruction"],
                "context_output_labels": codex_context_labels(),
                "session_snapshot_labels": codex_snapshot_labels(),
                "client_projection_output_labels": codex_client_projection_labels(),
                "live_stream": live_stream_config_value(),
                "native_tool_namespace": {
                    "name": "formed_interfaces",
                    "description": "Formed interface tools."
                },
                "model_result_projection": {
                    "max_tokens": 10000
                },
            })
            .as_object()
            .unwrap()
            .clone(),
            ..InterfaceDriverConfig::default()
        };
        let mut driver = CodexAppServerInterfaceDriver::new_with_config(
            AppServerClient::Remote(client),
            Some(&driver_config),
        );
        let mut web_policy = model_policy_input(serde_json::json!(["gpt-5.5"]));
        assert!(!driver.native_mcp_resource_discovery().unwrap());
        for enabled in [true, false] {
            driver.interface_private_config.insert("native_mcp_resource_discovery".into(), serde_json::json!(enabled));
            assert_eq!(driver.native_mcp_resource_discovery().unwrap(), enabled);
        }
        for value in [serde_json::Value::Null, serde_json::json!("true"), serde_json::json!({}), serde_json::json!(1)] {
            driver.interface_private_config.insert("native_mcp_resource_discovery".into(), value);
            assert!(driver.native_mcp_resource_discovery().is_err());
        }
        driver.interface_private_config.remove("native_mcp_resource_discovery");
        for modes in [serde_json::json!(["cached"]), serde_json::json!([]), serde_json::json!("live")] {
            web_policy.metadata.as_mut().unwrap().grant.interface_policy
                .get_mut("world_provider_codex").unwrap()
                .insert("web_search_modes".into(), modes);
            assert!(driver.enforce_native_web_search_policy(&web_policy).is_err());
        }
        assert_eq!(driver.native_tool_thread_config().unwrap()["web_search"], "live");
        assert_eq!(driver.native_tool_thread_config().unwrap()["features.view_image"], false);
        let result_bytes = serde_json::to_vec(&serde_json::json!({
            "label": "coder.exec.result",
            "tool": "coder.exec",
            "correlation_id": "call-1",
            "ok": false,
            "error": {
                "code": "file_not_found",
                "message": "AGENTS.md was not found"
            },
        }))
        .unwrap();
        let input_file = InterfaceArtifactFile::write(
            temp.path().join("input"),
            "coder.exec.result",
            &result_bytes,
        )
        .unwrap();
        let mut pending = continuity(
            "world_provider_codex",
            "run-a",
            "thr-exact",
            7,
            Some(CodexPendingContinuity::DynamicTool {
                native_operation: None,
                turn_id: "turn-1".to_string(),
                call_id: "call-1".to_string(),
                tool: "coder.exec".to_string(),
            }),
        );
        let native_affordances = namespace_native_affordances(vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
            name: "coder-dexec".into(), description: "execute".into(),
            input_schema: serde_json::json!({"type":"object"}), defer_loading: false,
        })], &NativeToolNamespace { name: "formed_interfaces".into(), description: "Formed interface tools.".into() }).unwrap();
        let role = bound("role-instruction", serde_json::json!({"label":"context.reference","text":"role"}));
        let mut static_bytes = Vec::new();
        append_static_context_source(&mut static_bytes, &role).unwrap();
        static_bytes.extend_from_slice(br#"{"coder-dexec":"coder.exec"}"#);
        pending.static_context_fingerprint = sha256_hex(&static_bytes);
        pending.thread_shape_fingerprint = thread_shape_fingerprint(
            Some("gpt-5.5"),
            "role",
            &native_affordances,
            &pending.static_context_fingerprint,
        )
        .unwrap();
        let mut runtime_grant = octopus_interfaces::GrantProjection::default();
        runtime_grant.tools.push("coder.exec".into());
        runtime_grant.interface_affordance_specs.push(octopus_ledger::InterfaceToolAffordance {
            name: "coder.exec".into(), description: "execute".into(), args_schema: serde_json::json!({"type":"object"}),
        });
        runtime_grant.interface_policy.insert(
            "world_provider_codex".to_string(),
            BTreeMap::from([
                ("models".to_string(), serde_json::json!(["gpt-5.5"])),
                ("web_search_modes".to_string(), serde_json::json!(["live"])),
            ]),
        );
        let mut runtime_metadata = octopus_interfaces::InvocationMetadata::new(runtime_grant)
            .with_runtime(RuntimeInvocationMetadata {
                run: octopus_interfaces::RunId::new("run-a"),
                ee: octopus_descriptors::EeId::new("ee-a"),
                worker: InterfaceId::new("system_worker_test"),
                checkpoint: octopus_descriptors::CheckpointId::new("checkpoint-a"),
                interface: InterfaceId::new("world_provider_codex"),
                epoch: 1,
                run_record_hash: None,
            });
        runtime_metadata.grant.role_artifacts = vec!["role-instruction".into()];
        runtime_metadata.artifact_binding_refs.push(role.artifact.clone());
        runtime_metadata.grant.artifact_access.insert("role-instruction".into(), serde_json::from_value(
            serde_json::json!({"read":true,"change":false,"create":false,"delete":false}),
        ).unwrap());
        let runtime = runtime_metadata.runtime.as_ref().unwrap();
        let mut pinned_record = octopus_run_record::RunRecord {
            id: runtime.run.clone(),
            ee: runtime.ee.clone(),
            worker: runtime.worker.clone(),
            trace: octopus_runtime::TraceId("trace:cold-continuity".to_string()),
            status: serde_json::json!("running"),
            interface_state: BTreeMap::new(),
            checkpoints: Vec::new(),
            active_checkpoints: Vec::new(),
        };
        let pinned_run_record = serde_json::to_vec(&pinned_record).unwrap();
        octopus_run_record::RunRecord::from_bytes(&pinned_run_record).unwrap();
        pinned_record.interface_state.insert(
            runtime.interface.clone(),
            BTreeMap::from([("context_generation".to_string(), serde_json::json!(7))]),
        );
        let catalog_bytes = serde_json::to_vec(&serde_json::json!({"limit": 1})).unwrap();
        let catalog_file = InterfaceArtifactFile::write(
            temp.path().join("catalog-input"),
            PROVIDER_MODELS_LIST_LABEL,
            &catalog_bytes,
        )
        .unwrap();
        let catalog_envelope = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("provider-models-list"),
                content: octopus_ledger::ContentHash::of_bytes(&catalog_bytes),
            },
            artifact_file: catalog_file,
            artifact_binding_payloads: vec![role.clone(), bound(
                "pending-catalog-tool-call",
                serde_json::json!({
                    "label": TOOL_CALL_LABEL,
                    CODEX_CONTINUITY_FIELD: pending.clone(),
                }),
            )],
            metadata: Some(runtime_metadata.clone()),
        };
        let descriptor: InterfaceDescriptor =
            serde_json::from_str(include_str!("../../../../interface.json")).unwrap();
        assert!(!is_provider_turn_input(&descriptor, &catalog_envelope).unwrap());
        let unsupported_bytes = b"{}";
        let unsupported = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("undeclared-provider-operation"),
                content: octopus_ledger::ContentHash::of_bytes(unsupported_bytes),
            },
            artifact_file: InterfaceArtifactFile::write(
                temp.path().join("undeclared-operation"), "provider.account.unknown", unsupported_bytes,
            ).unwrap(),
            artifact_binding_payloads: catalog_envelope.artifact_binding_payloads.clone(),
            metadata: catalog_envelope.metadata.clone(),
        };
        assert!(!is_provider_turn_input(&descriptor, &unsupported).unwrap(),
            "an undeclared operation must reach its canonical refusal without waiting for a tool result");
        let (catalog_output, _catalog_output_rx) = ProviderOutputStream::channel();
        let blocked_catalog = driver
            .receive_artifact_and_stream_outputs(CodexProviderInvocation {
                callable_tool_names: catalog_envelope.granted_tool_names().unwrap().into_iter().collect(),
                input: catalog_envelope,
                emit: catalog_output,
                resolved_session_snapshot: None,
                current_session_context: Arc::new(Vec::new()),
                output_root: &temp.path().join("catalog-output"),
                live_stream_root: &temp.path().join("catalog-streams"),
                source_movement_seq: 1,
                attempt: 1,
                admitted_run_record: &pinned_run_record,
                current_run_record: &pinned_run_record,
                deferred_context_generations: 0,
                input_changes_context: false,
                interrupted_after_artifact: None,
                recovered_handoff_artifacts: Arc::new(Vec::new()),
                durable_interrupted_continuity: None,
                reconciled_result_continuity: None,
                control: &CodexTurnControlBroker::new(Some(1)),
                handoff_barrier: ProviderHandoffBarrier::new(),
            })
            .await;
        assert!(matches!(
            &blocked_catalog,
            Err(InterfaceDriverError::FailedClosed(error))
                if error.contains("model catalog request cannot replace an unresolved app-server request")
        ), "{blocked_catalog:?}");
        assert!(method_rx.try_recv().is_err());
        let captured_inject = temp.path().join("inject-request.json");
        let rollout_path = temp.path().join("native-rollout.jsonl");
        std::fs::write(&rollout_path, format!("{}\n", serde_json::json!({
            "type":"response_item", "payload":{
                "type":"function_call", "call_id":"call-1", "name":"coder-dexec",
                "namespace":"formed_interfaces", "arguments":"{\"command\":\"pwd\"}"
            }
        }))).unwrap();
        let mut restored_thread = thread.clone();
        restored_thread.path = Some(rollout_path);
        let resume_response = serde_json::json!({"id":3, "result": ThreadResumeResponse {
            thread: restored_thread.clone(), model: "gpt-5.5".into(), model_provider: "openai".into(),
            service_tier: None, cwd: cwd.clone(), runtime_workspace_roots: Vec::new(),
            instruction_sources: Vec::new(), approval_policy: AskForApproval::OnFailure,
            approvals_reviewer: ApprovalsReviewer::User, sandbox: SandboxPolicy::DangerFullAccess,
            active_permission_profile: None, reasoning_effort: None, multi_agent_mode: None,
            initial_turns_page: None,
        }}).to_string();
        let read_response = serde_json::json!({"id":4, "result": ThreadReadResponse {
            thread: restored_thread.clone(),
        }}).to_string();
        let mut pending_thread = restored_thread;
        pending_thread.turns = vec![serde_json::from_value(serde_json::json!({
            "id":"turn-1","items":[],"itemsView":"full","status":"inProgress","error":null
        })).unwrap()];
        pending_thread.turns[0].items_view = TurnItemsView::NotLoaded;
        let first_turns = serde_json::json!({"id":2,"result":{"data":pending_thread.turns,"nextCursor":null,"backwardsCursor":null}}).to_string();
        pending_thread.turns[0].id = "turn-continued".into();
        let next_turns = serde_json::json!({"id":9,"result":{"data":pending_thread.turns,"nextCursor":null,"backwardsCursor":null}}).to_string();
        pending_thread.turns.clear();
        let terminal_probe = serde_json::json!({"id":1,"result":{"thread":pending_thread}}).to_string();
        let next_terminal_probe = serde_json::json!({"id":8,"result":{"thread":pending_thread}}).to_string();
        let script_path = temp.path().join("app-server.sh");
        std::fs::write(
            &script_path,
            r#"#!/bin/sh
IFS= read -r _
printf '%s\n' '{"id":"initialize","result":{"userAgent":"codex/test","codexHome":"/tmp"}}'
IFS= read -r _
IFS= read -r probe
case "$probe" in *'"method":"thread/read"'*) ;; *) exit 1;; esac
printf '%s\n' "$5"
IFS= read -r turns
case "$turns" in *'"method":"thread/turns/list"'*) ;; *) exit 1;; esac
printf '%s\n' "$7"
IFS= read -r _
printf '%s\n' "$3"
IFS= read -r _
printf '%s\n' "$4"
IFS= read -r inject
printf '%s\n' "$inject" > "$1"
printf '%s\n' '{"id":5,"result":{}}'
IFS= read -r turn
case "$turn" in *'"input":[]'*) ;; *) exit 1;; esac
printf '%s\n' '{"id":6,"result":{"turn":{"id":"turn-continued","items":[],"itemsView":"notLoaded","status":"inProgress","error":null,"startedAt":null,"completedAt":null,"durationMs":null}}}'
printf '%s\n' '{"method":"item/started","params":{"threadId":"thr-exact","turnId":"turn-continued","startedAtMs":1,"item":{"type":"webSearch","id":"web-1","query":"example","action":null}}}'
printf '%s\n' '{"method":"item/completed","params":{"threadId":"thr-exact","turnId":"turn-continued","completedAtMs":2,"item":{"type":"webSearch","id":"web-1","query":"example","action":{"type":"search","query":"example"},"results":[{"url":"https://example.com","title":"Example","snippet":"search evidence"}]}}}'
printf '%s\n' '{"id":"server-tool-1","method":"item/tool/call","params":{"threadId":"thr-exact","turnId":"turn-continued","callId":"call-2","namespace":"formed_interfaces","tool":"coder-dexec","arguments":{"command":"pwd"}}}'
printf '%s\n' '{"method":"item/completed","params":{"threadId":"thr-exact","turnId":"turn-continued","completedAtMs":3,"item":{"type":"agentMessage","id":"commentary-after-result","text":"Continuing","phase":"commentary"}}}'
printf '%s\n' '{"id":"server-tool-2","method":"item/tool/call","params":{"threadId":"thr-exact","turnId":"turn-continued","callId":"call-3","namespace":"formed_interfaces","tool":"coder-dexec","arguments":{"command":"ls"}}}'
IFS= read -r _
printf '%s\n' '{"id":7,"result":{"data":[]}}'
IFS= read -r probe
case "$probe" in *'"method":"thread/read"'*) ;; *) exit 1;; esac
printf '%s\n' "$6"
IFS= read -r turns
case "$turns" in *'"method":"thread/turns/list"'*) ;; *) exit 1;; esac
printf '%s\n' "$8"
IFS= read -r response
printf '%s\n' "$response" > "$2"
while IFS= read -r _; do :; done
"#,
        )
        .unwrap();
        let spawned = crate::SpawnAppServerClient::connect(crate::spawn::SpawnConfig {
            bin: "/bin/sh".to_string(),
            args: vec![
                script_path.display().to_string(),
                captured_inject.display().to_string(),
                temp.path()
                    .join("queued-native-response.json")
                    .display()
                    .to_string(),
                resume_response,
                read_response,
                terminal_probe,
                next_terminal_probe,
                first_turns,
                next_turns,
            ],
            cwd: Some(temp.path().to_path_buf()),
            codex_home: None,
            initialize_params: serde_json::from_value(serde_json::json!({
                "clientInfo": {"name": "octopus-test", "version": "0"},
                "capabilities": {},
            }))
            .unwrap(),
            compatibility: None,
            channel_capacity: 16,
        })
        .await
        .unwrap();
        let mut exact_driver_config = driver_config.clone();
        exact_driver_config.private_config.insert(
            "private_thread_cold_start".to_string(),
            serde_json::json!("resume_exact"),
        );
        let cold_driver = CodexAppServerInterfaceDriver::new_with_config(
            AppServerClient::Spawn(spawned),
            Some(&exact_driver_config),
        );
        let session_artifact: SessionContextArtifact = serde_json::from_value(session_artifact(
            "previous-message",
            "operator.message",
            serde_json::json!({"text": "earlier task"}),
            1,
        ))
        .unwrap();
        let pending_carrier = bound(
            "pending-tool-call",
            serde_json::json!({
                "label": "interface.artifact.output",
                CODEX_CONTINUITY_FIELD: pending.clone(),
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_codex",
                    "artifacts": [session_artifact.clone()],
                },
            }),
        );
        let resolved_session_snapshot = ProviderInvocationSessionSnapshot {
            carrier: pending_carrier.artifact.clone(),
            artifacts: Arc::new(vec![session_artifact]),
        };
        let document_bytes = b"# Architecture\n";
        let document = InterfaceArtifactFile::write(
            temp.path().join("document"), "architecture.md", document_bytes,
        ).unwrap();
        let document_ref = document.pin_manifest_into_blob(
            &mut octopus_ledger::InMemoryBlobs::new(),
        ).unwrap().artifact;
        let document = octopus_interfaces::ArtifactBindingPayload::new(
            document_ref.clone(), document_bytes.to_vec(),
        );
        let read_access = serde_json::json!({"read":true,"change":false,"create":false,"delete":false});
        let mut admitted_record = serde_json::to_value(&pinned_record).unwrap();
        admitted_record["interface_state"]["world_provider_codex"]["run_authoring_binding"] = serde_json::json!({
            "schema_version":2,"run":"run-a","worker":"system_worker_test",
            "provider_interface":"world_provider_codex","authoring_source_interface":"world_provider_codex",
            "payload_artifact_bindings":[document_ref],"checkpoints":[],
        });
        admitted_record["interface_state"]["world_provider_codex"]["context_generation"] = serde_json::json!(6);
        admitted_record["checkpoints"] = serde_json::json!([{
            "id":"checkpoint-a","run":"run-a","trace":"trace:cold-continuity","epoch":1,
            "interface_binding":octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new("system_coder")),
            "outbound_interface_bindings":[octopus_runtime::AuthoredInterfaceBinding::all(runtime.interface.clone())],
            "artifact_bindings":[{"id":document_ref.id,"access":read_access}],
            "required_verified_artifacts":[],
        }]);
        admitted_record["active_checkpoints"] = serde_json::json!(["checkpoint-a"]);
        let admitted_context_record = serde_json::to_vec(&admitted_record).unwrap();
        octopus_run_record::RunRecord::from_bytes(&admitted_context_record).unwrap();
        let mut current_record = admitted_record;
        current_record["interface_state"]["world_provider_codex"]["context_generation"] = serde_json::json!(7);
        current_record["checkpoints"] = serde_json::json!([]);
        current_record["active_checkpoints"] = serde_json::json!([]);
        let current_context_record = serde_json::to_vec(&current_record).unwrap();
        let mut result_metadata = runtime_metadata.clone();
        result_metadata.runtime.as_mut().unwrap().run_record_hash = Some(
            octopus_ledger::ContentHash::of_bytes(&admitted_context_record).to_hex(),
        );
        result_metadata.artifact_binding_refs = vec![pending_carrier.artifact.clone(), document_ref.clone(), role.artifact.clone()];
        result_metadata.grant.artifact_access.insert(
            document_ref.id.as_str().into(), serde_json::from_value(read_access).unwrap(),
        );
        let envelope = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("coder-exec-result"),
                content: octopus_ledger::ContentHash::of_bytes(&result_bytes),
            },
            artifact_file: input_file,
            artifact_binding_payloads: vec![pending_carrier, document, role.clone()],
            metadata: Some(result_metadata),
        };
        let (output, mut output_rx) = ProviderOutputStream::channel();
        let cold_output_root = temp.path().join("output");
        let cold_live_stream_root = temp.path().join("cold-streams");
        let cold_control = CodexTurnControlBroker::new(Some(1));
        let cold_pending =
            cold_driver.receive_artifact_and_stream_outputs(CodexProviderInvocation {
                callable_tool_names: envelope.granted_tool_names().unwrap().into_iter().collect(),
                input: envelope,
                emit: output,
                resolved_session_snapshot: Some(resolved_session_snapshot),
                current_session_context: Arc::new(Vec::new()),
                output_root: &cold_output_root,
                live_stream_root: &cold_live_stream_root,
                source_movement_seq: 1,
                attempt: 1,
                admitted_run_record: &admitted_context_record,
                current_run_record: &current_context_record,
                deferred_context_generations: 0,
                input_changes_context: true,
                interrupted_after_artifact: None,
                recovered_handoff_artifacts: Arc::new(Vec::new()),
                durable_interrupted_continuity: None,
                reconciled_result_continuity: None,
                control: &cold_control,
                handoff_barrier: ProviderHandoffBarrier::new(),
            });
        let collect = async {
            let mut emissions = Vec::new();
            while let Some(output) = output_rx.recv().await {
                match output {
                    ProviderStreamEmission::Turn(emission) | ProviderStreamEmission::Progress(emission) => emissions.push(emission),
                    ProviderStreamEmission::TransportDispatch { admitted, .. } => {
                        admitted.send(Ok(())).unwrap();
                    }
                    _ => panic!("unexpected cold-continuation output family"),
                }
            }
            emissions
        };
        let (cold_pending, emissions) = tokio::join!(cold_pending, collect);
        cold_pending.unwrap();
        let inject_request: serde_json::Value =
            serde_json::from_slice(&std::fs::read(captured_inject).unwrap()).unwrap();
        let injected_item = &inject_request["params"]["items"][0];
        assert_eq!(injected_item["call_id"], "call-1");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(injected_item["output"].as_str().unwrap())
                .unwrap()["error"]["code"],
            "file_not_found"
        );
        let search = emissions.iter().find(|emission| {
            emission.artifact.read_manifest().unwrap().label == TURN_WEB_SEARCH_LABEL
                && serde_json::from_slice::<serde_json::Value>(
                    &emission.artifact.read_payload_bytes().unwrap(),
                ).unwrap()["lifecycle"] == "completed"
        }).expect("native search must survive as a completed artifact before the next tool call");
        let search: serde_json::Value = serde_json::from_slice(
            &search.artifact.read_payload_bytes().unwrap(),
        ).unwrap();
        assert_eq!(search["results"][0]["url"], "https://example.com");
        let continued_call = emissions
            .iter()
            .find(|emission| emission.artifact.read_manifest().unwrap().label == TOOL_CALL_LABEL)
            .expect("the next model turn must emit its next tool-call artifact");
        let continued_call: serde_json::Value =
            serde_json::from_slice(&continued_call.artifact.read_payload_bytes().unwrap()).unwrap();
        assert_eq!(continued_call["correlation_id"], "call-2");
        let queue_scope = TransportStateScope::Run("run-a".to_string());
        let first_pending = cold_driver
            .pending_request(&queue_scope, "tool:call-2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            first_pending.request_id,
            RequestId::String("server-tool-1".to_string())
        );
        let recovered_queue: CodexContinuityState =
            serde_json::from_value(continued_call[CODEX_CONTINUITY_FIELD].clone()).unwrap();
        let queue_root = temp.path().join("queued-native-output");
        let queue_stream_root = temp.path().join("queued-native-streams");
        let context_labels = codex_context_labels();
        let projection_labels = codex_client_projection_labels();
        let stream_rules = live_stream_rules();
        let (queue_output, mut queue_output_rx) = ProviderOutputStream::channel();
        let queue_sink = TurnSink {
            emit: &queue_output,
            output_root: &queue_root,
            live_stream_root: &queue_stream_root,
            provider_interface: &recovered_queue.provider_interface,
            run: "run-a",
            source_movement_seq: 2,
            attempt: 1,
            context_output_labels: &context_labels,
            client_projection_output_labels: &projection_labels,
            live_stream_rules: &stream_rules,
            live_stream: std::sync::Mutex::new(None),
            continuity: Arc::new(std::sync::Mutex::new(ContinuityEmitterState {
                continuity: Some(recovered_queue.clone()),
                pending_input_context: false,
            })),
        };
        let source_artifact = bound(
            "queued-native-result",
            serde_json::json!({"output":"first"}),
        );
        let operations = BTreeMap::new();
        let barrier = ProviderHandoffBarrier::new();
        let queue_context = TurnContext {
            run_invocation: None,
            scope: &queue_scope,
            sink: &queue_sink,
            control: &cold_control,
            interface_operations: &operations,
            tool_names: &BTreeMap::from([("coder-dexec".into(), "coder.exec".into())]),
            source_artifact: &source_artifact.artifact,
            handoff_barrier: &barrier,
            effect_gate: &first_pending.effect_gate,
            turn_interrupted: &first_pending.turn_interrupted,
            lifecycle: None,
            lifecycle_prompt: None,
        };
        let first_result = ToolResultArtifact::success(
            "coder.exec",
            Some("call-2".to_string()),
            serde_json::json!({"output":"first"}),
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                cold_driver.resume_tool_result(
                    queue_context,
                    first_result.clone(),
                    Some(&recovered_queue),
                    recovered_queue.context_generation,
                    false
                )
            )
            .await
            .unwrap()
            .unwrap()
        );
        let second_pending = cold_driver
            .pending_request(&queue_scope, "tool:call-3")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            second_pending.request_id,
            RequestId::String("server-tool-2".to_string())
        );
        assert_eq!(second_pending.turn_id, "turn-continued");
        assert!(
            cold_driver
                .pending_request(&queue_scope, "tool:call-2")
                .await
                .is_err()
        );
        let mut second_call = None;
        let mut resolved_commentary = false;
        while let Ok(emission) = queue_output_rx.try_recv() {
            if let ProviderStreamEmission::Turn(emission) | ProviderStreamEmission::Progress(emission) = emission {
                if emission.artifact.read_manifest().unwrap().label == TURN_AGENT_MESSAGE_LABEL {
                    let value: serde_json::Value = serde_json::from_slice(&emission.artifact.read_payload_bytes().unwrap()).unwrap();
                    assert!(value[CODEX_CONTINUITY_FIELD].get("pending").is_none());
                    resolved_commentary = true;
                }
                if emission.artifact.read_manifest().unwrap().label == TOOL_CALL_LABEL {
                    assert!(second_call.is_none());
                    second_call = Some(
                        serde_json::from_slice::<serde_json::Value>(
                            &emission.artifact.read_payload_bytes().unwrap(),
                        )
                        .unwrap(),
                    );
                }
            }
        }
        assert!(resolved_commentary);
        let second_call =
            second_call.expect("the queued request must become its own tool artifact");
        assert_eq!(second_call["correlation_id"], "call-3");
        assert_eq!(second_call["arguments"]["command"], "ls");
        let second_continuity: CodexContinuityState =
            serde_json::from_value(second_call[CODEX_CONTINUITY_FIELD].clone()).unwrap();
        assert_eq!(
            second_continuity.pending,
            Some(CodexPendingContinuity::DynamicTool {
                native_operation: None,
                turn_id: "turn-continued".to_string(),
                call_id: "call-3".to_string(),
                tool: "coder.exec".to_string(),
            })
        );
        assert!(
            validate_recovered_tool_result(
                &first_result,
                second_continuity.pending.as_ref().unwrap()
            )
            .is_err()
        );
        let wire_response = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Ok(bytes) = std::fs::read(temp.path().join("queued-native-response.json")) {
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        break value;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(wire_response["id"], "server-tool-1");
        assert_eq!(wire_response["result"]["success"], true);

        let interrupted_pending = CodexPendingContinuity::DynamicTool {
            native_operation: None,
            turn_id: "turn-1".to_string(),
            call_id: "call-1".to_string(),
            tool: "coder.exec".to_string(),
        };
        let mut interrupted = continuity("world_provider_codex", "run-a", "thr-exact", 8, None);
        interrupted.interrupted_pending = Some(interrupted_pending.clone());
        let mut result_before_ack = interrupted.clone();
        result_before_ack.context_generation = 7;
        result_before_ack.pending = Some(interrupted_pending);
        result_before_ack.interrupted_pending = None;
        let interrupted_result_envelope = |root: &str| InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("coder-exec-interrupted-result"),
                content: octopus_ledger::ContentHash::of_bytes(&result_bytes),
            },
            artifact_file: InterfaceArtifactFile::write(
                temp.path().join(root),
                "coder.exec.result",
                &result_bytes,
            )
            .unwrap(),
            artifact_binding_payloads: vec![role.clone(), bound(
                &format!("result-before-interrupt-ack-{root}"),
                serde_json::json!({
                    "label": TOOL_CALL_LABEL,
                    CODEX_CONTINUITY_FIELD: result_before_ack.clone(),
                }),
            )],
            metadata: Some(runtime_metadata.clone()),
        };
        let queued_turn_bytes = serde_json::to_vec(&serde_json::json!({
            "text": "queued after stop",
            "message_id": "queued-message-1",
        }))
        .unwrap();
        let queued_turn_envelope = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("queued-turn-after-stop"),
                content: octopus_ledger::ContentHash::of_bytes(&queued_turn_bytes),
            },
            artifact_file: InterfaceArtifactFile::write(
                temp.path().join("queued-turn"),
                OPERATOR_MESSAGE_LABEL,
                &queued_turn_bytes,
            )
            .unwrap(),
            artifact_binding_payloads: vec![role.clone(), bound(
                "pending-result-before-queued-turn",
                serde_json::json!({
                    "label": TOOL_CALL_LABEL,
                    CODEX_CONTINUITY_FIELD: result_before_ack.clone(),
                }),
            )],
            metadata: Some(runtime_metadata.clone()),
        };
        let (queued_output, mut queued_output_rx) = ProviderOutputStream::channel();
        let queued = driver
            .receive_artifact_and_stream_outputs(CodexProviderInvocation {
                callable_tool_names: queued_turn_envelope.granted_tool_names().unwrap().into_iter().collect(),
                input: queued_turn_envelope,
                emit: queued_output,
                resolved_session_snapshot: None,
                current_session_context: Arc::new(Vec::new()),
                output_root: &temp.path().join("queued-turn-output"),
                live_stream_root: &temp.path().join("queued-turn-streams"),
                source_movement_seq: 2,
                attempt: 1,
                admitted_run_record: &pinned_run_record,
                current_run_record: &pinned_run_record,
                deferred_context_generations: 0,
                input_changes_context: true,
                interrupted_after_artifact: None,
                recovered_handoff_artifacts: Arc::new(Vec::new()),
                durable_interrupted_continuity: Some(interrupted.clone()),
                reconciled_result_continuity: None,
                control: &CodexTurnControlBroker::new(Some(2)),
                handoff_barrier: ProviderHandoffBarrier::new(),
            })
            .await;
        assert!(matches!(
            queued,
            Err(InterfaceDriverError::FailedClosed(error))
                if error.contains("cannot replace a durably interrupted exact effect")
        ));
        assert!(queued_output_rx.recv().await.is_none());

        let (interrupted_output, mut interrupted_output_rx) = ProviderOutputStream::channel();
        driver
            .receive_artifact_and_stream_outputs(CodexProviderInvocation {
                callable_tool_names: runtime_metadata.grant.tools.iter().cloned().collect(),
                input: interrupted_result_envelope("interrupted-output"),
                emit: interrupted_output,
                resolved_session_snapshot: None,
                current_session_context: Arc::new(Vec::new()),
                output_root: &temp.path().join("interrupted-emitted"),
                live_stream_root: &temp.path().join("interrupted-streams"),
                source_movement_seq: 3,
                attempt: 1,
                admitted_run_record: &pinned_run_record,
                current_run_record: &pinned_run_record,
                deferred_context_generations: 0,
                input_changes_context: true,
                interrupted_after_artifact: None,
                recovered_handoff_artifacts: Arc::new(Vec::new()),
                durable_interrupted_continuity: Some(interrupted.clone()),
                reconciled_result_continuity: None,
                control: &CodexTurnControlBroker::new(Some(3)),
                handoff_barrier: ProviderHandoffBarrier::new(),
            })
            .await
            .unwrap();
        let ProviderStreamEmission::InterruptedResult {
            activity: None,
            stream: Some(interrupted_stream),
            closure: interrupted_closure,
            ..
        } = interrupted_output_rx.recv().await.unwrap()
        else {
            panic!("expected one exact interrupted-result emission group")
        };
        assert_eq!(
            interrupted_stream.artifact.read_manifest().unwrap().label,
            "turn.stream"
        );
        let interrupted_stream_payload =
            String::from_utf8(interrupted_stream.artifact.read_payload_bytes().unwrap()).unwrap();
        assert!(interrupted_stream_payload.contains(TURN_DYNAMIC_TOOL_CALL_LABEL));
        assert!(interrupted_stream_payload.contains("durably_admitted_after_interrupt"));
        let interrupted_closure_bytes = interrupted_closure.artifact.read_payload_bytes().unwrap();
        let interrupted_closure_payload: serde_json::Value =
            serde_json::from_slice(&interrupted_closure_bytes).unwrap();
        assert_eq!(
            interrupted_closure.artifact.read_manifest().unwrap().label,
            TURN_INTERRUPTED_RESULT_LABEL
        );
        assert_eq!(
            interrupted_closure_payload["result_disposition"],
            "durably_admitted_after_interrupt"
        );
        assert!(
            interrupted_closure_payload
                .pointer("/codex_continuity/interrupted_pending")
                .is_none()
        );
        assert_eq!(
            interrupted_closure_payload.pointer("/codex_continuity/context_generation"),
            Some(&serde_json::json!(interrupted.context_generation + 2))
        );
        assert!(interrupted_output_rx.recv().await.is_none());

        let (recovered_output, mut recovered_output_rx) = ProviderOutputStream::channel();
        driver
            .receive_artifact_and_stream_outputs(CodexProviderInvocation {
                callable_tool_names: runtime_metadata.grant.tools.iter().cloned().collect(),
                input: interrupted_result_envelope("interrupted-recovery-output"),
                emit: recovered_output,
                resolved_session_snapshot: None,
                current_session_context: Arc::new(Vec::new()),
                output_root: &temp.path().join("interrupted-recovery-emitted"),
                live_stream_root: &temp.path().join("interrupted-streams"),
                source_movement_seq: 4,
                attempt: 2,
                admitted_run_record: &pinned_run_record,
                current_run_record: &pinned_run_record,
                deferred_context_generations: 0,
                input_changes_context: true,
                interrupted_after_artifact: Some(TURN_DYNAMIC_TOOL_CALL_LABEL.to_string()),
                recovered_handoff_artifacts: Arc::new(Vec::new()),
                durable_interrupted_continuity: Some(interrupted.clone()),
                reconciled_result_continuity: None,
                control: &CodexTurnControlBroker::new(Some(4)),
                handoff_barrier: ProviderHandoffBarrier::new(),
            })
            .await
            .unwrap();
        let ProviderStreamEmission::InterruptedResult {
            activity: None,
            stream: Some(recovered_stream),
            closure: recovered_closure,
            ..
        } = recovered_output_rx.recv().await.unwrap()
        else {
            panic!("expected resumed interrupted-result closure without duplicate activity")
        };
        assert_eq!(
            recovered_closure.artifact.read_manifest().unwrap().label,
            TURN_INTERRUPTED_RESULT_LABEL
        );
        assert_eq!(
            recovered_closure.artifact.read_payload_bytes().unwrap(),
            interrupted_closure_bytes
        );
        assert_eq!(
            recovered_stream.artifact.read_payload_bytes().unwrap(),
            interrupted_stream.artifact.read_payload_bytes().unwrap()
        );
        assert!(recovered_output_rx.recv().await.is_none());
        assert!(method_rx.try_recv().is_err());

        let static_context = "b".repeat(64);
        let fingerprint =
            thread_shape_fingerprint(Some("gpt-5.5"), "role", &[], &static_context).unwrap();
        let mut recovered = continuity("world_provider_codex", "run-a", "thr-exact", 7, None);
        recovered.thread_shape_fingerprint = fingerprint;
        let acquired = driver
            .acquire_transport_thread(
                &TransportStateScope::Run("run-a".to_string()),
                ThreadAcquisition {
                    recovered: Some(&recovered),
                    retired_history: None,
                    require_exact_resume: false,
                    context_generation: 8,
                    input_changes_context: true,
                    model: Some("gpt-5.5".to_string()),
                    bound_instructions: "role",
                    native_affordances: &[],
                    static_context_fingerprint: &static_context,
                    session_context_available: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(acquired, ("thr-exact".to_string(), false, true));
        assert_eq!(method_rx.recv().await.as_deref(), Some("thread/resume"));
        assert_eq!(method_rx.recv().await.as_deref(), Some("thread/read"));
        assert!(method_rx.try_recv().is_err());
        let mut regressed = recovered.clone();
        regressed.context_generation = 6;
        let rejected = driver
            .acquire_transport_thread(
                &TransportStateScope::Run("run-a".to_string()),
                ThreadAcquisition {
                    recovered: Some(&regressed),
                    retired_history: None,
                    require_exact_resume: false,
                    context_generation: 7,
                    input_changes_context: true,
                    model: Some("gpt-5.5".to_string()),
                    bound_instructions: "role",
                    native_affordances: &[],
                    static_context_fingerprint: &static_context,
                    session_context_available: false,
                },
            )
            .await;
        assert!(matches!(
            rejected,
            Err(InterfaceDriverError::FailedClosed(error))
                if error.contains("regressed behind the live transport context generation")
        ));
        assert!(method_rx.try_recv().is_err());
        driver.shutdown().await.unwrap();
        server.await.unwrap();
    }

    #[test]
    fn assemble_context_splits_role_from_lenta_by_label() {
        let payloads = vec![
            bound(
                "artifact-worker-role-template",
                serde_json::json!({"label":"context.reference","id":"role","text":"You are the coder worker."}),
            ),
            bound(
                "artifact-grounding-system-architecture",
                serde_json::json!({"label":"context.reference","id":"grounding","text":"How Octopus works."}),
            ),
            bound(
                "inbound-operator-message",
                serde_json::json!({"label":"operator.message","text":"earlier: fix the build"}),
            ),
        ];
        let assembled =
            assemble_context(&payloads, "world_provider_test", &codex_snapshot_labels())
                .expect("mixed context assembles, never fails closed");
        assert_eq!(
            assembled.role,
            vec![
                "You are the coder worker.".to_string(),
                "How Octopus works.".to_string()
            ],
            "context.reference artifacts become the role, in order"
        );
        assert_eq!(
            assembled.conversation,
            vec!["User\nearlier: fix the build".to_string()],
            "a non-context label is lenta, folded into the conversation, not the role"
        );
    }

    #[test]
    fn assemble_context_retains_a_json_artifact_without_text() {
        let payloads = vec![
            bound(
                "role",
                serde_json::json!({"label":"context.reference","text":"role text"}),
            ),
            bound(
                "lenta",
                serde_json::json!({"label":"operator.message","payload_path":"/tmp/x"}),
            ),
        ];
        let assembled =
            assemble_context(&payloads, "world_provider_test", &codex_snapshot_labels())
                .expect("textless lenta is tolerated");
        assert_eq!(assembled.role, vec!["role text".to_string()]);
        assert_eq!(assembled.conversation.len(), 1);
        assert!(assembled.conversation[0].starts_with("artifact lenta\n"));
        assert!(assembled.conversation[0].contains("\"payload_path\":\"/tmp/x\""));
    }

    #[test]
    fn session_snapshot_growth_preserves_the_static_context_fingerprint() {
        let role = bound(
            "role",
            serde_json::json!({"label":"context.reference","text":"role text"}),
        );
        let queued = assemble_context(
            &[
                role.clone(),
                bound("provider.peer.input:queued", serde_json::json!({"label":"provider.peer.input","message":{"action":"queue","text":"next assignment context"}})),
                bound("interface.context.updated:receipt", serde_json::json!({"label":"interface.context.updated","queued":true,"turn_triggered":false})),
            ],
            "world_provider_test",
            &codex_snapshot_labels(),
        ).unwrap();
        let first = assemble_context(
            &[
                role.clone(),
                bound(
                    "session-a",
                    serde_json::json!({
                        "label": "interface.artifact.output",
                        "text": "TWO",
                        "session_context": {
                            "schema_version": 3,
                            "provider_interface": "world_provider_test",
                            "artifacts": [session_artifact(
                                "operator.message:one",
                                "operator.message",
                                serde_json::json!({"text":"ONE"}),
                                1
                            )]
                        }
                    }),
                ),
            ],
            "world_provider_test",
            &codex_snapshot_labels(),
        )
        .expect("first snapshot assembles");
        let second = assemble_context(
            &[
                role,
                bound(
                    "session-b",
                    serde_json::json!({
                        "label": "interface.artifact.output",
                        "text": "THREE",
                        "session_context": {
                            "schema_version": 3,
                            "provider_interface": "world_provider_test",
                            "artifacts": [
                                session_artifact(
                                    "operator.message:one",
                                    "operator.message",
                                    serde_json::json!({"text":"ONE"}),
                                    1
                                ),
                                session_artifact(
                                    "interface.artifact.output:two",
                                    "interface.artifact.output",
                                    serde_json::json!({"text":"TWO"}),
                                    2
                                )
                            ]
                        }
                    }),
                ),
            ],
            "world_provider_test",
            &codex_snapshot_labels(),
        )
        .expect("grown snapshot assembles");
        assert_eq!(
            sha256_hex(&first.static_context_bytes),
            sha256_hex(&second.static_context_bytes)
        );
        assert_eq!(queued.static_context_bytes, first.static_context_bytes);
        assert_eq!(queued.conversation.len(), 2);
        assert_eq!(first.conversation, vec!["User\nONE", "Assistant\nTWO"]);
        assert_eq!(second.conversation, vec!["User\nONE", "Assistant\nTWO", "Assistant\nTHREE"]);
        assert!(
            first
                .conversation
                .iter()
                .all(|entry| !entry.contains(SESSION_CONTEXT_FIELD))
        );
    }

    #[test]
    fn native_compaction_is_same_turn_activity() {
        assert!(
            codex_client_projection_labels().contains(TURN_CONTEXT_COMPACTION_LABEL),
            "native compaction must cross the exact client projection allowlist"
        );
        assert!(codex_context_labels().contains(TURN_CONTEXT_COMPACTION_LABEL));
        let output = rich_item_output(ThreadItem::ContextCompaction {
            id: "compact-one".to_string(),
        })
        .expect("context compaction is projected");
        assert_eq!(output.label, TURN_CONTEXT_COMPACTION_LABEL);
        assert_eq!(output.value.get("run_boundary"), None);

        let state = continuity("world_provider_test", "run-one", "thread-one", 3, None);
        let encoded = serde_json::to_value(state).expect("continuity encodes");
        assert_eq!(encoded.get("run_boundary"), None);

        let manual = manual_compaction_completed_output(
            "compact-manual".to_string(),
            "thread-one",
            "turn-one",
            7,
        )
        .expect("manual compaction projects");
        assert_eq!(manual.value["source"], "manual");
        assert_eq!(manual.value["automatic"], false);
        assert_eq!(manual.value["thread_id"], "thread-one");
        assert_eq!(manual.value["turn_id"], "turn-one");
    }

    #[test]
    fn auth_recovery_is_turn_scoped_client_activity() {
        assert!(codex_client_projection_labels().contains(TURN_AUTH_RECOVERY_STARTED_LABEL));
        assert!(codex_client_projection_labels().contains(TURN_AUTH_RECOVERY_COMPLETED_LABEL));
        let recovery = octopus_appserver_proto::AuthRecoveryNotification {
            thread_id: "thread-one".to_string(),
            turn_id: "turn-one".to_string(),
            provider: "example".to_string(),
            message: "Authentication recovered".to_string(),
        };
        assert!(
            server_notification_matches_turn(
                &ServerNotification::AuthRecoveryStarted(recovery.clone()),
                "thread-one",
                "turn-one",
            )
            .unwrap()
        );
        assert!(
            !server_notification_matches_turn(
                &ServerNotification::AuthRecoveryCompleted(recovery),
                "thread-one",
                "turn-two",
            )
            .unwrap()
        );
    }

    #[test]
    fn session_snapshots_are_provider_scoped_and_globally_source_unique() {
        let source = session_artifact(
            "operator.message:one",
            "operator.message",
            serde_json::json!({"text":"ONE"}),
            1,
        );
        let snapshot = |id: &str, provider_interface: &str| {
            bound(
                id,
                serde_json::json!({
                    "label": "interface.artifact.output",
                    "text": "TWO",
                    "session_context": {
                        "schema_version": 3,
                        "provider_interface": provider_interface,
                        "artifacts": [source.clone()]
                    }
                }),
            )
        };

        let foreign = bound(
            "session-wrong-provider",
            serde_json::json!({
                "label": "interface.artifact.output",
                "text": "TWO",
                CODEX_CONTINUITY_FIELD: continuity(
                    "world_provider_other",
                    "run-other",
                    "thread-other",
                    4,
                    None,
                ),
                "context_window_usage": {
                    "used_tokens": 900,
                    "context_window": 1000
                },
                "session_context": {
                    "schema_version": 3,
                    "provider_interface": "world_provider_other",
                    "artifacts": [source.clone()]
                }
            }),
        );
        let assembled = assemble_context(
            std::slice::from_ref(&foreign),
            "world_provider_test",
            &codex_snapshot_labels(),
        )
        .expect("a foreign terminal is a fresh-run seed, not foreign provider continuity");
        assert_eq!(assembled.conversation, vec!["Assistant\nTWO"]);
        assert!(!assembled.session_context_available);
        let received = session_artifact(
            "received-foreign-terminal",
            "interface.artifact.output",
            serde_json::from_slice(foreign.read_bytes().unwrap().as_ref()).unwrap(),
            8,
        );
        let local = continuity("world_provider_test", "run-test", "local-thread", 5, None);
        let local_snapshot = bound("local-snapshot", serde_json::json!({
            "label": "interface.artifact.output", "text": "LOCAL",
            CODEX_CONTINUITY_FIELD: local,
            "session_context": {
                "schema_version": 3, "provider_interface": "world_provider_test",
                "artifacts": [received]
            }
        }));
        let local_assembled = assemble_context(std::slice::from_ref(&local_snapshot), "world_provider_test", &codex_snapshot_labels()).unwrap();
        assert_eq!(local_assembled.conversation, vec!["Assistant\nTWO", "Assistant\nLOCAL"]);
        assert_eq!(recover_codex_continuity(
            &[local_snapshot], &InterfaceId::new("world_provider_test"), "run-test",
            &codex_context_labels(), &codex_snapshot_labels(),
        ).unwrap(), Some(local));
        assert_eq!(
            recover_codex_continuity(
                &[foreign],
                &InterfaceId::new("world_provider_test"),
                "run-test",
                &codex_context_labels(),
                &codex_snapshot_labels(),
            )
            .expect("foreign private continuity is ignored"),
            None
        );
        assert!(matches!(
            assemble_context(
                &[
                    snapshot("session-a", "world_provider_test"),
                    snapshot("session-b", "world_provider_test"),
                ],
                "world_provider_test",
                &codex_snapshot_labels(),
            ),
            Err(InterfaceDriverError::FailedClosed(_))
        ));
    }

    #[test]
    fn assemble_context_still_requires_text_on_a_role_artifact() {
        let payloads = vec![bound(
            "role",
            serde_json::json!({"label":"context.reference","id":"role"}),
        )];
        assert!(
            matches!(
                assemble_context(&payloads, "world_provider_test", &codex_snapshot_labels(),),
                Err(InterfaceDriverError::Protocol(_))
            ),
            "role authority missing its text is a fail-closed protocol error"
        );
    }

    #[test]
    fn isolated_thread_start_params_suppress_environment_gated_native_tools() {
        let params = isolated_thread_start_params(
            Some("gpt-5.5".to_string()),
            "You are the flash worker.".to_string(),
            Vec::new(),
        );

        match &params.environments {
            Some(environments) => assert!(
                environments.is_empty(),
                "environments must be EMPTY to disable codex's environment-gated native tools"
            ),
            None => panic!(
                "environments must be Some(vec![]) for environment-gated tools, never \
                 None — None re-admits codex's default environment and its native tools"
            ),
        }

        assert_eq!(
            params.sandbox,
            Some(SandboxMode::ReadOnly),
            "the codex sandbox must be pinned read-only (no exec side effects outside the gate)"
        );

        assert_eq!(
            params.developer_instructions.as_deref(),
            Some("You are the flash worker.")
        );
        assert!(params.base_instructions.is_none());
    }

    #[test]
    fn model_names_preserve_exact_targets_and_reject_collisions() {
        let mut specs = ["workspace.read__a", "workspace.read__b"].map(|name| {
            DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: encode_tool_name(name), description: "Read".into(),
                input_schema: serde_json::json!({"type":"object"}), defer_loading: false,
            })
        });
        let mut config = serde_json::json!({"model_tool_names": {
            "workspace.read__a":"read_primary", "workspace.read__b":"read_secondary"
        }}).as_object().unwrap().clone();
        let original = specs.clone();
        let names = apply_model_tool_names(&mut specs, &config).unwrap();
        assert_eq!(names["read_primary"], "workspace.read__a");
        assert_eq!(names["read_secondary"], "workspace.read__b");
        assert!(!names.contains_key("workspace-dread__a"));
        config["model_tool_names"]["workspace.read__b"] = serde_json::json!("read_primary");
        assert!(apply_model_tool_names(&mut original.clone(), &config).is_err());
    }

    #[test]
    fn cold_history_keeps_model_decisions_without_private_transport_context() {
        let mut assembled = AssembledContext::default();
        for (label, payload) in [
            (TURN_REASONING_LABEL, serde_json::json!({"summary":["The first approach failed; use the alternative."],"codex_continuity":{"thread_id":"private"}})),
            (TURN_AGENT_MESSAGE_LABEL, serde_json::json!({"text":"I will check the remaining dependency."})),
            (TURN_PLAN_UPDATE_LABEL, serde_json::json!({"text":"Next: verify the result."})),
            ("tool.result", serde_json::json!({"ok":false,"error":{"message":"missing file"},"codex_continuity":{"thread_id":"private"},"provider_token_usage":{"total":9000}})),
        ] {
            let source = bound(label, payload.clone());
            append_assembled_context(&mut assembled, Some(label), &payload, label, &source.artifact, ModelResultProjection { max_tokens: 10000 }).unwrap();
        }
        let history = assembled.conversation.join("\n");
        for expected in ["alternative", "Assistant\nI will", "Assistant plan\nNext", "missing file"] { assert!(history.contains(expected)); }
        for excluded in ["private", "codex_continuity", "provider_token_usage"] { assert!(!history.contains(excluded)); }
    }

    #[test]
    fn interface_affordances_enter_the_native_dynamic_tools_field() {
        let native_affordances = vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
            name: "artifact.read".into(),
            description: "read a bound artifact through the context interface".into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            defer_loading: false,
        })];
        let params = isolated_thread_start_params(
            Some("gpt-5.5".into()),
            "You are flash.".into(),
            native_affordances.clone(),
        );
        let presented = params
            .dynamic_tools
            .as_ref()
            .expect("interface affordances must be presented in the native dynamic_tools field");
        assert_eq!(
            presented.len(),
            1,
            "exactly the one projected native affordance is presented"
        );
        match &presented[0] {
            DynamicToolSpec::Function(f) => assert_eq!(f.name, "artifact.read"),
            other => panic!("expected a native Function affordance spec, got {other:?}"),
        }
        assert_eq!(
            params.environments.as_deref(),
            Some(&[][..]),
            "codex's own natives stay OFF even while OUR affordances are presented (layer-2b)"
        );

        let none = isolated_thread_start_params(Some("gpt-5.5".into()), "x".into(), Vec::new());
        assert!(
            none.dynamic_tools.is_none(),
            "an empty affordance set presents NO native tool list (the model can only reply directly)"
        );
    }

    #[test]
    fn native_service_tiers_preserve_exact_catalog_and_wire_values() {
        let mut model: octopus_appserver_proto::Model = serde_json::from_value(serde_json::json!({
            "id":"declared-model", "model":"declared-model", "displayName":"Declared model",
            "description":"", "hidden":false, "supportedReasoningEfforts":[],
            "defaultReasoningEffort":"high", "isDefault":false,
            "serviceTiers":[
                {"id":"priority","name":"Fast","description":"Declared Fast"},
                {"id":"ultrafast","name":"Ultra Fast","description":"Declared Ultra Fast"},
                {"id":"custom-express","name":"Express","description":"Declared provider tier"}
            ]
        })).unwrap();
        for tier in ["default", "priority", "ultrafast", "custom-express"] {
            validate_model_service_tier(&model, tier).unwrap();
            let params = fresh_turn_start_params(
                "thread-1".into(), Vec::new(), model.model.clone(), Some(ReasoningEffort::XHigh),
                Some(Some(tier.into())), ReasoningSummary::Detailed, BTreeMap::new(), TURN_TRIGGER_INPUT,
            );
            let wire = serde_json::to_value(params).unwrap();
            assert_eq!(wire["model"], model.model);
            assert_eq!(wire["serviceTier"], tier);
            validate_native_service_tier_response(Some(tier), Some(tier)).unwrap();
            assert!(validate_native_service_tier_response(Some(tier), None).is_err());
        }
        for undeclared in ["ultra_fast", "turbo", "fast"] {
            assert!(validate_model_service_tier(&model, undeclared).is_err());
        }
        model.service_tiers.retain(|tier| tier.id == "priority");
        model.additional_speed_tiers = vec!["ultrafast".into()];
        assert!(validate_model_service_tier(&model, "ultrafast").is_err());
        validate_model_service_tier(&model, "priority").unwrap();
        assert!(validate_native_service_tier_response(Some("ultrafast"), Some("priority")).is_err());
        validate_native_service_tier_response(None, Some("priority")).unwrap();
    }

    #[test]
    fn fresh_turn_start_params_carries_bound_reasoning_effort() {
        let params = fresh_turn_start_params(
            "thread-1".to_string(),
            vec![UserInput::Text {
                text: "do the work".to_string(),
                text_elements: Vec::new(),
            }],
            "gpt-5.5".to_string(),
            Some(ReasoningEffort::XHigh),
            None,
            ReasoningSummary::Detailed,
            native_turn_context(Some("context".repeat(200_000)), Some("grant".into())),
            TURN_TRIGGER_INPUT,
        );

        assert_eq!(params.input.len(), 1);
        assert_eq!(params.additional_context.as_ref().unwrap()["octopus_bound_context"].value.len(), 1_400_000);
        let continuation = LifecycleContinuation::StartTurn {
            turn_input: serde_json::to_value(&params.input).unwrap(),
            additional_context: params.additional_context.clone().unwrap().into_iter().collect(),
            source_artifact: ArtifactRef { id: octopus_descriptors::ArtifactId::new("input"), content: octopus_ledger::ContentHash::of_bytes(b"input") },
        };
        assert_eq!(serde_json::from_value::<LifecycleContinuation>(serde_json::to_value(&continuation).unwrap()).unwrap(), continuation);
        assert_eq!(params.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(params.effort, Some(ReasoningEffort::XHigh));
        assert_eq!(params.summary, Some(ReasoningSummary::Detailed));
        assert_eq!(params.turn_trigger.as_deref(), Some(TURN_TRIGGER_INPUT));
        assert_eq!(
            serde_json::to_value(&params)
                .expect("turn params serialize")
                .get("effort")
                .and_then(serde_json::Value::as_str),
            Some("xhigh")
        );

        let resumed = fresh_turn_start_params(
            "thread-1".to_string(),
            Vec::new(),
            "gpt-5.5".to_string(),
            Some(ReasoningEffort::XHigh),
            None,
            ReasoningSummary::Detailed,
            BTreeMap::new(),
            TURN_TRIGGER_RESUME_INTERRUPTED_TASK,
        );
        assert!(resumed.input.is_empty());
        assert_eq!(
            resumed.turn_trigger.as_deref(),
            Some(TURN_TRIGGER_RESUME_INTERRUPTED_TASK)
        );
    }

    #[test]
    fn explicit_resume_is_an_empty_provider_turn_and_never_replays_a_prompt() {
        let input = decode_provider_input(
            br#"{"action":"resume","control_id":"control-resume"}"#,
            OPERATOR_TURN_CONTROL_LABEL,
            None,
            &BTreeMap::new(),
            None,
            &serde_json::Map::new(),
        )
        .unwrap();
        assert!(matches!(
            input,
            ProviderInput::Resume { control_id } if control_id == "control-resume"
        ));
        assert!(
            decode_provider_input(
                br#"{"action":"stop","control_id":"control-stop"}"#,
                OPERATOR_TURN_CONTROL_LABEL,
                None,
                &BTreeMap::new(),
                None,
                &serde_json::Map::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn operator_client_context_precedes_the_operator_text_and_fails_closed() {
        let decode = |payload: serde_json::Value| decode_provider_input(
            &serde_json::to_vec(&payload).unwrap(), OPERATOR_MESSAGE_LABEL, None, &BTreeMap::new(), None, &serde_json::Map::new());
        let ProviderInput::Turn(turn) = decode(serde_json::json!({"text":"What next?","message_id":"m","attachments":[],
            "client_context":[{"name":"home_state","text":"Tasks: none active."}]})).unwrap() else { panic!("operator turn expected") };
        let texts = turn.iter().map(|item| match item { UserInput::Text { text, .. } => text.as_str(), _ => "" }).collect::<Vec<_>>();
        assert_eq!(texts, ["<client_context name=\"home_state\">\nTasks: none active.\n</client_context>", "What next?"]);
        for context in [serde_json::json!([]), serde_json::json!([{"name":"Home","text":"x"}]), serde_json::json!([{"name":"a","text":"x","extra":1}]),
            serde_json::json!([{"name":"a","text":"x"},{"name":"a","text":"y"}]), serde_json::json!([{"name":"a","text":"x".repeat(17 * 1024)}])] {
            assert!(decode(serde_json::json!({"text":"What next?","message_id":"m","attachments":[],"client_context":context})).is_err());
        }
        assert!(decode(serde_json::json!({"text":"","message_id":"m","attachments":[],
            "client_context":[{"name":"home_state","text":"Tasks: none active."}]})).is_err());
    }

    #[test]
    fn extract_native_affordances_is_data_driven_and_fails_closed() {
        let parsed = extract_native_affordances_from_invocation(
            &metadata_input(serde_json::json!([
                {
                    "name": "artifact.read",
                    "description": "read bound artifact",
                    "input_schema": { "type": "object", "properties": { "path": { "type": "string" } } }
                }
            ])),
            &[],
        )
        .expect("a well-formed grant parses");
        assert_eq!(parsed.len(), 1);
        match &parsed[0] {
            DynamicToolSpec::Function(f) => {
                assert_eq!(f.name, "artifact-dread");
                assert!(
                    f.name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
                    "the presented native name must satisfy codex's ^[a-zA-Z0-9_-]+$ identifier pattern"
                );
                assert_eq!(
                    decode_tool_name(&f.name).unwrap(),
                    "artifact.read",
                    "the encoded native name decodes back to OUR real dotted tool id"
                );
                assert_eq!(f.description, "read bound artifact");
                assert!(f.input_schema.get("properties").is_some());
            }
            other => panic!("expected a Function spec, got {other:?}"),
        }

        assert!(extract_native_affordances_from_invocation(
            &metadata_input(serde_json::json!([])),
            &[],
        )
        .unwrap()
        .is_empty());

        let cut = extract_native_affordances_from_invocation(
            &metadata_input(serde_json::json!([
                {
                    "name": "file.write",
                    "description": "write file",
                    "input_schema": { "type": "object", "properties": {} }
                },
                {
                    "name": "artifact.read",
                    "description": "read",
                    "input_schema": { "type": "object", "properties": {} }
                }
            ])),
            &["file.write".to_string()],
        )
        .expect("formed metadata tools honor the interface's descriptor-owned hidden list");
        let cut_names: Vec<String> = cut
            .iter()
            .map(|spec| match spec {
                DynamicToolSpec::Function(f) => decode_tool_name(&f.name).unwrap(),
                other => panic!("expected a Function spec, got {other:?}"),
            })
            .collect();
        assert_eq!(cut_names, vec!["artifact.read".to_string()]);
        let descriptor: serde_json::Value =
            serde_json::from_str(include_str!("../../../../interface.json")).unwrap();
        let hidden = model_hidden_tool_names(
            descriptor["interface_driver_config"]["private_config"]
                .as_object()
                .unwrap(),
        )
        .unwrap();
        let mut application_input = metadata_input(serde_json::json!([
            {"name":"gui.app_state","description":"application state","input_schema":{"type":"object"}},
            {"name":"gui.app_resume","description":"operator continuation","input_schema":{"type":"object"}}
        ]));
        application_input.metadata.as_mut().unwrap().grant.tools =
            vec!["gui.app_state".into(), "gui.app_resume".into()];
        let granted = application_input.granted_tool_names().unwrap();
        let visible =
            extract_native_affordances_from_invocation(&application_input, &hidden).unwrap();
        let visible: Vec<String> = visible
            .iter()
            .map(|spec| match spec {
                DynamicToolSpec::Function(function) => decode_tool_name(&function.name).unwrap(),
                other => panic!("unexpected native spec: {other:?}"),
            })
            .collect();
        assert_eq!(visible, vec!["gui.app_state"]);
        assert_eq!(application_input.granted_tool_names().unwrap(), granted);
        assert!(granted.iter().any(|tool| tool == "gui.app_resume"));
        application_input.metadata.as_mut().unwrap().grant.tools.push("candidate".into());
        application_input.metadata.as_mut().unwrap().grant.interface_affordance_specs.push(octopus_interfaces::InterfaceToolAffordance {
            name:"candidate".into(),description:"not attached".into(),args_schema:serde_json::json!({"type":"object"}),
        });
        let before = application_input.metadata.clone();
        let exclusions = model_tool_exclusions(application_input.metadata().unwrap(), &granted.into_iter().collect(), &hidden);
        let visible = extract_native_affordances_from_invocation(&application_input, &exclusions).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(application_input.metadata, before);

        let mut missing_metadata = metadata_input(serde_json::json!([]));
        missing_metadata.metadata = None;
        assert!(matches!(
            extract_native_affordances_from_invocation(&missing_metadata, &[]),
            Err(InterfaceDriverError::Protocol(_))
        ));

        let mut missing_name = metadata_input(serde_json::json!([]));
        missing_name
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .interface_affordance_specs = vec![octopus_interfaces::InterfaceToolAffordance {
            name: octopus_descriptors::ToolId::new(""),
            description: "x".to_string(),
            args_schema: serde_json::json!({}),
        }];
        assert!(matches!(
            extract_native_affordances_from_invocation(&missing_name, &[]),
            Err(InterfaceDriverError::Protocol(_))
        ));

        let mut malformed_schema = metadata_input(serde_json::json!([]));
        malformed_schema
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .interface_affordance_specs = vec![octopus_interfaces::InterfaceToolAffordance {
            name: octopus_descriptors::ToolId::new("t"),
            description: String::new(),
            args_schema: serde_json::json!("oops"),
        }];
        assert!(matches!(
            extract_native_affordances_from_invocation(&malformed_schema, &[]),
            Err(InterfaceDriverError::Protocol(_))
        ));
    }

    #[test]
    fn bound_operation_catalog_becomes_an_exact_interface_affordance() {
        let mut input = metadata_input(serde_json::json!([
            {
                "name": "artifact.read",
                "description": "read bound artifact",
                "input_schema": { "type": "object", "properties": {} }
            }
        ]));
        input.metadata.as_mut().unwrap().grant.interfaces =
            vec!["world_mcp_drive_exact".to_string()];
        input.artifact_binding_payloads.push(bound(
            "artifact-operation-catalog-drive",
            serde_json::json!({
                "id": "artifact-operation-catalog-drive",
                "label": "mcp.operation_catalog",
                "interface": "world_mcp_drive_exact",
                "request_label": "mcp.operation.request",
                "task_input_labels": ["drive.event"],
                "operations": [{
                    "name": "google_drive.read_resource",
                    "description": "read one resource from the exact interface",
                    "input_schema": {
                        "type": "object",
                        "properties": {"uri": {"type": "string"}},
                        "required": ["uri"]
                    },
                    "request_label": "mcp.operation.request",
                    "output_label": "mcp.resource"
                }]
            }),
        ));
        let namespace = NativeToolNamespace {
            name: "formed_interfaces".to_string(),
            description: "Formed interface tools.".to_string(),
        };
        let catalog = extract_native_affordance_catalog(&input, &[], &namespace, None, &serde_json::Map::new()).unwrap();
        assert_eq!(catalog.specs.len(), 1);
        let DynamicToolSpec::Namespace(presented) = &catalog.specs[0] else {
            panic!("formed affordances must share the descriptor-owned namespace")
        };
        assert_eq!(presented.name, namespace.name);
        assert_eq!(presented.tools.len(), 2);
        let operation = catalog
            .interface_operations
            .get("google_drive.read_resource")
            .unwrap();
        assert_eq!(
            operation.interface,
            InterfaceId::new("world_mcp_drive_exact")
        );
        let output = CodexTurnOutput::interface_operation_call(
            operation,
            serde_json::json!({"uri": "drive://document"}),
            "call-1".to_string(),
        );
        assert_eq!(output.label, "mcp.operation.request");
        assert_eq!(
            output.route,
            ProviderEmissionRoute::Operation {
                interface: InterfaceId::new("world_mcp_drive_exact"),
                result_label: "mcp.resource".into(),
            }
        );
        let result = decode_interface_operation_result_artifact(
            &serde_json::json!({
                "label": "mcp.resource",
                "operation": "google_drive.read_resource",
                "correlation_id": "call-1",
                "session_disposition": "reused",
                "ok": true,
                "result": {"contents": []}
            }),
            "mcp.resource",
            &InterfaceId::new("world_mcp_drive_exact"),
            &catalog.interface_operations,
        )
        .unwrap()
        .unwrap();
        assert!(result.ok);
        assert_eq!(result.correlation_id, "call-1");

        let context = decode_provider_input(
            br#"{"ok":true,"resources":["drive://document"]}"#,
            "mcp.resources.updated",
            Some(&InterfaceId::new("world_mcp_drive_exact")),
            &catalog.interface_operations,
            None,
            &serde_json::Map::new(),
        )
        .unwrap();
        let ProviderInput::InterfaceContext(context) = context else {
            panic!("an unrelated operation-interface artifact must become live context")
        };
        assert_eq!(context.interface, InterfaceId::new("world_mcp_drive_exact"));
        assert_eq!(context.label, "mcp.resources.updated");
        assert_eq!(
            context.content,
            serde_json::json!({"ok": true, "resources": ["drive://document"]})
        );
        let drive = InterfaceId::new("world_mcp_drive_exact");
        let event = br#"{"ok":true,"text":"a document was shared with you"}"#;
        let event = decode_provider_input(event, "drive.event", Some(&drive), &catalog.interface_operations, None, &serde_json::Map::new());
        assert!(matches!(event.unwrap(), ProviderInput::Turn(_)), "a declared task input of an operation interface starts a turn");
    }

    #[test]
    fn checkpoint_source_interface_requires_the_canonical_binding_object() {
        let mut record = serde_json::json!({
            "active_checkpoints": ["checkpoint-a"],
            "checkpoints": [{
                "id": "checkpoint-a",
                "interface_binding": {
                    "interface": "world_mcp_drive_exact",
                    "config": {"artifact_policy": {"mode": "all"}}
                }
            }]
        });
        assert_eq!(
            source_interface_for_checkpoint(&record, "checkpoint-a").unwrap(),
            InterfaceId::new("world_mcp_drive_exact")
        );

        record["checkpoints"][0]["interface_binding"] = serde_json::json!("world_mcp_drive_exact");
        assert!(matches!(
            source_interface_for_checkpoint(&record, "checkpoint-a"),
            Err(InterfaceDriverError::Protocol(error))
                if error.contains("malformed canonical interface binding")
        ));
    }

    #[test]
    fn tool_name_encoding_is_a_codex_valid_bijection() {
        for name in [
            "probe.echo",
            "artifact.read",
            "coder.build.commit",
            "fixture.reply",
            "no_dot_name",
            "with-hyphen.and_underscore",
        ] {
            let encoded = encode_tool_name(name);
            assert!(
                encoded
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
                "encoded '{encoded}' must satisfy codex's ^[a-zA-Z0-9_-]+$ (no '.')"
            );
            assert!(
                !encoded.contains('.'),
                "the codex-forbidden '.' must be gone after encoding"
            );
            assert_eq!(
                decode_tool_name(&encoded).unwrap(),
                name,
                "the native name must decode back to OUR real tool id (C2)"
            );
        }
        assert_eq!(encode_tool_name("probe.echo"), "probe-decho");
        assert_eq!(encode_tool_name("a-b.c"), "a-hb-dc");

        assert!(matches!(
            decode_tool_name("bad-x"),
            Err(InterfaceDriverError::Protocol(_))
        ));
        assert!(matches!(
            decode_tool_name("trailing-"),
            Err(InterfaceDriverError::Protocol(_))
        ));
    }

    #[test]
    fn typed_error_maps_to_failclosed_taxonomy() {
        use octopus_appserver_proto::JSONRPCErrorError;
        let transport = TypedRequestError::Transport {
            method: "thread/start".into(),
            source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "x"),
        };
        assert!(matches!(
            typed_error(transport),
            InterfaceDriverError::Transport(_)
        ));

        let server = TypedRequestError::Server {
            method: "thread/start".into(),
            source: JSONRPCErrorError {
                code: -32000,
                message: "boom".into(),
                data: None,
            },
        };
        assert!(matches!(
            typed_error(server),
            InterfaceDriverError::Protocol(_)
        ));
    }

    #[test]
    fn transport_state_keys_by_run_id_and_reuses_the_warm_thread() {
        let flash = TransportStateScope::from_run(&octopus_interfaces::RunId::new("run_flash"));
        let planner = TransportStateScope::from_run(&octopus_interfaces::RunId::new("run_planner"));
        assert_eq!(flash, TransportStateScope::Run("run_flash".to_string()));
        assert_ne!(flash, planner);

        let fp = thread_shape_fingerprint(Some("gpt-5.5"), "role text", &[], "context-a")
            .expect("fingerprint computes");

        let mut transport_state: BTreeMap<TransportStateScope, TransportState> = BTreeMap::new();
        {
            let flash_state = transport_state.entry(flash.clone()).or_default();
            flash_state.thread_id = Some("thr_flash".to_string());
            flash_state.thread_shape_fingerprint = Some(fp.clone());
            flash_state.hooks_catalog_emitted = true;
        }

        match plan_thread_action(transport_state.get(&flash).unwrap(), &fp) {
            ThreadAction::Reuse { thread_id } => assert_eq!(thread_id, "thr_flash"),
            ThreadAction::Mint => panic!("a warm identical-shape turn must reuse the thread"),
        }

        let planner_state = transport_state.entry(planner.clone()).or_default();
        assert!(matches!(
            plan_thread_action(planner_state, &fp),
            ThreadAction::Mint
        ));
        assert!(
            !planner_state.hooks_catalog_emitted,
            "the once-per-run hooks-catalog bit is per-run and distinct across runs"
        );
    }

    #[test]
    fn grant_or_static_context_drift_forces_a_cold_thread() {
        let fp_a = thread_shape_fingerprint(Some("gpt-5.5"), "role text", &[], "context-a")
            .expect("fingerprint computes");
        let state = TransportState {
            context_generation: 0,
            hooks_catalog_emitted: true,
            thread_id: Some("thr_1".to_string()),
            thread_shape_fingerprint: Some(fp_a.clone()),
            static_context_fingerprint: Some("context-a".to_string()),
            rollout_path: None,
        };

        let drifted_grant = thread_shape_fingerprint(
            Some("gpt-5.5"),
            "role text",
            &[DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: "coder.exec".to_string(),
                description: "exec".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
                defer_loading: false,
            })],
            "context-a",
        )
        .expect("fingerprint computes");
        assert_ne!(
            fp_a, drifted_grant,
            "a formation that grants a different tool list must change the thread shape"
        );
        assert!(matches!(
            plan_thread_action(&state, &drifted_grant),
            ThreadAction::Mint
        ));

        let drifted_role =
            thread_shape_fingerprint(Some("gpt-5.5"), "another role", &[], "context-a")
                .expect("fingerprint computes");
        assert_ne!(fp_a, drifted_role);
        assert!(matches!(
            plan_thread_action(&state, &drifted_role),
            ThreadAction::Mint
        ));

        let drifted_context =
            thread_shape_fingerprint(Some("gpt-5.5"), "role text", &[], "context-b")
                .expect("fingerprint computes");
        assert!(
            matches!(
                plan_thread_action(&state, &drifted_context),
                ThreadAction::Mint
            ),
            "a changed non-session artifact binding must reset the transport thread"
        );
        assert!(matches!(
            plan_thread_action(&state, &fp_a),
            ThreadAction::Reuse { .. }
        ));

        let stable = thread_shape_fingerprint(Some("gpt-5.5"), "role text", &[], "context-a")
            .expect("fingerprint computes");
        assert_eq!(
            fp_a, stable,
            "a stable grant keeps a stable fingerprint — no spurious re-mints killing the cache"
        );
    }

    #[test]
    fn context_block_is_prepended_to_turn_input_every_turn() {
        let real_input = vec![UserInput::Text {
            text: "do the work".to_string(),
            text_elements: Vec::new(),
        }];

        let with_block = prepend_context_block_to_turn_input(
            Some("OBLIGATIONS — produce artifact `screenshot`".to_string()),
            real_input.clone(),
        );
        assert_eq!(
            with_block.len(),
            2,
            "the block is an extra leading input element"
        );
        match &with_block[0] {
            UserInput::Text { text, .. } => {
                assert!(
                    text.contains("OBLIGATIONS"),
                    "the obligations block leads the turn input"
                );
            }
            other => panic!("expected a leading Text obligations block, got {other:?}"),
        }
        match &with_block[1] {
            UserInput::Text { text, .. } => assert_eq!(text, "do the work"),
            other => panic!("expected the real input to follow, got {other:?}"),
        }

        assert_eq!(
            prepend_context_block_to_turn_input(None, real_input.clone()).len(),
            1
        );
        assert_eq!(
            prepend_context_block_to_turn_input(Some("   ".to_string()), real_input).len(),
            1
        );
        assert_selected_file_projection_uses_materialized_sources();
    }

    fn assert_selected_file_projection_uses_materialized_sources() {
        use octopus_ledger::ContentHash;
        use octopus_run_record::RunRecord;
        let root = tempfile::tempdir().unwrap().keep();
        eprintln!("selected file projection fixture artifacts: {}", root.display());
        let materialize = |label: &str, bytes: &[u8]| {
            let file = InterfaceArtifactFile::write(root.as_path(), label, bytes).unwrap();
            let reference = file
                .pin_manifest_into_blob(&mut octopus_ledger::InMemoryBlobs::new())
                .unwrap()
                .artifact;
            let payload = octopus_interfaces::ArtifactBindingPayload::new(
                reference.clone(),
                file.read_payload_bytes().unwrap(),
            );
            (file, reference, payload)
        };
        let (first_file, first_ref, first) =
            materialize("operator.file.binary", &[0xff, 0, 0x81, b'A']);
        let (second_file, second_ref, second) =
            materialize("operator.file.binary", &[0xff, 0, 0x81, b'B']);
        let attachment = |file: &InterfaceArtifactFile, filename: &str| {
            let bytes = file.read_payload_bytes().unwrap();
            serde_json::json!({"name":filename,"label":"operator.file.binary","mime_type":"application/zip",
                "size":bytes.len(),"payload_path":file.read_manifest().unwrap().payload_path,
                "payload_content_hash":sha256_hex(&bytes),"last_modified":0})
        };
        let original = serde_json::json!({"text":"Use the attached files","message_id":"original-message",
            "attachments":[attachment(&first_file,"shared.zip"),attachment(&second_file,"shared.zip")]});
        let (_, original_ref, original_bound) = materialize(
            OPERATOR_MESSAGE_LABEL,
            &serde_json::to_vec(&original).unwrap(),
        );
        let current = serde_json::json!({"text":"Use the same file with either exact name","message_id":"next-message",
            "attachments":[attachment(&first_file,"résumé.zip"),attachment(&first_file,"another.zip")]});
        let current_bytes = serde_json::to_vec(&current).unwrap();
        let (current_file, current_ref, _) = materialize(OPERATOR_MESSAGE_LABEL, &current_bytes);
        let mut input = InterfaceInvocationEnvelope {
            artifact: current_ref.clone(),
            artifact_file: current_file,
            artifact_binding_payloads: vec![first.clone(), second.clone(), original_bound.clone()],
            metadata: Some(octopus_interfaces::InvocationMetadata::new(
                octopus_interfaces::GrantProjection::default(),
            )),
        };
        let make_record = |input: &mut InterfaceInvocationEnvelope,
                           provider: &str,
                           selected: &[ArtifactRef]| {
            let bindings = input.artifact_binding_payloads.iter().map(|bound| serde_json::json!({
                "id":bound.artifact.id,"access":{"read":true,"change":false,"create":false,"delete":false}
            })).collect::<Vec<_>>();
            let inbound = octopus_runtime::AuthoredInterfaceBinding::all(
                InterfaceId::new("world_operator_chat_test"),
            );
            let outbound = octopus_runtime::AuthoredInterfaceBinding::all(
                InterfaceId::new(provider),
            );
            let record = serde_json::json!({"id":"file-projection-run","ee":"file-projection-ee",
                "worker":"system_worker_test","trace":"file-projection-trace","status":"running",
                "interface_state":{provider:{"run_authoring_binding":{"schema_version":2,
                    "run":"file-projection-run","worker":"system_worker_test","provider_interface":provider,
                    "authoring_source_interface":provider,"payload_artifact_bindings":selected,
                    "checkpoints":[{"name":"input","role":"stable","include_context":true,
                        "interface_binding":inbound,
                        "outbound_interface_bindings":[outbound],"required_verified_artifacts":[]}]}}},
                "checkpoints":[{"id":"input","run":"file-projection-run","trace":"file-projection-trace","epoch":1,
                    "interface_binding":inbound,"outbound_interface_bindings":[outbound],
                    "artifact_bindings":bindings,"required_verified_artifacts":[]}],"active_checkpoints":["input"]});
            let bytes = serde_json::to_vec(&record).unwrap();
            let record = RunRecord::from_bytes(&bytes).unwrap();
            let metadata = input.metadata.as_mut().unwrap();
            metadata.runtime = Some(RuntimeInvocationMetadata {
                run: record.id.clone(),
                ee: record.ee.clone(),
                worker: record.worker.clone(),
                checkpoint: record.checkpoints[0].id.clone(),
                interface: InterfaceId::new(provider),
                epoch: 1,
                run_record_hash: Some(ContentHash::of_bytes(&bytes).to_hex()),
            });
            metadata.artifact_binding_refs = input
                .artifact_binding_payloads
                .iter()
                .map(|bound| bound.artifact.clone())
                .collect();
            metadata.checkpoint_binding_policy =
                Some(octopus_interfaces::CheckpointBindingPolicySnapshot {
                    inbound: record.checkpoints[0].interface_binding.clone().unwrap(),
                    outbound: record.checkpoints[0].outbound_interface_bindings.clone(),
                });
            metadata.outbound_interface_bindings = record.checkpoints[0]
                .outbound_interfaces()
                .cloned()
                .collect();
            metadata.grant.interfaces = vec!["world_operator_chat_test".into(), provider.into()];
            metadata.grant.artifact_access = record.checkpoints[0]
                .artifact_bindings
                .iter()
                .map(|binding| (binding.id.as_str().to_string(), binding.access.clone()))
                .collect();
            record
        };
        let selected = vec![first_ref.clone(), second_ref.clone()];
        let record = make_record(&mut input, "world_provider_test", &selected);
        let mut retained_classification = input.clone();
        retained_classification.artifact_binding_payloads.retain(|bound| bound.artifact != second_ref);
        let retained_record = make_record(&mut retained_classification, "world_provider_test", &selected);
        let (context, admitted) = admitted_payload_context(&retained_record, &retained_classification).unwrap();
        assert_eq!(admitted, vec![first_ref.clone()]);
        assert_eq!(context.iter().map(|bound| &bound.artifact).collect::<Vec<_>>(), vec![&original_ref]);
        assert!(!retained_classification.metadata().unwrap().grant.artifact_access.contains_key(second_ref.id.as_str()));
        retained_classification.metadata.as_mut().unwrap().grant.artifact_access.get_mut(first_ref.id.as_str()).unwrap().read = false;
        assert!(admitted_payload_context(&retained_record, &retained_classification).is_err());
        let project = |record: &RunRecord,
                       input: &InterfaceInvocationEnvelope,
                       contexts: &BoundSessionContexts,
                       history: &[SessionContextArtifact],
                       limit| {
            selected_file_projection(
                record,
                input,
                &input.artifact,
                OPERATOR_MESSAGE_LABEL,
                &input.read_artifact_payload().unwrap(),
                contexts,
                history,
                ModelResultProjection { max_tokens: limit },
            )
        };
        let projection = project(&record, &input, &BTreeMap::new(), &[], 10_000).unwrap();
        let (_, document_ref, document) = materialize("architecture.md", b"# Architecture\n");
        let mut template_input = input.clone();
        template_input.artifact_binding_payloads.push(document);
        let mut template_selected = selected.clone();
        template_selected.push(document_ref.clone());
        let template_record = make_record(&mut template_input, "world_provider_test", &template_selected);
        let projected = project(&template_record, &template_input, &BTreeMap::new(), &[], 10_000).unwrap();
        let projected: serde_json::Value = serde_json::from_str(&projected.text).unwrap();
        assert_eq!(projected["selected_artifacts"], serde_json::json!([{
            "artifact_ref": document_ref, "label": "architecture.md", "size_bytes": 15,
        }]));
        let ProviderInput::Turn(turn) = decode_provider_input(
            &current_bytes,
            OPERATOR_MESSAGE_LABEL,
            None,
            &BTreeMap::new(),
            Some(&projection),
            &serde_json::Map::new(),
        )
        .unwrap() else {
            panic!("ordinary operator input lost its turn");
        };
        let UserInput::Text { text, .. } = &turn[0] else {
            panic!("file metadata is not visible text");
        };
        let visible: serde_json::Value = serde_json::from_str(text).unwrap();
        let rows = visible["selected_files"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        let chooser_args = serde_json::json!({"files":rows.iter().map(|row| serde_json::json!({
            "artifact_ref":row["artifact_ref"],"filename":row["filename"]
        })).collect::<Vec<_>>()});
        for file in chooser_args["files"].as_array().unwrap() {
            let reference: ArtifactRef =
                serde_json::from_value(file["artifact_ref"].clone()).unwrap();
            let bound = input
                .artifact_binding_payloads
                .iter()
                .find(|bound| bound.artifact == reference)
                .unwrap();
            assert!(selected.contains(&reference));
            assert!(
                rows.iter()
                    .any(|row| row["artifact_ref"] == file["artifact_ref"]
                        && row["filename"] == file["filename"]
                        && row["size_bytes"] == bound.read_bytes().unwrap().len())
            );
        }
        assert_eq!(
            rows.iter()
                .filter(|row| row["filename"] == "shared.zip")
                .count(),
            2
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row["source_message_id"] == "next-message")
                .count(),
            2
        );
        assert!(rows.iter().any(|row| row["filename"] == "résumé.zip"));
        assert!(!text.contains("payload_path"));
        assert!(!text.contains(&root.as_path().display().to_string()));
        assert_ne!(first_ref.content.to_hex(), sha256_hex(first.read_bytes().unwrap().as_ref()));
        let snapshot_body = serde_json::json!({"label":"interface.artifact.output","text":"Files retained",
            "session_context":{"schema_version":3,"provider_interface":"world_provider_test",
                "artifacts":[{"label":OPERATOR_MESSAGE_LABEL,"payload":original,
                    "source_artifact":original_ref,"source_movement_seq":null}]}});
        let (_, snapshot_ref, snapshot_bound) = materialize(
            "interface.artifact.output",
            &serde_json::to_vec(&snapshot_body).unwrap(),
        );
        let preserved = SessionContextArtifact {
            label: OPERATOR_MESSAGE_LABEL.into(),
            payload: serde_json::from_slice(original_bound.read_bytes().unwrap().as_ref()).unwrap(),
            source_artifact: original_ref,
            source_movement_seq: None,
        };
        let grounded = ProviderInvocationSessionSnapshot {
            carrier: snapshot_ref,
            artifacts: Arc::new(vec![preserved.clone()]),
        };
        let mut cold_input = input.clone();
        cold_input.artifact_binding_payloads[2] = snapshot_bound;
        let cold_record = make_record(&mut cold_input, "world_provider_test", &selected);
        let context_payloads = octopus_run_authoring::context_artifact_payloads(
            &cold_record,
            cold_input.metadata().unwrap(),
            &cold_input.artifact_binding_payloads,
        )
        .unwrap();
        let contexts = CodexAppServerInterfaceDriver::resolve_bound_session_contexts(
            &context_payloads,
            &InterfaceId::new("world_provider_test"),
            &codex_snapshot_labels(),
            Some(&grounded),
        )
        .unwrap();
        let cold = project(&cold_record, &cold_input, &contexts, &[], 10_000).unwrap();
        assert_eq!(cold.text, projection.text);
        assert_eq!(
            project(&cold_record, &cold_input, &contexts, &[preserved], 10_000)
                .unwrap()
                .text,
            projection.text
        );
        let switched_record = make_record(&mut cold_input, "world_provider_other", &selected);
        let contexts = CodexAppServerInterfaceDriver::resolve_bound_session_contexts(
            &context_payloads,
            &InterfaceId::new("world_provider_other"),
            &codex_snapshot_labels(),
            None,
        )
        .unwrap();
        let foreign_only: serde_json::Value = serde_json::from_str(
            &project(&switched_record, &cold_input, &contexts, &[], 10_000)
                .unwrap()
                .text,
        )
        .unwrap();
        assert_eq!(foreign_only["selected_files"].as_array().unwrap().len(), 2);
        cold_input.artifact_binding_payloads.push(original_bound);
        let switched_record = make_record(&mut cold_input, "world_provider_other", &selected);
        assert_eq!(
            project(&switched_record, &cold_input, &contexts, &[], 10_000)
                .unwrap()
                .text,
            projection.text
        );
        let mut denied = input.clone();
        for field in ["checkpoint", "epoch", "run"] {
            let mut mismatched = input.clone();
            let runtime = mismatched.metadata.as_mut().unwrap().runtime.as_mut().unwrap();
            match field {
                "checkpoint" => runtime.checkpoint = octopus_descriptors::CheckpointId::new("replaced"),
                "epoch" => runtime.epoch += 1,
                "run" => runtime.run = octopus_interfaces::RunId::new("other-run"),
                _ => unreachable!(),
            }
            assert!(project(&record, &mismatched, &BTreeMap::new(), &[], 10_000).is_err());
        }
        denied
            .metadata
            .as_mut()
            .unwrap()
            .grant
            .artifact_access
            .get_mut(first_ref.id.as_str())
            .unwrap()
            .read = false;
        assert!(project(&record, &denied, &BTreeMap::new(), &[], 10_000).is_err());
        let mut role = input.clone();
        role.metadata
            .as_mut()
            .unwrap()
            .grant
            .role_artifacts
            .push(first_ref.id.as_str().to_string());
        assert!(project(&record, &role, &BTreeMap::new(), &[], 10_000).is_err());
        let mut wrong = input.clone();
        wrong.artifact_binding_payloads[0].artifact.content =
            ContentHash::of_bytes(b"different manifest");
        assert!(project(&record, &wrong, &BTreeMap::new(), &[], 10_000).is_err());
        let mut cleared = input.clone();
        cleared.artifact_binding_payloads.clear();
        let cleared_record = make_record(&mut cleared, "world_provider_test", &[]);
        let empty = project(
            &cleared_record,
            &cleared,
            &BTreeMap::new(),
            &grounded.artifacts,
            10_000,
        )
        .unwrap();
        assert_eq!(empty.text, "{\"selected_files\":[]}");
        assert!(
            decode_provider_input(
                &current_bytes,
                OPERATOR_MESSAGE_LABEL,
                None,
                &BTreeMap::new(),
                Some(&empty),
                &serde_json::Map::new(),
            )
            .is_err()
        );
        assert!(
            decode_provider_input(
                &current_bytes,
                OPERATOR_MESSAGE_LABEL,
                None,
                &BTreeMap::new(),
                None,
                &serde_json::Map::new(),
            )
            .is_err()
        );
        let mut unselected = input.clone();
        let unselected_record = make_record(&mut unselected, "world_provider_test", &[]);
        let payloads = octopus_run_authoring::context_artifact_payloads(
            &unselected_record,
            unselected.metadata().unwrap(),
            &unselected.artifact_binding_payloads,
        )
        .unwrap();
        assert!(
            assemble_context_resolved(
                &payloads,
                &BTreeMap::new(),
                &InterfaceId::new("world_provider_test"),
                &codex_snapshot_labels(),
                ModelResultProjection { max_tokens: 10_000 }
            )
            .is_err()
        );
        let mut changed_size = current.clone();
        changed_size["attachments"][0]["size"] = serde_json::json!(5);
        let (file, reference, _) = materialize(
            OPERATOR_MESSAGE_LABEL,
            &serde_json::to_vec(&changed_size).unwrap(),
        );
        let mut mismatch = input.clone();
        mismatch.artifact_file = file;
        mismatch.artifact = reference;
        let mismatched = project(&record, &mismatch, &BTreeMap::new(), &[], 10_000).unwrap();
        assert!(!mismatched.current_attachments.contains(&0));
        assert!(
            decode_provider_input(
                &serde_json::to_vec(&changed_size).unwrap(),
                OPERATOR_MESSAGE_LABEL,
                None,
                &BTreeMap::new(),
                Some(&mismatched),
                &serde_json::Map::new(),
            )
            .is_err()
        );
        assert!(
            decode_provider_input(
                &serde_json::to_vec(&changed_size).unwrap(),
                OPERATOR_MESSAGE_LABEL,
                None,
                &BTreeMap::new(),
                Some(&projection),
                &serde_json::Map::new(),
            )
            .is_err()
        );
        let mut wrong_hash = current.clone();
        wrong_hash["attachments"][0]["payload_content_hash"] =
            serde_json::json!(first_ref.content.to_hex());
        let (file, reference, _) = materialize(
            OPERATOR_MESSAGE_LABEL,
            &serde_json::to_vec(&wrong_hash).unwrap(),
        );
        let mut wrong_hash_input = input.clone();
        wrong_hash_input.artifact_file = file;
        wrong_hash_input.artifact = reference;
        assert!(
            !project(&record, &wrong_hash_input, &BTreeMap::new(), &[], 10_000)
                .unwrap()
                .current_attachments
                .contains(&0)
        );
        let mut overlong = current.clone();
        overlong["attachments"][0]["name"] = serde_json::json!("x".repeat(4000));
        let (file, reference, _) = materialize(
            OPERATOR_MESSAGE_LABEL,
            &serde_json::to_vec(&overlong).unwrap(),
        );
        let mut oversized = input.clone();
        oversized.artifact_file = file;
        oversized.artifact = reference;
        assert!(project(&record, &oversized, &BTreeMap::new(), &[], 512).is_err());
        use base64::Engine;
        let png=base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEElEQVR4nGNwaDgARAwQCgAoDgYBQzpj2gAAAABJRU5ErkJggg==").unwrap();
        let (image_file, image_ref, image_bound) = materialize("operator.media.image", &png);
        let (text_file, text_ref, text_bound) =
            materialize("operator.file.text", b"original UTF-8 text");
        let mut image = attachment(&image_file, "image.png");
        image["label"] = serde_json::json!("operator.media.image");
        image["mime_type"] = serde_json::json!("image/png");
        let mut text = attachment(&text_file, "note.txt");
        text["label"] = serde_json::json!("operator.file.text");
        text["mime_type"] = serde_json::json!("text/plain");
        let supported_bytes=serde_json::to_vec(&serde_json::json!({"text":"read these","message_id":"supported-message","attachments":[image,text]})).unwrap();
        let (file, reference, _) = materialize(OPERATOR_MESSAGE_LABEL, &supported_bytes);
        let mut supported = input.clone();
        supported.artifact_file = file;
        supported.artifact = reference;
        supported.artifact_binding_payloads = vec![image_bound, text_bound];
        let supported_record = make_record(
            &mut supported,
            "world_provider_test",
            &[image_ref, text_ref],
        );
        let metadata =
            project(&supported_record, &supported, &BTreeMap::new(), &[], 10_000).unwrap();
        let ProviderInput::Turn(projected) = decode_provider_input(
            &supported_bytes,
            OPERATOR_MESSAGE_LABEL,
            None,
            &BTreeMap::new(),
            Some(&metadata),
            &serde_json::Map::new(),
        )
        .unwrap() else {
            panic!("supported input lost its turn");
        };
        let ProviderInput::Turn(original) = decode_provider_input(
            &supported_bytes,
            OPERATOR_MESSAGE_LABEL,
            None,
            &BTreeMap::new(),
            None,
            &serde_json::Map::new(),
        )
        .unwrap() else {
            panic!("original input lost its turn");
        };
        assert_eq!(
            serde_json::to_value(&projected[1..]).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(image_file.read_payload_bytes().unwrap(), png);
    }

    #[test]
    fn our_goal_from_metadata_reaches_the_model_context_block() {
        let metadata =
            octopus_interfaces::InvocationMetadata::new(octopus_interfaces::GrantProjection {
                tools: vec!["coder.exec".to_string()],
                goal: Some("drive the coder worker to a green build".to_string()),
                ..octopus_interfaces::GrantProjection::default()
            });
        let block = render_model_context_block(&metadata)
            .expect("our goal renders the model context block the driver prepends every turn");
        assert!(
            block.contains("GOAL"),
            "the driver-prepended block leads with OUR goal, not codex-native ThreadGoal"
        );
        assert!(block.contains("drive the coder worker to a green build"));
        let goal_at = block.find("GOAL").expect("goal section present");
        if let Some(grant_at) = block.find("GRANT") {
            assert!(
                goal_at < grant_at,
                "our goal precedes the grant in the model context"
            );
        }
    }

    #[test]
    fn model_resolves_only_from_bound_interface_folder_config() {
        let bound: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({ "model": "gpt-5.5", "reasoning_effort": "xhigh" }),
        )
        .unwrap();

        assert_eq!(
            non_empty_string_field(
                bound.get("model"),
                "interface_driver_config.private_config.model"
            )
            .unwrap(),
            Some("gpt-5.5".to_string())
        );
        assert_eq!(
            non_empty_string_field(
                bound.get("reasoning_effort"),
                "interface_driver_config.private_config.reasoning_effort"
            )
            .unwrap(),
            Some("xhigh".to_string())
        );

        let empty = serde_json::Map::new();
        assert_eq!(
            non_empty_string_field(
                empty.get("model"),
                "interface_driver_config.private_config.model"
            )
            .unwrap(),
            None
        );

        let malformed: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({ "model": 7 })).unwrap();
        assert!(matches!(
            non_empty_string_field(
                malformed.get("model"),
                "interface_driver_config.private_config.model"
            ),
            Err(InterfaceDriverError::Protocol(_))
        ));
        let blank: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({ "model": "  " })).unwrap();
        assert!(matches!(
            non_empty_string_field(
                blank.get("model"),
                "interface_driver_config.private_config.model"
            ),
            Err(InterfaceDriverError::Protocol(_))
        ));
    }

    #[test]
    fn model_change_is_pure_provider_interface_json_change() {
        let interface_a = InterfaceDriverConfig {
            private_config: serde_json::from_value(
                serde_json::json!({ "model": "provider-model-alpha", "reasoning_effort": "low" }),
            )
            .unwrap(),
            residual_ambient: None,
            host_slots: std::collections::BTreeMap::new(),
            transport: None,
            supported_reasoning_efforts: Vec::new(),
        };
        let interface_b = InterfaceDriverConfig {
            private_config: serde_json::from_value(
                serde_json::json!({ "model": "provider-model-beta", "reasoning_effort": "high" }),
            )
            .unwrap(),
            residual_ambient: None,
            host_slots: std::collections::BTreeMap::new(),
            transport: None,
            supported_reasoning_efforts: Vec::new(),
        };

        assert_eq!(
            non_empty_string_field(
                interface_a.private_config.get("model"),
                "interface_driver_config.private_config.model"
            )
            .unwrap(),
            Some("provider-model-alpha".to_string())
        );
        assert_eq!(
            non_empty_string_field(
                interface_a.private_config.get("reasoning_effort"),
                "interface_driver_config.private_config.reasoning_effort"
            )
            .unwrap(),
            Some("low".to_string())
        );

        assert_eq!(
            non_empty_string_field(
                interface_b.private_config.get("model"),
                "interface_driver_config.private_config.model"
            )
            .unwrap(),
            Some("provider-model-beta".to_string())
        );
        assert_eq!(
            non_empty_string_field(
                interface_b.private_config.get("reasoning_effort"),
                "interface_driver_config.private_config.reasoning_effort"
            )
            .unwrap(),
            Some("high".to_string())
        );
    }

    #[test]
    fn interface_private_config_comes_from_folder_config() {
        let config = InterfaceDriverConfig {
            private_config: serde_json::from_value(
                serde_json::json!({ "model": "gpt-5.5", "reasoning_effort": "xhigh" }),
            )
            .unwrap(),
            residual_ambient: None,
            host_slots: std::collections::BTreeMap::new(),
            transport: None,
            supported_reasoning_efforts: vec!["xhigh".to_string()],
        };
        let extracted = config.private_config.clone();
        assert_eq!(
            extracted.get("model").and_then(serde_json::Value::as_str),
            Some("gpt-5.5")
        );
        assert_eq!(
            extracted
                .get("reasoning_effort")
                .and_then(serde_json::Value::as_str),
            Some("xhigh")
        );
        assert!(
            None::<&InterfaceDriverConfig>
                .map(|config| config.private_config.clone())
                .unwrap_or_default()
                .is_empty()
        );
    }

    #[test]
    fn server_requests_classify_into_labelled_operator_artifacts() {
        let exec_approval: ServerRequest = serde_json::from_value(serde_json::json!({
            "method": "item/commandExecution/requestApproval",
            "id": 7,
            "params": {
                "threadId": "t1",
                "turnId": "u1",
                "itemId": "i1",
                "startedAtMs": 0,
                "command": "rm -rf /tmp/scratch",
                "reason": "needs write access"
            }
        }))
        .expect("exec approval server request deserializes from its wire form");
        let output = classify_operator_request(&exec_approval, &test_operator_continuity_config())
            .expect("classify does not error")
            .expect("an approval request has an operator home");
        assert_eq!(output.label, OPERATOR_APPROVAL_REQUEST_LABEL);
        let ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { inputs } = &output.route
        else {
            panic!("approval request must author verified response inputs before projection")
        };
        assert_eq!(inputs.len(), 2);
        assert_eq!(
            inputs
                .iter()
                .map(|input| input.required_verified_artifact.mark.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["operator.approval", "operator.input.provided"])
        );
        assert_eq!(
            output
                .value
                .get("method")
                .and_then(serde_json::Value::as_str),
            Some("item/commandExecution/requestApproval")
        );
        assert_eq!(
            output
                .value
                .get("request_id")
                .and_then(serde_json::Value::as_str),
            Some("7"),
            "the server request id (as request_id) correlates the operator's response back"
        );
        assert_eq!(
            output.value.get("kind").and_then(serde_json::Value::as_str),
            Some("exec")
        );
        assert_eq!(
            output
                .value
                .get("title")
                .and_then(serde_json::Value::as_str),
            Some("rm -rf /tmp/scratch"),
            "the exec approval's headline is the command line"
        );
        assert_eq!(
            output
                .value
                .pointer("/detail/command")
                .and_then(serde_json::Value::as_str),
            Some("rm -rf /tmp/scratch"),
            "the full prompt data (the command, the reason) rides the artifact as opaque detail"
        );

        let legacy_patch_approval: ServerRequest = serde_json::from_value(serde_json::json!({
            "method": "applyPatchApproval",
            "id": "req-8",
            "params": {
                "conversationId": "00000000-0000-0000-0000-000000000000",
                "callId": "c1",
                "fileChanges": {},
                "reason": null,
                "grantRoot": null
            }
        }))
        .expect("legacy patch approval server request deserializes from its wire form");
        let output =
            classify_operator_request(&legacy_patch_approval, &test_operator_continuity_config())
                .expect("classify does not error")
                .expect("a legacy approval request has an operator home");
        assert_eq!(output.label, OPERATOR_APPROVAL_REQUEST_LABEL);
        assert_eq!(
            output
                .value
                .get("request_id")
                .and_then(serde_json::Value::as_str),
            Some("req-8"),
            "a string request id passes through unchanged for correlation"
        );
        assert_eq!(
            output.value.get("kind").and_then(serde_json::Value::as_str),
            Some("patch")
        );

        let user_input: ServerRequest = serde_json::from_value(serde_json::json!({
            "method": "item/tool/requestUserInput",
            "id": 9,
            "params": {
                "threadId": "t1",
                "turnId": "u1",
                "itemId": "i1",
                "questions": [{ "id": "q1", "header": "Env", "question": "Which environment?" }]
            }
        }))
        .expect("user-input server request deserializes from its wire form");
        let output = classify_operator_request(&user_input, &test_operator_continuity_config())
            .expect("classify does not error")
            .expect("a user-input request has an operator home");
        assert_eq!(output.label, OPERATOR_USER_INPUT_REQUEST_LABEL);
        let ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { inputs } = &output.route
        else {
            panic!("user-input request must author its verified response input")
        };
        assert_eq!(inputs.len(), 1);
        assert_eq!(
            inputs[0].required_verified_artifact.mark.as_str(),
            "operator.input.provided"
        );
        assert_eq!(
            output
                .value
                .get("request_id")
                .and_then(serde_json::Value::as_str),
            Some("9")
        );
        assert_eq!(
            output
                .value
                .get("message")
                .and_then(serde_json::Value::as_str),
            Some("Which environment?"),
            "the first tool question is the operator-readable message"
        );
        assert!(
            output.value.pointer("/questions/0/options").is_some(),
            "a question form carries the complete question surface"
        );
        assert_eq!(
            output
                .value
                .get("is_blocking")
                .and_then(serde_json::Value::as_bool),
            Some(true),
            "legacy payloads remain blocking"
        );

        let attestation: ServerRequest = serde_json::from_value(serde_json::json!({
            "method": "attestation/generate",
            "id": 11,
            "params": {}
        }))
        .expect("attestation server request deserializes from its wire form");
        assert!(
            classify_operator_request(&attestation, &test_operator_continuity_config())
                .expect("classify does not error")
                .is_none(),
            "an out-of-band request is not surfaced to the operator; the caller fails it closed"
        );
    }

    #[test]
    fn interrupted_question_requires_its_exact_admitted_request() {
        let pending = CodexPendingContinuity::Operator { turn_id: "turn-1".into(), request_key: "operator:9".into(), method: "item/tool/requestUserInput".into() };
        let state = continuity("world_provider_test", "run-1", "thread-1", 2, Some(pending));
        let response = OperatorResponseArtifact::UserInput { request_id: "9".into(), answers: serde_json::json!({"q1":["Verified"]}) };
        let reference = ArtifactRef { id: octopus_descriptors::ArtifactId::new("question-1"), content: octopus_ledger::ContentHash::of_bytes(b"question") };
        let question = SessionContextArtifact { label: OPERATOR_USER_INPUT_REQUEST_LABEL.into(),
            payload: serde_json::json!({"request_id":"9","questions":[{"id":"q1","question":"Verify?"}], "codex_continuity":state}),
            source_artifact: reference.clone(), source_movement_seq: Some(10) };
        assert_eq!(exact_pending_question(&[question.clone()], &state, &response).unwrap(), reference);
        assert!(exact_pending_question(&[], &state, &response).is_err());
        assert!(exact_pending_question(&[question.clone(), question.clone()], &state, &response).is_err());
        let mut unadmitted = question.clone(); unadmitted.source_movement_seq = None;
        assert!(exact_pending_question(&[unadmitted], &state, &response).is_err());
        let mut wrong = question.clone(); wrong.payload["codex_continuity"]["pending"]["turn_id"] = "other".into();
        assert!(exact_pending_question(&[wrong], &state, &response).is_err());
        let approval = OperatorResponseArtifact::Approval { request_id: "9".into(), decision: serde_json::json!("accept") };
        assert!(exact_pending_question(&[question], &state, &approval).is_err());
    }

    #[test]
    fn operator_response_continuation_requires_exact_admitted_edge() {
        let continuity = test_operator_continuity_config();
        let response = OperatorResponseArtifact::Approval {
            request_id: "request-7".to_string(),
            decision: serde_json::json!("accept"),
        };
        let approved = continuity
            .approval
            .approved
            .required(&continuity.approval.response_label, "request-7");
        let metadata = InvocationMetadata::new(octopus_ledger::GrantProjection {
            mark_outcomes: vec!["operator.approval".to_string()],
            ..octopus_ledger::GrantProjection::default()
        })
        .with_required_verified_artifacts(vec![approved])
        .with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {
            artifact_label: OPERATOR_APPROVAL_RESPONSE_LABEL.to_string(),
            required_artifact_labels: vec![OPERATOR_APPROVAL_RESPONSE_LABEL.to_string()],
            complete: true,
        });
        validate_operator_response_evidence(
            &continuity,
            OPERATOR_APPROVAL_RESPONSE_LABEL,
            &response,
            &metadata,
        )
        .unwrap();

        let elicitation = OperatorResponseArtifact::Elicitation {
            request_id: "request-7".to_string(),
            answer: serde_json::json!({"value": "yes"}),
        };
        assert!(
            validate_operator_response_evidence(
                &continuity,
                OPERATOR_ELICITATION_RESPONSE_LABEL,
                &elicitation,
                &metadata,
            )
            .is_err()
        );
    }

    #[test]
    fn reasoning_item_becomes_turn_reasoning_artifact() {
        let item = ThreadItem::Reasoning {
            id: "reason-1".into(),
            summary: vec!["Plan the change".into(), "Run the tests".into()],
            content: vec!["First read the file".into()],
        };
        let output = rich_item_output(item).expect("a reasoning item is a rich turn item");
        assert_eq!(output.label, TURN_REASONING_LABEL);
        assert_eq!(
            output
                .value
                .get("item_id")
                .and_then(serde_json::Value::as_str),
            Some("reason-1")
        );
        assert_eq!(
            output
                .value
                .pointer("/summary/0")
                .and_then(serde_json::Value::as_str),
            Some("Plan the change")
        );
        assert_eq!(
            output
                .value
                .pointer("/content/0")
                .and_then(serde_json::Value::as_str),
            Some("First read the file")
        );
    }

    #[test]
    fn command_execution_item_becomes_turn_exec_command_artifact() {
        let item: ThreadItem = serde_json::from_value(serde_json::json!({
            "type": "commandExecution",
            "id": "exec-1",
            "command": "cargo test -p octopus-provider-codex",
            "cwd": "/work/repo",
            "processId": null,
            "source": "agent",
            "status": "completed",
            "commandActions": [],
            "aggregatedOutput": "test result: ok. 12 passed",
            "exitCode": 0,
            "durationMs": 1234
        }))
        .expect("a command-execution ThreadItem deserializes from its wire form");
        let output = rich_item_output(item).expect("an exec item is a rich turn item");
        assert_eq!(output.label, TURN_EXEC_COMMAND_LABEL);
        assert_eq!(
            output
                .value
                .get("command")
                .and_then(serde_json::Value::as_str),
            Some("cargo test -p octopus-provider-codex")
        );
        assert_eq!(
            output.value.get("cwd").and_then(serde_json::Value::as_str),
            Some("/work/repo")
        );
        assert_eq!(
            output
                .value
                .get("source")
                .and_then(serde_json::Value::as_str),
            Some("agent")
        );
        assert_eq!(
            output
                .value
                .get("status")
                .and_then(serde_json::Value::as_str),
            Some("completed")
        );
        assert_eq!(
            output
                .value
                .get("exit_code")
                .and_then(serde_json::Value::as_i64),
            Some(0)
        );
        assert_eq!(
            output
                .value
                .get("duration_ms")
                .and_then(serde_json::Value::as_i64),
            Some(1234)
        );
        assert_eq!(
            output
                .value
                .get("aggregated_output")
                .and_then(serde_json::Value::as_str),
            Some("test result: ok. 12 passed")
        );
    }

    #[test]
    fn dynamic_tool_lifecycle_is_projected_as_client_activity() {
        let started = CodexTurnOutput::dynamic_tool_started(
            "formed_interfaces".to_string(),
            "coder.exec__exact".to_string(),
            serde_json::json!({"cmd": "pwd"}),
            "call-7".to_string(),
            "thread-3".to_string(),
            "turn-9".to_string(),
        );
        assert_eq!(started.label, TURN_DYNAMIC_TOOL_CALL_LABEL);
        assert_eq!(started.route, ProviderEmissionRoute::ClientProjection);
        assert_eq!(started.value["call_id"], "call-7");
        assert_eq!(started.value["namespace"], "formed_interfaces");
        assert_eq!(started.value["tool"], "coder.exec__exact");
        assert_eq!(started.value["arguments"]["cmd"], "pwd");
        assert_eq!(started.value["status"], "started");

        let completed = CodexTurnOutput::dynamic_tool_completed(
            "formed_interfaces".to_string(),
            "coder.exec__exact".to_string(),
            "call-7".to_string(),
            "thread-3".to_string(),
            "turn-9".to_string(),
            true,
            Some(serde_json::json!({"output": {"exit_code": 0, "stdout": "ok"}})),
            None,
        );
        assert_eq!(completed.label, TURN_DYNAMIC_TOOL_CALL_LABEL);
        assert_eq!(completed.route, ProviderEmissionRoute::ClientProjection);
        assert_eq!(completed.value["call_id"], "call-7");
        assert_eq!(completed.value["namespace"], "formed_interfaces");
        assert_eq!(completed.value["status"], "completed");
        assert_eq!(completed.value["body"]["output"]["stdout"], "ok");

        let source = ArtifactRef {
            id: octopus_descriptors::ArtifactId::new("coder-result-1"),
            content: octopus_ledger::ContentHash::of_bytes(b"result"),
        };
        let interrupted = CodexTurnOutput::interrupted_result_closed(
            "tool_result",
            "call-7".to_string(),
            "thread-3".to_string(),
            "turn-9".to_string(),
            true,
            &source,
        );
        assert_eq!(interrupted.label, TURN_INTERRUPTED_RESULT_LABEL);
        assert_eq!(interrupted.value["correlation_id"], "call-7");
        assert_eq!(interrupted.value["returned_to_model"], false);
        assert_eq!(
            interrupted.value["result_disposition"],
            "durably_admitted_after_interrupt"
        );
        assert_eq!(interrupted.value["source_artifact"]["id"], "coder-result-1");
    }

    #[test]
    fn lifecycle_context_uses_exact_bound_and_session_artifact_refs() {
        let connection = ArtifactRef {
            id: octopus_descriptors::ArtifactId::new("lifecycle.connection:connected"),
            content: octopus_ledger::ContentHash::of_bytes(b"connection"),
        };
        let event = ArtifactRef {
            id: octopus_descriptors::ArtifactId::new("lifecycle.event:event-1"),
            content: octopus_ledger::ContentHash::of_bytes(b"event"),
        };
        let snapshot = ProviderInvocationSessionSnapshot {
            carrier: ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("session-context"),
                content: octopus_ledger::ContentHash::of_bytes(b"snapshot"),
            },
            artifacts: Arc::new(vec![
                SessionContextArtifact {
                    label: LIFECYCLE_CONNECTION_LABEL.to_string(),
                    payload: serde_json::json!({"label": LIFECYCLE_CONNECTION_LABEL}),
                    source_artifact: connection.clone(),
                    source_movement_seq: Some(7),
                },
                SessionContextArtifact {
                    label: LIFECYCLE_EVENT_LABEL.to_string(),
                    payload: serde_json::json!({"label": LIFECYCLE_EVENT_LABEL}),
                    source_artifact: event.clone(),
                    source_movement_seq: Some(8),
                },
                SessionContextArtifact {
                    label: "operator.message".to_string(),
                    payload: serde_json::json!({"label": "operator.message"}),
                    source_artifact: ArtifactRef {
                        id: octopus_descriptors::ArtifactId::new("operator-message"),
                        content: octopus_ledger::ContentHash::of_bytes(b"message"),
                    },
                    source_movement_seq: Some(9),
                },
            ]),
        };
        let payloads = lifecycle_context_payloads(&[], Some(&snapshot)).unwrap();
        assert_eq!(
            payloads
                .iter()
                .map(|payload| {
                    (
                        payload.artifact.id.as_str().to_string(),
                        payload.artifact.content.to_hex(),
                    )
                })
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                (
                    connection.id.as_str().to_string(),
                    connection.content.to_hex(),
                ),
                (event.id.as_str().to_string(), event.content.to_hex()),
            ])
        );
        assert!(lifecycle_payload_label(LIFECYCLE_DECISION_LABEL));
        assert!(lifecycle_payload_label(LIFECYCLE_HANDLER_OUTPUT_LABEL));
        assert!(!lifecycle_payload_label(TOOL_CALL_LABEL));

        let conflicting = ArtifactBindingPayload::new(connection, b"different".to_vec());
        assert!(lifecycle_context_payloads(&[conflicting], Some(&snapshot)).is_err());
    }

    #[test]
    fn isolated_turn_rejects_native_effect_items() {
        let exec: ThreadItem = serde_json::from_value(serde_json::json!({
            "type": "commandExecution",
            "id": "exec-9",
            "command": "cargo build -p octopus-runtime",
            "cwd": "/work/repo",
            "processId": null,
            "source": "agent",
            "status": "completed",
            "commandActions": [],
            "aggregatedOutput": "ok",
            "exitCode": 0,
            "durationMs": 10
        }))
        .expect("a command-execution ThreadItem deserializes");
        let patch: ThreadItem = serde_json::from_value(serde_json::json!({
            "type": "fileChange",
            "id": "patch-3",
            "status": "completed",
            "changes": []
        }))
        .expect("a file-change ThreadItem deserializes");
        assert!(forbidden_native_effect_item(&exec, WebSearchMode::Live, false));
        assert!(forbidden_native_effect_item(&patch, WebSearchMode::Live, false));
        assert!(forbidden_native_effect_item(&exec, WebSearchMode::Live, true));
        assert!(forbidden_native_effect_item(&patch, WebSearchMode::Live, true));
        let image: ThreadItem = serde_json::from_value(serde_json::json!({
            "type": "imageView", "id": "image-9", "path": "/work/repo/image.png",
        })).unwrap();
        for phase in ["started", "completed"] {
            let error = reject_native_effect_item(&image, phase, WebSearchMode::Live, false).unwrap_err().to_string();
            for expected in [phase, "imageView", "image-9"] { assert!(error.contains(expected)); }
        }
        assert!(forbidden_native_effect_item(&ThreadItem::Sleep {
            id: "sleep-1".into(),
            duration_ms: 100,
        }, WebSearchMode::Live, false));
        assert!(!forbidden_native_effect_item(&ThreadItem::Reasoning {
            id: "r-1".into(),
            summary: Vec::new(),
            content: Vec::new(),
        }, WebSearchMode::Live, false));
        assert!(!forbidden_native_effect_item(&ThreadItem::DynamicToolCall {
            id: "tool-1".into(),
            namespace: None,
            tool: "coder.exec".into(),
            arguments: serde_json::json!({"cmd": "pwd"}),
            status: octopus_appserver_proto::DynamicToolCallStatus::InProgress,
            content_items: None,
            success: None,
            duration_ms: None,
        }, WebSearchMode::Live, false));
        let search = ThreadItem::WebSearch {
            id: "web-1".into(), query: "example".into(), action: None, results: None,
        };
        assert!(forbidden_native_effect_item(&search, WebSearchMode::Disabled, false));
        assert!(!forbidden_native_effect_item(&search, WebSearchMode::Live, false));
        for phase in ["started", "completed"] {
            for tool in ["list_mcp_resources", "list_mcp_resource_templates"] {
                let value = serde_json::json!({"type":"mcpToolCall","id":"resource-1","server":"codex","tool":tool,"arguments":{},"status":if phase == "started" { "inProgress" } else { "completed" }});
                let item: ThreadItem = serde_json::from_value(value.clone()).unwrap();
                assert!(reject_native_effect_item(&item, phase, WebSearchMode::Live, false).is_err());
                assert!(reject_native_effect_item(&item, phase, WebSearchMode::Live, true).is_ok());
                for (server, name, args) in [
                    ("foreign", tool, serde_json::json!({})),
                    ("codex", "arbitrary_server_tool", serde_json::json!({})),
                    ("codex", "read_mcp_resource", serde_json::json!({"server":"codex","uri":"resource://private"})),
                    ("codex", tool, serde_json::json!({"server":"foreign"})),
                    ("codex", tool, serde_json::json!({"cursor":"next"})),
                    ("codex", tool, serde_json::Value::Null),
                ] {
                    let mut refused = value.clone();
                    refused["server"] = serde_json::json!(server); refused["tool"] = serde_json::json!(name); refused["arguments"] = args;
                    let refused: ThreadItem = serde_json::from_value(refused).unwrap();
                    assert!(reject_native_effect_item(&refused, phase, WebSearchMode::Live, true).is_err());
                }
            }
        }
    }


    #[test]
    fn exec_source_and_status_render_client_wire_words() {
        assert_eq!(exec_source_wire(CommandExecutionSource::Agent), "agent");
        assert_eq!(
            exec_source_wire(CommandExecutionSource::UserShell),
            "userShell"
        );
        assert_eq!(
            exec_source_wire(CommandExecutionSource::UnifiedExecStartup),
            "unifiedExecStartup"
        );
        assert_eq!(
            exec_source_wire(CommandExecutionSource::UnifiedExecInteraction),
            "unifiedExecInteraction"
        );
        assert_eq!(
            exec_status_wire(CommandExecutionStatus::InProgress),
            "in_progress"
        );
        assert_eq!(
            exec_status_wire(CommandExecutionStatus::Completed),
            "completed"
        );
        assert_eq!(exec_status_wire(CommandExecutionStatus::Failed), "failed");
        assert_eq!(
            exec_status_wire(CommandExecutionStatus::Declined),
            "declined"
        );
    }

    #[test]
    fn plan_item_becomes_turn_plan_update_artifact() {
        let item = ThreadItem::Plan {
            id: "plan-1".into(),
            text: "1. read\n2. edit\n3. test".into(),
        };
        let output = rich_item_output(item).expect("a plan item is a rich turn item");
        assert_eq!(output.label, TURN_PLAN_UPDATE_LABEL);
        assert_eq!(
            output
                .value
                .get("item_id")
                .and_then(serde_json::Value::as_str),
            Some("plan-1")
        );
        assert!(
            output
                .value
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .contains("2. edit")
        );
        assert_eq!(output.value["plan_kind"], "proposed_plan");
        assert_eq!(output.value["model_stream_authority"], "completed_item");
        assert_eq!(output.value["execution_authority"], "octopus_checkpoints");
    }

    #[test]
    fn turn_plan_update_notification_becomes_plan_artifact() {
        use octopus_appserver_proto::TurnPlanStepStatus;
        let plan = TurnPlanUpdatedNotification {
            thread_id: "t1".into(),
            turn_id: "u1".into(),
            explanation: Some("Here is the plan".into()),
            plan: vec![
                TurnPlanStep {
                    step: "read the file".into(),
                    status: TurnPlanStepStatus::Completed,
                },
                TurnPlanStep {
                    step: "edit it".into(),
                    status: TurnPlanStepStatus::InProgress,
                },
                TurnPlanStep {
                    step: "run tests".into(),
                    status: TurnPlanStepStatus::Pending,
                },
            ],
        };
        let output = plan_update_output(&plan);
        assert_eq!(output.label, TURN_PLAN_UPDATE_LABEL);
        assert_eq!(
            output
                .value
                .get("item_id")
                .and_then(serde_json::Value::as_str),
            Some("u1"),
            "the turn id is the stable plan cell id (the plan updates in place)"
        );
        let text = output
            .value
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(text.contains("Here is the plan"));
        assert!(text.contains("[x] read the file"));
        assert!(text.contains("[~] edit it"));
        assert!(text.contains("[ ] run tests"));
        assert_eq!(output.value["plan_kind"], "agent_checklist");
        assert_eq!(output.value["explanation"], "Here is the plan");
        assert_eq!(output.value["steps"][1]["status"], "inProgress");
        assert_eq!(output.value["execution_authority"], "octopus_checkpoints");
    }

    #[test]
    fn action_less_web_search_still_maps() {
        let bare = rich_item_output(ThreadItem::WebSearch {
            results: None,
            id: "ws-2".into(),
            query: "bare".into(),
            action: None,
        })
        .expect("an action-less web search still maps");
        assert_eq!(bare.label, TURN_WEB_SEARCH_LABEL);
        assert!(
            bare.value
                .get("action")
                .expect("action field present")
                .is_null()
        );
    }

    #[test]
    fn unmapped_items_are_skipped_not_fabricated() {
        assert!(
            rich_item_output(ThreadItem::AgentMessage {
                id: "msg-1".into(),
                text: "hello".into(),
                phase: None,
            })
            .is_none(),
            "the assistant message is the terminal reply, not a rich-item artifact"
        );
    }

    #[test]
    fn user_message_echo_is_not_duplicated_as_provider_activity() {
        assert!(
            rich_item_output(ThreadItem::UserMessage {
                id: "um-1".into(),
                client_id: None,
                content: vec![UserInput::Text {
                    text: "steer me".into(),
                    text_elements: Vec::new(),
                }],
            })
            .is_none(),
            "the inbound operator artifact is already the user-visible evidence"
        );
    }

    #[test]
    fn only_a_completed_turn_may_emit_a_terminal_reply() {
        let failure = octopus_appserver_proto::TurnError {
            message: "upstream rejected the turn".to_string(),
            codex_error_info: None,
            additional_details: Some("request id 42".to_string()),
        };
        assert!(terminal_turn_may_emit_reply(&TurnStatus::Completed, None).unwrap());
        assert!(!terminal_turn_may_emit_reply(&TurnStatus::Interrupted, None).unwrap());
        assert!(matches!(
            terminal_turn_may_emit_reply(&TurnStatus::Failed, Some(&failure)),
            Err(InterfaceDriverError::FailedClosed(message))
                if message.contains("upstream rejected the turn")
                    && message.contains("request id 42")
        ));
        assert!(matches!(
            terminal_turn_may_emit_reply(&TurnStatus::InProgress, None),
            Err(InterfaceDriverError::Protocol(_))
        ));
    }

    #[test]
    fn unauthorized_error_is_terminal_even_when_upstream_marks_it_retryable() {
        let unauthorized = octopus_appserver_proto::ErrorNotification {
            error: octopus_appserver_proto::TurnError {
                message: "expired credentials".to_string(),
                codex_error_info: Some(octopus_appserver_proto::CodexErrorInfo::Unauthorized),
                additional_details: None,
            },
            will_retry: true,
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
        };
        let output = turn_error_output(&unauthorized);
        assert_eq!(output.value["will_retry"], false);
        assert_eq!(
            output.value["failure_code"],
            PROVIDER_AUTHENTICATION_FAILURE_CODE
        );
        assert!(provider_error_is_terminal(&unauthorized));

        let transient = octopus_appserver_proto::ErrorNotification {
            error: octopus_appserver_proto::TurnError {
                message: "temporary upstream overload".to_string(),
                codex_error_info: None,
                additional_details: None,
            },
            will_retry: true,
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
        };
        assert!(!provider_error_is_terminal(&transient));
        assert_eq!(turn_error_output(&transient).value["will_retry"], true);
    }

    #[test]
    fn steer_ownership_defers_only_when_app_server_proves_the_target_turn_moved() {
        let complete = |application| match application {
            SteerAttemptApplication::Complete(application) => application,
            SteerAttemptApplication::Retarget { .. } => panic!("expected completed steer attempt"),
        };
        let accepted = complete(steer_attempt_application_with_retarget(
            "message-1".to_string(),
            "thread-1".to_string(),
            "turn-1".to_string(),
            Ok(TurnSteerResponse {
                turn_id: "turn-1".to_string(),
            }),
            true,
        ));
        assert_eq!(accepted.disposition, CodexTurnControlDisposition::Applied);
        assert!(accepted.result.acknowledged);

        let missing = complete(steer_attempt_application_with_retarget(
            "message-1".to_string(),
            "thread-1".to_string(),
            "turn-1".to_string(),
            Err(TypedRequestError::Server {
                method: "turn/steer".to_string(),
                source: octopus_appserver_proto::JSONRPCErrorError {
                    code: -32600,
                    data: None,
                    message: "no active turn to steer".to_string(),
                },
            }),
            true,
        ));
        assert_eq!(
            missing.disposition,
            CodexTurnControlDisposition::DeferredTurn(CodexDeferredTurnCause::TerminalTurnMoved)
        );
        assert_eq!(missing.result.detail["reason"], "no_active_turn");

        let mismatch = steer_attempt_application_with_retarget(
            "message-1".to_string(),
            "thread-1".to_string(),
            "turn-1".to_string(),
            Err(TypedRequestError::Server {
                method: "turn/steer".to_string(),
                source: octopus_appserver_proto::JSONRPCErrorError {
                    code: -32600,
                    data: None,
                    message: "expected active turn id `turn-1` but found `turn-2`".to_string(),
                },
            }),
            true,
        );
        assert!(matches!(
            mismatch,
            SteerAttemptApplication::Retarget { actual_turn_id } if actual_turn_id == "turn-2"
        ));
        let repeated_mismatch = complete(steer_attempt_application_with_retarget(
            "message-1".to_string(),
            "thread-1".to_string(),
            "turn-2".to_string(),
            Err(TypedRequestError::Server {
                method: "turn/steer".to_string(),
                source: octopus_appserver_proto::JSONRPCErrorError {
                    code: -32600,
                    data: None,
                    message: "expected active turn id `turn-2` but found `turn-3`".to_string(),
                },
            }),
            false,
        ));
        assert_eq!(
            repeated_mismatch.disposition,
            CodexTurnControlDisposition::Applied
        );
        assert_eq!(
            repeated_mismatch.result.detail["retarget_retry_exhausted"],
            true
        );

        let terminal = complete(steer_attempt_application_with_retarget(
            "message-1".to_string(),
            "thread-1".to_string(),
            "turn-1".to_string(),
            Err(TypedRequestError::Server {
                method: "turn/steer".to_string(),
                source: octopus_appserver_proto::JSONRPCErrorError {
                    code: -32600,
                    data: None,
                    message: "cannot steer a review turn".to_string(),
                },
            }),
            true,
        ));
        assert_eq!(terminal.disposition, CodexTurnControlDisposition::Applied);
        assert!(!terminal.result.acknowledged);

        let invalid_ack = complete(steer_attempt_application_with_retarget(
            "message-1".to_string(),
            "thread-1".to_string(),
            "turn-1".to_string(),
            Ok(TurnSteerResponse {
                turn_id: "turn-2".to_string(),
            }),
            true,
        ));
        assert_eq!(
            invalid_ack.disposition,
            CodexTurnControlDisposition::Applied
        );
        assert!(!invalid_ack.result.acknowledged);

        let request_ids = std::sync::atomic::AtomicI64::new(7);
        let ClientRequest::TurnSteer { params, .. } =
            turn_steer_request(&request_ids, "thread-1", "message-1", Vec::new(), "turn-2")
        else {
            panic!("expected turn/steer request")
        };
        assert_eq!(params.client_user_message_id.as_deref(), Some("message-1"));
        assert_eq!(params.expected_turn_id, "turn-2");
    }

    #[tokio::test]
    async fn suspended_stop_requires_exact_terminal_proof_and_preserves_late_result() {
        use futures::{SinkExt, StreamExt};
        use octopus_appserver_proto::{Thread, ThreadStatus, SessionSource};
        for (with_stop, cold, mode) in [
            (true, false, "completed"), (true, false, "completed_no_events"),
            (true, false, "drained"), (true, false, "drained_conflict"),
            (true, true, "completed_no_events"), (true, true, "wrong_thread"), (true, true, "live"),
            (true, false, "wrong_thread"), (true, false, "wrong_turn"), (true, false, "absent"),
            (true, false, "live"), (true, false, "read_error"), (true, false, "wrong_item_turn"),
            (false, false, "completed"), (false, false, "completed_no_events"), (false, true, "completed_no_events"),
            (false, false, "wrong_thread"), (false, false, "wrong_turn"), (false, false, "absent"),
            (false, false, "live"), (false, false, "read_error"), (false, false, "wrong_item_turn"),
        ] {
            let draining = matches!(mode, "drained" | "drained_conflict");
            let terminal_case = matches!(mode, "completed" | "completed_no_events") || draining;
            let temp = tempfile::tempdir().unwrap();
            let socket_path = temp.path().join("terminal.sock");
            let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
            let turn = octopus_appserver_proto::Turn {
                id: if mode == "wrong_turn" { "other-turn" } else { "turn-a" }.into(),
                items: vec![ThreadItem::Reasoning { id: "reason-a".into(), summary: vec!["Actual progress".into()], content: vec![] },
                    ThreadItem::AgentMessage { id: "final-a".into(), text: "Already produced native answer.".into(), phase: Some(MessagePhase::FinalAnswer) }],
                items_view: TurnItemsView::Full,
                status: match mode { "live" => TurnStatus::InProgress, "drained" | "drained_conflict" => TurnStatus::Interrupted, _ => TurnStatus::Completed },
                error: None, started_at: Some(10), completed_at: Some(20), duration_ms: Some(10_000),
            };
            let thread = Thread {
                id: if mode == "wrong_thread" { "other-thread" } else { "thread-a" }.into(), session_id: "thread-a".into(),
                forked_from_id: None, parent_thread_id: None, preview: String::new(), ephemeral: false, model_provider: "test".into(),
                created_at: 10, updated_at: 20, recency_at: Some(20), status: ThreadStatus::Idle, path: None,
                cwd: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(temp.path()).unwrap(), cli_version: "test".into(),
                source: SessionSource::Exec, thread_source: None, agent_nickname: None, agent_role: None, git_info: None, name: None,
                turns: vec![],
            };
            let server = tokio::spawn(async move {
                let mut socket = tokio_tungstenite::accept_async(listener.accept().await.unwrap()).await.unwrap();
                let octopus_appserver_proto::JSONRPCMessage::Request(init) = next_test_rpc(&mut socket).await else { panic!("initialize") };
                send_test_result(&mut socket, init.id, serde_json::json!({"userAgent":"codex/test","codexHome":"/tmp"})).await;
                assert!(matches!(next_test_rpc(&mut socket).await, octopus_appserver_proto::JSONRPCMessage::Notification(_)));
                if with_stop {
                    let octopus_appserver_proto::JSONRPCMessage::Request(stop) = next_test_rpc(&mut socket).await else { panic!("interrupt") };
                    assert_eq!(stop.method, "turn/interrupt");
                    assert_eq!(stop.params.as_ref().unwrap()["turnId"], "turn-a");
                    if draining { send_test_result(&mut socket, stop.id, serde_json::json!({})).await; }
                    else { send_test_error(&mut socket, stop.id, if terminal_case { "interrupt refused" } else { "no active turn to interrupt" }).await; }
                }
                if mode == "completed" || draining {
                    let mut notification_turn = turn.clone();
                    if mode == "drained_conflict" { notification_turn.status = TurnStatus::Failed; }
                    for frame in [
                        serde_json::json!({"method":"turn/completed","params":{"threadId":"other-thread","turn":turn}}),
                        serde_json::json!({"id":"abandoned","method":"item/tool/call","params":{"threadId":"thread-a","turnId":"turn-a","callId":"abandoned","namespace":"formed_interfaces","tool":"coder-dexec","arguments":{"command":"never execute"}}}),
                        serde_json::json!({"id":"abandoned-two","method":"item/tool/call","params":{"threadId":"thread-a","turnId":"turn-a","callId":"abandoned-two","namespace":"formed_interfaces","tool":"coder-dexec","arguments":{"command":"never execute"}}}),
                        serde_json::json!({"method":"item/completed","params":{"threadId":"thread-a","turnId":"turn-a","item":turn.items[1],"completedAtMs":20_000}}),
                        serde_json::json!({"method":"turn/completed","params":{"threadId":"thread-a","turn":notification_turn}}),
                    ] {
                        socket.send(tokio_tungstenite::tungstenite::Message::Text(frame.to_string().into())).await.unwrap();
                    }
                }
                let octopus_appserver_proto::JSONRPCMessage::Request(read) = next_test_rpc(&mut socket).await else { panic!("thread/read") };
                assert_eq!(read.method, "thread/read");
                assert_eq!(read.params.as_ref().unwrap(), &serde_json::json!({"threadId":"thread-a"}));
                if mode == "read_error" {
                    send_test_error(&mut socket, read.id, "read unavailable").await;
                } else {
                    send_test_result(&mut socket, read.id, serde_json::to_value(ThreadReadResponse { thread }).unwrap()).await;
                    if mode != "wrong_thread" {
                        let mut metadata = serde_json::to_value(&turn).unwrap();
                        metadata["items"] = serde_json::json!([]);
                        metadata["itemsView"] = serde_json::json!("notLoaded");
                        let mut newer = metadata.clone(); newer["id"] = serde_json::json!("newer-turn");
                        for (cursor, data, next) in [
                            (None, vec![newer], Some("older")),
                            (Some("older"), if mode == "absent" { vec![] } else { vec![metadata] }, None),
                        ] {
                            let JSONRPCMessage::Request(page) = next_test_rpc(&mut socket).await else { panic!("turn page") };
                            assert_eq!(page.method, "thread/turns/list");
                            assert_eq!(page.params.unwrap(), serde_json::json!({
                                "threadId":"thread-a", "cursor":cursor, "limit":100,
                                "sortDirection":"desc", "itemsView":"notLoaded",
                            }));
                            send_test_result(&mut socket, page.id, serde_json::json!({
                                "data":data, "nextCursor":next, "backwardsCursor":null,
                            })).await;
                        }
                        if !matches!(mode, "absent" | "wrong_turn" | "live") {
                            for (index, item) in turn.items.iter().enumerate() {
                                let JSONRPCMessage::Request(page) = next_test_rpc(&mut socket).await else { panic!("item page") };
                                assert_eq!(page.method, "thread/items/list");
                                assert_eq!(page.params.unwrap(), serde_json::json!({
                                    "threadId":"thread-a", "turnId":"turn-a", "limit":100,
                                    "cursor": if index == 0 { None } else { Some("last") }, "sortDirection":"asc",
                                }));
                                send_test_result(&mut socket, page.id, serde_json::json!({
                                    "data":[{"turnId":if mode == "wrong_item_turn" { "wrong" } else { "turn-a" }, "item":item}],
                                    "nextCursor": if index == 0 { Some("last") } else { None }, "backwardsCursor":null,
                                })).await;
                                if mode == "wrong_item_turn" { break; }
                            }
                        }
                    }
                }
                while let Some(Ok(frame)) = socket.next().await {
                    if let tokio_tungstenite::tungstenite::Message::Text(frame) = frame {
                        panic!("closed native turn received a response or continuation: {frame}");
                    }
                }
            });
            let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
                endpoint: crate::RemoteAppServerEndpoint::UnixSocket { socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(&socket_path).unwrap() },
                client_name: "octopus-test".into(), client_version: "0".into(), experimental_api: false,
                mcp_server_openai_form_elicitation: false, opt_out_notification_methods: vec![], channel_capacity: 16,
            }).await.unwrap();
            let driver_config = InterfaceDriverConfig { private_config: serde_json::json!({"native_tool_namespace":{"name":"formed_interfaces","description":"tools"}}).as_object().unwrap().clone(), ..InterfaceDriverConfig::default() };
            let driver = CodexAppServerInterfaceDriver::new_with_config(AppServerClient::Remote(client), Some(&driver_config));
            let scope = TransportStateScope::Run("run-a".into());
            let gate = Arc::new(tokio::sync::Mutex::new(()));
            let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let pending = PendingServerRequest {
                request_id: RequestId::String("pending".into()), thread_id: "thread-a".into(), turn_id: "turn-a".into(), key: "tool:call-a".into(),
                kind: PendingServerRequestKind::DynamicTool { call_id: "call-a".into(), tool: "coder.exec".into() },
                effect_gate: gate.clone(), turn_interrupted: interrupted.clone(),
            };
            let mut original = continuity("world_provider_codex", "run-a", "thread-a", 1, Some(CodexPendingContinuity::DynamicTool {
                turn_id: "turn-a".into(), call_id: "call-a".into(), tool: "coder.exec".into(), native_operation: None,
            }));
            original.turn_input = Some(bound("original-user-message", serde_json::json!({"text":"perform the task"})).artifact);
            if !cold { driver.record_drain_outcome(&scope, TurnDrainOutcome::Suspended(pending.clone())).await.unwrap(); }
            driver.transport_state.lock().await.insert(scope.clone(), TransportState {
                context_generation: 1, thread_id: Some("thread-a".into()), thread_shape_fingerprint: Some(original.thread_shape_fingerprint.clone()),
                static_context_fingerprint: Some(original.static_context_fingerprint.clone()), hooks_catalog_emitted: true, rollout_path: None,
            });
            let (emit, mut outputs) = ProviderOutputStream::channel();
            emit.continuity.lock().unwrap().continuity = Some(original.clone());
            let control = CodexTurnControlBroker::new(Some(7));
            let barrier = ProviderHandoffBarrier::new();
            control.publish(CodexTurnControlTarget::Suspended {
                handle: driver.client.lock().await.request_handle(), request_ids: driver.next_request_id.clone(),
                pending_requests: driver.pending_server_requests.clone(), scope: scope.clone(), output: Arc::downgrade(&emit),
                continuity: Some(original.clone()), handoff_barrier: barrier.clone(), effect_gate: gate.clone(), turn_interrupted: interrupted.clone(),
                thread_id: "thread-a".into(), turn_id: "turn-a".into(),
            });
            if cold && with_stop { driver.restore_pending_control(&control, original.clone()).await; }
            let mut applied = if with_stop { Some(tokio::time::timeout(std::time::Duration::from_secs(3), control.apply(
                CodexTurnControl::Interrupt { control_id: "stop-a".into(), action: "stop".into() }, |_| Ok(()),
            )).await.unwrap().unwrap()) } else { None };
            if let Some(applied) = applied.as_mut() {
                assert_eq!(applied.result.acknowledged, terminal_case, "{mode}");
                if !cold { assert_eq!(driver.pending_request(&scope, "tool:call-a").await.unwrap().unwrap().request_id, pending.request_id); }
                drop(applied.effect_guard.take());
            }
            if cold && with_stop {
                let applied = applied.unwrap();
                assert!(!driver.has_pending_request(&scope).await);
                if terminal_case {
                    let recovered = applied.result.continuity.unwrap();
                    assert_eq!(recovered.interrupted_pending, original.pending);
                    assert_eq!(recovered.context_generation, original.context_generation + 1);
                    assert!(control.has_interrupted_pending());
                    applied.durable_interrupt_handoff.unwrap().finish(Ok(()));
                    control.prepare_recovered_result().await.unwrap();
                    assert!(control.has_interrupted_pending());
                } else { assert!(control.is_recovered_pending()); }
                assert!(outputs.try_recv().is_err());
                driver.shutdown().await.unwrap(); server.await.unwrap();
                continue;
            }
            if terminal_case || !with_stop {
                let recovered = if let Some(applied) = applied {
                    if draining { assert_eq!(applied.result.detail["interrupted"], true); }
                    else { assert_eq!(applied.result.detail["already_terminal"], true); assert!(applied.result.detail.get("interrupted").is_none()); }
                    let recovered = applied.result.continuity.unwrap();
                    assert_eq!(recovered.interrupted_pending, original.pending);
                    applied.durable_interrupt_handoff.unwrap().finish(Ok(()));
                    recovered
                } else { original.clone() };
                assert_eq!(recovered.context_generation, original.context_generation + u64::from(with_stop));
                let labels = codex_context_labels(); let projected = codex_client_projection_labels(); let rules = live_stream_rules();
                let streams = temp.path().join("streams"); let provider = InterfaceId::new("world_provider_codex");
                let sink = TurnSink { emit: &emit, output_root: temp.path(), live_stream_root: &streams, provider_interface: &provider,
                    run: "run-a", source_movement_seq: 8, attempt: 1, context_output_labels: &labels, client_projection_output_labels: &projected,
                    live_stream_rules: &rules, live_stream: std::sync::Mutex::new(None), continuity: emit.continuity.clone() };
                let result = ToolResultArtifact::failure("coder.exec", Some("call-a".into()), octopus_tool_handlers::ToolResultError {
                    code: "tool_effect_indeterminate".into(), message: "original effect outcome is uncertain".into(), details: None,
                });
                let source = bound("exact-late-result", serde_json::to_value(&result).unwrap());
                let operations = BTreeMap::new(); let tools = BTreeMap::new();
                let context = TurnContext { run_invocation: None, scope: &scope, sink: &sink, control: &control, interface_operations: &operations,
                    tool_names: &tools, source_artifact: &source.artifact, handoff_barrier: &barrier, effect_gate: &gate, turn_interrupted: &interrupted,
                    lifecycle: None, lifecycle_prompt: None };
                if cold || !terminal_case {
                    let checked = tokio::time::timeout(std::time::Duration::from_secs(3), driver.complete_native_terminal_result(
                        context, &recovered, (!cold).then_some(&pending),
                        CodexTurnOutput::dynamic_tool_completed_after_interrupt("formed_interfaces".into(), result.tool.clone(), "call-a".into(), "thread-a".into(), "turn-a".into(), result.ok, result.body.clone(), result.error.clone()),
                        false, CodexTurnOutput::interrupted_result_closed("tool_result", "call-a".into(), "thread-a".into(), "turn-a".into(), result.ok, &source.artifact),
                        false,
                    )).await.unwrap().map(|checked| checked.settled());
                    if !terminal_case {
                        if mode == "live" { assert!(!checked.unwrap()); } else { assert!(checked.is_err(), "{mode}"); }
                        assert_eq!(emit.continuity_snapshot().unwrap(), original);
                        assert_eq!(driver.pending_request(&scope, "tool:call-a").await.unwrap().unwrap().request_id, pending.request_id);
                        assert!(outputs.try_recv().is_err());
                        driver.shutdown().await.unwrap(); server.await.unwrap();
                        continue;
                    }
                    assert!(checked.unwrap());
                } else {
                    let result = tokio::time::timeout(std::time::Duration::from_secs(3), driver.resume_tool_result(
                        context, result.clone(), Some(&recovered), recovered.context_generation, false,
                    )).await.unwrap();
                    if mode == "drained_conflict" {
                        assert!(result.unwrap_err().to_string().contains("native terminal notification conflicts"));
                        assert!(outputs.try_recv().is_err()); driver.shutdown().await.unwrap(); server.await.unwrap(); continue;
                    }
                    assert!(result.unwrap());
                }
                let ProviderStreamEmission::InterruptedResult { stream: Some(stream), retained_outputs, closure, .. } = outputs.try_recv().unwrap() else { panic!("exact closure group") };
                let closure_bytes = closure.artifact.read_payload_bytes().unwrap();
                let closed: serde_json::Value = serde_json::from_slice(&closure_bytes).unwrap();
                assert_eq!(closed["ok"], false); assert_eq!(closed["turn_interrupted"], draining);
                assert_eq!(closed["result_disposition"], "durably_admitted_after_native_terminal");
                assert_eq!(closed["codex_continuity"]["context_generation"], original.context_generation + 2 + u64::from(with_stop));
                assert!(interrupted_result_closure_payload(TURN_INTERRUPTED_RESULT_LABEL, &closure_bytes, &recovered, &source.artifact, &labels).unwrap().is_some());
                if draining {
                    assert!(recovered.native_terminal.is_none());
                    for field in ["thread_id", "context_generation", "interrupted_pending"] {
                        let mut forged = closed.clone(); forged["native_terminal_source_continuity"][field] = serde_json::Value::Null;
                        assert!(interrupted_result_closure_payload(TURN_INTERRUPTED_RESULT_LABEL, &serde_json::to_vec(&forged).unwrap(), &recovered, &source.artifact, &labels).is_err());
                    }
                }
                for retained in closed["retained_native_outputs"].as_array().unwrap() {
                    assert_eq!(retained["in_reply_to"], serde_json::to_value(&original.turn_input).unwrap());
                }
                for index in 0..closed["retained_native_outputs"].as_array().unwrap().len() {
                    let mut forged = closed.clone();
                    forged["retained_native_outputs"][index]["in_reply_to"]["id"] = serde_json::json!("other-user-message");
                    assert!(retained_native_output_payloads(&forged, &labels).is_err());
                    let mut missing = closed.clone();
                    missing["retained_native_outputs"][index].as_object_mut().unwrap().remove("in_reply_to");
                    assert!(retained_native_output_payloads(&missing, &labels).is_err());
                }
                assert_eq!(retained_outputs.len(), 2);
                let answer: serde_json::Value = serde_json::from_slice(&retained_outputs[0].artifact.read_payload_bytes().unwrap()).unwrap();
                assert_eq!(answer["text"], "Already produced native answer."); assert_eq!(answer["native_completed_at"], 20);
                let completion: serde_json::Value = serde_json::from_slice(&retained_outputs[1].artifact.read_payload_bytes().unwrap()).unwrap();
                assert_eq!(completion["turn"]["items"][0]["summary"][0], "Actual progress");
                if mode == "completed" || draining {
                    assert_eq!(completion["retained_native_events"].as_array().unwrap().iter().filter_map(|event| event["quarantined_server_request"]["id"].as_str()).collect::<Vec<_>>(), ["abandoned", "abandoned-two"]);
                } else {
                    assert!(completion["retained_native_events"].as_array().unwrap().is_empty());
                }
                let rows = String::from_utf8(stream.artifact.read_payload_bytes().unwrap()).unwrap();
                let receipt = rows.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .find(|row| row["label"] == TURN_DYNAMIC_TOOL_CALL_LABEL).unwrap();
                assert_eq!(receipt["payload"]["error"], serde_json::to_value(&result.error).unwrap());
                assert_eq!(receipt["payload"]["returned_to_model"], false);
                let replay = recover_retained_native_outputs(&closure.artifact, temp.path(), &labels).unwrap();
                for (original, replay) in retained_outputs.iter().zip(replay) { assert_eq!(original.artifact.read_payload_bytes().unwrap(), replay.artifact.read_payload_bytes().unwrap()); }
                if mode == "completed" || draining {
                    let Some(AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(foreign))) = driver.client.lock().await.next_event().await else { panic!("unrelated event retained") };
                    assert_eq!(foreign.thread_id, "other-thread");
                } else {
                    assert!(driver.client.lock().await.take_ready_events().is_empty());
                }
                assert!(!driver.has_pending_request(&scope).await); assert!(outputs.try_recv().is_err());
            } else {
                let applied = applied.unwrap();
                assert!(applied.result.continuity.is_none()); assert_eq!(emit.continuity_snapshot().unwrap(), original);
                assert!(!interrupted.load(std::sync::atomic::Ordering::Acquire));
                assert!(applied.result.detail.get("already_terminal").is_none());
            }
            driver.shutdown().await.unwrap(); server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn steer_retargets_once_with_the_same_operator_message_identity() {
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("steer-retarget.sock");
        let mut listener = crate::uds::UnixListener::bind(&socket_path).await.unwrap();
        let server = tokio::spawn(async move {
            use futures::StreamExt;

            let stream = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let initialize = next_test_rpc(&mut socket).await;
            let octopus_appserver_proto::JSONRPCMessage::Request(initialize) = initialize else {
                panic!("expected initialize request")
            };
            send_test_result(
                &mut socket,
                initialize.id,
                serde_json::json!({"userAgent": "codex/test", "codexHome": "/tmp"}),
            )
            .await;
            assert!(matches!(
                next_test_rpc(&mut socket).await,
                octopus_appserver_proto::JSONRPCMessage::Notification(_)
            ));

            let first = next_test_rpc(&mut socket).await;
            let octopus_appserver_proto::JSONRPCMessage::Request(first) = first else {
                panic!("expected first turn/steer request")
            };
            assert_eq!(first.method, "turn/steer");
            let first_params = first.params.as_ref().unwrap();
            assert_eq!(first_params["threadId"], "thread-1");
            assert_eq!(first_params["expectedTurnId"], "turn-1");
            assert_eq!(first_params["clientUserMessageId"], "message-1");
            assert_eq!(first_params["input"][0]["text"], "keep going");
            send_test_error(
                &mut socket,
                first.id,
                "expected active turn id `turn-1` but found `turn-2`",
            )
            .await;

            let second = next_test_rpc(&mut socket).await;
            let octopus_appserver_proto::JSONRPCMessage::Request(second) = second else {
                panic!("expected retargeted turn/steer request")
            };
            assert_eq!(second.method, "turn/steer");
            let second_params = second.params.as_ref().unwrap();
            assert_eq!(second_params["threadId"], "thread-1");
            assert_eq!(second_params["expectedTurnId"], "turn-2");
            assert_eq!(second_params["clientUserMessageId"], "message-1");
            assert_eq!(second_params["input"], first_params["input"]);
            send_test_result(
                &mut socket,
                second.id,
                serde_json::json!({"turnId": "turn-2"}),
            )
            .await;
            for accepted in [true, false] {
                let octopus_appserver_proto::JSONRPCMessage::Request(request) = next_test_rpc(&mut socket).await else { panic!("pending steer") };
                assert_eq!(request.method, "turn/steer");
                assert_eq!(request.params.as_ref().unwrap()["expectedTurnId"], "turn-2");
                if accepted { send_test_result(&mut socket, request.id, serde_json::json!({"turnId":"turn-2"})).await; }
                else { send_test_error(&mut socket, request.id, "expected active turn id `turn-2` but found `turn-3`").await; }
            }
            while socket.next().await.is_some() {}
        });
        let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
            endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
                socket_path: octopus_util_abspath::AbsolutePathBuf::from_absolute_path(
                    &socket_path,
                )
                .unwrap(),
            },
            client_name: "octopus-test".to_string(),
            client_version: "0".to_string(),
            experimental_api: false,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 8,
        })
        .await
        .unwrap();
        let request_handle = AppServerRequestHandle::Remote(client.request_handle());
        let mut retargets = Vec::new();
        let application = apply_control_to_target(
            CodexTurnControl::Steer {
                control_id: "message-1".to_string(),
                source_artifact: None,
                input: vec![UserInput::Text {
                    text: "keep going".to_string(),
                    text_elements: Vec::new(),
                }],
            },
            request_handle,
            Arc::new(std::sync::atomic::AtomicI64::new(1)),
            "thread-1".to_string(),
            "turn-1".to_string(),
            None,
            &mut |intent| {
                let CodexTurnControlEffectIntent::Retarget {
                    expected_turn_id,
                    actual_turn_id,
                } = intent
                else {
                    panic!("the broker owns the initial intent")
                };
                retargets.push((expected_turn_id, actual_turn_id));
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(retargets, [("turn-1".to_string(), "turn-2".to_string())]);
        assert_eq!(
            application.disposition,
            CodexTurnControlDisposition::Applied
        );
        assert!(application.result.acknowledged);
        assert_eq!(application.result.turn_id.as_deref(), Some("turn-2"));
        let scope = TransportStateScope::Run("run-a".into());
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let interrupted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let pending = PendingServerRequest { request_id: RequestId::String("request-a".into()), thread_id:"thread-1".into(), turn_id:"turn-2".into(),
            key:"tool:call-a".into(), kind:PendingServerRequestKind::DynamicTool { call_id:"call-a".into(), tool:"coder.exec".into() },
            effect_gate:gate.clone(), turn_interrupted:interrupted.clone() };
        let state = continuity("world_provider_codex", "run-a", "thread-1", 4, Some(CodexPendingContinuity::DynamicTool {
            native_operation:None, turn_id:"turn-2".into(), call_id:"call-a".into(), tool:"coder.exec".into(),
        }));
        let requests:PendingServerRequests=Arc::new(Mutex::new(BTreeMap::from([((scope.clone(),pending.key.clone()),pending.clone())])));
        let broker=CodexTurnControlBroker::new(Some(4));
        let barrier=ProviderHandoffBarrier::new(); barrier.finish(Ok(()));
        let steer=||CodexTurnControl::Steer { control_id:"pending-message".into(), input:vec![UserInput::Text{text:"pending clarification".into(),text_elements:vec![]}], source_artifact:None };
        for (index,bad) in [Some("thread"),Some("key"),Some("gate"),Some("interrupted"),None,None].into_iter().enumerate() {
            let mut live=pending.clone();
            if bad==Some("thread") {live.thread_id="foreign".into();}
            if bad==Some("key") {live.key="tool:wrong".into();}
            if bad==Some("gate") {live.effect_gate=Arc::new(tokio::sync::Mutex::new(()));}
            interrupted.store(bad==Some("interrupted"),std::sync::atomic::Ordering::Release);
            requests.lock().await.insert((scope.clone(),pending.key.clone()),live);
            broker.publish(CodexTurnControlTarget::Suspended { handle:AppServerRequestHandle::Remote(client.request_handle()),
                request_ids:Arc::new(std::sync::atomic::AtomicI64::new(10)), pending_requests:requests.clone(),scope:scope.clone(),
                output:Default::default(),continuity:Some(state.clone()),handoff_barrier:barrier.clone(),effect_gate:gate.clone(),
                turn_interrupted:interrupted.clone(),thread_id:"thread-1".into(),turn_id:"turn-2".into() });
            let mut intents=Vec::new();
            let applied=broker.apply(steer(),|intent|{intents.push(intent);Ok(())}).await.unwrap();
            assert_eq!(intents.len(),usize::from(bad.is_none()));
            assert!(applied.result.continuity.is_none());
            assert_eq!(applied.disposition,CodexTurnControlDisposition::Applied);
            assert_eq!(requests.lock().await.get(&(scope.clone(),pending.key.clone())).unwrap().request_id,pending.request_id);
            assert_eq!(applied.result.acknowledged,index==4);
            if index==5 {assert_eq!(applied.result.detail["retarget_retry_exhausted"],true);}
            drop(applied);
        }
        broker.publish(CodexTurnControlTarget::RecoveredPending {handle:AppServerRequestHandle::Remote(client.request_handle()),
            request_ids:Arc::new(std::sync::atomic::AtomicI64::new(20)),continuity:state.clone(),effect_gate:gate});
        let deferred=broker.apply(steer(),|_|panic!("cold pending must not dispatch")).await.unwrap();
        assert_eq!(deferred.disposition,CodexTurnControlDisposition::DeferredTurn(CodexDeferredTurnCause::PendingSettlement));
        client.shutdown().await.unwrap();
        server.await.unwrap();
    }

    #[test]
    fn message_delta_payload_carries_seq_and_text() {
        let output = CodexTurnOutput::scoped_message_delta(
            3,
            "hel".into(),
            "turn-1".into(),
            "item-1".into(),
            Some(MessagePhase::Commentary),
        );
        assert_eq!(output.label, AGENT_MESSAGE_DELTA_LABEL);
        assert_eq!(output.route, ProviderEmissionRoute::ClientProjection);
        assert_eq!(
            output.value.get("seq").and_then(serde_json::Value::as_u64),
            Some(3)
        );
        assert_eq!(
            output.value.get("text").and_then(serde_json::Value::as_str),
            Some("hel")
        );
        assert_eq!(
            output
                .value
                .get("phase")
                .and_then(serde_json::Value::as_str),
            Some("commentary")
        );
        let a = serde_json::to_vec(
            &CodexTurnOutput::scoped_message_delta(
                1,
                "same".into(),
                "turn-1".into(),
                "item-1".into(),
                None,
            )
            .value,
        )
        .unwrap();
        let b = serde_json::to_vec(
            &CodexTurnOutput::scoped_message_delta(
                2,
                "same".into(),
                "turn-1".into(),
                "item-1".into(),
                None,
            )
            .value,
        )
        .unwrap();
        assert_ne!(a, b);
    }
