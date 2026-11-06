use std::collections::BTreeMap;
use std::time::Duration;

use octopus_tool_door_runtime::ToolProcessEnvironment;
use octopus_worker_env::{WorkerEnvKindSpec, WorkerEnvironmentFormation};
use serde::Deserialize;

pub(crate) const CONFIG_FILE: &str = "desktop.json";
const MAX_REQUEST_TIMEOUT_MS: u64 = 600_000;
const MAX_RESPONSE_BYTES: usize = crate::protocol::MAX_NATIVE_RESULT_BYTES;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppApprovalConfig {
    pub(crate) operator_interface: octopus_descriptors::InterfaceId,
    pub(crate) resume_tool: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationPreferences {
    pub root_slot: String,
}

impl ApplicationPreferences {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.root_slot.is_empty() || self.root_slot.len() > 128
            || !self.root_slot.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            return Err("application preferences require an explicit private root slot".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceLock {
    pub root_slot: String,
    pub file_name: String,
}

impl DeviceLock {
    pub fn resolve(&self, roots: &BTreeMap<String, std::path::PathBuf>) -> Result<std::path::PathBuf, String> {
        if self.root_slot.is_empty() || self.root_slot.len() > 128
            || !self.root_slot.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') {
            return Err("desktop device lock requires an explicit root slot".into());
        }
        let name = std::path::Path::new(&self.file_name);
        if self.file_name.len() > 255 || self.file_name.contains('\0')
            || !matches!(name.components().collect::<Vec<_>>().as_slice(), [std::path::Component::Normal(part)] if *part == name.as_os_str()) {
            return Err("desktop device lock file_name must be one plain file name inside its root slot".into());
        }
        let root = roots.get(&self.root_slot).ok_or_else(|| format!(
            "desktop device lock root slot '{}' was not declared to this desktop host; declare it in host.json root_refs and in the host process authority",
            self.root_slot
        ))?;
        crate::native_connection::validate_directory(root).map_err(|error| format!(
            "desktop device lock root slot '{}' at '{}' is unusable: {error}",
            self.root_slot, root.display()
        ))?;
        Ok(root.join(name))
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesktopConfig {
    schema_version: u32,
    pub(crate) action_url: String,
    pub(crate) viewer_url: String,
    #[serde(default)]
    pub(crate) auth_env_key: Option<String>,
    #[serde(default = "default_auth_header")]
    pub(crate) auth_header: String,
    #[serde(default = "default_auth_value_prefix")]
    pub(crate) auth_value_prefix: String,
    #[serde(default)]
    pub(crate) headers: BTreeMap<String, String>,
    #[serde(default = "default_request_timeout_ms")]
    pub(crate) request_timeout_ms: u64,
    #[serde(default = "default_max_response_bytes")]
    pub(crate) max_response_bytes: usize,
    #[serde(default)]
    pub(crate) host: Option<crate::backend::HostConfig>,
    #[serde(default)]
    pub(crate) app_approval: Option<AppApprovalConfig>,
    #[serde(skip)]
    pub(crate) native_connection: Option<crate::native_connection::PrivateConnection>,
}

impl DesktopConfig {
    pub(crate) fn read(path: &std::path::Path) -> Result<Self, String> {
        let config: Self = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }
    pub(crate) fn load_formed(
        environment: &ToolProcessEnvironment,
        formation: &WorkerEnvironmentFormation,
        kind: &WorkerEnvKindSpec,
    ) -> Result<Self, String> {
        let mut config: Self = environment.load_formed_json_sidecar(formation, kind, CONFIG_FILE)?;
        config.validate()?;
        if let Some(host) = &config.host {
            let connection = crate::native_connection::PrivateConnection::read(
                environment.private_read_path(&host.connection_file)?, environment.interface.as_str(),
            )?;
            config.action_url = connection.action_url.clone();
            config.viewer_url = connection.viewer_url.clone();
            config.native_connection = Some(connection);
        }
        Ok(config)
    }

    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }

    pub(crate) fn validate_process_inputs(&self, descriptor: &octopus_tool_registry::ToolDescriptor) -> Result<(), String> {
        let process = &descriptor.process;
        if !process.env.is_empty() || !process.inherit_env_from_interface.is_empty() {
            return Err("desktop transport credentials require explicit process input declarations".into());
        }
        if self.host.is_some() {
            if !process.inherit_env.is_empty() || process.private_read_subpaths.len() != 1
                || !process.private_read_subpaths.iter().any(|input| input.path == crate::native_connection::CONNECTION_FILE) {
                return Err("native desktop tools require only their private connection input and no remote auth environment".into());
            }
        } else {
            let expected = self.auth_env_key.iter().cloned().collect::<std::collections::BTreeSet<_>>();
            if !process.private_read_subpaths.is_empty() || process.inherit_env != expected {
                return Err("remote desktop tools require no native private input and exactly their declared auth environment key".into());
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "desktop config has schema_version {}, expected 1",
                self.schema_version
            ));
        }
        validate_url("action_url", &self.action_url, &["http", "https"])?;
        validate_url(
            "viewer_url",
            &self.viewer_url,
            &["http", "https", "ws", "wss"],
        )?;
        if let Some(key) = self.auth_env_key.as_deref() {
            if key.trim() != key || !valid_env_key(key) {
                return Err(
                    "desktop auth_env_key must be an unpadded ASCII environment key".to_string(),
                );
            }
        }
        if let Some(host) = &self.host {
            host.application_behavior.validate()?;
            if let Some(policy) = &host.application_policy { policy.validate()?; }
            if let Some(preferences) = &host.application_preferences { preferences.validate()?; }
            if host.native_sandbox_profile.is_empty()
                || !std::path::Path::new(&host.native_sandbox_profile).components().all(|component| matches!(component, std::path::Component::Normal(_))) {
                return Err("native desktop host requires a declared package sandbox profile".into());
            }
            if host.connection_file != crate::native_connection::CONNECTION_FILE
                || host.state_directory != "desktop-host"
                || self.action_url != "http://127.0.0.1:0/v1/action"
                || self.viewer_url != "http://127.0.0.1:0/view"
                || self.auth_env_key.is_some() || self.auth_header != "Authorization"
                || self.auth_value_prefix != "Bearer " || !self.headers.is_empty() {
                return Err("native desktop config must declare its private rendezvous, port zero and private authentication".into());
            }
        }
        if let Some(approval) = &self.app_approval {
            if self.host.is_none() || approval.operator_interface.as_str().is_empty()
                || approval.resume_tool.is_empty() || approval.resume_tool.trim() != approval.resume_tool
                || approval.resume_tool.contains('\0') || approval.resume_tool.len() > 4096 {
                return Err("application approval requires a native host and exact declared operator/resume identities".into());
            }
        }
        validate_header_name("auth_header", &self.auth_header)?;
        if self.auth_value_prefix.contains('\r') || self.auth_value_prefix.contains('\n') {
            return Err("desktop auth_value_prefix contains a line break".to_string());
        }
        for (name, value) in &self.headers {
            validate_header_name("headers key", name)?;
            if value.contains('\r') || value.contains('\n') {
                return Err(format!("desktop header '{name}' contains a line break"));
            }
            if matches!(
                name.to_ascii_lowercase().as_str(),
                "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
            ) {
                return Err(format!(
                    "desktop header '{name}' is secret-bearing; use auth_env_key"
                ));
            }
        }
        if self.request_timeout_ms == 0 || self.request_timeout_ms > MAX_REQUEST_TIMEOUT_MS {
            return Err(format!(
                "desktop request_timeout_ms must be in 1..={MAX_REQUEST_TIMEOUT_MS}"
            ));
        }
        if !(1024..=MAX_RESPONSE_BYTES).contains(&self.max_response_bytes) {
            return Err(format!(
                "desktop max_response_bytes must be in 1024..={MAX_RESPONSE_BYTES}"
            ));
        }
        Ok(())
    }
}

fn validate_url(field: &str, value: &str, schemes: &[&str]) -> Result<(), String> {
    if value.trim() != value || value.is_empty() {
        return Err(format!("desktop {field} is empty or padded"));
    }
    let url =
        url::Url::parse(value).map_err(|error| format!("desktop {field} is invalid: {error}"))?;
    if !schemes.contains(&url.scheme()) || url.host_str().is_none() {
        return Err(format!(
            "desktop {field} must be an absolute {} URL",
            schemes.join("/")
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "desktop {field} must not contain credentials, query, or fragment"
        ));
    }
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if matches!(url.scheme(), "http" | "ws") && !loopback {
        return Err(format!("desktop {field} must use TLS outside loopback"));
    }
    Ok(())
}

fn validate_header_name(field: &str, value: &str) -> Result<(), String> {
    if value.trim() != value
        || value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(format!("desktop {field} is not a valid HTTP header name"));
    }
    Ok(())
}

fn valid_env_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn default_auth_header() -> String {
    "Authorization".to_string()
}

fn default_auth_value_prefix() -> String {
    "Bearer ".to_string()
}

fn default_request_timeout_ms() -> u64 {
    30_000
}

fn default_max_response_bytes() -> usize {
    8 * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::validate_url;

    #[test]
    fn remote_desktop_urls_require_tls_and_viewers_carry_no_secret_components() {
        assert!(validate_url(
            "action_url",
            "http://127.0.0.1:9222/action",
            &["http", "https"]
        )
        .is_ok());
        assert!(validate_url(
            "viewer_url",
            "ws://[::1]:9222/view",
            &["http", "https", "ws", "wss"]
        )
        .is_ok());
        assert!(validate_url(
            "action_url",
            "https://desktop.example/action",
            &["http", "https"]
        )
        .is_ok());
        assert!(validate_url(
            "viewer_url",
            "wss://desktop.example/view",
            &["http", "https", "ws", "wss"]
        )
        .is_ok());
        assert!(validate_url(
            "action_url",
            "http://desktop.example/action",
            &["http", "https"]
        )
        .is_err());
        assert!(validate_url(
            "viewer_url",
            "ws://desktop.example/view",
            &["http", "https", "ws", "wss"]
        )
        .is_err());
        assert!(validate_url(
            "viewer_url",
            "wss://desktop.example/view?token=secret",
            &["http", "https", "ws", "wss"]
        )
        .is_err());
    }

    #[test]
    fn device_locks_stay_inside_their_declared_roots_and_refuse_missing_or_unusable_roots() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};
        let lock = super::DeviceLock { root_slot: "desktop_device_lock_root".into(), file_name: "desktop-device.lock".into() };
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().canonicalize().unwrap();
        let private_root = |name: &str| {
            let path = parent.join(name);
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        };
        let roots = |root: &Path| std::collections::BTreeMap::from([(lock.root_slot.clone(), root.to_path_buf())]);
        let (first, second) = (private_root("first-user"), private_root("second-user"));
        let first_lock: PathBuf = lock.resolve(&roots(&first)).unwrap();
        let second_lock = lock.resolve(&roots(&second)).unwrap();
        assert_eq!(first_lock, first.join("desktop-device.lock"));
        assert_ne!(first_lock, second_lock);
        let exclusive = |file: &std::fs::File| unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        let first_holder = crate::backend::open_device_file(&first_lock).unwrap();
        let second_holder = crate::backend::open_device_file(&second_lock).unwrap();
        assert!(exclusive(&first_holder));
        assert!(exclusive(&second_holder));
        assert!(!exclusive(&crate::backend::open_device_file(&first_lock).unwrap()));

        let undeclared = lock.resolve(&std::collections::BTreeMap::new()).unwrap_err();
        assert!(undeclared.contains("root slot 'desktop_device_lock_root' was not declared"), "{undeclared}");
        let absent = lock.resolve(&roots(&parent.join("absent"))).unwrap_err();
        assert!(absent.contains("desktop_device_lock_root") && absent.contains("is unusable"), "{absent}");
        let read_only = private_root("read-only");
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o500)).unwrap();
        let unwritable = lock.resolve(&roots(&read_only)).unwrap_err();
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(unwritable.contains("is unusable"), "{unwritable}");
        assert!(!read_only.join("desktop-device.lock").exists());
        for file_name in ["", ".", "..", "../escape.lock", "nested/desktop-device.lock", "desktop-device.lock/"] {
            let escaped = super::DeviceLock { root_slot: lock.root_slot.clone(), file_name: file_name.into() };
            assert!(escaped.resolve(&roots(&first)).unwrap_err().contains("one plain file name"), "{file_name}");
        }
    }
}


