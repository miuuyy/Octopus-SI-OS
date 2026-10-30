use octopus_descriptors::ProcessSandboxMode;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use octopus_tool_handlers::{
    ToolCall, ToolHandler, ToolHandlerContext, ToolHandlerError, ToolHandlerResult,
    optional_string_argument, required_string_argument,
};
use octopus_worker_env::{ExecRequest, ProjectedSandbox, WorkerEnvError, WorkerEnvironmentId};

use crate::workspace_fs::{relative_room_path, resolve_room_path};
use crate::{WorkspaceHandlerConfig, WorkspaceOperation, workspace_handler_config};

pub struct WorkspaceExecHandler;

impl ToolHandler for WorkspaceExecHandler {
    fn execute<'a>(
        &'a self,
        ctx: &'a ToolHandlerContext,
        call: &'a ToolCall,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<ToolHandlerResult, ToolHandlerError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move { execute(ctx, call).await })
    }
}

async fn execute(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let config =
        workspace_handler_config(call).map_err(|reason| ToolHandlerError::InvalidArguments {
            tool_id: call.tool_id.clone(),
            reason,
        })?;
    match config.operation {
        WorkspaceOperation::RunCommand => run_command(ctx, call, &config).await,
        _ => Err(ToolHandlerError::UnsupportedTool {
            tool_id: call.tool_id.clone(),
        }),
    }
}

pub(crate) async fn start_command(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
    call_identity: &str,
    inherited_env: &std::collections::BTreeSet<String>,
    config: &WorkspaceHandlerConfig,
    manager: &octopus_workspace_substrate::WorkspaceProcessManager,
    worker_environment_id: &WorkerEnvironmentId,
    sandbox: &ProjectedSandbox,
    sandbox_mode: ProcessSandboxMode,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let argv = command_argv(tool_id, &call.arguments, config)?;
    let workdir = optional_string_argument(tool_id, &call.arguments, "workdir")?
        .unwrap_or_else(|| ".".to_string());
    let cwd = resolve_room_path(ctx, tool_id, &workdir)?;
    let timeout_default =
        required_handler_u64(tool_id, config.default_timeout_ms, "default_timeout_ms")?;
    let timeout_max = required_handler_u64(tool_id, config.max_timeout_ms, "max_timeout_ms")?;
    let yield_default = required_handler_u64(
        tool_id,
        config.default_yield_time_ms,
        "default_yield_time_ms",
    )?;
    let yield_max = required_handler_u64(tool_id, config.max_yield_time_ms, "max_yield_time_ms")?;
    let output_default = required_handler_usize(
        tool_id,
        config.default_max_output_bytes,
        "default_max_output_bytes",
    )?;
    let output_max = required_handler_usize(tool_id, config.max_output_bytes, "max_output_bytes")?;
    let timeout_ms =
        bounded_optional_u64_argument(tool_id, &call.arguments, "timeout_ms", 1, timeout_max)?
            .unwrap_or(timeout_default);
    let yield_time_ms =
        bounded_optional_u64_argument(tool_id, &call.arguments, "yield_time_ms", 0, u64::MAX)?
            .unwrap_or(yield_default)
            .min(yield_max);
    let max_output_bytes = bounded_optional_usize_argument(
        tool_id,
        &call.arguments,
        "max_output_bytes",
        1,
        output_max,
    )?
    .unwrap_or(output_default);
    let tty = optional_bool_argument(tool_id, &call.arguments, "tty")?.unwrap_or(false);
    let path = std::env::var("PATH").map_err(|_| ToolHandlerError::Execution {
        tool_id: tool_id.to_string(),
        reason: "persistent room process requires descriptor-inherited PATH".to_string(),
    })?;
    let home = std::env::var("HOME").map_err(|_| ToolHandlerError::Execution {
        tool_id: tool_id.to_string(),
        reason: "persistent room process requires descriptor-inherited HOME".to_string(),
    })?;
    let limits = octopus_workspace_substrate::WorkspaceProcessLimits {
        max_processes_per_worker_environment: required_handler_usize(
            tool_id,
            config.max_processes_per_worker_environment,
            "max_processes_per_worker_environment",
        )?,
        max_buffered_output_bytes: required_handler_usize(
            tool_id,
            config.max_buffered_output_bytes,
            "max_buffered_output_bytes",
        )?,
        max_buffered_output_events: required_handler_usize(
            tool_id,
            config.max_buffered_output_events,
            "max_buffered_output_events",
        )?,
        max_input_bytes: required_handler_usize(
            tool_id,
            config.max_input_bytes,
            "max_input_bytes",
        )?,
        process_timeout: Duration::from_millis(timeout_ms),
        cleanup_timeout: Duration::from_millis(required_handler_u64(
            tool_id,
            config.cleanup_timeout_ms,
            "cleanup_timeout_ms",
        )?),
    };
    let poll = manager
        .start(
            worker_environment_id,
            sandbox,
            sandbox_mode,
            argv,
            cwd.as_path().to_path_buf(),
            inherited_env
                .iter()
                .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
                .chain([("HOME".to_string(), home), ("PATH".to_string(), path)])
                .collect(),
            tty,
            yield_time_ms,
            max_output_bytes,
            limits,
            Some(call_identity),
        )
        .await
        .map_err(|reason| ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason,
        })?;
    persistent_poll_result(ctx, tool_id, poll)
}

