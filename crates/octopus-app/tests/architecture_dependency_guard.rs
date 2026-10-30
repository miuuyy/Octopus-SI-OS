use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const HOT_PATH_PACKAGES: &[&str] = &["octopus-app", "octopus-app-server"];

const ENTRY_SURFACE_PACKAGES: &[&str] = &[
    "octopus-core",
    "octopus-provider-codex",
    "octopus-tui",
    "octopus-cli",
    "octopus-codex-cli",
];

const REGISTRY_INTERFACE_OWNER_PACKAGES: &[(&str, &str, &str)] = &[
    (
        "octopus-provider-codex",
        "registry/interface/world_provider_codex/packages/octopus-provider-codex",
        "registry/interface/world_provider_codex",
    ),
    (
        "octopus-app-server-interface",
        "registry/interface/world_operator_chat/packages/octopus-app-server-interface",
        "registry/interface/world_operator_chat",
    ),
    (
        "octopus-model-context-render",
        "registry/interface/world_provider_codex/packages/octopus-model-context-render",
        "registry/interface/world_provider_codex",
    ),
    (
        "octopus-run-authoring",
        "registry/interface/system_interface_authoring/packages/octopus-run-authoring",
        "registry/interface/system_interface_authoring",
    ),
];

const PROVIDER_IMPLEMENTATION_PACKAGES: &[&str] = &[
    "octopus-model-context-render",
    "octopus-run-authoring",
];

const FORBIDDEN_HOT_PATH_CRATES: &[&str] = &[
    "octopus-state",
    "octopus-hooks",
    "octopus-extension-api",
    "octopus-core",
    "octopus-thread-store",
    "octopus-agent-graph-store",
    "octopus-analytics",
];

const FORBIDDEN_RUNTIME_LEGACY_CRATES: &[&str] = &[
    "octopus-state",
    "octopus-hooks",
    "octopus-extension-api",
    "octopus-thread-store",
    "octopus-analytics",
];

const REMOVED_SECOND_TRUTH_PACKAGES: &[&str] = &[
    "octopus-bundled-packages",
    "octopus-state",
    "octopus-hooks",
    "octopus-extension-api",
    "octopus-thread-store",
    "octopus-agent-graph-store",
    "octopus-analytics",
];

const REMOVED_SHARED_EXECUTION_PACKAGES: &[&str] = &["octopus-process-authority"];

const RUNTIME_DESCRIPTOR_ONLY_PACKAGES: &[&str] = &["octopus-runtime", "octopus-app-server"];

const PROVIDER_BLIND_PRODUCT_ROOT_PACKAGES: &[&str] = &[
    "octopus-runtime",
    "octopus-app-server",
    "octopus-app",
    "octopus-tui",
];

const FORBIDDEN_RUNTIME_PRODUCT_INTERFACE_PACKAGES: &[&str] = &[
    "octopus-app-server-interface",
    "octopus-app-server-transport",
    "octopus-appserver-proto",
    "octopus-backend-client",
    "octopus-backend-openapi-models",
    "octopus-client",
    "octopus-code-mode-engine",
    "octopus-collab-templates",
    "octopus-cxhome",
    "octopus-interface-transport",
    "octopus-message-history",
    "octopus-tui",
    "octopus-chatgpt",
    "octopus-ollama",
    "octopus-responses-api-proxy",
    "octopus-core",
    "octopus-config",
    "octopus-login",
    "octopus-prompts",
    "octopus-protocol",
    "octopus-realtime-webrtc",
    "octopus-terminal-detection",
    "octopus-world-terminal-render",
    "octopus-world-terminal-client",
    "octopus-world-terminal-keymap",
    "octopus-world-terminal-config",
    "octopus-identity",
    "octopus-cxapi",
    "octopus-utils-oss",
    "octopus-utils-sandbox-summary",
    "octopus-codex-cli",
    "octopus-features",
    "octopus-otel",
    "octopus-install-context",
    "octopus-sandbox-launcher",
    "octopus-util-pty",
    "octopus-util-image",
    "octopus-ansi-escape",
    "octopus-utils-fuzzy-match",
    "octopus-utils-elapsed",
    "octopus-utils-sleep-inhibitor",
    "octopus-process-hardening",
    "octopus-async-utils",
];

const FORBIDDEN_RUNTIME_TOOL_REGISTRY_PACKAGES: &[&str] =
    &["octopus-tool-handlers", "octopus-tool-drivers"];

