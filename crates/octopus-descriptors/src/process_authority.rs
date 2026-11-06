use std::collections::BTreeSet;
use std::path::{Component, Path};

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSandboxMode {
    #[default]
    Enforced,
    Disabled,
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ProcessAuthoritySpec {
    #[serde(default)]
    pub sandbox_mode: ProcessSandboxMode,
    pub read_root_slots: BTreeSet<String>,
    pub write_root_slots: BTreeSet<String>,
    pub executable_root_slots: BTreeSet<String>,
    #[serde(default)]
    pub optional_read_root_slots: BTreeSet<String>,
    #[serde(default)]
    pub optional_write_root_slots: BTreeSet<String>,
    #[serde(default)]
    pub optional_executable_root_slots: BTreeSet<String>,
    pub read_paths: BTreeSet<String>,
    pub write_paths: BTreeSet<String>,
    pub executable_paths: BTreeSet<String>,
    #[serde(default)]
    pub executable_root_subpaths: BTreeSet<ProcessRootSubpathSpec>,
    pub executable_commands: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_executable_relative_paths: Vec<ProcessExecutableRelativePathSpec>,
    pub read_env_paths: BTreeSet<String>,
    pub write_env_paths: BTreeSet<String>,
    pub executable_env_paths: BTreeSet<String>,
    #[serde(default)]
    pub optional_read_env_paths: BTreeSet<String>,
    #[serde(default)]
    pub optional_write_env_paths: BTreeSet<String>,
    #[serde(default)]
    pub read_env_subpaths: Vec<ProcessEnvironmentSubpathSpec>,
    #[serde(default)]
    pub write_env_subpaths: Vec<ProcessEnvironmentSubpathSpec>,
    #[serde(default)]
    pub executable_env_subpaths: Vec<ProcessEnvironmentSubpathSpec>,
    #[serde(default)]
    pub read_argument_paths: Vec<ProcessArgumentPathSpec>,
    #[serde(default)]
    pub write_argument_paths: Vec<ProcessArgumentPathSpec>,
    #[serde(default)]
    pub executable_argument_paths: Vec<ProcessArgumentPathSpec>,
    pub allow_network: bool,
    pub allow_local_ipc: bool,
    #[serde(default)]
    pub allow_application_launch: bool,
    #[serde(default)]
    pub allow_sandbox_delegation: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_process_signals: bool,
}

impl ProcessAuthoritySpec {
    pub fn declared_read_root_slots(&self) -> BTreeSet<&String> {
        self.read_root_slots
            .iter()
            .chain(&self.optional_read_root_slots)
            .chain(self.read_argument_paths.iter().flat_map(|binding| &binding.within_root_slots))
            .collect()
    }

    pub fn declared_write_root_slots(&self) -> BTreeSet<&String> {
        self.write_root_slots
            .iter()
            .chain(&self.optional_write_root_slots)
            .chain(self.write_argument_paths.iter().flat_map(|binding| &binding.within_root_slots))
            .collect()
    }
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ProcessArgumentPathSpec {
    pub pointer: String,
    pub within_root_slots: BTreeSet<String>,
    #[serde(default)]
    pub include_parent: bool,
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ProcessEnvironmentSubpathSpec {
    pub env: String,
    pub relative_path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, schemars::JsonSchema, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessExecutableRelativePathSpec {
    pub executable: String,
    pub relative_path: String,
}

#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct ProcessRootSubpathSpec {
    pub root_slot: String,
    pub relative_path: String,
}

impl ProcessAuthoritySpec {
    /// Every right this authority grants is also granted by `original`.
    pub fn narrows(&self, original: &ProcessAuthoritySpec) -> bool {
        let paths = |copy: &[ProcessArgumentPathSpec], held: &[ProcessArgumentPathSpec]| {
            copy.iter().all(|path| {
                held.iter().any(|known| {
                    known.pointer == path.pointer
                        && path.within_root_slots.is_subset(&known.within_root_slots)
                        && (!path.include_parent || known.include_parent)
                })
            })
        };
        let subpaths = |copy: &[ProcessEnvironmentSubpathSpec], held: &[ProcessEnvironmentSubpathSpec]| {
            copy.iter().all(|path| held.contains(path))
        };
        (self.sandbox_mode == original.sandbox_mode || self.sandbox_mode == ProcessSandboxMode::Enforced)
            && self.read_root_slots.is_subset(&original.read_root_slots)
            && self.write_root_slots.is_subset(&original.write_root_slots)
            && self.executable_root_slots.is_subset(&original.executable_root_slots)
            && self.optional_read_root_slots.is_subset(&original.optional_read_root_slots)
            && self.optional_write_root_slots.is_subset(&original.optional_write_root_slots)
            && self.optional_executable_root_slots.is_subset(&original.optional_executable_root_slots)
            && self.read_paths.is_subset(&original.read_paths)
            && self.write_paths.is_subset(&original.write_paths)
            && self.executable_paths.is_subset(&original.executable_paths)
            && self.executable_root_subpaths.is_subset(&original.executable_root_subpaths)
            && self.executable_commands.is_subset(&original.executable_commands)
            && self.read_executable_relative_paths.iter()
                .all(|path| original.read_executable_relative_paths.contains(path))
            && self.read_env_paths.is_subset(&original.read_env_paths)
            && self.write_env_paths.is_subset(&original.write_env_paths)
            && self.executable_env_paths.is_subset(&original.executable_env_paths)
            && self.optional_read_env_paths.is_subset(&original.optional_read_env_paths)
            && self.optional_write_env_paths.is_subset(&original.optional_write_env_paths)
            && subpaths(&self.read_env_subpaths, &original.read_env_subpaths)
            && subpaths(&self.write_env_subpaths, &original.write_env_subpaths)
            && subpaths(&self.executable_env_subpaths, &original.executable_env_subpaths)
            && paths(&self.read_argument_paths, &original.read_argument_paths)
            && paths(&self.write_argument_paths, &original.write_argument_paths)
            && paths(&self.executable_argument_paths, &original.executable_argument_paths)
            && (!self.allow_network || original.allow_network)
            && (!self.allow_local_ipc || original.allow_local_ipc)
            && (!self.allow_application_launch || original.allow_application_launch)
            && (!self.allow_sandbox_delegation || original.allow_sandbox_delegation)
            && (!self.allow_process_signals || original.allow_process_signals)
    }

    pub fn has_argument_paths(&self) -> bool {
        !self.read_argument_paths.is_empty()
            || !self.write_argument_paths.is_empty()
            || !self.executable_argument_paths.is_empty()
    }

    pub fn validate(&self) -> Result<(), String> {
        for (field, values) in [
            ("read_root_slots", &self.read_root_slots),
            ("write_root_slots", &self.write_root_slots),
            ("executable_root_slots", &self.executable_root_slots),
            ("optional_read_root_slots", &self.optional_read_root_slots),
            ("optional_write_root_slots", &self.optional_write_root_slots),
            (
                "optional_executable_root_slots",
                &self.optional_executable_root_slots,
            ),
        ] {
            for value in values {
                validate_atom(field, value)?;
            }
        }
        for (required, optional, field) in [
            (
                &self.read_root_slots,
                &self.optional_read_root_slots,
                "read_root_slots",
            ),
            (
                &self.write_root_slots,
                &self.optional_write_root_slots,
                "write_root_slots",
            ),
            (
                &self.executable_root_slots,
                &self.optional_executable_root_slots,
                "executable_root_slots",
            ),
        ] {
            if !required.is_disjoint(optional) {
                return Err(format!(
                    "process authority repeats a root slot across {field} and optional_{field}"
                ));
            }
        }
        for (field, values) in [
            ("read_paths", &self.read_paths),
            ("write_paths", &self.write_paths),
            ("executable_paths", &self.executable_paths),
        ] {
            for value in values {
                validate_declared_path(field, value)?;
            }
        }
        for value in &self.executable_commands {
            validate_declared_executable("executable_commands", value)?;
        }
        for value in &self.read_executable_relative_paths {
            if !self.executable_commands.contains(&value.executable) {
                return Err(format!("process authority read_executable_relative_paths names undeclared executable {:?}", value.executable));
            }
            validate_atom("read_executable_relative_paths.relative_path", &value.relative_path)?;
            if value.relative_path.contains('\0') || Path::new(&value.relative_path).components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::ParentDir | Component::CurDir))
            {
                return Err("process authority read_executable_relative_paths requires a relative path".into());
            }
        }
        for value in &self.executable_root_subpaths {
            value.validate("executable_root_subpaths")?;
        }
        for (field, values) in [
            ("read_env_paths", &self.read_env_paths),
            ("write_env_paths", &self.write_env_paths),
            ("executable_env_paths", &self.executable_env_paths),
            ("optional_read_env_paths", &self.optional_read_env_paths),
            ("optional_write_env_paths", &self.optional_write_env_paths),
        ] {
            for value in values {
                validate_env_name(field, value)?;
            }
        }
        if !self
            .read_env_paths
            .is_disjoint(&self.optional_read_env_paths)
        {
            return Err(
                "process authority repeats an env name across read_env_paths and optional_read_env_paths"
                    .to_string(),
            );
        }
        if !self
            .write_env_paths
            .is_disjoint(&self.optional_write_env_paths)
        {
            return Err(
                "process authority repeats an env name across write_env_paths and optional_write_env_paths"
                    .to_string(),
            );
        }
        for (field, values) in [
            ("read_env_subpaths", &self.read_env_subpaths),
            ("write_env_subpaths", &self.write_env_subpaths),
            ("executable_env_subpaths", &self.executable_env_subpaths),
        ] {
            for value in values {
                value.validate(field)?;
            }
        }
        for (field, values) in [
            ("read_argument_paths", &self.read_argument_paths),
            ("write_argument_paths", &self.write_argument_paths),
            ("executable_argument_paths", &self.executable_argument_paths),
        ] {
            for value in values {
                value.validate(field)?;
            }
        }
        Ok(())
    }
}

impl ProcessRootSubpathSpec {
    fn validate(&self, field: &str) -> Result<(), String> {
        validate_atom(&format!("{field}.root_slot"), &self.root_slot)?;
        validate_relative_subpath(&format!("{field}.relative_path"), &self.relative_path)
    }
}

impl ProcessArgumentPathSpec {
    fn validate(&self, field: &str) -> Result<(), String> {
        if !self.pointer.starts_with('/') || self.pointer == "/" {
            return Err(format!(
                "process authority {field} contains invalid pointer {:?}",
                self.pointer
            ));
        }
        if self.within_root_slots.is_empty() {
            return Err(format!(
                "process authority {field} pointer {:?} names no root slots",
                self.pointer
            ));
        }
        for slot in &self.within_root_slots {
            validate_atom(field, slot)?;
        }
        Ok(())
    }
}

impl ProcessEnvironmentSubpathSpec {
    fn validate(&self, field: &str) -> Result<(), String> {
        validate_env_name(field, &self.env)?;
        validate_relative_subpath(field, &self.relative_path)
    }
}

fn validate_atom(field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.trim() != value {
        return Err(format!(
            "process authority {field} contains invalid value {value:?}"
        ));
    }
    Ok(())
}

fn validate_env_name(field: &str, value: &str) -> Result<(), String> {
    validate_atom(field, value)?;
    if value.contains('=') || value.contains('\0') {
        return Err(format!(
            "process authority {field} contains invalid environment name {value:?}"
        ));
    }
    Ok(())
}

fn validate_declared_executable(field: &str, value: &str) -> Result<(), String> {
    validate_atom(field, value)?;
    let path = Path::new(value);
    if path.is_relative()
        && path.components().count() > 1
        && path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(format!(
            "process authority {field} contains parent traversal {value:?}"
        ));
    }
    Ok(())
}

