use super::*;

#[derive(Clone, Copy)]
pub(crate) enum ProviderLifeExpectation {
    Native,
    OpenCodex,
}

pub(crate) enum ProviderConnectionMode {
    Native,
    OpenCodex(OpenCodexDescriptorConfig),
}

pub(crate) struct ProviderConnectionFailure {
    pub(crate) message: String,
    pub(crate) retryable: bool,
}

impl std::fmt::Display for ProviderConnectionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub(crate) fn doctor_interface(expectation: ProviderLifeExpectation) -> Result<(), String> {
    let interface = std::env::var("OCTOPUS_INTERFACE_ID")
        .map(InterfaceId::new)
        .map_err(|_| "OCTOPUS_INTERFACE_ID is required by the doctor contract".to_string())?;
    let descriptor =
        octopus_world_pyecho_life_runtime::interface_descriptor_from_process_env(&interface)?
            .ok_or_else(|| {
                format!(
                    "{} is required by the doctor contract",
                    octopus_interface_transport::RESOLVED_INTERFACE_DESCRIPTOR_ENV
                )
            })?;
    validate_provider_descriptor_contract(&descriptor, expectation)?;
    CodexInstanceConfig::from_descriptor(&descriptor).map_err(|error| error.to_string())?.check_working_directory()
}

pub(crate) fn validate_provider_descriptor_contract(
    descriptor: &InterfaceDescriptor,
    expectation: ProviderLifeExpectation,
) -> Result<ProviderConnectionMode, String> {
    let connection_mode = match (expectation, opencodex_descriptor_config(descriptor)?) {
        (ProviderLifeExpectation::Native, None) => ProviderConnectionMode::Native,
        (ProviderLifeExpectation::Native, Some(_)) => {
            return Err(format!(
                "native Codex life refuses OpenCodex interface '{}'",
                descriptor.id
            ));
        }
        (ProviderLifeExpectation::OpenCodex, Some(config)) => {
            ProviderConnectionMode::OpenCodex(config)
        }
        (ProviderLifeExpectation::OpenCodex, None) => {
            return Err(format!(
                "OpenCodex life requires interface '{}' to declare private_config.opencodex",
                descriptor.id
            ));
        }
    };
    validate_codex_projection_descriptor_contract(descriptor).map_err(|error| {
        format!(
            "codex interface '{}' projection contract is invalid: {error}",
            descriptor.id
        )
    })?;
    CodexInstanceConfig::from_descriptor(descriptor).map_err(|error| error.to_string())?;
    RunAuthoringResultIdentity::from_descriptor(descriptor)?;
    Ok(connection_mode)
}
