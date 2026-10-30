use super::*;
use octopus_interface_transport::{InterfaceLiftContext, SystemInterfaceEmissionArtifact};
use octopus_run_authoring::{
    CheckedRunEvidence, CheckedRunEvidenceReader, FileCheckedRunReader,
};
use octopus_world_pyecho_life_runtime::LedgerTailReader;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

pub(crate) const REQUEST_LABEL: &str = "provider.composer_prediction.request";
pub(crate) const RESULT_LABEL: &str = "provider.composer_prediction.result";

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PredictionRequest {
    pub request_id: String,
    pub reply_to: InterfaceId,
    pub source: ArtifactRef,
    pub source_movement_seq: u64,
}

#[derive(serde::Deserialize)]
struct PredictionConfig {
    is_enabled: bool,
    prompt: String,
    prompt_version: String,
    prediction_reasoning_effort: String,
    #[serde(default)]
    unsupported_models: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum PredictionFailure {
    Cancelled,
    Boundary(String),
    Transport(String),
    Integrity(String),
    UnknownEffect(String),
    UnsettledFatal(String),
}

impl PredictionFailure {
    pub(crate) fn retires_driver(&self) -> bool {
        matches!(
            self,
            Self::Transport(_)
                | Self::Integrity(_)
                | Self::UnknownEffect(_)
                | Self::UnsettledFatal(_)
        )
    }

    pub(crate) fn result_admitted(&self) -> bool {
        matches!(
            self,
            Self::Transport(_) | Self::Integrity(_) | Self::UnknownEffect(_)
        )
    }
}

impl std::fmt::Display for PredictionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("composer prediction cancelled"),
            Self::Boundary(message)
            | Self::Transport(message)
            | Self::Integrity(message)
            | Self::UnknownEffect(message)
            | Self::UnsettledFatal(message) => f.write_str(message),
        }
    }
}

impl From<String> for PredictionFailure {
    fn from(message: String) -> Self {
        Self::Boundary(message)
    }
}

impl From<&str> for PredictionFailure {
    fn from(message: &str) -> Self {
        Self::Boundary(message.into())
    }
}

fn result_failure(
    failure: Option<&PredictionFailure>,
    message: impl std::fmt::Display,
) -> PredictionFailure {
    match failure {
        Some(failure) => PredictionFailure::UnsettledFatal(format!("{failure}; result: {message}")),
        None => PredictionFailure::UnsettledFatal(message.to_string()),
    }
}

fn cleanup_failure(primary: PredictionFailure, cleanup: PredictionFailure) -> PredictionFailure {
    let message = format!("{primary}; cleanup: {cleanup}");
    match primary {
        PredictionFailure::Transport(_) => PredictionFailure::Transport(message),
        PredictionFailure::UnknownEffect(_) => PredictionFailure::UnknownEffect(message),
        _ if cleanup.retires_driver() => match cleanup {
            PredictionFailure::Transport(_) => PredictionFailure::Transport(message),
            PredictionFailure::UnknownEffect(_) => PredictionFailure::UnknownEffect(message),
            _ => PredictionFailure::Integrity(message),
        },
        _ => PredictionFailure::Integrity(message),
    }
}

struct PredictionBudget {
    work_deadline: Instant,
    cleanup_deadline: Instant,
    result_deadline: Instant,
    cleanup_window: Duration,
}

impl PredictionBudget {
    fn new(work: Duration, cleanup: Duration) -> Self {
        let work_deadline = Instant::now() + work;
        let cleanup_deadline = work_deadline + cleanup;
        Self {
            work_deadline,
            cleanup_deadline,
            result_deadline: cleanup_deadline + Duration::from_secs(5),
            cleanup_window: cleanup,
        }
    }

    fn begin_cleanup(&self) -> Instant {
        self.cleanup_deadline
            .min(Instant::now() + self.cleanup_window)
    }
}

fn fence(
    cancel: &tokio_util::sync::CancellationToken,
    deadline: Instant,
) -> Result<(), PredictionFailure> {
    if cancel.is_cancelled() {
        return Err(PredictionFailure::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err("composer prediction operation deadline exceeded".into());
    }
    Ok(())
}

async fn read_only<T>(
    cancel: &tokio_util::sync::CancellationToken,
    deadline: Instant,
    future: impl std::future::Future<Output = Result<T, PredictionFailure>>,
) -> Result<T, PredictionFailure> {
    fence(cancel, deadline)?;
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(PredictionFailure::Cancelled),
        _ = tokio::time::sleep_until(deadline) => Err("composer prediction operation deadline exceeded".into()),
        result = future => result,
    }
}

pub(crate) async fn run(
    ctx: &InterfaceLiftContext,
    driver: &CodexAppServerInterfaceDriver,
    manager: &Arc<AuthManager>,
    input: &InterfaceInvocationEnvelope,
    sequence: u64,
) -> Result<(), PredictionFailure> {
    let cancel = ctx.cancel.child_token();
    let pending = process(ctx, driver, manager, input, sequence, &cancel);
    tokio::pin!(pending);
    tokio::select! {
        result = &mut pending => result,
        changed = wait_for_new_input(ctx, sequence) => {
            cancel.cancel();
            let result = pending.await;
            match result {
                Err(error) => Err(error),
                Ok(()) => changed.map_err(PredictionFailure::Boundary),
            }
        }
    }
}

