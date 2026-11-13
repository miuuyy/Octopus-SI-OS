pub mod process_authority;

pub use octopus_descriptors::ProcessAuthoritySpec;
use octopus_descriptors::{Descriptor, VerifierId};
use octopus_ledger::BlobStore;
use octopus_marks::{verified_artifact_payload_bytes, VerificationInput, VerificationVerdict};
use process_authority::{PreparedProcess, ProcessAuthoritySpecExecution};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};


#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifierDescriptor {
    pub id: VerifierId,
    pub package: String,
    pub script: String,
    pub authority: ProcessAuthoritySpec,
    #[serde(default)]
    pub default_params: serde_json::Value,
    /// VERIFIERS.md §3 — a package that reaches outside the system declares how
    /// long its check may run; `params.timeout_ms` may only narrow it.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl Descriptor for VerifierDescriptor {
    const KIND: &'static str = "verifier";
    const SCHEMA_VERSION: u32 = 1;
    const CONTRACT_VERSION: u32 = 3;

    fn declared_name(&self) -> Option<&str> {
        Some(self.package.as_str())
    }

    fn validate(&self) -> std::result::Result<(), String> {
        if self.id.as_str().trim().is_empty() {
            return Err("verifier descriptor has empty id".to_string());
        }
        if self.package.trim().is_empty() {
            return Err(format!("verifier '{}' has empty package", self.id.as_str()));
        }
        if self.package != self.id.as_str() {
            return Err(format!(
                "verifier descriptor package '{}' must equal id '{}'",
                self.package,
                self.id.as_str()
            ));
        }
        let script = self.script.trim();
        if script.is_empty() {
            return Err(format!(
                "verifier '{}' declares an empty script name",
                self.id.as_str()
            ));
        }
        if script.contains('/') || script.contains('\\') || script == "." || script == ".." {
            return Err(format!(
                "verifier '{}' script must be a plain file name inside its own package, got {script:?}",
                self.id.as_str()
            ));
        }
        if !self.default_params.is_null() && !self.default_params.is_object() {
            return Err(format!(
                "verifier '{}' default_params must be a JSON object",
                self.id.as_str()
            ));
        }
        self.authority.validate().map_err(|error| {
            format!(
                "verifier '{}' has invalid process authority: {error}",
                self.id.as_str()
            )
        })?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct VerifierPackageContext {
    pub descriptor: VerifierDescriptor,
    pub package_dir: Option<PathBuf>,
    pub repository_root: PathBuf,
    pub scratch_root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PinnedVerifierPackage {
    descriptor: VerifierDescriptor,
    repository_root: PathBuf,
    scratch_root: PathBuf,
    files: BTreeMap<String, PinnedVerifierFile>,
}

#[derive(Clone, Debug)]
struct PinnedVerifierFile {
    contents: Vec<u8>,
    executable: bool,
}

impl PinnedVerifierPackage {
    pub fn descriptor(&self) -> &VerifierDescriptor {
        &self.descriptor
    }

    pub fn closure_bytes(&self) -> Vec<u8> {
        let mut encoded = b"octopus.verifier.package.v1\0".to_vec();
        encoded.extend_from_slice(&(self.files.len() as u64).to_be_bytes());
        for (path, file) in &self.files {
            encoded.extend_from_slice(&(path.len() as u64).to_be_bytes());
            encoded.extend_from_slice(path.as_bytes());
            encoded.push(u8::from(file.executable));
            encoded.extend_from_slice(&(file.contents.len() as u64).to_be_bytes());
            encoded.extend_from_slice(&file.contents);
        }
        encoded
    }
}

pub fn pin_verifier_package(
    ctx: &VerifierPackageContext,
) -> Result<PinnedVerifierPackage, serde_json::Value> {
    let Some(package_dir) = ctx.package_dir.as_deref() else {
        return Err(serde_json::json!({
            "runner": "package_process",
            "error": "verifier package directory is unavailable to admission"
        }));
    };
    let package_metadata = std::fs::symlink_metadata(package_dir).map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "verifier package directory cannot be inspected",
            "source": error.to_string()
        })
    })?;
    if package_metadata.file_type().is_symlink() || !package_metadata.is_dir() {
        return Err(serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "verifier package path is not a regular directory"
        }));
    }
    let package_dir = std::fs::canonicalize(package_dir).map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "verifier package directory cannot be canonicalized",
            "source": error.to_string()
        })
    })?;
    let script_path = package_dir.join(&ctx.descriptor.script);
    let script_metadata = std::fs::symlink_metadata(&script_path).map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "package script file is missing",
            "script": script_path.display().to_string(),
            "source": error.to_string()
        })
    })?;
    if script_metadata.file_type().is_symlink() || !script_metadata.is_file() {
        return Err(serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "package script is not a regular file",
            "script": script_path.display().to_string()
        }));
    }
    let script_path = std::fs::canonicalize(&script_path).map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "package script cannot be canonicalized",
            "source": error.to_string()
        })
    })?;
    if !script_path.starts_with(&package_dir) {
        return Err(serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "package script resolves outside its verifier package"
        }));
    }
    let files = pin_package_files(&package_dir, &ctx.descriptor)?;
    let Some(entrypoint) = files.get(&ctx.descriptor.script) else {
        return Err(serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "pinned verifier package does not contain its declared script"
        }));
    };
    validate_pinned_entrypoint(&ctx.descriptor, &entrypoint.contents)?;
    Ok(PinnedVerifierPackage {
        descriptor: ctx.descriptor.clone(),
        repository_root: ctx.repository_root.clone(),
        scratch_root: ctx.scratch_root.clone(),
        files,
    })
}

