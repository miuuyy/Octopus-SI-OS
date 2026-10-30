fn exercise_independent_provider_owner(handoff: &ProviderOutputHandoff, initial: &[u8]) -> Vec<u8> {
    let mut owner = handoff.clone();
    owner.config.provider_interface = InterfaceId::new("world_independent_provider");
    owner.config.authoring_source_interface = InterfaceId::new("system_independent_writer_source");
    owner.config.file_write_tool = "file.write.independent".into();
    owner.config.file_write_result_label = "file.write.independent.result".into();
    let door = InterfaceId::new("system_independent_writer");
    let mut templates = serde_json::to_value(&handoff.config.checkpoints).unwrap();
    for cp in templates.as_array_mut().unwrap() {
        let name = format!("independent_{}", cp["name"].as_str().unwrap());
        cp["name"] = serde_json::json!(name);
        for binding in std::iter::once(cp["interface_binding"].clone())
            .chain(
                cp["outbound_interface_bindings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned(),
            )
            .collect::<Vec<_>>()
        {
            let old = binding["interface"].as_str().unwrap();
            let new = if old == handoff.config.provider_interface.as_str() {
                owner.config.provider_interface.as_str()
            } else if old == handoff.config.authoring_source_interface.as_str() {
                owner.config.authoring_source_interface.as_str()
            } else if old
                == handoff
                    .config
                    .checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)
                    .unwrap()
                    .interface()
                    .as_str()
            {
                door.as_str()
            } else {
                continue;
            };
            if cp["interface_binding"]["interface"] == old {
                cp["interface_binding"]["interface"] = serde_json::json!(new);
            }
            for out in cp["outbound_interface_bindings"].as_array_mut().unwrap() {
                if out["interface"] == old {
                    out["interface"] = serde_json::json!(new);
                }
            }
        }
        if cp["role"] == "stable" {
            cp["interface_binding"] = serde_json::json!(all_binding("world_independent_control"));
        }
        if cp["role"] == "authoring_call" {
            cp["required_verified_artifacts"] = serde_json::json!([{"mark":"tool.call.allowed","artifact_label":owner.config.file_write_label,"verifier":"tool_call_allowed","params":{"tools":[owner.config.file_write_tool]}}]);
        }
    }
    let output: RunAuthoringCheckpoint = serde_json::from_value(serde_json::json!({
        "name":"independent_delegation","role":"provider_interface_output","include_context":false,
        "interface_binding":names_binding(owner.config.provider_interface.as_str(), &["peer.delegation"]),
        "outbound_interface_bindings":[names_binding(handoff.config.provider_interface.as_str(), &["peer.delegation"])],
        "required_verified_artifacts":[]
    })).unwrap();
    owner.config.checkpoints = serde_json::from_value(templates).unwrap();
    owner.config.checkpoints.push(output.clone());
    owner.bound_topology = test_bound_topology(&owner.config);
    let writer = CheckedRunWriter {
        owner: owner.config.provider_interface.clone(),
        source: owner.config.authoring_source_interface.clone(),
        door,
        tool: owner.config.file_write_tool.clone(),
        call_label: owner.config.file_write_label.clone(),
        result_label: owner.config.file_write_result_label.clone(),
    };
    let mut input = output.clone();
    input.name = "independent_input".into();
    input.role = RunAuthoringCheckpointRole::OwnerInput;
    input.owner_authoring = Some(writer.clone());
    let mut receiver = handoff.bound_topology.clone();
    receiver.checkpoints.push(input);
    let mut value: serde_json::Value = serde_json::from_slice(initial).unwrap();
    value["interface_state"][handoff.config.provider_interface.as_str()][RUN_AUTHORING_BINDING] =
        serde_json::json!(receiver);
    let mut frontier = Vec::new();
    for cp in &owner.config.checkpoints {
        value["checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(materialize_checkpoint(cp, &cp.name, &owner.config.run, &[]));
        value["active_checkpoints"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(cp.name));
        if cp.role != RunAuthoringCheckpointRole::Stable {
            frontier.push(cp.name.clone());
        }
    }
    value["interface_state"][writer.owner.as_str()] = serde_json::json!({"context_generation":0,"authoring_generation":0,"frontier_checkpoint_ids":frontier,"run_authoring_binding":owner.bound_topology});
    let before = serde_json::to_vec(&value).unwrap();
    let record = RunRecord::from_bytes(&before).unwrap();
    receiver
        .validate_identity(&record, &handoff.config.provider_interface)
        .unwrap();
    let profile = RunAuthoringProfile {
        file_write_tool: handoff.config.file_write_tool.clone(),
        file_write_label: handoff.config.file_write_label.clone(),
        file_write_result_label: handoff.config.file_write_result_label.clone(),
        result_timeout_ms: 1000,
    };
    receiver
        .config(&profile)
        .validate(&handoff.config.provider_interface, &handoff.config.run)
        .unwrap();
    for field in ["source", "door", "tool"] {
        let mut forged = serde_json::to_value(&receiver).unwrap();
        forged["checkpoints"]
            .as_array_mut()
            .unwrap()
            .last_mut()
            .unwrap()["owner_authoring"][field] = serde_json::json!("wrong_exact_owner");
        let forged: BoundRunAuthoringTopology = serde_json::from_value(forged).unwrap();
        assert!(
            forged
                .validate_identity(&record, &handoff.config.provider_interface)
                .is_err(),
            "{field}"
        );
    }
    exercise_peer_verified_input_lifecycle(&owner, &before, &writer);
    let artifact = InterfaceArtifactFile::write(
        owner.var_dir.join("independent-test"),
        "peer.delegation",
        br#"{"text":"bounded delegated work"}"#,
    )
    .unwrap();
    let next = owner
        .author_run(
            &before,
            &[],
            &[PreparedHandoff {
                identity: artifact_identity(&artifact, &owner.var_dir).unwrap(),
                output_checkpoint: output,
                verified_inputs: Vec::new(),
            }],
            false,
            None,
            false,
            false,
        )
        .unwrap();
    let transition = CheckedRunTransition {
        writer: writer.clone(),
        before: before.clone(),
        after: next.content.as_bytes().to_vec(),
        call: ledger_row(100),
        result: ledger_row(101),
        placement: false,
    };
    validate_checked_owner_transition(&transition).unwrap();
    let mut changed = transition;
    let mut foreign: serde_json::Value = serde_json::from_slice(&changed.after).unwrap();
    foreign["interface_state"][handoff.config.provider_interface.as_str()]["context_generation"] =
        serde_json::json!(99);
    changed.after = serde_json::to_vec(&foreign).unwrap();
    assert!(
        validate_checked_owner_transition(&changed).is_err(),
        "another provider frontier/state remains protected"
    );
    before
}

fn exercise_peer_verified_input_lifecycle(
    owner: &ProviderOutputHandoff,
    before: &[u8],
    writer: &CheckedRunWriter,
) {
    let mut owner = owner.clone();
    let output = owner
        .config
        .checkpoint_for_role(RunAuthoringCheckpointRole::ProviderArtifactOutput)
        .unwrap()
        .clone();
    let route = ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { inputs: vec![ProviderVerifiedInput {
        artifact_label:"operator.input.response".into(), also_deliver_to_tool:None,
        required_verified_artifact:serde_json::from_value(serde_json::json!({"mark":"operator.input.provided","artifact_label":"operator.input.response","verifier":"operator_input","params":{"request_id":"peer-request"}})).unwrap(),
    }] };
    let verified = owner
        .config
        .verified_input_checkpoints(
            &route,
            &output,
            &[],
            &BTreeSet::from(["operator.input.provided".into()]),
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .unwrap();
    let file = InterfaceArtifactFile::write(
        owner.var_dir.join("peer-request"),
        "operator.input.request",
        br#"{"request_id":"peer-request"}"#,
    )
    .unwrap();
    let request = owner
        .author_run(
            before,
            &[],
            &[PreparedHandoff {
                identity: artifact_identity(&file, &owner.var_dir).unwrap(),
                output_checkpoint: output.clone(),
                verified_inputs: verified,
            }],
            true,
            None,
            false,
            true,
        )
        .unwrap();
    let change = |before: Vec<u8>, after: Vec<u8>| CheckedRunTransition {
        writer: writer.clone(),
        before,
        after,
        call: ledger_row(100),
        result: ledger_row(101),
        placement: false,
    };
    validate_checked_owner_transition(&change(
        before.to_vec(),
        request.content.as_bytes().to_vec(),
    ))
    .unwrap();
    let record = RunRecord::from_bytes(request.content.as_bytes()).unwrap();
    owner.bound_topology = serde_json::from_value(
        record.interface_state[&writer.owner][RUN_AUTHORING_BINDING].clone(),
    )
    .unwrap();
    owner.config.checkpoints = owner.bound_topology.checkpoints.clone();
    let response = owner
        .author_run(
            request.content.as_bytes(),
            &[],
            &[PreparedHandoff {
                identity: artifact_identity(&file, &owner.var_dir).unwrap(),
                output_checkpoint: output,
                verified_inputs: vec![],
            }],
            true,
            None,
            true,
            true,
        )
        .unwrap();
    validate_checked_owner_transition(&change(
        request.content.as_bytes().to_vec(),
        response.content.as_bytes().to_vec(),
    ))
    .unwrap();
    let mut forged: serde_json::Value = serde_json::from_str(&response.content).unwrap();
    forged["interface_state"][writer.owner.as_str()]["pending_handoff"]["terminal"] =
        serde_json::json!(false);
    forged["status"] = serde_json::json!("running");
    assert!(
        validate_checked_owner_transition(&change(
            request.content.into_bytes(),
            serde_json::to_vec(&forged).unwrap()
        ))
        .is_err()
    );
}
