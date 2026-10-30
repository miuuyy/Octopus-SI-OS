use super::*;
use serde_json::json;

#[tokio::test]
#[ignore = "requires an explicitly selected stopped Home with a committed error retaining its exact result"]
async fn selected_home_completed_error_retains_stopped_result_settlement() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1); drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(),dir,Some(descriptor.clone()),home.join("interfaces").join(provider.as_str()),
        home.join("ledger/movements.jsonl"),home.join("ledger/blobs"),scratch.path().join("unused-index.sqlite"),
        BTreeMap::from([("run_records_root".into(),home.join("state/runs")),("interface_hand_in_root".into(),home.join("interface-hand-in"))]),arrivals,Default::default())
        .with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let path = home.join("state/runs").join(format!("{run}.json"));
    let before = std::fs::read(&path).unwrap(); let ledger = std::fs::read(&ctx.movements_path).unwrap();
    let cursors = ["ledger-cursor.json", "ledger-cursor.turn-control.json"].map(|name| {
        let path = ctx.interface_var_dir().join(name); let bytes = std::fs::read(&path).unwrap(); (path, bytes)
    });
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    assert!(session.completed_source_candidates(&ctx).unwrap().contains(&(run.clone(), source)));
    let turn = replay_provider_source(&ctx,&run,source).unwrap().unwrap();
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope).unwrap();
    let handoff = session.from_invocation(&ctx,&descriptor,&run,&turn.envelope,source,&pinned).unwrap();
    let (recovery,outcomes) = handoff.recover_completed_source_with_outcomes().await.unwrap();
    assert_eq!(recovery,ProviderSourceRecovery::Completed);
    let context = handoff.current_session_context_artifacts().await;
    let proof = crate::handler::stopped_result::prove(&descriptor,&turn.envelope,source,&before,&context,&outcomes)
        .unwrap().expect("actual committed error must prove its exact stopped result");
    validate_stopped_result_control(&ctx,&descriptor,&run,&proof).unwrap();
    let mut forged_ack = proof.clone();
    forged_ack.acknowledgement.content = ContentHash::of_bytes(b"unadmitted Stop acknowledgement");
    assert!(validate_stopped_result_control(&ctx,&descriptor,&run,&forged_ack).is_err());
    let mut unadmitted_ack = proof.clone();
    unadmitted_ack.acknowledgement_seq += 1;
    assert!(validate_stopped_result_control(&ctx,&descriptor,&run,&unadmitted_ack).is_err());
    let value = serde_json::to_value(&proof.source).unwrap();
    assert!(value["pending"].is_object()); assert!(value["native_terminal"].is_null());
    let result:serde_json::Value = serde_json::from_slice(&turn.envelope.read_artifact_payload().unwrap()).unwrap();
    assert_eq!(result["ok"],true);
    assert_eq!(result["correlation_id"],value["pending"]["call_id"]);
    let mut changed = context.as_ref().clone();
    let error = changed.iter_mut().find(|entry|entry.source_artifact==proof.predecessor).unwrap();
    error.payload["codex_continuity"]["pending"]["call_id"] = json!("different-effect");
    assert!(crate::handler::stopped_result::prove(&descriptor,&turn.envelope,source,&before,&changed,&outcomes).is_err());
    let mut generation:serde_json::Value = serde_json::from_slice(&before).unwrap();
    generation["interface_state"][provider.as_str()]["context_generation"] = json!(999_999);
    assert!(crate::handler::stopped_result::prove(&descriptor,&turn.envelope,source,&serde_json::to_vec(&generation).unwrap(),&context,&outcomes).is_err());
    assert_eq!(std::fs::read(path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
    for (path,bytes) in cursors { assert_eq!(std::fs::read(path).unwrap(),bytes); }
    eprintln!("actual completed source {source}, Stop {}, checked generation {}: exact settlement eligible; native terminal read remains a live gate",proof.control_source,value["context_generation"]);
}

#[test]
#[ignore = "requires an explicitly selected Home for read-only authoring inspection"]
fn selected_home_authoring_inspector_reads_actual_selection() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(home.join("registry/interface").join(provider.as_str()).join("interface.json")).unwrap()).unwrap();
    let path=home.join("state/runs").join(format!("{run}.json"));
    let before=std::fs::read(&path).unwrap();
    let ledger_path=home.join("ledger/movements.jsonl");let ledger=std::fs::read(&ledger_path).unwrap();
    let roots: octopus_operator_chat_client::run_composition::CompositionRoots = serde_json::from_value(
        descriptor.interface_driver_config.as_ref().unwrap().private_config["run_affordances"]["roots"].clone()).unwrap();
    let record = octopus_run_record::RunRecord::from_bytes(&before).unwrap();
    let selection = octopus_operator_chat_client::run_composition::current_run::current_selection(&roots,&record,&provider).unwrap().unwrap();
    assert!(!selection.selected_tools.is_empty());
    let inspected = octopus_operator_chat_client::run_composition::validate_registry_candidate(&octopus_operator_chat_client::run_composition::RegistryValidationRoots {
        registry_root:roots.registry_root,ee_instances_root:roots.ee_instances_root,run_records_root:roots.run_records_root,
    },&[]).unwrap();
    eprintln!("authoring inspection: {} actual selected tools, {} installed subjects, {} diagnosed registry issues",
        selection.selected_tools.len(),inspected.stages[0].available.len(),inspected.stages[0].issues.len());
    assert_eq!(std::fs::read(path).unwrap(),before);assert_eq!(std::fs::read(ledger_path).unwrap(),ledger);
}

