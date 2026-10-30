use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_interfaces::{InterfaceArtifactFile, InterfaceInvocationEnvelope};
use octopus_tool_door_runtime::room::emission_root;
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo,
};
use octopus_tool_handlers::{
    ToolCall, ToolHandler, ToolHandlerContext, ToolHandlerError, ToolHandlerResult,
    ToolResultArtifact,
};
use octopus_tool_registry::ToolDescriptor;
use octopus_worker_env::{
    AbsPath, ExecOutput, ExecSurface, FileSurface, NetworkCeiling, PatchOutcome, ProjectedSandbox,
    WorkerEnvError, WorkerEnvironmentLifecycle,
};

use crate::artifact_bound::ArtifactBoundHandler;
use crate::bound_surface::InvocationArtifactSurface;
use crate::{capability, materialize_response};

struct ArtifactBoundProcess {
    handler: ArtifactBoundHandler,
    run_records_root: Option<PathBuf>,
    artifact_blobs_root: Option<PathBuf>,
    registry_artifact_root: Option<PathBuf>,
    ee_instances_root: Option<PathBuf>,
    durable_var_dir: Option<PathBuf>,
    memory_collection: Option<String>,
    observed_run: Option<String>,
    maximum_bytes: u64,
}

impl ArtifactBoundProcess {
    fn memory(
        &self,
        input: &InterfaceInvocationEnvelope,
        call: &ToolCall,
    ) -> Result<ToolHandlerResult, ToolHandlerError> {
        let unavailable = |reason: &str| ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason: reason.to_string(),
        };
        let collection = self.memory_collection.as_deref().ok_or_else(|| {
            unavailable("memory door declares no memory_collection in its private configuration")
        })?;
        let artifact_root = self.registry_artifact_root.as_deref().ok_or_else(|| {
            unavailable("memory operation has no granted registry_artifact_root")
        })?;
        let ee_instances_root = self.ee_instances_root.as_deref().ok_or_else(|| {
            unavailable("memory operation has no granted ee_instances_root")
        })?;
        let effect_root = self
            .durable_var_dir
            .as_ref()
            .map(|directory| directory.join("memory-effects"));
        crate::memory_bound::execute(
            input,
            call,
            &crate::memory_bound::MemoryHost {
                collection,
                artifact_root,
                ee_instances_root,
                effect_root: effect_root.as_deref(),
                maximum_bytes: self.maximum_bytes,
                observed_run: self.observed_run.as_deref(),
            },
        )
    }
}

impl ToolProcessHandler for ArtifactBoundProcess {
    fn substrate_family(&self) -> &str {
        crate::SUBSTRATE_FAMILY
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolProcessInvocation,
        io: ToolProcessIo,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<InterfaceArtifactFile>, ToolProcessError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let decoded = crate::decode_invocation(&invocation.descriptor, &invocation.envelope)
                .map_err(capability)?;
            let correlation_id = decoded.correlation_id;
            let mut call = ToolCall {
                tool_id: decoded.tool,
                arguments: decoded.arguments,
                handler_config: invocation.descriptor.handler_config.clone(),
            };
            attach_skill_source_context(
                &mut call,
                &invocation.descriptor,
                &invocation.envelope,
                self.run_records_root.as_ref(),
            )
            .map_err(capability)?;
            let surface = invocation_surface(&invocation.envelope, self.artifact_blobs_root.as_deref(), self.maximum_bytes).map_err(capability)?;
            let context =
                handler_context(&invocation.envelope, Arc::clone(&surface)).map_err(capability)?;
            let formation = invocation.worker_environment.as_ref().ok_or_else(|| {
                capability("artifact-bound invocation has no formed worker environment")
            })?;
            let outcome = if call.handler_config.get("operation").and_then(serde_json::Value::as_str) == Some("mcp") {
                Ok(crate::client_mcp::execute(&invocation.envelope, &context, &call, &io).await?)
            } else if matches!(call.handler_config.get("operation").and_then(serde_json::Value::as_str),
                Some("collection_discover" | "collection_read" | "collection_prepare")) {
                crate::collection_tools::execute(&invocation.envelope, &call, self.registry_artifact_root.as_deref(), self.maximum_bytes)
            } else if matches!(call.handler_config.get("operation").and_then(serde_json::Value::as_str),
                Some("memory_save" | "memory_search" | "memory_read" | "memory_list")) {
                self.memory(&invocation.envelope, &call)
            } else {
                self.handler.execute(&context, &call).await
            };
            let (lifecycle, response) = handler_completion(
                &call.tool_id,
                correlation_id,
                outcome,
            );
            io.record_worker_environment_lifecycle(formation, lifecycle)
                .await
                .map_err(ToolProcessError::Unavailable)?;
            Ok(vec![materialize_response(&invocation.envelope, response).map_err(capability)?])
        })
    }
}

