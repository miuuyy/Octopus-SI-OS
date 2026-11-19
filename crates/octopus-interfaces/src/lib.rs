use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use octopus_descriptors::{ArtifactId, Descriptor};
pub use octopus_descriptors::{InterfaceId, RunId};
pub use octopus_ledger::{
    ArtifactRef, AttachedInterfaceBinding, CheckpointAdmissionMetadata,
    CheckpointBindingPolicySnapshot, DeliveryMetadata, DeliveryVerdict, GrantProjection,
    InterfaceToolAffordance, InvocationMetadata, Mark, MarkClass, MovementMetadata, RefusedReason,
    RequiredVerifiedArtifact, RuntimeInvocationMetadata, VerificationFailureMetadata,
};
use octopus_ledger::{BlobStore, ContentHash};
use octopus_policy::InterfacePolicyDeclaration;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod worker_interface_registry;
pub use worker_interface_registry::{
    dangling_worker_interface_refs, interface_is_worker_pin, load_worker_interface_policies,
    scan_worker_interface_policies, DanglingWorkerInterfaceRef, WorkerInterfacePolicyScan,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceType {
    World,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterfaceRuntimeDisposition {
    ExternalReader,
    RuntimeRelay,
    Inert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Inbound,
    Outbound,
    Bidirectional,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Role(pub String);

mod host_root_paths;
pub use host_root_paths::{additional_host_root_paths, expand_host_root_authority};

pub const RETIRED_INTERFACE_REQUEST_WORKER_METADATA_FIELD: &str = "worker_metadata";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceDriverConfig {
    #[serde(default)]
    pub private_config: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub residual_ambient: Option<serde_json::Value>,
    /// notes2 2026-07-06 / 2026-07-11 — per-instance root coordinates for this
    /// interface folder: named host root-slot ⇒ absolute path, overriding the
    /// machine `host.json` defaults for THIS door instance. A typed first-class
    /// field (schema-contract), never fished out of the amorphous
    /// `residual_ambient` blob by a string pointer path.
    #[serde(default)]
    pub host_slots: BTreeMap<String, HostSlotOverride>,
    #[serde(default)]
    pub transport: Option<InterfaceTransportConfig>,
    #[serde(default)]
    pub supported_reasoning_efforts: Vec<String>,
}

/// A per-instance root-slot override: the absolute path the door interface binds
/// a named host root-slot to. `path` is validated absolute where it is resolved
/// (interface-transport merge).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSlotOverride {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceTransportConfig {
    pub base_url: String,
    #[serde(default)]
    pub base_url_env: Option<String>,
    #[serde(default)]
    pub wire: Option<String>,
    #[serde(default)]
    pub auth_env_key: Option<String>,
    #[serde(default)]
    pub auth_secret_ref: Option<String>,
    #[serde(default)]
    pub auth_header: Option<String>,
    #[serde(default)]
    pub auth_value_prefix: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub request_timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_response_bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceDescriptor {
    pub id: InterfaceId,
    pub interface_type: InterfaceType,
    pub role: Role,
    pub direction: Direction,
    pub source_names: Vec<String>,
    #[serde(default)]
    pub interface_driver_ref: String,
    #[serde(default)]
    pub interface_driver_config: Option<InterfaceDriverConfig>,
    #[serde(default)]
    pub interface_policy: InterfacePolicyDeclaration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_of: Option<InterfaceId>,
}

impl Descriptor for InterfaceDescriptor {
    const KIND: &'static str = "interface";
    const SCHEMA_VERSION: u32 = 1;
    const CONTRACT_VERSION: u32 = 1;
    fn declared_name(&self) -> Option<&str> {
        Some(self.id.as_str())
    }

    fn validate(&self) -> Result<(), String> {
        self.validate_source_names()?;
        if let Some(original) = &self.copy_of {
            if original.as_str().trim().is_empty()
                || original.as_str().trim() != original.as_str()
                || original == &self.id
            {
                return Err(format!(
                    "interface '{}' declares an invalid copy_of '{}'",
                    self.id.as_str(),
                    original.as_str()
                ));
            }
        }
        octopus_policy::validate_interface_policy_declaration(&self.interface_policy).map_err(
            |error| {
                format!(
                    "interface '{}' declares invalid interface policy: {error}",
                    self.id.as_str()
                )
            },
        )?;
        Ok(())
    }
}

impl InterfaceDescriptor {
    pub fn type_label(&self) -> &'static str {
        self.interface_type.as_label()
    }

    pub fn has_interface_driver(&self) -> bool {
        !self.interface_driver_ref.trim().is_empty()
    }

    pub fn source_classifications(&self) -> Vec<String> {
        self.source_names
            .iter()
            .map(|source| format!("{}.{}({})", self.type_label(), source, self.id.as_str()))
            .collect()
    }

    pub fn source_route_prefixes(&self) -> Vec<String> {
        self.source_names
            .iter()
            .map(|source| format!("{}.{}", self.type_label(), source))
            .collect()
    }

    fn validate_source_names(&self) -> Result<(), String> {
        if self.source_names.is_empty() {
            return Err(format!(
                "interface '{}' must declare at least one source_names entry",
                self.id.as_str()
            ));
        }
        for source in &self.source_names {
            validate_source_name(self.id.as_str(), self.interface_type, source)?;
        }
        Ok(())
    }
}

pub fn form_interface_runtime_disposition(
    instance: &InterfaceDescriptor,
    interface_root: &Path,
    worker_policy_loaded: bool,
) -> Result<InterfaceRuntimeDisposition, String> {
    let life_declared = interface_life_declared(instance, interface_root);
    let external_reader = instance.has_interface_driver();
    if life_declared && !external_reader {
        return Err(format!(
            "interface '{}' declares executable life but resolves no interface driver",
            instance.id
        ));
    }
    let worker_pin = interface_is_worker_pin(instance);
    if instance.role.0 == "worker" && !worker_pin {
        return Err(format!(
            "interface '{}' declares the worker role but is not a System interface",
            instance.id
        ));
    }

    match (worker_pin, worker_policy_loaded, external_reader) {
        (true, true, false) => Ok(InterfaceRuntimeDisposition::RuntimeRelay),
        (true, true, true) => Err(format!(
            "worker interface '{}' carries a policy ceiling and must remain a zero-code runtime relay, but it also declares an external reader",
            instance.id
        )),
        (true, false, _) => Err(format!(
            "worker interface '{}' has no loaded policy ceiling",
            instance.id
        )),
        (false, true, _) => Err(format!(
            "non-worker interface '{}' was paired with a worker policy ceiling",
            instance.id
        )),
        (false, false, true) => Ok(InterfaceRuntimeDisposition::ExternalReader),
        (false, false, false) => Ok(InterfaceRuntimeDisposition::Inert),
    }
}

fn interface_life_declared(instance: &InterfaceDescriptor, interface_root: &Path) -> bool {
    interface_root
        .join(instance.id.as_str())
        .join("life.json")
        .is_file()
}

impl InterfaceType {
    pub fn as_label(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::System => "system",
        }
    }
}

fn validate_source_name(
    interface_id: &str,
    interface_type: InterfaceType,
    source: &str,
) -> Result<(), String> {
    if source.trim() != source || source.is_empty() {
        return Err(format!(
            "interface '{interface_id}' source_names entries must be non-empty trimmed source ids"
        ));
    }
    if source.starts_with("world.") || source.starts_with("system.") {
        return Err(format!(
            "interface '{interface_id}' source_name '{source}' repeats the interface_type axis; \
             declare source-only '{}' so the classification renders as '{}.{}({interface_id})'",
            source
                .strip_prefix("world.")
                .or_else(|| source.strip_prefix("system."))
                .unwrap_or(source),
            interface_type.as_label(),
            source
                .strip_prefix("world.")
                .or_else(|| source.strip_prefix("system."))
                .unwrap_or(source)
        ));
    }
    for segment in source.split('.') {
        if segment.is_empty() {
            return Err(format!(
                "interface '{interface_id}' source_name '{source}' has an empty dot segment"
            ));
        }
        if !segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(format!(
                "interface '{interface_id}' source_name '{source}' must use lowercase ascii \
                 dot segments"
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterfaceArtifactFile {
    manifest_path: PathBuf,
    payload_cache_root: Option<PathBuf>,
    captured_snapshot: Option<Arc<CapturedInterfaceArtifactSnapshot>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedInterfaceArtifactSnapshot {
    manifest_bytes: Vec<u8>,
    payload_bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct LoadedInterfaceArtifactFile {
    pub artifact: ArtifactRef,
    pub manifest: InterfaceArtifactManifest,
    pub payload_bytes: Vec<u8>,
    pub payload_content_hash: ContentHash,
}

#[derive(Clone, Debug)]
pub struct BoundedInterfaceArtifactManifest {
    pub manifest: InterfaceArtifactManifest,
    pub manifest_bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct BoundedInterfaceArtifactSnapshot {
    pub manifest: InterfaceArtifactManifest,
    pub manifest_bytes: Vec<u8>,
    pub payload_bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct PinnedInterfaceArtifactManifest {
    pub artifact: ArtifactRef,
    pub manifest: InterfaceArtifactManifest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceArtifactManifest {
    pub label: String,
    pub payload_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_content_hash: Option<String>,
}

#[derive(Debug, Error)]
pub enum InterfaceArtifactFileError {
    #[error("interface artifact manifest path is empty")]
    EmptyManifestPath,
    #[error("interface artifact manifest path must be absolute: {0}")]
    RelativeManifestPath(String),
    #[error("failed to create interface artifact directory '{path}': {source}")]
    CreateDir {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to canonicalize interface artifact directory '{path}': {source}")]
    CanonicalizeDir {
        path: String,
        source: std::io::Error,
    },
    #[error("interface artifact manifest label is empty")]
    EmptyLabel,
    #[error("failed to write interface artifact payload '{path}': {source}")]
    WritePayload {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to encode interface artifact manifest for label '{label}': {source}")]
    EncodeManifest {
        label: String,
        source: serde_json::Error,
    },
    #[error("failed to write interface artifact manifest '{path}': {source}")]
    WriteManifest {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to read interface artifact manifest '{path}': {source}")]
    ReadManifest {
        path: String,
        source: std::io::Error,
    },
    #[error(
        "interface artifact manifest '{path}' is {size} bytes, above the caller limit {maximum}"
    )]
    ManifestTooLarge {
        path: String,
        size: u64,
        maximum: u64,
    },
    #[error("interface artifact manifest '{path}' is not valid JSON: {source}")]
    ParseManifest {
        path: String,
        source: serde_json::Error,
    },
    #[error("interface artifact manifest '{path}' has empty label")]
    ManifestEmptyLabel { path: String },
    #[error("interface artifact manifest '{path}' has empty payload_path")]
    ManifestEmptyPayloadPath { path: String },
    #[error("interface artifact manifest '{path}' payload_path must be absolute: {payload_path}")]
    ManifestRelativePayloadPath { path: String, payload_path: String },
    #[error("interface artifact manifest '{path}' has an invalid payload content hash")]
    ManifestInvalidPayloadHash { path: String },
    #[error("failed to read interface artifact payload '{path}': {source}")]
    ReadPayload {
        path: String,
        source: std::io::Error,
    },
    #[error(
        "interface artifact payload '{path}' is {size} bytes, above the caller limit {maximum}"
    )]
    PayloadTooLarge {
        path: String,
        size: u64,
        maximum: u64,
    },
    #[error(
        "interface artifact payload '{path}' content hash mismatch: manifest={expected} actual={actual}"
    )]
    PayloadHashMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error(
        "interface artifact {surface} '{path}' is not a stable regular-file carrier: {detail}"
    )]
    UnsafeCarrier {
        surface: &'static str,
        path: String,
        detail: &'static str,
    },
    #[error("interface artifact carrier '{path}' was handed to the runtime before capture")]
    UncapturedCarrier { path: String },
}

impl InterfaceArtifactFile {
    pub fn new(manifest_path: impl Into<PathBuf>) -> Result<Self, InterfaceArtifactFileError> {
        let manifest_path = manifest_path.into();
        if manifest_path.as_os_str().is_empty() {
            return Err(InterfaceArtifactFileError::EmptyManifestPath);
        }
        if !manifest_path.is_absolute() {
            return Err(InterfaceArtifactFileError::RelativeManifestPath(
                manifest_path.display().to_string(),
            ));
        }
        Ok(Self {
            manifest_path,
            payload_cache_root: None,
            captured_snapshot: None,
        })
    }

    pub fn from_captured_snapshot(
        manifest_path: impl Into<PathBuf>,
        manifest_bytes: Vec<u8>,
        payload_bytes: Vec<u8>,
    ) -> Result<Self, InterfaceArtifactFileError> {
        let mut artifact = Self::new(manifest_path)?;
        let manifest = parse_interface_artifact_manifest(&manifest_bytes, &artifact.manifest_path)?;
        validate_manifest_payload_hash_bytes(&manifest, &payload_bytes)?;
        artifact.captured_snapshot = Some(Arc::new(CapturedInterfaceArtifactSnapshot {
            manifest_bytes,
            payload_bytes,
        }));
        Ok(artifact)
    }

    pub fn with_payload_cache_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.payload_cache_root = Some(root.into());
        self
    }

    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    pub fn read_manifest(&self) -> Result<InterfaceArtifactManifest, InterfaceArtifactFileError> {
        let manifest_bytes = self.read_manifest_bytes()?;
        let manifest = parse_interface_artifact_manifest(&manifest_bytes, &self.manifest_path)?;
        self.validate_payload_hash_for_manifest(&manifest)?;
        Ok(manifest)
    }

    pub fn read_payload_bytes(&self) -> Result<Vec<u8>, InterfaceArtifactFileError> {
        let manifest_bytes = self.read_manifest_bytes()?;
        let manifest = parse_interface_artifact_manifest(&manifest_bytes, &self.manifest_path)?;
        let payload_bytes = self.read_payload_for_manifest(&manifest)?;
        validate_manifest_payload_hash_bytes(&manifest, &payload_bytes)?;
        Ok(payload_bytes)
    }

    pub fn read_payload_bytes_bounded(
        &self,
        maximum_bytes: u64,
    ) -> Result<Vec<u8>, InterfaceArtifactFileError> {
        let manifest_bytes = self.read_manifest_bytes()?;
        let manifest = parse_interface_artifact_manifest(&manifest_bytes, &self.manifest_path)?;
        let payload_bytes = self.read_payload_for_manifest_bounded(&manifest, maximum_bytes)?;
        validate_manifest_payload_hash_bytes(&manifest, &payload_bytes)?;
        Ok(payload_bytes)
    }

    pub fn read_manifest_bounded(
        &self,
        maximum_manifest_bytes: u64,
    ) -> Result<BoundedInterfaceArtifactManifest, InterfaceArtifactFileError> {
        let manifest_bytes = self.read_manifest_bytes_bounded(maximum_manifest_bytes)?;
        let manifest = parse_interface_artifact_manifest(&manifest_bytes, &self.manifest_path)?;
        Ok(BoundedInterfaceArtifactManifest {
            manifest,
            manifest_bytes,
        })
    }

    pub fn read_bounded(
        &self,
        maximum_manifest_bytes: u64,
        maximum_payload_bytes: u64,
    ) -> Result<BoundedInterfaceArtifactSnapshot, InterfaceArtifactFileError> {
        let bounded_manifest = self.read_manifest_bounded(maximum_manifest_bytes)?;
        let payload_bytes = self
            .read_payload_for_manifest_bounded(&bounded_manifest.manifest, maximum_payload_bytes)?;
        validate_manifest_payload_hash_bytes(&bounded_manifest.manifest, &payload_bytes)?;
        Ok(BoundedInterfaceArtifactSnapshot {
            manifest: bounded_manifest.manifest,
            manifest_bytes: bounded_manifest.manifest_bytes,
            payload_bytes,
        })
    }

    pub fn capture_bounded(
        &self,
        maximum_manifest_bytes: u64,
        maximum_payload_bytes: u64,
    ) -> Result<Self, InterfaceArtifactFileError> {
        let snapshot = self.read_bounded(maximum_manifest_bytes, maximum_payload_bytes)?;
        Self::from_captured_snapshot(
            self.manifest_path.clone(),
            snapshot.manifest_bytes,
            snapshot.payload_bytes,
        )
    }

    pub fn require_captured(&self) -> Result<Self, InterfaceArtifactFileError> {
        if self.captured_snapshot.is_none() {
            return Err(InterfaceArtifactFileError::UncapturedCarrier {
                path: self.manifest_path.display().to_string(),
            });
        }
        Ok(self.clone())
    }

    pub fn rewrite_payload(
        &self,
        payload_bytes: &[u8],
    ) -> Result<Self, InterfaceArtifactFileError> {
        let mut manifest = self.read_manifest()?;
        if self.captured_snapshot.is_some() {
            manifest.payload_content_hash = Some(ContentHash::of_bytes(payload_bytes).to_hex());
            let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|source| {
                InterfaceArtifactFileError::EncodeManifest {
                    label: manifest.label.clone(),
                    source,
                }
            })?;
            return Self::from_captured_snapshot(
                self.manifest_path.clone(),
                manifest_bytes,
                payload_bytes.to_vec(),
            );
        }
        let root = self.manifest_path.parent().ok_or_else(|| {
            InterfaceArtifactFileError::RelativeManifestPath(
                self.manifest_path.display().to_string(),
            )
        })?;
        Self::write(root, &manifest.label, payload_bytes)
    }

    pub fn write(
        root: impl AsRef<Path>,
        label: &str,
        payload_bytes: &[u8],
    ) -> Result<Self, InterfaceArtifactFileError> {
        let label = label.trim();
        if label.is_empty() {
            return Err(InterfaceArtifactFileError::EmptyLabel);
        }
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|source| InterfaceArtifactFileError::CreateDir {
            path: root.display().to_string(),
            source,
        })?;
        let root =
            root.canonicalize()
                .map_err(|source| InterfaceArtifactFileError::CanonicalizeDir {
                    path: root.display().to_string(),
                    source,
                })?;
        let payload_hash = ContentHash::of_bytes(payload_bytes);
        let safe_label = artifact_label_slug(label);
        let payload_path = root.join(format!("{}-{}.payload", safe_label, payload_hash.to_hex()));
        write_immutable_carrier(&payload_path, payload_bytes, CarrierSurface::Payload)?;
        let manifest = InterfaceArtifactManifest {
            label: label.to_string(),
            payload_path,
            payload_content_hash: Some(payload_hash.to_hex()),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|source| {
            InterfaceArtifactFileError::EncodeManifest {
                label: label.to_string(),
                source,
            }
        })?;
        let manifest_hash = ContentHash::of_bytes(&manifest_bytes);
        let manifest_path = root.join(format!(
            "{}-{}.artifact.json",
            safe_label,
            manifest_hash.to_hex()
        ));
        write_immutable_carrier(&manifest_path, &manifest_bytes, CarrierSurface::Manifest)?;
        Self::new(manifest_path)
    }

    pub fn store_manifest(
        root: impl AsRef<Path>,
        manifest_bytes: &[u8],
    ) -> Result<Self, InterfaceArtifactFileError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|source| InterfaceArtifactFileError::CreateDir {
            path: root.display().to_string(),
            source,
        })?;
        let root =
            root.canonicalize()
                .map_err(|source| InterfaceArtifactFileError::CanonicalizeDir {
                    path: root.display().to_string(),
                    source,
                })?;
        let provisional_path = root.join("incoming.artifact.json");
        let manifest = parse_interface_artifact_manifest(manifest_bytes, &provisional_path)?;
        validate_manifest_payload_hash(&manifest, None)?;
        let manifest_hash = ContentHash::of_bytes(manifest_bytes);
        let manifest_path = root.join(format!(
            "{}-{}.artifact.json",
            artifact_label_slug(&manifest.label),
            manifest_hash.to_hex()
        ));
        std::fs::write(&manifest_path, manifest_bytes).map_err(|source| {
            InterfaceArtifactFileError::WriteManifest {
                path: manifest_path.display().to_string(),
                source,
            }
        })?;
        Self::new(manifest_path)
    }

    pub fn store_manifest_bounded(
        root: impl AsRef<Path>,
        manifest_bytes: &[u8],
        maximum_manifest_bytes: u64,
        maximum_payload_bytes: u64,
    ) -> Result<Self, InterfaceArtifactFileError> {
        let artifact = Self::capture_incoming_manifest(root.as_ref(), manifest_bytes, maximum_manifest_bytes, maximum_payload_bytes, false)?;
        artifact.persist_captured_manifest()?;
        Ok(artifact)
    }

    pub fn capture_activation_manifest_bounded(
        root: impl AsRef<Path>,
        manifest_bytes: &[u8],
        maximum_manifest_bytes: u64,
        maximum_payload_bytes: u64,
    ) -> Result<Self, InterfaceArtifactFileError> {
        Self::capture_incoming_manifest(root.as_ref(), manifest_bytes, maximum_manifest_bytes, maximum_payload_bytes, true)
    }

    fn capture_incoming_manifest(
        root: &Path,
        manifest_bytes: &[u8],
        maximum_manifest_bytes: u64,
        maximum_payload_bytes: u64,
        activation_capture: bool,
    ) -> Result<Self, InterfaceArtifactFileError> {
        let provisional_path = root.join("incoming.artifact.json");
        let manifest_size = u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX);
        if manifest_size > maximum_manifest_bytes {
            return Err(InterfaceArtifactFileError::ManifestTooLarge {
                path: provisional_path.display().to_string(),
                size: manifest_size,
                maximum: maximum_manifest_bytes,
            });
        }
        std::fs::create_dir_all(root).map_err(|source| InterfaceArtifactFileError::CreateDir {
            path: root.display().to_string(),
            source,
        })?;
        let root =
            root.canonicalize()
                .map_err(|source| InterfaceArtifactFileError::CanonicalizeDir {
                    path: root.display().to_string(),
                    source,
                })?;
        let provisional_path = root.join("incoming.artifact.json");
        let manifest = parse_interface_artifact_manifest(manifest_bytes, &provisional_path)?;
        let payload_bytes = read_manifest_payload_bounded(&manifest, None, maximum_payload_bytes)?;
        validate_manifest_payload_hash_bytes(&manifest, &payload_bytes)?;
        let manifest_hash = if activation_capture && manifest.payload_content_hash.is_none() {
            let mut capture = manifest_bytes.to_vec();
            capture.extend_from_slice(ContentHash::of_bytes(&payload_bytes).as_bytes());
            ContentHash::of_bytes(&capture)
        } else {
            ContentHash::of_bytes(manifest_bytes)
        };
        let manifest_path = root.join(format!(
            "{}-{}.artifact.json",
            artifact_label_slug(&manifest.label),
            manifest_hash.to_hex()
        ));
        Self::from_captured_snapshot(manifest_path, manifest_bytes.to_vec(), payload_bytes)
    }

    pub fn persist_captured_manifest(&self) -> Result<(), InterfaceArtifactFileError> {
        let snapshot = self.captured_snapshot.as_ref().ok_or_else(|| InterfaceArtifactFileError::UncapturedCarrier {
            path: self.manifest_path.display().to_string(),
        })?;
        write_manifest_atomic(&self.manifest_path, &snapshot.manifest_bytes)
    }

    pub fn pin_manifest_into_blob(
        &self,
        blobs: &mut dyn BlobStore,
    ) -> Result<PinnedInterfaceArtifactManifest, InterfaceArtifactFileError> {
        let manifest_bytes = self.read_manifest_bytes()?;
        let manifest = parse_interface_artifact_manifest(&manifest_bytes, &self.manifest_path)?;
        let manifest_hash = blobs.put(&manifest_bytes);
        let artifact = ArtifactRef {
            id: ArtifactId::new(format!("{}:{}", manifest.label, manifest_hash.to_hex())),
            content: manifest_hash,
        };
        Ok(PinnedInterfaceArtifactManifest { artifact, manifest })
    }

    pub fn load_into_blob(
        &self,
        blobs: &mut dyn BlobStore,
    ) -> Result<LoadedInterfaceArtifactFile, InterfaceArtifactFileError> {
        let manifest_bytes = self.read_manifest_bytes()?;
        let mut manifest = parse_interface_artifact_manifest(&manifest_bytes, &self.manifest_path)?;
        let payload_bytes = self.read_payload_for_manifest(&manifest)?;
        validate_manifest_payload_hash_bytes(&manifest, &payload_bytes)?;
        let payload_content_hash = blobs.put(&payload_bytes);
        let pinned_manifest_bytes = if manifest.payload_content_hash.is_some() {
            manifest_bytes
        } else {
            manifest.payload_content_hash = Some(payload_content_hash.to_hex());
            serde_json::to_vec(&manifest).map_err(|source| {
                InterfaceArtifactFileError::EncodeManifest {
                    label: manifest.label.clone(),
                    source,
                }
            })?
        };
        let manifest_hash = blobs.put(&pinned_manifest_bytes);
        let artifact = ArtifactRef {
            id: ArtifactId::new(format!("{}:{}", manifest.label, manifest_hash.to_hex())),
            content: manifest_hash,
        };
        Ok(LoadedInterfaceArtifactFile {
            artifact,
            manifest,
            payload_bytes,
            payload_content_hash,
        })
    }

    fn read_manifest_bytes(&self) -> Result<Vec<u8>, InterfaceArtifactFileError> {
        if let Some(snapshot) = &self.captured_snapshot {
            return Ok(snapshot.manifest_bytes.clone());
        }
        read_regular_file_stable_bounded(&self.manifest_path, u64::MAX, CarrierSurface::Manifest)
    }

    fn read_manifest_bytes_bounded(
        &self,
        maximum_bytes: u64,
    ) -> Result<Vec<u8>, InterfaceArtifactFileError> {
        if let Some(snapshot) = &self.captured_snapshot {
            let size = u64::try_from(snapshot.manifest_bytes.len()).unwrap_or(u64::MAX);
            if size > maximum_bytes {
                return Err(InterfaceArtifactFileError::ManifestTooLarge {
                    path: self.manifest_path.display().to_string(),
                    size,
                    maximum: maximum_bytes,
                });
            }
            return Ok(snapshot.manifest_bytes.clone());
        }
        read_regular_file_stable_bounded(
            &self.manifest_path,
            maximum_bytes,
            CarrierSurface::Manifest,
        )
    }

    fn read_payload_for_manifest(
        &self,
        manifest: &InterfaceArtifactManifest,
    ) -> Result<Vec<u8>, InterfaceArtifactFileError> {
        if let Some(snapshot) = &self.captured_snapshot {
            return Ok(snapshot.payload_bytes.clone());
        }
        read_manifest_payload(manifest, self.payload_cache_root.as_deref())
    }

    fn read_payload_for_manifest_bounded(
        &self,
        manifest: &InterfaceArtifactManifest,
        maximum_bytes: u64,
    ) -> Result<Vec<u8>, InterfaceArtifactFileError> {
        if let Some(snapshot) = &self.captured_snapshot {
            let size = u64::try_from(snapshot.payload_bytes.len()).unwrap_or(u64::MAX);
            if size > maximum_bytes {
                return Err(InterfaceArtifactFileError::PayloadTooLarge {
                    path: manifest.payload_path.display().to_string(),
                    size,
                    maximum: maximum_bytes,
                });
            }
            return Ok(snapshot.payload_bytes.clone());
        }
        read_manifest_payload_bounded(manifest, self.payload_cache_root.as_deref(), maximum_bytes)
    }

    fn validate_payload_hash_for_manifest(
        &self,
        manifest: &InterfaceArtifactManifest,
    ) -> Result<(), InterfaceArtifactFileError> {
        if manifest.payload_content_hash.is_none() {
            return Ok(());
        }
        if let Some(snapshot) = &self.captured_snapshot {
            return validate_manifest_payload_hash_bytes(manifest, &snapshot.payload_bytes);
        }
        validate_manifest_payload_hash(manifest, self.payload_cache_root.as_deref())
    }
}

