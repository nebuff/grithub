//! HTTP request construction and response classification.

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::auth::{DEVICE_CODE_URL, TOKEN_URL};
use crate::host::HttpRequest;

pub const API_URL: &str = "https://api.github.com/graphql";
pub const USER_AGENT: &str = "grithub/0.1 (Grimoire)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    Unauthorized,
    RateLimited,
    NotFound,
    GraphQl(String),
    Http(u16),
    Malformed(String),
}

fn form_headers() -> Vec<(String, String)> {
    vec![
        ("Accept".into(), "application/json".into()),
        (
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ),
        ("User-Agent".into(), USER_AGENT.into()),
    ]
}

fn json_headers(token: &str) -> Vec<(String, String)> {
    vec![
        ("Accept".into(), "application/json".into()),
        ("Content-Type".into(), "application/json".into()),
        ("User-Agent".into(), USER_AGENT.into()),
        ("Authorization".into(), format!("bearer {token}")),
    ]
}

/// Percent-encode the few characters that may appear in our form fields.
fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn device_code_request(client_id: &str) -> HttpRequest {
    HttpRequest {
        method: "POST".to_owned(),
        url: DEVICE_CODE_URL.to_owned(),
        headers: form_headers(),
        body: format!(
            "client_id={}&scope={}",
            form_encode(client_id),
            form_encode(crate::auth::SCOPE)
        )
        .into_bytes(),
    }
}

pub fn token_poll_request(client_id: &str, device_code: &str) -> HttpRequest {
    HttpRequest {
        method: "POST".to_owned(),
        url: TOKEN_URL.to_owned(),
        headers: form_headers(),
        body: format!(
            "client_id={}&device_code={}&grant_type={}",
            form_encode(client_id),
            form_encode(device_code),
            form_encode("urn:ietf:params:oauth:grant-type:device_code")
        )
        .into_bytes(),
    }
}

pub fn graphql_request(token: &str, query: &str, variables: Value) -> HttpRequest {
    let body = json!({ "query": query, "variables": variables });
    HttpRequest {
        method: "POST".to_owned(),
        url: API_URL.to_owned(),
        headers: json_headers(token),
        body: serde_json::to_vec(&body).unwrap_or_default(),
    }
}

/// Map a transport result onto `data`, or a typed failure the app can act on.
pub fn classify(status: u16, body: &[u8]) -> Result<Value, ApiError> {
    match status {
        401 => return Err(ApiError::Unauthorized),
        403 | 429 => return Err(ApiError::RateLimited),
        404 => return Err(ApiError::NotFound),
        200 => {}
        other => return Err(ApiError::Http(other)),
    }

    let v: Value = serde_json::from_slice(body).map_err(|e| ApiError::Malformed(e.to_string()))?;

    // GitHub returns partial data alongside errors; real data wins.
    if let Some(data) = v.get("data") {
        if !data.is_null() {
            return Ok(data.clone());
        }
    }

    if let Some(first) = v.get("errors").and_then(Value::as_array).and_then(|e| e.first()) {
        let kind = first.get("type").and_then(Value::as_str).unwrap_or_default();
        let msg = first
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("GraphQL error");
        return Err(match kind {
            "RATE_LIMITED" => ApiError::RateLimited,
            "NOT_FOUND" => ApiError::NotFound,
            _ => ApiError::GraphQl(msg.to_owned()),
        });
    }

    Err(ApiError::Malformed("response contained no data".to_owned()))
}

pub fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, ApiError> {
    serde_json::from_value(value.clone()).map_err(|e| ApiError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_code_request_is_a_form_post() {
        let r = device_code_request("Iv1.abc");
        assert_eq!(r.method, "POST");
        assert_eq!(r.url, DEVICE_CODE_URL);
        assert_eq!(
            String::from_utf8(r.body).unwrap(),
            "client_id=Iv1.abc&scope=read%3Aproject"
        );
        assert!(r
            .headers
            .iter()
            .any(|(k, v)| k == "Accept" && v == "application/json"));
    }

    #[test]
    fn token_poll_uses_the_device_grant() {
        let r = token_poll_request("id", "dc");
        let body = String::from_utf8(r.body).unwrap();
        assert!(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
    }

    #[test]
    fn graphql_request_carries_the_bearer_token() {
        let r = graphql_request("gho_x", "{ viewer { login } }", json!({}));
        assert_eq!(r.url, API_URL);
        assert!(r
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "bearer gho_x"));
        // GitHub rejects requests without a User-Agent.
        assert!(r.headers.iter().any(|(k, _)| k == "User-Agent"));
    }

    #[test]
    fn status_codes_map_to_actionable_errors() {
        assert_eq!(classify(401, b""), Err(ApiError::Unauthorized));
        assert_eq!(classify(403, b""), Err(ApiError::RateLimited));
        assert_eq!(classify(404, b""), Err(ApiError::NotFound));
        assert_eq!(classify(500, b""), Err(ApiError::Http(500)));
    }

    #[test]
    fn rate_limited_graphql_errors_are_distinguished() {
        let body = br#"{"data":null,"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#;
        assert_eq!(classify(200, body), Err(ApiError::RateLimited));
    }

    #[test]
    fn partial_data_wins_over_errors() {
        let body = br#"{"data":{"viewer":{"login":"octocat"}},"errors":[{"message":"partial"}]}"#;
        let data = classify(200, body).unwrap();
        assert_eq!(data["viewer"]["login"], "octocat");
    }
}
