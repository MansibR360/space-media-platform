use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Audio analysis for one track — **global**, not per user. One row per track,
/// shared by everybody: the waveform, the key, the tempo and the descriptors
/// are properties of the recording, so they are computed once and read by all.
///
/// Every field past `media_id` is optional. A row exists as soon as analysis is
/// queued, and stays partially filled when ffmpeg is unavailable or the probe
/// only yields container metadata.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct MusicAnalysis {
    pub media_id: Uuid,
    pub file_id: Option<Uuid>,

    // ---- container ----
    pub duration_seconds: Option<f64>,
    pub sample_rate: Option<i32>,
    pub channels: Option<i32>,
    pub bitrate_kbps: Option<i32>,
    pub codec: Option<String>,

    // ---- embedded tags ----
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_no: Option<i32>,
    pub disc_no: Option<i32>,
    pub release_year: Option<i32>,
    pub tag_genre: Option<String>,
    pub tags: Option<serde_json::Value>,

    // ---- level ----
    pub loudness_lufs: Option<f64>,
    pub peak_db: Option<f64>,
    pub dynamic_range: Option<f64>,
    pub energy: Option<f64>,

    // ---- envelopes: JSON arrays of `waveform_buckets` floats in 0..1 ----
    pub waveform: Option<serde_json::Value>,
    pub rms_envelope: Option<serde_json::Value>,
    pub waveform_buckets: Option<i32>,

    // ---- spectral shape ----
    pub spectral_centroid: Option<f64>,
    pub spectral_rolloff: Option<f64>,
    pub spectral_flatness: Option<f64>,
    pub spectral_bandwidth: Option<f64>,
    pub spectral_flux: Option<f64>,
    pub zero_crossing_rate: Option<f64>,
    pub brightness: Option<f64>,

    // ---- tonal ----
    pub chroma: Option<serde_json::Value>,
    pub musical_key: Option<i16>,
    pub musical_mode: Option<i16>,
    pub key_confidence: Option<f64>,

    // ---- rhythm ----
    pub tempo_bpm: Option<f64>,
    pub tempo_confidence: Option<f64>,
    pub onset_rate: Option<f64>,
    pub beat_grid: Option<serde_json::Value>,

    // ---- high-level descriptors, 0..1 ----
    pub danceability: Option<f64>,
    pub valence: Option<f64>,
    pub acousticness: Option<f64>,
    pub instrumentalness: Option<f64>,
    pub speechiness: Option<f64>,
    pub liveness: Option<f64>,

    // ---- vectors ----
    pub timbre: Option<serde_json::Value>,
    pub embedding: Option<serde_json::Value>,
    pub segments: Option<serde_json::Value>,

    pub analysis_version: i32,
    pub analysis_ms: Option<i32>,
    pub analyzed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

impl MusicAnalysis {
    /// Human-readable key, e.g. "F# minor". `None` until the tonal pass runs.
    pub fn key_name(&self) -> Option<String> {
        let key = self.musical_key?;
        let name = crate::music::dsp::KEY_NAMES.get(key as usize)?;
        let mode = if self.musical_mode == Some(1) {
            "major"
        } else {
            "minor"
        };
        Some(format!("{name} {mode}"))
    }
}

/// The analysis plus its human-facing extras. What the API returns.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct AnalysisView {
    #[serde(flatten)]
    pub analysis: MusicAnalysis,
    pub key_name: Option<String>,
    pub tempo_band: Option<String>,
    pub energy_band: Option<String>,
}

impl From<MusicAnalysis> for AnalysisView {
    fn from(analysis: MusicAnalysis) -> Self {
        Self {
            key_name: analysis.key_name(),
            tempo_band: analysis.tempo_bpm.map(|b| tempo_band(b).to_string()),
            energy_band: analysis.energy.map(|e| energy_band(e).to_string()),
            analysis,
        }
    }
}

/// A single play, as reported by the client. Per user.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct MusicListen {
    pub id: Uuid,
    pub user_id: Uuid,
    pub media_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub seconds_played: f64,
    pub track_seconds: Option<f64>,
    pub percent_played: Option<f64>,
    pub completed: bool,
    pub skipped: bool,
    pub source: Option<String>,
    pub device: Option<String>,
    pub context: Option<String>,
}

/// Request body for `POST /api/music/listen`.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RecordListen {
    pub media_id: Uuid,
    #[serde(default)]
    pub seconds_played: f64,
    #[serde(default)]
    pub track_seconds: Option<f64>,
    /// Set by the client when playback reached the end.
    #[serde(default)]
    pub completed: Option<bool>,
    /// Set when the user moved on early. Overrides the derived guess.
    #[serde(default)]
    pub skipped: Option<bool>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    /// The track played immediately before this one, if any. Feeds the
    /// co-occurrence graph that powers "listeners who played this also played".
    #[serde(default)]
    pub previous_media_id: Option<Uuid>,
}

