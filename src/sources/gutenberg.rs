//! Project Gutenberg — 75,000+ public-domain ebooks via the Gutendex API.

use super::{NativeSource, SourceDetail, SourceItem, StreamLink};
use crate::error::{ApiError, ApiResult};
use async_trait::async_trait;
use serde_json::Value;

#[derive(Default)]
pub struct Gutenberg;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("space-media-server/0.1")
        .build()
        .expect("reqwest client")
}

fn pick_cover(formats: &Value) -> Option<String> {
    ["image/jpeg"]
        .iter()
        .find_map(|k| formats.get(*k).and_then(|v| v.as_str()).map(str::to_string))
}

fn pick_readables(formats: &Value) -> Vec<StreamLink> {
    let mut links = Vec::new();
    let entries = [
        ("text/html", "Read online (HTML)", "file"),
        ("application/epub+zip", "EPUB download", "file"),
        ("application/x-mobipocket-ebook", "Kindle (mobi)", "file"),
        ("text/plain; charset=us-ascii", "Plain text", "file"),
        ("text/plain", "Plain text", "file"),
    ];
    for (key, label, kind) in entries {
        if let Some(url) = formats.get(key).and_then(|v| v.as_str()) {
            links.push(StreamLink {
                label: label.into(),
                url: url.to_string(),
                kind: kind.into(),
            });
        }
    }
    links
}

#[async_trait]
impl NativeSource for Gutenberg {
    fn name(&self) -> &'static str {
        "gutenberg"
    }

    fn label(&self) -> &'static str {
        "Project Gutenberg · Public-Domain Books"
    }

    fn category(&self) -> &'static str {
        "book"
    }

    async fn search(&self, query: &str, limit: i32) -> ApiResult<Vec<SourceItem>> {
        let mut request = client()
            .get("https://gutendex.com/books")
            .query(&[("page", "1")]);

        if query.trim().is_empty() {
            // Gutendex default ordering is by popularity (downloads).
            request = request.query(&[("sort", "popular")]);
        } else {
            request = request.query(&[("search", query.trim())]);
        }

        let body: Value = request
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("gutendex unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let results = body
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(results
            .into_iter()
            .take(limit.clamp(1, 50) as usize)
            .filter_map(|book| {
                let id = book.get("id")?.as_i64()?.to_string();
                let authors: Vec<String> = book
                    .get("authors")
                    .and_then(|a| a.as_array())
                    .map(|list| {
                        list.iter()
                            .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let formats = book.get("formats").cloned().unwrap_or_else(|| Value::Null);

                Some(SourceItem {
                    external_id: id,
                    title: book.get("title")?.as_str()?.to_string(),
                    overview: format!("By {}", authors.join(", ")),
                    year: book.get("copyright").and_then(|_| None).or(book
                        .pointer("/authors/0/birth_year")
                        .and_then(|y| y.as_i64())
                        .map(|y| y as i32)),
                    poster: pick_cover(&formats),
                    category: Self::category_of(),
                })
            })
            .collect())
    }

    async fn item(&self, external_id: &str) -> ApiResult<SourceDetail> {
        let id_num: i64 = external_id
            .trim()
            .parse()
            .map_err(|_| ApiError::bad_request("gutenberg ids are numeric"))?;

        let book: Value = client()
            .get(format!("https://gutendex.com/books/{id_num}"))
            .send()
            .await
            .map_err(|e| ApiError::Unavailable(format!("gutendex unreachable: {e}")))?
            .json()
            .await
            .map_err(ApiError::from)?;

        let formats = book.get("formats").cloned().unwrap_or_else(|| Value::Null);
        let authors: Vec<String> = book
            .get("authors")
            .and_then(|a| a.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        Ok(SourceDetail {
            item: SourceItem {
                external_id: id_num.to_string(),
                title: book
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("Untitled")
                    .to_string(),
                overview: format!("By {}", authors.join(", ")),
                year: None,
                poster: pick_cover(&formats),
                category: Self::category_of(),
            },
            streams: pick_readables(&formats),
            chapters: None,
        })
    }
}

impl Gutenberg {
    fn category_of() -> &'static str {
        "book"
    }
}
