use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use octopus_descriptors::ArtifactId;
use octopus_ledger::{ArtifactRef, ContentHash};

fn spawn_mock_mcp_server() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock mcp server");
    let addr = listener.local_addr().expect("mock addr");
    let base = format!("http://{addr}/mcp");
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut headers_end = None;
                loop {
                    let read = match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => read,
                    };
                    buf.extend_from_slice(&chunk[..read]);
                    if let Some(pos) = find_headers_end(&buf) {
                        headers_end = Some(pos);
                        break;
                    }
                }
                let Some(header_len) = headers_end else {
                    return;
                };
                let head = String::from_utf8_lossy(&buf[..header_len]).to_string();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let lower = line.to_ascii_lowercase();
                        lower
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                let mut body = buf[header_len..].to_vec();
                while body.len() < content_length {
                    let read = match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => read,
                    };
                    body.extend_from_slice(&chunk[..read]);
                }
                let request: serde_json::Value =
                    serde_json::from_slice(&body).unwrap_or(Value::Null);
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                let id = request.get("id").cloned();
                if id.is_none() {
                    let head = "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nMcp-Session-Id: sess-1\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.flush();
                    return;
                }
                let result = match method {
                    "initialize" => json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {
                            "tools": { "listChanged": false },
                            "resources": { "subscribe": false, "listChanged": false }
                        },
                        "serverInfo": { "name": "mock-mcp", "version": "1" }
                    }),
                    "tools/list" => json!({
                        "tools": [
                            {
                                "name": "search",
                                "description": "Search the mock index",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": { "query": { "type": "string" } },
                                    "required": ["query"]
                                }
                            }
                        ]
                    }),
                    "resources/templates/list" => json!({
                        "resourceTemplates": [
                            {
                                "uriTemplate": "file:///{path}",
                                "name": "file",
                                "mimeType": "text/plain"
                            }
                        ]
                    }),
                    "resources/list" => json!({
                        "resources": [
                            { "uri": "file:///readme.md", "name": "readme", "mimeType": "text/markdown" }
                        ]
                    }),
                    "resources/read" => json!({
                        "contents": [
                            { "uri": "file:///readme.md", "mimeType": "text/markdown", "text": "hello from the external server" }
                        ]
                    }),
                    "tools/call" => {
                        let delay_ms = request
                            .pointer("/params/arguments/delay_ms")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        if delay_ms > 0 {
                            std::thread::sleep(Duration::from_millis(delay_ms));
                        }
                        let query = request
                            .pointer("/params/arguments/query")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        json!({
                            "content": [ { "type": "text", "text": format!("results for: {query}") } ],
                            "isError": false
                        })
                    }
                    other => json!({ "echo": other }),
                };
                let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                let payload = serde_json::to_vec(&response).unwrap();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: sess-1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&payload);
                let _ = stream.flush();
            });
        }
    });
    (base, handle)
}

fn direct_envelope(root: &Path, sequence: u64, arguments: Value) -> InterfaceInvocationEnvelope {
    direct_tool_envelope(root, sequence, "mcp.example.search", arguments)
}

fn direct_tool_envelope(
    root: &Path,
    sequence: u64,
    tool: &str,
    arguments: Value,
) -> InterfaceInvocationEnvelope {
    let payload = serde_json::to_vec(&json!({
        "operation": tool,
        "arguments": arguments,
        "correlation_id": format!("direct-{sequence}")
    }))
    .unwrap();
    let artifact_file = InterfaceArtifactFile::write(
        root.join(format!("direct-{sequence}")),
        "mcp.operation.request",
        &payload,
    )
    .and_then(|artifact| artifact.capture_bounded(1024 * 1024, 64 * 1024 * 1024))
    .unwrap();
    let content = ContentHash::of_bytes(&payload);
    InterfaceInvocationEnvelope {
        artifact: ArtifactRef {
            id: ArtifactId::new(format!("tool.call:{}", content.to_hex())),
            content,
        },
        artifact_file,
        artifact_binding_payloads: Vec::new(),
        metadata: None,
    }
}

