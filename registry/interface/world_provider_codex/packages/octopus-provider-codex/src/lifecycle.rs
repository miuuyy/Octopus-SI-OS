use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use octopus_descriptors::{ArtifactId, InterfaceId, MarkId, VerifierId};
use octopus_interfaces::{
    ArtifactBindingPayload, InterfaceArtifactFile, RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactRef, ContentHash, InvocationMetadata, RequiredVerifiedArtifact};
use sha2::{Digest, Sha256};

pub(crate) const LIFECYCLE_CONNECTION_LABEL: &str = "lifecycle.connection";
pub(crate) const LIFECYCLE_EVENT_LABEL: &str = "lifecycle.event";
pub(crate) const LIFECYCLE_DECISION_LABEL: &str = "lifecycle.decision";
pub(crate) const LIFECYCLE_HANDLER_PROMPT_LABEL: &str = "lifecycle.handler.prompt";
pub(crate) const LIFECYCLE_HANDLER_TASK_LABEL: &str = "lifecycle.handler.task";
pub(crate) const LIFECYCLE_HANDLER_OUTPUT_LABEL: &str = "lifecycle.handler.output";
pub(crate) const LIFECYCLE_INTERFACE: &str = "system_lifecycle_events";
pub(crate) const LIFECYCLE_DECISION_MARK: &str = "lifecycle.decision.allowed";
pub(crate) const LIFECYCLE_DECISION_VERIFIER: &str = "lifecycle_decision";

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecycleEventName {
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PreCompact,
    PostCompact,
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    SubagentStart,
    SubagentStop,
    Stop,
    Interrupt,
}

