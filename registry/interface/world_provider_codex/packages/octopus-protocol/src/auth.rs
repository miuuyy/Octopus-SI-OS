use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlanType {
    Known(KnownPlan),
    Unknown(String),
}

impl PlanType {
    pub fn from_raw_value(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "free" => Self::Known(KnownPlan::Free),
            "go" => Self::Known(KnownPlan::Go),
            "plus" => Self::Known(KnownPlan::Plus),
            "pro" => Self::Known(KnownPlan::Pro),
            "prolite" => Self::Known(KnownPlan::ProLite),
            "promax" => Self::Known(KnownPlan::ProMax),
            "team" => Self::Known(KnownPlan::Team),
            "self_serve_business_prolite" => Self::Known(KnownPlan::SelfServeBusinessProLite),
            "self_serve_business_usage_based" => {
                Self::Known(KnownPlan::SelfServeBusinessUsageBased)
            }
            "business" => Self::Known(KnownPlan::Business),
            "ent26" => Self::Known(KnownPlan::Ent26),
            "enterprise_cbp_automation" => Self::Known(KnownPlan::EnterpriseCbpAutomation),
            "enterprise_cbp_usage_based" => Self::Known(KnownPlan::EnterpriseCbpUsageBased),
            "enterprise" | "hc" => Self::Known(KnownPlan::Enterprise),
            "education" | "edu" => Self::Known(KnownPlan::Edu),
            "edu_plus" => Self::Known(KnownPlan::EduPlus),
            "edu_pro" => Self::Known(KnownPlan::EduPro),
            "sci" => Self::Known(KnownPlan::Sci),
            "sci_plus" => Self::Known(KnownPlan::SciPlus),
            _ => Self::Unknown(raw.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KnownPlan {
    Free,
    Go,
    Plus,
    Pro,
    ProLite,
    ProMax,
    Team,
    #[serde(rename = "self_serve_business_prolite")]
    SelfServeBusinessProLite,
    #[serde(rename = "self_serve_business_usage_based")]
    SelfServeBusinessUsageBased,
    Business,
    Ent26,
    #[serde(rename = "enterprise_cbp_automation")]
    EnterpriseCbpAutomation,
    #[serde(rename = "enterprise_cbp_usage_based")]
    EnterpriseCbpUsageBased,
    #[serde(alias = "hc")]
    Enterprise,
    #[serde(alias = "education")]
    Edu,
    #[serde(rename = "edu_plus")]
    EduPlus,
    #[serde(rename = "edu_pro")]
    EduPro,
    Sci,
    #[serde(rename = "sci_plus")]
    SciPlus,
}

impl KnownPlan {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Go => "Go",
            Self::Plus => "Plus",
            Self::Pro => "Pro",
            Self::ProLite => "Pro Lite",
            Self::ProMax => "Pro (Max)",
            Self::Team => "Team",
            Self::SelfServeBusinessProLite => "Self Serve Business ProLite",
            Self::SelfServeBusinessUsageBased => "Self Serve Business Usage Based",
            Self::Business => "Business",
            Self::Ent26 => "Enterprise",
            Self::EnterpriseCbpAutomation => "Enterprise (Automation)",
            Self::EnterpriseCbpUsageBased => "Enterprise CBP Usage Based",
            Self::Enterprise => "Enterprise",
            Self::Edu => "Edu",
            Self::EduPlus => "Edu Plus",
            Self::EduPro => "Edu Pro",
            Self::Sci => "Academic Researchers",
            Self::SciPlus => "Academic Researchers Plus",
        }
    }

    pub fn raw_value(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Go => "go",
            Self::Plus => "plus",
            Self::Pro => "pro",
            Self::ProLite => "prolite",
            Self::ProMax => "promax",
            Self::Team => "team",
            Self::SelfServeBusinessProLite => "self_serve_business_prolite",
            Self::SelfServeBusinessUsageBased => "self_serve_business_usage_based",
            Self::Business => "business",
            Self::Ent26 => "ent26",
            Self::EnterpriseCbpAutomation => "enterprise_cbp_automation",
            Self::EnterpriseCbpUsageBased => "enterprise_cbp_usage_based",
            Self::Enterprise => "enterprise",
            Self::Edu => "edu",
            Self::EduPlus => "edu_plus",
            Self::EduPro => "edu_pro",
            Self::Sci => "sci",
            Self::SciPlus => "sci_plus",
        }
    }

    pub fn is_workspace_account(self) -> bool {
        matches!(
            self,
            Self::Team
                | Self::SelfServeBusinessProLite
                | Self::SelfServeBusinessUsageBased
                | Self::Business
                | Self::Ent26
                | Self::EnterpriseCbpAutomation
                | Self::EnterpriseCbpUsageBased
                | Self::Enterprise
                | Self::Edu
                | Self::EduPlus
                | Self::EduPro
                | Self::Sci
                | Self::SciPlus
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct RefreshTokenFailedError {
    pub reason: RefreshTokenFailedReason,
    pub message: String,
}

impl RefreshTokenFailedError {
    pub fn new(reason: RefreshTokenFailedReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTokenFailedReason {
    Expired,
    Exhausted,
    Revoked,
    Other,
}

#[cfg(test)]
mod tests {
    use super::KnownPlan;
    use super::PlanType;
    use pretty_assertions::assert_eq;

    #[test]
    fn plan_type_deserializes_raw_aliases() {
        assert_eq!(
            serde_json::from_str::<PlanType>("\"hc\"").expect("hc should deserialize"),
            PlanType::Known(KnownPlan::Enterprise)
        );
        assert_eq!(
            serde_json::from_str::<PlanType>("\"education\"")
                .expect("education should deserialize"),
            PlanType::Known(KnownPlan::Edu)
        );
    }
}
