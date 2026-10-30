use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use crate::block_on::block_on;
use crate::{
    AbsPath, EnvContext, FileSurface, NetworkCeiling, ProjectedSandbox, WekDescriptorHash,
    WorkerEnvCaps, WorkerEnvDriver, WorkerEnvError, WorkerEnvKindRegistry, WorkerEnvKindSpec,
    WorkerEnvironment, WorkerEnvironmentFormation, WorkerEnvironmentFormationRequest,
    WorkerEnvironmentId, WorkerEnvironmentKindId, WorkerEnvironmentLifecycle,
    WorkerEnvironmentParts, WorkerEnvironmentRecordStore,
};

fn worktree_wek_json() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "worker_environment_kind_id": "local.coder.worktree",
        "substrate_family": "process_exec",
        "door_driver_refs": ["system_coder_env"],
        "tool_process_bins": ["target/debug/octopus-coder-tool"],
        "worker_consent": "explicit_ceiling",
        "supported_executor_kinds": ["exec"],
        "capabilities": ["filesystem", "process_exec", "git"],
        "boundary_defaults": {
            "read_root_refs": [{ "path": "repo_root", "access": ["read"] }],
            "write_root_refs": [{ "path": "coder.worktree_path", "access": ["write"] }],
            "protected_read_only_subpaths": [{ "path": "coder.worktree_git" }],
            "network_ceiling": "denied"
        },
        "lifecycle": { "requires_release": true }
    }))
    .unwrap()
}

fn registry() -> WorkerEnvKindRegistry {
    let mut reg = WorkerEnvKindRegistry::default();
    reg.upsert(&worktree_wek_json()).unwrap();
    reg
}

struct RefusingFs;

