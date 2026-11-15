#[path = "../../../../world_operator_chat/packages/octopus-operator-chat-client/tests/support/composition_fixture.rs"]
mod support;

#[path = "support/input_writer_conflict.rs"]
mod input_writer_conflict;

use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{InterfaceHostIoLimits, InterfaceLiftContext};
use octopus_interfaces::{InterfaceArtifactFile, InterfaceArtifactManifest};
use octopus_ledger::{
    ArtifactRef, BlobStore, ContentHash, Endpoint, InvocationMetadata, Movement, MovementDraft,
    MovementKind, MovementLog, Seq,
};
use octopus_ledger_index::LedgerSqliteIndex;
use octopus_ledger_store::{DurableBlobs, DurableLog};
use octopus_operator_chat_client::{
    input_intake::InputIntakeConfig, run_composition::plan_run_composition,
};
use octopus_provider_codex::intake_state::prepare_input;
use octopus_run_authoring::{
    CheckedRunEdit, CheckedRunEvidence, CheckedRunEvidenceReader, CheckedRunTransition,
    CheckedRunWriter, checked_provider_run_advance, checked_run_file_chain,
    checked_run_file_chain_before,
};
use octopus_run_record::RunRecord;
use serde_json::{Value, json};
use std::path::PathBuf;

struct Fixture {
    _root: tempfile::TempDir,
    ctx: InterfaceLiftContext,
    log: DurableLog,
    blobs: DurableBlobs,
    path: PathBuf,
    before: Vec<u8>,
    config: InputIntakeConfig,
    provider: CheckedRunWriter,
    failure_message: Option<String>,
}

impl CheckedRunEvidenceReader for &Fixture {
    fn max_payload_bytes(&self) -> u64 { self.ctx.host_io_limits.as_ref().unwrap().max_payload_bytes }
    fn movement(&mut self, seq: u64) -> Result<Option<Movement>, String> {
        Ok(self.log.read_from(Seq(seq)).into_iter().next())
    }

    fn artifact(
        &mut self,
        reference: &ArtifactRef,
    ) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> {
        let limits = self.ctx.host_io_limits.as_ref().unwrap();
        let artifact =
            InterfaceArtifactFile::new(self.ctx.blobs_dir.join(reference.content.to_hex()))
                .map_err(|e| e.to_string())?
                .with_payload_cache_root(&self.ctx.blobs_dir)
                .read_bounded(limits.max_manifest_bytes, limits.max_payload_bytes)
                .map_err(|e| e.to_string())?;
        assert_eq!(
            ContentHash::of_bytes(&artifact.manifest_bytes),
            reference.content
        );
        Ok((artifact.manifest, artifact.payload_bytes))
    }