const FORBIDDEN_APP_PRODUCTION_INTERFACE_PACKAGES: &[&str] = &[
    "octopus-app-server-interface",
    "octopus-app-server-transport",
    "octopus-appserver-proto",
    "octopus-backend-client",
    "octopus-backend-openapi-models",
    "octopus-client",
    "octopus-code-mode-engine",
    "octopus-collab-templates",
    "octopus-cxhome",
    "octopus-message-history",
    "octopus-provider-codex",
    "octopus-tui",
    "octopus-chatgpt",
    "octopus-ollama",
    "octopus-responses-api-proxy",
    "octopus-core",
    "octopus-config",
    "octopus-login",
    "octopus-prompts",
    "octopus-protocol",
    "octopus-realtime-webrtc",
    "octopus-terminal-detection",
    "octopus-world-terminal-render",
    "octopus-world-terminal-client",
    "octopus-world-terminal-keymap",
    "octopus-world-terminal-config",
    "octopus-identity",
    "octopus-cxapi",
    "octopus-utils-oss",
    "octopus-utils-sandbox-summary",
    "octopus-codex-cli",
    "octopus-features",
    "octopus-otel",
    "octopus-install-context",
    "octopus-sandbox-launcher",
    "octopus-util-pty",
    "octopus-util-image",
    "octopus-ansi-escape",
    "octopus-utils-fuzzy-match",
    "octopus-utils-elapsed",
    "octopus-utils-sleep-inhibitor",
    "octopus-process-hardening",
    "octopus-async-utils",
];

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("octopus-app crate lives under crates/")
}

fn cargo_metadata(repo_root: &Path) -> serde_json::Value {
    let output = Command::new("cargo")
        .current_dir(repo_root)
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .output()
        .unwrap_or_else(|err| panic!("failed to run cargo metadata: {err}"));

    assert!(
        output.status.success(),
        "cargo metadata failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("cargo metadata JSON")
}

fn cargo_binary_targets(metadata: &serde_json::Value) -> BTreeSet<String> {
    let mut binaries = BTreeSet::new();
    for package in metadata["packages"]
        .as_array()
        .expect("cargo metadata packages")
    {
        let package_name = package["name"].as_str().expect("cargo package name");
        for target in package["targets"]
            .as_array()
            .expect("cargo metadata package targets")
        {
            let is_binary = target["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")));
            if !is_binary {
                continue;
            }
            let binary = target["name"]
                .as_str()
                .expect("cargo binary target name")
                .to_string();
            assert!(
                binaries.insert(binary.clone()),
                "workspace binary target '{binary}' is declared more than once; registry target/debug wiring cannot select a package deterministically (second package: {package_name})"
            );
        }
    }
    binaries
}

