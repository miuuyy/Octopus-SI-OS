use super::*;

pub(crate) struct TurnStartTriggers<'a, 's> {
    pub(crate) sink: &'a TurnSink<'s>,
    pub(crate) lifecycle: Option<&'a LifecycleBinding>,
    pub(crate) run: &'a str,
    pub(crate) source_artifact: &'a ArtifactRef,
    pub(crate) thread_id: &'a str,
    pub(crate) cold_thread: bool,
    pub(crate) first_turn_of_run: bool,
}

impl TurnStartTriggers<'_, '_> {
    pub(crate) fn held(
        &self,
        turn_input: &[UserInput],
        tool_output: Option<&TurnToolOutput>,
        additional_context: &BTreeMap<String, octopus_appserver_proto::AdditionalContextEntry>,
    ) -> Result<bool, InterfaceDriverError> {
        let TurnStartTriggers {
            sink,
            lifecycle,
            run,
            source_artifact,
            thread_id,
            cold_thread,
            first_turn_of_run,
        } = *self;
        let serialized_turn_input = held_turn_start(turn_input, tool_output)?;
        let continuation = LifecycleContinuation::StartTurn {
            turn_input: serialized_turn_input.clone(),
            additional_context: additional_context.clone(),
            source_artifact: source_artifact.clone(),
        };
        let mut triggers = Vec::new();
        if first_turn_of_run {
            let source = if cold_thread { "startup" } else { "resume" };
            triggers.push(LifecycleTrigger {
                event: LifecycleEventName::SessionStart,
                trigger: "provider_thread_acquired".to_string(),
                match_values: vec![source.to_string()],
                subject_artifact: source_artifact.clone(),
                data: serde_json::json!({
                    "source": source,
                    "cold_thread": cold_thread,
                    "thread_id": thread_id.to_string(),
                }),
                continuation: Some(continuation.clone()),
            });
        }
        triggers.push(LifecycleTrigger {
            event: LifecycleEventName::UserPromptSubmit,
            trigger: "provider_turn_before_dispatch".to_string(),
            match_values: Vec::new(),
            subject_artifact: source_artifact.clone(),
            data: serde_json::json!({
                "turn_input": serialized_turn_input,
                "additional_context": additional_context,
                "thread_id": thread_id.to_string(),
            }),
            continuation: Some(continuation),
        });
        emit_lifecycle_trigger_set(sink, lifecycle, run, triggers)
    }
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn require_local_process_transport(
        &self,
        dispatch: &str,
    ) -> Result<(), InterfaceDriverError> {
        if matches!(&*self.client.lock().await, AppServerClient::Spawn(_)) {
            return Ok(());
        }
        Err(InterfaceDriverError::FailedClosed(format!(
            "Codex {dispatch} dispatch evidence only supports the exact local process transport"
        )))
    }

    pub(crate) async fn inject_recovered_item(
        &self,
        sink: &TurnSink<'_>,
        evidence: &RecoveredToolResultRolloutEvidence,
        thread_id: &str,
        item: &serde_json::Value,
    ) -> Result<(), InterfaceDriverError> {
        evidence.validate_injection(item)?;
        let inject = ClientRequest::ThreadInjectItems {
            request_id: self.mint_request_id(),
            params: ThreadInjectItemsParams {
                thread_id: thread_id.to_string(),
                items: vec![item.clone()],
            },
        };
        self.require_local_process_transport("result recovery")
            .await?;
        sink.admit_app_server_dispatch(&inject, None).await?;
        {
            let client = self.client.lock().await;
            client
                .request_typed::<ThreadInjectItemsResponse>(inject)
                .await
                .map_err(typed_error)?;
        }

        Ok(())
    }
}
