//! Stremio-compatible **extension system** — 1:1 addon-SDK support, kept
//! deliberately separate from the plugin/web-source/hub stacks.
//!
//! An extension is any remote addon speaking the Stremio Addon SDK protocol:
//!
//! ```text
//! GET {base}/manifest.json
//! GET {base}/catalog/{type}/{catalogId}/{encoded extras}.json
//! GET {base}/stream/{type}/{id}.json        id: tt… | tmdb:… | mal:… | kitsu:…
//! GET {base}/meta/{type}/{id}.json
//! GET {base}/subtitles/{type}/{id}.json?extra=…
//! ```
//!
//! SPACE installs addons by manifest url (exactly like Stremio: configure the
//! addon on its website, paste the resulting manifest url here). Installed
//! extensions join unified search through their searchable catalogs and
//! playback resolution through their /stream endpoints, with automatic
//! stream-kind mapping (url → mp4/hls/page, infoHash → torrent magnet).

pub mod handlers;
pub mod routes;

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

// ------------------------------------------------------------------- model

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct ExtensionRow {
    pub id: Uuid,
    pub manifest_url: String,
    pub manifest: Value,
    pub addon_id: String,
    pub name: String,
    pub enabled: bool,
    #[serde(skip_serializing)]
    pub created_by: Option<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub mod repo {
    use super::ExtensionRow;
    use crate::error::{ApiError, ApiResult};
    use sqlx::PgPool;
    use uuid::Uuid;

    pub async fn list(pool: &PgPool) -> ApiResult<Vec<ExtensionRow>> {
        Ok(sqlx::query_as::<_, ExtensionRow>(
            "SELECT * FROM extensions ORDER BY created_at",
        )
        .fetch_all(pool)
        .await?)
    }

    pub async fn get(pool: &PgPool, id: Uuid) -> ApiResult<ExtensionRow> {
        sqlx::query_as::<_, ExtensionRow>("SELECT * FROM extensions WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| ApiError::not_found("extension"))
    }

    pub async fn insert(pool: &PgPool, row: &ExtensionRow) -> ApiResult<ExtensionRow> {
        sqlx::query_as::<_, ExtensionRow>(
            "INSERT INTO extensions (id, manifest_url, manifest, addon_id, name, enabled)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (manifest_url) DO UPDATE SET
                manifest = EXCLUDED.manifest,
                name = EXCLUDED.name,
                enabled = TRUE,
                updated_at = now()
             RETURNING *",
        )
        .bind(row.id)
        .bind(&row.manifest_url)
        .bind(&row.manifest)
        .bind(&row.addon_id)
        .bind(&row.name)
        .bind(row.enabled)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
    }

    pub async fn set_enabled(pool: &PgPool, id: Uuid, enabled: bool) -> ApiResult<ExtensionRow> {
        sqlx::query_as::<_, ExtensionRow>(
            "UPDATE extensions SET enabled = $2, updated_at = now() WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(enabled)
        .fetch_one(pool)
        .await
        .map_err(Into::into)
    }

    pub async fn delete(pool: &PgPool, id: Uuid) -> ApiResult<()> {
        let result = sqlx::query("DELETE FROM extensions WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(ApiError::not_found("extension"));
        }
        Ok(())
    }
}

// ------------------------------------------------------ protocol helpers

/// Base url for resource calls = origin of the manifest url.
pub fn base_from_manifest_url(manifest_url: &str) -> Option<String> {
    let parsed = url::Url::parse(manifest_url).ok()?;
    let origin = parsed.origin().ascii_serialization();
    Some(origin)
}

fn encode_extra(extra: &[(&str, &str)]) -> String {
    // Extras are a path segment shaped `k=v&k2=v2`. Only the *values* are
    // percent-encoded — the `=` and `&` are structural, and encoding them
    // (as this did) turns `search=spider man` into `search%3Dspider%20man`,
    // which an addon does not recognise as a search extra. Stremio then
    // silently serves the unfiltered catalog instead of the search result.
    extra
        .iter()
        .map(|(k, v)| format!("{k}={}", utf8_percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn utf8_percent_encode(raw: &str) -> String {
    // keep unreserved chars; everything else percent-encoded (RFC 3986)
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub struct ResourceTarget {
    pub base: String,
}

impl ResourceTarget {
    pub fn catalog_url(&self, stype: &str, catalog_id: &str, extra: &[(&str, &str)]) -> String {
        let extra_seg = if extra.is_empty() {
            String::new()
        } else {
            format!("/{}", encode_extra(extra))
        };
        format!(
            "{}/catalog/{stype}/{catalog_id}{extra_seg}.json",
            self.base.trim_end_matches('/')
        )
    }

    pub fn stream_url(&self, stype: &str, id: &str) -> String {
        format!(
            "{}/stream/{stype}/{}.json",
            self.base.trim_end_matches('/'),
            utf8_percent_encode(id)
        )
    }

    pub fn meta_url(&self, stype: &str, id: &str) -> String {
        format!(
            "{}/meta/{stype}/{}.json",
            self.base.trim_end_matches('/'),
            utf8_percent_encode(id)
        )
    }

    pub fn subtitles_url(&self, stype: &str, id: &str, extra: &[(&str, &str)]) -> String {
        let extra_q = if extra.is_empty() {
            String::new()
        } else {
            format!("?extra={}", utf8_percent_encode(&encode_extra(extra)))
        };
        format!(
            "{}/subtitles/{stype}/{}.json{extra_q}",
            self.base.trim_end_matches('/'),
            utf8_percent_encode(id)
        )
    }
}

fn http_client(cfg: &crate::config::Config) -> ApiResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(cfg.extension_timeout_secs))
        .user_agent("space-media-server/0.1 (stremio-extension-host)")
        .build()
        .map_err(|e| ApiError::Unavailable(format!("http client: {e}")))
}

pub async fn fetch_manifest(
    cfg: &crate::config::Config,
    manifest_url: &str,
) -> ApiResult<Value> {
    let resp = http_client(cfg)?
        .get(manifest_url)
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("addon unreachable: {e}")))?;
    if !resp.status().is_success() {
        return Err(ApiError::Unavailable(format!(
            "manifest answered {}",
            resp.status()
        )));
    }
    let manifest: Value = resp.json().await.map_err(|e| {
        ApiError::bad_request(format!("manifest is not valid json: {e}"))
    })?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// Minimal 1:1 validation per the Addon SDK spec.
pub fn validate_manifest(manifest: &Value) -> ApiResult<()> {
    for field in ["id", "version", "name", "resources", "types"] {
        if manifest.get(field).is_none() {
            return Err(ApiError::bad_request(format!(
                "manifest missing required field '{field}'"
            )));
        }
    }
    if manifest["id"].as_str().map(str::is_empty).unwrap_or(true) {
        return Err(ApiError::bad_request("manifest id must be non-empty"));
    }
    if !manifest["resources"].is_array() {
        return Err(ApiError::bad_request("manifest resources must be an array"));
    }
    Ok(())
}

/// Flattens `resources` entries ("catalog" | {name, types?, idPrefixes?}) into
/// (name, types, idPrefixes).
pub fn resource_entries(manifest: &Value) -> Vec<(String, Vec<String>, Vec<String>)> {
    let mut out = Vec::new();
    for entry in manifest["resources"].as_array().into_iter().flatten() {
        match entry {
            Value::String(name) => out.push((name.clone(), Vec::new(), Vec::new())),
            obj @ Value::Object(_) => {
                let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
                let types = obj
                    .get("types")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default();
                let prefixes = obj
                    .get("idPrefixes")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default();
                if !name.is_empty() {
                    out.push((name.to_string(), types, prefixes));
                }
            }
            _ => {}
        }
    }
    out
}

pub fn supports_resource(manifest: &Value, resource: &str) -> bool {
    resource_entries(manifest).iter().any(|(n, _, _)| n == resource)
}

/// One catalog definition from the manifest.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogDef {
    pub stype: String,
    pub catalog_id: String,
    pub name: String,
    pub extra_supported: Vec<String>,
}

pub fn catalogs(manifest: &Value) -> Vec<CatalogDef> {
    manifest["catalogs"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let stype = c.get("type")?.as_str()?.to_string();
                    let catalog_id = c.get("id")?.as_str()?.to_string();
                    let name = c
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&catalog_id)
                        .to_string();
                    let extra_supported = c
                        .get("extraSupported")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                        .unwrap_or_default();
                    Some(CatalogDef {
                        stype,
                        catalog_id,
                        name,
                        extra_supported,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn searchable_catalogs(manifest: &Value) -> Vec<CatalogDef> {
    catalogs(manifest)
        .into_iter()
        .filter(|c| c.extra_supported.iter().any(|e| e == "search"))
        .collect()
}

fn has_stream_resource(manifest: &Value) -> bool {
    supports_resource(manifest, "stream")
}

/// Maps a stremio type onto SPACE's categories.
pub fn map_category(stype: &str) -> &'static str {
    match stype {
        "movie" => "movie",
        "anime" => "manga",
        "channel" => "series",
        "series" | _ => "series",
    }
}

// --------------------------------------------------------- stream mapping

#[derive(Debug, Clone, Deserialize)]
struct AddonStream {
    #[serde(default)]
    title: Option<String>,
    url: Option<String>,
    #[serde(default)]
    yt_id: Option<String>,
    #[serde(rename = "ytId")]
    yt_id_camel: Option<String>,
    info_hash: Option<String>,
    #[serde(rename = "infoHash")]
    info_hash_camel: Option<String>,
    #[serde(default)]
    behavior_hints: Value,
}

impl AddonStream {
    fn yt(&self) -> Option<&str> {
        self.yt_id.as_deref().or(self.yt_id_camel.as_deref())
    }
    fn hash(&self) -> Option<&str> {
        self.info_hash.as_deref().or(self.info_hash_camel.as_deref())
    }
}

/// Converts an addon's streams array into SPACE StreamLinks.
pub fn convert_streams(streams: &Value, context_title: &str) -> Vec<crate::sources::StreamLink> {
    let parsed: Vec<AddonStream> = serde_json::from_value(streams.clone()).unwrap_or_default();
    parsed
        .iter()
        .enumerate()
        .filter_map(|(idx, s)| {
            if let Some(url) = s.url.as_deref() {
                let lower = url.split(['?', '#']).next().unwrap_or(url).to_lowercase();
                let kind = if lower.ends_with(".m3u8") || lower.ends_with(".mpd") {
                    "hls"
                } else if [".mp4", ".webm", ".m4v", ".mov", ".ts"]
                    .iter()
                    .any(|e| lower.ends_with(e))
                {
                    "mp4"
                } else if lower.starts_with("magnet:") || lower.ends_with(".torrent") {
                    "torrent"
                } else {
                    "page"
                };
                return Some(crate::sources::StreamLink {
                    label: s.title.clone().unwrap_or_else(|| kind.to_string()),
                    url: url.to_string(),
                    kind: kind.into(),
                });
            }
            if let Some(yt) = s.yt() {
                return Some(crate::sources::StreamLink {
                    label: s.title.clone().unwrap_or_else(|| "YouTube".into()),
                    url: format!("https://www.youtube.com/watch?v={yt}"),
                    kind: "page".into(),
                });
            }
            if let Some(hash) = s.hash() {
                let dn = utf8_percent_encode(context_title);
                return Some(crate::sources::StreamLink {
                    label: s.title.clone().unwrap_or_else(|| format!("torrent #{idx}")),
                    url: format!("magnet:?xt=urn:btih:{hash}&dn={dn}"),
                    kind: "torrent".into(),
                });
            }
            None
        })
        .collect()
}

// ---------------------------------------------------------------- manager

pub struct ExtEntry {
    pub row: ExtensionRow,
    pub target: ResourceTarget,
}

impl ExtEntry {
    pub fn addon_id(&self) -> &str {
        &self.row.addon_id
    }
}

#[derive(Default)]
pub struct ExtensionManager {
    inner: RwLock<Vec<Arc<ExtEntry>>>,
}

impl ExtensionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn reload(&self, pool: &PgPool, cfg: &crate::config::Config) {
        let rows = match repo::list(pool).await {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(error = %err, "extensions reload failed");
                return;
            }
        };
        let entries: Vec<Arc<ExtEntry>> = rows
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| {
                base_from_manifest_url(&r.manifest_url).map(|base| {
                    Arc::new(ExtEntry {
                        row: r.clone(),
                        target: ResourceTarget { base },
                    })
                })
            })
            .collect();
        tracing::debug!(count = entries.len(), "extensions loaded");
        *self.inner.write().await = entries;
        let _ = cfg;
    }

    pub async fn list(&self) -> Vec<Arc<ExtEntry>> {
        self.inner.read().await.clone()
    }

    pub async fn find(&self, ext_id: &str) -> Option<Arc<ExtEntry>> {
        self.inner.read().await.iter().find(|e| e.row.addon_id == ext_id || e.row.id.to_string() == ext_id).cloned()
    }

    /// Resolves aggregator provider names of the form `ext:{addon_id}`.
    pub async fn find_by_prefix(&self, provider_name: &str) -> Option<Arc<ExtEntry>> {
        let id = provider_name.strip_prefix("ext:")?;
        self.find(id).await
    }

    /// Streams across every stream-capable extension for a stremio id.
    pub async fn streams_for(
        &self,
        state: &AppStateAlias,
        stype: &str,
        id: &str,
    ) -> Vec<(String, crate::sources::StreamLink)> {
        let mut out = Vec::new();
        for entry in self.list().await {
            if !supports_resource(&entry.row.manifest, "stream") {
                continue;
            }
            let url = entry.target.stream_url(stype, id);
            let fut = http_get_json_cfg(&state.cfg, &url);
            if let Ok(body) = fut.await
                && let Some(streams) = body.get("streams")
            {
                let title_hint = format!("{stype}:{id}");
                for link in convert_streams(streams, &title_hint) {
                    out.push((format!("ext:{}", entry.row.addon_id), link));
                }
            }
        }
        out
    }
}

type AppStateAlias = crate::state::AppState;

pub async fn http_get_json_cfg(
    cfg: &crate::config::Config,
    url: &str,
) -> ApiResult<Value> {
    let resp = http_client(cfg)?
        .get(url)
        .send()
        .await
        .map_err(|e| ApiError::Unavailable(format!("extension call failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(ApiError::Unavailable(format!(
            "extension answered {}",
            resp.status()
        )));
    }
    resp.json()
        .await
        .map_err(|e| ApiError::Unavailable(format!("bad extension json: {e}")))
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn load_live_cfg() -> crate::config::Config {
        // live tests never touch the db; satisfy the one required var
        // (set_var is unsafe in edition 2024 — single-threaded live runs only)
        #[allow(unsafe_op_in_unsafe_fn)]
        unsafe {
            std::env::set_var("DATABASE_URL", "postgres://live-test@localhost/live");
        }
        crate::config::Config {
            extension_timeout_secs: 25,
            ..crate::config::Config::load().expect("cfg")
        }
    }

    const MANIFEST: &str = r#"{
        "id": "org.example",
        "version": "1.0.0",
        "name": "Example",
        "types": ["movie","series"],
        "resources": [
            "catalog",
            {"name":"stream","types":["movie","series"],"idPrefixes":["tt"]}
        ],
        "catalogs": [
            {"type":"movie","id":"top","name":"Popular","extraSupported":["search","skip"]},
            {"type":"series","id":"latest","name":"Latest"}
        ],
        "idPrefixes": ["tt"]
    }"#;

    #[test]
    fn validates_and_parses_capabilities() {
        let m: Value = serde_json::from_str(MANIFEST).unwrap();
        validate_manifest(&m).unwrap();
        assert!(supports_resource(&m, "stream"));
        assert!(supports_resource(&m, "catalog"));
        assert!(!supports_resource(&m, "subtitles"));
        let searchables = searchable_catalogs(&m);
        assert_eq!(searchables.len(), 1);
        assert_eq!(searchables[0].catalog_id, "top");
        assert_eq!(searchables[0].stype, "movie");
    }

    #[test]
    fn rejects_broken_manifests() {
        assert!(validate_manifest(&json!({})).is_err());
        assert!(validate_manifest(&json!({"id":"x"})).is_err());
    }

    #[test]
    fn builds_spec_urls() {
        let t = ResourceTarget {
            base: "https://addon.strem.io".into(),
        };
        assert_eq!(
            t.stream_url("movie", "tt1254207"),
            "https://addon.strem.io/stream/movie/tt1254207.json"
        );
        // The extras segment is `k=v&k2=v2` with only the *values* encoded.
        // This previously asserted the separators encoded too
        // (`search%3Dbat%20man`), which an addon does not read as a search —
        // it silently served the unfiltered catalog instead of the results.
        assert_eq!(
            t.catalog_url("movie", "top", &[("search", "bat man"), ("skip", "100")]),
            "https://addon.strem.io/catalog/movie/top/search=bat%20man&skip=100.json"
        );
        assert_eq!(
            t.meta_url("series", "tt9364618"),
            "https://addon.strem.io/meta/series/tt9364618.json"
        );
    }

    #[test]
    fn converts_all_stream_shapes() {
        let streams = json!([
            {"title":"Direct","url":"https://cdn/x/master.m3u8"},
            {"url":"https://cdn/file.mp4"},
            {"title":"YTS","infoHash":"DEADBEEF00"},
            {"ytId":"dQw4w9WgXcQ"},
            {"externalUrl":"https://example.com"}
        ]);
        let links = convert_streams(&streams, "Big Buck Bunny");
        let kinds: Vec<&str> = links.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(kinds, vec!["hls", "mp4", "torrent", "page"]);
        assert!(links[2].url.starts_with("magnet:?xt=urn:btih:DEADBEEF00&dn="));
        assert_eq!(links[3].url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(links.len(), 4); // externalUrl-only entries are skipped
    }

    #[test]
    fn base_extraction() {
        assert_eq!(
            base_from_manifest_url("https://torrentio.strem.fun/manifest.json").as_deref(),
            Some("https://torrentio.strem.fun")
        );
    }

    /// Live end-to-end checks against the real Stremio addon network.
    /// Run explicitly:  cargo test extensions::tests::live -- --ignored --test-threads 1
    #[tokio::test]
    #[ignore]
    async fn live_cinemeta_manifest_and_catalog() {
        let cfg = load_live_cfg();
        let url = "https://v3-cinemeta.strem.io/manifest.json";
        let manifest = fetch_manifest(&cfg, url).await.expect("cinemeta manifest");
        assert_eq!(manifest["id"], "com.linvo.cinemeta");
        assert!(supports_resource(&manifest, "meta"));
        assert!(supports_resource(&manifest, "catalog"));
        let target = ResourceTarget {
            base: base_from_manifest_url(url).unwrap(),
        };

        // the exact call shape aggregator::provider_search makes
        let body = http_get_json_cfg(
            &cfg,
            &target.catalog_url("movie", "top", &[("search", "batman"), ("skip", "0")]),
        )
        .await
        .expect("catalog search");
        let metas = body.get("metas").and_then(Value::as_array).expect("metas[]");
        assert!(!metas.is_empty(), "batman search should hit");
        let first = &metas[0];
        let row_id = first["id"].as_str().expect("imdb id");
        assert!(row_id.starts_with("tt"), "got {row_id}");
        assert!(first["name"].as_str().is_some());
        println!(
            "catalog OK: {} results, first = {} ({})",
            metas.len(),
            first["name"],
            row_id
        );

        // meta resource
        let meta = http_get_json_cfg(&cfg, &target.meta_url("movie", row_id))
            .await
            .expect("meta");
        assert_eq!(meta["meta"]["id"], row_id);
        println!("meta OK: {}", meta["meta"]["name"]);
    }

    #[tokio::test]
    #[ignore]
    async fn live_torrentio_stream_resolution() {
        let cfg = load_live_cfg();
        let url = "https://torrentio.strem.fun/manifest.json";
        let manifest = fetch_manifest(&cfg, url).await.expect("torrentio manifest");
        assert_eq!(manifest["id"], "com.stremio.torrentio.addon");
        assert!(supports_resource(&manifest, "stream"));
        let target = ResourceTarget {
            base: base_from_manifest_url(url).unwrap(),
        };

        // The Dark Knight (2008)
        let body = http_get_json_cfg(&cfg, &target.stream_url("movie", "tt0468569"))
            .await
            .expect("streams");
        let raw = body.get("streams").and_then(Value::as_array).expect("streams[]");
        assert!(!raw.is_empty(), "torrentio should return streams");
        let links = convert_streams(body.get("streams").unwrap(), "The Dark Knight");
        assert_eq!(links.len(), raw.len(), "every stream must map");
        let magnets = links.iter().filter(|l| l.kind == "torrent").count();
        assert!(magnets > 0, "expected infoHash→magnet conversion");
        for l in &links {
            if l.kind == "torrent" {
                assert!(l.url.starts_with("magnet:?xt=urn:btih:"), "{}", l.url);
                break;
            }
        }
        println!(
            "stream OK: {} streams, {magnets} magnets; sample labels: {:?}",
            links.len(),
            links.iter().take(3).map(|l| l.label.as_str()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    #[ignore]
    async fn live_series_episode_id_passthrough() {
        // stremio episode ids flow straight through the URL builder
        let cfg = load_live_cfg();
        let target = ResourceTarget {
            base: "https://torrentio.strem.fun".into(),
        };
        let body = http_get_json_cfg(&cfg, &target.stream_url("series", "tt0903747:1:1"))
            .await
            .expect("breaking bad S1E1 streams");
        let raw = body.get("streams").and_then(Value::as_array).expect("streams[]");
        println!("episode OK: {} streams for tt0903747:1:1", raw.len());
        assert!(!raw.is_empty());
    }
}
