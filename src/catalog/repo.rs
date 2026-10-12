use crate::catalog::models::{
    CreateEpisodeRequest, CreateMediaRequest, CreateSeasonRequest, Credit, Episode,
    ExternalMapping, Genre, Media, MediaDetail, MediaStatus, MediaType, Person, Season, Studio,
    UpdateMediaRequest, UserMediaContext,
};
use crate::error::{ApiError, ApiResult};
use crate::pagination::{decode_cursor, encode_cursor};
use crate::utils::{new_id, slugify};
use serde_json::json;
use sqlx::{PgPool, QueryBuilder};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Newest,
    Title,
    Popularity,
    Rating,
}

impl Sort {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "newest" => Some(Self::Newest),
            "title" => Some(Self::Title),
            "popularity" => Some(Self::Popularity),
            "rating" => Some(Self::Rating),
            _ => None,
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            Self::Newest => "created_at",
            Self::Title => "title",
            Self::Popularity => "popularity",
            Self::Rating => "rating",
        }
    }
}

#[derive(Debug, Default)]
pub struct ListFilter {
    pub media_type: Option<MediaType>,
    pub is_anime: Option<bool>,
    pub status: Option<MediaStatus>,
    pub genres: Vec<String>,
    pub studio: Option<String>,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub min_rating: Option<f64>,
    pub q: Option<String>,
    /// actor/director name substring (credits join)
    pub person: Option<String>,
    /// ISO language code on original_language
    pub language: Option<String>,
    /// minimum available file height
    pub min_height: Option<i32>,
    /// video codec substring
    pub codec: Option<String>,
    pub sort: Sort,
}

fn cursor_value(media: &Media, sort: Sort) -> serde_json::Value {
    let v = match sort {
        Sort::Newest => json!(media.created_at.to_rfc3339()),
        Sort::Title => json!(media.title.clone()),
        Sort::Popularity => json!(media.popularity),
        Sort::Rating => json!(media.rating_avg),
    };
    json!({ "s": sort.key(), "v": v, "i": media.id })
}

