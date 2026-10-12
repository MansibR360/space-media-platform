use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::middleware::CurrentUser;
use crate::chat::models::{
    ChatMessage, Conversation, ConversationSummary, CreateConversationRequest, SendMessageRequest,
};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Debug, Deserialize, IntoParams)]
pub struct ConversationIdPath {
    pub conversation_id: Uuid,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct MessageIdPath {
    pub message_id: Uuid,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct MessagePageQuery {
    // NOT #[serde(flatten)]: serde_urlencoded hands flattened fields through as
    // strings, so `?limit=40` fails with "invalid type: string \"40\", expected
    // i64". Same trap as ListMediaQuery. Inlined, with `page()` rebuilding the
    // struct so call sites keep working.
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl MessagePageQuery {
    pub fn page(&self) -> crate::pagination::PageQuery {
        crate::pagination::PageQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MemberAdded {
    pub added: bool,
}

#[utoipa::path(get, path = "/api/chat/conversations", tag = "chat",
    security(("bearer" = [])),
    responses((status = 200, body = [ConversationSummary]))
)]
pub async fn list_conversations(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
) -> ApiResult<Json<Vec<ConversationSummary>>> {
    Ok(Json(
        crate::chat::repo::list_conversations(&state.pool, auth.id()).await?,
    ))
}

#[utoipa::path(post, path = "/api/chat/conversations", tag = "chat",
    security(("bearer" = [])),
    request_body = CreateConversationRequest,
    responses((status = 201, body = Conversation), (status = 400))
)]
pub async fn create_conversation(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Json(req): Json<CreateConversationRequest>,
) -> ApiResult<(StatusCode, Json<Conversation>)> {
    let conversation = match req.kind.as_str() {
        "direct" => {
            let other = *req
                .member_ids
                .first()
                .ok_or_else(|| ApiError::bad_request("member_ids must contain the other user"))?;
            crate::chat::repo::create_direct(&state.pool, auth.id(), other).await?
        }
        "group" => {
            crate::chat::repo::create_group(
                &state.pool,
                auth.id(),
                req.title.as_deref().unwrap_or("New group"),
                &req.member_ids,
            )
            .await?
        }
        other => {
            return Err(ApiError::bad_request(format!(
                "kind must be 'direct' or 'group', got '{other}'"
            )));
        }
    };
    Ok((StatusCode::CREATED, Json(conversation)))
}

#[utoipa::path(get, path = "/api/chat/conversations/{conversation_id}/messages", tag = "chat",
    params(ConversationIdPath, MessagePageQuery),
    security(("bearer" = [])),
    responses((status = 200, body = [ChatMessage]), (status = 403))
)]
pub async fn list_messages(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<ConversationIdPath>,
    Query(query): Query<MessagePageQuery>,
) -> ApiResult<Json<Vec<ChatMessage>>> {
    if !crate::chat::repo::is_member(&state.pool, path.conversation_id, auth.id()).await? {
        return Err(ApiError::forbidden("not a member of this conversation"));
    }
    let limit = query.page().clamp_limit();
    let before = query
        .cursor
        .as_deref()
        .and_then(|c| crate::pagination::decode_cursor(c).ok())
        .and_then(|v| {
            v.get("t")
                .and_then(|t| t.as_str())
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        })
        .map(|t| t.with_timezone(&chrono::Utc));

    let messages = crate::chat::repo::messages_page(
        &state.pool,
        path.conversation_id,
        auth.id(),
        before,
        limit,
    )
    .await?;
    Ok(Json(messages))
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct EditMessageRequest {
    pub body: String,
}

/// Edits your own message (soft marker: `edited_at`).
#[utoipa::path(patch, path = "/api/chat/messages/{message_id}", tag = "chat",
    security(("bearer" = [])),
    request_body = EditMessageRequest,
    responses((status = 200, body = ChatMessage), (status = 404))
)]
pub async fn edit_message(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<crate::chat::handlers::MessageIdPath>,
    Json(req): Json<EditMessageRequest>,
) -> ApiResult<Json<ChatMessage>> {
    let message =
        crate::chat::repo::edit_message(&state.pool, path.message_id, auth.id(), &req.body)
            .await?;
    let room = format!("conv:{}", message.conversation_id);
    state.hub.publish(
        &room,
        serde_json::json!({
            "type": "message.edited",
            "data": { "id": message.id, "body": message.body }
        })
        .to_string(),
    );
    Ok(Json(message))
}

/// Deletes a message (owner or staff). Soft delete — history stays auditable.
#[utoipa::path(delete, path = "/api/chat/messages/{message_id}", tag = "chat",
    security(("bearer" = [])),
    responses((status = 204), (status = 404))
)]
pub async fn delete_message(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<crate::chat::handlers::MessageIdPath>,
) -> ApiResult<axum::http::StatusCode> {
    let is_staff = matches!(
        auth.ctx.user.role,
        crate::users::models::Role::Admin | crate::users::models::Role::Moderator
    );
    if !crate::chat::repo::delete_message(&state.pool, path.message_id, auth.id(), is_staff).await?
    {
        return Err(ApiError::not_found("deletable message"));
    }
    metrics::counter!("messages_deleted_total").increment(1);
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/chat/conversations/{conversation_id}/messages", tag = "chat",
    params(ConversationIdPath),
    security(("bearer" = [])),
    request_body = SendMessageRequest,
    responses((status = 201, body = ChatMessage))
)]
pub async fn send_message(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<ConversationIdPath>,
    Json(req): Json<SendMessageRequest>,
) -> ApiResult<(StatusCode, Json<ChatMessage>)> {
    if !crate::chat::repo::is_member(&state.pool, path.conversation_id, auth.id()).await? {
        return Err(ApiError::forbidden("not a member of this conversation"));
    }
    let message = crate::chat::repo::send_message(
        &state.pool,
        path.conversation_id,
        auth.id(),
        auth.user().display_name.as_str(),
        &req.body,
    )
    .await?;

    let room = format!("conv:{}", path.conversation_id);
    state.hub.publish(&room, message.event().to_string());
    Ok((StatusCode::CREATED, Json(message)))
}

#[utoipa::path(post, path = "/api/chat/conversations/{conversation_id}/members/{user_id}", tag = "chat",
    params(ConversationIdPath),
    security(("bearer" = [])),
    responses((status = 200, body = MemberAdded))
)]
pub async fn add_member(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<ConversationMemberPath>,
) -> ApiResult<Json<MemberAdded>> {
    crate::chat::repo::add_member(&state.pool, path.conversation_id, auth.id(), path.user_id)
        .await?;
    Ok(Json(MemberAdded { added: true }))
}

#[utoipa::path(delete, path = "/api/chat/conversations/{conversation_id}/members/me", tag = "chat",
    params(ConversationIdPath),
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn leave_conversation(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<ConversationIdPath>,
) -> ApiResult<StatusCode> {
    crate::chat::repo::remove_member(&state.pool, path.conversation_id, auth.id(), auth.id())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/chat/conversations/{conversation_id}/read", tag = "chat",
    params(ConversationIdPath),
    security(("bearer" = [])),
    responses((status = 204))
)]
pub async fn mark_read(
    State(state): State<Arc<AppState>>,
    auth: CurrentUser,
    Path(path): Path<ConversationIdPath>,
) -> ApiResult<StatusCode> {
    crate::chat::repo::mark_read(&state.pool, path.conversation_id, auth.id()).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ConversationMemberPath {
    pub conversation_id: Uuid,
    pub user_id: Uuid,
}
