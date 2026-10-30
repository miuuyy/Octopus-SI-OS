use super::*;

#[cfg(test)]
#[path = "../../tests/support/result_invocation.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResultInvocation {
    pub(crate) source: input_artifacts::InputArtifactBinding,
    pub(crate) artifact: ArtifactRef,
    pub(crate) manifest_relative_path: PathBuf,
}

impl ResultInvocation {
    pub(super) fn validate(&self) -> Result<(), String> {
        self.source.read_bindings(Vec::new())?;
        input_artifacts::InputArtifactBinding {
            artifact: self.artifact.clone(),
            source_movement_seq: self.source.source_movement_seq,
        }
        .read_bindings(Vec::new())?;
        if self.artifact == self.source.artifact
            || self.manifest_relative_path.as_os_str().is_empty()
            || self.manifest_relative_path.is_absolute()
            || self
                .manifest_relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(
                "result invocation requires a distinct exact carrier and safe private path".into(),
            );
        }
        Ok(())
    }
}

pub(crate) struct BridgeDefinition {
    pub(crate) invocation: ResultInvocation,
    template: RunAuthoringCheckpoint,
    checkpoint: octopus_run_record::CheckpointRecord,
    original: RunAuthoringCheckpoint,
}

fn profile(writer: &CheckedRunWriter) -> RunAuthoringProfile {
    RunAuthoringProfile {
        file_write_tool: writer.tool.clone(),
        file_write_label: writer.call_label.clone(),
        file_write_result_label: writer.result_label.clone(),
        result_timeout_ms: 1,
    }
}

fn read_binding(
    bindings: &mut Vec<ArtifactBinding>,
    reference: &ArtifactRef,
) -> Result<(), String> {
    if let Some(binding) = bindings.iter().find(|binding| binding.id == reference.id) {
        if binding.access != Default::default() {
            return Err("result invocation cannot replace existing artifact rights".into());
        }
    } else {
        bindings.push(ArtifactBinding {
            id: reference.id.clone(),
            access: Default::default(),
        });
    }
    Ok(())
}

fn bindings(
    record: &RunRecord,
    config: &RunAuthoringConfig,
    call: &Correspondence,
    source: &input_artifacts::InputArtifactBinding,
) -> Result<(Vec<ArtifactBinding>, Vec<ArtifactId>), String> {
    let mut context = effective_context_bindings(record, &config.checkpoints, &config.provider_interface)?;
    let evidence = result_static_bindings(config, Some(&context), &call.call_artifact);
    read_binding(&mut context, &call.call_artifact)?;
    Ok((source.read_bindings(context)?, evidence))
}

