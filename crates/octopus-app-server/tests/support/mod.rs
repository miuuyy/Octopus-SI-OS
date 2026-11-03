#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use octopus_app_server::{
    EmbeddedRuntime, InterfaceArtifactFile, SharedBlobs, SpineRootConfig, build_embedded_spine,
};
use octopus_descriptors::{
    Descriptor, DescriptorHash, DescriptorName, EeId, InterfaceId, RunId, loader,
};
use octopus_ee::{EeInstance, EeTemplateDescriptor, EeTree};
use octopus_runtime::Runtime;

const TEST_INTERFACE_INPUT_LABEL: &str = "interface.input";
pub const OPERATOR_INPUT_LABEL: &str = "operator.input";

pub fn all_binding(interface: impl Into<String>) -> octopus_runtime::AuthoredInterfaceBinding {
    octopus_runtime::AuthoredInterfaceBinding::all(InterfaceId::new(interface))
}

pub fn worker_role_artifact_bindings(workers: &[&str]) -> Vec<serde_json::Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut bindings = Vec::new();
    for worker in workers {
        let policy_path = registry_root()
            .join("interface")
            .join(worker)
            .join("policy.json");
        let policy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&policy_path).expect("test worker policy reads"))
                .expect("test worker policy parses");
        assert_eq!(
            policy["id"], *worker,
            "test worker policy keeps exact identity"
        );
        for artifact in policy["role_artifacts"]
            .as_array()
            .expect("test worker policy role_artifacts is an array")
        {
            let artifact = artifact
                .as_str()
                .expect("test role artifact id is a string");
            if seen.insert(artifact.to_string()) {
                bindings.push(serde_json::json!({
                    "id": artifact,
                    "access": { "read": true }
                }));
            }
        }
    }
    bindings
}

pub fn write_terminal_run_record(root: &Path, runtime: &Runtime, run: &RunId) {
    write_terminal_run_record_at_checkpoint(root, runtime, run, "ckpt-0")
}

pub fn write_terminal_run_record_at_checkpoint(
    root: &Path,
    runtime: &Runtime,
    run: &RunId,
    checkpoint: &str,
) {
    let runs_dir = root.join("runs");
    runtime.set_run_records_dir(runs_dir.clone());
    write_literal_run_record_fixture(
        &runs_dir.join(format!("{}.json", run.as_str())),
        &serde_json::json!({
            "id": run,
            "ee": EeId::new("ee_existence"),
            "worker": InterfaceId::new("system_worker_coder_max"),
            "trace": format!("trace:{}", run.as_str()),
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": checkpoint,
                "run": run,
                "trace": format!("trace:{}:{checkpoint}", run.as_str()),
                "epoch": 0,
                "interface_binding": all_binding("world_operator_chat"),
                "outbound_interface_bindings": [all_binding("world_operator_chat")],
                "artifact_bindings": worker_role_artifact_bindings(&["system_worker_coder_max"]),
            }],
            "active_checkpoints": [checkpoint],
        }),
    )
    .expect("test terminal run JSON writes");
}

pub fn write_terminal_run_record_with_required_verified_artifacts(
    root: &Path,
    runtime: &Runtime,
    run: &RunId,
    required_verified_artifacts: serde_json::Value,
) {
    let runs_dir = root.join("runs");
    runtime.set_run_records_dir(runs_dir.clone());
    write_literal_run_record_fixture(
        &runs_dir.join(format!("{}.json", run.as_str())),
        &serde_json::json!({
            "id": run,
            "ee": EeId::new("ee_existence"),
            "worker": InterfaceId::new("system_worker_coder_max"),
            "trace": format!("trace:{}", run.as_str()),
            "status": "running",
            "interface_state": {},
            "checkpoints": [{
                "id": "ckpt-0",
                "run": run,
                "trace": format!("trace:{}:ckpt-0", run.as_str()),
                "epoch": 0,
                "interface_binding": all_binding("world_operator_chat"),
                "outbound_interface_bindings": [all_binding("world_operator_chat")],
                "artifact_bindings": worker_role_artifact_bindings(&["system_worker_coder_max"]),
                "required_verified_artifacts": required_verified_artifacts,
            }],
            "active_checkpoints": ["ckpt-0"],
        }),
    )
    .expect("test terminal run JSON with required verified artifacts writes");
}

