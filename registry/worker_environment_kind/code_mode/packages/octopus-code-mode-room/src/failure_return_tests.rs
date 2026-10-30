use super::*;
use std::{future::Future, pin::Pin};
use octopus_run_authoring::{CheckedRunEvidence, CheckedRunEvidenceReader, FileCheckedRunReader};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact, ToolResultError};

pub(super) fn rejected_write_is_proven_by_its_admitted_failure(
    admission: &FrontierAdmission, ctx: &octopus_tool_door_runtime::InterfaceLiftContext,
    input: &InterfaceInvocationEnvelope, call_seq: u64,
) {
    use octopus_run_authoring::{CheckedRunEdit, CheckedRunWriter, checked_failed_run_write};
    let call: ToolCallArtifact = serde_json::from_slice(&input.artifact_file.read_payload_bytes().unwrap()).unwrap();
    let peer = admission.deliveries.lock().unwrap().iter().rev()
        .find(|row| row.seq < call_seq && row.to.as_str() == "system_provider_file_test").cloned().unwrap();
    let payload: ToolCallArtifact = serde_json::from_slice(&peer.file.read_payload_bytes().unwrap()).unwrap();
    let entry = &payload.arguments["writes"][1];
    let preimage: octopus_run_authoring::Preimage = serde_json::from_value(entry["expected_before_content"].clone()).unwrap();
    let before_text = preimage.decode(entry["expected_before_hash"].as_str().unwrap(), &mut octopus_run_authoring::DecodeBudget::new(ctx.owner_io_limits().unwrap().max_payload_bytes).unwrap()).unwrap();
    let before = before_text.as_bytes();
    let patch:json_patch::Patch = serde_json::from_value(call.arguments["patch"].clone()).unwrap();
    let mut after:Value = serde_json::from_slice(before).unwrap();
    json_patch::patch(&mut after,&patch).unwrap();
    let edit = CheckedRunEdit::prepare(serde_json::from_value(call.arguments["path"].clone()).unwrap(),before,
        after,call.correlation_id.clone().unwrap()).unwrap();
    let writer = CheckedRunWriter { owner:InterfaceId::new(DOOR),source:InterfaceId::new(DOOR),
        door:input.runtime().unwrap().interface.clone(),tool:call.tool.clone(),
        call_label:input.artifact_file.read_manifest().unwrap().label,
        result_label:ToolResultArtifact::success(&call.tool,None,json!({})).label };
    let coauthor = CheckedRunWriter { owner:InterfaceId::new("world_provider_test"),source:peer.from.clone(),
        door:peer.to.clone(),tool:payload.tool.clone(),call_label:peer.file.read_manifest().unwrap().label,
        result_label:ToolResultArtifact::success(&payload.tool,None,json!({})).label };
    let limits = ctx.owner_io_limits().unwrap();
    let evidence = CheckedRunEvidence { movements_path:ctx.owner_movements_path(),ledger_index_path:ctx.owner_ledger_index_path(),
        blobs_dir:ctx.owner_blobs_dir(),max_control_frame_bytes:limits.max_control_frame_bytes,
        max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes };
    let mut reader = FileCheckedRunReader::open(&evidence,Some(call_seq)).unwrap();
    let mut failure = None;
    while let Some(row) = reader.movement(reader.next_sequence()).unwrap() {
        if row.seq.0 <= call_seq { continue; }
        assert!(row.from != octopus_ledger::Endpoint::Interface(coauthor.door.clone()), "peer receipt arrived before the pending proof");
        assert!(row.kind != octopus_ledger::MovementKind::DeliveryRefused, "a standing return edge refused the failed write result");
        if row.kind == octopus_ledger::MovementKind::InterfaceOutbound && row.from == octopus_ledger::Endpoint::Interface(writer.door.clone())
            && row.to == octopus_ledger::Endpoint::Interface(writer.owner.clone()) {
            failure = Some(row);
        }
    }
    let failure = failure.unwrap();
    assert!(peer.seq < call_seq && call_seq < failure.seq.0);
    assert_eq!(checked_failed_run_write(&evidence,&writer,&edit,call_seq,vec![coauthor]).unwrap(), Some(failure));
}

#[tokio::test]
async fn refused_failure_returns_exactly_once_from_the_retained_install() {
    for case in [FrontierCase::RefusedFailurePretty, FrontierCase::RefusedFailure] {
    tokio::time::timeout(std::time::Duration::from_secs(30),
        run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(case)))
        .await.unwrap();
    }
}

#[tokio::test]
async fn nested_failure_retains_its_admitted_carrier_across_cold_and_delivered_replay() {
    tokio::time::timeout(std::time::Duration::from_secs(75),
        run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::NestedFailure)))
        .await.unwrap();
}

#[tokio::test]
async fn readiness_failure_survives_its_consumed_checkpoint_without_reexecution() {
    tokio::time::timeout(std::time::Duration::from_secs(45),
        run_local_fixture(routed_frontiers_use_actual_runtime_and_file_write(FrontierCase::ReadinessFailure)))
        .await.unwrap();
}