fn parse_interface_artifact_manifest(
    bytes: &[u8],
    path: &Path,
) -> Result<InterfaceArtifactManifest, InterfaceArtifactFileError> {
    let manifest: InterfaceArtifactManifest = serde_json::from_slice(bytes).map_err(|source| {
        InterfaceArtifactFileError::ParseManifest {
            path: path.display().to_string(),
            source,
        }
    })?;
    if manifest.label.trim().is_empty() {
        return Err(InterfaceArtifactFileError::ManifestEmptyLabel {
            path: path.display().to_string(),
        });
    }
    if manifest.payload_path.as_os_str().is_empty() {
        return Err(InterfaceArtifactFileError::ManifestEmptyPayloadPath {
            path: path.display().to_string(),
        });
    }
    if !manifest.payload_path.is_absolute() {
        return Err(InterfaceArtifactFileError::ManifestRelativePayloadPath {
            path: path.display().to_string(),
            payload_path: manifest.payload_path.display().to_string(),
        });
    }
    if manifest
        .payload_content_hash
        .as_deref()
        .is_some_and(|hash| ContentHash::from_hex(hash).is_none())
    {
        return Err(InterfaceArtifactFileError::ManifestInvalidPayloadHash {
            path: path.display().to_string(),
        });
    }
    Ok(manifest)
}

