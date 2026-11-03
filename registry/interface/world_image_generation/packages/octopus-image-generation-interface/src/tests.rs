use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use reqwest::header::HeaderMap;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{
    GenerateArguments, GeneratedImage, ImageAuth, ImageCallFailure, ImageGenerationProcess,
    ImageHandler, ImageTransport,
};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nIMAGE";

async fn spawn_response(
    status: u16,
    body: Vec<u8>,
) -> (reqwest::Url, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint =
        reqwest::Url::parse(&format!("http://{}/images", listener.local_addr().unwrap())).unwrap();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = vec![0; 16 * 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            request.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&request).to_string();
            if let Some(split) = text.find("\r\n\r\n") {
                let length = text[..split]
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(str::to_string)
                    })
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= split + 4 + length || read == 0 {
                    break;
                }
            }
        }
        let head = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        socket.flush().await.unwrap();
        String::from_utf8_lossy(&request).to_string()
    });
    (endpoint, handle)
}

fn shipped_descriptor() -> octopus_interfaces::InterfaceDescriptor {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../interface.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn config() -> super::ImageConfig {
    let descriptor = shipped_descriptor();
    super::parse_config(&descriptor, &descriptor.id).unwrap()
}

fn account_auth(name: &str, expires_in: i64) -> ImageAuth {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let claims = URL_SAFE_NO_PAD.encode(json!({ "exp": now + expires_in }).to_string());
    let file = std::env::temp_dir().join(format!(
        "octopus-image-auth-{name}-{}.json",
        std::process::id()
    ));
    std::fs::write(
        &file,
        json!({ "auth_mode": "chatgpt", "tokens": { "access_token": format!("e30.{claims}.sig"), "account_id": "account-1" } }).to_string(),
    )
    .unwrap();
    let variable = format!("OCTOPUS_IMAGE_TEST_AUTH_{}", name.to_ascii_uppercase());
    std::env::set_var(&variable, &file);
    ImageAuth::ChatgptAccount {
        auth_file_env: variable,
        account_header: reqwest::header::HeaderName::from_static("chatgpt-account-id"),
    }
}

fn process(endpoint: reqwest::Url, auth: Option<ImageAuth>) -> ImageGenerationProcess {
    ImageGenerationProcess {
        transport: ImageTransport {
            endpoint,
            headers: HeaderMap::new(),
            auth,
            max_response_bytes: 4096,
        },
        client: Client::new(),
    }
}

fn catalog() -> super::ImageCatalog {
    super::load_catalog(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .as_path(),
        &config(),
    )
    .unwrap()
}

async fn generate(
    process: &ImageGenerationProcess,
    arguments: &GenerateArguments,
) -> Result<GeneratedImage, ImageCallFailure> {
    let request = process.request(&config(), arguments)?;
    process.generate(&config(), request).await
}

fn arguments(transparent: Option<bool>) -> GenerateArguments {
    GenerateArguments {
        prompt: "a red octopus".to_string(),
        size: Some("1024x1536".to_string()),
        quality: None,
        transparent_background: transparent,
    }
}

#[tokio::test]
async fn chatgpt_sign_in_reaches_the_image_service_as_its_account() {
    let response = serde_json::to_vec(&json!({
        "data": [{ "b64_json": BASE64_STANDARD.encode(PNG) }],
        "size": "1024x1536",
        "quality": "medium",
    }))
    .unwrap();
    let (endpoint, request) = spawn_response(200, response).await;
    let generated = generate(
        &process(endpoint, Some(account_auth("signed", 3600))),
        &arguments(Some(true)),
    )
    .await
    .unwrap();
    assert_eq!(generated.bytes, PNG);
    assert_eq!(generated.reported["quality"], "medium");
    let request = request.await.unwrap();
    assert!(request.contains("authorization: Bearer e30."));
    assert!(request.contains("chatgpt-account-id: account-1"));
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        body,
        json!({
            "prompt": "a red octopus",
            "model": "gpt-image-2",
            "size": "1024x1536",
            "quality": "auto",
            "background": "transparent",
        })
    );
}

#[tokio::test]
async fn expired_sign_in_never_reaches_the_service() {
    let unreachable = reqwest::Url::parse("http://127.0.0.1:9/images").unwrap();
    let failure = process(unreachable, Some(account_auth("expired", -60)))
        .request(&config(), &arguments(None))
        .err()
        .unwrap();
    assert_eq!(failure.code, "account_sign_in_expired");
}

#[tokio::test]
async fn service_refusals_name_their_reason() {
    let usage = json!({ "error": { "type": "usage_limit_reached", "message": "Image limit reached", "resets_at": 1_900_000_000 } });
    let (endpoint, request) = spawn_response(429, serde_json::to_vec(&usage).unwrap()).await;
    let failure = generate(&process(endpoint, None), &arguments(None))
        .await
        .err()
        .unwrap();
    request.await.unwrap();
    assert_eq!(
        failure.value(),
        json!({ "code": "usage_limit_reached", "message": "Image limit reached", "retryable": false, "endpoint_status": 429, "resets_at": 1_900_000_000 })
    );
    let (endpoint, request) = spawn_response(400, b"not json".to_vec()).await;
    let failure = generate(&process(endpoint, None), &arguments(None))
        .await
        .err()
        .unwrap();
    request.await.unwrap();
    assert_eq!(failure.code, "request_refused");
    assert!(failure.message.contains("bytes=8 blake3="));
}

