#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use fs2::FileExt as LockFileExt;
use octopus_descriptors::InterfaceId;
use octopus_interfaces::{
    ArtifactBindingPayload, InterfaceArtifactFile, InterfaceDescriptor, InterfaceInvocationEnvelope,
};
use octopus_ledger::ContentHash;
use octopus_tool_door_runtime::room::{admitted_tool, emission_root, TOOL_CALL_LABEL};
use octopus_tool_door_runtime::{
    ToolProcessEnvironment, ToolProcessError, ToolProcessHandler, ToolProcessInvocation,
    ToolProcessIo,
};
use octopus_tool_handlers::{ToolCallArtifact, ToolResultArtifact, ToolResultError};
use octopus_tool_registry::ToolDescriptor;
use rodio::Source;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const AUDIO_DRIVER_REF: &str = "audio_device";
const AUDIO_WORKER_ENVIRONMENT_KIND: &str = "audio_device_session";
const AUDIO_PROTOCOL_VERSION: u32 = 1;
const MAX_ABSOLUTE_AUDIO_BYTES: usize = 512 * 1024 * 1024;
const MAX_ABSOLUTE_DURATION_MS: u64 = 30 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeviceDirection {
    Input,
    Output,
}

#[derive(Clone)]
enum AudioInterfaceMode {
    Microphone(MicrophoneInterfaceConfig),
    Speaker(SpeakerInterfaceConfig),
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct MicrophoneInterfaceConfig {
    protocol_version: u32,
    device_direction: DeviceDirection,
    devices_label: String,
    audio_label: String,
    max_devices: usize,
    max_device_name_bytes: usize,
    max_audio_bytes: usize,
    max_capture_duration_ms: u64,
    min_capture_duration_ms: u64,
    max_silence_duration_ms: u64,
    min_silence_threshold_dbfs: f32,
    max_silence_threshold_dbfs: f32,
    default_silence_threshold_dbfs: f32,
    min_sample_rate: u32,
    max_sample_rate: u32,
    max_channels: u16,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpeakerInterfaceConfig {
    protocol_version: u32,
    device_direction: DeviceDirection,
    devices_label: String,
    max_devices: usize,
    max_device_name_bytes: usize,
    max_audio_bytes: usize,
    max_playback_duration_ms: u64,
    min_volume: f32,
    max_volume: f32,
    min_speed: f32,
    max_speed: f32,
    accepted_formats: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioOperation {
    operation: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDevicesArguments {
    #[serde(default)]
    host: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureArguments {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
    max_duration_ms: u64,
    #[serde(default)]
    minimum_duration_ms: Option<u64>,
    #[serde(default)]
    stop_after_silence_ms: Option<u64>,
    #[serde(default)]
    silence_threshold_dbfs: Option<f32>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaybackArguments {
    audio_artifact_id: String,
    format: String,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    device_name: Option<String>,
    #[serde(default)]
    volume: Option<f32>,
    #[serde(default)]
    speed: Option<f32>,
    #[serde(default)]
    max_duration_ms: Option<u64>,
}

#[derive(Clone, Debug)]
struct ValidatedCaptureRequest {
    host: Option<String>,
    device_name: Option<String>,
    max_duration: Duration,
    minimum_duration: Duration,
    silence: Option<SilenceStop>,
}

#[derive(Clone, Copy, Debug)]
struct SilenceStop {
    duration: Duration,
    threshold_dbfs: f32,
}

#[derive(Clone, Debug)]
struct ValidatedPlaybackRequest {
    source_artifact_id: String,
    format: String,
    host: Option<String>,
    device_name: Option<String>,
    volume: f32,
    speed: f32,
    max_duration: Duration,
}

#[derive(Clone, Debug, Serialize)]
struct DeviceInventory {
    direction: DeviceDirection,
    default_host: String,
    hosts: Vec<HostInventory>,
}

#[derive(Clone, Debug, Serialize)]
struct HostInventory {
    id: String,
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_device_error: Option<String>,
    devices: Vec<DeviceEvidence>,
}

#[derive(Clone, Debug, Serialize)]
struct DeviceEvidence {
    name: String,
    is_default: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_config: Option<StreamConfigEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct StreamConfigEvidence {
    channels: u16,
    sample_rate: u32,
    sample_format: String,
    buffer_size: BufferSizeEvidence,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BufferSizeEvidence {
    Unknown,
    Range { min: u32, max: u32 },
}

#[derive(Clone, Debug)]
struct CaptureOutput {
    wav_bytes: Vec<u8>,
    host: String,
    device: String,
    sample_rate: u32,
    channels: u16,
    frame_count: u64,
    duration_ms: u64,
    stop_reason: &'static str,
    peak_dbfs: f32,
    rms_dbfs: f32,
}

#[derive(Clone, Debug)]
struct PlaybackOutput {
    host: String,
    device: String,
    elapsed_ms: u64,
    decoded_duration_ms: Option<u64>,
    outcome: &'static str,
}

#[derive(Clone, Debug)]
struct AudioFailure {
    class: &'static str,
    detail: String,
}

impl AudioFailure {
    fn new(class: &'static str, detail: impl Into<String>) -> Self {
        Self {
            class,
            detail: detail.into(),
        }
    }
}

trait AudioBackend: Send + Sync {
    fn list_devices(
        &self,
        direction: DeviceDirection,
        host_filter: Option<&str>,
        max_devices: usize,
        max_device_name_bytes: usize,
    ) -> Result<DeviceInventory, AudioFailure>;

    fn capture(
        &self,
        request: &ValidatedCaptureRequest,
        config: &MicrophoneInterfaceConfig,
        interface_var_dir: &Path,
    ) -> Result<CaptureOutput, AudioFailure>;

    fn play(
        &self,
        audio: Vec<u8>,
        request: &ValidatedPlaybackRequest,
        config: &SpeakerInterfaceConfig,
        interface_var_dir: &Path,
    ) -> Result<PlaybackOutput, AudioFailure>;
}

#[derive(Default)]
struct SystemAudioBackend;

struct AudioDeviceProcess {
    interface: InterfaceId,
    interface_var_dir: PathBuf,
    max_manifest_bytes: u64,
    max_tool_call_bytes: u64,
    mode: AudioInterfaceMode,
    backend: Arc<dyn AudioBackend>,
}

impl ToolProcessHandler for AudioDeviceProcess {
    fn substrate_family(&self) -> &str {
        "audio_device"
    }

    fn execute<'a>(
        &'a self,
        invocation: ToolProcessInvocation,
        _io: ToolProcessIo,
    ) -> Pin<
        Box<dyn Future<Output = Result<Vec<InterfaceArtifactFile>, ToolProcessError>> + Send + 'a>,
    > {
        Box::pin(async move { self.execute_invocation(invocation).await })
    }
}

impl AudioDeviceProcess {
    async fn execute_invocation(
        &self,
        invocation: ToolProcessInvocation,
    ) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
        let call = decode_call(
            &invocation.descriptor,
            &invocation.envelope,
            self.max_manifest_bytes,
            self.max_tool_call_bytes,
        )
        .map_err(ToolProcessError::Capability)?;
        let operation =
            parse_operation(&invocation.descriptor).map_err(ToolProcessError::Capability)?;
        let output_root =
            emission_root(&invocation.envelope).map_err(ToolProcessError::Capability)?;
        match (&self.mode, operation.as_str()) {
            (AudioInterfaceMode::Microphone(config), "list_input_devices") => {
                let arguments: ListDevicesArguments =
                    decode_arguments(&invocation.descriptor, &call)?;
                self.execute_list_devices(
                    DeviceDirection::Input,
                    arguments,
                    config.max_devices,
                    config.max_device_name_bytes,
                    &config.devices_label,
                    &invocation.descriptor,
                    call.correlation_id,
                    output_root,
                )
                .await
            }
            (AudioInterfaceMode::Microphone(config), "capture_input") => {
                let arguments: CaptureArguments = decode_arguments(&invocation.descriptor, &call)?;
                self.execute_capture(
                    arguments,
                    config.clone(),
                    &invocation.descriptor,
                    call.correlation_id,
                    output_root,
                )
                .await
            }
            (AudioInterfaceMode::Speaker(config), "list_output_devices") => {
                let arguments: ListDevicesArguments =
                    decode_arguments(&invocation.descriptor, &call)?;
                self.execute_list_devices(
                    DeviceDirection::Output,
                    arguments,
                    config.max_devices,
                    config.max_device_name_bytes,
                    &config.devices_label,
                    &invocation.descriptor,
                    call.correlation_id,
                    output_root,
                )
                .await
            }
            (AudioInterfaceMode::Speaker(config), "play_output") => {
                let arguments: PlaybackArguments = decode_arguments(&invocation.descriptor, &call)?;
                let bound =
                    bound_artifact(&invocation.envelope, arguments.audio_artifact_id.trim())
                        .map_err(ToolProcessError::Capability)?;
                let audio = bound
                    .read_bounded(config.max_audio_bytes as u64)
                    .map_err(|error| ToolProcessError::Capability(error.to_string()))?;
                if audio.is_empty() || audio.len() > config.max_audio_bytes {
                    return Err(ToolProcessError::Capability(format!(
                        "bound audio artifact '{}' byte count {} is outside 1..={}",
                        arguments.audio_artifact_id,
                        audio.len(),
                        config.max_audio_bytes
                    )));
                }
                self.execute_playback(
                    arguments,
                    audio.into_owned(),
                    config.clone(),
                    &invocation.descriptor,
                    call.correlation_id,
                    output_root,
                )
                .await
            }
            _ => Err(ToolProcessError::Capability(format!(
                "tool '{}' operation '{}' is incompatible with interface '{}'",
                invocation.descriptor.id, operation, self.interface
            ))),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_list_devices(
        &self,
        direction: DeviceDirection,
        arguments: ListDevicesArguments,
        max_devices: usize,
        max_device_name_bytes: usize,
        devices_label: &str,
        descriptor: &ToolDescriptor,
        correlation_id: Option<String>,
        output_root: PathBuf,
    ) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
        let host =
            optional_trimmed(arguments.host, "host").map_err(ToolProcessError::Capability)?;
        let backend = Arc::clone(&self.backend);
        let inventory = tokio::task::spawn_blocking(move || {
            backend.list_devices(
                direction,
                host.as_deref(),
                max_devices,
                max_device_name_bytes,
            )
        })
        .await
        .map_err(|error| {
            ToolProcessError::Capability(format!("audio inventory task failed: {error}"))
        })?;
        match inventory {
            Ok(inventory) => {
                let inventory_payload = serde_json::to_value(&inventory).map_err(|error| {
                    ToolProcessError::Capability(format!("audio inventory did not encode: {error}"))
                })?;
                let inventory_artifact =
                    materialize_json(&output_root, devices_label, &inventory_payload)
                        .map_err(ToolProcessError::Capability)?;
                let inventory_ref = artifact_identity(&inventory_artifact, self.max_manifest_bytes)
                    .map_err(ToolProcessError::Capability)?;
                let body = json!({
                    "direction": direction,
                    "host_count": inventory.hosts.len(),
                    "device_count": inventory.hosts.iter().map(|host| host.devices.len()).sum::<usize>(),
                    "artifact": inventory_ref,
                });
                let result =
                    ToolResultArtifact::success(descriptor.id.as_str(), correlation_id, body);
                let result = materialize_tool_result(&output_root, &result)
                    .map_err(ToolProcessError::Capability)?;
                Ok(vec![inventory_artifact, result])
            }
            Err(failure) => materialize_failure_result(
                &output_root,
                descriptor.id.as_str(),
                correlation_id,
                None,
                failure,
            )
            .map(|artifact| vec![artifact])
            .map_err(ToolProcessError::Capability),
        }
    }

    async fn execute_capture(
        &self,
        arguments: CaptureArguments,
        config: MicrophoneInterfaceConfig,
        descriptor: &ToolDescriptor,
        correlation_id: Option<String>,
        output_root: PathBuf,
    ) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
        let request =
            validate_capture_request(arguments, &config).map_err(ToolProcessError::Capability)?;
        let evidence_request = request.clone();
        let backend = Arc::clone(&self.backend);
        let interface_var_dir = self.interface_var_dir.clone();
        let task_config = config.clone();
        let output = tokio::task::spawn_blocking(move || {
            backend.capture(&request, &task_config, &interface_var_dir)
        })
        .await
        .map_err(|error| {
            ToolProcessError::Capability(format!("audio capture task failed: {error}"))
        })?;
        match output {
            Ok(capture) => {
                let audio = InterfaceArtifactFile::write(
                    &output_root,
                    config.audio_label.trim(),
                    &capture.wav_bytes,
                )
                .map_err(|error| {
                    ToolProcessError::Capability(format!(
                        "captured audio artifact could not be written: {error}"
                    ))
                })?;
                let audio_ref = artifact_identity(&audio, self.max_manifest_bytes)
                    .map_err(ToolProcessError::Capability)?;
                let body = json!({
                    "format": "wav",
                    "pcm": "s16le",
                    "host": capture.host,
                    "device": capture.device,
                    "sample_rate": capture.sample_rate,
                    "channels": capture.channels,
                    "frame_count": capture.frame_count,
                    "duration_ms": capture.duration_ms,
                    "stop_reason": capture.stop_reason,
                    "requested_max_duration_ms": evidence_request.max_duration.as_millis(),
                    "requested_minimum_duration_ms": evidence_request.minimum_duration.as_millis(),
                    "requested_silence_stop_ms": evidence_request.silence.map(|silence| silence.duration.as_millis()),
                    "requested_silence_threshold_dbfs": evidence_request.silence.map(|silence| silence.threshold_dbfs),
                    "peak_dbfs": capture.peak_dbfs,
                    "rms_dbfs": capture.rms_dbfs,
                    "byte_count": capture.wav_bytes.len(),
                    "artifact": audio_ref,
                });
                let result =
                    ToolResultArtifact::success(descriptor.id.as_str(), correlation_id, body);
                let result = materialize_tool_result(&output_root, &result)
                    .map_err(ToolProcessError::Capability)?;
                Ok(vec![audio, result])
            }
            Err(failure) => materialize_failure_result(
                &output_root,
                descriptor.id.as_str(),
                correlation_id,
                None,
                failure,
            )
            .map(|artifact| vec![artifact])
            .map_err(ToolProcessError::Capability),
        }
    }

    async fn execute_playback(
        &self,
        arguments: PlaybackArguments,
        audio: Vec<u8>,
        config: SpeakerInterfaceConfig,
        descriptor: &ToolDescriptor,
        correlation_id: Option<String>,
        output_root: PathBuf,
    ) -> Result<Vec<InterfaceArtifactFile>, ToolProcessError> {
        let source_artifact_id = arguments.audio_artifact_id.clone();
        let request =
            validate_playback_request(arguments, &config).map_err(ToolProcessError::Capability)?;
        let evidence_request = request.clone();
        let backend = Arc::clone(&self.backend);
        let interface_var_dir = self.interface_var_dir.clone();
        let task_config = config.clone();
        let output = tokio::task::spawn_blocking(move || {
            backend.play(audio, &request, &task_config, &interface_var_dir)
        })
        .await
        .map_err(|error| {
            ToolProcessError::Capability(format!("audio playback task failed: {error}"))
        })?;
        match output {
            Ok(playback) => {
                let body = json!({
                    "outcome": playback.outcome,
                    "source_audio_artifact_id": source_artifact_id,
                    "format": evidence_request.format,
                    "volume": evidence_request.volume,
                    "speed": evidence_request.speed,
                    "max_duration_ms": evidence_request.max_duration.as_millis(),
                    "host": playback.host,
                    "device": playback.device,
                    "elapsed_ms": playback.elapsed_ms,
                    "decoded_duration_ms": playback.decoded_duration_ms,
                });
                let result = if playback.outcome == "completed" {
                    ToolResultArtifact::success(descriptor.id.as_str(), correlation_id, body)
                } else {
                    let mut details = serde_json::Map::new();
                    details.insert("operation_body".to_string(), body);
                    ToolResultArtifact::failure(
                        descriptor.id.as_str(),
                        correlation_id,
                        ToolResultError::new(
                            "audio_playback_incomplete",
                            format!("audio playback ended with outcome '{}'", playback.outcome),
                        )
                        .with_details(details),
                    )
                };
                materialize_tool_result(&output_root, &result)
                    .map(|artifact| vec![artifact])
                    .map_err(ToolProcessError::Capability)
            }
            Err(failure) => materialize_failure_result(
                &output_root,
                descriptor.id.as_str(),
                correlation_id,
                Some(json!({
                    "source_audio_artifact_id": source_artifact_id,
                    "format": evidence_request.format,
                    "volume": evidence_request.volume,
                    "speed": evidence_request.speed,
                    "max_duration_ms": evidence_request.max_duration.as_millis(),
                })),
                failure,
            )
            .map(|artifact| vec![artifact])
            .map_err(ToolProcessError::Capability),
        }
    }
}

impl AudioBackend for SystemAudioBackend {
    fn list_devices(
        &self,
        direction: DeviceDirection,
        host_filter: Option<&str>,
        max_devices: usize,
        max_device_name_bytes: usize,
    ) -> Result<DeviceInventory, AudioFailure> {
        list_system_devices(direction, host_filter, max_devices, max_device_name_bytes)
    }

    fn capture(
        &self,
        request: &ValidatedCaptureRequest,
        config: &MicrophoneInterfaceConfig,
        interface_var_dir: &Path,
    ) -> Result<CaptureOutput, AudioFailure> {
        capture_system_audio(request, config, interface_var_dir)
    }

    fn play(
        &self,
        audio: Vec<u8>,
        request: &ValidatedPlaybackRequest,
        config: &SpeakerInterfaceConfig,
        interface_var_dir: &Path,
    ) -> Result<PlaybackOutput, AudioFailure> {
        play_system_audio(audio, request, config, interface_var_dir)
    }
}

pub async fn run_audio_device_tool_process() -> Result<(), String> {
    let env = ToolProcessEnvironment::from_env()?;
    let resolved = env.resolved_interface_descriptor()?;
    let limits = env.host_io_limits;
    let mode = load_audio_interface_mode(resolved, &env.interface)?;
    octopus_tool_door_runtime::run_tool_process(Arc::new(AudioDeviceProcess {
        interface: env.interface,
        interface_var_dir: env.var_dir,
        max_manifest_bytes: limits.max_manifest_bytes,
        max_tool_call_bytes: limits.max_control_frame_bytes,
        mode,
        backend: Arc::new(SystemAudioBackend),
    }))
    .await
}

fn load_audio_interface_mode(
    resolved: InterfaceDescriptor,
    interface: &InterfaceId,
) -> Result<AudioInterfaceMode, String> {
    if resolved.id != *interface {
        return Err(format!(
            "resolved audio interface '{}' does not match process interface '{}'",
            resolved.id, interface
        ));
    }
    if resolved.interface_driver_ref != AUDIO_DRIVER_REF {
        return Err(format!(
            "audio interface '{}' resolves driver '{}', expected '{}'",
            resolved.id, resolved.interface_driver_ref, AUDIO_DRIVER_REF
        ));
    }
    let config = resolved.interface_driver_config.ok_or_else(|| {
        format!(
            "audio interface '{}' resolves no interface_driver_config",
            interface
        )
    })?;
    let direction: DeviceDirection = serde_json::from_value(
        config
            .private_config
            .get("device_direction")
            .cloned()
            .ok_or_else(|| {
                format!(
                    "audio interface '{}' private_config.device_direction is missing",
                    interface
                )
            })?,
    )
    .map_err(|error| {
        format!(
            "audio interface '{}' device_direction is malformed: {error}",
            interface
        )
    })?;
    let metadata = Value::Object(config.private_config);
    match direction {
        DeviceDirection::Input => {
            let config: MicrophoneInterfaceConfig =
                serde_json::from_value(metadata).map_err(|error| {
                    format!(
                        "microphone interface '{}' config is malformed: {error}",
                        interface
                    )
                })?;
            validate_microphone_config(&config)?;
            Ok(AudioInterfaceMode::Microphone(config))
        }
        DeviceDirection::Output => {
            let config: SpeakerInterfaceConfig =
                serde_json::from_value(metadata).map_err(|error| {
                    format!(
                        "speaker interface '{}' config is malformed: {error}",
                        interface
                    )
                })?;
            validate_speaker_config(&config)?;
            Ok(AudioInterfaceMode::Speaker(config))
        }
    }
}

fn validate_microphone_config(config: &MicrophoneInterfaceConfig) -> Result<(), String> {
    if config.protocol_version != AUDIO_PROTOCOL_VERSION
        || config.device_direction != DeviceDirection::Input
    {
        return Err(
            "microphone interface protocol_version or device_direction is invalid".to_string(),
        );
    }
    validate_labels([config.devices_label.as_str(), config.audio_label.as_str()])?;
    validate_common_limits(
        config.max_devices,
        config.max_device_name_bytes,
        config.max_audio_bytes,
        config.max_capture_duration_ms,
    )?;
    if config.min_capture_duration_ms == 0
        || config.min_capture_duration_ms > config.max_capture_duration_ms
        || config.max_silence_duration_ms == 0
        || !config.min_silence_threshold_dbfs.is_finite()
        || !config.max_silence_threshold_dbfs.is_finite()
        || !config.default_silence_threshold_dbfs.is_finite()
        || config.min_silence_threshold_dbfs >= config.max_silence_threshold_dbfs
        || config.default_silence_threshold_dbfs < config.min_silence_threshold_dbfs
        || config.default_silence_threshold_dbfs > config.max_silence_threshold_dbfs
        || config.min_sample_rate == 0
        || config.min_sample_rate > config.max_sample_rate
        || config.max_channels == 0
    {
        return Err("microphone interface numeric limits are inconsistent".to_string());
    }
    Ok(())
}

fn validate_speaker_config(config: &SpeakerInterfaceConfig) -> Result<(), String> {
    if config.protocol_version != AUDIO_PROTOCOL_VERSION
        || config.device_direction != DeviceDirection::Output
    {
        return Err(
            "speaker interface protocol_version or device_direction is invalid".to_string(),
        );
    }
    validate_labels([config.devices_label.as_str()])?;
    validate_common_limits(
        config.max_devices,
        config.max_device_name_bytes,
        config.max_audio_bytes,
        config.max_playback_duration_ms,
    )?;
    if !config.min_volume.is_finite()
        || !config.max_volume.is_finite()
        || !config.min_speed.is_finite()
        || !config.max_speed.is_finite()
        || config.min_volume < 0.0
        || config.min_volume > config.max_volume
        || config.min_speed <= 0.0
        || config.min_speed > config.max_speed
    {
        return Err("speaker interface gain or speed limits are inconsistent".to_string());
    }
    let mut formats = BTreeSet::new();
    for format in &config.accepted_formats {
        require_trimmed("accepted_formats[]", format)?;
        if !format
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
            || !formats.insert(format)
        {
            return Err(format!(
                "speaker interface contains invalid or duplicate format '{format}'"
            ));
        }
    }
    if formats.is_empty() {
        return Err("speaker interface accepted_formats must not be empty".to_string());
    }
    Ok(())
}

fn validate_common_limits(
    max_devices: usize,
    max_device_name_bytes: usize,
    max_audio_bytes: usize,
    max_duration_ms: u64,
) -> Result<(), String> {
    if max_devices == 0
        || max_devices > 4096
        || max_device_name_bytes == 0
        || max_device_name_bytes > 64 * 1024
        || max_audio_bytes <= 44
        || max_audio_bytes > MAX_ABSOLUTE_AUDIO_BYTES
        || max_duration_ms == 0
        || max_duration_ms > MAX_ABSOLUTE_DURATION_MS
    {
        return Err("audio interface common limits are outside absolute bounds".to_string());
    }
    Ok(())
}

fn validate_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    let mut unique = BTreeSet::new();
    for label in labels {
        require_trimmed("artifact label", label)?;
        if !unique.insert(label) {
            return Err(format!("audio interface repeats artifact label '{label}'"));
        }
    }
    Ok(())
}

fn parse_operation(descriptor: &ToolDescriptor) -> Result<String, String> {
    let operation: AudioOperation = serde_json::from_value(descriptor.handler_config.clone())
        .map_err(|error| {
            format!(
                "tool '{}' handler_config is malformed: {error}",
                descriptor.id
            )
        })?;
    require_trimmed("handler_config.operation", &operation.operation)?;
    match operation.operation.as_str() {
        "list_input_devices" | "capture_input" | "list_output_devices" | "play_output" => {
            Ok(operation.operation)
        }
        other => Err(format!(
            "tool '{}' declares unknown audio-device operation '{other}'",
            descriptor.id
        )),
    }
}

fn decode_arguments<T: serde::de::DeserializeOwned>(
    descriptor: &ToolDescriptor,
    call: &ToolCallArtifact,
) -> Result<T, ToolProcessError> {
    serde_json::from_value(call.arguments.clone()).map_err(|error| {
        ToolProcessError::Capability(format!(
            "tool '{}' arguments are malformed: {error}",
            descriptor.id
        ))
    })
}

fn decode_call(
    descriptor: &ToolDescriptor,
    envelope: &InterfaceInvocationEnvelope,
    max_manifest_bytes: u64,
    max_tool_call_bytes: u64,
) -> Result<ToolCallArtifact, String> {
    validate_audio_tool_code_world(descriptor)?;
    let artifact = envelope
        .read_artifact_bounded(max_manifest_bytes, max_tool_call_bytes)
        .map_err(|error| error.to_string())?;
    if artifact.manifest.label != TOOL_CALL_LABEL {
        return Err(format!(
            "audio interface expected manifest label '{TOOL_CALL_LABEL}', got '{}'",
            artifact.manifest.label
        ));
    }
    let call: ToolCallArtifact = serde_json::from_slice(&artifact.payload_bytes)
        .map_err(|error| format!("audio tool.call artifact is malformed: {error}"))?;
    if call.tool != descriptor.id.as_str() {
        return Err(format!(
            "tool descriptor '{}' does not match tool.call '{}'",
            descriptor.id, call.tool
        ));
    }
    admitted_tool("audio interface", envelope, &call).map_err(|error| error.to_string())?;
    Ok(call)
}

fn validate_audio_tool_code_world(descriptor: &ToolDescriptor) -> Result<(), String> {
    match descriptor.supported_worker_environment_kinds.as_slice() {
        [kind] if kind.as_str() == AUDIO_WORKER_ENVIRONMENT_KIND => Ok(()),
        _ => Err(format!(
            "audio tool '{}' must belong exclusively to worker-environment kind '{}'",
            descriptor.id, AUDIO_WORKER_ENVIRONMENT_KIND
        )),
    }
}

fn bound_artifact<'a>(
    envelope: &'a InterfaceInvocationEnvelope,
    id: &str,
) -> Result<&'a ArtifactBindingPayload, String> {
    require_trimmed("audio_artifact_id", id)?;
    let mut matches = envelope
        .artifact_binding_payloads
        .iter()
        .filter(|binding| binding.artifact.id.as_str() == id);
    let artifact = matches
        .next()
        .ok_or_else(|| format!("audio artifact '{id}' is not bound to this run"))?;
    if matches.next().is_some() {
        return Err(format!("audio artifact id '{id}' is bound more than once"));
    }
    Ok(artifact)
}

fn validate_capture_request(
    arguments: CaptureArguments,
    config: &MicrophoneInterfaceConfig,
) -> Result<ValidatedCaptureRequest, String> {
    let host = optional_trimmed(arguments.host, "host")?;
    let device_name = optional_trimmed(arguments.device_name, "device_name")?;
    if device_name
        .as_ref()
        .is_some_and(|name| name.len() > config.max_device_name_bytes)
    {
        return Err(format!(
            "device_name exceeds interface max_device_name_bytes {}",
            config.max_device_name_bytes
        ));
    }
    if arguments.max_duration_ms < config.min_capture_duration_ms
        || arguments.max_duration_ms > config.max_capture_duration_ms
    {
        return Err(format!(
            "max_duration_ms must be within {}..={} for this microphone interface",
            config.min_capture_duration_ms, config.max_capture_duration_ms
        ));
    }
    let minimum_duration_ms = arguments
        .minimum_duration_ms
        .unwrap_or(config.min_capture_duration_ms);
    if minimum_duration_ms < config.min_capture_duration_ms
        || minimum_duration_ms > arguments.max_duration_ms
    {
        return Err(format!(
            "minimum_duration_ms must be within {}..={} for this capture",
            config.min_capture_duration_ms, arguments.max_duration_ms
        ));
    }
    let silence = match arguments.stop_after_silence_ms {
        Some(duration_ms) => {
            if duration_ms < config.min_capture_duration_ms
                || duration_ms > config.max_silence_duration_ms
            {
                return Err(format!(
                    "stop_after_silence_ms must be within {}..={} for this microphone interface",
                    config.min_capture_duration_ms, config.max_silence_duration_ms
                ));
            }
            let threshold = arguments
                .silence_threshold_dbfs
                .unwrap_or(config.default_silence_threshold_dbfs);
            if !threshold.is_finite()
                || threshold < config.min_silence_threshold_dbfs
                || threshold > config.max_silence_threshold_dbfs
            {
                return Err(format!(
                    "silence_threshold_dbfs must be within {}..={}",
                    config.min_silence_threshold_dbfs, config.max_silence_threshold_dbfs
                ));
            }
            Some(SilenceStop {
                duration: Duration::from_millis(duration_ms),
                threshold_dbfs: threshold,
            })
        }
        None => {
            if arguments.silence_threshold_dbfs.is_some() {
                return Err("silence_threshold_dbfs requires stop_after_silence_ms".to_string());
            }
            None
        }
    };
    Ok(ValidatedCaptureRequest {
        host,
        device_name,
        max_duration: Duration::from_millis(arguments.max_duration_ms),
        minimum_duration: Duration::from_millis(minimum_duration_ms),
        silence,
    })
}

fn validate_playback_request(
    arguments: PlaybackArguments,
    config: &SpeakerInterfaceConfig,
) -> Result<ValidatedPlaybackRequest, String> {
    let source_artifact_id = arguments.audio_artifact_id.trim().to_string();
    require_trimmed("audio_artifact_id", &source_artifact_id)?;
    let format = arguments.format.trim().to_ascii_lowercase();
    if !config
        .accepted_formats
        .iter()
        .any(|accepted| accepted == &format)
    {
        return Err(format!(
            "audio format '{format}' is not accepted by this speaker interface"
        ));
    }
    let host = optional_trimmed(arguments.host, "host")?;
    let device_name = optional_trimmed(arguments.device_name, "device_name")?;
    if device_name
        .as_ref()
        .is_some_and(|name| name.len() > config.max_device_name_bytes)
    {
        return Err(format!(
            "device_name exceeds interface max_device_name_bytes {}",
            config.max_device_name_bytes
        ));
    }
    let volume = arguments.volume.unwrap_or(1.0);
    let speed = arguments.speed.unwrap_or(1.0);
    if !volume.is_finite() || volume < config.min_volume || volume > config.max_volume {
        return Err(format!(
            "volume must be within {}..={}",
            config.min_volume, config.max_volume
        ));
    }
    if !speed.is_finite() || speed < config.min_speed || speed > config.max_speed {
        return Err(format!(
            "speed must be within {}..={}",
            config.min_speed, config.max_speed
        ));
    }
    let max_duration_ms = arguments
        .max_duration_ms
        .unwrap_or(config.max_playback_duration_ms);
    if max_duration_ms == 0 || max_duration_ms > config.max_playback_duration_ms {
        return Err(format!(
            "max_duration_ms must be within 1..={} for this speaker interface",
            config.max_playback_duration_ms
        ));
    }
    Ok(ValidatedPlaybackRequest {
        source_artifact_id,
        format,
        host,
        device_name,
        volume,
        speed,
        max_duration: Duration::from_millis(max_duration_ms),
    })
}

fn list_system_devices(
    direction: DeviceDirection,
    host_filter: Option<&str>,
    max_devices: usize,
    max_device_name_bytes: usize,
) -> Result<DeviceInventory, AudioFailure> {
    let default_host = cpal::default_host().id().name().to_string();
    let available = cpal::available_hosts();
    let selected = match host_filter {
        Some(filter) => {
            let matches = available
                .iter()
                .copied()
                .filter(|id| id.name() == filter)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(AudioFailure::new(
                    "unknown_host",
                    format!("audio host '{filter}' is not uniquely available"),
                ));
            }
            matches
        }
        None => available,
    };
    let mut hosts = Vec::new();
    let mut device_count = 0_usize;
    for host_id in selected {
        let host_name = host_id.name().to_string();
        let host = match cpal::host_from_id(host_id) {
            Ok(host) => host,
            Err(error) => {
                hosts.push(HostInventory {
                    id: host_name,
                    available: false,
                    error: Some(error.to_string()),
                    default_device: None,
                    default_device_error: None,
                    devices: Vec::new(),
                });
                continue;
            }
        };
        let default = match direction {
            DeviceDirection::Input => host.default_input_device(),
            DeviceDirection::Output => host.default_output_device(),
        };
        let (default_device, default_device_error) = match default {
            Some(device) => match device.name() {
                Ok(name) => (Some(name), None),
                Err(error) => (None, Some(error.to_string())),
            },
            None => (None, None),
        };
        let devices = match direction {
            DeviceDirection::Input => host.input_devices(),
            DeviceDirection::Output => host.output_devices(),
        };
        let mut evidence = Vec::new();
        match devices {
            Ok(devices) => {
                for device in devices {
                    device_count = device_count.saturating_add(1);
                    if device_count > max_devices {
                        return Err(AudioFailure::new(
                            "device_limit",
                            format!("audio device count exceeds interface ceiling {max_devices}"),
                        ));
                    }
                    let name = device.name().map_err(|error| {
                        AudioFailure::new(
                            "device_identity",
                            format!("an audio device name cannot be read: {error}"),
                        )
                    })?;
                    if name.len() > max_device_name_bytes {
                        return Err(AudioFailure::new(
                            "device_identity",
                            format!(
                                "audio device name byte count {} exceeds interface ceiling {}",
                                name.len(),
                                max_device_name_bytes
                            ),
                        ));
                    }
                    let config = match direction {
                        DeviceDirection::Input => device.default_input_config(),
                        DeviceDirection::Output => device.default_output_config(),
                    };
                    let (default_config, config_error) = match config {
                        Ok(config) => (Some(stream_config_evidence(&config)), None),
                        Err(error) => (None, Some(error.to_string())),
                    };
                    evidence.push(DeviceEvidence {
                        is_default: default_device.as_deref() == Some(name.as_str()),
                        name,
                        default_config,
                        config_error,
                    });
                }
            }
            Err(error) => {
                hosts.push(HostInventory {
                    id: host_name,
                    available: false,
                    error: Some(error.to_string()),
                    default_device,
                    default_device_error,
                    devices: Vec::new(),
                });
                continue;
            }
        }
        evidence.sort_by(|left, right| left.name.cmp(&right.name));
        hosts.push(HostInventory {
            id: host_name,
            available: true,
            error: None,
            default_device,
            default_device_error,
            devices: evidence,
        });
    }
    hosts.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(DeviceInventory {
        direction,
        default_host,
        hosts,
    })
}

fn stream_config_evidence(config: &cpal::SupportedStreamConfig) -> StreamConfigEvidence {
    let buffer_size = match config.buffer_size() {
        cpal::SupportedBufferSize::Unknown => BufferSizeEvidence::Unknown,
        cpal::SupportedBufferSize::Range { min, max } => BufferSizeEvidence::Range {
            min: *min,
            max: *max,
        },
    };
    StreamConfigEvidence {
        channels: config.channels(),
        sample_rate: config.sample_rate().0,
        sample_format: format!("{:?}", config.sample_format()).to_ascii_lowercase(),
        buffer_size,
    }
}

fn resolve_host(filter: Option<&str>) -> Result<(cpal::Host, String), AudioFailure> {
    match filter {
        None => {
            let host = cpal::default_host();
            let name = host.id().name().to_string();
            Ok((host, name))
        }
        Some(filter) => {
            let matches = cpal::available_hosts()
                .into_iter()
                .filter(|id| id.name() == filter)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(AudioFailure::new(
                    "unknown_host",
                    format!("audio host '{filter}' is not uniquely available"),
                ));
            }
            let host = cpal::host_from_id(matches[0]).map_err(|error| {
                AudioFailure::new(
                    "host_unavailable",
                    format!("audio host '{filter}' cannot be opened: {error}"),
                )
            })?;
            Ok((host, filter.to_string()))
        }
    }
}

fn resolve_device(
    host: &cpal::Host,
    direction: DeviceDirection,
    requested_name: Option<&str>,
    max_name_bytes: usize,
) -> Result<(cpal::Device, String), AudioFailure> {
    let device = match requested_name {
        None => match direction {
            DeviceDirection::Input => host.default_input_device(),
            DeviceDirection::Output => host.default_output_device(),
        }
        .ok_or_else(|| {
            AudioFailure::new("device_unavailable", "audio host has no default device")
        })?,
        Some(requested) => {
            let devices = match direction {
                DeviceDirection::Input => host.input_devices(),
                DeviceDirection::Output => host.output_devices(),
            }
            .map_err(|error| {
                AudioFailure::new(
                    "device_enumeration",
                    format!("audio devices cannot be enumerated: {error}"),
                )
            })?;
            let mut matches = Vec::new();
            for device in devices {
                let name = device.name().map_err(|error| {
                    AudioFailure::new(
                        "device_identity",
                        format!("an audio device name cannot be read: {error}"),
                    )
                })?;
                if name == requested {
                    matches.push(device);
                }
            }
            if matches.len() != 1 {
                return Err(AudioFailure::new(
                    "device_unavailable",
                    format!("audio device '{requested}' is not uniquely available"),
                ));
            }
            matches.remove(0)
        }
    };
    let name = device.name().map_err(|error| {
        AudioFailure::new(
            "device_identity",
            format!("selected audio device name cannot be read: {error}"),
        )
    })?;
    if name.len() > max_name_bytes {
        return Err(AudioFailure::new(
            "device_identity",
            format!(
                "selected audio device name byte count {} exceeds interface ceiling {}",
                name.len(),
                max_name_bytes
            ),
        ));
    }
    Ok((device, name))
}

fn acquire_operation_lock(interface_var_dir: &Path, name: &str) -> Result<File, AudioFailure> {
    std::fs::create_dir_all(interface_var_dir).map_err(|error| {
        AudioFailure::new(
            "state",
            format!(
                "audio interface var directory '{}' cannot be created: {error}",
                interface_var_dir.display()
            ),
        )
    })?;
    let path = interface_var_dir.join(format!("{name}.lock"));
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            AudioFailure::new(
                "state",
                format!(
                    "audio operation lock '{}' cannot be opened: {error}",
                    path.display()
                ),
            )
        })?;
    LockFileExt::try_lock_exclusive(&file).map_err(|error| {
        AudioFailure::new(
            "busy",
            format!("audio interface already has an active {name} operation: {error}"),
        )
    })?;
    Ok(file)
}