#[tokio::test]
#[ignore = "requires an explicitly selected duplicate result with a completed earlier input"]
async fn selected_home_duplicate_result_preserves_current_wait() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1); drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(),dir,Some(descriptor.clone()),home.join("interfaces").join(provider.as_str()),
        home.join("ledger/movements.jsonl"),home.join("ledger/blobs"),scratch.path().join("unused-index.sqlite"),
        BTreeMap::from([("run_records_root".into(),home.join("state/runs")),("interface_hand_in_root".into(),home.join("interface-hand-in"))]),arrivals,Default::default())
        .with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let path = home.join("state/runs").join(format!("{run}.json"));
    let before = std::fs::read(&path).unwrap(); let ledger = std::fs::read(&ctx.movements_path).unwrap();
    let turn = replay_provider_source(&ctx,&run,source).unwrap().unwrap();
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope).unwrap();
    let handoff = ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&turn.envelope,source,&pinned).unwrap();
    // Match life's post-recovery read boundary. This selected case has only a
    // fully admitted earlier terminal handoff; it must emit and write nothing.
    let record: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let pending = &record["interface_state"][provider.as_str()]["pending_handoff"];
    assert_eq!(pending["terminal"],true);
    assert!(pending["source_movement_seq"].as_u64().unwrap() < source);
    assert!(pending["context_publication"].is_null());
    let (recovery,outcomes) = handoff.recover_source_with_outcomes().await.unwrap();
    assert_eq!(recovery,ProviderSourceRecovery::Process);assert!(outcomes.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
    let observed = handoff.current_session_context_artifacts().await;
    for entry in observed.iter().filter(|entry| entry.source_artifact == turn.envelope.artifact).chain(observed.iter().rev().take(3)) {
        eprintln!("context source={:?} label={} exact_input={} generation={}",entry.source_movement_seq,entry.label,
            entry.source_artifact==turn.envelope.artifact,entry.payload["codex_continuity"]["context_generation"]);
    }
    let prior = handoff.completed_input_artifact_source(&turn.envelope.artifact).await.unwrap().unwrap();
    assert!(prior < source);
    let session = handoff.current_session_context_artifacts().await;
    let continuity = reconcile_operator_session_continuity(&descriptor,&turn.envelope,None,&session).unwrap().unwrap();
    let receipt = crate::handler::duplicate_tool_result_receipt(&descriptor,&turn.envelope,source,prior,
        &handoff.current_run_record_bytes().await,&session,scratch.path()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&receipt.artifact.read_payload_bytes().unwrap()).unwrap();
    let original = serde_json::to_value(&continuity).unwrap();
    assert!(value.get("codex_continuity").is_none());
    assert_eq!(value["context_generation"],original["context_generation"]);
    assert_eq!(value["completed_source_movement_seq"],prior);
    let mut other = turn.envelope.artifact.clone(); other.content=ContentHash::of_bytes(b"different result");
    assert!(handoff.completed_input_artifact_source(&other).await.unwrap().is_none());
    assert_eq!(std::fs::read(path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
    eprintln!("duplicate source {source} proves completed source {prior}; current native wait preserved");
}

#[tokio::test]
#[ignore = "requires an explicitly selected deferred input whose native result is already settled"]
async fn selected_home_new_input_observes_the_settled_native_result() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1); drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(),dir,Some(descriptor.clone()),home.join("interfaces").join(provider.as_str()),
        home.join("ledger/movements.jsonl"),home.join("ledger/blobs"),scratch.path().join("unused-index.sqlite"),
        BTreeMap::from([("run_records_root".into(),home.join("state/runs")),("interface_hand_in_root".into(),home.join("interface-hand-in"))]),arrivals,Default::default())
        .with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let path = home.join("state/runs").join(format!("{run}.json"));
    let before = std::fs::read(&path).unwrap(); let ledger = std::fs::read(&ctx.movements_path).unwrap();
    let turn = replay_provider_source(&ctx,&run,source).unwrap().unwrap();
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope).unwrap();
    let snapshot = bound_operator_session_snapshot(&ctx,&descriptor,&turn).unwrap();
    let bound = recover_provider_turn_continuity(&descriptor,&turn.envelope,snapshot.as_ref().map(|(_,snapshot)|snapshot),&run,&pinned).unwrap();
    let handoff = ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx,&descriptor,&run,&turn.envelope,source,&pinned).unwrap();
    let current = handoff.current_session_context_artifacts().await;
    let settled = reconcile_operator_session_continuity(&descriptor,&turn.envelope,bound.clone(),&current).unwrap();
    for (name,state) in [("bound",bound.as_ref()),("current",settled.as_ref())] {
        let state=serde_json::to_value(state).unwrap();eprintln!("{name}: generation={} pending={} interrupted={}",state["context_generation"],!state["pending"].is_null(),!state["interrupted_pending"].is_null());
    }
    assert!(!settled.as_ref().is_some_and(codex_continuity_waits_for_result));
    assert!(!operator_turn_requires_deferral(&ctx,&descriptor,&run,&turn,&current).unwrap());
    let prefix = settled_envelope_prefix(&ctx,&descriptor,&run,&turn,None).unwrap().unwrap();
    validate_settled_envelope_release(&ctx,&descriptor,&run,&turn,prefix).unwrap();
    assert!(validate_settled_envelope_release(&ctx,&descriptor,&run,&turn,source).is_err());
    assert!(validate_settled_envelope_release(&ctx,&descriptor,&run,&turn,prefix+1).is_err());
    #[cfg(unix)] {
        let mut partial = ctx.clone();
        let dir = scratch.path().join("partial-ledger");std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(ctx.movements_path.parent().unwrap().join("metadata"),dir.join("metadata")).unwrap();
        partial.movements_path=dir.join("movements.jsonl");
        let mut bytes=ledger.clone();bytes.extend_from_slice(br#"{"seq":"#);std::fs::write(&partial.movements_path,bytes).unwrap();
        assert!(settled_envelope_prefix(&partial,&descriptor,&run,&turn,None).unwrap().is_none());
        validate_settled_envelope_release(&partial,&descriptor,&run,&turn,prefix).unwrap();
        assert!(validate_settled_envelope_release(&partial,&descriptor,&run,&turn,prefix+1).is_err());
    }
    assert_eq!(std::fs::read(path).unwrap(),before);assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
}

