//! Turning plays into taste, and taste into recommendations.
//!
//! Two layers, deliberately kept apart:
//!
//! * **Global** — `music_analysis` and `music_track_stats`. One row per track,
//!   shared by everyone. A track's waveform, key and tempo are facts about the
//!   recording; its play count and completion rate are facts about the whole
//!   audience. Every listen by anyone sharpens this layer, which is why the
//!   corpus is worth more the longer it runs.
//! * **Per user** — `music_listens` and `music_affinity`. What *this* listener
//!   plays, and the preferences derived from it. Never mixed into the global
//!   tables, and never read by another user's recommendations except through
//!   the anonymous co-occurrence graph.
//!
//! Recommendations blend four signals over that split: personal taste, the
//! co-play graph, content similarity, and global quality. A brand new listener
//! gets the global charts; a listener with history gets progressively more of
//! the first three.

use std::collections::HashMap;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::music::dsp::KEY_NAMES;
use crate::music::models::{
    energy_band, tempo_band, MusicListen, MusicRecommendation, MusicTaste, RecordListen,
};
use crate::music::repo::{self, Candidate};

/// Fraction of a track that counts as a genuine listen rather than a sample.
const LISTEN_THRESHOLD: f64 = 0.30;
/// Past this, treat it as finished even without an explicit flag.
const COMPLETE_THRESHOLD: f64 = 0.90;
/// Interests halve every three weeks unless refreshed.
const HALF_LIFE_DAYS: f64 = 21.0;
/// Listens before recommendations stop leaning on the global charts.
const COLD_START_LISTENS: i64 = 12;
/// How many of the user's strongest tracks seed the neighbour lookups.
const SEED_COUNT: i64 = 12;

/// Record one play, fold it into the user's profile, and fold it into the
/// global corpus.
///
/// The weight is proportional to how much of the track was actually heard, so
/// a full play teaches far more than a three-second sample, and a skip subtracts.
pub async fn record_listen(
    pool: &PgPool,
    user_id: Uuid,
    body: RecordListen,
) -> ApiResult<MusicListen> {
    if body.seconds_played < 0.0 || !body.seconds_played.is_finite() {
        return Err(ApiError::bad_request("seconds_played must be positive"));
    }
    if body.track_seconds.is_some_and(|t| !t.is_finite() || t < 0.0) {
        return Err(ApiError::bad_request("track_seconds must be positive"));
    }

    let percent = body.track_seconds.and_then(|total| {
        (total > 0.0).then(|| (body.seconds_played / total * 100.0).clamp(0.0, 100.0))
    });
    let fraction = percent.map(|p| p / 100.0).unwrap_or(0.0);

    let completed = body.completed.unwrap_or(fraction >= COMPLETE_THRESHOLD);
    let skipped = body
        .skipped
        .unwrap_or(percent.is_some() && fraction < LISTEN_THRESHOLD && !completed);

    let listen = MusicListen {
        id: crate::utils::new_id(),
        user_id,
        media_id: body.media_id,
        started_at: Utc::now(),
        seconds_played: body.seconds_played,
        track_seconds: body.track_seconds,
        percent_played: percent,
        completed,
        skipped,
        source: body.source,
        device: body.device,
        context: body.context,
    };

    repo::insert_listen(pool, &listen).await?;

    // Everything downstream is derived state: a failure there must not lose the
    // listen or fail the caller's playback report.
    if let Err(e) = repo::record_global_play(pool, &listen).await {
        tracing::warn!(error = %e, media_id = %listen.media_id, "global music stats update failed");
    }
    apply_to_profile(pool, user_id, &listen).await;
    link_previous(pool, &listen, body.previous_media_id).await;

    Ok(listen)
}

/// Strengthen the co-play edge between this track and the one before it.
///
/// This is the only place one user's listening influences another's
/// recommendations, and it carries no identity: the graph stores a pair of
/// track ids and a weight, never who played them.
async fn link_previous(pool: &PgPool, listen: &MusicListen, previous: Option<Uuid>) {
    if listen.skipped {
        return;
    }
    let previous = match previous {
        Some(id) => Some(id),
        // The client usually knows; fall back to the log when it does not.
        None => match repo::recent_listens(pool, listen.user_id, 2).await {
            Ok(rows) => rows
                .into_iter()
                .find(|r| r.id != listen.id && r.media_id != listen.media_id)
                .map(|r| r.media_id),
            Err(_) => None,
        },
    };

    let Some(previous) = previous.filter(|id| *id != listen.media_id) else {
        return;
    };

    // A pair is worth more when the second track was actually finished.
    let weight = if listen.completed { 1.0 } else { 0.5 };
    if let Err(e) = repo::bump_cooccurrence(pool, previous, listen.media_id, weight).await {
        tracing::warn!(error = %e, "music co-occurrence update failed");
    }
}

