#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use octopus_descriptors::{
    ProcessArgumentPathSpec, ProcessAuthoritySpec, ProcessEnvironmentSubpathSpec,
};

#[derive(Clone, Debug)]
pub struct ResolvedProcessAuthority {
    pub authority: ProcessAuthority,
    pub root_slots: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ProcessAuthority {
    sandbox_mode: octopus_descriptors::ProcessSandboxMode,
    readable: BTreeSet<PathBuf>,
    writable: BTreeSet<PathBuf>,
    executable: BTreeSet<PathBuf>,
    readable_exact: BTreeSet<PathBuf>,
    writable_exact: BTreeSet<PathBuf>,
    executable_exact: BTreeSet<PathBuf>,
    executable_links: BTreeSet<PathBuf>,
    allow_network: bool,
    allow_local_ipc: bool,
    allow_application_launch: bool,
    allow_sandbox_delegation: bool,
    allow_process_signals: bool,
}

const MAX_SEARCH_PATH_LINKS: usize = 40;

#[derive(Clone, Debug)]
pub struct PreparedProcess {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub executable: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
    pub sandbox_profile: Option<std::sync::Arc<SandboxProfileFile>>,
}

#[derive(Debug)]
pub struct SandboxProfileFile {
    path: PathBuf,
}

impl Drop for SandboxProfileFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessInvocationAuthorityError {
    Unavailable(String),
    Refused(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedInvocationPaths {
    pub readable: BTreeSet<PathBuf>,
    pub writable: BTreeSet<PathBuf>,
    pub executable: BTreeSet<PathBuf>,
}

impl ProcessAuthority {
    pub fn resolve(
        readable: impl IntoIterator<Item = PathBuf>,
        writable: impl IntoIterator<Item = PathBuf>,
        executable: impl IntoIterator<Item = PathBuf>,
        readable_exact: impl IntoIterator<Item = PathBuf>,
        writable_exact: impl IntoIterator<Item = PathBuf>,
        executable_exact: impl IntoIterator<Item = PathBuf>,
        allow_network: bool,
        allow_local_ipc: bool,
        allow_application_launch: bool,
        allow_sandbox_delegation: bool,
        allow_process_signals: bool,
        sandbox_mode: octopus_descriptors::ProcessSandboxMode,
    ) -> Result<Self, String> {
        let mut readable = readable
            .into_iter()
            .map(normalize_existing_path)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let writable = writable
            .into_iter()
            .map(normalize_declared_exact_path)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let executable = executable
            .into_iter()
            .map(normalize_existing_path)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut readable_exact = readable_exact
            .into_iter()
            .map(normalize_declared_exact_path)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let writable_exact = writable_exact
            .into_iter()
            .map(normalize_declared_exact_path)
            .collect::<Result<BTreeSet<_>, _>>()?;
        let executable_exact = executable_exact
            .into_iter()
            .map(normalize_existing_path)
            .collect::<Result<BTreeSet<_>, _>>()?;
        readable.extend(writable.iter().cloned());
        readable.extend(executable.iter().cloned());
        readable_exact.extend(writable_exact.iter().cloned());
        readable_exact.extend(executable_exact.iter().cloned());
        Ok(Self {
            sandbox_mode,
            readable,
            writable,
            executable,
            readable_exact,
            writable_exact,
            executable_exact,
            executable_links: BTreeSet::new(),
            allow_network,
            allow_local_ipc,
            allow_application_launch,
            allow_sandbox_delegation,
            allow_process_signals,
        })
    }

    fn with_executable_links(mut self, links: BTreeSet<PathBuf>) -> Self {
        self.executable_links = links;
        self
    }

    pub fn prepare(
        &self,
        declared_program: &str,
        args: &[OsString],
        cwd: &Path,
        search_path: Option<&OsStr>,
        profile_root: &Path,
    ) -> Result<PreparedProcess, String> {
        let cwd = normalize_existing_directory(cwd)?;
        let executable = resolve_executable(declared_program, &cwd, search_path)?;
        if self.sandbox_mode == octopus_descriptors::ProcessSandboxMode::Disabled {
            return Ok(PreparedProcess {
                program: executable.clone(),
                args: args.to_vec(),
                executable,
                environment: BTreeMap::new(),
                sandbox_profile: None,
            });
        }
        #[cfg(target_os = "macos")]
        {
            return prepare_macos(self, executable, args, &cwd, profile_root);
        }
        #[cfg(target_os = "linux")]
        {
            let _ = (self, executable, args, cwd, search_path, profile_root);
            return Err(
                "no Linux process-authority backend enforces independent read, write, and execute roots"
                    .to_string(),
            );
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (args, cwd, search_path, executable, profile_root);
            Err(
                "no fail-closed process authority backend is available on this operating system"
                    .to_string(),
            )
        }
    }
}

pub trait ProcessAuthoritySpecExecution {
    fn preflight_invocation_paths(
        &self,
        available_root_slots: &BTreeMap<String, PathBuf>,
        arguments: &serde_json::Value,
    ) -> Result<(), ProcessInvocationAuthorityError>;
    fn resolve_invocation_paths(
        &self,
        available_root_slots: &BTreeMap<String, PathBuf>,
        arguments: &serde_json::Value,
    ) -> Result<ResolvedInvocationPaths, ProcessInvocationAuthorityError>;
    fn resolve(
        &self,
        repository_root: &Path,
        available_root_slots: &BTreeMap<String, PathBuf>,
        environment: &BTreeMap<String, OsString>,
        extra_readable: impl IntoIterator<Item = PathBuf>,
        extra_writable: impl IntoIterator<Item = PathBuf>,
        extra_executable: impl IntoIterator<Item = PathBuf>,
        extra_readable_exact: impl IntoIterator<Item = PathBuf>,
    ) -> Result<ResolvedProcessAuthority, String>;
    fn resolve_for_invocation(
        &self,
        repository_root: &Path,
        available_root_slots: &BTreeMap<String, PathBuf>,
        environment: &BTreeMap<String, OsString>,
        arguments: &serde_json::Value,
        extra_readable: impl IntoIterator<Item = PathBuf>,
        extra_writable: impl IntoIterator<Item = PathBuf>,
        extra_executable: impl IntoIterator<Item = PathBuf>,
        extra_readable_exact: impl IntoIterator<Item = PathBuf>,
    ) -> Result<ResolvedProcessAuthority, String>;
    fn resolve_inner(
        &self,
        repository_root: &Path,
        available_root_slots: &BTreeMap<String, PathBuf>,
        environment: &BTreeMap<String, OsString>,
        arguments: Option<&serde_json::Value>,
        extra_readable: impl IntoIterator<Item = PathBuf>,
        extra_writable: impl IntoIterator<Item = PathBuf>,
        extra_executable: impl IntoIterator<Item = PathBuf>,
        extra_readable_exact: impl IntoIterator<Item = PathBuf>,
    ) -> Result<ResolvedProcessAuthority, String>;
}

impl ProcessAuthoritySpecExecution for ProcessAuthoritySpec {
    fn preflight_invocation_paths(
        &self,
        available_root_slots: &BTreeMap<String, PathBuf>,
        arguments: &serde_json::Value,
    ) -> Result<(), ProcessInvocationAuthorityError> {
        self.resolve_invocation_paths(available_root_slots, arguments)
            .map(|_| ())
    }

    fn resolve_invocation_paths(
        &self,
        available_root_slots: &BTreeMap<String, PathBuf>,
        arguments: &serde_json::Value,
    ) -> Result<ResolvedInvocationPaths, ProcessInvocationAuthorityError> {
        if !self.has_argument_paths() {
            return Ok(ResolvedInvocationPaths::default());
        }
        let requested_slots = self
            .read_argument_paths
            .iter()
            .chain(&self.write_argument_paths)
            .chain(&self.executable_argument_paths)
            .flat_map(|binding| binding.within_root_slots.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        let writable_slots = self
            .write_argument_paths
            .iter()
            .flat_map(|binding| binding.within_root_slots.iter())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut roots = BTreeMap::new();
        let mut unresolved = Vec::new();
        for slot in requested_slots {
            let Some(path) = available_root_slots.get(&slot) else {
                unresolved.push(format!("'{slot}' is unbound"));
                continue;
            };
            let path = if writable_slots.contains(&slot) {
                normalize_declared_exact_path(path.clone())
            } else {
                normalize_existing_path(path.clone())
            };
            match path {
                Ok(path) => {
                    roots.insert(slot, path);
                }
                Err(error) => unresolved.push(format!("'{slot}': {error}")),
            }
        }
        let refused = |error: String| {
            ProcessInvocationAuthorityError::Refused(if unresolved.is_empty() {
                error
            } else {
                format!("{error}; unresolved candidate root slots: {}", unresolved.join("; "))
            })
        };
        let readable = resolve_argument_paths(
            "read_argument_paths",
            &self.read_argument_paths,
            arguments,
            &roots,
        )
        .map_err(refused)?;
        let writable = resolve_argument_paths(
            "write_argument_paths",
            &self.write_argument_paths,
            arguments,
            &roots,
        )
        .map_err(refused)?;
        let executable = resolve_argument_paths(
            "executable_argument_paths",
            &self.executable_argument_paths,
            arguments,
            &roots,
        )
        .map_err(refused)?;
        Ok(ResolvedInvocationPaths {
            readable,
            writable,
            executable,
        })
    }

    fn resolve(
        &self,
        repository_root: &Path,
        available_root_slots: &BTreeMap<String, PathBuf>,
        environment: &BTreeMap<String, OsString>,
        extra_readable: impl IntoIterator<Item = PathBuf>,
        extra_writable: impl IntoIterator<Item = PathBuf>,
        extra_executable: impl IntoIterator<Item = PathBuf>,
        extra_readable_exact: impl IntoIterator<Item = PathBuf>,
    ) -> Result<ResolvedProcessAuthority, String> {
        self.resolve_inner(
            repository_root,
            available_root_slots,
            environment,
            None,
            extra_readable,
            extra_writable,
            extra_executable,
            extra_readable_exact,
        )
    }

    fn resolve_for_invocation(
        &self,
        repository_root: &Path,
        available_root_slots: &BTreeMap<String, PathBuf>,
        environment: &BTreeMap<String, OsString>,
        arguments: &serde_json::Value,
        extra_readable: impl IntoIterator<Item = PathBuf>,
        extra_writable: impl IntoIterator<Item = PathBuf>,
        extra_executable: impl IntoIterator<Item = PathBuf>,
        extra_readable_exact: impl IntoIterator<Item = PathBuf>,
    ) -> Result<ResolvedProcessAuthority, String> {
        self.resolve_inner(
            repository_root,
            available_root_slots,
            environment,
            Some(arguments),
            extra_readable,
            extra_writable,
            extra_executable,
            extra_readable_exact,
        )
    }

    fn resolve_inner(
        &self,
        repository_root: &Path,
        available_root_slots: &BTreeMap<String, PathBuf>,
        environment: &BTreeMap<String, OsString>,
        arguments: Option<&serde_json::Value>,
        extra_readable: impl IntoIterator<Item = PathBuf>,
        extra_writable: impl IntoIterator<Item = PathBuf>,
        extra_executable: impl IntoIterator<Item = PathBuf>,
        extra_readable_exact: impl IntoIterator<Item = PathBuf>,
    ) -> Result<ResolvedProcessAuthority, String> {
        self.validate()?;
        let repository_root = normalize_existing_directory(repository_root)?;
        let requested_slots = self
            .read_root_slots
            .iter()
            .chain(&self.write_root_slots)
            .chain(&self.executable_root_slots)
            .chain(
                self.executable_root_subpaths
                    .iter()
                    .map(|binding| &binding.root_slot),
            )
            .cloned()
            .collect::<BTreeSet<_>>();
        let candidate_slots = self
            .read_argument_paths
            .iter()
            .chain(&self.write_argument_paths)
            .chain(&self.executable_argument_paths)
            .flat_map(|binding| binding.within_root_slots.iter())
            .filter(|slot| !requested_slots.contains(*slot))
            .cloned()
            .collect::<BTreeSet<_>>();
        let optional_requested_slots = self
            .optional_read_root_slots
            .iter()
            .chain(&self.optional_write_root_slots)
            .chain(&self.optional_executable_root_slots)
            .cloned()
            .collect::<BTreeSet<_>>();
        let writable_slots = self
            .write_root_slots
            .iter()
            .chain(&self.optional_write_root_slots)
            .chain(
                self.write_argument_paths
                    .iter()
                    .flat_map(|binding| binding.within_root_slots.iter()),
            )
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut root_slots = BTreeMap::new();
        for slot in requested_slots {
            let path = available_root_slots
                .get(&slot)
                .ok_or_else(|| format!("process authority requests unbound root slot '{slot}'"))?;
            let path = if writable_slots.contains(&slot) {
                normalize_declared_exact_path(path.clone())?
            } else {
                normalize_existing_path(path.clone())?
            };
            root_slots.insert(slot, path);
        }
        for slot in candidate_slots {
            let Some(path) = available_root_slots.get(&slot) else {
                continue;
            };
            let path = if writable_slots.contains(&slot) {
                normalize_declared_exact_path(path.clone())
            } else {
                normalize_existing_path(path.clone())
            };
            if let Ok(path) = path {
                root_slots.insert(slot, path);
            }
        }
        for slot in optional_requested_slots {
            let Some(path) = available_root_slots.get(&slot) else {
                continue;
            };
            let path = if writable_slots.contains(&slot) {
                normalize_declared_exact_path(path.clone())?
            } else {
                normalize_existing_path(path.clone())?
            };
            root_slots.insert(slot, path);
        }

        let mut readable = paths_for_slots(&self.read_root_slots, &root_slots)?;
        let mut writable = paths_for_slots(&self.write_root_slots, &root_slots)?;
        let mut executable = paths_for_slots(&self.executable_root_slots, &root_slots)?;
        readable.extend(
            self.optional_read_root_slots
                .iter()
                .filter_map(|slot| root_slots.get(slot).cloned()),
        );
        writable.extend(
            self.optional_write_root_slots
                .iter()
                .filter_map(|slot| root_slots.get(slot).cloned()),
        );
        executable.extend(
            self.optional_executable_root_slots
                .iter()
                .filter_map(|slot| root_slots.get(slot).cloned()),
        );
        let mut readable_exact = BTreeSet::new();
        let mut writable_exact = BTreeSet::new();
        let mut executable_exact = BTreeSet::new();
        readable.extend(resolve_declared_paths(&self.read_paths, &repository_root)?);
        writable.extend(resolve_declared_paths(&self.write_paths, &repository_root)?);
        executable.extend(resolve_declared_paths(
            &self.executable_paths,
            &repository_root,
        )?);
        executable.extend(resolve_root_subpaths(
            "executable_root_subpaths",
            &self.executable_root_subpaths,
            &root_slots,
        )?);
        readable.extend(resolve_environment_paths(
            &self.read_env_paths,
            environment,
            false,
        )?);
        readable.extend(resolve_optional_environment_paths(
            &self.optional_read_env_paths,
            environment,
        )?);
        writable.extend(resolve_environment_paths(
            &self.write_env_paths,
            environment,
            false,
        )?);
        writable.extend(resolve_optional_environment_paths(
            &self.optional_write_env_paths,
            environment,
        )?);
        executable.extend(resolve_environment_paths(
            &self.executable_env_paths,
            environment,
            true,
        )?);
        readable.extend(resolve_environment_subpaths(
            "read_env_subpaths",
            &self.read_env_subpaths,
            environment,
        )?);
        writable.extend(resolve_environment_subpaths(
            "write_env_subpaths",
            &self.write_env_subpaths,
            environment,
        )?);
        executable.extend(resolve_environment_subpaths(
            "executable_env_subpaths",
            &self.executable_env_subpaths,
            environment,
        )?);
        let search_path = environment.get("PATH").map(OsString::as_os_str);
        let mut executable_links = BTreeSet::new();
        for command in &self.executable_commands {
            executable.insert(resolve_executable(command, &repository_root, search_path)?);
            executable_links.extend(search_path_links(command, search_path)?);
        }
        for binding in &self.read_executable_relative_paths {
            let command = resolve_executable(&binding.executable, &repository_root, search_path)?;
            let parent = command.parent().ok_or("resolved executable has no parent directory")?;
            readable.insert(normalize_existing_path(parent.join(&binding.relative_path))?);
        }
        let has_argument_paths = !self.read_argument_paths.is_empty()
            || !self.write_argument_paths.is_empty()
            || !self.executable_argument_paths.is_empty();
        if has_argument_paths {
            let arguments = arguments.ok_or_else(|| {
                "process authority declares invocation paths outside an invocation".to_string()
            })?;
            readable_exact.extend(resolve_argument_paths(
                "read_argument_paths",
                &self.read_argument_paths,
                arguments,
                &root_slots,
            )?);
            writable_exact.extend(resolve_argument_paths(
                "write_argument_paths",
                &self.write_argument_paths,
                arguments,
                &root_slots,
            )?);
            executable_exact.extend(resolve_argument_paths(
                "executable_argument_paths",
                &self.executable_argument_paths,
                arguments,
                &root_slots,
            )?);
        }
        readable.extend(extra_readable);
        writable.extend(extra_writable);
        executable.extend(extra_executable);
        readable_exact.extend(extra_readable_exact);
        let authority = ProcessAuthority::resolve(
            readable,
            writable,
            executable,
            readable_exact,
            writable_exact,
            executable_exact,
            self.allow_network,
            self.allow_local_ipc,
            self.allow_application_launch,
            self.allow_sandbox_delegation,
            self.allow_process_signals,
            self.sandbox_mode,
        )?
        .with_executable_links(executable_links);
        Ok(ResolvedProcessAuthority {
            authority,
            root_slots,
        })
    }
}

fn paths_for_slots(
    requested: &BTreeSet<String>,
    resolved: &BTreeMap<String, PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    requested
        .iter()
        .map(|slot| {
            resolved
                .get(slot)
                .cloned()
                .ok_or_else(|| format!("resolved process root slot '{slot}' disappeared"))
        })
        .collect()
}

fn resolve_declared_paths(
    declared: &BTreeSet<String>,
    repository_root: &Path,
) -> Result<BTreeSet<PathBuf>, String> {
    declared
        .iter()
        .map(|raw| {
            let path = Path::new(raw);
            if path.is_absolute() {
                Ok(path.to_path_buf())
            } else {
                Ok(repository_root.join(path))
            }
        })
        .collect()
}

fn resolve_root_subpaths(
    field: &str,
    declared: &BTreeSet<octopus_descriptors::ProcessRootSubpathSpec>,
    root_slots: &BTreeMap<String, PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    declared
        .iter()
        .map(|binding| {
            let root = root_slots.get(&binding.root_slot).ok_or_else(|| {
                format!(
                    "process authority {field} requests unbound root slot '{}'",
                    binding.root_slot
                )
            })?;
            normalize_existing_path(root.join(&binding.relative_path))
        })
        .collect()
}

fn resolve_environment_paths(
    names: &BTreeSet<String>,
    environment: &BTreeMap<String, OsString>,
    skip_missing: bool,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut paths = BTreeSet::new();
    for name in names {
        let value = environment.get(name).ok_or_else(|| {
            format!("process authority path environment variable '{name}' is unavailable")
        })?;
        let split = std::env::split_paths(value).collect::<Vec<_>>();
        if split.is_empty() {
            return Err(format!(
                "process authority path environment variable '{name}' is empty"
            ));
        }
        let mut resolved = 0usize;
        for path in split {
            if !path.is_absolute() {
                return Err(format!(
                    "process authority path environment variable '{name}' contains relative path '{}'",
                    path.display()
                ));
            }
            if skip_missing && !path.exists() {
                continue;
            }
            paths.insert(path);
            resolved += 1;
        }
        if skip_missing && resolved == 0 {
            return Err(format!(
                "process authority path environment variable '{name}' contains no existing paths"
            ));
        }
    }
    Ok(paths)
}

fn resolve_optional_environment_paths(
    names: &BTreeSet<String>,
    environment: &BTreeMap<String, OsString>,
) -> Result<BTreeSet<PathBuf>, String> {
    let present = names
        .iter()
        .filter(|name| environment.contains_key(*name))
        .cloned()
        .collect::<BTreeSet<_>>();
    resolve_environment_paths(&present, environment, false)
}

fn resolve_environment_subpaths(
    field: &str,
    declarations: &[ProcessEnvironmentSubpathSpec],
    environment: &BTreeMap<String, OsString>,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut paths = BTreeSet::new();
    for declaration in declarations {
        let value = environment.get(&declaration.env).ok_or_else(|| {
            format!(
                "process authority {field} environment variable '{}' is unavailable",
                declaration.env
            )
        })?;
        let roots = std::env::split_paths(value).collect::<Vec<_>>();
        if roots.is_empty() {
            return Err(format!(
                "process authority {field} environment variable '{}' is empty",
                declaration.env
            ));
        }
        for root in roots {
            let root = normalize_existing_path(root)?;
            let declared_candidate = root.join(&declaration.relative_path);
            let metadata = std::fs::symlink_metadata(&declared_candidate).map_err(|error| {
                format!(
                    "process authority {field} subpath '{}' cannot be inspected: {error}",
                    declared_candidate.display()
                )
            })?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "process authority {field} subpath '{}' is a symbolic link",
                    declared_candidate.display()
                ));
            }
            let candidate = normalize_existing_path(declared_candidate)?;
            if candidate == root || !candidate.starts_with(&root) {
                return Err(format!(
                    "process authority {field} subpath '{}' does not name a strict descendant of environment root '{}'",
                    candidate.display(),
                    root.display()
                ));
            }
            paths.insert(candidate);
        }
    }
    Ok(paths)
}

fn resolve_argument_paths(
    field: &str,
    declarations: &[ProcessArgumentPathSpec],
    arguments: &serde_json::Value,
    root_slots: &BTreeMap<String, PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    if declarations.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut resolved = BTreeSet::new();
    for declaration in declarations {
        let values = values_at_pointer_pattern(arguments, &declaration.pointer)?;
        for value in values {
            let raw = value.as_str().map(str::trim).filter(|value| !value.is_empty()).ok_or_else(
                || {
                    format!(
                        "process authority {field} pointer {:?} selected a non-string or empty value",
                        declaration.pointer
                    )
                },
            )?;
            let path = normalize_declared_exact_path(PathBuf::from(raw))?;
            let inside = declaration.within_root_slots.iter().any(|slot| {
                root_slots
                    .get(slot)
                    .is_some_and(|root| path.as_path() == root.as_path() || path.starts_with(root))
            });
            if !inside {
                return Err(format!(
                    "process authority {field} pointer {:?} selected path '{}' outside root slots [{}]",
                    declaration.pointer,
                    path.display(),
                    declaration
                        .within_root_slots
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if declaration.include_parent {
                let parent = path.parent().ok_or_else(|| {
                    format!(
                        "process authority {field} pointer {:?} selected path '{}' without a parent",
                        declaration.pointer,
                        path.display()
                    )
                })?;
                if !declaration.within_root_slots.iter().any(|slot| {
                    root_slots
                        .get(slot)
                        .is_some_and(|root| parent.starts_with(root))
                }) {
                    return Err(format!(
                        "process authority {field} pointer {:?} selected root path '{}' whose parent is outside the declared roots",
                        declaration.pointer,
                        path.display()
                    ));
                }
                resolved.insert(parent.to_path_buf());
            }
            resolved.insert(path);
        }
    }
    if resolved.is_empty() {
        return Err(format!(
            "process authority {field} selected no invocation paths"
        ));
    }
    Ok(resolved)
}

fn values_at_pointer_pattern<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
) -> Result<Vec<&'a serde_json::Value>, String> {
    let mut values = vec![value];
    for encoded in pointer.split('/').skip(1) {
        let segment = encoded.replace("~1", "/").replace("~0", "~");
        let mut next = Vec::new();
        for value in values {
            if segment == "*" {
                let array = value.as_array().ok_or_else(|| {
                    format!(
                        "process authority pointer {pointer:?} applies '*' to a non-array value"
                    )
                })?;
                next.extend(array.iter());
                continue;
            }
            match value {
                serde_json::Value::Object(object) => {
                    if let Some(value) = object.get(&segment) {
                        next.push(value);
                    }
                }
                serde_json::Value::Array(array) => {
                    let index = segment.parse::<usize>().map_err(|_| {
                        format!(
                            "process authority pointer {pointer:?} uses non-index segment {segment:?} on an array"
                        )
                    })?;
                    if let Some(value) = array.get(index) {
                        next.push(value);
                    }
                }
                _ => {}
            }
        }
        values = next;
        if values.is_empty() {
            break;
        }
    }
    Ok(values)
}

fn normalize_existing_path(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "process authority path '{}' is not absolute",
            path.display()
        ));
    }
    std::fs::canonicalize(&path).map_err(|error| {
        format!(
            "process authority path '{}' cannot be canonicalized: {error}",
            path.display()
        )
    })
}

