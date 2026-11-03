use super::*;

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn recovered_rollout_evidence(
        &self,
        scope: &TransportStateScope,
        continuation: Option<&RecoveredToolContinuation>,
        last_artifact_label: Option<&str>,
        recovered_handoff_artifacts: &[CodexRecoveredHandoffArtifact],
        recovered_turn_start_dispatches: &[RecoveredTurnStartDispatch],
        tool_names: &BTreeMap<String, String>,
    ) -> Result<RecoveredToolResultRolloutEvidence, InterfaceDriverError> {
        let Some(continuation) = continuation else {
            if !recovered_turn_start_dispatches.is_empty() {
                return Err(InterfaceDriverError::FailedClosed(
                    "Codex recovered turn/start dispatch evidence is detached from a tool-result continuation"
                        .to_string(),
                ));
            }
            return Ok(RecoveredToolResultRolloutEvidence::default());
        };
        if last_artifact_label == Some(PROVIDER_TRANSPORT_DISPATCH_LABEL)
            && recovered_handoff_artifacts.is_empty()
        {
            return Err(InterfaceDriverError::FailedClosed(
                "Codex interrupted result recovery lost its durable transport-dispatch artifact"
                    .to_string(),
            ));
        }
        let rollout_path = self
            .transport_state
            .lock()
            .await
            .get(scope)
            .and_then(|state| state.rollout_path.clone())
            .ok_or_else(|| {
                InterfaceDriverError::FailedClosed(
                    "Codex interrupted result recovery has no exact rollout path".to_string(),
                )
            })?;
        let mut native_activity = continuation.activity.value.clone();
        if let Some(tool) = native_activity
            .get("tool")
            .and_then(serde_json::Value::as_str)
        {
            let name = tool_names
                .iter()
                .find_map(|(name, exact)| (exact == tool).then_some(name))
                .ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(format!(
                        "recovered tool '{tool}' has no admitted model name"
                    ))
                })?;
            native_activity["tool"] = serde_json::json!(decode_tool_name(name)?);
        }
        let evidence = recovered_tool_result_rollout_evidence(
            &rollout_path,
            &continuation.item,
            Some(&native_activity),
        )?;
        if evidence.native_receipt {
            let field = |name: &str| {
                native_activity.get(name).and_then(serde_json::Value::as_str).map(str::to_owned).ok_or_else(|| {
                    InterfaceDriverError::FailedClosed(format!(
                        "Codex recovered tool activity has no native {name}"
                    ))
                })
            };
            let handle = self.client.lock().await.request_handle();
            let entries = native_history::read_native_items(
                &handle,
                &self.next_request_id,
                &field("thread_id")?,
                &field("turn_id")?,
                vec![field("call_id")?],
            )
            .await?;
            verify_native_tool_receipt(&entries[0], &continuation.item, &native_activity)?;
        }
        if let Some(last_artifact_label) = last_artifact_label
            && !evidence.output_persisted
            && last_artifact_label != PROVIDER_TRANSPORT_DISPATCH_LABEL
        {
            return Err(InterfaceDriverError::FailedClosed(format!(
                "Codex interrupted result recovery handed '{}' after dispatch without persisting the exact native tool result",
                last_artifact_label
            )));
        }
        reconcile_recovered_successor_dispatch(&evidence, recovered_turn_start_dispatches)?;
        Ok(evidence)
    }
}
