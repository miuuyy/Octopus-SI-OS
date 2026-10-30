use super::composer_prediction::{PredictionFailure, effect_rpc};
use super::*;
use octopus_interface_transport::{InterfaceLiftContext, SystemInterfaceEmissionArtifact};
use octopus_run_authoring::{CheckedRunEvidence, CheckedRunEvidenceReader, FileCheckedRunReader};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

pub(crate) const REQUEST_LABEL: &str = "provider.client_generation.request";
pub(crate) const RESULT_LABEL: &str = "provider.client_generation.result";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GenerationRequest {
    request_id: String,
    reply_to: InterfaceId,
    source: ArtifactRef,
    source_movement_seq: u64,
    text_pointer: String,
    instructions: String,
    output_schema: Value,
}

pub(crate) async fn run(
    ctx: &InterfaceLiftContext,
    driver: &CodexAppServerInterfaceDriver,
    input: &InterfaceInvocationEnvelope,
    sequence: u64,
) -> Result<(), PredictionFailure> {
    let request: GenerationRequest = serde_json::from_slice(
        &input
            .read_artifact_payload_bounded(32_768)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("invalid client generation request: {e}"))?;
    if request.request_id.is_empty()
        || request.request_id.len() > 128
        || request.request_id.chars().any(char::is_control)
        || request.instructions.trim().is_empty()
        || request.instructions.len() > 8_192
        || !request.text_pointer.starts_with('/')
        || !request.output_schema.is_object()
    {
        return Err("client generation request has invalid fields".into());
    }
    if !input
        .granted_tool_names()
        .map_err(|e| e.to_string())?
        .is_empty()
        || !input
            .worker_environment_kinds()
            .map_err(|e| e.to_string())?
            .is_empty()
        || !input
            .allowed_interfaces()
            .map_err(|e| e.to_string())?
            .contains(&request.reply_to.to_string())
    {
        return Err(
            "client generation needs a tool-free grant and its exact return interface".into(),
        );
    }
    let record =
        octopus_run_record::RunRecord::from_bytes(&ctx.pinned_run_record_bytes(input)?)
            .map_err(|e| e.to_string())?;
    if !record.checkpoints.iter().any(|cp| {
        record.active_checkpoints.contains(&cp.id)
            && cp.inbound_interface() == Some(&request.reply_to)
            && cp.outbound_interface_bindings.is_empty()
            && cp
                .interface_binding
                .as_ref()
                .is_some_and(|binding| binding.admits(RESULT_LABEL))
    }) {
        return Err("client generation has no result-only return binding".into());
    }
    let root = ctx
        .interface_var_dir()
        .join("client-generations")
        .join(sequence.to_string());
    let receipt = root.join("result.json");
    let expected = json!({"request":request, "input":input.artifact});
    let output = match std::fs::read(&receipt) {
        Ok(bytes) => {
            let retained: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if retained["request"] != expected["request"]
                || retained["input"] != expected["input"]
                || !retained["result"]["ok"].is_boolean()
            {
                return Err(
                    "client generation receipt has a different input or invalid result".into(),
                );
            }
            retained
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let text = source_text(ctx, input, &request, sequence)?;
            let mut output = expected;
            output["result"] = json!({"ok":false,"error":"Client generation was interrupted before its result was saved; the model request will not be repeated."});
            persist(&root, &receipt, &output)?;
            let result = driver
                .generate_client_text(input, &request, text, &ctx.cancel)
                .await;
            output["result"] = match result {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(error) => json!({"ok":false,"error":error.to_string()}),
            };
            persist(&root, &receipt, &output)?;
            output
        }
        Err(error) => return Err(error.to_string().into()),
    };
    let artifact = InterfaceArtifactFile::write(
        root,
        RESULT_LABEL,
        &serde_json::to_vec(&output).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    ctx.hand_system_emission(
        SystemInterfaceEmissionArtifact::Materialized(artifact),
        sequence,
        request.reply_to,
    )
    .await
    .map_err(PredictionFailure::Boundary)?;
    Ok(())
}

fn source_text(
    ctx: &InterfaceLiftContext,
    input: &InterfaceInvocationEnvelope,
    request: &GenerationRequest,
    sequence: u64,
) -> Result<String, String> {
    if request.source_movement_seq >= sequence {
        return Err("client generation source must precede its request".into());
    }
    let bound = input
        .artifact_binding_payloads
        .iter()
        .find(|item| item.artifact == request.source)
        .ok_or("client generation source is not a readable bound artifact")?;
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
    let movement = reader
        .admitted_movement(request.source_movement_seq)?
        .ok_or("client generation source movement is missing")?;
    let (_, bytes) = reader.artifact(&request.source)?;
    if movement.artifact.as_ref() != Some(&request.source)
        || bytes.as_slice() != bound.read_bytes().map_err(|e| e.to_string())?.as_ref()
    {
        return Err("client generation source does not match its admitted artifact".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let text = value
        .pointer(&request.text_pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or("client generation source has no text at its declared pointer")?;
    let excerpt: String = text.chars().take(8_000).collect();
    Ok(
        serde_json::to_string(&json!({"text":excerpt,"truncated":excerpt.len() < text.len()}))
            .map_err(|e| e.to_string())?,
    )
}

fn persist(root: &Path, receipt: &Path, output: &Value) -> Result<(), String> {
    use std::io::Write;
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let temporary = root.join("result.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(output).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(temporary, receipt).map_err(|e| e.to_string())?;
    std::fs::File::open(root)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())
}

impl CodexAppServerInterfaceDriver {
    async fn generate_client_text(
        &self,
        input: &InterfaceInvocationEnvelope,
        request: &GenerationRequest,
        text: String,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Value, PredictionFailure> {
        let model = self
            .resolve_private_config_string("model")
            .map_err(|e| e.to_string())?
            .ok_or("client generation provider has no selected model")?;
        if !input
            .interface_policy()
            .map_err(|e| e.to_string())?
            .and_then(|policy| policy.get("models"))
            .and_then(Value::as_array)
            .is_some_and(|models| models.contains(&json!(model)))
        {
            return Err("client generation model is outside the formed grant".into());
        }
        self.validate_native_service_tier(&model)
            .await
            .map_err(|e| e.to_string())?;
        let mut params = isolated_thread_start_params(
            Some(model.clone()),
            request.instructions.clone(),
            Vec::new(),
        );
        params.ephemeral = Some(true);
        params.dynamic_tools = Some(Vec::new());
        params.config = Some(
            self.native_tool_thread_config()
                .map_err(|e| e.to_string())?,
        );
        params
            .config
            .as_mut()
            .unwrap()
            .insert("web_search".into(), json!("disabled"));
        params.service_tier = self.bound_service_tier().map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut client = self.client.lock().await;
        let started = effect_rpc(
            self,
            &client,
            "thread/start",
            serde_json::to_value(params).map_err(|e| e.to_string())?,
            Some(cancel),
            deadline,
        )
        .await?;
        let thread = started["thread"]["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                PredictionFailure::UnknownEffect(
                    "client generation thread identity is missing".into(),
                )
            })?
            .to_owned();
        let mut turn_id = None;
        let mut deferred = Vec::new();
        let operation = async {
            if started["model"] != model { return Err("client generation provider changed the selected model".into()); }
            validate_native_service_tier_response(self.bound_service_tier().map_err(|e| e.to_string())?.as_ref().and_then(Option::as_deref),
                started["serviceTier"].as_str()).map_err(|e| e.to_string())?;
            let turn = effect_rpc(self, &client, "turn/start", json!({"threadId":thread,
                "input":[{"type":"text","text":text,"text_elements":[]}],
                "effort":self.resolve_private_config_string("reasoning_effort").map_err(|e| e.to_string())?,
                "outputSchema":request.output_schema}), Some(cancel), deadline).await?;
            let id = turn["turn"]["id"].as_str().filter(|id| !id.is_empty())
                .ok_or_else(|| PredictionFailure::UnknownEffect("client generation turn identity is missing".into()))?.to_owned();
            turn_id = Some(id.clone());
            let mut answer = None;
            loop {
                let event = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(PredictionFailure::Cancelled),
                    _ = tokio::time::sleep_until(deadline) => return Err("client generation timed out".into()),
                    event = client.next_event() => event,
                };
                match event {
                    Some(AppServerEvent::ServerNotification(notification)) => {
                        let value = serde_json::to_value(&notification).map_err(|e| e.to_string())?;
                        if value["params"]["threadId"] != thread { deferred.push(AppServerEvent::ServerNotification(notification)); continue; }
                        if value["method"] == "item/completed" && value["params"]["turnId"] == id && value["params"]["item"]["type"] == "agentMessage" {
                            answer = value["params"]["item"]["text"].as_str().map(str::to_owned);
                        }
                        if value["method"] == "turn/completed" && value["params"]["turn"]["id"] == id {
                            if value["params"]["turn"]["status"] != "completed" { return Err("client generation did not complete".into()); }
                            break;
                        }
                    }
                    Some(AppServerEvent::ServerRequest(request)) => {
                        let value = serde_json::to_value(&request).map_err(|e| e.to_string())?;
                        if value["params"]["threadId"] == thread { return Err("client generation requested a tool with no tool grant".into()); }
                        deferred.push(AppServerEvent::ServerRequest(request));
                    }
                    Some(AppServerEvent::Disconnected { message }) => return Err(PredictionFailure::Transport(message)),
                    Some(AppServerEvent::Lagged { skipped }) => return Err(PredictionFailure::Integrity(format!("client generation lost {skipped} events"))),
                    None => return Err(PredictionFailure::Transport("client generation connection closed".into())),
                    Some(event) => deferred.push(event),
                }
            }
            let answer = answer.ok_or("client generation returned no answer")?;
            if answer.len() > 16_384 { return Err("client generation result exceeds its bound".into()); }
            serde_json::from_str(&answer).map_err(|e| PredictionFailure::Boundary(format!("client generation returned invalid JSON: {e}")))
        }.await;
        let cleanup = Instant::now() + Duration::from_secs(5);
        let interrupted = if operation.is_err()
            && let Some(turn) = turn_id
        {
            effect_rpc(
                self,
                &client,
                "turn/interrupt",
                json!({"threadId":thread,"turnId":turn}),
                None,
                cleanup,
            )
            .await
            .map(|_| ())
        } else {
            Ok(())
        };
        let closed = effect_rpc(
            self,
            &client,
            "thread/unsubscribe",
            json!({"threadId":thread}),
            None,
            cleanup,
        )
        .await;
        client.restore_ready_events(deferred);
        interrupted?;
        closed?;
        operation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use octopus_descriptors::ArtifactId;
    use octopus_ledger::ContentHash;
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn client_generation_keeps_model_isolates_context_and_refuses_tools() {
        for (asks_for_tool, interrupt_fails) in [(false, false), (true, false), (true, true)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let mut socket =
                    tokio_tungstenite::accept_async(listener.accept().await.unwrap().0)
                        .await
                        .unwrap();
                let mut methods = Vec::new();
                while let Some(Ok(Message::Text(text))) = socket.next().await {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    if request.get("id").is_none() {
                        continue;
                    }
                    let method = request["method"].as_str().unwrap();
                    methods.push(method.to_owned());
                    if method == "turn/interrupt" && interrupt_fails {
                        socket.send(Message::Text(json!({"id":request["id"],"error":{"code":-32000,"message":"interrupt failed"}}).to_string().into())).await.unwrap();
                        continue;
                    }
                    let result = match method {
                        "initialize" => json!({"userAgent":"test","codexHome":"/tmp"}),
                        "thread/start" => {
                            assert_eq!(request["params"]["model"], "chosen-model");
                            assert_eq!(request["params"]["environments"], json!([]));
                            assert_eq!(request["params"]["dynamicTools"], json!([]));
                            assert_eq!(request["params"]["ephemeral"], true);
                            assert_eq!(request["params"]["config"]["web_search"], "disabled");
                            json!({"thread":{"id":"temporary"},"model":"chosen-model"})
                        }
                        "turn/start" => {
                            assert_eq!(request["params"]["effort"], "medium");
                            assert_eq!(request["params"]["input"][0]["text"], "bound source only");
                            assert_eq!(request["params"]["outputSchema"]["type"], "object");
                            json!({"turn":{"id":"generation"}})
                        }
                        "turn/interrupt" | "thread/unsubscribe" => json!({}),
                        method => panic!("undeclared generation operation {method}"),
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
                        let events = if asks_for_tool {
                            vec![
                                json!({"id":"tool","method":"item/tool/call","params":{"threadId":"temporary","turnId":"generation",
                                "callId":"forbidden","namespace":null,"tool":"exec","arguments":{}}}),
                            ]
                        } else {
                            vec![
                                json!({"method":"item/completed","params":{"threadId":"temporary","turnId":"generation","completedAtMs":1,
                                "item":{"type":"agentMessage","id":"answer","text":"{\"title\":\"Починить прокрутку\"}"}}}),
                                json!({"method":"turn/completed","params":{"threadId":"temporary","turn":{"id":"generation","status":"completed","items":[],"error":null}}}),
                            ]
                        };
                        for event in events {
                            socket
                                .send(Message::Text(event.to_string().into()))
                                .await
                                .unwrap();
                        }
                    }
                    if method == "thread/unsubscribe" {
                        break;
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
            let mut driver = CodexAppServerInterfaceDriver::new(AppServerClient::Remote(client));
            driver
                .interface_private_config
                .insert("model".into(), json!("chosen-model"));
            driver
                .interface_private_config
                .insert("reasoning_effort".into(), json!("medium"));
            let root = tempfile::tempdir().unwrap();
            let file =
                InterfaceArtifactFile::write(root.path().to_path_buf(), REQUEST_LABEL, b"{}")
                    .unwrap();
            let mut grant = octopus_interfaces::GrantProjection::default();
            grant.interface_policy.insert(
                "provider".into(),
                BTreeMap::from([("models".into(), json!(["chosen-model"]))]),
            );
            let input = InterfaceInvocationEnvelope {
                artifact: ArtifactRef {
                    id: ArtifactId::new("request"),
                    content: ContentHash::of_bytes(b"{}"),
                },
                artifact_file: file,
                artifact_binding_payloads: Vec::new(),
                metadata: Some(
                    octopus_interfaces::InvocationMetadata::new(grant).with_runtime(
                        RuntimeInvocationMetadata {
                            run: octopus_interfaces::RunId::new("title"),
                            ee: octopus_descriptors::EeId::new("client"),
                            worker: InterfaceId::new("worker"),
                            checkpoint: octopus_descriptors::CheckpointId::new("input"),
                            interface: InterfaceId::new("provider"),
                            epoch: 0,
                            run_record_hash: None,
                        },
                    ),
                ),
            };
            let request = GenerationRequest {
                request_id: "title".into(),
                reply_to: InterfaceId::new("result"),
                source: input.artifact.clone(),
                source_movement_seq: 1,
                text_pointer: "/text".into(),
                instructions: "Name the supplied text".into(),
                output_schema: json!({"type":"object"}),
            };
            let result = driver
                .generate_client_text(
                    &input,
                    &request,
                    "bound source only".into(),
                    &tokio_util::sync::CancellationToken::new(),
                )
                .await;
            if interrupt_fails {
                assert!(result.unwrap_err().to_string().contains("interrupt failed"));
            } else if asks_for_tool {
                assert!(result.unwrap_err().to_string().contains("no tool grant"));
            } else {
                assert_eq!(result.unwrap(), json!({"title":"Починить прокрутку"}));
            }
            let methods = server.await.unwrap();
            assert_eq!(methods.contains(&"turn/interrupt".into()), asks_for_tool);
            assert_eq!(
                methods.last().map(String::as_str),
                Some("thread/unsubscribe")
            );
        }
    }
}
