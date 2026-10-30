#![forbid(unsafe_code)]

mod caps;
mod descriptor;
mod formation;
mod lease;
mod registry;
mod registry_contracts;
mod sandbox;
mod surface;

#[cfg(test)]
mod block_on;
#[cfg(test)]
mod contract_test;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use octopus_descriptors::{
    ExecutorKind, InterfaceId, RootAccess, RootRef, WorkerEnvironmentKindId,
};

pub use caps::WorkerEnvCaps;
pub use descriptor::{
    BoundaryDefaults, EnvCapabilityTag, NetworkCeiling, WekLifecycle, WorkerConsent,
    WorkerEnvKindSpec,
};
pub use formation::{
    WorkerEnvironmentFormation, WorkerEnvironmentFormationRequest, WorkerEnvironmentLifecycle,
    WorkerEnvironmentRecord, WorkerEnvironmentRecordStore, WorkerEnvironmentSnapshot,
};
pub use lease::lease;
pub use registry::WorkerEnvKindRegistry;
pub use registry_contracts::{
    validate_worker_environment_registry, worker_environment_kind_ids,
    worker_environment_registry_issues, WorkerEnvironmentCeilingView,
    WorkerEnvironmentRegistryIssue, WorkerEnvironmentRegistrySubject, WorkerEnvironmentToolView,
};
pub use sandbox::ProjectedSandbox;
pub use surface::{
    ArtifactSurface, BoundArtifact, ExecOutput, ExecRequest, ExecSurface, FileReadOutput,
    FileSurface, FileWriteOutput, PatchOutcome, ReleaseHook, SandboxedCommandRunner,
    WorkerEnvDriver, WorkerEnvironment, WorkerEnvironmentParts,
};

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize)]
pub struct WorkerEnvironmentId(String);