fn normalize_declared_exact_path(path: PathBuf) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "process authority exact path '{}' is not absolute",
            path.display()
        ));
    }
    let mut existing = path.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            format!(
                "process authority exact path '{}' has no existing ancestor",
                path.display()
            )
        })?;
        suffix.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            format!(
                "process authority exact path '{}' has no existing ancestor",
                path.display()
            )
        })?;
    }
    let metadata = std::fs::symlink_metadata(existing).map_err(|error| {
        format!(
            "process authority exact path ancestor '{}' cannot be inspected: {error}",
            existing.display()
        )
    })?;
    if existing == path && metadata.file_type().is_symlink() {
        return Err(format!(
            "process authority exact path '{}' is a symbolic link",
            path.display()
        ));
    }
    let mut normalized = std::fs::canonicalize(existing).map_err(|error| {
        format!(
            "process authority exact path ancestor '{}' cannot be canonicalized: {error}",
            existing.display()
        )
    })?;
    for component in suffix.into_iter().rev() {
        normalized.push(component);
    }
    Ok(normalized)
}

pub fn resolve_process_exact_path(path: impl Into<PathBuf>) -> Result<PathBuf, String> {
    normalize_declared_exact_path(path.into())
}

fn normalize_existing_directory(path: &Path) -> Result<PathBuf, String> {
    let path = normalize_existing_path(path.to_path_buf())?;
    if !path.is_dir() {
        return Err(format!(
            "process working directory '{}' is not a directory",
            path.display()
        ));
    }
    Ok(path)
}

