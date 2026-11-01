use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

use crate::config::SDK_CHILD_PROTOCOL;

const MAX_EVENT_BYTES: usize = 1024 * 1024;
const READY_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionConfig {
    pub model: String,
    pub system_prompt: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub resume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TextInput {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
}

impl TextInput {
    pub fn new(text: impl Into<String>) -> Self {
        Self { kind: "text", text: text.into() }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command_ {
    Start { request_id: u64, session: String, config: SessionConfig },
    Turn { request_id: u64, session: String, turn: String, input: Vec<TextInput> },
    Steer { request_id: u64, session: String, turn: String, as_turn: Option<String>, input: Vec<TextInput> },
    Interrupt { request_id: u64, session: String, turn: String },
    ToolResult { request_id: u64, session: String, turn: String, call_id: String, content: Vec<TextInput>, is_error: bool },
    Close { request_id: u64, session: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub kind: ItemKind,
    pub text: String,
    pub message_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Text,
    Thinking,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Ready { protocol: String, sdk_version: String, claude_version: String },
    Ack { request_id: u64 },
    Session { session: String, claude_session_id: String, model: String, tools: Vec<String> },
    Item { session: String, turn: String, item: Item },
    TextDelta { session: String, turn: String, message_id: String, text: String },
    InputRecorded { session: String, turn: String, text: String, synthetic: bool },
    ToolCall { session: String, turn: String, call_id: String, name: String, input: serde_json::Map<String, Value> },
    Usage { session: String, turn: String, usage: Usage, total_cost_usd: serde_json::Number, duration_ms: serde_json::Number, num_turns: u64 },
    TurnEnd { session: String, turn: String, status: TurnStatus, result: Option<String>, claude_session_id: Option<String> },
    Error { request_id: Option<u64>, session: Option<String>, turn: Option<String>, code: String, message: String, fatal: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ChildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl ChildError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

#[derive(Clone, Debug)]
pub struct ReadyInfo {
    pub sdk_version: String,
    pub claude_version: String,
}

type Pending = Arc<Mutex<BTreeMap<u64, oneshot::Sender<Result<(), ChildError>>>>>;

pub struct SdkChild {
    process: Child,
    stdin: ChildStdin,
    next_request: u64,
    pending: Pending,
    events: mpsc::Receiver<Event>,
    pub ready: ReadyInfo,
}

impl SdkChild {
    pub async fn spawn(
        executable: &Path,
        claude_executable: &str,
        environment: impl IntoIterator<Item = (OsString, OsString)>,
        cwd: &Path,
    ) -> Result<Self, ChildError> {
        let mut process = Command::new(executable)
            .arg("--claude-executable")
            .arg(claude_executable)
            .env_clear()
            .envs(environment)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| ChildError::new("child_spawn_failed", format!("{}: {error}", executable.display())))?;
        let stdin = process.stdin.take().ok_or_else(|| ChildError::new("child_spawn_failed", "child stdin unavailable"))?;
        let stdout = process.stdout.take().ok_or_else(|| ChildError::new("child_spawn_failed", "child stdout unavailable"))?;
        let pending: Pending = Arc::new(Mutex::new(BTreeMap::new()));
        let (events_tx, mut events) = mpsc::channel(256);
        tokio::spawn(read_events(BufReader::new(stdout), Arc::clone(&pending), events_tx));
        let ready = tokio::time::timeout(READY_TIMEOUT, events.recv())
            .await
            .map_err(|_| ChildError::new("child_not_ready", "the SDK child did not report ready"))?;
        let ready = match ready {
            Some(Event::Ready { protocol, sdk_version, claude_version }) if protocol == SDK_CHILD_PROTOCOL => {
                ReadyInfo { sdk_version, claude_version }
            }
            Some(Event::Error { code, message, .. }) => return Err(ChildError { code, message }),
            Some(other) => return Err(ChildError::new("child_protocol_violation", format!("expected ready, got {other:?}"))),
            None => return Err(ChildError::new("child_exited", "the SDK child exited before ready")),
        };
        Ok(Self { process, stdin, next_request: 1, pending, events, ready })
    }

    pub async fn start(&mut self, session: &str, config: SessionConfig) -> Result<(), ChildError> {
        let request_id = self.request_id();
        self.request(request_id, Command_::Start { request_id, session: session.into(), config }).await
    }

    pub async fn turn(&mut self, session: &str, turn: &str, input: Vec<TextInput>) -> Result<(), ChildError> {
        let request_id = self.request_id();
        self.request(request_id, Command_::Turn { request_id, session: session.into(), turn: turn.into(), input }).await
    }

    /// Redirects the running turn `turn`: the child interrupts Claude and
    /// starts the steer text as a new Claude turn in the same session,
    /// reported under `as_turn` when given, else under `turn`.
    pub async fn steer(&mut self, session: &str, turn: &str, as_turn: Option<&str>, input: Vec<TextInput>) -> Result<(), ChildError> {
        let request_id = self.request_id();
        self.request(
            request_id,
            Command_::Steer { request_id, session: session.into(), turn: turn.into(), as_turn: as_turn.map(str::to_owned), input },
        )
        .await
    }

    pub async fn interrupt(&mut self, session: &str, turn: &str) -> Result<(), ChildError> {
        let request_id = self.request_id();
        self.request(request_id, Command_::Interrupt { request_id, session: session.into(), turn: turn.into() }).await
    }

    pub async fn tool_result(
        &mut self,
        session: &str,
        turn: &str,
        call_id: &str,
        content: Vec<TextInput>,
        is_error: bool,
    ) -> Result<(), ChildError> {
        let request_id = self.request_id();
        self.request(
            request_id,
            Command_::ToolResult { request_id, session: session.into(), turn: turn.into(), call_id: call_id.into(), content, is_error },
        )
        .await
    }

    pub async fn close(&mut self, session: &str) -> Result<(), ChildError> {
        let request_id = self.request_id();
        self.request(request_id, Command_::Close { request_id, session: session.into() }).await
    }

    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    pub async fn shutdown(mut self) -> Result<(), ChildError> {
        drop(self.stdin);
        let status = tokio::time::timeout(Duration::from_secs(10), self.process.wait())
            .await
            .map_err(|_| ChildError::new("child_shutdown_timeout", "the SDK child did not exit after stdin closed"))?
            .map_err(|error| ChildError::new("child_wait_failed", error.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(ChildError::new("child_exit_status", format!("the SDK child exited with {status}")))
        }
    }

    fn request_id(&mut self) -> u64 {
        let id = self.next_request;
        self.next_request += 1;
        id
    }

    async fn request(&mut self, request_id: u64, command: Command_) -> Result<(), ChildError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| ChildError::new("child_state_poisoned", "pending request table poisoned"))?
            .insert(request_id, reply_tx);
        let mut line = serde_json::to_vec(&command)
            .map_err(|error| ChildError::new("command_encode_failed", error.to_string()))?;
        line.push(b'\n');
        self.stdin
            .write_all(&line)
            .await
            .map_err(|error| ChildError::new("child_write_failed", error.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| ChildError::new("child_write_failed", error.to_string()))?;
        reply_rx
            .await
            .map_err(|_| ChildError::new("child_exited", "the SDK child exited before answering"))?
    }
}

async fn read_events(
    mut stdout: BufReader<tokio::process::ChildStdout>,
    pending: Pending,
    events: mpsc::Sender<Event>,
) {
    let failure = loop {
        let mut line = Vec::new();
        match (&mut stdout).take(MAX_EVENT_BYTES as u64 + 1).read_until(b'\n', &mut line).await {
            Ok(0) => break ChildError::new("child_exited", "the SDK child closed its output"),
            Ok(_) if line.last() != Some(&b'\n') => {
                break ChildError::new("child_protocol_violation", "event frame exceeds the size ceiling or is unterminated");
            }
            Ok(_) => {}
            Err(error) => break ChildError::new("child_read_failed", error.to_string()),
        }
        let event: Event = match serde_json::from_slice(&line) {
            Ok(event) => event,
            Err(error) => break ChildError::new("child_protocol_violation", format!("malformed event frame: {error}")),
        };
        let reply = match &event {
            Event::Ack { request_id } => Some((*request_id, Ok(()))),
            Event::Error { request_id: Some(request_id), code, message, .. } => {
                Some((*request_id, Err(ChildError { code: code.clone(), message: message.clone() })))
            }
            _ => None,
        };
        if let Some((request_id, result)) = reply {
            let sender = pending.lock().ok().and_then(|mut table| table.remove(&request_id));
            match sender {
                Some(sender) => {
                    let _ = sender.send(result);
                }
                None => break ChildError::new("child_protocol_violation", format!("reply to unknown request {request_id}")),
            }
            continue;
        }
        if let Event::Error { fatal: true, code, message, .. } = &event {
            let failure = ChildError { code: code.clone(), message: message.clone() };
            fail_pending(&pending, &failure);
            let _ = events.send(event).await;
            return;
        }
        if events.send(event).await.is_err() {
            break ChildError::new("child_abandoned", "the provider life stopped reading SDK child events");
        }
    };
    fail_pending(&pending, &failure);
    let _ = events
        .send(Event::Error { request_id: None, session: None, turn: None, code: failure.code, message: failure.message, fatal: true })
        .await;
}


fn fail_pending(pending: &Pending, failure: &ChildError) {
    if let Ok(mut table) = pending.lock() {
        for (_, sender) in std::mem::take(&mut *table) {
            let _ = sender.send(Err(failure.clone()));
        }
    }
}
