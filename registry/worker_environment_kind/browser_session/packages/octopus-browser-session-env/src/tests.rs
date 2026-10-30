use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId, ToolDescriptorId};
use octopus_interfaces::{
    GrantProjection, InterfaceArtifactFile, InterfaceInvocationEnvelope, InvocationMetadata,
    RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_tool_door_runtime::{
    InterfaceHostIoLimits, ToolProcessEnvironment, ToolProcessError, ToolProcessInvocation,
    WorkerEnvironmentLifecycleRecorder, WorkerEnvironmentLiveStateRecorder,
};
use octopus_tool_registry::{load_tool_descriptor_from_dir, ToolArgumentValidator, ToolDescriptor};
use octopus_worker_env::{
    WorkerEnvKindRegistry, WorkerEnvKindSpec, WorkerEnvironmentFormation,
    WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId, WorkerEnvironmentLifecycle,
    WorkerEnvironmentRecordStore,
};
use serde_json::{json, Value};
use tokio::net::TcpListener as TokioTcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use super::browser_session::{safe_control_body_evidence, BrowserRoom};

const DOOR: &str = "system_browser";
const WORKER: &str = "system_worker_coder_max";

#[derive(Default)]
struct TestWorkerEnvironmentRecorder {
    live_states: std::sync::Mutex<Vec<Value>>,
    records: Option<WorkerEnvironmentRecordStore>,
    fail_lifecycle: AtomicBool,
    fail_live_state: AtomicBool,
}

impl TestWorkerEnvironmentRecorder {
    fn fail_next_evidence_writes(&self) {
        self.fail_lifecycle.store(true, Ordering::Release);
        self.fail_live_state.store(true, Ordering::Release);
    }
}

impl WorkerEnvironmentLifecycleRecorder for TestWorkerEnvironmentRecorder {
    fn record<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        lifecycle: WorkerEnvironmentLifecycle,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            let completing = self.live_states.lock().unwrap().last()
                .is_some_and(|state| state["activity"]["status"] == "running");
            if completing && self.fail_lifecycle.swap(false, Ordering::AcqRel) {
                Err("injected lifecycle write failure".to_string())
            } else if let Some(records) = &self.records {
                match lifecycle {
                    WorkerEnvironmentLifecycle::Active => records.activate(formation, 1),
                    WorkerEnvironmentLifecycle::Failed => records.fail(formation, 1),
                    WorkerEnvironmentLifecycle::Released => records.release(formation, 1),
                }
                .map(|_| ())
                .map_err(|error| error.to_string())
            } else {
                Ok(())
            }
        })
    }
}

impl WorkerEnvironmentLiveStateRecorder for TestWorkerEnvironmentRecorder {
    fn record_live_state<'a>(
        &'a self,
        formation: &'a WorkerEnvironmentFormation,
        state: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if state["activity"]["status"] != "running" && self.fail_live_state.swap(false, Ordering::AcqRel) {
                return Err("injected live-state write failure".to_string());
            }
            if let Some(records) = &self.records {
                records.write_live_state(&formation.worker_environment_id, state)
                    .map_err(|error| error.to_string())?;
            }
            self.live_states.lock().unwrap().push(state.clone());
            Ok(())
        })
    }
}

fn registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}

