use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::error::{ApiError, ApiResult};
use crate::parties::manager::PartySyncState;
use crate::parties::models::{
    CreatePartyRequest, JoinPartyRequest, Party, PartyControlRequest, PartyEvent, PartyMember,
};
use crate::state::AppState;

#[derive(Debug, Deserialize, IntoParams)]
pub struct PartyIdPath {
    pub party_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct PartyEventsQuery {
    #[serde(default)]
    pub after: i64,
    #[serde(default = "default_event_limit")]
    pub limit: i64,
}

fn default_event_limit() -> i64 {
    100
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PartyCreated {
    #[serde(flatten)]
    pub party: Party,
    pub conversation_id: Option<Uuid>,
}

#[utoipa::path(post, path = "/api/parties", tag = "parties",
    security(("bearer" = [])),
    request_body = CreatePartyRequest,
    responses((status = 201, body = PartyCreated))
)]
pub async fn create_party(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<CreatePartyRequest>,
) -> ApiResult<(StatusCode, Json<PartyCreated>)> {
    let party = crate::parties::repo::create(
        &state.pool,
        auth.id(),
        &req.title,
        req.media_id,
        req.episode_id,
        &req.visibility,
        req.max_participants,
        req.password.as_deref(),
    )
    .await?;

    let conversation =
        crate::chat::repo::create_for_party(&state.pool, party.id, auth.id(), &party.title)
            .await
            .ok();

    state
        .parties
        .broadcast_member_event(
            party.id,
            "join",
            serde_json::json!({ "user_id": auth.id(), "display_name": auth.user().display_name }),
        )
        .await?;

    let conversation_id = conversation.map(|c| c.id);
    Ok((
        StatusCode::CREATED,
        Json(PartyCreated {
            party,
            conversation_id,
        }),
    ))
}

#[utoipa::path(post, path = "/api/parties/{party_id}/join", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    request_body = JoinPartyRequest,
    responses((status = 200, body = PartyMember), (status = 403), (status = 404))
)]
pub async fn join_party(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
    body: Option<Json<JoinPartyRequest>>,
) -> ApiResult<Json<PartyMember>> {
    let party = crate::parties::repo::get(&state.pool, path.party_id).await?;

    if party.ended_at.is_some() {
        return Err(ApiError::not_found("party has ended"));
    }

    // Password gate (any visibility) — sha256 compared server-side.
    if let Some(expected_hash) = party.password_hash.as_deref() {
        let provided = body
            .as_ref()
            .and_then(|Json(r)| r.password.clone())
            .unwrap_or_default();
        let supplied_hash = crate::utils::sha256_hex(&provided);
        let ok = constant_time_eq_str(&supplied_hash, expected_hash);
        let already = crate::parties::repo::is_member(&state.pool, party.id, auth.id()).await?;
        if !ok && !already {
            return Err(ApiError::forbidden("party password required"));
        }
    }

    if party.visibility == "private" {
        let provided = body.as_ref().and_then(|Json(r)| r.join_code.clone());
        let matches = provided
            .as_deref()
            .map(|code| code.eq_ignore_ascii_case(&party.join_code))
            .unwrap_or(false);
        let already_member =
            crate::parties::repo::is_member(&state.pool, party.id, auth.id()).await?;
        if !matches && !already_member {
            return Err(ApiError::forbidden(
                "join code required for private parties",
            ));
        }
    }

    if party.visibility == "friends" && party.host_id != auth.id() {
        let connected = sqlx::query_as::<_, (bool,)>(
            "SELECT EXISTS(SELECT 1 FROM follows WHERE (follower_id = $1 AND followed_id = $2) OR (follower_id = $2 AND followed_id = $1))",
        )
        .bind(party.host_id)
        .bind(auth.id())
        .fetch_one(&state.pool)
        .await?
        .0;
        if !connected {
            return Err(ApiError::forbidden(
                "this party is limited to friends of the host",
            ));
        }
    }

    let count = crate::parties::repo::member_count(&state.pool, party.id).await?;
    if count >= party.max_participants as i64 {
        return Err(ApiError::Conflict("party is full".into()));
    }

    let member = crate::parties::repo::join(&state.pool, party.id, auth.id()).await?;

    // Notification trigger: host learns someone joined their party.
    let _ = crate::platform::repo::notify(
        &state.pool,
        party.host_id,
        &state.hub,
        "party_invite",
        "Party joined",
        &format!("{} joined {}", auth.user().display_name, party.title),
        serde_json::json!({ "party_id": party.id }),
    )
    .await;

    // Auto-join the party chat conversation.
    if let Some(conversation) =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM conversations WHERE party_id = $1")
            .bind(party.id)
            .fetch_optional(&state.pool)
            .await?
    {
        let _ = sqlx::query(
            "INSERT INTO conversation_members (conversation_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(conversation)
        .bind(auth.id())
        .execute(&state.pool)
        .await;
    }

    state
        .parties
        .broadcast_member_event(
            party.id,
            "join",
            serde_json::json!({ "user_id": auth.id(), "display_name": auth.user().display_name }),
        )
        .await?;

    Ok(Json(member))
}

#[utoipa::path(post, path = "/api/parties/join-by-code", tag = "parties",
    security(("bearer" = [])),
    request_body = JoinPartyRequest,
    responses((status = 200, body = Party), (status = 404))
)]
pub async fn join_by_code(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<JoinPartyRequest>,
) -> ApiResult<Json<Party>> {
    let code = req
        .join_code
        .ok_or_else(|| ApiError::bad_request("join_code required"))?;
    let party = crate::parties::repo::get_by_code(&state.pool, &code).await?;
    crate::parties::repo::join(&state.pool, party.id, auth.id()).await?;
    Ok(Json(party))
}

