use super::*;

impl ProviderOutputHandoff {
    #[cfg(test)]
    pub(crate) fn author_run(
        &self,
        current: &[u8],
        context_artifacts: &[ArtifactId],
        outputs: &[PreparedHandoff],
        refresh_context: bool,
        consumed_context_free_independent_input: Option<&RunAuthoringCheckpoint>,
        consumed_verified_input: bool,
        terminal: bool,
    ) -> Result<AuthoredRun, String> {
        self.author_run_with_bindings(current, context_artifacts, outputs, refresh_context,
            consumed_context_free_independent_input, consumed_verified_input, terminal, &[])
    }

    pub(crate) fn author_run_with_bindings(
        &self,
        current: &[u8],
        context_artifacts: &[ArtifactId],
        outputs: &[PreparedHandoff],
        refresh_context: bool,
        consumed_context_free_independent_input: Option<&RunAuthoringCheckpoint>,
        consumed_verified_input: bool,
        terminal: bool,
        bindings: &[ProviderOutgoingBinding],
    ) -> Result<AuthoredRun, String> {
        self.author_run_with_generations(current, context_artifacts, outputs, refresh_context, u64::from(refresh_context),
            consumed_context_free_independent_input, consumed_verified_input, terminal, bindings, Vec::new(), None)
    }

