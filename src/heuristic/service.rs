//! The heuristic intelligence layer — SPACE's built-in brain.
//!
//! Fully local and deterministic — no external AI. Techniques over the
//! internal behavior stream:
//!
//! * exponential time-decay weighting (recency half-life) for all affinities
//! * circular-smoothed circadian + weekly habit histograms → chronotype
//! * gap-based session reconstruction with binge detection
//! * replay/loyalty vs exploration indices
//! * activity streaks and 7-day momentum
//! * multi-label persona classification with confidences
//! * Spotify-Wrapped style period recaps
//! * explainable next-up predictions (genre affinity × popularity × recency)

use chrono::{Datelike, Duration as ChronoDuration, NaiveDate, Timelike, Utc};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

use crate::error::ApiError;
use crate::heuristic::models::{
    ActivityBucket, Affinity, BiggestDay, HabitModel, HeuristicProfile, Insight, Momentum, Persona,
    PredictedItem, RepeatModel, SessionModel, SimilarTitle, Streaks, TopItem, TypeStat, Wrapped,
    WrappedPeriod,
};
use crate::heuristic::repo;
use crate::state::AppState;

/// TTL for the cached learned profile (predictions stay uncached — they are
/// time-of-day aware).
const PROFILE_CACHE_TTL_SECS: u64 = 90;

fn profile_cache_key(user_id: Uuid) -> String {
    format!("heur:user:{user_id}:profile")
}

/// Cached wrapper around [`build_profile`] — one Redis/memory round trip
/// instead of a dozen aggregate queries per page load.
pub async fn build_profile_cached(
    state: &Arc<AppState>,
    user_id: Uuid,
) -> Result<HeuristicProfile, ApiError> {
    let key = profile_cache_key(user_id);
    if let Some(value) = state.cache.get(&key).await {
        if let Ok(profile) = serde_json::from_value::<HeuristicProfile>(value) {
            return Ok(profile);
        }
    }
    let profile = build_profile(&state.pool, user_id).await?;
    if let Ok(value) = serde_json::to_value(&profile) {
        state
            .cache
            .set(&key, value, std::time::Duration::from_secs(PROFILE_CACHE_TTL_SECS))
            .await;
    }
    Ok(profile)
}

fn to_affinities(rows: Vec<(String, f64, i64)>) -> Vec<Affinity> {
    let max = rows.first().map(|r| r.1).unwrap_or(0.0).max(1e-9);
    rows.into_iter()
        .map(|(key, weight, plays)| Affinity {
            key,
            weight: ((weight / max) * 100.0 * 10.0).round() / 10.0,
            plays,
        })
        .collect()
}

// ---------- pure math helpers (unit-tested) ----------

/// Circular smoothing over the 24h clock so 23:00 and 01:00 are neighbors.
pub fn smooth_circular(hist: &[f64; 24]) -> [f64; 24] {
    let mut out = [0.0f64; 24];
    for i in 0..24 {
        let prev = hist[(i + 23) % 24];
        let next = hist[(i + 1) % 24];
        out[i] = prev * 0.25 + hist[i] * 0.5 + next * 0.25;
    }
    out
}

pub fn weighted_mean_hour(hist: &[f64; 24]) -> f64 {
    // circular mean via vector averaging on the clock face
    let mut sin_sum = 0.0;
    let mut cos_sum = 0.0;
    for (h, w) in hist.iter().enumerate() {
        let angle = (h as f64) * std::f64::consts::TAU / 24.0;
        sin_sum += w * angle.sin();
        cos_sum += w * angle.cos();
    }
    if sin_sum.abs() < 1e-12 && cos_sum.abs() < 1e-12 {
        return 0.0;
    }
    let mean_angle = sin_sum.atan2(cos_sum);
    let hours = mean_angle * 24.0 / std::f64::consts::TAU;
    if hours < 0.0 { hours + 24.0 } else { hours }
}

pub fn chronotype(mean_hour: f64) -> &'static str {
    match mean_hour {
        h if h < 4.5 => "nocturnal",
        h if h < 8.5 => "night owl",
        h if h < 11.5 => "late riser",
        h if h < 16.5 => "daytime regular",
        h if h < 20.5 => "evening wind-down",
        _ => "prime-time loyalist",
    }
}