    fn snapshot(&mut self, hash: &ContentHash) -> Result<Vec<u8>, String> {
        self.blobs.get(hash).ok_or("fixture snapshot absent".into())
    }
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let request = support::fixture(root.path());
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
        for relative in [
            "interface/system_service_input/interface.json",
            "interface/system_service_input/life.json",
        ] {
            support::write(
                &request.roots.registry_root,
                relative,
                serde_json::from_slice(
                    &std::fs::read(source.join("registry").join(relative)).unwrap(),
                )
                .unwrap(),
            );
        }
        for (file, pointer) in [
            ("registry/ee_template/existence/ee_template.json", "/allowed_interfaces"),
            ("ee/ee_existence.json", "/policy_narrowing/allowed_interfaces"),
            ("registry/interface/system_worker_template/policy.json", "/ceiling/interfaces"),
        ] {
            let mut value: Value =
                serde_json::from_slice(&std::fs::read(root.path().join(file)).unwrap()).unwrap();
            value.pointer_mut(pointer).unwrap().as_array_mut().unwrap().push(json!("system_service_input"));
            support::write(root.path(), file, value);
        }
        let descriptor = request
            .roots
            .registry_root
            .join("interface/world_provider_fixture/interface.json");
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(&descriptor).unwrap()).unwrap();
        value["interface_driver_config"]["private_config"]["input_intake"] =
            json!({"template_interface_id":"system_service_input"});
        std::fs::write(descriptor, serde_json::to_vec(&value).unwrap()).unwrap();
        let plan = plan_run_composition(&request).unwrap();
        for write in &plan.descriptor_writes {
            std::fs::create_dir_all(write.path.parent().unwrap()).unwrap();
            std::fs::write(&write.path, &write.content).unwrap();
        }
        let before = plan.run_write.content.into_bytes();
        let record = RunRecord::from_bytes(&before).unwrap();
        let intake = InterfaceId::new(&plan.receive_interface_id);
        let config: InputIntakeConfig =
            serde_json::from_value(record.interface_state[&intake]["input_intake"].clone())
                .unwrap();
        let topology = &record.interface_state[&config.provider_interface]["run_authoring_binding"];
        let result = topology["checkpoints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|cp| cp["role"] == "authoring_result")
            .unwrap();
        let provider = CheckedRunWriter {
            owner: config.provider_interface.clone(),
            source: serde_json::from_value(topology["authoring_source_interface"].clone()).unwrap(),
            door: serde_json::from_value(result["interface_binding"]["interface"].clone()).unwrap(),
            tool: config.provider_profile.file_write_tool.clone(),
            call_label: config.provider_profile.file_write_label.clone(),
            result_label: config.provider_profile.file_write_result_label.clone(),
        };
        let ledger = root.path().join("ledger");
        std::fs::create_dir_all(&ledger).unwrap();
        let (arrivals, _rx) = tokio::sync::mpsc::channel(1);
        let mut ctx = InterfaceLiftContext::unprivileged(
            intake,
            root.path().into(),
            None,
            root.path().into(),
            ledger.join("movements.jsonl"),
            ledger.join("blobs"),
            ledger.join("index.sqlite"),
            Default::default(),
            arrivals,
            tokio_util::sync::CancellationToken::new(),
        );
        ctx.host_io_limits = Some(InterfaceHostIoLimits {
            max_control_frame_bytes: 1 << 20,
            max_manifest_bytes: 1 << 20,
            max_payload_bytes: 1 << 24,
            max_stderr_bytes: 1024,
        });
        let log = DurableLog::open(&ctx.movements_path).unwrap();
        let blobs = DurableBlobs::open(&ctx.blobs_dir).unwrap();
        std::fs::create_dir_all(plan.run_write.path.parent().unwrap()).unwrap();
        std::fs::write(&plan.run_write.path, &before).unwrap();
        let mut fixture = Self {
            _root: root,
            ctx,
            log,
            blobs,
            path: plan.run_write.path,
            before,
            config,
            provider,
            failure_message: None,
        };
        let seed = fixture.artifact("native.work", json!({"text":"work", "artifact_ref":{"id":"opaque-nested-ref","content":ContentHash::of_bytes(b"opaque")}}));
        let sender: RunRecord = RunRecord::from_bytes(&serde_json::to_vec(&json!({"id":"sender-run","ee":"sender-ee","worker":"sender-worker","trace":"sender-run-trace","status":"running",
            "checkpoints":[{"id":"incoming-source","run":"sender-run","trace":"sender-checkpoint-trace","epoch":0,"interface_binding":octopus_runtime::AuthoredInterfaceBinding::all("source-provider".into()),
            "outbound_interface_bindings":[octopus_runtime::AuthoredInterfaceBinding::all(fixture.config.writer.owner.clone())]}],"active_checkpoints":["incoming-source"]})).unwrap()).unwrap();
        let checkpoint = sender.checkpoint(&"incoming-source".into()).unwrap();
        let source = checkpoint.inbound_interface().unwrap();
        let metadata: InvocationMetadata = serde_json::from_value(json!({"runtime":{
            "run":sender.id,"ee":sender.ee,"worker":sender.worker,"checkpoint":checkpoint.id,
            "interface":fixture.config.writer.owner,"epoch":0,"run_record_hash":fixture.blobs.put(&serde_json::to_vec(&sender).unwrap()).to_hex()},
            "checkpoint_binding_policy":{"inbound":checkpoint.interface_binding,"outbound":checkpoint.outbound_interface_bindings},
            "outbound_interface_bindings":[fixture.config.writer.owner],
            "checkpoint_admission":{"artifact_label":"native.work","required_artifact_labels":[],"complete":true}})).unwrap();
        fixture.log.append(MovementDraft {
            trace: checkpoint.trace.clone(), kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(source.clone()), to: Endpoint::Interface(fixture.config.writer.owner.clone()),
            artifact: Some(seed), invocation_metadata: Some(metadata.into()), causal_floor: None,
        });
        fixture
    }

    fn artifact(&mut self, label: &str, value: Value) -> ArtifactRef {
        let payload = self.blobs.put(&serde_json::to_vec(&value).unwrap());
        let manifest = InterfaceArtifactManifest {
            label: label.into(),
            payload_path: self.ctx.blobs_dir.join(payload.to_hex()),
            payload_content_hash: Some(payload.to_hex()),
        };
        let content = self.blobs.put(&serde_json::to_vec(&manifest).unwrap());
        ArtifactRef {
            id: format!("{label}:{}", content.to_hex()).into(),
            content,
        }
    }

    fn intake_edit(&self) -> CheckedRunEdit {
        prepare_input(&self.config, self.path.clone(), &self.before, 0, self.log.read_from(Seq(0))[0].artifact.as_ref().unwrap(), 0, 8 * 1024 * 1024)
            .unwrap()
            .edit
    }

    fn provider_edit(&mut self, before: &[u8]) -> CheckedRunEdit {
        let mut next: Value = serde_json::from_slice(before).unwrap();
        let owner = self.provider.owner.clone();
        let provider = owner.as_str();
        let frontier = next["interface_state"][provider]["frontier_checkpoint_ids"]
            .as_array()
            .unwrap()
            .clone();
        for row in next["checkpoints"].as_array_mut().unwrap() {
            if frontier.contains(&row["id"]) {
                row["id"] = json!(format!("{}_next", row["id"].as_str().unwrap()));
            }
        }
        for id in next["active_checkpoints"].as_array_mut().unwrap() {
            if frontier.contains(id) {
                *id = json!(format!("{}_next", id.as_str().unwrap()));
            }
        }
        let mut new_frontier = frontier
            .iter()
            .map(|id| json!(format!("{}_next", id.as_str().unwrap())))
            .collect::<Vec<_>>();
        let output = next["checkpoints"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| {
                new_frontier.contains(&row["id"])
                    && row["interface_binding"]["interface"] == provider
                    && row["required_verified_artifacts"]
                        .as_array()
                        .is_none_or(|requirements| requirements.is_empty())
            })
            .unwrap()
            .clone();
        let label = "provider.reply";
        let removed = next["checkpoints"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| {
                new_frontier.contains(&row["id"])
                    && row["interface_binding"]["interface"] == provider
                    && row["id"] != output["id"]
            })
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>();
        next["checkpoints"]
            .as_array_mut()
            .unwrap()
            .retain(|row| !removed.contains(&row["id"]));
        next["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .retain(|id| !removed.contains(id));
        new_frontier.retain(|id| !removed.contains(id));
        let artifact = self.artifact(label, json!({"text":"prepared output"}));
        let state = &mut next["interface_state"][provider];
        state["frontier_checkpoint_ids"] = json!(new_frontier);
        state["authoring_generation"] =
            json!(state["authoring_generation"].as_u64().unwrap_or(0) + 1);
        state["context_generation"] = json!(state["context_generation"].as_u64().unwrap_or(0) + 1);
        state["pending_handoff"] = json!({"source_movement_seq":0,"terminal":false,"items":[{
            "artifact_id":artifact.id,"artifact_content_hash":artifact.content.to_hex(),"artifact_label":label,
            "manifest_relative_path":"prepared-output.json","checkpoint":{
                "id":output["id"],"epoch":output["epoch"],"interface_binding":output["interface_binding"],
                "outbound_interface_bindings":output["outbound_interface_bindings"],
                "artifact_bindings":output["artifact_bindings"].as_array().unwrap().iter().map(|binding|binding["id"].clone()).collect::<Vec<_>>(),
                "required_verified_artifacts":[]}}]});
        next["status"] = json!("running");
        CheckedRunEdit::prepare(self.path.clone(), before, next, "provider-write".into()).unwrap()
    }

    fn effect(&mut self, edit: &CheckedRunEdit) {
        assert_eq!(
            ContentHash::of_bytes(&std::fs::read(&self.path).unwrap()).to_hex(),
            edit.before_hash
        );
        std::fs::write(&self.path, &edit.content).unwrap();
        assert_eq!(
            self.blobs.put(edit.content.as_bytes()).to_hex(),
            edit.after_hash
        );
    }

    fn movement(
        &mut self,
        writer: &CheckedRunWriter,
        edit: &CheckedRunEdit,
        snapshot: &[u8],
        result: Option<bool>,
    ) -> Movement {
        let draft = self.movement_draft(writer, edit, snapshot, result);
        self.log.append(draft)
    }

    fn movement_draft(
        &mut self,
        writer: &CheckedRunWriter,
        edit: &CheckedRunEdit,
        snapshot: &[u8],
        result: Option<bool>,
    ) -> MovementDraft {
        let record = RunRecord::from_bytes(snapshot).unwrap();
        let (from, to, label) = if result.is_some() {
            (&writer.door, &writer.owner, &writer.result_label)
        } else {
            (&writer.source, &writer.door, &writer.call_label)
        };
        let rows = record
            .checkpoints
            .iter()
            .filter(|cp| {
                record.active_checkpoints.contains(&cp.id)
                    && cp.inbound_interface() == Some(from)
                    && cp.outbound_interfaces().collect::<Vec<_>>() == [to]
            })
            .collect::<Vec<_>>();
        let [cp] = rows.as_slice() else {
            panic!("expected exact writer edge {from} -> {to}");
        };
        let payload = match result {
            None => json!({"tool":writer.tool,"correlation_id":edit.correlation_id,"arguments":edit.arguments().unwrap()}),
            Some(true) => {
                json!({"tool":writer.tool,"label":label,"correlation_id":edit.correlation_id,"ok":true,"body":{
                "result":"files_written","writes":[{"path":edit.path,"before_hash":edit.before_hash,"after_hash":edit.after_hash,"bytes":edit.content.len()}]}})
            }
            Some(false) => {
                json!({"tool":writer.tool,"label":label,"correlation_id":edit.correlation_id,"ok":false,
                "error":{"code":"tool_execution_failed","message":self.failure_message.clone().unwrap_or_else(|| format!("file.write patch optimistic hash mismatch for '{}': expected {}, observed {}", edit.path.display(), edit.before_hash, ContentHash::of_bytes(snapshot).to_hex()))}})
            }
        };
        if result.is_some() {
            serde_json::from_value::<octopus_tool_handlers::ToolResultArtifact>(payload.clone())
                .unwrap()
                .validate()
                .unwrap();
        } else {
            serde_json::from_value::<octopus_tool_handlers::ToolCallArtifact>(payload.clone())
                .unwrap();
        }
        let artifact = self.artifact(label, payload);
        let hash = self.blobs.put(snapshot);
        let metadata: InvocationMetadata = serde_json::from_value(json!({
            "runtime":{"run":record.id,"ee":record.ee,"worker":record.worker,"checkpoint":cp.id,"interface":to,"epoch":cp.epoch.0,"run_record_hash":hash.to_hex()},
            "grant":{"tools":[writer.tool],"interfaces":[to],"mark_outcomes":cp.required_verified_artifacts.iter().map(|r|r.mark.as_str()).collect::<Vec<_>>(),"role_artifacts":self.config.artifact_bindings.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
                "artifact_access":self.config.artifact_bindings.iter().map(|b|(b.id.to_string(),json!({"read":true,"change":false,"create":false,"delete":false}))).collect::<serde_json::Map<_,_>>()},
            "artifact_binding_refs":cp.artifact_bindings.iter().map(|b| ArtifactRef { id:b.id.clone(),content:ContentHash::of_bytes(b"bound fixture content") }).collect::<Vec<_>>(),
            "checkpoint_binding_policy":{"inbound":cp.interface_binding,"outbound":cp.outbound_interface_bindings},
            "outbound_interface_bindings":[to],"required_verified_artifacts":cp.required_verified_artifacts,
            "checkpoint_admission":{"complete":true,"artifact_label":label,"required_artifact_labels":cp.required_verified_artifacts.iter().map(|r|r.artifact_label.as_str()).collect::<Vec<_>>()}
        })).unwrap();
        MovementDraft {
            trace: record.trace,
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(from.clone()),
            to: Endpoint::Interface(to.clone()),
            artifact: Some(artifact),
            invocation_metadata: Some(metadata.into()),
            causal_floor: None,
        }
    }

    fn evidence(&self) -> CheckedRunEvidence<'_> {
        let limits = self.ctx.host_io_limits.as_ref().unwrap();
        CheckedRunEvidence {
            movements_path: &self.ctx.movements_path,
            ledger_index_path: &self.ctx.ledger_index_path,
            blobs_dir: &self.ctx.blobs_dir,
            max_control_frame_bytes: limits.max_control_frame_bytes,
            max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        }
    }

    fn chain(
        &self,
        before: &[u8],
        after: &[u8],
        after_seq: u64,
    ) -> Result<Vec<CheckedRunTransition>, String> {
        checked_run_file_chain(
            &self.evidence(),
            &self.path,
            before,
            after,
            after_seq,
            &[self.config.writer.clone(), self.provider.clone()],
        )
    }

    fn advance(
        &self,
        before: &[u8],
        after: &[u8],
        after_seq: u64,
    ) -> Result<Vec<CheckedRunTransition>, String> {
        checked_provider_run_advance(
            &self.ctx,
            &self.path,
            before,
            after,
            after_seq,
            &self.config.provider_interface,
            &self.config.provider_profile,
        )
    }
}

