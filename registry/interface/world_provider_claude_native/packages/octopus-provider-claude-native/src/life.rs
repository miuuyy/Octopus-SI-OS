use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use octopus_interface_transport::{InterfaceLifeFuture, InterfaceLiftContext, InterfaceLiftFn};
use octopus_interfaces::{InterfaceArtifactFile, InterfaceDescriptor, InterfaceInvocationEnvelope};
use octopus_run_authoring::{
    ProviderEmission, ProviderEmissionRoute, ProviderHandoffOutcome, ProviderHandoffSession, ProviderOutputHandoff,
    ProviderSourceRecovery, RunAuthoringResultIdentity,
};
use octopus_world_pyecho_life_runtime::{LedgerTailReader, OwnOutboundMovement};
use serde_json::{Value, json};

use crate::child::{Event, ItemKind, SdkChild, SessionConfig, TextInput, TurnStatus, Usage};
use crate::config::{ClaudeNativeConfig, verify_sdk_child};
use crate::controls::{ControlResult, TURN_CONTROL_LABEL, TurnControl, interrupted_result_payload};
use crate::journal::{InflightTurn, Journal, PendingCallRecord};
use crate::stream::LiveTurn;

const REPLY_LABEL: &str = "interface.artifact.output";
const TURN_ERROR_LABEL: &str = "turn.error";
const TOOL_CALL_LABEL: &str = "tool.call";
const AUTHENTICATION_FAILURE_CODE: &str = "provider.authentication.invalid";
const CHILD_ENVIRONMENT: [&str; 9] = ["HOME", "PATH", "USER", "LANG", "TMPDIR", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"];
const TURN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const CONTROL_READER: &str = "turn-control";
const CONTROL_BATCH: usize = 64;
const CUT_TURN_NOTE: &str = "Your answer to the previous message was cut off when the provider restarted. Answer that message again, in full.";

pub fn interface_life() -> InterfaceLiftFn {
    Arc::new(|ctx| Box::pin(provider_life(ctx)) as InterfaceLifeFuture)
}

struct Provider {
    ctx: InterfaceLiftContext,
    descriptor: InterfaceDescriptor,
    config: ClaudeNativeConfig,
    session: ProviderHandoffSession,
    journal: Journal,
    controls: LedgerTailReader,
    child_executable: PathBuf,
    claude_cwd: PathBuf,
    streams_root: PathBuf,
    provider_interface: String,
    child: Option<SdkChild>,
    open_sessions: BTreeMap<String, OpenSession>,
    pending_calls: BTreeMap<String, PendingCall>,
    /// Calls whose turn was interrupted or redirected before their result
    /// arrived. The result is closed as recorded, not returned to the model.
    interrupted_calls: BTreeMap<String, InterruptedCall>,
    /// Steer texts handed to the child per turn, awaiting the child's
    /// report that Claude took them up.
    steer_receipts: BTreeMap<String, Vec<(String, String)>>,
    /// Turns an operator control has interrupted; their end is an answer,
    /// not a failure.
    interrupting: std::collections::BTreeSet<String>,
}

struct OpenSession {
    fingerprint: String,
    tools: BTreeMap<String, String>,
}

struct PendingCall {
    reply_to: Option<Value>,
    run: String,
    session: String,
    turn: String,
    tool: String,
    live: LiveTurn,
    /// The call outlived the life that handed it: the Claude turn that made
    /// it is gone, and its result is carried into a resumed session.
    recovered: bool,
}

/// What a turn answers: a declared input message, or the rendered text of
/// another admitted artifact.
enum TurnInput<'a> {
    Declared { label: &'a str, contract: &'a crate::config::InputMessage },
    Text { label: &'a str, text: String },
}

impl TurnInput<'_> {
    fn label(&self) -> &str {
        match self {
            Self::Declared { label, .. } | Self::Text { label, .. } => label,
        }
    }
}

struct InterruptedCall {
    thread_id: Option<String>,
    turn: String,
}

enum TurnOutcome {
    Reply(TurnReply),
    ToolCall { call_id: String, tool: String, arguments: Value, claude_session_id: Option<String> },
    Interrupted { text: String, claude_session_id: Option<String> },
}

struct TurnReply {
    text: String,
    claude_session_id: Option<String>,
    usage: Option<Usage>,
}

struct TurnFailure {
    code: String,
    message: String,
}

