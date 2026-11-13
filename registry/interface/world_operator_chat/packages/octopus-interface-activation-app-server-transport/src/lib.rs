pub mod application_consent;
pub use octopus_app_server_interface::interface_control_socket_path_from_root;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use octopus_descriptors::InterfaceId;
use octopus_interface_transport::{
    ActivationSleep, ActivationStart, ActivationStartFuture, InterfaceActivationArrival,
    InterfaceHostIoLimits, send_arrival,
};
use octopus_interfaces::{DeliveryVerdict, RefusedReason};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

pub const TRANSPORT_REF: &str = "app_server_transport";

const CONTROL_SOCKET_DIR_NAME: &str = "app-server-control";
const CONTROL_SOCKET_FILE_NAME: &str = "app-server-control.sock";
const HAND_IN_ROOT_DIR_NAME: &str = "interface-hand-in";
const REPO_ROOT_SLOT: &str = "repo_root";
const APP_SERVER_CONTROL_ROOT_SLOT: &str = "app_server_control_root";
const IDLE_BEFORE_SLEEP: Duration = Duration::from_secs(120);

pub fn conventional_control_socket_path(octopus_home: &std::path::Path) -> PathBuf {
    octopus_home
        .join(CONTROL_SOCKET_DIR_NAME)
        .join(CONTROL_SOCKET_FILE_NAME)
}

pub fn conventional_hand_in_root_path(octopus_home: &std::path::Path) -> PathBuf {
    octopus_home.join(HAND_IN_ROOT_DIR_NAME)
}

pub fn interface_control_socket_path(
    octopus_home: &std::path::Path,
    interface: &InterfaceId,
) -> PathBuf {
    interface_control_socket_path_from_root(&octopus_home.join(CONTROL_SOCKET_DIR_NAME), interface)
}

struct ClientBundleLaunch {
    path: PathBuf,
    launcher: &'static str,
    flags: &'static [&'static str],
}

impl ClientBundleLaunch {
    fn prepare(repo_root: &std::path::Path, configured: &str) -> Result<Self, String> {
        let relative = std::path::Path::new(configured);
        if configured.trim().is_empty()
            || configured.trim() != configured
            || relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("open_client_bundle must be a normalized relative path".to_string());
        }
        let path = repo_root.join(relative);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "client bundle '{}' cannot be inspected: {error}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "client bundle '{}' is not a regular non-symlink directory",
                path.display()
            ));
        }
        #[cfg(target_os = "macos")]
        let (launcher, flags) = ("/usr/bin/open", &["-n"][..]);
        #[cfg(not(target_os = "macos"))]
        let (launcher, flags) = ("/usr/bin/xdg-open", &[][..]);
        let launcher_metadata = std::fs::metadata(launcher).map_err(|error| {
            format!("client bundle launcher '{launcher}' cannot be inspected: {error}")
        })?;
        if !launcher_metadata.is_file() {
            return Err(format!(
                "client bundle launcher '{launcher}' is not a regular file"
            ));
        }
        Ok(Self {
            path,
            launcher,
            flags,
        })
    }

    async fn launch(&self) -> Result<(), String> {
        let status = tokio::process::Command::new(self.launcher)
            .args(self.flags)
            .arg(&self.path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map_err(|error| {
                format!(
                    "client bundle '{}' could not be launched with '{}': {error}",
                    self.path.display(),
                    self.launcher
                )
            })?;
        if !status.success() {
            return Err(format!(
                "client bundle '{}' launcher '{}' exited with status {}",
                self.path.display(),
                self.launcher,
                status
            ));
        }
        Ok(())
    }
}

async fn stop_failed_transport(
    socket_path: &std::path::Path,
    accept: tokio::task::JoinHandle<()>,
    drain: tokio::task::JoinHandle<()>,
) -> Result<(), String> {
    accept.abort();
    drain.abort();
    let _ = accept.await;
    let _ = drain.await;
    match tokio::fs::remove_file(socket_path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed activation socket '{}' could not be removed: {error}",
            socket_path.display()
        )),
    }
}

fn prepare_client_bundle_launch(
    start: &ActivationStart,
    configured: Option<&str>,
) -> Result<Option<ClientBundleLaunch>, String> {
    let Some(configured) = configured else {
        return Ok(None);
    };
    let repo_root = start.root_slots.get(REPO_ROOT_SLOT).ok_or_else(|| {
        format!(
            "interface {} app_server_transport declares open_client_bundle but resolves no root slot '{}'",
            start.interface_id, REPO_ROOT_SLOT
        )
    })?;
    ClientBundleLaunch::prepare(repo_root, configured).map(Some)
}

