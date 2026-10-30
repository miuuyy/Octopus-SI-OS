use super::*;

const INTERFACE_OPERATION_SCOPE: &str = "interface_operation";

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperationOperatorRequest {
    schema_version: u32,
    approval_scope: String,
    pub(crate) request_id: String,
    pub(crate) call_id: String,
    pub(crate) interface: InterfaceId,
    pub(crate) operation: String,
    description: String,
    tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    annotations: Option<serde_json::Map<String, serde_json::Value>>,
    arguments: serde_json::Value,
    run: String,
    ee: String,
    worker: InterfaceId,
    source_artifact: serde_json::Value,
}

fn reject(message: impl Into<String>) -> InterfaceDriverError {
    InterfaceDriverError::FailedClosed(format!("interface operation approval: {}", message.into()))
}

impl OperationOperatorRequest {
    pub(crate) fn from_payload(bytes: &[u8]) -> Result<Option<Self>, InterfaceDriverError> {
        match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => Self::from_value(&value),
            Err(_) => Ok(None),
        }
    }

    pub(crate) fn from_value(value: &serde_json::Value) -> Result<Option<Self>, InterfaceDriverError> {
        let Some(request) = value.pointer("/result/operator_request") else {
            return Ok(None);
        };
        if request.get("approval_scope").and_then(serde_json::Value::as_str)
            != Some(INTERFACE_OPERATION_SCOPE)
        {
            return Ok(None);
        }
        if value.get("ok") != Some(&serde_json::Value::Bool(true))
            || value.pointer("/result/effect_performed") != Some(&serde_json::Value::Bool(false))
        {
            return Err(reject("request result must explicitly prove no effect"));
        }
        let request: Self = serde_json::from_value(request.clone())
            .map_err(|error| reject(format!("malformed request: {error}")))?;
        request.validate()?;
        if value.get("operation").and_then(serde_json::Value::as_str) != Some(request.operation.as_str()) {
            return Err(reject("request differs from the operation result that carries it"));
        }
        Ok(Some(request))
    }

    fn validate(&self) -> Result<(), InterfaceDriverError> {
        let exact = |value: &str| {
            !value.is_empty() && value.trim() == value && value.len() <= 4096 && !value.contains('\0')
        };
        if self.schema_version != 1
            || self.approval_scope != INTERFACE_OPERATION_SCOPE
            || !self.arguments.is_object()
            || [
                self.call_id.as_str(),
                self.interface.as_str(),
                self.operation.as_str(),
                self.tool.as_str(),
                self.run.as_str(),
                self.ee.as_str(),
                self.worker.as_str(),
            ]
            .into_iter()
            .any(|value| !exact(value))
        {
            return Err(reject("request lost its exact call, interface or run identity"));
        }
        let mut commitment = serde_json::to_value(self).map_err(|error| reject(error.to_string()))?;
        commitment
            .as_object_mut()
            .ok_or_else(|| reject("request did not encode as an object"))?
            .remove("request_id");
        let digest = blake3::hash(&octopus_descriptors::canonical_bytes(&commitment));
        if self.request_id != format!("blake3:{}", digest.to_hex()) {
            return Err(reject("request identity differs from its immutable commitment"));
        }
        Ok(())
    }

    pub(crate) fn validate_source(
        &self,
        input: &InterfaceInvocationEnvelope,
        pending: Option<&CodexPendingContinuity>,
    ) -> Result<(), InterfaceDriverError> {
        let metadata = input.metadata()?;
        let runtime = input.runtime()?;
        let bindings = metadata
            .checkpoint_binding_policy
            .as_ref()
            .ok_or_else(|| reject("missing admitted checkpoint binding snapshot"))?;
        if bindings.inbound.interface != self.interface
            || runtime.run.as_str() != self.run
            || runtime.ee.as_str() != self.ee
            || runtime.worker != self.worker
            || metadata.verification_failure.is_some()
            || !metadata.grant.interfaces.iter().any(|value| value == self.interface.as_str())
        {
            return Err(reject("request did not arrive from its own interface in this run's grant"));
        }
        if let Some(CodexPendingContinuity::InterfaceOperation { call_id, operation, interface, .. }) = pending
            && call_id == &self.call_id
            && (operation != &self.operation || interface != &self.interface)
        {
            return Err(reject("request names another operation for the pending native call"));
        }
        Ok(())
    }

    pub(crate) fn projection(&self, config: &OperatorContinuityConfig) -> CodexTurnOutput {
        let params = serde_json::json!({"request_id": self.request_id, "call_id": self.call_id});
        let inputs = config
            .approval_inputs(&self.request_id)
            .into_iter()
            .map(|mut input| {
                input.required_verified_artifact.params = params.clone();
                input.also_deliver_to_interface = Some(self.interface.clone());
                input
            })
            .collect();
        let name = self
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("title"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&self.tool);
        CodexTurnOutput::client(
            OPERATOR_APPROVAL_REQUEST_LABEL,
            serde_json::json!({
                "kind": INTERFACE_OPERATION_SCOPE,
                "request_id": self.request_id,
                "call_id": self.call_id,
                "title": format!("Allow {name}?"),
                "detail": {
                    "approval_scope": INTERFACE_OPERATION_SCOPE,
                    "interface": self.interface,
                    "operation": self.operation,
                    "tool": self.tool,
                    "description": self.description,
                    "annotations": self.annotations,
                    "arguments": self.arguments,
                },
            }),
        )
        .with_verified_inputs(inputs)
    }
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn retain_operation_operator_wait(
        &self,
        scope: &TransportStateScope,
        sink: &TurnSink<'_>,
        control: &CodexTurnControlBroker,
        handoff_barrier: &Arc<ProviderHandoffBarrier>,
        pending: Option<&CodexPendingContinuity>,
        call_id: &str,
        output: CodexTurnOutput,
    ) -> Result<(), InterfaceDriverError> {
        let waits = matches!(pending, Some(CodexPendingContinuity::InterfaceOperation { call_id: pending, .. }) if pending == call_id);
        let (Some(state), true) = (sink.continuity_snapshot(), waits) else {
            return sink.send(output);
        };
        let Some(pending) = self.pending_request(scope, &format!("tool:{call_id}")).await? else {
            return sink.send(output);
        };
        self.validate_pending_continuity(scope, Some(&state), &pending, state.context_generation)
            .await?;
        let guard = pending.effect_gate.lock().await;
        self.ensure_pending_request_current(scope, &pending).await?;
        if pending.turn_interrupted.load(std::sync::atomic::Ordering::Acquire) {
            return Err(reject("approval wait follows native interruption"));
        }
        sink.send(output)?;
        control.publish(CodexTurnControlTarget::Suspended {
            handle: self.client.lock().await.request_handle(),
            request_ids: self.next_request_id.clone(),
            pending_requests: self.pending_server_requests.clone(),
            scope: scope.clone(),
            output: Arc::downgrade(sink.emit),
            continuity: sink.continuity_snapshot(),
            handoff_barrier: handoff_barrier.clone(),
            effect_gate: pending.effect_gate.clone(),
            turn_interrupted: pending.turn_interrupted.clone(),
            thread_id: pending.thread_id.clone(),
            turn_id: pending.turn_id.clone(),
        });
        drop(guard);
        Ok(())
    }
}

