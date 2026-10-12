use super::{ExternalDetail, ExternalSearchResult, MetadataProvider};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

/// IMDb metadata through the OMDb API (https://www.omdbapi.com), the
/// community-standard way to query IMDb data with a free API key.
pub struct OmdbProvider {
    client: Client,
    api_key: String,
}

impl OmdbProvider {
    pub fn new(client: Client, api_key: String) -> Self {
        Self { client, api_key }
    }

    async fn call(&self, params: &[(&str, &str)]) -> ApiResult<Value> {
        let response = self
            .client
            .get("https://www.omdbapi.com/")
            .query(&[("apikey", self.api_key.as_str())])
            .query(params)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(ApiError::Unavailable(format!(
                "omdb request failed: {}",
                response.status()
            )));
        }
        let body: Value = response.json().await?;
        if body.get("Response").and_then(|r| r.as_str()) == Some("False") {
            let reason = body
                .get("Error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error");
            if reason.contains("not found") || reason.contains("Incorrect IMDb") {
                return Err(ApiError::not_found(format!("imdb title ({reason})")));
            }
            return Err(ApiError::Unavailable(format!("omdb error: {reason}")));
        }
        Ok(body)
    }

    fn runtime_minutes(body: &Value) -> Option<i32> {
        body.get("Runtime")
            .and_then(|r| r.as_str())
            .and_then(|s| s.split_whitespace().next())
            .and_then(|n| n.parse().ok())
    }

    fn released_date(body: &Value) -> Option<chrono::NaiveDate> {
        body.get("Released")
            .and_then(|r| r.as_str())
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%d %b %Y").ok())
    }

    /// `"8.4"` -> `8.4`. OMDb writes "N/A" for anything it lacks.
    fn number(body: &Value, field: &str) -> Option<f64> {
        body.get(field)
            .and_then(|v| v.as_str())
            .filter(|v| *v != "N/A")
            .and_then(|v| v.split('/').next())
            .and_then(|v| v.trim().parse().ok())
    }

    /// `"2,458,193"` -> `2458193`.
    fn votes(body: &Value) -> Option<i32> {
        body.get("imdbVotes")
            .and_then(|v| v.as_str())
            .filter(|v| *v != "N/A")
            .map(|v| v.replace(',', ""))
            .and_then(|v| v.parse().ok())
    }

    fn text(body: &Value, field: &str) -> Option<String> {
        body.get(field)
            .and_then(|v| v.as_str())
            .filter(|v| *v != "N/A" && !v.trim().is_empty())
            .map(str::to_string)
    }

    fn year_to_status(body: &Value) -> Option<String> {
        let status = body.get("Type").and_then(|t| t.as_str())?;
        match status {
            "series" => Some("ongoing".to_string()),
            _ => Some("released".to_string()),
        }
    }
}

#[async_trait]
impl MetadataProvider for OmdbProvider {
    fn source(&self) -> String {
        "imdb".to_string()
    }

    async fn search(&self, query: &str) -> ApiResult<Vec<ExternalSearchResult>> {
        let body = self.call(&[("s", query.trim()), ("type", "")]).await?;
        let results = body
            .get("Search")
            .and_then(|s| s.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(results
            .iter()
            .filter_map(|item| {
                let id = item.get("imdbID")?.as_str()?.to_string();
                Some(ExternalSearchResult {
                    source: "imdb".to_string(),
                    external_id: id.clone(),
                    title: item.get("Title")?.as_str()?.to_string(),
                    overview: String::new(),
                    year: item
                        .get("Year")
                        .and_then(|y| y.as_str())
                        .and_then(|y| y.get(..4))
                        .and_then(|y| y.parse().ok()),
                    poster: item
                        .get("Poster")
                        .and_then(|p| p.as_str())
                        .filter(|p| *p != "N/A")
                        .map(str::to_string),
                    media_type_hint: item.get("Type").and_then(|t| t.as_str()).map(|t| {
                        if t == "series" {
                            "tv".to_string()
                        } else {
                            "movie".to_string()
                        }
                    }),
                })
            })
            .collect())
    }

    async fn details(&self, external_id: &str) -> ApiResult<ExternalDetail> {
        let id = external_id.trim();
        let is_imdb_id = id.starts_with("tt") && id[2..].chars().all(|c| c.is_ascii_digit());
        let body = if is_imdb_id {
            self.call(&[("i", id), ("plot", "full")]).await?
        } else {
            self.call(&[("t", id), ("plot", "full")]).await?
        };

        let genres: Vec<String> = body
            .get("Genre")
            .and_then(|g| g.as_str())
            .map(|g| {
                g.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let mut cast = Vec::new();
        if let Some(actors) = body.get("Actors").and_then(|a| a.as_str()) {
            for (i, actor) in actors.split(',').enumerate().take(15) {
                cast.push((actor.trim().to_string(), None, i as i32));
            }
        }

        let media_type = body.get("Type").and_then(|t| t.as_str()).map(|t| {
            if t == "series" {
                "tv".to_string()
            } else {
                "movie".to_string()
            }
        });

        Ok(ExternalDetail {
            title: body
                .get("Title")
                .and_then(|t| t.as_str())
                .unwrap_or("Untitled")
                .to_string(),
            original_title: None,
            overview: body
                .get("Plot")
                .and_then(|p| p.as_str())
                .filter(|p| *p != "N/A")
                .unwrap_or("")
                .to_string(),
            release_date: Self::released_date(&body),
            runtime_minutes: Self::runtime_minutes(&body),
            status: Self::year_to_status(&body),
            poster: body
                .get("Poster")
                .and_then(|p| p.as_str())
                .filter(|p| *p != "N/A")
                .map(str::to_string),
            backdrop: None,
            is_anime: false,
            media_type,
            genres,
            studios: Self::text(&body, "Production")
                .map(|p| vec![p])
                .unwrap_or_default(),
            cast,
            imdb_id: Self::text(&body, "imdbID"),
            imdb_rating: Self::number(&body, "imdbRating"),
            imdb_votes: Self::votes(&body),
            metascore: Self::number(&body, "Metascore").map(|m| m as i32),
            content_rating: Self::text(&body, "Rated"),
        })
    }
}