#[test]
fn result_hands_the_image_to_the_calling_task_as_media() {
    let config = config();
    let catalog = catalog();
    let root = std::env::temp_dir().join(format!("octopus-image-results-{}", std::process::id()));
    let call = super::CallIdentity {
        operation: "image.generate".to_string(),
        correlation_id: "call-1".to_string(),
    };
    let generated = super::GeneratedImage {
        bytes: PNG.to_vec(),
        revised_prompt: None,
        reported: serde_json::Map::new(),
    };
    let artifact =
        super::result_artifact(&root, &config, &catalog, &call, Ok(generated), 4096).unwrap();
    let result: Value = serde_json::from_slice(&artifact.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(result["label"], "image.operation.result");
    assert_eq!(result["correlation_id"], "call-1");
    assert_eq!(result["ok"], true);
    assert_eq!(result["media"][0]["mime_type"], "image/png");
    assert_eq!(
        BASE64_STANDARD
            .decode(result["media"][0]["payload_base64"].as_str().unwrap())
            .unwrap(),
        PNG
    );
    assert_eq!(
        result["media"][0]["artifact"]["payload_content_hash"],
        blake3::hash(PNG).to_hex().as_str()
    );
    let refused = super::result_artifact(
        &root,
        &config,
        &catalog,
        &call,
        Err(ImageCallFailure::new("rate_limited", "busy").retryable()),
        4096,
    )
    .unwrap();
    let refused: Value = serde_json::from_slice(&refused.read_payload_bytes().unwrap()).unwrap();
    assert_eq!(
        (
            refused["ok"].clone(),
            refused["error"]["code"].clone(),
            refused.get("result")
        ),
        (json!(false), json!("rate_limited"), None)
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn interface_descriptor_owns_the_generation_contract() {
    let descriptor = shipped_descriptor();
    let config = config();
    let catalog = catalog();
    assert_eq!(catalog.request_label, "image.operation.request");
    assert_eq!(catalog.result_label, "image.operation.result");
    assert_eq!(
        catalog
            .operations
            .get("image.generate")
            .map(|operation| operation.handler),
        Some(ImageHandler::Generate)
    );
    let transport = super::load_transport(&descriptor, &descriptor.id, &config).unwrap();
    assert!(transport.auth.as_ref().is_some_and(ImageAuth::is_account));
    assert_eq!(
        transport.endpoint.as_str(),
        "https://chatgpt.com/backend-api/codex/images/generations"
    );
}

#[test]
fn tracked_headers_cannot_embed_secrets() {
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
        &["https://cdn.example".to_string()]
    )
    .is_ok());
    assert!(super::parse_declared_origin("https://cdn.example/path", "test origin").is_err());
}

#[test]
fn arguments_outside_the_declared_schema_never_reach_the_service() {
    let catalog = catalog();
    let schema = &catalog.operations["image.generate"].input_schema;
    let check = |arguments: Value| super::check_arguments(schema, &arguments);
    assert!(check(json!({ "prompt": "a red octopus", "size": "1536x1024", "quality": "high", "transparent_background": true })).is_ok());
    assert!(check(json!({ "prompt": "a red octopus", "quality": "ultra" })).is_err());
    assert!(check(json!({ "prompt": "a red octopus", "size": "square" })).is_err());
    assert!(check(json!({ "prompt": "a red octopus", "model": "other" })).is_err());
    assert!(check(json!({ "size": "auto" })).is_err());
    assert!(super::admit_input_schema(&json!({ "type": "object", "properties": { "size": { "type": "string", "pattern": "^a$" } }, "additionalProperties": false })).is_err());
}

#[test]
fn an_interrupted_call_is_answered_once_and_never_repeated() {
    let journal = super::CallJournal {
        root: std::env::temp_dir().join(format!("octopus-image-calls-{}", std::process::id())),
    };
    let call = super::CallIdentity {
        operation: "image.generate".to_string(),
        correlation_id: "call-1".to_string(),
    };
    let other = super::CallIdentity {
        operation: "image.generate".to_string(),
        correlation_id: "call-2".to_string(),
    };
    assert!(!journal.interrupted(7, &call).unwrap());
    journal.begin(7, &call).unwrap();
    assert!(journal.interrupted(7, &call).unwrap());
    assert!(journal.interrupted(7, &other).is_err());
    journal.settle(7).unwrap();
    assert!(!journal.interrupted(7, &call).unwrap());
    std::fs::remove_dir_all(&journal.root).unwrap();
}