fn validate_pinned_entrypoint(
    descriptor: &VerifierDescriptor,
    contents: &[u8],
) -> Result<(), serde_json::Value> {
    if contents.starts_with(b"#!") || native_executable_header(contents) {
        return Ok(());
    }
    Err(serde_json::json!({
        "runner": "package_process",
        "verifier": descriptor.id.as_str(),
        "script": descriptor.script,
        "error": "verifier entrypoint has no explicit executable format",
        "required_contract": "The declared script must be a native executable or begin with #!/absolute/interpreter. Declare any additional executable commands in authority. The runner makes the pinned entrypoint executable; it does not infer an interpreter from the filename or script body.",
    }))
}

fn native_executable_header(contents: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    {
        matches!(
            object::FileKind::parse(contents),
            Ok(object::FileKind::MachO32
                | object::FileKind::MachO64
                | object::FileKind::MachOFat32
                | object::FileKind::MachOFat64)
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        contents.starts_with(b"\x7fELF")
    }
}

fn pin_package_files(
    package_dir: &Path,
    descriptor: &VerifierDescriptor,
) -> Result<BTreeMap<String, PinnedVerifierFile>, serde_json::Value> {
    let mut files = BTreeMap::new();
    collect_package_files(package_dir, package_dir, descriptor, &mut files)?;
    Ok(files)
}

fn collect_package_files(
    package_root: &Path,
    directory: &Path,
    descriptor: &VerifierDescriptor,
    files: &mut BTreeMap<String, PinnedVerifierFile>,
) -> Result<(), serde_json::Value> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": descriptor.id.as_str(),
            "error": "verifier package directory cannot be read while pinning",
            "path": directory.display().to_string(),
            "source": error.to_string()
        })
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            serde_json::json!({
                "runner": "package_process",
                "verifier": descriptor.id.as_str(),
                "error": "verifier package directory entry cannot be read while pinning",
                "source": error.to_string()
            })
        })?;
        if entry.file_name() == octopus_file_storage::publication::COORDINATION_DIRECTORY {
            continue;
        }
        paths.push(entry.path());
    }
    paths.sort();
    for path in paths {
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            serde_json::json!({
                "runner": "package_process",
                "verifier": descriptor.id.as_str(),
                "error": "verifier package entry cannot be inspected while pinning",
                "path": path.display().to_string(),
                "source": error.to_string()
            })
        })?;
        if metadata.file_type().is_symlink() {
            return Err(serde_json::json!({
                "runner": "package_process",
                "verifier": descriptor.id.as_str(),
                "error": "verifier package cannot contain symbolic links",
                "path": path.display().to_string()
            }));
        }
        if metadata.is_dir() {
            collect_package_files(package_root, &path, descriptor, files)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(serde_json::json!({
                "runner": "package_process",
                "verifier": descriptor.id.as_str(),
                "error": "verifier package contains a non-regular entry",
                "path": path.display().to_string()
            }));
        }
        let relative = path.strip_prefix(package_root).map_err(|error| {
            serde_json::json!({
                "runner": "package_process",
                "verifier": descriptor.id.as_str(),
                "error": "verifier package entry escapes package root",
                "path": path.display().to_string(),
                "source": error.to_string()
            })
        })?;
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(serde_json::json!({
                "runner": "package_process",
                "verifier": descriptor.id.as_str(),
                "error": "verifier package entry has an unsafe relative path",
                "path": relative.display().to_string()
            }));
        }
        let relative = relative.to_str().ok_or_else(|| {
            serde_json::json!({
                "runner": "package_process",
                "verifier": descriptor.id.as_str(),
                "error": "verifier package entry path is not valid UTF-8",
                "path": relative.display().to_string()
            })
        })?;
        let bytes = std::fs::read(&path).map_err(|error| {
            serde_json::json!({
                "runner": "package_process",
                "verifier": descriptor.id.as_str(),
                "error": "verifier package entry bytes cannot be pinned",
                "path": path.display().to_string(),
                "source": error.to_string()
            })
        })?;
        let file = PinnedVerifierFile {
            contents: bytes,
            executable: package_file_executable(&metadata),
        };
        if files.insert(relative.to_string(), file).is_some() {
            return Err(serde_json::json!({
                "runner": "package_process",
                "verifier": descriptor.id.as_str(),
                "error": "verifier package has duplicate relative paths",
                "path": relative
            }));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn package_file_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn package_file_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

pub fn verify_package_process(
    ctx: &VerifierPackageContext,
    input: &VerificationInput,
    blobs: &dyn BlobStore,
) -> VerificationVerdict {
    let payload_bytes = match verified_artifact_payload_bytes(input, blobs, "package_process") {
        Ok(payload_bytes) => payload_bytes,
        Err(details) => return VerificationVerdict::failed(details),
    };
    run_verifier_process(ctx, input, payload_bytes)
}

pub fn run_verifier_process(
    ctx: &VerifierPackageContext,
    input: &VerificationInput,
    payload_bytes: Vec<u8>,
) -> VerificationVerdict {
    let pinned = match pin_verifier_package(ctx) {
        Ok(pinned) => pinned,
        Err(details) => return VerificationVerdict::failed(details),
    };
    run_pinned_verifier_process(&pinned, input, payload_bytes)
}

pub fn run_pinned_verifier_process(
    ctx: &PinnedVerifierPackage,
    input: &VerificationInput,
    payload_bytes: Vec<u8>,
) -> VerificationVerdict {
    match run_package_process(ctx, input, payload_bytes) {
        Ok(verdict) => verdict,
        Err(details) => VerificationVerdict::failed(details),
    }
}

fn run_package_process(
    ctx: &PinnedVerifierPackage,
    input: &VerificationInput,
    payload_bytes: Vec<u8>,
) -> Result<VerificationVerdict, serde_json::Value> {
    let scratch_dir = create_scratch_dir(&ctx.descriptor, &ctx.scratch_root, input)?;
    let script_path = materialize_pinned_package(ctx)?;
    let payload_path = scratch_dir.path.join("payload");
    std::fs::write(&payload_path, &payload_bytes).map_err(|e| {
        serde_json::json!({
            "runner": "package_process",
            "artifact": input.artifact.id.as_str(),
            "error": "failed to write verifier payload file",
            "source": e.to_string()
        })
    })?;
    let request = script_verifier_request(input, &payload_bytes, &payload_path);
    let request_bytes = serde_json::to_vec(&request).map_err(|e| {
        serde_json::json!({
            "runner": "package_process",
            "error": "failed to encode verifier request",
            "source": e.to_string()
        })
    })?;
    let environment = verifier_environment().map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "verifier process environment is unavailable",
            "source": error
        })
    })?;
    let available_root_slots = BTreeMap::new();
    let Some(pinned_package_dir) = script_path.parent().map(Path::to_path_buf) else {
        return Err(serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": "materialized verifier script has no package directory"
        }));
    };
    let resolved = ctx
        .descriptor
        .authority
        .resolve(
            &ctx.repository_root,
            &available_root_slots,
            &environment,
            [pinned_package_dir],
            [scratch_dir.path.clone()],
            [script_path.clone()],
            std::iter::empty(),
        )
        .map_err(|error| {
            serde_json::json!({
                "runner": "package_process",
                "verifier": ctx.descriptor.id.as_str(),
                "error": "verifier process authority could not be resolved",
                "source": error
            })
        })?;
    let prepared = resolved
        .authority
        .prepare(
            &script_path.to_string_lossy(),
            &[],
            &scratch_dir.path,
            environment.get("PATH").map(OsString::as_os_str),
            &ctx.scratch_root,
        )
        .map_err(|error| {
            serde_json::json!({
                "runner": "package_process",
                "verifier": ctx.descriptor.id.as_str(),
                "error": "verifier process could not be prepared",
                "source": error
            })
        })?;
    let timeout = process_timeout_from(&ctx.descriptor, &input.params);
    let result = run_process_with_timeout(
        prepared,
        &environment,
        &scratch_dir.path,
        timeout,
        Some(&request_bytes),
    );
    match result {
        Ok((true, stdout, stderr)) => script_verdict(&stdout, &stderr),
        Ok((false, _stdout, stderr)) => Err(serde_json::json!({
            "runner": "package_process",
            "error": "verifier script exited unsuccessfully",
            "stderr": stderr.trim()
        })),
        Err(mut details) => {
            if let Some(object) = details.as_object_mut() {
                object.insert("runner".into(), serde_json::json!("package_process"));
            }
            Err(details)
        }
    }
}

