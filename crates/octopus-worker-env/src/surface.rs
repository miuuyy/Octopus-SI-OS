use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::caps::WorkerEnvCaps;
use crate::descriptor::WorkerEnvKindSpec;
use crate::sandbox::ProjectedSandbox;
use crate::{
    AbsPath, EnvContext, WekDescriptorHash, WorkerEnvError, WorkerEnvironmentId,
    WorkerEnvironmentKindId,
};

pub trait WorkerEnvDriver: Send + Sync {
    fn substrate_family(&self) -> &str;

    fn open(
        &self,
        spec: &WorkerEnvKindSpec,
        descriptor_hash: WekDescriptorHash,
        caps: WorkerEnvCaps,
        ctx: EnvContext,
        id: WorkerEnvironmentId,
    ) -> Pin<Box<dyn Future<Output = Result<WorkerEnvironment, WorkerEnvError>> + Send + '_>>;
}

pub type ReleaseHook = Box<
    dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<(), WorkerEnvError>> + Send>> + Send + Sync,
>;

pub struct WorkerEnvironmentParts {
    pub sandbox: ProjectedSandbox,
    pub cwd: AbsPath,
    pub fs: Arc<dyn FileSurface>,
    pub release: Option<ReleaseHook>,
}

pub struct WorkerEnvironment {
    id: WorkerEnvironmentId,
    kind_id: WorkerEnvironmentKindId,
    descriptor_hash: WekDescriptorHash,
    substrate_family: String,
    sandbox: Arc<ProjectedSandbox>,
    cwd: AbsPath,
    fs: Arc<dyn FileSurface>,
    release: Option<ReleaseHook>,
}

impl WorkerEnvironment {
    pub fn assemble(
        spec: &WorkerEnvKindSpec,
        descriptor_hash: WekDescriptorHash,
        id: WorkerEnvironmentId,
        parts: WorkerEnvironmentParts,
    ) -> Result<Self, WorkerEnvError> {
        if spec.lifecycle.requires_release && parts.release.is_none() {
            return Err(WorkerEnvError::OpenFailed {
                substrate_family: spec.substrate_family.clone(),
                reason: format!(
                    "kind {} requires release but the driver assembled no release hook",
                    spec.worker_environment_kind_id.as_str()
                ),
            });
        }
        Ok(Self {
            id,
            kind_id: spec.worker_environment_kind_id.clone(),
            descriptor_hash,
            substrate_family: spec.substrate_family.clone(),
            sandbox: Arc::new(parts.sandbox),
            cwd: parts.cwd,
            fs: parts.fs,
            release: parts.release,
        })
    }

    pub fn id(&self) -> &WorkerEnvironmentId {
        &self.id
    }
    pub fn kind_id(&self) -> &WorkerEnvironmentKindId {
        &self.kind_id
    }
    pub fn descriptor_hash(&self) -> &WekDescriptorHash {
        &self.descriptor_hash
    }
    pub fn substrate_family(&self) -> &str {
        &self.substrate_family
    }
    pub fn sandbox(&self) -> &ProjectedSandbox {
        &self.sandbox
    }

    pub fn exec_with(&self, runner: Arc<dyn SandboxedCommandRunner>) -> Arc<dyn ExecSurface> {
        Arc::new(SandboxedExec {
            sandbox: self.sandbox.clone(),
            cwd: self.cwd.clone(),
            runner,
        })
    }

    pub fn fs(&self) -> Arc<dyn FileSurface> {
        self.fs.clone()
    }

    pub async fn release(mut self) -> Result<(), WorkerEnvError> {
        match self.release.take() {
            Some(hook) => hook().await,
            None => Ok(()),
        }
    }
}