pub fn resolve_executable(
    declared: &str,
    cwd: &Path,
    search_path: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let declared = declared.trim();
    if declared.is_empty() {
        return Err("process executable is empty".to_string());
    }
    let path = Path::new(declared);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else if path.components().count() > 1 {
        if path
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(format!(
                "process executable '{}' contains a parent traversal",
                declared
            ));
        }
        cwd.join(path)
    } else {
        let search_path = search_path.map(OsString::from).ok_or_else(|| {
            format!("process executable '{declared}' requires declared PATH resolution")
        })?;
        std::env::split_paths(&search_path)
            .map(|root| root.join(path))
            .find(|candidate| executable_file(candidate))
            .ok_or_else(|| format!("process executable '{declared}' is absent from PATH"))?
    };
    let executable = std::fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "process executable '{}' cannot be canonicalized: {error}",
            candidate.display()
        )
    })?;
    if !executable_file(&executable) {
        return Err(format!(
            "process executable '{}' is not an executable regular file",
            executable.display()
        ));
    }
    Ok(executable)
}

pub fn resolve_required_repository_file(
    declared: &str,
    repository_root: &Path,
) -> Result<PathBuf, String> {
    let declared = declared.trim();
    if declared.is_empty() {
        return Err("required process file is empty".to_string());
    }
    let relative = Path::new(declared);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "required process file '{}' is not a normalized repository-relative path",
            declared
        ));
    }
    let repository_root = normalize_existing_directory(repository_root)?;
    let candidate = repository_root.join(relative);
    let metadata = std::fs::symlink_metadata(&candidate).map_err(|error| {
        format!(
            "required process file '{}' cannot be inspected: {error}",
            candidate.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "required process file '{}' is not a regular non-symlink file",
            candidate.display()
        ));
    }
    let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "required process file '{}' cannot be canonicalized: {error}",
            candidate.display()
        )
    })?;
    if !resolved.starts_with(&repository_root) {
        return Err(format!(
            "required process file '{}' resolves outside repository root '{}'",
            resolved.display(),
            repository_root.display()
        ));
    }
    Ok(resolved)
}

