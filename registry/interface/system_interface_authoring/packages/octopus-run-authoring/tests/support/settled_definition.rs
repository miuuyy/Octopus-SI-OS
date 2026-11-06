use super::*;
use serde_json::{Value, json};

#[test]
fn definition_changes_preserve_writer_identity_and_private_continuity() {
    let provider = InterfaceId::new("provider");
    let original = json!({"id":"run", "ee":"ee", "worker":"worker", "trace":"trace",
        "status":"idle", "checkpoints":[], "active_checkpoints":[],
        "interface_state":{"provider":{"authoring_generation":8,"context_generation":3,
            "pending_handoff":{"terminal":true},"frontier_checkpoint_ids":["old"],
            "run_authoring_binding":{"schema_version":2,"run":"run","worker":"worker",
                "provider_interface":"provider","authoring_source_interface":"writer",
                "payload_artifact_bindings":[],"checkpoints":[]}}}});
    let decode =
        |value: &Value| RunRecord::from_bytes(&serde_json::to_vec(value).unwrap()).unwrap();
    let accepted = decode(&original);
    let mut changed = original.clone();
    changed["interface_state"]["provider"]["frontier_checkpoint_ids"] = json!(["new"]);
    changed["interface_state"]["provider"]["run_authoring_binding"]["checkpoints"] = json!([{
        "name":"input","role":"stable","interface_binding":{"interface":"input","config":{"artifact_policy":{"mode":"all"}}},
        "outbound_interface_bindings":[{"interface":"provider","config":{"artifact_policy":{"mode":"all"}}}],"include_context":true,
        "required_verified_artifacts":[]}]);
    assert!(preserves_settled_continuity(&accepted, &decode(&changed), &provider).unwrap());
    for pointer in [
        "/interface_state/provider/authoring_generation",
        "/interface_state/provider/context_generation",
        "/interface_state/provider/pending_handoff",
        "/interface_state/provider/run_authoring_binding/authoring_source_interface",
        "/interface_state/provider/run_authoring_binding/payload_artifact_bindings",
        "/worker",
        "/trace",
    ] {
        let mut forged = changed.clone();
        *forged.pointer_mut(pointer).unwrap() = match pointer {
            p if p.ends_with("generation") => json!(999),
            p if p.ends_with("pending_handoff") => json!({"terminal":false}),
            p if p.ends_with("payload_artifact_bindings") => {
                json!([{"id":"foreign","content":vec![0;32]}])
            }
            _ => json!("forged"),
        };
        assert!(
            !preserves_settled_continuity(&accepted, &decode(&forged), &provider).unwrap(),
            "{pointer}"
        );
    }
}

#[tokio::test]
#[ignore = "requires an explicitly selected Home with an admitted input after a settled definition edit"]
async fn selected_home_accepts_new_definition_without_replaying_completed_work() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source_movement_seq = std::env::var("OCTOPUS_REPLAY_SOURCE")
        .unwrap()
        .parse()
        .unwrap();
    let run_path = home.join("state/runs").join(format!("{run}.json"));
    let current = std::fs::read(&run_path).unwrap();
    let record = RunRecord::from_bytes(&current).unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor =
        serde_json::from_slice(&std::fs::read(interface_dir.join("interface.json")).unwrap())
            .unwrap();
    let profile = run_authoring_profile(&descriptor).unwrap();
    let (config, bound_topology) =
        resolve_profile_invocation(&profile, &provider, &run, &record, None).unwrap();
    let (context_output_labels, session_snapshot_labels) =
        provider_context_label_sets(&descriptor).unwrap();
    let hand_in = home.join("interface-hand-in");
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(),
        interface_dir,
        Some(descriptor),
        home.join("interfaces").join(provider.as_str()),
        home.join("ledger/movements.jsonl"),
        home.join("ledger/blobs"),
        home.join("ledger/ledger-index.sqlite"),
        BTreeMap::from([
            ("run_records_root".into(), home.join("state/runs")),
            ("interface_hand_in_root".into(), hand_in.clone()),
        ]),
        arrivals,
        Default::default(),
    )
    .with_host_io_limits(InterfaceHostIoLimits {
        max_control_frame_bytes: 1_048_576,
        max_manifest_bytes: 1_048_576,
        max_payload_bytes: 268_435_456,
        max_stderr_bytes: 262_144,
    })
    .unwrap();
    let ledger_before = std::fs::read(&ctx.movements_path).unwrap();
    let mut reader_ctx = ctx.clone();
    reader_ctx.var_dir = scratch.path().to_path_buf();
    let handoff = ProviderOutputHandoff {
        checked_settlement_output: None,
            invocation_metadata: InvocationMetadata::new(Default::default()),
        session: ProviderHandoffSession::new(&ctx).unwrap(),
        hand_in_root: hand_in.join(config.authoring_source_interface.as_str()),
        var_dir: ctx.var_dir.clone(),
        result_reader: Arc::new(tokio::sync::Mutex::new(
            LedgerTailReader::open_named(&reader_ctx, "definition-audit").unwrap(),
        )),
        ctx,
        config,
        run_path,
        source_movement_seq,
        source_carries_context: true,
        failed_source: false,
        context_output_labels,
        session_snapshot_labels,
        standing_candidate_labels: BTreeSet::new(),
        granted_tools: BTreeSet::new(),
        granted_interfaces: BTreeSet::new(),
        granted_mark_outcomes: BTreeSet::new(),
        bound_topology,
        delivery_mode: HandoffDeliveryMode::Rearm,
        invocation_session_snapshot: None,
        state: Arc::new(tokio::sync::Mutex::new(HandoffState {
            validated_withdrawal_output: None,
            context_artifacts: Vec::new(),
            static_context_artifacts: Vec::new(),
            session_context: ResolvedSessionContext {
                provider_interface: provider.clone(),
                artifacts: Vec::new(),
            },
            session_snapshot_head: None,
            run_record_bytes: current.clone(),
            run_record_evidence_seq: None,
            consumed_input: true,
            consumed_context_free_independent_input: None,
            consumed_verified_input: false,
            deferred_context_generations: 0,
            result_rebase: None,
        })),
    };
    assert!(handoff.settled_input_definition(&record, &current).unwrap());
    assert!(matches!(
        handoff
            .recover_authoring_attempt(&record, &current)
            .unwrap(),
        AuthoringAttemptRecovery::None
    ));
    assert_eq!(
        handoff.recover_source().await.unwrap(),
        ProviderSourceRecovery::Process
    );
    let mut changed = record.clone();
    changed
        .interface_state
        .get_mut(&provider)
        .unwrap()
        .insert("authoring_generation".into(), json!(999));
    assert!(
        !handoff
            .settled_input_definition(&changed, &serde_json::to_vec(&changed).unwrap())
            .unwrap()
    );
    assert!(handoff.recover_authoring_attempt(&changed, &serde_json::to_vec(&changed).unwrap()).is_err());
    let mut late = record.clone();
    late.status = json!("running");
    assert!(
        !handoff
            .settled_input_definition(&late, &serde_json::to_vec(&late).unwrap())
            .unwrap()
    );
    assert!(handoff.recover_authoring_attempt(&late, &serde_json::to_vec(&late).unwrap()).is_err());
    assert_eq!(std::fs::read(&handoff.run_path).unwrap(), current);
    assert_eq!(
        std::fs::read(&handoff.ctx.movements_path).unwrap(),
        ledger_before
    );
}
