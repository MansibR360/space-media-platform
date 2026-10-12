//! Script plugins: JavaScript providers, loaded from disk.
//!
//! Drop a folder into `plugins/js/` with a `manifest.json` and a provider
//! `.js` file and it becomes a source. The contract is Nuvio's, so providers
//! written for that ecosystem run here unchanged:
//!
//! ```text
//! plugins/js/
//!   my-provider/
//!     manifest.json   { id, name, version, supportedTypes, filename, enabled }
//!     provider.js     async function getStreams(tmdbId, mediaType, season, episode)
//! ```
//!
//! These are resolve-only sources: a provider is handed a TMDB id and returns
//! stream urls, so they never appear in search but supply the thing search
//! results actually need — a stream that plays in our own player instead of a
//! link to somebody's website.

pub mod routes;
pub mod runtime;

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};

/// Where plugins live, relative to the working directory.
const PLUGIN_DIR: &str = "plugins/js";

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `movie`, `tv` — what this provider can answer for.
    #[serde(default)]
    pub supported_types: Vec<String>,
    /// The provider file, relative to the plugin folder.
    #[serde(default = "default_filename")]
    pub filename: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_filename() -> String {
    "provider.js".into()
}

fn default_enabled() -> bool {
    true
}

/// Accepts both the camelCase a Nuvio manifest uses and snake_case.
fn parse_manifest(raw: &str) -> Result<Manifest, String> {
    let mut value: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    if let Some(object) = value.as_object_mut()
        && let Some(types) = object.remove("supportedTypes")
    {
        object.insert("supported_types".into(), types);
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

#[derive(Debug, Clone)]
pub struct ScriptPlugin {
    pub manifest: Manifest,
    pub source: String,
    pub dir: PathBuf,
}

impl ScriptPlugin {
    pub fn name(&self) -> &str {
        self.manifest.name.as_deref().unwrap_or(&self.manifest.id)
    }

    /// Does this provider handle that media type?
    pub fn handles(&self, media_type: &str) -> bool {
        self.manifest.supported_types.is_empty()
            || self
                .manifest
                .supported_types
                .iter()
                .any(|t| t.eq_ignore_ascii_case(media_type))
    }

    /// Run the provider. The engine is synchronous, so it runs on a blocking
    /// thread and never occupies a request worker.
    pub async fn streams(
        &self,
        client: reqwest::Client,
        tmdb_id: &str,
        media_type: &str,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> ApiResult<Vec<runtime::ScriptStream>> {
        let plugin = self.manifest.id.clone();
        let source = self.source.clone();
        let tmdb_id = tmdb_id.to_string();
        let media_type = media_type.to_string();
        let handle = tokio::runtime::Handle::current();

        tokio::task::spawn_blocking(move || {
            runtime::run_provider(
                &plugin,
                &source,
                &tmdb_id,
                &media_type,
                season,
                episode,
                handle,
                client,
            )
        })
        .await
        .map_err(|e| ApiError::Unavailable(format!("plugin task failed: {e}")))?
    }
}

/// Every plugin found on disk.
#[derive(Debug, Default)]
pub struct ScriptRegistry {
    plugins: Vec<ScriptPlugin>,
}

impl ScriptRegistry {
    /// Read `plugins/js/*/manifest.json`.
    ///
    /// A plugin that fails to load is skipped with a warning: one bad folder
    /// must not stop the server or hide the others.
    pub fn load() -> Self {
        Self::load_from(Path::new(PLUGIN_DIR))
    }

    pub fn load_from(root: &Path) -> Self {
        let mut plugins = Vec::new();

        let Ok(entries) = std::fs::read_dir(root) else {
            tracing::debug!(dir = %root.display(), "no script plugin directory");
            return Self { plugins };
        };

        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let manifest_path = dir.join("manifest.json");
            let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            let manifest = match parse_manifest(&raw) {
                Ok(manifest) => manifest,
                Err(err) => {
                    tracing::warn!(path = %manifest_path.display(), %err, "bad plugin manifest");
                    continue;
                }
            };
            if !manifest.enabled {
                continue;
            }
            if !crate::plugins::models::valid_plugin_name(&manifest.id) {
                tracing::warn!(id = %manifest.id, "plugin id rejected");
                continue;
            }
            let script_path = dir.join(&manifest.filename);
            let source = match std::fs::read_to_string(&script_path) {
                Ok(source) => source,
                Err(err) => {
                    tracing::warn!(path = %script_path.display(), %err, "plugin script unreadable");
                    continue;
                }
            };
            plugins.push(ScriptPlugin {
                manifest,
                source,
                dir,
            });
        }

        if !plugins.is_empty() {
            tracing::info!(count = plugins.len(), "script plugins loaded");
        }
        Self { plugins }
    }

    pub fn all(&self) -> &[ScriptPlugin] {
        &self.plugins
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&ScriptPlugin> {
        self.plugins.iter().find(|p| p.manifest.id == id)
    }

    /// What the admin console sees.
    pub fn describe(&self) -> Value {
        json!({
            "count": self.plugins.len(),
            "directory": PLUGIN_DIR,
            "plugins": self.plugins.iter().map(|p| json!({
                "id": p.manifest.id,
                "name": p.name(),
                "version": p.manifest.version,
                "author": p.manifest.author,
                "description": p.manifest.description,
                "supported_types": p.manifest.supported_types,
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_nuvio_style_manifest() {
        // camelCase `supportedTypes`, as Nuvio writes it
        let manifest = parse_manifest(
            r#"{
                "id": "demo",
                "name": "Demo Provider",
                "version": "1.0.0",
                "supportedTypes": ["movie", "tv"],
                "filename": "provider.js",
                "enabled": true
            }"#,
        )
        .expect("manifest parses");
        assert_eq!(manifest.id, "demo");
        assert_eq!(manifest.supported_types, vec!["movie", "tv"]);
        assert!(manifest.enabled);
    }

    #[test]
    fn fills_in_what_a_minimal_manifest_leaves_out() {
        let manifest = parse_manifest(r#"{ "id": "bare" }"#).unwrap();
        assert_eq!(manifest.filename, "provider.js");
        assert!(manifest.enabled, "plugins are enabled unless they say otherwise");
        assert!(manifest.supported_types.is_empty());
    }

    #[test]
    fn a_provider_with_no_declared_types_answers_for_anything() {
        let plugin = ScriptPlugin {
            manifest: parse_manifest(r#"{ "id": "any" }"#).unwrap(),
            source: String::new(),
            dir: PathBuf::new(),
        };
        assert!(plugin.handles("movie"));
        assert!(plugin.handles("tv"));
    }

    #[test]
    fn a_provider_only_answers_for_what_it_declares() {
        let plugin = ScriptPlugin {
            manifest: parse_manifest(r#"{ "id": "films", "supportedTypes": ["movie"] }"#).unwrap(),
            source: String::new(),
            dir: PathBuf::new(),
        };
        assert!(plugin.handles("movie"));
        assert!(!plugin.handles("tv"));
    }

    #[test]
    fn a_missing_plugin_directory_is_not_an_error() {
        let registry = ScriptRegistry::load_from(Path::new("does/not/exist"));
        assert!(registry.is_empty());
    }
}
