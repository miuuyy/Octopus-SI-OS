use octopus_descriptors::{Descriptor, ProcessSandboxMode};
use octopus_marks::MarkDescriptor;
use octopus_verifiers::{pin_verifier_package, VerifierDescriptor, VerifierPackageContext};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("octopus-verifiers lives under crates/")
        .to_path_buf()
}

#[test]
fn every_registry_verifier_descriptor_validates_and_pins() {
    let repo = repo_root();
    let verifier_root = repo.join("registry/verifier");
    let scratch = tempfile::tempdir().unwrap();
    let mut count = 0usize;

    for entry in std::fs::read_dir(&verifier_root).expect("registry/verifier readable") {
        let entry = entry.expect("registry/verifier entry readable");
        if !entry.file_type().expect("entry file type").is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let descriptor_path = entry.path().join("verifier.json");
        assert!(
            descriptor_path.is_file(),
            "registry/verifier/{dir_name} is missing verifier.json"
        );
        let bytes = std::fs::read(&descriptor_path).expect("verifier.json readable");
        let descriptor: VerifierDescriptor = octopus_descriptors::parse_strict(&bytes)
            .unwrap_or_else(|err| panic!("strict parse failed for '{dir_name}': {err}"));
        descriptor
            .validate()
            .unwrap_or_else(|err| panic!("validation failed for '{dir_name}': {err}"));
        assert_eq!(descriptor.id.as_str(), dir_name);
        assert_eq!(descriptor.package, dir_name);
        assert_eq!(
            descriptor.authority.sandbox_mode,
            ProcessSandboxMode::Enforced,
            "verifier '{dir_name}' must declare enforced sandbox mode"
        );

        let ctx = VerifierPackageContext {
            descriptor,
            package_dir: Some(entry.path()),
            repository_root: repo.clone(),
            scratch_root: scratch.path().to_path_buf(),
        };
        let pinned_a = pin_verifier_package(&ctx)
            .unwrap_or_else(|err| panic!("pin_verifier_package failed for '{dir_name}': {err}"));
        let pinned_b = pin_verifier_package(&ctx)
            .unwrap_or_else(|err| panic!("second pin failed for '{dir_name}': {err}"));
        assert!(!pinned_a.closure_bytes().is_empty());
        assert_eq!(
            pinned_a.closure_bytes(),
            pinned_b.closure_bytes(),
            "closure bytes must be deterministic for '{dir_name}'"
        );
        count += 1;
    }

    assert!(count > 0, "registry/verifier must contain at least one package");
}

#[test]
fn every_registry_mark_descriptor_validates() {
    let mark_root = repo_root().join("registry/mark");
    let mut count = 0usize;

    for entry in std::fs::read_dir(&mark_root).expect("registry/mark readable") {
        let entry = entry.expect("registry/mark entry readable");
        if !entry.file_type().expect("mark entry type").is_dir() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().to_string();
        let descriptor_path = entry.path().join("mark.json");
        assert!(
            descriptor_path.is_file(),
            "registry/mark/{dir_name} is missing mark.json"
        );
        let bytes = std::fs::read(&descriptor_path).expect("mark.json readable");
        let descriptor: MarkDescriptor = octopus_descriptors::parse_strict(&bytes)
            .unwrap_or_else(|err| panic!("strict parse failed for mark '{dir_name}': {err}"));
        descriptor
            .validate()
            .unwrap_or_else(|err| panic!("validation failed for mark '{dir_name}': {err}"));
        assert_eq!(descriptor.id.as_str(), dir_name);
        assert_eq!(descriptor.package, dir_name);
        count += 1;
    }

    assert!(count > 0, "registry/mark must contain at least one descriptor");
}
