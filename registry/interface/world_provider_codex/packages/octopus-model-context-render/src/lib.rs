use std::collections::BTreeSet;

use octopus_interfaces::{InterfaceToolAffordance, InvocationMetadata};
use serde_json::{Map, Value};

mod instructions;
pub use instructions::{ResolvedInstructionContext, resolve_instruction_context};

pub fn render_model_context_block(metadata: &InvocationMetadata) -> Option<String> {
    render_model_context_block_with_visible_tools(metadata, &[])
}

pub fn render_model_context_block_with_hidden_tools(
    metadata: &InvocationMetadata,
    hidden_tool_names: &[String],
) -> Option<String> {
    let visible_tools = metadata
        .grant()
        .tools
        .iter()
        .filter(|tool| !hidden_tool_names.iter().any(|hidden| hidden == *tool))
        .cloned()
        .collect::<Vec<_>>();
    render_model_context_block_with_visible_tools(metadata, &visible_tools)
}

fn render_model_context_block_with_visible_tools(
    metadata: &InvocationMetadata,
    visible_tools: &[String],
) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();

    if let Some(goal) = render_goal(metadata) {
        sections.push(goal);
    }
    if let Some(runtime) = &metadata.runtime {
        sections.push(format!(
            "CURRENT RUN — runtime-attested coordinates:\nrun: {}\nEE: {}\nworker: {}\ncheckpoint: {}\nepoch: {}\nreceiving interface: {}",
            runtime.run, runtime.ee, runtime.worker, runtime.checkpoint, runtime.epoch,
            runtime.interface,
        ));
    }
    if let Some(obligations) = render_obligations(&metadata.required_verified_artifacts) {
        sections.push(obligations);
    }
    if let Some(grant) = render_grant(metadata, visible_tools) {
        sections.push(grant);
    }
    if let Some(failure) = &metadata.verification_failure {
        sections.push(format!(
            "VERIFICATION FAILURE — evidence to inspect before repairing or repeating an effect:\n{}",
            serde_json::json!(failure),
        ));
    }

    if sections.is_empty() {
        return None;
    }
    Some(sections.join("\n\n"))
}

fn render_goal(metadata: &InvocationMetadata) -> Option<String> {
    let goal = metadata
        .goal()
        .map(str::trim)
        .filter(|goal| !goal.is_empty())?;
    Some(format!(
        "GOAL \u{2014} what this run exists to accomplish:\n{goal}"
    ))
}

fn render_obligations(required: &[octopus_marks::RequiredVerifiedArtifact]) -> Option<String> {
    if required.is_empty() {
        return None;
    }
    let mut out = String::from(
        "OBLIGATIONS — to close this checkpoint you must produce these verified artifacts \
         (a verifier checks each on arrival; only a pass records its mark):",
    );
    for req in required {
        out.push_str(&format!(
            "\n- mark `{}`: emit an artifact labeled `{}`, verified by `{}`",
            req.mark.as_str(),
            req.artifact_label,
            req.verifier.as_str(),
        ));
        if let Some(params) = render_params(&req.params) {
            out.push_str(&format!("; verifier params: {params}"));
        }
    }
    Some(out)
}

fn render_params(params: &Value) -> Option<String> {
    match params {
        Value::Null => None,
        Value::Object(map) if map.is_empty() => None,
        other => serde_json::to_string(other).ok(),
    }
}

fn render_grant(metadata: &InvocationMetadata, visible_tools: &[String]) -> Option<String> {
    let grant = metadata.grant();

    let mut lines: Vec<String> = Vec::new();
    push_grant_line(&mut lines, visible_tools, "callable tools");
    push_grant_line(&mut lines, &grant.interfaces, "reachable interfaces");
    push_grant_line(
        &mut lines,
        &grant.worker_environment_kinds,
        "worker environments",
    );
    push_grant_line(
        &mut lines,
        &grant.mark_outcomes,
        "mark outcomes this grant can close",
    );
    if let Some(slots) = &grant.ee_root_slot_allowance {
        if slots.is_empty() {
            lines.push("EE root-slot ceiling: [] (this EE permits no named filesystem roots)".into());
        } else {
            push_grant_line(
                &mut lines,
                slots,
                "EE root-slot ceiling (each tool may be narrower; slot names are not filesystem paths)",
            );
        }
    }

    if lines.is_empty() {
        return None;
    }
    let mut out = String::from(
        "GRANT — what this run may do here (the runtime gate enforces this regardless of the text \
         below; an action not listed here will be refused):",
    );
    for line in lines {
        out.push_str("\n- ");
        out.push_str(&line);
    }
    Some(out)
}