fn value(
    current: &[u8],
    writer: &CheckedRunWriter,
    selected: &CheckpointId,
    invocation: &ResultInvocation,
) -> Result<serde_json::Value, String> {
    invocation.validate()?;
    let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
    let topology = validate_declared_provider_writer(&record, writer)?;
    let config = topology.config(&profile(writer));
    let checkpoint = record
        .checkpoint(selected)
        .filter(|_| record.active_checkpoints.contains(selected))
        .ok_or("result invocation selected an absent receiving checkpoint")?;
    let original = topology
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && checkpoint_record_matches_template(checkpoint, template, &record.id)
        })
        .collect::<Vec<_>>();
    let [original] = original.as_slice() else {
        return Err("result invocation requires one exact receiving template".into());
    };
    let mut call = correspondence(original)?;
    let label = &original.required_verified_artifacts[0].artifact_label;
    if invocation.artifact.id.as_str()
        != format!("{label}:{}", invocation.artifact.content.to_hex())
        || invocation.source.artifact.id.as_str()
            != format!("{label}:{}", invocation.source.artifact.content.to_hex())
        || topology
            .checkpoints
            .iter()
            .filter(|template| template.role == RunAuthoringCheckpointRole::ResultInput)
            .map(correspondence)
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|call| {
                call.invocation
                    .as_ref()
                    .is_some_and(|previous| previous.source == invocation.source)
            })
    {
        return Err(
            "result invocation changed its result identity or repeated a bridged source".into(),
        );
    }
    let (context, call_evidence) = bindings(&record, &config, &call, &invocation.source)?;
    call.invocation = Some(invocation.clone());
    let mut template = (*original).clone();
    template.static_artifact_bindings = call_evidence;
    template.interface_binding.interface = config.authoring_source_interface.clone();
    template.required_verified_artifacts[0].params =
        serde_json::to_value(call).map_err(|error| error.to_string())?;
    template.name = derived_checkpoint_name(
        template.role,
        &template.interface_binding,
        &template.outbound_interface_bindings,
        template.include_context,
        &template.required_verified_artifacts,
    )?;
    let mut replacement = definition_value(current, writer, std::slice::from_ref(selected), None)?;
    let state = &mut replacement["interface_state"][writer.owner.as_str()];
    let mut next: BoundRunAuthoringTopology =
        serde_json::from_value(state[RUN_AUTHORING_BINDING].clone())
            .map_err(|error| error.to_string())?;
    if next
        .checkpoints
        .iter()
        .any(|existing| existing.name == template.name)
    {
        return Err("result invocation reused an existing template".into());
    }
    let generation = state["authoring_generation"]
        .as_u64()
        .ok_or("result invocation has no generation")?;
    let id = format!(
        "cp_result_invocation_{}_{}_{}",
        record.id, generation, template.name
    );
    if record.checkpoint(&CheckpointId::new(&id)).is_some() {
        return Err("result invocation reused a checkpoint identity".into());
    }
    next.checkpoints.push(template.clone());
    state[RUN_AUTHORING_BINDING] =
        serde_json::to_value(&next).map_err(|error| error.to_string())?;
    let context = context
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    replace_owned_frontier(
        replacement
            .as_object_mut()
            .ok_or("result invocation is not an object")?,
        &BTreeSet::new(),
        vec![materialize_checkpoint(&template, &id, &record.id, &context)],
        &[id],
    )?;
    let after = RunRecord::from_bytes(
        &serde_json::to_vec(&replacement).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let next_config = next.config(&profile(writer));
    next_config.validate(&writer.owner, &record.id)?;
    validate_bound_run_authoring_topology(&after, &next_config, None)?;
    Ok(replacement)
}

pub(super) fn definition(
    before: &[u8],
    after: &[u8],
    writer: &CheckedRunWriter,
) -> Result<Option<BridgeDefinition>, String> {
    let old = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let next = RunRecord::from_bytes(after).map_err(|error| error.to_string())?;
    let read = |record: &RunRecord| -> Result<Option<BoundRunAuthoringTopology>, String> {
        record
            .interface_state
            .get(&writer.owner)
            .and_then(|state| state.get(RUN_AUTHORING_BINDING))
            .map(|value| serde_json::from_value(value.clone()).map_err(|error| error.to_string()))
            .transpose()
    };
    let (Some(old_topology), Some(next_topology)) = (read(&old)?, read(&next)?) else {
        return Ok(None);
    };
    let added = next_topology
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && !old_topology.checkpoints.contains(template)
        })
        .map(|template| Ok((template, correspondence(template)?)))
        .collect::<Result<Vec<_>, String>>()?;
    if !added.iter().any(|(_, call)| call.invocation.is_some()) {
        return Ok(None);
    }
    let [(template, call)] = added.as_slice() else {
        return Err("result invocation must add exactly one checked input".into());
    };
    let invocation = call
        .invocation
        .clone()
        .ok_or("result invocation lost its source")?;
    let removed = old_topology
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && !next_topology.checkpoints.contains(template)
        })
        .collect::<Vec<_>>();
    let [original] = removed.as_slice() else {
        return Err("result invocation must replace exactly one receiving input".into());
    };
    let selected = old
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            old.active_checkpoints.contains(&checkpoint.id)
                && checkpoint_record_matches_template(checkpoint, original, &old.id)
        })
        .collect::<Vec<_>>();
    let [selected] = selected.as_slice() else {
        return Err("result invocation lost its original receiving row".into());
    };
    let expected = value(before, writer, &selected.id, &invocation)?;
    if expected
        != serde_json::from_slice::<serde_json::Value>(after).map_err(|error| error.to_string())?
    {
        return Err(
            "result invocation changed pending work, future context or a neighboring contract"
                .into(),
        );
    }
    let rows = next
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            next.active_checkpoints.contains(&checkpoint.id)
                && checkpoint_record_matches_template(checkpoint, template, &next.id)
        })
        .collect::<Vec<_>>();
    let [checkpoint] = rows.as_slice() else {
        return Err("result invocation has no unique installed row".into());
    };
    let checkpoint = (*checkpoint).clone();
    Ok(Some(BridgeDefinition {
        invocation,
        template: (*template).clone(),
        checkpoint,
        original: (*original).clone(),
    }))
}