fn validate_manifest_payload_hash(
    manifest: &InterfaceArtifactManifest,
    payload_cache_root: Option<&Path>,
) -> Result<(), InterfaceArtifactFileError> {
    if manifest.payload_content_hash.is_none() {
        return Ok(());
    }
    let payload_bytes = read_manifest_payload(manifest, payload_cache_root)?;
    validate_manifest_payload_hash_bytes(manifest, &payload_bytes)
}

fn validate_manifest_payload_hash_bytes(
    manifest: &InterfaceArtifactManifest,
    payload_bytes: &[u8],
) -> Result<(), InterfaceArtifactFileError> {
    let Some(expected) = manifest.payload_content_hash.as_deref() else {
        return Ok(());
    };
    let actual = ContentHash::of_bytes(payload_bytes).to_hex();
    if expected != actual {
        return Err(InterfaceArtifactFileError::PayloadHashMismatch {
            path: manifest.payload_path.display().to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

fn read_manifest_payload(
    manifest: &InterfaceArtifactManifest,
    payload_cache_root: Option<&Path>,
) -> Result<Vec<u8>, InterfaceArtifactFileError> {
    let path = payload_cache_root
        .zip(manifest.payload_content_hash.as_deref())
        .map(|(root, hash)| root.join(hash))
        .unwrap_or_else(|| manifest.payload_path.clone());
    read_regular_file_stable_bounded(&path, u64::MAX, CarrierSurface::Payload)
}

fn read_manifest_payload_bounded(
    manifest: &InterfaceArtifactManifest,
    payload_cache_root: Option<&Path>,
    maximum_bytes: u64,
) -> Result<Vec<u8>, InterfaceArtifactFileError> {
    let path = payload_cache_root
        .zip(manifest.payload_content_hash.as_deref())
        .map(|(root, hash)| root.join(hash))
        .unwrap_or_else(|| manifest.payload_path.clone());
    read_regular_file_stable_bounded(&path, maximum_bytes, CarrierSurface::Payload)
}

pub fn read_artifact_carrier_file_bounded(
    path: impl AsRef<Path>,
    maximum_bytes: u64,
) -> Result<Vec<u8>, InterfaceArtifactFileError> {
    read_regular_file_stable_bounded(path.as_ref(), maximum_bytes, CarrierSurface::Payload)
}

fn write_immutable_carrier(
    path: &Path,
    bytes: &[u8],
    surface: CarrierSurface,
) -> Result<(), InterfaceArtifactFileError> {
    let write_error = |source| match surface {
        CarrierSurface::Manifest => InterfaceArtifactFileError::WriteManifest {
            path: path.display().to_string(), source,
        },
        CarrierSurface::Payload => InterfaceArtifactFileError::WritePayload {
            path: path.display().to_string(), source,
        },
    };
    let verify = || {
        if read_regular_file_stable_bounded(path, bytes.len() as u64, surface)? != bytes {
            return Err(write_error(std::io::Error::other("immutable carrier bytes differ")));
        }
        Ok(())
    };
    let parent = path.parent().ok_or_else(|| {
        InterfaceArtifactFileError::RelativeManifestPath(path.display().to_string())
    })?;
    match std::fs::symlink_metadata(path) {
        Ok(_) => verify()?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let temporary = tempfile::Builder::new()
                .prefix(".artifact-")
                .make_in(parent, |temporary| octopus_file_storage::write_new(temporary, bytes))
                .map_err(write_error)?;
            octopus_file_storage::sync_ordered(temporary.as_file()).map_err(write_error)?;
            match temporary.persist_noclobber(path) {
                Ok(_) => {}
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => verify()?,
                Err(error) => return Err(write_error(error.error)),
            }
        }
        Err(error) => return Err(write_error(error)),
    }
    octopus_file_storage::sync_directory_ordered(parent).map_err(write_error)
}

fn write_manifest_atomic(
    path: &Path,
    manifest_bytes: &[u8],
) -> Result<(), InterfaceArtifactFileError> {
    let parent = path.parent().ok_or_else(|| {
        InterfaceArtifactFileError::RelativeManifestPath(path.display().to_string())
    })?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".incoming-artifact-")
        .tempfile_in(parent)
        .map_err(|source| InterfaceArtifactFileError::WriteManifest {
            path: path.display().to_string(),
            source,
        })?;
    std::io::Write::write_all(temporary.as_file_mut(), manifest_bytes)
        .and_then(|_| octopus_file_storage::sync_ordered(temporary.as_file()))
        .map_err(|source| InterfaceArtifactFileError::WriteManifest {
            path: path.display().to_string(),
            source,
        })?;
    temporary
        .persist(path)
        .map_err(|error| InterfaceArtifactFileError::WriteManifest {
            path: path.display().to_string(),
            source: error.error,
        })?;
    Ok(())
}

#[derive(Clone, Copy)]
enum CarrierSurface {
    Manifest,
    Payload,
}

impl CarrierSurface {
    fn name(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Payload => "payload",
        }
    }

    fn read_error(self, path: &Path, source: std::io::Error) -> InterfaceArtifactFileError {
        match self {
            Self::Manifest => InterfaceArtifactFileError::ReadManifest {
                path: path.display().to_string(),
                source,
            },
            Self::Payload => InterfaceArtifactFileError::ReadPayload {
                path: path.display().to_string(),
                source,
            },
        }
    }

    fn too_large(self, path: &Path, size: u64, maximum: u64) -> InterfaceArtifactFileError {
        match self {
            Self::Manifest => InterfaceArtifactFileError::ManifestTooLarge {
                path: path.display().to_string(),
                size,
                maximum,
            },
            Self::Payload => InterfaceArtifactFileError::PayloadTooLarge {
                path: path.display().to_string(),
                size,
                maximum,
            },
        }
    }
}

fn read_regular_file_stable_bounded(
    path: &Path,
    maximum_bytes: u64,
    surface: CarrierSurface,
) -> Result<Vec<u8>, InterfaceArtifactFileError> {
    if !path.is_absolute() {
        return Err(InterfaceArtifactFileError::UnsafeCarrier {
            surface: surface.name(),
            path: path.display().to_string(),
            detail: "carrier path is not absolute",
        });
    }
    let file = open_regular_file_no_follow(path, surface)?;
    read_open_regular_file_stable_bounded(file, path, maximum_bytes, surface)
}

fn read_open_regular_file_stable_bounded(
    mut file: std::fs::File,
    path: &Path,
    maximum_bytes: u64,
    surface: CarrierSurface,
) -> Result<Vec<u8>, InterfaceArtifactFileError> {
    let before = file
        .metadata()
        .map_err(|source| surface.read_error(path, source))?;
    if !before.file_type().is_file() {
        return Err(InterfaceArtifactFileError::UnsafeCarrier {
            surface: surface.name(),
            path: path.display().to_string(),
            detail: "opened object is not a regular file",
        });
    }
    let maximum_bytes = maximum_bytes.min(usize::MAX as u64);
    if before.len() > maximum_bytes {
        return Err(surface.too_large(path, before.len(), maximum_bytes));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| surface.read_error(path, source))?;
    let observed = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if observed > maximum_bytes {
        return Err(surface.too_large(path, observed, maximum_bytes));
    }
    let after = file
        .metadata()
        .map_err(|source| surface.read_error(path, source))?;
    if observed != before.len() || !same_open_file_state(&before, &after) {
        return Err(InterfaceArtifactFileError::UnsafeCarrier {
            surface: surface.name(),
            path: path.display().to_string(),
            detail: "file identity or content metadata changed during read",
        });
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_regular_file_no_follow(
    path: &Path,
    surface: CarrierSurface,
) -> Result<std::fs::File, InterfaceArtifactFileError> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| surface.read_error(path, source))
}

#[cfg(windows)]
fn open_regular_file_no_follow(
    path: &Path,
    surface: CarrierSurface,
) -> Result<std::fs::File, InterfaceArtifactFileError> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|source| surface.read_error(path, source))
}

