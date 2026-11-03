use std::collections::{BTreeMap, BTreeSet};

use octopus_appserver_proto::{
    ClientRequest, ExperimentalFeature, ExperimentalFeatureListParams,
    ExperimentalFeatureListResponse, ExperimentalFeatureStage, RequestId,
};
use serde::{Deserialize, Serialize};

use crate::SpawnAppServerClient;

pub const CODEX_SCHEMA_BASELINE: &str = "rust-v0.166.0";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodexCompatibilityRequirements {
    pub minimum_version: String,
    pub required_features: BTreeMap<String, bool>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodexCompatibilityEvidence {
    pub schema_baseline: String,
    pub server_version: String,
    pub requirements: CodexCompatibilityRequirements,
    pub features: Vec<ExperimentalFeature>,
    pub steering: CodexSteeringContract,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CodexSteeringContract {
    pub method: String,
    pub expected_turn_id_required: bool,
    pub client_message_id_field: String,
    pub acknowledgement_stage: String,
    pub input_recording_stage: String,
    pub consumption_proven_by_acknowledgement: bool,
    pub responses_api_steering: bool,
}

pub(crate) fn release_version(value: &str) -> Result<(u64, u64, u64), String> {
    let parts = value.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return Err(format!(
            "Codex version '{value}' is not a stable numeric release"
        ));
    }
    let parsed = parts
        .into_iter()
        .map(|part| {
            part.parse::<u64>()
                .map_err(|_| format!("Codex version '{value}' overflows"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((parsed[0], parsed[1], parsed[2]))
}

impl CodexCompatibilityRequirements {
    pub(crate) fn validate(&self) -> Result<(), String> {
        release_version(&self.minimum_version)?;
        if self.required_features.is_empty()
            || self.required_features.keys().any(|name| {
                name.is_empty()
                    || !name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte == b'_' || byte.is_ascii_digit()
                    })
            })
        {
            return Err("Codex compatibility requires explicit feature names and states".into());
        }
        Ok(())
    }

    fn check_version(&self, server_version: Option<&str>) -> Result<String, String> {
        self.validate()?;
        let actual = server_version.ok_or_else(|| {
            "Codex initialize returned no server version; compatibility cannot be established"
                .to_string()
        })?;
        let actual_version = release_version(actual)?;
        let minimum = release_version(&self.minimum_version)?;
        if actual_version.0 != minimum.0 || actual_version < minimum {
            return Err(format!(
                "Codex app-server {actual} does not meet the declared minimum {}; update the Octopus Codex provider package or the explicitly selected CLI and reconnect this provider",
                self.minimum_version
            ));
        }
        Ok(actual.to_string())
    }

    fn evidence(
        &self,
        server_version: String,
        available: BTreeMap<String, ExperimentalFeature>,
    ) -> Result<CodexCompatibilityEvidence, String> {
        let mut features = Vec::new();
        for (name, expected) in &self.required_features {
            let feature = available.get(name).ok_or_else(|| {
                format!("Codex {server_version} does not advertise required feature '{name}'")
            })?;
            if feature.enabled != *expected
                || matches!(feature.stage, ExperimentalFeatureStage::Removed)
            {
                return Err(format!(
                    "Codex {server_version} feature '{name}' is {:?} with enabled={}, required enabled={expected}",
                    feature.stage, feature.enabled
                ));
            }
            features.push(feature.clone());
        }
        Ok(CodexCompatibilityEvidence {
            schema_baseline: CODEX_SCHEMA_BASELINE.into(),
            server_version,
            requirements: self.clone(),
            features,
            steering: CodexSteeringContract {
                method: "turn/steer".into(),
                expected_turn_id_required: true,
                client_message_id_field: "clientUserMessageId".into(),
                acknowledgement_stage: "accepted_by_app_server".into(),
                input_recording_stage: "input_recorded_in_turn".into(),
                consumption_proven_by_acknowledgement: false,
                responses_api_steering: false,
            },
        })
    }

    pub(crate) async fn negotiate(
        &self,
        client: &SpawnAppServerClient,
    ) -> Result<CodexCompatibilityEvidence, String> {
        let server_version = self.check_version(client.server_version())?;
        let mut cursor = None;
        let mut cursors = BTreeSet::new();
        let mut available = BTreeMap::new();
        for page in 0..64 {
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                client.request_typed::<ExperimentalFeatureListResponse>(
                    ClientRequest::ExperimentalFeatureList {
                        request_id: RequestId::String(format!(
                            "octopus-compatibility-features-{page}"
                        )),
                        params: ExperimentalFeatureListParams {
                            cursor,
                            limit: Some(100),
                            thread_id: None,
                        },
                    },
                ),
            )
            .await
            .map_err(|_| "Codex compatibility feature negotiation timed out".to_string())?
            .map_err(|error| format!("Codex compatibility feature negotiation failed: {error}"))?;
            for feature in response.data {
                let name = feature.name.clone();
                if name.is_empty() || available.insert(name.clone(), feature).is_some() {
                    return Err(format!(
                        "Codex feature catalog has an empty or repeated name '{name}'"
                    ));
                }
            }
            match response.next_cursor {
                None => return self.evidence(server_version, available),
                Some(next) if !next.is_empty() && cursors.insert(next.clone()) => {
                    cursor = Some(next)
                }
                Some(_) => {
                    return Err("Codex feature catalog has an empty or repeated cursor".into());
                }
            }
        }
        Err("Codex feature catalog exceeds the compatibility page limit".into())
    }
}

#[cfg(test)]
#[path = "../tests/support/compatibility.rs"]
mod tests;