impl FileSurface for RefusingFs {
    fn read(
        &self,
        path: &AbsPath,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<u8>, WorkerEnvError>> + Send + '_>,
    > {
        let path = path.as_str().to_string();
        Box::pin(async move { Err(WorkerEnvError::ReadOutsideRoots(path)) })
    }
    fn write(
        &self,
        path: &AbsPath,
        _bytes: &[u8],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<crate::FileWriteOutput, WorkerEnvError>>
                + Send
                + '_,
        >,
    > {
        let path = path.as_str().to_string();
        Box::pin(async move { Err(WorkerEnvError::WriteOutsideRoots(path)) })
    }
    fn list(
        &self,
        dir: &AbsPath,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<AbsPath>, WorkerEnvError>> + Send + '_>,
    > {
        let dir = dir.as_str().to_string();
        Box::pin(async move { Err(WorkerEnvError::ReadOutsideRoots(dir)) })
    }
    fn apply_patch(
        &self,
        _patch: &str,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<crate::PatchOutcome, WorkerEnvError>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move { Err(WorkerEnvError::PatchFailed("no file surface".into())) })
    }
}

struct RecordingDriver {
    family: &'static str,
    seen_caps: std::sync::Mutex<Option<WorkerEnvCaps>>,
    seen_root_bindings: std::sync::Mutex<Option<BTreeMap<String, std::path::PathBuf>>>,
}

impl WorkerEnvDriver for RecordingDriver {
    fn substrate_family(&self) -> &str {
        self.family
    }
    fn open(
        &self,
        spec: &WorkerEnvKindSpec,
        descriptor_hash: WekDescriptorHash,
        caps: WorkerEnvCaps,
        ctx: EnvContext,
        id: WorkerEnvironmentId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<WorkerEnvironment, WorkerEnvError>> + Send + '_,
        >,
    > {
        *self.seen_caps.lock().unwrap() = Some(caps);
        *self.seen_root_bindings.lock().unwrap() = Some(ctx.provided_root_bindings());
        let spec = spec.clone();
        Box::pin(async move {
            let cwd = ctx.resolve_root("repo_root")?;
            WorkerEnvironment::assemble(
                &spec,
                descriptor_hash,
                id,
                WorkerEnvironmentParts {
                    sandbox: ProjectedSandbox::from_parts(
                        vec![],
                        vec![],
                        vec![],
                        NetworkCeiling::Denied,
                    ),
                    cwd,
                    fs: Arc::new(RefusingFs),
                    release: Some(Box::new(|| Box::pin(async { Ok(()) }))),
                },
            )
        })
    }
}

fn ctx_at(root: &str) -> EnvContext {
    let root = AbsPath::new(Path::new(root)).unwrap();
    EnvContext {
        additional_root_slots: Default::default(),
        root_slots: BTreeMap::from([("repo_root".to_string(), root)]),
        env: BTreeMap::new(),
    }
}

fn ctx_with_door_slots(root: &str, door_slots: &[&str]) -> EnvContext {
    let root_path = AbsPath::new(Path::new(root)).unwrap();
    let mut root_slots = BTreeMap::from([("repo_root".to_string(), root_path)]);
    for (index, slot) in door_slots.iter().enumerate() {
        root_slots.insert(
            (*slot).to_string(),
            AbsPath::new(Path::new(&format!("{root}/door-{index}"))).unwrap(),
        );
    }
    EnvContext {
        additional_root_slots: Default::default(),
        root_slots,
        env: BTreeMap::new(),
    }
}

#[test]
fn abs_path_lexical_boundary_holds() {
    let root = AbsPath::new(Path::new("/srv/work")).unwrap();
    let inside = AbsPath::new(Path::new("/srv/work/sub/file.txt")).unwrap();
    assert!(inside.is_within(&root));

    let sibling = AbsPath::new(Path::new("/srv/workshop/x")).unwrap();
    assert!(
        !sibling.is_within(&root),
        "sibling-prefix path rejected (the starts_with trap)"
    );

    let escaped = AbsPath::new(Path::new("/srv/work/../../etc/passwd")).unwrap();
    assert_eq!(escaped.as_str(), "/etc/passwd");
    assert!(!escaped.is_within(&root));
    assert!(AbsPath::new(Path::new("/../..")).is_err());
    assert!(AbsPath::new(Path::new("relative")).is_err());
}

#[test]
fn projected_sandbox_authorize_is_the_enforcement_contract() {
    let wt = AbsPath::new(Path::new("/srv/wt")).unwrap();
    let dot_git = AbsPath::new(Path::new("/srv/wt/.git")).unwrap();
    let repo = AbsPath::new(Path::new("/srv/repo")).unwrap();
    let sandbox = ProjectedSandbox::from_parts(
        vec![wt.clone()],
        vec![dot_git.clone()],
        vec![repo.clone()],
        NetworkCeiling::Denied,
    );

    sandbox
        .authorize_write(&AbsPath::new(Path::new("/srv/wt/a.txt")).unwrap())
        .unwrap();
    assert!(matches!(
        sandbox.authorize_write(&AbsPath::new(Path::new("/srv/wt/.git/config")).unwrap()),
        Err(WorkerEnvError::WriteOutsideRoots(_))
    ));
    assert!(matches!(
        sandbox.authorize_write(&AbsPath::new(Path::new("/srv/repo/x")).unwrap()),
        Err(WorkerEnvError::WriteOutsideRoots(_))
    ));
    sandbox
        .authorize_read(&AbsPath::new(Path::new("/srv/repo/x")).unwrap())
        .unwrap();
    assert!(matches!(
        sandbox.authorize_read(&AbsPath::new(Path::new("/etc/passwd")).unwrap()),
        Err(WorkerEnvError::ReadOutsideRoots(_))
    ));
}

#[test]
fn caps_derive_from_kind_boundary_and_only_narrow() {
    let reg = registry();
    let (spec, _) = reg
        .get(&WorkerEnvironmentKindId::from("local.coder.worktree"))
        .unwrap();

    let caps = WorkerEnvCaps::from_kind_boundary(&spec);
    assert!(caps.admits_writable("coder.worktree_path"));
    assert!(
        !caps.admits_writable("repo_root"),
        "a read-only boundary root is not writable in the derived caps"
    );
    assert!(caps.admits_readable("repo_root"));
    assert!(
        !caps.admits_readable("/etc"),
        "kind-boundary caps admit nothing outside the declared boundary (no top() seed)"
    );
    assert_eq!(caps.network(), NetworkCeiling::Denied);

    let narrowed = caps.intersect_writable(&[]);
    assert!(!narrowed.admits_writable("coder.worktree_path"));
    let widened_attempt =
        WorkerEnvCaps::from_kind_boundary(&spec).intersect_network(NetworkCeiling::Full);
    assert_eq!(
        widened_attempt.network(),
        NetworkCeiling::Denied,
        "network intersect is min-fold; no widening"
    );
}

#[test]
fn unknown_descriptor_field_fails_closed_at_the_typed_boundary() {
    let mut bad = serde_json::from_slice::<serde_json::Value>(&worktree_wek_json()).unwrap();
    bad.as_object_mut()
        .unwrap()
        .insert("environment_id".into(), serde_json::json!("legacy"));
    let mut reg = WorkerEnvKindRegistry::default();
    assert!(reg.upsert(&serde_json::to_vec(&bad).unwrap()).is_err());
}

#[test]
fn schema_version_body_field_fails_to_parse() {
    let mut with_version =
        serde_json::from_slice::<serde_json::Value>(&worktree_wek_json()).unwrap();
    with_version
        .as_object_mut()
        .unwrap()
        .insert("schema_version".into(), serde_json::json!(7));
    let mut reg = WorkerEnvKindRegistry::default();
    assert!(
        reg.upsert(&serde_json::to_vec(&with_version).unwrap())
            .is_err(),
        "a WEK descriptor carrying a body `schema_version` must FAIL to parse (version = trait const)"
    );
}

#[test]
fn retired_descriptor_axes_fail_closed() {
    for (field, value) in [
        ("kind", serde_json::json!("git_worktree")),
        ("door", serde_json::json!("system_coder_env")),
        ("substrate_locality", serde_json::json!("local")),
        (
            "instance_root_slots",
            serde_json::json!(["coder.worktree_path"]),
        ),
        (
            "transport",
            serde_json::json!({ "transport_ref": "git.worktree" }),
        ),
        ("doors", serde_json::json!(["system_coder_env"])),
        ("tools", serde_json::json!(["coder.exec"])),
        (
            "supported_workers",
            serde_json::json!(["system_worker_coder"]),
        ),
    ] {
        let mut stale = serde_json::from_slice::<serde_json::Value>(&worktree_wek_json()).unwrap();
        stale.as_object_mut().unwrap().insert(field.into(), value);
        let mut reg = WorkerEnvKindRegistry::default();
        assert!(
            reg.upsert(&serde_json::to_vec(&stale).unwrap()).is_err(),
            "a descriptor carrying the retired `{field}` axis must FAIL closed (deny_unknown_fields)"
        );
    }
}

#[test]
fn exact_door_and_worker_are_formed_from_the_concrete_grant_not_the_shared_kind() {
    let mut registry = WorkerEnvKindRegistry::default();
    registry.upsert(&worktree_wek_json()).unwrap();
    let kind = WorkerEnvironmentKindId::from("local.coder.worktree");
    let kinds = vec![kind];
    let tools = vec![
        "coder.exec".to_string(),
        "coder.apply_patch".to_string(),
        "coder.write_file".to_string(),
    ];
    let tool_hashes = BTreeMap::from([
        ("coder.exec".to_string(), "hash-exec".to_string()),
        ("coder.apply_patch".to_string(), "hash-patch".to_string()),
        ("coder.write_file".to_string(), "hash-write".to_string()),
    ]);
    let tool_process_bins = tools
        .iter()
        .map(|tool| (tool.clone(), "target/debug/octopus-coder-tool".to_string()))
        .collect::<BTreeMap<_, _>>();
    let context = ctx_with_door_slots("/srv/private", &["coder.worktree_path"]);
    let root_bindings = context.provided_root_bindings();
    let form = |door: &octopus_descriptors::InterfaceId,
                worker: &octopus_descriptors::InterfaceId| {
        registry.form(&WorkerEnvironmentFormationRequest {
            door_additional_root_bindings: &Default::default(),
            door_interface: door,
            door_descriptor_hash: "hash-door",
            door_driver_ref: "system_coder_env",
            tool: "coder.exec",
            tool_descriptor_hashes: &tool_hashes,
            tool_process_bins: &tool_process_bins,
            tool_worker_environment_kinds: &kinds,
            worker: &worker,
            ee: "ee_x",
            run: "run_y",
            granted_worker_environment_kinds: &kinds,
            granted_tools: &tools,
            door_root_bindings: &root_bindings,
            door_instance_dir: Path::new("."),
            ee_root_slot_allowance: None,
        })
    };
    for (door, worker) in [
        ("system_coder_env", "system_worker_coder"),
        ("system_coder_env_run_42", "system_worker_coder_run_42"),
    ] {
        let formed = form(
            &octopus_descriptors::InterfaceId::new(door),
            &octopus_descriptors::InterfaceId::new(worker),
        )
        .unwrap();
        assert_eq!(formed.door_interface.as_str(), door);
        assert_eq!(formed.worker.as_str(), worker);
    }
}

#[test]
fn lease_refuses_a_driver_of_another_substrate_family() {
    let reg = registry();
    let ctx = ctx_with_door_slots("/srv/room", &["coder.worktree_path"]);
    let formation = worktree_formation(&reg, &ctx, None, "coder.exec");
    let (spec, _) = reg
        .get(&WorkerEnvironmentKindId::from("local.coder.worktree"))
        .unwrap();
    let driver = Arc::new(RecordingDriver {
        family: "browser",
        seen_caps: std::sync::Mutex::new(None),
        seen_root_bindings: std::sync::Mutex::new(None),
    });
    assert!(matches!(
        block_on(crate::lease(&spec, &formation, driver, ctx, None)),
        Err(WorkerEnvError::UnknownSubstrateFamily(_))
    ));
}

#[test]
fn repository_worker_environment_kind_descriptors_load_as_exact_registry_entries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/worker_environment_kind");
    let reg = WorkerEnvKindRegistry::load_from_dir(&root).unwrap();

    let (browser, _) = reg
        .get(&WorkerEnvironmentKindId::from("browser_session"))
        .unwrap();
    assert_eq!(browser.substrate_family, "browser");
    let ids = reg
        .head_specs()
        .map(|spec| spec.worker_environment_kind_id.as_str().to_string())
        .collect::<BTreeSet<_>>();
    assert!(ids.contains("browser_session"));
    assert!(ids.contains("remote_exec_server"));
    assert!(ids.contains("system_room"));
}

#[test]
fn assemble_fails_closed_when_kind_requires_release_and_driver_has_no_hook() {
    let reg = registry();
    let (spec, hash) = reg
        .get(&WorkerEnvironmentKindId::from("local.coder.worktree"))
        .unwrap();
    let id = WorkerEnvironmentId::derive(b"release-contract");
    let refused = WorkerEnvironment::assemble(
        &spec,
        hash,
        id,
        WorkerEnvironmentParts {
            sandbox: ProjectedSandbox::from_parts(vec![], vec![], vec![], NetworkCeiling::Denied),
            cwd: AbsPath::new(Path::new("/srv")).unwrap(),
            fs: Arc::new(RefusingFs),
            release: None,
        },
    );
    assert!(
        matches!(refused, Err(WorkerEnvError::OpenFailed { .. })),
        "requires_release without a release hook is a lying room; assembly fails closed"
    );
}

fn lease_recording_caps(
    reg: &WorkerEnvKindRegistry,
    driver: &Arc<RecordingDriver>,
    ctx: EnvContext,
    ee_root_allowance: Option<&std::collections::BTreeSet<String>>,
) -> WorkerEnvCaps {
    let formation = worktree_formation(reg, &ctx, ee_root_allowance, "coder.exec");
    let (spec, _) = reg
        .get(&WorkerEnvironmentKindId::from("local.coder.worktree"))
        .unwrap();
    let env = block_on(crate::lease(
        &spec,
        &formation,
        driver.clone(),
        ctx,
        ee_root_allowance,
    ))
    .unwrap();
    assert_eq!(
        env.kind_id(),
        &WorkerEnvironmentKindId::from("local.coder.worktree")
    );
    let caps = driver.seen_caps.lock().unwrap().clone().unwrap();
    block_on(env.release()).unwrap();
    caps
}

fn worktree_formation(
    reg: &WorkerEnvKindRegistry,
    ctx: &EnvContext,
    ee_root_allowance: Option<&BTreeSet<String>>,
    tool: &str,
) -> WorkerEnvironmentFormation {
    let door = octopus_descriptors::InterfaceId::new("system_coder_env");
    let worker = octopus_descriptors::InterfaceId::new("system_worker_coder");
    let kind = WorkerEnvironmentKindId::from("local.coder.worktree");
    let kinds = vec![kind.clone()];
    let tools = vec![
        "coder.exec".to_string(),
        "coder.apply_patch".to_string(),
        "coder.write_file".to_string(),
    ];
    let tool_hashes = BTreeMap::from([
        ("coder.exec".to_string(), "hash-exec".to_string()),
        ("coder.apply_patch".to_string(), "hash-patch".to_string()),
        ("coder.write_file".to_string(), "hash-write".to_string()),
    ]);
    let tool_process_bins = tools
        .iter()
        .map(|tool| (tool.clone(), "target/debug/octopus-coder-tool".to_string()))
        .collect::<BTreeMap<_, _>>();
    let door_root_bindings = ctx.provided_root_bindings();
    let additional = ctx.additional_root_slots.iter().map(|(slot, paths)|
        (slot.clone(), paths.iter().map(|path| path.as_path().to_path_buf()).collect())).collect();
    reg.form(&WorkerEnvironmentFormationRequest {
        door_additional_root_bindings: &additional,
        door_interface: &door,
        door_descriptor_hash: "hash-door",
        door_driver_ref: "system_coder_env",
        tool,
        tool_descriptor_hashes: &tool_hashes,
        tool_process_bins: &tool_process_bins,
        tool_worker_environment_kinds: &kinds,
        worker: &worker,
        ee: "ee_x",
        run: "run_y",
        granted_worker_environment_kinds: &kinds,
        granted_tools: &tools,
        door_root_bindings: &door_root_bindings,
        door_instance_dir: std::path::Path::new("."),
        ee_root_slot_allowance: ee_root_allowance,
    })
    .unwrap()
}

#[test]
fn formation_identity_is_room_wide_across_tools() {
    let reg = registry();
    let ctx = ctx_with_door_slots("/srv/room", &["coder.worktree_path"]);
    let exec = worktree_formation(&reg, &ctx, None, "coder.exec");
    let write = worktree_formation(&reg, &ctx, None, "coder.write_file");
    let moved = worktree_formation(
        &reg,
        &ctx_with_door_slots("/srv/other-room", &["coder.worktree_path"]),
        None,
        "coder.exec",
    );

    assert_eq!(exec.worker_environment_id, write.worker_environment_id);
    assert_eq!(exec.formation_pin, write.formation_pin);
    assert_eq!(exec.tool_descriptor_hashes.len(), 3);
    assert_eq!(
        exec.door_root_bindings.get("coder.worktree_path"),
        Some(&Path::new("/srv/room/door-0").to_path_buf())
    );
    assert_ne!(exec.worker_environment_id, moved.worker_environment_id);
    let mut expanded = ctx.clone();
    expanded.additional_root_slots.insert("coder.worktree_path".into(), vec![AbsPath::new(Path::new("/srv/second-project")).unwrap()]);
    let multiple = worktree_formation(&reg, &expanded, None, "coder.exec");
    multiple.validate().unwrap();
    assert_ne!(multiple.formation_pin, exec.formation_pin);
    let encoded = serde_json::to_value(&exec).unwrap();
    assert!(encoded.get("door_additional_root_bindings").is_none());
    let mut changed = multiple.clone();
    changed.door_additional_root_bindings.clear();
    assert!(changed.validate().is_err());

}

#[test]
fn declared_instance_config_is_canonical_and_part_of_formation_identity() {
    let mut descriptor: serde_json::Value = serde_json::from_slice(&worktree_wek_json()).unwrap();
    descriptor.as_object_mut().unwrap().insert(
        "instance_config_files".to_string(),
        serde_json::json!(["room.json"]),
    );
    let mut reg = WorkerEnvKindRegistry::default();
    reg.upsert(&serde_json::to_vec(&descriptor).unwrap())
        .unwrap();

    let room = tempfile::tempdir().unwrap();
    std::fs::write(
        room.path().join("room.json"),
        br#"{"mode":"one","nested":{"b":2,"a":1}}"#,
    )
    .unwrap();
    let ctx = ctx_with_door_slots("/srv/room", &["coder.worktree_path"]);
    let door = octopus_descriptors::InterfaceId::new("system_coder_env");
    let worker = octopus_descriptors::InterfaceId::new("system_worker_coder");
    let kind = WorkerEnvironmentKindId::from("local.coder.worktree");
    let kinds = vec![kind.clone()];
    let tools = vec![
        "coder.exec".to_string(),
        "coder.apply_patch".to_string(),
        "coder.write_file".to_string(),
    ];
    let tool_hashes = BTreeMap::from([
        ("coder.exec".to_string(), "hash-exec".to_string()),
        ("coder.apply_patch".to_string(), "hash-patch".to_string()),
        ("coder.write_file".to_string(), "hash-write".to_string()),
    ]);
    let tool_process_bins = tools
        .iter()
        .map(|tool| (tool.clone(), "target/debug/octopus-coder-tool".to_string()))
        .collect::<BTreeMap<_, _>>();
    let door_root_bindings = ctx.provided_root_bindings();
    let form = || {
        reg.form(&WorkerEnvironmentFormationRequest {
            door_additional_root_bindings: &Default::default(),
            door_interface: &door,
            door_descriptor_hash: "hash-door",
            door_driver_ref: "system_coder_env",
            tool: "coder.exec",
            tool_descriptor_hashes: &tool_hashes,
            tool_process_bins: &tool_process_bins,
            tool_worker_environment_kinds: &kinds,
            worker: &worker,
            ee: "ee_x",
            run: "run_y",
            granted_worker_environment_kinds: &kinds,
            granted_tools: &tools,
            door_root_bindings: &door_root_bindings,
            door_instance_dir: room.path(),
            ee_root_slot_allowance: None,
        })
    };

    let first = form().unwrap();
    std::fs::write(
        room.path().join("room.json"),
        br#"{"nested":{"a":1,"b":2},"mode":"one"}"#,
    )
    .unwrap();
    let reordered = form().unwrap();
    assert_eq!(first.worker_environment_id, reordered.worker_environment_id);
    assert_eq!(
        first.instance_config_hashes,
        reordered.instance_config_hashes
    );

    std::fs::write(
        room.path().join("room.json"),
        br#"{"mode":"two","nested":{"a":1,"b":2}}"#,
    )
    .unwrap();
    let changed = form().unwrap();
    assert_ne!(first.worker_environment_id, changed.worker_environment_id);
    assert_ne!(first.formation_pin, changed.formation_pin);

    std::fs::remove_file(room.path().join("room.json")).unwrap();
    assert!(matches!(form(), Err(WorkerEnvError::MalformedFormation(_))));
}

#[test]
fn generic_lease_forms_caps_from_kind_boundary_door_and_ee() {
    let reg = registry();
    let driver = Arc::new(RecordingDriver {
        family: "process_exec",
        seen_caps: std::sync::Mutex::new(None),
        seen_root_bindings: std::sync::Mutex::new(None),
    });

    // Door provides the worktree root the kind boundary writes; EE unconstrained.
    let caps = lease_recording_caps(
        &reg,
        &driver,
        ctx_with_door_slots("/srv/room", &["coder.worktree_path"]),
        None,
    );
    assert!(
        caps.admits_writable("coder.worktree_path"),
        "the door provides the boundary write root, so the formed caps keep it"
    );
    assert!(
        !caps.admits_writable("anything_else"),
        "lease forms kind-boundary caps, never top()"
    );

    // Door does NOT provide the worktree root: the door leg strips it.
    let caps = lease_recording_caps(&reg, &driver, ctx_at("/srv/room"), None);
    assert!(
        !caps.admits_writable("coder.worktree_path"),
        "a boundary root the door instance does not expose is stripped (the door leg)"
    );

    // Door provides it, but the EE chain allowance excludes it: the EE leg strips it.
    let ee_allowance: std::collections::BTreeSet<String> =
        ["repo_root".to_string()].into_iter().collect();
    let caps = lease_recording_caps(
        &reg,
        &driver,
        ctx_with_door_slots("/srv/room", &["coder.worktree_path"]),
        Some(&ee_allowance),
    );
    assert!(
        !caps.admits_writable("coder.worktree_path"),
        "the EE chain allowance does not permit the worktree root — the EE leg strips it"
    );
    assert!(
        caps.admits_readable("repo_root"),
        "repo_root survives both legs (door provides it, EE allows it)"
    );
}

#[test]
fn generic_lease_preserves_kind_owned_root_coordinates() {
    let reg = registry();
    let driver = Arc::new(RecordingDriver {
        family: "process_exec",
        seen_caps: std::sync::Mutex::new(None),
        seen_root_bindings: std::sync::Mutex::new(None),
    });
    let ctx = ctx_with_door_slots("/srv/room", &["coder.worktree_path"]);
    let formation = worktree_formation(&reg, &ctx, None, "coder.exec");
    let base = formation
        .door_root_bindings
        .get("coder.worktree_path")
        .unwrap()
        .clone();
    let (spec, _) = reg
        .get(&WorkerEnvironmentKindId::from("local.coder.worktree"))
        .unwrap();

    let env = block_on(crate::lease(&spec, &formation, driver.clone(), ctx, None)).unwrap();
    let observed = driver.seen_root_bindings.lock().unwrap().clone().unwrap();
    assert_eq!(observed.get("coder.worktree_path"), Some(&base));
    assert_eq!(
        formation.door_root_bindings.get("coder.worktree_path"),
        Some(&base)
    );
    block_on(env.release()).unwrap();
}

#[test]
fn lease_rejects_a_door_root_coordinate_changed_after_formation() {
    let reg = registry();
    let formed_ctx = ctx_with_door_slots("/srv/room", &["coder.worktree_path"]);
    let formation = worktree_formation(&reg, &formed_ctx, None, "coder.exec");
    let (spec, _) = reg
        .get(&WorkerEnvironmentKindId::from("local.coder.worktree"))
        .unwrap();
    let driver = Arc::new(RecordingDriver {
        family: "process_exec",
        seen_caps: std::sync::Mutex::new(None),
        seen_root_bindings: std::sync::Mutex::new(None),
    });
    let changed_ctx = ctx_with_door_slots("/srv/other", &["coder.worktree_path"]);
    let result = block_on(crate::lease(&spec, &formation, driver, changed_ctx, None));
    assert!(matches!(result, Err(WorkerEnvError::MalformedFormation(_))));
    let mut changed_ctx = formed_ctx.clone();
    changed_ctx.additional_root_slots.insert("coder.worktree_path".into(), vec![AbsPath::new(Path::new("/srv/unformed")).unwrap()]);
    let result = block_on(crate::lease(&spec, &formation, Arc::new(RecordingDriver {
        family: "process_exec", seen_caps: std::sync::Mutex::new(None), seen_root_bindings: std::sync::Mutex::new(None),
    }), changed_ctx, None));
    assert!(matches!(result, Err(WorkerEnvError::MalformedFormation(_))));
}

#[test]
fn formed_record_store_projects_validated_record_and_live_state() {
    let reg = registry();
    let ctx = ctx_with_door_slots("/srv/room", &["coder.worktree_path"]);
    let formation = worktree_formation(&reg, &ctx, None, "coder.exec");
    let temp = tempfile::tempdir().unwrap();
    let store = WorkerEnvironmentRecordStore::new(temp.path());

    store.activate(&formation, 7).unwrap();
    let before = std::fs::metadata(store.record_path(&formation.worker_environment_id)).unwrap();
    store.activate(&formation, 7).unwrap();
    assert_eq!(
        std::fs::metadata(store.record_path(&formation.worker_environment_id))
            .unwrap()
            .modified()
            .unwrap(),
        before.modified().unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(store.record_path(&formation.worker_environment_id))
                .unwrap()
                .ino(),
            before.ino()
        );
    }
    store
        .write_live_state(
            &formation.worker_environment_id,
            &serde_json::json!({"viewer": {"kind": "terminal", "address": "pty://room"}}),
        )
        .unwrap();
    let snapshots = store.snapshots().unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].record.formation, formation);
    assert_eq!(
        snapshots[0].live_state.as_ref().unwrap()["viewer"]["address"],
        "pty://room"
    );

    store.fail(&formation, 8).unwrap();
    let failed = store.read(&formation.worker_environment_id).unwrap();
    assert_eq!(failed.lifecycle, WorkerEnvironmentLifecycle::Failed);
    assert_eq!(failed.last_activity_movement_seq, 8);
    assert!(store
        .read_live_state(&formation.worker_environment_id)
        .unwrap()
        .is_none());
    store
        .write_live_state(
            &formation.worker_environment_id,
            &serde_json::json!({"status": "failed", "detail": "connection refused"}),
        )
        .unwrap();
    assert_eq!(
        store
            .read_live_state(&formation.worker_environment_id)
            .unwrap()
            .unwrap()["status"],
        "failed"
    );
    store.activate(&formation, 9).unwrap();
    assert!(store
        .read_live_state(&formation.worker_environment_id)
        .unwrap()
        .is_none());
    store
        .write_live_state(
            &formation.worker_environment_id,
            &serde_json::json!({"status": "ready"}),
        )
        .unwrap();
    store.release(&formation, 10).unwrap();
    assert_eq!(
        store
            .read(&formation.worker_environment_id)
            .unwrap()
            .lifecycle,
        WorkerEnvironmentLifecycle::Released
    );
    assert!(store
        .read_live_state(&formation.worker_environment_id)
        .unwrap()
        .is_none());
    assert!(store.activate(&formation, 11).is_err());
    assert!(store.fail(&formation, 11).is_err());
}

