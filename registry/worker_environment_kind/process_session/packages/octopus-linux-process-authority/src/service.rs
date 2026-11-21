use crate::peer::{ensure_alive, with_stopped_peer, Peer};
use octopus_verifiers::process_authority::linux::{
    LinuxProcessPolicy, CONTROL_SOCKET, MAX_REQUEST_BYTES,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const STATE: &str = "/run/octopus-process-authority/leases";
const PARSER: &str = "/usr/sbin/apparmor_parser";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    uid: u32,
    parent: i32,
    parent_start: u64,
    parent_label: String,
    profile: String,
    delegation: bool,
}

struct Lease {
    record: Record,
    owner_open: bool,
    parent_process: Option<Arc<OwnedFd>>,
    last_cleanup_error: Option<String>,
}

struct Host {
    cgroup: PathBuf,
    leases: Mutex<BTreeMap<String, Lease>>,
}

pub fn serve() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(
            "the system service must run as root; Octopus itself runs as its ordinary user".into(),
        );
    }
    if fs::read_to_string("/sys/module/apparmor/parameters/enabled")
        .map_err(display)?
        .trim()
        != "Y"
    {
        return Err("AppArmor is not enabled".into());
    }
    secure_file(Path::new(PARSER), false)?;
    secure_directory(Path::new("/run/octopus-process-authority"))?;
    let _ownership = service_lock()?;
    if !Path::new(STATE).exists() {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(STATE)
            .map_err(display)?;
    }
    secure_directory(Path::new(STATE))?;
    let cgroup = own_cgroup()?;
    let host = Arc::new(Host {
        cgroup,
        leases: Mutex::new(BTreeMap::new()),
    });
    host.recover()?;
    match fs::symlink_metadata(CONTROL_SOCKET) {
        Ok(metadata) => {
            use std::os::unix::fs::FileTypeExt;
            if metadata.uid() != 0 || !metadata.file_type().is_socket() {
                return Err("control socket path is occupied by a foreign object".into());
            }
            fs::remove_file(CONTROL_SOCKET).map_err(display)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(display(error)),
    }
    let listener = UnixListener::bind(CONTROL_SOCKET).map_err(display)?;
    fs::set_permissions(CONTROL_SOCKET, fs::Permissions::from_mode(0o660)).map_err(display)?;
    let collector = Arc::clone(&host);
    std::thread::Builder::new()
        .name("apparmor-profile-release".into())
        .spawn(move || loop {
            if let Err(error) = collector.collect() {
                eprintln!("profile collection: {error}");
            }
            std::thread::sleep(Duration::from_secs(1));
        })
        .map_err(display)?;
    notify_ready()?;
    for connection in listener.incoming() {
        let stream = connection.map_err(display)?;
        let host = Arc::clone(&host);
        std::thread::Builder::new()
            .name("apparmor-process-authority".into())
            .spawn(move || {
                if let Err(error) = handle(&host, stream) {
                    eprintln!("process authority request: {error}");
                }
            })
            .map_err(display)?;
    }
    Ok(())
}

fn handle(host: &Host, stream: UnixStream) -> Result<(), String> {
    let peer = Peer::read(&stream)?;
    if fs::read_link(format!("/proc/{}/ns/user", peer.credentials.pid)).map_err(display)?
        != fs::read_link("/proc/self/ns/user").map_err(display)?
    {
        return Err("process authority cannot be delegated across user namespaces".into());
    }
    let mut reader = BufReader::new(stream);
    let mut frame = String::new();
    reader
        .by_ref()
        .take(MAX_REQUEST_BYTES + 10)
        .read_line(&mut frame)
        .map_err(display)?;
    if frame.len() as u64 > MAX_REQUEST_BYTES + 9 || !frame.ends_with('\n') {
        return Err("request exceeds its frame limit or is incomplete".into());
    }
    if let Some(body) = frame.strip_prefix("PREPARE ") {
        let result = serde_json::from_str::<LinuxProcessPolicy>(body)
            .map_err(display)
            .and_then(|policy| host.prepare(&peer, policy));
        let (token, profile) = match result {
            Ok(value) => value,
            Err(error) => {
                let text = error.replace(['\n', '\r'], " ");
                reader
                    .get_mut()
                    .write_all(format!("ERROR {text}\n").as_bytes())
                    .map_err(display)?;
                return Err(error);
            }
        };
        let outcome = reader
            .get_mut()
            .write_all(format!("OK {profile}\n").as_bytes())
            .and_then(|_| reader.read(&mut [0u8; 1]));
        host.close_owner(&token)?;
        match outcome {
            Ok(0) => Ok(()),
            Ok(_) => Err("preparation lease accepts no further requests".into()),
            Err(error) => Err(display(error)),
        }
    } else if let Some(token) = frame
        .strip_prefix("ATTACH ")
        .and_then(|line| line.strip_suffix('\n'))
    {
        let result = with_stopped_peer(&peer, || host.attach(&peer, token));
        reader
            .get_mut()
            .write_all(&[u8::from(result.is_err())])
            .map_err(display)?;
        result
    } else {
        Err("unknown process-authority request".into())
    }
}

