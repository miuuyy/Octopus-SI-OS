use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_descriptors::{ArtifactId, InterfaceId, RunId};
use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{
    InterfaceBoundaryError, InterfaceDescriptor, InterfaceInvocationEnvelope,
};
use octopus_ledger::{ArtifactRef, ContentHash, Endpoint, MovementKind};
use octopus_ledger_store::DurableMovementDecoder;
use octopus_login::AuthManager;
use octopus_model_context_render::model_hidden_tool_names;
use octopus_world_pyecho_life_runtime::{LedgerReplayReader, LedgerTailReader};

use crate::config::{
    CodexInstanceConfig, auth_manager_from_provider_home,
    connect_interface_driver_from_descriptor_with_auth_and_secrets,
};
use crate::handler::{
    CodexAppServerInterfaceDriver, CodexDeferredTurnCause, CodexProviderInvocation,
    CodexRecoveredHandoffArtifact, CodexTurnControl, CodexTurnControlApplied,
    CodexTurnControlBroker, CodexTurnControlDisposition, CodexTurnControlEffectIntent,
    CodexTurnControlResult, ControlOutputError, PROVIDER_TRANSPORT_DISPATCH_LABEL,
    ProviderHandoffBarrier, ProviderOutputStream, ProviderStreamEmission,
    TURN_ERROR_LABEL, TURN_INTERRUPTED_RESULT_LABEL, codex_continuity_incompatibility,
    codex_continuity_successor_is_settled, codex_continuity_waits_for_result, codex_continuity_has_interrupted_pending,
    codex_session_snapshot_labels, codex_context_output_labels, decode_inflight_turn_control,
    exact_interrupted_control_continuity, exact_interrupted_result_source, exact_interrupted_result_payload,
    interrupted_result_closure_ref, interrupted_result_closure_payload, result_turn_identity, recover_retained_native_outputs, native_terminal_result_closure_ref, recover_provider_result_continuity, is_explicit_turn_control_label,
    is_admitted_peer_interrupt, is_provider_turn_input,
    is_inflight_turn_control_candidate_label, materialize_incompatible_continuity_turn_error,
    materialize_turn_control_result,
    recover_provider_turn_continuity, reconcile_operator_session_continuity, reconcile_delivered_interrupted_continuity, recovered_failed_result_source, terminal_output_continuity,
    codex_projection_output_labels,
    validate_codex_projection_descriptor_contract, validate_durable_interrupted_continuity,
};
use crate::live_stream::{LiveStreamRules, artifact_payload_blake3, recover_sealed_terminal_stream, seal_source_turn_stream};

#[cfg(test)]
#[path = "../tests/support/interrupt_replay.rs"]
mod interrupt_replay_tests;
#[cfg(test)]
#[path = "../tests/support/failure_projection.rs"]
mod failure_projection_tests;
use crate::opencodex::{
    OpenCodexDescriptorConfig, descriptor_config as opencodex_descriptor_config,
};
use octopus_run_authoring::{
    CheckedRunError, FailedSourceHandoffError, ProviderEmission, ProviderEmissionRoute, ProviderHandoffOutcome,
    ProviderHandoffSession, ProviderInvocationSessionSnapshot, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity, payload_carries_session_context,
    resolve_session_snapshot_chain,
};

const TURN_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(2);
const PROVIDER_CONTROL_READ_BATCH_LIMIT: usize = 128;
const MACHINE_CODEX_AUTH_FILE_ENV: &str = "OCTOPUS_MACHINE_CODEX_AUTH_FILE";

#[cfg(test)]
#[path = "../tests/support/opencodex_auth.rs"]
mod opencodex_auth_tests;

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| {
        Box::pin(codex_life(ctx, ProviderLifeExpectation::Native)) as InterfaceLifeFuture
    })
}

pub fn opencodex_interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| {
        Box::pin(codex_life(ctx, ProviderLifeExpectation::OpenCodex)) as InterfaceLifeFuture
    })
}

pub fn doctor_requested() -> Result<bool, String> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(false),
        [argument] if argument == "--doctor" => Ok(true),
        _ => Err("provider life binary accepts no arguments or exactly '--doctor'".to_string()),
    }
}

pub fn doctor_native_interface() -> Result<(), String> {
    doctor_interface(ProviderLifeExpectation::Native)
}

pub fn doctor_opencodex_interface() -> Result<(), String> {
    doctor_interface(ProviderLifeExpectation::OpenCodex)
}


mod doctor;
pub(crate) use doctor::*;
mod state;
pub(crate) use state::*;
mod main_loop;
pub(crate) use main_loop::*;
mod control_reader;
pub(crate) use control_reader::*;
mod interrupted_effects;
pub(crate) use interrupted_effects::*;
mod deferred_turns;
pub(crate) use deferred_turns::*;
mod session_carrier;
pub(crate) use session_carrier::*;
mod ledger_scan;
pub(crate) use ledger_scan::*;
mod control_results;
pub(crate) use control_results::*;
mod turn_driver;
pub(crate) use turn_driver::*;
mod late_calls;
pub(crate) use late_calls::*;
mod dispatch_admission;
pub(crate) use dispatch_admission::*;
mod emission_forwarding;
pub(crate) use emission_forwarding::*;
mod source_failures;
pub(crate) use source_failures::*;
mod terminal_errors;
pub(crate) use terminal_errors::*;
mod auth_and_policy;
pub(crate) use auth_and_policy::*;

#[cfg(test)]
#[path = "../tests/support/life_tests.rs"]
mod tests;