fn cargo_binary_owner_manifests(
    metadata: &serde_json::Value,
    repo_root: &Path,
) -> BTreeMap<String, PathBuf> {
    let mut owners = BTreeMap::new();
    for package in metadata["packages"]
        .as_array()
        .expect("cargo metadata packages")
    {
        let manifest_path = Path::new(
            package["manifest_path"]
                .as_str()
                .expect("cargo package manifest_path"),
        );
        let manifest_path = manifest_path
            .strip_prefix(repo_root)
            .unwrap_or(manifest_path)
            .to_path_buf();
        for target in package["targets"]
            .as_array()
            .expect("cargo metadata package targets")
        {
            if !target["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("bin")))
            {
                continue;
            }
            let binary = target["name"]
                .as_str()
                .expect("cargo binary target name")
                .to_string();
            assert!(
                owners
                    .insert(binary.clone(), manifest_path.clone())
                    .is_none(),
                "workspace binary target '{binary}' has more than one package owner"
            );
        }
    }
    owners
}

fn assert_workspace_binary_owned_by_worker_environment_kind(
    binary_owners: &BTreeMap<String, PathBuf>,
    process_bin: &str,
    expected_owner: &str,
    surface: &str,
) {
    let Some(binary) = process_bin.strip_prefix("target/debug/") else {
        return;
    };
    let manifest = binary_owners.get(binary).unwrap_or_else(|| {
        panic!("{surface} references undeclared workspace binary '{process_bin}'")
    });
    let actual_owner = registry_owner(manifest, "worker_environment_kind");
    assert_eq!(
        actual_owner.as_deref(),
        Some(expected_owner),
        "{surface} executes workspace binary '{process_bin}' owned by '{}', not its own worker-environment kind '{expected_owner}'",
        actual_owner.as_deref().unwrap_or("no concrete kind")
    );
}

fn assert_declared_workspace_binary(
    binary_targets: &BTreeSet<String>,
    process_bin: &str,
    owner: &str,
) {
    let Some(binary) = process_bin.strip_prefix("target/debug/") else {
        return;
    };
    assert!(
        !binary.is_empty() && binary_targets.contains(binary),
        "{owner} references workspace binary '{process_bin}', but cargo metadata declares no binary target '{binary}'"
    );
}

#[test]
fn registry_process_wiring_resolves_to_executable_workspace_targets() {
    let repo_root = repo_root();
    let registry_root = repo_root.join("registry");
    let binary_targets = cargo_binary_targets(&cargo_metadata(repo_root));
    let tools = octopus_tool_registry::load_tool_descriptors(&registry_root)
        .expect("tool descriptors must load");
    let mut tool_interfaces = BTreeSet::new();

    for (_, tool) in tools {
        assert_declared_workspace_binary(
            &binary_targets,
            &tool.process.bin,
            &format!("tool '{}'", tool.id.as_str()),
        );
        tool_interfaces.insert(tool.interface.as_str().to_string());
    }

    let interface_root = registry_root.join("interface");
    for entry in std::fs::read_dir(&interface_root).expect("registry/interface readable") {
        let entry = entry.expect("registry/interface entry readable");
        if !entry.file_type().expect("interface entry type").is_dir()
            || !entry.path().join("interface.json").is_file()
        {
            continue;
        }
        let interface = entry.file_name().to_string_lossy().to_string();
        if interface.starts_with("world_provider_") {
            let bytes = std::fs::read(entry.path().join("interface.json"))
                .expect("provider interface descriptor readable");
            let descriptor: octopus_interfaces::InterfaceDescriptor =
                octopus_descriptors::parse_strict(&bytes)
                    .expect("provider interface descriptor parses strictly");
            assert_eq!(descriptor.id.as_str(), interface);
        }
        let resolved = octopus_interface_transport::resolve_spawn_life_config(
            &registry_root,
            &octopus_descriptors::InterfaceId::new(&interface),
        )
        .unwrap_or_else(|error| panic!("interface '{interface}' life failed to resolve: {error}"));
        if interface.starts_with("world_provider_") {
            assert!(
                resolved.is_some(),
                "provider interface '{interface}' must declare executable life"
            );
        }
        let Some(resolved) = resolved else {
            assert!(
                !tool_interfaces.contains(&interface),
                "tool-owning interface '{interface}' resolves no executable life"
            );
            continue;
        };

        for process in &resolved.config.processes {
            assert_declared_workspace_binary(
                &binary_targets,
                &process.bin,
                &format!("interface '{interface}' life"),
            );
        }

        if !tool_interfaces.remove(&interface) {
            continue;
        }
        assert!(
            resolved
                .config
                .processes
                .iter()
                .all(|process| process.bin != "target/debug/octopus-tool-door-life"),
            "tool-owning interface '{interface}' still uses the deleted shared tool-door executable"
        );
        let tool_life = resolved
            .config
            .processes
            .iter()
            .find(|process| {
                let Ok(serde_json::Value::Array(handles)) =
                    serde_json::to_value(&process.read_handles)
                else {
                    return false;
                };
                handles.iter().any(|handle| handle.as_str() == Some("ledger"))
                    && handles
                        .iter()
                        .any(|handle| handle.as_str() == Some("blobs"))
            })
            .unwrap_or_else(|| {
                panic!(
                    "tool-owning interface '{interface}' has no owner-local life reading ledger and blobs"
                )
            });
        let handles = serde_json::to_value(&tool_life.read_handles)
            .expect("tool-door read handles serialize");
        let handles = handles.as_array().expect("tool-door read handles array");
        for required in ["ledger", "blobs"] {
            assert!(
                handles
                    .iter()
                    .any(|handle| handle.as_str() == Some(required)),
                "tool-owning interface '{interface}' tool door lacks required '{required}' handle"
            );
        }
    }

    assert!(
        tool_interfaces.is_empty(),
        "tool descriptors name interfaces absent from registry/interface: {}",
        tool_interfaces.into_iter().collect::<Vec<_>>().join(", ")
    );
}

#[test]
fn removed_second_truth_packages_are_not_workspace_members() {
    let repo_root = repo_root();
    let metadata = cargo_metadata(repo_root);
    let package_names = metadata["packages"]
        .as_array()
        .expect("cargo metadata packages")
        .iter()
        .filter_map(|package| package["name"].as_str())
        .collect::<Vec<_>>();

    for removed in REMOVED_SECOND_TRUTH_PACKAGES {
        assert!(
            !package_names.contains(removed),
            "{removed} must not be a workspace package; the movement ledger is the durable runtime truth"
        );
    }
}

#[test]
fn removed_shared_execution_packages_are_not_workspace_members() {
    let metadata = cargo_metadata(repo_root());
    let package_names = metadata["packages"]
        .as_array()
        .expect("cargo metadata packages")
        .iter()
        .filter_map(|package| package["name"].as_str())
        .collect::<BTreeSet<_>>();

    for removed in REMOVED_SHARED_EXECUTION_PACKAGES {
        assert!(
            !package_names.contains(removed),
            "{removed} is a neutral shared execution spine; execution must belong to a concrete interface or worker-environment kind"
        );
    }
}

#[test]
fn every_workspace_package_lives_in_crates_or_the_registry() {
    let repo_root = repo_root();
    assert!(
        !repo_root.join("interfaces").exists(),
        "the interfaces/ root was dissolved (owner, 2026-07-06); packages live in crates/ (the \
         box) or registry/interface/<id>/ (the interface) only"
    );
    let metadata = cargo_metadata(repo_root);
    let packages = metadata["packages"]
        .as_array()
        .expect("cargo metadata packages");
    for package in packages {
        let Some(manifest_path) = package["manifest_path"].as_str() else {
            continue;
        };
        let manifest_path = Path::new(manifest_path);
        let Ok(relative) = manifest_path.strip_prefix(repo_root) else {
            continue;
        };
        assert!(
            relative.starts_with("crates")
                || relative.starts_with("registry/interface")
                || relative.starts_with("registry/worker_environment_kind")
                || relative.starts_with("registry/verifier"),
            "{} lives outside the registry homes (crates/, registry/interface/, \
             registry/worker_environment_kind/, or registry/verifier/): {}",
            package["name"].as_str().unwrap_or("?"),
            relative.display()
        );
    }
}

#[test]
fn interface_executable_packages_live_beside_their_owner_descriptor() {
    let repo_root = repo_root();
    let metadata = cargo_metadata(repo_root);
    let packages = metadata["packages"]
        .as_array()
        .expect("cargo metadata packages");

    for (package_name, package_dir, interface_dir) in REGISTRY_INTERFACE_OWNER_PACKAGES {
        let package = packages
            .iter()
            .find(|package| package["name"].as_str() == Some(package_name))
            .unwrap_or_else(|| panic!("{package_name} must be a workspace package"));
        let manifest_path = package["manifest_path"]
            .as_str()
            .expect("package manifest_path");
        let manifest_path = Path::new(manifest_path);
        let relative = manifest_path
            .strip_prefix(repo_root)
            .unwrap_or(manifest_path);
        assert_eq!(
            relative,
            Path::new(package_dir).join("Cargo.toml"),
            "{package_name} is interface-owned executable code and must live under its concrete registry owner folder"
        );
        let descriptor = repo_root.join(interface_dir).join("interface.json");
        assert!(
            descriptor.exists(),
            "{package_name}'s registry folder must hold its interface.json BESIDE the code, \
             missing {}",
            descriptor.display()
        );
    }
}

#[test]
fn registry_roots_hold_only_descriptor_owner_folders() {
    let repo_root = repo_root();
    for (kind_root, descriptor_file) in [
        ("registry/interface", "interface.json"),
        (
            "registry/worker_environment_kind",
            "worker_environment_kind.json",
        ),
    ] {
        let root = repo_root.join(kind_root);
        for entry in std::fs::read_dir(&root)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", root.display()))
        {
            let entry = entry.unwrap_or_else(|err| {
                panic!("failed to read an entry under {}: {err}", root.display())
            });
            if !entry
                .file_type()
                .unwrap_or_else(|err| {
                    panic!("failed to read {} type: {err}", entry.path().display())
                })
                .is_dir()
            {
                continue;
            }
            assert!(
                entry.path().join(descriptor_file).is_file(),
                "{} holds no {}. A registry root is a list of self-contained owner \
                 folders (an interface folder name is its routing id; a \
                 worker_environment_kind folder name is the name of an execution-code \
                 type); there is no neutral shared code home (owner, 2026-07-22) — \
                 executable code lives inside the concrete owner's folder. Interfaces \
                 and worker-environment kinds may explicitly import another owner's code",
                entry.path().display(),
                descriptor_file
            );
        }
    }
}

fn registry_owner(path: &Path, registry_kind: &str) -> Option<String> {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if component.as_os_str() != "registry" {
            continue;
        }
        if components.next()?.as_os_str() != registry_kind {
            continue;
        }
        return components
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .map(str::to_string);
    }
    None
}

