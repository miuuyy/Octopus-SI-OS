use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::ffi::OsStrExt;

pub const CONTROL_SOCKET: &str = "/run/octopus-process-authority/control.sock";
pub const PLATFORM_EXECUTABLE: &str = "/usr/libexec/octopus/octopus-linux-process-authority";
pub const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxProcessPolicy {
    pub files: Vec<FileRule>,
    pub read_only: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quiet_deny_write_exact: Vec<PathBuf>,
    pub allow_network: bool,
    pub allow_local_ipc: bool,
    pub allow_application_launch: bool,
    pub allow_sandbox_delegation: bool,
    pub allow_process_signals: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxRoomPolicy {
    pub readable: Vec<PathBuf>,
    pub writable: Vec<PathBuf>,
    pub read_only: Vec<PathBuf>,
    pub allow_network: bool,
}

pub fn prepare_room(
    room: LinuxRoomPolicy,
    argv: &[OsString],
    cwd: &Path,
    search_path: Option<&OsStr>,
) -> Result<PreparedProcess, String> {
    let (program, args) = argv.split_first().ok_or("room command is empty")?;
    let program = program
        .to_str()
        .ok_or("room executable path must be UTF-8")?;
    let cwd = normalize_existing_directory(cwd)?;
    let mut roots = room
        .readable
        .into_iter()
        .chain(room.writable.iter().cloned())
        .map(normalize_declared_exact_path)
        .collect::<Result<BTreeSet<_>, _>>()?;
    if !roots
        .iter()
        .any(|root| cwd == *root || root.is_dir() && cwd.starts_with(root))
    {
        return Err("room working directory is outside its declared roots".into());
    }
    for path in ["/usr", "/bin", "/sbin", "/lib", "/lib64"] {
        match std::fs::canonicalize(path) {
            Ok(path) => {
                roots.insert(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(format!("room runtime path '{path}': {error}")),
        }
    }
    let executable = resolve_executable(program, &cwd, search_path)?;
    let mut closure = BTreeSet::new();
    let mut pending = BTreeSet::from([executable.clone()]);
    while let Some(path) = pending.pop_first() {
        if !roots
            .iter()
            .any(|root| &path == root || root.is_dir() && path.starts_with(root))
        {
            return Err(format!(
                "room executable '{}' is outside its declared roots",
                path.display()
            ));
        }
        if closure.insert(path.clone()) {
            if let Some(interpreter) = executable_interpreter(&path)? {
                pending.insert(interpreter);
            }
        }
    }
    let authority = ProcessAuthority::resolve(
        roots.clone(),
        room.writable,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        closure,
        room.allow_network,
        false,
        false,
        false,
        false,
        octopus_descriptors::ProcessSandboxMode::Enforced,
    )?;
    let mut policy = project(&authority, &executable)?;
    policy.read_only = room
        .read_only
        .into_iter()
        .map(normalize_declared_exact_path)
        .collect::<Result<Vec<_>, _>>()?;
    for path in &policy.read_only {
        if let Some(parent) = path.parent() {
            if authority
                .writable
                .iter()
                .any(|root| parent.starts_with(root))
            {
                policy.files.push(FileRule {
                    selector: FileSelector::Exact {
                        path: parent.to_path_buf(),
                        directory: true,
                    },
                    access: FileAccess::CreateDirectory,
                });
            }
        }
    }
    policy.files.retain(|rule| {
        !matches!(
            rule.access,
            FileAccess::CreateDirectory | FileAccess::CreatePrivateDirectory
        ) || match &rule.selector {
            FileSelector::Exact { path, .. } => {
                !policy.read_only.iter().any(|root| path.starts_with(root))
            }
            _ => false,
        }
    });
    for root in roots {
        policy.add_path(root, FileAccess::Execute, false, false)?;
    }
    policy.compact();
    prepared(policy, executable, args)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRule {
    pub selector: FileSelector,
    pub access: FileAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileSelector {
    Exact {
        path: PathBuf,
        directory: bool,
    },
    Tree {
        path: PathBuf,
    },
    Names {
        directory: PathBuf,
        prefix: OsString,
        suffix: OsString,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAccess {
    Read,
    Write,
    Execute,
    Map,
    CreateDirectory,
    CreatePrivateDirectory,
}

pub(super) fn prepare(
    authority: &ProcessAuthority,
    executable: PathBuf,
    args: &[OsString],
    _cwd: &Path,
) -> Result<PreparedProcess, String> {
    let policy = project(authority, &executable)?;
    prepared(policy, executable, args)
}

fn project(authority: &ProcessAuthority, executable: &Path) -> Result<LinuxProcessPolicy, String> {
    let mut policy = project_uncompacted(authority, executable)?;
    policy.compact();
    Ok(policy)
}

fn project_uncompacted(
    authority: &ProcessAuthority,
    executable: &Path,
) -> Result<LinuxProcessPolicy, String> {
    let mut policy = LinuxProcessPolicy {
        files: Vec::new(),
        read_only: Vec::new(),
        quiet_deny_write_exact: Vec::new(),
        allow_network: authority.allow_network,
        allow_local_ipc: authority.allow_local_ipc,
        allow_application_launch: authority.allow_application_launch,
        allow_sandbox_delegation: authority.allow_sandbox_delegation,
        allow_process_signals: authority.allow_process_signals,
    };
    let exact_directories: BTreeSet<PathBuf> = authority
        .writable_exact_parents
        .iter()
        .cloned()
        .chain(path_ancestors(&authority.writable_exact))
        .chain(path_ancestors(&authority.readable_exact))
        .collect();
    for (paths, access, exact) in [
        (&authority.readable, FileAccess::Read, false),
        (&authority.writable, FileAccess::Write, false),
        (&authority.executable, FileAccess::Execute, false),
        (&authority.readable_exact, FileAccess::Read, true),
        (&authority.writable_exact, FileAccess::Write, true),
        (&authority.executable_exact, FileAccess::Execute, true),
    ] {
        for path in paths {
            policy.add_path(
                path.clone(),
                access,
                exact,
                exact && exact_directories.contains(path),
            )?;
        }
    }
    let mut pending = authority.executable.clone();
    pending.extend(authority.executable_exact.iter().cloned());
    pending.insert(executable.to_path_buf());
    for path in runtime_executables(pending)? {
        policy.add_path(path, FileAccess::Execute, true, false)?;
    }
    for path in ["/lib", "/lib64", "/usr/lib", "/usr/lib64"] {
        policy.add_system_path(path, FileAccess::Map)?;
    }
    for path in [
        "/usr/share/locale",
        "/usr/share/zoneinfo",
        "/etc/ld.so.cache",
        "/etc/localtime",
        "/etc/passwd",
        "/etc/group",
        "/etc/nsswitch.conf",
        "/dev/random",
        "/dev/urandom",
    ] {
        policy.add_system_path(path, FileAccess::Read)?;
    }
    for path in [
        "/dev/null",
        "/dev/zero",
        "/dev/tty",
        "/dev/ptmx",
        "/dev/pts",
    ] {
        policy.add_system_path(path, FileAccess::Write)?;
    }
    if authority.allow_network {
        for path in [
            "/etc/ssl/certs",
            "/usr/share/ca-certificates",
            "/etc/hosts",
            "/etc/resolv.conf",
            "/etc/gai.conf",
            "/etc/services",
            "/etc/protocols",
        ] {
            policy.add_system_path(path, FileAccess::Read)?;
        }
    }
    let mut paths = authority.readable.clone();
    paths.extend(authority.writable.iter().cloned());
    paths.extend(authority.readable_exact.iter().cloned());
    paths.extend(authority.writable_exact.iter().cloned());
    for path in path_ancestors(&paths) {
        policy.files.push(FileRule {
            selector: FileSelector::Exact {
                path,
                directory: true,
            },
            access: FileAccess::Read,
        });
    }
    let mut writable = authority.writable.clone();
    writable.extend(authority.writable_exact.iter().cloned());
    for path in path_ancestors(&writable) {
        policy.files.push(FileRule {
            selector: FileSelector::Exact {
                path,
                directory: true,
            },
            access: FileAccess::CreateDirectory,
        });
    }
    let mut publication_targets: BTreeSet<PathBuf> = authority
        .writable_exact
        .iter()
        .filter(|path| {
            !authority.writable_exact_parents.contains(*path)
                && (path.is_dir() || !exact_directories.contains(*path))
        })
        .cloned()
        .collect();
    publication_targets.extend(
        authority
            .writable
            .iter()
            .filter(|path| path.is_file())
            .cloned(),
    );
    for path in publication_targets {
        policy.add_publication(&path)?;
    }
    for path in &authority.quiet_deny_write_exact {
        if !policy.path_is_writable(path) {
            policy.quiet_deny_write_exact.push(path.clone());
        }
    }
    if authority.allow_sandbox_delegation {
        for path in &paths {
            let ancestry = octopus_file_storage::publication::publication_ancestor_access(path)
                .map_err(|error| {
                    format!(
                        "publication ancestry for '{}' is unavailable: {error}",
                        path.display()
                    )
                })?;
            policy.add_publication_rules(
                ancestry.directory_reads,
                Vec::new(),
                ancestry.file_reads,
                Vec::new(),
            );
        }
    }
    Ok(policy)
}

const ACCESS_READ: u8 = 1 << 0;
const ACCESS_WRITE: u8 = 1 << 1;
const ACCESS_EXECUTE: u8 = 1 << 2;
const ACCESS_MAP: u8 = 1 << 3;

fn access_bit(access: FileAccess) -> u8 {
    match access {
        FileAccess::Read => ACCESS_READ,
        FileAccess::Write => ACCESS_WRITE,
        FileAccess::Execute => ACCESS_EXECUTE,
        FileAccess::Map => ACCESS_MAP,
        FileAccess::CreateDirectory | FileAccess::CreatePrivateDirectory => 0,
    }
}

fn tree_mask_subsumes(mask: u8, access: FileAccess) -> bool {
    match access {
        FileAccess::Read => (mask & (ACCESS_READ | ACCESS_WRITE)) != 0,
        FileAccess::Write => (mask & ACCESS_WRITE) != 0,
        FileAccess::Execute => (mask & ACCESS_EXECUTE) != 0,
        FileAccess::Map => (mask & (ACCESS_MAP | ACCESS_EXECUTE)) != 0,
        FileAccess::CreateDirectory | FileAccess::CreatePrivateDirectory => false,
    }
}

fn prepared(
    policy: LinuxProcessPolicy,
    executable: PathBuf,
    args: &[OsString],
) -> Result<PreparedProcess, String> {
    let lease = crate::linux_process::LinuxLease::prepare(&policy)?;
    Ok(PreparedProcess {
        program: executable.clone(),
        args: args.to_vec(),
        executable,
        environment: BTreeMap::new(),
        sandbox_profile: None,
        linux_lease: Some(std::sync::Arc::new(lease)),
    })
}

impl LinuxProcessPolicy {
    pub fn encode_prepare_frame(&self) -> Result<Vec<u8>, String> {
        let payload = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if payload.len() as u64 > MAX_REQUEST_BYTES {
            return Err(format!(
                "Linux process policy exceeds its transport limit ({} bytes > {} bytes)",
                payload.len(),
                MAX_REQUEST_BYTES
            ));
        }
        let mut frame = Vec::with_capacity(8 + payload.len() + 1);
        frame.extend_from_slice(b"PREPARE ");
        frame.extend_from_slice(&payload);
        frame.push(b'\n');
        Ok(frame)
    }

    pub fn compact(&mut self) {
        self.read_only.sort();
        self.read_only.dedup();
        self.quiet_deny_write_exact.sort();
        self.quiet_deny_write_exact.dedup();

        let unique: BTreeSet<FileRule> = self.files.drain(..).collect();
        let mut tree_masks: BTreeMap<&Path, u8> = BTreeMap::new();
        let mut selector_masks: BTreeMap<&FileSelector, u8> = BTreeMap::new();
        for rule in &unique {
            let bit = access_bit(rule.access);
            if bit == 0 {
                continue;
            }
            *selector_masks.entry(&rule.selector).or_insert(0) |= bit;
            if let FileSelector::Tree { path } = &rule.selector {
                if literal_path(path).is_ok() {
                    *tree_masks.entry(path.as_path()).or_insert(0) |= bit;
                }
            }
        }

        let mut compacted = Vec::with_capacity(unique.len());
        for rule in &unique {
            match rule.access {
                FileAccess::CreateDirectory => {
                    let private_same_dir = FileRule {
                        selector: rule.selector.clone(),
                        access: FileAccess::CreatePrivateDirectory,
                    };
                    if !unique.contains(&private_same_dir) {
                        compacted.push(rule.clone());
                    }
                    continue;
                }
                FileAccess::CreatePrivateDirectory => {
                    compacted.push(rule.clone());
                    continue;
                }
                _ => {}
            }

            let same_mask = selector_masks.get(&rule.selector).copied().unwrap_or(0);
            let subsumed_on_same_selector = match rule.access {
                FileAccess::Read => (same_mask & ACCESS_WRITE) != 0,
                FileAccess::Map => (same_mask & ACCESS_EXECUTE) != 0,
                _ => false,
            };
            if subsumed_on_same_selector {
                continue;
            }

            let subsumed_by_tree = match &rule.selector {
                FileSelector::Tree { path } => {
                    literal_path(path).is_ok()
                        && path.ancestors().skip(1).any(|ancestor| {
                            tree_masks
                                .get(ancestor)
                                .is_some_and(|&mask| tree_mask_subsumes(mask, rule.access))
                        })
                }
                FileSelector::Exact { path, .. } => {
                    literal_path(path).is_ok()
                        && path.ancestors().any(|ancestor| {
                            tree_masks
                                .get(ancestor)
                                .is_some_and(|&mask| tree_mask_subsumes(mask, rule.access))
                        })
                }
                FileSelector::Names { directory, .. } => {
                    rule.selector.patterns().is_ok()
                        && directory.ancestors().any(|ancestor| {
                            tree_masks
                                .get(ancestor)
                                .is_some_and(|&mask| tree_mask_subsumes(mask, rule.access))
                        })
                }
            };
            if !subsumed_by_tree {
                compacted.push(rule.clone());
            }
        }
        self.files = compacted;
    }

    fn add_path(
        &mut self,
        path: PathBuf,
        access: FileAccess,
        exact: bool,
        known_directory: bool,
    ) -> Result<(), String> {
        let directory = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.is_dir(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => known_directory || !exact,
            Err(error) => {
                return Err(format!(
                    "Linux authority cannot inspect '{}': {error}",
                    path.display()
                ))
            }
        };
        let selector = if exact || !directory {
            FileSelector::Exact { path, directory }
        } else {
            FileSelector::Tree { path }
        };
        self.files.push(FileRule { selector, access });
        Ok(())
    }

    fn add_system_path(&mut self, path: &str, access: FileAccess) -> Result<(), String> {
        match std::fs::canonicalize(path) {
            Ok(path) => self.add_path(path, access, false, false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "Linux runtime resource '{path}' cannot be resolved: {error}"
            )),
        }
    }

    fn add_publication(&mut self, path: &Path) -> Result<(), String> {
        if path.is_dir() {
            let projection = octopus_file_storage::publication::directory_publication_access(path)
                .map_err(|error| {
                    format!(
                        "directory publication authority for '{}' failed: {error}",
                        path.display()
                    )
                })?;
            for tree in projection.directory_tree_reads {
                self.files.push(FileRule {
                    selector: FileSelector::Tree { path: tree },
                    access: FileAccess::Read,
                });
            }
            self.add_publication_rules(
                projection.directory_reads,
                projection.directory_creates,
                projection.file_reads,
                projection.file_writes,
            );
            return Ok(());
        }
        let projection =
            octopus_file_storage::publication::file_publication_access(path).map_err(|error| {
                format!(
                    "publication authority for '{}' failed: {error}",
                    path.display()
                )
            })?;
        self.add_publication_rules(
            projection.directory_reads,
            projection.directory_creates,
            projection.file_reads,
            projection.file_writes,
        );
        Ok(())
    }

    fn add_publication_rules(
        &mut self,
        directory_reads: Vec<PathBuf>,
        directory_creates: Vec<PathBuf>,
        file_reads: Vec<octopus_file_storage::publication::PublicationFileSelector>,
        file_writes: Vec<octopus_file_storage::publication::PublicationFileSelector>,
    ) {
        use octopus_file_storage::publication::PublicationFileSelector;
        for (paths, access) in [
            (directory_reads, FileAccess::Read),
            (directory_creates, FileAccess::CreatePrivateDirectory),
        ] {
            for path in paths {
                self.files.push(FileRule {
                    selector: FileSelector::Exact {
                        path,
                        directory: true,
                    },
                    access,
                });
            }
        }
        for (selectors, access) in [
            (file_reads, FileAccess::Read),
            (file_writes, FileAccess::Write),
        ] {
            for selector in selectors {
                let selector = match selector {
                    PublicationFileSelector::Exact(path) => FileSelector::Exact {
                        path,
                        directory: false,
                    },
                    PublicationFileSelector::Names {
                        directory,
                        prefix,
                        suffix,
                    } => FileSelector::Names {
                        directory,
                        prefix,
                        suffix,
                    },
                };
                self.files.push(FileRule { selector, access });
            }
        }
    }

    fn path_is_writable(&self, target: &Path) -> bool {
        self.files.iter().any(|rule| {
            if !matches!(rule.access, FileAccess::Write) {
                return false;
            }
            match &rule.selector {
                FileSelector::Exact { path, .. } => target == path.as_path(),
                FileSelector::Tree { path } => target == path.as_path() || target.starts_with(path),
                FileSelector::Names {
                    directory,
                    prefix,
                    suffix,
                } => {
                    target.parent() == Some(directory.as_path())
                        && target
                            .file_name()
                            .is_some_and(|name| name_matches(name, prefix, suffix))
                }
            }
        })
    }

    fn path_is_readable(&self, target: &Path) -> bool {
        self.files.iter().any(|rule| {
            if !matches!(
                rule.access,
                FileAccess::Read | FileAccess::Write | FileAccess::Execute | FileAccess::Map
            ) {
                return false;
            }
            match &rule.selector {
                FileSelector::Exact { path, .. } => target == path.as_path(),
                FileSelector::Tree { path } => target == path.as_path() || target.starts_with(path),
                FileSelector::Names {
                    directory,
                    prefix,
                    suffix,
                } => {
                    target.parent() == Some(directory.as_path())
                        && target
                            .file_name()
                            .is_some_and(|name| name_matches(name, prefix, suffix))
                }
            }
        })
    }

    fn tree_overlaps_readable(&self, dir: &Path) -> bool {
        self.files.iter().any(|rule| {
            if !matches!(
                rule.access,
                FileAccess::Read | FileAccess::Write | FileAccess::Execute | FileAccess::Map
            ) {
                return false;
            }
            match &rule.selector {
                FileSelector::Exact { path, .. } => {
                    path.as_path() == dir || path.starts_with(dir)
                }
                FileSelector::Tree { path } => {
                    dir == path.as_path() || dir.starts_with(path) || path.starts_with(dir)
                }
                FileSelector::Names {
                    directory,
                    prefix,
                    suffix,
                } => {
                    directory.as_path() == dir
                        || directory.starts_with(dir)
                        || (dir.parent() == Some(directory.as_path())
                            && dir
                                .file_name()
                                .is_some_and(|name| name_matches(name, prefix, suffix)))
                }
            }
        })
    }

    pub fn apparmor_profile(&self, name: &str, uid: u32) -> Result<String, String> {
        let prefix = format!("octopus-{uid}-");
        let suffix = name
            .strip_prefix(&prefix)
            .ok_or("AppArmor profile has a foreign owner")?;
        if suffix.len() != 64
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("AppArmor profile has an invalid identity".into());
        }
        let mut lines = BTreeSet::new();
        for rule in &self.files {
            let permissions = match rule.access {
                FileAccess::Read => "rk",
                FileAccess::Write => "rwkl",
                FileAccess::Execute => "rmix",
                FileAccess::Map => "mr",
                FileAccess::CreateDirectory | FileAccess::CreatePrivateDirectory => {
                    if !matches!(
                        &rule.selector,
                        FileSelector::Exact {
                            directory: true,
                            ..
                        }
                    ) {
                        return Err("directory preparation requires an exact directory".into());
                    }
                    "rk"
                }
            };
            for pattern in rule.selector.patterns()? {
                lines.insert(format!("  \"{pattern}\" {permissions},\n"));
            }
        }
        let mut profile = format!(
            "abi <abi/4.0>,\nprofile {name} flags=(mediate_deleted) {{\n  signal (receive),\n  signal (send) peer={name},\n  signal (send) peer={name}//&unconfined,\n  ptrace (read, trace) peer={name},\n  ptrace (read, trace) peer={name}//&unconfined,\n  ptrace (readby, tracedby),\n"
        );
        profile.push_str("  owner /proc/[0-9]*/{auxv,cmdline,environ,exe,fd/,fdinfo/,fdinfo/*,limits,maps,mountinfo,mounts,cgroup,stat,statm,status,task/,task/[0-9]*/{stat,status}} r,\n  /proc/{cpuinfo,meminfo,version,sys/kernel/hostname,sys/kernel/osrelease} r,\n  /sys/devices/system/cpu/ r,\n  /sys/devices/system/cpu/** r,\n  /sys/devices/system/node/ r,\n  /sys/devices/system/node/** r,\n  /sys/fs/cgroup/{cpu.max,cpu.cfs_quota_us,cpu.cfs_period_us,memory.max,memory.limit_in_bytes} r,\n  /sys/fs/cgroup/**/{cpu.max,cpu.cfs_quota_us,cpu.cfs_period_us,memory.max,memory.limit_in_bytes} r,\n");
        for line in lines {
            profile.push_str(&line);
        }
        let mut quiet_deny_lines = BTreeSet::new();
        for path in &self.quiet_deny_write_exact {
            if self.path_is_writable(path) {
                continue;
            }
            for pattern in (FileSelector::Exact {
                path: path.clone(),
                directory: false,
            })
            .patterns()?
            {
                quiet_deny_lines.insert(format!("  deny \"{pattern}\" w,\n"));
            }
        }
        for line in quiet_deny_lines {
            profile.push_str(&line);
        }
        for path in QUIET_DENY_READ_EXACT {
            if !self.path_is_readable(Path::new(path)) {
                profile.push_str(&format!("  deny {path} r,\n"));
            }
        }
        for dir in QUIET_DENY_READ_TREES {
            if !self.tree_overlaps_readable(Path::new(dir)) {
                profile.push_str(&format!("  deny {dir}/ r,\n  deny {dir}/** r,\n"));
            }
        }
        for path in &self.read_only {
            for pattern in (FileSelector::Tree { path: path.clone() }).patterns()? {
                profile.push_str(&format!("  deny \"{pattern}\" wl,\n"));
            }
            for ancestor in path.ancestors().skip(1) {
                for pattern in (FileSelector::Exact {
                    path: ancestor.to_path_buf(),
                    directory: true,
                })
                .patterns()?
                {
                    profile.push_str(&format!("  deny \"{pattern}\" w,\n"));
                }
            }
        }
        if self.allow_network {
            profile.push_str("  network,\n  unix,\n");
        } else if self.allow_local_ipc || self.allow_application_launch {
            profile.push_str("  network unix,\n  unix,\n");
        } else {
            profile.push_str(ANONYMOUS_STREAM_SOCKETS);
        }
        if self.allow_application_launch {
            profile.push_str("  dbus (send, receive),\n");
        }
        if self.allow_process_signals {
            profile.push_str("  signal (send),\n");
        }
        if self.allow_sandbox_delegation {
            profile.push_str(&format!("  change_profile -> octopus-{uid}-*,\n"));
            profile.push_str("  change_profile -> unconfined,\n  /proc/[0-9]*/attr/current w,\n");
            profile.push_str(&format!("  \"{PLATFORM_EXECUTABLE}\" rmix,\n"));
            profile.push_str(&format!(
                "  \"{CONTROL_SOCKET}\" rw,\n  unix (create, getattr, getopt, setopt),\n"
            ));
        } else {
            profile.push_str(&format!("  deny \"{CONTROL_SOCKET}\" rw,\n"));
        }
        profile.push_str(
            "  deny /sys/kernel/security/** wkl,\n  deny /sys/fs/cgroup/** wkl,\n  deny /run/octopus-process-authority/ wkl,\n  deny /run/octopus-process-authority/leases/** rwkl,\n  deny /run/octopus-process-authority/service.lock rwkl,\n}\n"
        );
        Ok(profile)
    }
}

/// A process without network or local IPC authority still owns unnamed stream
/// socket pairs, which event loops use as their wake-up channel. AppArmor
/// mediates a pathname socket through the file rule of its path, so the
/// control socket is granted or denied by path; a `unix` rule only accepts
/// abstract or unnamed addresses. Abstract sockets stay closed here.
const ANONYMOUS_STREAM_SOCKETS: &str =
    "  unix (create, read, write, shutdown, getattr, getopt, setopt) type=stream addr=none,\n";

const QUIET_DENY_READ_EXACT: &[&str] = &[
    "/proc/filesystems",
    "/proc/sys/vm/overcommit_memory",
    "/proc/sys/vm/mmap_min_addr",
    "/sys/kernel/mm/transparent_hugepage/enabled",
    "/etc/ssl/openssl.cnf",
    "/etc/default/locale",
];
const QUIET_DENY_READ_TREES: &[&str] = &[
    "/usr/share/coreutils/locales",
    "/usr/share/locale-langpack",
];

fn name_matches(name: &OsStr, prefix: &OsStr, suffix: &OsStr) -> bool {
    name.as_bytes()
        .strip_prefix(prefix.as_bytes())
        .is_some_and(|rest| rest.ends_with(suffix.as_bytes()))
}

impl FileSelector {
    fn patterns(&self) -> Result<Vec<String>, String> {
        match self {
            Self::Exact { path, directory } => {
                let mut path = literal_path(path)?;
                if *directory && path != "/" {
                    path.push('/');
                }
                Ok(vec![path])
            }
            Self::Tree { path } => {
                let path = literal_path(path)?;
                let stem = path.trim_end_matches('/');
                Ok(vec![path.clone(), format!("{stem}/"), format!("{stem}/**")])
            }
            Self::Names {
                directory,
                prefix,
                suffix,
            } => {
                if prefix.is_empty()
                    || suffix.is_empty()
                    || prefix.as_bytes().contains(&b'/')
                    || suffix.as_bytes().contains(&b'/')
                {
                    return Err("publication filename selector is not bounded".into());
                }
                let directory = literal_path(directory)?;
                Ok(vec![format!(
                    "{}/{}*{}",
                    directory.trim_end_matches('/'),
                    literal(prefix)?,
                    literal(suffix)?
                )])
            }
        }
    }
}

fn literal_path(path: &Path) -> Result<String, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err("AppArmor path must be normal and absolute".into());
    }
    literal(path.as_os_str())
}

fn literal(value: &OsStr) -> Result<String, String> {
    let text = value
        .to_str()
        .ok_or("AppArmor policy paths must be UTF-8")?;
    let mut escaped = String::new();
    for character in text.chars() {
        if character.is_control() {
            return Err("AppArmor policy path contains a control character".into());
        }
        if "\\\"*?[]{}^".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    Ok(escaped)
}

fn executable_interpreter(path: &Path) -> Result<Option<PathBuf>, String> {
    use object::read::elf::ProgramHeader;
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "Linux executable '{}' cannot be read: {error}",
            path.display()
        )
    })?;
    let interpreter = if bytes.starts_with(b"#!") {
        bytes[2..]
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default()
            .split(|byte| byte.is_ascii_whitespace())
            .find(|part| !part.is_empty())
            .ok_or("Linux script has an empty interpreter")?
            .to_vec()
    } else {
        let file = object::File::parse(bytes.as_slice()).map_err(|error| {
            format!("Linux executable '{}' is invalid: {error}", path.display())
        })?;
        let interpreter = match file {
            object::File::Elf32(file) => file
                .elf_program_headers()
                .iter()
                .map(|segment| segment.interpreter(file.endian(), bytes.as_slice()))
                .collect::<Result<Vec<_>, _>>(),
            object::File::Elf64(file) => file
                .elf_program_headers()
                .iter()
                .map(|segment| segment.interpreter(file.endian(), bytes.as_slice()))
                .collect::<Result<Vec<_>, _>>(),
            _ => {
                return Err(format!(
                    "Linux executable '{}' is not ELF or a script",
                    path.display()
                ))
            }
        }
        .map_err(|error| format!("Linux executable interpreter is invalid: {error}"))?;
        let mut interpreters = interpreter.into_iter().flatten();
        let Some(interpreter) = interpreters.next() else {
            return Ok(None);
        };
        if interpreters.next().is_some() {
            return Err("Linux executable declares several interpreters".into());
        }
        interpreter.to_vec()
    };
    let interpreter = PathBuf::from(OsStr::from_bytes(&interpreter));
    if !interpreter.is_absolute() {
        return Err("Linux executable interpreter is not absolute".into());
    }
    let interpreter = normalize_existing_path(interpreter)?;
    if !executable_file(&interpreter) {
        return Err("Linux executable interpreter is not executable".into());
    }
    Ok(Some(interpreter))
}

pub(super) fn runtime_executables(
    mut pending: BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut visited = BTreeSet::new();
    while let Some(path) = pending.pop_first() {
        if path.is_dir() || !visited.insert(path.clone()) {
            continue;
        }
        if let Some(interpreter) = executable_interpreter(&path)? {
            pending.insert(interpreter);
        }
    }
    Ok(visited)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_name() -> String {
        format!("octopus-1000-{}", "0".repeat(64))
    }

    fn policy_with(files: Vec<FileRule>) -> LinuxProcessPolicy {
        LinuxProcessPolicy {
            files,
            read_only: Vec::new(),
            quiet_deny_write_exact: Vec::new(),
            allow_network: false,
            allow_local_ipc: false,
            allow_application_launch: false,
            allow_sandbox_delegation: false,
            allow_process_signals: false,
        }
    }

    fn exact(path: &str, directory: bool, access: FileAccess) -> FileRule {
        FileRule { selector: FileSelector::Exact { path: PathBuf::from(path), directory }, access }
    }

    fn tree(path: &str, access: FileAccess) -> FileRule {
        FileRule { selector: FileSelector::Tree { path: PathBuf::from(path) }, access }
    }

    fn names(dir: &str, prefix: &str, suffix: &str, access: FileAccess) -> FileRule {
        FileRule {
            selector: FileSelector::Names { directory: PathBuf::from(dir), prefix: OsString::from(prefix), suffix: OsString::from(suffix) },
            access,
        }
    }

    fn quiet_deny_index_policy() -> LinuxProcessPolicy {
        let mut policy = policy_with(vec![
            tree("/var/tmp/octopus-room", FileAccess::Write),
            exact("/var/tmp/octopus-index/ledger-index.sqlite", false, FileAccess::Read),
            exact("/var/tmp/octopus-index/ledger-index.sqlite-wal", false, FileAccess::Read),
            exact("/var/tmp/octopus-index/ledger-index.sqlite-shm", false, FileAccess::Read),
        ]);
        policy.quiet_deny_write_exact = ["", "-wal", "-shm"]
            .into_iter()
            .map(|s| PathBuf::from(format!("/var/tmp/octopus-index/ledger-index.sqlite{s}")))
            .chain([PathBuf::from("/var/tmp/octopus-room/writable.txt")])
            .collect();
        policy
    }

    fn profile(network: bool, local_ipc: bool, delegation: bool) -> String {
        let mut policy = policy_with(vec![tree("/var/tmp/octopus-room", FileAccess::Write)]);
        policy.allow_network = network;
        policy.allow_local_ipc = local_ipc;
        policy.allow_sandbox_delegation = delegation;
        policy.apparmor_profile(&profile_name(), 1000).unwrap()
    }

    fn shapes() -> Vec<(bool, bool, bool)> {
        let mut shapes = Vec::new();
        for network in [false, true] {
            for local_ipc in [false, true] {
                for delegation in [false, true] {
                    shapes.push((network, local_ipc, delegation));
                }
            }
        }
        shapes
    }

    #[test]
    fn the_control_socket_is_granted_or_denied_by_its_path() {
        for (network, local_ipc, delegation) in shapes() {
            let profile = profile(network, local_ipc, delegation);
            assert!(!profile.contains("peer=(addr=\"/"), "{profile}");
            assert!(!profile.contains("deny unix"), "{profile}");
            assert!(!profile.contains("unix (connect"), "{profile}");
            let allowed = profile.contains(&format!("  \"{CONTROL_SOCKET}\" rw,\n"));
            let denied = profile.contains(&format!("  deny \"{CONTROL_SOCKET}\" rw,\n"));
            assert_eq!((allowed, denied), (delegation, !delegation), "{profile}");
        }
    }

    #[test]
    fn a_process_without_ipc_owns_only_unnamed_stream_sockets() {
        for delegation in [false, true] {
            let closed = profile(false, false, delegation);
            assert!(closed.contains(ANONYMOUS_STREAM_SOCKETS), "{closed}");
            assert!(!closed.contains("  unix,\n") && !closed.contains("network"), "{closed}");
            assert_eq!(
                closed.contains("  unix (create, getattr, getopt, setopt),\n"),
                delegation,
                "only delegation creates the socket it connects to the control path: {closed}"
            );
        }
        let local = profile(false, true, false);
        assert!(local.contains("  network unix,\n  unix,\n") && !local.contains(ANONYMOUS_STREAM_SOCKETS), "{local}");
        let network = profile(true, false, false);
        assert!(network.contains("  network,\n  unix,\n") && !network.contains(ANONYMOUS_STREAM_SOCKETS), "{network}");
    }

    #[test]
    fn cpu_topology_and_cgroup_limits_are_read_only() {
        let generated = profile(false, false, false);
        assert!(generated.contains("  /sys/devices/system/cpu/** r,\n"), "{generated}");
        assert!(generated.contains("  /sys/devices/system/node/** r,\n"), "{generated}");
        assert!(generated.contains("  /sys/fs/cgroup/**/{cpu.max,cpu.cfs_quota_us,cpu.cfs_period_us,memory.max,memory.limit_in_bytes} r,\n"), "{generated}");
        assert!(generated.contains("  deny /sys/fs/cgroup/** wkl,\n"), "{generated}");
    }

    #[test]
    fn quiet_deny_rules_respect_grants_and_ignore_ancestors() {
        let idx = quiet_deny_index_policy();
        assert_eq!(serde_json::from_str::<LinuxProcessPolicy>(&serde_json::to_string(&idx).unwrap()).unwrap(), idx);
        let idx_profile = idx.apparmor_profile(&profile_name(), 1000).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let p = format!("/var/tmp/octopus-index/ledger-index.sqlite{suffix}");
            assert!(idx_profile.contains(&format!("  \"{p}\" rk,\n")) && idx_profile.contains(&format!("  deny \"{p}\" w,\n")) && !idx_profile.contains(&format!("  \"{p}\" rwkl,\n")));
        }
        assert!(!idx_profile.contains("  deny \"/var/tmp/octopus-room/writable.txt\" w,\n"));

        let render = |files| policy_with(files).apparmor_profile(&profile_name(), 1000).unwrap();
        let ancestors = render(vec![
            tree("/var/tmp/octopus-room", FileAccess::Write),
            exact("/", true, FileAccess::Read),
            exact("/usr", true, FileAccess::Read),
            exact("/usr/share", true, FileAccess::Read),
            exact("/usr/share/coreutils", true, FileAccess::Read),
            exact("/etc", true, FileAccess::Read),
            exact("/etc/ssl", true, FileAccess::Read),
            exact("/proc", true, FileAccess::Read),
            names("/usr/share/coreutils", ".tmp.", ".lock", FileAccess::Read),
        ]);
        for path in QUIET_DENY_READ_EXACT {
            assert!(ancestors.contains(&format!("  deny {path} r,\n")));
        }
        for dir in QUIET_DENY_READ_TREES {
            assert!(ancestors.contains(&format!("  deny {dir}/ r,\n  deny {dir}/** r,\n")));
        }

        let usr_share = render(vec![tree("/usr/share", FileAccess::Read)]);
        assert!(QUIET_DENY_READ_TREES.iter().all(|d| !usr_share.contains(&format!("  deny {d}/ r,\n"))));
        assert!(QUIET_DENY_READ_EXACT.iter().all(|p| usr_share.contains(&format!("  deny {p} r,\n"))));
        for rule in [
            exact("/usr/share/coreutils/locales/foo", false, FileAccess::Read),
            tree("/usr/share/coreutils/locales", FileAccess::Read),
            names("/usr/share/coreutils", "loc", "les", FileAccess::Read),
        ] {
            let p = render(vec![rule]);
            assert!(!p.contains("  deny /usr/share/coreutils/locales/ r,\n"));
            assert!(p.contains("  deny /usr/share/locale-langpack/ r,\n  deny /usr/share/locale-langpack/** r,\n"));
        }
        let etc_tree = render(vec![tree("/etc", FileAccess::Read)]);
        assert!(!etc_tree.contains("  deny /etc/ssl/openssl.cnf r,\n") && !etc_tree.contains("  deny /etc/default/locale r,\n"));
        assert!(etc_tree.contains("  deny /proc/sys/vm/mmap_min_addr r,\n") && etc_tree.contains("  deny /sys/kernel/mm/transparent_hugepage/enabled r,\n"));

        let exact_root = policy_with(vec![exact("/", true, FileAccess::Read)]);
        let tree_root = policy_with(vec![tree("/", FileAccess::Read)]);
        assert!(QUIET_DENY_READ_EXACT.iter().all(|p| !exact_root.path_is_readable(Path::new(p)) && tree_root.path_is_readable(Path::new(p))));
        assert!(QUIET_DENY_READ_TREES.iter().all(|d| !exact_root.tree_overlaps_readable(Path::new(d)) && tree_root.tree_overlaps_readable(Path::new(d))));

        let workspace = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(workspace.path()).unwrap();
        let ancestor = canonical.parent().unwrap().to_path_buf();
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let auth = ProcessAuthority::resolve(
            vec![canonical.clone()], vec![canonical], Vec::new(), Vec::new(), Vec::new(),
            vec![exe.clone()], false, false, false, false, false, octopus_descriptors::ProcessSandboxMode::Enforced,
        ).unwrap();
        let projected = project(&auth, &exe).unwrap();
        assert!(projected.files.contains(&exact(ancestor.to_str().unwrap(), true, FileAccess::Read)));
        assert!(!projected.files.contains(&exact("/", true, FileAccess::Read)));
        let rendered = projected.apparmor_profile(&profile_name(), 1000).unwrap();
        assert!(QUIET_DENY_READ_EXACT.iter().all(|p| rendered.contains(&format!("  deny {p} r,\n"))));
        assert!(QUIET_DENY_READ_TREES.iter().all(|d| rendered.contains(&format!("  deny {d}/ r,\n  deny {d}/** r,\n"))));
    }

    #[test]
    fn compact_preserves_boundaries_and_shrinks_large_policies() {
        let mut policy = policy_with(vec![
            exact("/var", true, FileAccess::Read),
            exact("/var", true, FileAccess::Read),
            tree("/var/octopus/home", FileAccess::Write),
            tree("/var/octopus/home", FileAccess::Read),
            tree("/var/octopus/home/registry", FileAccess::Write),
            exact("/var/octopus/home/registry/run.json", false, FileAccess::Write),
            names("/var/octopus/home/registry/.octopus-publication", "run.", ".tmp", FileAccess::Write),
            exact("/var/octopus/home/registry", true, FileAccess::CreateDirectory),
            exact("/var/octopus/home/registry", true, FileAccess::CreateDirectory),
            exact("/var/octopus/home/registry/.octopus-publication", true, FileAccess::CreateDirectory),
            exact("/var/octopus/home/registry/.octopus-publication", true, FileAccess::CreatePrivateDirectory),
            exact("/var/octopus/home2/other.json", false, FileAccess::Read),
            tree("/opt/pkg", FileAccess::Read),
            exact("/opt/pkg/state.json", false, FileAccess::Write),
            tree("/opt/pkg/bin", FileAccess::Execute),
            tree("/opt/exec_only", FileAccess::Execute),
            exact("/opt/exec_only/helper", false, FileAccess::Map),
            exact("/opt/exec_only/lockable", false, FileAccess::Read),
            exact("/srv/entry", false, FileAccess::Read),
            exact("/srv/entry", true, FileAccess::Read),
        ]);
        policy.read_only = vec![PathBuf::from("/var/octopus/home/pinned"); 2];
        policy.quiet_deny_write_exact = vec![PathBuf::from("/opt/pkg/index.sqlite"); 2];
        let before = policy.clone();
        policy.compact();

        assert_eq!((policy.read_only.len(), policy.quiet_deny_write_exact.len()), (1, 1));
        assert!(policy.files.len() < before.files.len());
        for expected in [
            tree("/var/octopus/home", FileAccess::Write),
            exact("/var/octopus/home/registry", true, FileAccess::CreateDirectory),
            exact("/var/octopus/home/registry/.octopus-publication", true, FileAccess::CreatePrivateDirectory),
            exact("/var/octopus/home2/other.json", false, FileAccess::Read),
            exact("/opt/pkg/state.json", false, FileAccess::Write),
            tree("/opt/pkg/bin", FileAccess::Execute),
            exact("/opt/exec_only/lockable", false, FileAccess::Read),
            exact("/srv/entry", false, FileAccess::Read),
            exact("/srv/entry", true, FileAccess::Read),
        ] {
            assert!(policy.files.contains(&expected), "missing {expected:?}");
        }
        for dropped in [
            tree("/var/octopus/home", FileAccess::Read),
            tree("/var/octopus/home/registry", FileAccess::Write),
            exact("/var/octopus/home/registry/.octopus-publication", true, FileAccess::CreateDirectory),
            exact("/opt/exec_only/helper", false, FileAccess::Map),
        ] {
            assert!(!policy.files.contains(&dropped), "unexpected {dropped:?}");
        }
        for probe in [
            "/var/octopus/home/registry/run.json", "/var/octopus/home2/other.json", "/var/octopus/home2/ungranted.json",
            "/opt/pkg/state.json", "/opt/pkg/readonly.json", "/opt/exec_only/helper", "/opt/exec_only/lockable", "/etc/shadow",
        ] {
            assert_eq!(policy.path_is_readable(Path::new(probe)), before.path_is_readable(Path::new(probe)), "{probe}");
            assert_eq!(policy.path_is_writable(Path::new(probe)), before.path_is_writable(Path::new(probe)), "{probe}");
        }

        let temp = tempfile::tempdir().unwrap();
        let interfaces_root = std::fs::canonicalize(temp.path()).unwrap().join("registry/interface");
        std::fs::create_dir_all(&interfaces_root).unwrap();
        let (mut readable, mut writable_exact) = (Vec::with_capacity(220), Vec::with_capacity(440));
        for i in 0..220usize {
            let dir = interfaces_root.join(format!("world_iface_{i:03}"));
            std::fs::create_dir_all(&dir).unwrap();
            let desc = dir.join("interface.json");
            std::fs::write(&desc, "{}").unwrap();
            readable.push(dir.clone());
            writable_exact.extend([dir, desc]);
        }
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let auth = ProcessAuthority::resolve(
            readable, Vec::new(), Vec::new(), Vec::new(), writable_exact, vec![exe.clone()],
            false, false, false, true, false, octopus_descriptors::ProcessSandboxMode::Enforced,
        ).unwrap();
        let raw = project_uncompacted(&auth, &exe).unwrap();
        let compacted = project(&auth, &exe).unwrap();
        assert!(compacted.files.len() * 2 < raw.files.len());
        assert!(serde_json::to_vec(&compacted).unwrap().len() * 2 < serde_json::to_vec(&raw).unwrap().len());
        let frame = compacted.encode_prepare_frame().unwrap();
        let body = std::str::from_utf8(&frame).unwrap().strip_prefix("PREPARE ").and_then(|s| s.strip_suffix('\n')).unwrap();
        assert_eq!(serde_json::from_str::<LinuxProcessPolicy>(body).unwrap(), compacted);
        assert!(compacted.path_is_writable(&interfaces_root.join("world_iface_042/interface.json")));
        assert!(!compacted.path_is_readable(&interfaces_root.join("world_ungranted/interface.json")));
    }

    #[test]
    fn prepare_frame_enforces_transport_limit_with_diagnostics() {
        assert_eq!(MAX_REQUEST_BYTES, 16 * 1024 * 1024);
        let large = policy_with(
            (0..12_000usize)
                .map(|i| exact(&format!("/var/lib/octopus/registry/interface/iface_{i:05}/descriptor_payload_entry.json"), false, FileAccess::Write))
                .collect(),
        );
        let large_json = serde_json::to_vec(&large).unwrap();
        assert!(large_json.len() > 1024 * 1024);
        assert_eq!(large.encode_prepare_frame().unwrap().len(), 8 + large_json.len() + 1);

        let oversized_path = format!("/{}", "a".repeat(256 * 1024));
        let err = policy_with((0..70).map(|_| exact(&oversized_path, false, FileAccess::Read)).collect())
            .encode_prepare_frame()
            .unwrap_err();
        assert!(err.contains("Linux process policy exceeds its transport limit") && err.contains(&MAX_REQUEST_BYTES.to_string()));
    }

    #[test]
    fn nonexistent_parent_write_argument_projects_exact_directory_write_without_grandparent_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap().join("registry/tool");
        std::fs::create_dir_all(&root).unwrap();
        let new_dir = root.join("new-tool");
        let target = new_dir.join("tool.json");
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let spec = ProcessAuthoritySpec {
            write_argument_paths: vec![ProcessArgumentPathSpec {
                pointer: "/path".into(),
                within_root_slots: BTreeSet::from(["registry_tool_root".into()]),
                include_parent: true,
            }],
            ..ProcessAuthoritySpec::default()
        };
        let slots = BTreeMap::from([("registry_tool_root".into(), root.clone())]);
        let resolved = spec
            .resolve_for_invocation(&root, &slots, &BTreeMap::new(), &serde_json::json!({ "path": target }), [], [], [], [])
            .unwrap();
        let projected = project(&resolved.authority, &exe).unwrap();
        assert!(projected.files.contains(&exact(new_dir.to_str().unwrap(), true, FileAccess::Write)));
        assert!(projected.files.contains(&exact(new_dir.to_str().unwrap(), true, FileAccess::CreateDirectory)));
        assert!(projected.files.contains(&exact(new_dir.join(".octopus-publication").to_str().unwrap(), true, FileAccess::CreatePrivateDirectory)));
        assert!(!projected.files.contains(&exact(root.join(".octopus-publication").to_str().unwrap(), true, FileAccess::CreatePrivateDirectory)));
        let rendered = projected.apparmor_profile(&profile_name(), 1000).unwrap();
        assert!(rendered.contains(&format!("  \"{}/\" rwkl,\n", new_dir.display())));
        assert!(rendered.contains(&format!("  \"{}\" rwkl,\n", target.display())));
        assert!(!projected.path_is_writable(&root.join("sibling/tool.json")));
    }

    #[test]
    fn directory_write_argument_projects_bounded_directory_publication_without_siblings() {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap().join("registry/tool");
        let target_dir = root.join("old-tool");
        let sibling_dir = root.join("keep-tool");
        let target_file = target_dir.join("tool.json");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::create_dir_all(&sibling_dir).unwrap();
        std::fs::write(&target_file, "{}").unwrap();
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let spec = ProcessAuthoritySpec {
            write_argument_paths: ["/deletes/*/path", "/empty_directories/*"]
                .into_iter()
                .map(|pointer| ProcessArgumentPathSpec {
                    pointer: pointer.into(),
                    within_root_slots: BTreeSet::from(["registry_tool_root".into()]),
                    include_parent: true,
                })
                .collect(),
            ..ProcessAuthoritySpec::default()
        };
        let slots = BTreeMap::from([("registry_tool_root".into(), root.clone())]);
        let parent_coord = root.join(".octopus-publication");
        let parent_history = parent_coord.join("history");
        let coord_rule = exact(parent_coord.to_str().unwrap(), true, FileAccess::CreatePrivateDirectory);
        let hist_rule = exact(parent_history.to_str().unwrap(), true, FileAccess::CreatePrivateDirectory);
        for args in [
            serde_json::json!({ "deletes": [], "empty_directories": [target_dir] }),
            serde_json::json!({ "deletes": [{ "path": target_file }], "empty_directories": [target_dir] }),
        ] {
            let resolved = spec.resolve_for_invocation(&root, &slots, &BTreeMap::new(), &args, [], [], [], []).unwrap();
            let projected = project(&resolved.authority, &exe).unwrap();
            assert!(projected.files.contains(&exact("/", true, FileAccess::Read)));
            assert!(projected.files.contains(&tree(target_dir.join(".octopus-publication").to_str().unwrap(), FileAccess::Read)));
            let coord_pos = projected.files.iter().position(|r| r == &coord_rule).unwrap();
            let hist_pos = projected.files.iter().position(|r| r == &hist_rule).unwrap();
            assert!(coord_pos < hist_pos);
            assert!(projected.path_is_writable(&parent_coord.join("dir-00.plan")));
            assert!(projected.path_is_writable(&parent_history.join("directory-00.json")));
            assert!(!projected.path_is_readable(&sibling_dir.join("tool.json")));
            assert!(!projected.path_is_writable(&sibling_dir));
            let rendered = projected.apparmor_profile(&profile_name(), 1000).unwrap();
            assert!(rendered.contains("  \"/\" rk,\n"));
            assert!(rendered.contains(&format!("  \"{}/\" rwkl,\n", target_dir.display())));
            assert!(rendered.contains(&format!("  \"{}/.octopus-publication/**\" rk,\n", target_dir.display())));
        }
    }

    #[test]
    fn sandbox_delegation_grants_bounded_ancestor_history_reads() {
        let temp = tempfile::tempdir().unwrap();
        let state = std::fs::canonicalize(temp.path()).unwrap().join("state");
        let runs = state.join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let authority = ProcessAuthority::resolve(
            [runs],
            Vec::<PathBuf>::new(),
            Vec::<PathBuf>::new(),
            Vec::<PathBuf>::new(),
            Vec::<PathBuf>::new(),
            Vec::<PathBuf>::new(),
            false,
            false,
            false,
            true,
            false,
            octopus_descriptors::ProcessSandboxMode::Enforced,
        )
        .unwrap();
        let projected = project(&authority, &exe).unwrap();
        let history = state.join(".octopus-publication/history");
        assert!(projected.path_is_readable(&history.join("file-abc.json")));
        assert!(projected.path_is_readable(&history.join("directory-abc.json")));
        assert!(!projected.path_is_writable(&history.join("directory-abc.json")));
        assert!(!projected.path_is_readable(&state.join("unrelated.json")));
        let rendered = projected.apparmor_profile(&profile_name(), 1000).unwrap();
        assert!(rendered.contains(&format!("  \"{}/directory-*.json\" rk,\n", history.display())));
        assert!(rendered.contains(&format!("  \"{}/file-*.json\" rk,\n", history.display())));
    }

    #[test]
    fn the_installed_parser_accepts_every_profile_shape() {
        let parser = Path::new("/usr/sbin/apparmor_parser");
        if !parser.is_file() || !Path::new("/etc/apparmor.d/abi/4.0").is_file() {
            return;
        }
        let directory = std::env::temp_dir().join(format!("octopus-apparmor-shapes-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut profiles: Vec<(String, String)> = shapes()
            .into_iter()
            .enumerate()
            .map(|(i, (n, l, d))| (format!("shape-{i} ({n},{l},{d})"), profile(n, l, d)))
            .collect();
        profiles.push(("quiet-deny".into(), quiet_deny_index_policy().apparmor_profile(&profile_name(), 1000).unwrap()));
        let dir_pub = policy_with(vec![
            exact("/", true, FileAccess::Read),
            exact("/var/tmp/octopus-dir", true, FileAccess::Write),
            tree("/var/tmp/octopus-dir/.octopus-publication", FileAccess::Read),
            exact("/var/tmp/.octopus-publication", true, FileAccess::CreatePrivateDirectory),
            exact("/var/tmp/.octopus-publication/history", true, FileAccess::CreatePrivateDirectory),
            names("/var/tmp/.octopus-publication", "dir-", ".plan", FileAccess::Write),
            names("/var/tmp/.octopus-publication/history", "directory-", ".json", FileAccess::Write),
        ]);
        profiles.push(("directory-publication".into(), dir_pub.apparmor_profile(&profile_name(), 1000).unwrap()));
        for (index, (label, contents)) in profiles.into_iter().enumerate() {
            let path = directory.join(format!("shape-{index}"));
            std::fs::write(&path, contents).unwrap();
            let output = std::process::Command::new(parser)
                .args(["--config-file=/dev/null", "--skip-cache", "--skip-kernel-load", "--warn=all", "--Werror", "--jobs=1"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(output.status.success(), "{label}: {}", String::from_utf8_lossy(&output.stderr));
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
