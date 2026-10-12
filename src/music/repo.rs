use std::collections::HashMap;

use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::music::models::{
    wilson_lower_bound, AffinityRow, DatasetRow, MusicAnalysis, MusicListen, TrackStats,
};

/// Every analysis column, in one place, so the SELECT and the struct cannot
/// drift apart silently.
const ANALYSIS_COLS: &str = "media_id, file_id, duration_seconds, sample_rate, channels, \
     bitrate_kbps, codec, artist, album, album_artist, track_no, disc_no, release_year, \
     tag_genre, tags, loudness_lufs, peak_db, dynamic_range, energy, waveform, rms_envelope, \
     waveform_buckets, spectral_centroid, spectral_rolloff, spectral_flatness, \
     spectral_bandwidth, spectral_flux, zero_crossing_rate, brightness, chroma, musical_key, \
     musical_mode, key_confidence, tempo_bpm, tempo_confidence, onset_rate, beat_grid, \
     danceability, valence, acousticness, instrumentalness, speechiness, liveness, timbre, \
     embedding, segments, analysis_version, analysis_ms, analyzed_at, last_error";

// ============================================================== analysis

pub async fn get_analysis(pool: &PgPool, media_id: Uuid) -> ApiResult<Option<MusicAnalysis>> {
    let row = sqlx::query_as::<_, MusicAnalysis>(&format!(
        "SELECT {ANALYSIS_COLS} FROM music_analysis WHERE media_id = $1"
    ))
    .bind(media_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Claim music that has never been analyzed, or was analyzed by an older
/// version of the analyzer. The insert doubles as the claim for new rows, so
/// two workers cannot pick up the same track.
pub async fn claim_unanalyzed(
    pool: &PgPool,
    limit: i64,
    current_version: i32,
) -> ApiResult<Vec<Uuid>> {
    let fresh: Vec<(Uuid,)> = sqlx::query_as(
        "INSERT INTO music_analysis (media_id)
         SELECT m.id FROM media m
         WHERE m.media_type = 'music'
           AND NOT EXISTS (SELECT 1 FROM music_analysis a WHERE a.media_id = m.id)
         LIMIT $1
         ON CONFLICT (media_id) DO NOTHING
         RETURNING media_id",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut ids: Vec<Uuid> = fresh.into_iter().map(|(id,)| id).collect();
    let remaining = limit - ids.len() as i64;
    if remaining <= 0 {
        return Ok(ids);
    }

    // Backfill: rows from an older analyzer, oldest first. `FOR UPDATE
    // SKIP LOCKED` is not enough on its own here because the update is what
    // marks the claim, so the version bump happens in the same statement.
    let stale: Vec<(Uuid,)> = sqlx::query_as(
        "UPDATE music_analysis SET analysis_version = $2, updated_at = now()
         WHERE media_id IN (
             SELECT media_id FROM music_analysis
             WHERE analysis_version < $2
             ORDER BY analyzed_at NULLS FIRST
             LIMIT $1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING media_id",
    )
    .bind(remaining)
    .bind(current_version)
    .fetch_all(pool)
    .await?;

    ids.extend(stale.into_iter().map(|(id,)| id));
    Ok(ids)
}

pub async fn save_analysis(pool: &PgPool, a: &MusicAnalysis) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO music_analysis (
            media_id, file_id, duration_seconds, sample_rate, channels, bitrate_kbps, codec,
            artist, album, album_artist, track_no, disc_no, release_year, tag_genre, tags,
            loudness_lufs, peak_db, dynamic_range, energy, waveform, rms_envelope,
            waveform_buckets, spectral_centroid, spectral_rolloff, spectral_flatness,
            spectral_bandwidth, spectral_flux, zero_crossing_rate, brightness, chroma,
            musical_key, musical_mode, key_confidence, tempo_bpm, tempo_confidence, onset_rate,
            beat_grid, danceability, valence, acousticness, instrumentalness, speechiness,
            liveness, timbre, embedding, segments, analysis_version, analysis_ms, last_error,
            analyzed_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
                 $21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,$35,$36,$37,$38,
                 $39,$40,$41,$42,$43,$44,$45,$46,$47,$48,$49, now(), now())
         ON CONFLICT (media_id) DO UPDATE SET
            file_id = EXCLUDED.file_id,
            duration_seconds = EXCLUDED.duration_seconds,
            sample_rate = EXCLUDED.sample_rate,
            channels = EXCLUDED.channels,
            bitrate_kbps = EXCLUDED.bitrate_kbps,
            codec = EXCLUDED.codec,
            artist = EXCLUDED.artist,
            album = EXCLUDED.album,
            album_artist = EXCLUDED.album_artist,
            track_no = EXCLUDED.track_no,
            disc_no = EXCLUDED.disc_no,
            release_year = EXCLUDED.release_year,
            tag_genre = EXCLUDED.tag_genre,
            tags = EXCLUDED.tags,
            loudness_lufs = EXCLUDED.loudness_lufs,
            peak_db = EXCLUDED.peak_db,
            dynamic_range = EXCLUDED.dynamic_range,
            energy = EXCLUDED.energy,
            waveform = EXCLUDED.waveform,
            rms_envelope = EXCLUDED.rms_envelope,
            waveform_buckets = EXCLUDED.waveform_buckets,
            spectral_centroid = EXCLUDED.spectral_centroid,
            spectral_rolloff = EXCLUDED.spectral_rolloff,
            spectral_flatness = EXCLUDED.spectral_flatness,
            spectral_bandwidth = EXCLUDED.spectral_bandwidth,
            spectral_flux = EXCLUDED.spectral_flux,
            zero_crossing_rate = EXCLUDED.zero_crossing_rate,
            brightness = EXCLUDED.brightness,
            chroma = EXCLUDED.chroma,
            musical_key = EXCLUDED.musical_key,
            musical_mode = EXCLUDED.musical_mode,
            key_confidence = EXCLUDED.key_confidence,
            tempo_bpm = EXCLUDED.tempo_bpm,
            tempo_confidence = EXCLUDED.tempo_confidence,
            onset_rate = EXCLUDED.onset_rate,
            beat_grid = EXCLUDED.beat_grid,
            danceability = EXCLUDED.danceability,
            valence = EXCLUDED.valence,
            acousticness = EXCLUDED.acousticness,
            instrumentalness = EXCLUDED.instrumentalness,
            speechiness = EXCLUDED.speechiness,
            liveness = EXCLUDED.liveness,
            timbre = EXCLUDED.timbre,
            embedding = EXCLUDED.embedding,
            segments = EXCLUDED.segments,
            analysis_version = EXCLUDED.analysis_version,
            analysis_ms = EXCLUDED.analysis_ms,
            last_error = EXCLUDED.last_error,
            analyzed_at = now(),
            updated_at = now()",
    )
    .bind(a.media_id)
    .bind(a.file_id)
    .bind(a.duration_seconds)
    .bind(a.sample_rate)
    .bind(a.channels)
    .bind(a.bitrate_kbps)
    .bind(&a.codec)
    .bind(&a.artist)
    .bind(&a.album)
    .bind(&a.album_artist)
    .bind(a.track_no)
    .bind(a.disc_no)
    .bind(a.release_year)
    .bind(&a.tag_genre)
    .bind(&a.tags)
    .bind(a.loudness_lufs)
    .bind(a.peak_db)
    .bind(a.dynamic_range)
    .bind(a.energy)
    .bind(&a.waveform)
    .bind(&a.rms_envelope)
    .bind(a.waveform_buckets)
    .bind(a.spectral_centroid)
    .bind(a.spectral_rolloff)
    .bind(a.spectral_flatness)
    .bind(a.spectral_bandwidth)
    .bind(a.spectral_flux)
    .bind(a.zero_crossing_rate)
    .bind(a.brightness)
    .bind(&a.chroma)
    .bind(a.musical_key)
    .bind(a.musical_mode)
    .bind(a.key_confidence)
    .bind(a.tempo_bpm)
    .bind(a.tempo_confidence)
    .bind(a.onset_rate)
    .bind(&a.beat_grid)
    .bind(a.danceability)
    .bind(a.valence)
    .bind(a.acousticness)
    .bind(a.instrumentalness)
    .bind(a.speechiness)
    .bind(a.liveness)
    .bind(&a.timbre)
    .bind(&a.embedding)
    .bind(&a.segments)
    .bind(a.analysis_version)
    .bind(a.analysis_ms)
    .bind(&a.last_error)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_failed(pool: &PgPool, media_id: Uuid, err: &str) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO music_analysis (media_id, last_error)
         VALUES ($1, $2)
         ON CONFLICT (media_id) DO UPDATE
            SET last_error = EXCLUDED.last_error, updated_at = now()",
    )
    .bind(media_id)
    .bind(err)
    .execute(pool)
    .await?;
    Ok(())
}

/// Recompute content-based neighbours for one track against every other
/// analyzed track. Called after analysis, so a new upload is similar-searchable
/// before anybody has played it.
pub async fn refresh_similarity(pool: &PgPool, media_id: Uuid, top_n: i64) -> ApiResult<usize> {
    let Some(row) = sqlx::query(
        "SELECT embedding FROM music_analysis WHERE media_id = $1 AND embedding IS NOT NULL",
    )
    .bind(media_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(0);
    };
    let mine: serde_json::Value = row.try_get("embedding")?;

    let others = sqlx::query(
        "SELECT media_id, embedding FROM music_analysis
         WHERE embedding IS NOT NULL AND media_id <> $1",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await?;

    let mut scored: Vec<(Uuid, f64)> = Vec::with_capacity(others.len());
    for row in &others {
        let other_id: Uuid = row.try_get("media_id")?;
        let other: serde_json::Value = row.try_get("embedding")?;
        if let Some(sim) = crate::music::analysis::cosine(&mine, &other) {
            scored.push((other_id, sim));
        }
    }

    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(top_n.clamp(1, 200) as usize);

    // Replace this track's neighbour list wholesale; a partial update would
    // leave stale pairs behind when the analyzer changes.
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM music_similarity WHERE media_a = $1")
        .bind(media_id)
        .execute(&mut *tx)
        .await?;

    for (other_id, sim) in &scored {
        // Both directions, so a lookup from either side is one index scan.
        sqlx::query(
            "INSERT INTO music_similarity (media_a, media_b, similarity, computed_at)
             VALUES ($1,$2,$3,now()), ($2,$1,$3,now())
             ON CONFLICT (media_a, media_b) DO UPDATE
                SET similarity = EXCLUDED.similarity, computed_at = now()",
        )
        .bind(media_id)
        .bind(other_id)
        .bind(sim)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(scored.len())
}

// =============================================================== listens

pub async fn insert_listen(pool: &PgPool, l: &MusicListen) -> ApiResult<()> {
    use chrono::{Datelike, Timelike};
    sqlx::query(
        "INSERT INTO music_listens (
            id, user_id, media_id, started_at, seconds_played, track_seconds,
            percent_played, completed, skipped, source, device, context,
            hour_of_day, weekday)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(l.id)
    .bind(l.user_id)
    .bind(l.media_id)
    .bind(l.started_at)
    .bind(l.seconds_played)
    .bind(l.track_seconds)
    .bind(l.percent_played)
    .bind(l.completed)
    .bind(l.skipped)
    .bind(&l.source)
    .bind(&l.device)
    .bind(&l.context)
    .bind(l.started_at.hour() as i16)
    .bind(l.started_at.weekday().num_days_from_monday() as i16)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn recent_listens(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> ApiResult<Vec<MusicListen>> {
    let rows = sqlx::query_as::<_, MusicListen>(
        "SELECT id, user_id, media_id, started_at, seconds_played, track_seconds,
                percent_played, completed, skipped, source, device, context
         FROM music_listens WHERE user_id = $1 ORDER BY started_at DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// (total listens, distinct tracks, hours, completion %, skip %)
pub async fn listen_totals(pool: &PgPool, user_id: Uuid) -> ApiResult<(i64, i64, f64, f64, f64)> {
    let row: (i64, i64, Option<f64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT COUNT(*),
                COUNT(DISTINCT media_id),
                SUM(seconds_played),
                COUNT(*) FILTER (WHERE completed),
                COUNT(*) FILTER (WHERE skipped)
         FROM music_listens WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;

    let total = row.0.max(0);
    let hours = row.2.unwrap_or(0.0) / 3600.0;
    let denom = if total > 0 { total as f64 } else { 1.0 };
    Ok((
        total,
        row.1,
        hours,
        row.3.unwrap_or(0) as f64 / denom * 100.0,
        row.4.unwrap_or(0) as f64 / denom * 100.0,
    ))
}

pub async fn peak_hours(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<i16>> {
    let rows: Vec<(Option<i16>,)> = sqlx::query_as(
        "SELECT hour_of_day FROM music_listens
         WHERE user_id = $1 AND hour_of_day IS NOT NULL
         GROUP BY hour_of_day ORDER BY COUNT(*) DESC LIMIT 4",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().filter_map(|(h,)| h).collect())
}

/// The user's strongest recent tracks — the seeds for collaborative and
/// content-based candidate generation.
pub async fn seed_tracks(pool: &PgPool, user_id: Uuid, limit: i64) -> ApiResult<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT media_id FROM music_listens
         WHERE user_id = $1 AND NOT skipped AND started_at > now() - INTERVAL '60 days'
         GROUP BY media_id
         ORDER BY SUM(COALESCE(percent_played, 50)) DESC
         LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Tracks to keep out of recommendations: played in the last fortnight.
pub async fn recently_played(pool: &PgPool, user_id: Uuid) -> ApiResult<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT DISTINCT media_id FROM music_listens
         WHERE user_id = $1 AND started_at > now() - INTERVAL '14 days'",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

// ================================================= global track statistics

/// Fold one listen into the global, cross-user stats for its track.
///
/// Runs as a single statement per table so a burst of concurrent listens
/// serializes on the row lock rather than on a read-modify-write in Rust.
pub async fn record_global_play(pool: &PgPool, l: &MusicListen) -> ApiResult<()> {
    // The listener row decides whether this user is new to the track. xmax = 0
    // on the returned row means the INSERT won rather than the DO UPDATE.
    let is_new_listener: bool = sqlx::query_scalar(
        "INSERT INTO music_track_listeners
            (media_id, user_id, plays, seconds_played, first_played_at, last_played_at)
         VALUES ($1, $2, 1, $3, now(), now())
         ON CONFLICT (media_id, user_id) DO UPDATE SET
            plays = music_track_listeners.plays + 1,
            seconds_played = music_track_listeners.seconds_played + EXCLUDED.seconds_played,
            last_played_at = now()
         RETURNING (xmax = 0)",
    )
    .bind(l.media_id)
    .bind(l.user_id)
    .bind(l.seconds_played)
    .fetch_one(pool)
    .await?;

    let percent = l.percent_played.unwrap_or(0.0);

    sqlx::query(
        "INSERT INTO music_track_stats (
            media_id, plays, completions, skips, unique_listeners, seconds_played,
            avg_percent, first_played_at, last_played_at, updated_at)
         VALUES ($1, 1, $2, $3, $4, $5, $6, now(), now(), now())
         ON CONFLICT (media_id) DO UPDATE SET
            plays = music_track_stats.plays + 1,
            completions = music_track_stats.completions + EXCLUDED.completions,
            skips = music_track_stats.skips + EXCLUDED.skips,
            unique_listeners = music_track_stats.unique_listeners + EXCLUDED.unique_listeners,
            seconds_played = music_track_stats.seconds_played + EXCLUDED.seconds_played,
            -- running mean, so no history scan is needed
            avg_percent = (music_track_stats.avg_percent * music_track_stats.plays
                           + EXCLUDED.avg_percent) / (music_track_stats.plays + 1),
            last_played_at = now(),
            updated_at = now()",
    )
    .bind(l.media_id)
    .bind(i64::from(l.completed))
    .bind(i64::from(l.skipped))
    .bind(i64::from(is_new_listener))
    .bind(l.seconds_played)
    .bind(percent)
    .execute(pool)
    .await?;

    refresh_derived_stats(pool, l.media_id).await
}

/// Recompute the rates, the Wilson quality score and the 7-day trend for one
/// track. Cheap enough to run per play: two indexed counts and some arithmetic.
async fn refresh_derived_stats(pool: &PgPool, media_id: Uuid) -> ApiResult<()> {
    let row: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT plays, completions, skips FROM music_track_stats WHERE media_id = $1",
    )
    .bind(media_id)
    .fetch_optional(pool)
    .await?;
    let Some((plays, completions, skips)) = row else {
        return Ok(());
    };

    let denom = plays.max(1) as f64;
    let quality = wilson_lower_bound(completions, plays);

    let (recent, prior): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*) FILTER (WHERE started_at > now() - INTERVAL '7 days'),
                COUNT(*) FILTER (WHERE started_at > now() - INTERVAL '14 days'
                                   AND started_at <= now() - INTERVAL '7 days')
         FROM music_listens WHERE media_id = $1",
    )
    .bind(media_id)
    .fetch_one(pool)
    .await?;

    // Ratio of this week to last, with a +1 on both sides so a track going
    // from 0 to 3 plays does not report an infinite trend.
    let trend = (recent + 1) as f64 / (prior + 1) as f64;

    sqlx::query(
        "UPDATE music_track_stats
         SET completion_rate = $2, skip_rate = $3, quality = $4, trend = $5, updated_at = now()
         WHERE media_id = $1",
    )
    .bind(media_id)
    .bind(completions as f64 / denom)
    .bind(skips as f64 / denom)
    .bind(quality)
    .bind(trend)
    .execute(pool)
    .await?;
    Ok(())
}

/// Strengthen the "played together" edge between two tracks.
///
/// Symmetric by construction, and capped: a single user looping two tracks
/// cannot dominate the graph, because the weight saturates logarithmically.
pub async fn bump_cooccurrence(
    pool: &PgPool,
    a: Uuid,
    b: Uuid,
    weight: f64,
) -> ApiResult<()> {
    if a == b {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO music_cooccurrence (media_a, media_b, weight, pairs, updated_at)
         VALUES ($1,$2,$3,1,now()), ($2,$1,$3,1,now())
         ON CONFLICT (media_a, media_b) DO UPDATE SET
            -- diminishing returns: the 100th co-play adds far less than the 2nd
            weight = music_cooccurrence.weight
                     + EXCLUDED.weight / (1.0 + LN(1.0 + music_cooccurrence.pairs)),
            pairs = music_cooccurrence.pairs + 1,
            updated_at = now()",
    )
    .bind(a)
    .bind(b)
    .bind(weight)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn track_stats(pool: &PgPool, media_id: Uuid) -> ApiResult<Option<TrackStats>> {
    let row = sqlx::query_as::<_, TrackStats>(
        "SELECT media_id, plays, completions, skips, unique_listeners, seconds_played,
                avg_percent, completion_rate, skip_rate, quality, trend, last_played_at
         FROM music_track_stats WHERE media_id = $1",
    )
    .bind(media_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// The global charts. No user involved — this is what everybody sees, and what
/// a cold-start listener gets instead of a bad personalized guess.
pub async fn global_top(pool: &PgPool, limit: i64, trending: bool) -> ApiResult<Vec<TrackStats>> {
    let order = if trending {
        "trend DESC, plays DESC"
    } else {
        "quality DESC, plays DESC"
    };
    let rows = sqlx::query_as::<_, TrackStats>(&format!(
        "SELECT media_id, plays, completions, skips, unique_listeners, seconds_played,
                avg_percent, completion_rate, skip_rate, quality, trend, last_played_at
         FROM music_track_stats
         WHERE plays > 0
         ORDER BY {order}
         LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ============================================================== affinity

/// Add `delta` to one taste dimension. Negative deltas (skips) are how the
/// profile un-learns a bad guess; weight is floored at zero.
pub async fn bump_affinity(
    pool: &PgPool,
    user_id: Uuid,
    dimension: &str,
    key: &str,
    delta: f64,
    skipped: bool,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO music_affinity (user_id, dimension, key, weight, plays, skips, last_seen)
         VALUES ($1, $2, $3, $4, $5, $6, now())
         ON CONFLICT (user_id, dimension, key) DO UPDATE SET
            weight = GREATEST(0, music_affinity.weight + EXCLUDED.weight),
            plays  = music_affinity.plays + EXCLUDED.plays,
            skips  = music_affinity.skips + EXCLUDED.skips,
            last_seen = now()",
    )
    .bind(user_id)
    .bind(dimension)
    .bind(key)
    .bind(delta)
    .bind(if skipped { 0i32 } else { 1i32 })
    .bind(if skipped { 1i32 } else { 0i32 })
    .execute(pool)
    .await?;
    Ok(())
}

/// Read one dimension with exponential time decay applied in SQL, so stale
/// interests fade without a sweep job.
pub async fn top_affinity(
    pool: &PgPool,
    user_id: Uuid,
    dimension: &str,
    limit: i64,
    half_life_days: f64,
) -> ApiResult<Vec<AffinityRow>> {
    let rows = sqlx::query_as::<_, AffinityRow>(
        "SELECT dimension, key,
                weight * POWER(0.5, EXTRACT(EPOCH FROM (now() - last_seen)) / ($4 * 86400.0))
                    AS weight,
                plays, skips, last_seen
         FROM music_affinity
         WHERE user_id = $1 AND dimension = $2
         ORDER BY weight DESC
         LIMIT $3",
    )
    .bind(user_id)
    .bind(dimension)
    .bind(limit)
    .bind(half_life_days)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Genre affinity as a lookup, decayed the same way `top_affinity` decays it.
pub async fn genre_weights(
    pool: &PgPool,
    user_id: Uuid,
    half_life_days: f64,
) -> ApiResult<HashMap<String, f64>> {
    let rows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT key,
                weight * POWER(0.5, EXTRACT(EPOCH FROM (now() - last_seen)) / ($2 * 86400.0))
         FROM music_affinity
         WHERE user_id = $1 AND dimension = 'genre'",
    )
    .bind(user_id)
    .bind(half_life_days)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

// ================================================ candidate generation

/// One candidate track with everything needed to score and explain it.
pub struct Candidate {
    pub media_id: Uuid,
    pub title: String,
    pub poster_url: Option<String>,
    pub artist: Option<String>,
    pub tempo_bpm: Option<f64>,
    pub energy: Option<f64>,
    pub danceability: Option<f64>,
    pub valence: Option<f64>,
    pub musical_key: Option<i16>,
    pub musical_mode: Option<i16>,
    pub genres: Vec<String>,
    pub popularity: i32,
    pub quality: f64,
    pub trend: f64,
    pub plays: i64,
}

const CANDIDATE_SELECT: &str = "SELECT m.id, m.title, m.poster_url, m.popularity,
        a.artist, a.tempo_bpm, a.energy, a.danceability, a.valence,
        a.musical_key, a.musical_mode,
        COALESCE(s.quality, 0) AS quality,
        COALESCE(s.trend, 0) AS trend,
        COALESCE(s.plays, 0) AS plays,
        COALESCE(ARRAY_AGG(g.name) FILTER (WHERE g.name IS NOT NULL), '{}') AS genres
     FROM media m
     JOIN music_analysis a ON a.media_id = m.id AND a.analyzed_at IS NOT NULL
     LEFT JOIN music_track_stats s ON s.media_id = m.id
     LEFT JOIN media_genres mg ON mg.media_id = m.id
     LEFT JOIN genres g ON g.id = mg.genre_id";

const CANDIDATE_GROUP: &str = "GROUP BY m.id, m.title, m.poster_url, m.popularity, a.artist,
        a.tempo_bpm, a.energy, a.danceability, a.valence, a.musical_key, a.musical_mode,
        s.quality, s.trend, s.plays";

fn read_candidate(row: &sqlx::postgres::PgRow) -> ApiResult<Candidate> {
    Ok(Candidate {
        media_id: row.try_get("id")?,
        title: row.try_get("title")?,
        poster_url: row.try_get("poster_url")?,
        artist: row.try_get("artist")?,
        tempo_bpm: row.try_get("tempo_bpm")?,
        energy: row.try_get("energy")?,
        danceability: row.try_get("danceability")?,
        valence: row.try_get("valence")?,
        musical_key: row.try_get("musical_key")?,
        musical_mode: row.try_get("musical_mode")?,
        genres: row.try_get("genres")?,
        popularity: row.try_get("popularity")?,
        quality: row.try_get("quality")?,
        trend: row.try_get("trend")?,
        plays: row.try_get("plays")?,
    })
}

/// Tracks the user has never played, ranked by global quality and popularity.
/// The base pool that taste scoring then re-ranks.
pub async fn candidates_by_taste(
    pool: &PgPool,
    exclude: &[Uuid],
    limit: i64,
) -> ApiResult<Vec<Candidate>> {
    let rows = sqlx::query(&format!(
        "{CANDIDATE_SELECT}
         WHERE m.media_type = 'music' AND NOT (m.id = ANY($1))
         {CANDIDATE_GROUP}
         ORDER BY COALESCE(s.quality, 0) DESC, m.popularity DESC
         LIMIT $2"
    ))
    .bind(exclude)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter().map(read_candidate).collect()
}

/// Hydrate a known set of ids into candidates. Used by the charts, where the
/// ordering is decided by `music_track_stats` and this only fills in metadata.
pub async fn candidates_by_ids(pool: &PgPool, ids: &[Uuid]) -> ApiResult<Vec<Candidate>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(&format!(
        "{CANDIDATE_SELECT}
         WHERE m.id = ANY($1)
         {CANDIDATE_GROUP}"
    ))
    .bind(ids)
    .fetch_all(pool)
    .await?;
    rows.iter().map(read_candidate).collect()
}

/// Neighbours of the seed tracks in one of the two relationship graphs.
/// `collaborative` picks the co-play graph; otherwise it is content similarity.
pub async fn candidates_by_neighbours(
    pool: &PgPool,
    seeds: &[Uuid],
    exclude: &[Uuid],
    limit: i64,
    collaborative: bool,
) -> ApiResult<Vec<(Candidate, f64)>> {
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    let (table, weight_col) = if collaborative {
        ("music_cooccurrence", "weight")
    } else {
        ("music_similarity", "similarity")
    };

    let rows = sqlx::query(&format!(
        "WITH neighbours AS (
             SELECT media_b AS id, MAX({weight_col}) AS affinity
             FROM {table}
             WHERE media_a = ANY($1) AND NOT (media_b = ANY($2))
             GROUP BY media_b
             ORDER BY affinity DESC
             LIMIT $3
         )
         {CANDIDATE_SELECT}
         JOIN neighbours n ON n.id = m.id
         WHERE m.media_type = 'music'
         {CANDIDATE_GROUP}, n.affinity
         ORDER BY n.affinity DESC"
    ))
    .bind(seeds)
    .bind(exclude)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            let affinity: f64 = row.try_get("affinity")?;
            Ok((read_candidate(row)?, affinity))
        })
        .collect()
}

// ============================================================== metadata

/// Genres attached to a track, used to attribute a listen to taste dimensions.
pub async fn genres_for(pool: &PgPool, media_id: Uuid) -> ApiResult<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT g.name FROM media_genres mg JOIN genres g ON g.id = mg.genre_id
         WHERE mg.media_id = $1",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(n,)| n).collect())
}

/// Artists for a track: the analyzer's tag first, then any credited people.
/// Without this the artist dimension of the taste profile is never written.
pub async fn artists_for(pool: &PgPool, media_id: Uuid) -> ApiResult<Vec<String>> {
    let tagged: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT artist, album_artist FROM music_analysis WHERE media_id = $1")
            .bind(media_id)
            .fetch_optional(pool)
            .await?;

    let mut names: Vec<String> = Vec::new();
    if let Some((artist, album_artist)) = tagged {
        // Tags routinely pack collaborations into one string.
        for raw in [artist, album_artist].into_iter().flatten() {
            for part in raw.split([',', ';', '/', '&']).chain(raw.split(" feat. ")) {
                let name = part.trim();
                if !name.is_empty() && !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
            }
        }
    }

    if names.is_empty() {
        let credited: Vec<(String,)> = sqlx::query_as(
            "SELECT p.name FROM credits c JOIN people p ON p.id = c.person_id
             WHERE c.media_id = $1 ORDER BY c.sort_order LIMIT 4",
        )
        .bind(media_id)
        .fetch_all(pool)
        .await?;
        names = credited.into_iter().map(|(n,)| n).collect();
    }

    names.truncate(6);
    Ok(names)
}

/// Resolve a track's best audio file to an absolute path on disk, matching the
/// `root_path` + `relative_path` join the streaming and transcode paths use.
pub async fn audio_file_for(pool: &PgPool, media_id: Uuid) -> ApiResult<Option<(Uuid, String)>> {
    let row: Option<(Uuid, Option<String>, String)> = sqlx::query_as(
        "SELECT f.id, lp.root_path, f.relative_path
         FROM media_files f
         LEFT JOIN library_paths lp ON lp.id = f.library_path_id
         WHERE f.media_id = $1
         ORDER BY f.file_size DESC
         LIMIT 1",
    )
    .bind(media_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(id, root, rel)| {
        let full = match root {
            Some(r) => std::path::Path::new(&r)
                .join(&rel)
                .to_string_lossy()
                .into_owned(),
            None => rel,
        };
        (id, full)
    }))
}

// =============================================================== dataset

/// The exportable dataset: every analyzed track, its features, and aggregate
/// engagement. Joined from global tables only — there is no user column to
/// leak, by construction rather than by filtering.
pub async fn dataset(pool: &PgPool, limit: i64, offset: i64) -> ApiResult<Vec<DatasetRow>> {
    let rows = sqlx::query(
        "SELECT m.id, m.title,
                a.artist, a.album, a.release_year, a.duration_seconds,
                a.tempo_bpm, a.tempo_confidence, a.musical_key, a.musical_mode,
                a.key_confidence, a.loudness_lufs, a.dynamic_range, a.energy,
                a.danceability, a.valence, a.acousticness, a.instrumentalness,
                a.speechiness, a.liveness, a.brightness, a.spectral_centroid,
                a.spectral_flatness, a.zero_crossing_rate, a.timbre, a.embedding,
                a.analysis_version,
                COALESCE(s.plays, 0) AS plays,
                COALESCE(s.unique_listeners, 0) AS unique_listeners,
                COALESCE(s.completion_rate, 0) AS completion_rate,
                COALESCE(s.skip_rate, 0) AS skip_rate,
                COALESCE(s.quality, 0) AS quality,
                COALESCE(ARRAY_AGG(g.name) FILTER (WHERE g.name IS NOT NULL), '{}') AS genres
         FROM media m
         JOIN music_analysis a ON a.media_id = m.id AND a.analyzed_at IS NOT NULL
         LEFT JOIN music_track_stats s ON s.media_id = m.id
         LEFT JOIN media_genres mg ON mg.media_id = m.id
         LEFT JOIN genres g ON g.id = mg.genre_id
         GROUP BY m.id, m.title, a.artist, a.album, a.release_year, a.duration_seconds,
                  a.tempo_bpm, a.tempo_confidence, a.musical_key, a.musical_mode,
                  a.key_confidence, a.loudness_lufs, a.dynamic_range, a.energy,
                  a.danceability, a.valence, a.acousticness, a.instrumentalness,
                  a.speechiness, a.liveness, a.brightness, a.spectral_centroid,
                  a.spectral_flatness, a.zero_crossing_rate, a.timbre, a.embedding,
                  a.analysis_version, s.plays, s.unique_listeners, s.completion_rate,
                  s.skip_rate, s.quality
         ORDER BY m.id
         LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(|row| {
            Ok(DatasetRow {
                media_id: row.try_get("id")?,
                title: row.try_get("title")?,
                artist: row.try_get("artist")?,
                album: row.try_get("album")?,
                release_year: row.try_get("release_year")?,
                genres: row.try_get("genres")?,
                duration_seconds: row.try_get("duration_seconds")?,
                tempo_bpm: row.try_get("tempo_bpm")?,
                tempo_confidence: row.try_get("tempo_confidence")?,
                musical_key: row.try_get("musical_key")?,
                musical_mode: row.try_get("musical_mode")?,
                key_confidence: row.try_get("key_confidence")?,
                loudness_lufs: row.try_get("loudness_lufs")?,
                dynamic_range: row.try_get("dynamic_range")?,
                energy: row.try_get("energy")?,
                danceability: row.try_get("danceability")?,
                valence: row.try_get("valence")?,
                acousticness: row.try_get("acousticness")?,
                instrumentalness: row.try_get("instrumentalness")?,
                speechiness: row.try_get("speechiness")?,
                liveness: row.try_get("liveness")?,
                brightness: row.try_get("brightness")?,
                spectral_centroid: row.try_get("spectral_centroid")?,
                spectral_flatness: row.try_get("spectral_flatness")?,
                zero_crossing_rate: row.try_get("zero_crossing_rate")?,
                timbre: row.try_get("timbre")?,
                embedding: row.try_get("embedding")?,
                plays: row.try_get("plays")?,
                unique_listeners: row.try_get("unique_listeners")?,
                completion_rate: row.try_get("completion_rate")?,
                skip_rate: row.try_get("skip_rate")?,
                quality: row.try_get("quality")?,
                analysis_version: row.try_get("analysis_version")?,
            })
        })
        .collect()
}

/// Corpus-level counters for the dataset's cover sheet.
pub async fn dataset_summary(pool: &PgPool) -> ApiResult<(i64, i64, i64, i64, f64)> {
    let row: (i64, i64, i64, i64, Option<f64>) = sqlx::query_as(
        "SELECT
            (SELECT COUNT(*) FROM media WHERE media_type = 'music'),
            (SELECT COUNT(*) FROM music_analysis WHERE analyzed_at IS NOT NULL),
            (SELECT COUNT(*) FROM music_listens),
            (SELECT COUNT(DISTINCT user_id) FROM music_listens),
            (SELECT SUM(seconds_played) / 3600.0 FROM music_listens)",
    )
    .fetch_one(pool)
    .await?;
    Ok((row.0, row.1, row.2, row.3, row.4.unwrap_or(0.0)))
}
