use super::*;

struct HeldInput {
    started: tokio::sync::Notify,
    cancelled: tokio::sync::Notify,
}
impl DesktopExecutor for HeldInput {
    fn execute<'a>(&'a self, action: Value) -> Execution<'a> {
        Box::pin(async move {
            let screen = json!({"id":"screen","x":0.0,"y":0.0,"width":100.0,"height":100.0,"pixel_width":100,"pixel_height":100});
            let mut result = json!({"permissions":{"screen_capture":true,"accessibility":true},"screens":[screen.clone()]});
            if action["type"] == "observe" {
                result["observation"] = json!({"id":"native","screen":screen,"focus":null,"input_revision":"one","captured_at_unix_ms":1,"image":null});
            } else if action["type"] == "click" {
                self.started.notify_one();
                while !Path::new(action["cancel_file"].as_str().unwrap()).exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
                self.cancelled.notify_one();
                result["completed"] = json!(false);
                result["events_posted"] = json!(1);
            }
            Ok(result)
        })
    }
}

#[tokio::test]
async fn caller_disconnect_cancels_only_its_input_and_records_partial_effect() {
    let root = tempfile::tempdir().unwrap();
    let executor = Arc::new(HeldInput {
        started: tokio::sync::Notify::new(),
        cancelled: tokio::sync::Notify::new(),
    });
    let preferences = root.path().canonicalize().unwrap().join("preferences"); std::fs::create_dir(&preferences).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&preferences, std::fs::Permissions::from_mode(0o700)).unwrap();
    crate::app_authorization::create_record(&preferences.join("computer-use.json"), &json!({"schema_version":1,"enabled":true})).unwrap();
    let host = Arc::new(DesktopHost::new(executor.clone(), root.path().join("private")).unwrap()
        .with_application_preferences(Some(preferences)).unwrap());
    let request = |id: &str, action| Request {
        schema_version: 2,
        worker_environment_id: "room".into(),
        request_id: id.into(),
        tool: "gui.click".into(),
        action,
    };
    let control = host
        .handle(
            request("control", json!({"type":"control","mode":"acquire"})),
            false,
        )
        .await
        .evidence
        .unwrap()
        .control
        .generation;
    let observation = host
        .handle(
            request("observe", json!({"type":"observe","screen_id":"screen"})),
            false,
        )
        .await
        .evidence
        .unwrap()
        .observation
        .unwrap()
        .id;
    let call = request(
        "input",
        json!({"type":"click","screen_id":"screen","observation_id":observation,"control_generation":control,"x":20,"y":30}),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (server, _) = listener.accept().await.unwrap();
    let server_host = host.clone();
    let serving = tokio::spawn(async move { connection(server, server_host, "test-secret").await });
    let bytes = serde_json::to_vec(&call).unwrap();
    client.write_all(format!("POST /v1/action HTTP/1.1\r\nAuthorization: Bearer test-secret\r\nContent-Length: {}\r\n\r\n",bytes.len()).as_bytes()).await.unwrap();
    client.write_all(&bytes).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        executor.started.notified(),
    )
    .await
    .unwrap();
    let mut other = call.clone();
    other.request_id = "other".into();
    host.cancel_disconnected_request(&other).await.unwrap();
    assert_eq!(host.state.lock().unwrap().control.generation, control);
    let started = std::time::Instant::now();
    drop(client);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        executor.cancelled.notified(),
    )
    .await
    .unwrap();
    assert!(serving.await.unwrap().unwrap_err().contains("disconnected"));
    assert_eq!(host.state.lock().unwrap().control.mode, "stopped");
    let key = blake3::hash(b"room:input").to_hex().to_string();
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(root.path().join("private").join(format!("{key}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["events_posted"], 1);
    assert_eq!(receipt["completed"], false);
    eprintln!(
        "Exact HTTP disconnect cancellation and partial receipt: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn computer_use_preference_denies_and_revokes_native_input() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let preferences = root.path().canonicalize().unwrap().join("preferences"); std::fs::create_dir(&preferences).unwrap();
    std::fs::set_permissions(&preferences, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executor = Arc::new(HeldInput { started: tokio::sync::Notify::new(), cancelled: tokio::sync::Notify::new() });
    let host = Arc::new(DesktopHost::new(executor.clone(), root.path().join("private")).unwrap()
        .with_application_preferences(Some(preferences.clone())).unwrap());
    let request = |id: &str, action| Request { schema_version: 2, worker_environment_id: "room".into(),
        request_id: id.into(), tool: "gui.click".into(), action };
    let unconfigured = DesktopHost::new(executor.clone(), root.path().join("unconfigured")).unwrap();
    let refused = unconfigured.handle(request("unconfigured", json!({"type":"control","mode":"acquire"})), false).await;
    assert!(!refused.accepted); assert!(refused.error.unwrap().message.contains("computer_use_preference_unavailable"));
    for operation in ["observe","screenshot","view","click","move","type","key","scroll","drag","app_click","app_move","app_drag",
        "app_scroll","app_key","app_type","app_set_value","app_select_text","app_secondary_action","app_paste","app_state","app_resume"] {
        let result = host.handle(request(operation, json!({"type":operation})), operation == "view").await;
        assert!(!result.accepted); assert!(result.error.unwrap().message.contains("computer_use_disabled"));
    }
    let status = host.handle(request("status", json!({"type":"status"})), false).await;
    assert!(status.accepted); assert_eq!(status.evidence.unwrap().status, "disabled");
    assert_eq!(unconfigured.handle(request("unconfigured-status", json!({"type":"status"})), false).await.evidence.unwrap().status, "unavailable");
    let preference = preferences.join("computer-use.json");
    crate::app_authorization::create_record(&preference, &json!({"schema_version":1,"enabled":true})).unwrap();
    let mut native = executor.execute(json!({"type":"status"})).await.unwrap();
    native["permissions"]["accessibility"] = json!(false);
    assert_eq!(host.status_from_native(&native).unwrap().status, "permission_required");
    native["permissions"]["accessibility"] = json!(true);
    assert_eq!(host.status_from_native(&native).unwrap().status, "ready");
    let generation = host.handle(request("control", json!({"type":"control","mode":"acquire"})), false).await.evidence.unwrap().control.generation;
    let observation = host.handle(request("observe", json!({"type":"observe","screen_id":"screen"})), false).await.evidence.unwrap().observation.unwrap().id;
    let call = request("input", json!({"type":"click","screen_id":"screen","observation_id":observation,
        "control_generation":generation,"x":20,"y":30}));
    let running = { let host = host.clone(); tokio::spawn(async move { host.handle(call, false).await }) };
    tokio::time::timeout(std::time::Duration::from_secs(1), executor.started.notified()).await.unwrap();
    std::fs::write(&preference, b"{\"schema_version\":1,\"enabled\":false}").unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), running).await.unwrap().unwrap();
    assert!(!result.accepted); assert!(result.error.unwrap().message.contains("input_interrupted"));
    assert_eq!(host.state.lock().unwrap().control.mode, "stopped");
    let key = blake3::hash(b"room:input").to_hex().to_string();
    let receipt: Value = serde_json::from_slice(&std::fs::read(root.path().join("private").join(format!("{key}.json"))).unwrap()).unwrap();
    assert_eq!(receipt["completed"], false); assert_eq!(receipt["events_posted"], 1);
    for operation in ["observe","screenshot","view"] {
        let result = host.handle(request(operation, json!({"type":operation})), operation == "view").await;
        assert!(!result.accepted); assert!(result.error.unwrap().message.contains("computer_use_disabled"));
    }
    std::fs::write(&preference, b"{\"schema_version\":1,\"enabled\":\"true\"}").unwrap();
    assert!(crate::app_authorization::computer_use_enabled(&preferences).is_err());
    assert!(!host.handle(request("bad", json!({"type":"click"})), false).await.accepted);
    assert!(!host.handle(request("bad-capture", json!({"type":"screenshot"})), false).await.accepted);
    assert!(host.handle(request("stop", json!({"type":"stop"})), true).await.accepted);
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn saved_off_blocks_retained_source_bootstrap_and_cancels_exact_pending_stream() {
    use crate::app_authorization::{OriginalCall, OperatorRequest, ApplicationTarget, PendingRequest};
    use octopus_descriptors::{ArtifactId, InterfaceId};
    use std::os::unix::fs::PermissionsExt;
    struct BootstrapFixture { preference: PathBuf, disable_at: &'static str, calls: Mutex<Vec<Value>> }
    impl DesktopExecutor for BootstrapFixture {
        fn retained_process_id<'a>(&'a self) -> Pin<Box<dyn Future<Output=Result<u32,String>>+Send+'a>> { Box::pin(async { Ok(77) }) }
        fn execute<'a>(&'a self, action: Value) -> Execution<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(action.clone());
                if action["type"] == self.disable_at { std::fs::write(&self.preference,b"{\"schema_version\":1,\"enabled\":false}").unwrap(); }
                match action["type"].as_str() {
                    Some("pip_prepare") => Ok(json!({"schema_version":1,"producer_pid":77,"presentation_id":"presentation","stream_generation":1,
                        "native_window_incarnation":"1","native_cg_window_id":10})),
                    Some("pip_install") => Ok(json!({"schema_version":1,"installed":true,"bootstrap_id":action["bootstrap_id"],"producer_pid":77})),
                    Some("pip_cancel") => Ok(json!({"cancelled":true})),
                    _ => Err("unexpected native effect".into()),
                }
            })
        }
    }
    for disable_at in ["disabled","pip_prepare","pip_install","enabled"] {
        let temp = tempfile::tempdir().unwrap(); let root = temp.path().canonicalize().unwrap();
        let preferences = root.join("preferences"); std::fs::create_dir(&preferences).unwrap();
        std::fs::set_permissions(&preferences,std::fs::Permissions::from_mode(0o700)).unwrap();
        let preference = preferences.join("computer-use.json");
        crate::app_authorization::create_record(&preference,&json!({"schema_version":1,"enabled":disable_at!="disabled"})).unwrap();
        let executor=Arc::new(BootstrapFixture {preference,disable_at,calls:Mutex::new(Vec::new())});
        let mut host=DesktopHost::new(executor.clone(),root.join("host")).unwrap().with_application_preferences(Some(preferences)).unwrap();
        host.app_approval=Some((crate::config::AppApprovalConfig {operator_interface:InterfaceId::new("operator"),resume_tool:"gui.app_resume".into()},"host".into()));
        host.presentation_timeout=Some(std::time::Duration::from_secs(2));
        let room=protocol::application::Scope {host_generation:"host".into(),worker_environment_id:"room".into(),formation_pin:"formation".into()};
        let original=OriginalCall {run:"run".into(),ee:"ee".into(),worker:InterfaceId::new("worker"),door_interface:InterfaceId::new("door"),
            tool:"gui.app_state".into(),correlation_id:"call".into(),arguments:json!({}),source_movement_seq:1,
            source_artifact:octopus_ledger::ArtifactRef {id:ArtifactId::new("source"),content:octopus_ledger::ContentHash::of_bytes(b"source")} };
        let grant=OperatorRequest::new(original.clone(),room.clone(),ApplicationTarget {bundle_id:"example.app".into(),bundle_path:"/Applications/Example.app".into(),display_name:"Example".into(),instance:None},"gui.app_resume".into(),None,None).unwrap();
        let source:protocol::application::LiveAppSource=serde_json::from_value(json!({"scope":room,"grant_request_id":grant.request_id,
            "application":{"app":{"host_generation":"host","app_instance_id":"app"},"bundle_id":"example.app","bundle_path":"/Applications/Example.app","display_name":"Example","process_id":1,"launch_time_unix_ms":null},
            "source":original.source(),"observation":{"id":"observation","revision":1,"control_generation":0},
            "windows":[{"window_id":"window","cg_window_id":10,"title":null,"frame_screen_points":{"x":0,"y":0,"width":100,"height":100},"on_screen":true,"window_layer":0,"capture_geometry":null,"source_image":null}]})).unwrap();
        source.validate().unwrap();
        host.app_grants.lock().unwrap().insert(grant.request_id.clone(),PendingRequest {request:grant.clone(),native_target_handle:"target".into(),operator_interface:InterfaceId::new("operator"),provider_interface:InterfaceId::new("provider"),native_policy:None,native_policy_digest:None});
        host.app_sources.lock().unwrap().insert(("room".into(),"formation".into(),"app".into()),source);
        let request=Request {schema_version:2,worker_environment_id:"room".into(),request_id:"bootstrap".into(),tool:"operator.desktop".into(),
            action:json!({"type":"receiver_bootstrap","scope":{"host_generation":"host","worker_environment_id":"room","formation_pin":"formation","app_instance_id":"app","grant_request_id":grant.request_id},
                "source_window_id":"window","receiver_pid":88,"receiver_nonce":"a".repeat(64),"receiver_instance_nonce":"b".repeat(64)})};
        let result=host.handle(request,true).await;
        let calls=executor.calls.lock().unwrap();
        if disable_at=="enabled" {assert!(result.accepted);assert!(result.receiver_bootstrap.is_some());assert_eq!(calls.len(),2);}
        else {
            assert!(!result.accepted);assert!(result.receiver_bootstrap.is_none());assert!(result.error.unwrap().message.contains("computer_use_disabled"));
            if disable_at=="disabled" {assert!(calls.is_empty());}
            else {let cancel=calls.last().unwrap();assert_eq!(cancel["type"],"pip_cancel");assert_eq!(cancel["bootstrap_id"],calls[0]["bootstrap_id"]);
                assert_eq!(calls.iter().filter(|action|action["type"]=="pip_install").count(),usize::from(disable_at=="pip_install"));}
        }
    }
}