async fn wait_for_new_input(ctx: &InterfaceLiftContext, sequence: u64) -> Result<(), String> {
    let mut reader = LedgerTailReader::open_named(ctx, "composer-predictions")?;
    loop {
        if ctx.cancel.is_cancelled() {
            return Ok(());
        }
        for own in reader.read_new()? {
            reader.commit_through(&own)?;
            if own.movement.seq.0 > sequence {
                return Ok(());
            }
        }
        reader.wait_for_change(&ctx.cancel).await?;
    }
}

async fn process(
    ctx: &InterfaceLiftContext,
    driver: &CodexAppServerInterfaceDriver,
    manager: &Arc<AuthManager>,
    input: &InterfaceInvocationEnvelope,
    sequence: u64,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), PredictionFailure> {
    let budget = PredictionBudget::new(Duration::from_secs(30), Duration::from_secs(5));
    input.runtime().map_err(|e| e.to_string())?;
    let request: PredictionRequest = serde_json::from_slice(
        &input
            .read_artifact_payload_bounded(16_384)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("invalid composer prediction request: {e}"))?;
    let record =
        octopus_run_record::RunRecord::from_bytes(&ctx.pinned_run_record_bytes(input)?)
            .map_err(|e| e.to_string())?;
    let target = record
        .checkpoints
        .iter()
        .filter(|cp| record.active_checkpoints.contains(&cp.id))
        .find(|cp| {
            cp.inbound_interface() == Some(&request.reply_to)
                && cp.outbound_interface_bindings.is_empty()
        })
        .and_then(|cp| cp.interface_binding.as_ref())
        .filter(|binding| binding.admits(RESULT_LABEL))
        .map(|binding| binding.interface.clone())
        .ok_or("composer prediction requires an authored result-only return binding")?;
    if request.request_id.trim().is_empty()
        || request.request_id.len() > 128
        || request.request_id.chars().any(char::is_control)
    {
        return Err("composer prediction has an invalid request identity".into());
    }
    if !input
        .allowed_interfaces()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|interface| interface == target.as_str())
    {
        return Err(
            "composer prediction return binding is outside the formed interface grant".into(),
        );
    }
    if !input
        .granted_tool_names()
        .map_err(|e| e.to_string())?
        .is_empty()
        || !input
            .worker_environment_kinds()
            .map_err(|e| e.to_string())?
            .is_empty()
    {
        return Err("composer prediction run must have no tool or worker-environment grant".into());
    }
    let root = ctx
        .interface_var_dir()
        .join("composer-predictions")
        .join(sequence.to_string());
    let (output, failure) = settle_result(&root, &request, &input.artifact, sequence, async {
        fence(cancel, budget.work_deadline)?;
        let source = validate_source(ctx, input, &request, sequence)?;
        let config = read_only(cancel, budget.work_deadline, async {
            let auth = manager.auth().await.ok_or("Codex sign-in is unavailable")?;
            if !auth.is_chatgpt_auth() {
                return Ok(None);
            }
            let token = auth
                .get_token()
                .map_err(|_| "Codex sign-in token is unavailable")?;
            let account = auth
                .get_account_id()
                .ok_or("Codex account identity is unavailable")?;
            let response = octopus_login::default_client::create_client()
                .get("https://chatgpt.com/backend-api/wham/predictions/config")
                .bearer_auth(token)
                .header("ChatGPT-Account-Id", account)
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .map_err(|_| "Codex prediction configuration request failed")?;
            if !response.status().is_success() {
                return Err(format!(
                    "Codex prediction configuration returned {}",
                    response.status()
                )
                .into());
            }
            let config: PredictionConfig = response
                .json()
                .await
                .map_err(|_| "Codex prediction configuration is malformed")?;
            Ok(config.is_enabled.then_some(config))
        })
        .await?;
        let Some(config) = config else {
            return Ok(json!({"status":"unavailable", "suggestion":null}));
        };
        driver
            .predict(&source, &request.request_id, &config, cancel, &budget)
            .await
    })
    .await?;
    let bytes = serde_json::to_vec(&output).map_err(|e| result_failure(failure.as_ref(), e))?;
    let artifact = InterfaceArtifactFile::write(root, RESULT_LABEL, &bytes)
        .map_err(|e| result_failure(failure.as_ref(), e))?;
    admit_result(
        budget
            .result_deadline
            .min(Instant::now() + Duration::from_secs(5)),
        failure,
        ctx.hand_system_emission(
            SystemInterfaceEmissionArtifact::Materialized(artifact),
            sequence,
            target,
        ),
    )
    .await
}

async fn admit_result(
    deadline: Instant,
    failure: Option<PredictionFailure>,
    delivery: impl std::future::Future<Output = Result<octopus_interfaces::DeliveryVerdict, String>>,
) -> Result<(), PredictionFailure> {
    let admitted = tokio::time::timeout_at(deadline, delivery).await
        .map_err(|_| result_failure(failure.as_ref(), "composer prediction result admission timed out; outcome unknown and exact receipt retained"))?
        .map_err(|error| result_failure(failure.as_ref(), error))?;
    match admitted {
        octopus_interfaces::DeliveryVerdict::Admitted { .. } => failure.map_or(Ok(()), Err),
        verdict => Err(result_failure(
            failure.as_ref(),
            format!("composer prediction result was not admitted: {verdict:?}"),
        )),
    }
}