fn handler_completion(
    tool_id: &str,
    correlation_id: Option<String>,
    outcome: Result<ToolHandlerResult, ToolHandlerError>,
) -> (WorkerEnvironmentLifecycle, ToolResultArtifact) {
    match outcome {
        Ok(result) => (
            WorkerEnvironmentLifecycle::Active,
            ToolResultArtifact::success(tool_id, correlation_id, result.body),
        ),
        Err(error) => (
            WorkerEnvironmentLifecycle::Failed,
            ToolResultArtifact::failure(tool_id, correlation_id, error.into_tool_result_error()),
        ),
    }
}

pub async fn run_artifact_bound_tool_process() -> Result<(), String> {
    let env = ToolProcessEnvironment::from_env()?;
    let memory_collection = env
        .descriptor_snapshot
        .as_ref()
        .and_then(|descriptor| descriptor.interface_driver_config.as_ref())
        .and_then(|config| config.private_config.get("memory_collection"))
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "memory_collection must be an artifact id string".to_string())
        })
        .transpose()?;
    let observed_run = env
        .descriptor_snapshot
        .as_ref()
        .and_then(|descriptor| descriptor.interface_driver_config.as_ref())
        .and_then(|config| config.private_config.get("observed_run"))
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "observed_run must be a run id string".to_string())
        })
        .transpose()?;
    octopus_tool_door_runtime::run_tool_process(Arc::new(ArtifactBoundProcess {
        handler: ArtifactBoundHandler,
        run_records_root: env.root_slots.get("run_records_root").cloned(),
        artifact_blobs_root: env.root_slots.get("artifact_blobs_root").cloned(),
        registry_artifact_root: env.root_slots.get("registry_artifact_root").cloned(),
        ee_instances_root: env.root_slots.get("ee_instances_root").cloned(),
        durable_var_dir: env.durable_var_dir,
        memory_collection,
        observed_run,
        maximum_bytes: env.host_io_limits.max_payload_bytes,
    }))
    .await
}

fn attach_skill_source_context(
    call: &mut ToolCall,
    descriptor: &ToolDescriptor,
    input: &InterfaceInvocationEnvelope,
    run_records_root: Option<&PathBuf>,
) -> Result<(), String> {
    let operation = descriptor
        .handler_config
        .get("operation")
        .and_then(serde_json::Value::as_str);
    if !matches!(operation, Some("skill_capture" | "skill_upsert")) {
        return Ok(());
    }
    let runtime = input.runtime().map_err(|error| error.to_string())?;
    let expected_hash = runtime.run_record_hash.as_deref().ok_or_else(|| {
        "skill write requires the exact active run-record hash in invocation metadata".to_string()
    })?;
    let root = run_records_root
        .ok_or_else(|| "skill write process has no granted run_records_root".to_string())?;
    let run_name = runtime.run.as_str();
    let source_context =
        attested_skill_source_context(root, run_name, runtime.worker.as_str(), expected_hash)?;
    let arguments = call
        .arguments
        .as_object_mut()
        .ok_or_else(|| "skill write arguments must be a JSON object".to_string())?;
    if arguments.contains_key("_skill_source_context") {
        return Err("skill write arguments contain reserved source context".to_string());
    }
    arguments.insert("_skill_source_context".to_string(), source_context);
    Ok(())
}