fn process_artifact(result: Result<InterfaceArtifactFile, String>) -> InterfaceArtifactFile {
    match result {
        Ok(artifact) => artifact,
        Err(error) => panic!("MCP process invocation failed: {error}"),
    }
}

#[tokio::test]
async fn interface_operation_does_not_hold_client_state_across_remote_requests() {
    let (base, server) = spawn_mock_mcp_server();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_registry(root, &base);
    let interface = InterfaceId::new("world_mcp_example");
    let process = McpInterfaceProcess {
        interface: interface.clone(),
        config: load_interface_config(
            &root.join("registry/interface/world_mcp_example"),
            &interface,
        )
        .unwrap(),
        state_dir: root.join("var/mcp-concurrency"),
        client: Arc::new(tokio::sync::Mutex::new(McpClientState {
            generation: 0,
            client: None,
            transition: None,
        })),
        client_changed: Arc::new(tokio::sync::Notify::new()),
    };
    let descriptor = load_interface_descriptor(
        &root.join("registry/interface/world_mcp_example"),
        &interface,
    )
    .unwrap();
    let catalog = load_operation_catalog(
        &root.join("registry/interface/world_mcp_example"),
        &descriptor,
    )
    .unwrap();
    let spec = catalog.operations.get("mcp.example.search").unwrap();
    let output_root = root.join("results");
    let warm = process_artifact(
        process
            .receive(
                spec,
                direct_envelope(root, 49, json!({ "query": "warm" })),
                &output_root,
            )
            .await,
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&warm.read_payload_bytes().unwrap()).unwrap()["ok"],
        true
    );

    let slow = process.receive(
        spec,
        direct_envelope(root, 50, json!({ "query": "slow", "delay_ms": 400 })),
        &output_root,
    );
    let fast = async {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let started = std::time::Instant::now();
        let artifact = process_artifact(
            process
                .receive(
                    spec,
                    direct_envelope(root, 51, json!({ "query": "fast" })),
                    &output_root,
                )
                .await,
        );
        (artifact, started.elapsed())
    };
    let (slow, (fast, elapsed)) = tokio::join!(slow, fast);
    let slow: Value =
        serde_json::from_slice(&process_artifact(slow).read_payload_bytes().unwrap()).unwrap();
    let fast: Value = serde_json::from_slice(&fast.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(slow["result"]["content"][0]["text"], "results for: slow");
    assert_eq!(fast["result"]["content"][0]["text"], "results for: fast");
    assert!(
        elapsed < Duration::from_millis(250),
        "fast call took {elapsed:?}"
    );
    drop(server);
}

