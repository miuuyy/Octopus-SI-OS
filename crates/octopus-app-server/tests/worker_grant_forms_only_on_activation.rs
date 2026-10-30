mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use octopus_app_server::{
    AuthoredBindings, EmbeddedRuntime, SharedBlobs, SpineRootConfig, form_worker_run_policy,
};
use octopus_descriptors::{ArtifactId, InterfaceId, ToolId, WorkerEnvironmentKindId};
use octopus_interface_projection::InterfaceToolOwnerMap;
use octopus_ledger::InMemoryBlobs;
use octopus_policy::{
    ArtifactAccess, CapSet, DenySet, FormationPin, FormedPolicy, PolicyLayer,
    WorkerInterfacePolicy, form_policy,
};
use octopus_runtime::{CheckpointId, RunId, SystemInterfaceEmitter};

fn registry_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../registry")
}

async fn boot(run: &str, seed: u8) -> octopus_app_server::EmbeddedRuntime {
    let blobs: SharedBlobs = Arc::new(std::sync::Mutex::new(InMemoryBlobs::new()));
    let interface_artifact_root =
        std::env::temp_dir().join(format!("octopus-embedded-spine-artifacts-{}-{seed}", run));
    support::build_test_spine(SpineRootConfig {
        registry_root: registry_root(),
        run: RunId::new(run),
        blobs,
        interface_artifact_root: interface_artifact_root.clone(),
        run_records_root: interface_artifact_root.join("runs"),
        ee_instances_root: interface_artifact_root.join("ee_instances"),
    })
    .await
    .expect("the embedded spine must boot from the shared composition root")
}

fn worker_with_interface_surface(
    embedded: &EmbeddedRuntime,
) -> (&InterfaceId, &WorkerInterfacePolicy) {
    embedded
        .worker_interface_policies
        .iter()
        .find(|(_, policy)| !policy.ceiling.interfaces.is_empty())
        .or_else(|| embedded.worker_interface_policies.iter().next())
        .expect("registry has at least one worker System-interface pin")
}

/// The owned backing for an `AuthoredBindings` view: the checkpoint's exact
/// interface folders plus tool ownership and tool-to-WEK descriptor maps, which
/// form the authored-bindings leg of POLICY.md §5.
struct AuthoredOwned {
    interfaces: Vec<InterfaceId>,
    tool_ownership: InterfaceToolOwnerMap,
    tool_supported_weks: BTreeMap<ToolId, BTreeSet<WorkerEnvironmentKindId>>,
    artifact_access: BTreeMap<ArtifactId, ArtifactAccess>,
}

impl AuthoredOwned {
    fn for_doors(embedded: &EmbeddedRuntime, interfaces: Vec<InterfaceId>) -> Self {
        let tool_ownership =
            InterfaceToolOwnerMap::from_pairs(embedded.tool_descriptors_loaded.iter().map(
                |(_, descriptor)| {
                    (
                        ToolId::new(descriptor.id.as_str()),
                        descriptor.interface.clone(),
                    )
                },
            ));
        let tool_supported_weks = embedded
            .tool_descriptors_loaded
            .iter()
            .map(|(_, descriptor)| {
                (
                    ToolId::new(descriptor.id.as_str()),
                    descriptor
                        .supported_worker_environment_kinds
                        .iter()
                        .cloned()
                        .collect(),
                )
            })
            .collect();
        Self {
            interfaces,
            tool_ownership,
            tool_supported_weks,
            artifact_access: BTreeMap::new(),
        }
    }

    fn from_ceiling(embedded: &EmbeddedRuntime, policy: &WorkerInterfacePolicy) -> Self {
        Self::for_doors(embedded, policy.ceiling.interfaces.clone())
    }

    fn as_bindings(&self) -> AuthoredBindings<'_> {
        AuthoredBindings {
            interfaces: &self.interfaces,
            tool_ownership: &self.tool_ownership,
            tool_supported_weks: &self.tool_supported_weks,
            artifact_access: &self.artifact_access,
        }
    }
}