#[utoipa::path(post, path = "/api/parties/{party_id}/leave", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn leave_party(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
) -> ApiResult<StatusCode> {
    let party = crate::parties::repo::get(&state.pool, path.party_id).await?;
    if party.host_id == auth.id() {
        return Err(ApiError::bad_request(
            "hosts end the party instead of leaving",
        ));
    }
    crate::parties::repo::leave(&state.pool, party.id, auth.id()).await?;
    state
        .parties
        .broadcast_member_event(
            party.id,
            "leave",
            serde_json::json!({ "user_id": auth.id() }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/parties/{party_id}", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn end_party(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
) -> ApiResult<StatusCode> {
    let party = crate::parties::repo::get(&state.pool, path.party_id).await?;
    if party.host_id != auth.id() && !matches!(auth.user().role, crate::users::models::Role::Admin)
    {
        return Err(ApiError::forbidden("only the host can end the party"));
    }
    crate::parties::repo::end(&state.pool, party.host_id, party.id).await?;
    state.hub.publish_or_create(
        &format!("party:{}", party.id),
        serde_json::json!({ "type": "party.ended", "data": { "party_id": party.id } }).to_string(),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/parties/{party_id}", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = Party), (status = 403), (status = 404))
)]
pub async fn get_party(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
) -> ApiResult<Json<serde_json::Value>> {
    let party = crate::parties::repo::get(&state.pool, path.party_id).await?;
    let member = crate::parties::repo::is_member(&state.pool, party.id, auth.id()).await?;
    let is_host = party.host_id == auth.id();
    if !member && !is_host && party.visibility == "private" {
        return Err(ApiError::forbidden("private party"));
    }
    let members = crate::parties::repo::members(&state.pool, party.id).await?;
    let sync = state.parties.sync_state(&party);

    Ok(Json(serde_json::json!({
        "party": party,
        "members": members,
        "is_host": is_host,
        "can_control": is_host || members.iter().any(|m| m.user_id == auth.id() && m.role == "cohost"),
        "avg_latency_ms": state.parties.average_latency_ms(party.id),
        "state": sync,
    })))
}

#[utoipa::path(get, path = "/api/parties/{party_id}/members", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = [PartyMember]))
)]
pub async fn list_members(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
) -> ApiResult<Json<Vec<PartyMember>>> {
    if !crate::parties::repo::is_member(&state.pool, path.party_id, auth.id()).await? {
        return Err(ApiError::forbidden("not a party member"));
    }
    Ok(Json(
        crate::parties::repo::members(&state.pool, path.party_id).await?,
    ))
}

