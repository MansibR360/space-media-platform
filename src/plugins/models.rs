use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A registered extension. Built-in providers are compiled in; HTTP plugins
/// are external services speaking the JSON protocol documented in README.
///
/// Protocol for HTTP metadata plugins:
///   GET {base}/manifest.json ->
///     { "name": "...", "version": "...", "type": "metadata" }
///   GET {base}/search?q=<query> ->
///     { "results": [ { "id", "title", "overview", "year", "poster", "type" } ] }
///   GET {base}/details?id=<id> ->
///     { "title", "overview", "release_date", "runtime_minutes", "poster",
///       "backdrop", "media_type": "movie"|"tv"|"anime", "genres": [],
///       "studios": [], "cast": [{ "name", "character" }] }
#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct PluginRecord {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub transport: String,
    pub base_url: Option<String>,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PluginView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub transport: String,
    pub base_url: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

impl PluginRecord {
    pub fn view(&self) -> PluginView {
        PluginView {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind.clone(),
            transport: self.transport.clone(),
            base_url: self.base_url.clone(),
            enabled: self.enabled,
            created_at: self.created_at,
        }
    }
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreatePluginRequest {
    /// unique slug-style name, becomes the metadata source key
    pub name: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    pub base_url: String,
    #[serde(default)]
    pub enabled: bool,
    /// free-form plugin configuration (api keys etc.), never returned to clients
    #[serde(default)]
    pub config: serde_json::Value,
}

fn default_kind() -> String {
    "metadata".into()
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdatePluginRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

pub fn valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
