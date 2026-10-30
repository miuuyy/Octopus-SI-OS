use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use octopus_descriptors::{ArtifactId, CheckpointId, EeId, InterfaceId, RunId};
use octopus_interfaces::{
    GrantProjection, InterfaceArtifactFile, InterfaceInvocationEnvelope, InvocationMetadata,
    RuntimeInvocationMetadata,
};
use octopus_ledger::{ArtifactRef, ContentHash};
use octopus_tool_door_runtime::{
    ToolProcessError, ToolProcessInvocation, WorkerEnvironmentLifecycleRecorder,
};
use octopus_tool_registry::{load_tool_descriptor_from_dir, ToolDescriptor};
use octopus_worker_env::{
    WorkerEnvKindRegistry, WorkerEnvKindSpec, WorkerEnvironmentFormation,
    WorkerEnvironmentFormationRequest, WorkerEnvironmentKindId,
};
use serde_json::{json, Value};

use super::web_search::WebSearchRoom;

const DOOR: &str = "system_web_search";
const WORKER: &str = "system_worker_coder_max";

struct TestLifecycleSink;

impl WorkerEnvironmentLifecycleRecorder for TestLifecycleSink {
    fn record<'a>(
        &'a self,
        _formation: &'a WorkerEnvironmentFormation,
        _lifecycle: octopus_worker_env::WorkerEnvironmentLifecycle,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

fn registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
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
            door_driver_ref: "system_web_search",
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
        worker_environment_kinds: vec![if tool.starts_with("web.") {
            "web_search".to_string()
        } else {
            "browser_session".to_string()
        }],
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

#[tokio::test]
async fn web_search_uses_its_own_formed_network_room() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/search", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let read = stream.read(&mut request).unwrap();
        let body = json!({"results": [{"title": "Octopus", "url": "https://octopus.example", "snippet": "artifact-native"}]}).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8_lossy(&request[..read]).to_string()
    });
    let root = tempfile::tempdir().unwrap();
    let room = WebSearchRoom;
    let tools = vec!["web.search".to_string()];
    let (formation, kind) = formation("web_search", &tools, "web.search");
    let mut call = invocation(
        root.path(),
        1,
        "web.search",
        json!({"query": "artifact architecture"}),
        &tools,
        &formation,
        &kind,
    );
    call.descriptor.handler_config = json!({
        "operation": "search",
        "default_provider": "fixture",
        "providers": {
            "fixture": {
                "base_url": base_url,
                "path": "",
                "method": "POST",
                "headers": {},
                "query": {},
                "body": {
                    "argument_fields": {"query": "query", "limit": "limit"}
                },
                "response": {
                    "items_path": "results",
                    "title_paths": ["title"],
                    "url_paths": ["url"],
                    "snippet_paths": ["snippet"]
                },
                "retry": {"max_attempts": 1}
            }
        }
    });
    let searched = response(received(room.receive(call, &TestLifecycleSink).await));
    assert_eq!(searched["ok"], true, "{searched}");
    assert_eq!(searched["body"]["candidate_count"], 1);
    assert!(server.join().unwrap().contains("artifact architecture"));
}

#[tokio::test]
async fn web_search_stops_chunked_response_at_descriptor_ceiling() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/search", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n20\r\n0123456789abcdef0123456789abcdef\r\n0\r\n\r\n",
            )
            .unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let room = WebSearchRoom;
    let tools = vec!["web.search".to_string()];
    let (formation, kind) = formation("web_search", &tools, "web.search");
    let mut call = invocation(
        root.path(),
        1,
        "web.search",
        json!({"query": "bounded"}),
        &tools,
        &formation,
        &kind,
    );
    call.descriptor.handler_config = json!({
        "operation": "search",
        "default_provider": "fixture",
        "max_response_bytes": 16,
        "providers": {
            "fixture": {
                "base_url": base_url,
                "path": "",
                "method": "POST",
                "headers": {},
                "query": {},
                "body": {"argument_fields": {"query": "query"}},
                "response": {"items_path": "results"},
                "retry": {"max_attempts": 1}
            }
        }
    });
    let searched = response(received(room.receive(call, &TestLifecycleSink).await));
    assert_eq!(searched["ok"], false, "{searched}");
    assert!(searched["error"]["message"]
        .as_str()
        .unwrap()
        .contains("exceeds 16 bytes"));
    server.join().unwrap();
}

#[tokio::test]
async fn web_search_failure_artifact_contains_digest_not_provider_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/search", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request).unwrap();
        let body = "provider-private-response-body";
        let response = format!(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let room = WebSearchRoom;
    let tools = vec!["web.search".to_string()];
    let (formation, kind) = formation("web_search", &tools, "web.search");
    let mut call = invocation(
        root.path(),
        1,
        "web.search",
        json!({"query": "private failure"}),
        &tools,
        &formation,
        &kind,
    );
    call.descriptor.handler_config = json!({
        "operation": "search",
        "default_provider": "fixture",
        "providers": {
            "fixture": {
                "base_url": base_url,
                "path": "",
                "method": "POST",
                "headers": {},
                "query": {},
                "body": {"argument_fields": {"query": "query"}},
                "response": {"items_path": "results"},
                "retry": {"max_attempts": 1}
            }
        }
    });
    let searched = response(received(room.receive(call, &TestLifecycleSink).await));
    let error = searched["error"]["message"].as_str().unwrap();
    assert!(!error.contains("provider-private-response-body"), "{error}");
    assert!(error.contains("bytes=30 blake3="), "{error}");
    server.join().unwrap();
}

#[tokio::test]
async fn web_extract_maps_batch_documents_inside_the_same_room_contract() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/extract", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let read = stream.read(&mut request).unwrap();
        let body = json!({"documents": [{
            "title": "Octopus",
            "url": "https://octopus.example/docs",
            "markdown": "artifact-native content",
            "html": "<p>artifact-native content</p>"
        }]})
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8_lossy(&request[..read]).to_string()
    });
    let root = tempfile::tempdir().unwrap();
    let room = WebSearchRoom;
    let tools = vec!["web.extract".to_string()];
    let (formation, kind) = formation("web_search", &tools, "web.extract");
    let mut call = invocation(
        root.path(),
        1,
        "web.extract",
        json!({"urls": ["https://octopus.example/docs"], "max_chars": 1000}),
        &tools,
        &formation,
        &kind,
    );
    call.descriptor.handler_config = json!({
        "operation": "extract",
        "default_provider": "fixture",
        "providers": {
            "fixture": {
                "base_url": base_url,
                "path": "",
                "method": "POST",
                "headers": {},
                "query": {},
                "body": {"argument_fields": {"urls": "urls"}},
                "response": {
                    "items_path": "documents",
                    "title_paths": ["title"],
                    "url_paths": ["url"],
                    "content_paths": ["markdown"],
                    "raw_content_paths": ["html"]
                },
                "extraction_mode": "batch",
                "retry": {"max_attempts": 1}
            }
        }
    });
    let extracted = response(received(room.receive(call, &TestLifecycleSink).await));
    assert_eq!(extracted["ok"], true, "{extracted}");
    assert_eq!(extracted["body"]["document_count"], 1);
    assert_eq!(
        extracted["body"]["documents"][0]["content"],
        "artifact-native content"
    );
    assert!(server.join().unwrap().contains("octopus.example"));
}