impl TurnFailure {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

enum Drained {
    Event(Option<Event>),
    Controls,
    Wake,
}

async fn provider_life(ctx: InterfaceLiftContext) -> Result<(), String> {
    let descriptor = ctx.resolved_interface_descriptor()?;
    let config = ClaudeNativeConfig::from_descriptor(&descriptor)?;
    crate::config::verify_claude_grant(&ctx.interface_dir, &config.sdk_child.claude_executable)?;
    let registry_interface_root = ctx.root_slot("registry_interface_root").map_err(|error| error.to_string())?.to_path_buf();
    let child_executable = config.sdk_child_executable(&registry_interface_root)?;
    verify_sdk_child(&child_executable)?;
    let authoring_results = RunAuthoringResultIdentity::from_descriptor(&descriptor)?;
    let session = ProviderHandoffSession::new(&ctx)?;
    let mut reader = LedgerTailReader::open(&ctx)?;
    let controls = LedgerTailReader::open_named(&ctx, CONTROL_READER)?;
    let journal = Journal::open(&ctx.var_dir, ctx.required_host_io_limits()?.max_payload_bytes)?;
    let claude_cwd = ctx.var_dir.join("claude-cwd");
    std::fs::create_dir_all(&claude_cwd).map_err(|error| format!("claude working directory cannot be created: {error}"))?;
    let streams_root = ctx.var_dir.join("streams");
    crate::stream::retire_open_streams(&streams_root)?;
    let provider_interface = descriptor.id.as_str().to_string();
    let mut provider = Provider {
        ctx: ctx.clone(),
        descriptor,
        config,
        session,
        journal,
        controls,
        child_executable,
        claude_cwd,
        streams_root,
        provider_interface,
        child: None,
        open_sessions: BTreeMap::new(),
        pending_calls: BTreeMap::new(),
        interrupted_calls: BTreeMap::new(),
        steer_receipts: BTreeMap::new(),
        interrupting: std::collections::BTreeSet::new(),
    };
    provider.restore_pending_calls();
    ctx.signal_ready().await?;
    loop {
        if ctx.cancel.is_cancelled() {
            provider.shutdown().await;
            return Ok(());
        }
        for own in reader.read_new()? {
            if ctx.cancel.is_cancelled() {
                break;
            }
            if provider.journal.control_handled(own.movement.seq.0) {
                reader.commit_through(&own)?;
                continue;
            }
            let envelope = match reader.envelope(&own) {
                Ok(envelope) => envelope,
                Err(error) => {
                    ctx.hand_admitted_failure(own.movement.seq.0, "poisoned_provider_outbound", json!({"error": error})).await?;
                    reader.commit_through(&own)?;
                    continue;
                }
            };
            let manifest = envelope.read_artifact_manifest().map_err(|error| error.to_string())?;
            if authoring_results.consume_if_matches(&ctx, &own.movement, &envelope, &manifest.label).await? {
                reader.commit_through(&own)?;
                continue;
            }
            provider.process(&own, &envelope, &manifest.label).await?;
            reader.commit_through(&own)?;
        }
        tokio::select! {
            _ = ctx.cancel.cancelled() => {}
            result = reader.wait_for_change(&ctx.cancel) => result?,
        }
    }
}

impl Provider {
    /// Calls the previous life handed and never answered are still owed a
    /// turn: their results are carried into the run's resumed Claude session
    /// when they arrive.
    fn restore_pending_calls(&mut self) {
        for (run, entry) in self.journal.runs() {
            let Some(inflight) = &entry.inflight else { continue };
            let Some(record) = &inflight.pending_call else { continue };
            let turn = format!("{}-recovered", inflight.turn);
            let live = LiveTurn::new(
                self.streams_root.clone(),
                &self.provider_interface,
                run,
                entry.claude_session_id.as_deref(),
                &turn,
                inflight.source_seq,
            );
            self.pending_calls.insert(
                record.call_id.clone(),
                PendingCall {
                    reply_to: inflight.reply_to.clone(),
                    run: run.clone(),
                    session: session_name(run),
                    turn,
                    tool: record.tool.clone(),
                    live,
                    recovered: true,
                },
            );
        }
    }

    async fn process(&mut self, own: &OwnOutboundMovement, envelope: &InterfaceInvocationEnvelope, label: &str) -> Result<(), String> {
        let seq = own.movement.seq.0;
        let runtime = envelope.runtime().map_err(|error| error.to_string())?;
        let run = runtime.run.as_str().to_string();
        let refreshed = self.refresh_config();
        let pinned = self.ctx.pinned_run_record_bytes(envelope)?;
        let handoff = self
            .session
            .from_invocation_when_ready(&self.ctx, &self.descriptor, &runtime.run, envelope, seq, &pinned)
            .await
            .map_err(Into::<String>::into)?;
        let (recovery, _) = handoff.recover_source_with_outcomes().await?;
        let declared = self.config.input_messages.get(label).cloned();
        let reply_to = match declared {
            Some(_) => Some(serde_json::to_value(&envelope.artifact).map_err(|error| error.to_string())?),
            None => None,
        };
        let reply_to = reply_to.as_ref();
        if let Err(failure) = refreshed {
            return self.hand_failure(&handoff, reply_to, None, &failure).await;
        }
        let cut = self.journal.inflight(&run).is_some_and(|inflight| inflight.source_seq == seq && inflight.pending_call.is_none());
        match recovery {
            ProviderSourceRecovery::Completed => return Ok(()),
            ProviderSourceRecovery::Process => {}
            ProviderSourceRecovery::Interrupted { last_artifact_label } if !cut => {
                let failure = TurnFailure::new(
                    "transport_turn_interrupted",
                    format!("the provider restarted after handing '{last_artifact_label}'; the model call was not replayed"),
                );
                return self.hand_failure(&handoff, reply_to, None, &failure).await;
            }
            ProviderSourceRecovery::Interrupted { .. } => {}
            ProviderSourceRecovery::Bridged { invocation_movement_seq } => {
                let failure = TurnFailure::new(
                    "bridged_source_unsupported",
                    format!("source {seq} is bridged to invocation {invocation_movement_seq}, which this provider does not continue"),
                );
                return self.hand_failure(&handoff, reply_to, None, &failure).await;
            }
        }
        if label == TURN_CONTROL_LABEL {
            return self.control_between_turns(&handoff, envelope, &run, seq).await;
        }
        if let Some(input) = declared {
            let input = TurnInput::Declared { label, contract: &input };
            return self.begin_turn(&handoff, envelope, &pinned, &run, seq, input, reply_to, cut).await;
        }
        let bytes = match envelope.read_artifact_payload() {
            Ok(bytes) => bytes,
            Err(error) => return self.hand_failure(&handoff, None, None, &TurnFailure::new("input_unreadable", error.to_string())).await,
        };
        let source = serde_json::to_value(&envelope.artifact).map_err(|error| error.to_string())?;
        let text = match serde_json::from_slice::<Value>(&bytes) {
            Ok(payload) => match crate::tools::decode_tool_result(&payload) {
                Ok(Some(result)) => return self.continue_after_result(&handoff, envelope, &pinned, &run, seq, label, result).await,
                Ok(None) => crate::input::artifact_turn_text(label, movement_source(own).as_deref(), &payload),
                Err(error) => return self.hand_failure(&handoff, None, None, &TurnFailure::new("tool_result_malformed", error)).await,
            },
            Err(_) => match std::str::from_utf8(&bytes) {
                Ok(text) if !text.trim().is_empty() => Ok(text.to_string()),
                _ => Err(format!("input artifact '{label}' is neither JSON nor non-empty text")),
            },
        };
        match text {
            Ok(text) => self.begin_turn(&handoff, envelope, &pinned, &run, seq, TurnInput::Text { label, text }, Some(&source), cut).await,
            Err(error) => self.hand_failure(&handoff, Some(&source), None, &TurnFailure::new("input_unreadable", error)).await,
        }
    }

