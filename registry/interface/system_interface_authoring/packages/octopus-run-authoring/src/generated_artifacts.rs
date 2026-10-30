use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedArtifact {
    pub artifact: ArtifactRef,
    pub source_artifact: ArtifactRef,
    pub source_movement_seq: u64,
    pub admission_seq: u64,
    pub payload_content_hash: String,
    pub byte_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_artifact: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_movement_seq: Option<u64>,
}

pub fn output_artifact_label(correlation: &str) -> Result<String, String> {
    if correlation.is_empty() || correlation.trim() != correlation {
        return Err("generated artifact requires its exact original call correlation".into());
    }
    Ok(format!(
        "tool.output.{}",
        blake3::hash(correlation.as_bytes()).to_hex()
    ))
}

pub fn output_artifact_binding(
    interface: InterfaceId,
    correlation: &str,
) -> Result<AuthoredInterfaceBinding, String> {
    let mut binding = AuthoredInterfaceBinding::all(interface);
    binding.config.artifact_policy = BindingArtifactPolicy::Allow {
        names: vec![output_artifact_label(correlation)?],
        namespaces: Vec::new(),
    };
    Ok(binding)
}

pub fn mandatory_role_bindings(grant: &octopus_ledger::GrantProjection) -> Result<Vec<ArtifactBinding>, String> {
    let mut seen = BTreeSet::new();
    grant.role_artifacts.iter().map(|role| {
        if role.is_empty() || role.trim() != role || !seen.insert(role)
            || grant.artifact_access.get(role).is_none_or(|access| !access.read) {
            return Err("raw output checkpoint has no exact readable mandatory role closure".into());
        }
        Ok(ArtifactBinding { id: ArtifactId::new(role), access: octopus_policy::ArtifactAccess::read_only() })
    }).collect()
}

pub fn recorded_generated_artifacts(
    record: &RunRecord,
    owner: &InterfaceId,
) -> Result<Vec<GeneratedArtifact>, String> {
    record
        .interface_state
        .get(owner)
        .and_then(|state| state.get("generated_artifacts"))
        .map(|value| serde_json::from_value(value.clone()).map_err(|e| e.to_string()))
        .unwrap_or_else(|| Ok(Vec::new()))
}

