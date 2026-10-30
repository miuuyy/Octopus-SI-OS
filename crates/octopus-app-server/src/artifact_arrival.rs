use octopus_descriptors::{CheckpointId, InterfaceId};
use octopus_interface_projection::resolve_interface_arrival_payload;
use octopus_interfaces::{DeliveryVerdict, InterfaceArtifactFile, InvocationMetadata};
use octopus_ledger::{
    ArtifactRef, BlobStore, Endpoint, Lamport, MovementDraft, MovementKind, MovementLog, Seq,
    TraceId,
};
use octopus_runtime::RunId;

use crate::arrival::{
    ArrivalError, InterfaceArrivalRecord, InterfaceArtifactArrivalFromInterface,
    SystemInterfaceEmissionOutcome,
};

#[derive(Debug)]
pub struct InterfaceArtifactArrivalAdmission {
    pub records: Vec<InterfaceArrivalRecord>,
}

impl InterfaceArtifactArrivalAdmission {
    pub fn first_outbound_artifact(&self) -> Option<ArtifactRef> {
        self.records
            .iter()
            .flat_map(|record| record.outbound_transfers.iter())
            .next()
            .map(|transfer| transfer.artifact.clone())
    }
}

#[derive(Debug)]
pub struct InterfaceArtifactArrivalAdmissionOutcome {
    pub admission: InterfaceArtifactArrivalAdmission,
}

#[derive(Debug, thiserror::Error)]
pub enum InterfaceArtifactArrivalAdmissionError {
    #[error("interface artifact arrival failed closed: {0}")]
    Arrival(#[from] ArrivalError),
    #[error("interface artifact arrival awaits a stable registry snapshot after {attempts} observations")]
    RegistryUnstable { attempts: usize },
}

impl InterfaceArtifactArrivalAdmissionError {
    pub fn verdict(&self) -> DeliveryVerdict {
        match self {
            InterfaceArtifactArrivalAdmissionError::Arrival(error) => error.verdict(),
            Self::RegistryUnstable { attempts } => DeliveryVerdict::refused(
                octopus_ledger::RefusedReason::RegistryUnstable,
                serde_json::json!({"message":self.to_string(),"attempts":attempts}),
            ),
        }
    }

