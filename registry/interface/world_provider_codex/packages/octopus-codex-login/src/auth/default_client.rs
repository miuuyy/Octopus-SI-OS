use crate::user_agent::user_agent;
use octopus_client::BuildCustomCaTransportError;
use octopus_client::CodexHttpClient;
use octopus_client::build_reqwest_client_with_custom_ca;
use octopus_client::with_chatgpt_cloudflare_cookie_store;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use reqwest::header::USER_AGENT;

const DEFAULT_ORIGINATOR: &str = "codex_cli_rs";
const CODEX_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR: &str = "CODEX_INTERNAL_ORIGINATOR_OVERRIDE";

struct Originator {
    value: String,
    header_value: HeaderValue,
}

fn originator() -> Originator {
    let value = std::env::var(CODEX_INTERNAL_ORIGINATOR_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| DEFAULT_ORIGINATOR.to_string());
    match HeaderValue::from_str(&value) {
        Ok(header_value) => Originator {
            value,
            header_value,
        },
        Err(e) => {
            tracing::error!("Unable to turn originator override {value} into header value: {e}");
            Originator {
                value: DEFAULT_ORIGINATOR.to_string(),
                header_value: HeaderValue::from_static(DEFAULT_ORIGINATOR),
            }
        }
    }
}

fn codex_user_agent(originator: &str) -> String {
    let os_info = os_info::get();
    let candidate = format!(
        "{originator}/{} ({} {}; {}) {}",
        env!("CARGO_PKG_VERSION"),
        os_info.os_type(),
        os_info.version(),
        os_info.architecture().unwrap_or("unknown"),
        user_agent()
    );
    if HeaderValue::from_str(&candidate).is_ok() {
        return candidate;
    }
    candidate
        .chars()
        .map(|ch| if matches!(ch, ' '..='~') { ch } else { '_' })
        .collect()
}

fn default_headers() -> HeaderMap {
    let originator = originator();
    let mut headers = HeaderMap::new();
    if let Ok(user_agent) = HeaderValue::from_str(&codex_user_agent(&originator.value)) {
        headers.insert(USER_AGENT, user_agent);
    }
    headers.insert("originator", originator.header_value);
    headers
}

pub fn try_create_client() -> Result<CodexHttpClient, BuildCustomCaTransportError> {
    let mut builder = reqwest::Client::builder().default_headers(default_headers());
    if std::env::var("CODEX_SANDBOX").as_deref() == Ok("seatbelt") {
        builder = builder.no_proxy();
    }
    build_reqwest_client_with_custom_ca(with_chatgpt_cloudflare_cookie_store(builder))
        .map(CodexHttpClient::new)
}