pub(crate) async fn write_stdin(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
    call_identity: &str,
    config: &WorkspaceHandlerConfig,
    manager: &octopus_workspace_substrate::WorkspaceProcessManager,
    worker_environment_id: &WorkerEnvironmentId,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let process_id = required_string_argument(tool_id, &call.arguments, "process_id")?;
    let chars = optional_string_argument(tool_id, &call.arguments, "chars")?.unwrap_or_default();
    let max_input_bytes =
        required_handler_usize(tool_id, config.max_input_bytes, "max_input_bytes")?;
    if chars.len() > max_input_bytes {
        return Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!(
                "arguments field 'chars' contains {} bytes, exceeding the descriptor-owned {max_input_bytes} byte ceiling",
                chars.len()
            ),
        });
    }
    let yield_default = required_handler_u64(
        tool_id,
        config.default_yield_time_ms,
        "default_yield_time_ms",
    )?;
    let yield_max = required_handler_u64(tool_id, config.max_yield_time_ms, "max_yield_time_ms")?;
    let output_default = required_handler_usize(
        tool_id,
        config.default_max_output_bytes,
        "default_max_output_bytes",
    )?;
    let output_max = required_handler_usize(tool_id, config.max_output_bytes, "max_output_bytes")?;
    let yield_time_ms =
        bounded_optional_u64_argument(tool_id, &call.arguments, "yield_time_ms", 0, u64::MAX)?
            .unwrap_or(yield_default)
            .min(yield_max);
    let max_output_bytes = bounded_optional_usize_argument(
        tool_id,
        &call.arguments,
        "max_output_bytes",
        1,
        output_max,
    )?
    .unwrap_or(output_default);
    let poll = manager
        .write_and_poll(
            worker_environment_id,
            &process_id,
            chars.as_bytes(),
            yield_time_ms,
            max_output_bytes,
            Some(call_identity),
        )
        .await
        .map_err(|reason| ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason,
        })?;
    persistent_poll_result(ctx, tool_id, poll)
}

pub(crate) async fn process_signal(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
    call_identity: &str,
    config: &WorkspaceHandlerConfig,
    manager: &octopus_workspace_substrate::WorkspaceProcessManager,
    worker_environment_id: &WorkerEnvironmentId,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let process_id = required_string_argument(tool_id, &call.arguments, "process_id")?;
    let signal = required_string_argument(tool_id, &call.arguments, "signal")?;
    if signal != "interrupt" {
        return Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: "arguments field 'signal' must be 'interrupt'".to_string(),
        });
    }
    let (yield_time_ms, max_output_bytes) = poll_limits(tool_id, &call.arguments, config)?;
    let poll = manager
        .interrupt_and_poll(
            worker_environment_id,
            &process_id,
            yield_time_ms,
            max_output_bytes,
            Some(call_identity),
        )
        .await
        .map_err(|reason| ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason,
        })?;
    persistent_control_result(ctx, tool_id, "interrupt", poll)
}

