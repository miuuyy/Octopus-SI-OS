#[tokio::test]
async fn shutdown_keeps_the_event_consumer_until_the_transport_has_closed() {
    let (command_tx, mut command_rx) = mpsc::channel(1);
    let (event_tx, event_rx) = crate::app_server_event_channel();
    let joined = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished = joined.clone();
    let worker_handle = tokio::spawn(async move {
        let Some(SpawnClientCommand::Shutdown { response_tx }) = command_rx.recv().await else {
            panic!("shutdown command was not delivered");
        };
        deliver_event(&event_tx, AppServerEvent::Disconnected { message: "transport closing".into() })
            .expect("shutdown must not tear down the event consumer before the worker");
        response_tx.send(Ok(())).unwrap();
        tokio::task::yield_now().await;
        finished.store(true, std::sync::atomic::Ordering::Release);
    });
    let client = SpawnAppServerClient {
        command_tx, event_rx, pending_events: VecDeque::new(), server_version: None,
        compatibility: None, codex_home: None, worker_handle,
    };
    client.shutdown().await.unwrap();
    assert!(joined.load(std::sync::atomic::Ordering::Acquire));
}
