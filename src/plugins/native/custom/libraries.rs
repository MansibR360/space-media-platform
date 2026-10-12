//! Self-hosted library servers: Komga and Kavita.
//!
//! Books and manga were the weak spot — the public book sources contribute
//! almost nothing to search, so a query for a novel came back with films.
//! These two are the answer people actually use: your own library, with a real
//! API, holding the books and comics you already have.
//!
//! Both are off unless configured, since neither exists without a server to
//! point at:
//!
//! | server | url | credential |
//! |---|---|---|
//! | Komga | `KOMGA_URL` | `KOMGA_API_KEY` |
//! | Kavita | `KAVITA_URL` | `KAVITA_API_KEY` |
//!
//! Komga takes its key on every request. Kavita trades the key for a JWT
//! first, so that token is fetched once and reused until it stops working.

use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::error::{ApiError, ApiResult};
use crate::plugins::native::engines::{EngineCtx, truncate};

fn env_url(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
}

fn env_key(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ------------------------------------------------------------------- Komga

#[derive(Debug, Clone)]
pub struct Komga {
    pub base: String,
    pub api_key: String,
}

impl Komga {
    /// Configured only when both the url and a key are present.
    pub fn from_env() -> Option<Self> {
        Some(Self {
            base: env_url("KOMGA_URL")?,
            api_key: env_key("KOMGA_API_KEY")?,
        })
    }

    fn headers(&self) -> Vec<(&str, &str)> {
        vec![("x-api-key", self.api_key.as_str())]
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        let url = format!(
            "{}/api/v1/series?search={}&size={}",
            self.base,
            crate::plugins::native::engines::quote_plus(query),
            limit.clamp(1, 50)
        );
        let body = match ctx.get_json_with(&url, &self.headers()).await {
            Ok(body) => body,
            Err(err) => {
                return Ok(json!({ "results": [], "note": format!("komga unreachable: {err}") }));
            }
        };

        // Komga pages its answers; older builds return a bare array.
        let rows = body
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| body.as_array().cloned())
            .unwrap_or_default();

        let results: Vec<Value> = rows
            .iter()
            .filter_map(|row| {
                let id = row.get("id").and_then(Value::as_str)?;
                Some(json!({
                    "id": format!("komga:{id}"),
                    "title": row.get("name").and_then(Value::as_str).unwrap_or("Untitled"),
                    "overview": truncate(
                        row.pointer("/metadata/summary").and_then(Value::as_str).unwrap_or(""),
                        400,
                    ),
                    "year": row.pointer("/booksMetadata/releaseDate")
                        .and_then(Value::as_str)
                        .and_then(|d| d.get(..4))
                        .and_then(|y| y.parse::<i64>().ok()),
                    "poster": format!("{}/api/v1/series/{id}/thumbnail", self.base),
                    "type": "manga",
                }))
            })
            .collect();
        Ok(json!({ "results": results }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let series_id = id
            .strip_prefix("komga:")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ApiError::bad_request("id must be komga:{series_id}"))?;

        let series = ctx
            .get_json_with(
                &format!("{}/api/v1/series/{series_id}", self.base),
                &self.headers(),
            )
            .await?;

        let books = ctx
            .get_json_with(
                &format!("{}/api/v1/series/{series_id}/books?size=500", self.base),
                &self.headers(),
            )
            .await
            .unwrap_or_else(|_| json!({}));

        let rows = books
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| books.as_array().cloned())
            .unwrap_or_default();

        // Each book is a file the reader can open directly.
        let streams: Vec<Value> = rows
            .iter()
            .filter_map(|book| {
                let book_id = book.get("id").and_then(Value::as_str)?;
                let name = book.get("name").and_then(Value::as_str).unwrap_or("Book");
                Some(json!({
                    "label": name,
                    "url": format!("{}/api/v1/books/{book_id}/file", self.base),
                    "kind": "file",
                }))
            })
            .collect();

        Ok(json!({
            "title": series.get("name").and_then(Value::as_str).unwrap_or("Untitled"),
            "overview": truncate(
                series.pointer("/metadata/summary").and_then(Value::as_str).unwrap_or(""),
                600,
            ),
            "poster": format!("{}/api/v1/series/{series_id}/thumbnail", self.base),
            "year": Value::Null,
            "media_type": "manga",
            "streams": streams,
        }))
    }
}

// ------------------------------------------------------------------ Kavita

#[derive(Clone)]
pub struct Kavita {
    pub base: String,
    pub api_key: String,
    /// The JWT the key was traded for. Held so the exchange happens once.
    token: Arc<RwLock<Option<String>>>,
}

impl std::fmt::Debug for Kavita {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kavita").field("base", &self.base).finish()
    }
}

impl Kavita {
    pub fn from_env() -> Option<Self> {
        Some(Self {
            base: env_url("KAVITA_URL")?,
            api_key: env_key("KAVITA_API_KEY")?,
            token: Arc::new(RwLock::new(None)),
        })
    }

