use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingHandoffItem {
    pub(crate) artifact_id: ArtifactId,
    pub(crate) artifact_content_hash: String,
    pub(crate) artifact_label: String,
    pub(crate) manifest_relative_path: PathBuf,
    pub(crate) checkpoint: PendingHandoffCheckpoint,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingHandoffCheckpoint {
    pub(crate) id: CheckpointId,
    pub(crate) epoch: u64,
    pub(crate) interface_binding: AuthoredInterfaceBinding,
    pub(crate) outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    pub(crate) artifact_bindings: Vec<ArtifactId>,
    pub(crate) required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct HandoffArtifactIdentity {
    pub(crate) artifact: ArtifactRef,
    pub(crate) label: String,
    pub(crate) manifest_relative_path: PathBuf,
}

#[derive(Clone, Copy)]
pub(crate) struct PendingHandoffLedgerCoordinate<'a> {
    pub(crate) interface: &'a InterfaceId,
    pub(crate) run: &'a RunId,
    pub(crate) worker: &'a InterfaceId,
    pub(crate) artifact: &'a ArtifactRef,
    pub(crate) required_evidence: Option<(&'a str, &'a str)>,
    pub(crate) checkpoint: &'a CheckpointId,
    pub(crate) epoch: u64,
}

pub(crate) struct PreparedHandoff {
    pub(crate) identity: HandoffArtifactIdentity,
    pub(crate) output_checkpoint: RunAuthoringCheckpoint,
    pub(crate) verified_inputs: Vec<RunAuthoringCheckpoint>,
}

pub(crate) fn split_handoff_batches_by_artifact_name(
    emissions: Vec<ProviderHandoffEmission>,
    session_snapshot_labels: &BTreeSet<String>,
) -> Result<Vec<Vec<ProviderHandoffEmission>>, String> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut labels = BTreeSet::new();
    for emission in emissions {
        let manifest = emission.artifact.read_manifest().map_err(|error| {
            format!("provider output artifact manifest cannot be read: {error}")
        })?;
        let isolated = session_snapshot_labels.contains(&manifest.label)
            || matches!(
                &emission.route,
                ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { .. }
            );
        if !batch.is_empty() && (isolated || labels.contains(&manifest.label)) {
            batches.push(std::mem::take(&mut batch));
            labels.clear();
        }
        if isolated {
            batches.push(vec![emission]);
        } else {
            labels.insert(manifest.label);
            batch.push(emission);
        }
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    Ok(batches)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunAuthoringConfig {
    pub(crate) run: RunId,
    pub(crate) worker: InterfaceId,
    pub(crate) provider_interface: InterfaceId,
    pub(crate) authoring_source_interface: InterfaceId,
    pub(crate) file_write_tool: String,
    pub(crate) file_write_label: String,
    pub(crate) file_write_result_label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) payload_artifact_bindings: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) writer_run: Option<writer_run::WriterRunBinding>,
    pub(crate) checkpoints: Vec<RunAuthoringCheckpoint>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunAuthoringProfile {
    pub file_write_tool: String,
    pub file_write_label: String,
    pub file_write_result_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoundRunAuthoringTopology {
    pub(crate) schema_version: u32,
    pub(crate) run: RunId,
    pub(crate) worker: InterfaceId,
    pub(crate) provider_interface: InterfaceId,
    pub(crate) authoring_source_interface: InterfaceId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) payload_artifact_bindings: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) writer_run: Option<writer_run::WriterRunBinding>,
    pub(crate) checkpoints: Vec<RunAuthoringCheckpoint>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunAuthoringCheckpoint {
    pub(crate) name: String,
    pub(crate) role: RunAuthoringCheckpointRole,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) result_receiving: BTreeMap<String, RequiredVerifiedArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) static_artifact_bindings: Vec<ArtifactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) owner_authoring: Option<CheckedRunWriter>,
    pub(crate) interface_binding: AuthoredInterfaceBinding,
    pub(crate) outbound_interface_bindings: Vec<AuthoredInterfaceBinding>,
    pub(crate) include_context: bool,
    pub(crate) required_verified_artifacts: Vec<RequiredVerifiedArtifact>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RunAuthoringCheckpointRole {
    Stable,
    IndependentInput,
    OwnerInput,
    VerifiedInput,
    ResultInput,
    ProviderArtifactOutput,
    ProviderInterfaceOutput,
    ProviderToolOutput,
    ProviderTransportDispatch,
    ProviderHandoffCompletion,
    AuthoringCall,
    AuthoringResult,
}

impl RunAuthoringCheckpointRole {
    pub(crate) fn is_provider_input(self) -> bool {
        matches!(
            self,
            Self::Stable | Self::IndependentInput | Self::OwnerInput | Self::VerifiedInput | Self::ResultInput
        )
    }
}

impl RunAuthoringCheckpoint {
    pub(crate) fn interface(&self) -> &InterfaceId {
        &self.interface_binding.interface
    }

    pub(crate) fn outbound_interfaces(&self) -> impl Iterator<Item = &InterfaceId> {
        self.outbound_interface_bindings
            .iter()
            .map(|binding| &binding.interface)
    }

    pub(crate) fn binding_policy_snapshot(&self) -> CheckpointBindingPolicySnapshot {
        CheckpointBindingPolicySnapshot {
            inbound: self.interface_binding.clone(),
            outbound: self.outbound_interface_bindings.clone(),
        }
    }
}