#[tokio::test]
#[ignore = "requires an explicitly selected Home with an admitted nonterminal tool handoff"]
async fn selected_home_recovers_admitted_tool_handoff_without_replaying_the_effect() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(interface_dir.join("interface.json")).unwrap()).unwrap();
    let life: serde_json::Value = serde_json::from_slice(&std::fs::read(interface_dir.join("life.json")).unwrap()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor.clone()), home.join("interfaces").join(provider.as_str()),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"), scratch.path().join("unused-index.sqlite"),
        BTreeMap::from([("run_records_root".into(),home.join("state/runs")),("interface_hand_in_root".into(),home.join("interface-hand-in"))]),
        arrivals, Default::default(),
    ).with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let run_path = home.join("state/runs").join(format!("{run}.json"));
    let before = std::fs::read(&run_path).unwrap();
    let ledger = std::fs::read(&ctx.movements_path).unwrap();
    let turn = replay_provider_source(&ctx, &run, source).unwrap().unwrap();
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope).unwrap();
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    let handoff = session.from_invocation(&ctx, &descriptor, &run, &turn.envelope, source, &pinned).unwrap();
    let current = handoff.current_session_context_artifacts().await;
    let record: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let pending = &record["interface_state"][provider.as_str()]["pending_handoff"];
    assert_eq!(pending["source_movement_seq"],source);
    assert_eq!(pending["terminal"],false);
    let id = pending["items"].as_array().unwrap().last().unwrap()["artifact_id"].as_str().unwrap();
    // The constructor checks this publication against the run's write history
    // and exact ledger admissions. No handoff/effect method is called here.
    let effect = current.iter().find(|entry|entry.source_artifact.id.as_str()==id).unwrap();
    let outcomes = [ProviderHandoffOutcome::Admitted {seq:effect.source_movement_seq.unwrap(),artifact:effect.source_artifact.clone()}];
    let receipt = crate::handler::recovered_tool_handoff_receipt(&descriptor,&turn.envelope,source,&before,
        &current,&outcomes,scratch.path()).unwrap().expect("the exact admitted effect must close its consumed source");
    let value: serde_json::Value = serde_json::from_slice(&receipt.artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(value["effect_artifact"],serde_json::to_value(&effect.source_artifact).unwrap());
    assert_eq!(value["codex_continuity"]["pending"],effect.payload["codex_continuity"]["pending"]);
    assert_eq!(value["codex_continuity"]["context_generation"].as_u64().unwrap(),record["interface_state"][provider.as_str()]["context_generation"].as_u64().unwrap()+1);
    assert_eq!(std::fs::read(run_path).unwrap(),before);
    assert_eq!(std::fs::read(&ctx.movements_path).unwrap(),ledger);
}