impl Host {
    fn prepare(
        &self,
        caller: &Peer,
        policy: LinuxProcessPolicy,
    ) -> Result<(String, String), String> {
        let mut leases = self.leases.lock().map_err(display)?;
        let peer = &caller.credentials;
        let label = process_label(peer.pid)?;
        if label != "unconfined" && !label.ends_with(" (unconfined)") {
            let profile = enforced_profile_name(&label)
                .ok_or("caller is not in an enforced or unconfined profile")?;
            if !leases.values().any(|lease| {
                lease.record.uid == peer.uid
                    && lease.record.profile == profile
                    && lease.record.delegation
            }) {
                return Err("caller has no sandbox-delegation authority".into());
            }
        }
        let (_, parent_start) = process_identity(peer.pid)?;
        ensure_alive(&caller.process)?;
        let mut random = [0u8; 32];
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(display)?;
        let token = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let profile = format!("octopus-{}-{token}", peer.uid);
        let text = policy.apparmor_profile(&profile, peer.uid)?;
        let record = Record {
            uid: peer.uid,
            parent: peer.pid,
            parent_start,
            parent_label: label,
            profile: profile.clone(),
            delegation: policy.allow_sandbox_delegation,
        };
        let directory = Path::new(STATE).join(&profile);
        let preparing = Path::new(STATE).join(format!(".preparing-{profile}"));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&preparing)
            .map_err(display)?;
        let result = (|| {
            write_new(&preparing.join("policy"), text.as_bytes())?;
            write_new(
                &preparing.join("record.json"),
                &serde_json::to_vec(&record).map_err(display)?,
            )?;
            fs::rename(&preparing, &directory).map_err(display)?;
            fs::create_dir(self.cgroup.join(&profile)).map_err(display)?;
            parser("--add", &directory.join("policy"))?;
            if !profile_loaded(&profile)? {
                return Err("AppArmor did not enforce the loaded profile".into());
            }
            Ok(())
        })();
        if let Err(error) = result {
            let cleanup = self.release(&record).and_then(|released| {
                if released && preparing.try_exists().map_err(display)? {
                    self.remove_inactive_stage(&preparing, &profile)?;
                }
                Ok(released)
            });
            return Err(match cleanup {
                Ok(true) => error,
                Ok(false) => {
                    format!("{error}; profile retained because its cgroup still has processes")
                }
                Err(cleanup) => format!("{error}; profile cleanup failed: {cleanup}"),
            });
        }
        leases.insert(
            token.clone(),
            Lease {
                record,
                owner_open: true,
                parent_process: Some(Arc::clone(&caller.process)),
                last_cleanup_error: None,
            },
        );
        Ok((token, profile))
    }

    fn attach(&self, caller: &Peer, token: &str) -> Result<(), String> {
        let peer = &caller.credentials;
        validate_token(token)?;
        let leases = self.leases.lock().map_err(display)?;
        let lease = leases.get(token).ok_or("unknown preparation lease")?;
        if !lease.owner_open || lease.record.uid != peer.uid {
            return Err("preparation lease is closed or belongs to another user".into());
        }
        ensure_alive(
            lease
                .parent_process
                .as_deref()
                .ok_or("preparing process identity is absent")?,
        )?;
        if fs::read_link(format!("/proc/{}/ns/user", peer.pid)).map_err(display)?
            != fs::read_link("/proc/self/ns/user").map_err(display)?
        {
            return Err("child changed its user namespace before attachment".into());
        }
        let (parent, start) = process_identity(peer.pid)?;
        let same_process = peer.pid == lease.record.parent && start == lease.record.parent_start;
        let child = parent == lease.record.parent
            && process_identity(parent)?.1 == lease.record.parent_start;
        if !same_process && !child {
            return Err("process is neither the preparing process nor its actual child".into());
        }
        let label = process_label(peer.pid)?;
        if label != lease.record.parent_label || label != process_label(lease.record.parent)? {
            return Err("process confinement differs from its preparing parent".into());
        }
        if !profile_loaded(&lease.record.profile)? {
            return Err("prepared AppArmor profile is no longer enforced".into());
        }
        fs::write(
            self.cgroup.join(&lease.record.profile).join("cgroup.procs"),
            peer.pid.to_string(),
        )
        .map_err(display)
    }

    fn close_owner(&self, token: &str) -> Result<(), String> {
        let mut leases = self.leases.lock().map_err(display)?;
        leases
            .get_mut(token)
            .ok_or("preparation lease disappeared")?
            .owner_open = false;
        Ok(())
    }

    fn recover(&self) -> Result<(), String> {
        let mut leases = self.leases.lock().map_err(display)?;
        for entry in fs::read_dir(STATE).map_err(display)? {
            let directory = entry.map_err(display)?.path();
            secure_directory(&directory)?;
            let name = directory
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("platform state has an invalid directory name")?;
            if let Some(profile) = name
                .strip_prefix(".preparing-")
                .or_else(|| name.strip_prefix(".retired-"))
            {
                self.remove_inactive_stage(&directory, profile)?;
                continue;
            }
            let record_path = directory.join("record.json");
            secure_file(&record_path, true)?;
            let record: Record = serde_json::from_slice(&fs::read(record_path).map_err(display)?)
                .map_err(display)?;
            let prefix = format!("octopus-{}-", record.uid);
            let token = record
                .profile
                .strip_prefix(&prefix)
                .ok_or("retained profile owner is inconsistent")?;
            validate_token(token)?;
            if directory.file_name() != Some(std::ffi::OsStr::new(&record.profile)) {
                return Err("retained profile directory is inconsistent".into());
            }
            if leases
                .insert(
                    token.to_string(),
                    Lease {
                        record,
                        owner_open: false,
                        parent_process: None,
                        last_cleanup_error: None,
                    },
                )
                .is_some()
            {
                return Err("duplicate retained profile identity".into());
            }
        }
        Ok(())
    }

    fn collect(&self) -> Result<(), String> {
        let mut leases = self.leases.lock().map_err(display)?;
        let mut released = Vec::new();
        for (token, lease) in leases.iter_mut() {
            if lease.owner_open {
                continue;
            }
            match self.release(&lease.record) {
                Ok(true) => released.push(token.clone()),
                Ok(false) => (),
                Err(error) => {
                    if lease.last_cleanup_error.as_ref() != Some(&error) {
                        eprintln!("profile {} retained: {error}", lease.record.profile);
                        lease.last_cleanup_error = Some(error);
                    }
                }
            }
        }
        for token in released {
            leases.remove(&token);
        }
        Ok(())
    }

    fn release(&self, record: &Record) -> Result<bool, String> {
        let cgroup = self.cgroup.join(&record.profile);
        match fs::read_to_string(cgroup.join("cgroup.events")) {
            Ok(events) if events.lines().any(|line| line == "populated 0") => (),
            Ok(events) if events.lines().any(|line| line == "populated 1") => {
                return Ok(false);
            }
            Ok(_) => return Err("profile cgroup has no population evidence".into()),
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && !cgroup.try_exists().map_err(display)? =>
            {
                ()
            }
            Err(error) => return Err(format!("profile cgroup cannot prove closure: {error}")),
        }
        let directory = Path::new(STATE).join(&record.profile);
        if profile_loaded(&record.profile)? {
            parser("--remove", &directory.join("policy"))?;
        }
        if cgroup.exists() {
            fs::remove_dir(&cgroup).map_err(display)?;
        }
        let retired = Path::new(STATE).join(format!(".retired-{}", record.profile));
        if directory.try_exists().map_err(display)? {
            fs::rename(directory, &retired).map_err(display)?;
        }
        if retired.try_exists().map_err(display)? {
            self.remove_inactive_stage(&retired, &record.profile)?;
        }
        Ok(true)
    }

    fn remove_inactive_stage(&self, directory: &Path, profile: &str) -> Result<(), String> {
        let (uid, token) = profile
            .strip_prefix("octopus-")
            .and_then(|suffix| suffix.split_once('-'))
            .ok_or("invalid staged profile identity")?;
        let _: u32 = uid.parse().map_err(display)?;
        validate_token(token)?;
        if profile_loaded(profile)? || self.cgroup.join(profile).try_exists().map_err(display)? {
            return Err("staged platform state still has kernel ownership".into());
        }
        fs::remove_dir_all(directory).map_err(display)
    }
}