/// Indices of the n largest buckets, in descending order.
pub fn peak_buckets(hist: &[f64], n: usize) -> Vec<u32> {
    let mut order: Vec<usize> = (0..hist.len()).collect();
    order.sort_by(|a, b| hist[*b].partial_cmp(&hist[*a]).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = hist.iter().sum();
    order
        .into_iter()
        .filter(|i| total <= 0.0 || hist[*i] > total * 0.02)
        .take(n)
        .map(|i| i as u32)
        .collect()
}

pub struct SessionStats {
    pub count: i64,
    pub avg_items: f64,
    pub avg_minutes: f64,
    pub longest_minutes: f64,
    pub binges: i64,
}

/// Gap-based session reconstruction from a chronological event list.
pub fn sessions_from_timeline(
    events: &[(chrono::DateTime<Utc>, String)],
    gap_minutes: i64,
) -> SessionStats {
    if events.is_empty() {
        return SessionStats { count: 0, avg_items: 0.0, avg_minutes: 0.0, longest_minutes: 0.0, binges: 0 };
    }
    let gap = ChronoDuration::minutes(gap_minutes);
    let mut sessions: Vec<Vec<&(chrono::DateTime<Utc>, String)>> = vec![vec![&events[0]]];
    for pair in events[1..].iter() {
        let last = sessions.last_mut().expect("non-empty");
        if pair.0 - last.last().expect("non-empty").0 > gap {
            sessions.push(vec![pair]);
        } else {
            last.push(pair);
        }
    }

    let mut longest_minutes = 0.0f64;
    let mut total_span_minutes = 0.0f64;
    let mut total_items = 0usize;
    let mut binges = 0i64;
    for session in &sessions {
        total_items += session.len();
        let span = (session.last().unwrap().0 - session[0].0).num_seconds() as f64 / 60.0;
        total_span_minutes += span;
        if span > longest_minutes {
            longest_minutes = span;
        }
        // binge = 3+ events of the same media_type inside one session
        let mut kinds: HashMap<&str, usize> = HashMap::new();
        for (_, media_type) in session {
            *kinds.entry(media_type.as_str()).or_insert(0) += 1;
        }
        if kinds.values().any(|c| *c >= 3) {
            binges += 1;
        }
    }
    SessionStats {
        count: sessions.len() as i64,
        avg_items: total_items as f64 / sessions.len() as f64,
        avg_minutes: total_span_minutes / sessions.len() as f64,
        longest_minutes,
        binges,
    }
}

pub fn streaks_from_days(days: &[NaiveDate], today: NaiveDate) -> (i64, i64) {
    if days.is_empty() {
        return (0, 0);
    }
    let mut longest = 1i64;
    let mut run = 1i64;
    for window in days.windows(2) {
        if window[1] - window[0] == chrono::Duration::days(1) {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 1;
        }
    }
    // current streak counts back from the most recent active day (allow today or yesterday)
    let last = *days.last().unwrap();
    let mut current = 0i64;
    if (today - last).num_days() <= 1 {
        current = 1;
        let mut cursor = last;
        for day in days.iter().rev().skip(1) {
            if cursor - *day == chrono::Duration::days(1) {
                current += 1;
                cursor = *day;
            } else {
                break;
            }
        }
    }
    (current, longest)
}

/// Mean play-hour on weekends vs weekdays (positive = shifts later on weekends).
pub fn weekend_shift(events: &[(chrono::DateTime<Utc>, String)]) -> f64 {
    let mut weekend = (0.0, 0u32);
    let mut weekday = (0.0, 0u32);
    for (ts, _) in events {
        let dow = ts.weekday().num_days_from_sunday();
        let hour = ts.hour() as f64 + ts.minute() as f64 / 60.0;
        if dow == 0 || dow == 6 {
            weekend.0 += hour;
            weekend.1 += 1;
        } else {
            weekday.0 += hour;
            weekday.1 += 1;
        }
    }
    if weekend.1 == 0 || weekday.1 == 0 {
        return 0.0;
    }
    (weekend.0 / weekend.1 as f64) - (weekday.0 / weekday.1 as f64)
}

fn persona_label_for(media_type: &str) -> Option<(&'static str, &'static str)> {
    match media_type {
        "movie" => Some(("cinephile", "Cinephile")),
        "series" => Some(("series-binger", "Series Binger")),
        "music" => Some(("music-devotee", "Music Devotee")),
        "podcast" => Some(("podcast-head", "Podcast Head")),
        "audiobook" | "book" => Some(("bookworm", "Bookworm")),
        "manga" => Some(("manga-reader", "Manga Reader")),
        _ => None,
    }
}

pub struct PersonaInput<'a> {
    type_shares: &'a [(String, f64)],
    completion_rate: f64,
    explorer_index: f64,
    replay_rate: f64,
    binge_sessions: i64,
    avg_session_items: f64,
    current_streak: i64,
    chronotype: &'static str,
}

pub fn classify_personas(input: &PersonaInput) -> Vec<Persona> {
    let mut personas = Vec::new();

    for (media_type, share) in input.type_shares {
        if let Some((id, label)) = persona_label_for(media_type) {
            if *share >= 40.0 {
                let confidence = ((share - 40.0) / 30.0).clamp(0.05, 1.0);
                personas.push(Persona { id: id.to_string(), label: label.to_string(), confidence });
            }
        }
    }

    let push = |personas: &mut Vec<Persona>, cond: bool, base: f64, id: &'static str, label: &'static str| {
        if cond {
            personas.push(Persona {
                id: id.to_string(),
                label: label.to_string(),
                confidence: base.clamp(0.05, 1.0),
            });
        }
    };

    push(&mut personas, input.binge_sessions >= 3 || input.avg_session_items >= 6.0,
         (input.avg_session_items / 8.0).clamp(0.3, 1.0), "binge-watcher", "Binge Watcher");
    push(&mut personas, input.completion_rate >= 85.0,
         ((input.completion_rate - 85.0) / 15.0 * 0.6 + 0.4).clamp(0.2, 1.0),
         "completionist", "Completionist");
    push(&mut personas, input.explorer_index >= 0.7,
         ((input.explorer_index - 0.7) / 0.3).clamp(0.1, 1.0), "explorer", "Endless Explorer");
    push(&mut personas, input.replay_rate >= 3.0 && input.explorer_index < 0.55,
         ((input.replay_rate - 3.0) / 6.0).clamp(0.15, 1.0), "loyalist", "Comfort Replayer");
    push(&mut personas, input.current_streak >= 5,
         ((input.current_streak as f64 - 5.0) / 20.0).clamp(0.2, 1.0), "streak-keeper", "Streak Keeper");

    match input.chronotype {
        "nocturnal" => push(&mut personas, true, 0.9, "nocturne", "Nocturne"),
        "night owl" => push(&mut personas, true, 0.7, "nocturne", "Night Owl"),
        _ => {}
    }

    personas.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    personas.truncate(6);
    personas
}