    /// Reads the interface's current descriptor from the registry before each
    /// turn. The client records a Home's model and reasoning effort as
    /// private configuration of this interface, so a change made between
    /// turns applies to the next turn here: the session fingerprint then
    /// differs and the Claude session is resumed with the new values. A
    /// changed SDK child or Claude executable is not adopted without the
    /// restart that re-verifies it.
    fn refresh_config(&mut self) -> Result<(), TurnFailure> {
        let registry_root = self
            .ctx
            .interface_dir
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or_else(|| TurnFailure::new("descriptor_unavailable", "the interface directory has no registry root"))?;
        let descriptor = octopus_interface_transport::resolve_interface_descriptor(registry_root, &self.ctx.interface)
            .map_err(|error| TurnFailure::new("descriptor_unavailable", error))?
            .ok_or_else(|| TurnFailure::new("descriptor_unavailable", "the interface descriptor is no longer in the registry"))?;
        let config = ClaudeNativeConfig::from_descriptor(&descriptor).map_err(|error| TurnFailure::new("descriptor_invalid", error))?;
        if config.sdk_child != self.config.sdk_child {
            return Err(TurnFailure::new(
                "descriptor_changed",
                "the SDK child or Claude executable changed since this life started; the life must restart to verify it",
            ));
        }
        self.config = config;
        self.descriptor = descriptor;
        Ok(())
    }

    /// A turn control that reaches the main reader finds no Claude turn
    /// running: either the run is waiting for an Octopus tool result, which
    /// the control abandons, or there is nothing left to interrupt.
    async fn control_between_turns(&mut self, handoff: &ProviderOutputHandoff, envelope: &InterfaceInvocationEnvelope, run: &str, seq: u64) -> Result<(), String> {
        let payload: Value = match envelope
            .read_artifact_payload()
            .map_err(|error| error.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()))
        {
            Ok(payload) => payload,
            Err(error) => return self.hand_failure(handoff, None, None, &TurnFailure::new("input_unreadable", error)).await,
        };
        let control = match TurnControl::decode(&payload) {
            Ok(control) => control,
            Err(error) => return self.hand_failure(handoff, None, None, &TurnFailure::new("turn_control_malformed", error)).await,
        };
        let waiting = self.pending_calls.iter().find(|(_, call)| call.run == run).map(|(id, _)| id.clone());
        let (acknowledged, thread_id, turn_id, detail, live) = match (&control, waiting) {
            (control, Some(call_id)) if control.interrupts() => {
                let pending = self.pending_calls.remove(&call_id).expect("found above");
                if !pending.recovered {
                    let _ = self.answer_refused_call(&pending.session, &pending.turn, &call_id).await;
                }
                let thread_id = pending.live.thread_id().map(str::to_owned);
                self.interrupted_calls.insert(call_id, InterruptedCall { thread_id: thread_id.clone(), turn: pending.turn.clone() });
                self.journal.record_inflight(run, None)?;
                (true, thread_id, Some(pending.turn), json!({"interrupted": true, "exact_result_pending": true}), Some(pending.live))
            }
            (control, None) if control.interrupts() => {
                (true, None, None, json!({"already_terminal": true, "external_effect_dispatched": false}), None)
            }
            (control, _) => (
                false,
                None,
                None,
                json!({"error": format!("the '{}' control is not supported by this provider", control.action())}),
                None,
            ),
        };
        let result = ControlResult {
            control_id: control.control_id(),
            action: control.action(),
            acknowledged,
            source_movement_seq: seq,
            thread_id: thread_id.as_deref(),
            turn_id: turn_id.as_deref(),
            detail,
        };
        self.hand_terminal(handoff, result.label(), None, live, result.payload()).await
    }

    async fn begin_turn(
        &mut self,
        handoff: &ProviderOutputHandoff,
        envelope: &InterfaceInvocationEnvelope,
        pinned: &[u8],
        run: &str,
        seq: u64,
        input: TurnInput<'_>,
        reply_to: Option<&Value>,
        cut: bool,
    ) -> Result<(), String> {
        let label = input.label().to_string();
        let resume = self.journal.claude_session_id(run);
        let history = handoff.current_session_context_artifacts().await;
        let selection = self.config.instruction_selection()?;
        let recovering = cut && resume.is_some();
        let context = match (&input, recovering) {
            (_, true) => crate::context::assemble_with_text(envelope, pinned, &selection, &history, false, CUT_TURN_NOTE),
            (TurnInput::Declared { contract, .. }, false) => {
                crate::context::assemble(envelope, pinned, &label, contract, &selection, &history, resume.is_none())
            }
            (TurnInput::Text { text, .. }, false) => {
                crate::context::assemble_with_text(envelope, pinned, &selection, &history, resume.is_none(), text)
            }
        };
        let context = match context {
            Ok(context) => context,
            Err(error) => return self.hand_failure(handoff, reply_to, None, &TurnFailure::new("context_unavailable", error)).await,
        };
        let catalog = match crate::tools::admitted_catalog(handoff, envelope, &self.config.model_hidden_tools) {
            Ok(catalog) => catalog,
            Err(error) => return self.hand_failure(handoff, reply_to, None, &TurnFailure::new("tool_catalog_invalid", error)).await,
        };
        let session = session_name(run);
        let turn = if recovering { format!("source-{seq}-recovered") } else { format!("source-{seq}") };
        let mut live = LiveTurn::new(self.streams_root.clone(), &self.provider_interface, run, resume.as_deref(), &turn, seq);
        self.journal.record_inflight(run, Some(InflightTurn { source_seq: seq, turn: turn.clone(), reply_to: reply_to.cloned(), pending_call: None }))?;
        let waiting = self.pending_calls.iter().find(|(_, call)| call.run == run && !call.recovered).map(|(id, _)| id.clone());
        let started = match waiting {
            Some(call_id) => self.redirect_waiting_turn(&call_id, &turn, &context, &label, envelope).await,
            None => self.start_turn(&session, &turn, resume, context, catalog).await,
        };
        let outcome = match started {
            Ok(()) => self.drain(handoff, run, seq, &session, &turn, &mut live).await,
            Err(failure) => Err(failure),
        };
        self.finish(handoff, reply_to, run, seq, &session, &turn, live, outcome).await
    }