#[test]
fn interleaved_effects_accept_both_receipt_orders_and_look_back_before_last_result() {
    for intake_first in [true, false] {
        let mut f = Fixture::new();
        let before = f.before.clone();
        let i = f.config.writer.clone();
        let p = f.provider.clone();
        let input = f.intake_edit();
        let i_call = f.movement(&i, &input, &before, None);
        f.effect(&input);
        let provider = f.provider_edit(input.content.as_bytes());
        let p_call = f.movement(&p, &provider, input.content.as_bytes(), None);
        f.effect(&provider);
        let (i_result, p_result) = if intake_first {
            (
                f.movement(&i, &input, provider.content.as_bytes(), Some(true)),
                f.movement(&p, &provider, provider.content.as_bytes(), Some(true)),
            )
        } else {
            let p_result = f.movement(&p, &provider, provider.content.as_bytes(), Some(true));
            (
                f.movement(&i, &input, provider.content.as_bytes(), Some(true)),
                p_result,
            )
        };
        assert!(p_call.seq < i_result.seq);
        let historical = checked_run_file_chain_before(
            &mut &f,
            &f.path,
            &before,
            provider.content.as_bytes(),
            0,
            f.log.len() as u64,
            &[i, p],
        )
        .unwrap();
        assert_eq!(historical.len(), 2);
        for indexed in [false, true] {
            if indexed {
                LedgerSqliteIndex::open_healthy(&f.ctx.ledger_index_path, &f.log).unwrap();
            }
            let chain = f.chain(&before, provider.content.as_bytes(), 0).unwrap();
            assert_eq!(
                chain
                    .iter()
                    .map(|t| (t.call.seq, t.result.seq))
                    .collect::<Vec<_>>(),
                [(i_call.seq, i_result.seq), (p_call.seq, p_result.seq)]
            );
            assert_eq!(chain[0].after, input.content.as_bytes());
            assert_eq!(chain[1].before, input.content.as_bytes());
            assert!(
                f.chain(
                    input.content.as_bytes(),
                    provider.content.as_bytes(),
                    i_result.seq.0
                )
                .is_err()
            );
            assert_eq!(
                f.advance(&before, provider.content.as_bytes(), 0)
                    .unwrap()
                    .len(),
                2
            );
            let continued = f
                .advance(
                    input.content.as_bytes(),
                    provider.content.as_bytes(),
                    i_result.seq.0,
                )
                .unwrap();
            assert_eq!(continued.len(), 1);
            assert_eq!(continued[0].call.seq, p_call.seq);
        }
    }
}