#[tokio::test]
async fn presentation_completion_is_private_and_requires_exact_acknowledgement() {
    use protocol::presentation::{Action, Completion, CompletionResult};
    let target: Completion = serde_json::from_value(json!({
        "scope":{"host_generation":"host","worker_environment_id":"room","formation_pin":"pin",
            "app_instance_id":"app","grant_request_id":"grant"},
        "source_window_id":"window","presentation_id":"presentation","stream_generation":7,
        "receiver_instance_nonce":"a".repeat(64)
    })).unwrap();
    let action = serde_json::to_value(Action::PresentationComplete(target.clone())).unwrap();
    assert_eq!(action["type"], "presentation_complete");
    assert!(target.validate("room").is_ok());
    assert!(target.validate("other-room").is_err());
    for field in ["source_window_id", "presentation_id", "receiver_instance_nonce"] {
        let mut changed = serde_json::to_value(&target).unwrap(); changed[field] = json!("foreign");
        let acknowledgement: CompletionResult = serde_json::from_value(json!({"schema_version":1,"status":"ended","target":changed})).unwrap();
        assert!(acknowledgement.validate_for(&target).is_err());
    }
    for status in ["ended", "absent"] {
        let acknowledgement: CompletionResult = serde_json::from_value(json!({"schema_version":1,"status":status,"target":target})).unwrap();
        assert!(acknowledgement.validate_for(&target).is_ok());
    }
    assert!(serde_json::from_value::<CompletionResult>(json!({"schema_version":1,"status":"ending","target":target})).is_err());
    let mut extra = action.clone(); extra["approved"] = json!(true);
    assert!(serde_json::from_value::<Action>(extra).is_err());
    let root = tempfile::tempdir().unwrap();
    let executor = Arc::new(HeldInput { started:tokio::sync::Notify::new(),cancelled:tokio::sync::Notify::new() });
    let host = DesktopHost::new(executor,root.path().join("private")).unwrap();
    let mut request = Request { schema_version:2,worker_environment_id:"room".into(),request_id:"completion".into(),tool:"operator.desktop".into(),action };
    let response = host.handle(request.clone(),false).await;
    assert!(!response.accepted); assert!(response.presentation_result.is_none());
    assert!(response.error.unwrap().message.contains("private operator route"));
    request.tool="gui.app_click".into();
    assert!(!host.handle(request.clone(),true).await.accepted);
    request.tool="operator.desktop".into();
    let response=host.handle(request,true).await;
    assert!(!response.accepted); assert!(response.presentation_result.is_none());
    assert!(response.error.unwrap().message.contains("approval is not configured"));
}