pub(super) async fn actual_readiness_failure_return(
    admission: &std::sync::Arc<FrontierAdmission>, handoff: &std::sync::Arc<actual_owner_handoff::OwnerHandoff>,
    formation: &WorkerEnvironmentFormation, input: &(u64, ToolCallArtifact, InterfaceInvocationEnvelope), durable: &Path,
    room: &CodeModeRoom, kind: &WorkerEnvKindSpec,
) {
    use octopus_tool_door_runtime::owner_contract_tests::hand_owned_output;
    let ctx = admission.door.lock().await.context.clone();
    let source = admission.door.lock().await.output_source(input.0, &input.2, formation).unwrap();
    admission.fail_after_nested_output.store(true, Ordering::SeqCst);
    assert_eq!(admission.run(room, handoff, formation, kind, input.clone()).await.unwrap_err(),
        "fixture interrupted after admitted nested output");
    let pending = super::super::continuation::prepared_calls(durable).unwrap();
    assert_eq!(pending.len(), 1);
    let failure = ToolResultArtifact::failure(&input.1.tool, input.1.correlation_id.clone(),
        ToolResultError::new("tool_effect_indeterminate", "fixture stopped during readiness handoff")
            .with_details(serde_json::Map::from_iter([("source_movement_seq".into(), json!(input.0))])));
    let file = InterfaceArtifactFile::write(ctx.interface_var_dir().join("failure-artifacts"),
        &failure.label, &serde_json::to_vec(&failure).unwrap()).unwrap();
    let bytes = file.read_payload_bytes().unwrap();
    let effects = || admission.deliveries.lock().unwrap().iter()
        .filter(|row| row.to.as_str() == NESTED_INTERFACE || row.to.as_str() == "system_browser_test").count();
    let before_effects = effects();
    admission.reject_next_owner_arrival.store(true, Ordering::SeqCst);
    assert!(matches!(hand_owned_output(&ctx, &source, &actual_owner_handoff::OwnerHandoff::new().callback(), file.clone()).await.unwrap(),
        DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::Policy, .. }));
    let current: Value = serde_json::from_slice(&std::fs::read(admission.run_records.join("run_code_mode_test.json")).unwrap()).unwrap();
    assert!(!current["active_checkpoints"].as_array().unwrap().contains(&json!(input.2.runtime().unwrap().checkpoint)));
    let cold = actual_owner_handoff::OwnerHandoff::new();
    assert!(matches!(hand_owned_output(&ctx, &source, &cold.callback(), file.clone()).await.unwrap(), DeliveryVerdict::Admitted { .. }));
    let writes = admission.file_writes.load(Ordering::SeqCst);
    assert!(matches!(hand_owned_output(&ctx, &source, &actual_owner_handoff::OwnerHandoff::new().callback(), file).await.unwrap(),
        DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::CheckpointSpent, .. }));
    let delivered: Vec<_> = admission.deliveries.lock().unwrap().iter().filter(|row|
        row.from.as_str() == DOOR && row.file.read_payload_bytes().unwrap() == bytes).cloned().collect();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].to.as_str(), DOOR);
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
    assert_eq!(effects(), before_effects);
}