pub async fn list(
    pool: &PgPool,
    filter: &ListFilter,
    limit: i64,
    cursor: Option<&str>,
) -> ApiResult<(Vec<Media>, Option<String>)> {
    let mut builder = QueryBuilder::new("SELECT m.* FROM media m WHERE 1 = 1");

    if let Some(mt) = filter.media_type {
        builder.push(" AND m.media_type = ");
        builder.push_bind(mt);
    }
    if let Some(anime) = filter.is_anime {
        builder.push(" AND m.is_anime = ");
        builder.push_bind(anime);
    }
    if let Some(status) = filter.status {
        builder.push(" AND m.status = ");
        builder.push_bind(status);
    }
    if !filter.genres.is_empty() {
        builder.push(
            " AND EXISTS (SELECT 1 FROM media_genres mg JOIN genres g ON g.id = mg.genre_id \
             WHERE mg.media_id = m.id AND g.slug = ANY(",
        );
        builder.push_bind(filter.genres.clone());
        builder.push("))");
    }
    if let Some(studio) = &filter.studio {
        builder.push(
            " AND EXISTS (SELECT 1 FROM media_studios ms JOIN studios s ON s.id = ms.studio_id \
             WHERE ms.media_id = m.id AND s.slug = ",
        );
        builder.push_bind(studio.clone());
        builder.push(")");
    }
    if let Some(year) = filter.year_from {
        builder
            .push(" AND m.release_date >= make_date(")
            .push_bind(year)
            .push(", 1, 1)");
    }
    if let Some(year) = filter.year_to {
        builder
            .push(" AND m.release_date <= make_date(")
            .push_bind(year)
            .push(", 12, 31)");
    }
    if let Some(min) = filter.min_rating {
        builder.push(" AND m.rating_avg >= ").push_bind(min);
    }
    if let Some(person) = filter.person.as_deref().filter(|p| !p.trim().is_empty()) {
        builder.push(
            " AND EXISTS (SELECT 1 FROM credits c JOIN people p ON p.id = c.person_id \
             WHERE c.media_id = m.id AND p.name ILIKE ",
        );
        builder.push_bind(format!("%{}%", person.trim()));
        builder.push(")");
    }
    if let Some(lang) = filter.language.as_deref().filter(|l| !l.trim().is_empty()) {
        builder
            .push(" AND m.original_language = ")
            .push_bind(lang.trim().to_string());
    }
    if let Some(h) = filter.min_height {
        builder.push(
            " AND EXISTS (SELECT 1 FROM media_files mf WHERE mf.media_id = m.id \
             AND mf.height >= ",
        );
        builder.push_bind(h);
        builder.push(")");
    }
    if let Some(codec) = filter.codec.as_deref().filter(|c| !c.trim().is_empty()) {
        builder.push(
            " AND EXISTS (SELECT 1 FROM media_files mf WHERE mf.media_id = m.id \
             AND LOWER(mf.video_codec) LIKE ",
        );
        builder.push_bind(format!("%{}%", codec.trim().to_lowercase()));
        builder.push(")");
    }
    if let Some(q) = filter.q.as_deref().filter(|q| !q.trim().is_empty()) {
        builder.push(" AND m.title ILIKE ");
        builder.push_bind(format!("%{}%", q.trim()));
    }

    if let Some(raw) = cursor {
        let value = decode_cursor(raw)?;
        if value.get("s").and_then(|s| s.as_str()) != Some(filter.sort.key()) {
            return Err(ApiError::bad_request("cursor does not match current sort"));
        }
        let id: Uuid = value
            .get("i")
            .and_then(|i| i.as_str())
            .and_then(|i| Uuid::parse_str(i).ok())
            .ok_or_else(|| ApiError::bad_request("invalid cursor id"))?;
        match filter.sort {
            Sort::Newest => {
                let ts: chrono::DateTime<chrono::Utc> = value
                    .get("v")
                    .and_then(|v| v.as_str())
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                    .map(|t| t.with_timezone(&chrono::Utc))
                    .ok_or_else(|| ApiError::bad_request("invalid cursor value"))?;
                builder.push(" AND (m.created_at, m.id) < (");
                builder.push_bind(ts);
                builder.push(", ");
                builder.push_bind(id);
                builder.push(")");
                builder.push(" ORDER BY m.created_at DESC, m.id DESC");
            }
            Sort::Title => {
                let title = value
                    .get("v")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ApiError::bad_request("invalid cursor value"))?
                    .to_string();
                builder.push(" AND (LOWER(m.title), m.id) > (LOWER(");
                builder.push_bind(title);
                builder.push("), ");
                builder.push_bind(id);
                builder.push(")");
                builder.push(" ORDER BY LOWER(m.title) ASC, m.id ASC");
            }
            Sort::Popularity => {
                let pop: i32 = value
                    .get("v")
                    .and_then(|v| v.as_i64())
                    .and_then(|v| i32::try_from(v).ok())
                    .ok_or_else(|| ApiError::bad_request("invalid cursor value"))?;
                builder.push(" AND (m.popularity, m.id) < (");
                builder.push_bind(pop);
                builder.push(", ");
                builder.push_bind(id);
                builder.push(")");
                builder.push(" ORDER BY m.popularity DESC, m.id DESC");
            }
            Sort::Rating => {
                let rating: f64 = value
                    .get("v")
                    .and_then(|v| v.as_f64())
                    .ok_or_else(|| ApiError::bad_request("invalid cursor value"))?;
                builder.push(" AND (m.rating_avg, m.id) < (");
                builder.push_bind(rating);
                builder.push(", ");
                builder.push_bind(id);
                builder.push(")");
                builder.push(" ORDER BY m.rating_avg DESC, m.id DESC");
            }
        }
    } else {
        match filter.sort {
            Sort::Newest => builder.push(" ORDER BY m.created_at DESC, m.id DESC"),
            Sort::Title => builder.push(" ORDER BY LOWER(m.title) ASC, m.id ASC"),
            Sort::Popularity => builder.push(" ORDER BY m.popularity DESC, m.id DESC"),
            Sort::Rating => builder.push(" ORDER BY m.rating_avg DESC, m.id DESC"),
        };
    }

    builder.push(" LIMIT ").push_bind(limit + 1);

    let mut rows: Vec<Media> = builder.build_query_as().fetch_all(pool).await?;
    let next = (rows.len() as i64 > limit).then(|| {
        rows.pop();
        rows.last()
            .map(|last| encode_cursor(&cursor_value(last, filter.sort)))
    });
    Ok((rows, next.flatten()))
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> ApiResult<Option<Media>> {
    let media = sqlx::query_as::<_, Media>("SELECT * FROM media WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(media)
}

pub async fn create(pool: &PgPool, req: &CreateMediaRequest) -> ApiResult<Media> {
    if req.title.trim().is_empty() {
        return Err(ApiError::bad_request("title must not be empty"));
    }
    let id = new_id();
    let media = sqlx::query_as::<_, Media>(
        "INSERT INTO media (id, media_type, is_anime, title, original_title, overview, release_date, \
         runtime_minutes, status, poster_url, backdrop_url)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         RETURNING *",
    )
    .bind(id)
    .bind(req.media_type)
    .bind(req.is_anime)
    .bind(req.title.trim())
    .bind(req.original_title.as_deref().map(str::trim))
    .bind(req.overview.as_deref().unwrap_or(""))
    .bind(req.release_date)
    .bind(req.runtime_minutes)
    .bind(req.status.unwrap_or(match req.media_type {
        MediaType::Movie => MediaStatus::Released,
        MediaType::Series | MediaType::Book | MediaType::Music | MediaType::Manga | MediaType::Podcast | MediaType::Audiobook => MediaStatus::Ongoing,
    }))
    .bind(req.poster_url.as_deref())
    .bind(req.backdrop_url.as_deref())
    .fetch_one(pool)
    .await?;

    for genre in &req.genres {
        attach_genre_by_name(pool, media.id, genre).await?;
    }
    for studio in &req.studios {
        attach_studio_by_name(pool, media.id, studio).await?;
    }
    Ok(media)
}

pub async fn update(pool: &PgPool, id: Uuid, req: &UpdateMediaRequest) -> ApiResult<Media> {
    let release_null_flag = matches!(req.release_date, Some(None));
    let release_value = req.release_date.clone().flatten();

    let media = sqlx::query_as::<_, Media>(
        "UPDATE media SET
            title = COALESCE($2, title),
            overview = COALESCE($3, overview),
            runtime_minutes = COALESCE($4, runtime_minutes),
            status = COALESCE($5, status),
            poster_url = COALESCE($6, poster_url),
            backdrop_url = COALESCE($7, backdrop_url),
            trailer_url = COALESCE($8, trailer_url),
            is_anime = COALESCE($9, is_anime),
            release_date = CASE WHEN $10 THEN $11 ELSE release_date END,
            updated_at = now()
         WHERE id = $1
         RETURNING *",
    )
    .bind(id)
    .bind(
        req.title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
    .bind(req.overview.as_deref())
    .bind(req.runtime_minutes)
    .bind(req.status)
    .bind(req.poster_url.as_deref())
    .bind(req.backdrop_url.as_deref())
    .bind(req.trailer_url.as_deref())
    .bind(req.is_anime)
    .bind(release_null_flag)
    .bind(release_value)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("media"))?;
    Ok(media)
}

pub async fn delete(pool: &PgPool, id: Uuid) -> ApiResult<()> {
    let result = sqlx::query("DELETE FROM media WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("media"));
    }
    Ok(())
}

pub async fn ensure_genre(pool: &PgPool, name: &str) -> ApiResult<Genre> {
    let slug = slugify(name);
    let genre = sqlx::query_as::<_, Genre>(
        "INSERT INTO genres (id, name, slug) VALUES ($1, $2, $3)
         ON CONFLICT (slug) DO UPDATE SET name = EXCLUDED.name
         RETURNING *",
    )
    .bind(new_id())
    .bind(name.trim())
    .bind(slug)
    .fetch_one(pool)
    .await?;
    Ok(genre)
}

pub async fn ensure_studio(pool: &PgPool, name: &str) -> ApiResult<Studio> {
    let slug = slugify(name);
    let studio = sqlx::query_as::<_, Studio>(
        "INSERT INTO studios (id, name, slug) VALUES ($1, $2, $3)
         ON CONFLICT (slug) DO UPDATE SET name = EXCLUDED.name
         RETURNING *",
    )
    .bind(new_id())
    .bind(name.trim())
    .bind(slug)
    .fetch_one(pool)
    .await?;
    Ok(studio)
}

pub async fn ensure_person(pool: &PgPool, name: &str, image: Option<&str>) -> ApiResult<Person> {
    let existing: Option<Person> =
        sqlx::query_as("SELECT * FROM people WHERE LOWER(name) = LOWER($1)")
            .bind(name)
            .fetch_optional(pool)
            .await?;
    if let Some(person) = existing {
        return Ok(person);
    }
    let person = sqlx::query_as::<_, Person>(
        "INSERT INTO people (id, name, image_url) VALUES ($1, $2, $3) RETURNING *",
    )
    .bind(new_id())
    .bind(name.trim())
    .bind(image)
    .fetch_one(pool)
    .await?;
    Ok(person)
}

pub async fn attach_genre_by_name(pool: &PgPool, media_id: Uuid, name: &str) -> ApiResult<()> {
    let genre = ensure_genre(pool, name).await?;
    sqlx::query(
        "INSERT INTO media_genres (media_id, genre_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(media_id)
    .bind(genre.id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn attach_studio_by_name(pool: &PgPool, media_id: Uuid, name: &str) -> ApiResult<()> {
    let studio = ensure_studio(pool, name).await?;
    sqlx::query(
        "INSERT INTO media_studios (media_id, studio_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(media_id)
    .bind(studio.id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn add_credit(
    pool: &PgPool,
    media_id: Uuid,
    person_name: &str,
    credit_type: &str,
    character_name: Option<&str>,
    sort_order: i32,
) -> ApiResult<()> {
    let person = ensure_person(pool, person_name, None).await?;
    sqlx::query(
        "INSERT INTO credits (id, media_id, person_id, credit_type, character_name, sort_order)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (media_id, person_id, credit_type) DO NOTHING",
    )
    .bind(new_id())
    .bind(media_id)
    .bind(person.id)
    .bind(credit_type)
    .bind(character_name)
    .bind(sort_order)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn upsert_external_mapping(
    pool: &PgPool,
    media_id: Uuid,
    source: &str,
    external_id: &str,
    payload: serde_json::Value,
) -> ApiResult<Uuid> {
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM external_mappings WHERE source = $1 AND external_id = $2")
            .bind(source)
            .bind(external_id)
            .fetch_optional(pool)
            .await?;

    if let Some((mapping_id,)) = existing {
        sqlx::query(
            "UPDATE external_mappings SET media_id = $2, payload = $3, synced_at = now() WHERE id = $1",
        )
        .bind(mapping_id)
        .bind(media_id)
        .bind(payload)
        .execute(pool)
        .await?;
        return Ok(mapping_id);
    }

    let mapping_id = new_id();
    sqlx::query(
        "INSERT INTO external_mappings (id, media_id, source, external_id, payload)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(mapping_id)
    .bind(media_id)
    .bind(source)
    .bind(external_id)
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(mapping_id)
}

pub async fn media_for_external(
    pool: &PgPool,
    source: &str,
    external_id: &str,
) -> ApiResult<Option<Media>> {
    let media = sqlx::query_as::<_, Media>(
        "SELECT m.* FROM media m JOIN external_mappings e ON e.media_id = m.id \
         WHERE e.source = $1 AND e.external_id = $2",
    )
    .bind(source)
    .bind(external_id)
    .fetch_optional(pool)
    .await?;
    Ok(media)
}

pub async fn find_media_by_title(pool: &PgPool, title: &str) -> ApiResult<Option<Media>> {
    let media = sqlx::query_as::<_, Media>(
        "SELECT * FROM media WHERE LOWER(title) = LOWER($1) ORDER BY created_at LIMIT 1",
    )
    .bind(title.trim())
    .fetch_optional(pool)
    .await?;
    Ok(media)
}

pub async fn seasons_of(pool: &PgPool, media_id: Uuid) -> ApiResult<Vec<Season>> {
    let seasons =
        sqlx::query_as::<_, Season>("SELECT * FROM seasons WHERE media_id = $1 ORDER BY number")
            .bind(media_id)
            .fetch_all(pool)
            .await?;
    Ok(seasons)
}

pub async fn episodes_of_season(pool: &PgPool, season_id: Uuid) -> ApiResult<Vec<Episode>> {
    let episodes =
        sqlx::query_as::<_, Episode>("SELECT * FROM episodes WHERE season_id = $1 ORDER BY number")
            .bind(season_id)
            .fetch_all(pool)
            .await?;
    Ok(episodes)
}

pub async fn episodes_of_media(pool: &PgPool, media_id: Uuid) -> ApiResult<Vec<Episode>> {
    let episodes = sqlx::query_as::<_, Episode>(
        "SELECT * FROM episodes WHERE media_id = $1 ORDER BY season_id, number",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await?;
    Ok(episodes)
}

pub async fn create_season(
    pool: &PgPool,
    media_id: Uuid,
    req: &CreateSeasonRequest,
) -> ApiResult<Season> {
    let season = sqlx::query_as::<_, Season>(
        "INSERT INTO seasons (id, media_id, number, title, overview, air_date)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (media_id, number) DO UPDATE SET title = EXCLUDED.title
         RETURNING *",
    )
    .bind(new_id())
    .bind(media_id)
    .bind(req.number)
    .bind(req.title.as_deref())
    .bind(req.overview.as_deref().unwrap_or(""))
    .bind(req.air_date)
    .fetch_one(pool)
    .await?;
    Ok(season)
}

pub async fn create_episode(
    pool: &PgPool,
    media_id: Uuid,
    season_id: Uuid,
    req: &CreateEpisodeRequest,
) -> ApiResult<Episode> {
    let episode = sqlx::query_as::<_, Episode>(
        "INSERT INTO episodes (id, media_id, season_id, number, absolute_number, title, overview, air_date, runtime_minutes)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (season_id, number) DO UPDATE SET title = EXCLUDED.title
         RETURNING *",
    )
    .bind(new_id())
    .bind(media_id)
    .bind(season_id)
    .bind(req.number)
    .bind(req.absolute_number)
    .bind(req.title.trim())
    .bind(req.overview.as_deref().unwrap_or(""))
    .bind(req.air_date)
    .bind(req.runtime_minutes)
    .fetch_one(pool)
    .await?;
    Ok(episode)
}

pub async fn ensure_season_number(pool: &PgPool, media_id: Uuid, number: i32) -> ApiResult<Season> {
    let existing: Option<Season> =
        sqlx::query_as("SELECT * FROM seasons WHERE media_id = $1 AND number = $2")
            .bind(media_id)
            .bind(number)
            .fetch_optional(pool)
            .await?;
    if let Some(season) = existing {
        return Ok(season);
    }
    let season = sqlx::query_as::<_, Season>(
        "INSERT INTO seasons (id, media_id, number, title) VALUES ($1, $2, $3, NULL) RETURNING *",
    )
    .bind(new_id())
    .bind(media_id)
    .bind(number)
    .fetch_one(pool)
    .await?;
    Ok(season)
}

pub async fn ensure_episode(
    pool: &PgPool,
    media_id: Uuid,
    season_id: Uuid,
    number: i32,
    title: &str,
) -> ApiResult<Episode> {
    let existing: Option<Episode> =
        sqlx::query_as("SELECT * FROM episodes WHERE season_id = $1 AND number = $2")
            .bind(season_id)
            .bind(number)
            .fetch_optional(pool)
            .await?;
    if let Some(episode) = existing {
        return Ok(episode);
    }
    let episode = sqlx::query_as::<_, Episode>(
        "INSERT INTO episodes (id, media_id, season_id, number, title) VALUES ($1, $2, $3, $4, $5) RETURNING *",
    )
    .bind(new_id())
    .bind(media_id)
    .bind(season_id)
    .bind(number)
    .bind(title)
    .fetch_one(pool)
    .await?;
    Ok(episode)
}

async fn load_detail_parts(
    pool: &PgPool,
    media_id: Uuid,
) -> ApiResult<(Vec<Genre>, Vec<Studio>, Vec<Credit>, Vec<ExternalMapping>)> {
    let genres = sqlx::query_as::<_, Genre>(
        "SELECT g.* FROM genres g JOIN media_genres mg ON mg.genre_id = g.id \
         WHERE mg.media_id = $1 ORDER BY g.name",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await?;

    let studios = sqlx::query_as::<_, Studio>(
        "SELECT s.* FROM studios s JOIN media_studios ms ON ms.studio_id = s.id \
         WHERE ms.media_id = $1 ORDER BY s.name",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await?;

    let credits = sqlx::query_as::<_, Credit>(
        "SELECT c.id, c.media_id, c.person_id, p.name AS person_name, p.image_url AS person_image, \
                c.credit_type, c.character_name, c.sort_order
         FROM credits c JOIN people p ON p.id = c.person_id
         WHERE c.media_id = $1
         ORDER BY CASE WHEN c.credit_type IN ('cast', 'voice') THEN 0 ELSE 1 END, c.sort_order",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await?;

    let externals = sqlx::query_as::<_, ExternalMapping>(
        "SELECT * FROM external_mappings WHERE media_id = $1 ORDER BY source",
    )
    .bind(media_id)
    .fetch_all(pool)
    .await?;

    Ok((genres, studios, credits, externals))
}

pub async fn detail_for_user(
    pool: &PgPool,
    media: Media,
    user_id: Option<Uuid>,
) -> ApiResult<MediaDetail> {
    let (genres, studios, cast, external_ids) = load_detail_parts(pool, media.id).await?;
    let seasons_raw = seasons_of(pool, media.id).await?;
    let mut seasons = Vec::with_capacity(seasons_raw.len());
    for season in seasons_raw {
        let episodes = episodes_of_season(pool, season.id).await?;
        seasons.push(crate::catalog::models::SeasonWithEpisodes { season, episodes });
    }

    let my = match user_id {
        Some(uid) => {
            let entry: Option<(Option<String>, Option<f64>)> = sqlx::query_as(
                "SELECT status, score FROM list_entries WHERE user_id = $1 AND media_id = $2",
            )
            .bind(uid)
            .bind(media.id)
            .fetch_optional(pool)
            .await?;
            let favorite: Option<(bool,)> =
                sqlx::query_as("SELECT TRUE FROM favorites WHERE user_id = $1 AND media_id = $2")
                    .bind(uid)
                    .bind(media.id)
                    .fetch_optional(pool)
                    .await?;
            entry.map(|(status, score)| UserMediaContext {
                status,
                score,
                favorite: favorite.is_some(),
            })
        }
        None => None,
    };

    Ok(MediaDetail {
        media,
        genres,
        studios,
        cast,
        seasons,
        external_ids,
        my,
    })
}

pub async fn recompute_rating(pool: &PgPool, media_id: Uuid) -> ApiResult<()> {
    sqlx::query(
        "UPDATE media SET
            rating_avg = COALESCE((SELECT AVG(score) FROM reviews WHERE media_id = $1 AND status = 'published' AND score IS NOT NULL), 0),
            rating_count = (SELECT COUNT(*) FROM reviews WHERE media_id = $1 AND status = 'published' AND score IS NOT NULL)
         WHERE id = $1",
    )
    .bind(media_id)
    .execute(pool)
    .await?;
    Ok(())
}