#[derive(Debug)]
enum CaptureSignal {
    DurationLimit,
    Silence,
    StreamError(String),
}

struct CaptureState {
    samples: Vec<i16>,
    max_samples: usize,
    channels: usize,
    frames_seen: u64,
    silent_frames: u64,
    minimum_frames: u64,
    silence_frames: Option<u64>,
    silence_amplitude: f32,
    finished: bool,
}

fn capture_system_audio(
    request: &ValidatedCaptureRequest,
    config: &MicrophoneInterfaceConfig,
    interface_var_dir: &Path,
) -> Result<CaptureOutput, AudioFailure> {
    let _lock = acquire_operation_lock(interface_var_dir, "capture")?;
    let (host, host_name) = resolve_host(request.host.as_deref())?;
    let (device, device_name) = resolve_device(
        &host,
        DeviceDirection::Input,
        request.device_name.as_deref(),
        config.max_device_name_bytes,
    )?;
    let supported = device.default_input_config().map_err(|error| {
        AudioFailure::new(
            "input_config",
            format!("input device '{device_name}' has no usable default config: {error}"),
        )
    })?;
    let sample_rate = supported.sample_rate().0;
    let channels = supported.channels();
    if sample_rate < config.min_sample_rate
        || sample_rate > config.max_sample_rate
        || channels == 0
        || channels > config.max_channels
    {
        return Err(AudioFailure::new(
            "input_config",
            format!(
                "input config {sample_rate}Hz/{channels}ch is outside interface limits {}..={}Hz/1..={}ch",
                config.min_sample_rate, config.max_sample_rate, config.max_channels
            ),
        ));
    }
    let requested_frames = duration_frames(request.max_duration, sample_rate)?;
    let requested_samples = usize::try_from(requested_frames)
        .ok()
        .and_then(|frames| frames.checked_mul(usize::from(channels)))
        .ok_or_else(|| AudioFailure::new("size_limit", "capture sample count overflows usize"))?;
    let projected_bytes = requested_samples
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(44))
        .ok_or_else(|| AudioFailure::new("size_limit", "capture WAV size overflows usize"))?;
    if projected_bytes > config.max_audio_bytes {
        return Err(AudioFailure::new(
            "size_limit",
            format!(
                "requested capture may require {projected_bytes} bytes, exceeding interface ceiling {}",
                config.max_audio_bytes
            ),
        ));
    }
    let minimum_frames = duration_frames(request.minimum_duration, sample_rate)?;
    let silence_frames = request
        .silence
        .map(|silence| duration_frames(silence.duration, sample_rate))
        .transpose()?;
    let silence_amplitude = request
        .silence
        .map(|silence| 10.0_f32.powf(silence.threshold_dbfs / 20.0))
        .unwrap_or(0.0);
    let state = Arc::new(Mutex::new(CaptureState {
        samples: Vec::with_capacity(requested_samples),
        max_samples: requested_samples,
        channels: usize::from(channels),
        frames_seen: 0,
        silent_frames: 0,
        minimum_frames,
        silence_frames,
        silence_amplitude,
        finished: false,
    }));
    let (signal_tx, signal_rx) = std::sync::mpsc::sync_channel(4);
    let stream_config: cpal::StreamConfig = supported.clone().into();
    let stream = build_capture_stream(
        &device,
        &stream_config,
        supported.sample_format(),
        Arc::clone(&state),
        signal_tx,
    )?;
    stream.play().map_err(|error| {
        AudioFailure::new(
            "stream_start",
            format!("input stream on '{device_name}' could not start: {error}"),
        )
    })?;
    let signal = match signal_rx.recv_timeout(request.max_duration) {
        Ok(signal) => signal,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => CaptureSignal::DurationLimit,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            return Err(AudioFailure::new(
                "stream_state",
                "input stream completion channel disconnected",
            ));
        }
    };
    drop(stream);
    if let CaptureSignal::StreamError(error) = signal {
        return Err(AudioFailure::new(
            "stream_runtime",
            format!("input stream on '{device_name}' failed: {error}"),
        ));
    }
    let mut state = state
        .lock()
        .map_err(|_| AudioFailure::new("stream_state", "input sample state lock is poisoned"))?;
    let complete_samples = state.samples.len() - (state.samples.len() % usize::from(channels));
    state.samples.truncate(complete_samples);
    let frame_count = u64::try_from(complete_samples / usize::from(channels))
        .map_err(|_| AudioFailure::new("size_limit", "captured frame count exceeds u64"))?;
    if frame_count < minimum_frames {
        return Err(AudioFailure::new(
            "insufficient_audio",
            format!(
                "capture produced {frame_count} frames before the required minimum {minimum_frames}"
            ),
        ));
    }
    let (peak_dbfs, rms_dbfs) = pcm_metrics(&state.samples);
    let wav_bytes = encode_pcm16_wav(&state.samples, channels, sample_rate)?;
    if wav_bytes.len() > config.max_audio_bytes {
        return Err(AudioFailure::new(
            "size_limit",
            format!(
                "captured WAV byte count {} exceeds interface ceiling {}",
                wav_bytes.len(),
                config.max_audio_bytes
            ),
        ));
    }
    let duration_ms = frame_count.saturating_mul(1000) / u64::from(sample_rate);
    Ok(CaptureOutput {
        wav_bytes,
        host: host_name,
        device: device_name,
        sample_rate,
        channels,
        frame_count,
        duration_ms,
        stop_reason: match signal {
            CaptureSignal::Silence => "silence",
            CaptureSignal::DurationLimit => "duration_limit",
            CaptureSignal::StreamError(_) => unreachable!(),
        },
        peak_dbfs,
        rms_dbfs,
    })
}