pub(super) async fn actual_nested_failure_return(
    admission: &std::sync::Arc<FrontierAdmission>, handoff: &std::sync::Arc<actual_owner_handoff::OwnerHandoff>,
    formation: &WorkerEnvironmentFormation, input: &(u64, ToolCallArtifact, InterfaceInvocationEnvelope),
    other_input: &(u64, ToolCallArtifact, InterfaceInvocationEnvelope), durable: &Path,
    room: &CodeModeRoom, kind: &WorkerEnvKindSpec, config: &super::super::config::CodeModeConfig,
) {
    use octopus_tool_door_runtime::owner_contract_tests::hand_owned_output;
    let ctx = admission.door.lock().await.context.clone();
    assert_eq!(input.2.read_artifact_manifest().unwrap().label, "browser.tabs.result");
    assert_eq!(input.1.tool, "code_mode.resume");
    assert!(serde_json::from_slice::<ToolCallArtifact>(&input.2.artifact_file.read_payload_bytes().unwrap()).is_err());
    let records = octopus_worker_env::WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
    let record_path = records.record_path(&formation.worker_environment_id);
    let record_bytes = std::fs::read(&record_path).unwrap();
    let record = records.read(&formation.worker_environment_id).unwrap();
    assert!(record.opened_at_movement_seq <= record.last_activity_movement_seq && record.last_activity_movement_seq < input.0);
    let mut source = admission.door.lock().await.output_source(input.0, &input.2, formation).unwrap();
    source.formation = None;
    let failure = ToolResultArtifact::failure(&input.1.tool, input.1.correlation_id.clone(),
        ToolResultError::new("tool_effect_indeterminate", "fixture interrupted native continuation")
            .with_details(serde_json::Map::from_iter([("source_movement_seq".into(), json!(input.0))])));
    let materialize = |result: &ToolResultArtifact| InterfaceArtifactFile::write(ctx.interface_var_dir().join("failure-artifacts"),
        &result.label, &serde_json::to_vec(result).unwrap()).unwrap();
    let file = materialize(&failure);
    let reference = super::super::continuation::artifact_ref(&file).unwrap();
    let callback = handoff.callback();
    let writes = admission.file_writes.load(Ordering::SeqCst);
    let nested_count = || admission.deliveries.lock().unwrap().iter().filter(|row|
        row.to.as_str() == NESTED_INTERFACE || row.to.as_str() == "system_browser_test").count();
    let effects = nested_count();
    let mut foreign = source.clone(); foreign.movement.from = octopus_ledger::Endpoint::Interface(InterfaceId::new("foreign"));
    assert!(Box::pin(hand_owned_output(&ctx, &foreign, &callback, file.clone())).await.is_err());
    foreign = source.clone(); foreign.envelope.metadata.as_mut().unwrap().runtime.as_mut().unwrap().epoch += 1;
    assert!(Box::pin(hand_owned_output(&ctx, &foreign, &callback, file.clone())).await.is_err());
    let mut wrong = failure.clone(); wrong.correlation_id = Some("code-mode-resume:foreign:invalid".into());
    assert!(Box::pin(hand_owned_output(&ctx, &source, &callback, materialize(&wrong))).await.is_err());
    let key = input.1.correlation_id.as_ref().unwrap().rsplit(':').next().unwrap();
    let pending = durable.join(format!("{key}.pending.json"));
    let saved_pending = std::fs::read(&pending).unwrap();
    std::fs::write(&pending, b"{}").unwrap();
    assert!(Box::pin(hand_owned_output(&ctx, &source, &callback, file.clone())).await.is_err());
    std::fs::write(&pending, &saved_pending).unwrap();
    let admitted = durable.join(format!("{key}.admitted.json"));
    let saved_admitted = std::fs::read(&admitted).unwrap();
    let witness: Value = serde_json::from_slice(&saved_admitted).unwrap();
    assert_eq!(witness["source_movement_seq"], json!(input.0));
    assert_eq!(witness["artifact"], json!(input.2.artifact));
    let withheld = durable.join(format!("{key}.withheld-admitted.json"));
    std::fs::rename(&admitted, &withheld).unwrap();
    assert!(Box::pin(hand_owned_output(&ctx, &source, &callback, file.clone())).await.is_err());
    std::fs::rename(&withheld, &admitted).unwrap();
    for (field, value) in [("source_movement_seq", json!(input.0 + 1)), ("source_interface", json!("foreign")),
        ("artifact", json!({"id":"foreign", "content":input.2.artifact.content})),
        ("payload_hash", json!(ContentHash::of_bytes(b"foreign").to_hex())),
        ("metadata_hash", json!(ContentHash::of_bytes(b"foreign").to_hex()))] {
        let mut changed = witness.clone(); changed[field] = value;
        std::fs::write(&admitted, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(Box::pin(hand_owned_output(&ctx, &source, &callback, file.clone())).await.is_err(), "accepted changed {field}");
    }
    std::fs::write(&admitted, &saved_admitted).unwrap();
    let boundary = durable.join(format!("{key}.{}.return.json", input.2.runtime().unwrap().run_record_hash.as_ref().unwrap()));
    let saved_boundary = std::fs::read(&boundary).unwrap();
    let mut changed: Value = serde_json::from_slice(&saved_boundary).unwrap(); changed["epoch"] = json!(input.2.runtime().unwrap().epoch + 1);
    std::fs::write(&boundary, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(Box::pin(hand_owned_output(&ctx, &source, &callback, file.clone())).await.is_err());
    std::fs::write(&boundary, &saved_boundary).unwrap();
    for value in [json!("released"), json!(input.0 + 1)] {
        let mut changed: Value = serde_json::from_slice(&record_bytes).unwrap();
        if value.is_string() { changed["lifecycle"] = value; }
        else { changed["opened_at_movement_seq"] = value.clone(); changed["last_activity_movement_seq"] = value; }
        std::fs::write(&record_path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(Box::pin(hand_owned_output(&ctx, &source, &callback, file.clone())).await.is_err());
    }
    std::fs::write(&record_path, &record_bytes).unwrap();
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
    let old_output = admission.deliveries.lock().unwrap().iter().find(|row|
        row.from.as_str() == DOOR && row.to.as_str() == "world_provider_test").cloned().unwrap();
    assert_eq!(old_output.metadata.artifact_binding_refs, input.2.metadata().unwrap().artifact_binding_refs,
        "the old declared producer must retain its role, prelude, topology and static input bindings");
    let run_path = admission.run_records.join("run_code_mode_test.json");
    let before = std::fs::read(&run_path).unwrap();
    let before_value: Value = serde_json::from_slice(&before).unwrap();
    let writer = config.resolve_owner_topology(&input.2).unwrap().coauthor.unwrap();
    let mut desired = before_value["interface_state"][writer.owner.as_str()]["run_authoring_binding"]["checkpoints"]
        .as_array().unwrap().iter().filter(|row| row["role"] == "owner_input").cloned().collect::<Vec<_>>();
    assert_eq!(desired.len(), 1);
    assert_eq!(desired[0]["include_context"], false);
    desired[0]["include_context"] = json!(true);
    desired[0].as_object_mut().unwrap().remove("static_artifact_bindings");
    let edit = match octopus_run_authoring::prepare_provider_input_revision(run_path.clone(), &before, &writer, desired).unwrap() {
        octopus_run_authoring::ProviderInputRevision::Prepared { edit } => edit,
        other => panic!("the settled old output must allow the new definition: {other:?}"),
    };
    let context_bytes = std::fs::read(durable.join("context.json")).unwrap();
    let carrier = match octopus_run_authoring::OwnerWriterEdit::prepare(
        &before, &edit, &writer, ctx.owner_io_limits().unwrap().max_payload_bytes).unwrap() {
        Some(ordered) => ordered.materialize(&admission.root.join("input-revision")).unwrap(),
        None => InterfaceArtifactFile::write(admission.root.join("input-revision"), &writer.call_label,
            &serde_json::to_vec(&json!({"tool":writer.tool,"arguments":edit.arguments().unwrap(),"correlation_id":edit.correlation_id})).unwrap()).unwrap(),
    };
    admission.arrive(writer.source.as_str(), &carrier).await.unwrap();
    let after = std::fs::read(&run_path).unwrap();
    assert_eq!(after.as_slice(), edit.content.as_bytes());
    let after_value: Value = serde_json::from_slice(&after).unwrap();
    assert_eq!(after_value["status"], before_value["status"]);
    assert_eq!(after_value["interface_state"][DOOR], before_value["interface_state"][DOOR]);
    assert_eq!(after_value["interface_state"][writer.owner.as_str()]["authoring_generation"].as_u64().unwrap(),
        before_value["interface_state"][writer.owner.as_str()]["authoring_generation"].as_u64().unwrap() + 1);
    for field in ["pending_handoff", "context_generation", "last_authored_artifact"] {
        assert_eq!(after_value["interface_state"][writer.owner.as_str()][field], before_value["interface_state"][writer.owner.as_str()][field]);
    }
    let checked = super::super::continuation::reconcile_checked_provider_advance(&ctx, durable, config, None).unwrap().unwrap();
    assert_eq!(checked.after_hash, edit.after_hash);
    assert!(checked.source_seq > input.0 && checked.result_seq > checked.source_seq);
    assert_eq!(std::fs::read(durable.join("context.json")).unwrap(), context_bytes);
    assert_eq!(std::fs::read(&pending).unwrap(), saved_pending);
    assert_eq!(std::fs::read(&admitted).unwrap(), saved_admitted);
    assert_eq!(nested_count(), effects);
    let cell = serde_json::from_slice::<Value>(&saved_pending).unwrap()["identity"]["cell_id"].clone();
    let wait = envelope(&admission.root, 9099, "code_mode.wait", json!({"cell_id":cell,"yield_time_ms":20}));
    admission.model_call(&wait.artifact_file).await.unwrap();
    let wait = admission.next().await.unwrap();
    assert!(wait.0 > input.0);
    let wait_pin = wait.2.runtime().unwrap().clone();
    assert_ne!(wait_pin.checkpoint, input.2.runtime().unwrap().checkpoint);
    let cold_room = CodeModeRoom::new(room.environment.clone());
    let error = cold_room.receive_with_sink(ToolProcessInvocation {
        request_id: wait.0 + 1, source_movement_seq: wait.0,
        descriptor: effective_descriptor(&wait.1.tool), envelope: wait.2.clone(),
        worker_environment: Some(formation.clone()), worker_environment_kind: Some(kind.clone()),
    }, &FakeSink::default()).await.err().expect("cold wait executed without a live context");
    assert!(matches!(error, ToolProcessError::Capability(ref detail) if detail.contains("no live context to continue")));
    records.fail(formation, wait.0).unwrap();
    let record_bytes = std::fs::read(&record_path).unwrap();
    let record = records.read(&formation.worker_environment_id).unwrap();
    let mut wait_source = admission.door.lock().await.output_source(wait.0, &wait.2, formation).unwrap();
    wait_source.formation = None;
    let wait_failure = ToolResultArtifact::failure(&wait.1.tool, wait.1.correlation_id.clone(),
        ToolResultError::new("tool_process_failure", "persistent code mode has no live context to continue")
            .with_details(serde_json::Map::from_iter([("source_movement_seq".into(), json!(wait.0))])));
    let wait_file = materialize(&wait_failure);
    let wait_proof = serde_json::to_value(&wait_source.movement).unwrap();
    let wait_metadata = serde_json::to_value(wait_source.envelope.metadata().unwrap()).unwrap();
    admission.provider_progress(9099).await.unwrap();
    let run_path = admission.run_records.join("run_code_mode_test.json");
    let before_replacement: Value = serde_json::from_slice(&std::fs::read(&run_path).unwrap()).unwrap();
    assert!(!before_replacement["active_checkpoints"].as_array().unwrap().contains(&json!(wait_pin.checkpoint)));
    let old_frontier = before_replacement["interface_state"][DOOR]["frontier_checkpoint_ids"].clone();
    admission.reject_next_owner_arrival.store(true, Ordering::SeqCst);
    assert!(matches!(Box::pin(hand_owned_output(&ctx, &source, &callback, file.clone())).await.unwrap(),
        DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::Policy, .. }));
    assert!(!ctx.owner_blobs_dir().join(reference.content.to_hex()).exists());
    let other_source = admission.door.lock().await.output_source(other_input.0, &other_input.2, formation).unwrap();
    let other_failure = ToolResultArtifact::failure(&other_input.1.tool, other_input.1.correlation_id.clone(),
        ToolResultError::new("tool_effect_indeterminate", "fixture retained neighboring continuation")
            .with_details(serde_json::Map::from_iter([("source_movement_seq".into(), json!(other_input.0))])));
    let other_file = materialize(&other_failure);
    admission.reject_next_owner_arrival.store(true, Ordering::SeqCst);
    assert!(matches!(Box::pin(hand_owned_output(&ctx, &other_source, &actual_owner_handoff::OwnerHandoff::new().callback(), other_file.clone())).await.unwrap(),
        DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::Policy, .. }));
    admission.reject_next_owner_arrival.store(true, Ordering::SeqCst);
    assert!(matches!(Box::pin(hand_owned_output(&ctx, &source, &actual_owner_handoff::OwnerHandoff::new().callback(), file.clone())).await.unwrap(),
        DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::Policy, .. }));
    let latest_hash = ContentHash::of_bytes(&std::fs::read(&run_path).unwrap()).to_hex();
    let installation = durable.join(format!("{latest_hash}.installed.json"));
    let installation_bytes = std::fs::read(&installation).unwrap();
    let installed: super::super::continuation::InstalledHandoff = serde_json::from_slice(&installation_bytes).unwrap();
    let config: crate::config::CodeModeConfig = serde_json::from_slice(&std::fs::read(ctx.interface_registry_dir().join("code_mode.json")).unwrap()).unwrap();
    let generation = super::super::continuation::digest(&json!({"before":installed.before_hash,
        "emission":reference.content.to_hex(),"edge":config.resolve_owner_topology(&input.2).unwrap().receipt,
        "source_seq":input.0})).unwrap();
    let proposal = durable.join(format!("{generation}.edit.json"));
    let withheld_proposal = proposal.with_extension("withheld");
    let writes = admission.file_writes.load(Ordering::SeqCst);
    std::fs::rename(&proposal, &withheld_proposal).unwrap();
    assert!(Box::pin(hand_owned_output(&ctx, &source, &actual_owner_handoff::OwnerHandoff::new().callback(), file.clone())).await.is_err());
    std::fs::rename(&withheld_proposal, &proposal).unwrap();
    let withheld_installation = installation.with_extension("withheld");
    std::fs::rename(&installation, &withheld_installation).unwrap();
    let prior_boundary = durable.join(format!("{key}.{}.return.json", installed.before_hash));
    let prior_bytes = std::fs::read(&prior_boundary).unwrap();
    for (field, value) in [("run_record_hash", json!(ContentHash::of_bytes(b"foreign").to_hex())),
        ("epoch", json!(999)), ("binding", json!({"inbound":{"interface":"foreign","binding":"foreign"},"outbound":[]}))] {
        let mut boundary: Value = serde_json::from_slice(&prior_bytes).unwrap(); boundary[field] = value;
        std::fs::write(&prior_boundary, serde_json::to_vec(&boundary).unwrap()).unwrap();
        assert!(Box::pin(hand_owned_output(&ctx, &source, &actual_owner_handoff::OwnerHandoff::new().callback(), file.clone())).await.is_err(), "carried malformed {field}");
    }
    std::fs::write(&prior_boundary, &prior_bytes).unwrap();
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
    let receipt = durable.join(format!("{}.receipt.json", reference.content.to_hex()));
    let receipt_bytes = std::fs::read(&receipt).unwrap();
    let writes = admission.file_writes.load(Ordering::SeqCst);
    let restarted = actual_owner_handoff::OwnerHandoff::new();
    let callback = restarted.callback();
    let changed = json!({"artifact":reference,"source_seq":input.0+1});
    std::fs::write(&receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(Box::pin(hand_owned_output(&ctx, &source, &callback, file.clone())).await.is_err());
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
    std::fs::write(&receipt, &receipt_bytes).unwrap();
    let (first, second) = tokio::join!(
        Box::pin(hand_owned_output(&ctx, &source, &callback, file.clone())),
        Box::pin(hand_owned_output(&ctx, &source, &callback, file.clone())),
    );
    let returns = [first.unwrap(), second.unwrap()];
    assert_eq!(returns.iter().filter(|v| matches!(v, DeliveryVerdict::Admitted { .. })).count(), 1);
    assert_eq!(returns.iter().filter(|v| matches!(v, DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::CheckpointSpent, .. })).count(), 1);
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes + 1, "cold replay repeated the installed receipt write");
    assert_eq!(std::fs::read(&installation).unwrap(), installation_bytes);
    std::fs::remove_file(withheld_installation).unwrap();
    let delivered: Vec<_> = admission.deliveries.lock().unwrap().iter().filter(|row|
        row.from.as_str() == DOOR && row.file.read_manifest().unwrap().label == failure.label).cloned().collect();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].to.as_str(), DOOR);
    assert_eq!(delivered[0].file.read_payload_bytes().unwrap(), file.read_payload_bytes().unwrap());
    let writes = admission.file_writes.load(Ordering::SeqCst);
    let limits = ctx.owner_io_limits().unwrap();
    let evidence = CheckedRunEvidence { movements_path:ctx.owner_movements_path(),ledger_index_path:ctx.owner_ledger_index_path(),
        blobs_dir:ctx.owner_blobs_dir(),max_control_frame_bytes:limits.max_control_frame_bytes,
        max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes };
    let movement = octopus_run_authoring::generated_artifacts::read_movement(&evidence, delivered[0].seq).unwrap();
    let mut reader = octopus_tool_door_runtime::LedgerTailReader::open_named(&ctx, "nested-failure-contract").unwrap();
    for _ in 0..2 {
        let cold = actual_owner_handoff::OwnerHandoff::new();
        assert!(matches!(Box::pin(hand_owned_output(&ctx, &source, &cold.callback(), file.clone())).await.unwrap(),
            DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::CheckpointSpent, detail }
            if detail["recorded_result_movement_seq"] == json!(delivered[0].seq)));
        cold.reconcile_failure(&ctx, &mut reader, &movement).await.unwrap();
    }
    std::fs::write(&receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(restarted.reconcile_failure(&ctx, &mut reader, &movement).await.is_err());
    std::fs::write(&receipt, &receipt_bytes).unwrap();
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
    assert_eq!(nested_count(), effects);
    assert_eq!(std::fs::read(&pending).unwrap(), saved_pending);
    assert!(!durable.join(format!("{key}.accepted.json")).exists(), "failure recovery executed the continuation");
    assert_eq!(std::fs::read(&record_path).unwrap(), record_bytes);
    assert_eq!(std::fs::read(&admitted).unwrap(), saved_admitted);
    let current: Value = serde_json::from_slice(&std::fs::read(&run_path).unwrap()).unwrap();
    assert!(old_frontier.as_array().unwrap().iter().all(|id| !current["active_checkpoints"].as_array().unwrap().contains(id)));
    let replacing = admission.deliveries.lock().unwrap().iter().filter(|row|
        row.seq > wait.0 && row.from.as_str() == DOOR && row.to.as_str() == "system_file_test"
        && row.metadata.runtime.as_ref().unwrap().checkpoint == input.2.runtime().unwrap().checkpoint).cloned().collect::<Vec<_>>();
    assert_eq!(replacing.len(), 2, "retained installations must include the replaced and current nested output");
    assert!(!old_frontier.as_array().unwrap().contains(&json!(replacing[0].metadata.runtime.as_ref().unwrap().checkpoint)));
    let replacing_call: ToolCallArtifact = serde_json::from_slice(&replacing[0].file.read_payload_bytes().unwrap()).unwrap();
    let generation = replacing_call.correlation_id.as_ref().unwrap().rsplit(':').next().unwrap();
    let edit_path = durable.join(format!("{generation}.edit.json"));
    let edit: Value = super::super::continuation::read_record(&edit_path).unwrap();
    let install_path = durable.join(format!("{}.installed.json", edit["after_hash"].as_str().unwrap()));
    for (path, field, changed) in [
        (receipt.clone(), "source_seq", json!(input.0 + 1)),
        (edit_path, "content", json!("{}")),
        (install_path, "source_artifact", json!({"id":"foreign", "content":reference.content})),
        (admitted.clone(), "metadata_hash", json!(ContentHash::of_bytes(b"foreign").to_hex())),
    ] {
        let saved = std::fs::read(&path).unwrap();
        let mut value: Value = serde_json::from_slice(&saved).unwrap(); value[field] = changed;
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(Box::pin(hand_owned_output(&ctx, &wait_source, &actual_owner_handoff::OwnerHandoff::new().callback(), wait_file.clone())).await.is_err(), "accepted changed {field}");
        std::fs::write(&path, saved).unwrap();
    }
    let call_ref = super::super::continuation::artifact_ref(&replacing[0].file).unwrap();
    let manifest_path = ctx.owner_blobs_dir().join(call_ref.content.to_hex());
    let withheld = manifest_path.with_extension("withheld");
    std::fs::rename(&manifest_path, &withheld).unwrap();
    std::fs::write(&manifest_path, b"{}").unwrap();
    assert!(Box::pin(hand_owned_output(&ctx, &wait_source, &actual_owner_handoff::OwnerHandoff::new().callback(), wait_file.clone())).await.is_err());
    std::fs::remove_file(&manifest_path).unwrap();
    std::fs::rename(&withheld, &manifest_path).unwrap();
    let prospective = other_file.manifest_path();
    let saved_prospective = std::fs::read(prospective).unwrap();
    std::fs::write(prospective, b"{}").unwrap();
    assert!(Box::pin(hand_owned_output(&ctx, &wait_source, &actual_owner_handoff::OwnerHandoff::new().callback(), wait_file.clone())).await.is_err());
    std::fs::write(prospective, saved_prospective).unwrap();
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
    assert_eq!(nested_count(), effects);
    let cold = actual_owner_handoff::OwnerHandoff::new();
    assert!(matches!(Box::pin(hand_owned_output(&ctx, &wait_source, &cold.callback(), wait_file.clone())).await.unwrap(), DeliveryVerdict::Admitted { .. }));
    let settled = admission.deliveries.lock().unwrap().iter().filter(|row|
        row.from.as_str() == DOOR && row.to.as_str() == "world_provider_test"
        && row.file.read_payload_bytes().unwrap() == wait_file.read_payload_bytes().unwrap()).cloned().collect::<Vec<_>>();
    assert_eq!(settled.len(), 1);
    let result_pin = &settled[0].metadata.runtime.as_ref().unwrap().run_record_hash.as_ref().unwrap();
    let pinned = octopus_run_record::RunRecord::from_bytes(
        &std::fs::read(ctx.owner_blobs_dir().join(result_pin)).unwrap()).unwrap();
    let bindings = octopus_run_authoring::provider_context_bindings(
        &pinned, &InterfaceId::new("world_provider_test")).unwrap();
    assert_eq!(settled[0].metadata.artifact_binding_refs.iter().map(|reference| &reference.id).collect::<Vec<_>>(),
        bindings.iter().map(|binding| &binding.id).collect::<Vec<_>>());
    let restored = admission.provider_handoff().unwrap().unwrap().current_session_context_artifacts().await;
    let call = restored.iter().find(|entry| entry.source_artifact == wait.2.artifact).unwrap();
    assert_eq!(call.label, "tool.call");
    assert_eq!(call.payload, serde_json::to_value(&wait.1).unwrap());
    assert!(restored.iter().any(|entry| entry.source_artifact == super::super::continuation::artifact_ref(&wait_file).unwrap()
        && entry.payload == serde_json::to_value(&wait_failure).unwrap()));
    let writes = admission.file_writes.load(Ordering::SeqCst);
    assert!(matches!(Box::pin(hand_owned_output(&ctx, &wait_source, &actual_owner_handoff::OwnerHandoff::new().callback(), wait_file)).await.unwrap(),
        DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::CheckpointSpent, .. }));
    assert_eq!(serde_json::to_value(&wait_source.movement).unwrap(), wait_proof);
    assert_eq!(serde_json::to_value(wait_source.envelope.metadata().unwrap()).unwrap(), wait_metadata);
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
    assert_eq!(nested_count(), effects);
    let ordinary = envelope(&admission.root, 9100, "code_mode.run", json!({"script":"text('MUST_NOT_EXECUTE');", "yield_time_ms":20}));
    admission.model_call(&ordinary.artifact_file).await.unwrap();
    let ordinary = admission.next().await.unwrap();
    assert_eq!(ordinary.2.read_artifact_manifest().unwrap().label, "tool.call");
    assert!(ordinary.0 > record.last_activity_movement_seq);
    let mut ordinary_source = admission.door.lock().await.output_source(ordinary.0, &ordinary.2, formation).unwrap();
    ordinary_source.formation = None;
    let ordinary_failure = ToolResultArtifact::failure(&ordinary.1.tool, ordinary.1.correlation_id.clone(),
        ToolResultError::new("tool_effect_indeterminate", "fixture unexecuted ordinary call")
            .with_details(serde_json::Map::from_iter([("source_movement_seq".into(), json!(ordinary.0))])));
    let writes = admission.file_writes.load(Ordering::SeqCst);
    assert_eq!(Box::pin(hand_owned_output(&ctx, &ordinary_source, &callback, materialize(&ordinary_failure))).await.unwrap_err(),
        "retained output has no exact recorded formation");
    assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
    assert_eq!(nested_count(), effects);
    assert_eq!(std::fs::read(&record_path).unwrap(), record_bytes);
}