pub fn render_checkpoint_bindings_block(metadata: &InvocationMetadata) -> Option<String> {
    let snapshot = metadata.checkpoint_binding_policy.as_ref()?;
    let allowed = |interface: &str| metadata.grant.interfaces.iter().any(|id| id == interface);
    let inbound = std::iter::once(&snapshot.inbound).filter(|binding| allowed(binding.interface.as_str())).collect::<Vec<_>>();
    let outbound = snapshot.outbound.iter().filter(|binding| allowed(binding.interface.as_str())).collect::<Vec<_>>();
    let attached = metadata.grant.attached_interface_bindings.iter()
        .filter(|binding| metadata.grant.interfaces.iter().any(|id| id == binding.interface.as_str()))
        .collect::<Vec<_>>();
    Some(format!(
        "CHECKPOINT BINDINGS — exact interfaces and their admitted artifact vocabulary:\n{}\nThese are this checkpoint's authored routes within the current grant. Other reachable interfaces still require an authored binding; a registry entry alone does not create a route.",
        serde_json::json!({"inbound":inbound,"outbound":outbound,"interface_descriptions":attached}),
    ))
}

fn push_grant_line(lines: &mut Vec<String>, values: &[String], label: &str) {
    let mut normalized: Vec<&str> = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    normalized.sort_unstable();
    normalized.dedup();
    if !normalized.is_empty() {
        lines.push(format!("{label}: {}", normalized.join(", ")));
    }
}

pub fn model_visible_affordances(
    affordances: Vec<InterfaceToolAffordance>,
    hidden_tool_names: &[String],
) -> Result<Vec<Value>, String> {
    let mut visible = Vec::with_capacity(affordances.len());
    for affordance in affordances {
        if hidden_tool_names
            .iter()
            .any(|hidden| hidden == affordance.name.as_str())
        {
            continue;
        }
        if !affordance.args_schema.is_object() {
            return Err(format!(
                "interface affordance '{}' args_schema must be an object",
                affordance.name
            ));
        }
        visible.push(serde_json::json!({
            "name": affordance.name.as_str(),
            "description": affordance.description,
            "input_schema": affordance.args_schema,
        }));
    }
    Ok(visible)
}

pub fn model_tool_exclusions(
    metadata: &InvocationMetadata,
    selected: &BTreeSet<String>,
    declared_hidden: &[String],
) -> Vec<String> {
    declared_hidden.iter().cloned().chain(
        metadata.granted_tool_names().iter().map(String::as_str)
            .chain(metadata.interface_affordance_specs().iter().map(|spec| spec.name.as_str()))
            .filter(|tool| !selected.contains(*tool)).map(str::to_string)
    ).collect::<BTreeSet<_>>().into_iter().collect()
}