async fn settle_result(
    root: &std::path::Path,
    request: &PredictionRequest,
    input: &ArtifactRef,
    sequence: u64,
    operation: impl std::future::Future<Output = Result<Value, PredictionFailure>>,
) -> Result<(Value, Option<PredictionFailure>), PredictionFailure> {
    let receipt = root.join("result.json");
    match std::fs::read(&receipt) {
        Ok(bytes) => {
            let output: Value =
                serde_json::from_slice(&bytes).map_err(|error| result_failure(None, error))?;
            let result = &output["result"];
            if output.as_object().is_none_or(|object| object.len() != 4)
                || output["request"]
                    != serde_json::to_value(request).map_err(|error| result_failure(None, error))?
                || output["input"]
                    != serde_json::to_value(input).map_err(|error| result_failure(None, error))?
                || output["source_movement_seq"] != sequence
                || result
                    .as_object()
                    .is_none_or(|object| !object.contains_key("suggestion"))
                || !matches!(
                    result["status"].as_str(),
                    Some("completed" | "unavailable" | "failed")
                )
                || !(result["suggestion"].is_null() || result["suggestion"].is_string())
                || (result["status"] == "unavailable" && !result["suggestion"].is_null())
                || (result["status"] == "failed"
                    && (!result["suggestion"].is_null()
                        || result["error"]
                            .as_str()
                            .is_none_or(|error| error.trim().is_empty())))
            {
                return Err(result_failure(
                    None,
                    "retained composer prediction is malformed or belongs to another input",
                ));
            }
            Ok((output, None))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(root.join("result.tmp")) {
                Ok(_) => {
                    return Err(result_failure(
                        None,
                        "retained composer prediction receipt is incomplete; native outcome unknown",
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(result_failure(None, error)),
            }
            let unsettled = json!({"request":request, "input":input, "source_movement_seq":sequence,
                "result":{"status":"failed","suggestion":null,
                "error":"composer prediction attempt is unsettled; native outcome unknown; execution will not be replayed"}});
            persist_result(root, &receipt, &unsettled)
                .map_err(|error| result_failure(None, error))?;
            let (result, failure) = match operation.await {
                Ok(result) => (result, None),
                Err(PredictionFailure::Cancelled) => {
                    (json!({"status":"unavailable", "suggestion":null}), None)
                }
                Err(error) => (
                    json!({"status":"failed", "suggestion":null, "error":error.to_string()}),
                    error.retires_driver().then_some(error),
                ),
            };
            let output = json!({"request":request, "input":input, "source_movement_seq":sequence, "result":result});
            persist_result(root, &receipt, &output)
                .map_err(|error| result_failure(failure.as_ref(), error))?;
            Ok((output, failure))
        }
        Err(error) => Err(result_failure(None, error)),
    }
}

fn persist_result(
    root: &std::path::Path,
    receipt: &std::path::Path,
    output: &Value,
) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let temporary = root.join("result.tmp");
    use std::io::Write;
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(output).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, receipt).map_err(|e| e.to_string())?;
    std::fs::File::open(root)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn validate_source(
    ctx: &InterfaceLiftContext,
    input: &InterfaceInvocationEnvelope,
    request: &PredictionRequest,
    sequence: u64,
) -> Result<Value, String> {
    if request.source_movement_seq >= sequence {
        return Err("prediction source must precede its request".into());
    }
    let bound = input
        .artifact_binding_payloads
        .iter()
        .find(|bound| bound.artifact == request.source)
        .ok_or("prediction source is not an exact readable bound artifact")?;
    let limits = ctx.required_host_io_limits()?;
    let evidence = CheckedRunEvidence {
        movements_path: &ctx.movements_path,
        ledger_index_path: &ctx.ledger_index_path,
        blobs_dir: &ctx.blobs_dir,
        max_control_frame_bytes: limits.max_control_frame_bytes,
        max_manifest_bytes: limits.max_manifest_bytes,
        max_payload_bytes: limits.max_payload_bytes,
    };
    let mut reader = FileCheckedRunReader::open(&evidence, None)?;
    let row = reader
        .admitted_movement(request.source_movement_seq)?
        .ok_or("prediction source movement is absent")?;
    let (manifest, payload) = reader.artifact(&request.source)?;
    let source: Value = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    let native = &source["codex_continuity"];
    let runtime = row
        .runtime()
        .ok_or("prediction source has no admitted runtime identity")?;
    let continuity = parse_codex_continuity(native, &ctx.interface, runtime.run.as_str())
        .map_err(|e| e.to_string())?;
    if row.artifact.as_ref() != Some(&request.source)
        || manifest.label != INTERFACE_ARTIFACT_OUTPUT_LABEL
        || row.from != octopus_ledger::Endpoint::Interface(ctx.interface.clone())
        || payload.as_slice() != bound.read_bytes().map_err(|e| e.to_string())?.as_ref()
        || native["provider_interface"] != ctx.interface.as_str()
        || !native["pending"].is_null()
        || !native["interrupted_pending"].is_null()
        || source["thread_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .is_none()
        || source["turn_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .is_none()
        || source["thread_id"] != native["thread_id"]
    {
        return Err("prediction source is not an exact completed result from this provider".into());
    }
    if continuity.pending.is_some()
        || continuity.interrupted_pending.is_some()
        || continuity.tool_operator_request.is_some()
    {
        return Err("prediction source still has pending work".into());
    }
    Ok(source)
}

impl CodexAppServerInterfaceDriver {
    async fn predict(
        &self,
        source: &Value,
        request_id: &str,
        config: &PredictionConfig,
        cancel: &tokio_util::sync::CancellationToken,
        budget: &PredictionBudget,
    ) -> Result<Value, PredictionFailure> {
        let mut client = read_only(cancel, budget.work_deadline, async {
            Ok(self.client.lock().await)
        })
        .await?;
        let mut thread_id = None;
        let mut turn_id = None;
        let mut deferred_events = Vec::new();
        let work = async {
            let parent_thread = source["thread_id"]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or("prediction source has no thread")?;
            let read = request(
                self,
                "thread/read",
                json!({"threadId":parent_thread,"includeTurns":false}),
            )?;
            let parent: Value = read_only(
                cancel,
                budget
                    .work_deadline
                    .min(Instant::now() + Duration::from_secs(10)),
                async { client.request_typed(read).await.map_err(rpc_failure) },
            )
            .await?;
            let parent = &parent["thread"];
            if parent["id"] != parent_thread
                || parent["model"]
                    .as_str()
                    .is_none_or(|model| model.trim().is_empty())
            {
                return Err(PredictionFailure::Integrity(
                    "prediction source read returned a different or incomplete thread".into(),
                ));
            }
            if parent["modelProvider"] != "openai"
                || config
                    .unsupported_models
                    .iter()
                    .any(|model| parent["model"] == *model)
            {
                return Ok(json!({"status":"unavailable", "suggestion":null}));
            }
            require_head(
                self,
                &client,
                parent_thread,
                &source["turn_id"],
                cancel,
                budget.work_deadline,
            )
            .await?;
            let fork = effect_rpc(self, &client, "thread/fork", json!({"threadId":parent_thread,"ephemeral":true,
            "experimentalPredictionMode":true,"excludeTurns":true,"threadSource":"composer_predictions"}), Some(cancel), budget.work_deadline).await?;
            let thread = fork["thread"]["id"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    PredictionFailure::UnknownEffect(
                        "thread/fork acknowledgement has no thread; native outcome unknown".into(),
                    )
                })?
                .to_string();
            thread_id = Some(thread.clone());
            require_head(
                self,
                &client,
                parent_thread,
                &source["turn_id"],
                cancel,
                budget.work_deadline,
            )
            .await?;
            if fork["collaborationMode"]["mode"] == "plan" {
                return Ok(json!({"status":"unavailable", "suggestion":null}));
            }
            let prompt = format!(
                "{}\n\nReturn only JSON matching this schema: {{\"type\":\"object\",\"properties\":{{\"suggestion\":{{\"type\":[\"string\",\"null\"]}}}},\"required\":[\"suggestion\"],\"additionalProperties\":false}}.",
                config.prompt
            );
            let started = effect_rpc(self, &client, "turn/start", json!({"threadId":thread,"turnTrigger":"composer_predictions",
                "clientUserMessageId":request_id,"input":[{"type":"text","text":prompt,"text_elements":[]}],
                "effort": if config.prediction_reasoning_effort.is_empty() { Value::Null } else { json!(config.prediction_reasoning_effort) }}), Some(cancel), budget.work_deadline).await?;
            let turn = started["turn"]["id"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    PredictionFailure::UnknownEffect(
                        "turn/start acknowledgement has no turn; native outcome unknown".into(),
                    )
                })?
                .to_string();
            turn_id = Some(turn.clone());
            let mut answer = None;
            loop {
                let event = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(PredictionFailure::Cancelled),
                    _ = tokio::time::sleep_until(budget.work_deadline) => return Err("composer prediction operation deadline exceeded".into()),
                    event = client.next_event() => event,
                };
                match event {
                    Some(AppServerEvent::ServerNotification(notification)) => {
                        let value =
                            serde_json::to_value(&notification).map_err(|e| e.to_string())?;
                        if value["params"]["threadId"] != thread {
                            deferred_events.push(AppServerEvent::ServerNotification(notification));
                            continue;
                        }
                        if value["method"] == "item/completed"
                            && value["params"]["threadId"] == thread
                            && value["params"]["turnId"] == turn
                            && value["params"]["item"]["type"] == "agentMessage"
                        {
                            answer = value["params"]["item"]["text"].as_str().map(str::to_owned);
                        }
                        if value["method"] != "turn/completed"
                            || value["params"]["threadId"] != thread
                            || value["params"]["turn"]["id"] != turn
                        {
                            continue;
                        }
                        if value["params"]["turn"]["status"] != "completed" {
                            return Err("prediction turn did not complete".into());
                        }
                        break;
                    }
                    Some(AppServerEvent::ServerRequest(request)) => {
                        let value = serde_json::to_value(&request).map_err(|e| e.to_string())?;
                        if value["params"]["threadId"] == thread {
                            return Err(
                                "prediction requested an action instead of a suggestion".into()
                            );
                        }
                        deferred_events.push(AppServerEvent::ServerRequest(request));
                    }
                    Some(event @ AppServerEvent::Disconnected { .. }) => {
                        let AppServerEvent::Disconnected { message } = &event else {
                            unreachable!()
                        };
                        let error = PredictionFailure::Transport(format!(
                            "prediction connection closed: {message}"
                        ));
                        deferred_events.push(event);
                        return Err(error);
                    }
                    Some(event @ AppServerEvent::Lagged { .. }) => {
                        let AppServerEvent::Lagged { skipped } = &event else {
                            unreachable!()
                        };
                        let error = PredictionFailure::Integrity(format!(
                            "prediction event stream lagged ({skipped} skipped); connection integrity lost"
                        ));
                        deferred_events.push(event);
                        return Err(error);
                    }
                    None => {
                        return Err(PredictionFailure::Transport(
                            "prediction event stream closed".into(),
                        ));
                    }
                    Some(event) => deferred_events.push(event),
                }
            }
            let text = answer.ok_or("prediction returned no completed message")?;
            let value: Value =
                serde_json::from_str(&text).map_err(|_| "prediction returned invalid JSON")?;
            if value
                .as_object()
                .is_none_or(|object| object.len() != 1 || !object.contains_key("suggestion"))
                || !(value["suggestion"].is_null() || value["suggestion"].is_string())
            {
                return Err("prediction returned an invalid suggestion".into());
            }
            let suggestion = value["suggestion"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            Ok(
                json!({"status":"completed","suggestion":suggestion,"prompt_version":config.prompt_version,
                "prediction_thread_id":thread,"prediction_turn_id":turn}),
            )
        };
        let mut result = work.await;
        if let Some(thread) = thread_id {
            let deadline = budget.begin_cleanup();
            let mut cleanup_error = None;
            if result.is_err()
                && let Some(turn) = turn_id
            {
                cleanup_error = effect_rpc(
                    self,
                    &client,
                    "turn/interrupt",
                    json!({"threadId":thread,"turnId":turn}),
                    None,
                    deadline,
                )
                .await
                .err();
            }
            if let Err(error) = effect_rpc(
                self,
                &client,
                "thread/unsubscribe",
                json!({"threadId":thread}),
                None,
                deadline,
            )
            .await
            {
                cleanup_error = Some(match cleanup_error {
                    Some(primary) => cleanup_failure(primary, error),
                    None => error,
                });
            }
            if let Some(cleanup) = cleanup_error {
                result = Err(match result {
                    Err(primary) => cleanup_failure(primary, cleanup),
                    _ if cleanup.retires_driver() => cleanup,
                    _ => PredictionFailure::Integrity(format!(
                        "prediction cleanup was not acknowledged: {cleanup}"
                    )),
                });
            }
        }
        client.restore_ready_events(deferred_events);
        match result {
            Err(PredictionFailure::Cancelled) => {
                Ok(json!({"status":"unavailable", "suggestion":null}))
            }
            Ok(_) if cancel.is_cancelled() => {
                Ok(json!({"status":"unavailable", "suggestion":null}))
            }
            result => result,
        }
    }
}

fn request(
    driver: &CodexAppServerInterfaceDriver,
    method: &str,
    params: Value,
) -> Result<ClientRequest, PredictionFailure> {
    serde_json::from_value(json!({"id":driver.mint_request_id(),"method":method,"params":params}))
        .map_err(|e| e.to_string().into())
}

fn rpc_failure(error: TypedRequestError) -> PredictionFailure {
    match error {
        TypedRequestError::Transport { method, source } => {
            PredictionFailure::Transport(format!("{method}: {source}"))
        }
        TypedRequestError::Server { method, source } => PredictionFailure::Boundary(format!(
            "{method} failed: {} (code {})",
            source.message, source.code
        )),
        TypedRequestError::Deserialize { method, source } => {
            PredictionFailure::Integrity(format!("{method} response decode error: {source}"))
        }
    }
}

pub(super) async fn effect_rpc(
    driver: &CodexAppServerInterfaceDriver,
    client: &AppServerClient,
    method: &str,
    params: Value,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    deadline: Instant,
) -> Result<Value, PredictionFailure> {
    if let Some(cancel) = cancel {
        fence(cancel, deadline)?;
    }
    if Instant::now() >= deadline {
        return Err(PredictionFailure::Integrity(format!(
            "{method} was not dispatched before its deadline; cleanup incomplete"
        )));
    }
    let request = request(driver, method, params)?;
    let deadline = deadline.min(Instant::now() + Duration::from_secs(10));
    tokio::time::timeout_at(deadline, client.request_typed(request))
        .await
        .map_err(|_| {
            PredictionFailure::UnknownEffect(format!(
                "{method} acknowledgement timed out; native outcome unknown"
            ))
        })?
        .map_err(rpc_failure)
}

async fn require_head(
    driver: &CodexAppServerInterfaceDriver,
    client: &AppServerClient,
    thread: &str,
    turn: &Value,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: Instant,
) -> Result<(), PredictionFailure> {
    let request = request(
        driver,
        "thread/turns/list",
        json!({"threadId":thread,"limit":1,"sortDirection":"desc","itemsView":"notLoaded"}),
    )?;
    let turns: Value = read_only(
        cancel,
        deadline.min(Instant::now() + Duration::from_secs(10)),
        async { client.request_typed(request).await.map_err(rpc_failure) },
    )
    .await?;
    if turns["data"][0]["id"] != *turn || turns["data"][0]["status"] != "completed" {
        return Err("conversation advanced beyond the requested prediction source".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prediction_reuses_connection_preserves_foreign_events_and_cleans_up() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Case {
            Complete,
            Read,
            FirstHead,
            Fork,
            SecondHead,
            Start,
            Events,
            UnknownFork,
            UnknownStart,
            Cleanup,
            Disconnect,
            Lagged,
            Stop,
        }
        for case in [
            Case::Complete,
            Case::Read,
            Case::FirstHead,
            Case::Fork,
            Case::SecondHead,
            Case::Start,
            Case::Events,
            Case::UnknownFork,
            Case::UnknownStart,
            Case::Cleanup,
            Case::Disconnect,
            Case::Lagged,
            Case::Stop,
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let cancel = tokio_util::sync::CancellationToken::new();
            let server_cancel = cancel.clone();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let mut socket =
                    tokio_tungstenite::accept_async(listener.accept().await.unwrap().0)
                        .await
                        .unwrap();
                let mut methods = Vec::new();
                let mut ids = BTreeSet::new();
                let mut heads = 0;
                let mut started_tx = Some(started_tx);
                while let Some(Ok(Message::Text(text))) = socket.next().await {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    if request.get("id").is_none() {
                        continue;
                    }
                    assert!(ids.insert(request["id"].to_string()), "duplicate RPC id");
                    let method = request["method"].as_str().unwrap();
                    methods.push(method.to_string());
                    if method == "thread/turns/list" {
                        heads += 1;
                    }
                    assert_ne!(
                        method, "thread/resume",
                        "prediction must never load the source thread"
                    );
                    if (case == Case::Read && method == "thread/read")
                        || (case == Case::Fork && method == "thread/fork")
                        || (case == Case::Start && method == "turn/start")
                        || (case == Case::FirstHead && method == "thread/turns/list" && heads == 1)
                        || (case == Case::SecondHead && method == "thread/turns/list" && heads == 2)
                    {
                        server_cancel.cancel();
                        tokio::task::yield_now().await;
                        if matches!(case, Case::Read | Case::FirstHead | Case::SecondHead) {
                            continue;
                        }
                    }
                    if (case == Case::UnknownFork && method == "thread/fork")
                        || (case == Case::UnknownStart && method == "turn/start")
                        || (case == Case::Cleanup && method == "thread/unsubscribe")
                    {
                        continue;
                    }
                    let result = match method {
                        "initialize" => json!({"userAgent":"test","codexHome":"/tmp"}),
                        "thread/read" => {
                            assert_eq!(request["params"], json!({"threadId":"parent"}));
                            json!({"thread":{"id":"parent","modelProvider":"openai","model":"supported"}})
                        }
                        "thread/turns/list" => {
                            assert_eq!(request["params"]["threadId"], "parent");
                            json!({"data":[{"id":"source","status":"completed"}]})
                        }
                        "thread/fork" => {
                            assert_eq!(request["params"]["experimentalPredictionMode"], true);
                            assert_eq!(request["params"]["ephemeral"], true);
                            json!({"thread":{"id":"temporary"}})
                        }
                        "turn/start" => json!({"turn":{"id":"prediction"}}),
                        "turn/interrupt" | "thread/unsubscribe" => json!({}),
                        other => panic!("unexpected request {other}"),
                    };
                    socket
                        .send(Message::Text(
                            json!({"id":request["id"],"result":result})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .unwrap();
                    if method == "turn/start" {
                        socket.send(Message::Text(json!({"method":"item/completed","params":{
                            "threadId":"parent","turnId":"source","completedAtMs":1,"item":{"type":"agentMessage","id":"foreign","text":"Original answer"}
                        }}).to_string().into())).await.unwrap();
                        socket.send(Message::Text(json!({"id":"foreign-request","method":"item/tool/call","params":{
                            "threadId":"other","turnId":"other-turn","callId":"foreign-call","namespace":null,"tool":"foreign_tool","arguments":{}
                        }}).to_string().into())).await.unwrap();
                        if case == Case::Events {
                            server_cancel.cancel();
                        }
                        if case == Case::Disconnect {
                            socket.close(None).await.unwrap();
                            break;
                        }
                        if case == Case::Stop {
                            started_tx.take().unwrap().send(()).unwrap();
                        }
                        if matches!(case, Case::Complete | Case::Cleanup) {
                            for notification in [
                                json!({"method":"item/completed","params":{"threadId":"temporary","turnId":"prediction","completedAtMs":1,
                                    "item":{"type":"agentMessage","id":"answer","text":"{\"suggestion\":\"Show an example\"}"}}}),
                                json!({"method":"turn/completed","params":{"threadId":"temporary",
                                    "turn":{"id":"prediction","status":"completed","items":[],"error":null}}}),
                            ] {
                                socket
                                    .send(Message::Text(notification.to_string().into()))
                                    .await
                                    .unwrap();
                            }
                        }
                    }
                }
                methods
            });
            let client = crate::RemoteAppServerClient::connect(crate::RemoteAppServerConnectArgs {
                endpoint: crate::RemoteAppServerEndpoint::WebSocket {
                    websocket_url: format!("ws://{address}"),
                    auth_token: None,
                },
                client_name: "test".into(),
                client_version: "0".into(),
                experimental_api: true,
                mcp_server_openai_form_elicitation: false,
                opt_out_notification_methods: Vec::new(),
                channel_capacity: 16,
            })
            .await
            .unwrap();
            let driver = CodexAppServerInterfaceDriver::new(AppServerClient::Remote(client));
            if case == Case::Lagged {
                driver
                    .client
                    .lock()
                    .await
                    .restore_ready_events(vec![AppServerEvent::Lagged { skipped: 3 }]);
            }
            let handle = driver.client.lock().await.request_handle();
            let config = PredictionConfig {
                is_enabled: true,
                prompt: "Suggest".into(),
                prompt_version: "test".into(),
                prediction_reasoning_effort: "high".into(),
                unsupported_models: Vec::new(),
            };
            let budget = PredictionBudget::new(Duration::from_secs(1), Duration::from_millis(500));
            let source = json!({"thread_id":"parent","turn_id":"source"});
            let prediction = driver.predict(&source, "request", &config, &cancel, &budget);
            let result = tokio::time::timeout(Duration::from_secs(5), async {
                let (result, ()) = tokio::join!(prediction, async {
                    if case != Case::Stop {
                        return;
                    }
                    started_rx.await.unwrap();
                    let mut before = |_| Ok(());
                    let applied = apply_control_to_target(
                        CodexTurnControl::Interrupt {
                            control_id: "stop-other".into(),
                            action: "stop".into(),
                        },
                        handle,
                        driver.next_request_id.clone(),
                        "other".into(),
                        "other-turn".into(),
                        None,
                        &mut before,
                    )
                    .await
                    .unwrap();
                    assert!(applied.result.acknowledged);
                    cancel.cancel();
                });
                result
            })
            .await
            .unwrap();
            if matches!(case, Case::UnknownFork | Case::UnknownStart | Case::Cleanup) {
                assert!(
                    matches!(&result, Err(PredictionFailure::UnknownEffect(_))),
                    "{case:?}: {result:?}"
                );
            } else if case == Case::Disconnect {
                assert!(
                    matches!(&result, Err(PredictionFailure::Transport(_))),
                    "{result:?}"
                );
            } else if case == Case::Lagged {
                assert!(
                    matches!(&result, Err(PredictionFailure::Integrity(_))),
                    "{result:?}"
                );
            } else {
                let value = match result {
                    Ok(value) => value,
                    Err(PredictionFailure::Cancelled) => json!({"suggestion":null}),
                    error => panic!("{case:?}: {error:?}"),
                };
                assert_eq!(
                    value["suggestion"],
                    if case == Case::Complete {
                        json!("Show an example")
                    } else {
                        Value::Null
                    }
                );
            }
            if matches!(
                case,
                Case::Complete
                    | Case::Start
                    | Case::Events
                    | Case::Cleanup
                    | Case::Disconnect
                    | Case::Lagged
                    | Case::Stop
            ) {
                tokio::time::timeout(Duration::from_secs(2), async {
                    let mut client = driver.client.lock().await;
                    if case == Case::Lagged {
                        assert!(matches!(
                            client.next_event().await,
                            Some(AppServerEvent::Lagged { skipped: 3 })
                        ));
                    }
                    let Some(AppServerEvent::ServerNotification(notification)) =
                        client.next_event().await
                    else {
                        panic!("foreign notification lost")
                    };
                    assert_eq!(
                        serde_json::to_value(notification).unwrap()["params"]["threadId"],
                        "parent"
                    );
                    assert!(matches!(
                        client.next_event().await,
                        Some(AppServerEvent::ServerRequest(_))
                    ));
                    if case == Case::Disconnect {
                        assert!(matches!(
                            client.next_event().await,
                            Some(AppServerEvent::Disconnected { .. })
                        ));
                    }
                })
                .await
                .unwrap();
            }
            driver.shutdown().await.unwrap();
            let methods = server.await.unwrap();
            assert!(
                methods
                    .iter()
                    .filter(|method| method.as_str() == "thread/fork")
                    .count()
                    <= 1
            );
            if matches!(case, Case::Read | Case::FirstHead) {
                assert!(!methods.iter().any(|m| m == "thread/fork"));
            }
            if matches!(case, Case::Fork | Case::SecondHead) {
                assert!(!methods.iter().any(|m| m == "turn/start"));
                assert_eq!(methods.last().unwrap(), "thread/unsubscribe");
            }
            if matches!(case, Case::Start | Case::Events | Case::Lagged) {
                assert!(methods.iter().any(|m| m == "turn/interrupt"));
                assert_eq!(methods.last().unwrap(), "thread/unsubscribe");
            }
            if case == Case::UnknownFork {
                assert_eq!(methods.last().unwrap(), "thread/fork");
            }
            if case == Case::UnknownStart {
                assert!(!methods.iter().any(|m| m == "turn/interrupt"));
                assert_eq!(methods.last().unwrap(), "thread/unsubscribe");
            }
        }
    }

    #[tokio::test]
    async fn prediction_read_only_waits_cancel_and_expire_without_dispatching_effects() {
        let cancel = tokio_util::sync::CancellationToken::new();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (result, ()) = tokio::join!(
            read_only(&cancel, Instant::now() + Duration::from_secs(1), async {
                started_tx.send(()).unwrap();
                std::future::pending::<Result<(), PredictionFailure>>().await
            }),
            async {
                started_rx.await.unwrap();
                cancel.cancel();
            },
        );
        assert!(matches!(result, Err(PredictionFailure::Cancelled)));
        let result = read_only(
            &tokio_util::sync::CancellationToken::new(),
            Instant::now() + Duration::from_millis(10),
            std::future::pending::<Result<(), PredictionFailure>>(),
        )
        .await;
        assert!(matches!(result, Err(PredictionFailure::Boundary(_))));
    }

    #[tokio::test]
    async fn prediction_source_failure_settles_exact_result_and_corrupt_receipt_refuses_replay() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = InterfaceArtifactFile::write(temp.path(), REQUEST_LABEL, b"{}").unwrap();
        let input = InterfaceInvocationEnvelope {
            artifact: ArtifactRef {
                id: octopus_descriptors::ArtifactId::new("prediction-input"),
                content: octopus_ledger::ContentHash::of_bytes(
                    &std::fs::read(artifact.manifest_path()).unwrap(),
                ),
            },
            artifact_file: artifact,
            artifact_binding_payloads: Default::default(),
            metadata: None,
        };
        let request = PredictionRequest {
            request_id: "prediction-source".into(),
            reply_to: InterfaceId::new("prediction_result"),
            source: input.artifact.clone(),
            source_movement_seq: 1,
        };
        let (arrivals, _receiver) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("world_provider_codex"),
            temp.path().into(),
            None,
            temp.path().into(),
            temp.path().join("movements.jsonl"),
            temp.path().join("blobs"),
            temp.path().join("index.sqlite"),
            Default::default(),
            arrivals,
            tokio_util::sync::CancellationToken::new(),
        );
        let root = temp.path().join("result");
        let (output, failure) = settle_result(&root, &request, &input.artifact, 2, async {
            validate_source(&ctx, &input, &request, 2).map_err(PredictionFailure::Boundary)
        })
        .await
        .unwrap();
        assert!(failure.is_none());
        assert_eq!(output["request"], serde_json::to_value(&request).unwrap());
        assert_eq!(
            output["input"],
            serde_json::to_value(&input.artifact).unwrap()
        );
        assert_eq!(output["source_movement_seq"], 2);
        assert_eq!(output["result"]["status"], "failed");
        assert_eq!(output["result"]["suggestion"], Value::Null);
        assert!(
            output["result"]["error"]
                .as_str()
                .unwrap()
                .contains("exact readable bound artifact")
        );
        let error = admit_result(
            Instant::now() + Duration::from_millis(10),
            None,
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(error.retires_driver());
        assert!(!error.result_admitted());
        let error = admit_result(Instant::now() + Duration::from_secs(1), None, async {
            Err("delivery failed".into())
        })
        .await
        .unwrap_err();
        assert!(error.retires_driver());
        assert!(!error.result_admitted());
        let error = admit_result(Instant::now() + Duration::from_secs(1), None, async {
            Ok(octopus_interfaces::DeliveryVerdict::refused(
                octopus_ledger::RefusedReason::Malformed,
                json!({}),
            ))
        })
        .await
        .unwrap_err();
        assert!(error.retires_driver());
        assert!(!error.result_admitted());
        let error = admit_result(
            Instant::now() + Duration::from_secs(1),
            Some(PredictionFailure::Transport("closed".into())),
            async { Ok(octopus_interfaces::DeliveryVerdict::Admitted { seq: 3 }) },
        )
        .await
        .unwrap_err();
        assert!(error.result_admitted());
        let (replayed, _) = settle_result(&root, &request, &input.artifact, 2, async {
            panic!("native request replayed")
        })
        .await
        .unwrap();
        assert_eq!(replayed, output);
        let mut corrupt = output;
        corrupt["result"]
            .as_object_mut()
            .unwrap()
            .remove("suggestion");
        let bytes = serde_json::to_vec(&corrupt).unwrap();
        std::fs::write(root.join("result.json"), &bytes).unwrap();
        assert!(matches!(
            settle_result(&root, &request, &input.artifact, 2, async {
                panic!("corruption replayed")
            })
            .await,
            Err(PredictionFailure::UnsettledFatal(_))
        ));
        assert_eq!(std::fs::read(root.join("result.json")).unwrap(), bytes);
        let blocked = temp.path().join("blocked");
        std::fs::create_dir_all(blocked.join("result.tmp")).unwrap();
        assert!(
            settle_result(&blocked, &request, &input.artifact, 2, async {
                panic!("incomplete receipt replayed")
            })
            .await
            .unwrap_err()
            .retires_driver()
        );
        for fatal in [false, true] {
            let failed_write = temp.path().join(fatal.to_string());
            let error = settle_result(&failed_write, &request, &input.artifact, 2, async {
                std::fs::create_dir(failed_write.join("result.tmp")).unwrap();
                if fatal {
                    Err(PredictionFailure::UnknownEffect(
                        "fork outcome unknown".into(),
                    ))
                } else {
                    Ok(json!({"status":"unavailable","suggestion":null}))
                }
            })
            .await
            .unwrap_err();
            assert!(error.retires_driver());
            assert!(!error.result_admitted());
            let (retained, _) = settle_result(&failed_write, &request, &input.artifact, 2, async {
                panic!("persistence failure regenerated prediction")
            })
            .await
            .unwrap();
            assert_eq!(retained["request"], serde_json::to_value(&request).unwrap());
            assert_eq!(
                retained["input"],
                serde_json::to_value(&input.artifact).unwrap()
            );
            assert_eq!(retained["result"]["status"], "failed");
            assert!(
                retained["result"]["error"]
                    .as_str()
                    .unwrap()
                    .contains("native outcome unknown")
            );
        }
    }

    #[test]
    fn prediction_wire_preserves_native_temporary_fork_and_paginated_reads() {
        for (method, params) in [
            (
                "thread/fork",
                json!({"threadId":"parent", "ephemeral":true, "experimentalPredictionMode":true,
                "excludeTurns":true, "threadSource":"composer_predictions"}),
            ),
            (
                "thread/turns/list",
                json!({"threadId":"parent", "limit":1, "sortDirection":"desc", "itemsView":"notLoaded"}),
            ),
            (
                "turn/start",
                json!({"threadId":"temporary", "turnTrigger":"composer_predictions", "effort":"high",
                "clientUserMessageId":"prediction-1", "input":[{"type":"text","text":"Suggest the next message","text_elements":[]}]}),
            ),
        ] {
            let request: ClientRequest =
                serde_json::from_value(json!({"id":1,"method":method,"params":params})).unwrap();
            let encoded = serde_json::to_value(request).unwrap();
            for (key, value) in params.as_object().unwrap() {
                assert_eq!(&encoded["params"][key], value, "{method} lost {key}");
            }
        }
    }
}
