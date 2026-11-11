use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

const SOCKET_FILE: &str = "control.sock";
const MAX_FRAME_BYTES: u64 = 4096;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeControlRequest {
    Status,
    Stop,
    Restart,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeControlStatus {
    pub pid: u32,
    pub phase: String,
    pub started_at_unix_ms: u64,
    pub stopping: Option<RuntimeControlRequest>,
    pub interface_failures: Vec<RuntimeInterfaceFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInterfaceFailure {
    pub interface: String,
    pub error: String,
}

type InterfaceFailures = tokio::sync::watch::Receiver<
    std::collections::BTreeMap<octopus_descriptors::InterfaceId, String>,
>;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFrame {
    request: RuntimeControlRequest,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "outcome", rename_all = "snake_case")]
enum ResponseFrame {
    Status(RuntimeControlStatus),
    Refused { error: String },
}

pub fn socket_path(octopus_home: &Path) -> PathBuf {
    octopus_home
        .join("state")
        .join("runtime-instance")
        .join(SOCKET_FILE)
}

pub struct RuntimeControl {
    path: PathBuf,
    listener: std::os::unix::net::UnixListener,
    stopping: Arc<std::sync::Mutex<Option<RuntimeControlRequest>>>,
    interface_failures: Arc<OnceLock<InterfaceFailures>>,
}

impl RuntimeControl {
    pub fn bind(octopus_home: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let path = socket_path(octopus_home);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => std::fs::remove_file(&path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = std::os::unix::net::UnixListener::bind(&path)
            .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
            .map_err(|error| {
                format!(
                    "runtime control socket '{}' cannot be bound: {error}",
                    path.display()
                )
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            path,
            listener,
            stopping: Arc::new(std::sync::Mutex::new(None)),
            interface_failures: Arc::new(OnceLock::new()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn interface_failures(&self) -> Arc<OnceLock<InterfaceFailures>> {
        self.interface_failures.clone()
    }

    pub fn serve_on_own_thread(
        self,
        octopus_home: PathBuf,
        cancel: CancellationToken,
        restart: Arc<AtomicBool>,
    ) -> Result<ControlThread, String> {
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("runtime control executor cannot start: {error}"))?;
        let handle = std::thread::Builder::new()
            .name("octopus-runtime-control".into())
            .spawn(move || {
                executor.block_on(async move {
                    tokio::select! {
                        () = self.serve(octopus_home, cancel, restart) => {}
                        () = stop.cancelled() => {}
                    }
                });
            })
            .map_err(|error| format!("runtime control thread cannot start: {error}"))?;
        Ok(ControlThread { shutdown, handle: Some(handle) })
    }

    async fn serve(
        self,
        octopus_home: PathBuf,
        cancel: CancellationToken,
        restart: Arc<AtomicBool>,
    ) {
        let listener = match tokio::net::UnixListener::from_std(self.listener) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!(
                    "octopus: runtime control socket '{}' cannot be served: {error}",
                    self.path.display()
                );
                return;
            }
        };
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    eprintln!(
                        "octopus: runtime control socket '{}' stopped accepting: {error}",
                        self.path.display()
                    );
                    return;
                }
            };
            let home = octopus_home.clone();
            let cancel = cancel.clone();
            let restart = restart.clone();
            let stopping = self.stopping.clone();
            let interface_failures = self.interface_failures.clone();
            tokio::spawn(async move {
                if let Err(error) = tokio::time::timeout(
                    CONNECTION_TIMEOUT,
                    answer(stream, &home, &cancel, &restart, &stopping, &interface_failures),
                )
                .await
                .map_err(|_| "control connection timed out".to_string())
                .and_then(|result| result)
                {
                    eprintln!("octopus: runtime control request failed: {error}");
                }
            });
        }
    }
}

pub struct ControlThread {
    shutdown: CancellationToken,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ControlThread {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                eprintln!("octopus: runtime control thread panicked");
            }
        }
    }
}