#[tokio::test]
#[ignore = "requires explicitly selected read-only replay evidence"]
async fn selected_home_deferred_continuity_preserves_exact_evidence() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let seq = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse::<u64>().unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let life: serde_json::Value = serde_json::from_slice(
        &std::fs::read(interface_dir.join("life.json")).unwrap(),
    ).unwrap();
    let limits = serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let emitted = scratch.path().join("provider/emitted");
    std::fs::create_dir_all(&emitted).unwrap();
    for entry in std::fs::read_dir(home.join("interfaces").join(provider.as_str()).join("emitted")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().ends_with(".artifact.json") {
            std::fs::copy(entry.path(), emitted.join(entry.file_name())).unwrap();
        }
    }
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor.clone()), scratch.path().join("provider"),
        home.join("ledger/movements.jsonl"), home.join("ledger/blobs"),
        home.join("ledger/ledger-index.sqlite"),
        BTreeMap::from([
            ("run_records_root".into(), home.join("state/runs")),
            ("interface_hand_in_root".into(), scratch.path().join("hand-in")),
        ]), arrivals, Default::default(),
    ).with_host_io_limits(limits).unwrap();
    let path = home.join("interfaces").join(provider.as_str())
        .join("deferred-provider-turn").join(format!("{seq}.json"));
    let journal_bytes = std::fs::read(&path).unwrap();
    let staged = deferred_provider_turn_path(&ctx, seq);
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    std::fs::write(&staged, &journal_bytes).unwrap();
    let turn = load_deferred_provider_turns(&ctx, &descriptor).unwrap().remove(&seq).unwrap().turn;
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope).unwrap();
    let snapshot = bound_operator_session_snapshot(&ctx, &descriptor, &turn).unwrap();
    let bound = recover_provider_turn_continuity(
        &descriptor, &turn.envelope, snapshot.as_ref().map(|(_, snapshot)| snapshot), &run, &pinned,
    ).unwrap();
    let session = ProviderHandoffSession::new(&ctx).unwrap();
    let handoff = session.from_invocation_when_ready(
        &ctx, &descriptor, &run, &turn.envelope, seq, &pinned,
    ).await.unwrap();
    let current = handoff.current_session_context_artifacts().await;
    let recovered = reconcile_operator_session_continuity(
        &descriptor, &turn.envelope, bound.clone(), &current,
    ).unwrap();
    let labels = codex_session_snapshot_labels(&descriptor).unwrap();
    let scan = latest_provider_session_carrier(&ctx, &run, seq, &labels, None).unwrap();
    assert!(scan.complete_boundary);
    assert!(scan.latest.is_none());
    let interrupted_path = interrupted_effect_journal_path(&ctx, &run);
    let interrupted_bytes = std::fs::read(home.join("interfaces").join(provider.as_str())
        .join("interrupted-effect-journal").join(interrupted_path.file_name().unwrap())).unwrap();
    std::fs::create_dir_all(interrupted_path.parent().unwrap()).unwrap();
    std::fs::write(&interrupted_path, &interrupted_bytes).unwrap();
    let (release, private) = checked_deferred_interrupted_result(
        &ctx, &descriptor, &run, &turn, bound.as_ref(), None, None,
    ).await.unwrap().expect("the exact reconciled result must settle private continuity");
    assert!(codex_continuity_successor_is_settled(bound.as_ref(), Some(&private)).unwrap());
    assert_eq!(serde_json::to_value(&private).unwrap()["context_generation"], 32);
    assert!(matches!(&release, DeferredProviderTurnRelease::InterruptedResult {
        result_source, closure, ..
    } if result_source.movement_seq == 896 && closure.movement_seq == 912));
    let (_, replayed) = checked_deferred_interrupted_result(
        &ctx, &descriptor, &run, &turn, bound.as_ref(), Some(&release), None,
    ).await.unwrap().unwrap();
    assert_eq!(replayed, private);
    let deferred = DeferredProviderTurn { run:run.clone(), turn:turn.clone(), release:None };
    let DeferredTurnReadiness::Ready(ready) = prepare_deferred_provider_turn(
        &ctx, &descriptor, &run, &deferred, &ActiveTurnControls::default(),
    ).await.unwrap() else { panic!("exact interruption must release the original queued input"); };
    assert_eq!(capture_deferred_provider_turn(&ctx, &run, &ready).unwrap(),
        capture_deferred_provider_turn(&ctx, &run, &turn).unwrap());
    let loaded = load_deferred_provider_turns(&ctx, &descriptor).unwrap();
    assert_eq!(loaded[&seq].release.as_ref(), Some(&release));
    assert!(matches!(prepare_deferred_provider_turn(&ctx, &descriptor, &run, &loaded[&seq],
        &ActiveTurnControls::default()).await.unwrap(), DeferredTurnReadiness::Ready(_)));
    let unchanged = handoff.current_session_context_artifacts().await;
    assert_eq!(unchanged.len(), current.len());
    assert!(unchanged.iter().zip(current.iter()).all(|(left, right)|
        left.source_artifact == right.source_artifact && left.source_movement_seq == right.source_movement_seq
            && left.label == right.label && left.payload == right.payload));
    assert_eq!(std::fs::read(interrupted_path).unwrap(), interrupted_bytes);
    assert_eq!(std::fs::read(path).unwrap(), journal_bytes);
    eprintln!("read-only source {seq}: bound generation {}; ordinary checked generation {}; private exact-result generation {}; model-readable context unchanged",
        serde_json::to_value(bound.unwrap()).unwrap()["context_generation"],
        serde_json::to_value(recovered.unwrap()).unwrap()["context_generation"],
        serde_json::to_value(private).unwrap()["context_generation"]);
}