#[test]
fn worker_environment_tool_doors_and_processes_have_one_code_world() {
    let repo_root = repo_root();
    let metadata = cargo_metadata(repo_root);
    let binary_owners = cargo_binary_owner_manifests(&metadata, repo_root);
    let registry_root = repo_root.join("registry");
    let tools = octopus_tool_registry::load_tool_descriptors(&registry_root)
        .expect("tool descriptors must load")
        .into_iter()
        .map(|(_, tool)| (tool.id.as_str().to_string(), tool))
        .collect::<BTreeMap<_, _>>();
    let kinds = octopus_worker_env::WorkerEnvKindRegistry::load_from_dir(
        &registry_root.join("worker_environment_kind"),
    )
    .expect("worker-environment kinds must load");

    for tool in tools.values() {
        assert_eq!(
            tool.supported_worker_environment_kinds.len(),
            1,
            "tool '{}' must belong to exactly one worker-environment code kind",
            tool.id
        );
        let owner = tool.supported_worker_environment_kinds[0].as_str();
        kinds
            .get(&tool.supported_worker_environment_kinds[0])
            .unwrap_or_else(|error| {
                panic!(
                    "worker-environment tool '{}' names unavailable code kind '{owner}': {error}",
                    tool.id
                )
            });
        let life =
            octopus_interface_transport::resolve_spawn_life_config(&registry_root, &tool.interface)
                .unwrap_or_else(|error| {
                    panic!(
                        "worker-environment tool '{}' exact door '{}' life cannot resolve: {error}",
                        tool.id, tool.interface
                    )
                })
                .unwrap_or_else(|| {
                    panic!(
                        "worker-environment tool '{}' exact door '{}' has no executable life",
                        tool.id, tool.interface
                    )
                });
        for process in &life.config.processes {
            assert_workspace_binary_owned_by_worker_environment_kind(
                &binary_owners,
                &process.bin,
                owner,
                &format!(
                    "worker-environment tool '{}' exact door '{}'",
                    tool.id, tool.interface
                ),
            );
        }
        assert_workspace_binary_owned_by_worker_environment_kind(
            &binary_owners,
            &tool.process.bin,
            owner,
            &format!("worker-environment kind '{owner}' tool '{}'", tool.id),
        );
    }
}