    /// A new message for a run whose Claude turn is waiting on an Octopus
    /// tool redirects that turn: the waiting call is abandoned, its result
    /// closed as recorded when it arrives, and the message runs as the next
    /// Claude turn in the same session.
    async fn redirect_waiting_turn(
        &mut self,
        call_id: &str,
        turn: &str,
        context: &crate::context::TurnContext,
        label: &str,
        envelope: &InterfaceInvocationEnvelope,
    ) -> Result<(), TurnFailure> {
        let pending = self.pending_calls.remove(call_id).expect("found by caller");
        let _ = pending.live.abandon();
        self.interrupted_calls.insert(
            call_id.to_string(),
            InterruptedCall { thread_id: self.journal.claude_session_id(&pending.run), turn: pending.turn.clone() },
        );
        let control_id = steer_control_id(label, envelope, &self.config);
        self.steer_receipts.entry(turn.to_string()).or_default().push((context.prompt.clone(), control_id));
        self.child_mut()?
            .steer(&pending.session, &pending.turn, Some(turn), vec![TextInput::new(context.prompt.clone())])
            .await
            .map_err(|error| TurnFailure::new(&error.code, error.message))
    }

    async fn continue_after_result(
        &mut self,
        handoff: &ProviderOutputHandoff,
        envelope: &InterfaceInvocationEnvelope,
        pinned: &[u8],
        run: &str,
        seq: u64,
        label: &str,
        result: octopus_tool_handlers::ToolResultArtifact,
    ) -> Result<(), String> {
        let Some(call_id) = result.correlation_id.clone() else {
            return self.hand_failure(handoff, None, None, &TurnFailure::new("tool_result_uncorrelated", "the tool result carries no correlation_id")).await;
        };
        if let Some(interrupted) = self.interrupted_calls.remove(&call_id) {
            let source = serde_json::to_value(&envelope.artifact).map_err(|error| error.to_string())?;
            let payload = interrupted_result_payload(&result, &source, interrupted.thread_id.as_deref(), &interrupted.turn);
            return self.hand_terminal(handoff, crate::controls::INTERRUPTED_RESULT_LABEL, None, None, payload).await;
        }
        let cut_after_result = self.journal.inflight(run).filter(|inflight| inflight.source_seq == seq && inflight.pending_call.is_none()).cloned();
        let mut pending = match (self.pending_calls.remove(&call_id), cut_after_result) {
            (Some(pending), _) => pending,
            (None, Some(inflight)) => {
                let turn = format!("{}-recovered", inflight.turn);
                let live = LiveTurn::new(self.streams_root.clone(), &self.provider_interface, run, self.journal.claude_session_id(run).as_deref(), &turn, seq);
                PendingCall { reply_to: inflight.reply_to, run: run.to_string(), session: session_name(run), turn, tool: result.tool.clone(), live, recovered: true }
            }
            (None, None) => {
                let failure = TurnFailure::new(
                    "tool_result_not_pending",
                    format!("tool result for call '{call_id}' arrived, but this provider holds no open Claude turn for it; the model call was not replayed"),
                );
                return self.hand_failure(handoff, None, None, &failure).await;
            }
        };
        if result.tool != pending.tool || result.label != label {
            let failure = TurnFailure::new(
                "tool_result_mismatch",
                format!("call '{call_id}' was for tool '{}', the result names tool '{}' with label '{}'", pending.tool, result.tool, result.label),
            );
            if !pending.recovered {
                let _ = self.answer_refused_call(&pending.session, &pending.turn, &call_id).await;
            }
            return self.hand_failure(handoff, pending.reply_to.as_ref(), Some(pending.live), &failure).await;
        }
        let run = pending.run.clone();
        self.journal.record_inflight(&run, Some(InflightTurn { source_seq: seq, turn: pending.turn.clone(), reply_to: pending.reply_to.clone(), pending_call: None }))?;
        let text = crate::tools::model_result_text(&result);
        let answered = if pending.recovered {
            self.resume_with_result(handoff, envelope, pinned, &pending, &call_id, &text).await
        } else {
            match self.child_mut() {
                Ok(child) => child
                    .tool_result(&pending.session, &pending.turn, &call_id, vec![TextInput::new(text)], !result.ok)
                    .await
                    .map_err(|error| TurnFailure::new(&error.code, error.message)),
                Err(failure) => Err(failure),
            }
        };
        pending.live.source_movement_seq = seq;
        let outcome = match answered {
            Ok(()) => self.drain(handoff, &run, seq, &pending.session, &pending.turn, &mut pending.live).await,
            Err(failure) => Err(failure),
        };
        self.finish(handoff, pending.reply_to.as_ref(), &run, seq, &pending.session, &pending.turn, pending.live, outcome).await
    }