#[test]
#[ignore = "requires explicitly selected read-only replay evidence"]
fn selected_home_sealed_interruption_preserves_exact_evidence() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let seq = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse::<u64>().unwrap();
    let interface_dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(
        &std::fs::read(interface_dir.join("interface.json")).unwrap(),
    ).unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(
        provider.clone(), interface_dir, Some(descriptor.clone()),
        home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"),
        home.join("ledger/blobs"), home.join("ledger/ledger-index.sqlite"),
        BTreeMap::new(), arrivals, Default::default(),
    );
    let path = interrupted_effect_journal_path(&ctx, &run);
    let journal_bytes = std::fs::read(&path).unwrap();
    let journal: InterruptedEffectJournal = serde_json::from_slice(&journal_bytes).unwrap();
    let InterruptedEffectPhase::Resolving { result_source_movement_seq, source_artifact } = journal.phase else {
        panic!("selected evidence is not an interrupted result awaiting handoff");
    };
    assert_eq!(result_source_movement_seq, seq);
    let turn = QueuedTurn { seq, envelope: InterfaceInvocationEnvelope {
        artifact_file: octopus_interfaces::InterfaceArtifactFile::new(ctx.blobs_dir.join(source_artifact.content.to_hex())).unwrap(),
        artifact: source_artifact, artifact_binding_payloads: Vec::new(), metadata: None,
    } };
    let (emissions, closure) = recover_sealed_interrupted_result(&ctx, &descriptor, &turn, &journal.continuity)
        .unwrap().expect("selected interrupted source must have its sealed terminal pair");
    assert_eq!(emissions.len(), 2);
    assert_eq!(materialized_artifact_ref(&emissions[1].artifact).unwrap(), closure);
    let body = emissions[0].artifact.read_payload_bytes().unwrap();
    let rows = String::from_utf8(body.clone()).unwrap().lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()).collect::<Vec<_>>();
    let mut labels = BTreeMap::<String, usize>::new();
    for row in &rows {
        if row["type"] == "event" { *labels.entry(row["label"].as_str().unwrap().to_string()).or_default() += 1; }
    }
    assert_eq!(rows.last().unwrap()["source_movement_seq"], seq);
    assert_eq!(emissions[0].artifact.read_payload_bytes().unwrap(), body);
    assert_eq!(std::fs::read(path).unwrap(), journal_bytes);
    eprintln!("verified sealed source {seq}: {} rows, events {labels:?}, terminal {}", rows.len(), emissions[1].artifact.read_manifest().unwrap().payload_content_hash.unwrap());
}