#[test]
fn world_effect_tools_have_one_exact_system_worker_environment() {
    let repo_root = repo_root();
    let registry_root = repo_root.join("registry");
    let tools = octopus_tool_registry::load_tool_descriptors(&registry_root)
        .expect("tool descriptors must load");
    let kinds = octopus_worker_env::WorkerEnvKindRegistry::load_from_dir(
        &registry_root.join("worker_environment_kind"),
    )
    .expect("worker-environment kinds must load");

    for (_, tool) in tools {
        if !tool.is_world_effect {
            continue;
        }
        assert!(
            tool.interface.as_str().starts_with("system_"),
            "world-effect tool '{}' belongs to World interface '{}'; remote World-interface execution is an artifact operation, while Tool Registry effects require a System worker-environment door",
            tool.id,
            tool.interface
        );
        assert_eq!(
            tool.supported_worker_environment_kinds.len(),
            1,
            "world-effect tool '{}' must name exactly one self-contained worker-environment kind",
            tool.id
        );
        let kind_id = &tool.supported_worker_environment_kinds[0];
        kinds.get(kind_id).unwrap_or_else(|error| {
            panic!(
                "world-effect tool '{}' names unavailable worker-environment kind '{}': {error}",
                tool.id, kind_id
            )
        });
    }
}

#[test]
fn runtime_and_app_server_do_not_link_concrete_product_interface_packages() {
    let repo_root = repo_root();
    for package in RUNTIME_DESCRIPTOR_ONLY_PACKAGES {
        let manifest = repo_root.join("crates").join(package).join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest.display()));
        let production_sections = production_manifest_sections(&text);
        for forbidden in FORBIDDEN_RUNTIME_PRODUCT_INTERFACE_PACKAGES {
            assert!(
                !production_sections.contains(&format!("{forbidden} =")),
                "{package} must not link concrete product/interface package {forbidden}; runtime/app-server see interface descriptors, bindings, artifacts, and metadata only"
            );
        }
        for forbidden in PROVIDER_IMPLEMENTATION_PACKAGES {
            assert!(
                !production_sections.contains(&format!("{forbidden} =")),
                "{package} must not directly link neutral provider/interface package {forbidden}; runtime/app-server see interface descriptors, bindings, artifacts, and metadata only"
            );
        }
        for forbidden in FORBIDDEN_RUNTIME_TOOL_REGISTRY_PACKAGES {
            assert!(
                !production_sections.contains(&format!("{forbidden} =")),
                "{package} must not link tool HANDLER CODE ({forbidden}); the composition root loads tool DESCRIPTORS (octopus-tool-registry) and cross-validates them, but tool code crosses the door only as an artifact (IRON RULE / Separation Law §4)"
            );
        }
    }
}

#[test]
fn worker_environment_kind_packages_link_neither_runtime_nor_app_server() {
    let repo_root = repo_root();
    let kinds_root = repo_root.join("registry").join("worker_environment_kind");
    let kinds_root = kinds_root.to_str().expect("worker-environment kind root is UTF-8");
    for kernel in RUNTIME_DESCRIPTOR_ONLY_PACKAGES {
        let output = Command::new("cargo")
            .current_dir(repo_root)
            .args([
                "tree", "--locked", "--workspace", "--edges", "normal", "--prefix", "none",
                "--format", "{p}", "--invert", kernel,
            ])
            .output()
            .unwrap_or_else(|err| panic!("failed to run cargo tree: {err}"));
        assert!(
            output.status.success(),
            "cargo tree --invert {kernel} failed\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let linking: BTreeSet<&str> = std::str::from_utf8(&output.stdout)
            .expect("cargo tree output is UTF-8")
            .lines()
            .filter(|line| line.contains(kinds_root))
            .collect();
        assert!(
            linking.is_empty(),
            "worker-environment kind packages link {kernel}: {linking:?}; a door life or tool process is an application of the OS and reads run records through octopus-run-record, never the kernel"
        );
    }
}

#[test]
fn production_app_does_not_link_concrete_interface_packages() {
    let repo_root = repo_root();
    let manifest = repo_root
        .join("crates")
        .join("octopus-app")
        .join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest.display()));
    let production_sections = production_manifest_sections(&text);
    for forbidden in FORBIDDEN_APP_PRODUCTION_INTERFACE_PACKAGES {
        assert!(
            !production_sections.contains(&format!("{forbidden} =")),
            "octopus-app production dependencies must not link concrete interface package {forbidden}; \
             browser/runtime composition receives artifacts and registry descriptors, site vocabulary belongs to interface packages"
        );
    }
    for forbidden in PROVIDER_IMPLEMENTATION_PACKAGES {
        assert!(
            !production_sections.contains(&format!("{forbidden} =")),
            "octopus-app production dependencies must not directly link neutral provider/interface package {forbidden}; provider implementation belongs only in interface-driver composition roots"
        );
    }
}

fn production_manifest_sections(text: &str) -> &str {
    text.split("\n[dev-dependencies]")
        .next()
        .expect("manifest text has a production prefix")
}