    /// The Claude turn that made this call died with the previous life. Its
    /// session is resumed and the result reaches Claude as a message, since
    /// no call is open for it any more.
    async fn resume_with_result(
        &mut self,
        handoff: &ProviderOutputHandoff,
        envelope: &InterfaceInvocationEnvelope,
        pinned: &[u8],
        pending: &PendingCall,
        call_id: &str,
        result_text: &str,
    ) -> Result<(), TurnFailure> {
        let resume = self.journal.claude_session_id(&pending.run);
        let selection = self.config.instruction_selection().map_err(|error| TurnFailure::new("context_unavailable", error))?;
        let history = handoff.current_session_context_artifacts().await;
        let carried = format!(
            "The provider restarted while your call to the tool `{}` (correlation {call_id}) was running, so that call is no longer open. Octopus completed it; its result follows. Continue from this result.\n\n{result_text}",
            crate::tools::encode_tool_name(&pending.tool).unwrap_or_else(|_| pending.tool.clone()),
        );
        let context = crate::context::assemble_with_text(envelope, pinned, &selection, &history, resume.is_none(), &carried)
            .map_err(|error| TurnFailure::new("context_unavailable", error))?;
        let catalog = crate::tools::admitted_catalog(handoff, envelope, &self.config.model_hidden_tools)
            .map_err(|error| TurnFailure::new("tool_catalog_invalid", error))?;
        self.start_turn(&pending.session, &pending.turn, resume, context, catalog).await
    }

    async fn finish(
        &mut self,
        handoff: &ProviderOutputHandoff,
        reply_to: Option<&Value>,
        run: &str,
        seq: u64,
        session: &str,
        turn: &str,
        mut live: LiveTurn,
        outcome: Result<TurnOutcome, TurnFailure>,
    ) -> Result<(), String> {
        self.steer_receipts.remove(turn);
        self.interrupting.remove(turn);
        match outcome {
            Ok(TurnOutcome::Reply(reply)) => {
                if let Some(claude) = &reply.claude_session_id {
                    self.journal.record_session(run, claude)?;
                }
                self.journal.record_inflight(run, None)?;
                self.hand_reply(handoff, reply_to, live, turn, reply, "completed").await
            }
            Ok(TurnOutcome::Interrupted { text, claude_session_id }) => {
                if let Some(claude) = &claude_session_id {
                    self.journal.record_session(run, claude)?;
                }
                self.journal.record_inflight(run, None)?;
                let reply = TurnReply { text, claude_session_id, usage: None };
                self.hand_reply(handoff, reply_to, live, turn, reply, "interrupted").await
            }
            Ok(TurnOutcome::ToolCall { call_id, tool, arguments, claude_session_id }) => {
                if let Some(claude) = &claude_session_id {
                    self.journal.record_session(run, claude)?;
                }
                self.journal.record_inflight(
                    run,
                    Some(InflightTurn {
                        source_seq: seq,
                        turn: turn.to_string(),
                        reply_to: reply_to.cloned(),
                        pending_call: Some(PendingCallRecord { call_id: call_id.clone(), tool: tool.clone() }),
                    }),
                )?;
                match self.hand_tool_call(handoff, &tool, &arguments, &call_id, &mut live).await {
                    Ok(true) => {
                        self.pending_calls.insert(
                            call_id,
                            PendingCall {
                                reply_to: reply_to.cloned(),
                                run: run.to_string(),
                                session: session.to_string(),
                                turn: turn.to_string(),
                                tool,
                                live,
                                recovered: false,
                            },
                        );
                        Ok(())
                    }
                    Ok(false) => {
                        self.journal.record_inflight(run, None)?;
                        let _ = self.answer_refused_call(session, turn, &call_id).await;
                        live.abandon()
                    }
                    Err(error) => {
                        self.journal.record_inflight(run, None)?;
                        let _ = self.answer_refused_call(session, turn, &call_id).await;
                        let _ = live.abandon();
                        Err(error)
                    }
                }
            }
            Err(failure) => {
                self.journal.record_inflight(run, None)?;
                self.hand_failure(handoff, reply_to, Some(live), &failure).await
            }
        }
    }

    async fn answer_refused_call(&mut self, session: &str, turn: &str, call_id: &str) -> Result<(), TurnFailure> {
        let child = self.child_mut()?;
        child
            .tool_result(session, turn, call_id, vec![TextInput::new("Octopus did not admit this tool call.")], true)
            .await
            .map_err(|error| TurnFailure::new(&error.code, error.message))?;
        child.interrupt(session, turn).await.map_err(|error| TurnFailure::new(&error.code, error.message))
    }

    async fn start_turn(
        &mut self,
        session: &str,
        turn: &str,
        resume: Option<String>,
        context: crate::context::TurnContext,
        catalog: crate::tools::ToolCatalog,
    ) -> Result<(), TurnFailure> {
        self.ensure_child().await?;
        let fingerprint = format!(
            "{}:{}:{}:{}",
            self.config.model,
            self.config.reasoning_effort,
            context.system_fingerprint(),
            blake3::hash(catalog.fingerprint_material().as_bytes()).to_hex()
        );
        let resume = match self.open_sessions.get(session) {
            Some(open) if open.fingerprint == fingerprint => None,
            Some(_) => {
                let _ = self.child_mut()?.close(session).await;
                self.open_sessions.remove(session);
                resume
            }
            None => resume,
        };
        if !self.open_sessions.contains_key(session) {
            let config = SessionConfig {
                model: self.config.model.clone(),
                system_prompt: context.system_prompt.clone(),
                cwd: self.claude_cwd.display().to_string(),
                effort: Some(self.config.reasoning_effort.clone()),
                resume,
                max_turns: None,
                tools: catalog.specs.clone(),
            };
            self.child_mut()?.start(session, config).await.map_err(|error| self.child_failed(error))?;
            self.open_sessions.insert(session.to_string(), OpenSession { fingerprint, tools: catalog.tools_by_model_name });
        }
        self.child_mut()?
            .turn(session, turn, vec![TextInput::new(context.prompt)])
            .await
            .map_err(|error| TurnFailure::new(&error.code, error.message))
    }

