use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::intelligence::models::{
    BecauseYouWatched, EpisodeAir, GenreAffinity, HomeRails, TasteProfile, VIBES, VibeCandidateRow,
    VibePick, VibeResponse,
};

pub async fn taste_profile(pool: &PgPool, user_id: Uuid) -> anyhow::Result<TasteProfile> {
    let genre_rows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT g.name, COUNT(*)::float8 AS weight
         FROM watch_history wh
         JOIN media_genres mg ON mg.media_id = wh.media_id
         JOIN genres g ON g.id = mg.genre_id
         WHERE wh.user_id = $1
         GROUP BY g.name ORDER BY weight DESC LIMIT 8",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    // Weighted with list scores so rated items count more than raw watches.
    let list_rows: Vec<(String, f64)> = sqlx::query_as(
        "SELECT g.name, COALESCE(SUM(GREATEST(le.score, 5.0)), 0)::float8 AS weight
         FROM list_entries le
         JOIN media_genres mg ON mg.media_id = le.media_id
         JOIN genres g ON g.id = mg.genre_id
         WHERE le.user_id = $1 AND le.score IS NOT NULL
         GROUP BY g.name",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let mut weights: HashMap<String, f64> = HashMap::new();
    for (genre, weight) in genre_rows.iter().chain(list_rows.iter()) {
        *weights.entry(genre.clone()).or_insert(0.0) += weight;
    }
    let mut top_genres: Vec<GenreAffinity> = weights
        .into_iter()
        .map(|(genre, weight)| GenreAffinity { genre, weight })
        .collect();
    top_genres.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let max_weight = top_genres.first().map(|g| g.weight).unwrap_or(1.0);
    for entry in &mut top_genres {
        entry.weight = (entry.weight / max_weight.max(1.0) * 100.0).round();
    }
    top_genres.truncate(6);

    let decade_rows: Vec<(Option<i32>,)> = sqlx::query_as(
        "SELECT FLOOR(EXTRACT(YEAR FROM m.release_date) / 10) * 10 AS decade
         FROM watch_history wh JOIN media m ON m.id = wh.media_id
         WHERE wh.user_id = $1 AND m.release_date IS NOT NULL
         GROUP BY decade ORDER BY COUNT(*) DESC LIMIT 3",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let stats: Option<(Option<f64>, f64, f64, f64)> = sqlx::query_as(
        "SELECT AVG(m.runtime_minutes)::float8,
                AVG(CASE WHEN m.is_anime THEN 100.0 ELSE 0.0 END),
                COALESCE(SUM(DISTINCT wh.duration_seconds), 0) / 3600.0,
                AVG(CASE WHEN wh.completed THEN 100.0 ELSE 0.0 END)
         FROM watch_history wh JOIN media m ON m.id = wh.media_id
         WHERE wh.user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    let (avg_runtime, anime_affinity, watch_hours, completion) =
        stats.unwrap_or((None, 0.0, 0.0, 0.0));

    Ok(TasteProfile {
        top_genres,
        favorite_decades: decade_rows.into_iter().filter_map(|(d,)| d).collect(),
        avg_runtime_minutes: avg_runtime,
        anime_affinity: anime_affinity.round(),
        total_watch_hours: (watch_hours * 10.0).round() / 10.0,
        completion_rate: completion.round(),
    })
}

/// Natural-language mood search. Deterministic scoring over genres,
/// overview keywords and pace — no external AI service required.
pub async fn vibe_search(pool: &PgPool, query: &str, limit: i64) -> anyhow::Result<VibeResponse> {
    let normalized = query.to_lowercase();
    let mut matched: Vec<&crate::intelligence::models::VibeSpec> = Vec::new();

    for spec in VIBES {
        let label_lower = spec.label.to_lowercase();
        let label_words: Vec<&str> = label_lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .collect();
        let vibe_name = format!("{:?}", spec.vibe).to_lowercase();
        let first_word = label_words.first().copied().unwrap_or("").to_string();
        let hit = label_words.iter().any(|w| normalized.contains(w))
            || normalized.contains(&vibe_name)
            || (!first_word.is_empty() && normalized.contains(&first_word));
        if hit {
            matched.push(spec);
        }
    }

    if matched.is_empty() {
        matched.push(&VIBES[0]);
    }

    let genre_union: Vec<String> = matched
        .iter()
        .flat_map(|spec| spec.genres.iter())
        .map(|g| g.to_string())
        .collect::<Vec<_>>();
    let genre_union: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        genre_union
            .into_iter()
            .filter(|g| seen.insert(g.clone()))
            .collect()
    };

    if genre_union.is_empty() {
        return Ok(VibeResponse {
            matched_vibes: matched.iter().map(|s| s.label).collect(),
            items: vec![],
        });
    }

    let rows: Vec<VibeCandidateRow> = sqlx::query_as(
        "SELECT m.id, m.title, m.overview, m.poster_url, m.release_date, m.runtime_minutes, \
                m.rating_avg, m.popularity,
                (SELECT string_agg(g.slug, ',') FROM media_genres mg JOIN genres g ON g.id = mg.genre_id \
                 WHERE mg.media_id = m.id) AS genres
         FROM media m
         WHERE EXISTS (
             SELECT 1 FROM media_genres mg JOIN genres g ON g.id = mg.genre_id
             WHERE mg.media_id = m.id AND g.slug = ANY($1)
         )
         ORDER BY m.popularity DESC LIMIT $2",
    )
    .bind(&genre_union)
    .bind(limit.clamp(1, 60))
    .fetch_all(pool)
    .await?;

    let mut picks: Vec<VibePick> = rows
        .into_iter()
        .map(|row| {
            let mut score = row.rating_avg + row.popularity as f64 / 2000.0;
            let mut reasons: Vec<&str> = Vec::new();

            let haystack = format!("{} {}", row.title, row.overview).to_lowercase();
            for spec in &matched {
                for keyword in spec.keywords {
                    if haystack.contains(keyword) {
                        score += 1.5;
                        reasons.push(keyword);
                    }
                }
                if let Some(runtime) = row.runtime_minutes.map(|r| r as f64) {
                    if runtime >= spec.pace.0 && runtime <= spec.pace.1 {
                        score += 0.75;
                        reasons.push("right pace");
                    }
                }
            }

            let why = if reasons.is_empty() {
                format!("matches {}", matched[0].label)
            } else {
                format!(
                    "{} · {}",
                    matched[0].label,
                    reasons.into_iter().take(3).collect::<Vec<_>>().join(", ")
                )
            };

            VibePick {
                media_id: row.id,
                title: row.title,
                poster_url: row.poster_url,
                score: (score * 100.0).round() / 100.0,
                why,
            }
        })
        .collect();

    picks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    picks.truncate(limit.clamp(1, 24) as usize);

    Ok(VibeResponse {
        matched_vibes: matched.iter().map(|s| s.label).collect(),
        items: picks,
    })
}

pub async fn home_rails(
    pool: &PgPool,
    state: &crate::state::AppState,
    user_id: Uuid,
) -> anyhow::Result<HomeRails> {
    let continue_watching = crate::tracking::repo::continue_watching(pool, user_id, 12).await?;
    let trending =
        crate::search::repo::trending(pool, crate::search::models::TrendWindow::Week, 12).await?;
    let new_episodes = new_episodes_for_user(pool, user_id, 12).await?;
    let friends_activity = crate::social::repo::feed_for_user(pool, user_id, 8, None).await?;
    let recommended = state
        .recommendations
        .for_user(user_id, 12)
        .await
        .map(|r| r.items)
        .unwrap_or_default();
    let vibe_picks = vibe_search(pool, "cozy feel-good", 8).await?.items;

    let because_you_watched = because_anchor(pool, user_id).await?;

    Ok(HomeRails {
        continue_watching,
        trending,
        because_you_watched,
        new_episodes,
        friends_activity,
        recommended,
        vibe_picks,
    })
}

async fn because_anchor(pool: &PgPool, user_id: Uuid) -> anyhow::Result<Option<BecauseYouWatched>> {
    let anchor: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT le.media_id, m.title FROM list_entries le
         JOIN media m ON m.id = le.media_id
         WHERE le.user_id = $1 AND le.status IN ('watching', 'completed')
           AND (le.score IS NULL OR le.score >= 6)
         ORDER BY COALESCE(le.score, 7) DESC, le.updated_at DESC LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    let Some((anchor_id, anchor_title)) = anchor else {
        return Ok(None);
    };
    let items = crate::search::repo::similar(pool, anchor_id, 12).await?;
    Ok(Some(BecauseYouWatched {
        anchor_media_id: anchor_id,
        anchor_title,
        items,
    }))
}

async fn new_episodes_for_user(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> anyhow::Result<Vec<EpisodeAir>> {
    let rows = sqlx::query_as::<_, EpisodeAir>(
        "SELECT e.id, e.media_id, e.title, e.number, e.air_date, e.still_url
         FROM episodes e
         JOIN list_entries le ON le.media_id = e.media_id AND le.user_id = $1
             AND le.status IN ('watching', 'planned')
         WHERE e.air_date BETWEEN current_date - INTERVAL '2 days' AND current_date + INTERVAL '14 days'
         ORDER BY e.air_date ASC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

#[allow(dead_code)]
pub fn empty_profile() -> TasteProfile {
    TasteProfile {
        top_genres: vec![],
        favorite_decades: vec![],
        avg_runtime_minutes: None,
        anime_affinity: 0.0,
        total_watch_hours: 0.0,
        completion_rate: 0.0,
    }
}