const FORBIDDEN_TERMINAL_MODEL_PROVIDER_PACKAGES: &[&str] = &[
    "octopus-chatgpt",
    "octopus-ollama",
    "octopus-realtime-webrtc",
    "octopus-responses-api-proxy",
    "octopus-client",
    "octopus-backend-client",
    "octopus-cxapi",
    "octopus-prompts",
    "octopus-provider-codex",
    "octopus-appserver-proto",
    "octopus-collab-templates",
    "octopus-install-context",
    "octopus-message-history",
    "octopus-utils-sandbox-summary",
    "octopus-utils-oss",
    "octopus-otel",
    "octopus-login",
    "octopus-protocol",
    "octopus-features",
    "octopus-core",
    "octopus-config",
];

const TRACKED_TRANSITIONAL_TERMINAL_PROVIDER_EDGES: &[&str] = &[];

#[test]
fn terminal_product_does_not_refuse_to_the_model_provider() {
    let repo_root = repo_root();
    let manifest = repo_root
        .join("registry")
        .join("interface")
        .join("world_terminal")
        .join("packages")
        .join("octopus-tui")
        .join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest.display()));
    let production_sections = production_manifest_sections(&text);

    for forbidden in FORBIDDEN_TERMINAL_MODEL_PROVIDER_PACKAGES {
        assert!(
            !production_sections.contains(&format!("{forbidden} =")),
            "octopus-tui (the terminal product) must not link model-provider package {forbidden}; \
             the model turn runs ONLY under the octopus-app daemon (the interface-life switch \
             spawns world_provider_codex's life.json process — the box links zero provider \
             code). The terminal is a thin artifact client — do not re-fuse the spine"
        );
    }

    for forbidden in PROVIDER_IMPLEMENTATION_PACKAGES {
        assert!(
            !production_sections.contains(&format!("{forbidden} =")),
            "octopus-tui (the terminal product) must not directly link neutral provider/interface package {forbidden}; it speaks artifacts and descriptors only"
        );
    }

    for tracked in TRACKED_TRANSITIONAL_TERMINAL_PROVIDER_EDGES {
        let still_linked = production_sections.contains(&format!("{tracked} ="));
        assert!(
            still_linked,
            "octopus-tui no longer links the tracked transitional provider edge {tracked}: the \
             Stage B/C terminal↔model rewire has landed. MOVE {tracked} from \
             TRACKED_TRANSITIONAL_TERMINAL_PROVIDER_EDGES into \
             FORBIDDEN_TERMINAL_MODEL_PROVIDER_PACKAGES so the vertebra stays sealed against re-fusion"
        );
        assert!(
            !FORBIDDEN_TERMINAL_MODEL_PROVIDER_PACKAGES.contains(tracked),
            "{tracked} is both a tracked transitional edge and a forbidden package; pick one"
        );
    }

    for line in production_sections.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if !line.contains("world_provider_codex") && !line.contains("system_interface_authoring") {
            continue;
        }
        let dep_name = line.split('=').next().map(str::trim).unwrap_or_default();
        assert!(
            TRACKED_TRANSITIONAL_TERMINAL_PROVIDER_EDGES.contains(&dep_name),
            "octopus-tui production manifest path-links a provider code-world folder via \
             `{dep_name}` which is NOT in TRACKED_TRANSITIONAL_TERMINAL_PROVIDER_EDGES. No \
             provider edge may hide from the ratchet: conduct it through the architecture, or \
             enumerate it as a tracked transitional edge with its cut-map section"
        );
    }
}

fn manifest_dir_lives_under_provider_implementation(manifest_dir: &str) -> bool {
    let mut components = Path::new(manifest_dir).components();
    while let Some(component) = components.next() {
        if component.as_os_str() != "registry" {
            continue;
        }
        if components.next().map(|kind| kind.as_os_str()) != Some(std::ffi::OsStr::new("interface"))
        {
            continue;
        }
        let Some(instance) = components.next().and_then(|c| c.as_os_str().to_str()) else {
            continue;
        };
        if instance.starts_with("world_provider_") {
            return true;
        }
    }
    false
}

#[test]
fn provider_implementation_path_classifier_covers_concrete_provider_folders() {
    assert!(manifest_dir_lives_under_provider_implementation(
        "/workspace/registry/interface/world_provider_example/packages/example-driver"
    ));
    assert!(!manifest_dir_lives_under_provider_implementation(
        "/workspace/registry/interface/packages/octopus-world-provider-example"
    ));
    assert!(!manifest_dir_lives_under_provider_implementation(
        "/workspace/registry/interface/world_terminal/packages/octopus-tui"
    ));
}