#[tokio::test]
async fn explicit_close_reaches_the_remote_session_while_a_call_is_in_flight() {
    let (base, server) = spawn_mock_mcp_server();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_registry(root, &base);
    let interface = InterfaceId::new("world_mcp_example");
    let process = McpInterfaceProcess {
        interface: interface.clone(),
        config: load_interface_config(
            &root.join("registry/interface/world_mcp_example"),
            &interface,
        )
        .unwrap(),
        state_dir: root.join("var/mcp-close-concurrency"),
        client: Arc::new(tokio::sync::Mutex::new(McpClientState {
            generation: 0,
            client: None,
            transition: None,
        })),
        client_changed: Arc::new(tokio::sync::Notify::new()),
    };
    let descriptor = load_interface_descriptor(
        &root.join("registry/interface/world_mcp_example"),
        &interface,
    )
    .unwrap();
    let catalog = load_operation_catalog(
        &root.join("registry/interface/world_mcp_example"),
        &descriptor,
    )
    .unwrap();
    let call_descriptor = catalog.operations.get("mcp.example.search").unwrap();
    let close_descriptor = catalog.operations.get("mcp.example.close").unwrap();
    let output_root = root.join("results");
    let warm = process_artifact(
        process
            .receive(
                call_descriptor,
                direct_envelope(root, 69, json!({ "query": "warm" })),
                &output_root,
            )
            .await,
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&warm.read_payload_bytes().unwrap()).unwrap()["ok"],
        true
    );

    let slow = process.receive(
        call_descriptor,
        direct_envelope(root, 70, json!({ "query": "slow", "delay_ms": 400 })),
        &output_root,
    );
    let close = async {
        tokio::time::sleep(Duration::from_millis(40)).await;
        let started = std::time::Instant::now();
        let artifact = process_artifact(
            process
                .receive(
                    close_descriptor,
                    direct_tool_envelope(root, 71, "mcp.example.close", json!({})),
                    &output_root,
                )
                .await,
        );
        (artifact, started.elapsed())
    };
    let (slow, (closed, close_elapsed)) = tokio::join!(slow, close);
    let slow: Value =
        serde_json::from_slice(&process_artifact(slow).read_payload_bytes().unwrap()).unwrap();
    let closed: Value = serde_json::from_slice(&closed.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(closed["ok"], true, "{closed}");
    assert_eq!(closed["result"]["closed"], true, "{closed}");
    assert_eq!(slow["ok"], false, "{slow}");
    assert!(
        close_elapsed < Duration::from_millis(250),
        "close was serialized for {close_elapsed:?}"
    );
    drop(server);
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

fn write_registry(root: &Path, base_url: &str) {
    let interface_dir = root
        .join("registry")
        .join("interface")
        .join("world_mcp_example");
    std::fs::create_dir_all(&interface_dir).unwrap();
    let descriptor = json!({
        "id": "world_mcp_example",
        "interface_type": "world",
        "role": "service",
        "direction": "bidirectional",
        "source_names": ["mcp"],
        "interface_driver_ref": "mcp_client",
        "interface_driver_config": {
            "private_config": {
                "protocol_versions": ["2025-11-25"],
                "client_name": "octopus-mcp-client-test",
                "client_version": "1",
                "operation_catalog_dir": "operations",
                "operation_catalog_label": "mcp.operation_catalog",
                "operation_request_label": "mcp.operation.request",
                "max_catalog_pages": 8,
                "max_catalog_items": 64,
                "max_catalog_bytes": 65536,
                "event_journal_segment_bytes": 1048576,
                "event_journal_retained_segments": 8,
                "reconnect_backoff_ms": 20
            },
            "transport": {
                "base_url": base_url,
                "wire": "mcp_streamable_http"
            }
        }
    });
    std::fs::write(
        interface_dir.join("interface.json"),
        serde_json::to_vec_pretty(&descriptor).unwrap(),
    )
    .unwrap();

    let operations_dir = interface_dir.join("operations");
    std::fs::create_dir_all(&operations_dir).unwrap();
    let search = json!({
        "id": "mcp.example.search",
        "request_label": "mcp.operation.request",
        "output_label": "mcp.tool.result",
        "handler_config": { "operation": "call_tool", "mcp_tool_name": "search" },
        "description": "search",
        "input_schema": {
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "delay_ms": { "type": "integer", "minimum": 0 }
            },
            "required": ["query"],
            "additionalProperties": false
        }
    });
    std::fs::write(
        operations_dir.join("mcp.example.search.json"),
        serde_json::to_vec_pretty(&search).unwrap(),
    )
    .unwrap();

    let read_resource = json!({
        "id": "mcp.example.read_resource",
        "request_label": "mcp.operation.request",
        "output_label": "mcp.resource",
        "handler_config": { "operation": "read_resource" },
        "description": "read resource",
        "input_schema": {
            "type": "object",
            "properties": { "uri": { "type": "string", "minLength": 1 } },
            "required": ["uri"],
            "additionalProperties": false
        }
    });
    std::fs::write(
        operations_dir.join("mcp.example.read_resource.json"),
        serde_json::to_vec_pretty(&read_resource).unwrap(),
    )
    .unwrap();

    let close = json!({
        "id": "mcp.example.close",
        "request_label": "mcp.operation.request",
        "output_label": "mcp.result",
        "handler_config": { "operation": "close" },
        "description": "close the MCP session",
        "input_schema": { "type": "object", "properties": {}, "additionalProperties": false }
    });
    std::fs::write(
        operations_dir.join("mcp.example.close.json"),
        serde_json::to_vec_pretty(&close).unwrap(),
    )
    .unwrap();
}