fn build_capture_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    state: Arc<Mutex<CaptureState>>,
    signal: std::sync::mpsc::SyncSender<CaptureSignal>,
) -> Result<cpal::Stream, AudioFailure> {
    match format {
        cpal::SampleFormat::I8 => build_capture_stream_for::<i8>(device, config, state, signal),
        cpal::SampleFormat::I16 => build_capture_stream_for::<i16>(device, config, state, signal),
        cpal::SampleFormat::I24 => {
            build_capture_stream_for::<cpal::I24>(device, config, state, signal)
        }
        cpal::SampleFormat::I32 => build_capture_stream_for::<i32>(device, config, state, signal),
        cpal::SampleFormat::I64 => build_capture_stream_for::<i64>(device, config, state, signal),
        cpal::SampleFormat::U8 => build_capture_stream_for::<u8>(device, config, state, signal),
        cpal::SampleFormat::U16 => build_capture_stream_for::<u16>(device, config, state, signal),
        cpal::SampleFormat::U32 => build_capture_stream_for::<u32>(device, config, state, signal),
        cpal::SampleFormat::U64 => build_capture_stream_for::<u64>(device, config, state, signal),
        cpal::SampleFormat::F32 => build_capture_stream_for::<f32>(device, config, state, signal),
        cpal::SampleFormat::F64 => build_capture_stream_for::<f64>(device, config, state, signal),
        other => Err(AudioFailure::new(
            "unsupported_sample_format",
            format!("input sample format '{other}' is not supported"),
        )),
    }
}