#[test]
fn absent_receipt_is_not_an_effect_proof_and_later_admission_completes_it() {
    let mut f = Fixture::new();
    let before = f.before.clone();
    let i = f.config.writer.clone();
    let p = f.provider.clone();
    let input = f.intake_edit();
    f.movement(&i, &input, &before, None);
    f.effect(&input);
    let provider = f.provider_edit(input.content.as_bytes());
    f.movement(&p, &provider, input.content.as_bytes(), None);
    f.effect(&provider);
    f.movement(&p, &provider, provider.content.as_bytes(), Some(true));
    assert!(
        f.chain(&before, provider.content.as_bytes(), 0)
            .unwrap_err()
            .contains("awaits admitted checked file.write results")
    );
    f.movement(&i, &input, provider.content.as_bytes(), Some(true));
    assert_eq!(
        f.advance(&before, provider.content.as_bytes(), 0)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn rejected_cas_does_not_poison_successful_writers_but_claimed_success_does() {
    for rejected in [true, false] {
        let mut f = Fixture::new();
        let before = f.before.clone();
        let i = f.config.writer.clone();
        let p = f.provider.clone();
        let input = f.intake_edit();
        f.movement(&i, &input, &before, None);
        f.effect(&input);
        let mut stale = input.clone();
        stale.correlation_id = "stale-preimage".into();
        f.movement(&i, &stale, input.content.as_bytes(), None);
        let provider = f.provider_edit(input.content.as_bytes());
        f.movement(&p, &provider, input.content.as_bytes(), None);
        f.effect(&provider);
        f.movement(&i, &stale, provider.content.as_bytes(), Some(!rejected));
        f.movement(&i, &input, provider.content.as_bytes(), Some(true));
        f.movement(&p, &provider, provider.content.as_bytes(), Some(true));
        if rejected {
            assert_eq!(
                f.advance(&before, provider.content.as_bytes(), 0)
                    .unwrap()
                    .len(),
                2
            );
        } else {
            assert!(
                f.chain(&before, provider.content.as_bytes(), 0)
                    .unwrap_err()
                    .contains("exact before snapshot")
            );
        }
    }
}

#[test]
fn later_result_snapshot_must_preserve_the_exact_admitted_result_checkpoint() {
    let mut f = Fixture::new();
    let before = f.before.clone();
    let i = f.config.writer.clone();
    let p = f.provider.clone();
    let input = f.intake_edit();
    f.movement(&i, &input, &before, None);
    f.effect(&input);
    let provider = f.provider_edit(input.content.as_bytes());
    f.movement(&p, &provider, input.content.as_bytes(), None);
    f.effect(&provider);
    let mut changed: Value = serde_json::from_str(&provider.content).unwrap();
    let checkpoint = changed["checkpoints"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|cp| cp["interface_binding"]["interface"] == i.door.as_str())
        .unwrap();
    checkpoint["trace"] = json!("substituted-result-checkpoint");
    let snapshot = serde_json::to_vec_pretty(&changed).unwrap();
    f.movement(&i, &input, &snapshot, Some(true));
    f.movement(&p, &provider, provider.content.as_bytes(), Some(true));
    assert!(
        f.chain(&before, provider.content.as_bytes(), 0)
            .unwrap_err()
            .contains("exact admitted checkpoint")
    );
}

fn retained_input(f: &Fixture) -> octopus_provider_codex::intake_state::PendingInput {
    let source = f.log.read_from(Seq(0))[0].clone();
    octopus_provider_codex::intake_state::PendingInput {
        delivery: serde_json::from_value(json!({"schema_version":1,"source_movement_seq":source.seq.0,
            "source_artifact":source.artifact,"message":{"action":"followup","control_id":"retained-intent","text":"same original input"}})).unwrap(),
        source,
        edit: Some(prepare_input(&f.config, f.path.clone(), &f.before, 0, f.log.read_from(Seq(0))[0].artifact.as_ref().unwrap(), 0, 8 * 1024 * 1024).unwrap()),
    }
}

#[test]
fn intake_retries_both_negative_cas_boundaries_only_with_exact_call_and_independent_proof() {
    use octopus_provider_codex::intake_state::{
        IntakeState, checked_input_result, load_state, reprepare_input, save_state,
    };
    for boundary in [
        "patch optimistic hash mismatch",
        "optimistic hash changed before commit",
    ] {
        let mut f = Fixture::new();
        let before = f.before.clone();
        let i = f.config.writer.clone();
        let p = f.provider.clone();
        let pending = retained_input(&f);
        let attempt = pending.edit.as_ref().unwrap();
        let call = f.movement(&i, &attempt.edit, &before, None);
        let provider = f.provider_edit(&before);
        f.movement(&p, &provider, &before, None);
        f.effect(&provider);
        f.failure_message = Some(format!(
            "file.write {boundary} for '{}': expected {}, observed {}",
            f.path.display(),
            attempt.edit.before_hash,
            provider.after_hash
        ));
        let negative = f.movement(&i, &attempt.edit, provider.content.as_bytes(), Some(false));
        assert!(f.advance(&before, provider.content.as_bytes(), 0).is_err());
        f.movement(&p, &provider, provider.content.as_bytes(), Some(true));
        let rows = f.log.read_from(Seq(0));
        let result = checked_input_result(&f.ctx, &f.config, attempt, &rows)
            .unwrap()
            .unwrap();
        assert!(!result.ok && result.body.is_none());
        let chain = f.advance(&before, provider.content.as_bytes(), 0).unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].writer, p);
        let path = f._root.path().join("pending.json");
        save_state(
            &path,
            &IntakeState {
                completed_through: None,
                pending: Some(pending.clone()),
            },
            1 << 24,
        )
        .unwrap();
        let mut recovered = load_state(&path, 1 << 24).unwrap().pending.unwrap();
        recovered.edit = Some(
            reprepare_input(
                &f.config,
                &recovered,
                provider.content.as_bytes(),
                negative.seq.0,
                &chain, 8 * 1024 * 1024
            )
            .unwrap(),
        );
        assert_eq!(
            serde_json::to_value(&pending.delivery).unwrap(),
            serde_json::to_value(&recovered.delivery).unwrap()
        );
        assert_eq!(pending.source, recovered.source);
        let rebased = recovered.edit.as_ref().unwrap();
        assert_eq!(rebased.edit.before_hash, provider.after_hash);
        assert_ne!(rebased.edit.correlation_id, attempt.edit.correlation_id);
        let next = RunRecord::from_bytes(rebased.edit.content.as_bytes()).unwrap();
        let winner = RunRecord::from_bytes(provider.content.as_bytes()).unwrap();
        assert_eq!(
            next.interface_state[&p.owner],
            winner.interface_state[&p.owner]
        );
        assert!(reprepare_input(&f.config, &pending, &before, 0, &[], 8 * 1024 * 1024).is_err());
        let missing = rows
            .iter()
            .filter(|row| row.seq != call.seq)
            .cloned()
            .collect::<Vec<_>>();
        assert!(checked_input_result(&f.ctx, &f.config, attempt, &missing).is_err());
        for field in ["checkpoint", "ee", "worker"] {
            let mut forged = rows.clone();
            let index = forged
                .iter()
                .position(|row| row.seq == negative.seq)
                .unwrap();
            let mut metadata = serde_json::to_value(
                forged[index]
                    .invocation_metadata
                    .as_ref()
                    .unwrap()
                    .invocation()
                    .unwrap(),
            )
            .unwrap();
            metadata["runtime"][field] = json!("forged");
            forged[index].invocation_metadata = Some(
                serde_json::from_value::<InvocationMetadata>(metadata)
                    .unwrap()
                    .into(),
            );
            assert!(checked_input_result(&f.ctx, &f.config, attempt, &forged).is_err());
        }
        for field in [
            "path",
            "patch",
            "expected_before_hash",
            "expected_after_hash",
        ] {
            let mut forged = rows.clone();
            let index = forged.iter().position(|row| row.seq == call.seq).unwrap();
            let (_, payload) =
                CheckedRunEvidenceReader::artifact(&mut &f, call.artifact.as_ref().unwrap())
                    .unwrap();
            let mut value: Value = serde_json::from_slice(&payload).unwrap();
            value["arguments"][field] = json!("forged");
            forged[index].artifact = Some(f.artifact(&i.call_label, value));
            assert!(checked_input_result(&f.ctx, &f.config, attempt, &forged).is_err());
        }
        let mut forged = rows.clone();
        let index = forged
            .iter()
            .position(|row| row.seq == negative.seq)
            .unwrap();
        let mut claimed = serde_json::to_value(result).unwrap();
        claimed["body"] = json!({"result":"files_written"});
        forged[index].artifact = Some(f.artifact(&i.result_label, claimed));
        assert!(checked_input_result(&f.ctx, &f.config, attempt, &forged).is_err());
    }
}

