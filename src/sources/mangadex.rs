//! MangaDex — officially licensed manga platform with a public API.
//! Search → chapter feed → at-home page images.

use super::{MangaChapter, NativeSource, SourceDetail, SourceItem, StreamLink};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Default)]
pub struct MangaDex;

const API: &str = "https://api.mangadex.org";

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("space-media-server/0.1")
        .build()
        .expect("reqwest client")
}

async fn get_json(url: &str, query: &[(&str, &str)]) -> ApiResult<Value> {
    let response = client()
        .get(url)
        .header("Accept", "application/json")
        .query(query)
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("mangadex unreachable: {e}")))?;
    if !response.status().is_success() {
        return Err(ApiError::Unavailable(format!(
            "mangadex returned {}",
            response.status()
        )));
    }
    response.json().await.map_err(ApiError::from)
}

fn title_of(manga: &Value) -> String {
    let attrs = manga.get("attributes");
    ["en", "ja-ro", "ja"]
        .iter()
        .find_map(|lang| {
            attrs?
                .get("title")?
                .get(lang)
                .and_then(|t| t.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        // Preferred languages first, then *any* language rather than giving
        // up: a series titled only in ko/zh/es is not untitled, and showing
        // "Untitled" for it is worse than showing the original script.
        .or_else(|| {
            attrs?
                .get("title")?
                .as_object()?
                .values()
                .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
                .map(str::to_string)
        })
        .or_else(|| {
            attrs
                .and_then(|a| a.get("altTitles"))
                .and_then(|a| a.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|t| {
                        t.as_object()?
                            .values()
                            .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
                    })
                })
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Untitled".into())
}

fn cover_of(manga: &Value) -> Option<String> {
    let id = manga.get("id")?.as_str()?;
    let file = manga
        .get("relationships")?
        .as_array()?
        .iter()
        .find(|r| r.get("type").and_then(|t| t.as_str()) == Some("cover_art"))
        .and_then(|r| r.pointer("/attributes/fileName"))
        .and_then(|f| f.as_str())?;
    Some(format!(
        "https://uploads.mangadex.org/covers/{id}/{file}.256.jpg"
    ))
}

#[async_trait]
impl NativeSource for MangaDex {
    fn name(&self) -> &'static str {
        "mangadex"
    }

    fn label(&self) -> &'static str {
        "MangaDex · Licensed Manga"
    }

    fn category(&self) -> &'static str {
        "manga"
    }

    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let mut params: Vec<(&str, String)> = vec![
            ("limit", limit.clamp(1, 32).to_string()),
            ("contentRating[]", "safe".into()),
            ("contentRating[]", "suggestive".into()),
            ("includes[]", "cover_art".into()),
        ];

        if query.trim().is_empty() {
            params.push(("order[followedCount]", "desc".into()));
        } else {
            params.push(("title", query.trim().to_string()));
        }

        let borrowed: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let body = get_json(&format!("{API}/manga"), &borrowed).await?;

        let data = body
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(data
            .iter()
            .filter_map(|manga| {
                Some(SourceItem {
                    external_id: manga.get("id")?.as_str()?.to_string(),
                    title: title_of(manga),
                    overview: manga
                        .pointer("/attributes/description/en")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(600)
                        .collect(),
                    year: manga
                        .pointer("/attributes/year")
                        .and_then(|y| y.as_i64())
                        .map(|y| y as i32),
                    poster: cover_of(manga),
                    category: Self::CATEGORY,
                })
            })
            .collect())
    }

    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail> {
        let body = get_json(
            &format!("{API}/manga/{}", external_id.trim()),
            &[("includes[]", "cover_art")],
        )
        .await?;

        let manga = body.get("data").cloned().unwrap_or(Value::Null);
        let chapters = self.chapters_for(external_id).await.unwrap_or_default();

        // First chapter becomes the playable "stream" for uniform player wiring.
        let streams = chapters
            .first()
            .map(|first| {
                vec![StreamLink {
                    label: format!("Chapter {}", first.chapter),
                    url: format!("mangadex-chapter:{}", first.id),
                    kind: "page".into(),
                }]
            })
            .unwrap_or_default();

        Ok(SourceDetail {
            item: SourceItem {
                external_id: external_id.trim().to_string(),
                title: title_of(&manga),
                overview: manga
                    .pointer("/attributes/description/en")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(1200)
                    .collect(),
                year: manga
                    .pointer("/attributes/year")
                    .and_then(|y| y.as_i64())
                    .map(|y| y as i32),
                poster: cover_of(&manga),
                category: Self::CATEGORY,
            },
            streams,
            chapters: Some(chapters),
        })
    }
}

impl MangaDex {
    pub const CATEGORY: &str = "manga";
    pub async fn chapters_for(&self, manga_id: &str) -> ApiResult<Vec<MangaChapter>> {
        let body = get_json(
            &format!("{API}/manga/{}/feed", manga_id.trim()),
            &[
                ("translatedLanguage[]", "en"),
                ("order[chapter]", "asc"),
                ("limit", "500"),
                ("contentRating[]", "safe"),
                ("contentRating[]", "suggestive"),
                ("contentRating[]", "erotica"),
            ],
        )
        .await?;

        let data = body
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(data
            .iter()
            .filter_map(|ch| {
                let attrs = ch.get("attributes")?;
                Some(MangaChapter {
                    id: ch.get("id")?.as_str()?.to_string(),
                    chapter: attrs
                        .get("chapter")
                        .and_then(|c| c.as_str())
                        .unwrap_or("0")
                        .to_string(),
                    title: attrs
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    pages: attrs.get("pages").and_then(|p| p.as_i64()).unwrap_or(0),
                })
            })
            .collect())
    }

    /// Resolves a chapter into full-resolution page image URLs.
    pub async fn chapter_pages(&self, chapter_id: &str) -> ApiResult<Vec<String>> {
        let body = get_json(&format!("{API}/at-home/server/{}", chapter_id.trim()), &[]).await?;
        let base_url = body
            .get("baseUrl")
            .and_then(|b| b.as_str())
            .ok_or_else(|| ApiError::Unavailable("at-home returned no baseUrl".into()))?;
        let hash = body
            .pointer("/chapter/hash")
            .and_then(|h| h.as_str())
            .unwrap_or("");
        let files = body
            .pointer("/chapter/data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(files
            .iter()
            .filter_map(|f| f.as_str())
            .map(|f| format!("{base_url}/data/{hash}/{f}"))
            .collect())
    }
}