fn validate_declared_path(field: &str, value: &str) -> Result<(), String> {
    validate_atom(field, value)?;
    let path = Path::new(value);
    if path.is_relative()
        && path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(format!(
            "process authority {field} contains parent traversal {value:?}"
        ));
    }
    Ok(())
}

fn validate_relative_subpath(field: &str, value: &str) -> Result<(), String> {
    validate_atom(field, value)?;
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "process authority {field} contains invalid relative subpath {value:?}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_relative_reads_are_explicit_and_cannot_be_added_by_a_copy() {
        let original = ProcessAuthoritySpec {
            executable_commands: BTreeSet::from(["interpreter".into()]),
            ..Default::default()
        };
        let mut declared = original.clone();
        declared.read_executable_relative_paths.push(ProcessExecutableRelativePathSpec {
            executable: "interpreter".into(), relative_path: "../lib".into(),
        });
        assert!(declared.validate().is_ok());
        assert!(!declared.narrows(&original));
        assert!(original.narrows(&declared));
        declared.read_executable_relative_paths[0].relative_path = "/unrelated".into();
        assert!(declared.validate().is_err());
        declared.read_executable_relative_paths[0].relative_path = "../lib".into();
        declared.executable_commands.clear();
        assert!(declared.validate().is_err());
    }

    #[test]
    fn legacy_authority_preserves_pinned_canonical_bytes() {
        let prior: serde_json::Value = serde_json::from_str(r#"{"sandbox_mode":"enforced","read_root_slots":[],"write_root_slots":[],"executable_root_slots":[],"optional_read_root_slots":[],"optional_write_root_slots":[],"optional_executable_root_slots":[],"read_paths":[],"write_paths":[],"executable_paths":[],"executable_root_subpaths":[],"executable_commands":[],"read_env_paths":[],"write_env_paths":[],"executable_env_paths":[],"optional_read_env_paths":[],"optional_write_env_paths":[],"read_env_subpaths":[],"write_env_subpaths":[],"executable_env_subpaths":[],"read_argument_paths":[],"write_argument_paths":[],"executable_argument_paths":[],"allow_network":false,"allow_local_ipc":false,"allow_application_launch":false,"allow_sandbox_delegation":false}"#).unwrap();
        let decoded: ProcessAuthoritySpec = serde_json::from_value(prior.clone()).unwrap();
        let before = serde_json::to_vec(&prior).unwrap();
        let after = serde_json::to_vec(&serde_json::to_value(decoded).unwrap()).unwrap();
        assert_eq!(after, before);
        assert_eq!(blake3::hash(&after), blake3::hash(&before));
    }

    #[test]
    fn a_copy_cannot_add_host_signal_authority() {
        let original = ProcessAuthoritySpec::default();
        let authorized = ProcessAuthoritySpec { allow_process_signals: true, ..original.clone() };
        assert!(!authorized.narrows(&original));
        assert!(original.narrows(&authorized));
        assert!(authorized.narrows(&authorized));
    }

    #[test]
    fn executable_root_subpaths_reject_parent_traversal() {
        let authority = ProcessAuthoritySpec {
            executable_root_subpaths: BTreeSet::from([ProcessRootSubpathSpec {
                root_slot: "registry_worker_environment_kind_root".to_string(),
                relative_path: "../foreign/packages/bin/tool".to_string(),
            }]),
            ..Default::default()
        };

        assert!(authority
            .validate()
            .unwrap_err()
            .contains("invalid relative subpath"));
    }
}