fn search_path_links(
    declared: &str,
    search_path: Option<&OsStr>,
) -> Result<BTreeSet<PathBuf>, String> {
    let declared = declared.trim();
    let path = Path::new(declared);
    let mut links = BTreeSet::new();
    if path.is_absolute() || path.components().count() != 1 {
        return Ok(links);
    }
    let Some(mut current) = search_path.and_then(|search_path| {
        std::env::split_paths(search_path)
            .map(|root| root.join(path))
            .find(|candidate| executable_file(candidate))
    }) else {
        return Ok(links);
    };
    for _ in 0..MAX_SEARCH_PATH_LINKS {
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            format!(
                "process executable link '{}' cannot be inspected: {error}",
                current.display()
            )
        })?;
        if !metadata.file_type().is_symlink() {
            return Ok(links);
        }
        let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
            return Err(format!(
                "process executable link '{}' has no parent directory",
                current.display()
            ));
        };
        let parent = std::fs::canonicalize(parent).map_err(|error| {
            format!(
                "process executable link directory '{}' cannot be canonicalized: {error}",
                parent.display()
            )
        })?;
        if links.is_empty() && current.is_absolute() {
            links.insert(current.clone());
        }
        links.insert(parent.join(name));
        let target = std::fs::read_link(&current).map_err(|error| {
            format!(
                "process executable link '{}' cannot be read: {error}",
                current.display()
            )
        })?;
        current = parent.join(target);
    }
    Err(format!(
        "process executable '{declared}' resolves through more than {MAX_SEARCH_PATH_LINKS} symbolic links"
    ))
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(target_os = "macos")]
fn prepare_macos(
    authority: &ProcessAuthority,
    executable: PathBuf,
    args: &[OsString],
    cwd: &Path,
    profile_root: &Path,
) -> Result<PreparedProcess, String> {
    let sandbox = PathBuf::from("/usr/bin/sandbox-exec");
    if !executable_file(&sandbox) {
        return Err("/usr/bin/sandbox-exec is unavailable".to_string());
    }
    let mut readable = authority.readable.clone();
    readable.insert(executable.clone());
    readable.insert(sandbox.clone());
    add_macos_lexical_aliases(&mut readable);
    let runtime_roots = macos_runtime_roots();
    let mut closure_seeds = authority.executable.clone();
    closure_seeds.extend(authority.executable_exact.iter().cloned());
    closure_seeds.insert(executable.clone());
    closure_seeds.insert(sandbox.clone());
    let process_closure = macos_process_closure(closure_seeds, &runtime_roots)?;
    readable.extend(process_closure.executables.iter().cloned());
    readable.extend(runtime_roots.iter().cloned());
    readable.extend(process_closure.bundle_roots.iter().cloned());
    let mut readable_exact = authority.readable_exact.clone();
    readable_exact.extend(process_closure.selector_paths.iter().cloned());
    readable_exact.extend(process_closure.runtime_library_paths.iter().cloned());
    let mut environment = BTreeMap::new();
    if authority.allow_network {
        let trust_store = PathBuf::from("/private/etc/ssl/cert.pem");
        if !trust_store.is_file() {
            return Err(format!(
                "macOS network process trust store '{}' is unavailable",
                trust_store.display()
            ));
        }
        readable_exact.insert(trust_store.clone());
        environment.insert(
            OsString::from("SSL_CERT_FILE"),
            trust_store.into_os_string(),
        );
    }
    add_macos_lexical_aliases(&mut readable_exact);
    let mut writable = authority.writable.clone();
    add_macos_lexical_aliases(&mut writable);
    let mut writable_exact = authority.writable_exact.clone();
    add_macos_lexical_aliases(&mut writable_exact);
    let mut writable_ancestor_sources = writable.clone();
    writable_ancestor_sources.extend(writable_exact.iter().cloned());
    let writable_ancestors = path_ancestors(&writable_ancestor_sources);
    let publication = macos_file_publication_access(authority)?;
    let mut paths = BTreeSet::new();
    paths.extend(readable.iter().cloned());
    paths.extend(writable.iter().cloned());
    paths.extend(readable_exact.iter().cloned());
    paths.extend(writable_exact.iter().cloned());
    paths.insert(cwd.to_path_buf());
    let ancestors = path_ancestors(&paths);
    let mut directory_reads = ancestors.clone();
    if authority.allow_sandbox_delegation {
        for path in &paths {
            directory_reads.extend(octopus_file_storage::publication::publication_ancestor_directories(path)
                .map_err(|error| format!("publication ancestry for '{}' is unavailable: {error}", path.display()))?);
        }
    }
    let mut policy = String::from(
        "(version 1)\n(deny default)\n(allow process-fork)\n(allow signal (target same-sandbox))\n(allow process-info* (target same-sandbox))\n(allow sysctl-read)\n(allow sysctl-write (sysctl-name \"kern.grade_cputype\"))\n(allow iokit-open (iokit-registry-entry-class \"RootDomainUserClient\"))\n(allow ipc-posix-sem)\n(allow ipc-posix-shm-read-data ipc-posix-shm-write-create ipc-posix-shm-write-unlink)\n(allow pseudo-tty)\n(allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\") (global-name \"com.apple.system.opendirectoryd.membership\") (global-name \"com.apple.PowerManagement.control\") (global-name \"com.apple.cfprefsd.daemon\") (global-name \"com.apple.cfprefsd.agent\") (local-name \"com.apple.cfprefsd.agent\"))\n(allow file-read* file-test-existence (subpath \"/private/etc\") (subpath \"/etc\") (subpath \"/private/var/db/timezone\") (subpath \"/usr/share/icu\") (literal \"/dev/random\") (literal \"/dev/urandom\") (literal \"/dev/null\") (literal \"/dev/zero\"))\n(allow file-read* file-write* file-test-existence (subpath \"/dev/fd\") (literal \"/dev/null\") (literal \"/dev/zero\"))\n(allow file-read* file-write* file-ioctl (literal \"/dev/ptmx\") (regex #\"^/dev/ttys[0-9]+\"))\n(allow file-read* file-test-existence (literal \"/\"))\n",
    );
    if authority.allow_process_signals {
        policy.push_str("(allow signal)\n");
    }
    append_macos_access(
        &mut policy,
        "file-read* file-test-existence",
        readable,
    )?;
    append_macos_access(
        &mut policy,
        "file-write*",
        writable,
    )?;
    append_macos_exact_access(
        &mut policy,
        "file-read* file-test-existence",
        readable_exact,
    )?;
    append_macos_exact_access(
        &mut policy,
        "file-write*",
        writable_exact,
    )?;
    append_macos_typed_exact_access(
        &mut policy,
        "file-read-data",
        directory_reads,
        Some("DIRECTORY"),
    )?;
    append_macos_typed_exact_access(
        &mut policy,
        "file-read* file-test-existence",
        publication.directory_reads,
        Some("DIRECTORY"),
    )?;
    append_macos_typed_exact_access(
        &mut policy,
        "file-write-create",
        publication.directory_creates,
        Some("DIRECTORY"),
    )?;
    for (operations, files) in [
        ("file-read* file-test-existence", publication.read_files),
        ("file-read* file-test-existence file-write*", publication.write_files),
    ] {
        for pattern in macos_publication_patterns(&files)? {
            policy.push_str(&format!("(allow {operations} (require-all (vnode-type REGULAR-FILE) (regex {pattern})))\n"));
        }
    }
    append_macos_typed_exact_access(
        &mut policy,
        "file-write-create",
        writable_ancestors,
        Some("DIRECTORY"),
    )?;
    append_macos_access(
        &mut policy,
        "file-read-metadata file-test-existence",
        ancestors,
    )?;
    let mut executable_links = authority.executable_links.clone();
    add_macos_lexical_aliases(&mut executable_links);
    let mut link_metadata = path_ancestors(&executable_links);
    link_metadata.extend(executable_links);
    append_macos_exact_access(
        &mut policy,
        "file-read-metadata file-test-existence",
        link_metadata,
    )?;
    let mut executable_paths = authority.executable.clone();
    executable_paths.insert(executable.clone());
    executable_paths.extend(process_closure.executables.iter().cloned());
    executable_paths.extend(process_closure.bundle_roots.iter().cloned());
    executable_paths.remove(&sandbox);
    add_macos_lexical_aliases(&mut executable_paths);
    let mut executable_exact = authority.executable_exact.clone();
    add_macos_lexical_aliases(&mut executable_exact);
    append_macos_access(
        &mut policy,
        "process-exec",
        executable_paths.clone(),
    )?;
    append_macos_exact_access(
        &mut policy,
        "process-exec",
        executable_exact.clone(),
    )?;
    if authority.allow_sandbox_delegation {
        append_macos_exact_access(
            &mut policy,
            "process-exec (with no-sandbox)",
            [sandbox.clone()].into_iter().collect(),
        )?;
    }
    let mut mapped_paths = executable_paths;
    mapped_paths.insert(sandbox.clone());
    mapped_paths.extend(runtime_roots);
    mapped_paths.extend(process_closure.bundle_roots);
    append_macos_access(
        &mut policy,
        "file-map-executable",
        mapped_paths,
    )?;
    append_macos_exact_access(
        &mut policy,
        "file-map-executable",
        executable_exact,
    )?;
    append_macos_exact_access(
        &mut policy,
        "file-map-executable",
        process_closure.runtime_library_paths,
    )?;
    if authority.allow_network {
        policy.push_str(
            "(allow network*)\n(allow system-socket)\n(allow file-read* file-test-existence (literal \"/private/var\") (literal \"/var\"))\n(allow mach-lookup (global-name \"com.apple.SecurityServer\") (global-name \"com.apple.networkd\") (global-name \"com.apple.ocspd\") (global-name \"com.apple.trustd\") (global-name \"com.apple.trustd.agent\") (global-name \"com.apple.SystemConfiguration.DNSConfiguration\") (global-name \"com.apple.SystemConfiguration.configd\"))\n",
        );
    } else if authority.allow_local_ipc {
        policy.push_str(
            "(allow system-socket (socket-domain AF_UNIX))\n(allow network-bind (local unix-socket))\n(allow network-outbound (remote unix-socket))\n",
        );
    }
    if authority.allow_application_launch {
        policy.push_str(
            "(allow lsopen)\n(allow mach-lookup (global-name \"com.apple.CoreServices.coreservicesd\") (global-name \"com.apple.coreservices.launchservicesd\") (global-name \"com.apple.lsd.mapdb\") (global-name \"com.apple.lsd.modifydb\") (global-name \"com.apple.coreservices.quarantine-resolver\"))\n",
        );
    }
    let profile = write_macos_profile(profile_root, &policy)?;
    let mut sandbox_args = vec![
        OsString::from("-f"),
        profile.path.clone().into_os_string(),
        OsString::from("--"),
        executable.as_os_str().to_os_string(),
    ];
    sandbox_args.extend(args.iter().cloned());
    Ok(PreparedProcess {
        program: sandbox,
        args: sandbox_args,
        executable,
        environment,
        sandbox_profile: Some(std::sync::Arc::new(profile)),
    })
}

#[cfg(target_os = "macos")]
const MACOS_PROFILE_STRING_LIMIT: usize = 1023;

#[cfg(target_os = "macos")]
fn macos_publication_patterns(paths: &BTreeSet<Vec<MacosRegexAtom>>) -> Result<Vec<String>, String> {
    let paths = paths.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let pattern = |alternatives: &[&[MacosRegexAtom]]| {
        let mut regex = String::from("^");
        macos_regex_union(alternatives, &mut regex);
        regex.push('$');
        macos_profile_string(OsStr::new(&regex))
    };
    let mut patterns = Vec::new();
    let mut start = 0;
    while start < paths.len() {
        let remaining = paths.len() - start;
        let mut fitting = 1;
        let mut failing = None;
        while failing.is_none() && fitting < remaining {
            let trial = (fitting * 2).min(remaining);
            match pattern(&paths[start..start + trial]) {
                Ok(_) => fitting = trial,
                Err(_) => failing = Some(trial),
            }
        }
        if let Some(mut failing) = failing {
            while failing - fitting > 1 {
                let middle = (fitting + failing) / 2;
                match pattern(&paths[start..start + middle]) {
                    Ok(_) => fitting = middle,
                    Err(_) => failing = middle,
                }
            }
        }
        patterns.push(pattern(&paths[start..start + fitting])?);
        start += fitting;
    }
    Ok(patterns)
}

#[cfg(target_os = "macos")]
fn macos_regex_union(alternatives: &[&[MacosRegexAtom]], regex: &mut String) {
    let (Some(first), Some(last)) = (alternatives.first(), alternatives.last()) else {
        return;
    };
    let shared = first.iter().zip(last.iter()).take_while(|(left, right)| left == right).count();
    first[..shared].iter().for_each(|atom| atom.push_to(regex));
    let tails = alternatives.iter().map(|alternative| &alternative[shared..]).collect::<Vec<_>>();
    let optional = tails[0].is_empty();
    let branches = tails[usize::from(optional)..].chunk_by(|left, right| left[0] == right[0]).collect::<Vec<_>>();
    if branches.is_empty() {
        return;
    }
    let grouped = optional || branches.len() > 1;
    if grouped {
        regex.push('(');
    }
    for (index, branch) in branches.into_iter().enumerate() {
        if index > 0 {
            regex.push('|');
        }
        macos_regex_union(branch, regex);
    }
    if grouped {
        regex.push(')');
    }
    if optional {
        regex.push('?');
    }
}