fn form_descriptor_worker_grant(embedded: &EmbeddedRuntime) -> FormedPolicy {
    let (worker, policy) = worker_with_interface_surface(embedded);
    let authored = AuthoredOwned::from_ceiling(embedded, policy);
    form_worker_run_policy(
        worker,
        policy,
        &embedded.ee_tree,
        &embedded.existence_ee_id,
        &embedded.formation_source_hashes,
        &authored.as_bindings(),
        &octopus_policy::CopyLineage::default(),
    )
    .expect("worker run policy forms from the interface pin and the loaded EE root")
}

#[tokio::test]
async fn worker_run_grant_is_narrowed_within_its_ceiling_when_formed() {
    let embedded = boot("run_review_subset", 13).await;
    let formed = form_descriptor_worker_grant(&embedded);
    assert!(
        formed.context.tools.is_subset(&formed.boundary.tools),
        "worker-run tools are narrowed from the authored ceiling"
    );
    assert!(
        formed
            .context
            .interfaces
            .is_subset(&formed.boundary.interfaces),
        "worker-run interfaces are narrowed from the authored ceiling"
    );
    assert!(
        formed
            .context
            .mark_outcomes
            .is_subset(&formed.boundary.mark_outcomes),
        "worker-run mark-outcomes are narrowed from the authored ceiling"
    );
    assert!(
        !formed.context.interfaces.is_empty(),
        "a worker-run grant that pins its whole ceiling keeps an interface surface"
    );
}

#[tokio::test]
async fn context_interfaces_are_exactly_the_pinned_doors_not_the_whole_ceiling() {
    let embedded = boot("run_pinned_doors", 16).await;
    let (worker, policy) = worker_with_interface_surface(&embedded);

    let full = AuthoredOwned::from_ceiling(&embedded, policy);
    let reach = form_worker_run_policy(
        worker,
        policy,
        &embedded.ee_tree,
        &embedded.existence_ee_id,
        &embedded.formation_source_hashes,
        &full.as_bindings(),
        &octopus_policy::CopyLineage::default(),
    )
    .expect("authoring the whole ceiling forms the ceiling ∩ EE reach")
    .context
    .interfaces;
    let one_door = reach
        .iter()
        .next()
        .cloned()
        .expect("the ceiling ∩ EE reach carries at least one door");

    let authored = AuthoredOwned::for_doors(&embedded, vec![one_door.clone()]);
    let formed = form_worker_run_policy(
        worker,
        policy,
        &embedded.ee_tree,
        &embedded.existence_ee_id,
        &embedded.formation_source_hashes,
        &authored.as_bindings(),
        &octopus_policy::CopyLineage::default(),
    )
    .expect("authoring a single pinned door forms");

    assert_eq!(
        formed
            .context
            .interfaces
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![one_door.clone()],
        "context interfaces are exactly the pinned door — the model never sees a ceiling interface it did not pin"
    );
    assert!(
        formed.boundary.interfaces.contains(&one_door),
        "the boundary keeps the full static ceiling (§3a) even as context narrows to the pinned surface (§3b)"
    );
    assert!(
        formed
            .context
            .tools
            .iter()
            .all(|tool| formed.boundary.tools.contains(tool)),
        "tools stay within the ceiling; the pinned door alone decides which survive"
    );
}