    async fn drain(
        &mut self,
        handoff: &ProviderOutputHandoff,
        run: &str,
        source_seq: u64,
        session: &str,
        turn: &str,
        live: &mut LiveTurn,
    ) -> Result<TurnOutcome, TurnFailure> {
        let mut text = Vec::new();
        let mut usage = None;
        let mut failure: Option<TurnFailure> = None;
        let mut claude_session_id = live.thread_id().map(str::to_owned);
        let mut interrupted = false;
        let deadline = tokio::time::Instant::now() + TURN_TIMEOUT;
        let stream_failed = |error: String| TurnFailure::new("turn_stream_failed", error);
        loop {
            let wake = live.flush_due().map_or(deadline, |due| due.min(deadline));
            let cancel = self.ctx.cancel.clone();
            let drained = {
                let child = self.child.as_mut().ok_or_else(|| TurnFailure::new("child_unavailable", "the SDK child is not running"))?;
                let controls = &mut self.controls;
                tokio::select! {
                    event = child.next_event() => Drained::Event(event),
                    changed = controls.wait_for_change(&cancel) => {
                        changed.map_err(|error| TurnFailure::new("control_reader_failed", error))?;
                        Drained::Controls
                    }
                    _ = tokio::time::sleep_until(wake) => Drained::Wake,
                }
            };
            let event = match drained {
                Drained::Event(Some(event)) => event,
                Drained::Event(None) => return Err(self.child_lost("the SDK child closed its event stream")),
                Drained::Controls => {
                    if let Err(error) = self.poll_controls(handoff, run, source_seq, session, turn, live).await {
                        let _ = self.child_mut()?.interrupt(session, turn).await;
                        return Err(TurnFailure::new("control_handling_failed", error));
                    }
                    continue;
                }
                Drained::Wake if wake < deadline => {
                    live.flush().map_err(stream_failed)?;
                    continue;
                }
                Drained::Wake => {
                    let _ = self.child_mut()?.interrupt(session, turn).await;
                    return Err(TurnFailure::new("turn_timeout", "the Claude turn exceeded its time limit and was interrupted"));
                }
            };
            match event {
                Event::Session { session: name, claude_session_id: id, .. } if name == session => {
                    live.set_thread_id(&id).map_err(stream_failed)?;
                    self.journal.record_session(run, &id).map_err(|error| TurnFailure::new("journal_failed", error))?;
                    claude_session_id = Some(id);
                }
                Event::Item { session: name, turn: t, item } if name == session && t == turn => {
                    if item.kind == ItemKind::Text {
                        text.push(item.text);
                    }
                }
                Event::TextDelta { session: name, turn: t, message_id, text } if name == session && t == turn => {
                    if let Err(error) = live.delta(&message_id, &text) {
                        let _ = self.child_mut()?.interrupt(session, turn).await;
                        return Err(stream_failed(error));
                    }
                }
                Event::InputRecorded { session: name, turn: t, text: recorded, .. } if name == session && t == turn => {
                    let receipts = self.steer_receipts.entry(turn.to_string()).or_default();
                    if let Some(index) = receipts.iter().position(|(steer, _)| steer == &recorded) {
                        let (_, control_id) = receipts.remove(index);
                        text.clear();
                        live.input_recorded(&control_id, &format!("steer-{control_id}")).map_err(stream_failed)?;
                    }
                }
                Event::ToolCall { session: name, turn: t, call_id, name: model_name, input } if name == session && t == turn => {
                    let tool = self.open_sessions.get(session).and_then(|open| open.tools.get(&model_name)).cloned();
                    let Some(tool) = tool else {
                        let _ = self.child_mut()?.interrupt(session, turn).await;
                        return Err(TurnFailure::new("tool_not_admitted", format!("Claude called '{model_name}', which is not in this session's admitted catalog")));
                    };
                    return Ok(TurnOutcome::ToolCall { call_id, tool, arguments: Value::Object(input), claude_session_id });
                }
                Event::Usage { session: name, turn: t, usage: observed, .. } if name == session && t == turn => usage = Some(observed),
                Event::Error { fatal: true, code, message, .. } => {
                    self.child = None;
                    self.open_sessions.clear();
                    self.pending_calls.clear();
                    return Err(TurnFailure::new(&code, message));
                }
                Event::Error { session: Some(name), code, message, .. } if name == session => {
                    failure.get_or_insert(TurnFailure::new(&code, message));
                }
                Event::TurnEnd { session: name, turn: t, status, result, claude_session_id: id } if name == session && t == turn => {
                    if id.is_some() {
                        claude_session_id = id;
                    }
                    return match status {
                        TurnStatus::Completed => {
                            let text = result.filter(|result| !result.trim().is_empty()).unwrap_or_else(|| text.join("\n\n"));
                            if text.trim().is_empty() {
                                Err(TurnFailure::new("empty_reply", "Claude Code completed the turn without text"))
                            } else {
                                Ok(TurnOutcome::Reply(TurnReply { text, claude_session_id, usage }))
                            }
                        }
                        TurnStatus::Interrupted if interrupted => {
                            Ok(TurnOutcome::Interrupted { text: live.streamed_text().unwrap_or_else(|| text.join("\n\n")), claude_session_id })
                        }
                        TurnStatus::Interrupted => Err(TurnFailure::new("turn_interrupted", "the Claude turn was interrupted")),
                        TurnStatus::Failed => {
                            self.open_sessions.remove(session);
                            Err(failure.unwrap_or_else(|| TurnFailure::new("claude_turn_failed", "the Claude turn failed")))
                        }
                    };
                }
                _ => {}
            }
            if !interrupted && self.turn_interrupted(turn) {
                interrupted = true;
            }
        }
    }

    fn turn_interrupted(&self, turn: &str) -> bool {
        self.interrupting.contains(turn)
    }