async fn answer(
    stream: tokio::net::UnixStream,
    octopus_home: &Path,
    cancel: &CancellationToken,
    restart: &AtomicBool,
    stopping: &std::sync::Mutex<Option<RuntimeControlRequest>>,
    interface_failures: &OnceLock<InterfaceFailures>,
) -> Result<(), String> {
    let peer = stream
        .peer_cred()
        .map_err(|error| format!("control peer cannot be identified: {error}"))?;
    let (reader, mut writer) = stream.into_split();
    let response = if peer.uid() != current_uid() {
        ResponseFrame::Refused {
            error: format!("control peer uid {} is not the runtime owner", peer.uid()),
        }
    } else {
        let mut line = String::new();
        tokio::io::BufReader::new(tokio::io::AsyncReadExt::take(reader, MAX_FRAME_BYTES))
            .read_line(&mut line)
            .await
            .map_err(|error| format!("control request cannot be read: {error}"))?;
        match serde_json::from_str::<RequestFrame>(&line) {
            Err(error) => ResponseFrame::Refused {
                error: format!("control request is not a strict request frame: {error}"),
            },
            Ok(frame) => {
                let (accepted, current) = {
                    let mut state = stopping
                        .lock()
                        .map_err(|_| "control state is poisoned".to_string())?;
                    let accepted = match (frame.request, *state) {
                        (RuntimeControlRequest::Status, _) | (_, Some(_)) => None,
                        (request, None) => {
                            *state = Some(request);
                            Some(request)
                        }
                    };
                    (accepted, *state)
                };
                if accepted == Some(RuntimeControlRequest::Restart) {
                    restart.store(true, Ordering::SeqCst);
                    eprintln!(
                        "octopus: restart requested through the control socket — stopping cleanly, then starting again"
                    );
                } else if accepted == Some(RuntimeControlRequest::Stop) {
                    eprintln!("octopus: stop requested through the control socket — stopping cleanly");
                }
                let status = read_status(octopus_home, current, interface_failures.get());
                if accepted.is_some() {
                    cancel.cancel();
                }
                match status {
                    Ok(status) => ResponseFrame::Status(status),
                    Err(error) => ResponseFrame::Refused { error },
                }
            }
        }
    };
    let mut bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| format!("control response cannot be written: {error}"))?;
    writer.shutdown().await.map_err(|error| error.to_string())
}

fn read_status(
    octopus_home: &Path,
    stopping: Option<RuntimeControlRequest>,
    interface_failures: Option<&InterfaceFailures>,
) -> Result<RuntimeControlStatus, String> {
    let record = crate::runtime_instance::read_current(octopus_home).map_err(|error| error.to_string())?;
    Ok(RuntimeControlStatus {
        pid: record.pid,
        phase: record.phase,
        started_at_unix_ms: u64::try_from(record.started_at_unix_ms)
            .map_err(|_| "runtime start time exceeds the control frame range".to_string())?,
        stopping,
        interface_failures: interface_failures
            .map(|failures| failures.borrow().iter().map(|(interface, error)| RuntimeInterfaceFailure {
                interface: interface.as_str().to_string(),
                error: error.clone(),
            }).collect())
            .unwrap_or_default(),
    })
}

fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        rustix::process::geteuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

pub enum Answer {
    Status(RuntimeControlStatus),
    Unreachable(std::io::Error),
}

pub fn request(octopus_home: &Path, request: RuntimeControlRequest) -> Result<Answer, String> {
    #[cfg(unix)]
    {
        let path = socket_path(octopus_home);
        let stream = match std::os::unix::net::UnixStream::connect(&path) {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(Answer::Unreachable(error));
            }
            Err(error) => {
                return Err(format!(
                    "runtime control socket '{}' cannot be reached: {error}",
                    path.display()
                ));
            }
        };
        stream
            .set_read_timeout(Some(CONNECTION_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(CONNECTION_TIMEOUT)))
            .map_err(|error| error.to_string())?;
        let mut frame = serde_json::to_vec(&RequestFrame { request }).map_err(|error| error.to_string())?;
        frame.push(b'\n');
        (&stream)
            .write_all(&frame)
            .map_err(|error| format!("runtime control request cannot be sent: {error}"))?;
        let mut line = String::new();
        std::io::Read::take(std::io::BufReader::new(&stream), MAX_RESPONSE_BYTES)
            .read_line(&mut line)
            .map_err(|error| format!("runtime control response cannot be read: {error}"))?;
        match serde_json::from_str::<ResponseFrame>(&line)
            .map_err(|error| format!("runtime control response is malformed: {error}"))?
        {
            ResponseFrame::Status(status) => Ok(Answer::Status(status)),
            ResponseFrame::Refused { error } => Err(format!("runtime refused the control request: {error}")),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (octopus_home, request);
        Err("the runtime control socket requires Unix domain sockets".to_string())
    }
}