pub fn admitted_generated_artifacts(
    evidence: &CheckedRunEvidence<'_>,
    result_movement: &Movement,
    envelope: &InterfaceInvocationEnvelope,
) -> Result<Vec<GeneratedArtifact>, String> {
    let result: octopus_tool_handlers::ToolResultArtifact = serde_json::from_slice(
        &envelope
            .read_artifact_payload_bounded(evidence.max_payload_bytes)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if result_movement.artifact.as_ref() != Some(&envelope.artifact)
        || result_movement
            .invocation_metadata
            .as_ref()
            .and_then(|m| m.invocation())
            != Some(envelope.metadata().map_err(|e| e.to_string())?)
    {
        return Err(
            "generated artifact does not arrive in its admitted successful tool result".into(),
        );
    }
    let mut reader = FileCheckedRunReader::open(evidence, None)?;
    validate_result_outputs(&mut reader, result_movement, &result)
}

pub fn read_movement(
    evidence: &CheckedRunEvidence<'_>,
    seq: u64,
) -> Result<Movement, String> {
    let mut reader = FileCheckedRunReader::open(evidence, seq.checked_sub(1))?;
    while reader.next_seq <= seq {
        let current = reader.next_seq;
        let movement = reader
            .movement(current)?
            .ok_or("generated artifact movement is absent")?;
        if current == seq {
            return Ok(movement);
        }
    }
    Err("generated artifact movement cursor skipped its source".into())
}

fn validate_result_outputs(
    reader: &mut impl CheckedRunEvidenceReader,
    result_movement: &Movement,
    result: &ToolResultArtifact,
) -> Result<Vec<GeneratedArtifact>, String> {
    result.validate()?;
    let Some(value) = result
        .body
        .as_ref()
        .and_then(|body| body.get("generated_artifacts"))
    else {
        return Ok(Vec::new());
    };
    if !result.ok || result_movement.kind != MovementKind::InterfaceOutbound {
        return Err("generated artifact result is not an admitted success".into());
    }
    let mut outputs: Vec<GeneratedArtifact> =
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    if outputs.is_empty() {
        return Err("generated artifact declaration is empty".into());
    }
    let metadata = result_movement
        .invocation_metadata
        .as_ref()
        .and_then(|m| m.invocation())
        .ok_or("generated result lost admission metadata")?;
    let runtime = metadata
        .runtime
        .as_ref()
        .ok_or("generated result lost its runtime scope")?;
    if metadata.verification_failure.is_some()
        || !metadata
            .checkpoint_admission
            .as_ref()
            .is_some_and(|admission| admission.complete)
    {
        return Err("generated result is not completely admitted".into());
    }
    let correlation = result
        .correlation_id
        .as_deref()
        .ok_or("generated artifact result has no correlation")?;
    let label = output_artifact_label(correlation)?;
    let mut ids = BTreeSet::new();
    for output in &mut outputs {
        if !ids.insert(&output.artifact.id)
            || output.source_movement_seq >= output.admission_seq
            || output.admission_seq >= result_movement.seq.0
            || output.result_artifact.is_some()
            || output.result_movement_seq.is_some()
        {
            return Err("generated artifact sequence or identity is not exact".into());
        }
        let call = reader
            .admitted_movement(output.source_movement_seq)?
            .ok_or("generated artifact call is absent")?;
        let acknowledged = reader
            .admitted_movement(output.admission_seq)?
            .ok_or("generated artifact arrival is absent")?;
        if acknowledged.kind != MovementKind::InterfaceArrival
            || acknowledged.from != result_movement.from
            || acknowledged.artifact.as_ref() != Some(&output.artifact)
        {
            return Err("generated artifact hand-in receipt does not name its arrival".into());
        }
        let mut own_arrival = None;
        for seq in output.admission_seq..result_movement.seq.0 {
            let candidate = reader
                .admitted_movement(seq)?
                .ok_or("generated artifact arrival interval is incomplete")?;
            let matches = candidate.kind == MovementKind::InterfaceArrival
                && candidate.from == result_movement.from && candidate.artifact.as_ref() == Some(&output.artifact)
                && candidate.invocation_metadata.as_ref().and_then(|m|m.invocation()).is_some_and(|m|
                    m.runtime.as_ref().is_some_and(|r|r.run == runtime.run && r.worker == runtime.worker)
                    && m.verification_failure.is_none() && m.required_verified_artifacts.is_empty() && m.outbound_interface_bindings.is_empty()
                    && m.checkpoint_binding_policy.as_ref().is_some_and(|policy|policy.outbound.is_empty()
                        && matches!(&policy.inbound.config.artifact_policy, BindingArtifactPolicy::Allow {names,namespaces} if namespaces.is_empty() && names.as_slice() == [label.clone()])));
            if matches {
                own_arrival = Some(candidate);
                break;
            }
        }
        let arrival =
            own_arrival.ok_or("generated artifact has no exact admission-only edge in this run")?;
        output.admission_seq = arrival.seq.0;
        let call_metadata = call
            .invocation_metadata
            .as_ref()
            .and_then(|m| m.invocation())
            .ok_or("generated artifact call has no admitted metadata")?;
        let raw_metadata = arrival
            .invocation_metadata
            .as_ref()
            .and_then(|m| m.invocation())
            .ok_or("generated artifact has no admitted metadata")?;
        let call_runtime = call_metadata
            .runtime
            .as_ref()
            .ok_or("generated artifact call has no runtime scope")?;
        let raw_runtime = raw_metadata
            .runtime
            .as_ref()
            .ok_or("generated artifact has no runtime scope")?;
        let roles = mandatory_role_bindings(&raw_metadata.grant)?;
        if !raw_metadata.artifact_binding_refs.iter().map(|reference| &reference.id)
            .eq(roles.iter().map(|binding| &binding.id)) {
            return Err("generated artifact admission changed its mandatory role-only context".into());
        }
        if call.kind != MovementKind::InterfaceOutbound
            || call.artifact.as_ref() != Some(&output.source_artifact)
            || call.to != result_movement.from
            || arrival.from != result_movement.from
            || arrival.kind != MovementKind::InterfaceArrival
            || arrival.artifact.as_ref() != Some(&output.artifact)
            || call_runtime.run != runtime.run
            || call_runtime.worker != runtime.worker
            || raw_runtime.run != runtime.run
            || raw_runtime.worker != runtime.worker
            || raw_metadata.verification_failure.is_some()
            || !raw_metadata.required_verified_artifacts.is_empty()
            || !raw_metadata.outbound_interface_bindings.is_empty()
            || raw_metadata
                .checkpoint_binding_policy
                .as_ref()
                .is_none_or(|policy| !policy.inbound.admits(&label) || !policy.outbound.is_empty())
        {
            return Err(
                "generated artifact changed its admitted call, room, run or raw checkpoint".into(),
            );
        }
        let (_, call_bytes) = reader.artifact(&output.source_artifact)?;
        let call: octopus_tool_handlers::ToolCallArtifact =
            serde_json::from_slice(&call_bytes).map_err(|e| e.to_string())?;
        if call.tool != result.tool || call.correlation_id.as_deref() != Some(correlation) {
            return Err("generated artifact belongs to another model tool call".into());
        }
        let (manifest, bytes) = reader.artifact(&output.artifact)?;
        if manifest.label != label
            || output.artifact.id.as_str()
                != format!("{}:{}", label, output.artifact.content.to_hex())
            || manifest.payload_content_hash.as_deref()
                != Some(output.payload_content_hash.as_str())
            || blake3::hash(&bytes).to_hex().as_str() != output.payload_content_hash
            || bytes.len() as u64 != output.byte_count
        {
            return Err(
                "generated artifact payload differs from the admitted immutable bytes".into(),
            );
        }
        output.result_artifact = result_movement.artifact.clone();
        output.result_movement_seq = Some(result_movement.seq.0);
    }
    Ok(outputs)
}

pub fn validate_generated_transition(
    reader: &mut impl CheckedRunEvidenceReader,
    transition: &CheckedRunTransition,
) -> Result<(), String> {
    let before = RunRecord::from_bytes(&transition.before).map_err(|e| e.to_string())?;
    let after = RunRecord::from_bytes(&transition.after).map_err(|e| e.to_string())?;
    let previous = recorded_generated_artifacts(&before, &transition.writer.owner)?;
    for output in recorded_generated_artifacts(&after, &transition.writer.owner)? {
        if previous.contains(&output) {
            continue;
        }
        let seq = output
            .result_movement_seq
            .ok_or("generated binding has no admitted result sequence")?;
        if seq >= transition.call.seq.0 {
            return Err("generated read binding predates its result".into());
        }
        let movement = reader
            .admitted_movement(seq)?
            .ok_or("generated artifact result movement is absent")?;
        let reference = output
            .result_artifact
            .as_ref()
            .ok_or("generated binding has no admitted result artifact")?;
        if movement.artifact.as_ref() != Some(reference)
            || movement.to != Endpoint::Interface(transition.writer.owner.clone())
            || movement
                .invocation_metadata
                .as_ref()
                .and_then(|m| m.invocation())
                .and_then(|m| m.runtime.as_ref())
                .is_none_or(|runtime| {
                    runtime.run != after.id
                        || runtime.worker != after.worker
                })
        {
            return Err("generated read binding changed its admitted result owner".into());
        }
        let (_, bytes) = reader.artifact(reference)?;
        let result: ToolResultArtifact =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if !validate_result_outputs(reader, &movement, &result)?.contains(&output) {
            return Err("generated read binding differs from its admitted evidence".into());
        }
    }
    Ok(())
}
