use octopus_descriptors::{InterfaceId, ToolId};
pub use octopus_ledger::InterfaceToolAffordance;
use octopus_policy::{CapSet, ContextGrant};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub struct InterfaceToolOwnerMap {
    owner: BTreeMap<ToolId, InterfaceId>,
}

impl InterfaceToolOwnerMap {
    pub fn from_pairs(pairs: impl IntoIterator<Item = (ToolId, InterfaceId)>) -> Self {
        Self {
            owner: pairs.into_iter().collect(),
        }
    }

    pub fn owned_tools(&self) -> impl Iterator<Item = (&ToolId, &InterfaceId)> {
        self.owner.iter()
    }

    pub fn owning_interface(&self, tool_id: &ToolId) -> Option<&InterfaceId> {
        self.owner.get(tool_id)
    }

    pub fn grant_contains_owner(&self, grant: &ContextGrant, owner: &InterfaceId) -> bool {
        grant.interfaces.contains(owner)
    }
}

pub fn interface_tool_is_admissible(
    grant: &ContextGrant,
    tool_id: &ToolId,
    ownership: &InterfaceToolOwnerMap,
) -> bool {
    if !grant.allows_tool(tool_id) {
        return false;
    }
    ownership
        .owning_interface(tool_id)
        .map(|owner| ownership.grant_contains_owner(grant, owner))
        .unwrap_or(true)
}

pub fn admissible_interface_tools(
    grant: &ContextGrant,
    ownership: &InterfaceToolOwnerMap,
) -> CapSet<ToolId> {
    CapSet::from_allowed(grant.tools.iter().filter_map(|tool_id| {
        interface_tool_is_admissible(grant, tool_id, ownership).then_some(tool_id.clone())
    }))
}

#[derive(Clone, Debug, Default)]
pub struct InterfaceToolAffordanceList(pub Vec<InterfaceToolAffordance>);

impl InterfaceToolAffordanceList {
    pub fn new(affordances: Vec<InterfaceToolAffordance>) -> Self {
        Self(affordances)
    }

    pub fn iter(&self) -> impl Iterator<Item = &InterfaceToolAffordance> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_inner(self) -> Vec<InterfaceToolAffordance> {
        self.0
    }

    pub fn sorted_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .iter()
            .map(|affordance| affordance.name.as_str().to_string())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn to_metadata(&self) -> Result<Vec<InterfaceToolAffordance>, String> {
        for affordance in &self.0 {
            if !affordance.args_schema.is_object() {
                return Err(format!(
                    "tool '{}' args_schema must be a JSON object for interface invocation metadata",
                    affordance.name
                ));
            }
        }
        Ok(self.0.clone())
    }
}

pub trait InterfaceToolDescriptionView: Send + Sync {
    fn describe(&self, tool: &ToolId) -> Option<InterfaceToolAffordance>;
}

#[derive(Debug, thiserror::Error)]
pub enum InterfaceAffordanceProjectionError {
    #[error("projected interface tool {0} is outside the worker ceiling")]
    AffordanceExceedsCeiling(String),
    #[error("projected interface tool {0} has no loaded tool descriptor")]
    MissingToolDescriptor(String),
}

pub fn project_interface_tool_affordances(
    allowed_tools: &CapSet<ToolId>,
    grant: &ContextGrant,
    ownership: &InterfaceToolOwnerMap,
    tool_descriptions: &dyn InterfaceToolDescriptionView,
) -> Result<InterfaceToolAffordanceList, InterfaceAffordanceProjectionError> {
    let admissible = admissible_interface_tools(grant, ownership);
    let projected = allowed_tools.intersect(&admissible);

    for t in admissible.iter() {
        if !allowed_tools.contains(t) && projected.contains(t) {
            return Err(
                InterfaceAffordanceProjectionError::AffordanceExceedsCeiling(
                    t.as_str().to_string(),
                ),
            );
        }
    }

    let mut affordances = Vec::with_capacity(projected.len());
    for tool in projected.iter() {
        if !allowed_tools.contains(tool) {
            return Err(
                InterfaceAffordanceProjectionError::AffordanceExceedsCeiling(
                    tool.as_str().to_string(),
                ),
            );
        }
        let described = tool_descriptions.describe(tool).ok_or_else(|| {
            InterfaceAffordanceProjectionError::MissingToolDescriptor(tool.as_str().to_string())
        })?;
        affordances.push(described);
    }
    Ok(InterfaceToolAffordanceList(affordances))
}