#[test]
fn authored_world_mcp_example_descriptor_carries_an_explicit_transport_contract() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json");
    let bytes = std::fs::read(&path)
        .expect("registry/interface/world_mcp_example/interface.json must exist");
    let descriptor: InterfaceDescriptor =
        octopus_descriptors::parse_strict(&bytes).expect("world_mcp_example descriptor parses");
    assert_eq!(descriptor.interface_driver_ref, "mcp_client");
    let transport = descriptor
        .interface_driver_config
        .as_ref()
        .and_then(|config| config.transport.clone())
        .expect("descriptor carries interface_driver_config.transport for the mcp_client family");
    assert_eq!(transport.wire.as_deref(), Some("mcp_streamable_http"));
    assert!(transport.base_url.trim().is_empty());
    assert_eq!(
        transport.base_url_env.as_deref(),
        Some("WORLD_MCP_EXAMPLE_URL")
    );
    assert_eq!(transport.max_response_bytes, Some(8_388_608));
    let metadata = &descriptor
        .interface_driver_config
        .as_ref()
        .expect("descriptor carries driver config")
        .private_config;
    assert_eq!(
        metadata["protocol_versions"],
        json!(["2025-11-25", "2025-06-18", "2025-03-26"])
    );
    assert_eq!(metadata["max_catalog_pages"], 64);
    assert_eq!(metadata["max_catalog_items"], 4096);
    assert_eq!(metadata["max_catalog_bytes"], 16_777_216);
    assert_eq!(metadata["event_journal_segment_bytes"], 16_777_216);
    assert_eq!(metadata["event_journal_retained_segments"], 64);
}

#[test]
fn authored_interface_operation_maps_to_the_external_server_tool_name() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../operations/mcp.example.search.json");
    let descriptor: McpOperationFile =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        descriptor
            .handler_config
            .get("operation")
            .and_then(Value::as_str),
        Some("call_tool")
    );
    assert_eq!(
        descriptor
            .handler_config
            .get("mcp_tool_name")
            .and_then(Value::as_str),
        Some("search"),
        "the interface-owned MCP operation names external tool 'search'"
    );
}

