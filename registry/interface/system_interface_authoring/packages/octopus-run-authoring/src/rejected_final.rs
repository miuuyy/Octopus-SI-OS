use super::*;
use octopus_ledger::{MovementMetadata, RefusedReason};
use octopus_ledger_index::{ArrivalDedupIndex, RetainedArrivalState};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RowProof { seq: u64, content: ContentHash }

impl RowProof {
    fn of(row: &Movement) -> Result<Self, String> {
        Ok(Self { seq: row.seq.0, content: ContentHash::of_bytes(&serde_json::to_vec(row).map_err(|e| e.to_string())?) })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rejection {
    predecessor: PendingHandoff,
    predecessor_record: String,
    source: ArtifactRef,
    scope: RowProof,
    refusals: Vec<RowProof>,
}

#[derive(Clone)]
pub struct RejectedFinalOutput { proof: Rejection }

pub(super) fn follows(before: &PendingHandoff, after: &PendingHandoff) -> bool {
    after.replacement.as_ref().is_some_and(|proof| proof.predecessor == *before)
        && before.source_movement_seq == after.source_movement_seq && before.items != after.items
}

fn output(pending: &PendingHandoff) -> Result<&PendingHandoffItem, String> {
    let [item] = pending.items.as_slice() else { return Err("rejected final replacement requires one exact output".into()); };
    if !pending.terminal || !pending.dispatch_fenced || pending.context_publication.is_some()
        || pending.deferred_publication || pending.source_without_context || pending.settlement_of.is_some()
        || pending.withdrawal.is_some() || !pending.standing.is_empty()
    { return Err("rejected final replacement cannot consume context, effects or a partial batch".into()); }
    Ok(item)
}

fn reference(item: &PendingHandoffItem) -> Result<ArtifactRef, String> {
    Ok(ArtifactRef { id: item.artifact_id.clone(), content: ContentHash::from_hex(&item.artifact_content_hash)
        .ok_or("rejected final has an invalid artifact hash")? })
}

fn rejection(
    reader: &mut impl CheckedRunEvidenceReader, record: &RunRecord, provider: &InterfaceId,
    pending: &PendingHandoff, upper: u64,
) -> Result<Option<(RowProof, Vec<RowProof>)>, String> {
    let item = output(pending)?;
    let artifact = reference(item)?;
    let identity = pending_delivery::identity_for(record, provider, pending)?;
    let mut index = ArrivalDedupIndex::new();
    let mut rows = Vec::new();
    reader.visit_admitted_prefix(upper, &mut |row| {
        index.catch_up_movements(std::iter::once(row.clone())).map_err(|e| e.to_string())?;
        if row.seq.0 > pending.source_movement_seq && row.artifact.as_ref() == Some(&artifact) { rows.push(row.clone()); }
        Ok(())
    })?;
    let Some(scope) = index.retained_arrival_scope(provider, &identity).map_err(|e| e.to_string())? else { return Ok(None); };
    let edges = index.retained_arrival_edges(scope.seq.0).map_err(|e| e.to_string())?;
    let intended = |runtime: &octopus_ledger::RuntimeInvocationMetadata| runtime.run == record.id && runtime.ee == record.ee
        && runtime.worker == record.worker && runtime.interface == *provider
        && runtime.checkpoint == item.checkpoint.id && runtime.epoch == item.checkpoint.epoch;
    let targets = edges.iter().filter(|(runtime, _)| intended(runtime)).collect::<Vec<_>>();
    let [(_, target)] = targets.as_slice() else { return Err("rejected final scope lost its intended output".into()); };
    if !matches!(target, RetainedArrivalState::Finished(DeliveryVerdict::Refused { reason: RefusedReason::VerificationFailed, .. })) { return Ok(None); }
    if scope.artifact.as_ref() != Some(&artifact) || scope.seq.0 <= pending.source_movement_seq || edges.is_empty()
        || edges.iter().any(|(_, state)| !matches!(state, RetainedArrivalState::Finished(DeliveryVerdict::Refused { .. })))
    { return Err("rejected final has admitted, pending or unresolved original edges".into()); }
    let mut failed_arrivals = Vec::new();
    let mut refusals = Vec::new();
    let mut coordinates = HashSet::new();
    let mut intended_failure = false;
    for row in &rows {
        if row.seq == scope.seq { continue; }
        if row.from != Endpoint::Interface(provider.clone()) { return Err("rejected final has a delivered effect outside its scope".into()); }
        let (runtime, sequence) = match row.invocation_metadata.as_ref() {
            Some(MovementMetadata::Invocation(m)) => (m.runtime.as_ref(), m.delivery_scope_seq),
            Some(MovementMetadata::Delivery(m)) => (m.runtime.as_ref(), m.delivery_scope_seq),
            _ => return Err("rejected final has ambiguous artifact movement evidence".into()),
        };
        let runtime = runtime.ok_or("rejected final edge has no runtime coordinate")?;
        if sequence != Some(scope.seq.0) || !edges.iter().any(|(original, _)| original.same_checkpoint_coordinates(runtime)) {
            return Err("rejected final has another scope or changed original edge".into());
        }
        let terminal = match (&row.kind, row.invocation_metadata.as_ref()) {
            (MovementKind::InterfaceArrival, Some(MovementMetadata::Invocation(m))) if m.verification_failure.is_some() => {
                failed_arrivals.push(row); false
            }
            (MovementKind::VerificationFailed, Some(MovementMetadata::Invocation(m))) => {
                let matches = failed_arrivals.iter().filter(|arrival| arrival.seq < row.seq && arrival.from == row.from
                    && arrival.to == row.to && arrival.trace == row.trace && arrival.invocation_metadata == row.invocation_metadata).count();
                if matches != 1 { return Err("rejected final verification refusal has no unique exact arrival".into()); }
                let failure = m.verification_failure.as_ref().ok_or("rejected final lacks typed verifier failure")?;
                if intended(runtime) {
                    if !item.checkpoint.required_verified_artifacts.iter().any(|required| required.artifact_label == item.artifact_label
                        && failure.verifier.as_ref() == Some(&required.verifier) && failure.mark.as_ref() == Some(&required.mark))
                        || failure.run.as_ref().is_some_and(|run| run != &record.id)
                        || failure.checkpoint.as_ref().is_some_and(|cp| cp != &item.checkpoint.id)
                        || failure.artifact_label.as_ref().is_some_and(|label| label != &item.artifact_label)
                    { return Err("rejected final verifier failure differs from its intended contract".into()); }
                    intended_failure = true;
                }
                true
            }
            (MovementKind::DeliveryRefused, Some(MovementMetadata::Delivery(m))) => !m.delivery_verdict.awaits_current_source(),
            _ => return Err("rejected final has admitted or pending effect evidence".into()),
        };
        if terminal {
            if !coordinates.insert(RuntimeCoordinateKey::from_runtime(runtime)) { return Err("rejected final has repeated refusal evidence".into()); }
            refusals.push(RowProof::of(row)?);
        }
    }
    if !intended_failure || coordinates.len() != edges.len() { return Err("rejected final lacks complete typed refusal evidence".into()); }
    Ok(Some((RowProof::of(&scope)?, refusals)))
}

pub(super) fn validate_transition(before: &RunRecord, after: &RunRecord, config: &RunAuthoringConfig) -> Result<(), String> {
    let provider = &config.provider_interface;
    let Some(next) = persisted_pending_handoff(after, provider)? else { return Ok(()); };
    let Some(proof) = next.replacement.as_ref() else { return Ok(()); };
    let prior = persisted_pending_handoff(before, provider)?.ok_or("replacement lost its predecessor")?;
    if prior == next { return Ok(()); }
    if !follows(&prior, &next) { return Err("replacement changed its exact predecessor".into()); }
    let old = output(&prior)?; let new = output(&next)?;
    if reference(old)? == reference(new)? || old.artifact_label != new.artifact_label {
        return Err("rejected final replacement must change bytes and retain its declared label".into());
    }
    let mut expected_item = old.checkpoint.clone(); expected_item.id = new.checkpoint.id.clone();
    if expected_item != new.checkpoint || !config.checkpoints.iter().any(|template|
        template.role == RunAuthoringCheckpointRole::ProviderArtifactOutput && template.result_receiving.is_empty()
        && pending_output_matches_template(old, template)) {
        return Err("replacement changed its output contract or can dispatch a tool effect".into());
    }
    let generation = optional_u64(&before.interface_state[provider], "authoring_generation")?.checked_add(1).ok_or("replacement generation overflow")?;
    let prefix = checkpoint_prefix(&before.id, &new.artifact_id, generation);
    let previous_prefix = checkpoint_prefix(&before.id, &old.artifact_id, generation - 1);
    raw_frontier_checkpoint_ids(&before.interface_state[provider])?;
    raw_frontier_checkpoint_ids(&after.interface_state[provider])?;
    let old_frontier: Vec<String> = serde_json::from_value(before.interface_state[provider][FRONTIER_CHECKPOINT_IDS].clone()).map_err(|e| e.to_string())?;
    let new_frontier: Vec<String> = serde_json::from_value(after.interface_state[provider][FRONTIER_CHECKPOINT_IDS].clone()).map_err(|e| e.to_string())?;
    if old_frontier.len() != new_frontier.len() { return Err("replacement changed its frontier contracts".into()); }
    let mut checkpoints = Vec::new();
    for (old_id, new_id) in old_frontier.iter().zip(&new_frontier) {
        let previous = before.checkpoint(&CheckpointId::new(old_id)).ok_or("replacement predecessor checkpoint missing")?;
        let next = after.checkpoint(&CheckpointId::new(new_id)).ok_or("replacement successor checkpoint missing")?;
        let mut expected = previous.clone(); expected.id = next.id.clone();
        expected.trace = octopus_ledger::TraceId(format!("trace:{}:{}", before.id, next.id));
        if expected != *next || old_id.strip_prefix(&previous_prefix).map(|suffix| format!("{prefix}{suffix}")).as_ref() != Some(new_id) {
            let changes = json_patch::diff(&serde_json::to_value(&expected).map_err(|e| e.to_string())?, &serde_json::to_value(next).map_err(|e| e.to_string())?);
            return Err(format!("replacement changed frontier '{old_id}' -> '{new_id}': {}", changes.iter().map(|op|op.path().to_string()).collect::<Vec<_>>().join(",")));
        }
        checkpoints.push(serde_json::to_value(next).map_err(|e| e.to_string())?);
    }
    let mut expected = serde_json::to_value(before).map_err(|e| e.to_string())?;
    replace_owned_frontier(expected.as_object_mut().unwrap(), &old_frontier.iter().cloned().collect(), checkpoints, &new_frontier)?;
    let state = &mut expected["interface_state"][provider.as_str()];
    state["authoring_generation"] = serde_json::json!(generation);
    state["last_authored_artifact"] = serde_json::json!(new.artifact_id);
    state["pending_handoff"] = serde_json::to_value(&next).map_err(|e| e.to_string())?;
    state[FRONTIER_CHECKPOINT_IDS] = serde_json::json!(new_frontier);
    if expected != serde_json::to_value(after).map_err(|e| e.to_string())? || proof.predecessor != prior {
        return Err("replacement changed run identity, context, grants or neighboring state".into());
    }
    Ok(())
}

fn validate_proof(reader: &mut impl CheckedRunEvidenceReader, record: &RunRecord, provider: &InterfaceId, proof: &Rejection, upper: u64) -> Result<(), String> {
    let hash = ContentHash::from_hex(&proof.predecessor_record).ok_or("replacement preimage hash is invalid")?;
    let bytes = reader.snapshot(&hash)?;
    if ContentHash::of_bytes(&bytes) != hash { return Err("replacement preimage failed integrity".into()); }
    let predecessor = RunRecord::from_bytes(&bytes).map_err(|e| e.to_string())?;
    if predecessor.id != record.id || predecessor.ee != record.ee || predecessor.worker != record.worker
        || persisted_pending_handoff(&predecessor, provider)?.as_ref() != Some(&proof.predecessor) {
        return Err("replacement proof changed its original run or pending output".into());
    }
    source_withdrawal::validate_source(reader, &predecessor, provider, proof.predecessor.source_movement_seq, &proof.source)?;
    let actual = rejection(reader, &predecessor, provider, &proof.predecessor, upper)?.ok_or("replacement predecessor is not fully rejected")?;
    if actual != (proof.scope.clone(), proof.refusals.clone()) { return Err("replacement refusal evidence changed".into()); }
    Ok(())
}

pub(super) fn validate_evidence(reader: &mut impl CheckedRunEvidenceReader, transition: &CheckedRunTransition) -> Result<(), String> {
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
    let Some(next) = persisted_pending_handoff(&after, &transition.writer.owner)? else { return Ok(()); };
    let Some(proof) = next.replacement.as_ref() else { return Ok(()); };
    if persisted_pending_handoff(&before, &transition.writer.owner)?.as_ref() == Some(&next) { return Ok(()); }
    let config = config_from_persisted_profile(&before, &transition.writer.owner, &RunAuthoringProfile {
        file_write_tool: transition.writer.tool.clone(), file_write_label: transition.writer.call_label.clone(),
        file_write_result_label: transition.writer.result_label.clone(), result_timeout_ms: 1,
    })?;
    validate_transition(&before, &after, &config)?;
    if proof.predecessor_record != ContentHash::of_bytes(&transition.before).to_hex()
        || proof.refusals.iter().any(|row| row.seq >= transition.call.seq.0) { return Err("replacement rejection must precede its exact checked write".into()); }
    validate_proof(reader, &before, &transition.writer.owner, proof, transition.result.seq.0 + 1)
}

pub(super) fn validate_saved_attempt(ctx: &InterfaceLiftContext, before: &[u8], after: &[u8], config: &RunAuthoringConfig) -> Result<(), String> {
    let record = RunRecord::from_bytes(before).map_err(|e| e.to_string())?;
    let candidate = RunRecord::from_bytes(after).map_err(|e| e.to_string())?;
    validate_transition(&record, &candidate, config)?;
    let Some(pending) = persisted_pending_handoff(&candidate, &config.provider_interface)? else { return Ok(()); };
    let Some(proof) = pending.replacement.as_ref() else { return Ok(()); };
    if persisted_pending_handoff(&record, &config.provider_interface)?.as_ref() == Some(&pending) { return Ok(()); }
    if proof.predecessor_record != ContentHash::of_bytes(before).to_hex() { return Err("replacement attempt changed its exact preimage".into()); }
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence { movements_path:&ctx.movements_path, ledger_index_path:&ctx.ledger_index_path, blobs_dir:&ctx.blobs_dir,
        max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes };
    let mut reader = FileCheckedRunReader::open(&evidence, None)?.with_provider_context(ctx);
    let upper = pending_delivery::Ledger::new(config.provider_interface.clone(), ctx.movements_path.clone(), limits.max_control_frame_bytes).watermark()?;
    validate_proof(&mut reader, &record, &config.provider_interface, proof, upper)
}

impl ProviderOutputHandoff {
    pub async fn recover_replacement_final(&self) -> Result<Option<ProviderHandoffOutcome>, String> {
        let saved = {
            let state = self.state.lock().await;
            let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|e|e.to_string())?;
            let pending = match self.recover_authoring_attempt(&record, &state.run_record_bytes)? {
                AuthoringAttemptRecovery::InFlight(attempt) => Some(attempt.authored.pending_handoff),
                AuthoringAttemptRecovery::None => persisted_pending_handoff(&record, &self.config.provider_interface)?,
                AuthoringAttemptRecovery::OtherSource(_) => None,
            };
            let Some(pending) = pending.filter(|p| p.source_movement_seq == self.source_movement_seq && p.replacement.is_some()) else { return Ok(None); };
            if let Some((_, upper)) = self.retained_output_identity(&pending)? {
                if self.with_rejection_reader(|reader| rejection(reader,&record,&self.config.provider_interface,&pending,upper))?.is_some() { return Ok(None); }
            }
            let (artifact, _) = load_pending_artifact(&self.var_dir, output(&pending)?)?;
            (RejectedFinalOutput {proof:*pending.replacement.unwrap()}, artifact)
        };
        self.hand_replacement_final(&saved.0, ProviderEmission {artifact:saved.1,route:ProviderEmissionRoute::ClientProjection}).await.map(Some)
    }

    fn with_rejection_reader<T>(&self, f: impl FnOnce(&mut FileCheckedRunReader<'_>) -> Result<T, String>) -> Result<T, String> {
        let limits = self.ctx.required_host_io_limits()?;
        let evidence = CheckedRunEvidence { movements_path:&self.ctx.movements_path, ledger_index_path:&self.ctx.ledger_index_path,
            blobs_dir:&self.ctx.blobs_dir, max_control_frame_bytes:limits.max_control_frame_bytes,
            max_manifest_bytes:limits.max_manifest_bytes, max_payload_bytes:limits.max_payload_bytes };
        f(&mut FileCheckedRunReader::open(&evidence, None)?.with_provider_context(&self.ctx))
    }

    pub async fn rejected_final_output(&self) -> Result<Option<RejectedFinalOutput>, String> {
        let state = self.state.lock().await;
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|e| e.to_string())?;
        if !matches!(self.recover_authoring_attempt(&record, &state.run_record_bytes)?, AuthoringAttemptRecovery::None) { return Ok(None); }
        let Some(pending) = persisted_pending_handoff(&record, &self.config.provider_interface)? else { return Ok(None); };
        if pending.source_movement_seq != self.source_movement_seq { return Ok(None); }
        let Some((_, upper)) = self.retained_output_identity(&pending)? else { return Ok(None); };
        self.with_rejection_reader(|reader| {
            let rejected = rejection(reader, &record, &self.config.provider_interface, &pending, upper)?;
            if let Some((scope, refusals)) = rejected {
                let source = reader.admitted_movement(pending.source_movement_seq)?.and_then(|row| row.artifact).ok_or("rejected final source is absent")?;
                let proof = Rejection {predecessor:pending, predecessor_record:ContentHash::of_bytes(&state.run_record_bytes).to_hex(), source, scope, refusals};
                validate_proof(reader, &record, &self.config.provider_interface, &proof, upper)?;
                Ok(Some(RejectedFinalOutput {proof}))
            } else { Ok(None) }
        })
    }

    pub async fn hand_replacement_final(&self, rejected: &RejectedFinalOutput, emission: ProviderEmission) -> Result<ProviderHandoffOutcome, String> {
        if !matches!(emission.route, ProviderEmissionRoute::ClientProjection) { return Err("rejected final replacement is restricted to the declared artifact output".into()); }
        let mut state = self.state.lock().await;
        let ownership = self.dispatch_ownership().await?;
        self.reconcile_checked_run_state(&mut state).await?;
        let record = RunRecord::from_bytes(&state.run_record_bytes).map_err(|e| e.to_string())?;
        let proof = &rejected.proof;
        if proof.predecessor.source_movement_seq != self.source_movement_seq { return Err("replacement changed its pinned source".into()); }
        let current = persisted_pending_handoff(&record, &self.config.provider_interface)?.ok_or("replacement has no pending output")?;
        let (_, upper) = self.retained_output_identity(&proof.predecessor)?.ok_or("replacement lost its retained dispatch identity")?;
        self.with_rejection_reader(|reader| validate_proof(reader, &record, &self.config.provider_interface, proof, upper))?;
        let identity = artifact_identity(&emission.artifact, &self.var_dir)?;
        if identity.artifact == reference(output(&proof.predecessor)?)? { return Err("rejected final replacement artifact is unchanged".into()); }
        let attempt = self.recover_authoring_attempt(&record, &state.run_record_bytes)?;
        let authored = match attempt {
            AuthoringAttemptRecovery::InFlight(attempt) => Some(attempt.authored),
            AuthoringAttemptRecovery::OtherSource(_) => return Err("replacement writer belongs to another source".into()),
            AuthoringAttemptRecovery::None if current.replacement.as_deref() == Some(proof) => None,
            AuthoringAttemptRecovery::None => {
                if current != proof.predecessor || ContentHash::of_bytes(&state.run_record_bytes).to_hex() != proof.predecessor_record {
                    return Err("replacement predecessor changed before authoring".into());
                }
                if self.contributed_context_output_labels().contains(&identity.label) || self.session_snapshot_labels.contains(&identity.label) {
                    return Err("replacement cannot rewrite model context or a session snapshot".into());
                }
                let checkpoint = self.config.output_checkpoint(&ProviderHandoffRoute::ClientProjection, &self.granted_tools, &self.granted_interfaces, &identity.label)?;
                let mut authored = self.author_run_with_bindings(&state.run_record_bytes, &state.context_artifacts,
                    &[PreparedHandoff {identity:identity.clone(),output_checkpoint:checkpoint,verified_inputs:Vec::new()}],false,None,false,true,&[])?;
                authored.pending_handoff.replacement = Some(Box::new(proof.clone()));
                let mut value: serde_json::Value = serde_json::from_str(&authored.content).map_err(|e| e.to_string())?;
                value["interface_state"][self.config.provider_interface.as_str()]["pending_handoff"] = serde_json::to_value(&authored.pending_handoff).map_err(|e| e.to_string())?;
                authored.content = String::from_utf8(authored.json_format.unwrap_or(JsonFormat::Compact).encode(&value)?).map_err(|e| e.to_string())?;
                authored.after_hash = ContentHash::of_bytes(authored.content.as_bytes()).to_hex();
                authored.patch = json_patch::diff(&serde_json::from_slice(&state.run_record_bytes).map_err(|e| e.to_string())?, &value);
                validate_transition(&record, &RunRecord::from_bytes(authored.content.as_bytes()).map_err(|e| e.to_string())?, &self.config)?;
                Some(authored)
            }
        };
        let pending = authored.as_ref().map_or(&current, |attempt| &attempt.pending_handoff).clone();
        if pending.replacement.as_deref() != Some(proof) || reference(output(&pending)?)? != identity.artifact {
            return Err("recomputed output differs from its saved replacement attempt".into());
        }
        if let Some(authored) = authored {
            if !matches!(self.install_authored_run(&mut state, &authored).await?, AuthoredRunInstallOutcome::Installed) {
                return Err("rejected final replacement writer failed".into());
            }
        }
        drop(ownership);
        let outcomes = self.hand_pending_items(&pending, &state.run_record_bytes).await?;
        state.consumed_input = false; state.consumed_verified_input = false; state.consumed_context_free_independent_input = None;
        outcomes.into_iter().next().ok_or("replacement produced no admission outcome".into())
    }
}

#[test]
#[ignore = "requires the explicitly selected read-only rejected projection incident"]
fn selected_rejected_final_incident_has_exact_complete_refusal_proof() {
    let home=PathBuf::from(std::env::var("OCTOPUS_REPLAY_HOME").unwrap());
    let provider=InterfaceId::new("system_context_projection_731743f3a8af1bdf375eda5b");
    let path=home.join("state/runs/run_memory_projection_731743f3a8af1bdf375eda5b.json");
    let bytes=std::fs::read(&path).unwrap();let record=RunRecord::from_bytes(&bytes).unwrap();
    let dir=home.join("registry/interface").join(provider.as_str());
    let descriptor:InterfaceDescriptor=serde_json::from_slice(&std::fs::read(dir.join("interface.json")).unwrap()).unwrap();
    let life:serde_json::Value=serde_json::from_slice(&std::fs::read(dir.join("life.json")).unwrap()).unwrap();
    let scratch=tempfile::tempdir().unwrap();let (arrivals,_)=tokio::sync::mpsc::channel(1);
    let ctx=InterfaceLiftContext::unprivileged(provider.clone(),dir,Some(descriptor),scratch.path().join("provider"),
        home.join("ledger/movements.jsonl"),home.join("ledger/blobs"),home.join("ledger/ledger-index.sqlite"),
        BTreeMap::from([("run_records_root".into(),home.join("state/runs")),("interface_hand_in_root".into(),home.join("interface-hand-in"))]),
        arrivals,Default::default()).with_host_io_limits(serde_json::from_value(life["processes"][0]["io_limits"].clone()).unwrap()).unwrap();
    let pending=persisted_pending_handoff(&record,&provider).unwrap().unwrap();
    assert_eq!(pending.source_movement_seq,47886);
    let item=output(&pending).unwrap();
    assert_eq!(item.artifact_id.as_str(),"memory.experience.input:dc01eed83e14d5ddd4e481019a61d0b7c86e0ae78ac5d071cc21f8a7a02e0cec");
    assert_eq!(item.checkpoint.id.as_str(),"cp_27c0abaac3006e68cf76_derived_ffbf97a76da614623fbc");
    let old_file=InterfaceArtifactFile::new(home.join("interfaces").join(provider.as_str()).join(&item.manifest_relative_path)).unwrap();
    let limits=ctx.required_host_io_limits().unwrap();
    let captured=old_file.read_bounded(limits.max_manifest_bytes,limits.max_payload_bytes).unwrap();
    assert_eq!(captured.payload_bytes.len(),13973);
    let length=std::fs::metadata(&ctx.movements_path).unwrap().len();
    let upper=pending_delivery::Ledger::new(provider.clone(),ctx.movements_path.clone(),limits.max_control_frame_bytes).watermark().unwrap();
    let evidence=CheckedRunEvidence {movements_path:&ctx.movements_path,ledger_index_path:&ctx.ledger_index_path,blobs_dir:&ctx.blobs_dir,
        max_control_frame_bytes:limits.max_control_frame_bytes,max_manifest_bytes:limits.max_manifest_bytes,max_payload_bytes:limits.max_payload_bytes};
    let mut reader=FileCheckedRunReader::open(&evidence,None).unwrap().with_provider_context(&ctx);
    let (scope,refusals)=rejection(&mut reader,&record,&provider,&pending,upper).unwrap().unwrap();
    assert_eq!(scope.seq,47928);assert_eq!(refusals.iter().map(|row|row.seq).collect::<Vec<_>>(),vec![47930]);
    let source=reader.admitted_movement(47886).unwrap().unwrap().artifact.unwrap();
    let proof=Rejection {predecessor:pending,predecessor_record:ContentHash::of_bytes(&bytes).to_hex(),source,scope,refusals};
    validate_proof(&mut reader,&record,&provider,&proof,upper).unwrap();
    assert_eq!(std::fs::read(path).unwrap(),bytes);assert_eq!(std::fs::metadata(&ctx.movements_path).unwrap().len(),length);
    println!("readonly incident proof: source=47886 scope=47928 VerificationFailed=47930 payload=13973 complete-original-edges=1");
}