#[cfg(test)]
mod tests {
    use octopus_descriptors::{InterfaceId, MarkId, ToolId, WorkerEnvironmentKindId};
    use octopus_policy::{CapSet, ContextGrant, DenySet};

    use super::{
        InterfaceToolAffordance, InterfaceToolDescriptionView, InterfaceToolOwnerMap,
        admissible_interface_tools, project_interface_tool_affordances,
    };

    fn allowed_tools() -> CapSet<ToolId> {
        CapSet::from_allowed([ToolId::new("test.write_file"), ToolId::new("coder.build")])
    }

    struct TestToolDescriptions;
    impl InterfaceToolDescriptionView for TestToolDescriptions {
        fn describe(&self, tool: &ToolId) -> Option<InterfaceToolAffordance> {
            Some(InterfaceToolAffordance {
                name: tool.clone(),
                description: format!("desc:{}", tool.as_str()),
                args_schema: serde_json::json!({"type": "object"}),
            })
        }
    }

    fn grant_with(tools: &[&str]) -> ContextGrant {
        ContextGrant {
            tools: CapSet::from_allowed(tools.iter().map(|s| ToolId::new(*s))),
            interfaces: CapSet::from_allowed([InterfaceId::new("world_dialog_test")]),
            wek: CapSet::from_allowed([WorkerEnvironmentKindId::new("git_worktree")]),
            mark_outcomes: CapSet::from_allowed(std::iter::empty::<MarkId>()),
            hard_blocks_tools: DenySet::default(),
            interface_policy: Default::default(),
            ..Default::default()
        }
    }

    #[test]
    fn interface_affordance_projection_is_no_wider_than_the_gate() {
        let allowed = allowed_tools();
        let no_ownership = InterfaceToolOwnerMap::default();
        let grant = grant_with(&["coder.build"]);
        let projected = project_interface_tool_affordances(
            &allowed,
            &grant,
            &no_ownership,
            &TestToolDescriptions,
        )
        .unwrap();
        let names: Vec<&str> = projected.0.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["coder.build"]);
        assert!(!names.contains(&"test.write_file"));

        let wide_grant = grant_with(&["coder.build", "os.rm_rf"]);
        let proj2 = project_interface_tool_affordances(
            &allowed,
            &wide_grant,
            &no_ownership,
            &TestToolDescriptions,
        )
        .unwrap();
        let n2: Vec<&str> = proj2.0.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            n2,
            vec!["coder.build"],
            "out-of-ceiling grant tool is never projected"
        );

        let ownership = InterfaceToolOwnerMap::from_pairs([(
            ToolId::new("coder.build"),
            InterfaceId::new("world_dialog_test"),
        )]);
        let iface_grant = grant_with(&[]);
        let gate_admissible = admissible_interface_tools(&iface_grant, &ownership);
        assert!(
            !gate_admissible.contains(&ToolId::new("coder.build")),
            "interface ownership does not substitute for explicit tool authority"
        );
        let iface_proj = project_interface_tool_affordances(
            &allowed,
            &iface_grant,
            &ownership,
            &TestToolDescriptions,
        )
        .unwrap();
        let iface_names: std::collections::BTreeSet<&str> =
            iface_proj.0.iter().map(|t| t.name.as_str()).collect();
        let gate_in_ceiling: std::collections::BTreeSet<&str> = gate_admissible
            .iter()
            .filter(|t| allowed.contains(t))
            .map(|t| t.as_str())
            .collect();
        assert_eq!(
            iface_names, gate_in_ceiling,
            "the projection advertises exactly the gate's admissible surface"
        );
        assert!(
            iface_names.is_empty(),
            "interface-granted tools are hidden until explicitly granted as tools"
        );
    }
}
