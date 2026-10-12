use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PageQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl PageQuery {
    pub fn clamp_limit(&self) -> i64 {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    #[allow(dead_code)]
    pub fn of(items: Vec<T>) -> Self {
        Self {
            items,
            next_cursor: None,
        }
    }

    pub fn with_cursor(items: Vec<T>, cursor: Option<String>) -> Self {
        Self {
            items,
            next_cursor: cursor,
        }
    }
}

pub fn encode_cursor(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).expect("cursor serialization"))
}

pub fn decode_cursor(raw: &str) -> Result<Value, crate::error::ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| crate::error::ApiError::bad_request("invalid pagination cursor"))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| crate::error::ApiError::bad_request("invalid pagination cursor"))
}