    /// Kavita does not take the api key directly: it is exchanged for a JWT
    /// through the plugin endpoint, and that token is what every other call
    /// carries.
    async fn token(&self, ctx: &EngineCtx) -> ApiResult<String> {
        if let Some(token) = self.token.read().await.clone() {
            return Ok(token);
        }
        let url = format!(
            "{}/api/Plugin/authenticate?apiKey={}&pluginName=SPACE",
            self.base,
            crate::plugins::native::engines::quote_plus(&self.api_key)
        );
        let body = ctx.post_form(&url, &[]).await?;
        let token = body
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::Unavailable("kavita refused the api key".into()))?
            .to_string();

        *self.token.write().await = Some(token.clone());
        Ok(token)
    }

    /// Forget a token the server has stopped accepting, so the next call
    /// exchanges the key again rather than failing forever.
    async fn forget_token(&self) {
        *self.token.write().await = None;
    }

    pub async fn search(&self, ctx: &EngineCtx, query: &str, limit: usize) -> ApiResult<Value> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(json!({ "results": [] }));
        }
        let token = match self.token(ctx).await {
            Ok(token) => token,
            Err(err) => {
                return Ok(json!({ "results": [], "note": format!("kavita unavailable: {err}") }));
            }
        };
        let auth = format!("Bearer {token}");
        let url = format!(
            "{}/api/Search/search?queryString={}",
            self.base,
            crate::plugins::native::engines::quote_plus(query)
        );

        let body = match ctx.get_json_with(&url, &[("authorization", &auth)]).await {
            Ok(body) => body,
            Err(err) => {
                // most likely an expired token; drop it so the next try re-auths
                self.forget_token().await;
                return Ok(json!({ "results": [], "note": format!("kavita search failed: {err}") }));
            }
        };

        let results: Vec<Value> = body
            .get("series")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .take(limit)
            .filter_map(|row| {
                let id = row.get("seriesId").and_then(Value::as_i64)?;
                Some(json!({
                    "id": format!("kavita:{id}"),
                    "title": row.get("name").and_then(Value::as_str).unwrap_or("Untitled"),
                    "overview": "",
                    "year": Value::Null,
                    "poster": format!("{}/api/Image/series-cover?seriesId={id}", self.base),
                    "type": "book",
                }))
            })
            .collect();
        Ok(json!({ "results": results }))
    }

    pub async fn details(&self, ctx: &EngineCtx, id: &str) -> ApiResult<Value> {
        let series_id = id
            .strip_prefix("kavita:")
            .filter(|v| v.chars().all(|c| c.is_ascii_digit()) && !v.is_empty())
            .ok_or_else(|| ApiError::bad_request("id must be kavita:{series_id}"))?;

        let token = self.token(ctx).await?;
        let auth = format!("Bearer {token}");

        let series = ctx
            .get_json_with(
                &format!("{}/api/Series/{series_id}", self.base),
                &[("authorization", &auth)],
            )
            .await
            .map_err(|err| {
                ApiError::Unavailable(format!("kavita series lookup failed: {err}"))
            })?;

        Ok(json!({
            "title": series.get("name").and_then(Value::as_str).unwrap_or("Untitled"),
            "overview": truncate(
                series.get("summary").and_then(Value::as_str).unwrap_or(""),
                600,
            ),
            "poster": format!("{}/api/Image/series-cover?seriesId={series_id}", self.base),
            "year": Value::Null,
            "media_type": "book",
            // The whole series as a file the reader opens. Kavita serves this
            // with the api key on the query string, which is how its own OPDS
            // clients fetch it.
            "streams": [{
                "label": "Read",
                "url": format!(
                    "{}/api/Download/series?seriesId={series_id}&apiKey={}",
                    self.base,
                    crate::plugins::native::engines::quote_plus(&self.api_key)
                ),
                "kind": "file",
            }],
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_without_configuration_stays_off() {
        // no env set in the test process: neither should register
        unsafe {
            std::env::remove_var("KOMGA_URL");
            std::env::remove_var("KAVITA_URL");
        }
        assert!(Komga::from_env().is_none());
        assert!(Kavita::from_env().is_none());
    }

    #[test]
    fn a_url_alone_is_not_enough() {
        unsafe {
            std::env::set_var("KOMGA_URL", "https://books.example");
            std::env::remove_var("KOMGA_API_KEY");
        }
        assert!(
            Komga::from_env().is_none(),
            "a server with no credential cannot be called"
        );
        unsafe { std::env::remove_var("KOMGA_URL") };
    }

    #[test]
    fn a_trailing_slash_does_not_double_up() {
        unsafe {
            std::env::set_var("KOMGA_URL", "https://books.example/");
            std::env::set_var("KOMGA_API_KEY", "k");
        }
        let komga = Komga::from_env().expect("configured");
        assert_eq!(komga.base, "https://books.example");
        unsafe {
            std::env::remove_var("KOMGA_URL");
            std::env::remove_var("KOMGA_API_KEY");
        }
    }
}