    fn registry(error: crate::spine_root::RegistrySnapshotError, operation: &str) -> Self {
        match error {
            crate::spine_root::RegistrySnapshotError::Unstable { attempts } => Self::RegistryUnstable { attempts },
            crate::spine_root::RegistrySnapshotError::Invalid(error) => Self::Arrival(
                ArrivalError::Malformed(format!("{operation} registry reread failed closed: {error}")),
            ),
        }
    }
}

fn arrival_trace_id(arriving_interface: &InterfaceId) -> TraceId {
    TraceId(format!("arrival:{}", arriving_interface.as_str()))
}

struct AttestedEmissionSource {
    run: RunId,
    checkpoint: CheckpointId,
    epoch: octopus_run_record::GrantEpoch,
    trace: TraceId,
    lamport: Lamport,
}

fn attest_emission_source(
    log: &(dyn MovementLog + Send),
    source_movement_seq: u64,
    source_interface: &InterfaceId,
) -> Result<AttestedEmissionSource, InterfaceArtifactArrivalAdmissionError> {
    let movement = log
        .read_at(Seq(source_movement_seq))
        .ok_or_else(|| {
            InterfaceArtifactArrivalAdmissionError::Arrival(ArrivalError::Malformed(format!(
                "interface emission source movement seq {source_movement_seq} does not exist"
            )))
        })?;
    if movement.kind != MovementKind::InterfaceOutbound {
        return Err(InterfaceArtifactArrivalAdmissionError::Arrival(
            ArrivalError::Malformed(format!(
                "interface emission source movement seq {source_movement_seq} is not InterfaceOutbound"
            )),
        ));
    }
    if movement.artifact.is_none() {
        return Err(InterfaceArtifactArrivalAdmissionError::Arrival(
            ArrivalError::Malformed(format!(
                "interface emission source movement seq {source_movement_seq} carries no artifact"
            )),
        ));
    }
    match &movement.to {
        Endpoint::Interface(interface) if interface == source_interface => {}
        Endpoint::Interface(interface) => {
            return Err(InterfaceArtifactArrivalAdmissionError::Arrival(
                ArrivalError::Malformed(format!(
                    "interface emission source movement seq {source_movement_seq} targets interface '{}', not host-attested interface '{}'",
                    interface.as_str(),
                    source_interface.as_str()
                )),
            ));
        }
    }

    let metadata = movement.invocation_metadata.as_ref().ok_or_else(|| {
        InterfaceArtifactArrivalAdmissionError::Arrival(ArrivalError::Malformed(format!(
            "interface emission source movement seq {source_movement_seq} carries no invocation metadata"
        )))
    })?;
    let metadata: &InvocationMetadata = metadata.invocation().ok_or_else(|| {
        InterfaceArtifactArrivalAdmissionError::Arrival(ArrivalError::Malformed(format!(
            "interface emission source movement seq {source_movement_seq} carries non-invocation metadata"
        )))
    })?;
    let runtime = metadata.runtime().ok_or_else(|| {
        InterfaceArtifactArrivalAdmissionError::Arrival(ArrivalError::Malformed(format!(
            "interface emission source movement seq {source_movement_seq} carries no runtime coordinates"
        )))
    })?;
    if &runtime.interface != source_interface {
        return Err(InterfaceArtifactArrivalAdmissionError::Arrival(
            ArrivalError::Malformed(format!(
                "interface emission source movement seq {source_movement_seq} metadata names interface '{}', not host-attested interface '{}'",
                runtime.interface.as_str(),
                source_interface.as_str()
            )),
        ));
    }
    if !matches!(&movement.from, Endpoint::Interface(_)) {
        return Err(InterfaceArtifactArrivalAdmissionError::Arrival(
            ArrivalError::Malformed(format!(
                "interface emission source movement seq {source_movement_seq} does not carry Interface-to-Interface provenance"
            )),
        ));
    }
    Ok(AttestedEmissionSource {
        run: runtime.run.clone(),
        checkpoint: runtime.checkpoint.clone(),
        epoch: octopus_run_record::GrantEpoch(runtime.epoch),
        trace: movement.trace,
        lamport: movement.lamport,
    })
}

fn record_boundary_refusal(
    log: &mut (dyn MovementLog + Send),
    source_interface: &InterfaceId,
    target_interface: &InterfaceId,
    artifact: Option<ArtifactRef>,
    error: InterfaceArtifactArrivalAdmissionError,
) -> InterfaceArtifactArrivalAdmissionError {
    let verdict = error.verdict();
    log.append(MovementDraft {
        trace: TraceId(format!("interface-emission:{}", source_interface.as_str())),
        kind: MovementKind::DeliveryRefused,
        from: Endpoint::Interface(source_interface.clone()),
        to: Endpoint::Interface(target_interface.clone()),
        artifact,
        invocation_metadata: Some(verdict.into()),
        causal_floor: None,
    });
    error
}

pub async fn admit_interface_artifact_arrival(
    embedded: &crate::EmbeddedRuntime,
    log: &mut (dyn MovementLog + Send),
    blobs_arrival: &mut dyn BlobStore,
    arrival_artifact_file: &InterfaceArtifactFile,
    arriving_interface: &InterfaceId,
) -> Result<InterfaceArtifactArrivalAdmissionOutcome, InterfaceArtifactArrivalAdmissionError> {
    let arrival_artifact_file = prepare_interface_artifact_arrival(embedded, log, arrival_artifact_file, arriving_interface)?;
    let mut arrival = InterfaceArtifactArrivalFromInterface::new(
        &embedded.runtime, log, blobs_arrival, embedded.arrival_dedup.clone(),
        arrival_trace_id(arriving_interface), arriving_interface.clone(),
    );
    let dispatch = arrival.arrive_artifact_for_run(&arrival_artifact_file).await?;
    if dispatch.records.is_empty() {
        return Err(InterfaceArtifactArrivalAdmissionError::Arrival(
            ArrivalError::Malformed("interface artifact arrival produced no arrival record".into()),
        ));
    }
    Ok(InterfaceArtifactArrivalAdmissionOutcome {
        admission: InterfaceArtifactArrivalAdmission { records: dispatch.records },
    })
}

pub async fn admit_retained_interface_artifact_arrival(
    embedded: &crate::EmbeddedRuntime,
    log: &mut (dyn MovementLog + Send),
    blobs_arrival: &mut dyn BlobStore,
    arrival_artifact_file: &InterfaceArtifactFile,
    arriving_interface: &InterfaceId,
    identity: &str,
) -> Result<octopus_ledger::ArtifactDelivery, InterfaceArtifactArrivalAdmissionError> {
    let artifact = match prepare_interface_artifact_arrival(embedded, log, arrival_artifact_file, arriving_interface) {
        Ok(artifact) => artifact,
        Err(error) if error.verdict().awaits_current_source() => {
            return Ok(octopus_ledger::ArtifactDelivery::Complete { verdict: error.verdict() });
        }
        Err(error) => return Err(error),
    };
    InterfaceArtifactArrivalFromInterface::new(
        &embedded.runtime, log, blobs_arrival, embedded.arrival_dedup.clone(),
        arrival_trace_id(arriving_interface), arriving_interface.clone(),
    ).arrive_retained_artifact(&artifact, identity).await.map_err(Into::into)
}

fn prepare_interface_artifact_arrival(
    embedded: &crate::EmbeddedRuntime,
    log: &mut (dyn MovementLog + Send),
    arrival_artifact_file: &InterfaceArtifactFile,
    arriving_interface: &InterfaceId,
) -> Result<InterfaceArtifactFile, InterfaceArtifactArrivalAdmissionError> {
    embedded
        .reload_registry_snapshot_on_artifact_arrival()
        .map_err(|e| {
            record_boundary_refusal(
                log,
                arriving_interface,
                arriving_interface,
                None,
                InterfaceArtifactArrivalAdmissionError::registry(e, "artifact arrival"),
            )
        })?;
    let arrival_artifact_file =
        resolve_interface_arrival_payload(arrival_artifact_file).map_err(|e| {
            record_boundary_refusal(
                log,
                arriving_interface,
                arriving_interface,
                None,
                InterfaceArtifactArrivalAdmissionError::Arrival(e.into()),
            )
        })?;
    Ok(arrival_artifact_file)
}

#[allow(clippy::too_many_arguments)]
pub async fn admit_system_interface_emission(
    embedded: &crate::EmbeddedRuntime,
    log: &mut (dyn MovementLog + Send),
    blobs_arrival: &mut dyn BlobStore,
    emission_artifact_file: &InterfaceArtifactFile,
    source_movement_seq: u64,
    from_interface: &InterfaceId,
    to_interface: &InterfaceId,
) -> Result<SystemInterfaceEmissionOutcome, InterfaceArtifactArrivalAdmissionError> {
    embedded
        .reload_registry_snapshot_on_artifact_arrival()
        .map_err(|e| {
            record_boundary_refusal(
                log,
                from_interface,
                to_interface,
                None,
                InterfaceArtifactArrivalAdmissionError::registry(e, "interface emission"),
            )
        })?;
    let emission_artifact_file = resolve_interface_arrival_payload(emission_artifact_file)
        .map_err(|e| {
            record_boundary_refusal(
                log,
                from_interface,
                to_interface,
                None,
                InterfaceArtifactArrivalAdmissionError::Arrival(e.into()),
            )
        })?;
    let source = attest_emission_source(log, source_movement_seq, from_interface)
        .map_err(|error| record_boundary_refusal(log, from_interface, to_interface, None, error))?;
    let mut arrival = InterfaceArtifactArrivalFromInterface::new(
        &embedded.runtime,
        log,
        blobs_arrival,
        embedded.arrival_dedup.clone(),
        source.trace,
        from_interface.clone(),
    );
    let outcome = arrival
        .emit_artifact_to_interface(
            &emission_artifact_file,
            &source.run,
            &source.checkpoint,
            source.epoch,
            source.lamport,
            to_interface,
        )
        .await?;
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
pub async fn admit_system_interface_reference_emission(
    embedded: &crate::EmbeddedRuntime,
    log: &mut (dyn MovementLog + Send),
    blobs_arrival: &mut dyn BlobStore,
    artifact: &ArtifactRef,
    source_movement_seq: u64,
    from_interface: &InterfaceId,
    to_interface: &InterfaceId,
) -> Result<SystemInterfaceEmissionOutcome, InterfaceArtifactArrivalAdmissionError> {
    embedded
        .reload_registry_snapshot_on_artifact_arrival()
        .map_err(|e| {
            record_boundary_refusal(
                log,
                from_interface,
                to_interface,
                Some(artifact.clone()),
                InterfaceArtifactArrivalAdmissionError::registry(e, "interface emission"),
            )
        })?;
    let source =
        attest_emission_source(log, source_movement_seq, from_interface).map_err(|error| {
            record_boundary_refusal(
                log,
                from_interface,
                to_interface,
                Some(artifact.clone()),
                error,
            )
        })?;
    let mut arrival = InterfaceArtifactArrivalFromInterface::new(
        &embedded.runtime,
        log,
        blobs_arrival,
        embedded.arrival_dedup.clone(),
        source.trace,
        from_interface.clone(),
    );
    let outcome = arrival
        .emit_artifact_reference_to_interface(
            artifact,
            &source.run,
            &source.checkpoint,
            source.epoch,
            source.lamport,
            to_interface,
        )
        .await?;
    Ok(outcome)
}