// ---------- profile assembly ----------

pub async fn build_profile(pool: &PgPool, user_id: Uuid) -> Result<HeuristicProfile, ApiError> {
    let half = repo::HALF_LIFE_DAYS;

    let totals = repo::totals(pool, user_id).await?;
    let genres_raw = repo::genre_affinity(pool, user_id, half, None, 12).await?;
    let decades_raw = repo::decade_affinity(pool, user_id, half, None, 5).await?;
    let types_raw = repo::type_mix(pool, user_id, None).await?;
    let items_raw = repo::top_items(pool, user_id, half, None, 12, 1).await?;

    let hour_raw = repo::hour_histogram(pool, user_id, half, None).await?;
    let weekday_raw = repo::weekday_histogram(pool, user_id, half, None).await?;
    let smoothed = smooth_circular(&hour_raw);

    let completion = repo::completion_rate(pool, user_id).await?;
    let abandonment = repo::abandonment_rate(pool, user_id).await?;
    let timeline = repo::timeline(pool, user_id, Some(90)).await?;
    let days = repo::active_days(pool, user_id, None).await?;
    let (last7, prev7) = repo::momentum(pool, user_id).await?;

    let total_plays_f = totals.total_plays.max(1) as f64;

    let top_items: Vec<TopItem> = items_raw
        .iter()
        .map(|row| TopItem {
            media_id: row.media_id,
            title: row.title.clone(),
            poster_url: row.poster_url.clone(),
            media_type: row.media_type.clone(),
            plays: row.plays,
            weight: (row.weight * 100.0).round() / 100.0,
            last_played_at: row.last_played_at,
        })
        .collect();
    let max_weight = top_items.first().map(|t| t.weight).unwrap_or(1.0).max(1e-9);
    let mut normalized = top_items.clone();
    for item in &mut normalized {
        item.weight = ((item.weight / max_weight) * 100.0 * 10.0).round() / 10.0;
    }
    let most_replayed: Vec<TopItem> = normalized
        .iter()
        .filter(|t| t.plays >= 2)
        .cloned()
        .take(5)
        .collect();

    let type_total: i64 = types_raw.iter().map(|(_, p)| p).sum::<i64>().max(1);
    let type_stats: Vec<TypeStat> = types_raw
        .iter()
        .map(|(mt, plays)| TypeStat {
            media_type: mt.clone(),
            plays: *plays,
            share_percent: (*plays as f64 / type_total as f64 * 100.0 * 10.0).round() / 10.0,
        })
        .collect();
    let type_shares: Vec<(String, f64)> = type_stats
        .iter()
        .map(|t| (t.media_type.clone(), t.share_percent))
        .collect();

    let sessions = sessions_from_timeline(&timeline, repo::SESSION_GAP_MINUTES);
    let (current_days, longest_days) = streaks_from_days(&days, Utc::now().date_naive());
    let delta_percent = if prev7 == 0 {
        if last7 > 0 { 100.0 } else { 0.0 }
    } else {
        ((last7 - prev7) as f64 / prev7 as f64 * 100.0 * 10.0).round() / 10.0
    };

    let mean_hour = weighted_mean_hour(&smoothed);
    let chrono_type = chronotype(mean_hour);
    let explorer_index = (totals.unique_titles as f64 / total_plays_f * 1000.0).round() / 1000.0;
    let replay_rate = (total_plays_f / totals.unique_titles.max(1) as f64 * 100.0).round() / 100.0;

    let personas = classify_personas(&PersonaInput {
        type_shares: &type_shares,
        completion_rate: completion,
        explorer_index,
        replay_rate,
        binge_sessions: sessions.binges,
        avg_session_items: sessions.avg_items,
        current_streak: current_days,
        chronotype: chrono_type,
    });

    Ok(HeuristicProfile {
        total_plays: totals.total_plays,
        unique_titles: totals.unique_titles,
        total_hours: (totals.total_seconds as f64 / 3600.0 * 10.0).round() / 10.0,
        genre_affinity: to_affinities(genres_raw),
        decade_affinity: to_affinities(decades_raw),
        type_mix: type_stats,
        top_items: normalized,
        habits: HabitModel {
            hour_histogram: smoothed.to_vec(),
            weekday_histogram: weekday_raw.to_vec(),
            peak_hours: peak_buckets(&smoothed, 3),
            peak_weekdays: peak_buckets(&weekday_raw, 2),
            chronotype: chrono_type.to_string(),
            weekend_shift_hours: (weekend_shift(&timeline) * 10.0).round() / 10.0,
        },
        sessions: SessionModel {
            count: sessions.count,
            avg_items_per_session: (sessions.avg_items * 10.0).round() / 10.0,
            avg_session_minutes: (sessions.avg_minutes * 10.0).round() / 10.0,
            longest_session_minutes: (sessions.longest_minutes * 10.0).round() / 10.0,
            binge_sessions: sessions.binges,
        },
        repeat: RepeatModel { replay_rate, most_replayed },
        streaks: Streaks {
            current_days,
            longest_days,
            active_days_total: days.len() as i64,
        },
        momentum: Momentum { plays_last_7d: last7, plays_prev_7d: prev7, delta_percent },
        explorer_index,
        completion_rate: (completion * 10.0).round() / 10.0,
        abandonment_rate: (abandonment * 100.0 * 10.0).round() / 10.0,
        personas,
        window_first: totals.window_first,
        window_last: totals.window_last,
        generated_at: Utc::now(),
    })
}