#[test]
fn concurrent_room_live_state_publications_remain_whole() {
    let reg = registry();
    let ctx = ctx_with_door_slots("/srv/room", &["coder.worktree_path"]);
    let formation = worktree_formation(&reg, &ctx, None, "coder.exec");
    let temp = tempfile::tempdir().unwrap();
    let store = WorkerEnvironmentRecordStore::new(temp.path());
    store.activate(&formation, 7).unwrap();
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        for writer in 0..8 {
            let store = &store;
            let id = &formation.worker_environment_id;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..16 {
                    store
                        .write_live_state(
                            id,
                            &serde_json::json!({"writer": writer, "payload": "x".repeat(8192)}),
                        )
                        .unwrap();
                }
            });
        }
    });
    let state = store
        .read_live_state(&formation.worker_environment_id)
        .unwrap()
        .unwrap();
    assert_eq!(state["payload"].as_str().unwrap().len(), 8192);
    assert!(state["writer"].as_u64().unwrap() < 8);
    let directory = store
        .record_path(&formation.worker_environment_id)
        .parent()
        .unwrap()
        .to_path_buf();
    assert_eq!(std::fs::read_dir(directory).unwrap().count(), 2);
}

#[test]
fn worker_environment_identity_cannot_escape_the_record_store() {
    assert!(serde_json::from_str::<WorkerEnvironmentId>(r#""../../escape""#).is_err());
    assert!(serde_json::from_str::<WorkerEnvironmentId>(r#""we_not_hex""#).is_err());
    let id = WorkerEnvironmentId::derive(b"stable");
    assert_eq!(
        serde_json::from_str::<WorkerEnvironmentId>(&serde_json::to_string(&id).unwrap()).unwrap(),
        id
    );
}