pub trait ExecSurface: Send + Sync {
    fn run(
        &self,
        request: ExecRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ExecOutput, WorkerEnvError>> + Send + '_>>;
}

pub trait SandboxedCommandRunner: Send + Sync {
    fn run_sandboxed(
        &self,
        sandbox: &ProjectedSandbox,
        cwd: &AbsPath,
        argv: Vec<String>,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<ExecOutput, WorkerEnvError>> + Send + '_>>;
}

struct SandboxedExec {
    sandbox: Arc<ProjectedSandbox>,
    cwd: AbsPath,
    runner: Arc<dyn SandboxedCommandRunner>,
}

impl ExecSurface for SandboxedExec {
    fn run(
        &self,
        request: ExecRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ExecOutput, WorkerEnvError>> + Send + '_>> {
        let sandbox = self.sandbox.clone();
        let default_cwd = self.cwd.clone();
        let runner = self.runner.clone();
        Box::pin(async move {
            if request.argv.is_empty() {
                return Err(WorkerEnvError::ExecFailed("empty argv".into()));
            }
            if request.timeout.is_zero() {
                return Err(WorkerEnvError::ExecFailed(
                    "command timeout must be non-zero".into(),
                ));
            }
            if request.max_output_bytes == 0 {
                return Err(WorkerEnvError::ExecFailed(
                    "command output ceiling must be non-zero".into(),
                ));
            }
            let cwd = request.cwd.unwrap_or(default_cwd);
            sandbox.authorize_read(&cwd)?;
            runner
                .run_sandboxed(
                    &sandbox,
                    &cwd,
                    request.argv,
                    request.timeout,
                    request.max_output_bytes,
                )
                .await
        })
    }
}

#[derive(Clone, Debug)]
pub struct FileWriteOutput {
    pub previous_content: Result<Option<Vec<u8>>, String>,
}

pub trait FileSurface: Send + Sync {
    fn read(
        &self,
        path: &AbsPath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, WorkerEnvError>> + Send + '_>>;
    fn read_range(
        &self,
        path: &AbsPath,
        offset: u64,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<FileReadOutput, WorkerEnvError>> + Send + '_>> {
        let path = path.clone();
        Box::pin(async move {
            let bytes = self.read(&path).await?;
            let file_size = u64::try_from(bytes.len()).map_err(|_| {
                WorkerEnvError::ReadOutsideRoots(format!(
                    "{} is too large to address with a u64 offset",
                    path.as_str()
                ))
            })?;
            let start = usize::try_from(offset).map_err(|_| {
                WorkerEnvError::ReadOutsideRoots(format!(
                    "{} offset {offset} exceeds this platform",
                    path.as_str()
                ))
            })?;
            if start > bytes.len() {
                return Err(WorkerEnvError::ReadOutsideRoots(format!(
                    "{} offset {offset} exceeds file size {file_size}",
                    path.as_str()
                )));
            }
            let end = start.saturating_add(max_bytes).min(bytes.len());
            Ok(FileReadOutput {
                file_size,
                offset,
                bytes: bytes[start..end].to_vec(),
            })
        })
    }
    fn write(
        &self,
        path: &AbsPath,
        bytes: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<FileWriteOutput, WorkerEnvError>> + Send + '_>>;
    fn list(
        &self,
        dir: &AbsPath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<AbsPath>, WorkerEnvError>> + Send + '_>>;
    fn apply_patch(
        &self,
        patch: &str,
    ) -> Pin<Box<dyn Future<Output = Result<PatchOutcome, WorkerEnvError>> + Send + '_>>;
}

#[derive(Clone, Debug)]
pub struct BoundArtifact {
    pub artifact: octopus_ledger::ArtifactRef,
    pub text: Option<String>,
}

pub trait ArtifactSurface: Send + Sync {
    fn append(
        &self,
        label: &str,
        payload: &[u8],
    ) -> Pin<
        Box<dyn Future<Output = Result<octopus_ledger::ArtifactRef, WorkerEnvError>> + Send + '_>,
    >;
    fn read(
        &self,
        id: &str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, WorkerEnvError>> + Send + '_>>;
    fn list(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<BoundArtifact>, WorkerEnvError>> + Send + '_>>;
}

#[derive(Clone, Debug)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    pub cwd: Option<AbsPath>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_total_bytes: usize,
    pub stderr_total_bytes: usize,
    pub duration: Duration,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Clone, Debug)]
pub struct FileReadOutput {
    pub file_size: u64,
    pub offset: u64,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct PatchOutcome {
    pub changed_paths: Vec<AbsPath>,
    pub is_exact: bool,
}