async fn launch_client_bundle(launch: &ClientBundleLaunch) -> Result<(), String> {
    launch.launch().await
}

fn launch_error(interface: &InterfaceId, error: String, cleanup: Result<(), String>) -> String {
    match cleanup {
        Ok(()) => {
            format!("interface {interface} app_server_transport client launch failed: {error}")
        }
        Err(cleanup) => format!(
            "interface {interface} app_server_transport client launch failed: {error}; {cleanup}"
        ),
    }
}

pub fn activation_start(start: ActivationStart) -> ActivationStartFuture {
    Box::pin(async move {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Config {
            listen_env: String,
            #[serde(default)]
            bind_conventional_socket_when_env_unset: bool,
            #[serde(default)]
            bind_interface_socket_when_env_unset: bool,
            #[serde(default)]
            open_client_bundle: Option<String>,
        }

        let config: Config = serde_json::from_value(start.config.clone()).map_err(|e| {
            format!(
                "interface {} app_server_transport activation config is malformed: {e}",
                start.interface_id
            )
        })?;
        let listen: std::path::PathBuf = match std::env::var_os(&config.listen_env) {
            Some(listen) => std::path::PathBuf::from(listen),
            None if config.bind_conventional_socket_when_env_unset => start
                .root_slots
                .get(APP_SERVER_CONTROL_ROOT_SLOT)
                .ok_or_else(|| {
                    format!(
                        "interface {} app_server_transport requires root slot '{}'",
                        start.interface_id, APP_SERVER_CONTROL_ROOT_SLOT
                    )
                })?
                .join(CONTROL_SOCKET_FILE_NAME),
            None if config.bind_interface_socket_when_env_unset => {
                let control_root = start
                    .root_slots
                    .get(APP_SERVER_CONTROL_ROOT_SLOT)
                    .ok_or_else(|| {
                        format!(
                            "interface {} app_server_transport requires root slot '{}'",
                            start.interface_id, APP_SERVER_CONTROL_ROOT_SLOT
                        )
                    })?;
                interface_control_socket_path_from_root(control_root, &start.interface_id)
            }
            None => return Ok(Vec::new()),
        };
        let listen = if listen.is_absolute() {
            listen
        } else {
            std::path::absolute(&listen).map_err(|error| {
                format!(
                    "interface {} app_server_transport listen path '{}' cannot be made absolute: {error}",
                    start.interface_id,
                    listen.display()
                )
            })?
        };
        let socket_path = octopus_util_abspath::AbsolutePathBuf::from_absolute_path(listen.clone())
            .map_err(|error| {
                format!(
                    "interface {} app_server_transport listen path '{}' is invalid: {error}",
                    start.interface_id,
                    listen.display()
                )
            })?;
        let client_bundle =
            prepare_client_bundle_launch(&start, config.open_client_bundle.as_deref())?;
        let sleep = if client_bundle.is_some() {
            ActivationSleep::default()
        } else {
            start.sleep.clone()
        };
        let (accept, drain) = spawn_sleeping_transport_arrivals(
            socket_path,
            start.tx,
            start.interface_id.clone(),
            start.artifact_root,
            start.host_io_limits,
            start.cancel.clone(),
            sleep,
        )
        .await
        .map_err(|error| {
            format!(
                "interface {} app_server_transport could not start: {error}",
                start.interface_id
            )
        })?;
        if let Some(client_bundle) = client_bundle.as_ref()
            && let Err(error) = launch_client_bundle(client_bundle).await
        {
            let cleanup = stop_failed_transport(&listen, accept, drain).await;
            return Err(launch_error(&start.interface_id, error, cleanup));
        }
        tracing::info!(
            interface = %start.interface_id,
            socket = %listen.display(),
            "activated interface via app_server_transport"
        );
        Ok(vec![accept, drain])
    })
}

fn refused_frame(message: impl Into<String>) -> String {
    serde_json::to_string(&DeliveryVerdict::refused(
        RefusedReason::Malformed,
        serde_json::json!({ "message": message.into() }),
    ))
    .expect("delivery verdict is always serializable")
}

#[derive(Default)]
struct InFlightHandIns {
    entries: tokio::sync::Mutex<std::collections::HashMap<String, Arc<InFlightHandIn>>>,
}

struct InFlightHandIn {
    result: tokio::sync::Mutex<Option<Result<String, ()>>>,
    ready: tokio::sync::Notify,
}