fn script_verifier_request(
    input: &VerificationInput,
    payload_bytes: &[u8],
    payload_path: &Path,
) -> serde_json::Value {
    serde_json::json!({
        "run": input.run.as_str(),
        "checkpoint": input.checkpoint.as_str(),
        "epoch": input.epoch,
        "mark": input.mark.as_str(),
        "artifact": {
            "id": input.artifact.id.as_str(),
            "content": input.artifact.content.to_hex()
        },
        "artifact_label": input.artifact_label,
        "params": input.params.clone(),
        "metadata": input.metadata.clone(),
        "payload_path": payload_path.display().to_string(),
        "payload_utf8": std::str::from_utf8(payload_bytes).ok()
    })
}

fn script_verdict(stdout: &str, stderr: &str) -> Result<VerificationVerdict, serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        serde_json::json!({
            "runner": "package_process",
            "error": "verifier script stdout was not JSON",
            "stdout": stdout.trim(),
            "stderr": stderr.trim(),
            "source": e.to_string()
        })
    })?;
    let Some(passed) = value.get("passed").and_then(serde_json::Value::as_bool) else {
        return Err(serde_json::json!({
            "runner": "package_process",
            "error": "verifier verdict must include boolean field 'passed'",
            "verdict": value
        }));
    };
    let details = value
        .get("details")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if passed {
        Ok(VerificationVerdict::passed(details))
    } else {
        Ok(VerificationVerdict::failed(details))
    }
}

