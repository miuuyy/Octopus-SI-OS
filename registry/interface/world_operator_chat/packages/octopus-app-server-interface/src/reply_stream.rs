use octopus_descriptors::{InterfaceId, RunId};
use octopus_interfaces::RuntimeInvocationMetadata;
use octopus_ledger::{
    ArtifactRef, Cursor, Endpoint, Lamport, LiveMovementLog, LogTail, Movement, MovementCursor,
    MovementKind, MovementLog,
};
use tokio_stream::Stream;

use crate::projection::{
    COMMAND_EXECUTION_COMPLETED_LABEL, COMMAND_EXECUTION_STARTED_LABEL,
    INTERFACE_ARTIFACT_OUTPUT_LABEL,
};

pub const AGENT_MESSAGE_DELTA_METHOD: &str = "item/agentMessage/delta";

pub const ACT_OBSERVATION_REPLY_LABEL: &str = INTERFACE_ARTIFACT_OUTPUT_LABEL;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplyNotification {
    TurnStarted {
        run: RunId,
        artifact: ArtifactRef,
        at: Lamport,
    },
    OutputDelta {
        artifact: ArtifactRef,
        index: usize,
        at: Lamport,
    },
    MarkAdmitted {
        mark: String,
        evidence: Option<ArtifactRef>,
        at: Lamport,
    },
    TurnCompleted {
        reply: ArtifactRef,
        at: Lamport,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandExecutionPhase {
    Started,
    Completed,
}

impl CommandExecutionPhase {
    pub fn of_artifact_id(id: &str) -> Option<Self> {
        if artifact_id_has_label(id, COMMAND_EXECUTION_STARTED_LABEL) {
            Some(Self::Started)
        } else if artifact_id_has_label(id, COMMAND_EXECUTION_COMPLETED_LABEL) {
            Some(Self::Completed)
        } else {
            None
        }
    }
}

fn artifact_id_has_label(id: &str, label: &str) -> bool {
    id == label
        || id
            .strip_prefix(label)
            .is_some_and(|rest| rest.starts_with(':'))
}

impl ReplyNotification {
    pub fn server_notification_method(&self) -> &'static str {
        match self {
            Self::TurnStarted { .. } => "turn/started",
            Self::OutputDelta { artifact, .. } => {
                match CommandExecutionPhase::of_artifact_id(artifact.id.as_str()) {
                    Some(CommandExecutionPhase::Started) => "command_execution/started",
                    Some(CommandExecutionPhase::Completed) => "command_execution/completed",
                    None => AGENT_MESSAGE_DELTA_METHOD,
                }
            }
            Self::MarkAdmitted { .. } => "item/completed",
            Self::TurnCompleted { .. } => "turn/completed",
        }
    }

    pub fn command_execution_phase(&self) -> Option<CommandExecutionPhase> {
        match self {
            Self::OutputDelta { artifact, .. } => {
                CommandExecutionPhase::of_artifact_id(artifact.id.as_str())
            }
            _ => None,
        }
    }

    pub fn at(&self) -> Lamport {
        match self {
            Self::TurnStarted { at, .. }
            | Self::OutputDelta { at, .. }
            | Self::MarkAdmitted { at, .. }
            | Self::TurnCompleted { at, .. } => *at,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReplyStreamError {
    #[error("movement at lamport {0:?} ({1}) carries no artifact ref to project")]
    MissingArtifact(Lamport, &'static str),
    #[error("interface outbound movement at lamport {0:?} carries invalid runtime metadata")]
    InvalidOutboundMetadata(Lamport),
    #[error(
        "run {0} produced no terminal interface.artifact.output (act.observation) outbound movement"
    )]
    NoTerminal(String),
}

pub struct ReplyStream {
    notifications: std::vec::IntoIter<ReplyNotification>,
}

impl ReplyStream {
    pub fn remaining(&self) -> usize {
        self.notifications.len()
    }
}

impl Iterator for ReplyStream {
    type Item = ReplyNotification;
    fn next(&mut self) -> Option<Self::Item> {
        self.notifications.next()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplyStep {
    Note(ReplyNotification, bool),
    Terminal(ReplyNotification),
    NotForStream,
}

pub fn classify_movement(
    run: &RunId,
    interface: &InterfaceId,
    m: &Movement,
    delta_index: usize,
) -> Result<ReplyStep, ReplyStreamError> {
    let step = match &m.kind {
        MovementKind::InterfaceArrival if movement_touches_run(m, run) => {
            let artifact = require_artifact(m, "InterfaceArrival")?;
            ReplyStep::Note(
                ReplyNotification::TurnStarted {
                    run: run.clone(),
                    artifact,
                    at: m.lamport,
                },
                false,
            )
        }
        MovementKind::InterfaceOutbound => {
            if interface_outbound_runtime(m, interface)?.is_some_and(|runtime| &runtime.run == run)
            {
                let artifact = require_artifact(m, "InterfaceOutbound")?;
                if artifact_id_has_label(artifact.id.as_str(), ACT_OBSERVATION_REPLY_LABEL) {
                    ReplyStep::Terminal(ReplyNotification::TurnCompleted {
                        reply: artifact,
                        at: m.lamport,
                    })
                } else {
                    ReplyStep::Note(
                        ReplyNotification::OutputDelta {
                            artifact,
                            index: delta_index,
                            at: m.lamport,
                        },
                        true,
                    )
                }
            } else {
                ReplyStep::NotForStream
            }
        }
        MovementKind::MarkOutcomeAdmitted(name) if movement_touches_run(m, run) => ReplyStep::Note(
            ReplyNotification::MarkAdmitted {
                mark: name.as_str().to_string(),
                evidence: m.artifact.clone(),
                at: m.lamport,
            },
            false,
        ),
        _ => ReplyStep::NotForStream,
    };
    Ok(step)
}

pub fn project_movements_as_notifications(
    run: &RunId,
    interface: &InterfaceId,
    movements: &[Movement],
) -> Result<ReplyStream, ReplyStreamError> {
    let mut rows: Vec<&Movement> = movements
        .iter()
        .filter(|m| {
            movement_touches_run(m, run) || canonical_interface_outbound_targets(m, interface)
        })
        .collect();
    rows.sort_by_key(|m| (m.lamport, m.seq));

    let mut out: Vec<ReplyNotification> = Vec::with_capacity(rows.len());
    let mut delta_index = 0usize;
    let mut terminal: Option<ReplyNotification> = None;

    for m in rows {
        match classify_movement(run, interface, m, delta_index)? {
            ReplyStep::Note(note, consumed_delta) => {
                if consumed_delta {
                    delta_index += 1;
                }
                out.push(note);
            }
            ReplyStep::Terminal(note) => {
                terminal = Some(note);
            }
            ReplyStep::NotForStream => {}
        }
    }

    let terminal =
        terminal.ok_or_else(|| ReplyStreamError::NoTerminal(run.as_str().to_string()))?;
    out.push(terminal);

    Ok(ReplyStream {
        notifications: out.into_iter(),
    })
}

pub trait LiveLogReader: Send + Sync {
    fn read_tail(&self, cursor: Cursor) -> (Vec<Movement>, Cursor);
}

impl<L: MovementLog + Send> LiveLogReader for std::sync::Arc<std::sync::Mutex<LiveMovementLog<L>>> {
    fn read_tail(&self, cursor: Cursor) -> (Vec<Movement>, Cursor) {
        let guard = self.lock().expect("ledger mutex is not poisoned");
        guard.log().read_from_cursor(cursor)
    }
}

pub fn live_reply_notifications<R: LiveLogReader + 'static>(
    run: RunId,
    interface: InterfaceId,
    reader: R,
    mut tail: LogTail,
    start_cursor: Cursor,
) -> impl Stream<Item = Result<ReplyNotification, ReplyStreamError>> {
    async_stream::stream! {
        let mut cursor = start_cursor;
        let mut delta_index = 0usize;
        loop {
            let (rows, next) = reader.read_tail(cursor);
            cursor = next;

            if rows.is_empty() {
                let before = cursor;
                tail.wait_for(cursor).await;
                let (_, after_cursor) = reader.read_tail(before);
                if after_cursor == before {
                    return;
                }
                continue;
            }

            for m in &rows {
                match classify_movement(&run, &interface, m, delta_index) {
                    Ok(ReplyStep::Note(note, consumed_delta)) => {
                        if consumed_delta {
                            delta_index += 1;
                        }
                        yield Ok(note);
                    }
                    Ok(ReplyStep::Terminal(note)) => {
                        yield Ok(note);
                        return;
                    }
                    Ok(ReplyStep::NotForStream) => {}
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }
        }
    }
}

fn movement_touches_run(m: &Movement, run: &RunId) -> bool {
    m.run() == Some(run)
}

fn canonical_interface_outbound_targets(m: &Movement, interface: &InterfaceId) -> bool {
    matches!(
        (&m.kind, &m.from, &m.to),
        (
            MovementKind::InterfaceOutbound,
            Endpoint::Interface(_),
            Endpoint::Interface(target)
        ) if target == interface
    )
}

fn interface_outbound_runtime(
    m: &Movement,
    interface: &InterfaceId,
) -> Result<Option<RuntimeInvocationMetadata>, ReplyStreamError> {
    if !canonical_interface_outbound_targets(m, interface) {
        return Ok(None);
    }
    let metadata = m
        .invocation_metadata
        .as_ref()
        .and_then(|metadata| metadata.invocation())
        .and_then(|metadata| metadata.runtime.clone())
        .ok_or(ReplyStreamError::InvalidOutboundMetadata(m.lamport))?;
    if !runtime_coordinates_are_valid(&metadata) || &metadata.interface != interface {
        return Err(ReplyStreamError::InvalidOutboundMetadata(m.lamport));
    }
    Ok(Some(metadata))
}

fn runtime_coordinates_are_valid(runtime: &RuntimeInvocationMetadata) -> bool {
    [
        runtime.run.as_str(),
        runtime.ee.as_str(),
        runtime.worker.as_str(),
        runtime.checkpoint.as_str(),
        runtime.interface.as_str(),
    ]
    .into_iter()
    .all(|value| !value.is_empty() && value.trim() == value)
}


fn require_artifact(
    m: &Movement,
    movement_label: &'static str,
) -> Result<ArtifactRef, ReplyStreamError> {
    m.artifact
        .clone()
        .ok_or(ReplyStreamError::MissingArtifact(m.lamport, movement_label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, MarkId};
    use octopus_interfaces::{
        DeliveryVerdict, GrantProjection, InvocationMetadata, MovementMetadata,
        RuntimeInvocationMetadata,
    };
    use octopus_ledger::{ContentHash, InMemoryLog, MovementDraft, MovementLog, Seq, TraceId};

    fn artifact(label: &str, bytes: &[u8]) -> ArtifactRef {
        let h = ContentHash::of_bytes(bytes);
        ArtifactRef {
            id: ArtifactId::new(format!("{label}:{}", h.to_hex())),
            content: h,
        }
    }

    fn metadata(run: &RunId, checkpoint: &str, interface: &str) -> Option<MovementMetadata> {
        Some(
            InvocationMetadata::new(GrantProjection::default())
                .with_runtime(RuntimeInvocationMetadata {
                    run: run.clone(),
                    ee: EeId::new("ee_fixture"),
                    worker: InterfaceId::new("system_worker_fixture"),
                    checkpoint: CheckpointId::new(checkpoint),
                    interface: InterfaceId::new(interface),
                    epoch: 0,
                    run_record_hash: None,
                })
                .into(),
        )
    }

    fn reply_interface() -> InterfaceId {
        InterfaceId::new("system_fixture_artifact_out")
    }

    fn build_multistep_log(run: &RunId) -> InMemoryLog {
        let mut log = InMemoryLog::new();
        let arrival = log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("system_fixture_artifact")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_fixture")),
            artifact: Some(artifact(
                "operator.input",
                b"{\"method\":\"thread/injectItems\"}",
            )),
            invocation_metadata: metadata(run, "arrival", "system_fixture_artifact"),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_fixture")),
            to: Endpoint::Interface(InterfaceId::new("system_fixture_artifact_out")),
            artifact: Some(artifact("agent.message.delta", b"hel")),
            invocation_metadata: metadata(run, "delta-1", "system_fixture_artifact_out"),
            causal_floor: Some(arrival.lamport),
        });
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_fixture")),
            to: Endpoint::Interface(InterfaceId::new("system_fixture_artifact_out")),
            artifact: Some(artifact("agent.message.delta", b"lo")),
            invocation_metadata: metadata(run, "delta-2", "system_fixture_artifact_out"),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::MarkOutcomeAdmitted(MarkId::new("coder.build.ok")),
            from: Endpoint::Interface(InterfaceId::new("system_fixture_artifact")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_fixture")),
            artifact: Some(artifact("mark.evidence", b"evidence")),
            invocation_metadata: Some(serde_json::from_value::<octopus_ledger::Mark>(serde_json::json!({
                "name":"coder.build.ok", "subject":"arrival", "run":run, "run_epoch":0,
                "class":"success", "evidence":artifact("mark.evidence", b"evidence"),
                "verifier":"fixture_verifier"
            })).unwrap().into()),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_fixture")),
            to: Endpoint::Interface(InterfaceId::new("system_fixture_artifact_out")),
            artifact: Some(artifact(
                ACT_OBSERVATION_REPLY_LABEL,
                b"{\"label\":\"act.observation\"}",
            )),
            invocation_metadata: metadata(run, "terminal", "system_fixture_artifact_out"),
            causal_floor: None,
        });
        log
    }

    #[test]
    fn projects_ordered_stream_ending_in_terminal() {
        let run = RunId::new("run-stream");
        let log = build_multistep_log(&run);
        let movements = log.read_from(Seq(0));

        let stream = project_movements_as_notifications(&run, &reply_interface(), &movements)
            .expect("a completed multi-step run projects a reply stream");
        let notes: Vec<ReplyNotification> = stream.collect();

        assert!(matches!(notes[0], ReplyNotification::TurnStarted { .. }));
        assert_eq!(notes[0].server_notification_method(), "turn/started");

        match &notes[1] {
            ReplyNotification::OutputDelta { index, .. } => assert_eq!(*index, 0),
            other => panic!("expected first delta, got {other:?}"),
        }
        match &notes[2] {
            ReplyNotification::OutputDelta { index, .. } => assert_eq!(*index, 1),
            other => panic!("expected second delta, got {other:?}"),
        }
        assert_eq!(
            notes[1].server_notification_method(),
            "item/agentMessage/delta"
        );

        match &notes[3] {
            ReplyNotification::MarkAdmitted { mark, .. } => assert_eq!(mark, "coder.build.ok"),
            other => panic!("expected mark, got {other:?}"),
        }
        assert_eq!(notes[3].server_notification_method(), "item/completed");

        match notes.last().unwrap() {
            ReplyNotification::TurnCompleted { reply, .. } => {
                assert!(artifact_id_has_label(
                    reply.id.as_str(),
                    ACT_OBSERVATION_REPLY_LABEL
                ))
            }
            other => panic!("expected terminal TurnCompleted last, got {other:?}"),
        }
        assert_eq!(
            notes.last().unwrap().server_notification_method(),
            "turn/completed"
        );

        for w in notes.windows(2) {
            assert!(w[0].at() <= w[1].at(), "notifications are in causal order");
        }
    }

    #[test]
    fn no_terminal_fails_closed() {
        let run = RunId::new("run-incomplete");
        let mut log = InMemoryLog::new();
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("system_fixture_artifact")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_fixture")),
            artifact: Some(artifact("operator.input", b"{}")),
            invocation_metadata: metadata(&run, "arrival", "system_fixture_artifact"),
            causal_floor: None,
        });
        let movements = log.read_from(Seq(0));
        let err = project_movements_as_notifications(&run, &reply_interface(), &movements)
            .err()
            .expect("an incomplete run fails closed (no terminal), not a stream");
        assert_eq!(
            err,
            ReplyStreamError::NoTerminal("run-incomplete".to_string())
        );
    }

    #[test]
    fn scopes_to_one_run_over_a_shared_ledger() {
        let run_a = RunId::new("run-a");
        let run_b = RunId::new("run-b");
        let mut log = build_multistep_log(&run_a);
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("system_fixture_artifact")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_fixture")),
            artifact: Some(artifact("operator.input", b"{\"b\":1}")),
            invocation_metadata: metadata(&run_b, "arrival", "system_fixture_artifact"),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_fixture")),
            to: Endpoint::Interface(InterfaceId::new("system_fixture_artifact_out")),
            artifact: Some(artifact(
                ACT_OBSERVATION_REPLY_LABEL,
                b"{\"label\":\"act.observation\",\"b\":1}",
            )),
            invocation_metadata: metadata(&run_b, "terminal", "system_fixture_artifact_out"),
            causal_floor: None,
        });
        let movements = log.read_from(Seq(0));

        let a: Vec<ReplyNotification> =
            project_movements_as_notifications(&run_a, &reply_interface(), &movements)
                .unwrap()
                .collect();
        let b: Vec<ReplyNotification> =
            project_movements_as_notifications(&run_b, &reply_interface(), &movements)
                .unwrap()
                .collect();

        assert!(
            matches!(a.first().unwrap(), ReplyNotification::TurnStarted { run, .. } if run == &run_a)
        );
        assert!(matches!(
            a.last().unwrap(),
            ReplyNotification::TurnCompleted { .. }
        ));
        assert!(
            a.iter()
                .any(|n| matches!(n, ReplyNotification::OutputDelta { .. }))
        );

        assert_eq!(b.len(), 2, "run-b contributed only an arrival + a terminal");
        assert!(matches!(b[0], ReplyNotification::TurnStarted { .. }));
        assert!(matches!(b[1], ReplyNotification::TurnCompleted { .. }));
    }

    #[test]
    fn outbound_projection_requires_the_exact_interface_target() {
        let run = RunId::new("run-exact-interface");
        let mut log = build_multistep_log(&run);
        let foreign_terminal = artifact(ACT_OBSERVATION_REPLY_LABEL, b"foreign-target");
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_fixture")),
            to: Endpoint::Interface(InterfaceId::new("system_foreign_target")),
            artifact: Some(foreign_terminal.clone()),
            invocation_metadata: metadata(&run, "foreign", "system_foreign_target"),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("system_fixture_artifact")),
            to: Endpoint::Interface(InterfaceId::new("system_foreign_target")),
            artifact: Some(artifact(ACT_OBSERVATION_REPLY_LABEL, b"foreign-route")),
            invocation_metadata: None,
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_foreign")),
            to: Endpoint::Interface(InterfaceId::new("system_foreign_target")),
            artifact: Some(artifact(ACT_OBSERVATION_REPLY_LABEL, b"malformed-foreign")),
            invocation_metadata: Some(DeliveryVerdict::Admitted { seq: 7 }.into()),
            causal_floor: None,
        });

        let notifications: Vec<_> =
            project_movements_as_notifications(&run, &reply_interface(), &log.read_from(Seq(0)))
                .unwrap()
                .collect();
        let ReplyNotification::TurnCompleted { reply, .. } = notifications.last().unwrap() else {
            panic!("expected terminal notification")
        };
        assert_ne!(reply, &foreign_terminal);
    }

    #[test]
    fn non_invocation_exact_target_metadata_fails_closed() {
        let run = RunId::new("run-non-invocation-exact-target");
        let mut log = build_multistep_log(&run);
        let malformed = log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_fixture")),
            to: Endpoint::Interface(reply_interface()),
            artifact: Some(artifact(
                ACT_OBSERVATION_REPLY_LABEL,
                b"wrong-metadata-variant",
            )),
            invocation_metadata: Some(DeliveryVerdict::Admitted { seq: 0 }.into()),
            causal_floor: None,
        });

        let error =
            project_movements_as_notifications(&run, &reply_interface(), &log.read_from(Seq(0)))
                .err()
                .expect("an exact target with non-invocation metadata fails closed");
        assert_eq!(
            error,
            ReplyStreamError::InvalidOutboundMetadata(malformed.lamport)
        );
    }

    #[test]
    fn missing_artifact_fails_closed() {
        let run = RunId::new("run-corrupt");
        let mut log = InMemoryLog::new();
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceArrival,
            from: Endpoint::Interface(InterfaceId::new("system_fixture_artifact")),
            to: Endpoint::Interface(InterfaceId::new("system_worker_fixture")),
            artifact: None,
            invocation_metadata: metadata(&run, "arrival", "system_fixture_artifact"),
            causal_floor: None,
        });
        log.append(MovementDraft {
            trace: TraceId("rs".into()),
            kind: MovementKind::InterfaceOutbound,
            from: Endpoint::Interface(InterfaceId::new("world_provider_fixture")),
            to: Endpoint::Interface(InterfaceId::new("system_fixture_artifact_out")),
            artifact: Some(artifact(
                ACT_OBSERVATION_REPLY_LABEL,
                b"{\"label\":\"act.observation\"}",
            )),
            invocation_metadata: metadata(&run, "terminal", "system_fixture_artifact_out"),
            causal_floor: None,
        });
        let movements = log.read_from(Seq(0));
        let err = project_movements_as_notifications(&run, &reply_interface(), &movements)
            .err()
            .expect("a corrupt artifact-less movement fails closed, not an empty delta");
        assert!(matches!(
            err,
            ReplyStreamError::MissingArtifact(_, "InterfaceArrival")
        ));
    }
}