pub(crate) fn operation_decision_call(
    input: &InterfaceInvocationEnvelope,
    request_id: &str,
) -> Result<Option<String>, InterfaceDriverError> {
    let metadata = input.metadata()?;
    let runtime = input.runtime()?;
    let outbound = &metadata.outbound_interface_bindings;
    let [required] = metadata.required_verified_artifacts.as_slice() else {
        return Ok(None);
    };
    if outbound.len() != 2
        || outbound[0] != runtime.interface
        || outbound[1] == runtime.interface
        || required.artifact_label != OPERATOR_APPROVAL_RESPONSE_LABEL
        || required.params.get("request_id").and_then(serde_json::Value::as_str) != Some(request_id)
        || !request_id.starts_with("blake3:")
    {
        return Ok(None);
    }
    let call_id = required
        .params
        .get("call_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| reject("decision requirement has no exact call identity"))?;
    if metadata.checkpoint_admission.as_ref().is_none_or(|admission| {
        !admission.complete
            || admission.artifact_label != OPERATOR_APPROVAL_RESPONSE_LABEL
            || admission.required_artifact_labels != [OPERATOR_APPROVAL_RESPONSE_LABEL]
    }) || !metadata.grant.mark_outcomes.iter().any(|mark| mark == required.mark.as_str())
        || metadata.verification_failure.is_some()
    {
        return Err(reject("decision lacks its exact admitted verifier/mark edge"));
    }
    Ok(Some(call_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gated_result() -> serde_json::Value {
        let mut request = serde_json::json!({
            "schema_version": 1, "approval_scope": "interface_operation", "call_id": "call-1",
            "interface": "world_gmail", "operation": "gmail.send_email", "description": "Send an email",
            "tool": "send_email", "annotations": {"destructiveHint": false, "openWorldHint": true},
            "arguments": {"to": ["a@example.com"]}, "run": "run-1", "ee": "ee-1",
            "worker": "system_worker_test", "source_artifact": {"id": "request", "content": [0, 1]}
        });
        let digest = blake3::hash(&octopus_descriptors::canonical_bytes(&request));
        request["request_id"] = serde_json::json!(format!("blake3:{}", digest.to_hex()));
        serde_json::json!({"label": "mcp.tool.result", "operation": "gmail.send_email", "correlation_id": "call-1",
            "ok": true, "result": {"effect_performed": false, "operator_request": request}})
    }

    #[test]
    fn an_operation_request_projects_a_verified_decision_for_its_own_interface() {
        let result = gated_result();
        let request = OperationOperatorRequest::from_value(&result).unwrap().unwrap();
        let output = request.projection(&test_operator_continuity_config());
        assert_eq!(output.label, OPERATOR_APPROVAL_REQUEST_LABEL);
        assert_eq!(output.value["kind"], "interface_operation");
        assert_eq!(output.value["detail"]["tool"], "send_email");
        let ProviderEmissionRoute::ClientProjectionWithVerifiedInputs { inputs } = &output.route else {
            panic!("approval request carries no verified inputs");
        };
        assert_eq!(inputs.len(), 2);
        for input in inputs {
            assert_eq!(input.also_deliver_to_interface, Some(InterfaceId::new("world_gmail")));
            assert_eq!(input.also_deliver_to_tool, None);
            assert_eq!(input.required_verified_artifact.params,
                serde_json::json!({"request_id": request.request_id, "call_id": "call-1"}));
        }
        let mut changed = result.clone();
        changed["result"]["operator_request"]["arguments"]["to"][0] = serde_json::json!("b@example.com");
        assert!(OperationOperatorRequest::from_value(&changed).is_err());
        let mut performed = result.clone();
        performed["result"]["effect_performed"] = serde_json::json!(true);
        assert!(OperationOperatorRequest::from_value(&performed).is_err());
        let mut ordinary = result;
        ordinary["result"] = serde_json::json!({"content": []});
        assert!(OperationOperatorRequest::from_value(&ordinary).unwrap().is_none());
    }
}
