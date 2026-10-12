use crate::catalog::metadata::{ExternalDetail, ExternalSearchResult, MetadataProvider};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

/// Adapter that turns any HTTP endpoint implementing the plugin protocol
/// into a `MetadataProvider`. This is the core of the extension system:
/// third parties can run a tiny service in any language and plug it in at
/// runtime from the admin page.
pub struct HttpPluginProvider {
    name: String,
    base_url: String,
    /// optional bearer token forwarded to the plugin on every call
    auth_token: Option<String>,
    client: Client,
}

impl HttpPluginProvider {
    pub fn new(name: String, base_url: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");
        Self {
            name,
            base_url: base_url.trim_end_matches('/').to_string(),
            auth_token: None,
            client,
        }
    }

    #[allow(dead_code)]
    fn validate_base_url(base_url: &str) -> ApiResult<()> {
        let parsed = url::Url::parse(base_url)
            .map_err(|_| ApiError::bad_request("base_url must be an absolute http(s) URL"))?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(ApiError::bad_request("base_url must use http or https"));
        }
        if parsed.host_str().is_none() {
            return Err(ApiError::bad_request("base_url must include a host"));
        }
        Ok(())
    }

    pub fn with_auth(name: String, base_url: String, token: Option<String>) -> ApiResult<Self> {
        let mut provider = Self::checked(name, base_url)?;
        provider.auth_token = token;
        Ok(provider)
    }

    pub fn checked(name: String, base_url: String) -> ApiResult<Self> {
        Self::validate_base_url(&base_url)?;
        Ok(Self::new(name, base_url))
    }

    async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> ApiResult<Value> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .query(query)
            .header("authorization", self.auth_token.as_deref().unwrap_or(""))
            .send()
            .await
            .map_err(|e| {
                ApiError::Unavailable(format!("plugin '{}' unreachable: {e}", self.name))
            })?;

        if !response.status().is_success() {
            return Err(ApiError::Unavailable(format!(
                "plugin '{}' returned {}",
                self.name,
                response.status()
            )));
        }
        response.json::<Value>().await.map_err(|e| {
            ApiError::Unavailable(format!("plugin '{}' sent invalid json: {e}", self.name))
        })
    }
}

#[async_trait]
impl MetadataProvider for HttpPluginProvider {
    fn source(&self) -> String {
        self.name.clone()
    }

    async fn search(&self, query_text: &str) -> ApiResult<Vec<ExternalSearchResult>> {
        let body = self.get_json("/search", &[("q", query_text)]).await?;
        let results = body
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(results
            .iter()
            .filter_map(|item| {
                Some(ExternalSearchResult {
                    source: self.name.clone(),
                    external_id: item.get("id")?.as_str()?.to_string(),
                    title: item
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Untitled")
                        .to_string(),
                    overview: item
                        .get("overview")
                        .and_then(|o| o.as_str())
                        .unwrap_or("")
                        .to_string(),
                    year: item.get("year").and_then(|y| y.as_i64()).map(|y| y as i32),
                    poster: item
                        .get("poster")
                        .and_then(|p| p.as_str())
                        .map(str::to_string),
                    media_type_hint: item
                        .get("type")
                        .and_then(|t| t.as_str())
                        .map(str::to_string),
                })
            })
            .collect())
    }

    async fn details(&self, external_id: &str) -> ApiResult<ExternalDetail> {
        let body = self.get_json("/details", &[("id", external_id)]).await?;

        let genres = body
            .get("genres")
            .and_then(|g| g.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|g| g.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let studios = body
            .get("studios")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let cast = body
            .get("cast")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .enumerate()
                    .filter_map(|(i, member)| {
                        Some((
                            member.get("name")?.as_str()?.to_string(),
                            member
                                .get("character")
                                .and_then(|c| c.as_str())
                                .map(str::to_string),
                            i as i32,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let raw_type = body.get("media_type").and_then(|t| t.as_str());
        let is_anime = matches!(raw_type, Some("anime"));

        Ok(ExternalDetail {
            title: body
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("Untitled")
                .to_string(),
            original_title: body
                .get("original_title")
                .and_then(|t| t.as_str())
                .map(str::to_string),
            overview: body
                .get("overview")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string(),
            release_date: body
                .get("release_date")
                .and_then(|d| d.as_str())
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
            runtime_minutes: body
                .get("runtime_minutes")
                .and_then(|r| r.as_i64())
                .map(|r| r as i32),
            status: body
                .get("status")
                .and_then(|s| s.as_str())
                .map(str::to_string),
            poster: body
                .get("poster")
                .and_then(|p| p.as_str())
                .map(str::to_string),
            backdrop: body
                .get("backdrop")
                .and_then(|b| b.as_str())
                .map(str::to_string),
            is_anime,
            media_type: match raw_type {
                Some("movie") => Some("movie".to_string()),
                _ => Some("tv".to_string()),
            },
            genres,
            studios,
            cast,
            // A plugin may know the IMDb id; ratings stay the sync's job so
            // every title is rated from one source.
            imdb_id: body
                .get("imdb_id")
                .and_then(|v| v.as_str())
                .filter(|v| v.starts_with("tt"))
                .map(str::to_string),
            imdb_rating: None,
            imdb_votes: None,
            metascore: None,
            content_rating: None,
        })
    }
}