#[test]
fn intake_verification_failure_rebases_retained_input_without_inventing_tool_result() {
    use octopus_provider_codex::intake_state::{checked_input_result, reprepare_input};
    use octopus_run_authoring::checked_edit_verification_failure;
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut f = Fixture::new();
    let before = f.before.clone();
    let i = f.config.writer.clone();
    let p = f.provider.clone();
    let pending = retained_input(&f);
    let attempt = pending.edit.as_ref().unwrap();
    let provider = f.provider_edit(&before);
    f.movement(&p, &provider, &before, None);
    f.effect(&provider);
    f.movement(&p, &provider, provider.content.as_bytes(), Some(true));
    let mut draft = f.movement_draft(&i, &attempt.edit, provider.content.as_bytes(), None);
    let mut metadata = draft
        .invocation_metadata
        .take()
        .unwrap()
        .invocation()
        .unwrap()
        .clone();
    let (_, payload) = (&f).artifact(draft.artifact.as_ref().unwrap()).unwrap();
    let verify = |payload: &[u8], metadata: &InvocationMetadata| {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../../registry/verifier/tool_call_allowed/verify.py");
        let mut child = Command::new("python3")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                &serde_json::to_vec(&json!({
            "payload_utf8":std::str::from_utf8(payload).unwrap(),"metadata":metadata,
            "params":metadata.required_verified_artifacts[0].params}))
                .unwrap(),
            )
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let verdict = verify(&payload, &metadata);
    assert_eq!(verdict["passed"], false);
    assert_eq!(verdict["details"]["reason"], "metadata_match_mismatch");
    let runtime = metadata.runtime.as_mut().unwrap();
    runtime.interface = i.source.clone();
    let requirement = &metadata.required_verified_artifacts[0];
    metadata.verification_failure = Some(
        serde_json::from_value(json!({"error":"verifier_rejected",
        "run":runtime.run,"checkpoint":runtime.checkpoint,"verifier":requirement.verifier,
        "mark":requirement.mark,"artifact_label":i.call_label,"details":verdict["details"]}))
        .unwrap(),
    );
    draft.kind = MovementKind::InterfaceArrival;
    draft.to = Endpoint::Interface(runtime.worker.clone());
    draft.invocation_metadata = Some(metadata.into());
    let arrival = f.log.append(draft.clone());
    assert!(
        checked_edit_verification_failure(&f.ctx, &i, &attempt.edit, 0)
            .unwrap()
            .is_none()
    );
    draft.kind = MovementKind::VerificationFailed;
    let failure = f.log.append(draft);
    LedgerSqliteIndex::open_healthy(&f.ctx.ledger_index_path, &f.log).unwrap();
    assert_eq!(
        checked_edit_verification_failure(&f.ctx, &i, &attempt.edit, 1).unwrap(),
        Some(failure.clone())
    );
    let mut index = octopus_ledger_index::ArrivalDedupIndex::new();
    index.catch_up(&f.log).unwrap();
    let failed_metadata = failure
        .invocation_metadata
        .as_ref()
        .unwrap()
        .invocation()
        .unwrap()
        .runtime
        .as_ref()
        .unwrap();
    assert!(
        !index
            .checkpoint_admitted(
                &failed_metadata.run,
                &failed_metadata.checkpoint,
                failed_metadata.epoch
            )
            .unwrap()
    );
    let rows = f.log.read_from(Seq(0));
    assert!(
        checked_input_result(&f.ctx, &f.config, attempt, &rows)
            .unwrap()
            .is_none()
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.from == Endpoint::Interface(i.source.clone())
                && row.to == Endpoint::Interface(i.door.clone()))
    );
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row.kind, MovementKind::MarkOutcomeAdmitted(_)))
    );
    let chain = f.advance(&before, provider.content.as_bytes(), 0).unwrap();
    let rebased = reprepare_input(
        &f.config,
        &pending,
        provider.content.as_bytes(),
        failure.seq.0,
        &chain, 8 * 1024 * 1024
    )
    .unwrap();
    let mut rebased_draft = f.movement_draft(&i, &rebased.edit, provider.content.as_bytes(), None);
    let (_, payload) = (&f)
        .artifact(rebased_draft.artifact.as_ref().unwrap())
        .unwrap();
    let metadata = rebased_draft
        .invocation_metadata
        .take()
        .unwrap()
        .invocation()
        .unwrap()
        .clone();
    assert_eq!(verify(&payload, &metadata)["passed"], true);
    assert_eq!(
        metadata.runtime.as_ref().unwrap().checkpoint,
        arrival
            .invocation_metadata
            .as_ref()
            .unwrap()
            .invocation()
            .unwrap()
            .runtime
            .as_ref()
            .unwrap()
            .checkpoint
    );
    let next = RunRecord::from_bytes(rebased.edit.content.as_bytes()).unwrap();
    assert_eq!(
        next.interface_state[&p.owner],
        RunRecord::from_bytes(provider.content.as_bytes())
            .unwrap()
            .interface_state[&p.owner]
    );
    let mut foreign = attempt.edit.clone();
    foreign.correlation_id.push_str("-foreign");
    assert!(
        checked_edit_verification_failure(&f.ctx, &i, &foreign, 0)
            .unwrap()
            .is_none()
    );
    assert_eq!(f.log.len(), rows.len());
}

