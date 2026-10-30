use super::*;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorContinuityConfig {
    pub(crate) replay_timeout_ms: u64,
    pub(crate) approval: OperatorApprovalContinuity,
    pub(crate) elicitation: OperatorProvidedContinuity,
    pub(crate) user_input: OperatorProvidedContinuity,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorApprovalContinuity {
    pub(crate) request_label: String,
    pub(crate) response_label: String,
    pub(crate) approved: OperatorEvidenceEdge,
    pub(crate) denied: OperatorEvidenceEdge,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorProvidedContinuity {
    pub(crate) request_label: String,
    pub(crate) response_label: String,
    pub(crate) provided: OperatorEvidenceEdge,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OperatorEvidenceEdge {
    pub(crate) mark: String,
    pub(crate) verifier: String,
}

impl OperatorEvidenceEdge {
    pub(crate) fn required(&self, artifact_label: &str, request_id: &str) -> RequiredVerifiedArtifact {
        RequiredVerifiedArtifact {
            mark: MarkId::new(&self.mark),
            artifact_label: artifact_label.to_string(),
            verifier: VerifierId::new(&self.verifier),
            params: serde_json::json!({"request_id": request_id}),
        }
    }
}

impl OperatorContinuityConfig {
    pub(crate) fn validate(self) -> Result<Self, InterfaceDriverError> {
        if self.replay_timeout_ms == 0 {
            return Err(InterfaceDriverError::Protocol(
                "codex_app_server operator_continuity.replay_timeout_ms must be positive"
                    .to_string(),
            ));
        }
        let fields = [
            (
                "approval.request_label",
                self.approval.request_label.as_str(),
            ),
            (
                "approval.response_label",
                self.approval.response_label.as_str(),
            ),
            (
                "approval.approved.mark",
                self.approval.approved.mark.as_str(),
            ),
            (
                "approval.approved.verifier",
                self.approval.approved.verifier.as_str(),
            ),
            ("approval.denied.mark", self.approval.denied.mark.as_str()),
            (
                "approval.denied.verifier",
                self.approval.denied.verifier.as_str(),
            ),
            (
                "elicitation.request_label",
                self.elicitation.request_label.as_str(),
            ),
            (
                "elicitation.response_label",
                self.elicitation.response_label.as_str(),
            ),
            (
                "elicitation.provided.mark",
                self.elicitation.provided.mark.as_str(),
            ),
            (
                "elicitation.provided.verifier",
                self.elicitation.provided.verifier.as_str(),
            ),
            (
                "user_input.request_label",
                self.user_input.request_label.as_str(),
            ),
            (
                "user_input.response_label",
                self.user_input.response_label.as_str(),
            ),
            (
                "user_input.provided.mark",
                self.user_input.provided.mark.as_str(),
            ),
            (
                "user_input.provided.verifier",
                self.user_input.provided.verifier.as_str(),
            ),
        ];
        for (field, value) in fields {
            if value.trim().is_empty() || value.trim() != value {
                return Err(InterfaceDriverError::Protocol(format!(
                    "codex_app_server operator_continuity.{field} must be non-empty and trimmed"
                )));
            }
        }
        let exact = [
            (
                self.approval.request_label.as_str(),
                OPERATOR_APPROVAL_REQUEST_LABEL,
                "approval.request_label",
            ),
            (
                self.approval.response_label.as_str(),
                OPERATOR_APPROVAL_RESPONSE_LABEL,
                "approval.response_label",
            ),
            (
                self.approval.approved.mark.as_str(),
                "operator.approval",
                "approval.approved.mark",
            ),
            (
                self.approval.approved.verifier.as_str(),
                "operator_approval",
                "approval.approved.verifier",
            ),
            (
                self.approval.denied.mark.as_str(),
                "operator.input.provided",
                "approval.denied.mark",
            ),
            (
                self.approval.denied.verifier.as_str(),
                "operator_input",
                "approval.denied.verifier",
            ),
            (
                self.elicitation.request_label.as_str(),
                OPERATOR_ELICITATION_REQUEST_LABEL,
                "elicitation.request_label",
            ),
            (
                self.elicitation.response_label.as_str(),
                OPERATOR_ELICITATION_RESPONSE_LABEL,
                "elicitation.response_label",
            ),
            (
                self.elicitation.provided.mark.as_str(),
                "operator.input.provided",
                "elicitation.provided.mark",
            ),
            (
                self.elicitation.provided.verifier.as_str(),
                "operator_input",
                "elicitation.provided.verifier",
            ),
            (
                self.user_input.request_label.as_str(),
                OPERATOR_USER_INPUT_REQUEST_LABEL,
                "user_input.request_label",
            ),
            (
                self.user_input.response_label.as_str(),
                OPERATOR_USER_INPUT_RESPONSE_LABEL,
                "user_input.response_label",
            ),
            (
                self.user_input.provided.mark.as_str(),
                "operator.input.provided",
                "user_input.provided.mark",
            ),
            (
                self.user_input.provided.verifier.as_str(),
                "operator_input",
                "user_input.provided.verifier",
            ),
        ];
        for (actual, expected, field) in exact {
            if actual != expected {
                return Err(InterfaceDriverError::FailedClosed(format!(
                    "codex_app_server operator_continuity.{field} must be '{expected}', found '{actual}'"
                )));
            }
        }
        Ok(self)
    }

    pub(crate) fn approval_inputs(&self, request_id: &str) -> Vec<ProviderVerifiedInput> {
        [&self.approval.approved, &self.approval.denied]
            .into_iter()
            .map(|edge| ProviderVerifiedInput {
                also_deliver_to_tool: None,
                artifact_label: self.approval.response_label.clone(),
                required_verified_artifact: edge
                    .required(&self.approval.response_label, request_id),
            })
            .collect()
    }

    pub(crate) fn provided_input(
        &self,
        continuity: &OperatorProvidedContinuity,
        request_id: &str,
    ) -> Vec<ProviderVerifiedInput> {
        vec![ProviderVerifiedInput {
            also_deliver_to_tool: None,
            artifact_label: continuity.response_label.clone(),
            required_verified_artifact: continuity
                .provided
                .required(&continuity.response_label, request_id),
        }]
    }
}

#[cfg(test)]
pub(crate) fn test_operator_continuity_config() -> OperatorContinuityConfig {
    OperatorContinuityConfig {
        replay_timeout_ms: 30_000,
        approval: OperatorApprovalContinuity {
            request_label: OPERATOR_APPROVAL_REQUEST_LABEL.to_string(),
            response_label: OPERATOR_APPROVAL_RESPONSE_LABEL.to_string(),
            approved: OperatorEvidenceEdge {
                mark: "operator.approval".to_string(),
                verifier: "operator_approval".to_string(),
            },
            denied: OperatorEvidenceEdge {
                mark: "operator.input.provided".to_string(),
                verifier: "operator_input".to_string(),
            },
        },
        elicitation: OperatorProvidedContinuity {
            request_label: OPERATOR_ELICITATION_REQUEST_LABEL.to_string(),
            response_label: OPERATOR_ELICITATION_RESPONSE_LABEL.to_string(),
            provided: OperatorEvidenceEdge {
                mark: "operator.input.provided".to_string(),
                verifier: "operator_input".to_string(),
            },
        },
        user_input: OperatorProvidedContinuity {
            request_label: OPERATOR_USER_INPUT_REQUEST_LABEL.to_string(),
            response_label: OPERATOR_USER_INPUT_RESPONSE_LABEL.to_string(),
            provided: OperatorEvidenceEdge {
                mark: "operator.input.provided".to_string(),
                verifier: "operator_input".to_string(),
            },
        },
    }
    .validate()
    .expect("test operator continuity config")
}