#[cfg(not(any(unix, windows)))]
fn open_regular_file_no_follow(
    path: &Path,
    surface: CarrierSurface,
) -> Result<std::fs::File, InterfaceArtifactFileError> {
    Err(InterfaceArtifactFileError::UnsafeCarrier {
        surface: surface.name(),
        path: path.display().to_string(),
        detail: "platform has no supported no-follow file-open contract",
    })
}

#[cfg(unix)]
fn same_open_file_state(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.mode() == after.mode()
        && before.nlink() == after.nlink()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(windows)]
fn same_open_file_state(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    before.file_attributes() == after.file_attributes()
        && before.creation_time() == after.creation_time()
        && before.last_write_time() == after.last_write_time()
        && before.file_size() == after.file_size()
}

#[cfg(not(any(unix, windows)))]
fn same_open_file_state(_before: &std::fs::Metadata, _after: &std::fs::Metadata) -> bool {
    false
}

fn artifact_label_slug(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactBindingPayload {
    pub artifact: ArtifactRef,
    backing: ArtifactBindingBacking,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ArtifactBindingBacking {
    Inline(Arc<[u8]>),
    PinnedCarrier {
        path: PathBuf,
        payload_hash: ContentHash,
        byte_len: u64,
        maximum_bytes: u64,
    },
}

impl ArtifactBindingPayload {
    pub fn new(artifact: ArtifactRef, bytes: Vec<u8>) -> Self {
        Self { artifact, backing: ArtifactBindingBacking::Inline(bytes.into()) }
    }

    pub fn from_pinned_file(
        artifact: ArtifactRef,
        path: PathBuf,
        payload_hash: ContentHash,
        maximum_bytes: u64,
    ) -> Result<Self, InterfaceArtifactFileError> {
        validate_binding_carrier_path(&path)?;
        let file = open_binding_carrier_no_follow(&path)?;
        let metadata = file.metadata()
            .map_err(|error| CarrierSurface::Payload.read_error(&path, error))?;
        if !metadata.is_file() {
            return Err(InterfaceArtifactFileError::UnsafeCarrier {
                surface: "payload", path: path.display().to_string(),
                detail: "bound payload is not a regular file",
            });
        }
        if metadata.len() > maximum_bytes {
            return Err(CarrierSurface::Payload.too_large(&path, metadata.len(), maximum_bytes));
        }
        Ok(Self { artifact, backing: ArtifactBindingBacking::PinnedCarrier {
            path, payload_hash, byte_len: metadata.len(), maximum_bytes,
        } })
    }

    pub fn payload_len(&self) -> u64 {
        match &self.backing {
            ArtifactBindingBacking::Inline(bytes) => bytes.len() as u64,
            ArtifactBindingBacking::PinnedCarrier { byte_len, .. } => *byte_len,
        }
    }

    pub fn retained_byte_len(&self) -> u64 {
        match &self.backing {
            ArtifactBindingBacking::Inline(bytes) => bytes.len() as u64,
            ArtifactBindingBacking::PinnedCarrier { .. } => 0,
        }
    }

    pub fn payload_hash(&self) -> ContentHash {
        match &self.backing {
            ArtifactBindingBacking::Inline(bytes) => ContentHash::of_bytes(bytes),
            ArtifactBindingBacking::PinnedCarrier { payload_hash, .. } => *payload_hash,
        }
    }

    pub fn read_bytes(&self) -> Result<std::borrow::Cow<'_, [u8]>, InterfaceArtifactFileError> {
        let maximum = match &self.backing {
            ArtifactBindingBacking::Inline(bytes) => bytes.len() as u64,
            ArtifactBindingBacking::PinnedCarrier { maximum_bytes, .. } => *maximum_bytes,
        };
        self.read_bounded(maximum)
    }

    pub fn read_bounded(
        &self,
        maximum_bytes: u64,
    ) -> Result<std::borrow::Cow<'_, [u8]>, InterfaceArtifactFileError> {
        match &self.backing {
            ArtifactBindingBacking::Inline(bytes) => {
                if bytes.len() as u64 > maximum_bytes {
                    return Err(InterfaceArtifactFileError::PayloadTooLarge {
                        path: self.artifact.id.to_string(), size: bytes.len() as u64,
                        maximum: maximum_bytes,
                    });
                }
                Ok(std::borrow::Cow::Borrowed(bytes))
            }
            ArtifactBindingBacking::PinnedCarrier { path, payload_hash, byte_len, maximum_bytes: ceiling } => {
                validate_binding_carrier_path(path)?;
                let bytes = read_open_regular_file_stable_bounded(
                    open_binding_carrier_no_follow(path)?, path,
                    maximum_bytes.min(*ceiling).min(*byte_len), CarrierSurface::Payload,
                )?;
                validate_binding_carrier_path(path)?;
                if bytes.len() as u64 != *byte_len {
                    return Err(InterfaceArtifactFileError::UnsafeCarrier {
                        surface: "payload", path: path.display().to_string(),
                        detail: "bound payload length changed after capture",
                    });
                }
                let actual = ContentHash::of_bytes(&bytes);
                if actual != *payload_hash {
                    return Err(InterfaceArtifactFileError::PayloadHashMismatch {
                        path: path.display().to_string(), expected: payload_hash.to_hex(),
                        actual: actual.to_hex(),
                    });
                }
                Ok(std::borrow::Cow::Owned(bytes))
            }
        }
    }
}

