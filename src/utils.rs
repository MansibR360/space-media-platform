use chrono::{DateTime, Utc};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use uuid::Uuid;

static EMAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^@\s]+@[^@\s]+\.[^@\s]{2,}$").expect("email regex"));
static USERNAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_.-]{3,32}$").expect("username regex"));
static UUID_SEG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
        .expect("uuid regex")
});
static NUMERIC_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+$").expect("numeric regex"));

pub fn now() -> DateTime<Utc> {
    Utc::now()
}

pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn random_hex(bytes: usize) -> String {
    // UUIDv4 is sourced from the OS CSPRNG (getrandom); chaining gives us
    // arbitrary-length high-entropy tokens without extra deps.
    let mut out = String::with_capacity(bytes * 2);
    while out.len() < bytes * 2 {
        out.push_str(&hex::encode(Uuid::new_v4().as_bytes()));
    }
    out.truncate(bytes * 2);
    out
}

pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = true;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

pub fn validate_email(email: &str) -> bool {
    email.len() <= 254 && EMAIL_RE.is_match(email)
}

pub fn validate_username(username: &str) -> bool {
    USERNAME_RE.is_match(username)
}

pub fn normalize_route_path(path: &str) -> String {
    path.split('/')
        .map(|seg| match seg {
            "" | ":id" => seg.to_string(),
            s if NUMERIC_RE.is_match(s) || UUID_SEG_RE.is_match(s) => ":id".to_string(),
            s => s.to_string(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub fn parse_cookies(header_value: &str) -> Vec<(String, String)> {
    header_value
        .split(';')
        .filter_map(|pair| {
            pair.trim()
                .split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect()
}

#[allow(dead_code)]
pub fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((k.to_string(), v.replace("%20", " ")))
        })
        .collect()
}

pub fn clamp_f64(value: f64, min: f64, max: f64) -> f64 {
    value.clamp(min, max)
}

const PAYLOAD_ENGINE: base64::engine::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// URL-safe payload encoding for signed download tokens.
pub fn encode_payload(raw: &str) -> String {
    use base64::Engine as _;
    PAYLOAD_ENGINE.encode(raw.as_bytes())
}

pub fn decode_payload(encoded: &str) -> Result<String, crate::error::ApiError> {
    use base64::Engine as _;
    let bytes = PAYLOAD_ENGINE
        .decode(encoded)
        .map_err(|_| crate::error::ApiError::forbidden("malformed payload"))?;
    String::from_utf8(bytes).map_err(|_| crate::error::ApiError::forbidden("malformed payload"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basics() {
        assert_eq!(slugify("Attack on Titan!"), "attack-on-titan");
        assert_eq!(slugify("  --Hello World-- "), "hello-world");
    }

    #[test]
    fn token_hash_is_deterministic() {
        let t = "abc";
        assert_eq!(sha256_hex(t), sha256_hex(t));
        assert_ne!(sha256_hex(t), sha256_hex("abd"));
    }

    #[test]
    fn route_normalization() {
        let normalized =
            normalize_route_path("/api/media/0192c0d5-1111-7222-8333-444455556666/episodes");
        assert!(normalized.starts_with("/api/media/:id"), "got {normalized}");
        assert_eq!(normalize_route_path("/api/trending"), "/api/trending");
    }

    #[test]
    fn cookie_parsing() {
        let cookies = parse_cookies("a=1; b=\"two\"; c=3 ");
        assert_eq!(cookies[0], ("a".into(), "1".into()));
        assert_eq!(cookies[1], ("b".into(), "two".into()));
    }
}