pub(crate) async fn process_terminate(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
    call_identity: &str,
    config: &WorkspaceHandlerConfig,
    manager: &octopus_workspace_substrate::WorkspaceProcessManager,
    worker_environment_id: &WorkerEnvironmentId,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let process_id = required_string_argument(tool_id, &call.arguments, "process_id")?;
    let (yield_time_ms, max_output_bytes) = poll_limits(tool_id, &call.arguments, config)?;
    let poll = manager
        .terminate_and_poll(
            worker_environment_id,
            &process_id,
            yield_time_ms,
            max_output_bytes,
            Some(call_identity),
        )
        .await
        .map_err(|reason| ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason,
        })?;
    persistent_control_result(ctx, tool_id, "terminate", poll)
}

pub(crate) async fn process_list(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
    manager: &octopus_workspace_substrate::WorkspaceProcessManager,
    worker_environment_id: &WorkerEnvironmentId,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let processes = manager
        .list(worker_environment_id)
        .await
        .map_err(|reason| ToolHandlerError::Execution {
            tool_id: call.tool_id.clone(),
            reason,
        })?
        .into_iter()
        .map(|process| {
            Ok(serde_json::json!({
                "process_id": process.process_id,
                "running": process.running,
                "exited": process.exited,
                "exit_code": process.exit_code,
                "closed": process.closed,
                "timed_out": process.timed_out,
                "failure": process.failure,
                "continuation_available": process.continuation_available,
                "pending_output_bytes": process.pending_output_bytes,
                "next_source_seq": process.next_source_seq,
                "tty": process.tty,
                "workdir": relative_room_path(ctx, &call.tool_id, &process.cwd)?,
            }))
        })
        .collect::<Result<Vec<_>, ToolHandlerError>>()?;
    Ok(ToolHandlerResult::new(serde_json::json!({
        "result": "processes_listed",
        "worker_environment_id": worker_environment_id.as_str(),
        "process_count": processes.len(),
        "processes": processes,
    })))
}

fn poll_limits(
    tool_id: &str,
    arguments: &serde_json::Value,
    config: &WorkspaceHandlerConfig,
) -> Result<(u64, usize), ToolHandlerError> {
    let yield_default = required_handler_u64(
        tool_id,
        config.default_yield_time_ms,
        "default_yield_time_ms",
    )?;
    let yield_max = required_handler_u64(tool_id, config.max_yield_time_ms, "max_yield_time_ms")?;
    let output_default = required_handler_usize(
        tool_id,
        config.default_max_output_bytes,
        "default_max_output_bytes",
    )?;
    let output_max = required_handler_usize(tool_id, config.max_output_bytes, "max_output_bytes")?;
    let yield_time_ms =
        bounded_optional_u64_argument(tool_id, arguments, "yield_time_ms", 0, u64::MAX)?
            .unwrap_or(yield_default)
            .min(yield_max);
    let max_output_bytes =
        bounded_optional_usize_argument(tool_id, arguments, "max_output_bytes", 1, output_max)?
            .unwrap_or(output_default);
    Ok((yield_time_ms, max_output_bytes))
}

fn persistent_control_result(
    ctx: &ToolHandlerContext,
    tool_id: &str,
    action: &str,
    poll: octopus_workspace_substrate::WorkspaceProcessPoll,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let mut result = persistent_poll_result(ctx, tool_id, poll)?;
    let body = result
        .body
        .as_object_mut()
        .ok_or_else(|| ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason: "persistent process control result is not a JSON object".to_string(),
        })?;
    body.insert(
        "control".to_string(),
        serde_json::json!({"action": action, "delivered": true}),
    );
    Ok(result)
}