#[cfg(target_os = "macos")]
fn macos_profile_string(value: &OsStr) -> Result<String, String> {
    let text = value.to_str().ok_or_else(|| format!("macOS sandbox path {value:?} is not UTF-8"))?;
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            character if character.is_control() => {
                return Err(format!("macOS sandbox path {text:?} contains a control character"));
            }
            character => quoted.push(character),
        }
    }
    if quoted.len() - 1 > MACOS_PROFILE_STRING_LIMIT {
        return Err(format!("macOS sandbox profile string for {text:?} exceeds the profile reader limit of {MACOS_PROFILE_STRING_LIMIT} bytes"));
    }
    quoted.push('"');
    Ok(quoted)
}

#[cfg(target_os = "macos")]
fn write_macos_profile(profile_root: &Path, policy: &str) -> Result<SandboxProfileFile, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let root = normalize_existing_directory(profile_root)?;
    let mut guard = macos_regex_atoms(root.as_os_str())?;
    guard.extend(macos_regex_atoms(OsStr::new("/.sandbox-profile-"))?);
    guard.push(MacosRegexAtom::AnyName);
    guard.extend(macos_regex_atoms(OsStr::new(".sb"))?);
    let mut guard_regex = String::from("^");
    guard.into_iter().for_each(|atom| atom.push_to(&mut guard_regex));
    guard_regex.push('$');
    let policy = format!("{policy}(deny file-write* (regex {}))\n", macos_profile_string(OsStr::new(&guard_regex))?);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("sandbox profile clock is before the Unix epoch: {error}"))?
        .as_nanos();
    let path = root.join(format!(
        ".sandbox-profile-{}-{nonce}-{}.sb",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("sandbox profile '{}' cannot be created: {error}", path.display()))?;
    let profile = SandboxProfileFile { path };
    file.write_all(policy.as_bytes())
        .map_err(|error| format!("sandbox profile '{}' cannot be written: {error}", profile.path.display()))?;
    Ok(profile)
}

