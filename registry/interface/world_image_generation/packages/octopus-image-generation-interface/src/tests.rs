use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{ImageArguments, ImageGenerationProcess, ImageOperation, ImageTransport};

async fn spawn_response(
    status: u16,
    body: Vec<u8>,
) -> (reqwest::Url, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = reqwest::Url::parse(&format!("http://{address}/images")).unwrap();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 16 * 1024];
        let read = socket.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..read]).to_string();
        let head = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        socket.flush().await.unwrap();
        request
    });
    (endpoint, handle)
}

fn operation(max_image_bytes: u64) -> ImageOperation {
    ImageOperation {
        input_request_label: "image.generation.request".to_string(),
        prompt_field: "prompt".to_string(),
        model: Some("image-model".to_string()),
        size: Some("1024x1024".to_string()),
        n: Some(1),
        response_format: Some("b64_json".to_string()),
        data_path: "data".to_string(),
        b64_field: "b64_json".to_string(),
        url_field: "url".to_string(),
        allow_endpoint_origin: true,
        allowed_url_origins: Vec::new(),
        revised_prompt_field: Some("revised_prompt".to_string()),
        media_label: "image.generated".to_string(),
        max_image_bytes: Some(max_image_bytes),
        _client_connection: None,
    }
}

fn process(endpoint: reqwest::Url, max_response_bytes: usize) -> ImageGenerationProcess {
    ImageGenerationProcess {
        transport: ImageTransport {
            endpoint,
            headers: HeaderMap::new(),
            auth: None,
            max_response_bytes,
        },
        max_manifest_bytes: 4096,
        client: Client::new(),
    }
}

#[tokio::test]
async fn image_endpoint_emits_bounded_bytes_and_request_data() {
    let raw = b"\x89PNG\r\n\x1a\nIMAGE";
    let response = serde_json::to_vec(&json!({
        "data": [{
            "b64_json": BASE64_STANDARD.encode(raw),
            "revised_prompt": "a precise red octopus"
        }]
    }))
    .unwrap();
    let (endpoint, request) = spawn_response(200, response).await;
    let generated = process(endpoint, 4096)
        .generate(
            &operation(4096),
            &ImageArguments {
                prompt: "a red octopus".to_string(),
                size: None,
                model: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(generated.bytes, raw);
    assert_eq!(
        generated.revised_prompt.as_deref(),
        Some("a precise red octopus")
    );
    assert_eq!(generated.source, "inline_base64");
    let request = request.await.unwrap();
    assert!(request.contains("\"prompt\":\"a red octopus\""));
    assert!(request.contains("\"model\":\"image-model\""));
    assert!(request.contains("\"size\":\"1024x1024\""));
}

#[tokio::test]
async fn endpoint_refusal_is_explicit_and_body_is_bounded() {
    let (endpoint, request) = spawn_response(429, b"{\"error\":\"quota\"}".to_vec()).await;
    let failure = process(endpoint, 4096)
        .generate(
            &operation(4096),
            &ImageArguments {
                prompt: "an octopus".to_string(),
                size: None,
                model: None,
            },
        )
        .await
        .err()
        .unwrap();
    assert_eq!(failure.class, "endpoint");
    assert_eq!(failure.endpoint_status, Some(429));
    assert!(!failure.detail.contains("quota"));
    assert!(failure.detail.contains("bytes=17 blake3="));
    request.await.unwrap();
}

#[test]
fn tracked_headers_cannot_embed_secrets() {
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_string(), "secret".to_string());
    let name = super::parse_header_name("Authorization", "test").unwrap();
    assert!(super::reject_secret_header(&name).is_err());
    assert!(super::parse_http_url("http://image.example/v1", "test").is_err());
    assert!(super::parse_http_url("http://127.0.0.1:8080/v1", "test").is_ok());
    assert!(super::parse_http_url("https://image.example/v1", "test").is_ok());
}

#[test]
fn provider_media_url_requires_declared_origin_authority() {
    let endpoint = reqwest::Url::parse("https://image.example/v1/images").unwrap();
    let same_origin = reqwest::Url::parse("https://image.example/generated/one.png").unwrap();
    let foreign = reqwest::Url::parse("https://cdn.example/generated/one.png").unwrap();

    assert!(super::require_allowed_media_origin(&same_origin, &endpoint, true, &[]).is_ok());
    assert!(super::require_allowed_media_origin(&foreign, &endpoint, true, &[]).is_err());
    assert!(super::require_allowed_media_origin(
        &foreign,
        &endpoint,
        false,
        &["https://cdn.example".to_string()],
    )
    .is_ok());
    assert!(super::parse_declared_origin("https://cdn.example/path", "test origin").is_err());
    assert!(super::parse_declared_origin("http://localhost:8080", "test origin").is_ok());
}

#[test]
fn interface_descriptor_owns_the_generation_contract() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json");
    let descriptor: octopus_interfaces::InterfaceDescriptor =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let operation = super::parse_operation(&descriptor, &descriptor.id).unwrap();
    assert_eq!(operation.input_request_label, "image.generation.request");
    assert_eq!(operation.media_label, "image.generated");
    assert_eq!(operation.model.as_deref(), Some("gpt-image-1"));
}