fn write_literal_run_record_fixture(
    path: &Path,
    record: &serde_json::Value,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes =
        serde_json::to_vec_pretty(record).expect("test-authored run record JSON must encode");
    std::fs::write(path, bytes)
}

pub fn registry_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../registry")
}

pub fn descriptor_bootstrap_template<'a>(
    templates: &'a [(DescriptorName, EeTemplateDescriptor)],
) -> (DescriptorName, &'a EeTemplateDescriptor) {
    let mut matches = templates
        .iter()
        .filter(|(_, template)| template.bootstrap_root);
    let Some((name, template)) = matches.next() else {
        panic!("registry/ee_template must declare exactly one bootstrap_root=true root");
    };
    if let Some((_other_name, other_template)) = matches.next() {
        panic!(
            "registry/ee_template declares multiple bootstrap roots: '{}' and '{}'",
            template.id.as_str(),
            other_template.id.as_str()
        );
    }
    (name.clone(), template)
}

pub fn descriptor_bootstrap_ee_id(template: &EeTemplateDescriptor) -> EeId {
    EeId::new(format!("ee_{}", template.id.as_str()))
}

pub fn load_descriptor_bootstrap_ee_id() -> EeId {
    let templates = loader::load_kind::<EeTemplateDescriptor>(&registry_root().join("ee_template"))
        .expect("registry/ee_template must parse");
    let (_name, template) = descriptor_bootstrap_template(&templates);
    descriptor_bootstrap_ee_id(template)
}

pub fn load_descriptor_bootstrap_template_name() -> DescriptorName {
    let templates = loader::load_kind::<EeTemplateDescriptor>(&registry_root().join("ee_template"))
        .expect("registry/ee_template must parse");
    let (name, _template) = descriptor_bootstrap_template(&templates);
    name
}

pub async fn build_test_spine(
    config: SpineRootConfig,
) -> Result<EmbeddedRuntime, Box<dyn std::error::Error>> {
    let templates = loader::load_kind::<EeTemplateDescriptor>(
        &config.registry_root.join(EeTemplateDescriptor::KIND),
    )?;
    let (_name, template) = descriptor_bootstrap_template(&templates);
    std::fs::create_dir_all(&config.ee_instances_root)?;
    let instance = EeInstance {
        name: None,
        ee_id: descriptor_bootstrap_ee_id(template),
        template: template.id.clone(),
        parent_ee: None,
        goal: template.goal_class.clone(),
        config: Default::default(),
        policy_narrowing: Default::default(),
    };
    std::fs::write(
        config
            .ee_instances_root
            .join(format!("{}.json", instance.ee_id.as_str())),
        serde_json::to_vec_pretty(&instance)?,
    )?;
    let operator_template = templates
        .iter()
        .find(|(_, candidate)| candidate.id.as_str() == "operator_interaction")
        .map(|(_, candidate)| candidate)
        .ok_or("operator_interaction test EE template is missing")?;
    let operator = EeInstance {
        name: None,
        ee_id: EeId::new("ee_operator_interaction"),
        template: operator_template.id.clone(),
        parent_ee: Some(instance.ee_id.clone()),
        goal: operator_template.goal_class.clone(),
        config: Default::default(),
        policy_narrowing: Default::default(),
    };
    std::fs::write(
        config
            .ee_instances_root
            .join(format!("{}.json", operator.ee_id.as_str())),
        serde_json::to_vec_pretty(&operator)?,
    )?;
    build_embedded_spine(config).await
}