/// Attribute a listen across every dimension we can derive for the track.
/// Failures here are logged and swallowed for the same reason as above.
async fn apply_to_profile(pool: &PgPool, user_id: Uuid, listen: &MusicListen) {
    // A full play is worth up to 1.5; a skip costs 0.5; partial plays scale.
    let delta = if listen.skipped {
        -0.5
    } else {
        let f = listen.percent_played.map(|p| p / 100.0).unwrap_or(0.5);
        (f.max(LISTEN_THRESHOLD) * if listen.completed { 1.5 } else { 1.0 }).min(1.5)
    };

    let mut dimensions: Vec<(&str, String)> = Vec::new();

    for genre in repo::genres_for(pool, listen.media_id)
        .await
        .unwrap_or_default()
    {
        dimensions.push(("genre", genre));
    }
    for artist in repo::artists_for(pool, listen.media_id)
        .await
        .unwrap_or_default()
    {
        dimensions.push(("artist", artist));
    }

    // Tempo, energy and key come from the analyzer, so they only contribute
    // once a track has been analyzed.
    if let Ok(Some(a)) = repo::get_analysis(pool, listen.media_id).await {
        if let Some(bpm) = a.tempo_bpm {
            dimensions.push(("tempo_band", tempo_band(bpm).to_string()));
        }
        if let Some(energy) = a.energy {
            dimensions.push(("energy_band", energy_band(energy).to_string()));
        }
        if let Some(name) = a.key_name() {
            dimensions.push(("key", name));
        }
        if let Some(d) = a.danceability {
            dimensions.push(("mood", if d > 0.6 { "danceable" } else { "listening" }.to_string()));
        }
    }

    for (dimension, key) in dimensions {
        if let Err(e) = repo::bump_affinity(pool, user_id, dimension, &key, delta, listen.skipped)
            .await
        {
            tracing::warn!(error = %e, dimension, key, "music affinity update failed");
        }
    }

    // Context is where the play came from, so a skip there is not evidence
    // against the context itself — only positive deltas apply.
    if let Some(ctx) = &listen.context {
        if delta > 0.0 {
            let _ = repo::bump_affinity(pool, user_id, "context", ctx, delta, false).await;
        }
    }
}

/// The learned profile, decayed to now.
pub async fn taste(pool: &PgPool, user_id: Uuid) -> ApiResult<MusicTaste> {
    let (total_listens, distinct_tracks, total_hours, completion_rate, skip_rate) =
        repo::listen_totals(pool, user_id).await?;

    Ok(MusicTaste {
        total_listens,
        distinct_tracks,
        total_hours,
        completion_rate,
        skip_rate,
        top_genres: repo::top_affinity(pool, user_id, "genre", 8, HALF_LIFE_DAYS).await?,
        top_artists: repo::top_affinity(pool, user_id, "artist", 8, HALF_LIFE_DAYS).await?,
        top_keys: repo::top_affinity(pool, user_id, "key", 5, HALF_LIFE_DAYS).await?,
        tempo_profile: repo::top_affinity(pool, user_id, "tempo_band", 4, HALF_LIFE_DAYS).await?,
        energy_profile: repo::top_affinity(pool, user_id, "energy_band", 3, HALF_LIFE_DAYS).await?,
        peak_hours: repo::peak_hours(pool, user_id).await?,
        cold_start: total_listens < COLD_START_LISTENS,
    })
}

/// How much each signal counts, given how much history the listener has.
struct Blend {
    taste: f64,
    collaborative: f64,
    similarity: f64,
    global: f64,
}

impl Blend {
    /// Confidence ramps from 0 at the first listen to 1 at `COLD_START_LISTENS`.
    /// A new listener is recommended what is objectively good; an established
    /// one is recommended what suits them.
    fn for_history(listens: i64) -> Self {
        let c = (listens as f64 / COLD_START_LISTENS as f64).clamp(0.0, 1.0);
        Self {
            taste: 1.0 * c,
            collaborative: 1.2 * c,
            similarity: 0.9 * c,
            global: 1.0 - 0.6 * c,
        }
    }
}

/// A candidate accumulating score from several signals at once.
struct Scored {
    candidate: Candidate,
    score: f64,
    reasons: Vec<String>,
    /// Signal that contributed the most, for the UI to label the row.
    basis: &'static str,
    basis_score: f64,
}

impl Scored {
    fn new(candidate: Candidate) -> Self {
        Self {
            candidate,
            score: 0.0,
            reasons: Vec::new(),
            basis: "trending",
            basis_score: 0.0,
        }
    }