fn process_timeout_from(descriptor: &VerifierDescriptor, params: &serde_json::Value) -> Option<Duration> {
    let requested = params.get("timeout_ms").and_then(serde_json::Value::as_u64);
    let limit = match (descriptor.timeout_ms, requested) {
        (Some(declared), Some(requested)) => requested.min(declared),
        (declared, requested) => declared.or(requested)?,
    };
    Some(Duration::from_millis(limit.max(1)))
}

const PINNED_PACKAGES_DIRECTORY: &str = "pinned";

fn materialize_pinned_package(ctx: &PinnedVerifierPackage) -> Result<PathBuf, serde_json::Value> {
    let failure = |error: &str, source: String| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": ctx.descriptor.id.as_str(),
            "error": error,
            "source": source
        })
    };
    let root = ctx.scratch_root.join(PINNED_PACKAGES_DIRECTORY);
    std::fs::create_dir_all(&root)
        .map_err(|error| failure("pinned verifier package root cannot be created", error.to_string()))?;
    let package_dir = root.join(octopus_ledger::ContentHash::of_bytes(&ctx.closure_bytes()).to_hex());
    if !pinned_package_matches(ctx, &package_dir) {
        let staging = root.join(format!(".staging-{}-{}", std::process::id(), unique_nonce()));
        write_pinned_package(ctx, &staging)
            .map_err(|error| failure("pinned verifier package cannot be materialized", error))?;
        if std::fs::rename(&staging, &package_dir).is_err() {
            if pinned_package_matches(ctx, &package_dir) {
                let _ = std::fs::remove_dir_all(&staging);
            } else {
                let stale = root.join(format!(".stale-{}-{}", std::process::id(), unique_nonce()));
                if package_dir.exists() {
                    std::fs::rename(&package_dir, &stale).map_err(|error| {
                        failure("mismatched pinned verifier package cannot be replaced", error.to_string())
                    })?;
                    let _ = std::fs::remove_dir_all(&stale);
                }
                std::fs::rename(&staging, &package_dir).map_err(|error| {
                    failure("pinned verifier package cannot be installed", error.to_string())
                })?;
            }
        }
        if !pinned_package_matches(ctx, &package_dir) {
            return Err(failure(
                "materialized verifier package differs from its pinned bytes",
                package_dir.display().to_string(),
            ));
        }
    }
    std::fs::canonicalize(package_dir.join(&ctx.descriptor.script))
        .map_err(|error| failure("pinned verifier script cannot be canonicalized", error.to_string()))
}