fn build_capture_stream_for<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    state: Arc<Mutex<CaptureState>>,
    signal: std::sync::mpsc::SyncSender<CaptureSignal>,
) -> Result<cpal::Stream, AudioFailure>
where
    T: SizedSample + Sample + Copy + Send + 'static,
    i16: FromSample<T>,
    f32: FromSample<T>,
{
    let error_signal = signal.clone();
    device
        .build_input_stream(
            config,
            move |data: &[T], _| capture_samples(data, &state, &signal),
            move |error| {
                let _ = error_signal.try_send(CaptureSignal::StreamError(error.to_string()));
            },
            None,
        )
        .map_err(|error| {
            AudioFailure::new(
                "stream_open",
                format!("input stream could not be built: {error}"),
            )
        })
}

fn capture_samples<T>(
    data: &[T],
    state: &Arc<Mutex<CaptureState>>,
    signal: &std::sync::mpsc::SyncSender<CaptureSignal>,
) where
    T: Sample + Copy,
    i16: FromSample<T>,
    f32: FromSample<T>,
{
    let Ok(mut state) = state.lock() else {
        let _ = signal.try_send(CaptureSignal::StreamError(
            "input sample state lock is poisoned".to_string(),
        ));
        return;
    };
    if state.finished {
        return;
    }
    let remaining = state.max_samples.saturating_sub(state.samples.len());
    let accepted = data.len().min(remaining);
    let accepted = accepted - (accepted % state.channels);
    for sample in &data[..accepted] {
        state.samples.push(i16::from_sample(*sample));
    }
    for frame in data[..accepted].chunks_exact(state.channels) {
        let sum = frame
            .iter()
            .map(|sample| {
                let value = f32::from_sample(*sample);
                value * value
            })
            .sum::<f32>();
        let rms = (sum / state.channels as f32).sqrt();
        state.frames_seen = state.frames_seen.saturating_add(1);
        if state.silence_frames.is_some() {
            if rms <= state.silence_amplitude {
                state.silent_frames = state.silent_frames.saturating_add(1);
            } else {
                state.silent_frames = 0;
            }
        }
    }
    if state.frames_seen >= state.minimum_frames
        && state
            .silence_frames
            .is_some_and(|required| state.silent_frames >= required)
    {
        state.finished = true;
        let _ = signal.try_send(CaptureSignal::Silence);
    } else if state.samples.len() >= state.max_samples {
        state.finished = true;
        let _ = signal.try_send(CaptureSignal::DurationLimit);
    }
}