fn browser_tools() -> Vec<String> {
    [
        "browser.navigate",
        "browser.snapshot",
        "browser.click",
        "browser.type",
        "browser.key",
        "browser.scroll",
        "browser.back",
        "browser.forward",
        "browser.reload",
        "browser.viewport_set",
        "browser.viewport_reset",
        "browser.eval",
        "browser.hover",
        "browser.select",
        "browser.drag",
        "browser.wait",
        "browser.images",
        "browser.console",
        "browser.dialog",
        "browser.screenshot",
        "browser.pdf",
        "browser.cookies",
        "browser.cookie_set",
        "browser.cookie_delete",
        "browser.tabs",
        "browser.tab_new",
        "browser.tab_select",
        "browser.tab_mark",
        "browser.tab_close",
        "browser.download_arm",
        "browser.download_wait",
        "browser.download_cancel",
        "browser.close",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn descriptor(id: &str) -> ToolDescriptor {
    load_tool_descriptor_from_dir(&registry_root().join("tool"), id)
        .unwrap()
        .unwrap()
}

fn effective_descriptor(id: &str) -> ToolDescriptor {
    let mut descriptor = descriptor(id);
    descriptor.interface = InterfaceId::new(DOOR);
    descriptor
}

fn formation(
    kind: &str,
    tools: &[String],
    selected_tool: &str,
) -> (WorkerEnvironmentFormation, WorkerEnvKindSpec) {
    let registry =
        WorkerEnvKindRegistry::load_from_dir(&registry_root().join("worker_environment_kind"))
            .unwrap();
    let hashes = tools
        .iter()
        .map(|tool| {
            let value = serde_json::to_value(descriptor(tool)).unwrap();
            let hash = blake3::hash(&octopus_descriptors::canonical_bytes(&value))
                .to_hex()
                .to_string();
            (tool.clone(), hash)
        })
        .collect::<BTreeMap<_, _>>();
    let process_bins = tools
        .iter()
        .map(|tool| (tool.clone(), descriptor(tool).process.bin))
        .collect::<BTreeMap<_, _>>();
    let kind_id = WorkerEnvironmentKindId::new(kind);
    let kinds = vec![kind_id.clone()];
    let door = InterfaceId::new(DOOR);
    let worker = InterfaceId::new(WORKER);
    let formed = registry
        .form(&WorkerEnvironmentFormationRequest {
            door_additional_root_bindings: &Default::default(),
            door_interface: &door,
            door_descriptor_hash: "browser-test-door-hash",
            door_driver_ref: "system_browser",
            tool: selected_tool,
            tool_descriptor_hashes: &hashes,
            tool_process_bins: &process_bins,
            tool_worker_environment_kinds: &kinds,
            worker: &worker,
            ee: "ee_browser_test",
            run: "run_browser_test",
            granted_worker_environment_kinds: &kinds,
            granted_tools: tools,
            door_root_bindings: &BTreeMap::new(),
            door_instance_dir: std::path::Path::new("."),
            ee_root_slot_allowance: None,
        })
        .unwrap();
    let (kind, _) = registry.get(&kind_id).unwrap();
    (formed, (*kind).clone())
}

fn envelope(
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    tools: &[String],
) -> InterfaceInvocationEnvelope {
    let payload = serde_json::to_vec(&json!({ "tool": tool, "arguments": arguments })).unwrap();
    let artifact_file = InterfaceArtifactFile::write(
        root.join(format!("inbound-{sequence}")),
        "tool.call",
        &payload,
    )
    .unwrap();
    let content = ContentHash::of_bytes(&payload);
    let metadata = InvocationMetadata::new(GrantProjection {
        tools: tools.to_vec(),
        worker_environment_kinds: vec!["browser_session".to_string()],
        ..GrantProjection::default()
    })
    .with_runtime(RuntimeInvocationMetadata {
        run: RunId::new("run_browser_test"),
        ee: EeId::new("ee_browser_test"),
        worker: InterfaceId::new(WORKER),
        checkpoint: CheckpointId::new(format!("cp_{sequence}")),
        interface: InterfaceId::new(DOOR),
        epoch: 0,
        run_record_hash: None,
    });
    InterfaceInvocationEnvelope {
        artifact: ArtifactRef {
            id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
            content,
        },
        artifact_file,
        artifact_binding_payloads: Vec::new(),
        metadata: Some(metadata),
    }
}

fn cloned_scope(
    base_tool: &str,
    exact_tool: &str,
) -> (
    ToolDescriptor,
    Vec<String>,
    WorkerEnvironmentFormation,
    WorkerEnvKindSpec,
) {
    let mut clone = effective_descriptor(base_tool);
    clone.id = ToolDescriptorId::new(exact_tool);
    let tools = browser_tools();
    let (mut formation, kind) = formation("browser_session", &tools, base_tool);
    formation.granted_tools.push(exact_tool.to_string());
    formation.granted_tools.sort();
    formation.tool_descriptor_hashes.insert(
        exact_tool.to_string(),
        blake3::hash(&octopus_descriptors::canonical_bytes(
            &serde_json::to_value(&clone).unwrap(),
        ))
        .to_hex()
        .to_string(),
    );
    let mut granted_tools = tools;
    granted_tools.push(exact_tool.to_string());
    granted_tools.sort();
    (clone, granted_tools, formation, kind)
}

fn invocation(
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
    tools: &[String],
    formation: &WorkerEnvironmentFormation,
    kind: &WorkerEnvKindSpec,
) -> ToolProcessInvocation {
    ToolProcessInvocation {
        request_id: sequence,
        descriptor: effective_descriptor(tool),
        envelope: envelope(root, sequence, tool, arguments, tools),
        source_movement_seq: sequence,
        worker_environment: Some(formation.clone()),
        worker_environment_kind: Some(kind.clone()),
    }
}

fn response(artifact: InterfaceArtifactFile) -> Value {
    serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap()
}

fn received(result: Result<InterfaceArtifactFile, ToolProcessError>) -> InterfaceArtifactFile {
    result.unwrap_or_else(|error| match error {
        ToolProcessError::Capability(error) | ToolProcessError::Unavailable(error) => {
            panic!("browser room invocation failed: {error}")
        }
    })
}

fn write_browser_interface(path: &Path, discovery_url: &str) {
    std::fs::create_dir_all(path).unwrap();
    let descriptor = json!({
        "id": DOOR,
        "interface_type": "system",
        "role": "system",
        "direction": "bidirectional",
        "source_names": ["browser"],
        "interface_driver_ref": "system_browser",
        "interface_driver_config": {
            "private_config": {
                "browser_room": {
                    "page_editing_profile": {"rich_text_paste":true,"plain_text_documents":[{"host":"docs.google.com","first_path_segment":"spreadsheets"}]},
                    "schema_version": 1,
                    "transport": {
                        "wire": "cdp",
                        "discovery_url": discovery_url,
                        "discovery_url_env": "OCTOPUS_TEST_BROWSER_CDP_URL",
                        "auth_env_key": "OCTOPUS_TEST_BROWSER_CDP_AUTH",
                        "auth_required": false,
                        "auth_header": "Authorization",
                        "auth_value_prefix": "Bearer ",
                        "headers": {},
                        "allowed_websocket_hosts": [],
                        "request_timeout_ms": 5000,
                        "navigation_timeout_ms": 5000,
                        "max_control_response_bytes": 1048576
                    },
                    "limits": {
                        "max_snapshot_nodes": 100,
                        "max_snapshot_chars": 10000,
                        "max_type_chars": 10000,
                        "max_eval_chars": 10000,
                        "max_screenshot_bytes": 1000000,
                        "max_pdf_bytes": 1000000,
                        "max_event_count": 100,
                        "max_event_bytes": 100000,
                        "max_console_entries": 100,
                        "max_images": 100
                    }
                }
            }
        }
    });
    std::fs::write(
        path.join("interface.json"),
        serde_json::to_vec_pretty(&descriptor).unwrap(),
    )
    .unwrap();
}

async fn spawn_cdp_mock() -> (String, tokio::task::JoinHandle<Vec<String>>) {
    spawn_context_cdp_mock(true, true, 1).await
}

async fn spawn_context_cdp_mock(context_available: bool, target_available: bool, page_connections: usize) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let ws_listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_url = format!(
        "ws://{}/devtools/page/TARGET1",
        ws_listener.local_addr().unwrap()
    );
    let browser_ws_url = ws_url.replace("/page/TARGET1", "/browser/BROWSER1");
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let ws_finished = finished.clone();
    let ws = tokio::spawn(async move {
        let mut connections = Vec::new();
        let target_available = std::sync::Arc::new(AtomicBool::new(target_available));
        let second_available = std::sync::Arc::new(AtomicBool::new(false));
        for _ in 0..=page_connections {
            let (tcp, _) = ws_listener.accept().await.unwrap();
            let target_available = target_available.clone();
            let second_available = second_available.clone();
            connections.push(tokio::spawn(async move {
        let mut route_path = String::new();
        let mut socket = tokio_tungstenite::accept_hdr_async(tcp, |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
            route_path = request.uri().path().to_string(); Ok(response)
        }).await.unwrap();
        let mut methods = Vec::new();
        let mut loader = "LOAD0";
        let mut pending_navigation = 0;
        let mut moving_viewport = false;
        let mut viewport_y = 80.0;
        let mut attached = std::collections::BTreeSet::new();
        while let Some(message) = socket.next().await {
            let Ok(Message::Text(text)) = message else {
                break;
            };
            let request: Value = serde_json::from_str(&text).unwrap();
            let id = request["id"].clone();
            let method = request["method"].as_str().unwrap().to_string();
            methods.push(method.clone());
            if method == "Runtime.evaluate" && request["params"]["expression"] == "retained-target-proof" { methods.push(format!("retained-proof:{route_path}")); }
            if method == "Runtime.enable" {
                socket
                    .send(Message::Text(
                        json!({
                            "method": "Runtime.consoleAPICalled",
                            "params": {
                                "type": "log",
                                "args": [{"type":"string","value":"console-proof"}],
                                "timestamp": 1.0
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            if method == "Input.dispatchMouseEvent" && request["params"]["type"] == "mouseReleased"
            {
                socket
                    .send(Message::Text(
                        json!({
                            "method": "Page.javascriptDialogOpening",
                            "params": {
                                "type": "confirm",
                                "message": "continue?",
                                "url": "https://example.com/",
                                "hasBrowserHandler": false
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
            }
            if method == "Runtime.evaluate" && request["params"]["expression"] == "location.reload()" {
                loader = "LOAD2";
                socket.send(Message::Text(json!({"method":"Page.frameNavigated","params":{"frame":{"id":"FRAME1","loaderId":loader,"url":"https://example.com/"}}}).to_string().into())).await.unwrap();
            }
            if method == "Runtime.evaluate" && request["params"]["expression"] == "window.scrollTo(0, 80)" { moving_viewport = true; }
            let route = request["sessionId"].as_str();
            if method == "Target.setAutoAttach" && attached.insert(route.map(str::to_string)) && route != Some("NESTED_SESSION") {
                assert_eq!(request["params"]["filter"],json!([{"type":"iframe","exclude":false}]));
                let child = if route.is_none() { "CHILD_SESSION" } else { "NESTED_SESSION" };
                let mut event=json!({"method":"Target.attachedToTarget","params":{"sessionId":child,"targetInfo":{"targetId":child,"type":"iframe"}}});
                if let Some(route)=route { event["sessionId"]=json!(route); }
                socket.send(Message::Text(event.to_string().into())).await.unwrap();
            }
            let result = match (method.as_str(), request["params"]["expression"].as_str()) {
                ("Target.createBrowserContext", _) => json!({"browserContextId":"CONTEXT1"}),
                ("Target.getBrowserContexts", _) => json!({"browserContextIds":if context_available { vec!["CONTEXT1","FOREIGN_CONTEXT"] } else { vec!["FOREIGN_CONTEXT"] }}),
                ("Target.createTarget", _) => {
                    assert_eq!(request["params"]["browserContextId"], "CONTEXT1");
                    if request["params"]["url"] == "https://second.test/" { second_available.store(true, Ordering::Release); json!({"targetId":"TARGET2"}) }
                    else { target_available.store(true, Ordering::Release); json!({"targetId":"TARGET1"}) }
                },
                ("Target.getTargets", _) => {
                    let mut targets = vec![json!({"targetId":"FOREIGN","type":"page","title":"Private","url":"https://private.test/","browserContextId":"FOREIGN_CONTEXT"})];
                    if target_available.load(Ordering::Acquire) { targets.push(json!({"targetId":"TARGET1","type":"page","title":"Example","url":"https://example.com/","browserContextId":"CONTEXT1"})); }
                    if second_available.load(Ordering::Acquire) { targets.push(json!({"targetId":"TARGET2","type":"page","title":"Second","url":"https://second.test/","browserContextId":"CONTEXT1"})); }
                    json!({"targetInfos":targets})
                },
                ("Emulation.setDeviceMetricsOverride", _) => { assert_eq!(request["params"], json!({"width":800,"height":600,"deviceScaleFactor":1,"mobile":false})); assert!(route.is_none()); json!({}) },
                ("Emulation.clearDeviceMetricsOverride", _) => { assert_eq!(request["params"],json!({})); assert!(route.is_none()); json!({}) },
                ("Network.getCookies", _) => json!({"cookies":[{"name":"room_cookie","value":"owned"}]}),
                ("Target.activateTarget", _) => {
                    assert!(["TARGET1","TARGET2"].contains(&request["params"]["targetId"].as_str().unwrap()));
                    json!({})
                },
                ("Target.closeTarget", _) => {
                    match request["params"]["targetId"].as_str().unwrap() {
                        "TARGET1" => target_available.store(false, Ordering::Release),
                        "TARGET2" => second_available.store(false, Ordering::Release),
                        _ => panic!("foreign target close"),
                    }
                    json!({"success":true})
                },
                ("Page.getNavigationHistory", _) => json!({"currentIndex":0,"entries":[{"id":1}]}),
                ("Page.navigate", _) => {
                    pending_navigation = 2;
                    json!({"frameId": "FRAME1", "loaderId": "LOAD1"})
                },
                ("Page.getFrameTree", _) => {
                    if pending_navigation > 0 { pending_navigation -= 1; if pending_navigation == 0 { loader = "LOAD1"; } }
                    match route {
                        None => json!({"frameTree":{"frame":{"id":"FRAME1","loaderId":loader},"childFrames":[{"frame":{"id":"SAME","loaderId":"S1","parentId":"FRAME1"},"childFrames":[{"frame":{"id":"CHILD","parentId":"SAME"}}]}]}}),
                        Some("CHILD_SESSION") => json!({"frameTree":{"frame":{"id":"CHILD","loaderId":"C1","parentId":"SAME"},"childFrames":[{"frame":{"id":"INNER","loaderId":"I1","parentId":"CHILD"},"childFrames":[{"frame":{"id":"NESTED","parentId":"INNER"}}]}]}}),
                        Some("NESTED_SESSION") => json!({"frameTree":{"frame":{"id":"NESTED","loaderId":"N1","parentId":"INNER"}}}),
                        _ => panic!("unowned route"),
                    }
                },
                ("Page.createIsolatedWorld", _) => {
                    assert_eq!(request["params"]["grantUniveralAccess"],false);
                    let (context,expected_route)=match request["params"]["frameId"].as_str().unwrap() { "FRAME1"=>(1,None),"SAME"=>(2,None),"CHILD"=>(3,Some("CHILD_SESSION")),"INNER"=>(4,Some("CHILD_SESSION")),"NESTED"=>(5,Some("NESTED_SESSION")),_=>panic!("foreign frame") };
                    assert_eq!(route,expected_route);json!({"executionContextId":context})
                },
                ("DOMSnapshot.captureSnapshot", _) => {
                    let (frames,owners)=match route { None=>(vec!["FRAME1","SAME"],vec![10,20]),Some("CHILD_SESSION")=>(vec!["CHILD","INNER"],vec![30,40]),Some("NESTED_SESSION")=>(vec!["NESTED"],vec![0]),_=>panic!("foreign snapshot") };
                    let mut strings=frames.clone();strings.extend(["HTML","IFRAME"]);
                    json!({"strings":strings,"documents":owners.iter().enumerate().map(|(index,owner)|json!({"frameId":index,"nodes":{"nodeName":if *owner==0 {vec![frames.len()]}else{vec![frames.len()+1]},"backendNodeId":[if *owner==0 {50}else{*owner}]}})).collect::<Vec<_>>()})
                },
                ("DOM.describeNode", _) => json!({"node":{"frameId":match request["params"]["backendNodeId"].as_i64().unwrap() {10=>"SAME",20=>"CHILD",30=>"INNER",40=>"NESTED",_=>panic!("unknown owner")}}}),
                ("DOM.getFrameOwner", _) => json!({"backendNodeId":match request["params"]["frameId"].as_str().unwrap() {"SAME"=>10,"CHILD"=>20,"INNER"=>30,"NESTED"=>40,_=>panic!("unknown frame")}}),
                ("DOM.getNodeForLocation", _) => json!({"backendNodeId":if route.is_none(){20}else{40},"frameId":if route.is_none(){"CHILD"}else{"NESTED"}}),
                ("DOM.getContentQuads", _) => json!({"quads":[if route.is_none() {vec![100,200,200,200,200,300,100,300]}else{vec![50,60,150,60,150,160,50,160]}]}),
                ("DOM.resolveNode", _) => json!({"object":{"objectId":format!("NODE{}",request["params"]["backendNodeId"])}}),
                ("Runtime.callFunctionOn", _) => {
                    let function = request["params"]["functionDeclaration"].as_str().unwrap();
                    if request["params"]["returnByValue"] == false {
                        json!({"result":{"objectId":request["params"]["objectId"]}})
                    } else if function.contains("const offsetWidth=Number(this.offsetWidth)") {
                        json!({"result":{"value":{"status":"ready","left":10,"top":20,"scaleX":2,"scaleY":2,"width":100,"height":100}}})
                    } else if function.contains("receivesPoint(options,this)") {
                        json!({"result":{"value":true}})
                    } else if function.contains("needs_input") {
                        assert_eq!(request["params"]["objectId"], "NODE8");
                        json!({"result":{"value":{"status":"focused","needs_input":true}}})
                    } else if function.contains("return !!element") {
                        assert_eq!(request["params"]["objectId"], "NODE8");
                        json!({"result":{"value":true}})
                    } else {
                        json!({"result":{"value":{"status":"ready","x":20.0,"y":30.0}}})
                    }
                },
                ("Input.insertText", _) => { assert_eq!(route,Some("CHILD_SESSION")); assert_eq!(request["params"]["text"], "contract input"); json!({}) },
                ("Page.getLayoutMetrics", _) => {
                    if moving_viewport { viewport_y += 1.0; }
                    json!({"cssContentSize":{"x":-12,"y":24,"width":1200,"height":2400},
                        "cssLayoutViewport":{"pageX":10,"pageY":viewport_y,"clientWidth":600,"clientHeight":400},
                        "cssVisualViewport":{"offsetX":5,"offsetY":8,"pageX":15,"pageY":viewport_y+8.0,"clientWidth":300,"clientHeight":200,"scale":2,"zoom":1.25}})
                },
                ("Page.captureScreenshot", _) => {
                    assert_eq!(request["params"]["fromSurface"],true);
                    if request["params"]["captureBeyondViewport"] == true {
                        assert_eq!(request["params"]["clip"],json!({"x":-12.0,"y":24.0,"width":1200.0,"height":2400.0,"scale":1.0}));
                        json!({"data":"iVBORw0KGgoAAAANSUhEUgAAAAQAAAAICAIAAABRUclSAAAAEElEQVR4nGMQN42EIwbacQAo5BShaAlMbAAAAABJRU5ErkJggg=="})
                    } else {
                        assert!(request["params"].get("clip").is_none());
                        json!({"data":"iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAIAAAASFvFNAAAAEElEQVR4nGMQN42EIAY4CwAlNAPfot/ccAAAAABJRU5ErkJggg=="})
                    }
                },
                ("Runtime.evaluate", Some("window.devicePixelRatio")) => json!({"result":{"type":"number","value":2.5}}),
                ("Runtime.evaluate", Some("document.readyState")) => {
                    assert_eq!(pending_navigation, 0, "readyState of the previous loader must not admit navigation");
                    json!({"result": {"type": "string", "value": "complete"}})
                },
                ("Runtime.evaluate", Some(expression)) if expression.contains("performance.getEntriesByType('paint')") => json!({"result":{"value":{"readyState":"complete","painted":false}}}),
                ("Runtime.evaluate", Some(_)) => json!({
                    "result": {"type": "object", "value": {"url": "https://example.com/", "title": "Example"}}
                }),
                ("Accessibility.getFullAXTree", _) => {
                    let frame=request["params"]["frameId"].as_str().unwrap();
                    let mut nodes=vec![json!({"nodeId":"1","role":{"value":"RootWebArea"},"name":{"value":frame},"childIds":[]})];
                    if frame=="FRAME1" { nodes.extend([json!({"nodeId":"2","backendDOMNodeId":7,"role":{"value":"button"},"name":{"value":"Submit"}}),json!({"nodeId":"3","backendDOMNodeId":8,"role":{"value":"textbox"},"name":{"value":"Text"}})]); }
                    if frame=="CHILD" { nodes.push(json!({"nodeId":"2","backendDOMNodeId":8,"role":{"value":"textbox"},"name":{"value":"Child text"}})); }
                    if frame=="NESTED" { nodes.push(json!({"nodeId":"2","backendDOMNodeId":7,"role":{"value":"button"},"name":{"value":"Nested button"}})); }
                    let owner=match frame {"FRAME1"=>10,"SAME"=>20,"CHILD"=>30,"INNER"=>40,_=>0};
                    if owner>0 {nodes.push(json!({"nodeId":"9","backendDOMNodeId":owner,"role":{"value":"Iframe"},"name":{"value":"Frame"}}));}
                    nodes[0]["childIds"]=json!(nodes.iter().skip(1).map(|node|node["nodeId"].clone()).collect::<Vec<_>>());
                    json!({"nodes":nodes})
                },
                ("DOM.getBoxModel", _) => json!({
                    "model": {"content": [10.0, 20.0, 30.0, 20.0, 30.0, 40.0, 10.0, 40.0]}
                }),
                _ => json!({}),
            };
            socket
                .send(Message::Text(
                    { let mut response=json!({"id": id, "result": result}); if let Some(route)=route { response["sessionId"]=json!(route); } response.to_string().into() },
                ))
                .await
                .unwrap();
        }
        methods
            }));
        }
        let mut methods = Vec::new();
        for connection in connections { methods.extend(connection.await.unwrap()); }
        ws_finished.store(true, Ordering::Release);
        methods
    });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let discovery_url = format!("http://{}/", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        while !finished.load(Ordering::Acquire) {
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            Err(error) => panic!("HTTP fixture accept failed: {error}"),
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut request = [0u8; 4096];
        let count = stream.read(&mut request).unwrap();
        let request = std::str::from_utf8(&request[..count]).unwrap();
        let body = if request.starts_with("GET /json/version ") {
            json!({"webSocketDebuggerUrl":browser_ws_url})
        } else {
            assert!(request.starts_with("GET /json/list "), "{request}");
            json!([{"id":"TARGET1","type":"page","title":"","url":"about:blank","webSocketDebuggerUrl":ws_url},
                {"id":"TARGET2","type":"page","title":"","url":"about:blank","webSocketDebuggerUrl":ws_url.replace("TARGET1","TARGET2")}])
        }.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (discovery_url, ws)
}

#[tokio::test]
async fn browser_room_keeps_one_cdp_session_across_snapshot_ref_actions_and_publishes_viewer() {
    let (discovery_url, ws) = spawn_context_cdp_mock(true, true, 3).await;
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("registry/interface").join(DOOR);
    let process_var = root.path().join("var");
    write_browser_interface(&interface_dir, &discovery_url);
    let mut environment = ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: process_var.clone(),
        durable_var_dir: Some(root.path().join("durable")),
        host_io_limits: InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    };
    environment.descriptor_snapshot = Some(environment.resolved_interface_descriptor().unwrap());
    let mut restart_environment = environment.clone();
    let room = BrowserRoom::load(environment).unwrap();
    let mut tools = browser_tools();
    tools.push("browser.history".into());
    tools.push("browser.history_resume".into());
    tools.sort();
    let (formation, kind) = formation("browser_session", &tools, "browser.navigate");
    let recorder = TestWorkerEnvironmentRecorder {
        records: Some(WorkerEnvironmentRecordStore::new(root.path().join("host"))),
        ..Default::default()
    };

    let navigated = response(received(
        room.receive(
            invocation(
                root.path(),
                1,
                "browser.navigate",
                json!({"url": "https://example.com/"}),
                &tools,
                &formation,
                &kind,
            ),
            &recorder,
        )
        .await,
    ));
    assert_eq!(navigated["ok"], true, "{navigated}");
    assert_eq!(navigated["body"]["snapshot"]["ref_count"], 4);
    assert_eq!(navigated["body"]["snapshot"]["source_movement_seq"], 1);
    assert_eq!(navigated["body"]["snapshot"]["target_id"], "TARGET1");
    assert_eq!(navigated["body"]["snapshot"]["frame_id"], "FRAME1");
    assert_eq!(navigated["body"]["snapshot"]["loader_id"], "LOAD1");
    assert_eq!(navigated["body"]["snapshot"]["frames"].as_array().unwrap().len(),5);
    assert_eq!(navigated["body"]["snapshot"]["frame_capture_complete"],true);
    let reference = navigated["body"]["snapshot"]["refs"][3]["ref"].clone();
    let click_arguments = json!({"ref":reference,"snapshot":false});
    let mut type_arguments = json!({"ref":navigated["body"]["snapshot"]["refs"][2]["ref"],"text":"contract input","clear":true,"snapshot":false});
    for (tool, arguments) in [("browser.click", &click_arguments), ("browser.type", &type_arguments)] {
        let actual = descriptor(tool);
        let validator = ToolArgumentValidator::compile(&actual.args_schema).unwrap();
        validator.validate(arguments).unwrap();
        for malformed in ["@e1", "@m0e1", "@m1e0", "@m01e1", "@m1e01", "@m1e1suffix"] {
            let mut invalid = arguments.clone();
            invalid["ref"] = json!(malformed);
            assert!(validator.validate(&invalid).is_err(), "{tool} admitted malformed ref {malformed}");
        }
    }
    let initial_live = recorder.live_states.lock().unwrap()[0].clone();
    assert_eq!(initial_live["activity"]["status"], "running");
    assert!(initial_live["presentation_intent"].is_null());
    assert_eq!(initial_live["viewer"]["target_id"], "TARGET1");
    assert_eq!(initial_live["url"], "about:blank");
    let record = recorder.records.as_ref().unwrap()
        .read(&formation.worker_environment_id).unwrap();
    assert_eq!(record.formation, formation);
    assert_eq!(record.lifecycle, WorkerEnvironmentLifecycle::Active);

    let before_history = recorder.live_states.lock().unwrap().clone();
    let live_path = root.path().join("durable/worker-environments").join(formation.worker_environment_id.as_str()).join("live-state.json");
    let before_private_history = std::fs::read(&live_path).unwrap();
    let history = response(received(room.receive(invocation(root.path(), 101, "browser.history", json!({"source":{"id":"explicit-history","content":vec![6;32]}}), &tools, &formation, &kind), &recorder).await));
    assert_eq!(history["ok"], false);
    assert!(history["error"]["message"].as_str().unwrap().contains("native browser host transport"));
    assert_eq!(*recorder.live_states.lock().unwrap(), before_history);
    assert_eq!(std::fs::read(&live_path).unwrap(), before_private_history);
    assert_eq!(recorder.records.as_ref().unwrap().read(&formation.worker_environment_id).unwrap().lifecycle, WorkerEnvironmentLifecycle::Active);

    let console = response(received(
        room.receive(
            invocation(
                root.path(),
                2,
                "browser.console",
                json!({"clear": true}),
                &tools,
                &formation,
                &kind,
            ),
            &recorder,
        )
        .await,
    ));
    assert_eq!(console["ok"], true, "{console}");
    assert_eq!(console["body"]["entries"][0]["text"], "console-proof");

    let clicked = response(received(
        room.receive(
            invocation(
                root.path(),
                3,
                "browser.click",
                click_arguments,
                &tools,
                &formation,
                &kind,
            ),
            &recorder,
        )
        .await,
    ));
    assert_eq!(clicked["ok"], true, "{clicked}");
    assert_eq!(clicked["body"]["x"], 170.0);
    assert_eq!(clicked["body"]["y"], 290.0);
    assert_eq!(
        clicked["body"]["pending_dialogs"][0]["message"],
        "continue?"
    );

    let dialog = response(received(
        room.receive(
            invocation(
                root.path(),
                4,
                "browser.dialog",
                json!({"action":"accept"}),
                &tools,
                &formation,
                &kind,
            ),
            &recorder,
        )
        .await,
    ));
    assert_eq!(dialog["ok"], true, "{dialog}");
    assert_eq!(dialog["body"]["dialog"]["type"], "confirm");

    let live = {
        let live_states = recorder.live_states.lock().unwrap();
        assert_eq!(live_states.iter().filter(|state| state["activity"]["status"] == "completed").count(), 4);
        live_states.last().cloned().unwrap()
    };
    assert_eq!(live["viewer"]["protocol"], "cdp");
    assert_eq!(live["viewer"]["target_id"], "TARGET1");
    assert_eq!(live["activity"]["source_movement_seq"], 4);
    assert_eq!(live["activity"]["status"], "completed");
    assert_eq!(live["tabs"][0]["target_id"], "TARGET1");
    assert_eq!(live["tabs"][0]["url"], "https://example.com/");
    let private_live_path = root.path().join("durable")
        .join("worker-environments")
        .join(formation.worker_environment_id.as_str())
        .join("live-state.json");
    let private_live: Value =
        serde_json::from_slice(&std::fs::read(private_live_path).unwrap()).unwrap();
    assert_eq!(private_live, live);

    let observed = response(received(room.receive(invocation(
        root.path(), 5, "browser.snapshot", json!({}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(observed["ok"], true);
    assert_ne!(observed["body"]["snapshot"]["refs"][0]["ref"], navigated["body"]["snapshot"]["refs"][0]["ref"]);
    let stale = response(received(room.receive(invocation(
        root.path(), 6, "browser.click", json!({"ref": navigated["body"]["snapshot"]["refs"][0]["ref"]}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(stale["ok"], false);
    assert!(stale["error"]["message"].as_str().unwrap().contains("stale or unknown"));
    assert_eq!(recorder.records.as_ref().unwrap().read(&formation.worker_environment_id).unwrap().lifecycle, WorkerEnvironmentLifecycle::Active);
    let failed_live = recorder.live_states.lock().unwrap().last().unwrap().clone();
    assert_eq!(failed_live["status"], "ready");
    assert_eq!(failed_live["activity"]["status"], "failed");
    assert_eq!(failed_live["detail"], stale["error"]["message"]);
    assert_eq!(failed_live["browser_context_id"], "CONTEXT1");
    assert_eq!(failed_live["target_id"], "TARGET1");
    let capture = response(received(room.receive(invocation(
        root.path(), 7, "browser.screenshot", json!({"full_page":true}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(capture["ok"], true, "{capture}");
    let image = &capture["body"]["media"][0];
    assert_eq!(image["mime_type"], "image/png");
    assert_eq!(image["artifact"]["payload_content_hash"], capture["body"]["content_hash"]);
    let media_bytes = std::fs::read(image["artifact"]["payload_path"].as_str().unwrap()).unwrap();
    assert_eq!(image["artifact"]["payload_path"], capture["body"]["path"]);
    let media_root = PathBuf::from(image["artifact"]["payload_path"].as_str().unwrap()).parent().unwrap().join("artifacts");
    assert!(std::fs::read_dir(media_root).unwrap().all(|entry| entry.unwrap().path().extension().is_some_and(|extension| extension == "json")));
    assert_eq!(blake3::hash(&media_bytes).to_hex().to_string(), capture["body"]["content_hash"].as_str().unwrap());
    assert_eq!(capture["body"]["source_movement_seq"], 7);
    let geometry = &capture["body"]["geometry"];
    assert_eq!(geometry["image"],json!({"width":4,"height":8,"media_type":"image/png","content_hash":capture["body"]["content_hash"]}));
    assert_eq!(geometry["capture"]["document_css_rect"],json!({"x":-12.0,"y":24.0,"width":1200.0,"height":2400.0}));
    assert_eq!(geometry["document"]["frame_id"],"FRAME1");
    assert_eq!(geometry["document"]["loader_id"],"LOAD1");
    assert_eq!(geometry["observed"]["device_pixel_ratio"],2.5);
    assert_eq!(geometry["coordinates"]["document_css_per_image_pixel"],json!({"x":300.0,"y":300.0}));
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["last_capture"]["geometry"],*geometry);
    assert_eq!(media_bytes,base64::engine::general_purpose::STANDARD.decode(image["payload_base64"].as_str().unwrap()).unwrap());


    let tabs = response(received(room.receive(invocation(
        root.path(), 8, "browser.tabs", json!({}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(tabs["body"]["tabs"].as_array().unwrap().len(), 1);
    assert_eq!(tabs["body"]["tabs"][0]["target_id"], "TARGET1");
    for (sequence, tool) in [(9,"browser.tab_select"),(10,"browser.tab_close")] {
        let refused = response(received(room.receive(invocation(
            root.path(), sequence, tool, json!({"target_id":"FOREIGN"}), &tools, &formation, &kind,
        ), &recorder).await));
        assert_eq!(refused["ok"], false);
        assert!(refused["error"]["message"].as_str().unwrap().contains("owned context"));
    }
    let cookies = response(received(room.receive(invocation(
        root.path(), 11, "browser.cookies", json!({"urls":["https://example.com/"]}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(cookies["body"]["cookies"][0]["name"], "room_cookie");

    let selected = response(received(room.receive(invocation(
        root.path(), 12, "browser.tab_select", json!({"target_id":"TARGET1"}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(selected["ok"], true, "{selected}");
    let intent = json!({"id":12,"target_id":"TARGET1","source":"tool","activate":true});
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["presentation_intent"], intent);
    let observed = response(received(room.receive(invocation(
        root.path(), 13, "browser.snapshot", json!({}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(observed["ok"], true, "{observed}");
    let refused = response(received(room.receive(invocation(
        root.path(), 14, "browser.tab_select", json!({"target_id":"FOREIGN"}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(refused["ok"], false);
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["presentation_intent"], intent);

    let back = response(received(room.receive(invocation(
        root.path(), 15, "browser.back", json!({}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(back["ok"], false);
    assert!(back["error"]["message"].as_str().unwrap().contains("previous history entry"));
    let capture = response(received(room.receive(invocation(
        root.path(), 16, "browser.screenshot", json!({"full_page":true}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(capture["ok"], true, "{capture}");
    assert_eq!(capture["body"]["target_id"], "TARGET1");
    for sequence in [17, 18] {
        let closed = response(received(room.receive(invocation(root.path(), sequence, "browser.close", json!({}), &tools, &formation, &kind), &recorder).await));
        assert_eq!(closed["ok"], sequence == 17);
        assert_eq!(recorder.records.as_ref().unwrap().read(&formation.worker_environment_id).unwrap().lifecycle, WorkerEnvironmentLifecycle::Active);
        assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["status"], "closed");
    }
    let reopened = response(received(room.receive(invocation(root.path(), 19, "browser.tab_new", json!({}), &tools, &formation, &kind), &recorder).await));
    assert_eq!(reopened["ok"], true, "{reopened}");
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["presentation_intent"],json!({"id":19,"target_id":"TARGET1","source":"tool","activate":true}));

    let observed = response(received(room.receive(invocation(
        root.path(), 22, "browser.snapshot", json!({}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(observed["ok"], true, "{observed}");
    type_arguments["ref"] = observed["body"]["snapshot"]["refs"][2]["ref"].clone();
    ToolArgumentValidator::compile(&descriptor("browser.type").args_schema).unwrap().validate(&type_arguments).unwrap();
    let typed = response(received(room.receive(invocation(
        root.path(), 23, "browser.type", type_arguments, &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(typed["ok"], true, "{typed}");
    assert_eq!(typed["body"]["typed_chars"], 14);
    let observed = response(received(room.receive(invocation(
        root.path(), 24, "browser.snapshot", json!({}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(observed["ok"], true, "{observed}");
    let changed = response(received(room.receive(invocation(
        root.path(), 25, "browser.eval", json!({"script":"location.reload()"}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(changed["ok"], true, "{changed}");
    let stale_document = response(received(room.receive(invocation(
        root.path(), 26, "browser.click", json!({"ref":observed["body"]["snapshot"]["refs"][0]["ref"]}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(stale_document["ok"], false);
    assert!(stale_document["error"]["message"].as_str().unwrap().contains("stale document"));
    let viewport = response(received(room.receive(invocation(root.path(), 27, "browser.screenshot", json!({}), &tools, &formation, &kind), &recorder).await));
    assert_eq!(viewport["ok"],true,"{viewport}");
    let geometry = &viewport["body"]["geometry"];
    assert_eq!(geometry["capture"]["scope"],"viewport");
    assert_eq!(geometry["capture"]["clip"],Value::Null);
    assert_eq!(geometry["capture"]["document_css_rect"],Value::Null);
    assert_eq!(geometry["coordinates"],json!({"image_space":"encoded_image_pixels","mapping_status":"unavailable_unclipped_surface"}));
    assert_eq!(geometry["observed"]["css_visual_viewport"],json!({"offsetX":5.0,"offsetY":8.0,"pageX":15.0,"pageY":88.0,"clientWidth":300.0,"clientHeight":200.0,"scale":2.0,"zoom":1.25}));
    assert_eq!((geometry["image"]["width"].as_u64(),geometry["image"]["height"].as_u64()),(Some(3),Some(2)));
    let mismatch = response(received(room.receive(invocation(root.path(), 28, "browser.screenshot", json!({"format":"jpeg"}), &tools, &formation, &kind), &recorder).await));
    assert_eq!(mismatch["ok"],false);
    assert!(mismatch["error"]["message"].as_str().unwrap().contains("image format or original bytes"));
    let scrolled = response(received(room.receive(invocation(root.path(), 29, "browser.eval", json!({"script":"window.scrollTo(0, 80)"}), &tools, &formation, &kind), &recorder).await));
    assert_eq!(scrolled["ok"],true);
    let unstable = response(received(room.receive(invocation(root.path(), 30, "browser.screenshot", json!({}), &tools, &formation, &kind), &recorder).await));
    assert_eq!(unstable["ok"],false);
    assert!(unstable["error"]["message"].as_str().unwrap().contains("metrics changed during capture"));
    let live = recorder.live_states.lock().unwrap().last().unwrap().clone();
    assert_eq!(live["activity"]["status"],"failed");
    assert_eq!(live["last_capture"]["source_movement_seq"],27);
    assert_eq!(live["last_capture"]["geometry"],*geometry);
    assert_eq!(recorder.records.as_ref().unwrap().read(&formation.worker_environment_id).unwrap().lifecycle, WorkerEnvironmentLifecycle::Active);
    let captures = process_var.join("worker-environments").join(formation.worker_environment_id.as_str()).join("captures");
    assert!(!captures.join("capture-30.png").exists());
    assert!(!captures.join("capture-28.jpg").exists());

    let retained = response(received(room.receive(invocation(root.path(),37,"browser.snapshot",json!({"target_id":"TARGET1"}),&tools,&formation,&kind),&recorder).await));
    assert_eq!(retained["ok"],true,"{retained}");
    let second = response(received(room.receive(invocation(root.path(),38,"browser.tab_new",json!({"url":"https://second.test/"}),&tools,&formation,&kind),&recorder).await));
    assert_eq!(second["body"]["target_id"],"TARGET2");
    let second_intent = json!({"id":38,"target_id":"TARGET2","source":"tool","activate":true});
    let direct = response(received(room.receive(invocation(root.path(),39,"browser.click",json!({"target_id":"TARGET1","ref":retained["body"]["snapshot"]["refs"][0]["ref"]}),&tools,&formation,&kind),&recorder).await));
    assert_eq!(direct["ok"],true,"{direct}"); assert_eq!(direct["body"]["target_id"],"TARGET1");
    let direct = response(received(room.receive(invocation(root.path(),40,"browser.eval",json!({"target_id":"TARGET1","script":"retained-target-proof"}),&tools,&formation,&kind),&recorder).await));
    assert_eq!(direct["ok"],true,"{direct}");
    let live = recorder.live_states.lock().unwrap().last().unwrap().clone();
    assert_eq!(live["target_id"],"TARGET2"); assert_eq!(live["presentation_intent"],second_intent);
    let foreign = response(received(room.receive(invocation(root.path(),41,"browser.snapshot",json!({"target_id":"FOREIGN"}),&tools,&formation,&kind),&recorder).await));
    assert_eq!(foreign["ok"],false);
    let closed = response(received(room.receive(invocation(root.path(),42,"browser.tab_close",json!({"target_id":"TARGET2"}),&tools,&formation,&kind),&recorder).await));
    assert_eq!(closed["ok"],true,"{closed}");
    let selected = response(received(room.receive(invocation(root.path(),43,"browser.tab_select",json!({"target_id":"TARGET1"}),&tools,&formation,&kind),&recorder).await));
    assert_eq!(selected["ok"],true,"{selected}");
    let intent = json!({"id":43,"target_id":"TARGET1","source":"tool","activate":true});


    drop(room);
    let methods = ws.await.unwrap();
    assert_eq!(methods.iter().filter(|method| *method == "Target.createBrowserContext").count(), 1);
    assert_eq!(methods.iter().filter(|method| *method == "Target.createTarget").count(), 3);
    assert_eq!(methods.iter().filter(|method| *method == "Target.activateTarget").count(), 2);
    assert!(methods.iter().any(|method| method == "retained-proof:/devtools/page/TARGET1"));
    assert!(!methods.iter().any(|method| method == "retained-proof:/devtools/page/TARGET2"));
    assert!(!methods.iter().any(|method| method == "Page.navigateToHistoryEntry"));
    assert_eq!(methods.iter().filter(|method| *method == "Page.captureScreenshot").count(),7);
    assert_eq!(methods.iter().filter(|method| *method == "Input.insertText").count(), 1);
    assert_eq!(
        methods
            .iter()
            .filter(|method| *method == "Page.navigate")
            .count(),
        1
    );
    assert_eq!(
        methods
            .iter()
            .filter(|method| *method == "Input.dispatchMouseEvent")
            .count(),
        6
    );

    let (discovery, recovered_ws) = spawn_context_cdp_mock(true, false, 0).await;
    write_browser_interface(&restart_environment.interface_dir, &discovery);
    restart_environment.descriptor_snapshot = None;
    restart_environment.var_dir = root.path().join("next-process-generation");
    let resumed = BrowserRoom::load(restart_environment.clone()).unwrap();
    let missing = response(received(resumed.receive(invocation(
        root.path(), 20, "browser.navigate", json!({"url": "https://example.com/"}), &tools, &formation, &kind,
    ), &recorder).await));
    assert_eq!(missing["ok"], false);
    assert!(missing["error"]["message"].as_str().unwrap().contains("exact target 'TARGET1' was preserved"));
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["target_id"], "TARGET1");
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["presentation_intent"], intent);
    assert_eq!(recorder.records.as_ref().unwrap().read(&formation.worker_environment_id).unwrap().lifecycle, WorkerEnvironmentLifecycle::Active);
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["status"], "error");
    drop(resumed);
    let recovered_methods = recovered_ws.await.unwrap();
    assert!(!recovered_methods.iter().any(|method| method == "Target.createBrowserContext" || method == "Target.createTarget"));

    let (discovery, lost_ws) = spawn_context_cdp_mock(false, false, 0).await;
    write_browser_interface(&restart_environment.interface_dir, &discovery);
    let lost = BrowserRoom::load(restart_environment.clone()).unwrap();
    let failed = response(received(lost.receive(invocation(root.path(), 21, "browser.screenshot", json!({}), &tools, &formation, &kind), &recorder).await));
    assert_eq!(failed["ok"], false);
    assert_eq!(recorder.records.as_ref().unwrap().read(&formation.worker_environment_id).unwrap().lifecycle, WorkerEnvironmentLifecycle::Failed);
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["status"], "error");
    drop(lost);
    assert!(lost_ws.await.unwrap().iter().all(|method| method == "Target.getBrowserContexts"));

    let (discovery, viewport_ws) = spawn_context_cdp_mock(true, false, 1).await;
    write_browser_interface(&restart_environment.interface_dir, &discovery);
    restart_environment.descriptor_snapshot = None;
    restart_environment.durable_var_dir = Some(root.path().join("viewport-context"));
    let viewport_room = BrowserRoom::load(restart_environment).unwrap();
    for (seq, tool, args) in [(31, "browser.viewport_set", json!({"width":10,"height":9000})), (32, "browser.viewport_reset", json!({})), (33, "browser.viewport_set", json!({"width":800,"height":600}))] {
        ToolArgumentValidator::compile(&descriptor(tool).args_schema).unwrap().validate(&args).unwrap();
        let result = response(received(viewport_room.receive(invocation(root.path(), seq, tool, args, &tools, &formation, &kind), &recorder).await));
        assert_eq!(result["ok"], true, "{result}");
        assert!(result["body"]["target_id"].is_null());
        let live = recorder.live_states.lock().unwrap().last().unwrap().clone();
        assert_eq!(live["status"], "closed"); assert_eq!(live["browser_context_id"], "CONTEXT1");
        assert!(live["tabs"].as_array().unwrap().is_empty()); assert!(live["presentation_intent"].is_null());
        assert_eq!(recorder.records.as_ref().unwrap().read(&formation.worker_environment_id).unwrap().lifecycle, WorkerEnvironmentLifecycle::Active);
        if seq == 31 { assert_eq!(live["viewport"]["pending"]["viewport"], json!({"width":240,"height":4096})); }
        if seq == 32 { assert!(live["viewport"]["pending"].is_null()); }
    }
    for bad in [json!({"width":0,"height":600}),json!({"width":1.5,"height":600}),json!({"width":800,"height":600,"session_id":"foreign"})] {
        assert!(ToolArgumentValidator::compile(&descriptor("browser.viewport_set").args_schema).unwrap().validate(&bad).is_err());
    }
    for (seq, tool, args) in [(34,"browser.tab_new",json!({"url":"https://example.com/"})),(35,"browser.viewport_set",json!({"width":800,"height":600})),(36,"browser.viewport_reset",json!({}))] {
        let result = response(received(viewport_room.receive(invocation(root.path(),seq,tool,args,&tools,&formation,&kind),&recorder).await));
        assert_eq!(result["ok"],true,"{result}");
        let live = recorder.live_states.lock().unwrap().last().unwrap().clone();
        assert!(live["viewport"]["pending"].is_null());
        assert_eq!(live["viewport"]["pages"]["TARGET1"]["model_viewport"]["revision"], if seq == 36 {36} else {33});
    }
    drop(viewport_room);
    let methods = viewport_ws.await.unwrap();
    assert_eq!(methods.iter().filter(|m| m.as_str() == "Target.createBrowserContext").count(),1);
    assert_eq!(methods.iter().filter(|m| m.as_str() == "Target.createTarget").count(),1);
    assert_eq!(methods.iter().filter(|m| m.as_str() == "Emulation.setDeviceMetricsOverride").count(),1);
    assert_eq!(methods.iter().filter(|m| m.as_str() == "Emulation.clearDeviceMetricsOverride").count(),1);
    assert!(methods.iter().position(|m| m == "Emulation.setDeviceMetricsOverride").unwrap() < methods.iter().position(|m| m == "Page.enable").unwrap());
    assert!(methods.iter().position(|m| m == "Emulation.setDeviceMetricsOverride").unwrap() < methods.iter().position(|m| m == "Page.navigate").unwrap());
}

#[tokio::test]
async fn completed_browser_effect_returns_artifact_when_evidence_writes_fail() {
    let (discovery_url, ws) = spawn_cdp_mock().await;
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("registry/interface").join(DOOR);
    write_browser_interface(&interface_dir, &discovery_url);
    let mut environment = ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: root.path().join("var"),
        durable_var_dir: Some(root.path().join("durable")),
        host_io_limits: InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    };
    environment.descriptor_snapshot = Some(environment.resolved_interface_descriptor().unwrap());
    let room = BrowserRoom::load(environment).unwrap();
    let tools = browser_tools();
    let (formation, kind) = formation("browser_session", &tools, "browser.navigate");
    let recorder = TestWorkerEnvironmentRecorder::default();

    let navigated = response(received(
        room.receive(
            invocation(
                root.path(),
                90,
                "browser.navigate",
                json!({"url": "https://example.com/"}),
                &tools,
                &formation,
                &kind,
            ),
            &recorder,
        )
        .await,
    ));
    assert_eq!(navigated["ok"], true, "{navigated}");

    recorder.fail_next_evidence_writes();
    let clicked = response(received(
        room.receive(
            invocation(
                root.path(),
                91,
                "browser.click",
                json!({"ref": navigated["body"]["snapshot"]["refs"][0]["ref"], "snapshot": false}),
                &tools,
                &formation,
                &kind,
            ),
            &recorder,
        )
        .await,
    ));
    assert_eq!(clicked["ok"], false, "{clicked}");
    assert_eq!(
        clicked["error"]["details"]["operation_body"]["result"],
        "browser.click"
    );
    assert_eq!(clicked["error"]["details"]["operation_body"]["x"], 20.0);
    assert_eq!(
        clicked["error"]["details"]["evidence_write_errors"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    drop(room);
    let methods = ws.await.unwrap();
    assert_eq!(
        methods
            .iter()
            .filter(|method| *method == "Input.dispatchMouseEvent")
            .count(),
        3
    );
}

#[test]
fn authenticated_control_failures_expose_only_length_and_digest() {
    let reflected = b"Authorization: Bearer reusable-browser-secret";
    let evidence = safe_control_body_evidence(reflected);
    assert!(evidence.contains(&format!("body_bytes={}", reflected.len())));
    assert!(evidence.contains("body_blake3="));
    assert!(!evidence.contains("reusable-browser-secret"));
}

#[test]
fn browser_room_refuses_plaintext_remote_control() {
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("registry/interface").join(DOOR);
    write_browser_interface(&interface_dir, "http://browser.example:9222/");
    let mut environment = ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: root.path().join("var"),
        durable_var_dir: Some(root.path().join("durable")),
        host_io_limits: InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    };
    environment.descriptor_snapshot = Some(environment.resolved_interface_descriptor().unwrap());
    let error = BrowserRoom::load(environment).err().unwrap();
    assert!(error.contains("https outside loopback"), "{error}");
}

#[tokio::test]
async fn exact_clone_id_executes_copied_navigate_operation_and_stays_exact_in_evidence() {
    let (discovery_url, ws) = spawn_cdp_mock().await;
    let root = tempfile::tempdir().unwrap();
    let interface_dir = root.path().join("registry/interface").join(DOOR);
    write_browser_interface(&interface_dir, &discovery_url);
    let mut environment = ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR),
        interface_dir,
        descriptor_snapshot: None,
        var_dir: root.path().join("var"),
        durable_var_dir: Some(root.path().join("durable")),
        host_io_limits: InterfaceHostIoLimits {
            max_control_frame_bytes: 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            max_manifest_bytes: 1024 * 1024,
            max_payload_bytes: 64 * 1024 * 1024,
        },
        private_reads: BTreeMap::new(),
        root_slots: BTreeMap::new(),
    };
    environment.descriptor_snapshot = Some(environment.resolved_interface_descriptor().unwrap());
    let room = BrowserRoom::load(environment).unwrap();
    let exact_tool = "browser.navigate.run_42";
    let (clone, tools, formation, kind) = cloned_scope("browser.navigate", exact_tool);
    let recorder = TestWorkerEnvironmentRecorder::default();
    let invocation = ToolProcessInvocation {
        request_id: 42,
        descriptor: clone.clone(),
        envelope: envelope(
            root.path(),
            42,
            exact_tool,
            json!({"url": "https://example.com/"}),
            &tools,
        ),
        source_movement_seq: 42,
        worker_environment: Some(formation.clone()),
        worker_environment_kind: Some(kind.clone()),
    };
    let response = response(received(room.receive(invocation, &recorder).await));
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["tool"], exact_tool);
    assert_eq!(response["label"], format!("{exact_tool}.result"));
    assert_eq!(response["body"]["result"], "browser.navigate");

    for (sequence, handler_config) in [
        (43, json!({})),
        (44, json!({"operation": 1})),
        (45, json!({"operation": "not_a_browser_operation"})),
    ] {
        let mut malformed = clone.clone();
        malformed.handler_config = handler_config;
        let result = room
            .receive(
                ToolProcessInvocation {
                    request_id: sequence,
                    descriptor: malformed,
                    envelope: envelope(root.path(), sequence, exact_tool, json!({}), &tools),
                    source_movement_seq: sequence,
                    worker_environment: Some(formation.clone()),
                    worker_environment_kind: Some(kind.clone()),
                },
                &recorder,
            )
            .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("malformed browser handler_config unexpectedly executed"),
        };
        let message = match error {
            ToolProcessError::Capability(message) | ToolProcessError::Unavailable(message) => {
                message
            }
        };
        assert!(message.contains(exact_tool), "{message}");
        assert!(message.contains("handler_config"), "{message}");
    }

    drop(room);
    let methods = ws.await.unwrap();
    assert_eq!(
        methods
            .iter()
            .filter(|method| *method == "Page.navigate")
            .count(),
        1
    );
}

#[tokio::test]
async fn native_transport_authenticates_admitted_body_and_preserves_partial_and_chunked_frames() {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use crate::native_transport::{NativeHostConfig, NativeTransport, PRIVATE_CREDENTIAL, HOST_ROOT_SLOT};
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket_path = root.join("host.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let key = [71u8; 32];
    let generation = "00112233445566778899aabbccddeeff";
    let challenge = "11223344556677889900aabbccddeeff";
    let credential = root.join("credential");
    std::fs::write(&credential, serde_json::to_vec(&json!({"schema_version":1,"host_root_slot":HOST_ROOT_SLOT,"host_generation":generation,"socket_path":socket_path,"auth_key":base64::engine::general_purpose::STANDARD.encode(key)})).unwrap()).unwrap();
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    let interface_dir = root.join("registry/interface").join(DOOR);
    write_browser_interface(&interface_dir, "http://127.0.0.1:1");
    let environment = ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR), interface_dir, descriptor_snapshot: None,
        var_dir: root.join("var"), durable_var_dir: Some(root.join("durable")),
        private_reads: BTreeMap::from([(PRIVATE_CREDENTIAL.to_string(), credential)]), root_slots: BTreeMap::new(),
        host_io_limits: InterfaceHostIoLimits { max_control_frame_bytes:1048576,max_stderr_bytes:262144,max_manifest_bytes:1048576,max_payload_bytes:67108864 },
    };
    let mut public_descriptor_evidence = Vec::new();
    let registry = WorkerEnvKindRegistry::load_from_dir(&registry_root().join("worker_environment_kind")).unwrap();
    for (index, (source_id, exact_id, exact_door, arguments)) in [
        ("browser.screenshot", "browser.screenshot__chat_fixture", "system_browser__chat_fixture", json!({"format":"png"})),
        ("browser.download_wait", "browser.download_wait__chat_fixture", "door", json!({"download_id":"1".repeat(32)})),
    ].into_iter().enumerate() {
        use sha2::Digest;
        let source_path = format!("registry/tool/{source_id}/tool.json");
        let source_bytes = std::fs::read(registry_root().join("tool").join(source_id).join("tool.json")).unwrap();
        assert!(source_bytes.len() <= 65_536);
        let mut raw: Value = serde_json::from_slice(&source_bytes).unwrap();
        raw["id"] = json!(exact_id); raw["interface"] = json!(exact_door);
        let clone: ToolDescriptor = serde_json::from_value(raw.clone()).unwrap();
        let normalized = serde_json::to_value(&clone).unwrap();
        let canonical = octopus_descriptors::canonical_bytes(&normalized);
        assert!(canonical.len() <= 131_072);
        let hashes = BTreeMap::from([(exact_id.to_string(), blake3::hash(&canonical).to_hex().to_string())]);
        let bins = BTreeMap::from([(exact_id.to_string(), clone.process.bin.clone())]);
        let tools = vec![exact_id.to_string()];
        let kinds = vec![WorkerEnvironmentKindId::new("browser_session")];
        let door = InterfaceId::new(exact_door); let worker = InterfaceId::new(WORKER);
        let formed = registry.form(&WorkerEnvironmentFormationRequest {
            door_additional_root_bindings: &Default::default(),
            door_interface: &door, door_descriptor_hash: "browser-test-door-hash", door_driver_ref: "system_browser",
            tool: exact_id, tool_descriptor_hashes: &hashes, tool_process_bins: &bins, tool_worker_environment_kinds: &kinds,
            worker: &worker, ee: "ee_browser_test", run: "run_browser_test", granted_worker_environment_kinds: &kinds,
            granted_tools: &tools, door_root_bindings: &BTreeMap::new(), door_instance_dir: Path::new("."), ee_root_slot_allowance: None,
        }).unwrap();
        let sequence = 100 + index as u64;
        let mut input = envelope(&root, sequence, exact_id, arguments, &tools);
        input.metadata = Some(InvocationMetadata::new(GrantProjection {
            tools: tools.clone(), worker_environment_kinds: vec!["browser_session".into()], ..GrantProjection::default()
        }).with_runtime(RuntimeInvocationMetadata {
            run: RunId::new("run_browser_test"), ee: EeId::new("ee_browser_test"), worker,
            checkpoint: CheckpointId::new(format!("cp_{sequence}")), interface: door, epoch: 0, run_record_hash: None,
        }));
        let (kind, _) = registry.get(&kinds[0]).unwrap();
        let admitted = crate::decode_room_invocation(ToolProcessInvocation {
            request_id: sequence, descriptor: clone, envelope: input, source_movement_seq: sequence,
            worker_environment: Some(formed.clone()), worker_environment_kind: Some((*kind).clone()),
        }).unwrap_or_else(|_| panic!("source clone must pass existing browser invocation admission"));
        assert_eq!(admitted.descriptor_bytes, canonical);
        let pin = formed.tool_descriptor_hashes.get(exact_id).unwrap();
        assert_eq!(blake3::hash(&admitted.descriptor_bytes).to_hex().as_str(), pin);
        public_descriptor_evidence.push(json!({
            "source_path":source_path,"source_id":source_id,"source_sha256":format!("{:x}",Sha256::digest(&source_bytes)),
            "source_bytes_base64":base64::engine::general_purpose::STANDARD.encode(source_bytes),
            "raw_clone":raw,"normalized_descriptor":normalized,
            "normalized_canonical_base64":base64::engine::general_purpose::STANDARD.encode(admitted.descriptor_bytes),
            "exact_id":exact_id,"door":exact_door,"worker_environment_id":formed.worker_environment_id,
            "formation_pin":formed.formation_pin,"tool_descriptor_hash":pin,
            "tool_descriptor_contract_version":<ToolDescriptor as octopus_descriptors::Descriptor>::CONTRACT_VERSION,
        }));
    }
    let mut tools = browser_tools(); tools.push("browser.wait_complete".into());
    let (formation, kind) = formation("browser_session", &tools, "browser.navigate");
    let arguments = json!({"url":"https://example.com/","wait_until":"none","snapshot":false});
    let decoded = crate::decode_room_invocation(invocation(&root, 15, "browser.navigate", arguments.clone(), &tools, &formation, &kind)).unwrap_or_else(|_| panic!("fixture invocation must pass existing browser admission"));
    let expected_formation = serde_json::to_value(&formation).unwrap();
    let waiter = json!({"wait_id":"c".repeat(32),"host_generation":generation,"worker_environment_id":formation.worker_environment_id,"formation_pin":formation.formation_pin,"browser_context_id":"CONTEXT1","target_id":"TARGET1","debugger_epoch":0,"start_source_movement_seq":15,"start_artifact_content_hash":"d".repeat(64),"condition_digest":"e".repeat(64)});
    let expected_waiter = waiter.clone();
    let server = tokio::spawn(async move {
        let (owner_read, mut owner_write) = native_fixture_open(&listener, &key, generation, challenge).await;
        let mut owner_reader = BufReader::new(owner_read);
        let begin = native_fixture_body(&mut owner_reader, &key).await;
        assert_eq!(begin["schema_version"], 2); assert_eq!(begin["request_kind"], "invocation");
        assert_eq!(begin["executor"]["connection_role"], "owner");
        assert_eq!(begin["operation"]["method"], "OctopusBrowser.beginInvocation");
        assert_eq!(begin["formation"], expected_formation); assert_eq!(begin["arguments"], arguments);
        let descriptor_bytes = base64::engine::general_purpose::STANDARD.decode(begin["tool_descriptor"].as_str().unwrap()).unwrap();
        assert_eq!(blake3::hash(&descriptor_bytes).to_hex().as_str(), begin["formation"]["tool_descriptor_hashes"]["browser.navigate"].as_str().unwrap());
        assert_eq!(serde_json::from_slice::<Value>(&descriptor_bytes).unwrap()["id"], "browser.navigate");
        owner_write.write_all(format!("{}\n",json!({"id":1,"result":{"invocation_handle":"a".repeat(32)}})).as_bytes()).await.unwrap();
        let (read, mut write) = native_fixture_open(&listener, &key, generation, challenge).await;
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(frame["payload"].as_str().unwrap()).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap(); mac.update(&bytes);
        assert_eq!(frame["mac"], mac.finalize().into_bytes().iter().map(|byte| format!("{byte:02x}")).collect::<String>());
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["schema_version"], 2); assert_eq!(body["executor"]["connection_role"], "member");
        assert_eq!(body["executor"]["instance_nonce"], begin["executor"]["instance_nonce"]);
        assert_eq!(body["invocation_handle"], "a".repeat(32));
        assert_eq!(body["formation"], expected_formation);
        assert_eq!(body["arguments"], arguments);
        assert_eq!(body["tool"], json!({"id":"browser.navigate","operation":"navigate"}));
        assert_eq!(body["source_movement_seq"], 15);
        assert_eq!(body["effective_allow_network"], true);
        assert_eq!(body["operation"], json!({"target_id":null,"session_id":null,"method":"Target.createBrowserContext","params":{"disposeOnDetach":false}}));
        write.write_all(b"{\"id\":1,").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        write.write_all(b"\"result\":{}}\n").await.unwrap();
        let second = native_fixture_body(&mut reader, &key).await;
        assert_eq!(second["request_id"], 2); assert_eq!(second["invocation_handle"], body["invocation_handle"]);
        let response = serde_json::to_vec(&json!({"id":2,"result":{"data":"x".repeat(8*1024*1024)}})).unwrap();
        let total = response.chunks(8*1024*1024).len();
        for (index, chunk) in response.chunks(8*1024*1024).enumerate() {
            write.write_all(format!("{}\n",json!({"type":"response_chunk","id":2,"index":index,"total":total,"data":base64::engine::general_purpose::STANDARD.encode(chunk)})).as_bytes()).await.unwrap();
        }
        let ended = native_fixture_body(&mut owner_reader, &key).await;
        assert_eq!(ended["operation"]["method"], "OctopusBrowser.endInvocation");
        assert_eq!(ended["invocation_handle"], "a".repeat(32));
        owner_write.write_all(b"{\"id\":2,\"result\":{\"ended\":true}}\n").await.unwrap();
        drop(write);
        let (observer_read, mut observer_write) = native_fixture_open(&listener, &key, generation, challenge).await;
        let mut observer_reader = BufReader::new(observer_read);
        let admitted = native_fixture_body(&mut observer_reader, &key).await;
        assert_eq!(admitted["tool"]["operation"], "wait_complete"); assert_eq!(admitted["effective_allow_network"], false);
        assert_eq!(admitted["operation"]["method"], "OctopusBrowser.beginInvocation");
        observer_write.write_all(format!("{}\n",json!({"id":1,"result":{"invocation_handle":"f".repeat(32)}})).as_bytes()).await.unwrap();
        let (observer_member, mut observer_result) = native_fixture_open(&listener, &key, generation, challenge).await;
        let mut observer_member = BufReader::new(observer_member);
        let request = native_fixture_body(&mut observer_member, &key).await;
        assert_eq!(request["arguments"],json!({"waiter":expected_waiter}));
        assert_eq!(request["operation"],json!({"target_id":null,"session_id":null,"method":"OctopusBrowser.completeWait","params":{"waiter":expected_waiter}}));
        let outcome = json!({"waiter":expected_waiter,"status":"satisfied","observation":{"url":"https://example.com/"}});
        observer_result.write_all(format!("{}\n",json!({"id":1,"result":outcome})).as_bytes()).await.unwrap();
        let captured = native_fixture_body(&mut observer_reader, &key).await;
        assert_eq!(captured["operation"]["method"],"OctopusBrowser.endInvocation");
        assert_eq!(captured["operation"]["params"]["captured_wait"]["wait_id"],expected_waiter["wait_id"]);
        use sha2::Digest;
        assert_eq!(captured["operation"]["params"]["captured_wait"]["outcome_digest"],format!("{:x}",Sha256::digest(octopus_descriptors::canonical_bytes(&outcome))));
        observer_write.write_all(b"{\"id\":2,\"result\":{\"ended\":true}}\n").await.unwrap();
        let (release_read, mut release_write) = native_fixture_open(&listener, &key, generation, challenge).await;
        let release = native_fixture_body(&mut BufReader::new(release_read), &key).await;
        assert_eq!(release["request_kind"],"release_formation"); assert_eq!(release["release_mode"],"cold_recovery");
        assert_eq!(release["formation"],expected_formation); assert!(release.get("tool").is_none());
        release_write.write_all(b"{\"id\":1,\"result\":{\"status\":\"released\"}}\n").await.unwrap();
    });
    let transport = NativeTransport::new(&environment, &NativeHostConfig { host_root_slot:HOST_ROOT_SLOT.into(),request_timeout_ms:5000,navigation_timeout_ms:5000 }).unwrap();
    transport.authorize(&decoded).await.unwrap();
    let mut connection = transport.connect(None).await.unwrap();
    connection.send(1, None, "Target.createBrowserContext", json!({"disposeOnDetach":false})).await.unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_millis(10), connection.next()).await.is_err());
    assert_eq!(connection.next().await.unwrap(), json!({"id":1,"result":{}}));
    connection.send(2,None,"Target.getTargets",json!({})).await.unwrap();
    assert_eq!(connection.next().await.unwrap()["result"]["data"].as_str().unwrap().len(), 8*1024*1024);
    transport.end(None).await.unwrap();
    let descriptor_path=environment.interface_dir.join("interface.json");
    let mut descriptor:Value=serde_json::from_slice(&std::fs::read(&descriptor_path).unwrap()).unwrap();
    descriptor["interface_driver_config"]["private_config"]["browser_room"]["transport"]=json!({"wire":"browser.native.v1","host_root_slot":HOST_ROOT_SLOT,"request_timeout_ms":5000,"navigation_timeout_ms":5000});
    descriptor["interface_driver_config"]["private_config"]["browser_room"]["wait_completion"]=json!({"request_tools":["browser.wait"],"completion_tool":"browser.wait_complete"});
    std::fs::write(&descriptor_path,serde_json::to_vec(&descriptor).unwrap()).unwrap();
    let observer = crate::browser_wait_completion::BrowserWaitCompletion::load(environment).unwrap();
    let artifact = observer.receive(invocation(&root,16,"browser.wait_complete",json!({"waiter":waiter}),&tools,&formation,&kind)).await.unwrap_or_else(|_|panic!("actual completion handler must return its ordinary artifact"));
    let result:Value=serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(result["ok"],true,"{result}"); assert_eq!(result["tool"],"browser.wait_complete"); assert_eq!(result["body"]["waiter"],waiter); assert_eq!(result["body"]["completion_source_movement_seq"],16);
    use octopus_tool_door_runtime::{ToolProcessHandler,WorkerEnvironmentReleaseContext,WorkerEnvironmentReleaseMode};
    assert!(observer.release_with_context(WorkerEnvironmentReleaseContext {formation:&formation,kind:&kind,mode:WorkerEnvironmentReleaseMode::ColdRecovery}).await.is_ok());
    server.await.unwrap();
    assert!(connection.next().await.unwrap_err().contains("disconnected"));
    assert!(connection.send(3,None,"Target.getTargets",json!({})).await.is_err());
    println!("OCTOPUS_BROWSER_TOOL_PIN_EVIDENCE={}", json!({"schema_version":1,"producer":"native_transport_authenticates_admitted_body_and_preserves_partial_and_chunked_frames","crate_version":env!("CARGO_PKG_VERSION"),"pairs":public_descriptor_evidence}));
}


#[tokio::test]
async fn native_download_import_outlives_process_generation_and_closed_origin() {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use crate::native_transport::{PRIVATE_CREDENTIAL, HOST_ROOT_SLOT};

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = root.join("host.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let key = [73u8; 32];
    let generation = "00112233445566778899aabbccddeeff";
    let challenge = "11223344556677889900aabbccddeeff";
    let download_id = "aabbccddeeff00112233445566778899";
    let payload = b"download from the closed origin page";
    let sha256 = format!("{:x}", Sha256::digest(payload));
    let credential = root.join("credential");
    std::fs::write(&credential, serde_json::to_vec(&json!({"schema_version":1,"host_root_slot":HOST_ROOT_SLOT,"host_generation":generation,"socket_path":socket,"auth_key":base64::engine::general_purpose::STANDARD.encode(key)})).unwrap()).unwrap();
    std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
    let interface_dir = root.join("registry/interface").join(DOOR);
    write_browser_interface(&interface_dir, "http://127.0.0.1:1");
    let mut config: Value = serde_json::from_slice(&std::fs::read(interface_dir.join("interface.json")).unwrap()).unwrap();
    config["interface_driver_config"]["private_config"]["browser_room"]["transport"] = json!({"wire":"browser.native.v1","host_root_slot":HOST_ROOT_SLOT,"request_timeout_ms":5000,"navigation_timeout_ms":5000});
    std::fs::write(interface_dir.join("interface.json"), serde_json::to_vec(&config).unwrap()).unwrap();
    let process_var = root.join("generation");
    let durable = root.join("durable");
    std::fs::create_dir(&process_var).unwrap();
    let environment = ToolProcessEnvironment {
        interface: InterfaceId::new(DOOR), interface_dir, descriptor_snapshot: None,
        var_dir: process_var.clone(), durable_var_dir: Some(durable.clone()),
        private_reads: BTreeMap::from([(PRIVATE_CREDENTIAL.to_string(), credential.clone())]), root_slots: BTreeMap::new(),
        host_io_limits: InterfaceHostIoLimits { max_control_frame_bytes:1048576,max_stderr_bytes:262144,max_manifest_bytes:1048576,max_payload_bytes:67108864 },
    };
    let tools = browser_tools();
    let (formation, kind) = formation("browser_session", &tools, "browser.download_wait");
    let state_path = durable.join("worker-environments").join(formation.worker_environment_id.as_str()).join("live-state.json");
    std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
    let current_tab = json!({"target_id":"TARGET2","type":"page","title":"Other page","url":"https://example.com/other"});
    std::fs::write(&state_path, serde_json::to_vec(&json!({"schema_version":1,"worker_environment_id":formation.worker_environment_id,"worker_environment_kind":formation.worker_environment_kind,"status":"ready","generation":1,"browser_context_id":"CONTEXT1","target_id":"TARGET2","url":"https://example.com/other","title":"Other page","viewer":{"protocol":"browser.native.v1","engine_handle":"engine1","host_generation":generation,"browser_context_id":"CONTEXT1","target_id":"TARGET2"},"tabs":[current_tab]})).unwrap()).unwrap();
    let original_pin = formation.formation_pin.clone();
    let expected_formation = serde_json::to_value(&formation).unwrap();
    let server = tokio::spawn(async move {
        let (owner_read, mut owner_write) = native_fixture_open(&listener, &key, generation, challenge).await;
        let mut owner_reader = BufReader::new(owner_read);
        let begin = native_fixture_body(&mut owner_reader, &key).await;
        assert_eq!(begin["operation"]["method"], "OctopusBrowser.beginInvocation");
        assert_eq!(begin["formation"], expected_formation);
        owner_write.write_all(format!("{}\n",json!({"id":1,"result":{"invocation_handle":"b".repeat(32)}})).as_bytes()).await.unwrap();
        let (read, mut write) = native_fixture_open(&listener, &key, generation, challenge).await;
        let sign = |bytes: &[u8]| {
            let mut mac = Hmac::<Sha256>::new_from_slice(&key).unwrap(); mac.update(bytes);
            mac.finalize().into_bytes().iter().map(|byte| format!("{byte:02x}")).collect::<String>()
        };
        let mut reader = BufReader::new(read);
        let steps = [
            ("Target.getBrowserContexts", json!({"browserContextIds":["CONTEXT1"]})),
            ("Target.getTargets", json!({"targetInfos":[{"targetId":"TARGET2","type":"page","browserContextId":"CONTEXT1","title":"Other page","url":"https://example.com/other"}]})),
            ("OctopusBrowser.waitDownload", json!({"download_id":download_id,"target_id":"TARGET1","browser_context_id":"CONTEXT1","host_generation":generation,"status":"completed","bytes":payload.len(),"max_bytes":1024,"sha256":sha256,"native_event":{"state":"completed"},"arm":{"formation_pin":original_pin,"source_movement_seq":1}})),
            ("OctopusBrowser.readDownload", json!({"download_id":download_id,"target_id":"TARGET1","browser_context_id":"CONTEXT1","offset":0,"bytes":payload.len(),"sha256":sha256,"payload_base64":base64::engine::general_purpose::STANDARD.encode(payload)})),
            ("OctopusBrowser.finishDownload", json!({"download_id":download_id,"target_id":"TARGET1","browser_context_id":"CONTEXT1","status":"imported"})),
        ];
        for (index, (method, result)) in steps.into_iter().enumerate() {
            let mut line = String::new(); reader.read_line(&mut line).await.unwrap();
            let frame: Value = serde_json::from_str(&line).unwrap();
            let bytes = base64::engine::general_purpose::STANDARD.decode(frame["payload"].as_str().unwrap()).unwrap();
            assert_eq!(frame["mac"], sign(&bytes));
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["formation"], expected_formation);
            assert_eq!(body["source_movement_seq"], 10);
            assert_eq!(body["tool"], json!({"id":"browser.download_wait","operation":"download_wait"}));
            assert_eq!(body["arguments"], json!({"download_id":download_id,"timeout_ms":100}));
            assert_eq!(body["operation"]["target_id"], Value::Null);
            assert_eq!(body["operation"]["method"], method);
            if index >= 2 { assert_eq!(body["operation"]["params"]["download_id"], download_id); }
            write.write_all(format!("{}\n", json!({"id":index+1,"result":result})).as_bytes()).await.unwrap();
        }
        let end = native_fixture_body(&mut owner_reader, &key).await;
        assert_eq!(end["operation"]["method"], "OctopusBrowser.endInvocation"); assert_eq!(end["invocation_handle"], "b".repeat(32));
        owner_write.write_all(b"{\"id\":2,\"result\":{\"ended\":true}}\n").await.unwrap();
        listener
    });
    let room = BrowserRoom::load(environment.clone()).unwrap();
    let recorder = TestWorkerEnvironmentRecorder::default();
    let mut call = invocation(&root, 10, "browser.download_wait", json!({"download_id":download_id,"timeout_ms":100}), &tools, &formation, &kind);
    call.envelope.artifact_file=InterfaceArtifactFile::write(root.join("correlated-input"),"tool.call",&serde_json::to_vec(&json!({"tool":"browser.download_wait","arguments":{"download_id":download_id,"timeout_ms":100},"correlation_id":"original-download-call"})).unwrap()).unwrap();
    let content=ContentHash::of_bytes(&std::fs::read(call.envelope.artifact_file.manifest_path()).unwrap());
    call.envelope.artifact=ArtifactRef{id:ArtifactId::new(format!("tool.call:{}",content.to_hex())),content};
    let source = call.envelope.artifact.clone();
    let result_file = received(room.receive(call, &recorder).await);
    let result = response(result_file.clone());
    let (raw, published, _) = room.prepare_download_output(&result_file,&source,10).unwrap().unwrap();
    assert_eq!(raw.read_payload_bytes().unwrap(),payload);
    assert_eq!(raw.read_manifest().unwrap().payload_path, PathBuf::from(result["body"]["artifact"]["payload_path"].as_str().unwrap()));
    assert!(std::fs::read_dir(raw.manifest_path().parent().unwrap()).unwrap().all(|entry| entry.unwrap().path().extension().is_some_and(|extension| extension == "json")));
    assert!(raw.manifest_path().starts_with(&process_var));
    assert!(published.body.as_ref().unwrap().get("path").is_none());
    let published_ref:ArtifactRef=serde_json::from_value(published.body.as_ref().unwrap()["artifact"].clone()).unwrap();
    assert_eq!(published_ref.content,ContentHash::of_bytes(&std::fs::read(raw.manifest_path()).unwrap()));
    let mut alias:octopus_tool_handlers::ToolResultArtifact=serde_json::from_slice(&result_file.read_payload_bytes().unwrap()).unwrap();
    alias.tool="browser.download_wait__exact_task".into(); alias.label=format!("{}.result",alias.tool);
    let alias_file=InterfaceArtifactFile::write(process_var.join("alias"),&alias.label,&serde_json::to_vec(&alias).unwrap()).unwrap();
    assert!(room.prepare_download_output(&alias_file,&source,10).unwrap().is_some());
    assert!(room.prepare_download_output(&result_file,&source,11).is_err());
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["body"]["target_id"], "TARGET1");
    assert_eq!(result["body"]["native_copy_released"], true);
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["target_id"], "TARGET2");
    let imported = PathBuf::from(result["body"]["artifact"]["payload_path"].as_str().unwrap());
    let manifests = imported.parent().unwrap().join("artifacts");
    let manifest = std::fs::read_dir(manifests).unwrap().next().unwrap().unwrap().path();
    drop(room); let listener = server.await.unwrap();
    std::fs::remove_dir_all(process_var).unwrap();
    assert_eq!(std::fs::read(&imported).unwrap(), payload);
    assert_eq!(InterfaceArtifactFile::new(manifest).unwrap().read_payload_bytes().unwrap(), payload);
    assert_eq!(result["body"]["artifact"]["payload_content_hash"], blake3::hash(payload).to_hex().as_str());
    let next_generation = "ffeeddccbbaa99887766554433221100";
    let mut auth: Value = serde_json::from_slice(&std::fs::read(&credential).unwrap()).unwrap(); auth["host_generation"] = json!(next_generation); std::fs::write(&credential, serde_json::to_vec(&auth).unwrap()).unwrap();
    let server = tokio::spawn(async move {
        let mut tasks = Vec::new();
        for _ in 0..3 {
            let (read, mut write) = native_fixture_open(&listener, &key, next_generation, challenge).await;
            tasks.push(tokio::spawn(async move {
                let mut reader = BufReader::new(read); let mut created = false;
                while !reader.fill_buf().await.unwrap().is_empty() {
                    let body = native_fixture_body(&mut reader, &key).await;
                    let operation = &body["operation"]; let method = operation["method"].as_str().unwrap();
                    let result = match method {
                        "OctopusBrowser.beginInvocation" => json!({"invocation_handle":"c".repeat(32)}), "OctopusBrowser.endInvocation" => json!({"ended":true}),
                        "Target.createBrowserContext" => { assert_eq!(body["tool"]["operation"], "tab_new"); json!({"browserContextId":"FRESH_CONTEXT","engine_handle":"fresh-engine","host_generation":next_generation}) },
                        "Target.getBrowserContexts" => json!({"browserContextIds":["FRESH_CONTEXT"]}),
                        "Target.createTarget" => { assert!(!created); created = true; assert_eq!(body["source_movement_seq"],13); assert_eq!(operation["params"]["browserContextId"], "FRESH_CONTEXT"); assert_eq!(operation["params"]["url"], "https://fresh.test/"); json!({"targetId":"FRESH_TARGET"}) },
                        "Target.getTargets" => json!({"targetInfos":[{"targetId":"FRESH_TARGET","type":"page","browserContextId":"FRESH_CONTEXT","title":"Fresh","url":"https://fresh.test/"}]}),
                        "OctopusBrowser.getViewportState" => json!({"schema_version":1,"worker_environment_id":body["formation"]["worker_environment_id"],"formation_pin":body["formation"]["formation_pin"],"host_generation":next_generation,"engine_handle":"fresh-engine","browser_context_id":"FRESH_CONTEXT","target_id":"FRESH_TARGET","last_intent":null,"pending":null,"model_viewport":{"revision":0,"emulatedSize":null},"application":null,"debugger_epoch":1}),
                        "Page.enable" | "Runtime.enable" | "Log.enable" | "Network.enable" => json!({}), _ => panic!("unexpected browser operation: {method}"),
                    };
                    let mut reply = json!({"id":body["request_id"],"result":result});
                    if method == "Target.createBrowserContext" && body["source_movement_seq"] == 12 { reply = json!({"id":body["request_id"],"error":{"code":"network_denied","message":"denied"}}); }
                    if operation["target_id"].is_string() { reply["frame_inventory"] = json!({"target_id":operation["target_id"],"sessions":[]}); }
                    write.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
                }
            }));
        }
        for task in tasks { task.await.unwrap(); }
    });
    let room = BrowserRoom::load(environment).unwrap();
    for (seq, tool, args) in [(11,"browser.navigate",json!({"url":"https://fresh.test/"})), (12,"browser.tab_new",json!({"url":"https://fresh.test/"}))] {
        let result = response(received(room.receive(invocation(&root,seq,tool,args,&tools,&formation,&kind),&recorder).await)); assert_eq!(result["ok"],false,"{result}");
        let states = recorder.live_states.lock().unwrap(); let live = states.last().unwrap(); assert_eq!(live["target_id"],"TARGET2"); assert_eq!(live["viewer"]["host_generation"],generation);
    }
    let fresh = response(received(room.receive(invocation(&root,13,"browser.tab_new",json!({"url":"https://fresh.test/"}),&tools,&formation,&kind),&recorder).await)); assert_eq!(fresh["ok"],true,"{fresh}");
    assert_eq!(fresh["body"]["target_id"],"FRESH_TARGET"); assert_eq!(fresh["body"]["browser_context_id"],"FRESH_CONTEXT"); assert_eq!(fresh["body"]["previous_context"]["host_generation"],generation); assert_eq!(fresh["body"]["host_generation"],next_generation);
    let old = response(received(room.receive(invocation(&root,14,"browser.navigate",json!({"target_id":"TARGET2","url":"https://old.test/"}),&tools,&formation,&kind),&recorder).await)); assert_eq!(old["ok"],false);
    assert_eq!(recorder.live_states.lock().unwrap().last().unwrap()["target_id"],"FRESH_TARGET");
    drop(room); tokio::time::timeout(std::time::Duration::from_secs(5),server).await.unwrap().unwrap();
}

#[tokio::test]
async fn upload_resume_requires_exact_verified_current_file_binding_and_never_replays_consumption() {
    use crate::native_transport::PRIVATE_CREDENTIAL;
    use octopus_interfaces::{ArtifactBindingPayload, CheckpointAdmissionMetadata};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir().unwrap(); let root = temporary.path().canonicalize().unwrap();
    std::fs::set_permissions(&root,std::fs::Permissions::from_mode(0o700)).unwrap();
    let private = root.join("generation"); let durable = root.join("durable"); let pending_root = durable.join("upload-pending");
    for directory in [&private,&durable,&pending_root] { std::fs::create_dir(directory).unwrap(); std::fs::set_permissions(directory,std::fs::Permissions::from_mode(0o700)).unwrap(); }
    use tokio::io::{AsyncWriteExt, BufReader};
    let socket_path = root.join("host.sock"); let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    std::fs::set_permissions(&socket_path,std::fs::Permissions::from_mode(0o600)).unwrap();
    let key = [91u8;32]; let generation = "4".repeat(32);
    let credential = root.join("credential");
    std::fs::write(&credential,serde_json::to_vec(&json!({"schema_version":1,"host_root_slot":crate::native_transport::HOST_ROOT_SLOT,"host_generation":generation,"socket_path":socket_path,"auth_key":base64::engine::general_purpose::STANDARD.encode(key)})).unwrap()).unwrap();
    std::fs::set_permissions(&credential,std::fs::Permissions::from_mode(0o600)).unwrap();
    let server = tokio::spawn(async move {
        let mut previous_instance = None;
        for sequences in [vec![2u64,3],vec![4u64]] {
            let (read,mut write) = native_fixture_open(&listener,&key,&generation,&"5".repeat(32)).await;
            let mut reader = BufReader::new(read); let mut instance = None; let mut next_id = 1u64;
            for sequence in sequences {
                let begin = native_fixture_body(&mut reader,&key).await;
                assert_eq!(begin["schema_version"],2); assert_eq!(begin["request_kind"],"invocation");
                assert_eq!(begin["request_id"],next_id); next_id += 1;
                assert_eq!(begin["operation"]["method"],"OctopusBrowser.beginInvocation");
                assert_eq!(begin["source_movement_seq"],sequence); assert_eq!(begin["tool"]["operation"],"filechooser_resume");
                assert_eq!(begin["executor"]["connection_role"],"owner");
                let current_instance = begin["executor"]["instance_nonce"].clone();
                if let Some(expected) = &instance { assert_eq!(&current_instance,expected); }
                else { if let Some(previous) = &previous_instance { assert_ne!(&current_instance,previous); } instance = Some(current_instance); }
                let bytes = base64::engine::general_purpose::STANDARD.decode(begin["tool_descriptor"].as_str().unwrap()).unwrap();
                assert_eq!(begin["formation"]["tool_descriptor_hashes"]["browser.filechooser_resume"],blake3::hash(&bytes).to_hex().as_str());
                let handle = format!("{sequence:032x}");
                write.write_all(format!("{}\n",json!({"id":begin["request_id"],"result":{"invocation_handle":handle}})).as_bytes()).await.unwrap();
                let end = native_fixture_body(&mut reader,&key).await;
                assert_eq!(end["request_id"],next_id); next_id += 1;
                assert_eq!(end["operation"],json!({"target_id":null,"session_id":null,"method":"OctopusBrowser.endInvocation","params":{}}));
                assert_eq!(end["invocation_handle"],handle); assert_eq!(end["arguments"],begin["arguments"]);
                write.write_all(format!("{}\n",json!({"id":end["request_id"],"result":{"ended":true}})).as_bytes()).await.unwrap();
            }
            previous_instance = instance;
        }
    });
    let interface_dir = root.join("registry/interface").join(DOOR); write_browser_interface(&interface_dir,"http://127.0.0.1:1");
    let mut descriptor: Value = serde_json::from_slice(&std::fs::read(interface_dir.join("interface.json")).unwrap()).unwrap();
    descriptor["interface_driver_config"]["private_config"]["browser_room"]["transport"] = json!({"wire":"browser.native.v1","host_root_slot":"browser_native_host_root","request_timeout_ms":1000,"navigation_timeout_ms":1000});
    descriptor["interface_driver_config"]["private_config"]["browser_room"]["upload_approval"] = json!({"request_tools":["browser.filechooser_set_files"],"operator_interface":"world_operator_upload_test","resume_tool":"browser.filechooser_resume","permission_policy":{"permitted_persistence":["once","session","always"],"requirements":{"default_policy":{},"origins":[]},"configuration":{"default_policy":{},"origins":[]}}});
    descriptor["interface_driver_config"]["private_config"]["browser_room"]["max_upload_staged_bytes"] = json!(268435456);
    std::fs::write(interface_dir.join("interface.json"),serde_json::to_vec(&descriptor).unwrap()).unwrap();
    let environment = ToolProcessEnvironment { interface:InterfaceId::new(DOOR),interface_dir,descriptor_snapshot:None,var_dir:private.clone(),durable_var_dir:Some(durable.clone()),private_reads:BTreeMap::from([(PRIVATE_CREDENTIAL.into(),credential)]),root_slots:BTreeMap::new(),host_io_limits:InterfaceHostIoLimits { max_control_frame_bytes:1048576,max_stderr_bytes:262144,max_manifest_bytes:1048576,max_payload_bytes:268435456 } };
    let mut tools = browser_tools(); tools.extend(["browser.filechooser_set_files".to_string(),"browser.filechooser_resume".to_string()]); tools.sort(); tools.dedup();
    let (formed,kind) = formation("browser_session",&tools,"browser.filechooser_resume");
    let room = BrowserRoom::load(environment.clone()).unwrap(); let recorder = TestWorkerEnvironmentRecorder::default();
    let selected_bytes = b"admitted binary bytes";
    let selected = ArtifactRef { id:ArtifactId::new("upload-file"),content:ContentHash::of_bytes(b"actual manifest, deliberately not payload") };
    let original = json!({"run":formed.run,"ee":formed.ee,"worker":formed.worker,"door_interface":formed.door_interface,"tool":"browser.filechooser_set_files","correlation_id":"upload-call","arguments":{"reservation_id":"1".repeat(32),"file_chooser_id":"2".repeat(32),"files":[{"artifact_ref":selected,"filename":"report.txt"}]},"source_movement_seq":1,"source_artifact":{"id":"initial-tool-call","content":ContentHash::of_bytes(b"initial tool call")}});
    let upload = json!({"browser_context_id":"CONTEXT1","target_id":"TARGET1","reservation_id":"1".repeat(32),"file_chooser_id":"2".repeat(32),"document_token":"3".repeat(32),"backend_node_id":17,"top_level_origin":"https://upload.test","files":[{"artifact_ref":selected,"filename":"report.txt","payload_sha256":format!("{:x}",Sha256::digest(selected_bytes)),"size_bytes":selected_bytes.len()}]});
    let scope = json!({"worker_environment_id":formed.worker_environment_id,"formation_pin":formed.formation_pin,"host_generation":"4".repeat(32)});
    let permission_offer=json!({"client_interface":"world_operator_upload_test","offered_choices":[{"decision":"accept","persistence":"once"},{"decision":"accept","persistence":"session"},{"decision":"accept","persistence":"always"},{"decision":"decline","persistence":"once"}]});
    let digest = format!("{:x}",Sha256::digest(octopus_descriptors::canonical_bytes(&json!({"schema_version":1,"original":original,"scope":scope,"approval_scope":"browser_file_upload","upload":upload,"permission_offer":permission_offer}))));
    let request_id = format!("sha256:{digest}");
    let request = json!({"schema_version":1,"request_id":request_id,"call_id":"upload-call","original":original,"scope":scope,"approval_scope":"browser_file_upload","upload":upload,"permission_offer":permission_offer,"resume":{"tool":"browser.filechooser_resume","correlation_id":format!("browser-upload-resume:{digest}")}});
    let pending_path = pending_root.join(format!("{digest}.request.json"));
    std::fs::write(&pending_path,serde_json::to_vec(&json!({"request":request,"formation":formed,"operator_interface":"world_operator_upload_test","provider_interface":"world_provider_upload_test"})).unwrap()).unwrap(); std::fs::set_permissions(&pending_path,std::fs::Permissions::from_mode(0o600)).unwrap();
    let copy_call = |call: &ToolProcessInvocation| ToolProcessInvocation { request_id:call.request_id,descriptor:call.descriptor.clone(),envelope:call.envelope.clone(),source_movement_seq:call.source_movement_seq,worker_environment:call.worker_environment.clone(),worker_environment_kind:call.worker_environment_kind.clone() };
    let mut call = invocation(&root,2,"browser.filechooser_resume",json!({"request_id":request_id}),&tools,&formed,&kind);
    assert!(crate::decode_room_invocation(copy_call(&call)).is_err(),"model tool.call cannot enter private resume");
    let payload = serde_json::to_vec(&json!({"operator_approval_response":{"request_id":request_id,"call_id":"upload-call","decision":"accept","selected_persistence":"session"}})).unwrap();
    call.envelope.artifact_file = InterfaceArtifactFile::write(root.join("operator"),"operator.approval.response",&payload).unwrap();
    call.envelope.artifact = ArtifactRef { id:ArtifactId::new("operator-response"),content:ContentHash::of_bytes(&payload) };
    call.envelope.artifact_binding_payloads = vec![ArtifactBindingPayload::new(selected.clone(),b"changed bytes after human approval".to_vec())];
    let metadata = call.envelope.metadata.as_mut().unwrap();
    metadata.grant.interfaces = vec![DOOR.into(),"world_operator_upload_test".into(),"world_provider_upload_test".into()]; metadata.grant.mark_outcomes = vec!["operator.approval".into(),"operator.input.provided".into()];
    metadata.grant.artifact_access.insert(selected.id.to_string(),serde_json::from_value(json!({"read":true})).unwrap()); metadata.artifact_binding_refs = vec![selected.clone()];
    metadata.outbound_interface_bindings = vec![InterfaceId::new("world_provider_upload_test"),InterfaceId::new(DOOR)];
    metadata.checkpoint_binding_policy = Some(serde_json::from_value(json!({"inbound":{"interface":"world_operator_upload_test","config":{"artifact_policy":{"mode":"all"}}},"outbound":[{"interface":"world_provider_upload_test","config":{"artifact_policy":{"mode":"all"}}},{"interface":DOOR,"config":{"artifact_policy":{"mode":"all"}}}]})).unwrap());
    metadata.required_verified_artifacts = serde_json::from_value(json!([{"mark":"operator.approval","artifact_label":"operator.approval.response","verifier":"operator_approval","params":{"request_id":request_id,"call_id":"upload-call","offered_choices":permission_offer["offered_choices"]}}])).unwrap();
    let unverified = response(received(room.receive(copy_call(&call),&recorder).await)); assert_eq!(unverified["ok"],false);
    assert!(!pending_root.join(format!("{digest}.consumed.json")).exists());
    call.request_id = 3; call.source_movement_seq = 3;
    call.envelope.metadata.as_mut().unwrap().checkpoint_admission = Some(CheckpointAdmissionMetadata { artifact_label:"operator.approval.response".into(),required_artifact_labels:vec!["operator.approval.response".into()],complete:true });
    let settled = response(received(room.receive(copy_call(&call),&recorder).await));
    assert_eq!(settled["ok"],true,"{settled}"); assert_eq!(settled["body"]["original_result"]["ok"],false);
    assert_eq!(settled["body"]["original_result"]["correlation_id"],"upload-call");
    assert!(settled["body"]["original_result"]["error"]["message"].as_str().unwrap().contains("currently admitted file bytes"));
    assert!(pending_root.join(format!("{digest}.consumed.json")).is_file());
    drop(room); std::fs::remove_dir_all(&private).unwrap(); std::fs::create_dir(&private).unwrap();
    call.envelope.artifact_binding_payloads[0] = ArtifactBindingPayload::new(
        call.envelope.artifact_binding_payloads[0].artifact.clone(),
        selected_bytes.to_vec(),
    );
    call.request_id = 4; call.source_movement_seq = 4;
    let recovered = BrowserRoom::load(environment).unwrap();
    let replay = response(received(recovered.receive(call,&recorder).await));
    assert_eq!(replay["body"]["original_result"],settled["body"]["original_result"],"restoring file bytes cannot reuse a consumed human choice");
    drop(recovered); server.await.unwrap();
    assert_eq!(std::fs::read(&pending_path).unwrap(),serde_json::to_vec(&json!({"request":request,"formation":formed,"operator_interface":"world_operator_upload_test","provider_interface":"world_provider_upload_test"})).unwrap());
}

async fn native_fixture_open(listener: &tokio::net::UnixListener, key: &[u8;32], generation: &str, challenge: &str) -> (tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf) {
    use hmac::{Hmac, Mac}; use sha2::Sha256; use tokio::io::AsyncWriteExt;
    let (stream,_) = listener.accept().await.unwrap(); let (read,mut write) = stream.into_split();
    let mut mac=Hmac::<Sha256>::new_from_slice(key).unwrap(); mac.update(format!("browser.native.v1.challenge\n{generation}\n{challenge}").as_bytes());
    let signature=mac.finalize().into_bytes().iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    write.write_all(format!("{}\n",json!({"type":"challenge","schema_version":1,"host_generation":generation,"challenge":challenge,"mac":signature})).as_bytes()).await.unwrap();
    (read,write)
}
async fn native_fixture_body(reader: &mut tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>, key: &[u8;32]) -> Value {
    use hmac::{Hmac,Mac}; use sha2::Sha256; use tokio::io::AsyncBufReadExt;
    let mut line=String::new();assert!(reader.read_line(&mut line).await.unwrap()>0);
    let frame:Value=serde_json::from_str(&line).unwrap();let bytes=base64::engine::general_purpose::STANDARD.decode(frame["payload"].as_str().unwrap()).unwrap();
    let mut mac=Hmac::<Sha256>::new_from_slice(key).unwrap();mac.update(&bytes);
    assert_eq!(frame["mac"],mac.finalize().into_bytes().iter().map(|byte|format!("{byte:02x}")).collect::<String>());
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn history_query_result_requires_exact_source_and_one_explicit_backend() {
    use super::browser_session::history::{QueryPolicy, Request};
    use sha2::{Digest, Sha256};
    let source = json!({"id":"history-fixture","content":vec![6;32]});
    let query = json!({"queries":["fixture"]});
    let epoch = "11111111-1111-4111-8111-111111111111";
    let request: Request = serde_json::from_value(json!({"schema_version":1,"request_id":"sha256:fixture","call_id":"fixture",
        "original":{"run":"run","ee":"ee","worker":WORKER,"door_interface":DOOR,"tool":"browser.history","correlation_id":"fixture",
            "arguments":{"source":source,"queries":["fixture"]},"source_movement_seq":1,"source_artifact":source},
        "scope":{"worker_environment_id":"room","formation_pin":"a".repeat(64),"host_generation":"b".repeat(32)},
        "approval_scope":"browser_history_read","history":{"source":source,"source_payload_digest":"c".repeat(64),"source_epoch":epoch,"query":query},
        "resume":{"tool":"browser.history_resume","correlation_id":"resume"},"permission_offer":{"client_interface":"world_operator_test",
            "offered_choices":[{"decision":"accept","persistence":"once"}]}})).unwrap();
    let policy = QueryPolicy { model_read_enabled:true, request_timeout_ms:1000, max_queries:4, max_query_bytes:4096, max_results:100, max_result_bytes:65536 };
    let digest = format!("{:x}", Sha256::digest(octopus_descriptors::canonical_bytes(&query)));
    let native = json!({"request_id":"d".repeat(32),"native_request_ids":["e".repeat(32)],"query_digest":digest});
    let standard = json!({"api":"octopus.standardNavigationHistory.v1","request_id":epoch,"query_digest":digest,
        "matching_algorithm":"literalSubstring","timestamp":"main_process_event_observation","day_timezone":null,
        "retention":{"max_entries":5000,"max_bytes":16777216}});
    let base = json!({"status":"completed","effect_performed":false,"request_id":"sha256:fixture","host_generation":"b".repeat(32),
        "source_ref":source,"source_payload_digest":"c".repeat(64),"source_epoch":epoch,"items":[]});
    for (field, evidence) in [("native_query_evidence", native.clone()), ("standard_query_evidence", standard.clone())] {
        let mut result = base.clone(); result[field] = evidence;
        request.validate_result(&result, &policy).unwrap();
        for key in ["request_id", "host_generation", "source_ref", "source_payload_digest", "source_epoch", "status", "effect_performed", "items"] {
            let mut invalid = result.clone(); invalid[key] = Value::Null;
            assert!(request.validate_result(&invalid, &policy).is_err(), "{field}: {key}");
        }
        result[field]["query_digest"] = json!("f".repeat(64)); assert!(request.validate_result(&result, &policy).is_err());
    }
    let mut result = base; result["standard_query_evidence"] = standard.clone(); result["native_query_evidence"] = native;
    assert!(request.validate_result(&result, &policy).is_err()); result.as_object_mut().unwrap().remove("native_query_evidence");
    for (field, value) in [("api",json!("electron.ownedNavigationHistory.v1")), ("matching_algorithm",json!("alwaysPrefix")),
        ("timestamp",json!("chromium_visit_time")), ("day_timezone",json!("UTC")), ("request_id",Value::Null), ("retention",json!({"max_entries":0,"max_bytes":1024}))] {
        result["standard_query_evidence"] = standard.clone(); result["standard_query_evidence"][field] = value;
        assert!(request.validate_result(&result, &policy).is_err(), "{field}");
    }
}