/// One learned preference: "genre=jazz weighs 12.4 across 31 plays".
#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct AffinityRow {
    pub dimension: String,
    pub key: String,
    pub weight: f64,
    pub plays: i32,
    pub skips: i32,
    pub last_seen: DateTime<Utc>,
}

/// The per-user taste summary, derived from that user's listens alone.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MusicTaste {
    pub total_listens: i64,
    pub distinct_tracks: i64,
    pub total_hours: f64,
    pub completion_rate: f64,
    pub skip_rate: f64,
    pub top_genres: Vec<AffinityRow>,
    pub top_artists: Vec<AffinityRow>,
    pub top_keys: Vec<AffinityRow>,
    /// Preferred tempo bands (`slow`, `mid`, `up`, `fast`) by weight.
    pub tempo_profile: Vec<AffinityRow>,
    pub energy_profile: Vec<AffinityRow>,
    /// Hours of day, 0-23, ordered by listening volume.
    pub peak_hours: Vec<i16>,
    /// True until there is enough history to personalize. The client shows the
    /// global charts instead of pretending to know the user.
    pub cold_start: bool,
}

/// Global, cross-user engagement for one track. This is the half of the data
/// that compounds: every listen by anyone sharpens it.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, utoipa::ToSchema)]
pub struct TrackStats {
    pub media_id: Uuid,
    pub plays: i64,
    pub completions: i64,
    pub skips: i64,
    pub unique_listeners: i64,
    pub seconds_played: f64,
    pub avg_percent: f64,
    pub completion_rate: f64,
    pub skip_rate: f64,
    /// Wilson lower bound on completion — ranks by evidence, so one lucky play
    /// cannot outrank a track with a thousand.
    pub quality: f64,
    pub trend: f64,
    pub last_played_at: Option<DateTime<Utc>>,
}

/// A recommended track plus the reasons it surfaced.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MusicRecommendation {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub artist: Option<String>,
    pub score: f64,
    pub tempo_bpm: Option<f64>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub valence: Option<f64>,
    pub key_name: Option<String>,
    /// Which signal put it here: `taste`, `similar`, `collaborative`, `trending`.
    pub basis: String,
    pub reasons: Vec<String>,
}

/// One row of the exportable dataset: track features plus anonymous aggregate
/// engagement. Contains no user identifiers by construction.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct DatasetRow {
    pub media_id: Uuid,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub release_year: Option<i32>,
    pub genres: Vec<String>,
    pub duration_seconds: Option<f64>,
    pub tempo_bpm: Option<f64>,
    pub tempo_confidence: Option<f64>,
    pub musical_key: Option<i16>,
    pub musical_mode: Option<i16>,
    pub key_confidence: Option<f64>,
    pub loudness_lufs: Option<f64>,
    pub dynamic_range: Option<f64>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub valence: Option<f64>,
    pub acousticness: Option<f64>,
    pub instrumentalness: Option<f64>,
    pub speechiness: Option<f64>,
    pub liveness: Option<f64>,
    pub brightness: Option<f64>,
    pub spectral_centroid: Option<f64>,
    pub spectral_flatness: Option<f64>,
    pub zero_crossing_rate: Option<f64>,
    pub timbre: Option<serde_json::Value>,
    pub embedding: Option<serde_json::Value>,
    // aggregate engagement — no per-user rows, ever
    pub plays: i64,
    pub unique_listeners: i64,
    pub completion_rate: f64,
    pub skip_rate: f64,
    pub quality: f64,
    pub analysis_version: i32,
}

/// Tempo buckets. Coarse on purpose — exact BPM is too sparse to learn from.
pub fn tempo_band(bpm: f64) -> &'static str {
    match bpm {
        b if b < 80.0 => "slow",
        b if b < 110.0 => "mid",
        b if b < 140.0 => "up",
        _ => "fast",
    }
}

pub fn energy_band(energy: f64) -> &'static str {
    match energy {
        e if e < 0.33 => "calm",
        e if e < 0.66 => "steady",
        _ => "intense",
    }
}

/// Wilson score lower bound at ~95% confidence.
///
/// The point is evidence, not average: 1 completion out of 1 play scores well
/// below 900 out of 1000, so a new track cannot leapfrog a proven one.
pub fn wilson_lower_bound(successes: i64, total: i64) -> f64 {
    if total <= 0 {
        return 0.0;
    }
    const Z: f64 = 1.96;
    let n = total as f64;
    let p = successes as f64 / n;
    let z2 = Z * Z;
    let denom = 1.0 + z2 / n;
    let centre = p + z2 / (2.0 * n);
    let margin = Z * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt();
    ((centre - margin) / denom).clamp(0.0, 1.0)
}