    /// Reads the movements that arrived while this turn runs and answers the
    /// ones that control it: an `operator.turn.control` interrupts Claude, a
    /// new declared input for the same run redirects it. Each is answered on
    /// the running turn's handoff and journaled, so the main reader does not
    /// start it as a turn of its own.
    async fn poll_controls(
        &mut self,
        handoff: &ProviderOutputHandoff,
        run: &str,
        source_seq: u64,
        session: &str,
        turn: &str,
        live: &mut LiveTurn,
    ) -> Result<(), String> {
        loop {
            let batch = self.controls.read_new_uncommitted_limit(CONTROL_BATCH)?;
            if batch.is_empty() {
                return Ok(());
            }
            for own in batch {
                let seq = own.movement.seq.0;
                if seq <= source_seq || self.journal.control_handled(seq) {
                    continue;
                }
                let Ok(envelope) = self.controls.envelope(&own) else { continue };
                let Ok(runtime) = envelope.runtime() else { continue };
                if runtime.run.as_str() != run {
                    continue;
                }
                let Ok(manifest) = envelope.read_artifact_manifest() else { continue };
                let label = manifest.label.clone();
                let is_control = label == TURN_CONTROL_LABEL;
                let is_steer = self.config.input_messages.contains_key(&label);
                if !is_control && !is_steer {
                    continue;
                }
                let payload: Value = serde_json::from_slice(&envelope.read_artifact_payload().map_err(|error| error.to_string())?)
                    .map_err(|error| format!("control artifact is not JSON: {error}"))?;
                let thread_id = live.thread_id().map(str::to_owned);
                let result = if is_control {
                    self.interrupt_running_turn(&payload, seq, session, turn, thread_id.as_deref()).await
                } else {
                    self.steer_running_turn(&label, &payload, &envelope, seq, session, turn, thread_id.as_deref()).await
                };
                let (acknowledged, control_id, action, detail) = result?;
                let control = ControlResult {
                    control_id: &control_id,
                    action,
                    acknowledged,
                    source_movement_seq: seq,
                    thread_id: thread_id.as_deref(),
                    turn_id: Some(turn),
                    detail,
                };
                let bytes = serde_json::to_vec(&control.payload()).map_err(|error| error.to_string())?;
                let artifact = InterfaceArtifactFile::write(self.ctx.var_dir.join("emitted"), control.label(), &bytes).map_err(|error| error.to_string())?;
                live.boundary(control.label(), &artifact)?;
                handoff
                    .hand_from_input(ProviderEmission { artifact, route: ProviderEmissionRoute::ClientProjection }, false, seq, acknowledged)
                    .await?;
                self.journal.record_handled_control(seq)?;
                self.controls.commit_through(&own)?;
            }
        }
    }

    async fn interrupt_running_turn(
        &mut self,
        payload: &Value,
        seq: u64,
        session: &str,
        turn: &str,
        _thread_id: Option<&str>,
    ) -> Result<(bool, String, &'static str, Value), String> {
        let control = match TurnControl::decode(payload) {
            Ok(control) => control,
            Err(error) => return Ok((false, format!("movement-{seq}"), "decode", json!({"error": error}))),
        };
        if !control.interrupts() {
            let detail = json!({"error": format!("the '{}' control is not supported while a turn is running", control.action())});
            return Ok((false, control.control_id().to_string(), control.action(), detail));
        }
        match self.child_mut().map_err(|failure| failure.message) {
            Ok(child) => match child.interrupt(session, turn).await {
                Ok(()) => {
                    self.interrupting.insert(turn.to_string());
                    Ok((true, control.control_id().to_string(), control.action(), json!({"interrupted": true, "exact_result_pending": false})))
                }
                Err(error) => Ok((false, control.control_id().to_string(), control.action(), json!({"error": error.to_string()}))),
            },
            Err(error) => Ok((false, control.control_id().to_string(), control.action(), json!({"error": error}))),
        }
    }

    async fn steer_running_turn(
        &mut self,
        label: &str,
        payload: &Value,
        envelope: &InterfaceInvocationEnvelope,
        _seq: u64,
        session: &str,
        turn: &str,
        _thread_id: Option<&str>,
    ) -> Result<(bool, String, &'static str, Value), String> {
        let control_id = steer_control_id(label, envelope, &self.config);
        let contract = self.config.input_messages.get(label).cloned().expect("declared by caller");
        let text = match crate::input::decode(label, &contract, payload) {
            Ok(decoded) => decoded.prompt_text(),
            Err(error) => return Ok((false, control_id, "steer", json!({"error": error}))),
        };
        if self.turn_interrupted(turn) {
            return Ok((false, control_id, "steer", json!({"error": "the turn is being interrupted"})));
        }
        let steered = match self.child_mut().map_err(|failure| failure.message) {
            Ok(child) => child.steer(session, turn, None, vec![TextInput::new(text.clone())]).await.map_err(|error| error.to_string()),
            Err(error) => Err(error),
        };
        match steered {
            Ok(()) => {
                self.steer_receipts.entry(turn.to_string()).or_default().push((text, control_id.clone()));
                Ok((true, control_id, "steer", json!({"stage": "accepted_by_provider", "consumption_proven": false})))
            }
            Err(error) => Ok((false, control_id, "steer", json!({"error": error}))),
        }
    }