fn duration_frames(duration: Duration, sample_rate: u32) -> Result<u64, AudioFailure> {
    let millis = u64::try_from(duration.as_millis()).map_err(|_| {
        AudioFailure::new("duration_limit", "audio duration exceeds u64 milliseconds")
    })?;
    millis
        .checked_mul(u64::from(sample_rate))
        .and_then(|value| value.checked_add(999))
        .map(|value| value / 1000)
        .ok_or_else(|| AudioFailure::new("duration_limit", "audio frame count overflows u64"))
}

fn encode_pcm16_wav(
    samples: &[i16],
    channels: u16,
    sample_rate: u32,
) -> Result<Vec<u8>, AudioFailure> {
    if channels == 0 || samples.len() % usize::from(channels) != 0 {
        return Err(AudioFailure::new(
            "wav_contract",
            "PCM sample count is not aligned to the channel count",
        ));
    }
    let data_bytes = samples
        .len()
        .checked_mul(2)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| AudioFailure::new("wav_contract", "PCM data exceeds WAV u32 size"))?;
    let riff_size = data_bytes
        .checked_add(36)
        .ok_or_else(|| AudioFailure::new("wav_contract", "WAV RIFF size overflows u32"))?;
    let byte_rate = sample_rate
        .checked_mul(u32::from(channels))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| AudioFailure::new("wav_contract", "WAV byte rate overflows u32"))?;
    let block_align = channels
        .checked_mul(2)
        .ok_or_else(|| AudioFailure::new("wav_contract", "WAV block align overflows u16"))?;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(wav)
}

