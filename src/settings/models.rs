use serde::{Deserialize, Serialize};
use std::collections::HashMap;

fn default_theme() -> String {
    "dark".into()
}

fn default_language() -> String {
    "en".into()
}

fn default_quality() -> String {
    "1080p".into()
}

fn true_value() -> bool {
    true
}

/// Typed view over the per-user settings blob. Unknown keys sent by clients
/// are preserved through `extra` so plugins/frontends can extend freely.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UserSettings {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_quality")]
    pub max_streaming_quality: String,
    #[serde(default = "true_value")]
    pub autoplay_next_episode: bool,
    #[serde(default = "true_value")]
    pub play_sound_on_notification: bool,
    #[serde(default = "true_value")]
    pub show_continue_watching: bool,
    #[serde(default = "true_value")]
    pub public_watch_activity: bool,
    #[serde(default = "default_language")]
    pub preferred_audio_language: String,
    #[serde(default)]
    pub preferred_subtitle_language: Option<String>,
    #[serde(default)]
    pub reduced_motion: bool,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl Default for UserSettings {
    fn default() -> Self {
        serde_json::from_value(serde_json::json!({})).expect("default settings")
    }
}

const QUALITY_TIERS: &[&str] = &["480p", "720p", "1080p", "4k"];
const THEMES: &[&str] = &["dark", "light", "system"];

pub fn validate(settings: &UserSettings) -> Result<(), crate::error::ApiError> {
    use crate::error::ApiError;

    if !THEMES.contains(&settings.theme.as_str()) {
        return Err(ApiError::bad_request(format!(
            "theme must be one of: {}",
            THEMES.join(", ")
        )));
    }
    if !QUALITY_TIERS.contains(&settings.max_streaming_quality.as_str()) {
        return Err(ApiError::bad_request(format!(
            "max_streaming_quality must be one of: {}",
            QUALITY_TIERS.join(", ")
        )));
    }
    if settings.language.len() > 10 || settings.preferred_audio_language.len() > 10 {
        return Err(ApiError::bad_request(
            "language codes must be <= 10 characters",
        ));
    }
    Ok(())
}