    async fn ensure_child(&mut self) -> Result<(), TurnFailure> {
        if self.child.is_some() {
            return Ok(());
        }
        let environment = CHILD_ENVIRONMENT
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(*name), value)))
            .collect::<Vec<_>>();
        let child = SdkChild::spawn(
            &self.child_executable,
            &self.config.sdk_child.claude_executable,
            environment,
            &self.claude_cwd,
        )
        .await
        .map_err(|error| TurnFailure::new(&error.code, error.message))?;
        self.child = Some(child);
        self.open_sessions.clear();
        Ok(())
    }

    fn child_mut(&mut self) -> Result<&mut SdkChild, TurnFailure> {
        self.child.as_mut().ok_or_else(|| TurnFailure::new("child_unavailable", "the SDK child is not running"))
    }

    fn child_failed(&mut self, error: crate::child::ChildError) -> TurnFailure {
        if matches!(error.code.as_str(), "child_exited" | "child_write_failed" | "child_failed" | "child_protocol_violation") {
            self.child = None;
            self.open_sessions.clear();
            self.pending_calls.clear();
        }
        TurnFailure::new(&error.code, error.message)
    }

    fn child_lost(&mut self, message: &str) -> TurnFailure {
        self.child = None;
        self.open_sessions.clear();
        self.pending_calls.clear();
        TurnFailure::new("child_exited", message)
    }

    async fn hand_reply(
        &mut self,
        handoff: &ProviderOutputHandoff,
        reply_to: Option<&Value>,
        live: LiveTurn,
        turn: &str,
        reply: TurnReply,
        lifecycle: &str,
    ) -> Result<(), String> {
        let item_id = live.reply_item_id(&reply.text).unwrap_or_else(|| format!("msg_{turn}"));
        let mut value = json!({
            "thread_id": reply.claude_session_id,
            "turn_id": turn,
            "item_id": item_id,
            "text": reply.text,
            "phase": "final_answer",
            "lifecycle": lifecycle,
        });
        if let Some(usage) = reply.usage {
            value["provider_token_usage"] = json!({
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
                "cached_input_tokens": usage.cache_read_input_tokens,
                "cache_creation_input_tokens": usage.cache_creation_input_tokens,
            });
        }
        self.hand_terminal(handoff, REPLY_LABEL, reply_to, Some(live), value).await
    }

    async fn hand_tool_call(&mut self, handoff: &ProviderOutputHandoff, tool: &str, arguments: &Value, call_id: &str, live: &mut LiveTurn) -> Result<bool, String> {
        let value = json!({"tool": tool, "arguments": arguments, "correlation_id": call_id});
        let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        let artifact = InterfaceArtifactFile::write(self.ctx.var_dir.join("emitted"), TOOL_CALL_LABEL, &bytes).map_err(|error| error.to_string())?;
        live.boundary(TOOL_CALL_LABEL, &artifact)?;
        let outcome = handoff
            .hand_final(ProviderEmission { artifact, route: ProviderEmissionRoute::Tool { tool: tool.to_string() } })
            .await?;
        Ok(matches!(outcome, ProviderHandoffOutcome::Admitted { .. }))
    }

    async fn hand_failure(&mut self, handoff: &ProviderOutputHandoff, reply_to: Option<&Value>, live: Option<LiveTurn>, failure: &TurnFailure) -> Result<(), String> {
        let mut value = json!({
            "message": failure.message,
            "will_retry": false,
            "additional_details": {"code": failure.code},
        });
        if failure.code == "claude_authentication_failed" {
            value["failure_code"] = Value::String(AUTHENTICATION_FAILURE_CODE.into());
        }
        self.hand_terminal(handoff, TURN_ERROR_LABEL, reply_to, live, value).await
    }

    /// Hands a client-visible terminal output. `reply_to` is the admitted
    /// turn input this output answers. It is recorded as `in_reply_to`, the
    /// same link the Codex provider writes, so the client can attribute the
    /// output to its source; for a voice delegation that is what lets the
    /// call speak this answer under the delegation's ids. A turn that
    /// streamed hands its sealed `turn.stream` first, as Codex hands its
    /// terminal group.
    async fn hand_terminal(
        &mut self,
        handoff: &ProviderOutputHandoff,
        label: &str,
        reply_to: Option<&Value>,
        live: Option<LiveTurn>,
        mut value: Value,
    ) -> Result<(), String> {
        if let Some(source) = reply_to {
            value["in_reply_to"] = source.clone();
        }
        let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        let artifact = InterfaceArtifactFile::write(self.ctx.var_dir.join("emitted"), label, &bytes).map_err(|error| error.to_string())?;
        if let Some(stream) = live.map(|live| live.seal(label, &artifact)).transpose()?.flatten() {
            let outcomes = handoff
                .hand_progress_batch(vec![ProviderEmission { artifact: stream, route: ProviderEmissionRoute::ClientProjection }])
                .await?;
            if !outcomes.iter().all(|outcome| matches!(outcome, ProviderHandoffOutcome::Admitted { .. })) {
                return Ok(());
            }
        }
        handoff
            .hand_final(ProviderEmission { artifact, route: ProviderEmissionRoute::ClientProjection })
            .await
            .map(|_| ())
    }

    async fn shutdown(&mut self) {
        if let Some(child) = self.child.take() {
            let _ = child.shutdown().await;
        }
    }
}

/// The id the client gave a steering message, which its acknowledgement and
/// its recorded-input row must repeat; the artifact id when it gave none.
fn steer_control_id(label: &str, envelope: &InterfaceInvocationEnvelope, config: &ClaudeNativeConfig) -> String {
    let identity = config.input_messages.get(label).and_then(|contract| contract.identity_field.clone());
    let from_payload = identity.and_then(|field| {
        let payload: Value = serde_json::from_slice(&envelope.read_artifact_payload().ok()?).ok()?;
        payload.get(&field)?.as_str().map(str::to_owned)
    });
    from_payload.unwrap_or_else(|| envelope.artifact.id.as_str().to_string())
}

/// The interface a movement came from, as the ledger records it.
fn movement_source(own: &OwnOutboundMovement) -> Option<String> {
    serde_json::to_value(&own.movement.from).ok()?.get("Interface")?.as_str().map(str::to_owned)
}

fn session_name(run: &str) -> String {
    let digest = blake3::hash(run.as_bytes()).to_hex();
    format!("run-{}", &digest[..32])
}