fn attested_skill_source_context(
    root: &Path,
    run_name: &str,
    expected_worker: &str,
    expected_hash: &str,
) -> Result<serde_json::Value, String> {
    let mut components = std::path::Path::new(run_name).components();
    let safe = matches!(components.next(), Some(std::path::Component::Normal(name)) if name.to_str() == Some(run_name))
        && components.next().is_none();
    if !safe {
        return Err(format!(
            "skill write run id '{run_name}' is not a record file stem"
        ));
    }
    let canonical_root = root.canonicalize().map_err(|error| {
        format!(
            "skill write run_records_root '{}' cannot be resolved: {error}",
            root.display()
        )
    })?;
    let path = canonical_root.join(format!("{run_name}.json"));
    if path.parent() != Some(canonical_root.as_path()) {
        return Err(format!(
            "skill write run record '{}' escapes run_records_root",
            path.display()
        ));
    }
    let canonical_path = path.canonicalize().map_err(|error| {
        format!(
            "skill write run record '{}' cannot be resolved: {error}",
            path.display()
        )
    })?;
    if canonical_path.parent() != Some(canonical_root.as_path()) {
        return Err(format!(
            "skill write run record '{}' resolves outside run_records_root",
            canonical_path.display()
        ));
    }
    let mut file = std::fs::File::open(&canonical_path).map_err(|error| {
        format!(
            "skill write run record '{}' cannot be opened: {error}",
            canonical_path.display()
        )
    })?;
    let before = file.metadata().map_err(|error| {
        format!(
            "skill write run record '{}' metadata cannot be read: {error}",
            canonical_path.display()
        )
    })?;
    if !before.is_file() || before.len() > 4 * 1024 * 1024 {
        return Err(format!(
            "skill write run record '{}' is not a bounded regular file",
            canonical_path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.read_to_end(&mut bytes).map_err(|error| {
        format!(
            "skill write run record '{}' cannot be read: {error}",
            canonical_path.display()
        )
    })?;
    let after = file.metadata().map_err(|error| {
        format!(
            "skill write run record '{}' final metadata cannot be read: {error}",
            canonical_path.display()
        )
    })?;
    if before.len() != after.len() || bytes.len() as u64 != before.len() {
        return Err(format!(
            "skill write run record '{}' changed while being read",
            canonical_path.display()
        ));
    }
    let observed_hash = octopus_ledger::ContentHash::of_bytes(&bytes).to_hex();
    if observed_hash != expected_hash {
        return Err(format!(
            "skill write run record hash changed: invocation has {expected_hash}, file has {observed_hash}"
        ));
    }
    let record: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("skill write run record is malformed: {error}"))?;
    let object = record
        .as_object()
        .ok_or_else(|| "skill write run record must be a JSON object".to_string())?;
    let exact = |name: &str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("skill write run record has no non-empty string '{name}'"))
    };
    let record_id = exact("id")?;
    let trace = exact("trace")?;
    let worker = exact("worker")?;
    if record_id != run_name || worker != expected_worker {
        return Err(format!(
            "skill write run provenance does not match invocation coordinates: record=({record_id},{worker}) invocation=({run_name},{expected_worker})"
        ));
    }
    Ok(serde_json::json!({
        "run": run_name,
        "trace": trace,
        "worker": expected_worker,
    }))
}

fn invocation_surface(
    input: &InterfaceInvocationEnvelope,
    artifact_blobs_root: Option<&std::path::Path>,
    maximum_bytes: u64,
) -> Result<Arc<InvocationArtifactSurface>, String> {
    let grant = input.grant().map_err(|error| error.to_string())?.clone();
    InvocationArtifactSurface::new(
        &input.artifact_binding_payloads,
        &emission_root(input)?,
        grant,
        artifact_blobs_root,
        maximum_bytes,
    )
    .map(Arc::new)
    .map_err(|error| error.to_string())
}

