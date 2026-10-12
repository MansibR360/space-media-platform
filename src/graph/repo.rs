//! Graph storage + traversal: relationships, neighborhood, franchise journeys.

use chrono::NaiveDate;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::utils::new_id;

pub const RELATIONS: &[&str] = &[
    "adaptation_of",
    "based_on",
    "sequel_of",
    "prequel_of",
    "spin_off_of",
    "remake_of",
    "side_story_of",
    "parent_of",
    "same_universe",
    "same_franchise",
    "alternate_version_of",
    "related_to",
];

/// Relations that carry a story across media formats — the backbone of
/// cross-media journeys.
pub const ADAPTATION_RELATIONS: &[&str] = &[
    "adaptation_of",
    "based_on",
    "spin_off_of",
    "remake_of",
    "alternate_version_of",
];

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct Relationship {
    pub id: Uuid,
    pub from_media_id: Uuid,
    pub to_media_id: Uuid,
    pub relation: String,
    pub source: String,
    pub confidence: f64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub fn validate_relation(relation: &str) -> ApiResult<()> {
    if !RELATIONS.contains(&relation) {
        return Err(ApiError::bad_request(format!(
            "relation must be one of: {}",
            RELATIONS.join(", ")
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relations_validated() {
        assert!(validate_relation("adaptation_of").is_ok());
        assert!(validate_relation("sequel_of").is_ok());
        assert!(validate_relation("not_a_relation").is_err());
    }
}

pub async fn add(
    pool: &PgPool,
    from_media_id: Uuid,
    to_media_id: Uuid,
    relation: &str,
    source: &str,
) -> ApiResult<Relationship> {
    if from_media_id == to_media_id {
        return Err(ApiError::bad_request("cannot relate media to itself"));
    }
    validate_relation(relation)?;

    // Both endpoints must exist.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE id = ANY($1)")
        .bind(vec![from_media_id, to_media_id])
        .fetch_one(pool)
        .await?;
    if count < 2 {
        return Err(ApiError::not_found("media"));
    }

    let rel = sqlx::query_as::<_, Relationship>(
        "INSERT INTO media_relationships (id, from_media_id, to_media_id, relation, source)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (from_media_id, to_media_id, relation) DO UPDATE SET
             source = EXCLUDED.source
         RETURNING *",
    )
    .bind(new_id())
    .bind(from_media_id)
    .bind(to_media_id)
    .bind(relation)
    .bind(source)
    .fetch_one(pool)
    .await?;
    Ok(rel)
}

pub async fn remove(pool: &PgPool, from: Uuid, to: Uuid, relation: &str) -> ApiResult<bool> {
    let result = sqlx::query(
        "DELETE FROM media_relationships \
         WHERE from_media_id = $1 AND to_media_id = $2 AND relation = $3",
    )
    .bind(from)
    .bind(to)
    .bind(relation)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// One hop in either direction, joined with the *other* side's card.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct RelatedItem {
    pub relation: String,
    /// out = this media points at the other; in = the other points here
    pub direction: String,
    pub media_id: Uuid,
    pub title: String,
    pub media_type: String,
    pub poster_url: Option<String>,
    pub release_date: Option<NaiveDate>,
}

pub async fn related(pool: &PgPool, media_id: Uuid) -> ApiResult<Vec<RelatedItem>> {
    let rows = sqlx::query_as::<_, RelatedItem>(
        "(SELECT r.relation, 'out'::text AS direction, r.to_media_id AS media_id,
                m.title, m.media_type, m.poster_url, m.release_date
         FROM media_relationships r JOIN media m ON m.id = r.to_media_id
         WHERE r.from_media_id = $1)
         UNION ALL
         (SELECT r.relation, 'in'::text AS direction, r.from_media_id AS media_id,
                m.title, m.media_type, m.poster_url, m.release_date
         FROM media_relationships r JOIN media m ON m.id = r.from_media_id
         WHERE r.to_media_id = $1)
         ORDER BY release_date NULLS LAST",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct JourneyNode {
    pub media_id: Uuid,
    pub title: String,
    pub media_type: String,
    pub poster_url: Option<String>,
    pub release_date: Option<NaiveDate>,
    /// relation connecting this node to the previous one (root = null)
    pub via_relation: Option<String>,
    pub depth: i32,
}

/// Breadth-first franchise traversal up to `max_depth` hops (both directions).
pub async fn journey(
    pool: &PgPool,
    root: Uuid,
    max_depth: i32,
) -> ApiResult<Vec<JourneyNode>> {
    let max_depth = max_depth.clamp(1, 4);
    let mut nodes: Vec<JourneyNode> = Vec::new();
    let mut visited: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
    visited.insert(root);

    let card = |media_id: Uuid, title: String, media_type: String, poster_url: Option<String>, release_date: Option<NaiveDate>, via: Option<String>, depth: i32| JourneyNode {
        media_id,
        title,
        media_type,
        poster_url,
        release_date,
        via_relation: via,
        depth,
    };

    let root_row: Option<(Uuid, String, String, Option<String>, Option<NaiveDate>)> =
        sqlx::query_as(
            "SELECT id, title, media_type, poster_url, release_date FROM media WHERE id = $1",
        )
        .bind(root)
        .fetch_optional(pool)
        .await?;
    let Some((id, title, media_type, poster_url, release_date)) = root_row else {
        return Err(ApiError::not_found("media"));
    };
    nodes.push(card(id, title, media_type, poster_url, release_date, None, 0));

    let mut frontier: Vec<Uuid> = vec![root];
    for depth in 1..=max_depth {
        if frontier.is_empty() {
            break;
        }
        // One SQL round-trip per depth level keeps traversal bounded.
        let next: Vec<RelatedItem> = sqlx::query_as::<_, RelatedItem>(
            "(SELECT r.relation, 'out'::text AS direction, r.to_media_id AS media_id,
                    m.title, m.media_type, m.poster_url, m.release_date
             FROM media_relationships r JOIN media m ON m.id = r.to_media_id
             WHERE r.from_media_id = ANY($1))
             UNION ALL
             (SELECT r.relation, 'in'::text AS direction, r.from_media_id AS media_id,
                    m.title, m.media_type, m.poster_url, m.release_date
             FROM media_relationships r JOIN media m ON m.id = r.from_media_id
             WHERE r.to_media_id = ANY($1))",
        )
        .bind(&frontier)
        .fetch_all(pool)
        .await?;

        frontier = Vec::new();
        for item in next {
            if visited.insert(item.media_id) {
                frontier.push(item.media_id);
                nodes.push(card(
                    item.media_id,
                    item.title,
                    item.media_type,
                    item.poster_url,
                    item.release_date,
                    Some(item.relation),
                    depth,
                ));
            }
        }
    }
    Ok(nodes)
}

// ------------------------------------------------------------ user journeys

#[derive(Debug, Clone, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct JourneyStep {
    pub media_id: Uuid,
    pub title: String,
    pub media_type: String,
    pub poster_url: Option<String>,
    pub release_date: Option<NaiveDate>,
    pub relation: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CrossMediaJourney {
    pub anchor_media_id: Uuid,
    pub anchor_title: String,
    pub anchor_poster: Option<String>,
    pub reason: String,
    pub steps: Vec<JourneyStep>,
}

/// Cross-media journey recommendations: story continuations the user hasn't
/// consumed yet, reached through adaptation-style relations from anchors they
/// finished or rated highly.
pub async fn journeys_for_user(
    pool: &PgPool,
    user_id: Uuid,
    limit: i64,
) -> ApiResult<Vec<CrossMediaJourney>> {
    let limit = limit.clamp(1, 20);
    let seen: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT media_id FROM list_entries WHERE user_id = $1
         UNION SELECT media_id FROM watch_history WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    let seen: Vec<Uuid> = seen.into_iter().map(|(id,)| id).collect();

    // Anchors: recently finished or well-rated items.
    let anchors: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT le.media_id, m.title, m.poster_url
         FROM list_entries le JOIN media m ON m.id = le.media_id
         WHERE le.user_id = $1 AND (le.status = 'completed' OR le.score >= 7)
         ORDER BY le.updated_at DESC LIMIT 8",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    let mut journeys = Vec::new();
    for (anchor_id, anchor_title, anchor_poster) in anchors {
        if journeys.len() >= limit as usize {
            break;
        }
        // Story-carrying edges out of and into the anchor, unseen targets only.
        let steps: Vec<JourneyStep> = sqlx::query_as::<_, JourneyStep>(
            "(SELECT r.to_media_id AS media_id, m.title, m.media_type, m.poster_url,
                     m.release_date, r.relation
             FROM media_relationships r JOIN media m ON m.id = r.to_media_id
             WHERE r.from_media_id = $1 AND r.relation = ANY($2))
             UNION ALL
             (SELECT r.from_media_id AS media_id, m.title, m.media_type, m.poster_url,
                     m.release_date, r.relation || '_reverse'
             FROM media_relationships r JOIN media m ON m.id = r.from_media_id
             WHERE r.to_media_id = $1 AND r.relation = ANY($2))
             ORDER BY release_date NULLS LAST LIMIT 6",
        )
        .bind(anchor_id)
        .bind(ADAPTATION_RELATIONS)
        .fetch_all(pool)
        .await?;

        let steps: Vec<JourneyStep> = steps
            .into_iter()
            // Seen is seen — regardless of which direction the edge pointed.
            .filter(|step| !seen.contains(&step.media_id))
            .collect();
        if steps.is_empty() {
            continue;
        }
        let reason = format!(
            "you finished {anchor_title} — the same story continues in another format"
        );
        journeys.push(CrossMediaJourney {
            anchor_media_id: anchor_id,
            anchor_title,
            anchor_poster,
            reason,
            steps,
        });
    }
    Ok(journeys)
}