#[test]
fn delivered_source_is_bound_read_only_and_requires_its_exact_admitted_edge() {
    use octopus_run_authoring::{input_artifacts, context_artifact_payloads};
    let mut f = Fixture::new();
    let source = f.log.read_from(Seq(0))[0].clone();
    let before = f.before.clone();
    let writer = f.config.writer.clone();
    let source_ref = source.artifact.clone().unwrap();
    let mut historical: Value = serde_json::from_str(&f.intake_edit().content).unwrap();
    historical["interface_state"][writer.owner.as_str()].as_object_mut().unwrap().remove("input_artifact_bindings");
    for row in historical["checkpoints"].as_array_mut().unwrap() { row["artifact_bindings"].as_array_mut().unwrap().retain(|binding| binding["id"] != source_ref.id.as_str()); }
    let historical = CheckedRunEdit::prepare(f.path.clone(), &before, historical, "historical-intake".into()).unwrap();
    f.movement(&writer, &historical, &before, None); f.effect(&historical);
    let result = f.movement(&writer, &historical, historical.content.as_bytes(), Some(true));
    assert_eq!(f.advance(&before, historical.content.as_bytes(), 0).unwrap().len(), 1);
    f.before = historical.content.as_bytes().to_vec();
    let before = f.before.clone();
    let edit = f.intake_edit();
    f.movement(&writer, &edit, &before, None);
    f.effect(&edit);
    f.movement(&writer, &edit, edit.content.as_bytes(), Some(true));
    let chain = f.advance(&before, edit.content.as_bytes(), result.seq.0).unwrap();
    let record = RunRecord::from_bytes(edit.content.as_bytes()).unwrap();
    let checkpoint = record.checkpoints.iter().find(|cp| cp.inbound_interface() == Some(&writer.owner)
        && cp.outbound_interfaces().next() == Some(&f.config.provider_interface)).unwrap();
    assert_eq!(checkpoint.artifact_bindings.iter().filter(|b| b.id == source_ref.id).count(), 1);
    assert_eq!(checkpoint.artifact_bindings.last().unwrap().access, octopus_runtime::ArtifactAccess::read_only());
    assert!(!checkpoint.artifact_bindings.iter().any(|b| b.id.as_str() == "opaque-nested-ref"));
    let (_, bytes) = (&f).artifact(&source_ref).unwrap();
    let payloads = vec![octopus_interfaces::ArtifactBindingPayload::new(source_ref.clone(), bytes)];
    let mut metadata: InvocationMetadata = serde_json::from_value(json!({"runtime":{"run":record.id,"ee":record.ee,"worker":record.worker,
        "checkpoint":checkpoint.id,"interface":f.config.provider_interface,"epoch":0},
        "artifact_binding_refs":[source_ref],"grant":{"artifact_access":{source_ref.id.as_str():{"read":true,"change":false,"create":false,"delete":false}}}})).unwrap();
    assert!(context_artifact_payloads(&record, &metadata, &payloads).unwrap().is_empty());
    metadata.grant.artifact_access.clear();
    assert!(context_artifact_payloads(&record, &metadata, &payloads).is_err());
    metadata.artifact_binding_refs.clear();
    assert!(context_artifact_payloads(&record, &metadata, &[]).is_err());
    let template = record.interface_state[&f.config.provider_interface]["run_authoring_binding"]["checkpoints"].as_array().unwrap().iter().find(|cp| cp["role"] == "stable").unwrap();
    let stable = record.checkpoints.iter().find(|cp| serde_json::to_value(&cp.interface_binding).unwrap() == template["interface_binding"]).unwrap();
    let stable_payloads = stable.artifact_bindings.iter().map(|binding| { let bytes = std::fs::read(f._root.path().join("registry/artifact").join(binding.id.as_str()).join("artifact.json")).unwrap(); octopus_interfaces::ArtifactBindingPayload::new(ArtifactRef { id:binding.id.clone(),content:ContentHash::of_bytes(&bytes) },bytes) }).collect::<Vec<_>>();
    metadata.runtime.as_mut().unwrap().checkpoint = stable.id.clone();
    metadata.artifact_binding_refs = stable_payloads.iter().map(|bound| bound.artifact.clone()).collect();
    metadata.grant.artifact_access = stable.artifact_bindings.iter().map(|binding| (binding.id.to_string(),binding.access.clone())).collect();
    assert_eq!(context_artifact_payloads(&record, &metadata, &stable_payloads).unwrap().len(), stable_payloads.len());
    struct Changed<'a>(&'a Fixture, Movement);
    impl CheckedRunEvidenceReader for Changed<'_> {
        fn max_payload_bytes(&self) -> u64 { self.0.max_payload_bytes() }
        fn movement(&mut self, seq: u64) -> Result<Option<Movement>, String> { Ok((seq == self.1.seq.0).then(|| self.1.clone())) }
        fn artifact(&mut self, reference: &ArtifactRef) -> Result<(InterfaceArtifactManifest, Vec<u8>), String> { let mut reader = self.0; reader.artifact(reference) }
        fn snapshot(&mut self, hash: &ContentHash) -> Result<Vec<u8>, String> { let mut reader = self.0; reader.snapshot(hash) }
    }
    for case in 0..9 {
        let mut changed = source.clone();
        let mut meta = changed.invocation_metadata.take().unwrap().invocation().unwrap().clone();
        match case {
            0 => changed.artifact.as_mut().unwrap().id = "unadmitted".into(),
            1 => meta.runtime.as_mut().unwrap().run = "foreign".into(),
            2 => changed.to = Endpoint::Interface("foreign".into()),
            3 => meta.checkpoint_admission.as_mut().unwrap().complete = false,
            4 => meta.runtime.as_mut().unwrap().epoch += 1,
            5 => changed.seq.0 += 1,
            7 => meta.checkpoint_admission = None,
            8 => meta.outbound_interface_bindings.clear(),
            _ => changed.trace = octopus_ledger::TraceId("sender-run-trace".into()),
        }
        changed.invocation_metadata = Some(meta.into());
        assert!(input_artifacts::validate_input_transition(&mut Changed(&f, changed), &chain[0]).is_err());
    }
    let original = source.invocation_metadata.as_ref().unwrap().invocation().unwrap();
    let mut sender: Value = serde_json::from_slice(&f.blobs.get(&ContentHash::from_hex(original.runtime.as_ref().unwrap().run_record_hash.as_ref().unwrap()).unwrap()).unwrap()).unwrap(); let target = sender["checkpoints"][0].clone();
    sender["checkpoints"][0]["interface_binding"]["config"]["artifact_policy"] = json!({"mode":"allow","names":["tool.call"],"namespaces":[]});
    sender["checkpoints"][0]["outbound_interface_bindings"] = json!([octopus_runtime::AuthoredInterfaceBinding::all("source-room".into())]);
    sender["checkpoints"].as_array_mut().unwrap().push(target); sender["checkpoints"][1]["id"] = json!("target-binding"); sender["active_checkpoints"].as_array_mut().unwrap().push(json!("target-binding"));
    for case in 0..9 {
        let mut pinned = sender.clone();
        let mut emitted = source.clone();
        emitted.from = Endpoint::Interface("source-room".into());
        let mut meta = original.clone();
        meta.checkpoint_admission = None; meta.required_verified_artifacts.clear(); meta.outbound_interface_bindings.clear(); meta.artifact_binding_refs.clear();
        meta.grant.interfaces = serde_json::from_value(json!([writer.owner])).unwrap();
        match case {
            1 => meta.grant.interfaces = Default::default(),
            2 => emitted.from = Endpoint::Interface("unbound-room".into()),
            3 => pinned["active_checkpoints"] = json!(["incoming-source"]),
            4 => { pinned["checkpoints"][1]["interface_binding"] = json!({"interface":writer.owner,"config":{"artifact_policy":{"mode":"allow","names":["other.work"],"namespaces":[]}}}); meta.checkpoint_binding_policy.as_mut().unwrap().outbound.insert(0, serde_json::from_value(pinned["checkpoints"][1]["interface_binding"].clone()).unwrap()); }
            5 => meta.runtime.as_mut().unwrap().epoch += 1,
            6 => emitted.trace = octopus_ledger::TraceId("sender-run-trace".into()),
            7 => meta.runtime.as_mut().unwrap().worker = "foreign".into(),
            8 => pinned["checkpoints"][1]["outbound_interface_bindings"] = json!([]),
            _ => {}
        }
        meta.checkpoint_binding_policy.as_mut().unwrap().inbound = serde_json::from_value(pinned["checkpoints"][0]["interface_binding"].clone()).unwrap();
        meta.runtime.as_mut().unwrap().run_record_hash = Some(f.blobs.put(&serde_json::to_vec(&pinned).unwrap()).to_hex());
        emitted.invocation_metadata = Some(meta.into());
        assert_eq!(input_artifacts::validate_input_transition(&mut Changed(&f, emitted), &chain[0]).is_ok(), case == 0, "explicit emission case {case}");
    }
}
