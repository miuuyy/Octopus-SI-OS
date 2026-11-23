use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::ffi::OsStrExt;

pub const CONTROL_SOCKET: &str = "/run/octopus-process-authority/control.sock";
pub const PLATFORM_EXECUTABLE: &str = "/usr/libexec/octopus/octopus-linux-process-authority";
pub const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxProcessPolicy {
    pub files: Vec<FileRule>,
    pub read_only: Vec<PathBuf>,
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
        policy.add_path(root, FileAccess::Execute, false)?;
    }
    prepared(policy, executable, args)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRule {
    pub selector: FileSelector,
    pub access: FileAccess,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
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
    let mut policy = LinuxProcessPolicy {
        files: Vec::new(),
        read_only: Vec::new(),
        allow_network: authority.allow_network,
        allow_local_ipc: authority.allow_local_ipc,
        allow_application_launch: authority.allow_application_launch,
        allow_sandbox_delegation: authority.allow_sandbox_delegation,
        allow_process_signals: authority.allow_process_signals,
    };
    for (paths, access, exact) in [
        (&authority.readable, FileAccess::Read, false),
        (&authority.writable, FileAccess::Write, false),
        (&authority.executable, FileAccess::Execute, false),
        (&authority.readable_exact, FileAccess::Read, true),
        (&authority.writable_exact, FileAccess::Write, true),
        (&authority.executable_exact, FileAccess::Execute, true),
    ] {
        for path in paths {
            policy.add_path(path.clone(), access, exact)?;
        }
    }
    let mut pending = authority.executable.clone();
    pending.extend(authority.executable_exact.iter().cloned());
    pending.insert(executable.to_path_buf());
    for path in runtime_executables(pending)? {
        policy.add_path(path, FileAccess::Execute, true)?;
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
        policy.add_path(path, FileAccess::Read, true)?;
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
    let mut publication_targets = authority.writable_exact.clone();
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
    if authority.allow_sandbox_delegation {
        for path in &paths {
            for ancestor in octopus_file_storage::publication::publication_ancestor_directories(
                path,
            )
            .map_err(|error| {
                format!(
                    "publication ancestry for '{}' is unavailable: {error}",
                    path.display()
                )
            })? {
                policy.files.push(FileRule {
                    selector: FileSelector::Exact {
                        path: ancestor,
                        directory: true,
                    },
                    access: FileAccess::Read,
                });
            }
        }
    }
    Ok(policy)
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
    fn add_path(&mut self, path: PathBuf, access: FileAccess, exact: bool) -> Result<(), String> {
        let directory = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.is_dir(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => !exact,
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
            Ok(path) => self.add_path(path, access, false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "Linux runtime resource '{path}' cannot be resolved: {error}"
            )),
        }
    }

    fn add_publication(&mut self, path: &Path) -> Result<(), String> {
        use octopus_file_storage::publication::PublicationFileSelector;
        if path.is_dir() {
            return Ok(());
        }
        let projection =
            octopus_file_storage::publication::file_publication_access(path).map_err(|error| {
                format!(
                    "publication authority for '{}' failed: {error}",
                    path.display()
                )
            })?;
        for (paths, access) in [
            (projection.directory_reads, FileAccess::Read),
            (
                projection.directory_creates,
                FileAccess::CreatePrivateDirectory,
            ),
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
            (projection.file_reads, FileAccess::Read),
            (projection.file_writes, FileAccess::Write),
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
        Ok(())
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

    fn profile(network: bool, local_ipc: bool, delegation: bool) -> String {
        LinuxProcessPolicy {
            files: vec![FileRule {
                selector: FileSelector::Tree {
                    path: PathBuf::from("/var/tmp/octopus-room"),
                },
                access: FileAccess::Write,
            }],
            read_only: Vec::new(),
            allow_network: network,
            allow_local_ipc: local_ipc,
            allow_application_launch: false,
            allow_sandbox_delegation: delegation,
            allow_process_signals: false,
        }
        .apparmor_profile(&format!("octopus-1000-{}", "0".repeat(64)), 1000)
        .unwrap()
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
    fn the_installed_parser_accepts_every_profile_shape() {
        let parser = Path::new("/usr/sbin/apparmor_parser");
        if !parser.is_file() || !Path::new("/etc/apparmor.d/abi/4.0").is_file() {
            return;
        }
        let directory = std::env::temp_dir().join(format!("octopus-apparmor-shapes-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        for (index, (network, local_ipc, delegation)) in shapes().into_iter().enumerate() {
            let path = directory.join(format!("shape-{index}"));
            std::fs::write(&path, profile(network, local_ipc, delegation)).unwrap();
            let output = std::process::Command::new(parser)
                .args([
                    "--config-file=/dev/null",
                    "--skip-cache",
                    "--skip-kernel-load",
                    "--warn=all",
                    "--Werror",
                    "--jobs=1",
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "network={network} local_ipc={local_ipc} delegation={delegation}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