pub(crate) fn metadata_matches_checkpoint(
    metadata: &InvocationMetadata,
    checkpoint: &RunAuthoringCheckpoint,
) -> bool {
    metadata.outbound_interface_bindings
        == checkpoint
            .outbound_interfaces()
            .cloned()
            .collect::<Vec<_>>()
        && metadata.checkpoint_binding_policy.as_ref()
            == Some(&checkpoint.binding_policy_snapshot())
        && metadata.required_verified_artifacts == checkpoint.required_verified_artifacts
}

pub(crate) fn indexed_evidence_matches_checkpoint(
    evidence: &IndexedOutboundArtifact,
    checkpoint: &RunAuthoringCheckpoint,
) -> bool {
    evidence.outbound_interface_bindings
        == checkpoint
            .outbound_interfaces()
            .cloned()
            .collect::<Vec<_>>()
        && evidence.checkpoint_binding_policy.as_ref()
            == Some(&checkpoint.binding_policy_snapshot())
        && evidence.required_verified_artifacts == checkpoint.required_verified_artifacts
}

pub(crate) fn indexed_source_run_record(
    ctx: &InterfaceLiftContext,
    evidence: &IndexedOutboundArtifact,
    movement_seq: u64,
) -> Result<RunRecord, String> {
    let expected = evidence
        .run_record_hash
        .as_deref()
        .and_then(ContentHash::from_hex)
        .ok_or_else(|| {
            format!(
                "provider source movement {movement_seq} carries no valid pinned run-record hash"
            )
        })?;
    pinned_run_record(
        ctx,
        &expected,
        &format!("provider source movement {movement_seq}"),
    )
    .map(|(_, record)| record)
}

pub(crate) fn pinned_run_record(
    ctx: &InterfaceLiftContext,
    expected: &ContentHash,
    surface: &str,
) -> Result<(Vec<u8>, RunRecord), String> {
    pinned_run_record_from_cache(&ctx.blobs_dir, ctx.required_host_io_limits()?, expected, surface)
}

pub(crate) fn pinned_run_record_from_cache(
    blobs_dir: &Path, limits: InterfaceHostIoLimits, expected: &ContentHash, surface: &str,
) -> Result<(Vec<u8>, RunRecord), String> {
    let path = blobs_dir.join(expected.to_hex());
    let bytes =
        octopus_interfaces::read_artifact_carrier_file_bounded(&path, limits.max_payload_bytes)
            .map_err(|error| {
                format!(
                    "{surface} pinned run record '{}' cannot be read: {error}",
                    path.display()
                )
            })?;
    if ContentHash::of_bytes(&bytes) != *expected {
        return Err(format!(
            "{surface} pinned run record does not match its hash"
        ));
    }
    let record = RunRecord::from_bytes(&bytes)
        .map_err(|error| format!("{surface} pinned run record is malformed: {error}"))?;
    Ok((bytes, record))
}

pub(crate) fn artifact_policies_overlap(left: &BindingArtifactPolicy, right: &BindingArtifactPolicy) -> bool {
    match (left, right) {
        (BindingArtifactPolicy::All, _) | (_, BindingArtifactPolicy::All) => true,
        (
            BindingArtifactPolicy::Allow {
                names: left_names,
                namespaces: left_namespaces,
            },
            BindingArtifactPolicy::Allow {
                names: right_names,
                namespaces: right_namespaces,
            },
        ) => {
            left_names.iter().any(|name| right.admits(name))
                || right_names.iter().any(|name| left.admits(name))
                || left_namespaces.iter().any(|left_namespace| {
                    right_namespaces.iter().any(|right_namespace| {
                        left_namespace == right_namespace
                            || left_namespace
                                .strip_prefix(right_namespace)
                                .is_some_and(|suffix| suffix.starts_with('.'))
                            || right_namespace
                                .strip_prefix(left_namespace)
                                .is_some_and(|suffix| suffix.starts_with('.'))
                    })
                })
        }
    }
}

pub(crate) fn output_artifact_policies_overlap(left: &RunAuthoringCheckpoint, right: &RunAuthoringCheckpoint) -> bool {
    let policies = std::iter::once(&left.interface_binding)
        .chain(&left.outbound_interface_bindings)
        .chain(std::iter::once(&right.interface_binding))
        .chain(&right.outbound_interface_bindings)
        .map(|binding| &binding.config.artifact_policy)
        .collect::<Vec<_>>();
    let candidates = policies.iter().flat_map(|policy| match policy {
        BindingArtifactPolicy::All => Vec::new(),
        BindingArtifactPolicy::Allow { names, namespaces } => names.iter().chain(namespaces).collect(),
    }).collect::<Vec<_>>();
    candidates.is_empty() || candidates.into_iter().any(|label| policies.iter().all(|policy| policy.admits(label)))
}

pub(crate) fn binding_admits_exact_name(binding: &AuthoredInterfaceBinding, artifact_label: &str) -> bool {
    matches!(
        &binding.config.artifact_policy,
        BindingArtifactPolicy::Allow { names, namespaces }
            if names.as_slice() == [artifact_label] && namespaces.is_empty()
    )
}

pub(crate) fn validate_pending_handoff_delivery_isolation(
    record: &RunRecord,
    pending: &PendingHandoff,
) -> Result<(), String> {
    for (index, left) in pending.items.iter().enumerate() {
        for right in pending.items.iter().skip(index + 1) {
            if artifact_policies_overlap(
                &left.checkpoint.interface_binding.config.artifact_policy,
                &right.checkpoint.interface_binding.config.artifact_policy,
            ) {
                return Err(format!(
                    "provider run '{}' pending handoff artifact labels '{}' and '{}' have overlapping inbound artifact policies",
                    record.id.as_str(),
                    left.artifact_label,
                    right.artifact_label
                ));
            }
        }
    }
    Ok(())
}