fn pcm_metrics(samples: &[i16]) -> (f32, f32) {
    if samples.is_empty() {
        return (-96.0, -96.0);
    }
    let mut peak = 0.0_f64;
    let mut square_sum = 0.0_f64;
    for sample in samples {
        let normalized = f64::from(*sample) / 32768.0;
        peak = peak.max(normalized.abs());
        square_sum += normalized * normalized;
    }
    let rms = (square_sum / samples.len() as f64).sqrt();
    (amplitude_dbfs(peak), amplitude_dbfs(rms))
}

fn amplitude_dbfs(amplitude: f64) -> f32 {
    if amplitude <= 0.0 {
        -96.0
    } else {
        (20.0 * amplitude.log10()).max(-96.0) as f32
    }
}

fn play_system_audio(
    audio: Vec<u8>,
    request: &ValidatedPlaybackRequest,
    config: &SpeakerInterfaceConfig,
    interface_var_dir: &Path,
) -> Result<PlaybackOutput, AudioFailure> {
    let _lock = acquire_operation_lock(interface_var_dir, "playback")?;
    let byte_len = u64::try_from(audio.len())
        .map_err(|_| AudioFailure::new("size_limit", "bound audio byte count exceeds u64"))?;
    let decoder = rodio::Decoder::builder()
        .with_data(Cursor::new(audio))
        .with_byte_len(byte_len)
        .with_hint(&request.format)
        .build()
        .map_err(|error| {
            AudioFailure::new(
                "decode",
                format!(
                    "bound artifact '{}' cannot be decoded as '{}': {error}",
                    request.source_artifact_id, request.format
                ),
            )
        })?;
    let decoded_duration = decoder.total_duration();
    let effective_duration =
        decoded_duration.map(|duration| duration.div_f64(f64::from(request.speed)));
    if effective_duration.is_some_and(|duration| duration > request.max_duration) {
        return Err(AudioFailure::new(
            "duration_limit",
            format!(
                "decoded playback duration {}ms exceeds this call's {}ms limit",
                effective_duration
                    .map(|duration| duration.as_millis())
                    .unwrap_or_default(),
                request.max_duration.as_millis()
            ),
        ));
    }
    let (host, host_name) = resolve_host(request.host.as_deref())?;
    let (device, device_name) = resolve_device(
        &host,
        DeviceDirection::Output,
        request.device_name.as_deref(),
        config.max_device_name_bytes,
    )?;
    let (error_tx, error_rx) = std::sync::mpsc::sync_channel(4);
    let stream = rodio::OutputStreamBuilder::from_device(device)
        .map_err(|error| {
            AudioFailure::new(
                "output_config",
                format!("output device '{device_name}' cannot be configured: {error}"),
            )
        })?
        .with_error_callback(move |error| {
            let _ = error_tx.try_send(error.to_string());
        })
        .open_stream()
        .map_err(|error| {
            AudioFailure::new(
                "stream_open",
                format!("output stream on '{device_name}' could not open: {error}"),
            )
        })?;
    let sink = Arc::new(rodio::Sink::connect_new(stream.mixer()));
    sink.set_volume(request.volume);
    sink.set_speed(request.speed);
    sink.append(decoder);
    let (complete_tx, complete_rx) = std::sync::mpsc::sync_channel(1);
    let waiting_sink = Arc::clone(&sink);
    let waiter = std::thread::Builder::new()
        .name("octopus-audio-playback-wait".to_string())
        .spawn(move || {
            waiting_sink.sleep_until_end();
            let _ = complete_tx.send(());
        })
        .map_err(|error| {
            AudioFailure::new(
                "stream_state",
                format!("playback completion waiter could not start: {error}"),
            )
        })?;
    let started = Instant::now();
    let outcome = match complete_rx.recv_timeout(request.max_duration) {
        Ok(()) => "completed",
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            sink.stop();
            "stopped_at_duration_limit"
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            sink.stop();
            return Err(AudioFailure::new(
                "stream_state",
                "playback completion waiter disconnected",
            ));
        }
    };
    waiter
        .join()
        .map_err(|_| AudioFailure::new("stream_state", "playback completion waiter panicked"))?;
    if let Ok(error) = error_rx.try_recv() {
        return Err(AudioFailure::new(
            "stream_runtime",
            format!("output stream on '{device_name}' failed: {error}"),
        ));
    }
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let decoded_duration_ms =
        decoded_duration.and_then(|duration| u64::try_from(duration.as_millis()).ok());
    drop(stream);
    Ok(PlaybackOutput {
        host: host_name,
        device: device_name,
        elapsed_ms,
        decoded_duration_ms,
        outcome,
    })
}