pub(crate) fn persistent_poll_result(
    ctx: &ToolHandlerContext,
    tool_id: &str,
    poll: octopus_workspace_substrate::WorkspaceProcessPoll,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let failure_reason = poll.failure.clone();
    let result = if poll.failure.is_some() {
        "command_output_failed"
    } else if poll.timed_out {
        "command_timed_out"
    } else if poll.closed && !poll.continuation_available {
        "command_finished"
    } else if poll.exited {
        "command_exited"
    } else {
        "command_running"
    };
    let chunks = poll
        .chunks
        .iter()
        .map(|chunk| {
            serde_json::json!({
                "source_seq": chunk.seq,
                "stream": match chunk.stream {
                    octopus_workspace_substrate::ExecOutputStream::Stdout => "stdout",
                    octopus_workspace_substrate::ExecOutputStream::Stderr => "stderr",
                    octopus_workspace_substrate::ExecOutputStream::Pty => "pty",
                },
                "byte_offset": chunk.byte_offset,
                "end_of_event": chunk.end_of_event,
                "content": retained_stream_evidence(&chunk.bytes),
                "preview": model_text_preview(&chunk.bytes),
            })
        })
        .collect::<Vec<_>>();
    let body = serde_json::json!({
        "result": result,
        "completed": result == "command_finished",
        "process_id": poll.process_id,
        "continuation_available": poll.continuation_available,
        "workdir": relative_room_path(ctx, tool_id, &poll.cwd)?,
        "tty": poll.tty,
        "output": {
            "chunks": chunks,
            "next_source_seq": poll.next_seq,
            "running": poll.running,
            "exited": poll.exited,
            "exit_code": poll.exit_code,
            "exit_seq": poll.exit_seq,
            "closed": poll.closed,
            "close_seq": poll.close_seq,
            "timed_out": poll.timed_out,
            "failure": poll.failure,
        }
    });
    if let Some(reason) = failure_reason {
        return Err(ToolHandlerError::ExecutionWithDetails {
            tool_id: tool_id.to_string(),
            reason,
            details: body.as_object().cloned().unwrap_or_default(),
        });
    }
    Ok(ToolHandlerResult::new(body))
}

async fn run_command(
    ctx: &ToolHandlerContext,
    call: &ToolCall,
    config: &WorkspaceHandlerConfig,
) -> Result<ToolHandlerResult, ToolHandlerError> {
    let tool_id = &call.tool_id;
    let argv = command_argv(tool_id, &call.arguments, config)?;
    let workdir = optional_string_argument(tool_id, &call.arguments, "workdir")?
        .unwrap_or_else(|| ".".to_string());
    let cwd = resolve_room_path(ctx, tool_id, &workdir)?;
    let timeout_default =
        required_handler_u64(tool_id, config.default_timeout_ms, "default_timeout_ms")?;
    let timeout_max = required_handler_u64(tool_id, config.max_timeout_ms, "max_timeout_ms")?;
    let output_default = required_handler_usize(
        tool_id,
        config.default_max_output_bytes,
        "default_max_output_bytes",
    )?;
    let output_max = required_handler_usize(tool_id, config.max_output_bytes, "max_output_bytes")?;
    let timeout_ms =
        bounded_optional_u64_argument(tool_id, &call.arguments, "timeout_ms", 1, timeout_max)?
            .unwrap_or(timeout_default);
    let max_output_bytes = bounded_optional_usize_argument(
        tool_id,
        &call.arguments,
        "max_output_bytes",
        1,
        output_max,
    )?
    .unwrap_or(output_default);
    let request = ExecRequest {
        argv,
        cwd: Some(cwd.clone()),
        timeout: Duration::from_millis(timeout_ms),
        max_output_bytes,
    };
    match ctx.exec.run(request).await {
        Ok(out) => Ok(ToolHandlerResult::new(serde_json::json!({
            "result": if out.timed_out { "command_timed_out" } else { "command_finished" },
            "completed": !out.timed_out,
            "workdir": relative_room_path(ctx, tool_id, &cwd)?,
            "output": command_output(&out),
        }))),
        Err(WorkerEnvError::ExecFailedWithOutput { reason, captured }) => {
            let mut output = command_output(&captured.output);
            if !captured.exit_observed {
                output["exit_code"] = serde_json::Value::Null;
            }
            output["failure"] = serde_json::json!(reason);
            output["exit_observed"] = serde_json::json!(captured.exit_observed);
            output["cancelled"] = serde_json::json!(captured.cancelled);
            output["stdout_complete"] = serde_json::json!(captured.stdout_complete);
            output["stderr_complete"] = serde_json::json!(captured.stderr_complete);
            output["capture_complete"] =
                serde_json::json!(captured.stdout_complete && captured.stderr_complete);
            output["cleanup_errors"] = serde_json::json!(captured.cleanup_errors);
            let details = serde_json::json!({
                "result": if captured.cancelled { "command_cancelled" } else { "command_failed" },
                "completed": false,
                "workdir": relative_room_path(ctx, tool_id, &cwd)?,
                "effect_status": "unknown",
                "output": output,
            });
            Err(ToolHandlerError::ExecutionWithDetails {
                tool_id: tool_id.to_string(),
                reason: format!("sandbox.exec_failed: {reason}"),
                details: details
                    .as_object()
                    .expect("command failure details are an object")
                    .clone(),
            })
        }
        Err(WorkerEnvError::ExecFailed(reason)) => Err(ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason: format!("sandbox.exec_failed: {reason}"),
        }),
        Err(e) => Err(ToolHandlerError::Execution {
            tool_id: tool_id.to_string(),
            reason: e.to_string(),
        }),
    }
}

