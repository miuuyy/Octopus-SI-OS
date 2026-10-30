use std::sync::Arc;
use octopus_interfaces::{InterfaceArtifactFile, InterfaceInvocationEnvelope};
use octopus_tool_door_runtime::{ToolProcessEnvironment, ToolProcessHandler, ToolProcessInvocation, ToolProcessIo, ToolProcessError, WorkerEnvironmentReleaseContext};
use octopus_tool_handlers::{ToolResultArtifact, ToolResultError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use crate::{DecodedRoomInvocation, BrowserOperation, capability, decode_room_invocation, response_artifact};
use crate::native_transport::NativeTransport;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WaitCompletionConfig { request_tools: Vec<String>, completion_tool: String }
impl WaitCompletionConfig {
    pub(crate) fn validate(&self, decoded: &DecodedRoomInvocation, observer: bool) -> Result<(), String> {
        if self.request_tools.is_empty() || self.completion_tool.is_empty()
            || self.request_tools.iter().any(|id| id.is_empty() || id == &self.completion_tool)
            || self.request_tools.iter().collect::<std::collections::BTreeSet<_>>().len() != self.request_tools.len()
            || !decoded.formation.granted_tools.contains(&self.completion_tool)
            || self.request_tools.iter().any(|id| !decoded.formation.granted_tools.contains(id))
            || if observer { decoded.call.tool_id != self.completion_tool || decoded.operation != BrowserOperation::WaitComplete }
                else { !self.request_tools.contains(&decoded.call.tool_id) || decoded.operation != BrowserOperation::Wait } {
            return Err("Wait source and observer must be exact selected tools in this formed room".into());
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Waiter {
    wait_id: String, host_generation: String, worker_environment_id: String, formation_pin: String,
    browser_context_id: String, target_id: String, debugger_epoch: u64, start_source_movement_seq: u64,
    start_artifact_content_hash: String, condition_digest: String,
}
pub(crate) fn validate_waiter(value: &Value, decoded: &DecodedRoomInvocation) -> Result<(), String> {
    let waiter: Waiter = serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    for (value, length) in [(&waiter.wait_id,32),(&waiter.host_generation,32),(&waiter.formation_pin,64),(&waiter.start_artifact_content_hash,64),(&waiter.condition_digest,64)] {
        if value.len()!=length || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) { return Err("Waiter digest or identity is invalid".into()); }
    }
    if waiter.worker_environment_id != decoded.formation.worker_environment_id.as_str() || waiter.formation_pin != decoded.formation.formation_pin
        || waiter.start_source_movement_seq == 0 || waiter.start_source_movement_seq > 9_007_199_254_740_991 || waiter.debugger_epoch > 9_007_199_254_740_991
        || [&waiter.target_id,&waiter.browser_context_id].into_iter().any(|value| value.is_empty() || value.len()>256 || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte==b'_' || byte==b'-')) {
        return Err("Waiter does not identify this exact formed browser source".into());
    }
    Ok(())
}

pub(crate) async fn finish(envelope: &InterfaceInvocationEnvelope, response: ToolResultArtifact, transport: Option<&NativeTransport>, capture: Option<Value>) -> Result<InterfaceArtifactFile, ToolProcessError> {
    let value = serde_json::to_value(&response).map_err(|error| capability(error.to_string()))?;
    let artifact = response_artifact(envelope, response);
    if let Some(transport) = transport {
        if let Err(error) = transport.end(if artifact.is_ok() { capture } else { None }).await {
            let tool = value.get("tool").and_then(Value::as_str).ok_or_else(|| capability("Browser terminal result lost its tool"))?;
            let correlation = value.get("correlation_id").and_then(Value::as_str).map(str::to_string);
            return response_artifact(envelope, ToolResultArtifact::failure(tool, correlation, ToolResultError::new("browser_invocation_end_uncertain", error)
                .with_details(serde_json::Map::from_iter([("operation_result".into(),value.clone())]))));
        }
    }
    artifact
}

pub(crate) struct BrowserWaitCompletion { transport: NativeTransport, config: WaitCompletionConfig, execution: tokio::sync::Mutex<()> }
impl BrowserWaitCompletion {
    pub(crate) fn load(environment: ToolProcessEnvironment) -> Result<Self,String> {
        let (transport,config) = crate::browser_session::completion_transport(&environment)?;
        Ok(Self {transport,config,execution:tokio::sync::Mutex::new(())})
    }
    pub(crate) async fn receive(&self, invocation: ToolProcessInvocation) -> Result<InterfaceArtifactFile,ToolProcessError> {
            let _execution = self.execution.lock().await;
            let decoded = decode_room_invocation(invocation)?;
            self.config.validate(&decoded, true).map_err(capability)?;
            let args = decoded.call.arguments.as_object().filter(|value| value.len()==1).ok_or_else(|| capability("Wait completion requires only its exact handle"))?;
            let waiter = args.get("waiter").ok_or_else(|| capability("Wait completion has no handle"))?;
            validate_waiter(waiter, &decoded).map_err(capability)?;
            self.transport.authorize(&decoded).await.map_err(capability)?;
            let mut member = None;
            let result = async {
                member = Some(self.transport.connect(None).await?);
                let connection = member.as_mut().ok_or("Wait observer connection disappeared")?;
                if connection.generation != waiter["host_generation"].as_str().ok_or("Waiter has no host generation")? { return Err("Waiter belongs to a previous native engine generation".into()); }
                connection.request("OctopusBrowser.completeWait", json!({"waiter":waiter}), self.transport.request_timeout).await
            }.await;
            let mut capture = None;
            let response = match result {
                Ok(body) => {
                    if body.get("waiter") != Some(waiter) || !matches!(body["status"].as_str(), Some("satisfied"|"failed"|"cancelled"|"source_lost")) {
                        ToolResultArtifact::failure(&decoded.call.tool_id,decoded.correlation_id.clone(),ToolResultError::new("invalid_wait_result","Native completion returned a different source or nonterminal result"))
                    } else {
                        let digest = format!("{:x}",Sha256::digest(octopus_descriptors::canonical_bytes(&body)));
                        capture = Some(json!({"wait_id":waiter["wait_id"],"outcome_digest":digest}));
                        let mut evidence = body.clone(); evidence["completion_source_movement_seq"] = json!(decoded.source_movement_seq);
                        if body["status"] == "satisfied" { ToolResultArtifact::success(&decoded.call.tool_id,decoded.correlation_id.clone(),evidence) }
                        else { ToolResultArtifact::failure(&decoded.call.tool_id,decoded.correlation_id.clone(),ToolResultError::new("wait_observation_failed","The independently admitted browser wait did not satisfy its condition").with_details(serde_json::Map::from_iter([("observation".into(),evidence)]))) }
                    }
                }
                Err(error) => ToolResultArtifact::failure(&decoded.call.tool_id,decoded.correlation_id.clone(),ToolResultError::new("wait_completion_unavailable_or_uncertain",error)),
            };
            let finished = finish(&decoded.envelope,response,Some(&self.transport),capture).await;
            drop(member);
            finished
    }
}
impl ToolProcessHandler for BrowserWaitCompletion {
    fn substrate_family(&self) -> &str { crate::SUBSTRATE_FAMILY }
    fn execute<'a>(&'a self, invocation: ToolProcessInvocation, _io: ToolProcessIo) -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<Vec<InterfaceArtifactFile>,ToolProcessError>> + Send + 'a>> {
        Box::pin(async move { self.receive(invocation).await.map(|artifact|vec![artifact]) })
    }
    fn release_with_context<'a>(&'a self, context: WorkerEnvironmentReleaseContext<'a>) -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<(),ToolProcessError>> + Send + 'a>> {
        Box::pin(async move { self.transport.release(context).await.map_err(capability) })
    }
    fn quiesce<'a>(&'a self) -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<(),ToolProcessError>> + Send + 'a>> { Box::pin(async move { self.transport.shutdown().await; Ok(()) }) }
}
pub async fn run_browser_wait_completion_tool_process() -> Result<(),String> {
    let environment = ToolProcessEnvironment::from_env()?;
    octopus_tool_door_runtime::run_tool_process(Arc::new(BrowserWaitCompletion::load(environment)?)).await
}