impl InFlightHandIn {
    fn new() -> Self {
        Self {
            result: tokio::sync::Mutex::new(None),
            ready: tokio::sync::Notify::new(),
        }
    }

    async fn wait(&self) -> Result<String, ()> {
        loop {
            let ready = self.ready.notified();
            if let Some(result) = self.result.lock().await.clone() {
                return result;
            }
            ready.await;
        }
    }

    async fn complete(&self, result: Result<String, ()>) {
        *self.result.lock().await = Some(result);
        self.ready.notify_waiters();
    }
}

async fn handle_wire_frame_with_inflight(
    text: &str,
    tx: &tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    interface_id: &InterfaceId,
    artifact_root: &std::path::Path,
    host_io_limits: InterfaceHostIoLimits,
    in_flight: Arc<InFlightHandIns>,
    cancel: CancellationToken,
) -> Result<String, ()> {
    let artifact = match octopus_interfaces::InterfaceArtifactFile::store_manifest_bounded(
        artifact_root,
        text.as_bytes(),
        host_io_limits.max_manifest_bytes,
        host_io_limits.max_payload_bytes,
    ) {
        Ok(artifact) => artifact,
        Err(error) => return Ok(refused_frame(error.to_string())),
    };
    let payload_bytes = match artifact.read_payload_bytes_bounded(host_io_limits.max_payload_bytes)
    {
        Ok(payload_bytes) => payload_bytes,
        Err(error) => return Ok(refused_frame(error.to_string())),
    };
    let mut digest = Sha256::new();
    digest.update(u64::try_from(text.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(text.as_bytes());
    digest.update(
        u64::try_from(payload_bytes.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    digest.update(&payload_bytes);
    let key = format!("{:x}", digest.finalize());
    let (entry, owns_hand_in) = {
        let mut entries = in_flight.entries.lock().await;
        if let Some(entry) = entries.get(&key) {
            (entry.clone(), false)
        } else {
            let entry = Arc::new(InFlightHandIn::new());
            entries.insert(key.clone(), entry.clone());
            (entry, true)
        }
    };
    if owns_hand_in {
        let tx = tx.clone();
        let interface_id = interface_id.clone();
        let in_flight = in_flight.clone();
        let task_entry = entry.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let result = match std::panic::AssertUnwindSafe(hand_in_captured_artifact(
                &tx,
                &interface_id,
                artifact,
                cancel,
            ))
            .catch_unwind()
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    tracing::error!(interface = %interface_id, "captured artifact hand-in panicked");
                    Err(())
                }
            };
            task_entry.complete(result).await;
            let mut entries = in_flight.entries.lock().await;
            if entries
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &task_entry))
            {
                entries.remove(&key);
            }
        });
    }
    entry.wait().await
}

async fn hand_in_captured_artifact(
    tx: &tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    interface_id: &InterfaceId,
    artifact: octopus_interfaces::InterfaceArtifactFile,
    cancel: CancellationToken,
) -> Result<String, ()> {
    let hand_in = async move {
        let (verdict_tx, verdict_rx) = tokio::sync::oneshot::channel();
        if send_arrival(tx, interface_id, artifact, Vec::new(), Some(verdict_tx))
            .await
            .is_err()
        {
            return Err(());
        }
        let verdict = verdict_rx.await.map_err(|_| ())?;
        serde_json::to_string(&verdict).map_err(|_| ())
    };
    tokio::select! {
        _ = cancel.cancelled() => Err(()),
        result = hand_in => result,
    }
}

