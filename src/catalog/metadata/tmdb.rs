use super::{ExternalDetail, ExternalSearchResult, MetadataProvider};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

pub struct TmdbProvider {
    client: Client,
    api_key: String,
}

impl TmdbProvider {
    pub fn new(client: Client, api_key: String) -> Self {
        Self { client, api_key }
    }

    fn year_of(value: &Value, key: &str) -> Option<i32> {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| s.get(..4))
            .and_then(|y| y.parse().ok())
    }

    fn date_of(value: &Value, key: &str) -> Option<chrono::NaiveDate> {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
    }

    fn image(path: Option<&str>) -> Option<String> {
        path.filter(|p| !p.is_empty())
            .map(|p| format!("https://image.tmdb.org/t/p/w500{p}"))
    }

    fn map_search(kind: &str, body: &Value) -> Vec<ExternalSearchResult> {
        body.get("results")
            .and_then(|r| r.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let id = item.get("id")?.as_i64()?.to_string();
                        let title = item
                            .get("title")
                            .or_else(|| item.get("name"))
                            .and_then(|t| t.as_str())?
                            .to_string();
                        Some(ExternalSearchResult {
                            source: "tmdb".to_string(),
                            external_id: format!("{kind}:{id}"),
                            title,
                            overview: item
                                .get("overview")
                                .and_then(|o| o.as_str())
                                .unwrap_or("")
                                .to_string(),
                            year: Self::year_of(item, "release_date")
                                .or_else(|| Self::year_of(item, "first_air_date")),
                            poster: Self::image(item.get("poster_path").and_then(|p| p.as_str())),
                            media_type_hint: Some(kind.to_string()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait]
impl MetadataProvider for TmdbProvider {
    fn source(&self) -> String {
        "tmdb".to_string()
    }

    async fn search(&self, query: &str) -> ApiResult<Vec<ExternalSearchResult>> {
        // Through the permanent cache: the same search asked twice should
        // never reach TMDB twice.
        let url = format!(
            "https://api.themoviedb.org/3/search/multi?api_key={}&query={}&include_adult=false",
            self.api_key,
            crate::plugins::native::engines::quote_plus(query)
        );
        let body = crate::catalog::tmdb_cache::fetch(&self.client, &url)
            .await
            .ok_or_else(|| ApiError::Unavailable("tmdb search failed".into()))?;
        let mut results: Vec<ExternalSearchResult> = Self::map_search("movie", &body)
            .into_iter()
            .chain(Self::map_search("tv", &body))
            .collect();
        results.dedup_by(|a, b| a.external_id == b.external_id);
        Ok(results)
    }

    async fn details(&self, external_id: &str) -> ApiResult<ExternalDetail> {
        let (kind, id) = external_id.split_once(':').ok_or_else(|| {
            ApiError::bad_request("external_id must be 'movie:<id>' or 'tv:<id>'")
        })?;
        if kind != "movie" && kind != "tv" {
            return Err(ApiError::bad_request("unknown tmdb media kind"));
        }

        let url = format!(
            "https://api.themoviedb.org/3/{kind}/{id}?api_key={}&append_to_response=credits,external_ids",
            self.api_key
        );
        let body = crate::catalog::tmdb_cache::fetch(&self.client, &url)
            .await
            .ok_or_else(|| ApiError::Unavailable("tmdb details failed".into()))?;

        let runtime = match kind {
            "movie" => body
                .get("runtime")
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
            _ => body
                .get("episode_run_time")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_i64())
                .map(|v| v as i32),
        };

        let genres = body
            .get("genres")
            .and_then(|g| g.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|g| g.get("name").and_then(|n| n.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let studios = body
            .get("production_companies")
            .and_then(|g| g.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|g| g.get("name").and_then(|n| n.as_str()))
                    .take(5)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let cast = body
            .get("credits")
            .and_then(|c| c.get("cast"))
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
                    .take(20)
                    .collect()
            })
            .unwrap_or_default();

        Ok(ExternalDetail {
            title: body
                .get("title")
                .or_else(|| body.get("name"))
                .and_then(|t| t.as_str())
                .unwrap_or("Untitled")
                .to_string(),
            original_title: body
                .get("original_title")
                .or_else(|| body.get("original_name"))
                .and_then(|t| t.as_str())
                .map(str::to_string),
            overview: body
                .get("overview")
                .and_then(|o| o.as_str())
                .unwrap_or("")
                .to_string(),
            release_date: Self::date_of(&body, "release_date")
                .or_else(|| Self::date_of(&body, "first_air_date")),
            runtime_minutes: runtime,
            status: body
                .get("status")
                .and_then(|s| s.as_str())
                .map(str::to_string),
            poster: Self::image(body.get("poster_path").and_then(|p| p.as_str())),
            backdrop: Self::image(body.get("backdrop_path").and_then(|p| p.as_str())),
            is_anime: false,
            media_type: Some(kind.to_string()),
            genres,
            studios,
            cast,
            // Movies carry imdb_id at the top level; TV only exposes it under
            // external_ids. Capturing it here is what lets the IMDb sync
            // enrich a title that was imported from TMDB.
            imdb_id: body
                .get("imdb_id")
                .and_then(|v| v.as_str())
                .or_else(|| body.pointer("/external_ids/imdb_id").and_then(|v| v.as_str()))
                .filter(|v| v.starts_with("tt"))
                .map(str::to_string),
            // Ratings are IMDb's, not TMDB's — left for the IMDb sync to fill.
            imdb_rating: None,
            imdb_votes: None,
            metascore: None,
            content_rating: None,
        })
    }
}
