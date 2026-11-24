use serde_json::{json, Value};
use std::path::Path;

fn put(root: &Path, path: &str, value: Value) {
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn empty_authority() -> Value {
    json!({
        "sandbox_mode": "enforced",
        "read_root_slots": [],
        "write_root_slots": [],
        "executable_root_slots": [],
        "read_paths": [],
        "write_paths": [],
        "executable_paths": [],
        "executable_commands": [],
        "read_env_paths": [],
        "write_env_paths": [],
        "executable_env_paths": [],
        "allow_network": false,
        "allow_local_ipc": false
    })
}

fn tool_descriptor(id: &str, interface: &str, copy_of: Option<&str>, args_schema: Value) -> Value {
    let mut doc = json!({
        "id": id,
        "interface": interface,
        "process": {
            "bin": "bin/generic-tool",
            "args": [],
            "env": {},
            "authority": empty_authority()
        },
        "handler_config": null,
        "supported_worker_environment_kinds": ["restricted_room"],
        "description": "Generic test tool",
        "args_schema": args_schema,
        "is_world_effect": false
    });
    if let Some(origin) = copy_of {
        doc["copy_of"] = json!(origin);
    }
    doc
}

#[test]
fn copied_tool_retains_lineage_in_restricted_ee_after_non_widening_base_upgrade() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let registry = root.join("registry");
    let ees = root.join("ees");
    std::fs::create_dir(&ees).unwrap();
    for kind in [
        "interface",
        "artifact",
        "mark",
        "verifier",
        "tool",
        "worker_environment_kind",
        "ee_template",
    ] {
        std::fs::create_dir_all(registry.join(kind)).unwrap();
    }

    // Room kind & base + copied door interfaces.
    put(
        &registry,
        "worker_environment_kind/restricted_room/worker_environment_kind.json",
        json!({
            "worker_environment_kind_id": "restricted_room",
            "substrate_family": "generic_room",
            "door_driver_refs": ["generic_door"],
            "tool_process_bins": ["bin/generic-tool"],
            "worker_consent": "explicit_ceiling",
            "supported_executor_kinds": ["exec"],
            "capabilities": ["generic_cap"],
            "boundary_defaults": {
                "read_root_refs": [],
                "write_root_refs": [],
                "protected_read_only_subpaths": [],
                "network_ceiling": "denied"
            },
            "lifecycle": {
                "requires_release": false
            }
        }),
    );
    put(
        &registry,
        "interface/system_door/interface.json",
        json!({
            "id": "system_door",
            "interface_type": "system",
            "role": "system",
            "interface_driver_ref": "generic_door",
            "direction": "bidirectional",
            "source_names": ["tool.call", "tool.result"]
        }),
    );
    let door_life = json!({
        "processes": [{
            "bin": "/usr/bin/true",
            "args": [],
            "authority": empty_authority()
        }]
    });
    put(&registry, "interface/system_door/life.json", door_life.clone());
    put(
        &registry,
        "interface/system_door_run1/interface.json",
        json!({
            "id": "system_door_run1",
            "copy_of": "system_door",
            "interface_type": "system",
            "role": "system",
            "interface_driver_ref": "generic_door",
            "direction": "bidirectional",
            "source_names": ["tool.call", "tool.result"]
        }),
    );
    put(&registry, "interface/system_door_run1/life.json", door_life);
    put(
        &registry,
        "interface/world_input/interface.json",
        json!({
            "id": "world_input",
            "interface_type": "world",
            "role": "provider",
            "interface_driver_ref": "audit",
            "direction": "bidirectional",
            "source_names": ["input"]
        }),
    );
    put(
        &registry,
        "interface/system_worker_restricted/interface.json",
        json!({
            "id": "system_worker_restricted",
            "interface_type": "system",
            "role": "worker",
            "direction": "bidirectional",
            "source_names": ["input"]
        }),
    );
    put(
        &registry,
        "interface/system_worker_restricted/policy.json",
        json!({
            "id": "system_worker_restricted",
            "ceiling": {
                "interfaces": ["system_worker_restricted", "world_input", "system_door"],
                "tools": ["generic.publish"],
                "wek": ["restricted_room"],
                "mark_outcomes": [],
                "artifact_verbs": []
            },
            "role_artifacts": []
        }),
    );

    // Base tool descriptor upgraded with a new optional property on a closed object schema;
    // existing per-run copy keeps the pre-upgrade narrower schema.
    let pre_upgrade_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["entry"],
        "properties": {
            "entry": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id"],
                "properties": {
                    "id": { "type": "string", "minLength": 1 }
                }
            }
        }
    });
    let upgraded_base_schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["entry"],
        "properties": {
            "entry": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id"],
                "properties": {
                    "id": { "type": "string", "minLength": 1 },
                    "allowed_operations": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1
                    }
                }
            }
        }
    });

    put(
        &registry,
        "tool/generic.publish/tool.json",
        tool_descriptor("generic.publish", "system_door", None, upgraded_base_schema.clone()),
    );
    put(
        &registry,
        "tool/generic.publish__run1/tool.json",
        tool_descriptor(
            "generic.publish__run1",
            "system_door_run1",
            Some("generic.publish"),
            pre_upgrade_schema,
        ),
    );

    // Restricted EE template declaring explicit `allowed_interfaces` (which triggers
    // `WorkerInterfacePolicy::narrowed_by` and requires valid `CopyLineage::tools`).
    put(
        &registry,
        "ee_template/existence/ee_template.json",
        json!({
            "id": "existence",
            "goal_class": "existence",
            "bootstrap_root": true,
            "allowed_interfaces": ["system_worker_restricted", "world_input", "system_door"],
            "allowed_worker_environment_kinds": ["restricted_room"],
            "allowed_child_ee_templates": ["restricted_core"],
            "allowed_root_slots": []
        }),
    );
    put(
        &registry,
        "ee_template/restricted_core/ee_template.json",
        json!({
            "id": "restricted_core",
            "goal_class": "self_improvement",
            "bootstrap_root": false,
            "allowed_interfaces": ["system_worker_restricted", "world_input", "system_door"],
            "allowed_worker_environment_kinds": ["restricted_room"],
            "allowed_child_ee_templates": [],
            "allowed_root_slots": []
        }),
    );
    put(
        &ees,
        "ee_existence.json",
        json!({
            "ee_id": "ee_existence",
            "template_name": "existence",
            "parent_ee_id": null,
            "goal": "root"
        }),
    );
    put(
        &ees,
        "ee_restricted_core.json",
        json!({
            "ee_id": "ee_restricted_core",
            "template_name": "restricted_core",
            "parent_ee_id": "ee_existence",
            "goal": "restricted"
        }),
    );

    let record = serde_json::from_value(json!({
        "id": "run_restricted_1",
        "ee": "ee_restricted_core",
        "worker": "system_worker_restricted",
        "trace": "trace:run_restricted_1",
        "status": "idle",
        "interface_state": {},
        "checkpoints": [{
            "id": "cp_in",
            "run": "run_restricted_1",
            "epoch": 0,
            "interface_binding": {
                "interface": "world_input",
                "config": { "artifact_policy": { "mode": "all" } }
            },
            "outbound_interface_bindings": [{
                "interface": "system_door_run1",
                "config": { "artifact_policy": { "mode": "all" } }
            }],
            "artifact_bindings": [],
            "required_verified_artifacts": [],
            "trace": "trace:run_restricted_1:in"
        }],
        "active_checkpoints": ["cp_in"]
    }))
    .unwrap();

    // 1. Pre-upgrade copy whose schema narrows the upgraded base retains lineage and survives grant formation.
    let issues = octopus_app_server::registry_worker_policy_issues(
        &registry,
        &[octopus_descriptors::InterfaceId::new("system_worker_restricted")],
    )
    .unwrap();
    assert!(issues.is_empty(), "fixture worker policy issues: {issues:?}");
    let grant = octopus_app_server::spine_root::inspect_registry_run_grant(
        &registry,
        &ees,
        &record,
        &octopus_descriptors::CheckpointId::new("cp_in"),
    )
    .unwrap();
    assert!(
        grant.tools.iter().any(|tool| tool == "generic.publish__run1"),
        "expected narrowed copy_of tool to survive restricted EE grant formation, got: {:?}",
        grant.tools
    );

    // 2. Widened copy (adding an undeclared property disallowed by the base schema) loses lineage
    // and is stripped fail-closed from the formed grant.
    let mut widened_schema = upgraded_base_schema.clone();
    widened_schema["properties"]["entry"]["properties"]["unauthorized_extra"] =
        json!({ "type": "string" });
    put(
        &registry,
        "tool/generic.publish__run1/tool.json",
        tool_descriptor(
            "generic.publish__run1",
            "system_door_run1",
            Some("generic.publish"),
            widened_schema,
        ),
    );
    let widened_grant = octopus_app_server::spine_root::inspect_registry_run_grant(
        &registry,
        &ees,
        &record,
        &octopus_descriptors::CheckpointId::new("cp_in"),
    )
    .unwrap();
    assert!(
        !widened_grant
            .tools
            .iter()
            .any(|tool| tool == "generic.publish__run1"),
        "widened copy_of tool must be stripped fail-closed from restricted EE grant: {:?}",
        widened_grant.tools
    );

    // 3. Hard-blocking the base tool still blocks the narrowed copy via preserved lineage.
    put(
        &registry,
        "tool/generic.publish__run1/tool.json",
        tool_descriptor(
            "generic.publish__run1",
            "system_door_run1",
            Some("generic.publish"),
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["entry"],
                "properties": {
                    "entry": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["id"],
                        "properties": {
                            "id": { "type": "string", "minLength": 1 }
                        }
                    }
                }
            }),
        ),
    );
    put(
        &registry,
        "ee_template/restricted_core/ee_template.json",
        json!({
            "id": "restricted_core",
            "goal_class": "self_improvement",
            "bootstrap_root": false,
            "allowed_interfaces": ["system_worker_restricted", "world_input", "system_door"],
            "allowed_worker_environment_kinds": ["restricted_room"],
            "hard_blocked_tools": ["generic.publish"],
            "allowed_child_ee_templates": [],
            "allowed_root_slots": []
        }),
    );
    let hard_blocked_grant = octopus_app_server::spine_root::inspect_registry_run_grant(
        &registry,
        &ees,
        &record,
        &octopus_descriptors::CheckpointId::new("cp_in"),
    )
    .unwrap();
    assert!(
        !hard_blocked_grant
            .tools
            .iter()
            .any(|tool| tool == "generic.publish__run1"),
        "hard-blocked base tool must block narrowed copy_of tool via lineage: {:?}",
        hard_blocked_grant.tools
    );
}