#[test]
fn every_remote_mcp_operation_is_interface_local_and_strictly_loadable() {
    let registry_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let repository_root = registry_root.parent().unwrap();
    let generic_operations: &[&str] = &[
        "call_tool",
        "cancel_request",
        "close",
        "complete",
        "get_prompt",
        "list_prompts",
        "list_resource_templates",
        "list_resources",
        "list_tools",
        "oauth_complete",
        "oauth_start",
        "oauth_status",
        "ping",
        "poll_events",
        "read_resource",
        "respond",
        "send_progress",
        "set_logging_level",
        "status",
        "subscribe_resource",
        "unsubscribe_resource",
    ];
    let tool_filtered_operations: &[&str] = &["call_tool", "close", "list_tools", "status"];
    let expected: [(&str, &str, Option<&str>, usize, &[&str]); 4] = [
        (
            "world_mcp_example",
            "mcp.example",
            None,
            22,
            generic_operations,
        ),
        (
            "world_mcp_firecrawl",
            "firecrawl",
            Some("artifact-application-mcp-firecrawl-context"),
            21,
            generic_operations,
        ),
        (
            "world_mcp_github",
            "github",
            Some("artifact-application-mcp-github-context"),
            21,
            generic_operations,
        ),
        (
            "world_gmail",
            "gmail",
            Some("artifact-application-gmail-context"),
            4,
            tool_filtered_operations,
        ),
    ];
    let mut total = 0;
    for (interface, prefix, artifact_id, expected_count, operations) in expected {
        let interface_dir = registry_root.join("interface").join(interface);
        let bytes = std::fs::read(interface_dir.join("interface.json")).unwrap();
        let descriptor: InterfaceDescriptor = octopus_descriptors::parse_strict(&bytes).unwrap();
        assert_eq!(descriptor.id.as_str(), interface);
        assert_eq!(descriptor.interface_driver_ref, "mcp_client");
        load_interface_config(&interface_dir, &descriptor.id).unwrap();
        octopus_interface_transport::validate_spawn_life_config(&registry_root, &descriptor.id)
            .unwrap()
            .expect("remote MCP interface declares its life");
        let catalog = load_operation_catalog(&interface_dir, &descriptor).unwrap();
        assert_eq!(catalog.operations.len(), expected_count, "{interface}");
        assert_eq!(catalog.affordances.len(), expected_count, "{interface}");
        for suffix in operations {
            assert!(
                catalog
                    .operations
                    .contains_key(&format!("{prefix}.{suffix}")),
                "{interface} omits generic MCP operation {suffix}"
            );
        }
        for affordance in &catalog.affordances {
            assert_eq!(
                affordance["request_label"],
                json!("mcp.operation.request"),
                "{interface}"
            );
            assert!(affordance["name"].as_str().is_some(), "{interface}");
            assert!(affordance["description"].as_str().is_some(), "{interface}");
            assert!(affordance["input_schema"].is_object(), "{interface}");
            assert!(affordance["output_label"].as_str().is_some(), "{interface}");
        }
        for operation in catalog.operations.keys() {
            assert!(operation.starts_with(&format!("{prefix}.")), "{interface}");
            assert!(
                !registry_root
                    .join("tool")
                    .join(operation)
                    .join("tool.json")
                    .exists(),
                "remote MCP operation '{operation}' leaked back into Tool Registry"
            );
        }
        if let Some(artifact_id) = artifact_id {
            let application: Value = serde_json::from_slice(
                &std::fs::read(interface_dir.join("application.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(application["id"], json!(interface));
            assert_eq!(application["interfaces"], json!([interface]));
            assert_eq!(
                application["artifact_bindings"].as_array().unwrap().len(),
                1
            );
            assert_eq!(
                application["artifact_bindings"][0]["artifact_id"],
                json!(artifact_id)
            );
            assert!(application["artifact_bindings"][0]["label"]
                .as_str()
                .is_some_and(|label| !label.trim().is_empty()));

            let artifact: Value = serde_json::from_slice(
                &std::fs::read(
                    registry_root
                        .join("artifact")
                        .join(artifact_id)
                        .join("artifact.json"),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(artifact["id"], json!(artifact_id));
            assert_eq!(artifact["label"], json!("context.reference"));
            let context = artifact["text"].as_str().unwrap();
            for required in [
                interface,
                &format!("{prefix}.call_tool"),
                &format!("{prefix}.status"),
            ] {
                assert!(
                    context.contains(required),
                    "{interface} context omits {required}"
                );
            }

            let package: Value = serde_json::from_slice(
                &std::fs::read(
                    repository_root
                        .join("app/distribution/local-packages/packages")
                        .join(format!("{interface}.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(package["schema"], json!("octopus.local_package"));
            assert_eq!(package["id"], json!(interface));
            assert_eq!(package["product_kind"], json!("application"));
            assert_eq!(package["product_id"], json!(interface));
            assert_eq!(
                package["members"],
                json!([
                    {"registry_kind": "interface", "owner": interface, "primary": true},
                    {"registry_kind": "artifact", "owner": artifact_id}
                ])
            );
        }
        total += catalog.operations.len();
    }
    assert_eq!(total, 68);
}

#[test]
fn private_interface_instance_owns_its_complete_config() {
    let dir = tempfile::tempdir().unwrap();
    let interface_root = dir.path().join("registry/interface");
    let instance_dir = interface_root.join("world_mcp_notion_private");
    std::fs::create_dir_all(&instance_dir).unwrap();
    let instance = json!({
        "id": "world_mcp_notion_private",
        "interface_type": "world",
        "role": "service",
        "direction": "bidirectional",
        "source_names": ["mcp"],
        "interface_driver_ref": "mcp_client",
        "interface_driver_config": {
            "private_config": {
                "protocol_versions": ["2025-11-25"],
                "client_name": "octopus-mcp-client",
                "client_version": "1",
                "max_catalog_pages": 64,
                "max_catalog_items": 4096,
                "max_catalog_bytes": 16777216,
                "event_journal_segment_bytes": 16777216,
                "event_journal_retained_segments": 64,
                "reconnect_backoff_ms": 2000
            },
            "transport": {
                "base_url": "",
                "base_url_env": "WORLD_MCP_NOTION_URL",
                "wire": "mcp_streamable_http"
            }
        }
    });
    std::fs::write(
        instance_dir.join("interface.json"),
        serde_json::to_vec(&instance).unwrap(),
    )
    .unwrap();
    let config =
        load_interface_config(&instance_dir, &InterfaceId::new("world_mcp_notion_private"))
            .unwrap();
    assert_eq!(
        config.transport.base_url_env.as_deref(),
        Some("WORLD_MCP_NOTION_URL")
    );
    assert_eq!(config.protocol_versions, vec!["2025-11-25"]);
    assert_eq!(config.max_catalog_bytes, 16_777_216);
}

#[test]
fn reconnect_replay_never_duplicates_effectful_operations() {
    assert!(McpOperation::ListTools.replay_safe_after_reconnect());
    assert!(McpOperation::ReadResource.replay_safe_after_reconnect());
    assert!(!McpOperation::CallTool {
        mcp_tool_name: Some("search".to_string())
    }
    .replay_safe_after_reconnect());
    assert!(!McpOperation::Respond.replay_safe_after_reconnect());
    assert!(!McpOperation::SendProgress.replay_safe_after_reconnect());
}

type RecordedRequests = Arc<std::sync::Mutex<Vec<Value>>>;

fn spawn_account_mcp_server(
    unauthorized: Arc<std::sync::atomic::AtomicBool>,
) -> (String, RecordedRequests) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/mcp", listener.local_addr().unwrap());
    let requests: RecordedRequests = Arc::default();
    let recorded = Arc::clone(&requests);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_len = loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break None;
                }
                buf.extend_from_slice(&chunk[..read]);
                if let Some(end) = find_headers_end(&buf) {
                    break Some(end);
                }
            };
            let Some(header_len) = header_len else {
                continue;
            };
            let head = String::from_utf8_lossy(&buf[..header_len]).to_string();
            let header = |name: &str| {
                head.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case(name)
                        .then(|| value.trim().to_string())
                })
            };
            let length = header("content-length")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_len + length {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..read]);
            }
            let request: Value = serde_json::from_slice(&buf[header_len..]).unwrap_or(Value::Null);
            recorded.lock().unwrap().push(json!({
                "method": request["method"],
                "tool": request.pointer("/params/name"),
                "authorization": header("authorization"),
                "account": header("chatgpt-account-id"),
            }));
            let reply = |status: &str, body: Option<Value>| match body {
                Some(body) => {
                    let body = serde_json::to_vec(&body).unwrap();
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        String::from_utf8(body).unwrap()
                    )
                }
                None => {
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                }
            };
            let response = if unauthorized.load(std::sync::atomic::Ordering::SeqCst) {
                reply("401 Unauthorized", None)
            } else if request.get("id").is_none() {
                reply("202 Accepted", None)
            } else {
                let result = match request["method"].as_str().unwrap_or_default() {
                    "initialize" => json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": { "tools": {}, "resources": {} },
                        "serverInfo": { "name": "plugin-runtime-mock", "version": "1" }
                    }),
                    "tools/list" => json!({ "tools": [
                        { "name": "mail.search", "inputSchema": { "type": "object" },
                          "_meta": { "connector_id": "connector_mail", "link_id": "link_1" } },
                        { "name": "other.post", "inputSchema": { "type": "object" },
                          "_meta": { "connector_id": "connector_other" } },
                        { "name": "bare", "inputSchema": { "type": "object" } }
                    ] }),
                    "tools/call" => json!({
                        "content": [{ "type": "text", "text": request["params"]["name"] }],
                        "isError": false
                    }),
                    other => json!({ "echo": other }),
                };
                reply(
                    "200 OK",
                    Some(json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })),
                )
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (base, requests)
}