fn notify_ready() -> Result<(), String> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    let path = std::env::var_os("NOTIFY_SOCKET").ok_or("systemd readiness socket is absent")?;
    let address = if let Some(name) = path.as_bytes().strip_prefix(b"@") {
        SocketAddr::from_abstract_name(name).map_err(display)?
    } else {
        SocketAddr::from_pathname(path).map_err(display)?
    };
    let socket = UnixDatagram::unbound().map_err(display)?;
    let message = b"READY=1";
    if socket.send_to_addr(message, &address).map_err(display)? != message.len() {
        return Err("systemd readiness notification was incomplete".into());
    }
    Ok(())
}

fn process_identity(pid: i32) -> Result<(i32, u64), String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(display)?;
    let (_, fields) = stat.rsplit_once(") ").ok_or("malformed process identity")?;
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    let parent = fields
        .get(1)
        .ok_or("process parent is absent")?
        .parse::<i32>()
        .map_err(display)?;
    let start = fields
        .get(19)
        .ok_or("process start identity is absent")?
        .parse::<u64>()
        .map_err(display)?;
    Ok((parent, start))
}

fn process_label(pid: i32) -> Result<String, String> {
    fs::read_to_string(format!("/proc/{pid}/attr/current"))
        .map(|text| text.trim_end_matches(['\n', '\0']).to_string())
        .map_err(display)
}