#[cfg(target_os = "macos")]
fn add_macos_lexical_aliases(paths: &mut BTreeSet<PathBuf>) {
    let existing = paths.iter().cloned().collect::<Vec<_>>();
    for alias in ["/bin", "/etc", "/sbin", "/tmp", "/var"] {
        let alias = Path::new(alias);
        let Ok(target) = std::fs::canonicalize(alias) else {
            continue;
        };
        for path in &existing {
            let Ok(suffix) = path.strip_prefix(&target) else {
                continue;
            };
            paths.insert(alias.join(suffix));
        }
    }
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct MacosFilePublicationAccess {
    directory_reads: BTreeSet<PathBuf>,
    directory_creates: BTreeSet<PathBuf>,
    read_files: BTreeSet<Vec<MacosRegexAtom>>,
    write_files: BTreeSet<Vec<MacosRegexAtom>>,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MacosRegexAtom {
    Character(char),
    AnyName,
}

#[cfg(target_os = "macos")]
impl MacosRegexAtom {
    fn push_to(self, regex: &mut String) {
        match self {
            Self::Character(character) => {
                if "\\.^$|?*+()[]{}".contains(character) {
                    regex.push('\\');
                }
                regex.push(character);
            }
            Self::AnyName => regex.push_str("[^/]*"),
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_file_publication_access(authority: &ProcessAuthority) -> Result<MacosFilePublicationAccess, String> {
    use octopus_file_storage::publication::{COORDINATION_DIRECTORY, PublicationFileSelector, file_publication_access};
    let mut targets = authority.writable_exact.clone();
    targets.extend(authority.writable.iter().filter(|path| path.is_file()).cloned());
    let mut access = MacosFilePublicationAccess::default();
    for target in targets {
        match std::fs::symlink_metadata(&target) {
            Ok(metadata) if !metadata.is_file() => continue,
            Ok(_) => (),
            Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => (),
            Err(error) => return Err(format!("file publication authority target cannot be inspected: {error}")),
        }
        let parent = target.parent().ok_or_else(|| "file publication authority target has no parent".to_string())?;
        let local = parent.join(COORDINATION_DIRECTORY);
        let ancestors = parent.ancestors().map(Path::to_path_buf).collect::<BTreeSet<_>>();
        let metadata = ancestors.iter().map(|ancestor| ancestor.join(COORDINATION_DIRECTORY)).collect::<BTreeSet<_>>();
        let history = metadata.iter().map(|root| root.join("history")).collect::<BTreeSet<_>>();
        let projection = file_publication_access(&target).map_err(|error| format!("file publication authority for '{}' failed: {error}", target.display()))?;
        for path in projection.directory_reads {
            validate_macos_publication_path(&path, true)?;
            if !ancestors.contains(&path) && !metadata.contains(&path) && !history.contains(&path) {
                return Err("file publication projection reads an unrelated directory".into());
            }
            access.directory_reads.insert(path);
        }
        for path in projection.directory_creates {
            validate_macos_publication_path(&path, true)?;
            if path != local { return Err("file publication projection creates an unrelated directory".into()); }
            access.directory_reads.insert(path.clone());
            access.directory_creates.insert(path);
        }
        for (writable, selectors) in [(false, projection.file_reads), (true, projection.file_writes)] {
            for selector in selectors {
                match selector {
                    PublicationFileSelector::Exact(path) => {
                        validate_macos_publication_path(&path, false)?;
                        let directory = path.parent().ok_or_else(|| "file publication selector has no parent".to_string())?;
                        if if writable { directory != local.as_path() } else { !metadata.contains(directory) && !history.contains(directory) } {
                            return Err("file publication projection selects unrelated metadata".into());
                        }
                        let name = path.file_name().ok_or_else(|| "file publication selector has no file name".to_string())?;
                        insert_macos_publication_files(&mut access, directory, macos_regex_atoms(name)?, writable)?;
                    }
                    PublicationFileSelector::Names { directory, prefix, suffix } => {
                        validate_macos_publication_path(&directory, true)?;
                        if directory != local || [&prefix, &suffix].into_iter().any(|part| part.is_empty() || part.as_encoded_bytes().contains(&b'/')) {
                            return Err("file publication projection has an unbounded filename selector".into());
                        }
                        let mut names = macos_regex_atoms(&prefix)?;
                        names.push(MacosRegexAtom::AnyName);
                        names.extend(macos_regex_atoms(&suffix)?);
                        insert_macos_publication_files(&mut access, &directory, names, writable)?;
                    }
                }
            }
        }
    }
    let write_files = &access.write_files;
    access.read_files.retain(|path| !write_files.contains(path));
    Ok(access)
}

#[cfg(target_os = "macos")]
fn insert_macos_publication_files(
    access: &mut MacosFilePublicationAccess,
    directory: &Path,
    names: Vec<MacosRegexAtom>,
    writable: bool,
) -> Result<(), String> {
    let mut path = macos_regex_atoms(directory.as_os_str())?;
    path.push(MacosRegexAtom::Character('/'));
    path.extend(names);
    let files = if writable { &mut access.write_files } else { &mut access.read_files };
    files.insert(path);
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_macos_publication_path(path: &Path, directory: bool) -> Result<(), String> {
    if !path.is_absolute() || path.components().any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        || normalize_declared_exact_path(path.to_path_buf())? != path {
        return Err("file publication projection requires original normal absolute paths".into());
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if if directory { metadata.is_dir() } else { metadata.is_file() } => Ok(()),
        Ok(_) => Err("file publication projection has a different filesystem type".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("file publication projection cannot be inspected: {error}")),
    }
}

#[cfg(target_os = "macos")]
fn macos_regex_atoms(value: &OsStr) -> Result<Vec<MacosRegexAtom>, String> {
    let value = value.to_str().ok_or_else(|| "file publication filename projection requires UTF-8".to_string())?;
    value.chars().map(|character| if character.is_control() {
        Err("file publication filename projection contains control text".to_string())
    } else {
        Ok(MacosRegexAtom::Character(character))
    }).collect()
}

#[cfg(target_os = "macos")]
struct MacosProcessClosure {
    executables: BTreeSet<PathBuf>,
    selector_paths: BTreeSet<PathBuf>,
    bundle_roots: BTreeSet<PathBuf>,
    runtime_library_paths: BTreeSet<PathBuf>,
}

#[cfg(target_os = "macos")]
fn macos_process_closure(
    seeds: BTreeSet<PathBuf>,
    runtime_roots: &BTreeSet<PathBuf>,
) -> Result<MacosProcessClosure, String> {
    let mut pending = seeds;
    let mut executables = BTreeSet::new();
    let mut selector_paths = BTreeSet::new();
    let mut runtime_library_paths = BTreeSet::new();
    while let Some(path) = pending.pop_first() {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.is_file() {
            continue;
        }
        let executable = std::fs::canonicalize(&path).map_err(|error| {
            format!(
                "process executable '{}' cannot be canonicalized while forming its runtime closure: {error}",
                path.display()
            )
        })?;
        if !executables.insert(executable.clone()) {
            continue;
        }
        if let Some(interpreter) = macos_script_interpreter(&executable)? {
            pending.insert(interpreter);
        }
        if let Some((selectors, selected, libraries)) = macos_selected_executable(&executable)? {
            selector_paths.extend(selectors);
            pending.insert(selected);
            runtime_library_paths.extend(libraries);
        }
    }
    let mut bundle_roots = BTreeSet::new();
    for library in runtime_library_paths.clone() {
        runtime_library_paths.extend(macos_runtime_library_paths(
            &library,
            &BTreeSet::new(),
            runtime_roots,
        )?);
    }
    for executable in &executables {
        let executable_roots = macos_executable_bundle_roots(executable);
        runtime_library_paths.extend(macos_runtime_library_paths(
            executable,
            &executable_roots,
            runtime_roots,
        )?);
        bundle_roots.extend(executable_roots);
    }
    Ok(MacosProcessClosure {
        executables,
        selector_paths,
        bundle_roots,
        runtime_library_paths,
    })
}

#[cfg(target_os = "macos")]
fn macos_script_interpreter(executable: &Path) -> Result<Option<PathBuf>, String> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;

    let mut file = std::fs::File::open(executable).map_err(|error| {
        format!(
            "process executable '{}' cannot be read while forming its runtime closure: {error}",
            executable.display()
        )
    })?;
    let mut prefix = [0_u8; 4096];
    let length = file.read(&mut prefix).map_err(|error| {
        format!(
            "process executable '{}' cannot be inspected while forming its runtime closure: {error}",
            executable.display()
        )
    })?;
    let prefix = &prefix[..length];
    if !prefix.starts_with(b"#!") {
        return Ok(None);
    }
    let line = &prefix[2..prefix
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(prefix.len())];
    let interpreter = line
        .split(|byte| byte.is_ascii_whitespace())
        .find(|part| !part.is_empty())
        .ok_or_else(|| {
            format!(
                "process script '{}' has an empty interpreter declaration",
                executable.display()
            )
        })?;
    let interpreter = PathBuf::from(OsStr::from_bytes(interpreter));
    if !interpreter.is_absolute() {
        return Err(format!(
            "process script '{}' declares non-absolute interpreter '{}'",
            executable.display(),
            interpreter.display()
        ));
    }
    let interpreter = std::fs::canonicalize(&interpreter).map_err(|error| {
        format!(
            "process script '{}' interpreter '{}' cannot be canonicalized: {error}",
            executable.display(),
            interpreter.display()
        )
    })?;
    if !executable_file(&interpreter) {
        return Err(format!(
            "process script '{}' interpreter '{}' is not executable",
            executable.display(),
            interpreter.display()
        ));
    }
    Ok(Some(interpreter))
}

#[cfg(target_os = "macos")]
fn macos_selected_executable(
    executable: &Path,
) -> Result<Option<(BTreeSet<PathBuf>, PathBuf, BTreeSet<PathBuf>)>, String> {
    let Some(parent) = executable.parent() else {
        return Ok(None);
    };
    if parent != Path::new("/bin") && parent != Path::new("/usr/bin") {
        return Ok(None);
    }
    let Some(name) = executable.file_name() else {
        return Ok(None);
    };
    let bytes = std::fs::read(executable).map_err(|error| {
        format!(
            "process executable '{}' cannot be inspected: {error}",
            executable.display()
        )
    })?;
    if macos_imported_libraries(&bytes).0.contains(b"/usr/lib/libxcselect.dylib".as_slice()) {
        let selector = PathBuf::from("/private/var/select/developer_dir");
        let metadata = std::fs::symlink_metadata(&selector).map_err(|error| {
            format!(
                "developer executable selector '{}' cannot be inspected: {error}",
                selector.display()
            )
        })?;
        if !metadata.file_type().is_symlink() {
            return Err(format!(
                "developer executable selector '{}' is not a symbolic link",
                selector.display()
            ));
        }
        let developer_dir = std::fs::canonicalize(&selector).map_err(|error| {
            format!(
                "developer executable selector '{}' cannot be resolved: {error}",
                selector.display()
            )
        })?;
        if !developer_dir.is_dir() {
            return Err(format!(
                "developer executable selector '{}' does not select a directory",
                selector.display()
            ));
        }
        let selectors = BTreeSet::from([selector, developer_dir.clone()]);
        let library = developer_dir.join("usr/lib/libxcrun.dylib");
        let canonical_library = normalize_existing_path(library.clone())?;
        if !canonical_library.starts_with(&developer_dir) || !canonical_library.is_file() {
            return Err(format!(
                "developer selector library '{}' is outside selected developer directory '{}' or is not a file",
                canonical_library.display(), developer_dir.display()
            ));
        }
        let libraries = BTreeSet::from([library, canonical_library]);
        let resolver = PathBuf::from("/usr/bin/xcrun");
        if executable == resolver {
            return Ok(Some((selectors, executable.to_path_buf(), libraries)));
        }
        let output = std::process::Command::new(&resolver)
            .args([OsStr::new("--no-cache"), OsStr::new("--find"), name])
            .env_clear()
            .output()
            .map_err(|error| {
                format!(
                    "developer executable '{}' cannot be selected: {error}",
                    executable.display()
                )
            })?;
        if !output.status.success() {
            return Err(format!(
                "developer executable '{}' selection failed: {}",
                executable.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let selected = std::str::from_utf8(&output.stdout)
            .map_err(|error| format!("developer executable selection is not UTF-8: {error}"))?
            .trim();
        let selected = normalize_existing_path(PathBuf::from(selected))?;
        if !selected.starts_with(&developer_dir) || !executable_file(&selected) {
            return Err(format!("developer executable '{}' is not an executable inside selected developer directory '{}'", selected.display(), developer_dir.display()));
        }
        return Ok(Some((selectors, selected, libraries)));
    }
    let selector = PathBuf::from("/private/var/select").join(name);
    let metadata = match std::fs::symlink_metadata(&selector) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "process executable selector '{}' cannot be inspected: {error}",
                selector.display()
            ));
        }
    };
    if !metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let selected = std::fs::canonicalize(&selector).map_err(|error| {
        format!(
            "process executable selector '{}' cannot be canonicalized: {error}",
            selector.display()
        )
    })?;
    if !executable_file(&selected) {
        return Ok(None);
    }
    Ok(Some((
        BTreeSet::from([selector]),
        selected,
        BTreeSet::new(),
    )))
}

#[cfg(target_os = "macos")]
fn macos_runtime_roots() -> BTreeSet<PathBuf> {
    [
        "/System",
        "/usr/lib",
        "/Library/Apple",
        "/Library/Frameworks",
        "/Library/Developer/CommandLineTools/Library/Frameworks",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|path| path.exists())
    .filter_map(|path| std::fs::canonicalize(path).ok())
    .collect()
}

#[cfg(target_os = "macos")]
fn macos_executable_bundle_roots(executable: &Path) -> BTreeSet<PathBuf> {
    let mut roots = BTreeSet::new();
    for ancestor in executable.ancestors().skip(1) {
        let Some(name) = ancestor.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        if name.ends_with(".app") {
            roots.insert(ancestor.to_path_buf());
        }
        let Some(versions) = ancestor.parent() else {
            continue;
        };
        let Some(framework) = versions.parent() else {
            continue;
        };
        if versions.file_name() == Some(OsStr::new("Versions"))
            && framework
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.ends_with(".framework"))
        {
            roots.insert(ancestor.to_path_buf());
        }
    }
    roots
}

#[cfg(target_os = "macos")]
fn macos_runtime_library_paths(
    executable: &Path,
    bundle_roots: &BTreeSet<PathBuf>,
    runtime_roots: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    let executable_bytes = std::fs::read(executable).map_err(|error| error.to_string())?;
    let (_, executable_rpaths) = macos_imported_libraries(&executable_bytes);
    let inherited_rpaths = executable_rpaths.iter()
        .filter_map(|path| resolve_macos_import(path, executable, executable)).collect::<Vec<_>>();
    let mut candidates = BTreeSet::from([executable.to_path_buf()]);
    for root in bundle_roots {
        collect_macos_runtime_candidates(root, &mut candidates)?;
    }
    let mut pending = candidates.into_iter().map(|path| (path, inherited_rpaths.clone())).collect::<BTreeMap<_, _>>();
    let mut inspected = BTreeSet::new();
    let mut libraries = BTreeSet::new();
    while let Some((path, inherited_rpaths)) = pending.pop_first() {
        let resolved = match std::fs::canonicalize(&path) {
            Ok(path) => path,
            Err(_) => continue,
        };
        if !inspected.insert(resolved.clone()) {
            continue;
        }
        let bytes = match std::fs::read(&resolved) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let (imports, declared_rpaths) = macos_imported_libraries(&bytes);
        let mut rpaths = declared_rpaths.iter()
            .filter_map(|path| resolve_macos_import(path, &resolved, executable)).collect::<Vec<_>>();
        for inherited in inherited_rpaths {
            if !rpaths.contains(&inherited) { rpaths.push(inherited); }
        }
        for import in imports {
            let selected = if let Some(relative) = import.strip_prefix(b"@rpath/") {
                use std::os::unix::ffi::OsStrExt;
                rpaths.iter().map(|root| root.join(OsStr::from_bytes(relative)))
                    .find(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()))
            } else {
                resolve_macos_import(&import, &resolved, executable)
            };
            let Some(import) = selected else {
                continue;
            };
            if bundle_roots.iter().any(|root| import.starts_with(root))
                || runtime_roots.iter().any(|root| import.starts_with(root))
            {
                continue;
            }
            let canonical = match std::fs::canonicalize(&import) {
                Ok(path) => path,
                Err(_) => continue,
            };
            if !std::fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_file()) {
                continue;
            }
            libraries.insert(import);
            libraries.insert(canonical.clone());
            pending.entry(canonical).or_insert_with(|| rpaths.clone());
        }
    }
    Ok(libraries)
}

#[cfg(target_os = "macos")]
fn collect_macos_runtime_candidates(
    root: &Path,
    candidates: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    let entries = std::fs::read_dir(root).map_err(|error| {
        format!(
            "process executable bundle '{}' cannot be scanned: {error}",
            root.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "process executable bundle '{}' contains an unreadable entry: {error}",
                root.display()
            )
        })?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "process executable bundle entry '{}' cannot be inspected: {error}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_macos_runtime_candidates(&path, candidates)?;
            continue;
        }
        if metadata.is_file() && macos_runtime_candidate(&path, &metadata) {
            candidates.insert(path);
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_runtime_candidate(path: &Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
        || path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| matches!(extension, "dylib" | "so" | "bundle"))
}

#[cfg(target_os = "macos")]
fn macos_imported_libraries(bytes: &[u8]) -> (BTreeSet<Vec<u8>>, Vec<Vec<u8>>) {
    use object::read::macho::{FatArch, MachOFatFile32, MachOFatFile64, LoadCommandVariant};

    fn thin(bytes: &[u8], imports: &mut BTreeSet<Vec<u8>>, rpaths: &mut Vec<Vec<u8>>) {
        let Ok(file) = object::File::parse(bytes) else {
            return;
        };
        macro_rules! collect {
            ($file:expr) => {{
                let file = $file;
                let endian = file.endian();
                let Ok(mut commands) = file.macho_load_commands() else {
                    return;
                };
                loop {
                    let command = match commands.next() {
                        Ok(Some(command)) => command,
                        Ok(None) | Err(_) => break,
                    };
                    if let Ok(LoadCommandVariant::Rpath(rpath)) = command.variant() {
                        if let Ok(path) = command.string(endian, rpath.path) {
                            let path = path.to_vec();
                            if !rpaths.contains(&path) { rpaths.push(path); }
                        }
                    }
                    let Ok(Some(dylib)) = command.dylib() else {
                        continue;
                    };
                    if let Ok(name) = command.string(endian, dylib.dylib.name) {
                        imports.insert(name.to_vec());
                    }
                }
            }};
        }
        match file {
            object::File::MachO32(file) => collect!(file),
            object::File::MachO64(file) => collect!(file),
            _ => {}
        }
    }

    let mut imports = BTreeSet::new();
    let mut rpaths = Vec::new();
    match object::FileKind::parse(bytes) {
        Ok(object::FileKind::MachO32 | object::FileKind::MachO64) => {
            thin(bytes, &mut imports, &mut rpaths);
        }
        Ok(object::FileKind::MachOFat32) => {
            if let Ok(file) = MachOFatFile32::parse(bytes) {
                for arch in file.arches() {
                    if let Ok(bytes) = arch.data(bytes) {
                        thin(bytes, &mut imports, &mut rpaths);
                    }
                }
            }
        }
        Ok(object::FileKind::MachOFat64) => {
            if let Ok(file) = MachOFatFile64::parse(bytes) {
                for arch in file.arches() {
                    if let Ok(bytes) = arch.data(bytes) {
                        thin(bytes, &mut imports, &mut rpaths);
                    }
                }
            }
        }
        _ => {}
    }
    (imports, rpaths)
}

#[cfg(target_os = "macos")]
fn resolve_macos_import(import: &[u8], loader: &Path, executable: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let import = OsStr::from_bytes(import);
    let path = Path::new(import);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    let loader_prefix = OsStr::from_bytes(b"@loader_path/");
    if let Some(relative) = import.as_bytes().strip_prefix(loader_prefix.as_bytes()) {
        return loader
            .parent()
            .map(|root| root.join(OsStr::from_bytes(relative)));
    }
    let executable_prefix = OsStr::from_bytes(b"@executable_path/");
    if let Some(relative) = import.as_bytes().strip_prefix(executable_prefix.as_bytes()) {
        return executable
            .parent()
            .map(|root| root.join(OsStr::from_bytes(relative)));
    }
    None
}

#[cfg(target_os = "macos")]
fn append_macos_access(policy: &mut String, operations: &str, paths: BTreeSet<PathBuf>) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    policy.push_str(&format!("(allow {operations}\n"));
    for path in paths {
        let path = macos_profile_string(path.as_os_str())?;
        policy.push_str(&format!("(literal {path}) (subpath {path})\n"));
    }
    policy.push_str(")\n");
    Ok(())
}