fn write_account_registry(root: &Path, base_url: &str, auth_file_env: &str) -> McpOperationCatalog {
    let interface_dir = root.join("registry/interface/world_account_mail");
    let operations = interface_dir.join("operations");
    std::fs::create_dir_all(&operations).unwrap();
    let descriptor = json!({
        "id": "world_account_mail",
        "interface_type": "world",
        "role": "service",
        "direction": "bidirectional",
        "source_names": ["mcp"],
        "interface_driver_ref": "mcp_client",
        "interface_driver_config": {
            "private_config": {
                "protocol_versions": ["2025-11-25"],
                "client_name": "octopus-mcp-client-test",
                "client_version": "1",
                "operation_catalog_dir": "operations",
                "operation_catalog_label": "mcp.operation_catalog",
                "operation_request_label": "mcp.operation.request",
                "max_catalog_pages": 8,
                "max_catalog_items": 64,
                "max_catalog_bytes": 65536,
                "event_journal_segment_bytes": 1048576,
                "event_journal_retained_segments": 8,
                "reconnect_backoff_ms": 20,
                "auth": {
                    "mode": "chatgpt_account",
                    "auth_file_env": auth_file_env,
                    "account_header": "ChatGPT-Account-ID"
                },
                "tool_filter": { "meta_key": "connector_id", "values": ["connector_mail"] }
            },
            "transport": { "base_url": base_url, "wire": "mcp_streamable_http" }
        }
    });
    std::fs::write(
        interface_dir.join("interface.json"),
        serde_json::to_vec(&descriptor).unwrap(),
    )
    .unwrap();
    for (operation, output_label) in [
        ("call_tool", "mcp.tool.result"),
        ("list_tools", "mcp.result"),
        ("list_resources", "mcp.result"),
    ] {
        let id = format!("mail.{operation}");
        std::fs::write(
            operations.join(format!("{id}.json")),
            serde_json::to_vec(&json!({
                "id": id,
                "request_label": "mcp.operation.request",
                "output_label": output_label,
                "handler_config": { "operation": operation },
                "description": operation,
                "input_schema": { "type": "object" }
            }))
            .unwrap(),
        )
        .unwrap();
    }
    let descriptor =
        load_interface_descriptor(&interface_dir, &InterfaceId::new("world_account_mail")).unwrap();
    load_operation_catalog(&interface_dir, &descriptor).unwrap()
}