fn own_cgroup() -> Result<PathBuf, String> {
    let groups = fs::read_to_string("/proc/self/cgroup").map_err(display)?;
    let path = groups
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or("cgroup v2 is required")?;
    let relative = path
        .strip_prefix('/')
        .ok_or("cgroup identity is not absolute")?;
    if relative.is_empty()
        || Path::new(relative)
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err("process authority must run in its own delegated systemd cgroup".into());
    }
    let root = Path::new("/sys/fs/cgroup").join(relative);
    if !root.join("cgroup.events").is_file() {
        return Err("delegated cgroup v2 state is unavailable".into());
    }
    Ok(root)
}

fn parser(operation: &str, policy: &Path) -> Result<(), String> {
    secure_file(policy, true)?;
    let output = Command::new(PARSER)
        .env_clear()
        .args([
            "--config-file=/dev/null",
            "--skip-cache",
            "--warn=all",
            "--Werror",
            "--jobs=1",
            operation,
        ])
        .arg(policy)
        .stdin(Stdio::null())
        .output()
        .map_err(display)?;
    if !output.status.success() {
        return Err(format!(
            "AppArmor {operation} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

fn profile_loaded(profile: &str) -> Result<bool, String> {
    let profiles = fs::read_to_string("/sys/kernel/security/apparmor/profiles").map_err(display)?;
    let expected = format!("{profile} (enforce)");
    let prefix = format!("{profile} (");
    for line in profiles.lines() {
        if line == expected {
            return Ok(true);
        }
        if line.starts_with(&prefix) {
            return Err("prepared profile is not in enforce mode".into());
        }
    }
    Ok(false)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(display)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(display)
}

fn secure_file(path: &Path, private: bool) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(display)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & (if private { 0o077 } else { 0o022 }) != 0
    {
        return Err(format!(
            "'{}' is not a protected root-owned regular file",
            path.display()
        ));
    }
    Ok(())
}

fn secure_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(display)?;
    if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(format!(
            "'{}' is not a protected root-owned directory",
            path.display()
        ));
    }
    Ok(())
}

fn service_lock() -> Result<File, String> {
    let path = Path::new("/run/octopus-process-authority/service.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(display)?;
    secure_file(path, true)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("another process-authority service holds the platform state".into());
    }
    Ok(file)
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.len() != 64
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid preparation identity".into());
    }
    Ok(())
}

fn enforced_profile_name(label: &str) -> Option<&str> {
    let name = label.strip_suffix(" (enforce)")?;
    let name = name.strip_suffix("//&unconfined").unwrap_or(name);
    if name.is_empty() || name.contains("//&") {
        return None;
    }
    Some(name)
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::enforced_profile_name;

    #[test]
    fn accepts_plain_and_unconfined_stacked_enforced_labels() {
        let profile = format!("octopus-1001-{}", "a".repeat(64));
        assert_eq!(
            enforced_profile_name(&format!("{profile} (enforce)")),
            Some(profile.as_str())
        );
        assert_eq!(
            enforced_profile_name(&format!("{profile}//&unconfined (enforce)")),
            Some(profile.as_str())
        );
    }

    #[test]
    fn rejects_non_enforced_and_multi_profile_stacked_labels() {
        let profile = format!("octopus-1001-{}", "b".repeat(64));
        assert_eq!(enforced_profile_name("unconfined"), None);
        assert_eq!(enforced_profile_name(&format!("{profile} (complain)")), None);
        assert_eq!(
            enforced_profile_name(&format!("{profile}//&other (enforce)")),
            None
        );
        assert_eq!(
            enforced_profile_name(&format!("{profile}//&other//&unconfined (enforce)")),
            None
        );
    }
}