    pub(crate) fn author_run_with_generations(
        &self,
        current: &[u8],
        context_artifacts: &[ArtifactId],
        outputs: &[PreparedHandoff],
        refresh_context: bool,
        context_generations: u64,
        consumed_context_free_independent_input: Option<&RunAuthoringCheckpoint>,
        consumed_verified_input: bool,
        terminal: bool,
        bindings: &[ProviderOutgoingBinding],
        standing: Vec<PendingHandoffItem>,
        continuation: Option<&PendingHandoff>,
    ) -> Result<AuthoredRun, String> {
        let first_output = outputs
            .first()
            .ok_or_else(|| "provider cannot author an empty response batch".to_string())?;
        let record = RunRecord::from_bytes(current).map_err(|error| {
            format!(
                "provider run '{}' file '{}' is malformed: {error}",
                self.config.run.as_str(),
                self.run_path.display()
            )
        })?;
        if record.id != self.config.run || record.worker != self.config.worker {
            return Err(format!(
                "provider run topology drifted: file has run='{}' worker='{}', config requires run='{}' worker='{}'",
                record.id.as_str(),
                record.worker.as_str(),
                self.config.run.as_str(),
                self.config.worker.as_str()
            ));
        }
        let continued_context = continuation
            .map(|prior| source_continuation::continued_context(&record, &self.config, prior, context_artifacts, outputs))
            .transpose()?;
        let context_artifacts = continued_context.as_deref().unwrap_or(context_artifacts);
        let verified_input_carriers = outputs
            .iter()
            .filter(|output| !output.verified_inputs.is_empty())
            .collect::<Vec<_>>();
        if !verified_input_carriers.is_empty() && !terminal {
            return Err(
                "provider must author verified inputs in the terminal batch before exposing their request artifact"
                    .to_string(),
            );
        }
        if verified_input_carriers.len() > 1 {
            return Err(
                "provider response batch declares verified inputs from more than one request artifact"
                    .to_string(),
            );
        }
        let mut next_topology = self.bound_topology.clone();
        for binding in bindings {
            if binding.interface.as_str().is_empty() || binding.interface == self.config.provider_interface
                || binding.artifact_label.is_empty() || binding.artifact_label.trim() != binding.artifact_label {
                return Err("provider outgoing binding has an invalid exact interface or artifact label".into());
            }
            let policy = BindingArtifactPolicy::Allow {
                names: vec![binding.artifact_label.clone()], namespaces: Vec::new(),
            };
            let outbound = AuthoredInterfaceBinding {
                interface: binding.interface.clone(),
                config: InterfaceBindingConfig { artifact_policy: policy.clone() },
            };
            if next_topology.checkpoints.iter().any(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderInterfaceOutput
                    && checkpoint.outbound_interface_bindings == [outbound.clone()]
            }) { continue; }
            let identity = serde_json::to_vec(binding).map_err(|error| error.to_string())?;
            next_topology.checkpoints.push(RunAuthoringCheckpoint {
                name: format!("outgoing_{}", ContentHash::of_bytes(&identity).to_hex()),
                role: RunAuthoringCheckpointRole::ProviderInterfaceOutput,
                result_receiving: BTreeMap::new(),
                static_artifact_bindings: binding.artifact_bindings.clone(), owner_authoring: None,
                interface_binding: AuthoredInterfaceBinding {
                    interface: self.config.provider_interface.clone(),
                    config: InterfaceBindingConfig { artifact_policy: policy },
                },
                outbound_interface_bindings: vec![outbound],
                include_context: false, required_verified_artifacts: Vec::new(),
            });
        }
        if terminal && consumed_verified_input {
            next_topology
                .checkpoints
                .retain(|checkpoint| checkpoint.role != RunAuthoringCheckpointRole::VerifiedInput);
        }
        if let Some(carrier) = verified_input_carriers.first() {
            if next_topology
                .checkpoints
                .iter()
                .any(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::VerifiedInput)
            {
                return Err(
                    "provider cannot expose a second operator request while another verified input remains outstanding"
                        .to_string(),
                );
            }
            next_topology
                .checkpoints
                .extend(carrier.verified_inputs.iter().cloned());
        }
        let deferred_publication = terminal && (outputs.iter().any(|output| {
            !output.output_checkpoint.result_receiving.is_empty() && context_artifacts.contains(&output.identity.artifact.id)
        }) || continuation.is_some_and(|prior| prior.deferred_publication));
        let consumed_result_input = result_inputs::prepare(
            self, &record, outputs, terminal.then_some(context_artifacts), &mut next_topology,
        )?;
        if continuation.is_some() && consumed_result_input.is_some() {
            return Err("a source continuation cannot consume its source input again".into());
        }
        let mut next_config = self.config.clone();
        next_config.checkpoints = next_topology.checkpoints.clone();
        next_config.validate(&self.config.provider_interface, &self.config.run)?;
        let provider_state = record
            .interface_state
            .get(&self.config.provider_interface)
            .ok_or_else(|| {
                format!(
                    "provider run '{}' has no interface_state for '{}'",
                    record.id.as_str(),
                    self.config.provider_interface.as_str()
                )
            })?;
        let previous_frontier = {
            validate_bound_run_authoring_topology(&record, &self.config, None)?;
            if provider_state.contains_key("pending_handoff") {
                frontier_checkpoint_ids(provider_state)?
            } else if optional_u64(provider_state, "context_generation")? == 0 {
                input_revision::validate_initial_history(&self.ctx, &self.run_path, current, &self.config, self.source_movement_seq)?;
                raw_frontier_checkpoint_ids(provider_state)?
            } else {
                return Err(format!(
                    "provider run '{}' profile-derived frontier lost pending handoff evidence",
                    record.id.as_str()
                ));
            }
        };
        let mut mandatory_role_artifacts = current_mandatory_role_artifacts(&record, &self.config)?;
        for binding in bindings {
            for artifact in &binding.artifact_bindings {
                append_artifact(&mut mandatory_role_artifacts, artifact.clone());
            }
        }
        let previous_context_generation = optional_u64(provider_state, "context_generation")?;
        let authoring_generation = optional_u64(provider_state, "authoring_generation")?;
        let generation = authoring_generation.checked_add(1).ok_or_else(|| {
            format!(
                "provider run '{}' authoring generation overflowed",
                self.config.run.as_str()
            )
        })?;
        let batch_prefix =
            checkpoint_prefix(&record.id, &first_output.identity.artifact.id, generation);
        let mut target_context = mandatory_role_artifacts.clone();
        for artifact in context_artifacts { append_artifact(&mut target_context, artifact.clone()); }
        let mut live_context = target_context.clone();
        if continuation.is_some() {
            live_context = mandatory_role_artifacts.clone();
            for artifact in current_context_artifacts(&record, &self.config)? { append_artifact(&mut live_context, artifact); }
        } else if target_context.iter().any(|id| outputs.iter().any(|output| &output.identity.artifact.id == id)) {
            live_context = mandatory_role_artifacts.clone();
            for artifact in current_context_artifacts(&record, &self.config)? { append_artifact(&mut live_context, artifact); }
            for artifact in context_artifacts.iter().filter(|id| !outputs.iter().any(|output| &output.identity.artifact.id == *id)) {
                append_artifact(&mut live_context, artifact.clone());
            }
        }
        let existing_bindings = current_context_bindings(&record, &self.config.checkpoints)?;
        let bind = |ids: &[ArtifactId]| ids.iter().map(|id| {
            serde_json::to_value(existing_bindings.iter().find(|binding| &binding.id == id).cloned()
                .unwrap_or_else(|| ArtifactBinding { id: id.clone(), access: Default::default() }))
                .map_err(|error| error.to_string())
        }).collect::<Result<Vec<_>, _>>();
        let artifact_bindings = bind(&live_context)?;
        let mandatory_role_bindings = bind(&mandatory_role_artifacts)?;
        let result_context = deferred_publication.then(|| target_context.iter().map(|id| {
            existing_bindings.iter().find(|binding| &binding.id == id).cloned()
                .unwrap_or_else(|| ArtifactBinding { id: id.clone(), access: Default::default() })
        }).collect::<Vec<_>>());
        let authoring_call = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)?;
        let authoring_result = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let authoring_result_checkpoint =
            CheckpointId::new(format!("{}_{}", batch_prefix, authoring_result.name));
        let mut next_frontier = Vec::with_capacity(3);
        let mut next_frontier_ids = Vec::with_capacity(3);
        let mut pending_items = Vec::with_capacity(outputs.len());
        for output in outputs {
            let output_prefix =
                checkpoint_prefix(&record.id, &output.identity.artifact.id, generation);
            let id = format!("{output_prefix}_{}", output.output_checkpoint.name);
            let checkpoint = pending_output_checkpoint(
                &output.output_checkpoint,
                &id,
                &output.identity.label,
                context_artifacts,
                &mandatory_role_artifacts,
            );
            next_frontier_ids.push(id);
            next_frontier.push(materialize_pending_checkpoint(&checkpoint, &record.id));
            pending_items.push(PendingHandoffItem {
                artifact_id: output.identity.artifact.id.clone(),
                artifact_content_hash: output.identity.artifact.content.to_hex(),
                artifact_label: output.identity.label.clone(),
                manifest_relative_path: output.identity.manifest_relative_path.clone(),
                checkpoint,
            });
        }
        for template in next_config.checkpoints.iter().filter(|template| retains_future_output(template)) {
            if !pending_items.iter().any(|item| pending_output_matches_template(item, template)) {
                let id = format!("{batch_prefix}_{}",template.name);
                next_frontier_ids.push(id.clone());
                next_frontier.push(materialize_checkpoint(template,&id,&record.id,&mandatory_role_bindings));
            }
        }
        let standing_role_artifacts = if self.config.writer_run.is_some() {
            current_mandatory_role_artifacts(&record, &self.config)?
        } else {
            mandatory_role_artifacts.clone()
        };
        for (id, checkpoint) in standing_routes::materialize_standing_routes(
            &next_config.checkpoints,
            &self.config.provider_interface,
            &self.standing_candidate_labels,
            &next_frontier,
            &batch_prefix,
            &record.id,
            &standing_role_artifacts,
        )? {
            next_frontier_ids.push(id);
            next_frontier.push(checkpoint);
        }
        for checkpoint in [authoring_call, authoring_result].into_iter().filter(|_| self.config.writer_run.is_none()) {
            let id = format!("{batch_prefix}_{}", checkpoint.name);
            next_frontier_ids.push(id.clone());
            next_frontier.push(materialize_checkpoint(
                checkpoint,
                &id,
                &record.id,
                &mandatory_role_bindings,
            ));
        }
        let mut value: serde_json::Value = serde_json::from_slice(current).map_err(|error| {
            format!(
                "provider run '{}' raw JSON cannot be preserved: {error}",
                record.id.as_str()
            )
        })?;
        let previous_value = value.clone();
        let root = value.as_object_mut().ok_or_else(|| {
            format!(
                "provider run '{}' JSON root is not an object",
                record.id.as_str()
            )
        })?;
        root.insert(
            "status".to_string(),
            serde_json::json!(if terminal { "idle" } else { "running" }),
        );
        replace_owned_frontier(root, &previous_frontier, next_frontier, &next_frontier_ids)?;
        if refresh_context {
            replace_context_checkpoints(
                root,
                &self.config,
                &record.id,
                &batch_prefix,
                &artifact_bindings,
            )?;
        }
        if let Some(template) = consumed_context_free_independent_input {
            replace_consumed_independent_input_checkpoint(
                root,
                template,
                &record.id,
                &batch_prefix,
                &mandatory_role_bindings,
            )?;
        }
        if self.config.checkpoints.iter().filter(|row|row.role == RunAuthoringCheckpointRole::VerifiedInput)
            .ne(next_config.checkpoints.iter().filter(|row|row.role == RunAuthoringCheckpointRole::VerifiedInput)) {
            let verified_context = bind(&target_context)?;
            replace_verified_input_checkpoints(
                root,
                &self.config,
                &next_config,
                &record.id,
                &batch_prefix,
                &verified_context,
            )?;
        }
        result_inputs::materialize(root, &record, &self.config, &next_config, &batch_prefix, consumed_result_input.as_ref(), result_context.as_deref())?;
        let interface_state = root
            .get_mut("interface_state")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| {
                format!(
                    "provider run '{}' interface_state is not an object",
                    record.id.as_str()
                )
            })?;
        let provider_state = interface_state
            .get_mut(self.config.provider_interface.as_str())
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| {
                format!(
                    "provider run '{}' interface_state for '{}' is not an object",
                    record.id.as_str(),
                    self.config.provider_interface.as_str()
                )
            })?;
        let context_generation = if (refresh_context || continuation.is_some()) && !self.failed_source {
            previous_context_generation.checked_add(context_generations).ok_or_else(|| {
                format!(
                    "provider run '{}' context generation overflowed",
                    self.config.run.as_str()
                )
            })?
        } else {
            previous_context_generation
        };
        provider_state.insert(
            "context_generation".to_string(),
            serde_json::json!(context_generation),
        );
        provider_state.insert(
            "authoring_generation".to_string(),
            serde_json::json!(generation),
        );
        provider_state.insert(
            "last_authored_artifact".to_string(),
            serde_json::json!(
                outputs
                    .last()
                    .map(|output| output.identity.artifact.id.clone())
                    .ok_or_else(|| "provider response batch became empty".to_string())?
            ),
        );
        let standing = standing_routes::distinct_standing(standing, &pending_items)?;
        let pending_batch = PendingHandoff {
            source_movement_seq: self.source_movement_seq,
            items: pending_items,
            terminal,
            context_publication: (live_context != target_context).then_some(target_context),
            deferred_publication,
            source_without_context: self.failed_source,
            settlement_of: None,
            continuation_of: continuation.map(source_continuation::last_output).transpose()?,
            dispatch_fenced: true,
            withdrawal: None,
            replacement: None,
            standing,
            rebase: Vec::new(),
        };
        provider_state.insert(
            "pending_handoff".to_string(),
            serde_json::to_value(&pending_batch)
                .map_err(|error| format!("provider pending handoff did not encode: {error}"))?,
        );
        provider_state.insert(
            RUN_AUTHORING_BINDING.to_string(),
            serde_json::to_value(&next_topology).map_err(|error| {
                format!("provider run authoring binding did not encode: {error}")
            })?,
        );
        provider_state.insert(
            FRONTIER_CHECKPOINT_IDS.to_string(),
            serde_json::json!(next_frontier_ids),
        );
        let bytes = JsonFormat::Compact.encode(&value)?;
        let authored_record = RunRecord::from_bytes(&bytes).map_err(|error| {
            format!(
                "provider authored malformed run '{}': {error}",
                self.config.run.as_str()
            )
        })?;
        let authored_pending = pending_handoff(&authored_record, &self.config.provider_interface)?
            .ok_or_else(|| "provider authored run lost its pending handoff batch".to_string())?;
        if authored_pending != pending_batch {
            return Err("provider authored run changed its pending handoff batch".to_string());
        }
        validate_pending_handoff_delivery_isolation(&authored_record, &authored_pending)?;
        validate_persisted_profile_binding(&authored_record, &next_config, &next_topology)?;
        let content = String::from_utf8(bytes)
            .map_err(|error| format!("provider authored non-UTF8 run JSON: {error}"))?;
        let before_hash = ContentHash::of_bytes(current).to_hex();
        let after_hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
        let patch = json_patch::diff(&previous_value, &value);
        if patch.is_empty() {
            return Err(format!(
                "provider run '{}' authoring produced no JSON changes",
                self.config.run.as_str()
            ));
        }
        let reconstructed = apply_run_json_patch(current, &patch, &after_hash, Some(JsonFormat::Compact))?;
        if reconstructed != content {
            return Err(format!(
                "provider run '{}' JSON patch did not reconstruct the exact authored bytes",
                self.config.run.as_str()
            ));
        }
        let mut authored = AuthoredRun {
            json_format: Some(JsonFormat::Compact),
            before_hash,
            after_hash,
            correlation_id: format!(
                "provider-run-authoring:{}:{}",
                self.config.run.as_str(),
                generation
            ),
            authoring_result_checkpoint,
            authoring_result_interface: authoring_result.interface().clone(),
            pending_handoff: pending_batch,
            content,
            patch,
        };
        self.seal_authored_run(current, &mut authored, None)?;
        Ok(authored)
    }

    pub(crate) fn seal_authored_run(
        &self,
        current: &[u8],
        authored: &mut AuthoredRun,
        consumed: Option<&[ArtifactRef]>,
    ) -> Result<(), String> {
        let before = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
        let mut value: serde_json::Value =
            serde_json::from_str(&authored.content).map_err(|error| error.to_string())?;
        let pending = result_inputs::seal_pending(&before, &mut value, &self.config)?;
        if consumed.is_some_and(|required| required != pending.rebase.as_slice()) {
            return Err("result consumption context differs from its recorded rebase".into());
        }
        if pending == authored.pending_handoff {
            return Ok(());
        }
        let format = authored.json_format.unwrap_or(JsonFormat::Compact);
        let content = String::from_utf8(format.encode(&value)?).map_err(|error| error.to_string())?;
        let after_hash = ContentHash::of_bytes(content.as_bytes()).to_hex();
        let patch = json_patch::diff(
            &serde_json::from_slice(current).map_err(|error| error.to_string())?,
            &value,
        );
        if apply_run_json_patch(current, &patch, &after_hash, Some(format))? != content {
            return Err("sealed provider write did not reconstruct its exact bytes".into());
        }
        authored.json_format = Some(format);
        authored.content = content;
        authored.after_hash = after_hash;
        authored.patch = patch;
        authored.pending_handoff = pending;
        Ok(())
    }

    pub(crate) fn materialize_file_write_call(
        &self,
        authored: &AuthoredRun,
    ) -> Result<InterfaceArtifactFile, String> {
        let payload = serde_json::to_vec(&serde_json::json!({
            "tool": self.config.file_write_tool,
            "arguments": FileWriteCallArguments {
                path: self.run_path.clone(), patch: authored.patch.clone(),
                expected_before_hash: authored.before_hash.clone(),
                expected_after_hash: authored.after_hash.clone(), json_format: authored.json_format,
            },
            "correlation_id": authored.correlation_id
        }))
        .map_err(|error| format!("provider file.write call did not encode: {error}"))?;
        InterfaceArtifactFile::write(&self.hand_in_root, &self.config.file_write_label, &payload)
            .map_err(|error| format!("provider file.write artifact could not be written: {error}"))
    }

    pub(crate) async fn send_authoring_manifest(
        &self,
        manifest_bytes: &[u8],
    ) -> Result<DeliveryVerdict, String> {
        send_authoring_manifest(&self.ctx, &self.config, manifest_bytes).await
    }

    pub(crate) async fn wait_for_write_result(
        &self,
        admitted_seq: Option<u64>,
        current_run_record: &[u8],
        authored: &AuthoredRun,
        deadline: Instant,
    ) -> Result<AuthoringWriteOutcome, String> {
        let mut reader = self.result_reader.lock().await;
        let current_record = RunRecord::from_bytes(current_run_record).map_err(|error| {
            format!(
                "provider run '{}' current authoring record is malformed: {error}",
                self.config.run.as_str()
            )
        })?;
        let authoring_result = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let failure_label = format!("{}.failure", authoring_result.interface().as_str());
        loop {
            if let Some(rejected) = authoring_retry::rejected_write(&self.ctx, &self.config, authored)? {
                return Ok(AuthoringWriteOutcome::Rejected(rejected));
            }
            if let Some((failure_seq, failure, _)) =
                self.recover_authoring_failure(&current_record, current_run_record)?
            {
                if admitted_seq.is_some_and(|admitted| failure.source_movement_seq != admitted) {
                    return Err(format!(
                        "provider run '{}' recovered authoring failure names movement {} instead of admitted call {}",
                        self.config.run.as_str(),
                        failure.source_movement_seq,
                        admitted_seq.unwrap_or_default()
                    ));
                }
                return Ok(AuthoringWriteOutcome::TerminalFailure {
                    failure_seq,
                    failure,
                });
            }
            if self
                .authoring_success_evidence(authored, admitted_seq)?
                .is_some()
            {
                return Ok(AuthoringWriteOutcome::Written);
            }
            let movements = reader.read_new()?;
            let mut outcome = None;
            for movement in &movements {
                if admitted_seq.is_some_and(|admitted| movement.movement.seq.0 < admitted) {
                    continue;
                }
                let envelope = reader.envelope(movement)?;
                let manifest = envelope.artifact_file.read_manifest().map_err(|error| {
                    format!(
                        "provider authoring result movement {} has malformed manifest: {error}",
                        movement.movement.seq.0
                    )
                })?;
                if manifest.label != self.config.file_write_result_label
                    && manifest.label != failure_label
                {
                    continue;
                }
                let payload = envelope.read_artifact_payload().map_err(|error| {
                    format!(
                        "provider authoring result movement {} payload cannot be read: {error}",
                        movement.movement.seq.0
                    )
                })?;
                if manifest.label == failure_label {
                    let failure = validate_authoring_failure_delivery(
                        &self.config.provider_interface,
                        &movement.movement,
                        &envelope,
                        &current_record,
                        &self.config,
                        authoring_result,
                        &payload,
                    )?;
                    if completed_authoring_failure_duplicate(
                        &self.ctx,
                        &self.session,
                        &current_record,
                        current_run_record,
                        &self.config,
                        &failure,
                        movement.movement.seq.0,
                    )? {
                        continue;
                    }
                    validate_authoring_failure_source(AuthoringFailureSourceValidation {
                        ctx: &self.ctx,
                        record: &current_record,
                        record_bytes: current_run_record,
                        config: &self.config,
                        failure: &failure,
                        failure_seq: movement.movement.seq.0,
                        after_seq: Some(self.source_movement_seq),
                    })?;
                    outcome = Some(AuthoringWriteOutcome::TerminalFailure {
                        failure_seq: movement.movement.seq.0,
                        failure,
                    });
                    break;
                }
                let result: FileWriteResultEnvelope =
                    serde_json::from_slice(&payload).map_err(|error| {
                        format!(
                            "provider authoring result movement {} payload is malformed: {error}",
                            movement.movement.seq.0
                        )
                    })?;
                if result.correlation_id.as_deref() != Some(&authored.correlation_id) { continue; }
                if !result.ok && result.error.as_ref().is_some_and(|error| error.details.is_none()) {
                    if let Some(rejected) = authoring_retry::rejected_write(&self.ctx, &self.config, authored)? {
                        outcome = Some(AuthoringWriteOutcome::Rejected(rejected));
                        break;
                    }
                    authoring_retry::validate_rejected_result(&self.ctx, &self.config, &movement.movement, &envelope, &current_record, &result)?;
                    continue;
                }
                if let Some(failed) =
                    file_write_failure_result(&result, &self.config, movement.movement.seq.0)?
                {
                    validate_authoring_failure_movement(
                        &self.config.provider_interface,
                        &movement.movement,
                        &envelope,
                        &current_record,
                        &self.config,
                        authoring_result,
                    )?;
                    if completed_authoring_failure_duplicate(
                        &self.ctx,
                        &self.session,
                        &current_record,
                        current_run_record,
                        &self.config,
                        &failed.failure,
                        movement.movement.seq.0,
                    )? {
                        continue;
                    }
                    let validated =
                        validate_authoring_failure_source(AuthoringFailureSourceValidation {
                            ctx: &self.ctx,
                            record: &current_record,
                            record_bytes: current_run_record,
                            config: &self.config,
                            failure: &failed.failure,
                            failure_seq: movement.movement.seq.0,
                            after_seq: Some(self.source_movement_seq),
                        })?;
                    if failed.correlation_id != validated.correlation_id {
                        return Err(format!(
                            "provider run '{}' failed authoring result names the wrong correlation_id",
                            self.config.run.as_str()
                        ));
                    }
                    outcome = Some(AuthoringWriteOutcome::TerminalFailure {
                        failure_seq: movement.movement.seq.0,
                        failure: failed.failure,
                    });
                    break;
                }
                if result.correlation_id.as_deref() != Some(&authored.correlation_id) {
                    continue;
                }
                self.validate_authoring_result_movement(&movement.movement, &envelope, authored)?;
                self.validate_write_result(result, authored)?;
                outcome = Some(AuthoringWriteOutcome::Written);
                break;
            }
            if !movements.is_empty() {
                reader.commit()?;
            }
            if let Some(outcome) = outcome {
                return Ok(outcome);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "provider run '{}' authoring result did not arrive within {}ms",
                    self.config.run.as_str(),
                    self.config.result_timeout_ms
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::timeout(remaining, reader.wait_for_change(&self.ctx.cancel))
                .await
                .map_err(|_| {
                    format!(
                        "provider run '{}' authoring result did not arrive within {}ms",
                        self.config.run.as_str(),
                        self.config.result_timeout_ms
                    )
                })??;
        }
    }

    pub(crate) fn validate_authoring_result_movement(
        &self,
        movement: &Movement,
        envelope: &InterfaceInvocationEnvelope,
        authored: &AuthoredRun,
    ) -> Result<(), String> {
        if movement.kind != MovementKind::InterfaceOutbound
            || movement.from != Endpoint::Interface(authored.authoring_result_interface.clone())
            || movement.to != Endpoint::Interface(self.config.provider_interface.clone())
            || movement.artifact.as_ref() != Some(&envelope.artifact)
        {
            return Err(format!(
                "provider run '{}' authoring result has invalid movement provenance",
                self.config.run.as_str()
            ));
        }
        let runtime = envelope.runtime().map_err(|error| error.to_string())?;
        if runtime.run != self.config.run
            || runtime.worker != self.config.worker
            || runtime.interface != self.config.provider_interface
            || runtime.checkpoint != authored.authoring_result_checkpoint
            || runtime.epoch != 0
        {
            return Err(format!(
                "provider run '{}' authoring result has invalid runtime coordinate",
                self.config.run.as_str()
            ));
        }
        let expected = self
            .config
            .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        let metadata = envelope.metadata().map_err(|error| error.to_string())?;
        let authored_record =
            RunRecord::from_bytes(authored.content.as_bytes()).map_err(|error| {
                format!(
                    "provider run '{}' authored result record is malformed: {error}",
                    self.config.run.as_str()
                )
            })?;
        let authored_checkpoint = authored_record
            .checkpoint(&authored.authoring_result_checkpoint)
            .ok_or_else(|| {
                format!(
                    "provider run '{}' authored result checkpoint is absent",
                    self.config.run.as_str()
                )
            })?;
        if !metadata_matches_checkpoint(metadata, expected)
            || !artifact_refs_match_bindings(
                &metadata.artifact_binding_refs,
                &authored_checkpoint.artifact_bindings,
            )
        {
            return Err(format!(
                "provider run '{}' authoring result metadata differs from its authored checkpoint",
                self.config.run.as_str()
            ));
        }
        let pinned = self.ctx.pinned_run_record_bytes(envelope)?;
        if pinned != authored.content.as_bytes() {
            if !self.config.checkpoints.iter().any(|cp| cp.owner_authoring.is_some()) {
                return Err("provider authoring result did not pin the exact authored run bytes".into());
            }
            validate_unchanged_admitted_checkpoint(authored.content.as_bytes(), &pinned,
                &authored.authoring_result_checkpoint)?;
        }
        Ok(())
    }

    pub(crate) fn validate_write_result(
        &self,
        result: FileWriteResultEnvelope,
        authored: &AuthoredRun,
    ) -> Result<(), String> {
        self.validate_write_result_evidence(result, authored)?;
        let actual = std::fs::read(&self.run_path).map_err(|error| {
            format!(
                "provider run '{}' authored file '{}' cannot be reread: {error}",
                self.config.run.as_str(),
                self.run_path.display()
            )
        })?;
        if actual != authored.content.as_bytes() {
            if !self.config.checkpoints.iter().any(|cp| cp.owner_authoring.is_some()) {
                return Err("provider file.write result does not match the bytes on disk".into());
            }
            validate_unchanged_admitted_checkpoint(authored.content.as_bytes(), &actual,
                &authored.authoring_result_checkpoint)?;
        }
        RunRecord::from_bytes(&actual).map_err(|error| {
            format!(
                "provider run '{}' authored file is malformed after write: {error}",
                self.config.run.as_str()
            )
        })?;
        Ok(())
    }

    pub(crate) fn validate_write_result_evidence(
        &self,
        result: FileWriteResultEnvelope,
        authored: &AuthoredRun,
    ) -> Result<(), String> {
        validate_write_result_evidence(&self.config, &self.run_path, result, authored)
    }
}
