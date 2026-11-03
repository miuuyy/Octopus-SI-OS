use base64::Engine;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Map;
use serde_json::Value;

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub(crate) struct TokenData {
    #[serde(
        deserialize_with = "deserialize_id_token",
        serialize_with = "serialize_id_token"
    )]
    pub id_token: IdTokenInfo,

    pub access_token: String,

    pub refresh_token: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_provenance: Option<AccountOAuthProvenance>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_owner_id: Option<String>,

    pub account_id: Option<String>,

    #[serde(flatten)]
    pub other: Map<String, Value>,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountOAuthProvenance {
    pub issuer: String,
    pub client_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IdTokenInfo {
    pub chatgpt_user_id: Option<String>,
    pub raw_jwt: String,
}

#[derive(Deserialize)]
struct IdClaims {
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_user_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
}

#[derive(Deserialize)]
struct StandardJwtClaims {
    #[serde(default)]
    exp: Option<i64>,
}

#[derive(Debug)]
pub(crate) enum IdTokenInfoError {
    InvalidFormat,
    Base64(base64::DecodeError),
    Json(serde_json::Error),
}

impl std::fmt::Display for IdTokenInfoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFormat => f.write_str("invalid ID token format"),
            Self::Base64(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for IdTokenInfoError {}

fn decode_jwt_payload<T: DeserializeOwned>(jwt: &str) -> Result<T, IdTokenInfoError> {
    let mut parts = jwt.split('.');
    let payload_b64 = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty() => p,
        _ => return Err(IdTokenInfoError::InvalidFormat),
    };
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(IdTokenInfoError::Base64)?;
    serde_json::from_slice(&payload_bytes).map_err(IdTokenInfoError::Json)
}

pub(crate) fn parse_jwt_expiration(jwt: &str) -> Result<Option<DateTime<Utc>>, IdTokenInfoError> {
    let claims: StandardJwtClaims = decode_jwt_payload(jwt)?;
    Ok(claims
        .exp
        .and_then(|exp| DateTime::<Utc>::from_timestamp(exp, 0)))
}

pub(crate) fn parse_chatgpt_jwt_claims(jwt: &str) -> Result<IdTokenInfo, IdTokenInfoError> {
    let claims: IdClaims = decode_jwt_payload(jwt)?;
    Ok(IdTokenInfo {
        chatgpt_user_id: claims
            .auth
            .and_then(|auth| auth.chatgpt_user_id.or(auth.user_id)),
        raw_jwt: jwt.to_string(),
    })
}

fn deserialize_id_token<'de, D>(deserializer: D) -> Result<IdTokenInfo, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_chatgpt_jwt_claims(&s).map_err(serde::de::Error::custom)
}

fn serialize_id_token<S>(id_token: &IdTokenInfo, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&id_token.raw_jwt)
}
