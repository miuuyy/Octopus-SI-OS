#![cfg(unix)]

use std::os::fd::IntoRawFd;
use std::time::Duration;

use futures::SinkExt;
use octopus_descriptors::InterfaceId;
use octopus_interface_activation_app_server_transport::spawn_transport_arrivals;
use octopus_interface_transport::{INHERITED_SOCKETS_ENV, InterfaceHostIoLimits};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn a_woken_transport_serves_the_caller_the_host_carried_over() {
    let root = tempfile::Builder::new().prefix("oct").tempdir_in("/tmp").unwrap();
    let socket = root.path().join("c.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let caller = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    let (carried, _) = listener.accept().unwrap();
    let inherited = serde_json::Map::from_iter([(
        socket.to_str().unwrap().to_string(),
        serde_json::json!({
            "listener": listener.into_raw_fd(),
            "connections": [carried.into_raw_fd()],
        }),
    )]);
    unsafe {
        std::env::set_var(INHERITED_SOCKETS_ENV, serde_json::Value::Object(inherited).to_string());
    }
    let payload = root.path().join("source.payload");
    std::fs::write(&payload, b"word").unwrap();
    let (tx, mut arrivals) = tokio::sync::mpsc::channel(1);
    let cancel = CancellationToken::new();
    let (acceptor, guard) = spawn_transport_arrivals(
        octopus_util_abspath::AbsolutePathBuf::from_absolute_path(&socket).unwrap(),
        tx,
        InterfaceId::new("world_operator_chat"),
        root.path().join("artifacts"),
        InterfaceHostIoLimits {
            max_control_frame_bytes: 1024,
            max_stderr_bytes: 1024,
            max_manifest_bytes: 1024,
            max_payload_bytes: 1024,
        },
        cancel.clone(),
    )
    .await
    .unwrap();
    caller.set_nonblocking(true).unwrap();
    let caller = tokio::net::UnixStream::from_std(caller).unwrap();
    let (mut caller, _) = tokio_tungstenite::client_async("ws://localhost/", caller).await.unwrap();
    caller
        .send(Message::Text(
            serde_json::json!({"label": "terminal.input", "payload_path": payload}).to_string().into(),
        ))
        .await
        .unwrap();
    let arrival = tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
        .await
        .expect("the carried caller's hand-in reaches the woken life");
    assert!(arrival.is_some());
    cancel.cancel();
    acceptor.await.unwrap();
    guard.await.unwrap();
}