fn command_output(out: &octopus_worker_env::ExecOutput) -> serde_json::Value {
    serde_json::json!({
        "exit_code": out.exit_code,
        "stdout": retained_stream_evidence(&out.stdout),
        "stderr": retained_stream_evidence(&out.stderr),
        "stdout_preview": model_text_preview(&out.stdout),
        "stderr_preview": model_text_preview(&out.stderr),
        "stdout_bytes": out.stdout.len(),
        "stderr_bytes": out.stderr.len(),
        "stdout_total_bytes": out.stdout_total_bytes,
        "stderr_total_bytes": out.stderr_total_bytes,
        "stdout_omitted_bytes": out.stdout_total_bytes.saturating_sub(out.stdout.len()),
        "stderr_omitted_bytes": out.stderr_total_bytes.saturating_sub(out.stderr.len()),
        "stdout_retention": if out.stdout_truncated { "head_tail" } else { "complete" },
        "stderr_retention": if out.stderr_truncated { "head_tail" } else { "complete" },
        "stdout_truncated": out.stdout_truncated,
        "stderr_truncated": out.stderr_truncated,
        "duration_ms": u64::try_from(out.duration.as_millis()).unwrap_or(u64::MAX),
        "timed_out": out.timed_out,
    })
}

pub(crate) fn retained_stream_evidence(bytes: &[u8]) -> serde_json::Value {
    let retained_blake3 = blake3::hash(bytes).to_hex().to_string();
    match std::str::from_utf8(bytes) {
        Ok(text) => serde_json::json!({
            "encoding": "utf8",
            "text": text,
            "retained_bytes": bytes.len(),
            "retained_blake3": retained_blake3,
        }),
        Err(_) => serde_json::json!({
            "encoding": "base64",
            "base64": BASE64_STANDARD.encode(bytes),
            "retained_bytes": bytes.len(),
            "retained_blake3": retained_blake3,
        }),
    }
}

pub(crate) fn model_text_preview(bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "encoding": "utf8_lossy",
        "text": String::from_utf8_lossy(bytes),
        "lossy": std::str::from_utf8(bytes).is_err(),
    })
}