#[cfg(target_os = "macos")]
struct PermissionFixture {
    calls: Mutex<Vec<Value>>,
    snapshot: Mutex<Value>,
    fail_settings: std::sync::atomic::AtomicBool,
    hold_settings: std::sync::atomic::AtomicBool,
    hold_reader: std::sync::atomic::AtomicBool,
    reader_started: tokio::sync::Notify,
    reader_release: tokio::sync::Notify,
    started: tokio::sync::Notify,
    cancelled: tokio::sync::Notify,
}
#[cfg(target_os = "macos")]
impl DesktopExecutor for PermissionFixture {
    fn retained_process_id<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<u32, String>> + Send + 'a>> {
        Box::pin(async { Ok(42) })
    }
    fn execute<'a>(&'a self, action: Value) -> Execution<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(action.clone());
            if action["type"] == "permission_settings_source" {
                assert!(action.get("cancel_file").is_none());
                if self.hold_reader.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    self.reader_started.notify_one();
                    tokio::time::timeout(std::time::Duration::from_secs(2), self.reader_release.notified()).await.unwrap();
                }
                return Ok(json!({"schema_version":1,"host_generation":action["host_generation"],
                    "permission_request_id":action["permission_request_id"],"permission":"opaque-screen","state":"observed",
                    "settings":{"process_id":73,"executable_path":"/fixture/Settings","start_seconds":"100","start_microseconds":"7",
                        "display_name":"Settings","bundle":null,"responsible_app":null},
                    "window":{"id":9,"observation_sequence":1,"bounds":{"x":-10.0,"y":20.0,"width":650.0,"height":500.0}}}));
            }
            assert_eq!(action["type"], "permissions");
            if action["operation"] == "open_settings" && self.fail_settings.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err("native settings outcome is unknown after its deadline".into());
            }
            if action["operation"] == "open_settings" {
                protocol::permission::validate_request_id(action["request_id"].as_str().unwrap()).unwrap();
                assert!(action["expires_at_mach_continuous_ticks"].as_str().unwrap().parse::<u64>().unwrap() > 0);
                assert_eq!(action["worker_environment_id"], "room");
                assert_eq!(action["host_generation"], "a".repeat(32));
                if self.hold_settings.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let cancel = PathBuf::from(action["cancel_file"].as_str().unwrap());
                    self.started.notify_one();
                    tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        while !cancel.exists() { tokio::time::sleep(std::time::Duration::from_millis(2)).await; }
                    }).await.unwrap();
                    assert_eq!(std::fs::read(cancel).unwrap(), b"caller_disconnected");
                    self.cancelled.notify_one();
                    return Err("native Settings continuation cancelled; inspect current permission status".into());
                }
            }
            Ok(self.snapshot.lock().unwrap().clone())
        })
    }
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn private_helper_permissions_bind_native_identity_and_never_enter_model_path() {
    let executor = Arc::new(PermissionFixture {
        calls: Mutex::new(Vec::new()), fail_settings: std::sync::atomic::AtomicBool::new(false),
        hold_reader: std::sync::atomic::AtomicBool::new(false), reader_started: tokio::sync::Notify::new(), reader_release: tokio::sync::Notify::new(),
        hold_settings: std::sync::atomic::AtomicBool::new(false), started: tokio::sync::Notify::new(), cancelled: tokio::sync::Notify::new(),
        snapshot: Mutex::new(json!({"helper":{"process_id":42,"executable_path":"/fixture/native-helper","display_name":"native-helper","bundle":null,"responsible_app":null},
            "permissions":[
                {"id":"opaque-screen","kind":"screen_capture","title":"Screen Recording","status":"not_granted","can_request":false,"can_open_settings":true,"provenance":{"process_id":42,"api":"CGPreflightScreenCaptureAccess"}},
                {"id":"opaque-ax","kind":"accessibility","title":"Accessibility","status":"granted","can_request":false,"can_open_settings":true,"provenance":{"process_id":42,"api":"AXIsProcessTrusted"}}
            ]})),
    });
    let root = tempfile::tempdir().unwrap();
    let host = Arc::new(DesktopHost::new(executor.clone(),root.path().join("private")).unwrap()
        .with_app_approval(None,"a".repeat(32)).with_presentation_timeout(std::time::Duration::from_secs(2)));
    let request = |id: u64, operation: &str, permission: Option<&str>| Request {
        schema_version:2, worker_environment_id:"room".into(), request_id:format!("00000000-0000-4000-8000-{id:012x}"),
        tool:"operator.desktop".into(), action:json!({"type":"permissions","operation":operation,"permission":permission}),
    };
    let status = request(1,"status",None);
    assert!(!host.handle(status.clone(),false).await.accepted);
    let mut model = status.clone(); model.tool="gui.status".into();
    assert!(!host.handle(model,true).await.accepted);
    assert!(!host.handle(request(2,"open_settings",Some("opaque-screen")),true).await.accepted);
    assert!(executor.calls.lock().unwrap().is_empty());
    for route in ["/v1/action","/v1/view","/v1/operator"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (server,_) = listener.accept().await.unwrap(); let serving_host=host.clone();
        let serving=tokio::spawn(async move { connection(server,serving_host,"fixture-secret").await });
        let body=serde_json::to_vec(&status).unwrap();
        client.write_all(format!("POST {route} HTTP/1.1\r\nAuthorization: Bearer fixture-secret\r\nContent-Length: {}\r\n\r\n",body.len()).as_bytes()).await.unwrap();
        client.write_all(&body).await.unwrap(); let mut reply=Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(1),client.read_to_end(&mut reply)).await.unwrap().unwrap();
        if route != "/v1/operator" {
            assert!(serving.await.unwrap().unwrap_err().contains("private operator endpoint"));
            assert!(reply.is_empty()); assert!(executor.calls.lock().unwrap().is_empty());
        } else {
            serving.await.unwrap().unwrap();
            let boundary=reply.windows(4).position(|bytes| bytes==b"\r\n\r\n").unwrap()+4;
            let response: Response=serde_json::from_slice(&reply[boundary..]).unwrap();
            assert!(response.accepted); assert!(response.evidence.is_none() && response.application_result.is_none()
                && response.receiver_bootstrap.is_none() && response.presentation_result.is_none());
            assert!(protocol::validate_response(&response,&status).is_err());
            let snapshot=response.permission_result.unwrap();
            assert_eq!(snapshot.host_generation,"a".repeat(32)); assert_eq!(snapshot.helper.process_id,42);
            assert_eq!(snapshot.permissions[0].status,protocol::permission::Status::NotGranted);
            assert!(!snapshot.permissions[0].can_request); assert!(snapshot.helper.bundle.is_none() && snapshot.helper.responsible_app.is_none());
        }
    }
    let calls=executor.calls.lock().unwrap().len();
    assert!(!host.handle(request(3,"request",Some("opaque-screen")),true).await.accepted);
    assert!(!host.handle(request(4,"open_settings",Some("foreign")),true).await.accepted);
    let mut malformed=status.clone(); malformed.action.as_object_mut().unwrap().remove("permission");
    assert!(!host.handle(malformed,true).await.accepted);
    for field in ["request_id", "cancel_file", "expires_at_mach_continuous_ticks"] {
        let mut forged = status.clone(); forged.action[field] = json!("foreign");
        assert!(!host.handle(forged,true).await.accepted);
    }
    assert_eq!(executor.calls.lock().unwrap().len(),calls);
    let settings=request(5,"open_settings",Some("opaque-screen"));
    assert!(host.handle(settings.clone(),true).await.accepted);
    assert!(!host.handle(settings,true).await.accepted);
    assert_eq!(executor.calls.lock().unwrap().iter().filter(|action| action["operation"]=="open_settings").count(),1);
    assert!(host.app_grants.lock().unwrap().is_empty()); assert_eq!(host.state.lock().unwrap().control.mode,"idle");
    executor.fail_settings.store(true,std::sync::atomic::Ordering::SeqCst);
    let failed=request(6,"open_settings",Some("opaque-ax"));
    assert!(host.handle(failed.clone(),true).await.error.unwrap().message.contains("unknown"));
    let calls=executor.calls.lock().unwrap().len();
    assert!(!host.handle(failed,true).await.accepted);
    assert!(!host.handle(request(7,"open_settings",Some("opaque-ax")),true).await.accepted);
    assert_eq!(executor.calls.lock().unwrap().len(),calls);
    assert!(host.handle(request(8,"status",None),true).await.accepted);
    executor.hold_settings.store(true, std::sync::atomic::Ordering::SeqCst);
    let pending = request(10,"open_settings",Some("opaque-screen"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (server,_) = listener.accept().await.unwrap(); let server_host = host.clone();
    let serving = tokio::spawn(async move { connection(server,server_host,"fixture-secret").await });
    let bytes = serde_json::to_vec(&pending).unwrap();
    client.write_all(format!("POST /v1/operator HTTP/1.1\r\nAuthorization: Bearer fixture-secret\r\nContent-Length: {}\r\n\r\n",bytes.len()).as_bytes()).await.unwrap();
    client.write_all(&bytes).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1),executor.started.notified()).await.unwrap();
    let cancel = host.permission_pending.lock().unwrap().as_ref().unwrap().cancel_file.clone();
    let reader = |id: u64| Request { action:json!({"type":"permission_settings_source","permission_request_id":pending.request_id}),
        ..request(id,"status",None) };
    assert!(!host.handle(reader(90),false).await.accepted);
    for field in ["permission_request_id", "worker_environment_id"] {
        let mut foreign = reader(91);
        if field == "worker_environment_id" { foreign.worker_environment_id = "foreign".into(); }
        else { foreign.action[field] = json!(request(92,"status",None).request_id); }
        assert!(!host.handle(foreign,true).await.accepted);
    }
    let open_reader = |call: Request, host: Arc<DesktopHost>, route: &'static str| async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (server,_) = listener.accept().await.unwrap();
        let serving = tokio::spawn(async move { connection(server,host,"fixture-secret").await });
        let bytes = serde_json::to_vec(&call).unwrap();
        client.write_all(format!("POST {route} HTTP/1.1\r\nAuthorization: Bearer fixture-secret\r\nContent-Length: {}\r\n\r\n",bytes.len()).as_bytes()).await.unwrap();
        client.write_all(&bytes).await.unwrap();
        (client,serving)
    };
    let mut readers = tokio::task::JoinSet::new();
    for id in 100..180 {
        let (mut client, serving) = open_reader(reader(id),host.clone(),"/v1/operator").await;
        let expected = pending.request_id.clone();
        readers.spawn(async move {
            let mut reply=Vec::new(); client.read_to_end(&mut reply).await.unwrap(); serving.await.unwrap().unwrap();
            let boundary=reply.windows(4).position(|bytes| bytes==b"\r\n\r\n").unwrap()+4;
            let response: Response=serde_json::from_slice(&reply[boundary..]).unwrap();
            assert!(response.accepted, "{:?}", response.error); assert!(response.permission_result.is_none() && response.evidence.is_none());
            let source=response.permission_settings_source.unwrap();
            source.validate(&"a".repeat(32),&expected).unwrap();
            assert_eq!(source.state,protocol::permission::SettingsState::Observed); assert_eq!(source.settings.unwrap().process_id,73);
        });
    }
    tokio::time::timeout(std::time::Duration::from_secs(2),async { while let Some(result)=readers.join_next().await { result.unwrap(); } }).await.unwrap();
    assert!(!cancel.exists()); assert!(!serving.is_finished()); assert!(host.permission_state.try_lock().is_err());
    for route in ["/v1/action", "/v1/view"] {
        let (mut client, serving)=open_reader(reader(180),host.clone(),route).await;
        let mut reply=Vec::new(); client.read_to_end(&mut reply).await.unwrap();
        assert!(reply.is_empty()); assert!(serving.await.unwrap().unwrap_err().contains("private operator endpoint"));
    }
    executor.hold_reader.store(true,std::sync::atomic::Ordering::SeqCst);
    let (reader_client, reader_serving)=open_reader(reader(181),host.clone(),"/v1/operator").await;
    tokio::time::timeout(std::time::Duration::from_secs(1),executor.reader_started.notified()).await.unwrap();
    drop(reader_client);
    tokio::task::yield_now().await;
    host.cancel_disconnected_request(&reader(181)).await.unwrap();
    assert!(!cancel.exists()); assert!(!serving.is_finished());
    executor.reader_release.notify_one();
    let reader_completion = tokio::time::timeout(std::time::Duration::from_secs(1),reader_serving).await.unwrap().unwrap();
    if let Err(error) = reader_completion { assert!(error.contains("disconnected"), "{error}"); }
    assert!(!cancel.exists()); assert!(!serving.is_finished());
    assert_eq!(executor.calls.lock().unwrap().iter().filter(|call| call["type"]=="permission_settings_source").count(),81);

    let unrelated = root.path().join("private/unrelated-input.cancel");
    host.state.lock().unwrap().active_cancel = Some(unrelated.clone());
    let control = serde_json::to_value(&host.state.lock().unwrap().control).unwrap();
    let mut foreign = pending.clone(); foreign.request_id = request(11,"status",None).request_id;
    host.cancel_disconnected_request(&foreign).await.unwrap();
    let mut foreign = pending.clone(); foreign.action["permission"] = json!("opaque-ax");
    host.cancel_disconnected_request(&foreign).await.unwrap();
    assert!(!cancel.exists());
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(1),executor.cancelled.notified()).await.unwrap();
    assert!(serving.await.unwrap().unwrap_err().contains("disconnected"));
    assert!(host.permission_pending.lock().unwrap().is_none());
    let ended = host.handle(reader(182),true).await.permission_settings_source.unwrap();
    assert_eq!(ended.state,protocol::permission::SettingsState::Ended);
    assert!(ended.permission.is_none() && ended.settings.is_none() && ended.window.is_none());
    assert!(host.permission_state.lock().await.snapshot.is_none());
    assert_eq!(serde_json::to_value(&host.state.lock().unwrap().control).unwrap(),control);
    assert_eq!(host.state.lock().unwrap().active_cancel.as_ref(),Some(&unrelated));
    assert!(!unrelated.exists()); assert!(host.app_grants.lock().unwrap().is_empty());
    let calls = executor.calls.lock().unwrap().len();
    assert!(!host.handle(pending,true).await.accepted);
    assert_eq!(executor.calls.lock().unwrap().len(),calls);
    assert!(host.handle(request(12,"status",None),true).await.accepted);
    executor.snapshot.lock().unwrap()["permissions"][0]["provenance"]["process_id"]=json!(43);
    let foreign=host.handle(request(9,"status",None),true).await;
    assert!(!foreign.accepted); assert!(foreign.permission_result.is_none());
    assert!(host.permission_state.lock().await.snapshot.is_none());
}