pub fn descriptor_bootstrap_tree(
    templates: &[(DescriptorName, EeTemplateDescriptor)],
) -> (EeTree, EeId, BTreeMap<DescriptorName, DescriptorHash>) {
    let mut tree = EeTree::new();
    for (_name, template) in templates {
        tree.insert_template(template.clone());
    }
    let (bootstrap_name, bootstrap_template) = descriptor_bootstrap_template(templates);
    let ee = descriptor_bootstrap_ee_id(bootstrap_template);
    tree.insert_instance(EeInstance {
        name: None,
        ee_id: ee.clone(),
        template: bootstrap_template.id.clone(),
        parent_ee: None,
        goal: bootstrap_template.goal_class.clone(),
        config: Default::default(),
        policy_narrowing: Default::default(),
    });

    let chain_hashes = [(
        bootstrap_name,
        DescriptorHash::of_canonical_json(&serde_json::to_value(bootstrap_template).unwrap()),
    )]
    .into_iter()
    .collect();
    (tree, ee, chain_hashes)
}

pub fn descriptor_bootstrap_tree_with_all_template_hashes(
    templates: &[(DescriptorName, EeTemplateDescriptor)],
) -> (EeTree, EeId, BTreeMap<DescriptorName, DescriptorHash>) {
    let mut tree = EeTree::new();
    let mut hashes = BTreeMap::new();
    for (name, template) in templates {
        tree.insert_template(template.clone());
        hashes.insert(
            name.clone(),
            DescriptorHash::of_canonical_json(&serde_json::to_value(template).unwrap()),
        );
    }
    let (_bootstrap_name, bootstrap_template) = descriptor_bootstrap_template(templates);
    let ee = descriptor_bootstrap_ee_id(bootstrap_template);
    tree.insert_instance(EeInstance {
        name: None,
        ee_id: ee.clone(),
        template: bootstrap_template.id.clone(),
        parent_ee: None,
        goal: bootstrap_template.goal_class.clone(),
        config: Default::default(),
        policy_narrowing: Default::default(),
    });

    (tree, ee, hashes)
}

pub fn descriptor_bootstrap_pin() -> octopus_runtime::FormationPin {
    let templates = loader::load_kind::<EeTemplateDescriptor>(&registry_root().join("ee_template"))
        .expect("registry/ee_template must parse");
    let (bootstrap_name, bootstrap_template) = descriptor_bootstrap_template(&templates);
    let mut inputs = BTreeMap::new();
    inputs.insert(
        bootstrap_name,
        DescriptorHash::of_canonical_json(&serde_json::to_value(bootstrap_template).unwrap()),
    );
    octopus_runtime::FormationPin { inputs }
}

pub fn interface_input_artifact(root: &Path, payload: &[u8]) -> InterfaceArtifactFile {
    InterfaceArtifactFile::write(root, OPERATOR_INPUT_LABEL, payload)
        .expect("test interface artifact file writes")
}

pub fn interface_arrival_submit_payload(root: &Path, request: &serde_json::Value) -> Vec<u8> {
    write_test_input_artifact_reference(
        root.join("interface-request-artifacts"),
        TEST_INTERFACE_INPUT_LABEL,
        request,
    )
}

pub fn interface_arrival_submit_artifact(
    root: &Path,
    blobs: &SharedBlobs,
    request: &serde_json::Value,
) -> InterfaceArtifactFile {
    let _ = blobs;
    let payload = interface_arrival_submit_payload(root, request);
    interface_input_artifact(root, &payload)
}

pub fn materialized_interface_arrival_submit_artifact(
    root: &Path,
    blobs: &SharedBlobs,
    request: &serde_json::Value,
) -> InterfaceArtifactFile {
    let _ = blobs;
    let manifest = write_test_input_artifact_reference(
        root.join("interface-request-artifacts"),
        OPERATOR_INPUT_LABEL,
        request,
    );
    InterfaceArtifactFile::store_manifest_bounded(
        root.join("materialized-arrivals"),
        &manifest,
        1024 * 1024,
        64 * 1024 * 1024,
    )
    .expect("test artifact-turn input artifact materializes")
}

fn write_test_input_artifact_reference(
    root: PathBuf,
    label: &str,
    body: &serde_json::Value,
) -> Vec<u8> {
    let bytes = serde_json::to_vec(body).expect("test artifact-turn input body encodes");
    let artifact = InterfaceArtifactFile::write(root, label, &bytes)
        .expect("test artifact-turn input artifact reference writes");
    std::fs::read(artifact.manifest_path())
        .expect("test artifact-turn input artifact reference reads")
}