pub(crate) fn command_argv(
    tool_id: &str,
    arguments: &serde_json::Value,
    config: &WorkspaceHandlerConfig,
) -> Result<Vec<String>, ToolHandlerError> {
    let Some(command) = arguments.get("command") else {
        return Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: "arguments must include 'command'".to_string(),
        });
    };
    match command {
        serde_json::Value::Array(items) => {
            if arguments.get("shell").is_some() || arguments.get("login").is_some() {
                return Err(ToolHandlerError::InvalidArguments {
                    tool_id: tool_id.to_string(),
                    reason: "shell and login are valid only for a command string".to_string(),
                });
            }
            let argv = items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    item.as_str()
                        .filter(|part| !part.is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| ToolHandlerError::InvalidArguments {
                            tool_id: tool_id.to_string(),
                            reason: format!("command[{index}] must be a non-empty string"),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if argv.is_empty() {
                return Err(ToolHandlerError::InvalidArguments {
                    tool_id: tool_id.to_string(),
                    reason: "command array must be non-empty".to_string(),
                });
            }
            Ok(argv)
        }
        serde_json::Value::String(command) if !command.trim().is_empty() => {
            let default_shell = config.default_shell.as_deref().ok_or_else(|| {
                ToolHandlerError::InvalidArguments {
                    tool_id: tool_id.to_string(),
                    reason: "handler_config is missing 'default_shell'".to_string(),
                }
            })?;
            let allowed_shells = config.allowed_shells.as_ref().ok_or_else(|| {
                ToolHandlerError::InvalidArguments {
                    tool_id: tool_id.to_string(),
                    reason: "handler_config is missing 'allowed_shells'".to_string(),
                }
            })?;
            let requested_shell = optional_string_argument(tool_id, arguments, "shell")?
                .unwrap_or_else(|| default_shell.to_string());
            if !allowed_shells
                .iter()
                .any(|allowed| allowed == &requested_shell)
            {
                return Err(ToolHandlerError::InvalidArguments {
                    tool_id: tool_id.to_string(),
                    reason: format!(
                        "shell '{}' is outside the descriptor-owned allowed_shells",
                        requested_shell
                    ),
                });
            }
            let allow_login_shell =
                config
                    .allow_login_shell
                    .ok_or_else(|| ToolHandlerError::InvalidArguments {
                        tool_id: tool_id.to_string(),
                        reason: "handler_config is missing 'allow_login_shell'".to_string(),
                    })?;
            let login =
                optional_bool_argument(tool_id, arguments, "login")?.unwrap_or(allow_login_shell);
            if login && !allow_login_shell {
                return Err(ToolHandlerError::InvalidArguments {
                    tool_id: tool_id.to_string(),
                    reason: "login shell is disabled by the tool descriptor".to_string(),
                });
            }
            Ok(vec![
                requested_shell,
                if login { "-lc" } else { "-c" }.to_string(),
                command.to_string(),
            ])
        }
        serde_json::Value::String(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: "command string must be non-empty".to_string(),
        }),
        _ => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: "command must be a shell string or exact argv array".to_string(),
        }),
    }
}

fn required_handler_u64(
    tool_id: &str,
    value: Option<u64>,
    name: &str,
) -> Result<u64, ToolHandlerError> {
    value.ok_or_else(|| ToolHandlerError::InvalidArguments {
        tool_id: tool_id.to_string(),
        reason: format!("handler_config is missing '{name}'"),
    })
}

fn required_handler_usize(
    tool_id: &str,
    value: Option<usize>,
    name: &str,
) -> Result<usize, ToolHandlerError> {
    value.ok_or_else(|| ToolHandlerError::InvalidArguments {
        tool_id: tool_id.to_string(),
        reason: format!("handler_config is missing '{name}'"),
    })
}

fn bounded_optional_u64_argument(
    tool_id: &str,
    arguments: &serde_json::Value,
    name: &str,
    minimum: u64,
    maximum: u64,
) -> Result<Option<u64>, ToolHandlerError> {
    match arguments.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .filter(|value| (minimum..=maximum).contains(value))
            .map(Some)
            .ok_or_else(|| ToolHandlerError::InvalidArguments {
                tool_id: tool_id.to_string(),
                reason: format!(
                    "arguments field '{name}' must be an integer between {minimum} and {maximum}"
                ),
            }),
        Some(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!(
                "arguments field '{name}' must be an integer between {minimum} and {maximum}"
            ),
        }),
    }
}

fn bounded_optional_usize_argument(
    tool_id: &str,
    arguments: &serde_json::Value,
    name: &str,
    minimum: usize,
    maximum: usize,
) -> Result<Option<usize>, ToolHandlerError> {
    match arguments.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(value)) => value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| (minimum..=maximum).contains(value))
            .map(Some)
            .ok_or_else(|| ToolHandlerError::InvalidArguments {
                tool_id: tool_id.to_string(),
                reason: format!(
                    "arguments field '{name}' must be an integer between {minimum} and {maximum}"
                ),
            }),
        Some(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!(
                "arguments field '{name}' must be an integer between {minimum} and {maximum}"
            ),
        }),
    }
}

fn optional_bool_argument(
    tool_id: &str,
    arguments: &serde_json::Value,
    name: &str,
) -> Result<Option<bool>, ToolHandlerError> {
    match arguments.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ToolHandlerError::InvalidArguments {
            tool_id: tool_id.to_string(),
            reason: format!("arguments field '{name}' must be a boolean"),
        }),
    }
}
