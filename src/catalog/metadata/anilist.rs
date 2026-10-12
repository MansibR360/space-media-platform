use super::{ExternalDetail, ExternalSearchResult, MetadataProvider};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};

const ANILIST_GRAPHQL: &str = "https://graphql.anilist.co";

pub struct AniListProvider {
    client: Client,
}

impl AniListProvider {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    async fn query(&self, query: &str, variables: Value) -> ApiResult<Value> {
        let response = self
            .client
            .post(ANILIST_GRAPHQL)
            .json(&json!({ "query": query, "variables": variables }))
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(ApiError::Unavailable(format!(
                "anilist request failed: {}",
                response.status()
            )));
        }
        let body: Value = response.json().await?;
        if let Some(errors) = body.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                return Err(ApiError::Unavailable(format!(
                    "anilist error: {}",
                    errors[0]
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("?")
                )));
            }
        }
        Ok(body)
    }
}

const SEARCH_QUERY: &str = r#"
query ($search: String) {
  Page(perPage: 20) {
    media(search: $search, type: ANIME, sort: SEARCH_MATCH) {
      id
      title { romaji english native }
      description(asHtml: false)
      seasonYear
      format
      coverImage { large }
    }
  }
}"#;

const DETAILS_QUERY: &str = r#"
query ($id: Int) {
  Media(id: $id, type: ANIME) {
    id
    title { romaji english native }
    description(asHtml: false)
    startDate { year month day }
    episodes
    duration
    status
    format
    coverImage { extraLarge }
    bannerImage
    genres
    studios(isMain: true) { nodes { name } }
    characters(sort: ROLE, perPage: 15) {
      edges { role node { name { full } } voiceActors(language: JAPANESE) { name { full } } }
    }
  }
}"#;

fn pick_title(title: &Value) -> String {
    ["english", "romaji", "native"]
        .iter()
        .find_map(|key| {
            title
                .get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("Untitled")
        .to_string()
}

fn anilist_status(raw: Option<&str>) -> Option<String> {
    raw.map(|s| match s {
        "RELEASING" => "ongoing".to_string(),
        "FINISHED" => "finished".to_string(),
        "NOT_YET_RELEASED" => "announced".to_string(),
        "CANCELLED" => "cancelled".to_string(),
        _ => "released".to_string(),
    })
}

fn anilist_format(raw: Option<&str>) -> Option<&'static str> {
    raw.map(|f| match f {
        "MOVIE" => "movie",
        _ => "series",
    })
}

#[async_trait]
impl MetadataProvider for AniListProvider {
    fn source(&self) -> String {
        "anilist".to_string()
    }

    async fn search(&self, query: &str) -> ApiResult<Vec<ExternalSearchResult>> {
        let body = self.query(SEARCH_QUERY, json!({ "search": query })).await?;
        let media_list = body
            .pointer("/data/Page/media")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(media_list
            .iter()
            .filter_map(|media| {
                let id = media.get("id")?.as_i64()?;
                Some(ExternalSearchResult {
                    source: "anilist".to_string(),
                    external_id: id.to_string(),
                    title: pick_title(media.get("title")?),
                    overview: plain_text(media.get("description")),
                    year: media
                        .get("seasonYear")
                        .and_then(|y| y.as_i64())
                        .map(|y| y as i32),
                    poster: media
                        .pointer("/coverImage/large")
                        .and_then(|p| p.as_str())
                        .map(str::to_string),
                    media_type_hint: anilist_format(media.get("format").and_then(|f| f.as_str()))
                        .map(str::to_string),
                })
            })
            .collect())
    }

    async fn details(&self, external_id: &str) -> ApiResult<ExternalDetail> {
        let id: i64 = external_id
            .parse()
            .map_err(|_| ApiError::bad_request("anilist ids are numeric"))?;

        let body = self.query(DETAILS_QUERY, json!({ "id": id })).await?;
        let media = body
            .pointer("/data/Media")
            .cloned()
            .ok_or_else(|| ApiError::not_found("anilist media"))?;

        let start_date = media.get("startDate");
        let release_date = start_date.and_then(|d| {
            let year = d.get("year")?.as_i64()? as i32;
            let month = d.get("month").and_then(|m| m.as_i64()).unwrap_or(1) as u32;
            let day = d.get("day").and_then(|day| day.as_i64()).unwrap_or(1) as u32;
            chrono::NaiveDate::from_ymd_opt(year, month, day)
        });

        let genres = media
            .get("genres")
            .and_then(|g| g.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|g| g.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let studios = media
            .pointer("/studios/nodes")
            .and_then(|n| n.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let mut cast = Vec::new();
        if let Some(edges) = media
            .pointer("/characters/edges")
            .and_then(|e| e.as_array())
        {
            for (i, edge) in edges.iter().enumerate() {
                let Some(character) = edge.get("node") else {
                    continue;
                };
                let Some(name) = character.pointer("/name/full").and_then(|n| n.as_str()) else {
                    continue;
                };
                let role = edge.get("role").and_then(|r| r.as_str()).unwrap_or("");
                let voice = edge
                    .pointer("/voiceActors/0/name/full")
                    .and_then(|v| v.as_str());
                let display = match voice {
                    Some(va) if !role.is_empty() => format!("{name} ({role}) / VA: {va}"),
                    _ => name.to_string(),
                };
                cast.push((display, None, i as i32));
            }
        }

        Ok(ExternalDetail {
            title: pick_title(media.get("title").unwrap_or(&serde_json::Value::Null)),
            original_title: media
                .pointer("/title/native")
                .and_then(|t| t.as_str())
                .map(str::to_string),
            overview: plain_text(media.get("description")),
            release_date,
            runtime_minutes: media
                .get("duration")
                .and_then(|d| d.as_i64())
                .map(|d| d as i32),
            status: anilist_status(media.get("status").and_then(|s| s.as_str())),
            poster: media
                .pointer("/coverImage/extraLarge")
                .and_then(|p| p.as_str())
                .map(str::to_string),
            backdrop: media
                .get("bannerImage")
                .and_then(|b| b.as_str())
                .map(str::to_string),
            is_anime: true,
            media_type: anilist_format(media.get("format").and_then(|f| f.as_str()))
                .map(str::to_string),
            genres,
            studios,
            cast,
            // AniList indexes anime, which IMDb ids do not cover reliably.
            imdb_id: None,
            imdb_rating: None,
            imdb_votes: None,
            metascore: None,
            content_rating: None,
        })
    }
}

fn plain_text(value: Option<&Value>) -> String {
    value
        .and_then(|v| v.as_str())
        .map(|s| {
            s.replace("<br>", "\n")
                .replace("<br/>", "\n")
                .replace("<i>", "")
                .replace("</i>", "")
                .replace("<b>", "")
                .replace("</b>", "")
        })
        .unwrap_or_default()
}