pub fn await_departure(octopus_home: &Path, deadline: Instant) -> Result<(), String> {
    loop {
        match request(octopus_home, RuntimeControlRequest::Status) {
            Ok(Answer::Unreachable(_)) => return Ok(()),
            Ok(Answer::Status(_)) | Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(Answer::Status(status)) => {
                return Err(format!(
                    "octopus pid {} is still answering in phase '{}'",
                    status.pid, status.phase
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn await_successor(
    octopus_home: &Path,
    predecessor_started_at_unix_ms: u64,
    deadline: Instant,
    mut progress: impl FnMut(&RuntimeControlStatus),
) -> Result<RuntimeControlStatus, String> {
    let mut last_phase = String::new();
    loop {
        match request(octopus_home, RuntimeControlRequest::Status) {
            Ok(Answer::Status(status))
                if status.started_at_unix_ms != predecessor_started_at_unix_ms =>
            {
                if status.phase != last_phase {
                    last_phase = status.phase.clone();
                    progress(&status);
                }
                if status.phase == "ready" && status.stopping.is_none() {
                    return Ok(status);
                }
            }
            Ok(_) | Err(_) if Instant::now() < deadline => {}
            Ok(Answer::Status(status)) => {
                return Err(format!(
                    "octopus pid {} has not started again; it answers in phase '{}'",
                    status.pid, status.phase
                ));
            }
            Ok(Answer::Unreachable(error)) => {
                return Err(format!("no Octopus runtime answered after the restart: {error}"));
            }
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the restarted runtime did not become ready in time; last phase '{}'",
                if last_phase.is_empty() { "none" } else { &last_phase }
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn control_socket_path_is_the_right_to_stop_and_restart_the_instance() {
        let temporary = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temporary.path()).unwrap();
        let instance = crate::runtime_instance::RuntimeInstance::acquire(&home).unwrap();
        let control = RuntimeControl::bind(&home).unwrap();
        assert_eq!(control.path(), socket_path(&home));
        let (failures, observed) = tokio::sync::watch::channel(std::collections::BTreeMap::new());
        control.interface_failures().set(observed).unwrap();
        let cancel = CancellationToken::new();
        let restart = Arc::new(AtomicBool::new(false));
        let server = control.serve_on_own_thread(home.clone(), cancel.clone(), restart.clone()).unwrap();
        let ask = |request| super::request(&home, request);
        let Answer::Status(status) = ask(RuntimeControlRequest::Status).unwrap() else {
            panic!("a live instance answers its control socket");
        };
        assert_eq!((status.pid, status.phase.as_str(), status.stopping), (std::process::id(), "starting", None));
        assert!(status.interface_failures.is_empty());
        failures.send_modify(|values| {
            values.insert(octopus_descriptors::InterfaceId::new("provider"), "No space left on device".to_string());
        });
        let Answer::Status(failed) = ask(RuntimeControlRequest::Status).unwrap() else {
            panic!("life failures remain observable without writing the ledger");
        };
        assert_eq!(failed.interface_failures, vec![RuntimeInterfaceFailure {
            interface: "provider".into(), error: "No space left on device".into(),
        }]);
        failures.send_modify(|values| values.clear());
        assert!(!cancel.is_cancelled());
        let Answer::Status(accepted) = ask(RuntimeControlRequest::Restart).unwrap() else {
            panic!("restart is answered");
        };
        assert_eq!(accepted.stopping, Some(RuntimeControlRequest::Restart));
        assert!(accepted.interface_failures.is_empty());
        assert!(cancel.is_cancelled() && restart.load(Ordering::SeqCst));
        let Answer::Status(repeated) = ask(RuntimeControlRequest::Stop).unwrap() else {
            panic!("a stopping instance still answers");
        };
        assert_eq!(repeated.stopping, Some(RuntimeControlRequest::Restart));
        drop(server);
        drop(instance);
        assert!(matches!(ask(RuntimeControlRequest::Status).unwrap(), Answer::Unreachable(_)));
        await_departure(&home, Instant::now()).unwrap();
    }
}
