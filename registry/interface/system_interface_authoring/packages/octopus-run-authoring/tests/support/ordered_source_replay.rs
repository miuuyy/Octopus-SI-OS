async fn exercise_interrupted_source_order(
    original_ctx: &InterfaceLiftContext, descriptor: &InterfaceDescriptor,
    initial: &[u8], input: &InterfaceInvocationEnvelope,
) {
    fn append(h: &ProviderOutputHandoff, rows: &mut Vec<Movement>, row: Movement) {
        rows.push(row);
        std::fs::write(&h.ctx.movements_path, rows.iter().map(|row|
            format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    }
    fn edge(h: &ProviderOutputHandoff, rows: &mut Vec<Movement>, bytes: &[u8], cp: &CheckpointId,
        file: &InterfaceArtifactFile, target: InterfaceId, kind: MovementKind) {
        let record = RunRecord::from_bytes(bytes).unwrap();
        let checkpoint = record.checkpoint(cp).unwrap();
        let hash = ContentHash::of_bytes(bytes).to_hex();
        std::fs::write(h.ctx.blobs_dir.join(&hash), bytes).unwrap();
        let mut metadata = InvocationMetadata::new(GrantProjection {
            tools: vec![h.config.file_write_tool.clone()], interfaces: vec![target.to_string()],
            mark_outcomes: checkpoint.required_verified_artifacts.iter().map(|requirement| requirement.mark.to_string()).collect(), ..Default::default()
        }).with_runtime(RuntimeInvocationMetadata {
            run: record.id.clone(), ee: record.ee.clone(), worker: record.worker.clone(), checkpoint: cp.clone(),
            interface: if kind == MovementKind::InterfaceArrival { h.ctx.interface.clone() } else { target.clone() },
            epoch: checkpoint.epoch.0, run_record_hash: Some(hash),
        }).with_outbound_interface_bindings(checkpoint.outbound_interface_bindings.iter().map(|b| b.interface.clone()).collect())
            .with_checkpoint_binding_policy(CheckpointBindingPolicySnapshot {
                inbound: checkpoint.interface_binding.clone().unwrap(), outbound: checkpoint.outbound_interface_bindings.clone(),
        }).with_required_verified_artifacts(checkpoint.required_verified_artifacts.clone()).with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {
                artifact_label: file.read_manifest().unwrap().label,
                required_artifact_labels: checkpoint.required_verified_artifacts.iter().map(|requirement| requirement.artifact_label.clone()).collect(), complete: true,
            });
        metadata.artifact_binding_refs = checkpoint.artifact_bindings.iter().map(|binding| {
            let hash = binding.id.as_str().rsplit(':').next().and_then(ContentHash::from_hex).unwrap();
            ArtifactRef { id: binding.id.clone(), content: hash }
        }).collect();
        let mut row = ledger_row(rows.len() as u64);
        row.kind = kind; row.from = Endpoint::Interface(checkpoint.interface_binding.as_ref().unwrap().interface.clone());
        row.to = Endpoint::Interface(target); row.artifact = Some(pin_artifact(file, &h.ctx.blobs_dir));
        row.invocation_metadata = Some(metadata.into()); append(h, rows, row);
    }
    fn install(h: &ProviderOutputHandoff, rows: &mut Vec<Movement>, authored: &AuthoredRun) {
        let before = std::fs::read(&h.run_path).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        let call = h.materialize_file_write_call(authored).unwrap();
        let cp = active_checkpoint_for_role(&record, &h.config, RunAuthoringCheckpointRole::AuthoringCall).unwrap();
        edge(h, rows, &before, &cp.id, &call, authored.authoring_result_interface.clone(), MovementKind::InterfaceOutbound);
        std::fs::write(&h.run_path, &authored.content).unwrap();
        let result = InterfaceArtifactFile::write(h.var_dir.join("results"), &h.config.file_write_result_label,
            &serde_json::to_vec(&serde_json::json!({"label":h.config.file_write_result_label,"tool":h.config.file_write_tool,
                "correlation_id":authored.correlation_id,"ok":true,"body":{"result":"files_written","writes":[{
                    "path":h.run_path,"before_hash":authored.before_hash,"after_hash":authored.after_hash,"bytes":authored.content.len()}]}})).unwrap()).unwrap();
        edge(h, rows, authored.content.as_bytes(), &authored.authoring_result_checkpoint, &result, h.ctx.interface.clone(), MovementKind::InterfaceOutbound);
    }
    fn checked_call(h: &ProviderOutputHandoff, before: &[u8], edit: &CheckedRunEdit, root: &Path) -> InterfaceArtifactFile {
        let writer = declared_provider_run_writer(&RunRecord::from_bytes(before).unwrap(), &h.ctx.interface,
            &RunAuthoringProfile { file_write_tool:h.config.file_write_tool.clone(), file_write_label:h.config.file_write_label.clone(),
                file_write_result_label:h.config.file_write_result_label.clone(), result_timeout_ms:1000 }).unwrap();
        match OwnerWriterEdit::prepare(before, edit, &writer, h.ctx.required_host_io_limits().unwrap().max_payload_bytes).unwrap() {
            Some(ordered) => ordered.materialize(root).unwrap(),
            None => InterfaceArtifactFile::write(root, &writer.call_label,
                &serde_json::to_vec(&serde_json::json!({"tool":writer.tool,"correlation_id":edit.correlation_id,"arguments":edit.arguments().unwrap()})).unwrap()).unwrap(),
        }
    }
    fn install_edit(h: &ProviderOutputHandoff, rows: &mut Vec<Movement>, edit: &CheckedRunEdit) {
        let before = std::fs::read(&h.run_path).unwrap();
        let call = checked_call(h, &before, edit, &h.var_dir.join("revision-installs"));
        let writer_path = h.config.writer_run.as_ref().map_or_else(||h.run_path.clone(), |binding|
            h.run_path.parent().unwrap().join(format!("{}.json", binding.run)));
        let writer_before = std::fs::read(&writer_path).unwrap();
        let record = RunRecord::from_bytes(&writer_before).unwrap();
        let select = |record: &RunRecord, role| record.checkpoints.iter().find(|cp| record.active_checkpoints.contains(&cp.id)
            && checkpoint_record_matches_template(cp, h.config.checkpoint_for_role(role).unwrap(), &record.id)).unwrap().id.clone();
        let door = h.config.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult).unwrap().interface().clone();
        edge(h, rows, &writer_before, &select(&record, RunAuthoringCheckpointRole::AuthoringCall), &call, door, MovementKind::InterfaceOutbound);
        let envelope = InterfaceInvocationEnvelope { artifact:pin_artifact(&call, &h.ctx.blobs_dir),artifact_file:call.clone(),artifact_binding_payloads:vec![],
            metadata:Some(InvocationMetadata::new(GrantProjection {worker_environment_kinds:vec!["system_room".into()],..Default::default()})) };
        let payload: serde_json::Value = serde_json::from_slice(&call.read_payload_bytes().unwrap()).unwrap();
        let body = octopus_system_room_env::execute_file_write_result(&envelope, &payload["arguments"], &["system_room".into()],
            &BTreeMap::from([("run_records_root".into(),h.run_path.parent().unwrap().to_path_buf())]), &h.var_dir.join("revision-effects"),
            h.ctx.required_host_io_limits().unwrap().max_payload_bytes).unwrap();
        assert_eq!(std::fs::read(&h.run_path).unwrap(), edit.content.as_bytes());
        let after = std::fs::read(writer_path).unwrap();
        let result = InterfaceArtifactFile::write(h.var_dir.join("revision-results"), &h.config.file_write_result_label,
            &serde_json::to_vec(&serde_json::json!({"label":h.config.file_write_result_label,"tool":h.config.file_write_tool,
                "correlation_id":edit.correlation_id,"ok":true,"body":body})).unwrap()).unwrap();
        edge(h, rows, &after, &select(&RunRecord::from_bytes(&after).unwrap(), RunAuthoringCheckpointRole::AuthoringResult),
            &result, h.ctx.interface.clone(), MovementKind::InterfaceOutbound);
    }
    let mut ctx = original_ctx.clone();
    ctx.movements_path = ctx.var_dir.join("ordered-replay.jsonl");
    ctx.ledger_index_path = ctx.var_dir.join("ordered-replay.sqlite");
    ctx.root_slots.insert("interface_hand_in_root".into(), ctx.var_dir.join("ordered-hand-in"));
    std::fs::write(&ctx.movements_path, b"").unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(initial).unwrap();
    value["checkpoints"][0]["artifact_bindings"] = serde_json::json!([]);
    let run = RunRecord::from_bytes(initial).unwrap().id;
    let mut topology: BoundRunAuthoringTopology = serde_json::from_value(value["interface_state"][ctx.interface.as_str()][RUN_AUTHORING_BINDING].clone()).unwrap();
    let profile = run_authoring_profile(descriptor).unwrap();
    let requirements = serde_json::json!([{"mark":"tool.call.allowed","artifact_label":profile.file_write_label,
        "verifier":"tool_call_allowed","params":{"tools":[profile.file_write_tool]}}]);
    topology.checkpoints.iter_mut().find(|cp| cp.role == RunAuthoringCheckpointRole::AuthoringCall).unwrap().required_verified_artifacts = serde_json::from_value(requirements.clone()).unwrap();
    value["checkpoints"].as_array_mut().unwrap().iter_mut().find(|cp| cp["id"] == "authoring_call").unwrap()["required_verified_artifacts"] = requirements;
    let mut output_initial = value.clone();
    output_initial["interface_state"][ctx.interface.as_str()][RUN_AUTHORING_BINDING] = serde_json::to_value(&topology).unwrap();
    let control: RunAuthoringCheckpoint = serde_json::from_value(serde_json::json!({"name":"control","role":"independent_input",
        "interface_binding":names_binding("system_worker_widget_return", &["client.surface.document"]),"outbound_interface_bindings":[names_binding(ctx.interface.as_str(), &["client.surface.document"]), names_binding("world_home_client", &["client.surface.document"])],
        "include_context":false,"required_verified_artifacts":[]})).unwrap();
    let owner_input: RunAuthoringCheckpoint = serde_json::from_value(serde_json::json!({"name":"owner_result","role":"owner_input",
        "interface_binding":names_binding("system_owner_return", &["tool.result"]),
        "outbound_interface_bindings":[names_binding(ctx.interface.as_str(), &["tool.result"])],
        "include_context":true,"required_verified_artifacts":[],
        "owner_authoring":{"owner":"system_owner_return","source":"system_owner_return","door":"system_owner_writer",
            "tool":"file.write.owner","call_label":"tool.call","result_label":"file.write.owner.result"}})).unwrap();
    topology.checkpoints.extend([control.clone(), owner_input.clone()]);
    let mut future_input = owner_input.clone();
    future_input.name = "future_owner_result".into();
    future_input.include_context = false;
    future_input.interface_binding = names_binding(owner_input.interface().as_str(), &["owner.future.result"]);
    future_input.outbound_interface_bindings = vec![names_binding(ctx.interface.as_str(), &["owner.future.result"])];
    topology.checkpoints.push(future_input);
    value["interface_state"][ctx.interface.as_str()][RUN_AUTHORING_BINDING] = serde_json::to_value(topology).unwrap();
    value["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&control, "control", &run, &[]));
    value["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!("control"));
    value["checkpoints"].as_array_mut().unwrap().push(materialize_checkpoint(&owner_input, "owner_result", &run, &[]));
    value["active_checkpoints"].as_array_mut().unwrap().push(serde_json::json!("owner_result"));
    let initial = serde_json::to_vec(&value).unwrap();
    std::fs::write(ctx.root_slot("run_records_root").unwrap().join(format!("{run}.json")), &initial).unwrap();
    let mut input = input.clone();
    input.metadata.as_mut().unwrap().runtime.as_mut().unwrap().run_record_hash = Some(ContentHash::of_bytes(&initial).to_hex());
    let mut h = ProviderHandoffSession::new(&ctx).unwrap().from_invocation(&ctx, descriptor, &run, &input, 1452, &initial).unwrap();
    h.session_snapshot_labels.clear(); h.context_output_labels = BTreeSet::from(["interface.artifact.output".into()]);
    let widget = InterfaceArtifactFile::write(h.var_dir.join("widget"), "client.surface.document", br#"{"id":"task-widget","version":1}"#).unwrap();
    let owner_result = InterfaceArtifactFile::write(h.var_dir.join("owner-result"), "tool.result", br#"{"label":"tool.result","tool":"owner.run","correlation_id":"owner-call","ok":true,"body":{"output":"completed"}}"#).unwrap();
    let queued = InterfaceArtifactFile::write(h.var_dir.join("queued"), "operator.message", br#"{"text":"queued new message"}"#).unwrap();
    let queued_ref = pin_artifact(&queued, &h.ctx.blobs_dir);
    let mut rows = (0..1452).map(ledger_row).collect::<Vec<_>>();
    for (seq, role) in [(1452, RunAuthoringCheckpointRole::Stable), (1456, RunAuthoringCheckpointRole::IndependentInput)] {
        while rows.len() < seq { rows.push(ledger_row(rows.len() as u64)); }
        let record = RunRecord::from_bytes(&initial).unwrap();
        let cp = record.checkpoints.iter().find(|cp| record.active_checkpoints.contains(&cp.id)
            && checkpoint_record_matches_template(cp, h.config.checkpoint_for_role(role).unwrap(), &run)).unwrap();
        edge(&h, &mut rows, &initial, &cp.id, if role == RunAuthoringCheckpointRole::IndependentInput { &widget } else { &input.artifact_file }, ctx.interface.clone(), MovementKind::InterfaceOutbound);
    }
    let mut closure = None;
    for (source, terminal) in [(1456, true), (1452, true), (1481, false), (1481, true)] {
        h.source_movement_seq = source;
        let current = std::fs::read(&h.run_path).unwrap();
        let file = InterfaceArtifactFile::write(h.var_dir.join("emitted"), "interface.artifact.output",
            &serde_json::to_vec(&serde_json::json!({"text":format!("source {source} terminal {terminal}")})).unwrap()).unwrap();
        let identity = artifact_identity(&file, &h.var_dir).unwrap();
        let mut context = current_context_artifacts(&RunRecord::from_bytes(&current).unwrap(), &h.config).unwrap();
        context.push(identity.artifact.id.clone());
        let authored = h.author_run(&current, &context, &[PreparedHandoff { identity: identity.clone(),
            output_checkpoint:h.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),verified_inputs:vec![] }],
            true, (source == 1456).then_some(&control), false, terminal).unwrap();
        if source == 1452 {
            h.materialize_file_write_call(&authored).unwrap();
            let mut stop = h.clone(); stop.source_movement_seq = 1481;
            stop.state.lock().await.run_record_bytes = current.clone();
            assert!(stop.recover_source().await.unwrap_err().contains("private attempt belongs to source movement 1452, current movement is 1481"));
        }
        if source == 1456 {
            let before = RunRecord::from_bytes(&current).unwrap();
            let after = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
            let old = before.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &control, &run)).unwrap();
            let new = after.checkpoints.iter().find(|cp| checkpoint_record_matches_template(cp, &control, &run)).unwrap();
            assert_ne!(old.id, new.id, "each widget receipt authors its own next receiving checkpoint");
            assert_eq!(new.outbound_interface_bindings, control.outbound_interface_bindings);
            assert_eq!(after.status, serde_json::json!("idle"));
        }
        install(&h, &mut rows, &authored);
        edge(&h, &mut rows, authored.content.as_bytes(), &authored.pending_handoff.items[0].checkpoint.id, &file,
            h.config.worker.clone(), MovementKind::InterfaceArrival);
        if source == 1452 { closure = Some(identity.artifact); }
        let published = if authored.pending_handoff.context_publication.is_some() {
            let publication = h.author_context_publication(authored.content.as_bytes(), &authored.pending_handoff).unwrap();
            install(&h, &mut rows, &publication); publication
        } else { authored };
        if !terminal {
            let mut queued_replay = h.clone(); queued_replay.source_movement_seq = 1477;
            assert!(queued_replay.recover_overtaken_source(published.content.as_bytes(), &published.pending_handoff).is_err());
        }
        if source == 1456 {
            for (seq, role) in [(1477, RunAuthoringCheckpointRole::Stable), (1481, RunAuthoringCheckpointRole::OwnerInput)] {
                while rows.len() < seq { rows.push(ledger_row(rows.len() as u64)); }
                let record = RunRecord::from_bytes(published.content.as_bytes()).unwrap();
                let template = if role == RunAuthoringCheckpointRole::OwnerInput { &owner_input } else { h.config.checkpoint_for_role(role).unwrap() };
                let cp = record.checkpoints.iter().find(|cp| record.active_checkpoints.contains(&cp.id)
                    && checkpoint_record_matches_template(cp, template, &run)).unwrap().id.clone();
                edge(&h, &mut rows, published.content.as_bytes(), &cp, if seq == 1477 { &queued } else { &owner_result }, ctx.interface.clone(), MovementKind::InterfaceOutbound);
            }
        }
    }
    let current = std::fs::read(&h.run_path).unwrap();
    let pending = persisted_pending_handoff(&RunRecord::from_bytes(&current).unwrap(), &h.config.provider_interface).unwrap().unwrap();
    assert!(pending.terminal && pending.context_publication.is_none());
    h.session = ProviderHandoffSession::new(&h.ctx).unwrap();
    for (seq, expected) in [(1452, ProviderSourceRecovery::Completed), (1456, ProviderSourceRecovery::Completed), (1481, ProviderSourceRecovery::Completed), (1477, ProviderSourceRecovery::Process)] {
        h.source_movement_seq = seq;
        let mut state = h.state.lock().await; state.run_record_bytes = current.clone(); state.session_context.artifacts.clear();
        if seq == 1477 { append_session_payload(&mut state.session_context, "operator.message",
            &queued.read_payload_bytes().unwrap(), &queued_ref, Some(seq), "queued source").unwrap(); }
        drop(state);
        let (recovered, outcomes) = h.recover_source_with_outcomes().await.unwrap();
        assert_eq!(recovered, expected, "source {seq}");
        if seq == 1452 { assert!(outcomes.iter().any(|o| matches!(o, ProviderHandoffOutcome::Admitted {artifact,..} if Some(artifact) == closure.as_ref()))); }
        if seq == 1477 {
            let context = h.current_session_context_artifacts().await;
            assert!(context.iter().any(|entry| Some(&entry.source_artifact) == closure.as_ref()));
            assert!(context.iter().any(|entry| entry.source_artifact.id == pending.items[0].artifact_id));
            assert_eq!(context.last().unwrap().source_artifact, queued_ref);
        }
    }
    let mut invalid = h.clone();
    invalid.ctx.movements_path = h.var_dir.join("invalid-owner-source.jsonl");
    invalid.ctx.ledger_index_path = h.var_dir.join("invalid-owner-source.sqlite");
    let mut invalid_rows = rows.clone();
    let mut metadata = invalid_rows[1481].invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    metadata.runtime.as_mut().unwrap().epoch += 1;
    invalid_rows[1481].invocation_metadata = Some(metadata.into());
    std::fs::write(&invalid.ctx.movements_path, invalid_rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    invalid.session = ProviderHandoffSession::new(&invalid.ctx).unwrap();
    assert!(invalid.recover_overtaken_source(&current, &pending).is_err());
    for detached in [false, true] {
        let mut cold = h.clone();
        cold.ctx.movements_path = h.var_dir.join(format!("revision-cold-{detached}.jsonl"));
        cold.ctx.ledger_index_path = h.var_dir.join(format!("revision-cold-{detached}.sqlite"));
        cold.hand_in_root = h.var_dir.join(format!("revision-cold-calls-{detached}"));
        let mut seed: serde_json::Value = serde_json::from_slice(&initial).unwrap();
        if detached {
            let writer_id = RunId::new("run_revision_cold_writer");
            let writer = separate_writer_run(&mut seed, &cold.ctx.interface, &writer_id).unwrap();
            std::fs::write(cold.run_path.parent().unwrap().join(format!("{writer_id}.json")), JsonFormat::Compact.encode(&writer).unwrap()).unwrap();
        }
        let seed = JsonFormat::Compact.encode(&seed).unwrap();
        std::fs::write(&cold.run_path, &seed).unwrap();
        cold.config = attempt_config(&RunRecord::from_bytes(&seed).unwrap(), &h.config).unwrap();
        cold.bound_topology = test_bound_topology(&cold.config);
        let mut proof = vec![ledger_row(0)];
        let stable = |bytes: &[u8]| RunRecord::from_bytes(bytes).unwrap().checkpoints.into_iter().find(|cp|
            checkpoint_record_matches_template(cp, cold.config.checkpoint_for_role(RunAuthoringCheckpointRole::Stable).unwrap(), &run)).unwrap().id;
        edge(&cold, &mut proof, &seed, &stable(&seed), &input.artifact_file, cold.ctx.interface.clone(), MovementKind::InterfaceOutbound);
        cold.source_movement_seq = 1;
        let seed_output = InterfaceArtifactFile::write(cold.var_dir.join("revision-output"), "interface.artifact.output", br#"{"text":"prior output"}"#).unwrap();
        let make = |cold: &ProviderOutputHandoff, bytes: &[u8], file: &InterfaceArtifactFile| cold.author_run(bytes,
            &[input.artifact.id.clone()], &[PreparedHandoff { identity:artifact_identity(file, &cold.var_dir).unwrap(),
                output_checkpoint:cold.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),verified_inputs:vec![] }],
            true,None,false,true).unwrap();
        let edit = |authored: &AuthoredRun| CheckedRunEdit {path:cold.run_path.clone(),json_format:authored.json_format,
            before_hash:authored.before_hash.clone(),after_hash:authored.after_hash.clone(),content:authored.content.clone(),
            patch:authored.patch.clone(),correlation_id:authored.correlation_id.clone()};
        let prior = make(&cold, &seed, &seed_output);
        checked_call(&cold, &seed, &edit(&prior), &cold.hand_in_root);
        install_edit(&cold, &mut proof, &edit(&prior));
        edge(&cold, &mut proof, prior.content.as_bytes(), &prior.pending_handoff.items[0].checkpoint.id,
            &seed_output, cold.config.worker.clone(), MovementKind::InterfaceArrival);
        let before = std::fs::read(&cold.run_path).unwrap();
        for invalid in ["effect", "generation"] {
            let mut probe = cold.clone();
            probe.hand_in_root = cold.hand_in_root.join(format!("settlement-{invalid}"));
            probe.session = ProviderHandoffSession::new(&probe.ctx).unwrap();
            let label = if invalid == "effect" { "tool.call" } else { "interface.artifact.output" };
            let receipt = InterfaceArtifactFile::write(probe.var_dir.join("settlement-negative"), label, br#"{"text":"settlement"}"#).unwrap();
            let identity = artifact_identity(&receipt, &probe.var_dir).unwrap();
            let record = RunRecord::from_bytes(&before).unwrap();
            let mut context = current_context_artifacts(&record, &probe.config).unwrap();
            context.retain(|id| id != &prior.pending_handoff.items[0].artifact_id);
            append_artifact(&mut context, identity.artifact.id.clone());
            let role = if invalid == "effect" { RunAuthoringCheckpointRole::ProviderToolOutput } else { RunAuthoringCheckpointRole::ProviderArtifactOutput };
            let mut forged = probe.author_run(&before, &context, &[PreparedHandoff { identity,
                output_checkpoint:probe.config.checkpoint_for_role(role).unwrap().clone(),verified_inputs:vec![] }],true,None,false,true).unwrap();
            forged.pending_handoff.settlement_of = Some(pin_artifact(&seed_output, &probe.ctx.blobs_dir));
            let mut value:serde_json::Value = serde_json::from_str(&forged.content).unwrap();
            value["interface_state"][probe.ctx.interface.as_str()]["pending_handoff"] = serde_json::to_value(&forged.pending_handoff).unwrap();
            if invalid == "generation" { value["interface_state"][probe.ctx.interface.as_str()]["context_generation"] = serde_json::json!(999); }
            forged.content = String::from_utf8(JsonFormat::Compact.encode(&value).unwrap()).unwrap();
            forged.after_hash = ContentHash::of_bytes(forged.content.as_bytes()).to_hex();
            forged.patch = json_patch::diff(&serde_json::from_slice(&before).unwrap(), &value);
            let mut forged_edit = edit(&forged);
            if detached { forged_edit.correlation_id = format!("provider-writer-run:{run}:2:2"); }
            checked_call(&probe, &before, &forged_edit, &probe.hand_in_root);
            let ledger = std::fs::read(&probe.ctx.movements_path).unwrap();
            assert!(probe.recover_authoring_attempt(&record, &before).is_err(), "{invalid} detached={detached}");
            assert_eq!(std::fs::read(&probe.run_path).unwrap(), before);
            assert_eq!(std::fs::read(&probe.ctx.movements_path).unwrap(), ledger);
        }
        cold.source_movement_seq = proof.len() as u64;
        edge(&cold, &mut proof, &before, &stable(&before), &queued, cold.ctx.interface.clone(), MovementKind::InterfaceOutbound);
        cold.session = ProviderHandoffSession::new(&cold.ctx).unwrap();
        let answer = InterfaceArtifactFile::write(cold.var_dir.join("revision-output"), "interface.artifact.output",
            &serde_json::to_vec(&serde_json::json!({"text":"retained native answer","detached":detached})).unwrap()).unwrap();
        let saved = make(&cold, &before, &answer);
        let mut saved_edit = edit(&saved);
        if detached { saved_edit.correlation_id = format!("provider-writer-run:{run}:2:2"); }
        let carrier = checked_call(&cold, &before, &saved_edit, &cold.hand_in_root);
        let record = RunRecord::from_bytes(&before).unwrap();
        assert!(cold.settled_input_definition(&record, &before).unwrap(), "the exact completed write is a settled definition, detached={detached}");
        assert!(matches!(cold.recover_authoring_attempt(&record, &before).unwrap(), AuthoringAttemptRecovery::InFlight(_)));
        let writer = declared_provider_run_writer(&record, &cold.ctx.interface, &run_authoring_profile(descriptor).unwrap()).unwrap();
        let desired = cold.config.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::OwnerInput).map(|cp| {
            let mut cp = cp.clone(); if cp.name == "future_owner_result" { cp.include_context = true; }
            serde_json::to_value(cp).unwrap()
        }).collect();
        let ProviderInputRevision::Prepared {edit:revision} = prepare_provider_input_revision(cold.run_path.clone(), &before, &writer, desired).unwrap() else { panic!("revision deferred"); };
        install_edit(&cold, &mut proof, &revision);
        let after = std::fs::read(&cold.run_path).unwrap();
        let record = RunRecord::from_bytes(&after).unwrap();
        cold.config = attempt_config(&record, &cold.config).unwrap();
        cold.session = ProviderHandoffSession::new(&cold.ctx).unwrap();
        assert_eq!(revision_attempt_generations(&cold.ctx, &cold.session, &record, &after, &cold.config).unwrap(), BTreeMap::from([(2, revision.after_hash.clone())]));
        let recover = || match cold.recover_authoring_attempt(&record, &after).unwrap() {
            AuthoringAttemptRecovery::InFlight(attempt) => attempt, _ => panic!("lost saved native output"),
        };
        let recovered = recover();
        assert_eq!(recovered.manifest_path.canonicalize().unwrap(), carrier.manifest_path().canonicalize().unwrap());
        assert_eq!(recovered.provider_source_movement_seq, cold.source_movement_seq);
        assert_eq!(recovered.authored.pending_handoff, saved.pending_handoff);
        assert_eq!(recovered.authored.content, saved.content);
        assert_eq!(recover().authored, recovered.authored);
        let mut other_source = cold.clone(); other_source.source_movement_seq += 1;
        assert!(matches!(other_source.recover_authoring_attempt(&record, &after).unwrap(),
            AuthoringAttemptRecovery::OtherSource(source) if source == cold.source_movement_seq));
        let journal = std::fs::read(&cold.ctx.movements_path).unwrap();
        assert_eq!(std::fs::read(&cold.run_path).unwrap(), after);
        assert!(!proof.iter().any(|row| row.artifact.as_ref().is_some_and(|artifact| artifact.id == saved.pending_handoff.items[0].artifact_id)));
        let last = proof.pop().unwrap();
        std::fs::write(&cold.ctx.movements_path, proof.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
        assert!(cold.recover_authoring_attempt(&record, &after).is_err(), "accepted a revision without its exact result");
        proof.push(last);
        std::fs::write(&cold.ctx.movements_path, journal).unwrap();
    }
    for detached in [false, true] {
        let mut attached = h.clone();
        attached.ctx.movements_path = h.var_dir.join(format!("output-static-{detached}.jsonl"));
        attached.ctx.ledger_index_path = h.var_dir.join(format!("output-static-{detached}.sqlite"));
        let mut seed = output_initial.clone();
        let role = pin_artifact(&InterfaceArtifactFile::write(h.var_dir.join("writer-role"), "context.reference", b"mandatory role").unwrap(), &h.ctx.blobs_dir);
        for checkpoint in seed["checkpoints"].as_array_mut().unwrap() {
            checkpoint["artifact_bindings"].as_array_mut().unwrap().push(serde_json::json!({"id":role.id}));
        }
        if detached {
            let writer_id = RunId::new("run_output_static_writer");
            let writer = separate_writer_run(&mut seed, &attached.ctx.interface, &writer_id).unwrap();
            std::fs::write(attached.run_path.parent().unwrap().join(format!("{writer_id}.json")), JsonFormat::Compact.encode(&writer).unwrap()).unwrap();
        }
        let before = JsonFormat::Compact.encode(&seed).unwrap();
        std::fs::write(&attached.run_path, &before).unwrap();
        let record = RunRecord::from_bytes(&before).unwrap();
        attached.config = attempt_config(&record, &h.config).unwrap();
        attached.bound_topology = test_bound_topology(&attached.config);
        let writer = declared_provider_run_writer(&record, &attached.ctx.interface, &profile).unwrap();
        let mut proof = vec![ledger_row(0)];
        attached.source_movement_seq = 1;
        edge(&attached, &mut proof, &before, &CheckpointId::new("stable_input"), &input.artifact_file, attached.ctx.interface.clone(), MovementKind::InterfaceOutbound);
        let scoped = pin_artifact(&InterfaceArtifactFile::write(h.var_dir.join("output-static"), "context.reference", b"one output's static content").unwrap(), &h.ctx.blobs_dir);
        let outgoing = ProviderOutgoingBinding { interface:"system_new_service_input".into(), artifact_label:"provider.peer.message".into(), artifact_bindings:vec![scoped.id.clone()] };
        let output = InterfaceArtifactFile::write(h.var_dir.join("connection-output"), "tool.call", br#"{"tool":"artifact.send","arguments":{},"correlation_id":"saved-seed"}"#).unwrap();
        let connected = attached.author_run_with_bindings(&before, &[input.artifact.id.clone()], &[PreparedHandoff {
            identity:artifact_identity(&output, &h.var_dir).unwrap(),
            output_checkpoint:attached.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderToolOutput).unwrap().clone(), verified_inputs:vec![],
        }], true, None, false, true, &[outgoing]).unwrap();
        let edit = CheckedRunEdit { path:attached.run_path.clone(), json_format:connected.json_format,
            before_hash:connected.before_hash.clone(), after_hash:connected.after_hash.clone(), content:connected.content.clone(),
            patch:connected.patch.clone(), correlation_id:connected.correlation_id.clone() };
        install_edit(&attached, &mut proof, &edit);
        let limits = attached.ctx.required_host_io_limits().unwrap();
        let evidence = CheckedRunEvidence { movements_path:&attached.ctx.movements_path, ledger_index_path:&attached.ctx.ledger_index_path,
            blobs_dir:&attached.ctx.blobs_dir, max_control_frame_bytes:limits.max_control_frame_bytes,
            max_manifest_bytes:limits.max_manifest_bytes, max_payload_bytes:limits.max_payload_bytes };
        let chain = checked_run_file_chain(&evidence, &attached.run_path, &before, connected.content.as_bytes(), proof[proof.len()-2].seq.0-1, &[writer]).unwrap();
        assert_eq!(chain.len(),1); validate_checked_provider_transition(&chain[0]).unwrap();
        let next = RunRecord::from_bytes(&chain[0].after).unwrap();
        let config = attempt_config(&next,&attached.config).unwrap();
        let expected_roles = if detached { vec![role.id.clone()] } else { vec![role.id.clone(),scoped.id.clone()] };
        assert_eq!(current_mandatory_role_artifacts(&next,&config).unwrap(),expected_roles);
        let template = config.checkpoints.iter().find(|cp|cp.role==RunAuthoringCheckpointRole::ProviderInterfaceOutput).unwrap();
        let checkpoint = next.checkpoints.iter().find(|cp|checkpoint_record_matches_template(cp,template,&next.id)).unwrap();
        assert_eq!(checkpoint.artifact_bindings.iter().map(|binding|binding.id.clone()).collect::<Vec<_>>(),vec![role.id.clone(),scoped.id.clone()]);
        for mutation in ["missing_static","widened_static","changed_roles"] {
            let mut value:serde_json::Value=serde_json::from_slice(&chain[0].after).unwrap();
            for cp in value["checkpoints"].as_array_mut().unwrap() {
                let outgoing = cp["id"]==checkpoint.id.as_str();
                if mutation=="missing_static" && outgoing { cp["artifact_bindings"].as_array_mut().unwrap().retain(|b|b["id"]!=scoped.id.as_str()); }
                for binding in cp["artifact_bindings"].as_array_mut().unwrap() {
                    if (mutation=="widened_static" && outgoing && binding["id"]==scoped.id.as_str())
                        || (mutation=="changed_roles" && binding["id"]==role.id.as_str()) { binding["access"]=serde_json::json!({"read":true,"change":true,"create":false,"delete":false}); }
                }
            }
            let mut invalid=chain[0].clone();invalid.after=serde_json::to_vec(&value).unwrap();
            assert!(validate_checked_provider_transition(&invalid).is_err(),"{mutation}, detached={detached}");
        }
    }
    // Payload selection changes future grants; unchanged owner rows keep their pinned bytes.
    let mut payload = h.clone();
    payload.ctx.movements_path = h.var_dir.join("payload-cutover.jsonl");
    payload.ctx.ledger_index_path = h.var_dir.join("payload-cutover.sqlite");
    let a = pin_artifact(&InterfaceArtifactFile::write(h.var_dir.join("payload-a"), "selected.file", b"old binary file").unwrap(), &h.ctx.blobs_dir);
    let b = pin_artifact(&InterfaceArtifactFile::write(h.var_dir.join("payload-b"), "selected.file", b"new binary file").unwrap(), &h.ctx.blobs_dir);
    let mut seed: serde_json::Value = serde_json::from_slice(&initial).unwrap();
    seed["status"] = serde_json::json!("idle");
    let mut topology: BoundRunAuthoringTopology = serde_json::from_value(seed["interface_state"][h.ctx.interface.as_str()][RUN_AUTHORING_BINDING].clone()).unwrap();
    topology.payload_artifact_bindings = vec![a.clone()];
    for template in topology.checkpoints.iter_mut().filter(|cp| cp.role == RunAuthoringCheckpointRole::ProviderToolOutput) {
        for tool in checkpoint_tool_ids(template).unwrap() {
            template.result_receiving.insert(tool.clone(), serde_json::from_value(serde_json::json!({
                "artifact_label":format!("{tool}.result"), "mark":"tool.result.corresponds",
                "verifier":"tool_result_correspondence", "params":{}
            })).unwrap());
        }
    }
    topology.checkpoints.iter_mut().find(|cp| cp.name == "owner_result").unwrap().static_artifact_bindings.push(a.id.clone());
    seed["interface_state"][h.ctx.interface.as_str()][RUN_AUTHORING_BINDING] = serde_json::to_value(topology).unwrap();
    for cp in seed["checkpoints"].as_array_mut().unwrap() {
        if cp["id"] == "stable_input" || cp["id"] == "owner_result" { cp["artifact_bindings"] = serde_json::json!([{"id":a.id}]); }
    }
    let seed = JsonFormat::Compact.encode(&seed).unwrap();
    std::fs::write(&payload.run_path, &seed).unwrap();
    payload.config = attempt_config(&RunRecord::from_bytes(&seed).unwrap(), &h.config).unwrap();
    payload.bound_topology = test_bound_topology(&payload.config);
    let source = payload.config.checkpoint_for_role(RunAuthoringCheckpointRole::Stable).unwrap().interface().clone();
    let stable_template = payload.config.checkpoint_for_role(RunAuthoringCheckpointRole::Stable).unwrap().clone();
    let stable_row = |record: &RunRecord| record.checkpoints.iter().find(|cp| record.active_checkpoints.contains(&cp.id)
        && checkpoint_record_matches_template(cp, &stable_template, &run)).unwrap().clone();
    let writer = declared_provider_run_writer(&RunRecord::from_bytes(&seed).unwrap(), &payload.ctx.interface, &run_authoring_profile(descriptor).unwrap()).unwrap();
    let evidence = CheckedRunEvidence {movements_path:&payload.ctx.movements_path,ledger_index_path:&payload.ctx.ledger_index_path,
        blobs_dir:&payload.ctx.blobs_dir,max_control_frame_bytes:1_048_576,max_manifest_bytes:1_048_576,max_payload_bytes:268_435_456};
    let ready = |bytes: &[u8]| provider_input_payloads_ready(&evidence, &payload.run_path, bytes, &writer, &source).unwrap();
    let mut proof = vec![]; append(&payload, &mut proof, ledger_row(0));
    assert!(ready(&seed));
    let first = prepare_provider_input_payloads(payload.run_path.clone(), &seed, &writer, source.clone(), vec![a.clone(),b.clone()]).unwrap();
    install_edit(&payload, &mut proof, &first);
    let first_record = RunRecord::from_bytes(first.content.as_bytes()).unwrap();
    payload.config = attempt_config(&first_record, &payload.config).unwrap();
    payload.bound_topology = test_bound_topology(&payload.config);
    assert_eq!(first_record.interface_state[&payload.ctx.interface]["authoring_generation"], serde_json::json!(1));
    assert!(persisted_pending_handoff(&first_record, &payload.ctx.interface).unwrap().is_none());
    assert!(ready(first.content.as_bytes()), "an authored but unspent initial input remains ready");
    let first_chain = checked_run_file_chain(&evidence, &payload.run_path, &seed, first.content.as_bytes(), 0, &[writer.clone()]).unwrap();
    assert_eq!(first_chain.len(), 1); validate_checked_provider_transition(&first_chain[0]).unwrap();
    payload.source_movement_seq = proof.len() as u64;
    let stable = stable_row(&first_record);
    edge(&payload, &mut proof, first.content.as_bytes(), &stable.id, &input.artifact_file, payload.ctx.interface.clone(), MovementKind::InterfaceOutbound);
    assert!(!ready(first.content.as_bytes()), "admitted input cannot be overtaken by payload selection");
    let answer = InterfaceArtifactFile::write(h.var_dir.join("payload-answer"), "interface.artifact.output", br#"{"text":"settled payload source"}"#).unwrap();
    let authored = payload.author_run(first.content.as_bytes(), &[a.id.clone(),b.id.clone()], &[PreparedHandoff {
        identity:artifact_identity(&answer, &payload.var_dir).unwrap(), output_checkpoint:payload.config.checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput).unwrap().clone(),verified_inputs:vec![],
    }], true,None,false,true).unwrap();
    let output_edit = CheckedRunEdit {path:payload.run_path.clone(),json_format:authored.json_format,before_hash:authored.before_hash.clone(),
        after_hash:authored.after_hash.clone(),content:authored.content.clone(),patch:authored.patch.clone(),correlation_id:authored.correlation_id.clone()};
    install_edit(&payload, &mut proof, &output_edit);
    assert!(!ready(authored.content.as_bytes()), "terminal flag without actual admission is not readiness");
    edge(&payload, &mut proof, authored.content.as_bytes(), &authored.pending_handoff.items[0].checkpoint.id,
        &answer, payload.config.worker.clone(), MovementKind::InterfaceArrival);
    assert!(ready(authored.content.as_bytes()));
    let before = RunRecord::from_bytes(authored.content.as_bytes()).unwrap();
    let next = prepare_provider_input_payloads(payload.run_path.clone(), authored.content.as_bytes(), &writer, source.clone(), vec![b.clone()]).unwrap();
    let after = RunRecord::from_bytes(next.content.as_bytes()).unwrap();
    let after_topology: BoundRunAuthoringTopology = serde_json::from_value(after.interface_state[&payload.ctx.interface][RUN_AUTHORING_BINDING].clone()).unwrap();
    for template in after_topology.checkpoints.iter().filter(|cp| cp.role == RunAuthoringCheckpointRole::ProviderToolOutput) {
        let retained = after.checkpoints.iter().find(|cp| after.active_checkpoints.contains(&cp.id)
            && checkpoint_record_matches_template(cp, template, &after.id)).unwrap();
        assert!(retained.artifact_bindings.iter().any(|bound| bound.id == b.id));
        assert!(!retained.artifact_bindings.iter().any(|bound| bound.id == a.id));
    }
    let owner = CheckpointId::new("owner_result");
    assert_eq!(after.checkpoint(&owner), before.checkpoint(&owner));
    assert!(after.checkpoint(&owner).unwrap().artifact_bindings.iter().any(|bound| bound.id == a.id));
    assert_eq!(payload_artifact_bindings(&after, &payload.ctx.interface).unwrap(), vec![b.clone(),a.clone()]);
    assert_eq!(provider_context_bindings(&after, &payload.ctx.interface).unwrap().iter().map(|bound| bound.id.clone()).collect::<Vec<_>>(), vec![b.id.clone()]);
    assert_eq!(persisted_pending_handoff(&after, &payload.ctx.interface).unwrap(), Some(authored.pending_handoff.clone()));
    let stable_before = stable_row(&before);
    let stable_after = stable_row(&after);
    assert_ne!(stable_before.id, stable_after.id);
    assert_eq!(after.checkpoints.iter().filter(|cp| after.active_checkpoints.contains(&cp.id)
        && checkpoint_record_matches_template(cp, payload.config.checkpoint_for_role(RunAuthoringCheckpointRole::Stable).unwrap(), &run)).count(), 1);
    let after_seq = proof.last().unwrap().seq.0;
    install_edit(&payload, &mut proof, &next);
    let chain = checked_run_file_chain(&evidence, &payload.run_path, authored.content.as_bytes(), next.content.as_bytes(), after_seq, &[writer.clone()]).unwrap();
    assert_eq!(chain.len(), 1); validate_checked_provider_transition(&chain[0]).unwrap();
    assert!(ready(next.content.as_bytes()));
    let mut hot = payload.clone(); hot.session = ProviderHandoffSession::new(&hot.ctx).unwrap();
    let mut hot_state = hot.state.lock().await.clone(); hot_state.run_record_bytes = authored.content.as_bytes().to_vec();
    hot_state.run_record_evidence_seq = Some(after_seq); hot_state.session_context.artifacts.clear();
    hot_state.static_context_artifacts = vec![a.id.clone(),input.artifact.id.clone()];
    hot_state.context_artifacts = hot_state.static_context_artifacts.clone();
    hot.reconcile_checked_run_state(&mut hot_state).await.unwrap();
    assert_eq!(hot_state.static_context_artifacts, vec![input.artifact.id.clone(),b.id.clone()]);
    assert_eq!(hot_state.context_artifacts, hot_state.static_context_artifacts, "snapshot seed preserves current payload and non-payload static context");
    let mut stale = payload.clone(); stale.session = ProviderHandoffSession::new(&stale.ctx).unwrap();
    let mut stale_state = stale.state.lock().await.clone(); stale_state.run_record_bytes = authored.content.as_bytes().to_vec();
    stale_state.run_record_evidence_seq = Some(after_seq); stale_state.session_context.artifacts.clear();
    stale_state.static_context_artifacts = vec![a.id.clone(),input.artifact.id.clone()];
    stale_state.context_artifacts = stale_state.static_context_artifacts.clone();
    stale.state = Arc::new(tokio::sync::Mutex::new(stale_state.clone()));
    let limit = stale.ctx.required_host_io_limits().unwrap().max_payload_bytes;
    let observe = |handoff: &ProviderOutputHandoff, state: &mut HandoffState| stable_run_observation(
        &handoff.run_path, &state.run_record_bytes, limit, || Ok(state.run_record_bytes.clone())).map_err(String::from);
    assert_eq!(stale.observe_live_record(observe).await.unwrap(), next.content.as_bytes());
    assert_eq!(stale.state.lock().await.static_context_artifacts, hot_state.static_context_artifacts);
    std::fs::write(&payload.run_path, [next.content.as_bytes(), b" "].concat()).unwrap();
    stale.state = Arc::new(tokio::sync::Mutex::new(stale_state));
    assert!(stale.observe_live_record(observe).await.unwrap_err().contains("run changed during evidence collection"));
    assert_eq!(stale.state.lock().await.run_record_bytes, authored.content.as_bytes());
    std::fs::write(&payload.run_path, next.content.as_bytes()).unwrap();
    let mut changed = chain[0].clone(); changed.writer.tool = "file.write.foreign".into();
    assert!(validate_checked_provider_transition(&changed).is_err());
    for field in ["source_movement_seq", "terminal"] {
        let mut changed = chain[0].clone(); let mut value: serde_json::Value = serde_json::from_slice(&changed.after).unwrap();
        value["interface_state"][payload.ctx.interface.as_str()]["pending_handoff"][field] = if field == "terminal" { serde_json::json!(false) } else { serde_json::json!(0) };
        changed.after = serde_json::to_vec(&value).unwrap(); assert!(validate_checked_provider_transition(&changed).is_err());
    }
    let admitted = proof.iter().position(|row| row.kind == MovementKind::InterfaceArrival).unwrap();
    let saved = proof[admitted].clone(); proof[admitted] = ledger_row(saved.seq.0);
    std::fs::write(&payload.ctx.movements_path, proof.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    assert!(!ready(next.content.as_bytes()));
    assert!(checked_run_file_chain(&evidence, &payload.run_path, authored.content.as_bytes(), next.content.as_bytes(), after_seq, &[writer]).is_err());
    proof[admitted] = saved;
    std::fs::write(&payload.ctx.movements_path, proof.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    std::fs::write(&h.run_path, &current).unwrap();
    let closure_row = rows.iter_mut().find(|row| row.artifact.as_ref() == closure.as_ref()).unwrap();
    *closure_row = ledger_row(closure_row.seq.0);
    std::fs::write(&h.ctx.movements_path, rows.iter().map(|row| format!("{}\n", serde_json::to_string(row).unwrap())).collect::<String>()).unwrap();
    h.session = ProviderHandoffSession::new(&h.ctx).unwrap();
    let error = h.recover_source().await.unwrap_err();
    assert!(error.contains("admitted ledger evidence"), "{error}");
}