#[tokio::test]
async fn active_run_bindings_form_exact_role_dependency_closure_without_widening() {
    let embedded = boot("run_active_binding_union", 17).await;
    let run = RunId::new("run_active_binding_union");
    let current = CheckpointId::new("cp_current");
    let worker = InterfaceId::new("system_worker_coder_max");
    let target = InterfaceId::new("system_worker_web");
    let foreign = InterfaceId::new("system_worker_computer_use");
    let worker_policy = embedded.worker_interface_policies.get(&worker).unwrap();
    let target_policy = embedded.worker_interface_policies.get(&target).unwrap();
    let foreign_policy = embedded.worker_interface_policies.get(&foreign).unwrap();
    assert!(worker_policy.ceiling.interfaces.contains(&target));
    assert!(worker_policy.ceiling.interfaces.contains(&foreign));
    let target_only = ArtifactId::new("artifact-worker-role-web");
    let foreign_only = ArtifactId::new("artifact-worker-role-computer-use");
    assert!(target_policy.role_artifacts.contains(&target_only));
    assert!(foreign_policy.role_artifacts.contains(&foreign_only));
    let mut expected_role_artifacts = worker_policy.role_artifacts.clone();
    expected_role_artifacts.push(target_only.clone());
    let readable_role_bindings = expected_role_artifacts
        .iter()
        .map(|id| serde_json::json!({"id": id, "access": {"read": true}}))
        .collect::<Vec<_>>();
    let mut record = serde_json::json!({
        "id": run,
        "ee": embedded.existence_ee_id,
        "worker": worker,
        "trace": format!("trace:{}", run.as_str()),
        "status": "running",
        "interface_state": {},
        "checkpoints": [
            {
                "id": current,
                "run": run,
                "trace": format!("trace:{}:cp_current", run.as_str()),
                "epoch": 0,
                "interface_binding": support::all_binding("world_terminal"),
                "outbound_interface_bindings": [support::all_binding("world_image_generation")],
                "artifact_bindings": readable_role_bindings,
                "required_verified_artifacts": []
            },
            {
                "id": "cp_context",
                "run": run,
                "trace": format!("trace:{}:cp_context", run.as_str()),
                "epoch": 0,
                "interface_binding": support::all_binding("system_context"),
                "outbound_interface_bindings": [support::all_binding("world_image_generation")],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            },
            {
                "id": "cp_room",
                "run": run,
                "trace": format!("trace:{}:cp_room", run.as_str()),
                "epoch": 0,
                "interface_binding": support::all_binding("system_room"),
                "outbound_interface_bindings": [support::all_binding("world_image_generation")],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            },
            {
                "id": "cp_worker",
                "run": run,
                "trace": format!("trace:{}:cp_worker", run.as_str()),
                "epoch": 0,
                "interface_binding": support::all_binding(target.as_str()),
                "outbound_interface_bindings": [support::all_binding("world_image_generation")],
                "artifact_bindings": [],
                "required_verified_artifacts": []
            }
        ],
        "active_checkpoints": [current, "cp_context", "cp_room", "cp_worker"]
    });
    std::fs::create_dir_all(&embedded.run_records_root).unwrap();
    std::fs::write(
        embedded
            .run_records_root
            .join(format!("{}.json", run.as_str())),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();

    let authority = embedded
        .runtime
        .resolve_system_interface_emission_grant(
            SystemInterfaceEmitter {
                run: &run,
                checkpoint: &current,
                epoch: octopus_run_record::GrantEpoch(0),
            },
            &InterfaceId::new("world_terminal"),
            &target,
            "test.artifact",
        )
        .unwrap();
    assert!(authority.grant.context.interfaces.contains(&target));
    assert!(
        authority
            .grant
            .context
            .tools
            .contains(&ToolId::new("artifact.send"))
    );
    assert_eq!(
        record["checkpoints"][0]["outbound_interface_bindings"],
        serde_json::json!([support::all_binding("world_image_generation")])
    );
    assert_eq!(
        authority.metadata_projection.role_artifacts,
        expected_role_artifacts
            .iter()
            .map(|artifact| artifact.as_str().to_string())
            .collect::<Vec<_>>()
    );
    for artifact in &expected_role_artifacts {
        let source = octopus_app_server::worker_policy::artifact_descriptor_source_name(artifact);
        assert!(authority.grant.pin.inputs.contains_key(&source));
    }
    assert!(
        !authority
            .metadata_projection
            .role_artifacts
            .contains(&foreign_only.as_str().to_string())
    );
    assert!(!authority.grant.pin.inputs.contains_key(
        &octopus_app_server::worker_policy::artifact_descriptor_source_name(&foreign_only)
    ));
    let borrowed = embedded
        .runtime
        .resolve_system_interface_emission_grant(
            SystemInterfaceEmitter {
                run: &run,
                checkpoint: &current,
                epoch: octopus_run_record::GrantEpoch(0),
            },
            &InterfaceId::new("system_context"),
            &target,
            "test.artifact",
        )
        .unwrap_err();
    assert!(
        borrowed
            .to_string()
            .contains("refusing borrowed emitter authority")
    );

    record["checkpoints"][0]["artifact_bindings"]
        .as_array_mut()
        .unwrap()
        .retain(|binding| binding["id"] != target_only.as_str());
    std::fs::write(
        embedded
            .run_records_root
            .join(format!("{}.json", run.as_str())),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    let missing = embedded
        .runtime
        .resolve_system_interface_emission_grant(
            SystemInterfaceEmitter {
                run: &run,
                checkpoint: &current,
                epoch: octopus_run_record::GrantEpoch(0),
            },
            &InterfaceId::new("world_terminal"),
            &target,
            "test.artifact",
        )
        .unwrap_err()
        .to_string();
    assert!(missing.contains(&format!(
        "mandatory role artifact dependency '{}' required by worker interface '{}' is not explicitly bound with read=true",
        target_only.as_str(),
        target.as_str()
    )));
}

#[tokio::test]
async fn a_non_narrowing_ee_layer_does_not_clamp_the_worker_pin() {
    let embedded = boot("run_review_self_lift", 15).await;
    let ghost = InterfaceId::new("system_worker_sovereign");
    let policy = embedded
        .worker_interface_policies
        .get(&ghost)
        .expect("the sovereign worker pin is on disk");
    let operator_ee = embedded
        .ee_tree
        .instances()
        .map(|(id, _)| id.clone())
        .find(|id| id.as_str() == "ee_operator_interaction")
        .expect("the operator EE boot-opens");
    let authored = AuthoredOwned::for_doors(&embedded, Vec::new());
    let formed = form_worker_run_policy(
        &ghost,
        policy,
        &embedded.ee_tree,
        &operator_ee,
        &embedded.formation_source_hashes,
        &authored.as_bindings(),
        &octopus_policy::CopyLineage::default(),
    )
    .expect("operator_interaction has a descriptor-authored non-narrowing interface layer");
    assert!(
        formed.boundary.interfaces.contains(&ghost),
        "the worker pin remains bounded by its own descriptor ceiling"
    );
}

#[tokio::test]
async fn a_policy_layer_over_the_worker_fold_can_only_narrow_the_turn() {
    let embedded = boot("run_review_narrow", 14).await;
    let base = form_descriptor_worker_grant(&embedded);
    let narrowed_tool = ToolId::new("artifact.read");
    assert!(
        !base.context.interfaces.is_empty(),
        "base worker form keeps an interface surface"
    );
    assert!(
        !base.boundary.hard_blocks_tools.contains(&narrowed_tool),
        "base worker form hard-blocks nothing"
    );

    let narrowing = PolicyLayer {
        tools: None,
        interfaces: None,
        wek: None,
        mark_outcomes: None,
        hard_blocks_tools: DenySet::from_denied([narrowed_tool.clone()]),
        ..Default::default()
    };
    let reviewed = form_policy(
        base.boundary.clone(),
        &[narrowing],
        &[],
        FormationPin::default(),
    );
    assert!(
        reviewed.context.hard_blocks_tools.contains(&narrowed_tool),
        "a layer that hard-blocks a tool denies it in the folded turn"
    );
    assert!(
        reviewed.context.tools.is_subset(&base.boundary.tools),
        "the narrowed turn is bounded by the worker ceiling (C6)"
    );
    let widening_attempt = PolicyLayer {
        tools: Some(CapSet::from_allowed([ToolId::new(
            "a_tool_not_in_the_ceiling",
        )])),
        interfaces: None,
        wek: None,
        mark_outcomes: None,
        hard_blocks_tools: DenySet::default(),
        ..Default::default()
    };
    let attempted = form_policy(
        base.boundary.clone(),
        &[widening_attempt],
        &[],
        FormationPin::default(),
    );
    assert!(
        attempted.context.tools.is_subset(&base.boundary.tools),
        "no policy layer can widen a turn beyond the ceiling (C6)"
    );
}
