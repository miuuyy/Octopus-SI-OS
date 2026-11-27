use super::*;

#[tokio::test]
async fn provider_bridge_preserves_retained_delivery_and_reply_contract() {
    let root = tempfile::tempdir().unwrap();
    let (arrivals, mut incoming) = tokio::sync::mpsc::channel(1);
    let ctx = InterfaceLiftContext::unprivileged(
        InterfaceId::new("provider"), root.path().join("interface"), None,
        root.path().to_path_buf(), root.path().join("movements.jsonl"),
        root.path().join("blobs"), root.path().join("index.sqlite"), BTreeMap::new(),
        arrivals, CancellationToken::new(),
    ).with_host_io_limits(InterfaceHostIoLimits {
        max_control_frame_bytes: 65536, max_stderr_bytes: 65536,
        max_manifest_bytes: 65536, max_payload_bytes: 65536,
    }).unwrap();
    let artifact = InterfaceArtifactFile::write(root.path(), "provider.output", b"payload").unwrap();
    let pending: PendingRequests = PendingRequests::default();
    let mut next = 1;
    for expected in [
        Ok(octopus_ledger::ArtifactDelivery::Pending { scope_seq: 5 }),
        Ok(octopus_ledger::ArtifactDelivery::Complete { verdict: DeliveryVerdict::Admitted { seq: 6 } }),
        Err("exact scope refused".to_string()),
    ] {
        let caller = ctx.hand_arrival_retained(artifact.clone(), Vec::new(), "provider-original-output");
        let host = async {
            let output = spawned_arrival_output(incoming.recv().await.unwrap(), &pending, &mut next).await.unwrap();
            let frame = serde_json::to_value(output).unwrap();
            assert_eq!(frame["kind"], "retained_arrival");
            assert_eq!(frame["delivery_id"], "provider-original-output");
            assert_eq!(frame["artifact_manifest_path"], artifact.manifest_path().to_str().unwrap());
            let response = serde_json::json!({"kind":"retained_delivery", "request_id":frame["request_id"], "delivery":expected});
            accept_spawned_life_input(&pending, serde_json::from_value(response).unwrap()).await.unwrap();
        };
        let (result, ()) = tokio::join!(caller, host);
        assert_eq!(result, expected);
        assert!(pending.verdicts.lock().await.is_empty());
    }
    let caller = ctx.hand_arrival(artifact.clone(), Vec::new());
    let host = async {
        let frame = serde_json::to_value(spawned_arrival_output(incoming.recv().await.unwrap(), &pending, &mut next).await.unwrap()).unwrap();
        assert_eq!(frame["kind"], "arrival");
        accept_spawned_life_input(&pending, serde_json::from_value(serde_json::json!({
            "kind":"delivery_verdict", "request_id":frame["request_id"], "verdict":{"admitted":{"seq":7}}
        })).unwrap()).await.unwrap();
    };
    let (result, ()) = tokio::join!(caller, host);
    assert_eq!(result.unwrap(), DeliveryVerdict::Admitted { seq: 7 });
    let (reply, result) = tokio::sync::oneshot::channel();
    let request_id = register_spawned_reply(&pending, &mut next, SpawnedReply::Retained(reply)).await.unwrap();
    let wrong = SpawnedLifeInput::DeliveryVerdict { request_id, verdict: DeliveryVerdict::Admitted { seq: 8 } };
    assert!(accept_spawned_life_input(&pending, wrong).await.unwrap_err().contains("acknowledgement contract"));
    assert!(result.await.is_err());
    let (verdict_tx, _) = tokio::sync::oneshot::channel();
    let (reply, _) = tokio::sync::oneshot::channel();
    let invalid = InterfaceActivationArrival {
        artifact, interface: InterfaceId::new("provider"), evidence: Vec::new(), verdict_tx: Some(verdict_tx),
        retained: Some(octopus_interface_transport::RetainedInterfaceArrival { id: "original".into(), reply }),
    };
    assert!(spawned_arrival_output(invalid, &pending, &mut next).await.is_err());
    assert!(pending.verdicts.lock().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn ready_publication_precedes_synchronous_backlog() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Writer {
        bytes: Vec<u8>,
        flushed: Arc<AtomicBool>,
        receipt: Option<tokio::sync::oneshot::Sender<std::time::Instant>>,
        failure: Option<&'static str>,
    }
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.failure == Some("write") { return Err(std::io::Error::other("probe write refused")); }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.failure == Some("flush") { return Err(std::io::Error::other("probe flush refused")); }
            self.flushed.store(true, Ordering::SeqCst);
            if let Some(receipt) = self.receipt.take() { let _ = receipt.send(std::time::Instant::now()); }
            Ok(())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let channels = || {
        let (arrivals, arrival_rx) = tokio::sync::mpsc::channel(1);
        let (emissions, emission_rx) = tokio::sync::mpsc::channel(1);
        let ctx = InterfaceLiftContext::unprivileged(
            InterfaceId::new("ready-probe"), root.path().join("interface"), None,
            root.path().to_path_buf(), root.path().join("movements.jsonl"),
            root.path().join("blobs"), root.path().join("index.sqlite"), BTreeMap::new(),
            arrivals, CancellationToken::new(),
        ).with_system_emission_sender(emissions);
        (ctx, arrival_rx, emission_rx)
    };
    let (ctx, arrival_rx, emission_rx) = channels();
    let (ctx, readiness, publication) = ctx.with_readiness_publication_channel();
    let flushed = Arc::new(AtomicBool::new(false));
    let (receipt, published) = tokio::sync::oneshot::channel();
    let mut writer = Writer { bytes: Vec::new(), flushed: flushed.clone(), receipt: Some(receipt), failure: None };
    let (backlog_tx, backlog_rx) = tokio::sync::oneshot::channel();
    let started = std::time::Instant::now();
    let life = async move {
        ctx.signal_ready().await?;
        ctx.clone().signal_ready().await?;
        let published_before_backlog = flushed.load(Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(80));
        backlog_tx.send(published_before_backlog).unwrap();
        std::future::pending::<Result<(), String>>().await
    };
    let output = pump_spawned_life_outputs_to(arrival_rx, emission_rx, readiness, publication,
        PendingRequests::default(), 65536, &mut writer);
    let receipt = async { (published.await.unwrap(), backlog_rx.await.unwrap()) };
    let (published, published_before_backlog) = tokio::select! {
        biased;
        result = life => panic!("probe life unexpectedly finished: {result:?}"),
        result = output => panic!("probe output unexpectedly finished: {result:?}"),
        receipt = receipt => receipt,
        _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => panic!("readiness publication stalled"),
    };
    eprintln!("Ready flushed after {:?}; flushed before synchronous backlog: {published_before_backlog}", published.duration_since(started));
    assert!(published_before_backlog, "signal_ready returned before Ready was flushed");
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&writer.bytes).unwrap(), serde_json::json!({"kind":"ready"}));

    for failure in ["write", "flush", "bound"] {
        let (ctx, arrival_rx, emission_rx) = channels();
        let (ctx, readiness, publication) = ctx.with_readiness_publication_channel();
        let mut writer = Writer { bytes: Vec::new(), flushed: Arc::new(AtomicBool::new(false)), receipt: None, failure: Some(failure) };
        let mut ready = Box::pin(ctx.signal_ready());
        let output = pump_spawned_life_outputs_to(arrival_rx, emission_rx, readiness, publication,
            PendingRequests::default(), if failure == "bound" { 1 } else { 65536 }, &mut writer);
        let refused = tokio::select! {
            biased;
            result = &mut ready => panic!("readiness escaped a failed publisher: {result:?}"),
            result = output => result.unwrap_err(),
        };
        assert!(!writer.flushed.load(Ordering::SeqCst));
        let error = ready.await.unwrap_err();
        if failure == "bound" {
            assert!(refused.contains("exceeds"));
            assert!(error.contains("closed without confirmation"));
        } else {
            assert!(refused.contains(failure));
            assert_eq!(error, refused);
        }
    }
    for failure in ["cancel", "drop"] {
        let (ctx, _arrivals, _emissions) = channels();
        let (ctx, mut requested, publication) = ctx.with_readiness_publication_channel();
        let mut ready = Box::pin(ctx.signal_ready());
        tokio::select! {
            biased;
            result = &mut ready => panic!("readiness escaped before publication: {result:?}"),
            _ = async {} => {}
        }
        requested.try_recv().unwrap().unwrap();
        if failure == "cancel" { ctx.cancel.cancel(); } else { drop(publication); }
        let error = ready.await.unwrap_err();
        assert!(error.contains(if failure == "cancel" { "cancelled" } else { "closed without confirmation" }));
    }
}
