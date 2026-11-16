async fn hand_through_gate<T>(
    hand: impl std::future::Future<Output = T>,
    gate: impl std::future::Future<Output = Vec<(String, ArtifactRef, u64)>>,
) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        tokio::select! {
            handed = hand => handed,
            _ = gate => panic!("authoring gate ended before the emission was handed"),
        }
    })
    .await
    .expect("the emission is handed before the fixture deadline")
}

fn parallel_tool_call(ctx: &InterfaceLiftContext, tool: &str, correlation: &str) -> ProviderEmission {
    let call = artifact(
        &ctx.interface_var_dir().join(format!("parallel-{correlation}")),
        "tool.call",
        json!({"label":"tool.call","tool":tool,"arguments":{},"correlation_id":correlation}),
    );
    ProviderEmission { artifact: call, route: ProviderEmissionRoute::Tool { tool: tool.to_string() } }
}

fn admitted_artifact(outcome: &octopus_run_authoring::ProviderHandoffOutcome) -> ArtifactRef {
    match outcome {
        octopus_run_authoring::ProviderHandoffOutcome::Admitted { artifact, .. } => artifact.clone(),
        other => panic!("the gate admits the handed output: {other:?}"),
    }
}

fn result_input_templates<'a>(
    record: &'a octopus_run_record::RunRecord,
    provider: &InterfaceId,
) -> Vec<&'a serde_json::Value> {
    record.interface_state[provider]["run_authoring_binding"]["checkpoints"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|template| template["role"] == "result_input")
        .collect()
}

fn result_row_holds_call_as_evidence(
    record: &octopus_run_record::RunRecord,
    provider: &InterfaceId,
    call: &ArtifactRef,
) -> bool {
    let templates = result_input_templates(record, provider);
    let template = templates
        .iter()
        .find(|template| template["required_verified_artifacts"][0]["params"]["call_artifact"]["id"] == json!(call.id))
        .expect("an outstanding call has its result row template");
    template["static_artifact_bindings"].as_array().is_some_and(|ids| ids.contains(&json!(call.id)))
}

fn result_input_row<'a>(
    record: &'a octopus_run_record::RunRecord,
    call: &ArtifactRef,
) -> &'a octopus_run_record::CheckpointRecord {
    record
        .checkpoints
        .iter()
        .find(|cp| {
            record.active_checkpoints.contains(&cp.id)
                && cp.required_verified_artifacts.first().is_some_and(|requirement| {
                    requirement.params["call_artifact"]["id"] == json!(call.id)
                })
        })
        .expect("an outstanding call has one active result row")
}

fn keep_declared_role_artifacts(row: &mut Movement, record: &octopus_run_record::RunRecord, provider: &InterfaceId) {
    let declared = record.interface_state[provider]["run_authoring_binding"]["checkpoints"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|template| template["role"] == "stable")
        .flat_map(|template| template["static_artifact_bindings"].as_array().into_iter().flatten())
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let mut metadata = row.invocation_metadata.as_ref().unwrap().invocation().unwrap().clone();
    metadata.grant.role_artifacts.retain(|id| declared.contains(id));
    row.invocation_metadata = Some(metadata.into());
}

fn pending_snapshot_sources(
    ctx: &InterfaceLiftContext,
    record: &octopus_run_record::RunRecord,
    provider: &InterfaceId,
) -> Vec<String> {
    let item = &record.interface_state[provider]["pending_handoff"]["items"][0];
    let snapshot = InterfaceArtifactFile::new(
        ctx.interface_var_dir().join(item["manifest_relative_path"].as_str().unwrap()),
    )
    .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&snapshot.read_payload_bytes().unwrap()).unwrap();
    payload["session_context"]["artifacts"]
        .as_array()
        .expect("a session snapshot output carries its session context")
        .iter()
        .map(|entry| entry["source_artifact"]["id"].as_str().unwrap().to_string())
        .collect()
}