// ---------- Wrapped-style recap ----------

pub async fn wrapped(
    pool: &PgPool,
    user_id: Uuid,
    period: WrappedPeriod,
) -> Result<Wrapped, ApiError> {
    let half = repo::HALF_LIFE_DAYS;
    let days = period.days();

    let window: (i64, i64, i64) = sqlx::query_as(
        &format!(
            "SELECT COUNT(*) FILTER (WHERE e.action IN ('play','complete','rewatch'))::bigint,
                    COUNT(DISTINCT e.media_id)::bigint,
                    COALESCE(SUM(e.effective_seconds), 0)::bigint
             FROM heuristic_events e
             WHERE e.user_id = $1{}",
            repo_window(days),
        ),
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    let (total_plays, unique_titles, total_seconds) = window;

    let items = repo::top_items(pool, user_id, half, days, 10, 1).await?;
    let max_weight = items.first().map(|i| i.weight).unwrap_or(1.0).max(1e-9);
    let top_items: Vec<TopItem> = items
        .into_iter()
        .map(|row| TopItem {
            media_id: row.media_id,
            title: row.title,
            poster_url: row.poster_url,
            media_type: row.media_type,
            plays: row.plays,
            weight: ((row.weight / max_weight) * 100.0 * 10.0).round() / 10.0,
            last_played_at: row.last_played_at,
        })
        .collect();

    let top_genres = to_affinities(repo::genre_affinity(pool, user_id, half, days, 8).await?);
    let top_decades = to_affinities(repo::decade_affinity(pool, user_id, half, days, 3).await?);
    let top_people = to_affinities(repo::top_people(pool, user_id, half, days, 5).await?);
    let activity_buckets: Vec<ActivityBucket> = repo::activity_buckets(pool, user_id, days)
        .await?
        .into_iter()
        .map(|(label, plays)| ActivityBucket { label, plays })
        .collect();
    let biggest_day = repo::biggest_day(pool, user_id, days)
        .await?
        .map(|(day, plays)| BiggestDay { day, plays });
    let timeline = repo::timeline(pool, user_id, days).await?;
    let longest_session_minutes =
        (sessions_from_timeline(&timeline, repo::SESSION_GAP_MINUTES).longest_minutes * 10.0).round() / 10.0;
    let active_days = repo::active_days(pool, user_id, days).await?.len() as i64;
    let discoveries = repo::new_discoveries(pool, user_id, days).await?;
    let completion = repo::completion_rate(pool, user_id).await?;

    let headline = build_headline(period, &top_genres, &top_decades, total_hours(total_seconds), unique_titles);

    Ok(Wrapped {
        period: period.label().to_string(),
        total_plays,
        total_hours: total_hours(total_seconds),
        unique_titles,
        new_discoveries: discoveries,
        top_items,
        top_genres,
        top_decades,
        top_people,
        activity_buckets,
        biggest_day,
        longest_session_minutes,
        active_days,
        completion_rate: (completion * 10.0).round() / 10.0,
        headline,
        generated_at: Utc::now(),
    })
}

fn repo_window(days: Option<i32>) -> String {
    match days {
        Some(d) => format!(" AND e.occurred_at > now() - make_interval(days => {d})"),
        None => String::new(),
    }
}

fn total_hours(seconds: i64) -> f64 {
    (seconds as f64 / 3600.0 * 10.0).round() / 10.0
}

fn build_headline(
    period: WrappedPeriod,
    genres: &[Affinity],
    decades: &[Affinity],
    hours: f64,
    titles: i64,
) -> String {
    let genre = genres
        .first()
        .map(|g| g.key.clone())
        .unwrap_or_else(|| "everything".to_string());
    match decades.first().map(|d| d.key.clone()) {
        Some(decade) => format!(
            "{} was a deep-dive into {} {}: {:.1} hours across {} titles.",
            period.label(), decade, genre, hours, titles.max(0)
        ),
        None => format!(
            "{} in {}: {:.1} hours across {} titles.",
            period.label(), genre, hours, titles.max(0)
        ),
    }
}

// ---------- deterministic insights ----------

pub fn derive_insights(profile: &HeuristicProfile) -> Vec<Insight> {
    let mut out = Vec::new();
    let fmt_hours = |h: u32| format!("{:02}:00", h % 24);

    if profile.total_plays > 0 {
        let peaks: Vec<String> = profile.habits.peak_hours.iter().map(|h| fmt_hours(*h)).collect();
        out.push(Insight {
            id: "chronotype".into(),
            headline: format!("You are a {}.", profile.habits.chronotype),
            detail: format!(
                "Your play density peaks around {}.",
                if peaks.is_empty() { "an even spread across the day".into() } else { peaks.join(", ") }
            ),
            confidence: 0.8,
        });

        if let Some(top) = profile.top_items.first() {
            out.push(Insight {
                id: "top-item".into(),
                headline: format!("\"{}\" owns your play history.", top.title),
                detail: format!(
                    "{} total plays, most recently {}.",
                    top.plays,
                    top.last_played_at.format("%b %d")
                ),
                confidence: 0.95,
            });
        }

        if let Some(genre) = profile.genre_affinity.first() {
            out.push(Insight {
                id: "top-genre".into(),
                headline: format!("{} is your home base.", genre.key),
                detail: format!(
                    "{} engagements touched this genre — your strongest taste signal.",
                    genre.plays
                ),
                confidence: 0.85,
            });
        }

        if profile.momentum.plays_prev_7d > 0 || profile.momentum.plays_last_7d > 0 {
            let direction = if profile.momentum.delta_percent >= 0.0 { "up" } else { "down" };
            out.push(Insight {
                id: "momentum".into(),
                headline: format!(
                    "Activity is {} {}% week-over-week.",
                    direction,
                    profile.momentum.delta_percent.abs()
                ),
                detail: format!(
                    "{} plays in the last 7 days vs {} the week before.",
                    profile.momentum.plays_last_7d, profile.momentum.plays_prev_7d
                ),
                confidence: 0.85,
            });
        }

        if profile.sessions.longest_session_minutes > 45.0 {
            out.push(Insight {
                id: "longest-session".into(),
                headline: format!(
                    "Longest session: {:.0} minutes straight.",
                    profile.sessions.longest_session_minutes
                ),
                detail: format!(
                    "Detected {} binge-style sessions in the last 90 days.",
                    profile.sessions.binge_sessions
                ),
                confidence: 0.75,
            });
        }

        if profile.streaks.current_days >= 3 {
            out.push(Insight {
                id: "streak".into(),
                headline: format!("{}-day active streak.", profile.streaks.current_days),
                detail: format!(
                    "Personal best: {} consecutive days across {} active days total.",
                    profile.streaks.longest_days, profile.streaks.active_days_total
                ),
                confidence: 0.9,
            });
        }

        if profile.habits.weekend_shift_hours >= 1.0 {
            out.push(Insight {
                id: "weekend-shift".into(),
                headline: "Weekends push you later.".into(),
                detail: format!(
                    "Weekend plays start about {:.1} h later than weekdays.",
                    profile.habits.weekend_shift_hours
                ),
                confidence: 0.6,
            });
        } else if profile.habits.weekend_shift_hours <= -1.0 {
            out.push(Insight {
                id: "weekend-shift".into(),
                headline: "Weekends make you an early bird.".into(),
                detail: format!(
                    "Weekend plays start about {:.1} h earlier than weekdays.",
                    -profile.habits.weekend_shift_hours
                ),
                confidence: 0.6,
            });
        }

        if profile.explorer_index >= 0.85 {
            out.push(Insight {
                id: "explorer".into(),
                headline: "You almost never repeat yourself.".into(),
                detail: format!(
                    "{:.0}% of everything you play is something new — a deep explorer pattern.",
                    profile.explorer_index * 100.0
                ),
                confidence: 0.8,
            });
        } else if profile.repeat.replay_rate >= 2.5 {
            out.push(Insight {
                id: "replay".into(),
                headline: "You are a comfort replayer.".into(),
                detail: format!(
                    "Average title is played {:.1}× before you move on.",
                    profile.repeat.replay_rate
                ),
                confidence: 0.8,
            });
        }

        if profile.abandonment_rate >= 25.0 {
            out.push(Insight {
                id: "abandonment".into(),
                headline: format!("You bail early on {:.0}% of plays.", profile.abandonment_rate),
                detail: "Titles dropped under 15% progress now count against similar picks."
                    .into(),
                confidence: 0.65,
            });
        }
    }

    out
}

// ---------- explainable next-up prediction ----------

const CANDIDATE_POOL: i64 = 250;

/// Blended ranking: personal genre affinity + server popularity +
/// collaborative co-watch strength + sequential (played-after) transitions,
/// then MMR-diversified so one dominant genre cannot monopolize the list.
pub async fn predict_next(pool: &PgPool, user_id: Uuid) -> Result<Vec<PredictedItem>, ApiError> {
    let affinity_rows = repo::genre_affinity(pool, user_id, repo::HALF_LIFE_DAYS, None, 20).await?;
    let max_affinity = affinity_rows.first().map(|r| r.1).unwrap_or(0.0).max(1e-9);
    let genre_weights: HashMap<String, f64> = affinity_rows
        .into_iter()
        .map(|(g, w, _)| (g.to_lowercase(), w / max_affinity))
        .collect();

    let candidates = repo::catalog_candidates(pool, user_id, CANDIDATE_POOL).await?;
    let max_popularity = candidates
        .iter()
        .map(|c| c.popularity)
        .fold(1e-9f64, f64::max);

    // my recent titles → names for reasons + seeds for sequential signal
    let my_items = repo::top_items(pool, user_id, repo::HALF_LIFE_DAYS, None, 40, 1).await?;
    let item_titles: HashMap<Uuid, String> = my_items
        .iter()
        .map(|i| (i.media_id, i.title.clone()))
        .collect();
    let now = Utc::now();
    let recent_ids: HashSet<Uuid> = my_items
        .iter()
        .filter(|i| (now - i.last_played_at).num_days() <= 14)
        .map(|i| i.media_id)
        .collect();

    // collaborative: strongest "users like you also engaged" partner per seed
    let pairs = repo::co_watch(pool, user_id, 600).await?;
    let mut collab: HashMap<Uuid, (f64, Uuid)> = HashMap::new();
    for (seed, partner, strength) in pairs {
        if !item_titles.contains_key(&seed) {
            continue; // only credit seeds the user actually knows
        }
        let entry = collab.entry(partner).or_insert((0.0, seed));
        if strength as f64 > entry.0 {
            *entry = (strength as f64, seed);
        }
    }
    let max_collab = collab.values().map(|(s, _)| *s).fold(1e-9f64, f64::max);

    // sequential: what tends to get played right after something I recently played
    let transitions = repo::transitions(pool, 800).await?;
    let mut markov: HashMap<Uuid, (i64, Uuid)> = HashMap::new();
    for (from, to, n) in transitions {
        if !recent_ids.contains(&from) {
            continue;
        }
        let entry = markov.entry(to).or_insert((0, from));
        if n > entry.0 {
            *entry = (n, from);
        }
    }

    let has_taste = !genre_weights.is_empty() || !collab.is_empty();
    let cold_start = !has_taste;

    let mut scored: Vec<ScoredCandidate> = candidates
        .into_iter()
        .filter_map(|candidate| {
            if candidate.completed {
                return None; // finished titles leave the next-up lane
            }
            let matches: Vec<(&String, f64)> = candidate
                .genres
                .iter()
                .filter_map(|g| genre_weights.get(&g.to_lowercase()).map(|w| (g, *w)))
                .collect();
            let genre_fit = if matches.is_empty() {
                0.0
            } else {
                matches.iter().map(|(_, w)| *w).sum::<f64>() / matches.len() as f64
            };

            let popularity_fit = candidate.popularity / max_popularity;
            let collab_fit = collab
                .get(&candidate.media_id)
                .map(|(strength, _)| (strength / max_collab).min(1.0))
                .unwrap_or(0.0);
            let markov_fit = markov
                .get(&candidate.media_id)
                .map(|(n, _)| (*n as f64 / 5.0).min(1.0))
                .unwrap_or(0.0);

            let minutes_idle = candidate.minutes_since_last.unwrap_or(i64::MAX / 2);
            let freshness = if candidate.minutes_since_last.is_none() {
                1.15 // fresh discovery boost
            } else if minutes_idle < 90 {
                return None; // engaged within the last ~90 min: already queued
            } else if minutes_idle < 60 * 24 * 14 {
                0.9
            } else {
                1.05 // long enough ago that a revisit is plausible
            };

            let raw = if has_taste {
                (0.45 * genre_fit
                    + 0.15 * popularity_fit
                    + 0.25 * collab_fit
                    + 0.15 * markov_fit)
                    * freshness
            } else {
                // cold start: server-wide popularity still beats an empty page
                0.25 + 0.75 * popularity_fit
            };
            if raw <= 0.01 {
                return None;
            }

            let mut reasons = Vec::new();
            if collab_fit > 0.0 {
                if let Some((_, seed)) = collab.get(&candidate.media_id) {
                    match item_titles.get(seed) {
                        Some(title) => reasons.push(format!("watchers of \"{title}\" also play this")),
                        None => reasons.push("popular with similar watchers".into()),
                    }
                }
            }
            if markov_fit > 0.0 {
                if let Some((_, from)) = markov.get(&candidate.media_id) {
                    match item_titles.get(from) {
                        Some(title) => reasons.push(format!("often played right after \"{title}\"")),
                        None => reasons.push("common next pick".into()),
                    }
                }
            }
            if !matches.is_empty() {
                let mut ranked: Vec<(&String, f64)> = matches.clone();
                ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                let names: Vec<String> =
                    ranked.iter().take(2).map(|(g, _)| (*g).clone()).collect();
                reasons.push(format!("matches your taste for {}", names.join(" & ")));
            }
            if candidate.minutes_since_last.is_none() {
                reasons.push("new to you".into());
            } else if minutes_idle >= 60 * 24 * 14 {
                reasons.push("back after a while".into());
            }
            if popularity_fit > 0.8 && (cold_start || matches.len() < 2) {
                reasons.push("trending on your server".into());
            }
            if reasons.is_empty() {
                return None;
            }

            Some(ScoredCandidate {
                item: PredictedItem {
                    media_id: candidate.media_id,
                    title: candidate.title,
                    poster_url: candidate.poster_url,
                    media_type: candidate.media_type,
                    score: (raw * 100.0).round() / 100.0,
                    reasons,
                },
                genres: candidate.genres.iter().map(|g| g.to_lowercase()).collect(),
            })
        })
        .collect();

    scored.sort_by(|a, b| b.item.score.partial_cmp(&a.item.score).unwrap_or(std::cmp::Ordering::Equal));
    Ok(mmr_diversify(scored, MMR_LAMBDA, 12))
}

/// A scored prediction plus its genre fingerprint for diversification.
pub struct ScoredCandidate {
    pub item: PredictedItem,
    pub genres: HashSet<String>,
}

/// Exploration weight for MMR diversification: 0 = pure relevance,
/// 1 = maximal variety.
pub const MMR_LAMBDA: f64 = 0.55;

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

/// Greedy MMR: repeatedly take the highest `score − λ × max-similarity-to-
/// already-selected`, so one dominant genre cannot monopolize next-up.
pub fn mmr_diversify(mut pool: Vec<ScoredCandidate>, lambda: f64, keep: usize) -> Vec<PredictedItem> {
    pool.sort_by(|a, b| b.item.score.partial_cmp(&a.item.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut selected_genres: Vec<HashSet<String>> = Vec::new();
    let mut selected: Vec<PredictedItem> = Vec::with_capacity(keep);

    while !pool.is_empty() && selected.len() < keep {
        let best = pool
            .iter()
            .enumerate()
            .map(|(idx, cand)| {
                let penalty = selected_genres
                    .iter()
                    .map(|sel| jaccard(sel, &cand.genres))
                    .fold(0.0f64, f64::max);
                (idx, cand.item.score - lambda * penalty * cand.item.score.max(1.0))
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(idx, _)| idx)
            .expect("pool non-empty");
        let chosen = pool.remove(best);
        selected_genres.push(chosen.genres);
        selected.push(chosen.item);
    }
    selected
}

// ---------- learned "more like this" ----------

/// Blend content similarity (shared genres) with collaborative co-watch
/// strength around one anchor title.
pub async fn similar_titles(
    pool: &PgPool,
    user_id: Uuid,
    media_id: Uuid,
    limit: i64,
) -> Result<Vec<SimilarTitle>, ApiError> {
    let anchor: Option<(String,)> = sqlx::query_as("SELECT title FROM media WHERE id = $1")
        .bind(media_id)
        .fetch_optional(pool)
        .await?;
    let Some((anchor_title,)) = anchor else {
        return Err(ApiError::not_found("media"));
    };

    let target_genres: HashSet<String> = sqlx::query_as::<_, (String,)>(
        "SELECT g.name FROM media_genres mg JOIN genres g ON g.id = mg.genre_id
         WHERE mg.media_id = $1",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(g,)| g.to_lowercase())
    .collect();

    let neighbors = sqlx::query_as::<_, (Uuid, String, Option<String>, String, Vec<String>)>(
        "SELECT m.id, m.title, m.poster_url, m.media_type,
                COALESCE(array_agg(DISTINCT g.name) FILTER (WHERE g.name IS NOT NULL), '{}')
         FROM media m
         JOIN media_genres mg ON mg.media_id = m.id
         JOIN genres g ON g.id = mg.genre_id
         WHERE m.id <> $1
           AND EXISTS (
               SELECT 1 FROM media_genres mg2
               WHERE mg2.media_id = m.id AND mg2.genre_id IN (
                   SELECT genre_id FROM media_genres WHERE media_id = $1))
           AND NOT EXISTS (
               SELECT 1 FROM watch_history wh
               WHERE wh.user_id = $2 AND wh.media_id = m.id AND wh.completed)
         GROUP BY m.id, m.title, m.poster_url, m.media_type
         ORDER BY m.popularity DESC LIMIT $3",
    )
    .bind(media_id)
    .bind(user_id)
    .bind(limit.clamp(1, 40) * 4)
    .fetch_all(pool)
    .await?;

    // collaborative strength involving this anchor (either direction)
    let pairs = repo::co_watch(pool, user_id, 1200).await?;
    let mut collab_strength: HashMap<Uuid, i64> = HashMap::new();
    for (seed, partner, strength) in pairs {
        if partner == media_id {
            *collab_strength.entry(seed).or_insert(0) =
                (*collab_strength.get(&seed).unwrap_or(&0)).max(strength);
        } else if seed == media_id {
            *collab_strength.entry(partner).or_insert(0) =
                (*collab_strength.get(&partner).unwrap_or(&0)).max(strength);
        }
    }
    let max_strength = collab_strength
        .values()
        .fold(0.0f64, |acc, &s| acc.max(s as f64))
        .max(1e-9);

    let mut scored: Vec<SimilarTitle> = neighbors
        .into_iter()
        .map(|(id, title, poster, media_type, genres)| {
            let genre_set: HashSet<String> = genres.iter().map(|g| g.to_lowercase()).collect();
            let shared: Vec<String> = target_genres
                .intersection(&genre_set)
                .take(2)
                .cloned()
                .collect();
            let content_fit = if target_genres.is_empty() {
                0.0
            } else {
                target_genres.intersection(&genre_set).count() as f64
                    / target_genres.union(&genre_set).count() as f64
            };
            let collab_fit = collab_strength
                .get(&id)
                .map(|s| (*s as f64 / max_strength).min(1.0))
                .unwrap_or(0.0);
            let score = 0.6 * content_fit + 0.4 * collab_fit;

            let mut reasons = Vec::new();
            if collab_fit > 0.0 {
                reasons.push(format!("fans of \"{anchor_title}\" also play this"));
            }
            if !shared.is_empty() {
                reasons.push(format!("shares {}", shared.join(" & ")));
            }

            SimilarTitle {
                media_id: id,
                title,
                poster_url: poster,
                media_type,
                score: (score * 100.0).round() / 100.0,
                reasons,
            }
        })
        .filter(|s| s.score > 0.0)
        .collect();

    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit.clamp(1, 40) as usize);
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn circular_smoothing_bridges_midnight() {
        let mut hist = [0.0f64; 24];
        hist[23] = 10.0;
        hist[1] = 10.0;
        let smoothed = smooth_circular(&hist);
        assert!(smoothed[0] > smoothed[5]);
        assert!(smoothed[23] >= smoothed[0]);
    }

    #[test]
    fn weighted_mean_hour_is_circular() {
        let mut hist = [0.0f64; 24];
        hist[23] = 5.0;
        hist[1] = 5.0;
        let mean = weighted_mean_hour(&hist);
        // naive average would be 12:00; circular mean must be midnight
        assert!(mean < 1.0 || mean > 23.0);
        assert_eq!(chronotype(weighted_mean_hour(&[0.0; 24])), "nocturnal");
        assert_eq!(chronotype(21.5), "prime-time loyalist");
    }

    #[test]
    fn peak_buckets_descending_and_filtered() {
        let mut hist = [0.1f64; 24];
        hist[20] = 9.0;
        hist[21] = 7.0;
        hist[3] = 2.0;
        assert_eq!(peak_buckets(&hist, 3), vec![20, 21, 3]);
    }

    #[test]
    fn sessions_split_on_gaps_and_detect_binges() {
        let t = |h: u32, m: u32| Utc.with_ymd_and_hms(2026, 1, 10, h, m, 0).unwrap();
        let events = vec![
            (t(20, 0), "series".to_string()),
            (t(20, 40), "series".to_string()),
            (t(21, 15), "series".to_string()), // binge: 3 series events, gaps < 45m
            (t(23, 59), "music".to_string()),  // new session
        ];
        let stats = sessions_from_timeline(&events, repo::SESSION_GAP_MINUTES);
        assert_eq!(stats.count, 2);
        assert_eq!(stats.binges, 1);
        assert_eq!(stats.longest_minutes, 75.0);
    }

    #[test]
    fn streaks_count_current_and_longest() {
        let d = |day| NaiveDate::from_ymd_opt(2026, 1, day).unwrap();
        let days = vec![d(3), d(4), d(5), d(8), d(9)];
        let today = d(9);
        let (current, longest) = streaks_from_days(&days, today);
        assert_eq!(current, 2);
        assert_eq!(longest, 3);
        assert_eq!(streaks_from_days(&days, d(20)).0, 0);
    }

    #[test]
    fn personas_reflect_dominant_signals() {
        let input = PersonaInput {
            type_shares: &[("music".to_string(), 72.0)],
            completion_rate: 92.0,
            explorer_index: 0.45,
            replay_rate: 4.2,
            binge_sessions: 5,
            avg_session_items: 8.0,
            current_streak: 9,
            chronotype: "night owl",
        };
        let personas = classify_personas(&input);
        let ids: Vec<&str> = personas.iter().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"music-devotee"));
        assert!(ids.contains(&"binge-watcher"));
        assert!(ids.contains(&"completionist"));
        assert!(ids.contains(&"loyalist"));
        assert!(ids.contains(&"streak-keeper"));
        assert!(ids.contains(&"nocturne"));
        assert!(!ids.contains(&"explorer"));
        assert!(personas.first().map(|p| p.confidence <= 1.0).unwrap_or(false));
    }

    #[test]
    fn action_weights_rank_signal_strength() {
        use crate::heuristic::models::{action_weight, WrappedPeriod};
        assert!(action_weight("favorite") > action_weight("complete"));
        assert!(action_weight("complete") > action_weight("play"));
        assert!(action_weight("drop") < 0.0);
        assert_eq!(WrappedPeriod::Year.days(), Some(365));
        assert_eq!(WrappedPeriod::All.days(), None);
        assert_eq!(WrappedPeriod::Week.label(), "This Week");
    }

    fn candidate(id: u8, score: f64, genres: &[&str]) -> ScoredCandidate {
        ScoredCandidate {
            item: PredictedItem {
                media_id: Uuid::from_bytes([
                    id, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, id,
                ]),
                title: format!("t{id}"),
                poster_url: None,
                media_type: "movie".into(),
                score,
                reasons: vec![],
            },
            genres: genres.iter().map(|g| g.to_string()).collect(),
        }
    }

    #[test]
    fn mmr_diversifies_across_genres() {
        let pool = vec![
            candidate(1, 9.0, &["action"]),
            candidate(2, 8.5, &["action"]), // near-duplicate of #1
            candidate(3, 7.0, &["drama"]),
        ];
        let picked = mmr_diversify(pool, MMR_LAMBDA, 3);
        let ids: Vec<String> = picked.iter().map(|p| p.title.clone()).collect();
        // the lower-scored action clone must not crowd out drama
        assert_eq!(picked.len(), 3);
        assert!(ids.contains(&"t1".to_string()));
        assert!(ids.contains(&"t3".to_string()));
        // top pick is still the highest scorer
        assert_eq!(picked[0].title, "t1");
        // and the two action titles cannot both sit above the drama pick
        assert!(ids.iter().position(|t| t == "t3").unwrap() < ids.len() - 1);
    }

    #[test]
    fn jaccard_handles_empty_sets() {
        let a: HashSet<String> = HashSet::new();
        let mut b: HashSet<String> = HashSet::new();
        b.insert("x".into());
        assert_eq!(jaccard(&a, &b), 0.0);
    }
}