#[test]
fn production_roots_keep_registry_implementation_boundaries() {
    let repo_root = repo_root();
    for package in PROVIDER_BLIND_PRODUCT_ROOT_PACKAGES {
        let output = Command::new("cargo")
            .current_dir(repo_root)
            .args([
                "tree", "--locked", "-p", package, "-e", "normal", "--prefix", "none",
            ])
            .output()
            .unwrap_or_else(|err| panic!("failed to run cargo tree for {package}: {err}"));

        assert!(
            output.status.success(),
            "cargo tree failed for {package}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let tree = String::from_utf8_lossy(&output.stdout);
        let mut breached: Vec<String> = Vec::new();
        for line in tree.lines() {
            let line = line.trim();
            let Some(open) = line.rfind(" (") else {
                continue;
            };
            let Some(path) = line.get(open + 2..).and_then(|rest| rest.strip_suffix(')')) else {
                continue;
            };
            let registry_implementation = registry_owner(Path::new(path), "interface").is_some()
                || registry_owner(Path::new(path), "worker_environment_kind").is_some();
            if (RUNTIME_DESCRIPTOR_ONLY_PACKAGES.contains(package) && registry_implementation)
                || manifest_dir_lives_under_provider_implementation(path)
            {
                let name = line.split_whitespace().next().unwrap_or(line).to_string();
                breached.push(format!("{name} ({path})"));
            }
        }
        breached.sort();
        breached.dedup();

        assert!(
            breached.is_empty(),
            "{package} transitively links {} registry implementation package(s) outside its boundary. Runtime and app-server consume descriptors and artifacts, never registry implementations. Product roots must not link provider implementations.\nBREACHED:\n{}",
            breached.len(),
            breached.join("\n")
        );
    }
}

#[test]
fn terminal_client_surface_is_selected_only_by_registry_policy() {
    let repo_root = repo_root();
    let interface_root = repo_root.join("registry").join("interface");
    let mut selected = Vec::new();
    for entry in std::fs::read_dir(&interface_root).expect("registry/interface readable") {
        let entry = entry.expect("registry/interface entry readable");
        if !entry.file_type().expect("interface entry type").is_dir() {
            continue;
        }
        let path = entry.path().join("client_surface.json");
        if !path.exists() {
            continue;
        }
        let policy: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display())),
        )
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
        if policy
            .pointer("/client_surface/embedded_composer")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            selected.push(entry.file_name().to_string_lossy().to_string());
        }
    }

    assert_eq!(
        selected,
        vec!["world_terminal".to_string()],
        "embedded terminal/client surface must be registry policy, not a runtime/TUI literal"
    );
}

#[test]
fn interface_activation_driver_refs_are_declared_inline() {
    let repo_root = repo_root();
    let registry = repo_root.join("registry");

    for entry in std::fs::read_dir(registry.join("interface")).expect("registry/interface readable")
    {
        let entry = entry.expect("registry/interface entry readable");
        if !entry.file_type().expect("interface entry type").is_dir() {
            continue;
        }
        assert!(
            activation_file_uses_inline_driver_refs(entry.path().join("activation.json")),
            "activation JSON must use activation_driver_ref inline, not a second activation-driver descriptor registry"
        );
    }
}

#[test]
fn production_spine_hot_path_has_no_legacy_state_or_second_extension_graph() {
    let repo_root = repo_root();

    for package in HOT_PATH_PACKAGES {
        let output = Command::new("cargo")
            .current_dir(repo_root)
            .args(["tree", "-p", package, "-e", "normal", "--prefix", "none"])
            .output()
            .unwrap_or_else(|err| panic!("failed to run cargo tree for {package}: {err}"));

        assert!(
            output.status.success(),
            "cargo tree failed for {package}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let tree = String::from_utf8_lossy(&output.stdout);
        for forbidden in FORBIDDEN_HOT_PATH_CRATES {
            assert!(
                !tree.lines().any(|line| line.starts_with(forbidden)),
                "{package} normal dependency graph must not include {forbidden}; tree:\n{tree}"
            );
        }
    }
}

fn activation_file_uses_inline_driver_refs(path: std::path::PathBuf) -> bool {
    if !path.exists() {
        return true;
    }
    let descriptor: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display())),
    )
    .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()));
    for activation in descriptor
        .get("activations")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if activation
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            continue;
        }
        if activation.get("interface_driver_ref").is_some() {
            return false;
        }
        let Some(activation_driver_ref) = activation
            .get("activation_driver_ref")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return false;
        };
        if activation_driver_ref.contains('/') {
            return false;
        }
    }
    true
}

#[test]
fn entry_surfaces_have_no_legacy_state_or_analytics_store_in_normal_graph() {
    let repo_root = repo_root();

    for package in ENTRY_SURFACE_PACKAGES {
        let output = Command::new("cargo")
            .current_dir(repo_root)
            .args(["tree", "-p", package, "-e", "normal", "--prefix", "none"])
            .output()
            .unwrap_or_else(|err| panic!("failed to run cargo tree for {package}: {err}"));

        assert!(
            output.status.success(),
            "cargo tree failed for {package}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let tree = String::from_utf8_lossy(&output.stdout);
        for forbidden in FORBIDDEN_RUNTIME_LEGACY_CRATES {
            assert!(
                !tree.lines().any(|line| line.starts_with(forbidden)),
                "{package} normal dependency graph must not include {forbidden}; tree:\n{tree}"
            );
        }
    }
}

#[test]
fn the_box_does_not_path_link_the_provider_folder() {
    let repo_root = repo_root();
    let crates_root = repo_root.join("crates");
    for entry in std::fs::read_dir(&crates_root).expect("crates/ readable") {
        let entry = entry.expect("crates/ entry readable");
        let manifest = entry.path().join("Cargo.toml");
        if !manifest.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", manifest.display()));
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            assert!(
                !line.contains("world_provider_codex"),
                "{} path-links the provider folder (`{}`): the box must build with \
                 registry/interface/world_provider_codex deleted (codex-cut-map §7.3). Conduct \
                 the edge through the architecture: substrate types via octopus-substrate-contract, \
                 the provider life via its life.json spawn socket, product fronts into the \
                 provider's own packages/",
                manifest.display(),
                line
            );
        }
    }
}