fn unique_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn write_pinned_package(ctx: &PinnedVerifierPackage, staging: &Path) -> Result<(), String> {
    std::fs::create_dir(staging).map_err(|error| error.to_string())?;
    for (relative, file) in &ctx.files {
        let path = staging.join(relative);
        let parent = path.parent().ok_or_else(|| format!("pinned file '{relative}' has no parent"))?;
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        std::fs::write(&path, &file.contents).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable = file.executable || *relative == ctx.descriptor.script;
            let mode = if executable { 0o500 } else { 0o400 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn pinned_package_matches(ctx: &PinnedVerifierPackage, package_dir: &Path) -> bool {
    let mut found = BTreeMap::new();
    if collect_materialized_files(package_dir, package_dir, &mut found).is_err() {
        return false;
    }
    found.len() == ctx.files.len()
        && ctx.files.iter().all(|(relative, file)| {
            found.get(relative).is_some_and(|(contents, executable)| {
                *contents == file.contents
                    && *executable == (file.executable || *relative == ctx.descriptor.script)
            })
        })
}

fn collect_materialized_files(
    root: &Path,
    directory: &Path,
    found: &mut BTreeMap<String, (Vec<u8>, bool)>,
) -> Result<(), ()> {
    for entry in std::fs::read_dir(directory).map_err(|_| ())? {
        let path = entry.map_err(|_| ())?.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|_| ())?;
        if metadata.is_dir() {
            collect_materialized_files(root, &path, found)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(());
        }
        let relative = path.strip_prefix(root).map_err(|_| ())?.to_str().ok_or(())?.to_string();
        let contents = std::fs::read(&path).map_err(|_| ())?;
        found.insert(relative, (contents, package_file_executable(&metadata)));
    }
    Ok(())
}

fn create_scratch_dir(
    descriptor: &VerifierDescriptor,
    scratch_root: &Path,
    input: &VerificationInput,
) -> Result<ScratchDirectory, serde_json::Value> {
    std::fs::create_dir_all(scratch_root).map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": descriptor.id.as_str(),
            "error": "verifier scratch root cannot be created",
            "source": error.to_string()
        })
    })?;
    let root = std::fs::canonicalize(scratch_root).map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": descriptor.id.as_str(),
            "error": "verifier scratch root cannot be canonicalized",
            "source": error.to_string()
        })
    })?;
    let safe_artifact_id: String = input
        .artifact
        .id
        .as_str()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = root.join(format!(
        "{}-{}-{}-{}",
        descriptor.id.as_str(),
        std::process::id(),
        nonce,
        safe_artifact_id
    ));
    std::fs::create_dir(&path).map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": descriptor.id.as_str(),
            "error": "verifier scratch directory cannot be created",
            "source": error.to_string()
        })
    })?;
    let path = std::fs::canonicalize(&path).map_err(|error| {
        serde_json::json!({
            "runner": "package_process",
            "verifier": descriptor.id.as_str(),
            "error": "verifier scratch directory cannot be canonicalized",
            "source": error.to_string()
        })
    })?;
    Ok(ScratchDirectory { path })
}

struct ScratchDirectory {
    path: PathBuf,
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn verifier_environment() -> Result<BTreeMap<String, OsString>, String> {
    let path = std::env::var_os("PATH")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "host PATH is absent".to_string())?;
    Ok(BTreeMap::from([("PATH".to_string(), path)]))
}