#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationBehavior {
    pub schema_version: u32,
    pub defaults: ApplicationProfile,
    pub profiles: Vec<SelectedApplicationProfile>,
    pub instruction_resource: ApplicationInstructionResource,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedApplicationProfile {
    pub selector: ApplicationSelector,
    pub behavior: ApplicationProfile,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(tag = "match", content = "bundle_id", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationSelector { Exact(String), Prefix(String) }

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseTargeting { NativeHit, SuppliedWindow }

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationProfile {
    pub full_ui: AxEnablementOptions,
    pub event_stream: AxEnablementOptions,
    pub informational: AxEnablementOptions,
    pub typing_interval_us: u32,
    pub focus_dom_field_by_click: bool,
    pub mouse_targeting: MouseTargeting,
    pub suppress_menu_dismissal_on_window: bool,
    pub menu_bar_presentation: MenuBarPresentation,
    pub accessibility_presentation: AccessibilityPresentation,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessibilityPresentation {
    pub context_type: String,
    pub hide_cancel_actions: bool,
    pub omit_calendar_event_children: bool,
    pub excluded_action_descriptions: Vec<String>,
    pub capture_note: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuBarPresentation {
    pub omit_first_item: bool,
    pub excluded_title_substrings: Vec<String>,
    pub clear_immediate_actions: bool,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AxEnablementOptions {
    pub attribute: AxAttributeMode,
    pub enforce_focus_before_enhanced: bool,
    pub universal_access: AxUniversalAccessMode,
    pub handshake: AxHandshake,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AxAttributeMode { None, ManualAccessibility, EnhancedUserInterface, ManualIfSupportedOtherwiseEnhanced }
#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AxUniversalAccessMode { None, UntilRelease, Pulse100ms }
#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AxHandshake { None, NativeWindow }

impl ApplicationBehavior {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 || self.profiles.len() > 64 || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 65536 {
            return Err("native application behavior requires schema version 1 and a bounded profile set".into());
        }
        self.instruction_resource.validate()?;
        let mut selectors = std::collections::BTreeSet::new();
        for selected in &self.profiles {
            let (kind, value) = match &selected.selector {
                ApplicationSelector::Exact(value) => (0, value),
                ApplicationSelector::Prefix(value) => (1, value),
            };
            if value.is_empty() || value.len() > 255 || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
                || !selectors.insert((kind, value)) {
                return Err("native application behavior has an invalid or repeated exact selector".into());
            }
        }
        for profile in std::iter::once(&self.defaults).chain(self.profiles.iter().map(|p| &p.behavior)) {
            let presentation = &profile.accessibility_presentation;
            if presentation.context_type != "full_ui" || presentation.excluded_action_descriptions.len() > 128
                || presentation.excluded_action_descriptions.iter().any(|text| text.is_empty() || text.len() > 2048 || text.contains('\0'))
                || presentation.capture_note.as_ref().is_some_and(|text| text.len() > 32768 || text.contains('\0')) {
                return Err("application accessibility presentation has an unsupported context or unbounded data".into());
            }
            let menu = &profile.menu_bar_presentation;
            if menu.excluded_title_substrings.len() > 16 || menu.excluded_title_substrings.iter().any(|s| s.is_empty() || s.contains('\0') || s.len() > 2048)
                || menu.excluded_title_substrings.iter().collect::<std::collections::BTreeSet<_>>().len() != menu.excluded_title_substrings.len() {
                return Err("native menu presentation requires bounded unique explicit title filters".into());
            }
            if profile.typing_interval_us > 1_000_000 {
                return Err("native application typing interval exceeds its bounded native event domain".into());
            }
            for options in [&profile.full_ui, &profile.event_stream, &profile.informational] {
                if options.enforce_focus_before_enhanced && !matches!(options.attribute, AxAttributeMode::EnhancedUserInterface | AxAttributeMode::ManualIfSupportedOtherwiseEnhanced) {
                    return Err("native focus preparation requires the enhanced accessibility attribute".into());
                }
            }
        }
        Ok(())
    }

    pub fn resolve(&self, bundle_id: &str) -> Result<&ApplicationProfile, String> {
        let mut matched = self.profiles.iter().filter(|profile| match &profile.selector {
            ApplicationSelector::Exact(id) => bundle_id == id,
            ApplicationSelector::Prefix(prefix) => bundle_id.starts_with(prefix),
        });
        let result = matched.next().map(|p| &p.behavior).unwrap_or(&self.defaults);
        if matched.next().is_some() { return Err("native application behavior has overlapping selectors for this exact target".into()); }
        Ok(result)
    }
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationInstructionResource {
    pub path: String,
    pub content_sha256: String,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationInstructionText {
    pub content_sha256: String,
    pub text: String,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationInstructionCatalog {
    pub schema_version: u32,
    pub resources: BTreeMap<String, ApplicationInstructionText>,
    pub aliases: BTreeMap<String, Vec<String>>,
    pub suppressed_bundle_ids: std::collections::BTreeSet<String>,
    pub http_scheme: Option<ApplicationInstructionText>,
}

impl ApplicationInstructionText {
    pub(crate) fn validate(&self) -> Result<(), String> {
        use sha2::Digest;
        if self.text.trim().is_empty() || self.text.len() > 32768 || self.text.contains('\0')
            || format!("{:x}", sha2::Sha256::digest(self.text.as_bytes())) != self.content_sha256 {
            return Err("native application instructions require bounded UTF-8 text and its exact content digest".into());
        }
        Ok(())
    }
}

impl ApplicationInstructionResource {
    fn validate(&self) -> Result<(), String> {
        if self.path.is_empty() || !std::path::Path::new(&self.path).components().all(|part| matches!(part, std::path::Component::Normal(_)))
            || self.content_sha256.len() != 64 || !self.content_sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Err("native application instruction resource requires a package-relative path and exact SHA-256".into());
        }
        Ok(())
    }

    pub(crate) fn read(&self, root: &std::path::Path) -> Result<ApplicationInstructionCatalog, String> {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        use sha2::Digest;
        self.validate()?;
        let root = root.canonicalize().map_err(|error| error.to_string())?;
        let path = root.join(&self.path);
        if path.canonicalize().map_err(|error| error.to_string())? != path {
            return Err("native application instruction resource must stay in its exact package without symlinks".into());
        }
        let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path).map_err(|error| error.to_string())?;
        let stat = file.metadata().map_err(|error| error.to_string())?;
        if !stat.is_file() || stat.len() > 65536 || stat.mode() & 0o022 != 0 {
            return Err("native application instruction resource must be a bounded protected regular file".into());
        }
        let mut bytes = Vec::new();
        file.by_ref().take(65537).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
        if bytes.len() > 65536 || format!("{:x}", sha2::Sha256::digest(&bytes)) != self.content_sha256 {
            return Err("native application instruction resource changed from its declared content digest".into());
        }
        let result: ApplicationInstructionCatalog = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        result.validate()?;
        Ok(result)
    }
}

impl ApplicationInstructionCatalog {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let bundle = |id: &str| !id.is_empty() && id.len() <= 255 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b));
        if self.schema_version != 1 || self.resources.len() > 64 || self.aliases.len() > 64 || self.suppressed_bundle_ids.len() > 64
            || serde_json::to_vec(self).map_err(|error| error.to_string())?.len() > 65536 {
            return Err("native application instruction catalog exceeds its declared schema or bounds".into());
        }
        for (name, text) in &self.resources {
            if name.is_empty() || name.len() > 255 || name.trim() != name || name.contains(['/', '\\', '\0']) || name == "." || name == ".." {
                return Err("native application instruction resource name must be one exact basename".into());
            }
            text.validate()?;
        }
        for (id, names) in &self.aliases {
            if !bundle(id) || names.len() > 8 || names.iter().collect::<std::collections::BTreeSet<_>>().len() != names.len()
                || names.iter().any(|name| !self.resources.contains_key(name)) {
                return Err("native application instruction aliases must reference exact declared resources".into());
            }
        }
        if self.suppressed_bundle_ids.iter().any(|id| !bundle(id)) { return Err("native application instruction suppression requires exact bundle identities".into()); }
        if let Some(text) = &self.http_scheme { text.validate()?; }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationAccess { Allow, Deny }

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationAccessPolicy {
    pub default_access: ApplicationAccess,
    pub bundle_ids: BTreeMap<String, ApplicationAccess>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationPolicy {
    pub schema_version: u32,
    pub enabled: bool,
    pub allow_persistent_approval: bool,
    pub required_access: ApplicationAccessPolicy,
    pub configured_access: ApplicationAccessPolicy,
    pub forbidden_bundle_ids: Vec<String>,
    pub elevated_risk_bundle_ids: Vec<String>,
    pub high_risk_url_schemes: Vec<String>,
    pub high_risk_warning_subtitle: String,
}

impl ApplicationPolicy {
    pub fn validate(&self) -> Result<(), String> {
        let exact = |text: &String| !text.is_empty() && text.len() <= 4096 && text.trim() == text && !text.contains('\0');
        if self.schema_version != 1 || !exact(&self.high_risk_warning_subtitle)
            || self.required_access.bundle_ids.len() > 4096 || self.configured_access.bundle_ids.len() > 4096
            || self.required_access.bundle_ids.keys().chain(self.configured_access.bundle_ids.keys()).any(|value| !exact(value)) {
            return Err("application eligibility policy has invalid declared values".into());
        }
        for values in [&self.forbidden_bundle_ids, &self.elevated_risk_bundle_ids, &self.high_risk_url_schemes] {
            if values.len() > 4096 || values.iter().any(|value| !exact(value))
                || values.iter().collect::<std::collections::BTreeSet<_>>().len() != values.len() {
                return Err("application eligibility policy requires bounded distinct exact values".into());
            }
        }
        if self.bytes()?.len() > 65536 { return Err("application eligibility policy exceeds its private input bound".into()); }
        Ok(())
    }

    pub(crate) fn bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_value(self).map(|value| octopus_descriptors::canonical_bytes(&value)).map_err(|error| error.to_string())
    }

    pub(crate) fn digest(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        Ok(format!("{:x}", Sha256::digest(self.bytes()?)))
    }
}