#[utoipa::path(post, path = "/api/parties/{party_id}/control", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    request_body = PartyControlRequest,
    responses((status = 200, body = PartySyncState), (status = 403))
)]
pub async fn control(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
    Json(req): Json<PartyControlRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let party = crate::parties::repo::get(&state.pool, path.party_id).await?;
    if req.action == "sync_tracks" {
        if party.host_id != auth.id() {
            return Err(ApiError::forbidden("only the host can sync tracks"));
        }
        state.hub.publish_or_create(
            &format!("party:{}", party.id),
            serde_json::json!({
                "type": "track.sync",
                "data": {
                    "audio_stream_id": req.audio_stream_id,
                    "subtitle_stream_id": req.subtitle_stream_id,
                }
            })
            .to_string(),
        );
        return Ok(Json(serde_json::json!({ "synced": "tracks" })));
    }

    let sync = state
        .parties
        .apply_control(auth.id(), &req.action, req.position_seconds, party)
        .await?;
    Ok(Json(serde_json::to_value(sync)?))
}

#[utoipa::path(get, path = "/api/parties/{party_id}/state", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = PartySyncState), (status = 403))
)]
pub async fn current_state(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
) -> ApiResult<Json<PartySyncState>> {
    if !crate::parties::repo::is_member(&state.pool, path.party_id, auth.id()).await? {
        return Err(ApiError::forbidden("not a party member"));
    }
    Ok(Json(state.parties.current_state(path.party_id).await?))
}

/// Reconnect flow: replay events missed since `after`, then poll /state or the ws room.
#[utoipa::path(get, path = "/api/parties/{party_id}/events", tag = "parties",
    params(PartyIdPath, PartyEventsQuery),
    security(("bearer" = [])),
    responses((status = 200, body = [PartyEvent]), (status = 403))
)]
pub async fn events(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
    axum::extract::Query(query): axum::extract::Query<PartyEventsQuery>,
) -> ApiResult<Json<Vec<PartyEvent>>> {
    if !crate::parties::repo::is_member(&state.pool, path.party_id, auth.id()).await? {
        return Err(ApiError::forbidden("not a party member"));
    }
    let events =
        crate::parties::repo::events_after(&state.pool, path.party_id, query.after, query.limit)
            .await?;
    Ok(Json(events))
}

#[utoipa::path(get, path = "/api/me/parties", tag = "parties",
    security(("bearer" = [])),
    responses((status = 200, body = [Party]))
)]
pub async fn my_parties(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<Party>>> {
    Ok(Json(
        crate::parties::repo::my_parties(&state.pool, auth.id()).await?,
    ))
}

#[utoipa::path(post, path = "/api/parties/{party_id}/reactions", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    request_body = crate::parties::models::PartyReactionRequest,
    responses((status = 202), (status = 403))
)]
pub async fn add_reaction(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
    Json(req): Json<crate::parties::models::PartyReactionRequest>,
) -> ApiResult<axum::http::StatusCode> {
    if !crate::parties::repo::is_member(&state.pool, path.party_id, auth.id()).await? {
        return Err(ApiError::forbidden("not a party member"));
    }
    crate::parties::repo::add_reaction(
        &state.pool,
        path.party_id,
        auth.id(),
        req.second_offset,
        &req.emoji,
    )
    .await?;
    state.hub.publish_or_create(
        &format!("party:{}", path.party_id),
        serde_json::json!({
            "type": "reaction",
            "data": { "user_id": auth.id(), "emoji": req.emoji, "second_offset": req.second_offset }
        })
        .to_string(),
    );
    Ok(axum::http::StatusCode::ACCEPTED)
}