#[cfg(unix)]
fn open_binding_carrier_no_follow(path: &Path) -> Result<std::fs::File, InterfaceArtifactFileError> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_os = "macos")]
    let directory_access = libc::O_SEARCH;
    #[cfg(target_os = "linux")]
    let directory_access = libc::O_PATH | libc::O_DIRECTORY;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let directory_access = libc::O_RDONLY | libc::O_DIRECTORY;
    let root_fd = unsafe { libc::open(c"/".as_ptr(), directory_access | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    if root_fd < 0 { return Err(CarrierSurface::Payload.read_error(path, std::io::Error::last_os_error())); }
    let mut directory = unsafe { std::fs::File::from_raw_fd(root_fd) };
    let mut parts = path.components().skip(1).peekable();
    while let Some(part) = parts.next() {
        let std::path::Component::Normal(name) = part else {
            return Err(InterfaceArtifactFileError::UnsafeCarrier {
                surface: "payload", path: path.display().to_string(), detail: "bound carrier path has a non-normal component",
            });
        };
        let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| InterfaceArtifactFileError::UnsafeCarrier {
            surface: "payload", path: path.display().to_string(), detail: "bound carrier path contains NUL",
        })?;
        let flags = libc::O_CLOEXEC | libc::O_NOFOLLOW
            | if parts.peek().is_some() { directory_access } else { libc::O_RDONLY | libc::O_NONBLOCK };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(CarrierSurface::Payload.read_error(path, std::io::Error::last_os_error()));
        }
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn open_binding_carrier_no_follow(path: &Path) -> Result<std::fs::File, InterfaceArtifactFileError> {
    open_regular_file_no_follow(path, CarrierSurface::Payload)
}

fn validate_binding_carrier_path(path: &Path) -> Result<(), InterfaceArtifactFileError> {
    if !path.is_absolute() || path.canonicalize()
        .map_err(|error| CarrierSurface::Payload.read_error(path, error))? != path
    {
        return Err(InterfaceArtifactFileError::UnsafeCarrier {
            surface: "payload", path: path.display().to_string(),
            detail: "bound payload path is not its exact canonical path",
        });
    }
    Ok(())
}

pub type ArtifactBindingPayloadSet = Vec<ArtifactBindingPayload>;

#[derive(Debug, Error)]
pub enum InterfaceBoundaryError {
    #[error("interface boundary transport error: {0}")]
    Transport(String),
    #[error("interface boundary protocol error: {0}")]
    Protocol(String),
    #[error("interface boundary failed closed: {0}")]
    FailedClosed(String),
}

#[derive(Clone, Debug)]
pub struct InterfaceInvocationEnvelope {
    pub artifact: ArtifactRef,
    pub artifact_file: InterfaceArtifactFile,
    pub artifact_binding_payloads: ArtifactBindingPayloadSet,
    pub metadata: Option<InvocationMetadata>,
}

impl InterfaceInvocationEnvelope {
    pub fn metadata(&self) -> Result<&InvocationMetadata, InterfaceBoundaryError> {
        self.metadata.as_ref().ok_or_else(|| {
            InterfaceBoundaryError::Protocol(
                "interface invocation is missing metadata envelope".into(),
            )
        })
    }

    pub fn grant(&self) -> Result<&GrantProjection, InterfaceBoundaryError> {
        self.metadata().map(InvocationMetadata::grant)
    }

    pub fn runtime(&self) -> Result<&RuntimeInvocationMetadata, InterfaceBoundaryError> {
        self.metadata()?.runtime().ok_or_else(|| {
            InterfaceBoundaryError::Protocol(
                "interface invocation is missing runtime coordinates".into(),
            )
        })
    }

    pub fn interface_affordance_specs(
        &self,
    ) -> Result<Vec<InterfaceToolAffordance>, InterfaceBoundaryError> {
        Ok(self.metadata()?.interface_affordance_specs().to_vec())
    }

    pub fn granted_tool_names(&self) -> Result<Vec<String>, InterfaceBoundaryError> {
        Ok(self.metadata()?.granted_tool_names().to_vec())
    }

    pub fn hard_blocked_tool_names(&self) -> Result<Vec<String>, InterfaceBoundaryError> {
        Ok(self.metadata()?.hard_blocked_tool_names().to_vec())
    }

    pub fn worker_environment_kinds(&self) -> Result<&[String], InterfaceBoundaryError> {
        self.metadata()
            .map(InvocationMetadata::worker_environment_kinds)
    }

    pub fn interface_policy(
        &self,
    ) -> Result<Option<&octopus_policy::InterfacePolicyValues>, InterfaceBoundaryError> {
        self.metadata().map(InvocationMetadata::interface_policy)
    }

    pub fn allowed_interfaces(&self) -> Result<&[String], InterfaceBoundaryError> {
        self.metadata().map(InvocationMetadata::allowed_interfaces)
    }

    pub fn read_artifact_payload(&self) -> Result<Vec<u8>, InterfaceBoundaryError> {
        self.artifact_file
            .read_payload_bytes()
            .map_err(|e| InterfaceBoundaryError::Protocol(e.to_string()))
    }

    pub fn read_artifact_payload_bounded(
        &self,
        maximum_bytes: u64,
    ) -> Result<Vec<u8>, InterfaceBoundaryError> {
        self.artifact_file
            .read_payload_bytes_bounded(maximum_bytes)
            .map_err(|e| InterfaceBoundaryError::Protocol(e.to_string()))
    }

    pub fn read_artifact_bounded(
        &self,
        maximum_manifest_bytes: u64,
        maximum_payload_bytes: u64,
    ) -> Result<BoundedInterfaceArtifactSnapshot, InterfaceBoundaryError> {
        self.artifact_file
            .read_bounded(maximum_manifest_bytes, maximum_payload_bytes)
            .map_err(|e| InterfaceBoundaryError::Protocol(e.to_string()))
    }

    pub fn read_artifact_manifest_bounded(
        &self,
        maximum_manifest_bytes: u64,
    ) -> Result<BoundedInterfaceArtifactManifest, InterfaceBoundaryError> {
        self.artifact_file
            .read_manifest_bounded(maximum_manifest_bytes)
            .map_err(|e| InterfaceBoundaryError::Protocol(e.to_string()))
    }

    pub fn read_artifact_manifest(
        &self,
    ) -> Result<InterfaceArtifactManifest, InterfaceBoundaryError> {
        self.artifact_file
            .read_manifest()
            .map_err(|e| InterfaceBoundaryError::Protocol(e.to_string()))
    }
}

#[derive(Default)]
pub struct InterfaceRegistry {
    descriptors: BTreeMap<InterfaceId, InterfaceDescriptor>,
}

impl InterfaceRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, desc: InterfaceDescriptor) {
        self.descriptors.insert(desc.id.clone(), desc);
    }
    pub fn get(&self, id: &InterfaceId) -> Option<&InterfaceDescriptor> {
        self.descriptors.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use octopus_descriptors::parse_strict;

    #[test]
    fn bound_payload_reads_preserve_bytes_and_refuse_changed_carriers() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let path = root.join("payload");
        let bytes = b"raw\0\xff bytes";
        let reference = ArtifactRef { id: octopus_descriptors::ArtifactId::new("bound"), content: ContentHash::of_bytes(b"manifest") };
        let inline = ArtifactBindingPayload::new(reference.clone(), bytes.to_vec());
        assert!(matches!(inline.read_bytes().unwrap(), std::borrow::Cow::Borrowed(_)));
        assert!(inline.read_bounded(2).is_err());
        std::fs::write(&path, bytes).unwrap();
        let lazy = ArtifactBindingPayload::from_pinned_file(reference.clone(), path.clone(), ContentHash::of_bytes(bytes), 64).unwrap();
        assert_eq!(lazy.artifact, reference);
        assert_eq!(lazy.clone().read_bytes().unwrap().as_ref(), bytes);
        assert!(lazy.read_bounded(2).is_err());
        std::fs::write(&path, b"bad\0\xff bytes").unwrap();
        assert!(matches!(lazy.read_bytes(), Err(InterfaceArtifactFileError::PayloadHashMismatch { .. })));
        std::fs::write(&path, b"short").unwrap();
        assert!(lazy.read_bytes().is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(lazy.read_bytes().is_err());
        #[cfg(unix)] {
            let target = root.join("target");
            std::fs::write(&target, bytes).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(lazy.read_bytes().is_err());
            assert!(ArtifactBindingPayload::from_pinned_file(reference.clone(), path, ContentHash::of_bytes(bytes), 64).is_err());
            std::os::unix::fs::symlink(&root, root.join("alias")).unwrap();
            assert!(ArtifactBindingPayload::from_pinned_file(reference, root.join("alias/target"), ContentHash::of_bytes(bytes), 64).is_err());
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn sandboxed_bound_payload_requires_no_ancestor_directory_read() {
        const CHILD: &str = "OCTOPUS_BOUND_PAYLOAD_SANDBOX_ROOT";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = PathBuf::from(root);
            assert_eq!(std::fs::read_dir(root.parent().unwrap()).unwrap_err().raw_os_error(), Some(libc::EPERM));
            assert!(std::fs::read(root.parent().unwrap().join("outside")).is_err());
            let bytes = b"exact bound payload";
            let reference = ArtifactRef { id: octopus_descriptors::ArtifactId::new("bound"), content: ContentHash::of_bytes(b"manifest") };
            let payload = ArtifactBindingPayload::from_pinned_file(reference, root.join("payload"), ContentHash::of_bytes(bytes), 64).unwrap();
            assert_eq!(payload.read_bytes().unwrap().as_ref(), bytes);
            assert!(payload.read_bounded(2).is_err());
            println!("bound payload sandbox: exact bytes; ancestor denied");
            return;
        }
        let temp = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = temp.path().canonicalize().unwrap().join("generation");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("payload"), b"exact bound payload").unwrap();
        std::fs::write(root.parent().unwrap().join("outside"), b"not granted").unwrap();
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let profile = format!("{}\n{}\n(allow file-read* file-test-existence (literal {}) (subpath {}))",
            include_str!("../../../registry/worker_environment_kind/process_session/packages/octopus-sandbox-substrate/src/seatbelt_base_policy.sbpl"),
            include_str!("../../../registry/worker_environment_kind/process_session/packages/octopus-sandbox-substrate/src/restricted_read_only_platform_defaults.sbpl"),
            serde_json::to_string(&executable).unwrap(), serde_json::to_string(&root).unwrap());
        let output = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile]).arg(executable)
            .args(["--exact", "tests::sandboxed_bound_payload_requires_no_ancestor_directory_read", "--nocapture", "--test-threads=1"])
            .current_dir(&root).env_clear().env(CHILD, &root).output().unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("bound payload sandbox: exact bytes; ancestor denied"));
    }

    #[test]
    fn manifest_without_hash_does_not_read_payload() {
        let root = std::env::temp_dir().join(format!(
            "octopus-interface-manifest-only-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let manifest_path = root.join("artifact.json");
        let missing_payload = root.join("missing.payload");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec(&InterfaceArtifactManifest {
                label: "manifest.only".to_string(),
                payload_path: missing_payload,
                payload_content_hash: None,
            })
            .unwrap(),
        )
        .unwrap();

        let artifact = InterfaceArtifactFile::new(&manifest_path).unwrap();
        assert_eq!(artifact.read_manifest().unwrap().label, "manifest.only");
        assert!(artifact.read_payload_bytes().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_payload_read_refuses_an_oversized_carrier() {
        let root = std::env::temp_dir().join(format!(
            "octopus-interface-bounded-payload-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let artifact = InterfaceArtifactFile::write(&root, "bounded", b"12345").unwrap();
        let error = artifact.read_payload_bytes_bounded(4).unwrap_err();
        assert!(matches!(
            error,
            InterfaceArtifactFileError::PayloadTooLarge {
                size: 5,
                maximum: 4,
                ..
            }
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn immutable_carriers_publish_exact_bytes_and_refuse_existing_corruption() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"{\"exact\":\"immutable evidence\"}\n".repeat(32768);
        let barrier = std::sync::Barrier::new(4);
        let artifacts = std::thread::scope(|scope| {
            let handles = (0..4).map(|_| scope.spawn(|| {
                barrier.wait();
                InterfaceArtifactFile::write(root.path(), "exact.payload", &bytes).unwrap()
            })).collect::<Vec<_>>();
            handles.into_iter().map(|handle| handle.join().unwrap()).collect::<Vec<_>>()
        });
        for artifact in &artifacts {
            assert_eq!(artifact.manifest_path(), artifacts[0].manifest_path());
            let reopened = InterfaceArtifactFile::new(artifact.manifest_path()).unwrap();
            assert_eq!(reopened.read_payload_bytes().unwrap(), bytes);
        }
        let payload = artifacts[0].read_manifest().unwrap().payload_path;
        #[cfg(target_vendor = "apple")]
        {
            use std::os::macos::fs::MetadataExt as _;
            use std::os::unix::fs::MetadataExt as _;
            let metadata = std::fs::metadata(&payload).unwrap();
            assert_ne!(metadata.st_flags() & 0x20, 0);
            assert!(metadata.blocks() * 512 < metadata.len());
            assert_eq!(metadata.nlink(), 1);
        }
        std::fs::write(&payload, b"corruption").unwrap();
        assert!(InterfaceArtifactFile::write(root.path(), "exact.payload", &bytes).is_err());
        assert_eq!(std::fs::read(&payload).unwrap(), b"corruption");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn executable_interface_declares_its_own_driver_and_rejects_reference_identity() {
        let interface: InterfaceDescriptor = parse_strict(
            br#"{"id":"world_exact","interface_type":"world","role":"service","direction":"bidirectional","source_names":["exact"],"interface_driver_ref":"exact_driver","interface_policy":{"variables":{"max_attempts":{"reduce":"min","default":5}}}}"#,
        )
        .unwrap();
        assert_eq!(
            form_interface_runtime_disposition(
                &interface,
                Path::new("/nonexistent-interface-registry"),
                false,
            )
            .unwrap(),
            InterfaceRuntimeDisposition::ExternalReader
        );
        assert!(parse_strict::<InterfaceDescriptor>(
            br#"{"id":"world_instance","reference":"world_exact","interface_type":"world","role":"service","direction":"bidirectional","source_names":["instance"]}"#,
        )
        .is_err());
        assert!(parse_strict::<InterfaceDescriptor>(
            br#"{"id":"world_invalid_policy","interface_type":"world","role":"service","direction":"bidirectional","source_names":["invalid.policy"],"interface_driver_ref":"exact_driver","interface_policy":{"variables":{"max_attempts":{"reduce":"min","default":"five"}}}}"#,
        )
        .is_err());
    }

    #[test]
    fn real_registry_interface_descriptors_parse() {
        use octopus_descriptors::loader;
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/interface");
        let loaded =
            loader::load_kind::<InterfaceDescriptor>(&root).expect("registry/interface must parse");
        assert!(
            !loaded.is_empty(),
            "registry/interface must contain at least one descriptor"
        );
        assert!(
            loaded
                .iter()
                .any(|(_, d)| d.interface_driver_config.is_some()),
            "registry/interface must include at least one transport-configured interface"
        );
    }

    #[test]
    fn loader_asserts_interface_dir_equals_body_id_failclosed() {
        use octopus_descriptors::loader;
        use std::fs;
        let base = std::env::temp_dir().join(format!("octo_iface_ident_{}", std::process::id()));
        let root = base.join("interface");
        let _ = fs::remove_dir_all(&base);

        let dir_ok = root.join("world_dialog_alpha");
        fs::create_dir_all(&dir_ok).unwrap();
        let body_ok = br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver"}"#;
        fs::write(dir_ok.join("interface.json"), body_ok).unwrap();
        let loaded =
            loader::load_kind::<InterfaceDescriptor>(&root).expect("dir==id interface must load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].1.id, InterfaceId::new("world_dialog_alpha"));

        let body_drift = br#"{"id":"world_dialog_beta","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver"}"#;
        fs::write(dir_ok.join("interface.json"), body_drift).unwrap();
        let err = loader::load_kind::<InterfaceDescriptor>(&root)
            .expect_err("dir≠id interface MUST be rejected by the identity gate");
        let msg = err.to_string();
        assert!(
            msg.contains("identity mismatch")
                && msg.contains("world_dialog_alpha")
                && msg.contains("world_dialog_beta"),
            "the gate must name the dir/body disagreement, got: {msg}"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn interface_driver_config_carries_opaque_driver_data_and_transport() {
        use octopus_descriptors::parse_strict;

        parse_strict::<InterfaceDescriptor>(
            br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver"}"#,
        )
        .expect("an interface may omit interface_driver_config in JSON (serde default)");
        let bounded: InterfaceDescriptor = parse_strict(
            br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","interface_driver_config":{"private_config":{"target":"target-x","profile":"profile-high"}}}"#,
        )
        .expect("an interface may author opaque driver metadata");
        let bounded_value =
            serde_json::to_value(&bounded).expect("interface descriptor serializes");
        assert_eq!(
            bounded_value
                .pointer("/interface_driver_config/private_config/target")
                .and_then(serde_json::Value::as_str),
            Some("target-x"),
            "request target is opaque interface driver config, not root interface schema"
        );
        assert_eq!(
            bounded_value
                .pointer("/interface_driver_config/private_config/profile")
                .and_then(serde_json::Value::as_str),
            Some("profile-high"),
            "request profile is opaque interface driver config, not root interface schema"
        );

        let transport: InterfaceDescriptor = parse_strict(
            br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","interface_driver_config":{"transport":{"base_url":"http://localhost:11434/v1","wire":"json_rpc","request_timeout_ms":120000}}}"#,
        )
        .expect("interface_driver_config may carry transport config");
        assert!(
            transport
                .interface_driver_config
                .as_ref()
                .and_then(|config| config.transport.as_ref())
                .is_some(),
            "interface_driver_config may carry interface-local transport config"
        );

        assert!(
            parse_strict::<InterfaceDescriptor>(
                br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","model":{"transport":{"base_url":"http://localhost:11434/v1"}}}"#
            )
            .is_err(),
            "root model.transport is retired; transport belongs under interface_driver_config.transport"
        );
        assert!(
            parse_strict::<InterfaceDescriptor>(
                br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","model":{"model":"model-x"}}"#,
            )
            .is_err(),
            "root model block is no longer a generic interface descriptor field"
        );
        assert!(
            parse_strict::<InterfaceDescriptor>(
                br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","model":{"reasoning_effort":"xhigh"}}"#,
            )
            .is_err(),
            "reasoning_effort must not live under a root model block"
        );
        assert!(
            parse_strict::<InterfaceDescriptor>(
                br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","artifact_turn":{"input_artifact_label":"interface.input"}}"#,
            )
            .is_err(),
            "root artifact_turn is retired and fails closed"
        );
        assert!(
            parse_strict::<InterfaceDescriptor>(
                br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","interface_driver_config":{"native_call_loop":true}}"#,
            )
            .is_err(),
            "a retired native-call loop flag is an unknown field and fails closed"
        );
        assert!(
            parse_strict::<InterfaceDescriptor>(
                br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","interface_driver_config":{"native_call_loop_max_steps":64}}"#,
            )
            .is_err(),
            "a retired native-call loop step field is unknown and fails closed"
        );
        assert!(
            parse_strict::<InterfaceDescriptor>(
                br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","interface_driver_config":{"artifact_turn_max_steps":64}}"#,
            )
            .is_err(),
            "retired artifact_turn config must not live under interface_driver_config"
        );
        assert!(
            parse_strict::<InterfaceDescriptor>(
                br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","interface_driver_config":{"artifact_protocol":{"input_artifact_label":"interface.input"}}}"#,
            )
            .is_err(),
            "artifact protocol must not live under interface_driver_config"
        );

        assert!(
            parse_strict::<InterfaceDescriptor>(
                br#"{"id":"world_dialog_alpha","interface_type":"world","role":"service","direction":"bidirectional","source_names":["dialog.alpha"],"interface_driver_ref":"shared_boundary_driver","interface_driver_config":{"bad":1}}"#
            )
            .is_err(),
            "deny_unknown_fields must reject drift on interface_driver_config"
        );
    }

    #[test]
    fn source_names_are_source_only_and_render_full_classification() {
        let descriptor: InterfaceDescriptor = parse_strict(
            br#"{"id":"system_internal_probe","interface_type":"system","role":"user","direction":"inbound","source_names":["operator"]}"#,
        )
        .expect("source-only interface descriptor parses");
        assert_eq!(
            descriptor.source_classifications(),
            vec!["system.operator(system_internal_probe)".to_string()]
        );
        assert_eq!(
            descriptor.source_route_prefixes(),
            vec!["system.operator".to_string()]
        );

        let err = parse_strict::<InterfaceDescriptor>(
            br#"{"id":"system_internal_probe","interface_type":"system","role":"user","direction":"inbound","source_names":["system.operator"]}"#,
        )
        .expect_err("source_names must not repeat the interface_type axis");
        assert!(
            err.to_string().contains("repeats the interface_type axis"),
            "error should explain the source-only contract, got: {err}"
        );
    }
}