#[cfg(unix)]
mod socket {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message as WsMessage, protocol::WebSocketConfig};

    const CONTROL_SOCKET_MODE: u32 = 0o600;
    const SOCKET_DIR_MODE: u32 = 0o700;
    const SOCKET_DIR_PERMISSION_BITS: u32 = 0o777;

    pub async fn spawn_transport_arrivals(
        socket_path: octopus_util_abspath::AbsolutePathBuf,
        tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
        interface_id: InterfaceId,
        artifact_root: PathBuf,
        host_io_limits: InterfaceHostIoLimits,
        cancel: CancellationToken,
    ) -> Result<
        (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>),
        Box<dyn std::error::Error>,
    > {
        spawn_sleeping_transport_arrivals(
            socket_path,
            tx,
            interface_id,
            artifact_root,
            host_io_limits,
            cancel,
            ActivationSleep::default(),
        )
        .await
    }

    pub(crate) async fn spawn_sleeping_transport_arrivals(
        socket_path: octopus_util_abspath::AbsolutePathBuf,
        tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
        interface_id: InterfaceId,
        artifact_root: PathBuf,
        host_io_limits: InterfaceHostIoLimits,
        cancel: CancellationToken,
        sleep: ActivationSleep,
    ) -> Result<
        (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>),
        Box<dyn std::error::Error>,
    > {
        host_io_limits
            .validate()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let maximum_frame_bytes = usize::try_from(
            host_io_limits
                .max_control_frame_bytes
                .min(host_io_limits.max_manifest_bytes),
        )
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "app-server transport frame ceiling exceeds host address space",
            )
        })?;
        let listener = match octopus_interface_transport::take_inherited_listener(socket_path.as_path()) {
            Some(inherited) => tokio::net::UnixListener::from_std(inherited)?,
            None => {
                prepare_control_socket_path(socket_path.as_path()).await?;
                let listener = tokio::net::UnixListener::bind(socket_path.as_path())?;
                set_control_socket_permissions(socket_path.as_path()).await?;
                listener
            }
        };
        tracing::info!(
            socket_path = %socket_path.as_path().display(),
            "interface control socket listening"
        );
        let guard_path = socket_path.clone();
        let accept_cancel = cancel.clone();
        let in_flight = Arc::new(InFlightHandIns::default());
        let open = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let last_work = Arc::new(std::sync::Mutex::new(tokio::time::Instant::now()));
        let sleep_socket = socket_path.as_path().to_path_buf();
        let accept_handle = tokio::spawn(async move {
            loop {
                let idle_until = *last_work.lock().expect("last work lock") + super::IDLE_BEFORE_SLEEP;
                let stream = tokio::select! {
                    _ = accept_cancel.cancelled() => break,
                    _ = tokio::time::sleep_until(idle_until), if sleep.enabled() => {
                        let quiet = open.load(std::sync::atomic::Ordering::SeqCst) == 0
                            && in_flight.entries.lock().await.is_empty()
                            && last_work.lock().expect("last work lock").elapsed() >= super::IDLE_BEFORE_SLEEP;
                        if quiet && sleep.offer(sleep_socket.clone()).await {
                            return;
                        }
                        *last_work.lock().expect("last work lock") = tokio::time::Instant::now();
                        continue;
                    }
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _addr)) => stream,
                        Err(err) => {
                            tracing::warn!(error = %err, "control socket accept error");
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    },
                };
                open.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                *last_work.lock().expect("last work lock") = tokio::time::Instant::now();
                let connection = run_connection(
                    stream,
                    tx.clone(),
                    interface_id.clone(),
                    artifact_root.clone(),
                    host_io_limits,
                    maximum_frame_bytes,
                    in_flight.clone(),
                    accept_cancel.clone(),
                );
                let open = open.clone();
                let last_work = last_work.clone();
                tokio::spawn(async move {
                    connection.await;
                    *last_work.lock().expect("last work lock") = tokio::time::Instant::now();
                    open.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                });
            }
            tracing::info!("control socket acceptor shutting down");
        });
        let guard_handle = tokio::spawn(async move {
            cancel.cancelled().await;
            if let Err(err) = std::fs::remove_file(guard_path.as_path())
                && err.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(
                    socket_path = %guard_path.as_path().display(),
                    error = %err,
                    "failed to remove interface control socket file"
                );
            }
        });
        Ok((accept_handle, guard_handle))
    }

    async fn run_connection(
        stream: tokio::net::UnixStream,
        tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
        interface_id: InterfaceId,
        artifact_root: PathBuf,
        host_io_limits: InterfaceHostIoLimits,
        maximum_frame_bytes: usize,
        in_flight: Arc<InFlightHandIns>,
        cancel: CancellationToken,
    ) {
        let websocket_config = WebSocketConfig::default()
            .max_frame_size(Some(maximum_frame_bytes))
            .max_message_size(Some(maximum_frame_bytes));
        let mut ws =
            match tokio_tungstenite::accept_async_with_config(stream, Some(websocket_config)).await
            {
                Ok(ws) => ws,
                Err(err) => {
                    tracing::warn!(error = %err, "control socket websocket upgrade failed");
                    return;
                }
            };
        loop {
            let message = tokio::select! {
                _ = cancel.cancelled() => break,
                message = ws.next() => match message {
                    Some(Ok(message)) => message,
                    Some(Err(err)) => {
                        tracing::warn!(error = %err, "control socket receive error");
                        break;
                    }
                    None => break,
                },
            };
            let text = match message {
                WsMessage::Text(text) => text.to_string(),
                WsMessage::Ping(payload) => {
                    if ws.send(WsMessage::Pong(payload)).await.is_err() {
                        break;
                    }
                    continue;
                }
                WsMessage::Close(_) => break,
                WsMessage::Pong(_) | WsMessage::Frame(_) => continue,
                WsMessage::Binary(_) => {
                    tracing::warn!("dropping unsupported binary control-socket message");
                    continue;
                }
            };
            match handle_wire_frame_with_inflight(
                &text,
                &tx,
                &interface_id,
                &artifact_root,
                host_io_limits,
                in_flight.clone(),
                cancel.clone(),
            )
            .await
            {
                Ok(reply) => {
                    if ws.send(WsMessage::Text(reply.into())).await.is_err() {
                        break;
                    }
                }
                Err(()) => break,
            }
        }
    }

    async fn prepare_control_socket_path(socket_path: &std::path::Path) -> std::io::Result<()> {
        use std::io::ErrorKind;
        use std::os::unix::fs::FileTypeExt;
        use std::os::unix::fs::PermissionsExt;

        if let Some(parent) = socket_path.parent() {
            let mut dir_builder = tokio::fs::DirBuilder::new();
            dir_builder.mode(SOCKET_DIR_MODE);
            match dir_builder.create(parent).await {
                Ok(()) => {}
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    let metadata = tokio::fs::symlink_metadata(parent).await?;
                    if !metadata.is_dir() {
                        return Err(std::io::Error::new(
                            ErrorKind::AlreadyExists,
                            format!(
                                "socket directory path exists and is not a directory: {}",
                                parent.display()
                            ),
                        ));
                    }
                    if metadata.permissions().mode() & SOCKET_DIR_PERMISSION_BITS != SOCKET_DIR_MODE
                    {
                        tokio::fs::set_permissions(
                            parent,
                            std::fs::Permissions::from_mode(SOCKET_DIR_MODE),
                        )
                        .await?;
                    }
                }
                Err(err) => return Err(err),
            }
        }

        match tokio::net::UnixStream::connect(socket_path).await {
            Ok(_stream) => {
                return Err(std::io::Error::new(
                    ErrorKind::AddrInUse,
                    format!(
                        "interface control socket is already in use at {}",
                        socket_path.display()
                    ),
                ));
            }
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
            Err(err) if err.kind() == ErrorKind::ConnectionRefused => {}
            Err(err) => {
                if !socket_path.exists() {
                    return Ok(());
                }
                return Err(err);
            }
        }

        if !socket_path.try_exists()? {
            return Ok(());
        }
        let is_socket = tokio::fs::symlink_metadata(socket_path)
            .await?
            .file_type()
            .is_socket();
        if !is_socket {
            return Err(std::io::Error::new(
                ErrorKind::AlreadyExists,
                format!(
                    "interface control socket path exists and is not a socket: {}",
                    socket_path.display()
                ),
            ));
        }
        tokio::fs::remove_file(socket_path).await
    }

    async fn set_control_socket_permissions(socket_path: &std::path::Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(
            socket_path,
            std::fs::Permissions::from_mode(CONTROL_SOCKET_MODE),
        )
        .await
    }
}