async fn run_operation(
    process: &McpInterfaceProcess,
    catalog: &McpOperationCatalog,
    root: &Path,
    sequence: u64,
    operation: &str,
    arguments: Value,
) -> Value {
    let spec = catalog.operations.get(operation).unwrap();
    let envelope = direct_tool_envelope(root, sequence, operation, arguments);
    let artifact = process_artifact(process.receive(spec, envelope, &root.join("results")).await);
    serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap()
}

#[tokio::test]
async fn chatgpt_account_auth_and_tool_filter_gate_the_remote_surface() {
    let unauthorized = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (base, requests) = spawn_account_mcp_server(Arc::clone(&unauthorized));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let auth_file_env = "OCTOPUS_MCP_CLIENT_TEST_CHATGPT_AUTH_FILE";
    let auth_file = root.join("auth.json");
    std::env::set_var(auth_file_env, &auth_file);
    let write_auth = |payload: &str| {
        let document = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": format!("e30.{payload}.sig"),
                "account_id": "account-1",
                "refresh_token": "refresh-never-used"
            }
        });
        std::fs::write(&auth_file, serde_json::to_vec(&document).unwrap()).unwrap();
    };
    let catalog = write_account_registry(root, &base, auth_file_env);
    let interface = InterfaceId::new("world_account_mail");
    let config = load_interface_config(
        &root.join("registry/interface/world_account_mail"),
        &interface,
    )
    .unwrap();
    let process = mcp_interface_process(interface, config, root.join("var/remote-mcp"));

    write_auth("eyJleHAiOjF9");
    let expired = run_operation(&process, &catalog, root, 1, "mail.list_tools", json!({})).await;
    assert_eq!(
        expired["error"]["code"], "account_sign_in_expired",
        "{expired}"
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "an expired sign-in reached the network"
    );

    write_auth("eyJleHAiOjQxMDI0NDQ4MDB9");
    let signed_in = std::fs::read(&auth_file).unwrap();
    let listed = run_operation(&process, &catalog, root, 2, "mail.list_tools", json!({})).await;
    assert_eq!(
        listed["result"]["tools"],
        json!([{
            "name": "mail.search", "inputSchema": { "type": "object" },
            "_meta": { "connector_id": "connector_mail", "link_id": "link_1" }
        }])
    );
    let call = |name: &str| json!({ "name": name, "arguments": {} });
    let called = run_operation(
        &process,
        &catalog,
        root,
        3,
        "mail.call_tool",
        call("mail.search"),
    )
    .await;
    assert_eq!(
        called["result"]["content"][0]["text"], "mail.search",
        "{called}"
    );
    let refused = run_operation(
        &process,
        &catalog,
        root,
        4,
        "mail.call_tool",
        call("other.post"),
    )
    .await;
    assert_eq!(refused["error"]["code"], "tool_not_admitted", "{refused}");
    let resources = run_operation(
        &process,
        &catalog,
        root,
        5,
        "mail.list_resources",
        json!({}),
    )
    .await;
    assert_eq!(
        resources["error"]["code"], "surface_not_admitted",
        "{resources}"
    );
    {
        let requests = requests.lock().unwrap();
        assert!(requests.iter().all(|request| {
            request["authorization"] == json!("Bearer e30.eyJleHAiOjQxMDI0NDQ4MDB9.sig")
                && request["account"] == json!("account-1")
        }));
        let called = requests
            .iter()
            .filter(|request| request["method"] == "tools/call")
            .map(|request| request["tool"].clone())
            .collect::<Vec<_>>();
        assert_eq!(called, vec![json!("mail.search")]);
    }

    unauthorized.store(true, std::sync::atomic::Ordering::SeqCst);
    let rejected = run_operation(
        &process,
        &catalog,
        root,
        6,
        "mail.call_tool",
        call("mail.search"),
    )
    .await;
    assert_eq!(
        rejected["error"]["code"], "account_sign_in_expired",
        "{rejected}"
    );
    assert_eq!(
        rejected["error"]["message"],
        "ChatGPT sign-in expired — sign in again in Octopus"
    );
    assert_eq!(std::fs::read(&auth_file).unwrap(), signed_in);
}
