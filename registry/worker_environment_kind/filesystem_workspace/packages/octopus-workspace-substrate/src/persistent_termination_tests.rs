use super::*;
use octopus_exec::{ExecProcessFuture, ExecServerError, ReadResponse, SignalResponse, TerminateResponse, WriteResponse};
use std::sync::atomic::AtomicUsize;

struct FirstStopFails {
    inner: Arc<dyn ExecProcess>,
    attempts: AtomicUsize,
    denied: bool,
}

impl ExecProcess for FirstStopFails {
    fn process_id(&self) -> &ProcessId { self.inner.process_id() }
    fn subscribe_wake(&self) -> tokio::sync::watch::Receiver<u64> { self.inner.subscribe_wake() }
    fn subscribe_events(&self) -> ExecProcessEventReceiver { self.inner.subscribe_events() }
    fn read(&self, seq: Option<u64>, max: Option<usize>, wait: Option<u64>) -> ExecProcessFuture<'_, ReadResponse> {
        self.inner.read(seq, max, wait)
    }
    fn write(&self, chunk: Vec<u8>) -> ExecProcessFuture<'_, WriteResponse> { self.inner.write(chunk) }
    fn signal(&self, signal: ProcessSignal) -> ExecProcessFuture<'_, SignalResponse> { self.inner.signal(signal) }
    fn terminate(&self) -> ExecProcessFuture<'_, TerminateResponse> {
        Box::pin(async move {
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                if self.denied {
                    Err(ExecServerError::Protocol(std::io::Error::from_raw_os_error(1).to_string()))
                } else {
                    Ok(TerminateResponse { running: true })
                }
            } else {
                self.inner.terminate().await
            }
        })
    }
}

#[tokio::test]
async fn failed_timeout_and_explicit_stop_preserve_evidence_and_per_call_retry() {
    for (denied, timer) in [(true, false), (true, true), (false, true)] {
        let temp = tempfile::tempdir().unwrap();
        let cwd = AbsPath::new(temp.path()).unwrap();
        let process = start_exec_process(
            "coder-process-0123456789abcdef", vec!["/bin/sleep".into(), "10".into()],
            &cwd, BTreeMap::new(), false,
        ).await.unwrap();
        let injected = Arc::new(FirstStopFails { inner: process, attempts: AtomicUsize::new(0), denied });
        let events = injected.subscribe_events();
        let managed = Arc::new(ManagedProcess {
            process: injected.clone(), state: tokio::sync::Mutex::new(ManagedProcessState::new(4096, 64)),
            notify: tokio::sync::Notify::new(),
        });
        let activity = Arc::new(tokio::sync::Notify::new());
        spawn_event_collector(managed.clone(), events, activity.clone());
        let spec = KeeperSpec {
            schema_version: KEEPER_SPEC_SCHEMA_VERSION, worker_environment_id: "test".into(),
            process_id: injected.process_id().to_string(), start_call: None, argv: Vec::new(),
            cwd: temp.path().into(), env: BTreeMap::new(), tty: false, socket: temp.path().join("socket"),
            max_buffered_output_bytes: 4096, max_buffered_output_events: 64, max_input_bytes: 64,
            process_timeout_ms: 1, cleanup_timeout_ms: 1000,
        };
        let keeper = Keeper {
            spec_path: temp.path().join("process.json"), spec, managed: managed.clone(), activity: activity.clone(),
            requests: tokio::sync::Mutex::new(()), delivered: AtomicBool::new(false), released: AtomicBool::new(false),
            closed_at: std::sync::Mutex::new(None), stop: tokio::sync::Notify::new(),
        };
        let stop = |call: &str| KeeperRequest::Terminate { call: Some(call.into()), wait_ms: 0, max_output_bytes: 1024 };
        let first = if !timer {
            keeper.respond(stop("denied-stop")).await
        } else {
            spawn_process_timeout(managed.clone(), Duration::from_millis(1), Duration::from_millis(50), activity.clone());
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let notified = activity.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if managed.state.lock().await.termination_failure.is_some() { break; }
                    notified.await;
                }
            }).await.unwrap();
            keeper.respond(KeeperRequest::Poll { call: Some("timed-out".into()), input: String::new(), wait_ms: 0, max_output_bytes: 1024 }).await
        };
        let KeeperResponse::Poll(failed) = &first else { panic!("expected process evidence"); };
        assert!(failed.running && !failed.exited && !failed.closed);
        assert_eq!(failed.timed_out, timer);
        assert!(failed.failure.as_ref().unwrap().contains(if denied { "os error 1" } else { "within 50 ms" }));
        if !timer {
            assert_eq!(serde_json::to_value(keeper.respond(stop("denied-stop")).await).unwrap(), serde_json::to_value(&first).unwrap());
            assert_eq!(injected.attempts.load(Ordering::SeqCst), 1);
        }
        let KeeperResponse::Poll(retried) = keeper.respond(stop("retry-stop")).await else { panic!("expected closure"); };
        assert!(!retried.running && retried.exited && retried.closed);
        assert!(retried.exit_code.is_some());
        assert!(retried.failure.is_none());
        assert_eq!(injected.attempts.load(Ordering::SeqCst), 2);
        if !timer {
            assert_eq!(serde_json::to_value(keeper.respond(stop("denied-stop")).await).unwrap(), serde_json::to_value(first).unwrap());
        }
    }
}