#[cfg(unix)]
pub use socket::spawn_transport_arrivals;
#[cfg(unix)]
use socket::spawn_sleeping_transport_arrivals;

#[cfg(not(unix))]
async fn spawn_sleeping_transport_arrivals(
    socket_path: octopus_util_abspath::AbsolutePathBuf,
    tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    interface_id: InterfaceId,
    artifact_root: PathBuf,
    host_io_limits: InterfaceHostIoLimits,
    cancel: CancellationToken,
    _sleep: ActivationSleep,
) -> Result<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>> {
    spawn_transport_arrivals(socket_path, tx, interface_id, artifact_root, host_io_limits, cancel).await
}

#[cfg(not(unix))]
pub async fn spawn_transport_arrivals(
    _socket_path: octopus_util_abspath::AbsolutePathBuf,
    _tx: tokio::sync::mpsc::Sender<InterfaceActivationArrival>,
    _interface_id: InterfaceId,
    _artifact_root: PathBuf,
    _host_io_limits: InterfaceHostIoLimits,
    _cancel: CancellationToken,
) -> Result<(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>>
{
    Err("the app_server_transport activation source requires unix domain sockets".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_limits() -> InterfaceHostIoLimits {
        InterfaceHostIoLimits {
            max_control_frame_bytes: 1024,
            max_stderr_bytes: 1024,
            max_manifest_bytes: 1024,
            max_payload_bytes: 1024,
        }
    }

    fn start_for(
        octopus_home: &std::path::Path,
        config: serde_json::Value,
        cancel: &CancellationToken,
    ) -> ActivationStart {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        ActivationStart {
            interface_id: InterfaceId::new("world_operator_chat"),
            config,
            host_io_limits: test_limits(),
            artifact_root: octopus_home.join("interface-artifacts"),
            tx,
            cancel: cancel.clone(),
            interface_var_dir: octopus_home.join("interfaces/world_operator_chat"),
            root_slots: std::collections::BTreeMap::from([
                (
                    APP_SERVER_CONTROL_ROOT_SLOT.to_string(),
                    octopus_home.join(CONTROL_SOCKET_DIR_NAME),
                ),
                (REPO_ROOT_SLOT.to_string(), octopus_home.to_path_buf()),
            ]),
            sleep: ActivationSleep::default(),
        }
    }

    fn unset_env_name(tag: &str) -> String {
        let name = format!("OCTOPUS_TEST_UNSET_LISTEN_{tag}_{}", std::process::id());
        assert!(std::env::var_os(&name).is_none());
        name
    }

    #[tokio::test]
    async fn env_unset_with_opt_in_binds_the_conventional_socket() {
        let home = tempfile::tempdir().expect("temp octopus home");
        let cancel = CancellationToken::new();
        let config = serde_json::json!({
            "listen_env": unset_env_name("OPTIN"),
            "bind_conventional_socket_when_env_unset": true,
        });
        let tasks = activation_start(start_for(home.path(), config, &cancel))
            .await
            .expect("the default-bind activation starts");
        assert!(
            !tasks.is_empty(),
            "the conventional default must START the transport, not stay dormant"
        );
        let socket = conventional_control_socket_path(home.path());
        assert!(
            socket.exists(),
            "the CONVENTIONAL control socket must be bound at {} (the terminal \
             client's daemon_control_socket_path)",
            socket.display()
        );
        tokio::net::UnixStream::connect(&socket)
            .await
            .expect("the conventional socket accepts a client connection");
        cancel.cancel();
        for task in tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn env_unset_without_opt_in_stays_dormant() {
        let home = tempfile::tempdir().expect("temp octopus home");
        let cancel = CancellationToken::new();
        let config = serde_json::json!({ "listen_env": unset_env_name("DORMANT") });
        let tasks = activation_start(start_for(home.path(), config, &cancel))
            .await
            .expect("a dormant env-only activation is not an error");
        assert!(tasks.is_empty(), "no env + no opt-in = dormant, no tasks");
        assert!(
            !conventional_control_socket_path(home.path()).exists(),
            "a dormant source must not bind the conventional socket"
        );
    }

    #[tokio::test]
    async fn interface_bind_uses_the_short_control_root_coordinate() {
        let home = tempfile::tempdir_in("/tmp").expect("temp octopus home");
        let cancel = CancellationToken::new();
        let config = serde_json::json!({
            "listen_env": unset_env_name("INTERFACE"),
            "bind_interface_socket_when_env_unset": true,
        });
        let tasks = activation_start(start_for(home.path(), config, &cancel))
            .await
            .expect("the interface bind activation starts");
        let socket =
            interface_control_socket_path(home.path(), &InterfaceId::new("world_operator_chat"));

        assert!(socket.exists());
        assert_eq!(
            socket.parent(),
            Some(home.path().join(CONTROL_SOCKET_DIR_NAME).as_path())
        );
        tokio::net::UnixStream::connect(&socket)
            .await
            .expect("the interface socket accepts a client connection");
        cancel.cancel();
        for task in tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn declared_missing_client_bundle_refuses_before_socket_activation() {
        let home = tempfile::tempdir_in("/tmp").expect("temp octopus home");
        let cancel = CancellationToken::new();
        let config = serde_json::json!({
            "listen_env": unset_env_name("MISSING_BUNDLE"),
            "bind_interface_socket_when_env_unset": true,
            "open_client_bundle": "app/build/Octopus.app"
        });
        let error = activation_start(start_for(home.path(), config, &cancel))
            .await
            .expect_err("a declared missing client bundle must fail closed");
        assert!(error.contains("cannot be inspected"));
        assert!(
            !interface_control_socket_path(home.path(), &InterfaceId::new("world_operator_chat"))
                .exists()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn client_bundle_launch_refuses_a_nonzero_launcher_status() {
        let bundle = tempfile::tempdir_in("/tmp").expect("temp client bundle");
        let launch = ClientBundleLaunch {
            path: bundle.path().to_path_buf(),
            launcher: "/usr/bin/false",
            flags: &[],
        };
        let error = launch
            .launch()
            .await
            .expect_err("a nonzero launcher status must fail closed");
        assert!(error.contains("exited with status"));
    }

    #[tokio::test]
    async fn hand_in_captures_one_bounded_carrier_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let payload = root.path().join("source.payload");
        std::fs::write(&payload, b"before").unwrap();
        let frame = serde_json::json!({
            "label": "terminal.input",
            "payload_path": payload,
        })
        .to_string();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let interface = InterfaceId::new("world_operator_chat");
        let in_flight = Arc::new(InFlightHandIns::default());

        let handler = handle_wire_frame_with_inflight(
            &frame,
            &tx,
            &interface,
            root.path(),
            test_limits(),
            in_flight,
            CancellationToken::new(),
        );
        let receiver = async {
            let arrival = rx.recv().await.unwrap();
            std::fs::write(&payload, b"after!").unwrap();
            assert_eq!(arrival.artifact.read_payload_bytes().unwrap(), b"before");
            arrival
                .verdict_tx
                .unwrap()
                .send(DeliveryVerdict::Admitted { seq: 7 })
                .unwrap();
        };
        let (reply, ()) = tokio::join!(handler, receiver);
        assert_eq!(
            serde_json::from_str::<DeliveryVerdict>(&reply.unwrap()).unwrap(),
            DeliveryVerdict::Admitted { seq: 7 }
        );
    }

    #[tokio::test]
    async fn concurrent_identical_hand_ins_share_one_arrival_and_verdict() {
        let root = tempfile::tempdir().unwrap();
        let payload = root.path().join("source.payload");
        std::fs::write(&payload, b"same").unwrap();
        let frame = serde_json::json!({
            "label": "terminal.input",
            "payload_path": payload,
        })
        .to_string();
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let interface = InterfaceId::new("world_operator_chat");
        let in_flight = Arc::new(InFlightHandIns::default());
        let first = handle_wire_frame_with_inflight(
            &frame,
            &tx,
            &interface,
            root.path(),
            test_limits(),
            in_flight.clone(),
            CancellationToken::new(),
        );
        let second = handle_wire_frame_with_inflight(
            &frame,
            &tx,
            &interface,
            root.path(),
            test_limits(),
            in_flight,
            CancellationToken::new(),
        );
        let receiver = async {
            let arrival = rx.recv().await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), rx.recv())
                    .await
                    .is_err()
            );
            arrival
                .verdict_tx
                .unwrap()
                .send(DeliveryVerdict::Admitted { seq: 9 })
                .unwrap();
        };
        let (first, second, ()) = tokio::join!(first, second, receiver);
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first, second);
        assert_eq!(
            serde_json::from_str::<DeliveryVerdict>(&first).unwrap(),
            DeliveryVerdict::Admitted { seq: 9 }
        );
    }

    #[tokio::test]
    async fn activation_cancellation_finishes_all_identical_hand_in_waiters() {
        let root = tempfile::tempdir().unwrap();
        let payload = root.path().join("source.payload");
        std::fs::write(&payload, b"same").unwrap();
        let frame = serde_json::json!({
            "label": "terminal.input",
            "payload_path": payload,
        })
        .to_string();
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let interface = InterfaceId::new("world_operator_chat");
        let in_flight = Arc::new(InFlightHandIns::default());
        let cancel = CancellationToken::new();
        let spawn_waiter = |frame: String| {
            let tx = tx.clone();
            let interface = interface.clone();
            let artifact_root = root.path().to_path_buf();
            let in_flight = in_flight.clone();
            let cancel = cancel.clone();
            tokio::spawn(async move {
                handle_wire_frame_with_inflight(
                    &frame,
                    &tx,
                    &interface,
                    &artifact_root,
                    test_limits(),
                    in_flight,
                    cancel,
                )
                .await
            })
        };
        let first = spawn_waiter(frame.clone());
        let arrival = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let second = spawn_waiter(frame);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), rx.recv())
                .await
                .is_err()
        );
        cancel.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), first)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), second)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(in_flight.entries.lock().await.is_empty());
        drop(arrival);
    }

    #[tokio::test]
    async fn identical_manifests_with_different_captured_payloads_do_not_share_a_verdict() {
        let root = tempfile::tempdir().unwrap();
        let payload = root.path().join("source.payload");
        let artifact_root = root.path().join("artifacts");
        std::fs::write(&payload, b"first").unwrap();
        let frame = serde_json::json!({
            "label": "terminal.input",
            "payload_path": payload,
        })
        .to_string();
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let interface = InterfaceId::new("world_operator_chat");
        let in_flight = Arc::new(InFlightHandIns::default());
        let first = tokio::spawn({
            let tx = tx.clone();
            let frame = frame.clone();
            let interface = interface.clone();
            let artifact_root = artifact_root.clone();
            let in_flight = in_flight.clone();
            async move {
                handle_wire_frame_with_inflight(
                    &frame,
                    &tx,
                    &interface,
                    &artifact_root,
                    test_limits(),
                    in_flight,
                    CancellationToken::new(),
                )
                .await
            }
        });
        let first_arrival = rx.recv().await.unwrap();
        assert_eq!(
            first_arrival.artifact.read_payload_bytes().unwrap(),
            b"first"
        );
        std::fs::write(&payload, b"second").unwrap();
        let second = tokio::spawn({
            let tx = tx.clone();
            let frame = frame.clone();
            let interface = interface.clone();
            let artifact_root = artifact_root.clone();
            let in_flight = in_flight.clone();
            async move {
                handle_wire_frame_with_inflight(
                    &frame,
                    &tx,
                    &interface,
                    &artifact_root,
                    test_limits(),
                    in_flight,
                    CancellationToken::new(),
                )
                .await
            }
        });
        let second_arrival = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            second_arrival.artifact.read_payload_bytes().unwrap(),
            b"second"
        );
        first_arrival
            .verdict_tx
            .unwrap()
            .send(DeliveryVerdict::Admitted { seq: 10 })
            .unwrap();
        second_arrival
            .verdict_tx
            .unwrap()
            .send(DeliveryVerdict::Admitted { seq: 11 })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<DeliveryVerdict>(&first.await.unwrap().unwrap()).unwrap(),
            DeliveryVerdict::Admitted { seq: 10 }
        );
        assert_eq!(
            serde_json::from_str::<DeliveryVerdict>(&second.await.unwrap().unwrap()).unwrap(),
            DeliveryVerdict::Admitted { seq: 11 }
        );
    }

    #[tokio::test]
    async fn concurrent_sources_admit_stop_before_the_prior_authoring_verdict() {
        let root = tempfile::tempdir().unwrap();
        let authoring_payload = root.path().join("authoring.payload");
        let stop_payload = root.path().join("stop.payload");
        std::fs::write(&authoring_payload, b"authoring").unwrap();
        std::fs::write(&stop_payload, b"stop").unwrap();
        let authoring_frame = serde_json::json!({
            "label": "test.authoring",
            "payload_path": authoring_payload,
        })
        .to_string();
        let stop_frame = serde_json::json!({
            "label": "test.stop",
            "payload_path": stop_payload,
        })
        .to_string();
        let (tx, mut rx) = tokio::sync::mpsc::channel(2);
        let interface = InterfaceId::new("system_test_authoring");
        let artifact_root = root.path().join("artifacts");
        let in_flight = Arc::new(InFlightHandIns::default());
        let authoring = tokio::spawn({
            let tx = tx.clone();
            let interface = interface.clone();
            let artifact_root = artifact_root.clone();
            let in_flight = in_flight.clone();
            async move {
                handle_wire_frame_with_inflight(
                    &authoring_frame,
                    &tx,
                    &interface,
                    &artifact_root,
                    test_limits(),
                    in_flight,
                    CancellationToken::new(),
                )
                .await
            }
        });
        let authoring_arrival = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let stop = tokio::spawn({
            let interface = interface.clone();
            let artifact_root = artifact_root.clone();
            let in_flight = in_flight.clone();
            async move {
                handle_wire_frame_with_inflight(
                    &stop_frame,
                    &tx,
                    &interface,
                    &artifact_root,
                    test_limits(),
                    in_flight,
                    CancellationToken::new(),
                )
                .await
            }
        });
        let stop_arrival = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stop_arrival.artifact.read_manifest().unwrap().label,
            "test.stop"
        );
        stop_arrival
            .verdict_tx
            .unwrap()
            .send(DeliveryVerdict::Admitted { seq: 21973 })
            .unwrap();
        let stop_reply = tokio::time::timeout(std::time::Duration::from_secs(1), stop)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<DeliveryVerdict>(&stop_reply).unwrap(),
            DeliveryVerdict::Admitted { seq: 21973 }
        );
        assert!(!authoring.is_finished());
        authoring_arrival
            .verdict_tx
            .unwrap()
            .send(DeliveryVerdict::Admitted { seq: 21971 })
            .unwrap();
        assert!(authoring.await.unwrap().is_ok());
    }
}