fn optional_trimmed(value: Option<String>, field: &str) -> Result<Option<String>, String> {
    match value {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(format!("{field} must not be blank"));
            }
            Ok(Some(trimmed.to_string()))
        }
        None => Ok(None),
    }
}

fn require_trimmed(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value {
        return Err(format!("{field} must be non-empty and trimmed"));
    }
    Ok(())
}

fn materialize_json(
    root: &Path,
    label: &str,
    value: &Value,
) -> Result<InterfaceArtifactFile, String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("audio result artifact did not encode: {error}"))?;
    InterfaceArtifactFile::write(root, label, &bytes)
        .map_err(|error| format!("audio result artifact could not be written: {error}"))
}

fn materialize_failure_result(
    root: &Path,
    tool: &str,
    correlation_id: Option<String>,
    context: Option<Value>,
    failure: AudioFailure,
) -> Result<InterfaceArtifactFile, String> {
    let mut error = ToolResultError::new(failure.class, failure.detail);
    if let Some(context) = context {
        let mut details = serde_json::Map::new();
        details.insert("context".to_string(), context);
        error = error.with_details(details);
    }
    let result = ToolResultArtifact::failure(tool, correlation_id, error);
    materialize_tool_result(root, &result)
}

fn materialize_tool_result(
    root: &Path,
    result: &ToolResultArtifact,
) -> Result<InterfaceArtifactFile, String> {
    materialize_json(
        root,
        &result.label,
        &serde_json::to_value(result)
            .map_err(|error| format!("audio tool result did not encode: {error}"))?,
    )
}

fn artifact_identity(
    artifact: &InterfaceArtifactFile,
    max_manifest_bytes: u64,
) -> Result<Value, String> {
    let bounded = artifact
        .read_manifest_bounded(max_manifest_bytes)
        .map_err(|error| format!("audio artifact manifest is invalid: {error}"))?;
    let content = ContentHash::of_bytes(&bounded.manifest_bytes).to_hex();
    Ok(json!({
        "id": format!("{}:{content}", bounded.manifest.label),
        "content": content,
        "payload_content": bounded.manifest.payload_content_hash,
    }))
}

#[cfg(test)]
mod tests;
