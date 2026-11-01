use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use octopus_interfaces::InterfaceDescriptor;
use serde::Deserialize;
use serde_json::{Value, json};

pub const SDK_CHILD_PROTOCOL: &str = "octopus.claude_sdk_child/1";
const IMPORT_PREFIX: &str = "interface://";
const SDK_CHILD_RECEIPT: &str = "build-receipt.json";
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SdkChildConfig {
    pub executable: String,
    pub protocol: String,
    pub claude_executable: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputMessage {
    pub text_fields: Vec<String>,
    #[serde(default)]
    pub client_context_field: Option<String>,
    #[serde(default)]
    pub attachments_field: Option<String>,
    #[serde(default)]
    pub identity_field: Option<String>,
    pub identity_required: bool,
    #[serde(default)]
    pub context_fields: Vec<String>,
    #[serde(default)]
    pub request_context_fields: BTreeMap<String, RequestContextKind>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestContextKind {
    Array,
    Object,
}

impl InputMessage {
    fn validate(&self, label: &str) -> Result<(), String> {
        let invalid = || format!("input_messages '{label}' has invalid fields");
        if self.text_fields.is_empty() && self.request_context_fields.is_empty() {
            return Err(format!("input_messages '{label}' declares no text_fields"));
        }
        if self.identity_required && self.identity_field.is_none() {
            return Err(format!("input_messages '{label}' requires an identity but declares no identity_field"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for field in self
            .text_fields
            .iter()
            .chain(self.client_context_field.iter())
            .chain(self.attachments_field.iter())
            .chain(self.identity_field.iter())
            .chain(self.context_fields.iter())
            .chain(self.request_context_fields.keys())
        {
            if field.is_empty() || field.trim() != field || !seen.insert(field) {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeNativeConfig {
    pub model: String,
    pub reasoning_effort: String,
    pub sdk_child: SdkChildConfig,
    pub input_messages: BTreeMap<String, InputMessage>,
    pub client_metadata: Value,
    pub model_catalog: Vec<Value>,
    pub run_authoring_profile: Value,
    pub model_hidden_tools: Vec<String>,
    pub context_output_labels: Vec<String>,
    pub client_projection_output_labels: Vec<String>,
    pub session_snapshot_labels: Vec<String>,
    #[serde(default)]
    pub worker_instruction_artifacts: Vec<String>,
    #[serde(default)]
    pub instruction_artifacts: Vec<String>,
    #[serde(default)]
    pub collection_catalogue_instruction_artifacts: Vec<String>,
}

impl ClaudeNativeConfig {
    pub fn from_descriptor(descriptor: &InterfaceDescriptor) -> Result<Self, String> {
        let driver = descriptor
            .interface_driver_config
            .as_ref()
            .ok_or("claude native interface has no interface_driver_config")?;
        let config: Self = serde_json::from_value(json!(driver.private_config))
            .map_err(|error| format!("claude native private_config is invalid: {error}"))?;
        validate_identity(&config.model, "private_config.model")?;
        if !EFFORTS.contains(&config.reasoning_effort.as_str()) {
            return Err(format!(
                "private_config.reasoning_effort '{}' is not one of {EFFORTS:?}",
                config.reasoning_effort
            ));
        }
        if config.sdk_child.protocol != SDK_CHILD_PROTOCOL {
            return Err(format!(
                "private_config.sdk_child.protocol must be '{SDK_CHILD_PROTOCOL}'"
            ));
        }
        validate_claude_executable(&config.sdk_child.claude_executable)?;
        if config.input_messages.is_empty() {
            return Err("private_config.input_messages declares no input".into());
        }
        for (label, input) in &config.input_messages {
            validate_identity(label, "input_messages label")?;
            input.validate(label)?;
        }
        import_relative_path(&config.sdk_child.executable)?;
        config.instruction_selection()?;
        let declared = declared_models(descriptor)?;
        if !declared.iter().any(|model| model == &config.model) {
            return Err(format!(
                "private_config.model '{}' is outside interface_policy models",
                config.model
            ));
        }
        Ok(config)
    }

    pub fn sdk_child_executable(&self, registry_interface_root: &Path) -> Result<PathBuf, String> {
        Ok(registry_interface_root.join(import_relative_path(&self.sdk_child.executable)?))
    }

    pub fn instruction_selection(&self) -> Result<Vec<String>, String> {
        let mut selected = Vec::new();
        for (key, ids) in [
            ("worker_instruction_artifacts", &self.worker_instruction_artifacts),
            ("instruction_artifacts", &self.instruction_artifacts),
        ] {
            let mut local = std::collections::BTreeSet::new();
            for id in ids {
                if id.trim().is_empty() || id.trim() != id || !local.insert(id.as_str()) {
                    return Err(format!("private_config.{key} requires unique nonempty artifact ids"));
                }
                if !selected.contains(id) {
                    selected.push(id.clone());
                }
            }
        }
        Ok(selected)
    }
}

fn declared_models(descriptor: &InterfaceDescriptor) -> Result<Vec<String>, String> {
    let policy = serde_json::to_value(&descriptor.interface_policy)
        .map_err(|error| format!("interface_policy failed to encode: {error}"))?;
    policy
        .pointer("/variables/models/default")
        .and_then(Value::as_array)
        .ok_or("interface_policy.variables.models.default must be an array")?
        .iter()
        .map(|model| {
            model
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "interface_policy models must be strings".to_string())
        })
        .collect()
}

fn import_relative_path(declared: &str) -> Result<PathBuf, String> {
    let relative = declared
        .strip_prefix(IMPORT_PREFIX)
        .ok_or_else(|| format!("sdk_child.executable '{declared}' must be an {IMPORT_PREFIX} import"))?;
    let path = Path::new(relative);
    let mut components = path.components();
    let owner = components.next();
    let packages = components.next();
    if !matches!(owner, Some(Component::Normal(_)))
        || packages != Some(Component::Normal("packages".as_ref()))
        || path.components().any(|component| !matches!(component, Component::Normal(_)))
        || path.components().count() < 3
    {
        return Err(format!(
            "sdk_child.executable '{declared}' must name a normal path under <owner>/packages/"
        ));
    }
    Ok(path.to_path_buf())
}

fn validate_claude_executable(value: &str) -> Result<(), String> {
    validate_identity(value, "private_config.sdk_child.claude_executable")?;
    if value.contains('/') && !Path::new(value).is_absolute() {
        return Err("sdk_child.claude_executable must be a bare command or an absolute path".into());
    }
    Ok(())
}

pub fn verify_claude_grant(interface_dir: &Path, claude_executable: &str) -> Result<(), String> {
    let life_path = interface_dir.join("life.json");
    let life: Value = serde_json::from_slice(
        &std::fs::read(&life_path).map_err(|error| format!("'{}' is unreadable: {error}", life_path.display()))?,
    )
    .map_err(|error| format!("'{}' is malformed: {error}", life_path.display()))?;
    let authority = life
        .pointer("/processes/0/authority")
        .ok_or_else(|| format!("'{}' declares no process authority", life_path.display()))?;
    let (field, granted) = if Path::new(claude_executable).is_absolute() {
        ("executable_paths", authority.get("executable_paths"))
    } else {
        ("executable_commands", authority.get("executable_commands"))
    };
    let granted = granted
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(claude_executable)));
    if !granted {
        return Err(format!(
            "sdk_child.claude_executable '{claude_executable}' is not granted by life.json authority.{field}; grant exactly that value there"
        ));
    }
    Ok(())
}

pub fn validate_identity(value: &str, name: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(format!("invalid {name}"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct SdkChildReceipt {
    schema_version: u32,
    executable: String,
    executable_sha256: String,
    executable_bytes: u64,
}

pub fn verify_sdk_child(executable: &Path) -> Result<(), String> {
    let bin = executable
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "bin"))
        .ok_or("sdk child executable must live in its package's bin/ folder")?;
    let package = bin.parent().ok_or("sdk child has no package folder")?;
    let receipt_path = package.join(SDK_CHILD_RECEIPT);
    let receipt: SdkChildReceipt = serde_json::from_slice(
        &std::fs::read(&receipt_path)
            .map_err(|error| format!("sdk child receipt '{}' is unreadable: {error}", receipt_path.display()))?,
    )
    .map_err(|error| format!("sdk child receipt is malformed: {error}"))?;
    let bytes = std::fs::read(executable)
        .map_err(|error| format!("sdk child '{}' is unreadable: {error}", executable.display()))?;
    let expected = format!(
        "bin/{}",
        executable
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("sdk child file name is not UTF-8")?
    );
    if receipt.schema_version != 1
        || receipt.executable != expected
        || receipt.executable_bytes != bytes.len() as u64
        || receipt.executable_sha256 != sha256_hex(&bytes)
    {
        return Err(format!(
            "sdk child receipt '{}' does not attest '{}'",
            receipt_path.display(),
            executable.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(executable)
            .map_err(|error| format!("sdk child metadata unreadable: {error}"))?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(format!("sdk child '{}' is not executable", executable.display()));
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