fn handler_context(
    input: &InterfaceInvocationEnvelope,
    surface: Arc<InvocationArtifactSurface>,
) -> Result<ToolHandlerContext, String> {
    let runtime = input.runtime().map_err(|error| error.to_string())?;
    let room_root = emission_root(input)?;
    Ok(ToolHandlerContext {
        run: runtime.run.as_str().to_string(),
        room_root,
        sandbox: ProjectedSandbox::from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            NetworkCeiling::Denied,
        ),
        fs: Arc::new(SealedFs),
        exec: Arc::new(SealedExec),
        artifacts: Some(surface),
    })
}

struct SealedFs;

impl FileSurface for SealedFs {
    fn read(
        &self,
        _path: &AbsPath,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<u8>, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no file surface".into(),
            ))
        })
    }

    fn write(
        &self,
        _path: &AbsPath,
        _bytes: &[u8],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<octopus_worker_env::FileWriteOutput, WorkerEnvError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no file surface".into(),
            ))
        })
    }

    fn list(
        &self,
        _dir: &AbsPath,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<AbsPath>, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no file surface".into(),
            ))
        })
    }

    fn apply_patch(
        &self,
        _patch: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<PatchOutcome, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no file surface".into(),
            ))
        })
    }
}

struct SealedExec;

impl ExecSurface for SealedExec {
    fn run(
        &self,
        _request: octopus_worker_env::ExecRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecOutput, WorkerEnvError>> + Send + '_>,
    > {
        Box::pin(async {
            Err(WorkerEnvError::ArtifactSurfaceFailed(
                "system_context process holds no exec surface".into(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use octopus_ledger::ContentHash;

    use super::{WorkerEnvironmentLifecycle, attested_skill_source_context, handler_completion};

    #[test]
    fn artifact_bound_handler_outcomes_carry_lifecycle_and_typed_results() {
        let (lifecycle, result) = handler_completion(
            "artifact.append",
            Some("append-1".to_string()),
            Ok(octopus_tool_handlers::ToolHandlerResult::new(
                serde_json::json!({"result": "artifact_appended"}),
            )),
        );
        assert_eq!(lifecycle, WorkerEnvironmentLifecycle::Active);
        assert!(result.ok && result.tool == "artifact.append" && result.error.is_none());
        let (lifecycle, result) = handler_completion(
            "artifact.list",
            Some("list-1".to_string()),
            Err(octopus_tool_handlers::ToolHandlerError::Execution {
                tool_id: "artifact.list".to_string(),
                reason: "bound surface failed closed".to_string(),
            }),
        );
        assert_eq!(lifecycle, WorkerEnvironmentLifecycle::Failed);
        assert!(!result.ok && result.tool == "artifact.list" && result.body.is_none());
        assert_eq!(result.error.unwrap().code, "tool_execution_failed");
    }

    #[test]
    fn skill_source_context_is_bound_to_exact_run_bytes_and_coordinates() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "id": "run_skill",
            "ee": "ee_skill",
            "worker": "system_worker_memory",
            "trace": "trace_skill",
            "status": {},
            "checkpoints": [],
            "active_checkpoints": []
        }))
        .unwrap();
        std::fs::write(temp.path().join("run_skill.json"), &bytes).unwrap();
        let hash = ContentHash::of_bytes(&bytes).to_hex();
        let context =
            attested_skill_source_context(temp.path(), "run_skill", "system_worker_memory", &hash)
                .unwrap();
        assert_eq!(context["run"], "run_skill");
        assert_eq!(context["trace"], "trace_skill");
        assert_eq!(context["worker"], "system_worker_memory");
        assert!(
            attested_skill_source_context(temp.path(), "run_skill", "system_worker_coder", &hash,)
                .is_err()
        );
        assert!(
            attested_skill_source_context(
                temp.path(),
                "run_skill",
                "system_worker_memory",
                &ContentHash::of_bytes(b"other").to_hex(),
            )
            .is_err()
        );
        assert!(
            attested_skill_source_context(
                temp.path(),
                "../run_skill",
                "system_worker_memory",
                &hash,
            )
            .is_err()
        );
    }
}
