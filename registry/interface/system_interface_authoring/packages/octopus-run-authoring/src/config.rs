use super::*;

impl RunAuthoringConfig {
    pub(crate) fn validate(&self, lifted_interface: &InterfaceId, run: &RunId) -> Result<(), String> {
        if &self.run != run {
            return Err(format!(
                "run_authoring config belongs to run '{}', invocation belongs to '{}'",
                self.run.as_str(),
                run.as_str()
            ));
        }
        if &self.provider_interface != lifted_interface {
            return Err(format!(
                "run_authoring provider '{}' does not match lifted interface '{}'",
                self.provider_interface.as_str(),
                lifted_interface.as_str()
            ));
        }
        if self.result_timeout_ms == 0 {
            return Err("run_authoring result_timeout_ms must be positive".to_string());
        }
        for (name, value) in [
            ("file_write_tool", self.file_write_tool.as_str()),
            ("file_write_label", self.file_write_label.as_str()),
            (
                "file_write_result_label",
                self.file_write_result_label.as_str(),
            ),
        ] {
            non_empty_string(value, name)?;
        }
        if self.checkpoints.is_empty() {
            return Err("run_authoring checkpoints must not be empty".to_string());
        }
        let mut checkpoint_names = BTreeSet::new();
        let mut role_counts = std::collections::BTreeMap::new();
        let mut tool_owners = BTreeMap::new();
        for (index, checkpoint) in self.checkpoints.iter().enumerate() {
            let owner = format!("checkpoints[{index}]");
            validate_id_fragment(&checkpoint.name, &format!("{owner}.name"))?;
            if !checkpoint_names.insert(checkpoint.name.as_str()) {
                return Err(format!(
                    "run_authoring checkpoints repeat name '{}'",
                    checkpoint.name
                ));
            }
            checkpoint.interface_binding.validate().map_err(|error| {
                format!("run_authoring {owner}.interface_binding is invalid: {error}")
            })?;
            if checkpoint.outbound_interface_bindings.is_empty()
                && !matches!(checkpoint.role, RunAuthoringCheckpointRole::ProviderHandoffCompletion | RunAuthoringCheckpointRole::ProviderArtifactOutput)
            {
                return Err(format!(
                    "run_authoring {owner}.outbound_interface_bindings must not be empty"
                ));
            }
            let mut outbound = BTreeSet::new();
            for (outbound_index, binding) in
                checkpoint.outbound_interface_bindings.iter().enumerate()
            {
                binding.validate().map_err(|error| {
                    format!(
                        "run_authoring {owner}.outbound_interface_bindings[{outbound_index}] is invalid: {error}"
                    )
                })?;
                if !outbound.insert(binding.interface.as_str()) {
                    return Err(format!(
                        "run_authoring {owner}.outbound_interface_bindings repeats interface '{}'",
                        binding.interface.as_str()
                    ));
                }
            }
            let mut artifact_labels = BTreeSet::new();
            for (required_index, required) in
                checkpoint.required_verified_artifacts.iter().enumerate()
            {
                required
                    .validate(&format!(
                        "run_authoring {owner}.required_verified_artifacts[{required_index}]"
                    ))
                    .map_err(|error| format!("run_authoring checkpoint is malformed: {error}"))?;
                if !artifact_labels.insert(required.artifact_label.as_str()) {
                    return Err(format!(
                        "run_authoring {owner}.required_verified_artifacts repeats artifact_label '{}'",
                        required.artifact_label
                    ));
                }
            }
            if let Some(writer) = &checkpoint.owner_authoring {
                if checkpoint.role != RunAuthoringCheckpointRole::OwnerInput || writer.owner == self.provider_interface || (writer.source == writer.owner && writer.owner != checkpoint.interface_binding.interface) || writer.source == self.provider_interface || writer.door == writer.owner || writer.source == writer.door || writer.door == *self.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?.interface() {
                    return Err("external owner writer differs from its exact independent input".into());
                }
                for value in [writer.owner.as_str(),writer.source.as_str(),writer.door.as_str(),&writer.tool,&writer.call_label,&writer.result_label] { non_empty_string(value,"external owner writer")?; }
            }
            if !checkpoint.static_artifact_bindings.is_empty() {
                if (checkpoint.include_context && !matches!(checkpoint.role, RunAuthoringCheckpointRole::Stable | RunAuthoringCheckpointRole::IndependentInput | RunAuthoringCheckpointRole::OwnerInput | RunAuthoringCheckpointRole::ResultInput | RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ProviderToolOutput)) || !matches!(checkpoint.role, RunAuthoringCheckpointRole::Stable | RunAuthoringCheckpointRole::IndependentInput | RunAuthoringCheckpointRole::OwnerInput | RunAuthoringCheckpointRole::ResultInput | RunAuthoringCheckpointRole::VerifiedInput | RunAuthoringCheckpointRole::ProviderToolOutput | RunAuthoringCheckpointRole::ProviderInterfaceOutput) {
                    return Err(format!("run_authoring {owner} static artifacts require a declared input or tool/interface output"));
                }
                let mut ids = BTreeSet::new();
                for id in &checkpoint.static_artifact_bindings {
                    non_empty_string(id.as_str(), "static artifact binding")?;
                    if !ids.insert(id) { return Err(format!("run_authoring {owner} repeats a static artifact")); }
                }
            }
            *role_counts.entry(checkpoint.role).or_insert(0usize) += 1;
            match checkpoint.role {
                RunAuthoringCheckpointRole::Stable => {
                    if checkpoint.interface() == &self.provider_interface
                        || checkpoint.interface() == &self.authoring_source_interface
                        || !checkpoint.include_context
                        || checkpoint.outbound_interface_bindings.len() != 1
                        || checkpoint.outbound_interfaces().next() != Some(&self.provider_interface)
                    {
                        return Err(format!(
                            "run_authoring {owner} context input must carry context through exactly one provider edge"
                        ));
                    }
                }
                RunAuthoringCheckpointRole::IndependentInput => {
                    if checkpoint.interface() == &self.provider_interface
                        || checkpoint.interface() == &self.authoring_source_interface
                        || !checkpoint.outbound_interfaces().any(|id| id == &self.provider_interface)
                    {
                        return Err(format!(
                            "run_authoring {owner} independent input must include its owning provider edge"
                        ));
                    }
                }
                RunAuthoringCheckpointRole::OwnerInput => {
                    if checkpoint.interface() == &self.provider_interface || checkpoint.interface() == &self.authoring_source_interface
                        || !checkpoint.outbound_interfaces().any(|id| id == &self.provider_interface)
                        || (checkpoint.owner_authoring.is_none() && !self.checkpoints.iter().any(|output| output.role == RunAuthoringCheckpointRole::ProviderToolOutput && output.outbound_interfaces().next() == Some(checkpoint.interface()))) {
                        return Err(format!("run_authoring {owner} owner input must name an exact provider edge with its declared writer or tool door"));
                    }
                }
                RunAuthoringCheckpointRole::ResultInput => {}
                RunAuthoringCheckpointRole::VerifiedInput => {
                    let [required] = checkpoint.required_verified_artifacts.as_slice() else {
                        return Err(format!(
                            "run_authoring {owner} verified input must require exactly one verified artifact"
                        ));
                    };
                    if checkpoint.interface() == &self.provider_interface
                        || checkpoint.interface() == &self.authoring_source_interface
                        || !checkpoint.include_context
                        || !matches!(checkpoint.outbound_interface_bindings.len(), 1 | 2)
                        || checkpoint.outbound_interfaces().next() != Some(&self.provider_interface)
                        || !binding_admits_exact_name(
                            &checkpoint.interface_binding,
                            &required.artifact_label,
                        )
                        || checkpoint
                            .outbound_interface_bindings
                            .iter()
                            .any(|binding| {
                                !binding_admits_exact_name(binding, &required.artifact_label)
                            })
                    {
                        return Err(format!(
                            "run_authoring {owner} verified input must be one exact-name, context-bearing client-to-provider verifier edge"
                        ));
                    }
                    if let [_, door] = checkpoint.outbound_interface_bindings.as_slice() {
                        let connected = self.checkpoints.iter().any(|route| {
                            route.role == RunAuthoringCheckpointRole::ProviderToolOutput
                                && route.interface() == &self.provider_interface
                                && route.outbound_interface_bindings.len() == 1
                                && route.outbound_interfaces().next() == Some(&door.interface)
                        });
                        if !connected || door.interface == checkpoint.interface_binding.interface {
                            return Err(format!(
                                "run_authoring {owner} verified input tool door is not connected"
                            ));
                        }
                    }
                }
                RunAuthoringCheckpointRole::ProviderArtifactOutput => {
                    if checkpoint.interface() != &self.provider_interface
                        || checkpoint.include_context
                        || checkpoint.required_verified_artifacts.len() > 1
                    {
                        return Err(format!(
                            "run_authoring {owner} artifact output must be one provider-input edge with at most one verifier and no context"
                        ));
                    }
                }
                RunAuthoringCheckpointRole::ProviderInterfaceOutput => {
                    let [target] = checkpoint.outbound_interface_bindings.as_slice() else {
                        return Err(format!("run_authoring {owner} interface output must have one exact recipient"));
                    };
                    if checkpoint.interface() != &self.provider_interface || target.interface == self.provider_interface
                        || checkpoint.include_context || !checkpoint.required_verified_artifacts.is_empty()
                        || checkpoint.interface_binding.config.artifact_policy != target.config.artifact_policy
                        || !matches!(&target.config.artifact_policy, BindingArtifactPolicy::Allow { names, namespaces } if names.len()==1 && namespaces.is_empty()) {
                        return Err(format!("run_authoring {owner} interface output must preserve one exact name and declared recipient without context or verifier"));
                    }
                }
                RunAuthoringCheckpointRole::ProviderToolOutput => {
                    if checkpoint.interface() != &self.provider_interface
                        || checkpoint.outbound_interface_bindings.len() != 1
                    {
                        return Err(format!(
                            "run_authoring {owner} tool output must be one verified provider-input edge; include_context selects its bound artifact visibility"
                        ));
                    }
                    for tool in checkpoint_tool_ids(checkpoint)? {
                        if let Some(existing) =
                            tool_owners.insert(tool.clone(), checkpoint.name.clone())
                        {
                            return Err(format!(
                                "run_authoring tool '{}' is owned by both '{}' and '{}'",
                                tool, existing, checkpoint.name
                            ));
                        }
                    }
                }
                RunAuthoringCheckpointRole::ProviderTransportDispatch => {
                    if checkpoint.interface() != &self.provider_interface
                        || checkpoint.include_context
                        || checkpoint.outbound_interface_bindings.len() != 1
                    {
                        return Err(format!(
                            "run_authoring {owner} transport dispatch must be one verified provider-input edge without context"
                        ));
                    }
                    transport_dispatch_attempt(checkpoint)?;
                }
                RunAuthoringCheckpointRole::ProviderHandoffCompletion => {
                    if checkpoint.interface() != &self.provider_interface
                        || checkpoint.include_context
                        || !checkpoint.outbound_interface_bindings.is_empty()
                    {
                        return Err(format!(
                            "run_authoring {owner} handoff completion must be one private provider-input checkpoint without outbound edges or context"
                        ));
                    }
                    validate_handoff_completion_checkpoint(checkpoint)?;
                }
                RunAuthoringCheckpointRole::AuthoringCall => {
                    if checkpoint.interface() != &self.authoring_source_interface
                        || checkpoint.include_context
                        || checkpoint.outbound_interface_bindings.len() != 1
                    {
                        return Err(format!(
                            "run_authoring {owner} authoring call must be one authoring-source edge without context"
                        ));
                    }
                }
                RunAuthoringCheckpointRole::AuthoringResult => {
                    if checkpoint.interface() == &self.provider_interface
                        || checkpoint.interface() == &self.authoring_source_interface
                        || checkpoint.include_context
                        || checkpoint.outbound_interface_bindings.len() != 1
                        || checkpoint.outbound_interfaces().next() != Some(&self.provider_interface)
                        || !checkpoint.required_verified_artifacts.is_empty()
                    {
                        return Err(format!(
                            "run_authoring {owner} authoring result must return unverified to the provider interface"
                        ));
                    }
                }
            }
            result_inputs::validate_template(checkpoint, self)?;
        }
        let authoring_call = self.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringCall)?;
        let authoring_result =
            self.checkpoint_for_role(RunAuthoringCheckpointRole::AuthoringResult)?;
        if authoring_call.outbound_interfaces().next() != Some(authoring_result.interface()) {
            return Err(
                "run_authoring authoring call must target the exact authoring-result interface"
                    .to_string(),
            );
        }
        if let Some(stable) = self.checkpoints.iter().find(|checkpoint| {
            checkpoint.role.is_provider_input()
                && checkpoint.interface() == authoring_result.interface()
        }) {
            return Err(format!(
                "run_authoring context input '{}' shares the authoring-result interface '{}'",
                stable.name,
                authoring_result.interface().as_str()
            ));
        }
        let context_inputs = self
            .checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.role.is_provider_input() && checkpoint.include_context)
            .collect::<Vec<_>>();
        if context_inputs.is_empty() {
            return Err(
                "run_authoring checkpoints require at least one context-bearing input".to_string(),
            );
        }
        for (index, left) in context_inputs.iter().enumerate() {
            for right in context_inputs.iter().skip(index + 1) {
                if left.interface_binding == right.interface_binding
                    && left.outbound_interface_bindings == right.outbound_interface_bindings
                    && left.required_verified_artifacts == right.required_verified_artifacts
                {
                    return Err(format!(
                        "run_authoring context inputs '{}' and '{}' declare the same route",
                        left.name, right.name
                    ));
                }
            }
        }
        let artifact_outputs = self
            .checkpoints
            .iter()
            .filter(|checkpoint| {
                checkpoint.role == RunAuthoringCheckpointRole::ProviderArtifactOutput
            })
            .collect::<Vec<_>>();
        for (index, left) in artifact_outputs.iter().enumerate() {
            for right in artifact_outputs.iter().skip(index + 1) {
                if output_artifact_policies_overlap(left, right) {
                    return Err(format!(
                        "run_authoring artifact outputs '{}' and '{}' have overlapping outbound artifact policies",
                        left.name, right.name
                    ));
                }
            }
        }
        let artifact_output_count = role_counts
            .get(&RunAuthoringCheckpointRole::ProviderArtifactOutput)
            .copied()
            .unwrap_or_default();
        if artifact_output_count == 0 {
            return Err(
                "run_authoring checkpoints require at least one ProviderArtifactOutput".to_string(),
            );
        }
        for role in [
            RunAuthoringCheckpointRole::AuthoringCall,
            RunAuthoringCheckpointRole::AuthoringResult,
        ] {
            let count = role_counts.get(&role).copied().unwrap_or_default();
            if count != 1 {
                return Err(format!(
                    "run_authoring checkpoints require exactly one {role:?}, found {count}"
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn checkpoint_for_role(
        &self,
        role: RunAuthoringCheckpointRole,
    ) -> Result<&RunAuthoringCheckpoint, String> {
        let mut matches = self
            .checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.role == role);
        let checkpoint = matches
            .next()
            .ok_or_else(|| format!("run_authoring has no {role:?} checkpoint"))?;
        if matches.next().is_some() {
            return Err(format!(
                "run_authoring has more than one {role:?} checkpoint"
            ));
        }
        Ok(checkpoint)
    }

    pub(crate) fn callable_tool_names(&self, metadata: &InvocationMetadata) -> Result<BTreeSet<String>, String> {
        let granted_tools = metadata.granted_tool_names().iter().cloned().collect::<BTreeSet<_>>();
        let granted_interfaces = metadata.allowed_interfaces().iter().map(InterfaceId::new).collect::<BTreeSet<_>>();
        let mut selected = BTreeSet::new();
        for checkpoint in self.checkpoints.iter().filter(|checkpoint| checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput) {
            for tool in checkpoint_tool_ids(checkpoint)? {
                if !selected.insert(tool.clone()) { return Err(format!("selected provider tool '{tool}' has duplicate output owners")); }
                if metadata.hard_blocked_tool_names().contains(&tool) {
                    return Err(format!("selected provider tool '{tool}' is hard-blocked"));
                }
                let owner = self.output_checkpoint(&ProviderHandoffRoute::Tool { tool: tool.clone() }, &granted_tools, &granted_interfaces, &self.file_write_label)?;
                if owner.interface() != &self.provider_interface || !owner.interface_binding.admits(&self.file_write_label)
                    || std::iter::once(owner.interface()).chain(owner.outbound_interfaces()).any(|interface| !granted_interfaces.contains(interface)) {
                    return Err(format!("selected provider tool '{tool}' has a denied output route"));
                }
                let specs = metadata.interface_affordance_specs().iter().filter(|spec| spec.name.as_str() == tool).collect::<Vec<_>>();
                let [spec] = specs.as_slice() else { return Err(format!("selected provider tool '{tool}' requires one exact descriptor affordance")); };
                if !spec.args_schema.is_object() { return Err(format!("selected provider tool '{tool}' has a malformed descriptor affordance")); }
            }
        }
        Ok(selected)
    }

    pub(crate) fn output_checkpoint(
        &self,
        route: &ProviderHandoffRoute,
        granted_tools: &BTreeSet<String>,
        granted_interfaces: &BTreeSet<InterfaceId>,
        artifact_label: &str,
    ) -> Result<RunAuthoringCheckpoint, String> {
        match route {
            ProviderHandoffRoute::ClientProjection
            | ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { .. } => {
                standing_routes::client_projection_owner(&self.checkpoints, artifact_label).cloned()
            }
            ProviderHandoffRoute::Tool { tool } => {
                if !granted_tools.contains(tool) {
                    return Err(format!(
                        "provider emitted tool '{}' outside the formed invocation grant",
                        tool
                    ));
                }
                let mut owners = Vec::new();
                for checkpoint in self.checkpoints.iter().filter(|checkpoint| {
                    checkpoint.role == RunAuthoringCheckpointRole::ProviderToolOutput
                }) {
                    if checkpoint_tool_ids(checkpoint)?.contains(tool)
                        && checkpoint.outbound_interface_bindings.len() == 1
                        && checkpoint.outbound_interface_bindings[0].admits(artifact_label)
                        && checkpoint.required_verified_artifacts[0].artifact_label
                            == artifact_label
                    {
                        owners.push(checkpoint);
                    }
                }
                match owners.as_slice() {
                    [] => Err(format!(
                        "provider emitted granted tool '{}' with no descriptor-derived output owner",
                        tool
                    )),
                    [owner] => Ok((*owner).clone()),
                    _ => Err(format!(
                        "provider emitted granted tool '{}' with {} descriptor-derived output owners",
                        tool,
                        owners.len()
                    )),
                }
            }
            ProviderHandoffRoute::Interface { interface }
            | ProviderHandoffRoute::Operation { interface, .. } => {
                if !granted_interfaces.contains(interface) {
                    return Err(format!(
                        "provider emitted to interface '{}' outside the formed invocation grant",
                        interface.as_str()
                    ));
                }
                let owners = self
                    .checkpoints
                    .iter()
                    .filter(|checkpoint| {
                        matches!(checkpoint.role, RunAuthoringCheckpointRole::ProviderArtifactOutput | RunAuthoringCheckpointRole::ProviderInterfaceOutput)
                            && checkpoint.outbound_interface_bindings.len() == 1
                            && checkpoint.outbound_interfaces().next() == Some(interface)
                            && checkpoint.outbound_interface_bindings[0].admits(artifact_label)
                            && checkpoint.required_verified_artifacts.is_empty()
                    })
                    .collect::<Vec<_>>();
                match owners.as_slice() {
                    [] => Err(format!(
                        "provider emitted interface artifact '{}' to '{}' with no descriptor-derived output owner",
                        artifact_label,
                        interface.as_str()
                    )),
                    [owner] => Ok((*owner).clone()),
                    _ => Err(format!(
                        "provider emitted interface artifact '{}' to '{}' with {} descriptor-derived output owners",
                        artifact_label,
                        interface.as_str(),
                        owners.len()
                    )),
                }
            }
            ProviderHandoffRoute::TransportDispatch { attempt } => {
                if *attempt == 0 {
                    return Err("provider transport dispatch attempt must be positive".to_string());
                }
                Ok(RunAuthoringCheckpoint {
                    name: format!("transport_dispatch_{attempt}"),
                    role: RunAuthoringCheckpointRole::ProviderTransportDispatch,
                    result_receiving: BTreeMap::new(),
                    interface_binding: AuthoredInterfaceBinding::all(
                        self.provider_interface.clone(),
                    ),
                    outbound_interface_bindings: Vec::new(),
                    include_context: false,
                    static_artifact_bindings: Vec::new(),
        owner_authoring: None,
                    required_verified_artifacts: Vec::new(),
                })
            }
        }
    }

    pub(crate) fn verified_input_checkpoints(
        &self,
        route: &ProviderHandoffRoute,
        output_checkpoint: &RunAuthoringCheckpoint,
        static_artifact_bindings: &[ArtifactId],
        granted_mark_outcomes: &BTreeSet<String>,
        granted_tools: &BTreeSet<String>,
        granted_interfaces: &BTreeSet<InterfaceId>,
    ) -> Result<Vec<RunAuthoringCheckpoint>, String> {
        let ProviderHandoffRoute::ClientProjectionWithVerifiedInputs { inputs } = route else {
            return Ok(Vec::new());
        };
        if inputs.is_empty() {
            return Err(
                "provider client projection declares an empty verified-input set".to_string(),
            );
        }
        let [client_binding] = output_checkpoint.outbound_interface_bindings.as_slice() else {
            return Err(
                "provider verified-input projection has no exact client interface owner"
                    .to_string(),
            );
        };
        let mut checkpoints = Vec::with_capacity(inputs.len());
        let mut requirements = BTreeSet::new();
        for (index, input) in inputs.iter().enumerate() {
            let owner = format!("provider verified input[{index}]");
            non_empty_string(&input.artifact_label, &format!("{owner}.artifact_label"))?;
            input
                .required_verified_artifact
                .validate(&format!("{owner}.required_verified_artifact"))?;
            if input.required_verified_artifact.artifact_label != input.artifact_label {
                return Err(format!(
                    "{owner} artifact label '{}' differs from verifier artifact label '{}'",
                    input.artifact_label, input.required_verified_artifact.artifact_label
                ));
            }
            if !granted_mark_outcomes.contains(input.required_verified_artifact.mark.as_str()) {
                return Err(format!(
                    "{owner} requires mark '{}' outside the formed invocation grant",
                    input.required_verified_artifact.mark.as_str()
                ));
            }
            let identity = serde_json::to_string(&input.required_verified_artifact)
                .map_err(|error| format!("{owner} verifier requirement did not encode: {error}"))?;
            if !requirements.insert(identity) {
                return Err(format!(
                    "provider verified-input projection repeats requirement for '{}'",
                    input.artifact_label
                ));
            }
            let policy = BindingArtifactPolicy::Allow {
                names: vec![input.artifact_label.clone()],
                namespaces: Vec::new(),
            };
            let interface_binding = AuthoredInterfaceBinding {
                interface: client_binding.interface.clone(),
                config: InterfaceBindingConfig {
                    artifact_policy: policy.clone(),
                },
            };
            let mut outbound_interface_bindings = vec![AuthoredInterfaceBinding {
                interface: self.provider_interface.clone(),
                config: InterfaceBindingConfig {
                    artifact_policy: policy.clone(),
                },
            }];
            if let Some(tool) = &input.also_deliver_to_tool {
                let route = self.output_checkpoint(
                    &ProviderHandoffRoute::Tool { tool: tool.clone() },
                    granted_tools,
                    granted_interfaces,
                    &self.file_write_label,
                )?;
                let [door] = route.outbound_interface_bindings.as_slice() else {
                    return Err(format!("{owner} has no exact tool door"));
                };
                if !granted_interfaces.contains(&door.interface)
                    || door.interface == self.provider_interface
                    || door.interface == client_binding.interface
                {
                    return Err(format!(
                        "{owner} tool door is not a distinct granted interface"
                    ));
                }
                outbound_interface_bindings.push(AuthoredInterfaceBinding {
                    interface: door.interface.clone(),
                    config: InterfaceBindingConfig {
                        artifact_policy: policy,
                    },
                });
            }
            let required_verified_artifacts = vec![input.required_verified_artifact.clone()];
            checkpoints.push(RunAuthoringCheckpoint {
                name: derived_checkpoint_name(
                    RunAuthoringCheckpointRole::VerifiedInput,
                    &interface_binding,
                    &outbound_interface_bindings,
                    true,
                    &required_verified_artifacts,
                )?,
                role: RunAuthoringCheckpointRole::VerifiedInput,
                result_receiving: BTreeMap::new(),
                interface_binding,
                outbound_interface_bindings,
                include_context: true,
                static_artifact_bindings: static_artifact_bindings.to_vec(),
        owner_authoring: None,
                required_verified_artifacts,
            });
        }
        Ok(checkpoints)
    }
}