#[tokio::test]
#[ignore = "requires explicitly selected read-only terminal-source evidence"]
async fn selected_home_terminal_source_retains_exact_context() {
    let home = PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider = InterfaceId::new(std::env::var("OCTOPUS_REPLAY_PROVIDER").unwrap());
    let run = RunId::new(std::env::var("OCTOPUS_REPLAY_RUN").unwrap());
    let source: u64 = std::env::var("OCTOPUS_REPLAY_SOURCE").unwrap().parse().unwrap();
    let dir = home.join("registry/interface").join(provider.as_str());
    let descriptor: InterfaceDescriptor = serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let (arrivals, receiver) = tokio::sync::mpsc::channel(1);
    drop(receiver);
    let ctx = InterfaceLiftContext::unprivileged(provider.clone(), dir, Some(descriptor.clone()),
        home.join("interfaces").join(provider.as_str()), home.join("ledger/movements.jsonl"),
        home.join("ledger/blobs"), home.join("ledger/ledger-index.sqlite"),
        BTreeMap::from([("run_records_root".into(), home.join("state/runs")),
            ("interface_hand_in_root".into(), home.join("interface-hand-in"))]), arrivals, Default::default())
        .with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let path = home.join("state/runs").join(format!("{run}.json"));
    let before = std::fs::read(&path).unwrap();
    let turn = replay_provider_source(&ctx, &run, source).unwrap().unwrap();
    let pinned = ctx.pinned_run_record_bytes(&turn.envelope).unwrap();
    let handoff = ProviderHandoffSession::new(&ctx).unwrap()
        .from_invocation(&ctx, &descriptor, &run, &turn.envelope, source, &pinned).unwrap();
    let current = handoff.current_session_context_artifacts().await;
    let record = handoff.current_run_record_bytes().await;
    let deferred = handoff.deferred_context_generations().await;
    let continuity = terminal_output_continuity(&descriptor, &turn.envelope, &current, &run, &record, deferred)
        .unwrap().expect("retained source has exact continuity");
    eprintln!("source {source}: {} context artifacts, {deferred} standing generations, terminal generation {}",
        current.len(), continuity.context_generation);
    assert_eq!(std::fs::read(path).unwrap(), before);
}