#[cfg(target_os = "macos")]
fn append_macos_exact_access(policy: &mut String, operations: &str, paths: BTreeSet<PathBuf>) -> Result<(), String> {
    append_macos_typed_exact_access(policy, operations, paths, None)
}

#[cfg(target_os = "macos")]
fn append_macos_typed_exact_access(
    policy: &mut String,
    operations: &str,
    paths: BTreeSet<PathBuf>,
    vnode: Option<&str>,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    policy.push_str(&format!("(allow {operations}\n"));
    for path in paths {
        let path = macos_profile_string(path.as_os_str())?;
        match vnode {
            Some(vnode) => policy.push_str(&format!("(require-all (literal {path}) (vnode-type {vnode}))\n")),
            None => policy.push_str(&format!("(literal {path})\n")),
        }
    }
    policy.push_str(")\n");
    Ok(())
}

#[cfg(target_os = "macos")]
fn path_ancestors(paths: &BTreeSet<PathBuf>) -> BTreeSet<PathBuf> {
    let mut ancestors = BTreeSet::new();
    for path in paths {
        let mut current = path.parent();
        while let Some(parent) = current {
            if parent == Path::new("/") {
                break;
            }
            ancestors.insert(parent.to_path_buf());
            current = parent.parent();
        }
    }
    ancestors
}

#[cfg(test)]
mod future_write_authority_tests {
    use super::*;

    #[test]
    fn writable_subtree_may_be_formed_before_materialization() {
        let root = tempfile::tempdir().expect("temporary root");
        let future = root.path().join("state").join("future-root");
        let authority = ProcessAuthority::resolve(
            std::iter::empty(),
            [future.clone()],
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            false,
            false,
            false,
            false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        )
        .expect("future writable subtree");
        let expected = std::fs::canonicalize(root.path())
            .expect("canonical temporary root")
            .join("state")
            .join("future-root");
        assert!(!future.exists());
        assert!(authority.writable.contains(&expected));
        assert!(authority.readable.contains(&expected));
    }

    #[test]
    fn read_only_subtree_must_already_exist() {
        let root = tempfile::tempdir().expect("temporary root");
        let future = root.path().join("state").join("missing-read-root");
        assert!(ProcessAuthority::resolve(
            [future],
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            false,
            false,
            false,
            false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        )
        .is_err());
    }