pub(crate) fn definition_source(
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<Option<u64>, String> {
    if !config
        .checkpoints
        .iter()
        .any(|template| template.role == RunAuthoringCheckpointRole::ResultInput)
    {
        return Ok(None);
    }
    let writer = declared_provider_run_writer(
        before,
        &config.provider_interface,
        &RunAuthoringProfile {
            file_write_tool: config.file_write_tool.clone(),
            file_write_label: config.file_write_label.clone(),
            file_write_result_label: config.file_write_result_label.clone(),
            result_timeout_ms: config.result_timeout_ms,
        },
    )?;
    Ok(definition(
        &serde_json::to_vec(before).map_err(|error| error.to_string())?,
        &serde_json::to_vec(after).map_err(|error| error.to_string())?,
        &writer,
    )?
    .map(|bridge| bridge.invocation.source.source_movement_seq))
}

pub(crate) fn rebased_value(
    before: &[u8],
    current: &[u8],
    after: &[u8],
    config: &RunAuthoringConfig,
) -> Result<Option<serde_json::Value>, String> {
    if !config.checkpoints.iter().any(|template| template.role == RunAuthoringCheckpointRole::ResultInput) {
        return Ok(None);
    }
    let original = RunRecord::from_bytes(before).map_err(|error| error.to_string())?;
    let writer = declared_provider_run_writer(&original, &config.provider_interface, &RunAuthoringProfile {
        file_write_tool: config.file_write_tool.clone(),
        file_write_label: config.file_write_label.clone(),
        file_write_result_label: config.file_write_result_label.clone(),
        result_timeout_ms: config.result_timeout_ms,
    })?;
    let Some(retained) = definition(before, after, &writer)? else { return Ok(None); };
    let selected = original.checkpoints.iter().filter(|checkpoint| {
        original.active_checkpoints.contains(&checkpoint.id)
            && checkpoint_record_matches_template(checkpoint, &retained.original, &original.id)
    }).collect::<Vec<_>>();
    let [selected] = selected.as_slice() else {
        return Err("result invocation retry lost its exact original receiving row".into());
    };
    let next = value(current, &writer, &selected.id, &retained.invocation)?;
    let rebuilt = definition(current, &serde_json::to_vec(&next).map_err(|error| error.to_string())?, &writer)?
        .ok_or("result invocation retry lost its checked definition")?;
    if rebuilt.invocation != retained.invocation || rebuilt.original != retained.original
        || rebuilt.template != retained.template || rebuilt.checkpoint != retained.checkpoint
    {
        return Err("result invocation retry changed its admitted identity, context or provenance".into());
    }
    Ok(Some(next))
}

fn validate_source(
    reader: &mut impl CheckedRunEvidenceReader,
    before: &RunRecord,
    writer: &CheckedRunWriter,
    bridge: &BridgeDefinition,
    before_seq: Option<u64>,
) -> Result<(), String> {
    let config = validate_declared_provider_writer(before, writer)?.config(&profile(writer));
    validate_result_source(
        reader,
        before,
        &config,
        &bridge.original,
        &bridge.invocation.source,
        before_seq,
    )?;
    let (_, original) = reader.artifact(&bridge.invocation.source.artifact)?;
    let copied = reader.pending_output_payload(
        &writer.owner,
        &bridge.invocation.artifact,
        &bridge.invocation.manifest_relative_path,
    )?;
    if original != copied {
        return Err(
            "result invocation carrier differs from the exact admitted result bytes".into(),
        );
    }
    Ok(())
}

pub(super) fn validate_checked(
    reader: &mut impl CheckedRunEvidenceReader,
    transition: &CheckedRunTransition,
) -> Result<(), String> {
    if let Some(bridge) = definition(&transition.before, &transition.after, &transition.writer)? {
        let before =
            RunRecord::from_bytes(&transition.before).map_err(|error| error.to_string())?;
        validate_source(
            reader,
            &before,
            &transition.writer,
            &bridge,
            Some(transition.call.seq.0),
        )?;
    }
    Ok(())
}

pub(super) fn validate_saved(
    ctx: &InterfaceLiftContext,
    before: &RunRecord,
    after: &RunRecord,
    config: &RunAuthoringConfig,
) -> Result<(), String> {
    let writer = declared_provider_run_writer(
        before,
        &config.provider_interface,
        &RunAuthoringProfile {
            file_write_tool: config.file_write_tool.clone(),
            file_write_label: config.file_write_label.clone(),
            file_write_result_label: config.file_write_result_label.clone(),
            result_timeout_ms: config.result_timeout_ms,
        },
    )?;
    let Some(bridge) = definition(
        &serde_json::to_vec(before).map_err(|error| error.to_string())?,
        &serde_json::to_vec(after).map_err(|error| error.to_string())?,
        &writer,
    )?
    else {
        return Ok(());
    };
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence {
        movements_path: &ctx.movements_path,
        ledger_index_path: &ctx.ledger_index_path,
        blobs_dir: &ctx.blobs_dir,
        max_control_frame_bytes: limits.max_control_frame_bytes,
        max_manifest_bytes: limits.max_manifest_bytes,
        max_payload_bytes: limits.max_payload_bytes,
    };
    validate_source(
        &mut FileCheckedRunReader::open(&evidence, None)?.with_provider_context(ctx),
        before,
        &writer,
        &bridge,
        None,
    )
}

pub(crate) fn source_was_bridged(
    chain: &[CheckedRunTransition],
    source: u64,
) -> Result<Option<BridgeDefinition>, String> {
    let mut found = None;
    for transition in chain {
        if let Some(bridge) = definition(&transition.before, &transition.after, &transition.writer)?
        {
            if bridge.invocation.source.source_movement_seq == source {
                if found.replace(bridge).is_some() {
                    return Err("result source has repeated checked invocation bridges".into());
                }
            }
        }
    }
    Ok(found)
}

pub(crate) fn bound_source(
    record: &RunRecord,
    provider: &InterfaceId,
    checkpoint: &CheckpointId,
) -> Result<Option<ArtifactRef>, String> {
    let Some(topology) = record
        .interface_state
        .get(provider)
        .and_then(|state| state.get(RUN_AUTHORING_BINDING))
        .map(|value| {
            serde_json::from_value::<BoundRunAuthoringTopology>(value.clone())
                .map_err(|error| error.to_string())
        })
        .transpose()?
    else {
        return Ok(None);
    };
    let Some(checkpoint) = record.checkpoint(checkpoint) else {
        return Ok(None);
    };
    let matching = topology
        .checkpoints
        .iter()
        .filter(|template| {
            template.role == RunAuthoringCheckpointRole::ResultInput
                && checkpoint_record_matches_template(checkpoint, template, &record.id)
        })
        .collect::<Vec<_>>();
    match matching.as_slice() {
        [] => Ok(None),
        [template] => Ok(correspondence(template)?
            .invocation
            .map(|invocation| invocation.source.artifact)),
        _ => Err("result invocation repeats its exact template".into()),
    }
}

impl ProviderOutputHandoff {
    pub(crate) fn result_input_template(&self) -> Result<Option<RunAuthoringCheckpoint>, String> {
        if self.failed_source || !enabled(&self.config) {
            return Ok(None);
        }
        let Some(runtime) = self.invocation_metadata.runtime.as_ref() else {
            return Ok(None);
        };
        let hash = runtime
            .run_record_hash
            .as_deref()
            .and_then(ContentHash::from_hex)
            .ok_or("result source has no pinned invocation snapshot")?;
        let (_, record) = pinned_run_record(&self.ctx, &hash, "result invocation")?;
        let config = attempt_config(&record, &self.config)?;
        let checkpoint = record
            .checkpoint(&runtime.checkpoint)
            .ok_or("result source checkpoint is absent")?;
        let templates = config
            .checkpoints
            .iter()
            .filter(|template| {
                template.role == RunAuthoringCheckpointRole::ResultInput
                    && checkpoint_record_matches_template(checkpoint, template, &record.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        match templates.as_slice() {
            [] => Ok(None),
            [template] => Ok(Some(template.clone())),
            _ => Err("result source has multiple receiving templates".into()),
        }
    }

    pub(crate) fn author_result_invocation(&self, current: &[u8]) -> Result<AuthoredRun, String> {
        let record = RunRecord::from_bytes(current).map_err(|error| error.to_string())?;
        let runtime = self
            .invocation_metadata
            .runtime
            .as_ref()
            .ok_or("result invocation source coordinate absent")?;
        let (_, source) = self.session.index()?.exact_provider_source_outbound(
            &record.id,
            &record.worker,
            &self.config.provider_interface,
            self.source_movement_seq,
        )?;
        let (manifest, payload) = read_pinned_artifact_from_cache(
            &self.ctx.blobs_dir,
            self.ctx.required_host_io_limits()?,
            &source.artifact,
        )?;
        let carrier = InterfaceArtifactFile::write(
            self.hand_in_root
                .join("result-inputs")
                .join(self.source_movement_seq.to_string()),
            &manifest.label,
            &payload,
        )
        .map_err(|error| error.to_string())?;
        let file = InterfaceArtifactFile::store_manifest(
            self.var_dir
                .join("result-inputs")
                .join(self.source_movement_seq.to_string()),
            &std::fs::read(carrier.manifest_path()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let identity = artifact_identity(&file, &self.var_dir)?;
        let invocation = ResultInvocation {
            source: input_artifacts::InputArtifactBinding {
                artifact: source.artifact,
                source_movement_seq: self.source_movement_seq,
            },
            artifact: identity.artifact,
            manifest_relative_path: identity.manifest_relative_path,
        };
        let writer = declared_provider_run_writer(
            &record,
            &self.config.provider_interface,
            &RunAuthoringProfile {
                file_write_tool: self.config.file_write_tool.clone(),
                file_write_label: self.config.file_write_label.clone(),
                file_write_result_label: self.config.file_write_result_label.clone(),
                result_timeout_ms: self.config.result_timeout_ms,
            },
        )?;
        let prospective = value(current, &writer, &runtime.checkpoint, &invocation)?;
        let generation =
            prospective["interface_state"][writer.owner.as_str()]["authoring_generation"]
                .as_u64()
                .ok_or("result invocation generation absent")?;
        let edit = CheckedRunEdit::prepare(
            self.run_path.clone(),
            current,
            prospective,
            format!("provider-run-authoring:{}:{generation}", record.id),
        )?;
        let after =
            RunRecord::from_bytes(edit.content.as_bytes()).map_err(|error| error.to_string())?;
        validate_saved(&self.ctx, &record, &after, &self.config)?;
        let config = attempt_config(&after, &self.config)?;
        Ok(AuthoredRun {
            json_format: edit.json_format,
            content: edit.content,
            patch: edit.patch,
            before_hash: edit.before_hash,
            after_hash: edit.after_hash,
            correlation_id: edit.correlation_id,
            authoring_result_checkpoint: if config.writer_run.is_some() {
                CheckpointId::new(format!("cp_result_invocation_writer_{generation}"))
            } else {
                active_checkpoint_for_role(
                    &after,
                    &config,
                    RunAuthoringCheckpointRole::AuthoringResult,
                )?
                .id
                .clone()
            },
            authoring_result_interface: writer.door,
            pending_handoff: persisted_pending_handoff(&after, &writer.owner)?
                .ok_or("result invocation lost its pending history")?,
        })
    }

    fn admitted_result_invocation(&self, bridge: &BridgeDefinition) -> Result<Option<u64>, String> {
        let coordinate = RuntimeCoordinateKey::new(
            &self.config.run,
            &self.config.worker,
            &self.config.provider_interface,
            &bridge.checkpoint.id,
            bridge.checkpoint.epoch.0,
        );
        let Some(output) = self.session.index()?.unique_outbound_artifact(
            &coordinate,
            &self.config.authoring_source_interface,
            Some(bridge.invocation.source.source_movement_seq),
        )?
        else {
            return Ok(None);
        };
        if output.artifact != bridge.invocation.artifact
            || !indexed_evidence_matches_checkpoint(&output, &bridge.template)
            || !artifact_refs_match_bindings(
                &output.artifact_binding_refs,
                &bridge.checkpoint.artifact_bindings,
            )
        {
            return Err("result invocation has different admitted artifact or bindings".into());
        }
        let limits = self.ctx.required_host_io_limits()?;
        let evidence = CheckedRunEvidence {
            movements_path: &self.ctx.movements_path,
            ledger_index_path: &self.ctx.ledger_index_path,
            blobs_dir: &self.ctx.blobs_dir,
            max_control_frame_bytes: limits.max_control_frame_bytes,
            max_manifest_bytes: limits.max_manifest_bytes,
            max_payload_bytes: limits.max_payload_bytes,
        };
        let record = indexed_source_run_record(&self.ctx, &output, output.seq)?;
        let config = attempt_config(&record, &self.config)?;
        validate_result_source(
            &mut FileCheckedRunReader::open(&evidence, None)?,
            &record,
            &config,
            &bridge.template,
            &input_artifacts::InputArtifactBinding {
                artifact: output.artifact,
                source_movement_seq: output.seq,
            },
            None,
        )?;
        Ok(Some(output.seq))
    }

    async fn hand_result_invocation(
        &self,
        state: &mut HandoffState,
        bridge: &BridgeDefinition,
    ) -> Result<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>), String> {
        let sequence = match self.admitted_result_invocation(bridge)? {
            Some(sequence) => sequence,
            None => {
                self.reconcile_checked_run_state(state).await?;
                let current = RunRecord::from_bytes(&state.run_record_bytes)
                    .map_err(|error| error.to_string())?;
                if current.checkpoint(&bridge.checkpoint.id) != Some(&bridge.checkpoint)
                    || !current.active_checkpoints.contains(&bridge.checkpoint.id)
                {
                    return Err(
                        "result invocation was intentionally removed before its new admission"
                            .into(),
                    );
                }
                let file = InterfaceArtifactFile::new(
                    self.var_dir.join(&bridge.invocation.manifest_relative_path),
                )
                .map_err(|error| error.to_string())?;
                if artifact_identity(&file, &self.var_dir)?.artifact != bridge.invocation.artifact {
                    return Err("result invocation carrier changed before hand-in".into());
                }
                let manifest =
                    std::fs::read(file.manifest_path()).map_err(|error| error.to_string())?;
                let verdict = self.send_authoring_manifest(&manifest).await?;
                match verdict {
                    DeliveryVerdict::Admitted { .. } => {}
                    DeliveryVerdict::Refused { reason, .. }
                        if reason == octopus_interfaces::RefusedReason::CheckpointSpent => {}
                    DeliveryVerdict::Refused { reason, detail } => {
                        return Err(format!(
                            "result invocation admission refused as '{}': {detail}",
                            reason.as_str()
                        ));
                    }
                }
                await_checked_evidence(&self.ctx, self.config.result_timeout_ms, || {
                    self.admitted_result_invocation(bridge)?.ok_or_else(|| {
                        "result invocation awaits its exact admitted provider movement".into()
                    })
                })
                .await?
            }
        };
        state.consumed_input = false;
        state.consumed_verified_input = false;
        Ok((
            ProviderSourceRecovery::Bridged {
                invocation_movement_seq: sequence,
            },
            vec![ProviderHandoffOutcome::Admitted {
                seq: sequence,
                artifact: bridge.invocation.artifact.clone(),
            }],
        ))
    }

    pub(crate) async fn bridge_result_input(
        &self,
        state: &mut HandoffState,
    ) -> Result<Option<(ProviderSourceRecovery, Vec<ProviderHandoffOutcome>)>, String> {
        let Some(original) = self.result_input_template()? else {
            return Ok(None);
        };
        self.reconcile_checked_run_state(state).await?;
        let mut current =
            RunRecord::from_bytes(&state.run_record_bytes).map_err(|error| error.to_string())?;
        if let Some(bridge) = source_was_bridged(
            &self.completed_source_chain(&state.run_record_bytes)?,
            self.source_movement_seq,
        )? {
            return self.hand_result_invocation(state, &bridge).await.map(Some);
        }
        let runtime = self
            .invocation_metadata
            .runtime
            .as_ref()
            .ok_or("result source coordinate absent")?;
        let Some(checkpoint) = current
            .checkpoint(&runtime.checkpoint)
            .filter(|_| current.active_checkpoints.contains(&runtime.checkpoint))
        else {
            return Ok(None);
        };
        if !checkpoint_record_matches_template(checkpoint, &original, &current.id) {
            return Err("result invocation no longer has its admitted receiving contract".into());
        }
        let awaited = correspondence(&original)?;
        if awaited.invocation.is_none()
            && persisted_pending_handoff(&current, &self.config.provider_interface)?.is_some_and(|pending| {
                pending.deferred_publication
                    && pending.items.iter().any(|item| item.artifact_id == awaited.call_artifact.id)
            })
        {
            return Ok(None);
        }
        let config = attempt_config(&current, &self.config)?;
        let mut active = self.clone();
        active.config = config;
        active.bound_topology.checkpoints = active.config.checkpoints.clone();
        let attempt = active.recover_authoring_attempt(&current, &state.run_record_bytes)?;
        let attempt = match attempt {
            AuthoringAttemptRecovery::OtherSource(owner) => {
                let pending =
                    persisted_pending_handoff(&current, &active.config.provider_interface)?
                        .ok_or("result invocation found an unrelated private authoring attempt")?;
                let mut publisher = active.clone();
                publisher.source_movement_seq = owner;
                let AuthoringAttemptRecovery::InFlight(attempt) =
                    publisher.recover_authoring_attempt(&current, &state.run_record_bytes)?
                else {
                    return Err(
                        "result invocation cannot attribute the prior authoring attempt".into(),
                    );
                };
                if !context_publication::is_publication_completion(
                    &pending,
                    &attempt.authored.pending_handoff,
                ) || publisher
                    .pending_handoff_evidence(&pending)?
                    .iter()
                    .any(Option::is_none)
                {
                    return Err(
                        "result invocation awaits the prior source's checked authoring completion"
                            .into(),
                    );
                }
                if !matches!(
                    publisher
                        .install_authored_run(state, &attempt.authored)
                        .await?,
                    AuthoredRunInstallOutcome::Installed
                ) {
                    return Err("prior context publication failed before result invocation".into());
                }
                current = RunRecord::from_bytes(&state.run_record_bytes)
                    .map_err(|error| error.to_string())?;
                active.config = attempt_config(&current, &active.config)?;
                AuthoringAttemptRecovery::None
            }
            other => other,
        };
        let authored = match attempt {
            AuthoringAttemptRecovery::InFlight(attempt) => {
                let before = active.authored_run_preimage(&state.run_record_bytes, &attempt.authored)?;
                let source = definition_source(
                    &RunRecord::from_bytes(&before).map_err(|error| error.to_string())?,
                    &RunRecord::from_bytes(attempt.authored.content.as_bytes())
                        .map_err(|error| error.to_string())?,
                    &active.config,
                )?;
                if source != Some(self.source_movement_seq) {
                    return Ok(None);
                }
                attempt.authored
            }
            AuthoringAttemptRecovery::None => {
                if let Some(pending) =
                    persisted_pending_handoff(&current, &active.config.provider_interface)?
                {
                    if active
                        .pending_handoff_evidence(&pending)?
                        .iter()
                        .any(Option::is_none)
                    {
                        source_withdrawal::require_validated_output(state, &pending)?;
                        let mut publisher = active.clone();
                        publisher.source_movement_seq = pending.source_movement_seq;
                        publisher
                            .hand_pending_items(&pending, &state.run_record_bytes)
                            .await?;
                    }
                    if pending.context_publication.is_some() {
                        let mut publisher = active.clone();
                        publisher.source_movement_seq = pending.source_movement_seq;
                        publisher.publish_admitted_context(state).await?;
                        current = RunRecord::from_bytes(&state.run_record_bytes)
                            .map_err(|error| error.to_string())?;
                        active.config = attempt_config(&current, &active.config)?;
                    }
                }
                let call = correspondence(&original)?;
                if let Some(invocation) = &call.invocation {
                    let checkpoint = current
                        .checkpoint(&runtime.checkpoint)
                        .ok_or("result invocation lost its receiving row")?;
                    let (bindings, call_evidence) = bindings(&current, &active.config, &call, &invocation.source)?;
                    if checkpoint.artifact_bindings == bindings
                        && original.static_artifact_bindings == call_evidence
                    {
                        return Ok(Some((ProviderSourceRecovery::Process, Vec::new())));
                    }
                }
                active.author_result_invocation(&state.run_record_bytes)?
            }
            AuthoringAttemptRecovery::OtherSource(_) => {
                return Err("result invocation lost its private attempt owner".into());
            }
        };
        let writer = declared_provider_run_writer(
            &current,
            &active.config.provider_interface,
            &RunAuthoringProfile {
                file_write_tool: active.config.file_write_tool.clone(),
                file_write_label: active.config.file_write_label.clone(),
                file_write_result_label: active.config.file_write_result_label.clone(),
                result_timeout_ms: active.config.result_timeout_ms,
            },
        )?;
        let bridge = definition(
            &active.authored_run_preimage(&state.run_record_bytes, &authored)?,
            authored.content.as_bytes(),
            &writer,
        )?
        .ok_or("result invocation lost its exact checked definition")?;
        match active.install_authored_run(state, &authored).await? {
            AuthoredRunInstallOutcome::Installed => active
                .hand_result_invocation(state, &bridge)
                .await
                .map(Some),
            AuthoredRunInstallOutcome::TerminalFailure(_) => {
                Err("result invocation file effect failed".into())
            }
        }
    }
}