pub fn model_hidden_tool_names(private_config: &Map<String, Value>) -> Result<Vec<String>, String> {
    let Some(value) = private_config.get("model_hidden_tools") else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or_else(|| {
        "interface_driver_config.private_config.model_hidden_tools must be an array".to_string()
    })?;
    let mut names = Vec::with_capacity(values.len());
    let mut seen = BTreeSet::new();
    for (index, value) in values.iter().enumerate() {
        let name = value
            .as_str()
            .filter(|name| !name.is_empty() && name.trim() == *name)
            .ok_or_else(|| {
                format!(
                    "interface_driver_config.private_config.model_hidden_tools[{index}] must be a non-empty trimmed string"
                )
            })?;
        if !seen.insert(name.to_string()) {
            return Err(format!(
                "interface_driver_config.private_config.model_hidden_tools repeats '{name}'"
            ));
        }
        names.push(name.to_string());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{InterfaceId, MarkId, ToolId, VerifierId};
    use octopus_interfaces::{GrantProjection, InterfaceToolAffordance};
    use octopus_marks::RequiredVerifiedArtifact;
    use serde_json::json;

    fn metadata_with(
        required: Vec<RequiredVerifiedArtifact>,
        grant: GrantProjection,
        outbound: Vec<InterfaceId>,
    ) -> InvocationMetadata {
        InvocationMetadata::new(grant)
            .with_required_verified_artifacts(required)
            .with_outbound_interface_bindings(outbound)
    }

    fn grant_axes(
        tools: &[&str],
        interfaces: &[&str],
        wek: &[&str],
        marks: &[&str],
    ) -> GrantProjection {
        GrantProjection {
            tools: tools.iter().map(|s| s.to_string()).collect(),
            interfaces: interfaces.iter().map(|s| s.to_string()).collect(),
            worker_environment_kinds: wek.iter().map(|s| s.to_string()).collect(),
            mark_outcomes: marks.iter().map(|s| s.to_string()).collect(),
            ..GrantProjection::default()
        }
    }

    fn required(
        mark: &str,
        label: &str,
        verifier: &str,
        params: Value,
    ) -> RequiredVerifiedArtifact {
        RequiredVerifiedArtifact {
            mark: MarkId::new(mark),
            artifact_label: label.to_string(),
            verifier: VerifierId::new(verifier),
            params,
        }
    }

    fn outbound(interface: &str) -> InterfaceId {
        InterfaceId::new(interface)
    }

    fn affordance(name: &str) -> InterfaceToolAffordance {
        InterfaceToolAffordance {
            name: ToolId::new(name),
            description: String::new(),
            args_schema: json!({}),
        }
    }

    #[test]
    fn obligation_render_shows_mark_label_verifier_and_params() {
        let metadata = metadata_with(
            vec![required(
                "ocr.text.contains",
                "screenshot",
                "ocr_text_contains",
                json!({ "expected_text": "BUILD OK" }),
            )],
            GrantProjection::default(),
            Vec::new(),
        );
        let block = render_model_context_block(&metadata).expect("obligations render");
        assert!(block.contains("mark `ocr.text.contains`"));
        assert!(block.contains("labeled `screenshot`"));
        assert!(block.contains("verified by `ocr_text_contains`"));
        assert!(
            block.contains("\"expected_text\":\"BUILD OK\""),
            "verifier params must render verbatim so the model sees expected_text, got: {block}"
        );
    }

    #[test]
    fn grant_render_lists_only_actionable_non_empty_axes() {
        let metadata = metadata_with(
            Vec::new(),
            grant_axes(
                &["coder.build", "artifact.read"],
                &["world_provider_codex"],
                &["package_script"],
                &["coder.build.ok"],
            )
            .with_ee_root_slot_allowance(Some(vec!["workspace_root".into()])),
            vec![outbound("surface_client"), outbound("system_coder_env")],
        );
        let block = render_model_context_block(&metadata).expect("grant renders");
        assert!(!block.contains("callable tools:"));
        assert!(block.contains("reachable interfaces: world_provider_codex"));
        assert!(block.contains("worker environments: package_script"));
        assert!(block.contains("mark outcomes this grant can close: coder.build.ok"));
        assert!(!block.contains("spawnable worker types"));
        assert!(!block.contains("surface_client"));
        assert!(!block.contains("system_coder_env"));
        assert!(block.contains("EE root-slot ceiling (each tool may be narrower; slot names are not filesystem paths): workspace_root"));
        assert!(!block.contains("/workspace_root"));
    }

    #[test]
    fn empty_envelope_renders_nothing() {
        let metadata = metadata_with(Vec::new(), GrantProjection::default(), Vec::new());
        assert!(render_model_context_block(&metadata).is_none());
    }

    #[test]
    fn current_coordinates_and_binding_filters_survive_model_projection() {
        let mut metadata = InvocationMetadata::new(grant_axes(&[], &["provider", "new_room"], &[], &[]));
        metadata.runtime = Some(serde_json::from_value(json!({
            "run":"task_7","ee":"project_2","worker":"worker_7",
            "checkpoint":"input_3","interface":"provider","epoch":4,
        })).unwrap());
        let binding = |id| json!({"interface":id,"config":{"artifact_policy":{"mode":"allow","names":["assignment"],"namespaces":[]}}});
        metadata.checkpoint_binding_policy = Some(serde_json::from_value(json!({
            "inbound":binding("provider"),"outbound":[binding("new_room"),binding("denied_room")],
        })).unwrap());
        let block = render_model_context_block(&metadata).unwrap();
        for expected in ["run: task_7", "EE: project_2", "epoch: 4", "\"interface\":\"new_room\"", "\"names\":[\"assignment\"]"] {
            assert!(block.contains(expected), "missing {expected}: {block}");
        }
        assert!(!block.contains("denied_room"));
        metadata.grant.interfaces.retain(|id| id != "new_room");
        assert!(!render_model_context_block(&metadata).unwrap().contains("new_room"));
    }

    #[test]
    fn empty_root_ceiling_and_failure_details_remain_actionable() {
        let mut metadata = InvocationMetadata::new(GrantProjection::default().with_ee_root_slot_allowance(Some(Vec::new())));
        metadata.verification_failure = Some(serde_json::from_value(json!({
            "error":"verification_failed","verifier":"schema_check","details":{"path":"/required_field","reason":"missing"},
        })).unwrap());
        let block = render_model_context_block(&metadata).unwrap();
        assert!(block.contains("EE root-slot ceiling: []"));
        assert!(block.contains("\"path\":\"/required_field\""));
        assert!(block.contains("missing"));
        assert!(block.contains("not resolved filesystem paths"));
    }

    #[test]
    fn hidden_tools_are_absent_from_textual_and_api_surfaces() {
        let metadata = metadata_with(
            Vec::new(),
            grant_axes(&["file.write", "artifact.read"], &[], &[], &[]),
            Vec::new(),
        );
        let hidden = vec!["file.write".to_string()];
        let block = render_model_context_block_with_hidden_tools(&metadata, &hidden)
            .expect("visible grant renders");
        assert!(block.contains("callable tools: artifact.read"));
        assert!(!block.contains("file.write"));
        let specs = vec![affordance("file.write"), affordance("artifact.read")];
        let visible = model_visible_affordances(specs, &hidden).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0]["name"], "artifact.read");
    }

    #[test]
    fn our_goal_leads_the_model_context_block() {
        let metadata = metadata_with(
            Vec::new(),
            GrantProjection::default().with_goal(Some("ship the coder worker end to end".into())),
            Vec::new(),
        );
        let block = render_model_context_block(&metadata).expect("our goal renders a block");
        assert!(
            block.contains("GOAL"),
            "our goal is rendered as a leading GOAL section, got: {block}"
        );
        assert!(block.contains("ship the coder worker end to end"));
    }

    #[test]
    fn our_goal_precedes_grant_and_obligations() {
        let metadata = metadata_with(
            vec![required(
                "ocr.text.contains",
                "screenshot",
                "ocr_text_contains",
                json!({}),
            )],
            grant_axes(&["artifact.read"], &["world_provider_codex"], &[], &[])
                .with_goal(Some(" our goal".into())),
            vec![outbound("surface_client")],
        );
        let block = render_model_context_block(&metadata).expect("full block renders");
        let goal_at = block.find("GOAL").expect("goal present");
        let obligations_at = block.find("OBLIGATIONS").expect("obligations present");
        let grant_at = block.find("GRANT").expect("grant present");
        assert!(
            goal_at < obligations_at && obligations_at < grant_at,
            "our goal leads, then obligations, then grant, got: {block}"
        );
    }

    #[test]
    fn empty_or_absent_goal_renders_no_goal_section() {
        let absent = metadata_with(
            Vec::new(),
            grant_axes(&[], &["world_test"], &[], &[]),
            Vec::new(),
        );
        let block = render_model_context_block(&absent).expect("grant still renders");
        assert!(!block.contains("GOAL"), "no goal key means no GOAL section");

        let blank = metadata_with(
            Vec::new(),
            GrantProjection::default().with_goal(Some("   ".into())),
            Vec::new(),
        );
        assert!(
            render_model_context_block(&blank).is_none(),
            "a blank goal renders nothing (fail to a bare block, never a phantom goal)"
        );
    }

    #[test]
    fn tool_cut_removes_reserved_names_only() {
        let specs = vec![
            affordance("coder.build"),
            affordance("coder.write_file"),
            affordance("artifact.read"),
        ];
        let visible = model_visible_affordances(specs, &["coder.write_file".to_string()]).unwrap();
        let names: Vec<&str> = visible
            .iter()
            .filter_map(|spec| spec.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names, vec!["coder.build", "artifact.read"]);
        let mut metadata = metadata_with(Vec::new(), grant_axes(&["coder.build", "coder.write_file", "candidate"], &[], &[], &[]), Vec::new());
        metadata.grant.interface_affordance_specs = vec![affordance("coder.build"), affordance("coder.write_file"), affordance("candidate")];
        let before = metadata.clone();
        let excluded = model_tool_exclusions(&metadata, &BTreeSet::from(["coder.build".into(), "coder.write_file".into()]), &["coder.write_file".into()]);
        let visible = model_visible_affordances(metadata.interface_affordance_specs().to_vec(), &excluded).unwrap();
        assert_eq!(visible.iter().map(|spec| spec["name"].as_str().unwrap()).collect::<Vec<_>>(), vec!["coder.build"]);
        let block = render_model_context_block_with_hidden_tools(&metadata, &excluded).unwrap();
        assert!(block.contains("callable tools: coder.build"));
        assert!(!block.contains("candidate") && !block.contains("coder.write_file"));
        assert_eq!(metadata, before);
    }

    #[test]
    fn tool_cut_with_no_reserved_names_is_identity() {
        let visible = model_visible_affordances(vec![affordance("coder.build")], &[]).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0]["name"], "coder.build");
    }

    #[test]
    fn hidden_tool_names_are_strict_descriptor_data() {
        let metadata = serde_json::from_value(json!({
            "model_hidden_tools": ["file.write", "coder.exec"]
        }))
        .unwrap();
        assert_eq!(
            model_hidden_tool_names(&metadata).unwrap(),
            vec!["file.write".to_string(), "coder.exec".to_string()]
        );

        let malformed = serde_json::from_value(json!({
            "model_hidden_tools": ["file.write", "file.write"]
        }))
        .unwrap();
        assert!(model_hidden_tool_names(&malformed).is_err());
    }
}
