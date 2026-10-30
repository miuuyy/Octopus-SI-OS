use super::*;

pub(crate) fn build_chat_run_record(
    config: &ClientConfig,
    identity: &PlannedIdentity,
    cycle: &str,
    topology: &FormedCapabilityTopology,
    role_artifacts: &[String],
    selection: &RunSelection,
    existing: Option<&serde_json::Value>,
    conversation: &ConversationSeed,
    external_inputs: &[ExternalInputSeed],
    external_relays: &[ExternalRelaySeed],
    model_outputs: &[ModelOutputSeed],
    context_artifact_bindings: &[ArtifactBindingSeed],
    payload_artifact_bindings: &[octopus_ledger::ArtifactRef],
    provider_client_operation_labels: &[String],
    previous_authoring_profile: Option<&OctopusOsAuthoringProfile>,
    operations: &OperationRoutes,
) -> Result<String, String> {
    if let Some(existing) = existing {
        assert_existing_run_contract(existing, identity, selection)?;
    }
    let retained_connections = retained_connections::selected(
        existing, identity, previous_authoring_profile, external_inputs, external_relays, model_outputs,
    )?;
    let mut mandatory_role_artifacts = Vec::new();
    let mut seen = BTreeSet::new();
    for id in role_artifacts.iter().cloned() {
        if id.trim().is_empty() {
            return Err("role artifact binding id is empty".to_string());
        }
        if seen.insert(id.clone()) {
            mandatory_role_artifacts.push(serde_json::json!({ "id": id }));
        }
    }
    let mut context_artifacts = mandatory_role_artifacts.clone();
    let retained_history = existing_bound_run_authoring_topology(existing, identity)?
        .map(|binding| {
            let bytes = serde_json::to_vec(existing.ok_or("retained history has no run")?)
                .map_err(|error| error.to_string())?;
            let record = RunRecord::from_bytes(&bytes).map_err(|error| error.to_string())?;
            let static_ids = binding
                .checkpoints
                .iter()
                .flat_map(|template| &template.static_artifact_bindings)
                .chain(binding.payload_artifact_bindings.iter().map(|reference| &reference.id))
                .collect::<BTreeSet<_>>();
            Ok::<_, String>(
                octopus_run_authoring::provider_context_bindings(
                    &record,
                    &InterfaceId::new(&identity.model_interface_id),
                )?
                .into_iter()
                .filter(|artifact| !static_ids.contains(&artifact.id))
                .collect::<Vec<_>>(),
            )
        })
        .transpose()?;
    for id in config
        .run
        .context_artifact_bindings
        .iter()
        .map(|binding| binding.id.clone())
        .chain(
            conversation
                .artifact_bindings
                .iter()
                .filter(|_| retained_history.is_none())
                .map(|binding| binding.id.clone()),
        )
        .chain(
            conversation
                .pinned_artifact_bindings
                .iter()
                .map(|binding| binding.id.clone()),
        )
        .chain(
            context_artifact_bindings
                .iter()
                .map(|binding| binding.id.clone()),
        )
        .chain(retained_connections.iter().flat_map(|connection|
            connection.template.static_artifact_bindings.iter().map(ToString::to_string)))
    {
        if id.trim().is_empty() {
            return Err("context artifact binding id is empty".to_string());
        }
        if identity.code_mode_owners.values().any(|owner| owner.prelude_sources.iter().any(|source| source.source.id.as_str() == id)) {
            continue;
        }
        if seen.insert(id.clone()) {
            context_artifacts.push(serde_json::json!({ "id": id }));
        }
    }

    octopus_run_authoring::validate_payload_artifact_selection(payload_artifact_bindings)?;
    let reserved = identity.code_mode_owners.values().flat_map(|owner| {
        std::iter::once(&owner.prelude.id).chain(std::iter::once(&owner.topology.id))
            .chain(owner.prelude_sources.iter().flat_map(|source| [&source.source.id, &source.snapshot.id]))
    }).collect::<BTreeSet<_>>();
    for reference in payload_artifact_bindings {
        if seen.contains(reference.id.as_str()) || reserved.contains(&reference.id) {
            return Err(format!("payload selection '{}' collides with a context, role or prelude binding", reference.id));
        }
        seen.insert(reference.id.as_str().to_string());
        context_artifacts.push(serde_json::json!({"id":reference.id}));
    }
    for binding in retained_history.into_iter().flatten() {
        if seen.insert(binding.id.to_string()) {
            context_artifacts.push(serde_json::to_value(binding).map_err(|error| error.to_string())?);
        }
    }

    let checkpoint = |id: String,
                      inbound: Option<&str>,
                      outbound: Vec<&str>,
                      artifacts: &[serde_json::Value],
                      required: Vec<serde_json::Value>|
     -> serde_json::Value {
        let mut object = serde_json::Map::new();
        object.insert("id".to_string(), serde_json::json!(&id));
        object.insert("run".to_string(), serde_json::json!(identity.run_id));
        object.insert(
            "trace".to_string(),
            serde_json::json!(format!("trace:{}:{id}", identity.run_id)),
        );
        object.insert("epoch".to_string(), serde_json::json!(0));
        if let Some(inbound) = inbound {
            object.insert(
                "interface_binding".to_string(),
                serde_json::json!(AuthoredInterfaceBinding::all(InterfaceId::new(inbound))),
            );
        }
        object.insert(
            "outbound_interface_bindings".to_string(),
            serde_json::json!(
                outbound
                    .into_iter()
                    .map(|interface| AuthoredInterfaceBinding::all(InterfaceId::new(interface)))
                    .collect::<Vec<_>>()
            ),
        );
        object.insert(
            "artifact_bindings".to_string(),
            serde_json::Value::Array(artifacts.to_vec()),
        );
        object.insert(
            "required_verified_artifacts".to_string(),
            serde_json::Value::Array(required),
        );
        serde_json::Value::Object(object)
    };

    let input_id = format!("cp_operator_input_{cycle}");
    let turn_control_id = format!("cp_operator_turn_control_{cycle}");
    let provider_operation_id = format!("cp_operator_provider_operation_{cycle}");
    let worker_input_id = format!("cp_worker_input_{cycle}");
    let client_id = if identity.native_communication.is_some() {
        format!("cp_provider_output_{cycle}")
    } else {
        format!("cp_provider_client_{cycle}")
    };
    let authoring_call_id = format!("cp_provider_authoring_{cycle}");
    let authoring_result_id = format!("cp_authoring_provider_{cycle}");
    let mut cycle_checkpoints = Vec::new();
    if identity.owns_client() {
        let chat_input_binding = operator_chat_input_binding_names(
            &identity.chat_interface_id,
            &home_outputs::operator_input_labels(identity, &config.artifact_labels.message),
        )?;
        let turn_control_binding = operator_chat_input_binding(
            &identity.chat_interface_id,
            &config.artifact_labels.turn_control,
        )?;
        let mut operator_input = checkpoint(
            input_id.clone(),
            Some(&identity.chat_interface_id),
            vec![&identity.model_interface_id],
            &context_artifacts,
            Vec::new(),
        );
        operator_input["interface_binding"] = serde_json::to_value(chat_input_binding)
            .map_err(|error| format!("operator chat input binding did not encode: {error}"))?;
        let mut turn_control_input = checkpoint(
            turn_control_id,
            Some(&identity.chat_interface_id),
            vec![&identity.model_interface_id],
            &mandatory_role_artifacts,
            Vec::new(),
        );
        turn_control_input["interface_binding"] = serde_json::to_value(turn_control_binding)
            .map_err(|error| format!("operator turn-control binding did not encode: {error}"))?;
        cycle_checkpoints.extend([operator_input, turn_control_input]);
    }
    cycle_checkpoints.push(checkpoint(
            worker_input_id.clone(),
            Some(&identity.worker_interface_id),
            vec![&identity.model_interface_id],
            &context_artifacts,
            Vec::new(),
        ));
    if let Some(client) = identity.native_communication.as_ref().filter(|_| !identity.owns_client()).and_then(|routes| routes.client_interface_id.as_deref()) {
        cycle_checkpoints.push(checkpoint(format!("cp_client_input_{cycle}"), Some(client), vec![&identity.model_interface_id], &context_artifacts, Vec::new()));
    }
    if identity.owns_client() && !provider_client_operation_labels.is_empty() {
        let provider_operation_binding = operator_chat_input_binding_names(
            &identity.chat_interface_id,
            provider_client_operation_labels,
        )?;
        let mut provider_operation_input = checkpoint(
            provider_operation_id,
            Some(&identity.chat_interface_id),
            vec![&identity.model_interface_id],
            &mandatory_role_artifacts,
            Vec::new(),
        );
        provider_operation_input["interface_binding"] =
            serde_json::to_value(provider_operation_binding).map_err(|error| {
                format!("operator provider-operation binding did not encode: {error}")
            })?;
        cycle_checkpoints.push(provider_operation_input);
    }
    for (source_interface_id, binding) in &topology.tool_bindings {
        let tools = capability_binding_tools(config, identity, source_interface_id, binding)?;
        if tools.is_empty() {
            continue;
        }
        let door_id = identity
            .capability_doors
            .get(source_interface_id.as_str())
            .ok_or_else(|| {
                format!(
                    "capability interface '{}' has no planned exact door",
                    source_interface_id
                )
            })?;
        if identity.code_mode_owners.values().any(|owner| owner.nested_doors.contains(door_id)) { continue; }
        let segment = capability_interface_segment(source_interface_id.as_str());
        let capability_input_id = format!("cp_capability_{segment}_input_{cycle}");
        let owner = identity.code_mode_owners.get(source_interface_id.as_str());
        let mut artifacts = mandatory_role_artifacts.clone();
        if let Some(owner) = owner {
            artifacts.extend([serde_json::json!({"id":owner.prelude.id}),serde_json::json!({"id":owner.topology.id})]);
        } else if config.run.tool_result_receiving.is_none() {
            cycle_checkpoints.push(checkpoint(capability_input_id,Some(door_id),vec![&identity.model_interface_id],&context_artifacts,Vec::new()));
        }
        artifacts.extend(payload_artifact_bindings.iter().map(|reference| serde_json::json!({"id":reference.id})));
        cycle_checkpoints.push(checkpoint(
            format!("cp_capability_{segment}_output_{cycle}"),
            Some(&identity.model_interface_id),
            vec![door_id],
            &artifacts,
            vec![tool_call_requirement(config, tools)],
        ));
    }
    let model_output_ids = model_outputs
        .iter()
        .map(|output| {
            if cycle_checkpoints.iter().any(|checkpoint| {
                checkpoint.get("id").and_then(serde_json::Value::as_str)
                    == Some(output.checkpoint_id.as_str())
            }) {
                return Err(format!(
                    "model output checkpoint '{}' collides with the client cycle",
                    output.checkpoint_id
                ));
            }
            let mut output_checkpoint = checkpoint(
                output.checkpoint_id.clone(),
                Some(&identity.model_interface_id),
                vec![&output.interface_id],
                &mandatory_role_artifacts,
                vec![
                    serde_json::to_value(&output.required_verified_artifact).map_err(|error| {
                        format!("model output requirement did not encode: {error}")
                    })?,
                ],
            );
            output_checkpoint["interface_binding"]["config"]["artifact_policy"] = serde_json::json!({
                "mode":"allow","names":[output.required_verified_artifact.artifact_label],"namespaces":[]
            });
            cycle_checkpoints.push(output_checkpoint);
            Ok(output.checkpoint_id.clone())
        })
        .collect::<Result<Vec<_>, String>>()?;
    let return_interfaces = match &identity.native_communication {
        Some(routes) => routes.return_interface_ids.iter().map(String::as_str)
            .chain(routes.client_interface_id.as_deref()).collect(),
        None => vec![identity.chat_interface_id.as_str()],
    };
    let mut provider_output = checkpoint(
        client_id.clone(),
        Some(&identity.model_interface_id),
        return_interfaces,
        &mandatory_role_artifacts,
        Vec::new(),
    );
    let recorded_output_id = if let Some(selection) = identity.native_communication.as_ref()
        .and_then(|routes| routes.output_selection.as_ref())
    {
        provider_output["interface_binding"]["config"]["artifact_policy"] = serde_json::json!({"mode":"allow","names":selection.return_labels,"namespaces":[]});
        let recorded_id = format!("cp_provider_recorded_{cycle}");
        let client = identity.native_communication.as_ref().and_then(|routes| routes.client_interface_id.as_deref());
        let mut recorded = checkpoint(recorded_id.clone(), Some(&identity.model_interface_id), client.into_iter().collect(), &mandatory_role_artifacts, Vec::new());
        recorded["interface_binding"]["config"]["artifact_policy"] = serde_json::json!({"mode":"allow","names":selection.record_labels,"namespaces":[]});
        cycle_checkpoints.push(recorded);
        Some(recorded_id)
    } else { None };
    cycle_checkpoints.push(provider_output);
    cycle_checkpoints.push(checkpoint(
        authoring_call_id.clone(),
        Some(&identity.authoring_source_id),
        vec![&identity.authoring_environment_id],
        &mandatory_role_artifacts,
        vec![authoring_call_requirement(config, &identity.authoring_tool_id)],
    ));
    cycle_checkpoints.push(checkpoint(
        authoring_result_id.clone(),
        Some(&identity.authoring_environment_id),
        vec![&identity.model_interface_id],
        &mandatory_role_artifacts,
        Vec::new(),
    ));
    let cycle_ids = cycle_checkpoints
        .iter()
        .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
        .collect::<BTreeSet<_>>();
    let external_input_checkpoints = external_inputs
        .iter()
        .filter_map(|input| operations.standing_input(input).transpose().map(|binding| (input, binding)))
        .map(|(input, binding)| {
            if cycle_ids.contains(input.checkpoint_id.as_str()) {
                return Err(format!(
                    "external input checkpoint '{}' collides with the client cycle",
                    input.checkpoint_id
                ));
            }
            let required = input
                .required_verified_artifacts
                .iter()
                .map(|required| {
                    serde_json::to_value(required).map_err(|error| {
                        format!(
                            "external input checkpoint '{}' requirement did not encode: {error}",
                            input.checkpoint_id
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut input_checkpoint = checkpoint(
                input.checkpoint_id.clone(),
                Some(&input.interface_id),
                vec![&identity.model_interface_id],
                &context_artifacts,
                required,
            );
            input_checkpoint["interface_binding"] = serde_json::to_value(binding?)
                .map_err(|error| format!("external input checkpoint '{}' binding did not encode: {error}", input.checkpoint_id))?;
            Ok(input_checkpoint)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let external_relay_checkpoints = external_relays
        .iter()
        .map(|relay| {
            if cycle_ids.contains(relay.checkpoint_id.as_str()) {
                return Err(format!(
                    "external relay checkpoint '{}' collides with the client cycle",
                    relay.checkpoint_id
                ));
            }
            let required = relay
                .required_verified_artifacts
                .iter()
                .map(|required| {
                    serde_json::to_value(required).map_err(|error| {
                        format!(
                            "external relay checkpoint '{}' requirement did not encode: {error}",
                            relay.checkpoint_id
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(checkpoint(
                relay.checkpoint_id.clone(),
                Some(&relay.interface_id),
                relay
                    .outbound_interface_ids
                    .iter()
                    .map(String::as_str)
                    .collect(),
                &mandatory_role_artifacts,
                required,
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let external_checkpoints = external_input_checkpoints
        .into_iter()
        .chain(external_relay_checkpoints)
        .collect::<Vec<_>>();

    let previous_checkpoints = match existing.and_then(|record| record.get("checkpoints")) {
        None => Vec::new(),
        Some(serde_json::Value::Array(values)) => values.clone(),
        Some(_) => return Err("existing run.checkpoints must be an array".to_string()),
    };
    let previous_active = match existing.and_then(|record| record.get("active_checkpoints")) {
        None => BTreeSet::new(),
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| "existing active checkpoint id is malformed".to_string())
            })
            .collect::<Result<BTreeSet<_>, _>>()?,
        Some(_) => return Err("existing run.active_checkpoints must be an array".to_string()),
    };
    let mut previous_checkpoint_ids = BTreeSet::new();
    for checkpoint in &previous_checkpoints {
        let id = checkpoint
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "existing checkpoint.id is malformed".to_string())?;
        if !previous_checkpoint_ids.insert(id.to_string()) {
            return Err(format!("existing checkpoints repeat id '{id}'"));
        }
    }
    if !previous_active.is_subset(&previous_checkpoint_ids) {
        return Err("existing active checkpoints contain ids absent from checkpoints".to_string());
    }
    let previous_client_cycle = match existing {
        None => BTreeSet::new(),
        Some(record) => record
            .get("interface_state")
            .and_then(|state| state.get(&identity.chat_interface_id))
            .and_then(|state| state.get("cycle_checkpoint_ids"))
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                format!(
                    "existing chat state '{}' does not record cycle_checkpoint_ids",
                    identity.chat_interface_id
                )
            })?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| "existing client cycle checkpoint id is malformed".to_string())
            })
            .collect::<Result<BTreeSet<_>, _>>()?,
    };
    let existing_provider_state = existing
        .and_then(|record| record.get("interface_state"))
        .and_then(|state| state.get(&identity.model_interface_id))
        .and_then(serde_json::Value::as_object)
        .cloned();
    let mut provider_frontier = BTreeSet::new();
    if let Some(state) = existing_provider_state.as_ref() {
        let frontier = state
            .get("frontier_checkpoint_ids")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                "existing provider state.frontier_checkpoint_ids must be an array".to_string()
            })?;
        for value in frontier {
            let id = value
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| "existing provider frontier id is malformed".to_string())?;
            if !provider_frontier.insert(id.to_string()) {
                return Err(format!("existing provider frontier repeats id '{id}'"));
            }
        }
    }
    if !provider_frontier.is_subset(&previous_active) {
        return Err("existing provider frontier is not fully active".to_string());
    }
    let mut previous_provider_context = BTreeSet::new();
    let previous_binding = existing_bound_run_authoring_topology(existing, identity)?;
    let mut admitted_inputs = Vec::new();
    let mut admitted_input_ids = BTreeSet::new();
    if let Some(binding) = &previous_binding {
        if config.run.tool_result_receiving.is_some() && binding.checkpoints.iter().any(|output| {
            output.role == BoundCheckpointRole::ProviderToolOutput && output.result_receiving.is_empty()
                && output.outbound_interface_bindings.iter().any(|door| binding.checkpoints.iter().any(|input|
                    input.role == BoundCheckpointRole::Stable && input.interface_binding.interface == door.interface))
        }) {
            return Err("existing tool returns require checked native result-receiving adoption before task recomposition".into());
        }
        for template in binding.checkpoints.iter().filter(|template| matches!(template.role, BoundCheckpointRole::VerifiedInput | BoundCheckpointRole::ResultInput)) {
            let rows = previous_checkpoints.iter().filter(|cp| cp.get("id").and_then(serde_json::Value::as_str).is_some_and(|id| previous_active.contains(id)))
                .filter_map(|cp| match checkpoint_matches_bound_template(cp, template, &identity.run_id) { Ok(true) => Some(Ok(cp)), Ok(false) => None, Err(error) => Some(Err(error)) })
                .collect::<Result<Vec<_>, String>>()?;
            let [row] = rows.as_slice() else { return Err("retained admitted input is absent or ambiguous".into()); };
            let id = row["id"].as_str().ok_or("retained input id is malformed")?.to_string();
            if !admitted_input_ids.insert(id) { return Err("retained admitted input is claimed twice".into()); }
            admitted_inputs.push(template.clone());
        }
    }
    if let Some(binding) = previous_binding.as_ref() {
        previous_provider_context = bound_context_checkpoint_ids(
            &previous_checkpoints,
            &previous_active,
            binding,
            identity,
        )?;
    } else if existing.is_some() && !provider_frontier.is_empty() {
        return Err("existing provider frontier has no durable run authoring binding".to_string());
    }
    let previous_external_routes = resolve_previous_external_route_checkpoints(
        previous_authoring_profile,
        &previous_checkpoints,
        &previous_active,
        &identity.run_id,
        &identity.model_interface_id,
        operations,
    )?;
    let replaced_ids = previous_client_cycle
        .union(&provider_frontier)
        .cloned()
        .collect::<BTreeSet<_>>()
        .union(&previous_provider_context)
        .cloned()
        .collect::<BTreeSet<_>>()
        .union(&previous_external_routes)
        .cloned()
        .collect::<BTreeSet<_>>()
        .difference(&admitted_input_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut retained = Vec::new();
    let mut observed_active = BTreeSet::new();
    for mut candidate in previous_checkpoints {
        let id = candidate
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| "existing checkpoint.id is malformed".to_string())?
            .to_string();
        if replaced_ids.contains(&id) {
            continue;
        }
        if previous_active.contains(&id) {
            if !admitted_input_ids.contains(&id) { merge_mandatory_role_bindings(&mut candidate, &mandatory_role_artifacts)?; }
            observed_active.insert(id);
            retained.push(candidate);
        }
    }
    let expected_external_active = previous_active
        .difference(&replaced_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    if observed_active != expected_external_active {
        return Err("existing active checkpoints do not resolve to retained records".to_string());
    }

    let mut next_frontier = vec![serde_json::json!(client_id)];
    next_frontier.extend(recorded_output_id.map(serde_json::Value::String));
    for (source_interface_id, binding) in &topology.tool_bindings {
        if capability_binding_tools(config, identity, source_interface_id, binding)?.is_empty() {
            continue;
        }
        let door = identity.capability_doors.get(source_interface_id.as_str()).ok_or("capability frontier has no exact door")?;
        if identity.code_mode_owners.values().any(|owner| owner.nested_doors.contains(door)) { continue; }
        let segment = capability_interface_segment(source_interface_id.as_str());
        next_frontier.push(serde_json::json!(format!(
            "cp_capability_{segment}_output_{cycle}"
        )));
    }
    next_frontier.extend(model_output_ids.into_iter().map(serde_json::Value::String));
    next_frontier.push(serde_json::json!(authoring_call_id));
    next_frontier.push(serde_json::json!(authoring_result_id));

    let mut next_binding = derive_bound_run_authoring_topology(
        identity,
        &cycle_checkpoints,
        &external_checkpoints,
        &next_frontier,
        &config.artifact_labels.message,
        &config.run.tool_call_verifier,
    )?;
    for template in admitted_inputs {
        if next_binding.checkpoints.iter().any(|next| next.name == template.name) { return Err("retained admitted input collides with a new template".into()); }
        next_binding.checkpoints.push(template);
    }

    for connection in retained_connections {
        let template = connection.template;
        if next_binding.checkpoints.iter().any(|planned| planned.name == template.name) {
            return Err("retained connection collides with a client-authored template".into());
        }
        let id = format!("cp_retained_{}_{}", safe_segment(&template.name, "retained template name")?, cycle);
        if previous_checkpoint_ids.contains(&id) || cycle_checkpoints.iter().chain(&external_checkpoints)
            .any(|cp| cp.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())) {
            return Err("retained connection collides with an existing checkpoint".into());
        }
        let mut artifacts = mandatory_role_artifacts.clone();
        for binding in connection.artifacts {
            if !artifacts.iter().any(|value| value.get("id").and_then(serde_json::Value::as_str) == Some(binding.id.as_str())) {
                artifacts.push(serde_json::to_value(binding).map_err(|e| e.to_string())?);
            }
        }
        let mut row = checkpoint(id.clone(), None, Vec::new(), &artifacts, Vec::new());
        row["interface_binding"] = serde_json::to_value(&template.interface_binding).map_err(|e| e.to_string())?;
        row["outbound_interface_bindings"] = serde_json::to_value(&template.outbound_interface_bindings).map_err(|e| e.to_string())?;
        cycle_checkpoints.push(row);
        next_frontier.push(serde_json::json!(id));
        next_binding.checkpoints.push(template);
    }

    next_binding.payload_artifact_bindings = payload_artifact_bindings.to_vec();
    if let Some(result) = &config.run.tool_result_receiving {
        for output in next_binding.checkpoints.iter_mut().filter(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput) {
            let [door] = output.outbound_interface_bindings.as_slice() else { return Err("tool output requires one exact result door".into()); };
            if identity.code_mode_owners.keys().any(|source| identity.capability_doors.get(source).is_some_and(|id| id == door.interface.as_str())) { continue; }
            output.result_receiving = tool_result_requirements(output, result)?;
        }
    }
    for output in next_binding.checkpoints.iter_mut().filter(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput) {
        output.static_artifact_bindings.extend(payload_artifact_bindings.iter().map(|reference| reference.id.clone()));
    }
    for (interface, route) in &operations.connected {
        let result = config.run.tool_result_receiving.as_ref()
            .ok_or("application operations require the client's tool_result_receiving contract")?;
        let request = |id: &str| AuthoredInterfaceBinding {
            interface: InterfaceId::new(id),
            config: InterfaceBindingConfig { artifact_policy: BindingArtifactPolicy::Allow {
                names: vec![route.request_label.clone()], namespaces: Vec::new(),
            } },
        };
        let mut template = bound_checkpoint_template(BoundCheckpointRole::ProviderInterfaceOutput,
            request(&identity.model_interface_id), vec![request(interface)], Vec::new())?;
        template.result_receiving = route.result_labels.iter()
            .map(|label| Ok((label.clone(), result.contract(label)?)))
            .collect::<Result<_, String>>()?;
        let id = format!("cp_operation_{}_output_{cycle}", capability_interface_segment(interface));
        if previous_checkpoint_ids.contains(&id) || cycle_checkpoints.iter().chain(&external_checkpoints)
            .any(|cp| cp.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())) {
            return Err("application operation route collides with an existing checkpoint".into());
        }
        let mut row = checkpoint(id.clone(), None, Vec::new(), &mandatory_role_artifacts, Vec::new());
        row["interface_binding"] = serde_json::to_value(&template.interface_binding).map_err(|e| e.to_string())?;
        row["outbound_interface_bindings"] = serde_json::to_value(&template.outbound_interface_bindings).map_err(|e| e.to_string())?;
        cycle_checkpoints.push(row);
        next_frontier.push(serde_json::json!(id));
        next_binding.checkpoints.push(template);
    }
    for (source, owner) in &identity.code_mode_owners {
        let door = identity.capability_doors.get(source).ok_or("persistent owner has no exact door")?;
        let mut ids = vec![owner.prelude.id.clone(),owner.topology.id.clone()];
        ids.extend(payload_artifact_bindings.iter().map(|reference| reference.id.clone()));
        let output = next_binding.checkpoints.iter_mut().find(|cp| cp.role == BoundCheckpointRole::ProviderToolOutput && cp.outbound_interface_bindings.len()==1 && cp.outbound_interface_bindings[0].interface.as_str()==door).ok_or("persistent owner has no exact provider call template")?;
        output.static_artifact_bindings = ids.clone();
        let mut returned = bound_checkpoint_template(BoundCheckpointRole::OwnerInput,AuthoredInterfaceBinding::all(InterfaceId::new(door)),vec![AuthoredInterfaceBinding::all(InterfaceId::new(&identity.model_interface_id))],Vec::new())?;
        returned.include_context = true;
        returned.owner_authoring = Some(BoundAuthoringWriter {
            owner: InterfaceId::new(door),source:InterfaceId::new(door),door:InterfaceId::new(&owner.authoring_environment),tool:owner.authoring_tool.clone(),call_label:config.artifact_labels.tool_call.clone(),result_label:format!("{}.result",owner.authoring_tool),
        });
        next_binding.checkpoints.push(returned);
    }

    let mut interface_state = existing
        .and_then(|record| record.get("interface_state"))
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let cycle_checkpoint_ids = cycle_checkpoints
        .iter()
        .map(|entry| {
            entry
                .get("id")
                .and_then(serde_json::Value::as_str)
                .expect("planned checkpoint carries an id")
                .to_string()
        })
        .collect::<Vec<_>>();
    if identity.owns_client() {
        interface_state.insert(
            identity.chat_interface_id.clone(),
            serde_json::json!({
                "schema_version": 1,
                "client_surface_role": identity.surface_role,
                "provider_choice_interface": identity.provider_choice_interface,
                "provider_runtime_interface": identity.model_interface_id,
                "worker_choice_interface": identity.worker_choice_interface,
                "ledger_offset": conversation.ledger_offset,
                "conversation_artifact_bindings": conversation.artifact_bindings,
                "pinned_artifact_bindings": conversation.pinned_artifact_bindings,
                "pinned_context_items": conversation.pinned_context_items,
                "continued_from": conversation.continued_from,
                "cycle_checkpoint_ids": cycle_checkpoint_ids,
                "voice_context": identity.voice.as_ref().map(|voice| &voice.context),
            }),
        );
    }
    let mut provider_state = existing_provider_state.unwrap_or_else(|| {
        serde_json::json!({
            "context_generation": 0,
            "authoring_generation": 0,
        })
        .as_object()
        .cloned()
        .expect("provider state literal is an object")
    });
    provider_state.insert(
        "frontier_checkpoint_ids".to_string(),
        serde_json::Value::Array(next_frontier),
    );
    provider_state.insert(
        "run_authoring_binding".to_string(),
        serde_json::to_value(next_binding)
            .map_err(|error| format!("run authoring binding did not encode: {error}"))?,
    );
    interface_state.insert(
        identity.model_interface_id.clone(),
        serde_json::Value::Object(provider_state),
    );

    let mut active_checkpoints = expected_external_active.into_iter().collect::<Vec<_>>();
    let mut checkpoints = retained;
    active_checkpoints.extend(
        external_checkpoints
            .iter()
            .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_str))
            .map(str::to_string),
    );
    checkpoints.extend(external_checkpoints);
    active_checkpoints.extend(cycle_checkpoint_ids);
    checkpoints.extend(cycle_checkpoints);

    let trace = match existing.and_then(|record| record.get("trace")) {
        Some(serde_json::Value::String(trace)) if !trace.trim().is_empty() => trace.clone(),
        Some(_) => return Err("existing chat run.trace must be a non-empty string".to_string()),
        None => format!("trace:{}", identity.run_id),
    };
    for (source, owner) in &identity.code_mode_owners {
        let door = identity.capability_doors.get(source).ok_or("persistent owner has no exact door")?;
        let prelude_sources = serde_json::Value::Array(owner.prelude_sources.iter().map(|source| serde_json::json!({
            "source":source.source,"snapshot":source.snapshot,"source_sha256":source.source_sha256
        })).collect());
        if let Some(state) = interface_state.get(door) {
            if state.get("prelude_sources") != Some(&prelude_sources) {
                return Err("persistent owner source artifact identities changed during task composition".into());
            }
            let installed = state.get("owned_checkpoint_ids").and_then(serde_json::Value::as_array).ok_or("persistent owner lost its exact checkpoint ownership")?;
            if installed.is_empty() || installed.iter().any(|id| !active_checkpoints.iter().any(|active| id.as_str() == Some(active.as_str())) || !checkpoints.iter().any(|cp| cp.get("id") == Some(id))) {
                return Err("persistent owner checkpoint ownership changed during task composition".into());
            }
            continue;
        }
        let owner_id = format!("cp_owner_{door}_initial");
        let authoring_id = format!("cp_owner_{door}_authoring_initial");
        let mut artifacts = mandatory_role_artifacts.clone();
        artifacts.extend([serde_json::json!({"id":owner.prelude.id}),serde_json::json!({"id":owner.topology.id})]);
        artifacts.extend(payload_artifact_bindings.iter().map(|reference| serde_json::json!({"id":reference.id})));
        checkpoints.push(checkpoint(owner_id.clone(),Some(door),vec![door],&artifacts,Vec::new()));
        checkpoints.push(checkpoint(authoring_id.clone(),Some(&owner.authoring_environment),vec![door],&artifacts,Vec::new()));
        active_checkpoints.extend([owner_id.clone(),authoring_id.clone()]);
        let mut owned_ids = vec![owner_id.clone(),authoring_id.clone()];
        let routes = owner.topology_body.get("routes").and_then(serde_json::Value::as_array).ok_or("persistent topology has no declared routes")?;
        let mut returns = BTreeMap::new();
        for route in routes {
            let result = route.get("result").ok_or("persistent route lost its return")?;
            let name = result.get("name").and_then(serde_json::Value::as_str).ok_or("persistent return has no identity")?;
            if let Some(prior) = returns.insert(name, result) {
                if prior != result { return Err("persistent routes disagree on a shared return".into()); }
            }
        }
        for (name, result) in returns {
            if checkpoints.iter().any(|cp| cp.get("id").and_then(serde_json::Value::as_str) == Some(name)) {
                return Err("persistent initial return collides with a neighboring checkpoint".into());
            }
            let mut row = checkpoint(name.into(),None,Vec::new(),&artifacts,Vec::new());
            row["trace"] = serde_json::json!(trace);
            row["interface_binding"] = result["binding"]["inbound"].clone();
            row["outbound_interface_bindings"] = result["binding"]["outbound"].clone();
            checkpoints.push(row);
            active_checkpoints.push(name.into());
            owned_ids.push(name.to_string());
        }
        interface_state.insert(door.clone(),serde_json::json!({"frontier_checkpoint_ids":[owner_id,authoring_id],"owned_checkpoint_ids":owned_ids,"prelude_sources":prelude_sources}));
    }
    let mut record = serde_json::json!({
        "id": identity.run_id,
        "ee": selection.ee_id,
        "worker": identity.worker_interface_id,
        "trace": trace,
        "status": "idle",
        "interface_state": interface_state,
        "checkpoints": checkpoints,
        "active_checkpoints": active_checkpoints,
    });
    context_bindings::bind_selected_context(
        &mut record,
        existing,
        &identity.model_interface_id,
        config.run.context_artifact_bindings.iter().chain(context_artifact_bindings),
        identity.code_mode_owners.values().flat_map(|owner| owner.prelude_sources.iter().map(|source| &source.source.id)),
    )?;
    let content = pretty_json(&record)?;
    RunRecord::from_bytes(content.as_bytes())
        .map_err(|error| format!("planned chat run record failed validation: {error}"))?;
    Ok(content)
}

pub(crate) fn merge_mandatory_role_bindings(
    checkpoint: &mut serde_json::Value,
    mandatory_role_artifacts: &[serde_json::Value],
) -> Result<(), String> {
    let checkpoint_id = checkpoint
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "retained checkpoint.id is malformed".to_string())?
        .to_string();
    let bindings = checkpoint
        .get_mut("artifact_bindings")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| {
            format!("retained checkpoint '{checkpoint_id}' artifact_bindings must be an array")
        })?;
    let mut positions = BTreeMap::new();
    for (index, value) in bindings.iter().enumerate() {
        let binding = serde_json::from_value::<ArtifactBinding>(value.clone()).map_err(|error| {
            format!(
                "retained checkpoint '{checkpoint_id}' has a malformed artifact binding: {error}"
            )
        })?;
        if positions
            .insert(binding.id.as_str().to_string(), index)
            .is_some()
        {
            return Err(format!(
                "retained checkpoint '{checkpoint_id}' repeats artifact binding '{}'",
                binding.id.as_str()
            ));
        }
    }
    for value in mandatory_role_artifacts {
        let required = serde_json::from_value::<ArtifactBinding>(value.clone())
            .map_err(|error| format!("mandatory role artifact binding is malformed: {error}"))?;
        if let Some(index) = positions.get(required.id.as_str()).copied() {
            let mut existing = serde_json::from_value::<ArtifactBinding>(bindings[index].clone())
                .map_err(|error| {
                    format!(
                        "retained checkpoint '{checkpoint_id}' has a malformed artifact binding: {error}"
                    )
                })?;
            if !existing.access.read {
                existing.access.read = true;
                bindings[index] = serde_json::to_value(existing).map_err(|error| {
                    format!(
                        "retained checkpoint '{checkpoint_id}' role binding did not encode: {error}"
                    )
                })?;
            }
        } else {
            positions.insert(required.id.as_str().to_string(), bindings.len());
            bindings.push(value.clone());
        }
    }
    Ok(())
}