fn arrival<'a>(
    admission: &'a FrontierAdmission,
    file: &'a InterfaceArtifactFile,
) -> Pin<Box<dyn Future<Output = Result<u64, String>> + Send + 'a>> {
    Box::pin(admission.arrive(DOOR, file))
}

pub(super) fn actual_refused_failure_return<'a>(
    admission: &'a std::sync::Arc<FrontierAdmission>, room: &'a CodeModeRoom,
    handoff: &'a std::sync::Arc<actual_owner_handoff::OwnerHandoff>, formation: &'a WorkerEnvironmentFormation,
    _kind: &WorkerEnvKindSpec, config: &'a super::super::config::CodeModeConfig,
    initial: &'a (u64, ToolCallArtifact, InterfaceInvocationEnvelope), durable: &'a Path,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        use octopus_tool_door_runtime::owner_contract_tests::hand_owned_output;
        let ctx = admission.door.lock().await.context.clone();
        super::super::continuation::ContinuationContext::create(durable, formation, config,
            &config.resolve_context(&initial.2).unwrap(), &initial.2,
            super::super::continuation::OriginalRun::from_envelope(&initial.2, initial.0).unwrap()).unwrap();
        let records = octopus_worker_env::WorkerEnvironmentRecordStore::new(ctx.interface_var_dir());
        records.activate(formation, initial.0).unwrap();
        let source = admission.door.lock().await.output_source(initial.0, &initial.2, formation).unwrap();
        let first = ToolResultArtifact::success(&initial.1.tool, initial.1.correlation_id.clone(),
            json!({"state":"completed","error_text":"SyntaxError: Illegal return statement"}));
        let first_file = InterfaceArtifactFile::write(ctx.interface_var_dir().join("failure-artifacts"),
            &first.label, &serde_json::to_vec(&first).unwrap()).unwrap();
        admission.reject_next_owner_arrival.store(true, Ordering::SeqCst);
        assert!(matches!(hand_owned_output(&ctx, &source, &handoff.callback(), first_file.clone()).await.unwrap(),
            DeliveryVerdict::Refused { .. }));
        arrival(admission, &first_file).await.unwrap();
        let failure_file = |input: &(u64, ToolCallArtifact, InterfaceInvocationEnvelope)| {
            let result = ToolResultArtifact::failure(&input.1.tool, input.1.correlation_id.clone(),
                ToolResultError::new("tool_effect_indeterminate", "checked historical interval ends during an unjoined authoring call")
                    .with_details(serde_json::Map::from_iter([("source_movement_seq".into(), json!(input.0))])));
            InterfaceArtifactFile::write(ctx.interface_var_dir().join("failure-artifacts"),
                &result.label, &serde_json::to_vec(&result).unwrap()).unwrap()
        };
        let limits = ctx.owner_io_limits().unwrap();
        let evidence = CheckedRunEvidence {
            movements_path: ctx.owner_movements_path(), ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
            max_control_frame_bytes: limits.max_control_frame_bytes, max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes,
        };
        let delivered_to_model = || admission.deliveries.lock().unwrap().iter().filter(|row|
            row.from.as_str() == DOOR && row.to.as_str() == "world_provider_test" && row.file.read_manifest().unwrap().label == first.label)
            .cloned().collect::<Vec<_>>();
        let answered = delivered_to_model();
        assert_eq!(answered.len(), 1);
        let answered = answered[0].clone();
        let writes = admission.file_writes.load(Ordering::SeqCst);
        let closed = hand_owned_output(&ctx, &source, &handoff.callback(), failure_file(initial)).await.unwrap();
        assert!(matches!(closed, DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::CheckpointSpent, detail }
            if detail["recorded_result_movement_seq"] == json!(answered.seq)), "an answered call took a second result");
        assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes);
        assert_eq!(delivered_to_model().len(), 1);

        let next = envelope(&admission.root, 9001, "code_mode.run", json!({"script":"text('MUST_NOT_EXECUTE');", "yield_time_ms":20}));
        admission.model_call(&next.artifact_file).await.unwrap();
        let next = admission.next().await.unwrap();
        assert_eq!(next.2.read_artifact_manifest().unwrap().label, "tool.call");
        assert_eq!(next.1.tool, "code_mode.run");
        let run_path = admission.run_records.join("run_code_mode_test.json");
        let before_progress = std::fs::read(&run_path).unwrap();
        admission.provider_progress(9002).await.unwrap();
        let after_progress = std::fs::read(&run_path).unwrap();
        actual_checked_recovery_observation(&ctx, &run_path, &before_progress, &after_progress, next.0,
            &config.resolve_owner_topology(&next.2).unwrap().coauthor.unwrap());
        records.activate(formation, next.0).unwrap();
        let second_file = failure_file(&next);
        let mut native_source = admission.door.lock().await.output_source(next.0, &next.2, formation).unwrap();
        native_source.formation = None;
        let writes = admission.file_writes.load(Ordering::SeqCst);
        admission.reject_next_owner_arrival.store(true, Ordering::SeqCst);
        let interrupted = hand_owned_output(&ctx, &native_source, &handoff.callback(), second_file.clone()).await;
        assert!(format!("{interrupted:?}").contains("fixture interrupted after checked owner installation"), "{interrupted:?}");
        assert_eq!(admission.file_writes.load(Ordering::SeqCst), writes + 1);
        assert_eq!(delivered_to_model().len(), 1);
        let restarted = actual_owner_handoff::OwnerHandoff::new();
        let callback = restarted.callback();
        let writes = admission.file_writes.load(Ordering::SeqCst);
        let returned = hand_owned_output(&ctx, &native_source, &callback, second_file.clone()).await.unwrap();
        assert!(matches!(returned, DeliveryVerdict::Admitted { .. }), "{returned:?}");
        let delivered = delivered_to_model();
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered[0].file.read_payload_bytes().unwrap(), first_file.read_payload_bytes().unwrap());
        assert_eq!(delivered[1].file.read_payload_bytes().unwrap(), second_file.read_payload_bytes().unwrap());
        let installed_writes = admission.file_writes.load(Ordering::SeqCst);
        assert_eq!(installed_writes, writes + 1, "the prepared return was rewritten once before its delivery");
        let verdict = hand_owned_output(&ctx, &native_source, &callback, second_file.clone()).await.unwrap();
        assert!(matches!(verdict, DeliveryVerdict::Refused { reason: octopus_interfaces::RefusedReason::CheckpointSpent, detail }
            if detail["recorded_result_movement_seq"] == json!(delivered[1].seq)));
        assert!(octopus_tool_door_runtime::owner_contract_tests::hand_owned_invocation_failure(&ctx, &native_source, &callback)
            .await.unwrap());

        arrival(admission, &second_file).await.unwrap();
        assert_eq!(delivered_to_model().len(), 2, "a standing return reached the model twice with one failure");
        let looped = {
            let reference = super::super::continuation::artifact_ref(&second_file).unwrap();
            let mut reader = FileCheckedRunReader::open(&evidence, None).unwrap();
            let mut selected = None;
            while let Some(movement) = reader.movement(reader.next_sequence()).unwrap() {
                if movement.kind == octopus_ledger::MovementKind::InterfaceOutbound && movement.artifact.as_ref() == Some(&reference)
                    && movement.from == octopus_ledger::Endpoint::Interface(InterfaceId::new(DOOR))
                    && movement.to == octopus_ledger::Endpoint::Interface(InterfaceId::new(DOOR)) {
                    selected = Some(movement);
                }
            }
            selected.unwrap()
        };
        let mut reader = octopus_tool_door_runtime::LedgerTailReader::open_named(&ctx, "refused-failure-contract").unwrap();
        let mut wrong = looped.clone(); wrong.artifact.as_mut().unwrap().id = ArtifactId::new("foreign-failure");
        assert!(restarted.reconcile_failure(&ctx, &mut reader, &wrong).await.is_err());
        restarted.reconcile_failure(&ctx, &mut reader, &looped).await.unwrap();
        assert_eq!(delivered_to_model().len(), 2);
        assert_eq!(admission.file_writes.load(Ordering::SeqCst), installed_writes);
        assert!(room.rooms.lock().await.is_empty(), "failure recovery executed the admitted script");
    })
}

