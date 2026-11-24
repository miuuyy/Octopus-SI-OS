use super::*;

pub(crate) fn validate_external_routes(
    inputs: &[ExternalInputSeed],
    relays: &[ExternalRelaySeed],
    model_outputs: &[ModelOutputSeed],
) -> Result<Vec<String>, String> {
    validate_external_route_shapes(inputs, relays, model_outputs)
}

pub(crate) fn validate_external_route_shapes(
    inputs: &[ExternalInputSeed],
    relays: &[ExternalRelaySeed],
    model_outputs: &[ModelOutputSeed],
) -> Result<Vec<String>, String> {
    let mut checkpoints = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut interfaces = BTreeSet::new();
    for input in inputs {
        let checkpoint = safe_segment(&input.checkpoint_id, "external_inputs.checkpoint_id")?;
        let interface = safe_segment(&input.interface_id, "external_inputs.interface_id")?;
        if !checkpoints.insert(checkpoint.clone()) {
            return Err(format!("external routes repeat checkpoint '{checkpoint}'"));
        }
        if !sources.insert(interface.clone()) {
            return Err(format!(
                "external routes repeat source interface '{interface}'"
            ));
        }
        validate_external_route_requirements(
            &input.required_verified_artifacts,
            &format!("external input checkpoint '{checkpoint}'"),
        )?;
        if let Some(operations) = &input.allowed_operations {
            if operations.is_empty() {
                return Err(format!(
                    "external input checkpoint '{checkpoint}' allowed_operations must not be empty"
                ));
            }
            let mut seen = BTreeSet::new();
            for operation in operations {
                if operation.is_empty() || operation.trim() != operation || operation.contains(['\r', '\n', '\0']) {
                    return Err(format!(
                        "external input checkpoint '{checkpoint}' allowed_operations contains malformed operation '{operation}'"
                    ));
                }
                if !seen.insert(operation.as_str()) {
                    return Err(format!(
                        "external input checkpoint '{checkpoint}' allowed_operations repeats '{operation}'"
                    ));
                }
            }
        }
        external_input_binding(input)?;
        interfaces.insert(interface);
    }
    for relay in relays {
        let checkpoint = safe_segment(&relay.checkpoint_id, "external_relays.checkpoint_id")?;
        let interface = safe_segment(&relay.interface_id, "external_relays.interface_id")?;
        if !checkpoints.insert(checkpoint.clone()) {
            return Err(format!("external routes repeat checkpoint '{checkpoint}'"));
        }
        if !sources.insert(interface.clone()) {
            return Err(format!(
                "external routes repeat source interface '{interface}'"
            ));
        }
        if relay.outbound_interface_ids.is_empty() {
            return Err(format!(
                "external relay checkpoint '{checkpoint}' has no outbound interfaces"
            ));
        }
        validate_external_route_requirements(
            &relay.required_verified_artifacts,
            &format!("external relay checkpoint '{checkpoint}'"),
        )?;
        interfaces.insert(interface);
        let mut outbound = BTreeSet::new();
        for target in &relay.outbound_interface_ids {
            let target = safe_segment(target, "external_relays.outbound_interface_ids")?;
            if !outbound.insert(target.clone()) {
                return Err(format!(
                    "external relay checkpoint '{checkpoint}' repeats outbound interface '{target}'"
                ));
            }
            interfaces.insert(target);
        }
    }
    for output in model_outputs {
        let checkpoint = safe_segment(&output.checkpoint_id, "model_outputs.checkpoint_id")?;
        let interface = safe_segment(&output.interface_id, "model_outputs.interface_id")?;
        if !checkpoints.insert(checkpoint.clone()) {
            return Err(format!("external routes repeat checkpoint '{checkpoint}'"));
        }
        output
            .required_verified_artifact
            .validate(&format!("model output checkpoint '{checkpoint}'"))?;
        interfaces.insert(interface);
    }
    Ok(interfaces.into_iter().collect())
}

pub(crate) fn validate_external_route_requirements(
    requirements: &[RequiredVerifiedArtifact],
    owner: &str,
) -> Result<(), String> {
    let mut labels = BTreeSet::new();
    for required in requirements {
        required.validate(owner)?;
        if !labels.insert(required.artifact_label.as_str()) {
            return Err(format!(
                "{owner} repeats required artifact label '{}'",
                required.artifact_label
            ));
        }
    }
    Ok(())
}