impl LifecycleEventName {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "pre_tool_use",
            Self::PermissionRequest => "permission_request",
            Self::PostToolUse => "post_tool_use",
            Self::PreCompact => "pre_compact",
            Self::PostCompact => "post_compact",
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::UserPromptSubmit => "user_prompt_submit",
            Self::SubagentStart => "subagent_start",
            Self::SubagentStop => "subagent_stop",
            Self::Stop => "stop",
            Self::Interrupt => "interrupt",
        }
    }

    pub(crate) fn positive_decision(self) -> Option<&'static str> {
        match self {
            Self::PreToolUse | Self::PermissionRequest => Some("allow"),
            Self::PostToolUse
            | Self::PreCompact
            | Self::PostCompact
            | Self::SessionStart
            | Self::UserPromptSubmit
            | Self::SubagentStop
            | Self::Stop => Some("continue"),
            Self::SessionEnd | Self::SubagentStart | Self::Interrupt => None,
        }
    }

    fn matcher_is_ignored(self) -> bool {
        matches!(self, Self::UserPromptSubmit | Self::Stop | Self::Interrupt)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecycleMode {
    Sync,
    Async,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecycleHandlerType {
    Command,
    McpTool,
    Prompt,
    Agent,
}

impl LifecycleHandlerType {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::McpTool => "mcp_tool",
            Self::Prompt => "prompt",
            Self::Agent => "agent",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum LifecycleTrust {
    Managed,
    Trusted,
    Untrusted,
    Modified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum LifecycleSourceLayer {
    PackagedDefaults,
    System,
    User,
    Project,
    Session,
    Plugin,
    Mdm,
    EnterpriseManaged,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleSourceEvidence {
    layer: LifecycleSourceLayer,
    path: String,
    trust: LifecycleTrust,
    handler_content_sha256: String,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LifecycleHandlerDestination {
    Command {
        tool: String,
        arguments: serde_json::Value,
        result_pointer: String,
    },
    McpTool {
        interface: InterfaceId,
        operation: String,
        request_label: String,
        input: serde_json::Value,
        result_pointer: String,
    },
    Prompt {
        provider_interface: InterfaceId,
        prompt: String,
    },
    Agent {
        worker_interface: InterfaceId,
        task: String,
    },
}

impl LifecycleHandlerDestination {
    fn handler_type(&self) -> LifecycleHandlerType {
        match self {
            Self::Command { .. } => LifecycleHandlerType::Command,
            Self::McpTool { .. } => LifecycleHandlerType::McpTool,
            Self::Prompt { .. } => LifecycleHandlerType::Prompt,
            Self::Agent { .. } => LifecycleHandlerType::Agent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleHandler {
    id: String,
    event: LifecycleEventName,
    emitter_run: String,
    owner_run: String,
    mode: LifecycleMode,
    order: u32,
    enabled: bool,
    matcher: Option<String>,
    source: LifecycleSourceEvidence,
    handler: LifecycleHandlerDestination,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct LifecycleConnection {
    label: String,
    schema_version: u32,
    connection_id: String,
    lifecycle_interface: InterfaceId,
    subagent_interfaces: Vec<InterfaceId>,
    handlers: Vec<LifecycleHandler>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecycleRunPin {
    schema_version: u32,
    connection_id: String,
    lifecycle_interface: InterfaceId,
    artifact: ArtifactRef,
}

#[derive(Clone, Debug)]
pub(crate) struct LifecycleBinding {
    connection: LifecycleConnection,
    artifact: ArtifactRef,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LifecycleContinuation {
    StartTurn {
        turn_input: serde_json::Value,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        additional_context: BTreeMap<String, octopus_appserver_proto::AdditionalContextEntry>,
        source_artifact: ArtifactRef,
    },
    HeldEmission {
        artifact: ArtifactRef,
        manifest_path: PathBuf,
        route: LifecycleRoute,
    },
    HeldEmissionAndRecoverBoundary {
        artifact: ArtifactRef,
        manifest_path: PathBuf,
        route: LifecycleRoute,
        source_artifact: ArtifactRef,
    },
    ProviderInput {
        manifest_label: String,
        payload: serde_json::Value,
        source_artifact: ArtifactRef,
    },
    RecoverBoundary {
        source_artifact: ArtifactRef,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LifecycleRoute {
    ClientProjection,
    ClientProjectionWithVerifiedInputs { inputs: Vec<LifecycleVerifiedInput> },
    Tool { tool: String },
    Interface { interface: InterfaceId },
    Operation { interface: InterfaceId, result_label: String },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LifecycleVerifiedInput {
    pub(crate) artifact_label: String,
    pub(crate) required_verified_artifact: RequiredVerifiedArtifact,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LifecycleEventArtifact {
    pub(crate) label: String,
    pub(crate) schema_version: u32,
    pub(crate) event_id: String,
    pub(crate) connection_id: String,
    pub(crate) connection_artifact: ArtifactRef,
    pub(crate) emitter_run: String,
    pub(crate) owner_run: String,
    pub(crate) handler_id: String,
    pub(crate) event: LifecycleEventName,
    pub(crate) mode: LifecycleMode,
    pub(crate) handler_type: LifecycleHandlerType,
    pub(crate) subject_artifact: ArtifactRef,
    pub(crate) trigger: String,
    pub(crate) data: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) continuation: Option<LifecycleContinuation>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LifecycleDecisionArtifact {
    pub(crate) label: String,
    pub(crate) schema_version: u32,
    pub(crate) event_id: String,
    pub(crate) connection_id: String,
    pub(crate) connection_artifact: ArtifactRef,
    pub(crate) emitter_run: String,
    pub(crate) owner_run: String,
    pub(crate) handler_id: String,
    pub(crate) event: LifecycleEventName,
    pub(crate) mode: LifecycleMode,
    pub(crate) decision: String,
    pub(crate) handler_type: LifecycleHandlerType,
    pub(crate) subject_artifact: ArtifactRef,
    pub(crate) event_artifact: LifecycleEventArtifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) additional_context_artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) replacement_artifact: Option<ArtifactRef>,
}

#[derive(Clone, Debug)]
pub(crate) struct LifecycleTrigger {
    pub(crate) event: LifecycleEventName,
    pub(crate) trigger: String,
    pub(crate) match_values: Vec<String>,
    pub(crate) subject_artifact: ArtifactRef,
    pub(crate) data: serde_json::Value,
    pub(crate) continuation: Option<LifecycleContinuation>,
}

#[derive(Clone, Debug)]
pub(crate) struct LifecyclePlannedOutput {
    pub(crate) label: String,
    pub(crate) value: serde_json::Value,
    pub(crate) route: LifecycleRoute,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LifecycleTriggerPlan {
    pub(crate) reactive: Vec<LifecyclePlannedOutput>,
    pub(crate) blocking: Option<LifecyclePlannedOutput>,
}

#[derive(Clone, Debug)]
pub(crate) struct LifecycleHandlerDispatch {
    pub(crate) label: String,
    pub(crate) value: serde_json::Value,
    pub(crate) route: LifecycleRoute,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecycleHandlerResultBody {
    decision: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    additional_context_artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    replacement_artifact: Option<ArtifactRef>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LifecycleHandlerOutputArtifact {
    label: String,
    schema_version: u32,
    connection_id: String,
    connection_artifact: ArtifactRef,
    event_id: String,
    emitter_run: String,
    owner_run: String,
    handler_id: String,
    event: LifecycleEventName,
    mode: LifecycleMode,
    handler_type: LifecycleHandlerType,
    subject_artifact: ArtifactRef,
    event_artifact: LifecycleEventArtifact,
    status: String,
    source: String,
    result: serde_json::Value,
}

pub(crate) enum LifecycleInbound {
    Event(LifecycleEventArtifact),
    Decision(LifecycleDecisionArtifact),
    HandlerPrompt(serde_json::Value),
    HandlerTask(serde_json::Value),
    HandlerOutput(serde_json::Value),
}

impl LifecycleBinding {
    pub(crate) fn resolve(
        payloads: &[ArtifactBindingPayload],
        run_record: &serde_json::Value,
        runtime: &RuntimeInvocationMetadata,
        granted_interfaces: &BTreeSet<InterfaceId>,
        granted_marks: &BTreeSet<String>,
    ) -> Result<Option<Self>, String> {
        let mut candidates = Vec::new();
        for bound in payloads {
            let bytes = bound.read_bytes().map_err(|error| error.to_string())?;
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes.as_ref()) else {
                continue;
            };
            if value.get("label").and_then(serde_json::Value::as_str)
                == Some(LIFECYCLE_CONNECTION_LABEL)
            {
                if let Some((_, existing)) = candidates
                    .iter()
                    .find(|(artifact, _)| artifact == &bound.artifact)
                {
                    if existing != &value {
                        return Err(format!(
                            "lifecycle connection artifact '{}' has conflicting payloads",
                            bound.artifact.id.as_str()
                        ));
                    }
                    continue;
                }
                candidates.push((bound.artifact.clone(), value));
            }
        }
        let provider_state = run_record
            .get("interface_state")
            .and_then(serde_json::Value::as_object)
            .and_then(|state| state.get(runtime.interface.as_str()))
            .and_then(serde_json::Value::as_object);
        let raw_pin = provider_state.and_then(|state| state.get("lifecycle_connection"));
        match (candidates.as_slice(), raw_pin) {
            ([], None) => return Ok(None),
            ([], Some(_)) => {
                return Err(format!(
                    "run '{}' pins lifecycle_connection but the exact connection artifact is not bound",
                    runtime.run.as_str()
                ));
            }
            ([..], None) => {
                return Err(format!(
                    "run '{}' binds lifecycle.connection without an explicit interface_state.{}.lifecycle_connection pin",
                    runtime.run.as_str(),
                    runtime.interface.as_str()
                ));
            }
            ([_, _, ..], Some(_)) => {
                return Err(format!(
                    "run '{}' binds more than one lifecycle.connection artifact",
                    runtime.run.as_str()
                ));
            }
            ([_], Some(_)) => {}
        }
        let (artifact, value) = candidates.pop().expect("one lifecycle candidate");
        let pin: LifecycleRunPin = serde_json::from_value(raw_pin.cloned().expect("pin exists"))
            .map_err(|error| {
                format!(
                    "run '{}' lifecycle_connection pin is malformed: {error}",
                    runtime.run.as_str()
                )
            })?;
        if pin.schema_version != 1
            || pin.artifact != artifact
            || pin.lifecycle_interface.as_str() != LIFECYCLE_INTERFACE
        {
            return Err(format!(
                "run '{}' lifecycle_connection pin does not match the bound exact artifact and lifecycle interface",
                runtime.run.as_str()
            ));
        }
        let connection: LifecycleConnection = serde_json::from_value(value).map_err(|error| {
            format!(
                "bound lifecycle connection '{}' is malformed: {error}",
                artifact.id.as_str()
            )
        })?;
        connection.validate()?;
        if pin.connection_id != connection.connection_id
            || pin.lifecycle_interface != connection.lifecycle_interface
        {
            return Err(format!(
                "run '{}' lifecycle_connection pin names a different connection",
                runtime.run.as_str()
            ));
        }
        if !granted_interfaces.contains(&connection.lifecycle_interface) {
            return Err(format!(
                "run '{}' connects lifecycle '{}' outside its formed exact interface grant",
                runtime.run.as_str(),
                connection.lifecycle_interface.as_str()
            ));
        }
        let has_sync = connection.handlers.iter().any(|handler| {
            handler.enabled
                && handler.mode == LifecycleMode::Sync
                && (handler.emitter_run == runtime.run.as_str()
                    || handler.owner_run == runtime.run.as_str())
        });
        if has_sync && !granted_marks.contains(LIFECYCLE_DECISION_MARK) {
            return Err(format!(
                "run '{}' connects a sync lifecycle edge without formed mark outcome '{}'",
                runtime.run.as_str(),
                LIFECYCLE_DECISION_MARK
            ));
        }
        Ok(Some(Self {
            connection,
            artifact,
        }))
    }

    pub(crate) fn validate_bound_event(
        &self,
        event: &LifecycleEventArtifact,
    ) -> Result<(), String> {
        self.validate_event(event)
    }

    pub(crate) fn validate_handler_invocation(
        &self,
        event: &LifecycleEventArtifact,
        value: &serde_json::Value,
        label: &str,
    ) -> Result<(), String> {
        self.validate_event(event)?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("{label} payload is not a JSON object"))?;
        let common = [
            ("label", label),
            ("connection_id", event.connection_id.as_str()),
            ("event_id", event.event_id.as_str()),
            ("handler_id", event.handler_id.as_str()),
        ];
        for (field, expected) in common {
            if object.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
                return Err(format!("{label} field '{field}' conflicts with its event"));
            }
        }
        if object
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
            || object.get("event")
                != Some(
                    &serde_json::to_value(event)
                        .map_err(|error| format!("lifecycle event did not encode: {error}"))?,
                )
        {
            return Err(format!(
                "{label} metadata conflicts with its embedded event"
            ));
        }
        let handler = self.handler_for_event(event)?;
        match (&handler.handler, label) {
            (LifecycleHandlerDestination::Prompt { .. }, LIFECYCLE_HANDLER_PROMPT_LABEL) => {
                let expected = BTreeSet::from([
                    "connection_id",
                    "event",
                    "event_id",
                    "handler_id",
                    "label",
                    "output_contract",
                    "prompt",
                    "schema_version",
                ]);
                if object.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
                    return Err(
                        "lifecycle.handler.prompt has unknown or missing fields".to_string()
                    );
                }
                let output = object
                    .get("output_contract")
                    .and_then(serde_json::Value::as_object)
                    .ok_or_else(|| {
                        "lifecycle.handler.prompt output_contract is not an object".to_string()
                    })?;
                let allowed = serde_json::json!([
                    "decision",
                    "reason",
                    "additional_context_artifacts",
                    "replacement_artifact"
                ]);
                if output.len() != 2
                    || output.get("decision")
                        != Some(&serde_json::json!(
                            (event.mode == LifecycleMode::Sync)
                                .then(|| event.event.positive_decision())
                                .flatten()
                        ))
                    || output.get("allowed_fields") != Some(&allowed)
                {
                    return Err(
                        "lifecycle.handler.prompt output contract conflicts with its event"
                            .to_string(),
                    );
                }
            }
            (LifecycleHandlerDestination::Agent { .. }, LIFECYCLE_HANDLER_TASK_LABEL) => {
                let expected = BTreeSet::from([
                    "connection_id",
                    "event",
                    "event_id",
                    "handler_id",
                    "label",
                    "result_interface",
                    "result_label",
                    "schema_version",
                    "task",
                ]);
                if object.keys().map(String::as_str).collect::<BTreeSet<_>>() != expected {
                    return Err("lifecycle.handler.task has unknown or missing fields".to_string());
                }
                let expected_result = if event.mode == LifecycleMode::Sync {
                    LIFECYCLE_DECISION_LABEL
                } else {
                    LIFECYCLE_HANDLER_OUTPUT_LABEL
                };
                if object
                    .get("result_interface")
                    .and_then(serde_json::Value::as_str)
                    != Some(self.connection.lifecycle_interface.as_str())
                    || object
                        .get("result_label")
                        .and_then(serde_json::Value::as_str)
                        != Some(expected_result)
                {
                    return Err(
                        "lifecycle.handler.task result edge conflicts with its connection"
                            .to_string(),
                    );
                }
            }
            _ => {
                return Err(format!(
                    "{label} does not match lifecycle handler '{}' destination",
                    event.handler_id
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn validate_command_result(
        &self,
        event: &LifecycleEventArtifact,
        tool: &str,
    ) -> Result<(), String> {
        self.validate_event(event)?;
        match &self.handler_for_event(event)?.handler {
            LifecycleHandlerDestination::Command { tool: expected, .. } if expected == tool => {
                Ok(())
            }
            _ => Err(format!(
                "tool result '{}' does not close lifecycle command handler '{}'",
                tool, event.handler_id
            )),
        }
    }

    pub(crate) fn validate_mcp_result(
        &self,
        event: &LifecycleEventArtifact,
        interface: Option<&InterfaceId>,
        operation: &str,
    ) -> Result<(), String> {
        self.validate_event(event)?;
        match (&self.handler_for_event(event)?.handler, interface) {
            (
                LifecycleHandlerDestination::McpTool {
                    interface: expected_interface,
                    operation: expected_operation,
                    ..
                },
                Some(actual_interface),
            ) if expected_interface == actual_interface && expected_operation == operation => {
                Ok(())
            }
            _ => Err(format!(
                "interface result '{}:{}' does not close lifecycle MCP handler '{}'",
                interface.map(InterfaceId::as_str).unwrap_or("<missing>"),
                operation,
                event.handler_id
            )),
        }
    }

    pub(crate) fn is_subagent_interface(&self, interface: &InterfaceId) -> bool {
        self.connection.subagent_interfaces.contains(interface)
    }

    pub(crate) fn plan_trigger(
        &self,
        emitter_run: &str,
        trigger: LifecycleTrigger,
    ) -> Result<LifecycleTriggerPlan, String> {
        validate_trimmed(&trigger.trigger, "lifecycle trigger")?;
        if !trigger.data.is_object() {
            return Err("lifecycle trigger data must be a JSON object".to_string());
        }
        let mut selected = self
            .connection
            .handlers
            .iter()
            .filter(|handler| {
                handler.enabled
                    && handler.emitter_run == emitter_run
                    && handler.event == trigger.event
            })
            .filter(|handler| matcher_matches(handler, &trigger.match_values).unwrap_or(false))
            .collect::<Vec<_>>();
        for handler in self.connection.handlers.iter().filter(|handler| {
            handler.enabled && handler.emitter_run == emitter_run && handler.event == trigger.event
        }) {
            matcher_matches(handler, &trigger.match_values)?;
        }
        selected.sort_by_key(|handler| (handler.order, handler.id.as_str()));
        let sync_count = selected
            .iter()
            .filter(|handler| handler.mode == LifecycleMode::Sync)
            .count();
        if sync_count > 1 {
            return Err(format!(
                "lifecycle connection '{}' selected {sync_count} sync handlers for {} in run '{}'; Octopus requires one explicitly authored aggregate decision edge",
                self.connection.connection_id,
                trigger.event.as_str(),
                emitter_run
            ));
        }
        if sync_count == 1 && trigger.event.positive_decision().is_none() {
            return Err(format!(
                "reactive lifecycle event '{}' cannot select a sync handler",
                trigger.event.as_str()
            ));
        }
        let mut plan = LifecycleTriggerPlan::default();
        for handler in selected {
            let continuation = if handler.mode == LifecycleMode::Sync {
                trigger.continuation.clone().ok_or_else(|| {
                    format!(
                        "sync lifecycle event '{}' has no held continuation",
                        trigger.event.as_str()
                    )
                })?
            } else {
                trigger
                    .continuation
                    .clone()
                    .unwrap_or(LifecycleContinuation::ProviderInput {
                        manifest_label: LIFECYCLE_EVENT_LABEL.to_string(),
                        payload: serde_json::json!({}),
                        source_artifact: trigger.subject_artifact.clone(),
                    })
            };
            let event_continuation = (handler.mode == LifecycleMode::Sync).then_some(continuation);
            let event_id = lifecycle_event_id(
                &self.connection.connection_id,
                &self.artifact,
                &handler.id,
                trigger.event,
                &trigger.subject_artifact,
                &trigger.trigger,
                &trigger.data,
                &event_continuation,
            )?;
            let event = LifecycleEventArtifact {
                label: LIFECYCLE_EVENT_LABEL.to_string(),
                schema_version: 1,
                event_id: event_id.clone(),
                connection_id: self.connection.connection_id.clone(),
                connection_artifact: self.artifact.clone(),
                emitter_run: emitter_run.to_string(),
                owner_run: handler.owner_run.clone(),
                handler_id: handler.id.clone(),
                event: trigger.event,
                mode: handler.mode,
                handler_type: handler.handler.handler_type(),
                subject_artifact: trigger.subject_artifact.clone(),
                trigger: trigger.trigger.clone(),
                data: trigger.data.clone(),
                continuation: event_continuation,
            };
            let value = serde_json::to_value(&event)
                .map_err(|error| format!("lifecycle event did not encode: {error}"))?;
            let output = if handler.mode == LifecycleMode::Sync {
                let positive = trigger
                    .event
                    .positive_decision()
                    .expect("sync event has positive decision");
                let requirement = lifecycle_decision_requirement(&event, positive)?;
                LifecyclePlannedOutput {
                    label: LIFECYCLE_EVENT_LABEL.to_string(),
                    value,
                    route: LifecycleRoute::ClientProjectionWithVerifiedInputs {
                        inputs: vec![LifecycleVerifiedInput {
                            artifact_label: LIFECYCLE_DECISION_LABEL.to_string(),
                            required_verified_artifact: requirement,
                        }],
                    },
                }
            } else {
                LifecyclePlannedOutput {
                    label: LIFECYCLE_EVENT_LABEL.to_string(),
                    value,
                    route: LifecycleRoute::ClientProjection,
                }
            };
            if handler.mode == LifecycleMode::Sync {
                plan.blocking = Some(output);
            } else {
                plan.reactive.push(output);
            }
        }
        Ok(plan)
    }

    pub(crate) fn dispatch_handler(
        &self,
        current_run: &str,
        event: &LifecycleEventArtifact,
        granted_tools: &BTreeSet<String>,
        granted_interfaces: &BTreeSet<InterfaceId>,
    ) -> Result<Option<LifecycleHandlerDispatch>, String> {
        self.validate_event(event)?;
        if event.owner_run != current_run {
            return Ok(None);
        }
        let handler = self.handler_for_event(event)?;
        let event_value = serde_json::to_value(event)
            .map_err(|error| format!("lifecycle event did not encode for handler: {error}"))?;
        let correlation_id = lifecycle_correlation_id(event);
        let dispatch = match &handler.handler {
            LifecycleHandlerDestination::Command {
                tool, arguments, ..
            } => {
                if !granted_tools.contains(tool) {
                    return Err(format!(
                        "lifecycle command handler '{}' selects tool '{}' outside run '{}' formed grant",
                        handler.id, tool, current_run
                    ));
                }
                LifecycleHandlerDispatch {
                    label: "tool.call".to_string(),
                    value: serde_json::json!({
                        "tool": tool,
                        "arguments": render_template_value(arguments, &event_value)?,
                        "correlation_id": correlation_id,
                    }),
                    route: LifecycleRoute::Tool { tool: tool.clone() },
                }
            }
            LifecycleHandlerDestination::McpTool {
                interface,
                operation,
                request_label,
                input,
                ..
            } => {
                require_granted_interface(granted_interfaces, interface, &handler.id, current_run)?;
                LifecycleHandlerDispatch {
                    label: request_label.clone(),
                    value: serde_json::json!({
                        "operation": operation,
                        "arguments": render_template_value(input, &event_value)?,
                        "correlation_id": correlation_id,
                    }),
                    route: LifecycleRoute::Interface {
                        interface: interface.clone(),
                    },
                }
            }
            LifecycleHandlerDestination::Prompt {
                provider_interface,
                prompt,
            } => {
                require_granted_interface(
                    granted_interfaces,
                    provider_interface,
                    &handler.id,
                    current_run,
                )?;
                LifecycleHandlerDispatch {
                    label: LIFECYCLE_HANDLER_PROMPT_LABEL.to_string(),
                    value: serde_json::json!({
                        "label": LIFECYCLE_HANDLER_PROMPT_LABEL,
                        "schema_version": 1,
                        "connection_id": event.connection_id,
                        "event_id": event.event_id,
                        "handler_id": event.handler_id,
                        "prompt": render_template_string(prompt, &event_value)?,
                        "event": event,
                        "output_contract": {
                            "decision": (event.mode == LifecycleMode::Sync)
                                .then(|| event.event.positive_decision())
                                .flatten(),
                            "allowed_fields": [
                                "decision",
                                "reason",
                                "additional_context_artifacts",
                                "replacement_artifact"
                            ]
                        }
                    }),
                    route: LifecycleRoute::Interface {
                        interface: provider_interface.clone(),
                    },
                }
            }
            LifecycleHandlerDestination::Agent {
                worker_interface,
                task,
            } => {
                require_granted_interface(
                    granted_interfaces,
                    worker_interface,
                    &handler.id,
                    current_run,
                )?;
                LifecycleHandlerDispatch {
                    label: LIFECYCLE_HANDLER_TASK_LABEL.to_string(),
                    value: serde_json::json!({
                        "label": LIFECYCLE_HANDLER_TASK_LABEL,
                        "schema_version": 1,
                        "connection_id": event.connection_id,
                        "event_id": event.event_id,
                        "handler_id": event.handler_id,
                        "task": render_template_string(task, &event_value)?,
                        "event": event,
                        "result_interface": self.connection.lifecycle_interface,
                        "result_label": if event.mode == LifecycleMode::Sync {
                            LIFECYCLE_DECISION_LABEL
                        } else {
                            LIFECYCLE_HANDLER_OUTPUT_LABEL
                        }
                    }),
                    route: LifecycleRoute::Interface {
                        interface: worker_interface.clone(),
                    },
                }
            }
        };
        Ok(Some(dispatch))
    }

    pub(crate) fn normalize_handler_result(
        &self,
        event: &LifecycleEventArtifact,
        source: &str,
        ok: bool,
        value: &serde_json::Value,
    ) -> Result<LifecyclePlannedOutput, String> {
        self.validate_event(event)?;
        let handler = self.handler_for_event(event)?;
        let selected = match &handler.handler {
            LifecycleHandlerDestination::Command { result_pointer, .. }
            | LifecycleHandlerDestination::McpTool { result_pointer, .. } => {
                value.pointer(result_pointer).ok_or_else(|| {
                    format!(
                        "lifecycle handler '{}' result has no configured pointer '{}'",
                        handler.id, result_pointer
                    )
                })?
            }
            LifecycleHandlerDestination::Prompt { .. }
            | LifecycleHandlerDestination::Agent { .. } => value,
        };
        if event.mode == LifecycleMode::Async {
            return Ok(LifecyclePlannedOutput {
                label: LIFECYCLE_HANDLER_OUTPUT_LABEL.to_string(),
                value: serde_json::json!({
                    "label": LIFECYCLE_HANDLER_OUTPUT_LABEL,
                    "schema_version": 1,
                    "connection_id": event.connection_id,
                    "connection_artifact": event.connection_artifact,
                    "event_id": event.event_id,
                    "emitter_run": event.emitter_run,
                    "owner_run": event.owner_run,
                    "handler_id": event.handler_id,
                    "event": event.event,
                    "mode": event.mode,
                    "handler_type": event.handler_type,
                    "subject_artifact": event.subject_artifact,
                    "event_artifact": event,
                    "status": if ok { "completed" } else { "failed" },
                    "source": source,
                    "result": selected,
                }),
                route: LifecycleRoute::Interface {
                    interface: self.connection.lifecycle_interface.clone(),
                },
            });
        }
        let body = if ok {
            serde_json::from_value::<LifecycleHandlerResultBody>(selected.clone()).map_err(
                |error| {
                    format!(
                        "sync lifecycle handler '{}' returned a malformed decision body: {error}",
                        handler.id
                    )
                },
            )?
        } else {
            LifecycleHandlerResultBody {
                decision: negative_decision(event.event).to_string(),
                reason: Some(format!(
                    "lifecycle handler '{}' failed at {source}",
                    handler.id
                )),
                additional_context_artifacts: Vec::new(),
                replacement_artifact: None,
            }
        };
        let decision = LifecycleDecisionArtifact {
            label: LIFECYCLE_DECISION_LABEL.to_string(),
            schema_version: 1,
            event_id: event.event_id.clone(),
            connection_id: event.connection_id.clone(),
            connection_artifact: event.connection_artifact.clone(),
            emitter_run: event.emitter_run.clone(),
            owner_run: event.owner_run.clone(),
            handler_id: event.handler_id.clone(),
            event: event.event,
            mode: event.mode,
            decision: body.decision,
            handler_type: event.handler_type,
            subject_artifact: event.subject_artifact.clone(),
            event_artifact: event.clone(),
            reason: body.reason,
            additional_context_artifacts: body.additional_context_artifacts,
            replacement_artifact: body.replacement_artifact,
        };
        Ok(LifecyclePlannedOutput {
            label: LIFECYCLE_DECISION_LABEL.to_string(),
            value: serde_json::to_value(decision)
                .map_err(|error| format!("lifecycle decision did not encode: {error}"))?,
            route: LifecycleRoute::Interface {
                interface: self.connection.lifecycle_interface.clone(),
            },
        })
    }

    pub(crate) fn validate_decision(
        &self,
        current_run: &str,
        decision: &LifecycleDecisionArtifact,
        metadata: &InvocationMetadata,
    ) -> Result<LifecycleContinuation, String> {
        let event = &decision.event_artifact;
        self.validate_event(event)?;
        if event.emitter_run != current_run {
            return Err(format!(
                "run '{current_run}' cannot resume lifecycle event '{}' emitted by '{}'",
                event.event_id, event.emitter_run
            ));
        }
        if event.mode != LifecycleMode::Sync {
            return Err("reactive lifecycle event cannot be resumed by a decision".to_string());
        }
        let positive = event
            .event
            .positive_decision()
            .ok_or_else(|| "reactive lifecycle event has no positive decision".to_string())?;
        if decision.label != LIFECYCLE_DECISION_LABEL
            || decision.schema_version != 1
            || decision.event_id != event.event_id
            || decision.connection_id != event.connection_id
            || decision.connection_artifact != event.connection_artifact
            || decision.emitter_run != event.emitter_run
            || decision.owner_run != event.owner_run
            || decision.handler_id != event.handler_id
            || decision.event != event.event
            || decision.mode != LifecycleMode::Sync
            || decision.handler_type != event.handler_type
            || decision.subject_artifact != event.subject_artifact
            || decision.decision != positive
        {
            return Err(format!(
                "lifecycle decision does not close exact event '{}' handler '{}'",
                event.event_id, event.handler_id
            ));
        }
        let expected_requirement = lifecycle_decision_requirement(event, positive)?;
        let [required] = metadata.required_verified_artifacts.as_slice() else {
            return Err(format!(
                "lifecycle decision for event '{}' must arrive through exactly one authored verified-artifact requirement",
                event.event_id
            ));
        };
        if required != &expected_requirement {
            return Err(format!(
                "lifecycle decision for event '{}' arrived through a different verified-artifact requirement",
                event.event_id
            ));
        }
        if !metadata
            .grant
            .mark_outcomes
            .iter()
            .any(|mark| mark == LIFECYCLE_DECISION_MARK)
        {
            return Err(format!(
                "lifecycle decision for event '{}' has no formed mark outcome '{}'",
                event.event_id, LIFECYCLE_DECISION_MARK
            ));
        }
        let admission = metadata.checkpoint_admission.as_ref().ok_or_else(|| {
            format!(
                "lifecycle decision for event '{}' carries no checkpoint-admission evidence",
                event.event_id
            )
        })?;
        if !admission.complete
            || admission.artifact_label != LIFECYCLE_DECISION_LABEL
            || admission.required_artifact_labels.as_slice() != [LIFECYCLE_DECISION_LABEL]
            || metadata.verification_failure.is_some()
        {
            return Err(format!(
                "lifecycle decision for event '{}' did not arrive as one complete admitted verified artifact",
                event.event_id
            ));
        }
        event
            .continuation
            .clone()
            .ok_or_else(|| "blocking lifecycle event lost its held continuation".to_string())
    }

    pub(crate) fn validate_handler_output(&self, value: &serde_json::Value) -> Result<(), String> {
        let output: LifecycleHandlerOutputArtifact = serde_json::from_value(value.clone())
            .map_err(|error| format!("lifecycle handler output is malformed: {error}"))?;
        let event = &output.event_artifact;
        self.validate_event(event)?;
        if output.label != LIFECYCLE_HANDLER_OUTPUT_LABEL
            || output.schema_version != 1
            || output.mode != LifecycleMode::Async
            || event.mode != LifecycleMode::Async
            || output.connection_id != event.connection_id
            || output.connection_artifact != event.connection_artifact
            || output.event_id != event.event_id
            || output.emitter_run != event.emitter_run
            || output.owner_run != event.owner_run
            || output.handler_id != event.handler_id
            || output.event != event.event
            || output.handler_type != event.handler_type
            || output.subject_artifact != event.subject_artifact
            || !matches!(output.status.as_str(), "completed" | "failed")
            || output.source.trim().is_empty()
            || output.source.trim() != output.source
        {
            return Err(format!(
                "lifecycle handler output '{}' conflicts with its exact connected event",
                output.event_id
            ));
        }
        let _ = &output.result;
        Ok(())
    }

    pub(crate) fn bound_event_for_correlation(
        &self,
        payloads: &[ArtifactBindingPayload],
        correlation_id: &str,
    ) -> Result<Option<LifecycleEventArtifact>, String> {
        let mut events = Vec::new();
        for bound in payloads {
            let bytes = bound.read_bytes().map_err(|error| error.to_string())?;
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes.as_ref()) else {
                continue;
            };
            if value.get("label").and_then(serde_json::Value::as_str) != Some(LIFECYCLE_EVENT_LABEL)
            {
                continue;
            }
            let event: LifecycleEventArtifact = serde_json::from_value(value).map_err(|error| {
                format!(
                    "bound lifecycle event '{}' is malformed: {error}",
                    bound.artifact.id.as_str()
                )
            })?;
            self.validate_event(&event)?;
            if lifecycle_correlation_id(&event) == correlation_id
                && !events.iter().any(|existing| existing == &event)
            {
                events.push(event);
            }
        }
        match events.len() {
            0 if correlation_id.starts_with("lifecycle:lifecycle:") => Err(format!(
                "lifecycle handler result correlation '{correlation_id}' has no exact bound source event"
            )),
            0 => Ok(None),
            1 => Ok(events.pop()),
            count => Err(format!(
                "provider invocation binds {count} lifecycle events for correlation '{correlation_id}'"
            )),
        }
    }

    fn validate_event(&self, event: &LifecycleEventArtifact) -> Result<(), String> {
        if event.label != LIFECYCLE_EVENT_LABEL
            || event.schema_version != 1
            || event.connection_id != self.connection.connection_id
            || event.connection_artifact != self.artifact
            || event.event_id
                != lifecycle_event_id(
                    &event.connection_id,
                    &event.connection_artifact,
                    &event.handler_id,
                    event.event,
                    &event.subject_artifact,
                    &event.trigger,
                    &event.data,
                    &event.continuation,
                )?
        {
            return Err("lifecycle event identity does not match its bound connection".to_string());
        }
        let handler = self.handler_for_event(event)?;
        if handler.emitter_run != event.emitter_run
            || handler.owner_run != event.owner_run
            || handler.mode != event.mode
            || handler.handler.handler_type() != event.handler_type
            || (event.mode == LifecycleMode::Sync) != event.continuation.is_some()
        {
            return Err(format!(
                "lifecycle event '{}' conflicts with handler '{}' connection metadata",
                event.event_id, event.handler_id
            ));
        }
        Ok(())
    }

    fn handler_for_event(
        &self,
        event: &LifecycleEventArtifact,
    ) -> Result<&LifecycleHandler, String> {
        self.connection
            .handlers
            .iter()
            .find(|handler| handler.id == event.handler_id && handler.event == event.event)
            .ok_or_else(|| {
                format!(
                    "lifecycle event '{}' names handler '{}' absent from connection '{}'",
                    event.event_id, event.handler_id, self.connection.connection_id
                )
            })
    }
}

impl LifecycleConnection {
    fn validate(&self) -> Result<(), String> {
        if self.label != LIFECYCLE_CONNECTION_LABEL || self.schema_version != 1 {
            return Err("lifecycle connection label/schema_version is unsupported".to_string());
        }
        validate_trimmed(&self.connection_id, "lifecycle connection_id")?;
        if self.lifecycle_interface.as_str() != LIFECYCLE_INTERFACE {
            return Err(format!(
                "lifecycle connection '{}' must use exact interface '{}'",
                self.connection_id, LIFECYCLE_INTERFACE
            ));
        }
        if self.handlers.is_empty() {
            return Err(format!(
                "lifecycle connection '{}' has no handlers",
                self.connection_id
            ));
        }
        let mut subagents = BTreeSet::new();
        for interface in &self.subagent_interfaces {
            validate_trimmed(interface.as_str(), "lifecycle subagent interface")?;
            if !subagents.insert(interface.clone()) {
                return Err(format!(
                    "lifecycle connection '{}' repeats subagent interface '{}'",
                    self.connection_id,
                    interface.as_str()
                ));
            }
        }
        let mut ids = BTreeSet::new();
        let mut orders = BTreeSet::new();
        for handler in &self.handlers {
            validate_trimmed(&handler.id, "lifecycle handler id")?;
            validate_trimmed(&handler.emitter_run, "lifecycle handler emitter_run")?;
            validate_trimmed(&handler.owner_run, "lifecycle handler owner_run")?;
            validate_trimmed(&handler.source.path, "lifecycle handler source.path")?;
            validate_hex_256(
                &handler.source.handler_content_sha256,
                "lifecycle handler source.handler_content_sha256",
            )?;
            if !ids.insert(handler.id.clone()) {
                return Err(format!(
                    "lifecycle connection '{}' repeats handler id '{}'",
                    self.connection_id, handler.id
                ));
            }
            if !orders.insert((handler.emitter_run.clone(), handler.event, handler.order)) {
                return Err(format!(
                    "lifecycle connection '{}' repeats handler order {} for {} in emitter '{}'",
                    self.connection_id,
                    handler.order,
                    handler.event.as_str(),
                    handler.emitter_run
                ));
            }
            let actual = handler_content_sha256(&handler.handler)?;
            if actual != handler.source.handler_content_sha256 {
                return Err(format!(
                    "lifecycle handler '{}' content hash changed: expected '{}' actual '{}'",
                    handler.id, handler.source.handler_content_sha256, actual
                ));
            }
            if handler.enabled
                && !matches!(
                    handler.source.trust,
                    LifecycleTrust::Managed | LifecycleTrust::Trusted
                )
            {
                return Err(format!(
                    "enabled lifecycle handler '{}' is {:?}, not managed/trusted",
                    handler.id, handler.source.trust
                ));
            }
            if handler.event.matcher_is_ignored() && handler.matcher.is_some() {
                return Err(format!(
                    "lifecycle handler '{}' declares a matcher for event '{}' whose matcher is ignored",
                    handler.id,
                    handler.event.as_str()
                ));
            }
            if let Some(matcher) = handler.matcher.as_deref() {
                validate_trimmed(matcher, "lifecycle handler matcher")?;
                regex::Regex::new(matcher).map_err(|error| {
                    format!(
                        "lifecycle handler '{}' matcher is invalid: {error}",
                        handler.id
                    )
                })?;
            }
            validate_handler_destination(&handler.id, &handler.handler)?;
            if handler.mode == LifecycleMode::Sync && handler.event.positive_decision().is_none() {
                return Err(format!(
                    "lifecycle handler '{}' makes reactive event '{}' sync",
                    handler.id,
                    handler.event.as_str()
                ));
            }
        }
        Ok(())
    }
}

pub(crate) fn decode_lifecycle_inbound(
    label: &str,
    bytes: &[u8],
) -> Result<Option<LifecycleInbound>, String> {
    let kind = match label {
        LIFECYCLE_EVENT_LABEL => 0,
        LIFECYCLE_DECISION_LABEL => 1,
        LIFECYCLE_HANDLER_PROMPT_LABEL => 2,
        LIFECYCLE_HANDLER_TASK_LABEL => 3,
        LIFECYCLE_HANDLER_OUTPUT_LABEL => 4,
        _ => return Ok(None),
    };
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("lifecycle input '{label}' is not JSON: {error}"))?;
    match kind {
        0 => serde_json::from_value(value)
            .map(LifecycleInbound::Event)
            .map(Some)
            .map_err(|error| format!("lifecycle.event is malformed: {error}")),
        1 => serde_json::from_value(value)
            .map(LifecycleInbound::Decision)
            .map(Some)
            .map_err(|error| format!("lifecycle.decision is malformed: {error}")),
        2 => Ok(Some(LifecycleInbound::HandlerPrompt(value))),
        3 => Ok(Some(LifecycleInbound::HandlerTask(value))),
        _ => Ok(Some(LifecycleInbound::HandlerOutput(value))),
    }
}

pub(crate) fn artifact_ref_for_file(file: &InterfaceArtifactFile) -> Result<ArtifactRef, String> {
    let manifest = file
        .read_manifest()
        .map_err(|error| format!("lifecycle held artifact manifest is unreadable: {error}"))?;
    let bytes = std::fs::read(file.manifest_path()).map_err(|error| {
        format!(
            "lifecycle held artifact manifest '{}' is unreadable: {error}",
            file.manifest_path().display()
        )
    })?;
    let content = ContentHash::of_bytes(&bytes);
    Ok(ArtifactRef {
        id: ArtifactId::new(format!("{}:{}", manifest.label, content.to_hex())),
        content,
    })
}

pub(crate) fn validate_held_file(
    output_root: &Path,
    continuation: &LifecycleContinuation,
) -> Result<(InterfaceArtifactFile, LifecycleRoute), String> {
    let (artifact, manifest_path, route) = match continuation {
        LifecycleContinuation::HeldEmission {
            artifact,
            manifest_path,
            route,
        }
        | LifecycleContinuation::HeldEmissionAndRecoverBoundary {
            artifact,
            manifest_path,
            route,
            ..
        } => (artifact, manifest_path, route),
        _ => return Err("lifecycle continuation is not a held emission".to_string()),
    };
    let canonical_root = output_root
        .canonicalize()
        .map_err(|error| format!("lifecycle output root is unreadable: {error}"))?;
    let canonical_manifest = manifest_path
        .canonicalize()
        .map_err(|error| format!("lifecycle held manifest is unreadable: {error}"))?;
    if !canonical_manifest.starts_with(&canonical_root) {
        return Err("lifecycle held manifest escapes the provider output root".to_string());
    }
    let file = InterfaceArtifactFile::new(canonical_manifest)
        .map_err(|error| format!("lifecycle held manifest is invalid: {error}"))?;
    if artifact_ref_for_file(&file)? != *artifact {
        return Err("lifecycle held artifact no longer matches its event evidence".to_string());
    }
    Ok((file, route.clone()))
}

fn lifecycle_decision_requirement(
    event: &LifecycleEventArtifact,
    positive: &str,
) -> Result<RequiredVerifiedArtifact, String> {
    let requirement = RequiredVerifiedArtifact {
        mark: MarkId::new(LIFECYCLE_DECISION_MARK),
        artifact_label: LIFECYCLE_DECISION_LABEL.to_string(),
        verifier: VerifierId::new(LIFECYCLE_DECISION_VERIFIER),
        params: serde_json::json!({
            "expected_event": event.event.as_str(),
            "expected_decision": positive,
            "expected_handler_type": event.handler_type.as_str(),
            "connection_id": event.connection_id,
            "connection_artifact_id": event.connection_artifact.id.as_str(),
            "connection_artifact_content": event.connection_artifact.content.to_hex(),
            "event_id": event.event_id,
            "emitter_run": event.emitter_run,
            "owner_run": event.owner_run,
            "handler_id": event.handler_id,
            "mode": "sync",
            "subject_artifact_id": event.subject_artifact.id.as_str(),
            "subject_artifact_content": event.subject_artifact.content.to_hex(),
            "event_artifact": event,
        }),
    };
    requirement.validate("lifecycle decision requirement")?;
    Ok(requirement)
}

fn matcher_matches(handler: &LifecycleHandler, values: &[String]) -> Result<bool, String> {
    let Some(matcher) = handler.matcher.as_deref() else {
        return Ok(true);
    };
    let matcher = regex::Regex::new(matcher).map_err(|error| {
        format!(
            "lifecycle handler '{}' matcher is invalid: {error}",
            handler.id
        )
    })?;
    Ok(values.iter().any(|value| matcher.is_match(value)))
}

fn lifecycle_event_id(
    connection_id: &str,
    connection_artifact: &ArtifactRef,
    handler_id: &str,
    event: LifecycleEventName,
    subject: &ArtifactRef,
    trigger: &str,
    data: &serde_json::Value,
    continuation: &Option<LifecycleContinuation>,
) -> Result<String, String> {
    let canonical = serde_json::to_vec(&serde_json::json!({
        "connection_id": connection_id,
        "connection_artifact": connection_artifact,
        "handler_id": handler_id,
        "event": event,
        "subject_artifact": subject,
        "trigger": trigger,
        "data": data,
        "continuation": continuation,
    }))
    .map_err(|error| format!("lifecycle event identity did not encode: {error}"))?;
    Ok(format!("lifecycle:{}", sha256_hex(&canonical)))
}

fn lifecycle_correlation_id(event: &LifecycleEventArtifact) -> String {
    format!("lifecycle:{}:{}", event.event_id, event.handler_id)
}

fn negative_decision(event: LifecycleEventName) -> &'static str {
    match event {
        LifecycleEventName::PermissionRequest | LifecycleEventName::PreToolUse => "deny",
        _ => "block",
    }
}

fn require_granted_interface(
    granted: &BTreeSet<InterfaceId>,
    interface: &InterfaceId,
    handler: &str,
    run: &str,
) -> Result<(), String> {
    if !granted.contains(interface) {
        return Err(format!(
            "lifecycle handler '{handler}' selects interface '{}' outside run '{run}' formed grant",
            interface.as_str()
        ));
    }
    Ok(())
}

fn validate_handler_destination(
    handler_id: &str,
    handler: &LifecycleHandlerDestination,
) -> Result<(), String> {
    match handler {
        LifecycleHandlerDestination::Command {
            tool,
            arguments,
            result_pointer,
        } => {
            validate_trimmed(tool, "lifecycle command tool")?;
            validate_json_template(arguments)?;
            validate_json_pointer(result_pointer, "lifecycle command result_pointer")?;
        }
        LifecycleHandlerDestination::McpTool {
            interface,
            operation,
            request_label,
            input,
            result_pointer,
        } => {
            validate_trimmed(interface.as_str(), "lifecycle MCP interface")?;
            validate_trimmed(operation, "lifecycle MCP operation")?;
            validate_trimmed(request_label, "lifecycle MCP request_label")?;
            validate_json_template(input)?;
            validate_json_pointer(result_pointer, "lifecycle MCP result_pointer")?;
        }
        LifecycleHandlerDestination::Prompt {
            provider_interface,
            prompt,
        } => {
            validate_trimmed(
                provider_interface.as_str(),
                "lifecycle prompt provider_interface",
            )?;
            validate_trimmed(prompt, "lifecycle prompt")?;
        }
        LifecycleHandlerDestination::Agent {
            worker_interface,
            task,
        } => {
            validate_trimmed(
                worker_interface.as_str(),
                "lifecycle agent worker_interface",
            )?;
            validate_trimmed(task, "lifecycle agent task")?;
        }
    }
    let encoded = serde_json::to_value(handler)
        .map_err(|error| format!("lifecycle handler '{handler_id}' did not encode: {error}"))?;
    validate_json_template(&encoded)
}

fn validate_json_pointer(pointer: &str, field: &str) -> Result<(), String> {
    if pointer.is_empty() || pointer.starts_with('/') {
        Ok(())
    } else {
        Err(format!("{field} must be empty or a JSON pointer"))
    }
}

fn validate_json_template(value: &serde_json::Value) -> Result<(), String> {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                validate_json_template(value)?;
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values() {
                validate_json_template(value)?;
            }
        }
        serde_json::Value::String(template) => {
            for token in template_tokens(template)? {
                validate_template_path(&token)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn render_template_value(
    template: &serde_json::Value,
    event: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match template {
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| render_template_value(value, event))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(values) => values
            .iter()
            .map(|(key, value)| {
                render_template_value(value, event).map(|value| (key.clone(), value))
            })
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(serde_json::Value::Object),
        serde_json::Value::String(template) => {
            let tokens = template_tokens(template)?;
            if tokens.len() == 1 && *template == format!("${{{}}}", tokens[0]) {
                return template_lookup(event, &tokens[0]).cloned();
            }
            let mut rendered = template.clone();
            for token in tokens {
                let replacement = template_scalar(template_lookup(event, &token)?)?;
                rendered = rendered.replace(&format!("${{{token}}}"), &replacement);
            }
            Ok(serde_json::Value::String(rendered))
        }
        value => Ok(value.clone()),
    }
}

fn render_template_string(template: &str, event: &serde_json::Value) -> Result<String, String> {
    match render_template_value(&serde_json::Value::String(template.to_string()), event)? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err("lifecycle string template rendered a non-string value".to_string()),
    }
}

fn template_lookup<'a>(
    event: &'a serde_json::Value,
    token: &str,
) -> Result<&'a serde_json::Value, String> {
    let pointer = format!(
        "/{}",
        token
            .split('.')
            .map(|segment| segment.replace('~', "~0").replace('/', "~1"))
            .collect::<Vec<_>>()
            .join("/")
    );
    event
        .pointer(&pointer)
        .ok_or_else(|| format!("lifecycle template field '{token}' is unavailable"))
}

fn template_scalar(value: &serde_json::Value) -> Result<String, String> {
    match value {
        serde_json::Value::Null => Ok("null".to_string()),
        serde_json::Value::Bool(value) => Ok(value.to_string()),
        serde_json::Value::Number(value) => Ok(value.to_string()),
        serde_json::Value::String(value) => Ok(value.clone()),
        _ => serde_json::to_string(value)
            .map_err(|error| format!("lifecycle template value did not encode: {error}")),
    }
}

fn template_tokens(template: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("${") {
        let after = &remaining[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| "lifecycle template has an unterminated field".to_string())?;
        let token = &after[..end];
        validate_template_path(token)?;
        tokens.push(token.to_string());
        remaining = &after[end + 1..];
    }
    Ok(tokens)
}

fn validate_template_path(token: &str) -> Result<(), String> {
    if token.is_empty()
        || token.split('.').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err(format!("lifecycle template field '{token}' is malformed"));
    }
    Ok(())
}

fn validate_trimmed(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        Err(format!("{field} must be non-empty and trimmed"))
    } else {
        Ok(())
    }
}

fn validate_hex_256(value: &str, field: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(format!("{field} must be lowercase sha256 hex"))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn handler_content_sha256(handler: &LifecycleHandlerDestination) -> Result<String, String> {
    let value = serde_json::to_value(handler)
        .map_err(|error| format!("lifecycle handler did not encode: {error}"))?;
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| format!("lifecycle handler canonical JSON did not encode: {error}"))?;
    Ok(sha256_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(id: &str, byte: u8) -> ArtifactRef {
        ArtifactRef {
            id: ArtifactId::new(id),
            content: ContentHash::of_bytes(&[byte]),
        }
    }

    fn handler_hash(handler: &LifecycleHandlerDestination) -> String {
        handler_content_sha256(handler).unwrap()
    }

    fn admitted_decision_metadata(event: &LifecycleEventArtifact) -> InvocationMetadata {
        let requirement = lifecycle_decision_requirement(
            event,
            event.event.positive_decision().expect("blocking event"),
        )
        .unwrap();
        InvocationMetadata::new(octopus_ledger::GrantProjection {
            mark_outcomes: vec![LIFECYCLE_DECISION_MARK.to_string()],
            ..Default::default()
        })
        .with_required_verified_artifacts(vec![requirement])
        .with_checkpoint_admission(octopus_ledger::CheckpointAdmissionMetadata {
            artifact_label: LIFECYCLE_DECISION_LABEL.to_string(),
            required_artifact_labels: vec![LIFECYCLE_DECISION_LABEL.to_string()],
            complete: true,
        })
    }

    #[test]
    fn installed_connection_example_is_the_strict_layered_source_contract() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../system_lifecycle_events/connection.example.json");
        let connection: LifecycleConnection =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        connection.validate().unwrap();
        assert_eq!(connection.handlers.len(), 4);
        assert!(connection.handlers.iter().any(|handler| {
            handler.source.trust == LifecycleTrust::Managed
                && handler.source.layer == LifecycleSourceLayer::Mdm
        }));
    }

    fn connection(handler: LifecycleHandlerDestination, mode: LifecycleMode) -> LifecycleBinding {
        let source = LifecycleSourceEvidence {
            layer: LifecycleSourceLayer::Project,
            path: "project/hooks.json".to_string(),
            trust: LifecycleTrust::Trusted,
            handler_content_sha256: handler_hash(&handler),
        };
        let connection = LifecycleConnection {
            label: LIFECYCLE_CONNECTION_LABEL.to_string(),
            schema_version: 1,
            connection_id: "connection-1".to_string(),
            lifecycle_interface: InterfaceId::new(LIFECYCLE_INTERFACE),
            subagent_interfaces: vec![InterfaceId::new("system_worker_reviewer")],
            handlers: vec![LifecycleHandler {
                id: "handler-1".to_string(),
                event: LifecycleEventName::PreToolUse,
                emitter_run: "emitter".to_string(),
                owner_run: "owner".to_string(),
                mode,
                order: 0,
                enabled: true,
                matcher: Some("^coder\\.".to_string()),
                source,
                handler,
            }],
        };
        connection.validate().unwrap();
        LifecycleBinding {
            connection,
            artifact: artifact("connection", 1),
        }
    }

    #[test]
    fn sync_event_materializes_verified_input_and_exact_command_tool() {
        let handler = LifecycleHandlerDestination::Command {
            tool: "coder.exec".to_string(),
            arguments: serde_json::json!({
                "command": "check ${data.tool}",
                "raw": "${data.arguments}"
            }),
            result_pointer: "/body".to_string(),
        };
        let binding = connection(handler, LifecycleMode::Sync);
        let plan = binding
            .plan_trigger(
                "emitter",
                LifecycleTrigger {
                    event: LifecycleEventName::PreToolUse,
                    trigger: "dynamic_tool_request".to_string(),
                    match_values: vec!["coder.exec".to_string()],
                    subject_artifact: artifact("tool", 2),
                    data: serde_json::json!({
                        "tool": "coder.exec",
                        "arguments": {"x": 1}
                    }),
                    continuation: Some(LifecycleContinuation::RecoverBoundary {
                        source_artifact: artifact("source", 3),
                    }),
                },
            )
            .unwrap();
        assert!(plan.reactive.is_empty());
        let blocking = plan.blocking.unwrap();
        let LifecycleRoute::ClientProjectionWithVerifiedInputs { inputs } = blocking.route else {
            panic!("expected verified input route");
        };
        assert_eq!(inputs.len(), 1);
        assert_eq!(
            inputs[0].required_verified_artifact.mark.as_str(),
            LIFECYCLE_DECISION_MARK
        );
        let event: LifecycleEventArtifact = serde_json::from_value(blocking.value).unwrap();
        assert_eq!(
            inputs[0].required_verified_artifact.params["event_artifact"],
            serde_json::to_value(&event).unwrap()
        );
        let dispatch = binding
            .dispatch_handler(
                "owner",
                &event,
                &BTreeSet::from(["coder.exec".to_string()]),
                &BTreeSet::from([InterfaceId::new(LIFECYCLE_INTERFACE)]),
            )
            .unwrap()
            .unwrap();
        assert_eq!(dispatch.label, "tool.call");
        assert_eq!(dispatch.value["arguments"]["command"], "check coder.exec");
        assert_eq!(
            dispatch.value["arguments"]["raw"],
            serde_json::json!({"x": 1})
        );
        assert_eq!(
            dispatch.route,
            LifecycleRoute::Tool {
                tool: "coder.exec".to_string()
            }
        );
        let decision = binding
            .normalize_handler_result(
                &event,
                "command_result",
                true,
                &serde_json::json!({"body": {"decision": "allow"}}),
            )
            .unwrap();
        let decision: LifecycleDecisionArtifact = serde_json::from_value(decision.value).unwrap();
        assert_eq!(decision.connection_artifact, binding.artifact);
        assert_eq!(decision.event_artifact, event);
        let metadata = admitted_decision_metadata(&event);
        assert!(matches!(
            binding
                .validate_decision("emitter", &decision, &metadata)
                .unwrap(),
            LifecycleContinuation::RecoverBoundary { .. }
        ));
        assert!(
            binding
                .validate_decision(
                    "emitter",
                    &decision,
                    &InvocationMetadata::new(Default::default()),
                )
                .is_err()
        );
    }

    #[test]
    fn all_destination_types_resolve_to_exact_granted_surfaces() {
        let cases = [
            LifecycleHandlerDestination::McpTool {
                interface: InterfaceId::new("world_mcp_docs"),
                operation: "search".to_string(),
                request_label: "mcp.search".to_string(),
                input: serde_json::json!({"query": "${data.query}"}),
                result_pointer: "/result".to_string(),
            },
            LifecycleHandlerDestination::Prompt {
                provider_interface: InterfaceId::new("world_provider_codex"),
                prompt: "Decide ${event}".to_string(),
            },
            LifecycleHandlerDestination::Agent {
                worker_interface: InterfaceId::new("system_worker_reviewer"),
                task: "Review ${data.query}".to_string(),
            },
        ];
        for handler in cases {
            let mut binding = connection(handler, LifecycleMode::Async);
            binding.connection.handlers[0].matcher = None;
            binding.connection.handlers[0].source.handler_content_sha256 =
                handler_hash(&binding.connection.handlers[0].handler);
            let plan = binding
                .plan_trigger(
                    "emitter",
                    LifecycleTrigger {
                        event: LifecycleEventName::PreToolUse,
                        trigger: "test".to_string(),
                        match_values: Vec::new(),
                        subject_artifact: artifact("subject", 4),
                        data: serde_json::json!({"query": "octopus"}),
                        continuation: None,
                    },
                )
                .unwrap();
            let event: LifecycleEventArtifact =
                serde_json::from_value(plan.reactive[0].value.clone()).unwrap();
            let dispatch = binding
                .dispatch_handler(
                    "owner",
                    &event,
                    &BTreeSet::new(),
                    &BTreeSet::from([
                        InterfaceId::new(LIFECYCLE_INTERFACE),
                        InterfaceId::new("world_mcp_docs"),
                        InterfaceId::new("world_provider_codex"),
                        InterfaceId::new("system_worker_reviewer"),
                    ]),
                )
                .unwrap()
                .unwrap();
            match event.handler_type {
                LifecycleHandlerType::McpTool => {
                    assert_eq!(dispatch.label, "mcp.search");
                    assert_eq!(dispatch.value["arguments"]["query"], "octopus");
                }
                LifecycleHandlerType::Prompt => {
                    assert_eq!(dispatch.label, LIFECYCLE_HANDLER_PROMPT_LABEL);
                    assert_eq!(dispatch.value["prompt"], "Decide pre_tool_use");
                    binding
                        .validate_handler_invocation(
                            &event,
                            &dispatch.value,
                            LIFECYCLE_HANDLER_PROMPT_LABEL,
                        )
                        .unwrap();
                }
                LifecycleHandlerType::Agent => {
                    assert_eq!(dispatch.label, LIFECYCLE_HANDLER_TASK_LABEL);
                    assert_eq!(dispatch.value["task"], "Review octopus");
                    binding
                        .validate_handler_invocation(
                            &event,
                            &dispatch.value,
                            LIFECYCLE_HANDLER_TASK_LABEL,
                        )
                        .unwrap();
                    let output = binding
                        .normalize_handler_result(
                            &event,
                            "provider_model",
                            true,
                            &serde_json::json!({"result": "reviewed"}),
                        )
                        .unwrap();
                    binding.validate_handler_output(&output.value).unwrap();
                }
                LifecycleHandlerType::Command => unreachable!(),
            }
        }
    }

    #[test]
    fn trust_hash_and_sync_aggregation_fail_closed() {
        let destination = LifecycleHandlerDestination::Command {
            tool: "coder.exec".to_string(),
            arguments: serde_json::json!({}),
            result_pointer: "/body".to_string(),
        };
        let mut binding = connection(destination.clone(), LifecycleMode::Sync);
        binding.connection.handlers[0].source.trust = LifecycleTrust::Modified;
        assert!(binding.connection.validate().is_err());
        binding.connection.handlers[0].source.trust = LifecycleTrust::Trusted;
        let mut second = binding.connection.handlers[0].clone();
        second.id = "handler-2".to_string();
        second.order = 1;
        second.source.handler_content_sha256 = handler_hash(&destination);
        binding.connection.handlers.push(second);
        let error = binding
            .plan_trigger(
                "emitter",
                LifecycleTrigger {
                    event: LifecycleEventName::PreToolUse,
                    trigger: "test".to_string(),
                    match_values: vec!["coder.exec".to_string()],
                    subject_artifact: artifact("subject", 5),
                    data: serde_json::json!({}),
                    continuation: Some(LifecycleContinuation::RecoverBoundary {
                        source_artifact: artifact("source", 6),
                    }),
                },
            )
            .unwrap_err();
        assert!(error.contains("aggregate decision edge"));
    }
}