    #[test]
    fn writable_root_slot_keeps_its_future_canonical_identity() {
        let root = tempfile::tempdir().expect("temporary root");
        let future = root.path().join("state").join("future-root");
        let spec = ProcessAuthoritySpec {
            write_root_slots: ["future_root".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let resolved = spec
            .resolve(
                root.path(),
                &[("future_root".to_string(), future.clone())]
                    .into_iter()
                    .collect(),
                &BTreeMap::new(),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
            )
            .expect("future root-slot authority");
        let expected = std::fs::canonicalize(root.path())
            .expect("canonical temporary root")
            .join("state")
            .join("future-root");
        assert_eq!(resolved.root_slots["future_root"], expected);
        assert!(!future.exists());
    }

    #[cfg(unix)]
    #[test]
    fn existing_symlink_cannot_become_a_future_write_root() {
        let root = tempfile::tempdir().expect("temporary root");
        let outside = tempfile::tempdir().expect("outside root");
        let link = root.path().join("declared-root");
        std::os::unix::fs::symlink(outside.path(), &link).expect("symlink");
        assert!(ProcessAuthority::resolve(
            std::iter::empty(),
            [link],
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            false,
            false,
            false,
            false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        )
        .is_err());
    }
}

#[cfg(test)]
mod executable_environment_path_tests {
    use super::*;

    #[test]
    fn executable_path_list_ignores_nonexistent_search_roots() {
        let root = tempfile::tempdir().expect("temporary root");
        let existing = root.path().join("bin");
        let missing = root.path().join("missing-bin");
        std::fs::create_dir(&existing).expect("existing PATH root");
        let path = std::env::join_paths([missing, existing.clone()]).expect("PATH value");
        let spec = ProcessAuthoritySpec {
            executable_env_paths: ["PATH".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let resolved = spec
            .resolve(
                root.path(),
                &BTreeMap::new(),
                &BTreeMap::from([("PATH".to_string(), path)]),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
            )
            .expect("PATH authority");
        assert_eq!(
            resolved.authority.executable,
            [std::fs::canonicalize(existing).expect("canonical PATH root")]
                .into_iter()
                .collect()
        );
    }
}

#[cfg(test)]
mod executable_root_subpath_tests {
    use super::*;

    #[test]
    fn executable_root_subpath_grants_only_the_declared_file() {
        let root = tempfile::tempdir().expect("temporary root");
        let kinds = root.path().join("worker_environment_kind");
        let executable = kinds.join("filesystem_workspace/packages/bin/workspace-tool");
        std::fs::create_dir_all(executable.parent().unwrap()).expect("owner package directory");
        std::fs::write(&executable, b"tool").expect("owner executable");
        let spec = ProcessAuthoritySpec {
            executable_root_subpaths: BTreeSet::from([
                octopus_descriptors::ProcessRootSubpathSpec {
                    root_slot: "registry_worker_environment_kind_root".to_string(),
                    relative_path: "filesystem_workspace/packages/bin/workspace-tool".to_string(),
                },
            ]),
            ..Default::default()
        };

        let resolved = spec
            .resolve(
                root.path(),
                &BTreeMap::from([(
                    "registry_worker_environment_kind_root".to_string(),
                    kinds.clone(),
                )]),
                &BTreeMap::new(),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
            )
            .expect("exact kind-owned executable authority");

        assert_eq!(
            resolved.authority.executable,
            BTreeSet::from([std::fs::canonicalize(executable).unwrap()])
        );
        assert!(!resolved
            .authority
            .executable
            .contains(&std::fs::canonicalize(kinds).expect("kind registry root")));
    }
}

#[cfg(test)]
mod optional_environment_path_tests {
    use super::*;

    #[test]
    fn absent_optional_environment_paths_grant_nothing() {
        let root = tempfile::tempdir().expect("temporary root");
        let spec = ProcessAuthoritySpec {
            optional_read_env_paths: BTreeSet::from(["OPTIONAL_READ_ROOT".to_string()]),
            optional_write_env_paths: BTreeSet::from(["OPTIONAL_WRITE_ROOT".to_string()]),
            ..Default::default()
        };
        let resolved = spec
            .resolve(
                root.path(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
            )
            .expect("absent optional roots");
        assert!(resolved.authority.readable.is_empty());
        assert!(resolved.authority.writable.is_empty());
    }

    #[test]
    fn present_optional_environment_paths_are_validated_and_granted() {
        let root = tempfile::tempdir().expect("temporary root");
        let readable = root.path().join("readable");
        let writable = root.path().join("writable");
        std::fs::create_dir(&readable).expect("readable root");
        let spec = ProcessAuthoritySpec {
            optional_read_env_paths: BTreeSet::from(["OPTIONAL_READ_ROOT".to_string()]),
            optional_write_env_paths: BTreeSet::from(["OPTIONAL_WRITE_ROOT".to_string()]),
            ..Default::default()
        };
        let resolved = spec
            .resolve(
                root.path(),
                &BTreeMap::new(),
                &BTreeMap::from([
                    (
                        "OPTIONAL_READ_ROOT".to_string(),
                        readable.as_os_str().to_os_string(),
                    ),
                    (
                        "OPTIONAL_WRITE_ROOT".to_string(),
                        writable.as_os_str().to_os_string(),
                    ),
                ]),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
            )
            .expect("present optional roots");
        assert!(resolved
            .authority
            .readable
            .contains(&std::fs::canonicalize(&readable).unwrap()));
        assert!(resolved
            .authority
            .writable
            .contains(&normalize_declared_exact_path(writable.clone()).unwrap()));
        assert!(resolved
            .authority
            .readable
            .contains(&normalize_declared_exact_path(writable).unwrap()));
    }

    #[test]
    fn relative_or_empty_optional_environment_paths_fail_closed() {
        let root = tempfile::tempdir().expect("temporary root");
        let spec = ProcessAuthoritySpec {
            optional_read_env_paths: BTreeSet::from(["OPTIONAL_READ_ROOT".to_string()]),
            ..Default::default()
        };
        for value in [OsString::from("relative"), OsString::new()] {
            let error = spec
                .resolve(
                    root.path(),
                    &BTreeMap::new(),
                    &BTreeMap::from([("OPTIONAL_READ_ROOT".to_string(), value)]),
                    std::iter::empty(),
                    std::iter::empty(),
                    std::iter::empty(),
                    std::iter::empty(),
                )
                .unwrap_err();
            assert!(error.contains("OPTIONAL_READ_ROOT"));
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_network_process_tests {
    use super::*;

    #[test]
    fn system_locale_data_is_readable_while_undeclared_files_stay_private() {
        let root = tempfile::tempdir().expect("temporary root");
        let private = root.path().join("private.txt");
        std::fs::write(&private, "private").unwrap();
        let locale = std::fs::read_dir("/usr/share/icu").unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "dat")).expect("system ICU data");
        let authority = ProcessAuthority::resolve(
            [], [], [], [], [], [], false, false, false, false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        ).unwrap();
        for (file, allowed) in [(locale, true), (private, false)] {
            let prepared = authority.prepare("/usr/bin/head", &["-c".into(), "1".into(), file.into_os_string()], root.path(), None, root.path()).unwrap();
            let output = std::process::Command::new(prepared.program).args(prepared.args)
                .env_clear().envs(prepared.environment).current_dir(root.path()).output().unwrap();
            assert_eq!(output.status.success(), allowed, "{}", String::from_utf8_lossy(&output.stderr));
        }
    }

    #[test]
    fn future_write_root_can_materialize_its_own_subtree() {
        let root = tempfile::tempdir().expect("temporary root");
        let future = root.path().join("state").join("future-root");
        let executable = PathBuf::from("/bin/mkdir");
        let authority = ProcessAuthority::resolve(
            std::iter::empty(),
            [future.clone()],
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            [executable.clone()],
            false,
            false,
            false,
            false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        )
        .expect("future write authority");
        let target = future.join("nested");
        let args = vec![OsString::from("-p"), target.clone().into_os_string()];
        let prepared = authority
            .prepare(
                executable.to_str().expect("executable path"),
                &args,
                root.path(),
                None,
                root.path(),
            )
            .expect("future-root process preparation");
        let status = std::process::Command::new(&prepared.program)
            .args(&prepared.args)
            .env_clear()
            .envs(&prepared.environment)
            .current_dir(root.path())
            .status()
            .expect("future-root process execution");
        assert!(status.success());
        assert!(target.is_dir());

        let sibling = root.path().join("outside");
        let denied_args = vec![OsString::from("-p"), sibling.clone().into_os_string()];
        let denied = authority
            .prepare(
                executable.to_str().expect("executable path"),
                &denied_args,
                root.path(),
                None,
                root.path(),
            )
            .expect("sibling process preparation");
        let denied_status = std::process::Command::new(&denied.program)
            .args(&denied.args)
            .env_clear()
            .envs(&denied.environment)
            .current_dir(root.path())
            .status()
            .expect("sibling process execution");
        assert!(!denied_status.success());
        assert!(!sibling.exists());
    }

    #[test]
    fn large_exact_authority_reaches_the_sandbox_through_a_private_profile_file() {
        let root = tempfile::tempdir().expect("temporary root");
        let base = root.path().canonicalize().expect("canonical root");
        let profiles = base.join("profiles");
        let unusual = base.join("quote\"back\\slash é");
        std::fs::create_dir_all(&profiles).expect("profile root");
        std::fs::create_dir_all(&unusual).expect("unusual directory");
        let granted = unusual.join("granted");
        let mut writable = (0..400).map(|index| base.join(format!("registry/interface/run_{index}/interface.json"))).collect::<Vec<_>>();
        writable.push(granted.clone());
        let executable = PathBuf::from("/usr/bin/touch");
        let authority = ProcessAuthority::resolve(
            std::iter::empty(), std::iter::empty(), std::iter::empty(), std::iter::empty(),
            writable, [executable.clone()], false, false, false, false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        ).expect("large exact authority");
        let run = |target: &Path| {
            let prepared = authority
                .prepare(executable.to_str().expect("executable path"), &[target.as_os_str().to_os_string()], &base, None, &profiles)
                .expect("large authority preparation");
            assert_eq!(prepared.args.len(), 5);
            std::process::Command::new(&prepared.program).args(&prepared.args).env_clear()
                .envs(&prepared.environment).current_dir(&base).status().expect("sandboxed process").success()
        };
        assert!(run(&granted));
        assert!(granted.is_file());
        let denied = unusual.join("denied");
        assert!(!run(&denied));
        assert!(!denied.exists());
        assert_eq!(std::fs::read_dir(&profiles).expect("profile root").count(), 0);
        let broad = ProcessAuthority::resolve(
            std::iter::empty(), [unusual.clone()], std::iter::empty(), std::iter::empty(),
            std::iter::empty(), [executable.clone()], false, false, false, false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        ).expect("broad authority");
        let touch = |target: &Path| {
            let prepared = broad
                .prepare(executable.to_str().expect("executable path"), &[target.as_os_str().to_os_string()], &base, None, &unusual)
                .expect("broad authority preparation");
            std::process::Command::new(&prepared.program).args(&prepared.args).env_clear()
                .envs(&prepared.environment).current_dir(&base).status().expect("sandboxed process").success()
        };
        assert!(touch(&unusual.join("ordinary")));
        let pending = unusual.join(".sandbox-profile-1-2-3.sb");
        assert!(!touch(&pending));
        assert!(!pending.exists());
    }

    #[test]
    fn network_preparation_materializes_the_platform_trust_contract() {
        let cwd = std::env::current_dir().expect("current directory");
        let profiles = tempfile::tempdir().expect("profile root");
        let executable = PathBuf::from("/usr/bin/true");
        let network = ProcessAuthority::resolve(
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            [executable.clone()],
            true,
            false,
            false,
            false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        )
        .expect("network authority");
        let prepared = network
            .prepare(
                executable.to_str().expect("executable path"),
                &[],
                &cwd,
                None,
                profiles.path(),
            )
            .expect("network process preparation");
        assert_eq!(
            prepared.environment.get(OsStr::new("SSL_CERT_FILE")),
            Some(&OsString::from("/private/etc/ssl/cert.pem"))
        );
        assert!(std::fs::read_to_string(&prepared.args[1]).expect("network profile")
            .contains("(literal \"/private/etc/ssl/cert.pem\")"));

        let offline = ProcessAuthority::resolve(
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            std::iter::empty(),
            [executable.clone()],
            false,
            false,
            false,
            false,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        )
        .expect("offline authority")
        .prepare(
            executable.to_str().expect("executable path"),
            &[],
            &cwd,
            None,
            profiles.path(),
        )
        .expect("offline process preparation");
        assert!(!offline
            .environment
            .contains_key(OsStr::new("SSL_CERT_FILE")));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod native_path_tests {
    use super::*;

    #[test]
    fn macos_system_firmlink_aliases_preserve_authorized_paths() {
        let private_etc = std::fs::canonicalize("/etc").expect("/etc");
        let private_tmp = std::fs::canonicalize("/tmp").expect("/tmp");
        let private_var = std::fs::canonicalize("/var").expect("/var");
        let mut paths = BTreeSet::from([
            private_etc.join("hosts"),
            private_tmp.join("octopus"),
            private_var.join("tmp"),
        ]);
        add_macos_lexical_aliases(&mut paths);
        assert!(paths.contains(&PathBuf::from("/etc/hosts")));
        assert!(paths.contains(&PathBuf::from("/tmp/octopus")));
        assert!(paths.contains(&PathBuf::from("/var/tmp")));
    }

    #[test]
    fn macos_projects_declared_process_capabilities() {
        let authority = ProcessAuthority::resolve(
            [], [], [], [], [], [], false, false, true, true,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        ).expect("process authority");
        let profiles = tempfile::tempdir().expect("profile root");
        let prepared = authority
            .prepare("/usr/bin/true", &[], Path::new("/tmp"), None, profiles.path())
            .expect("prepared process");
        let profile = std::fs::read_to_string(&prepared.args[1]).expect("prepared profile");
        assert!(profile.contains("process-exec (with no-sandbox)\n(literal \"/usr/bin/sandbox-exec\")"));
        assert!(profile.contains("lsopen"));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod search_path_link_tests {
    use super::*;

    #[test]
    fn search_path_symlink_runs_by_name_while_its_neighbours_stay_hidden() {
        let root = tempfile::tempdir().expect("temporary root");
        let base = root.path().canonicalize().expect("canonical root");
        let links = base.join("links");
        let profiles = base.join("profiles");
        for directory in [&links, &profiles] {
            std::fs::create_dir_all(directory).expect("test directory");
        }
        for name in ["octopus-linked-probe", "octopus-neighbour"] {
            std::os::unix::fs::symlink("/usr/bin/true", links.join(name)).expect("command link");
        }
        let spec = ProcessAuthoritySpec {
            executable_commands: BTreeSet::from(["octopus-linked-probe".to_string()]),
            ..ProcessAuthoritySpec::default()
        };
        let environment = BTreeMap::from([("PATH".to_string(), links.clone().into_os_string())]);
        let resolved = spec
            .resolve(
                &base,
                &BTreeMap::new(),
                &environment,
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
                std::iter::empty(),
            )
            .expect("search path authority");
        assert_eq!(
            resolved.authority.executable_links,
            BTreeSet::from([links.join("octopus-linked-probe")])
        );
        let run = |command: &str| {
            let prepared = resolved
                .authority
                .prepare("/usr/bin/env", &[OsString::from(command)], Path::new("/"), None, &profiles)
                .expect("sandboxed env");
            std::process::Command::new(&prepared.program)
                .args(&prepared.args)
                .env_clear()
                .envs(&prepared.environment)
                .env("PATH", &links)
                .current_dir("/")
                .status()
                .expect("sandboxed env execution")
                .success()
        };
        assert!(run("octopus-linked-probe"));
        assert!(!run("octopus-neighbour"));
    }
}