fn actual_checked_recovery_observation(
    ctx: &octopus_tool_door_runtime::InterfaceLiftContext, path: &Path,
    before: &[u8], after: &[u8], after_seq: u64,
    writer: &octopus_run_authoring::CheckedRunWriter,
) {
    let limits = ctx.owner_io_limits().unwrap();
    let evidence = CheckedRunEvidence { movements_path: ctx.owner_movements_path(),
        ledger_index_path: ctx.owner_ledger_index_path(), blobs_dir: ctx.owner_blobs_dir(),
        max_control_frame_bytes: limits.max_control_frame_bytes,
        max_manifest_bytes: limits.max_manifest_bytes, max_payload_bytes: limits.max_payload_bytes };
    let prove = |captured: &[u8]| octopus_run_authoring::checked_declared_run_advance(
        &evidence, path, before, captured, after_seq, vec![writer.clone()]);
    let expected = prove(after).unwrap();
    assert!(!expected.is_empty());
    let mut invalid: Value = serde_json::from_slice(after).unwrap();
    invalid["status"] = json!("unproved mutation");
    let invalid = serde_json::to_vec(&invalid).unwrap();
    let invalid_error = prove(&invalid).unwrap_err();
    for first in [before, invalid.as_slice()] {
        let mut reads = std::collections::VecDeque::from([
            first.to_vec(), after.to_vec(), after.to_vec(), after.to_vec()]);
        let mut attempts = 0;
        let checked = super::super::continuation::checked_recovery_observation(
            || Ok(reads.pop_front().unwrap()),
            |captured| { attempts += 1; prove(captured) }, || false).unwrap();
        assert_eq!(attempts, 2);
        assert!(reads.is_empty());
        assert_eq!(checked.last().unwrap().after, expected.last().unwrap().after);
    }
    let mut attempts = 0;
    let error = super::super::continuation::checked_recovery_observation(
        || Ok(invalid.clone()), |captured| { attempts += 1; prove(captured) }, || false).unwrap_err();
    assert_eq!(error.to_string(), invalid_error.to_string());
    assert_eq!(attempts, 1);
    let cancelled = std::cell::Cell::new(false);
    let mut reads = 0;
    let error = super::super::continuation::checked_recovery_observation(
        || { reads += 1; if reads == 2 { cancelled.set(true); } Ok(if reads == 1 { before.to_vec() } else { after.to_vec() }) },
        prove, || cancelled.get()).unwrap_err();
    assert_eq!(error.to_string(), "shared run recovery cancelled");
    assert_eq!(reads, 2);
    assert_eq!(std::fs::read(path).unwrap(), after);
}
