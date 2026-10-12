use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Behavior actions the engine learns from. Every in-app touchpoint maps to
/// one of these; weights express signal strength (`drop` is a negative vote).
pub const ACTION_WEIGHTS: &[(&str, f64)] = &[
    ("play", 1.0),
    ("complete", 2.0),
    ("rewatch", 1.5),
    ("rate", 1.25),
    ("favorite", 3.0),
    ("drop", -1.5),
];

pub fn action_weight(action: &str) -> f64 {
    ACTION_WEIGHTS
        .iter()
        .find(|(a, _)| *a == action)
        .map(|(_, w)| *w)
        .unwrap_or(1.0)
}

// ---------- learned profile ----------

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Affinity {
    pub key: String,
    /// decayed weight normalized to 0–100 against the top entry
    pub weight: f64,
    pub plays: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TopItem {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub media_type: String,
    pub plays: i64,
    /// exponential time-decayed weight (half-life = HEURISTIC_HALF_LIFE_DAYS)
    pub weight: f64,
    pub last_played_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HabitModel {
    /// smoothed play density per hour of day (UTC, 24 buckets)
    pub hour_histogram: Vec<f64>,
    /// smoothed play density per weekday (0=Sunday … 6=Saturday)
    pub weekday_histogram: Vec<f64>,
    pub peak_hours: Vec<u32>,
    pub peak_weekdays: Vec<u32>,
    pub chronotype: String,
    pub weekend_shift_hours: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SessionModel {
    pub count: i64,
    pub avg_items_per_session: f64,
    pub avg_session_minutes: f64,
    pub longest_session_minutes: f64,
    pub binge_sessions: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RepeatModel {
    pub replay_rate: f64,
    pub most_replayed: Vec<TopItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TypeStat {
    pub media_type: String,
    pub plays: i64,
    pub share_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Streaks {
    pub current_days: i64,
    pub longest_days: i64,
    pub active_days_total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Momentum {
    pub plays_last_7d: i64,
    pub plays_prev_7d: i64,
    pub delta_percent: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Persona {
    pub id: String,
    pub label: String,
    pub confidence: f64,
}

/// The user's full learned behavioral profile — the engine's brain dump.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HeuristicProfile {
    pub total_plays: i64,
    pub unique_titles: i64,
    pub total_hours: f64,
    pub genre_affinity: Vec<Affinity>,
    pub decade_affinity: Vec<Affinity>,
    pub type_mix: Vec<TypeStat>,
    pub top_items: Vec<TopItem>,
    pub habits: HabitModel,
    pub sessions: SessionModel,
    pub repeat: RepeatModel,
    pub streaks: Streaks,
    pub momentum: Momentum,
    pub explorer_index: f64,
    pub completion_rate: f64,
    /// share of plays abandoned under 15% progress
    pub abandonment_rate: f64,
    pub personas: Vec<Persona>,
    pub window_first: Option<DateTime<Utc>>,
    pub window_last: Option<DateTime<Utc>>,
    pub generated_at: DateTime<Utc>,
}

// ---------- Wrapped-style recap ----------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WrappedPeriod {
    Week,
    Month,
    Year,
    All,
}

impl WrappedPeriod {
    pub fn days(self) -> Option<i32> {
        match self {
            WrappedPeriod::Week => Some(7),
            WrappedPeriod::Month => Some(30),
            WrappedPeriod::Year => Some(365),
            WrappedPeriod::All => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            WrappedPeriod::Week => "This Week",
            WrappedPeriod::Month => "This Month",
            WrappedPeriod::Year => "This Year",
            WrappedPeriod::All => "All Time",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BiggestDay {
    pub day: NaiveDate,
    pub plays: i64,
}

/// Spotify-Wrapped / Netflix-Recap style period summary.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Wrapped {
    pub period: String,
    pub total_plays: i64,
    pub total_hours: f64,
    pub unique_titles: i64,
    pub new_discoveries: i64,
    pub top_items: Vec<TopItem>,
    pub top_genres: Vec<Affinity>,
    pub top_decades: Vec<Affinity>,
    /// most-engaged cast/directors across the period's titles
    pub top_people: Vec<Affinity>,
    /// play counts bucketed by day (week/month) or month (year/all)
    pub activity_buckets: Vec<ActivityBucket>,
    pub biggest_day: Option<BiggestDay>,
    pub longest_session_minutes: f64,
    pub active_days: i64,
    pub completion_rate: f64,
    pub headline: String,
    pub generated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ActivityBucket {
    /// ISO date (day buckets) or "YYYY-MM" (month buckets)
    pub label: String,
    pub plays: i64,
}

// ---------- insights & predictions ----------

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Insight {
    pub id: String,
    pub headline: String,
    pub detail: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct PredictedItem {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub media_type: String,
    pub score: f64,
    pub reasons: Vec<String>,
}

/// Learned "more like this" blending content + collaborative similarity.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SimilarTitle {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub media_type: String,
    pub score: f64,
    pub reasons: Vec<String>,
}