    fn add(&mut self, basis: &'static str, amount: f64, reason: Option<String>) {
        if amount <= 0.0 {
            return;
        }
        self.score += amount;
        if amount > self.basis_score {
            self.basis_score = amount;
            self.basis = basis;
        }
        if let Some(r) = reason {
            if !self.reasons.contains(&r) {
                self.reasons.push(r);
            }
        }
    }
}

/// Personalized picks, blended across four signals.
pub async fn recommendations(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> ApiResult<Vec<MusicRecommendation>> {
    let limit = limit.clamp(1, 100);
    let (total_listens, ..) = repo::listen_totals(pool, user_id).await?;
    let blend = Blend::for_history(total_listens);

    let exclude = repo::recently_played(pool, user_id).await?;
    let seeds = repo::seed_tracks(pool, user_id, SEED_COUNT).await?;

    let genre_weights = repo::genre_weights(pool, user_id, HALF_LIFE_DAYS).await?;
    let max_genre = genre_weights.values().cloned().fold(0.0f64, f64::max).max(1e-6);

    let tempo_rows = repo::top_affinity(pool, user_id, "tempo_band", 1, HALF_LIFE_DAYS).await?;
    let energy_rows = repo::top_affinity(pool, user_id, "energy_band", 1, HALF_LIFE_DAYS).await?;
    let target_tempo = tempo_rows.first().map(|r| band_to_bpm(&r.key)).unwrap_or(110.0);
    let target_energy = energy_rows.first().map(|r| band_to_energy(&r.key)).unwrap_or(0.5);

    // Over-fetch: the three pools overlap, and the merge below is what decides
    // the final order.
    let pool_size = (limit * 4).clamp(40, 300);
    let mut merged: HashMap<Uuid, Scored> = HashMap::new();

    let mut absorb = |list: Vec<Candidate>, merged: &mut HashMap<Uuid, Scored>| {
        for candidate in list {
            merged
                .entry(candidate.media_id)
                .or_insert_with(|| Scored::new(candidate));
        }
    };

    absorb(
        repo::candidates_by_taste(pool, &exclude, pool_size).await?,
        &mut merged,
    );

    let collaborative =
        repo::candidates_by_neighbours(pool, &seeds, &exclude, pool_size, true).await?;
    let max_cooc = collaborative.iter().map(|(_, w)| *w).fold(0.0f64, f64::max).max(1e-6);
    for (candidate, weight) in collaborative {
        let id = candidate.media_id;
        let entry = merged.entry(id).or_insert_with(|| Scored::new(candidate));
        entry.add(
            "collaborative",
            blend.collaborative * (weight / max_cooc),
            Some("listeners who play what you play also play this".into()),
        );
    }

    let similar = repo::candidates_by_neighbours(pool, &seeds, &exclude, pool_size, false).await?;
    for (candidate, similarity) in similar {
        let id = candidate.media_id;
        let entry = merged.entry(id).or_insert_with(|| Scored::new(candidate));
        // Cosine over normalized vectors is in -1..1; only real likeness counts.
        entry.add(
            "similar",
            blend.similarity * ((similarity - 0.5).max(0.0) * 2.0),
            Some("sounds like the tracks you finish".into()),
        );
    }

    // Score the pool. Taste and global quality apply to every candidate,
    // whichever way it got here.
    for scored in merged.values_mut() {
        score_taste(scored, &genre_weights, max_genre, target_tempo, target_energy, &blend);
        score_global(scored, &blend);
    }

    let mut ranked: Vec<Scored> = merged.into_values().collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    ranked.truncate(limit as usize);

    Ok(ranked.into_iter().map(into_recommendation).collect())
}

fn score_taste(
    scored: &mut Scored,
    genre_weights: &HashMap<String, f64>,
    max_genre: f64,
    target_tempo: f64,
    target_energy: f64,
    blend: &Blend,
) {
    let best_genre = scored
        .candidate
        .genres
        .iter()
        .filter_map(|g| genre_weights.get(g).map(|w| (g.clone(), *w)))
        .max_by(|a, b| a.1.total_cmp(&b.1));

    if let Some((genre, weight)) = best_genre {
        scored.add(
            "taste",
            blend.taste * (weight / max_genre),
            Some(format!("matches your taste for {genre}")),
        );
    }

    // Tempo and energy proximity: a bonus that decays to zero at the edge of
    // the band rather than a hard filter, so neighbours still surface.
    if let Some(bpm) = scored.candidate.tempo_bpm {
        let closeness = 1.0 - ((bpm - target_tempo).abs() / 60.0).clamp(0.0, 1.0);
        scored.add(
            "taste",
            blend.taste * 0.5 * closeness,
            (closeness > 0.75).then(|| format!("around {bpm:.0} BPM, like what you play")),
        );
    }
    if let Some(energy) = scored.candidate.energy {
        let closeness = 1.0 - (energy - target_energy).abs().clamp(0.0, 1.0);
        scored.add("taste", blend.taste * 0.35 * closeness, None);
    }
}

fn score_global(scored: &mut Scored, blend: &Blend) {
    let quality = scored.candidate.quality;
    let plays = scored.candidate.plays;

    scored.add(
        "trending",
        blend.global * quality,
        (quality > 0.6 && plays >= 20)
            .then(|| format!("{:.0}% of listeners play it through", quality * 100.0)),
    );

    // Trend is a ratio around 1.0; only genuine acceleration scores.
    let momentum = ((scored.candidate.trend - 1.0) / 3.0).clamp(0.0, 1.0);
    scored.add(
        "trending",
        blend.global * 0.6 * momentum,
        (momentum > 0.3).then(|| "picking up across the platform".to_string()),
    );

    // A small, saturating popularity term so an empty corpus still orders.
    let popularity = (scored.candidate.popularity as f64 / 1000.0).clamp(0.0, 0.3);
    scored.add("trending", popularity, None);
}

fn into_recommendation(scored: Scored) -> MusicRecommendation {
    let Scored {
        candidate,
        score,
        mut reasons,
        basis,
        ..
    } = scored;

    if reasons.is_empty() {
        reasons.push("popular and new to you".into());
    }
    reasons.truncate(3);

    let key_name = candidate.musical_key.and_then(|k| {
        let name = KEY_NAMES.get(k as usize)?;
        let mode = if candidate.musical_mode == Some(1) {
            "major"
        } else {
            "minor"
        };
        Some(format!("{name} {mode}"))
    });

    MusicRecommendation {
        media_id: candidate.media_id,
        title: candidate.title,
        poster_url: candidate.poster_url,
        artist: candidate.artist,
        score,
        tempo_bpm: candidate.tempo_bpm,
        energy: candidate.energy,
        danceability: candidate.danceability,
        valence: candidate.valence,
        key_name,
        basis: basis.to_string(),
        reasons,
    }
}

fn band_to_bpm(band: &str) -> f64 {
    match band {
        "slow" => 70.0,
        "mid" => 95.0,
        "up" => 125.0,
        "fast" => 160.0,
        _ => 110.0,
    }
}

fn band_to_energy(band: &str) -> f64 {
    match band {
        "calm" => 0.2,
        "steady" => 0.5,
        "intense" => 0.85,
        _ => 0.5,
    }
}

/// Tracks that sound like a given one, straight from the content graph.
///
/// No user involved: this is a property of the recordings, so it is the same
/// answer for everybody and works before the track has ever been played.
pub async fn similar_to(
    pool: &PgPool,
    media_id: Uuid,
    limit: i64,
) -> ApiResult<Vec<MusicRecommendation>> {
    let seeds = [media_id];
    let neighbours =
        repo::candidates_by_neighbours(pool, &seeds, &seeds, limit, false).await?;

    Ok(neighbours
        .into_iter()
        .map(|(candidate, similarity)| {
            let mut scored = Scored::new(candidate);
            scored.add(
                "similar",
                similarity,
                Some(describe_similarity(similarity)),
            );
            into_recommendation(scored)
        })
        .collect())
}

fn describe_similarity(similarity: f64) -> String {
    match similarity {
        s if s > 0.95 => "almost the same sound".into(),
        s if s > 0.85 => "very close in tone and tempo".into(),
        s if s > 0.7 => "a similar feel".into(),
        _ => "loosely related".into(),
    }
}

/// The global charts: what the whole audience plays through, or what is
/// climbing this week. The cold-start answer, and a product surface in its
/// own right.
pub async fn charts(
    pool: &PgPool,
    limit: i64,
    trending: bool,
) -> ApiResult<Vec<MusicRecommendation>> {
    let stats = repo::global_top(pool, limit, trending).await?;
    if stats.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<Uuid> = stats.iter().map(|s| s.media_id).collect();
    let candidates = repo::candidates_by_ids(pool, &ids).await?;
    let mut by_id: HashMap<Uuid, Candidate> =
        candidates.into_iter().map(|c| (c.media_id, c)).collect();

    // Preserve the chart order rather than the lookup order.
    Ok(stats
        .into_iter()
        .filter_map(|stat| {
            let candidate = by_id.remove(&stat.media_id)?;
            let mut scored = Scored::new(candidate);
            let reason = if trending {
                format!("{:.1}x the plays of last week", stat.trend)
            } else {
                format!(
                    "{:.0}% of {} listeners play it through",
                    stat.completion_rate * 100.0,
                    stat.unique_listeners
                )
            };
            scored.add(
                "trending",
                if trending { stat.trend } else { stat.quality },
                Some(reason),
            );
            Some(into_recommendation(scored))
        })
        .collect())
}
