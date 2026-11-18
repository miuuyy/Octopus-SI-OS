use super::*;

// additionalContext is a small client annotation channel: Codex truncates each
// value to 1,000 tokens. Bound history and formed policy are complete messages.
pub(crate) fn native_context_items(
    source: u64,
    context: BTreeMap<String, AdditionalContextEntry>,
) -> Vec<serde_json::Value> {
    context.into_iter().map(|(name, entry)| {
        let (role, tag) = match entry.kind {
            AdditionalContextKind::Untrusted => ("user", format!("external_{name}")),
            AdditionalContextKind::Application => ("developer", name.clone()),
        };
        serde_json::json!({
            "type":"message", "id":format!("octopus-context-{source}-{name}"),
            "role":role, "content":[{"type":"input_text",
                "text":format!("<{tag}>\n{}\n</{tag}>", entry.value)}],
        })
    }).collect()
}

fn recorded_native_context(path: &Path, thread: &str, items: &[serde_json::Value]) -> Result<bool, InterfaceDriverError> {
    let refuse = |detail: String| InterfaceDriverError::FailedClosed(format!("native context recovery: {detail}"));
    let file = std::fs::File::open(path).map_err(|error| refuse(error.to_string()))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut owner = false;
    let mut found = BTreeSet::new();
    loop {
        line.clear();
        let count = (&mut reader).take((RECOVERY_ROLLOUT_MAX_LINE_BYTES + 1) as u64)
            .read_line(&mut line).map_err(|error| refuse(error.to_string()))?;
        if count == 0 { break; }
        if count > RECOVERY_ROLLOUT_MAX_LINE_BYTES || !line.ends_with('\n') {
            return Err(refuse("oversized or incomplete rollout record".into()));
        }
        let row: serde_json::Value = serde_json::from_str(&line).map_err(|error| refuse(error.to_string()))?;
        if row["type"] == "session_meta" {
            if owner || row["payload"]["id"] != thread { return Err(refuse("rollout belongs to a different or repeated thread".into())); }
            owner = true;
        }
        if !found.is_empty() && (row["type"] == "compacted" || row["payload"]["type"] == "thread_rolled_back") {
            return Err(refuse("native history changed after context injection".into()));
        }
        if row["type"] != "response_item" { continue; }
        let actual = &row["payload"];
        if let Some(expected) = items.iter().find(|item| item["id"] == actual["id"]) {
            let id = expected["id"].as_str().expect("context message id");
            if !found.insert(id.to_string()) || actual["type"] != expected["type"]
                || actual["role"] != expected["role"] || actual["content"] != expected["content"] {
                return Err(refuse(format!("context message '{id}' is duplicated or changed")));
            }
        }
    }
    if !owner || (!found.is_empty() && found.len() != items.len()) {
        return Err(refuse("rollout has no exact thread identity or only part of its context".into()));
    }
    Ok(found.len() == items.len())
}

impl CodexAppServerInterfaceDriver {
    pub(crate) async fn inject_native_context(
        &self,
        sink: &TurnSink<'_>,
        scope: &TransportStateScope,
        thread: &str,
        context: BTreeMap<String, AdditionalContextEntry>,
        recovering: bool,
    ) -> Result<(), InterfaceDriverError> {
        let fingerprint = octopus_ledger::ContentHash::of_bytes(&serde_json::to_vec(&(thread, &context))
            .map_err(|error| InterfaceDriverError::Protocol(error.to_string()))?).to_hex();
        if self.transport_state.lock().await.get(scope)
            .is_some_and(|state| state.native_context_fingerprint.as_ref() == Some(&fingerprint)) {
            return Ok(());
        }
        let items = native_context_items(sink.source_movement_seq, context);
        if items.is_empty() { return Ok(()); }
        let recorded = if recovering {
            // A fresh thread may not have had a rollout when it was acquired.
            // Injection materializes it; obtain its current native coordinate.
            let response = self.client.lock().await.request_typed::<ThreadReadResponse>(ClientRequest::ThreadRead {
                request_id: self.mint_request_id(),
                params: ThreadReadParams { thread_id: thread.into(), include_turns: false },
            }).await.map_err(typed_error)?;
            let path = (response.thread.id == thread).then_some(response.thread.path).flatten()
                .ok_or_else(|| InterfaceDriverError::FailedClosed("native context recovery has no exact rollout path".into()))?;
            recorded_native_context(&path, thread, &items)?
        } else { false };
        if !recorded {
            let request = ClientRequest::ThreadInjectItems {
                request_id: self.mint_request_id(),
                params: ThreadInjectItemsParams { thread_id: thread.into(), items },
            };
            sink.admit_app_server_dispatch(&request, None).await?;
            self.client.lock().await.request_typed::<ThreadInjectItemsResponse>(request).await.map_err(typed_error)?;
        }
        if let Some(state) = self.transport_state.lock().await.get_mut(scope) {
            state.native_context_fingerprint = Some(fingerprint);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_context_preserves_roles_and_recovers_only_exact_complete_messages() {
        let history = "earlier decision\n".repeat(30_000);
        let policy = "declared permission\n".repeat(1_000);
        let items = native_context_items(42, native_turn_context(Some(history.clone()), Some(policy.clone()), None));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["role"], "user");
        assert_eq!(items[0]["content"][0]["text"], format!("<external_octopus_bound_context>\n{history}\n</external_octopus_bound_context>"));
        assert_eq!(items[1]["role"], "developer");
        assert_eq!(items[1]["content"][0]["text"], format!("<octopus_formed_policy>\n{policy}\n</octopus_formed_policy>"));
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rollout.jsonl");
        let write = |messages: &[serde_json::Value]| {
            let mut rows = vec![serde_json::json!({"type":"session_meta","payload":{"id":"thread"}}).to_string()];
            rows.extend(messages.iter().map(|item| serde_json::json!({"type":"response_item","payload":item}).to_string()));
            std::fs::write(&path, rows.join("\n") + "\n").unwrap();
        };
        write(&[]);
        assert!(!recorded_native_context(&path, "thread", &items).unwrap());
        write(&items);
        assert!(recorded_native_context(&path, "thread", &items).unwrap());
        assert!(recorded_native_context(&path, "other-thread", &items).is_err());
        write(&items[..1]);
        assert!(recorded_native_context(&path, "thread", &items).is_err());
        let mut changed = items.clone();
        changed[0]["content"][0]["text"] = serde_json::json!("truncated");
        write(&changed);
        assert!(recorded_native_context(&path, "thread", &items).is_err());
        let mut duplicated = items.clone();
        duplicated.push(items[0].clone());
        write(&duplicated);
        assert!(recorded_native_context(&path, "thread", &items).is_err());
    }
}