/// Timestamped reaction heatmap for the episode/movie — the social replay layer.
#[utoipa::path(get, path = "/api/parties/{party_id}/reactions/heatmap", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = [crate::parties::models::ReactionBucket]), (status = 403))
)]
pub async fn reaction_heatmap(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
) -> ApiResult<Json<Vec<crate::parties::models::ReactionBucket>>> {
    if !crate::parties::repo::is_member(&state.pool, path.party_id, auth.id()).await? {
        return Err(ApiError::forbidden("not a party member"));
    }
    let rows = crate::parties::repo::reaction_heatmap(&state.pool, path.party_id).await?;
    Ok(Json(
        rows.into_iter()
            .map(
                |(bucket_start, emoji, count)| crate::parties::models::ReactionBucket {
                    bucket_start,
                    emoji,
                    count,
                },
            )
            .collect(),
    ))
}

fn constant_time_eq_str(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct TransferHostRequest {
    pub user_id: Uuid,
}

#[utoipa::path(post, path = "/api/parties/{party_id}/transfer", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    request_body = TransferHostRequest,
    responses((status = 200), (status = 403))
)]
pub async fn transfer_host(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
    Json(req): Json<TransferHostRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let party = crate::parties::repo::get(&state.pool, path.party_id).await?;
    if party.host_id != auth.id() {
        return Err(ApiError::forbidden("only the host can transfer"));
    }
    if !crate::parties::repo::is_member(&state.pool, party.id, req.user_id).await? {
        return Err(ApiError::bad_request("target must be a member"));
    }
    sqlx::query("UPDATE parties SET host_id = $2 WHERE id = $1")
        .bind(party.id)
        .bind(req.user_id)
        .execute(&state.pool)
        .await?;
    sqlx::query("UPDATE party_members SET role = 'cohost' WHERE party_id = $1 AND user_id = $2")
        .bind(party.id)
        .bind(auth.id())
        .execute(&state.pool)
        .await?;
    sqlx::query("UPDATE party_members SET role = 'host' WHERE party_id = $1 AND user_id = $2")
        .bind(party.id)
        .bind(req.user_id)
        .execute(&state.pool)
        .await?;
    Ok(Json(serde_json::json!({ "host": req.user_id })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PromoteRequest {
    pub user_id: Uuid,
}

#[utoipa::path(post, path = "/api/parties/{party_id}/promote", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    request_body = PromoteRequest,
    responses((status = 200))
)]
pub async fn promote_cohost(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
    Json(req): Json<PromoteRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let party = crate::parties::repo::get(&state.pool, path.party_id).await?;
    if party.host_id != auth.id() {
        return Err(ApiError::forbidden("only the host can promote co-hosts"));
    }
    sqlx::query(
        "INSERT INTO party_members (party_id, user_id, role) VALUES ($1, $2, 'cohost')
         ON CONFLICT (party_id, user_id) DO UPDATE SET role = 'cohost'",
    )
    .bind(party.id)
    .bind(req.user_id)
    .execute(&state.pool)
    .await?;
    Ok(Json(serde_json::json!({ "cohost": req.user_id })))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct LatencyReport {
    pub rtt_ms: f64,
    #[serde(default)]
    pub drift_seconds: f64,
}

#[utoipa::path(post, path = "/api/parties/{party_id}/latency", tag = "parties",
    params(PartyIdPath),
    security(("bearer" = [])),
    request_body = LatencyReport,
    responses((status = 202))
)]
pub async fn report_latency(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<PartyIdPath>,
    Json(req): Json<LatencyReport>,
) -> ApiResult<axum::http::StatusCode> {
    if !crate::parties::repo::is_member(&state.pool, path.party_id, auth.id()).await? {
        return Err(ApiError::forbidden("not a party member"));
    }
    state
        .parties
        .report_latency(path.party_id, req.rtt_ms, req.drift_seconds)
        .await;
    Ok(axum::http::StatusCode::ACCEPTED)
}