fn worker_environment_kind_door_runtime_sources(repo_root: &Path) -> BTreeMap<String, PathBuf> {
    let kinds_root = repo_root.join("registry/worker_environment_kind");
    let mut owners = BTreeMap::new();
    for entry in std::fs::read_dir(&kinds_root).expect("registry/worker_environment_kind readable")
    {
        let entry = entry.expect("worker_environment_kind entry readable");
        let packages = entry.path().join("packages");
        if !packages.is_dir() {
            continue;
        }
        for package in std::fs::read_dir(&packages).expect("kind packages readable") {
            let package = package.expect("kind package entry readable");
            let name = package.file_name().to_string_lossy().to_string();
            if !name.ends_with("-tool-runtime") {
                continue;
            }
            owners.insert(
                entry.file_name().to_string_lossy().to_string(),
                package.path().join("src"),
            );
        }
    }
    owners
}

#[test]
fn worker_environment_kind_door_runtimes_import_the_owner_instead_of_copying_it() {
    let repo_root = repo_root();
    let owners = worker_environment_kind_door_runtime_sources(repo_root);
    assert!(
        owners.len() > 1,
        "every worker-environment kind owns a -tool-runtime package"
    );

    let mut runtime_bodies = Vec::new();
    let mut copied_owner_apis = Vec::new();
    for (kind, src) in &owners {
        for file in ["door_life.rs", "process_authority.rs"] {
            if src.join(file).is_file() {
                runtime_bodies.push(format!("{kind}/{file}"));
            }
        }
        let lib = src.join("lib.rs");
        if lib.is_file() {
            let text = std::fs::read_to_string(&lib)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", lib.display()));
            if text.contains("pub struct ToolDoorOwner") {
                copied_owner_apis.push(format!("{kind}/lib.rs"));
            }
        }
    }

    assert_eq!(
        runtime_bodies.len(),
        1 * 2,
        "the shared tool-door runtime body must live in exactly ONE owner package and be IMPORTED \
         by the other kinds (owner, 2026-07-22: code is reused by explicit import, never by \
         per-kind copies). Found: {runtime_bodies:?}"
    );
    assert_eq!(
        copied_owner_apis.len(),
        1,
        "exactly one package may define the tool-door runtime API; the others import it. A kind's \
         own lib.rs is for ITS kind-specific module only. Found definitions in: {copied_owner_apis:?}"
    );
}

fn strip_rust_test_modules(text: &str) -> String {
    let mut kept = String::new();
    let mut depth = 0i32;
    let mut skipping_from: Option<i32> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if skipping_from.is_none()
            && (trimmed.starts_with("#[cfg(test)]") || trimmed.starts_with("#[test]"))
        {
            skipping_from = Some(depth);
            continue;
        }
        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;
        if skipping_from.is_none() {
            kept.push_str(line);
            kept.push('\n');
        }
        depth += opens - closes;
        if let Some(start) = skipping_from {
            if depth <= start && closes > 0 {
                skipping_from = None;
            }
        }
    }
    kept
}

const LANGUAGE_HARDCODE_TERMS: &[&str] = &[
    "python",
    "node",
    "bash",
    "ruby",
    "perl",
    "deno",
    "javascript",
    "typescript",
];

#[test]
fn the_verifier_box_stays_blind_to_verifier_identity_and_language() {
    let repo_root = repo_root();
    let verifier_ids = std::fs::read_dir(repo_root.join("registry/verifier"))
        .expect("registry/verifier readable")
        .filter_map(|entry| {
            let entry = entry.expect("registry/verifier entry readable");
            entry
                .path()
                .join("verifier.json")
                .exists()
                .then(|| entry.file_name().to_string_lossy().to_string())
        })
        .collect::<Vec<_>>();
    assert!(
        verifier_ids.len() > 1,
        "registry/verifier holds the live verifier packages"
    );
    let box_src = repo_root.join("crates/octopus-verifiers/src");
    for entry in std::fs::read_dir(&box_src).expect("verifier box src readable") {
        let path = entry.expect("verifier box entry readable").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let source = strip_rust_test_modules(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display())),
        );
        for id in &verifier_ids {
            assert!(
                !source.contains(id.as_str()),
                "{} names the concrete verifier '{id}'. A verifier is a removable package \
                 (VERIFIERS.md §4): the box runs whatever script the descriptor declares and stays \
                 blind to which check it is, so a domain name here is a runtime branch by another \
                 spelling.",
                path.display()
            );
        }
        let lowered = source.to_lowercase();
        for term in LANGUAGE_HARDCODE_TERMS {
            let named = lowered.match_indices(term).any(|(at, _)| {
                !lowered[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            });
            assert!(
                !named,
                "{} names the language/runner '{term}'. Verifiers are polyglot by contract — from \
                 twenty lines of shell to an out-of-system judge — and the box resolves the runner \
                 from the package's own shebang plus its declared `executable_commands`, never from \
                 a language literal.",
                path.display()
            );
        }
    }
}