impl WorkerEnvironmentId {
    pub fn derive(formation_pin: &[u8]) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(b"weid\0");
        h.update(formation_pin);
        let hex = h.finalize().to_hex();
        Self(format!("we_{}", &hex.as_str()[..32]))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, WorkerEnvError> {
        let value = value.into();
        let valid = value.len() == 35
            && value.starts_with("we_")
            && value[3..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !valid {
            return Err(WorkerEnvError::MalformedWorkerEnvironmentId(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for WorkerEnvironmentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for WorkerEnvironmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EeIdRef<'a>(pub &'a str);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RunIdRef<'a>(pub &'a str);

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct WekDescriptorHash(pub [u8; 32]);

impl WekDescriptorHash {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct AbsPath(String);

impl AbsPath {
    pub fn new(p: &Path) -> Result<Self, WorkerEnvError> {
        if !p.is_absolute() {
            return Err(WorkerEnvError::NonAbsolutePath(p.display().to_string()));
        }
        let mut stack: Vec<String> = Vec::new();
        for comp in p.components() {
            match comp {
                Component::RootDir => {}
                Component::CurDir => {}
                Component::Prefix(_) => {
                    return Err(WorkerEnvError::NonAbsolutePath(p.display().to_string()));
                }
                Component::ParentDir => {
                    if stack.pop().is_none() {
                        return Err(WorkerEnvError::NonAbsolutePath(format!(
                            "{} escapes above root via '..'",
                            p.display()
                        )));
                    }
                }
                Component::Normal(seg) => {
                    let s = seg
                        .to_str()
                        .ok_or_else(|| WorkerEnvError::NonAbsolutePath(p.display().to_string()))?;
                    stack.push(s.to_string());
                }
            }
        }
        Ok(Self(format!("/{}", stack.join("/"))))
    }
    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_within(&self, root: &AbsPath) -> bool {
        let child_segs: Vec<&str> = self.0.split('/').filter(|s| !s.is_empty()).collect();
        let root_segs: Vec<&str> = root.0.split('/').filter(|s| !s.is_empty()).collect();
        if root_segs.len() > child_segs.len() {
            return false;
        }
        root_segs.iter().zip(child_segs.iter()).all(|(r, c)| r == c)
    }
}

#[derive(Clone, Debug)]
pub struct EnvContext {
    pub additional_root_slots: BTreeMap<String, Vec<AbsPath>>,
    pub root_slots: BTreeMap<String, AbsPath>,
    pub env: BTreeMap<String, String>,
}

impl EnvContext {
    pub fn resolve_roots(&self, name: &str) -> Result<Vec<AbsPath>, WorkerEnvError> {
        let mut roots = vec![self.resolve_root(name)?];
        roots.extend(self.additional_root_slots.get(name).into_iter().flatten().cloned());
        Ok(roots)
    }

    pub fn resolve_root(&self, name: &str) -> Result<AbsPath, WorkerEnvError> {
        self.root_slots
            .get(name)
            .cloned()
            .ok_or_else(|| WorkerEnvError::CapExceedsBoundary(name.to_string()))
    }

    /// The host root-slots the door instance actually exposes — the door leg of
    /// root-cap formation. A boundary root the door does not resolve here cannot
    /// enter the formed caps (`WorkerEnvCaps::form`).
    pub fn provided_root_slots(&self) -> BTreeSet<String> {
        self.provided_root_bindings().into_keys().collect()
    }

    pub fn provided_root_bindings(&self) -> BTreeMap<String, PathBuf> {
        self.root_slots
            .iter()
            .map(|(name, root)| (name.clone(), root.as_path().to_path_buf()))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct CapturedExecFailure {
    pub output: ExecOutput,
    pub exit_observed: bool,
    pub stdout_complete: bool,
    pub stderr_complete: bool,
    pub cancelled: bool,
    pub cleanup_errors: Vec<String>,
}

#[derive(Debug, Error)]
pub enum WorkerEnvError {
    #[error("malformed worker-environment-kind descriptor at {path}: {reason}")]
    MalformedDescriptor { path: String, reason: String },
    #[error("duplicate worker_environment_kind_id: {0}")]
    DuplicateKind(String),
    #[error("worker-environment formation has no compatible kind: {0}")]
    NoCompatibleFormation(String),
    #[error("malformed worker-environment formation: {0}")]
    MalformedFormation(String),
    #[error("unknown worker_environment_kind_id: {0}")]
    UnknownKind(String),
    #[error("unknown pinned descriptor hash (no historical version retained): {0:?}")]
    UnknownDescriptorHash(WekDescriptorHash),
    #[error("worker environment driver does not serve substrate_family: {0}")]
    UnknownSubstrateFamily(String),
    #[error("cap references root {0} absent from boundary ceiling (no widening)")]
    CapExceedsBoundary(String),
    #[error("path is not absolute/canonical: {0}")]
    NonAbsolutePath(String),
    #[error("write outside projected writable roots: {0}")]
    WriteOutsideRoots(String),
    #[error("read outside projected readable roots: {0}")]
    ReadOutsideRoots(String),
    #[error("substrate open failed for {substrate_family}: {reason}")]
    OpenFailed {
        substrate_family: String,
        reason: String,
    },
    #[error("substrate release failed for {id}: {reason}")]
    ReleaseFailed { id: String, reason: String },
    #[error("malformed worker_environment_id: {0}")]
    MalformedWorkerEnvironmentId(String),
    #[error("patch apply failed: {0}")]
    PatchFailed(String),
    #[error("sandboxed exec failed: {0}")]
    ExecFailed(String),
    #[error("sandboxed exec failed: {reason}")]
    ExecFailedWithOutput {
        reason: String,
        captured: Box<CapturedExecFailure>,
    },
    #[error("bound artifact surface failed: {0}")]
    ArtifactSurfaceFailed(String),
    #[error("worker-environment record failed at {path}: {reason}")]
    RecordFailed { path: String, reason: String },
}