fn run_process_with_timeout(
    prepared: PreparedProcess,
    environment: &BTreeMap<String, OsString>,
    cwd: &Path,
    timeout: Option<Duration>,
    stdin_bytes: Option<&[u8]>,
) -> Result<(bool, String, String), serde_json::Value> {
    let executable = prepared.executable.display().to_string();
    let mut child = Command::new(&prepared.program)
        .args(prepared.args)
        .env_clear()
        .envs(prepared.environment)
        .envs(environment)
        .env("TMPDIR", cwd)
        .current_dir(cwd)
        .stdin(if stdin_bytes.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            serde_json::json!({
                "error": "failed to spawn verifier process",
                "executable": executable,
                "source": e.to_string()
            })
        })?;

    let mut stdin_error = None;
    if let Some(bytes) = stdin_bytes {
        if let Some(mut stdin) = child.stdin.take() {
            stdin_error = stdin.write_all(bytes).err();
            if stdin_error.is_none() {
                stdin_error = stdin.write_all(b"\n").err();
            }
        }
    }

    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|e| {
            serde_json::json!({
                "error": "failed to poll verifier process",
                "source": e.to_string()
            })
        })? {
            let mut stdout = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_string(&mut stdout);
            }
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            if let Some(error) = stdin_error {
                return Err(serde_json::json!({
                    "error": "failed to write verifier process stdin",
                    "source": error.to_string(),
                    "exit_code": status.code(),
                    "stderr": stderr.trim()
                }));
            }
            return Ok((status.success(), stdout, stderr));
        }
        if let Some(timeout) = timeout.filter(|timeout| started.elapsed() >= *timeout) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(serde_json::json!({
                "error": "verifier process timed out",
                "timeout_ms": timeout.as_millis()
            }));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::{ArtifactId, CheckpointId, MarkId, RunId};
    use octopus_ledger::{ArtifactRef, BlobStore, InMemoryBlobs};

    fn test_input(artifact: ArtifactRef, params: serde_json::Value) -> VerificationInput {
        VerificationInput {
            run: RunId::new("run_verifier"),
            checkpoint: CheckpointId::new("checkpoint_verifier"),
            epoch: 0,
            mark: MarkId::new("verifier.ok"),
            artifact,
            artifact_label: "payload.verifier".to_string(),
            params,
            metadata: serde_json::json!({"route": "test"}),
        }
    }

    fn package_ctx(id: &str) -> VerifierPackageContext {
        VerifierPackageContext {
            descriptor: VerifierDescriptor {
                id: VerifierId::new(id),
                package: id.to_string(),
                script: "verify.py".to_string(),
                timeout_ms: None,
                authority: ProcessAuthoritySpec {
                    executable_commands: std::collections::BTreeSet::from(["/bin/sh".to_string()]),
                    ..ProcessAuthoritySpec::default()
                },
                default_params: serde_json::Value::Null,
            },
            package_dir: None,
            repository_root: std::env::temp_dir(),
            scratch_root: std::env::temp_dir()
                .join(format!("octopus-verifier-test-{}", std::process::id())),
        }
    }

    fn blob(bytes: &[u8]) -> (InMemoryBlobs, ArtifactRef) {
        let mut blobs = InMemoryBlobs::new();
        let hash = blobs.put(bytes);
        (
            blobs,
            ArtifactRef {
                id: ArtifactId::new("artifact.verifier"),
                content: hash,
            },
        )
    }

    #[cfg(unix)]
    #[test]
    fn pinned_package_script_is_unchanged_after_the_registry_file_moves() {
        use std::os::unix::fs::PermissionsExt;

        let package_dir = std::env::temp_dir().join(format!(
            "octopus-verifier-package-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&package_dir).unwrap();
        let script_path = package_dir.join("verify.sh");
        let helper_path = package_dir.join("helper.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nscript_dir=${0%/*}\n. \"$script_dir/helper.sh\"\nverify_payload\n",
        )
        .unwrap();
        std::fs::write(
            &helper_path,
            "verify_payload() { request=$(cat); case \"$request\" in *\"hello from artifact\"*) printf '{\"passed\":true,\"details\":{\"via\":\"package_helper\"}}';; *) printf '{\"passed\":false,\"details\":{\"reason\":\"payload_missing\"}}';; esac; }\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script_path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).unwrap();

        let (_, artifact) = blob(b"hello from artifact");
        let ctx = VerifierPackageContext {
            descriptor: VerifierDescriptor {
                id: VerifierId::new("script_probe"),
                package: "script_probe".to_string(),
                script: "verify.sh".to_string(),
                timeout_ms: None,
                authority: ProcessAuthoritySpec {
                    executable_commands: std::collections::BTreeSet::from([
                        "/bin/cat".to_string(),
                        "/bin/sh".to_string(),
                    ]),
                    ..ProcessAuthoritySpec::default()
                },
                default_params: serde_json::Value::Null,
            },
            package_dir: Some(package_dir.clone()),
            repository_root: std::env::temp_dir(),
            scratch_root: std::env::temp_dir()
                .join(format!("octopus-verifier-test-{}", std::process::id())),
        };
        let input = test_input(artifact, serde_json::json!({"timeout_ms": 2000}));
        let pinned = pin_verifier_package(&ctx).expect("the package pins before mutation");
        let pinned_closure = pinned.closure_bytes();
        std::fs::write(
            &helper_path,
            "verify_payload() { printf '{\"passed\":false,\"details\":{\"reason\":\"mutated\"}}'; }\n",
        )
        .unwrap();
        let mutated = pin_verifier_package(&ctx).expect("the mutated package pins independently");
        assert_ne!(pinned_closure, mutated.closure_bytes());
        let verdict = run_pinned_verifier_process(&pinned, &input, b"hello from artifact".to_vec());
        let _ = std::fs::remove_dir_all(&package_dir);

        assert!(verdict.passed, "unexpected verdict: {:?}", verdict.details);
        assert_eq!(
            verdict
                .details
                .get("via")
                .and_then(serde_json::Value::as_str),
            Some("package_helper")
        );
    }

    #[test]
    fn pinned_package_is_materialized_once_and_restored_before_it_runs() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let package = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let script = package.path().join("verify.sh");
        std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\nprintf '{\"passed\":true,\"details\":{}}'\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut ctx = package_ctx("materialized_probe");
        ctx.descriptor.script = "verify.sh".to_string();
        ctx.descriptor.authority.executable_commands.insert("/bin/cat".to_string());
        ctx.package_dir = Some(package.path().to_path_buf());
        ctx.scratch_root = scratch.path().to_path_buf();
        let pinned = pin_verifier_package(&ctx).unwrap();
        let (_, artifact) = blob(b"payload");
        let input = test_input(artifact, serde_json::json!({"timeout_ms": 2000}));
        let materialized = scratch.path().join(PINNED_PACKAGES_DIRECTORY)
            .join(octopus_ledger::ContentHash::of_bytes(&pinned.closure_bytes()).to_hex())
            .join("verify.sh");

        assert!(run_pinned_verifier_process(&pinned, &input, b"payload".to_vec()).passed);
        let first = std::fs::metadata(&materialized).unwrap().ino();
        assert!(run_pinned_verifier_process(&pinned, &input, b"payload".to_vec()).passed);
        assert_eq!(std::fs::metadata(&materialized).unwrap().ino(), first);

        std::fs::set_permissions(&materialized, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&materialized, "#!/bin/sh\nprintf '{\"passed\":false,\"details\":{}}'\n").unwrap();
        std::fs::write(materialized.with_file_name("planted.sh"), "#!/bin/sh\n").unwrap();
        assert!(run_pinned_verifier_process(&pinned, &input, b"payload".to_vec()).passed);
        assert!(!materialized.with_file_name("planted.sh").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn authored_text_verifier_requires_explicit_interpreter_and_preserves_pins() {
        use std::os::unix::fs::PermissionsExt;

        let package = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "not granted").unwrap();
        let script = package.path().join("verify.py");
        let body = "import json,sys\nrequest=json.load(sys.stdin)\ntry:\n open(request['params']['outside_path']).read()\n isolated=False\nexcept PermissionError:\n isolated=True\nprint(json.dumps({'passed':request['payload_utf8']=='verified payload' and isolated,'details':{'received':request['payload_utf8'],'isolated':isolated}}))\n";
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut ctx = package_ctx("system_interpreter");
        ctx.package_dir = Some(package.path().to_path_buf());
        ctx.repository_root = package.path().to_path_buf();
        ctx.scratch_root = package.path().join("scratch");
        ctx.descriptor.authority.executable_commands =
            std::collections::BTreeSet::from(["/usr/bin/env".to_string(), "python3".to_string()]);
        ctx.descriptor.authority.read_executable_relative_paths = vec![
            octopus_descriptors::ProcessExecutableRelativePathSpec {
                executable: "python3".into(), relative_path: "../lib".into(),
            },
        ];
        let (_, artifact) = blob(b"verified payload");
        let input = test_input(
            artifact,
            serde_json::json!({"outside_path": outside.path()}),
        );
        let missing = run_verifier_process(&ctx, &input, b"verified payload".to_vec());
        assert!(!missing.passed);
        assert_eq!(
            missing.details["error"],
            "verifier entrypoint has no explicit executable format"
        );

        std::fs::write(&script, format!("#!/usr/bin/env python3\n{body}")).unwrap();
        assert_eq!(
            std::fs::metadata(&script).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let pinned = pin_verifier_package(&ctx).unwrap();
        let pinned_closure = pinned.closure_bytes();
        let passed = run_pinned_verifier_process(&pinned, &input, b"verified payload".to_vec());
        assert!(passed.passed, "unexpected verdict: {:?}", passed.details);
        assert_eq!(passed.details["isolated"], true);
        let (_, other) = blob(b"other payload");
        let refused = run_pinned_verifier_process(
            &pinned,
            &test_input(other, input.params.clone()),
            b"other payload".to_vec(),
        );
        assert!(!refused.passed);
        assert_eq!(refused.details["received"], "other payload");

        ctx.descriptor
            .authority
            .executable_commands
            .remove("python3");
        let denied = run_verifier_process(&ctx, &input, b"verified payload".to_vec());
        assert!(
            !denied.passed,
            "undeclared env-selected command executed: {:?}",
            denied.details
        );
        ctx.descriptor
            .authority
            .executable_commands
            .insert("python3".to_string());
        std::fs::write(
            &script,
            "#!/usr/bin/env python3\nprint('{\"passed\":false,\"details\":{\"revision\":2}}')\n",
        )
        .unwrap();
        let updated = pin_verifier_package(&ctx).unwrap();
        assert_ne!(pinned_closure, updated.closure_bytes());
        let retained = run_pinned_verifier_process(&pinned, &input, b"verified payload".to_vec());
        assert!(
            retained.passed,
            "pinned code changed: {:?}",
            retained.details
        );
        let changed = run_pinned_verifier_process(&updated, &input, b"verified payload".to_vec());
        assert!(!changed.passed);
        assert_eq!(changed.details["revision"], 2);

        std::fs::write(&script, format!("#!/usr/bin/python3\n{body}")).unwrap();
        ctx.descriptor.authority.executable_commands =
            std::collections::BTreeSet::from(["/usr/bin/python3".to_string()]);
        ctx.descriptor.authority.read_executable_relative_paths[0].executable = "/usr/bin/python3".into();
        let absolute = run_verifier_process(&ctx, &input, b"verified payload".to_vec());
        assert!(
            absolute.passed,
            "explicit absolute interpreter failed: {:?}",
            absolute.details
        );

        let authoring: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../registry/artifact/artifact-codebase-layout/artifact.json"
        )))
        .unwrap();
        let example = &authoring["executable_authoring"]["examples"]["python_verifier"];
        ctx.descriptor = serde_json::from_value(example["descriptor"].clone()).unwrap();
        ctx.descriptor.validate().unwrap();
        std::fs::write(&script, example["source"].as_str().unwrap()).unwrap();
        let payload = br#"{"label":"example.echo.result","tool":"example.echo","ok":true,"body":{"value":"example"}}"#;
        let (blobs, artifact) = blob(payload);
        let mut input = test_input(artifact, example["params"].clone());
        let verdict = verify_package_process(&ctx, &input, &blobs);
        assert!(
            verdict.passed,
            "published example failed: {:?}",
            verdict.details
        );
        input.params["expected"] = serde_json::json!("different");
        let verdict = verify_package_process(&ctx, &input, &blobs);
        assert!(!verdict.passed);
        assert_eq!(verdict.details["observed"], "example");
    }

    #[cfg(unix)]
    #[test]
    fn only_a_declared_limit_bounds_a_verifier_process() {
        let mut descriptor = package_ctx("timing").descriptor;
        assert_eq!(process_timeout_from(&descriptor, &serde_json::Value::Null), None);
        assert_eq!(process_timeout_from(&descriptor, &serde_json::json!({"timeout_ms": 700})), Some(Duration::from_millis(700)));
        descriptor.timeout_ms = Some(60_000);
        assert_eq!(process_timeout_from(&descriptor, &serde_json::Value::Null), Some(Duration::from_secs(60)));
        assert_eq!(process_timeout_from(&descriptor, &serde_json::json!({"timeout_ms": 90_000})), Some(Duration::from_secs(60)));
    }

    #[test]
    fn verifier_stdin_failure_retains_child_exit_evidence() {
        let package = tempfile::tempdir().unwrap();
        std::fs::write(
            package.path().join("verify.sh"),
            "#!/bin/sh\nprintf 'verifier launch failed' >&2\nexit 23\n",
        )
        .unwrap();
        let mut ctx = package_ctx("stdin_failure");
        ctx.descriptor.script = "verify.sh".to_string();
        ctx.package_dir = Some(package.path().to_path_buf());
        ctx.repository_root = package.path().to_path_buf();
        ctx.scratch_root = package.path().join("scratch");
        let payload = vec![b'x'; 2 * 1024 * 1024];
        let (_, artifact) = blob(&payload);
        let verdict = run_pinned_verifier_process(
            &pin_verifier_package(&ctx).unwrap(),
            &test_input(artifact, serde_json::json!({})),
            payload,
        );
        assert!(!verdict.passed);
        assert_eq!(
            verdict.details["error"],
            "failed to write verifier process stdin"
        );
        assert_eq!(verdict.details["exit_code"], 23);
        assert_eq!(verdict.details["stderr"], "verifier launch failed");
    }

    #[test]
    fn package_script_verifier_fails_closed_without_package_script() {
        let (blobs, artifact) = blob(b"payload");

        let ctx = package_ctx("script_probe");
        let verdict = verify_package_process(
            &ctx,
            &test_input(artifact.clone(), serde_json::json!({})),
            &blobs,
        );
        assert!(!verdict.passed);
        assert_eq!(
            verdict
                .details
                .get("error")
                .and_then(serde_json::Value::as_str),
            Some("verifier package directory is unavailable to admission")
        );

        let mut ctx = package_ctx("script_probe");
        let empty_package = tempfile::tempdir().unwrap();
        ctx.package_dir = Some(empty_package.path().to_path_buf());
        let verdict =
            verify_package_process(&ctx, &test_input(artifact, serde_json::json!({})), &blobs);
        assert!(!verdict.passed);
        assert_eq!(
            verdict
                .details
                .get("error")
                .and_then(serde_json::Value::as_str),
            Some("package script file is missing")
        );
    }
}
