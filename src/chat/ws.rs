use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query as AxQuery, State};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::chat::hub::{user_room, GLOBAL_ROOM, Hub};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct WsAuthQuery {
    #[serde(default)]
    pub token: Option<String>,
}

struct Connection {
    user_id: Uuid,
    display_name: String,
    sink: mpsc::UnboundedSender<Message>,
    rooms: HashMap<String, tokio::task::JoinHandle<()>>,
    /// Last sign of life from the client (pong or any inbound frame).
    last_seen: std::time::Instant,
}

const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
/// A connection silent for 2.5 intervals is considered dead and reaped.
const DEAD_AFTER: std::time::Duration = std::time::Duration::from_secs(75);

fn room_event(room: &str, raw: String) -> Message {
    let data = serde_json::from_str::<Value>(&raw).unwrap_or(Value::String(raw));
    let envelope = serde_json::json!({ "type": "event", "room": room, "data": data });
    Message::text(envelope.to_string())
}

async fn forward_room(hub: Arc<Hub>, room: String, sink: mpsc::UnboundedSender<Message>) {
    let mut rx = hub.subscribe(&room);
    loop {
        match rx.recv().await {
            Ok(payload) => {
                if sink.send(room_event(&room, payload)).is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!(%room, skipped = n, "slow consumer lagged");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

pub async fn gateway(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    AxQuery(query): AxQuery<WsAuthQuery>,
) -> Response {
    let authed = match query.token.as_deref() {
        Some(token) if !token.is_empty() => state.auth.resolve_session(token).await.ok(),
        _ => None,
    };

    let Some(ctx) = authed else {
        return ApiError::Unauthorized.into_response();
    };

    ws.on_upgrade(move |socket| {
        handle_socket(state, ctx.user.id, ctx.user.display_name.clone(), socket)
    })
}

async fn handle_socket(
    state: Arc<AppState>,
    user_id: Uuid,
    display_name: String,
    socket: WebSocket,
) {
    let (mut ws_sink, mut ws_source) = socket.split();
    let (sink_tx, mut sink_rx) = mpsc::unbounded_channel::<Message>();
    let hub = state.hub.clone();

    let mut conn = Connection {
        user_id,
        display_name: display_name.clone(),
        sink: sink_tx.clone(),
        rooms: HashMap::new(),
        last_seen: std::time::Instant::now(),
    };

    hub.connect(user_id);
    subscribe_room(&mut conn, hub.clone(), GLOBAL_ROOM.to_string());
    // Personal room: notifications, security flags and AI notices land here.
    subscribe_room(&mut conn, hub.clone(), user_room(&user_id));

    let welcome = serde_json::json!({
        "type": "hello",
        "room": GLOBAL_ROOM,
        "data": {
            "user_id": user_id,
            "online_users": hub.online_users(),
        }
    });
    let _ = sink_tx.send(Message::text(welcome.to_string()));

    let result = run_loop(
        &state,
        &mut conn,
        &hub,
        &mut ws_sink,
        &mut sink_rx,
        &mut ws_source,
    )
    .await;

    for (_, task) in conn.rooms.drain() {
        task.abort();
    }
    hub.disconnect(user_id);
    metrics::gauge!("ws_connections").decrement(1.0);

    if let Err(err) = result {
        tracing::debug!(%user_id, error = %err, "websocket closed");
    }
}

type WsResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn run_loop(
    state: &Arc<AppState>,
    conn: &mut Connection,
    hub: &Arc<Hub>,
    ws_sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    sink_rx: &mut mpsc::UnboundedReceiver<Message>,
    ws_source: &mut futures_util::stream::SplitStream<WebSocket>,
) -> WsResult {
    metrics::gauge!("ws_connections").increment(1.0);

    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await; // first tick fires immediately — consume it

    loop {
        tokio::select! {
            outbound = sink_rx.recv() => {
                match outbound {
                    Some(message) => {
                        if ws_sink.send(message).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            inbound = ws_source.next() => {
                match inbound {
                    Some(Ok(Message::Text(text))) => {
                        conn.last_seen = std::time::Instant::now();
                        if !handle_client_event(state, conn, hub, &text).await {
                            continue;
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        conn.last_seen = std::time::Instant::now();
                        let _ = conn.sink.send(Message::Pong(data));
                    }
                    Some(Ok(Message::Pong(_))) => {
                        conn.last_seen = std::time::Instant::now();
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(Box::new(err)),
                }
            }
            _ = heartbeat.tick() => {
                if conn.last_seen.elapsed() > DEAD_AFTER {
                    tracing::debug!(%conn.user_id, "websocket reaped (no activity)");
                    break;
                }
                // Server-initiated liveness probe; browsers answer automatically.
                let _ = conn.sink.send(Message::Ping(bytes::Bytes::new()));
            }
        }
    }
    Ok(())
}

fn subscribe_room(conn: &mut Connection, hub: Arc<Hub>, room: String) {
    if conn.rooms.contains_key(&room) {
        return;
    }
    let sink = conn.sink.clone();
    let task = tokio::spawn(forward_room(hub, room.clone(), sink));
    conn.rooms.insert(room, task);
}

fn unsubscribe_room(conn: &mut Connection, room: &str) {
    if let Some(task) = conn.rooms.remove(room) {
        task.abort();
    }
}

async fn authorize_room(state: &Arc<AppState>, user_id: Uuid, room: &str) -> bool {
    if room == GLOBAL_ROOM {
        return true;
    }
    // Personal room: strictly your own.
    if let Some(id) = room.strip_prefix("user:") {
        return Uuid::parse_str(id).is_ok_and(|uid| uid == user_id);
    }
    if let Some(id) = room.strip_prefix("conv:") {
        let Ok(cid) = Uuid::parse_str(id) else {
            return false;
        };
        return crate::chat::repo::is_member(&state.pool, cid, user_id)
            .await
            .unwrap_or(false);
    }
    if let Some(id) = room.strip_prefix("party:") {
        let Ok(pid) = Uuid::parse_str(id) else {
            return false;
        };
        return crate::parties::repo::is_member(&state.pool, pid, user_id)
            .await
            .unwrap_or(false);
    }
    false
}

/// Returns true when the connection should keep going.
async fn handle_client_event(
    state: &Arc<AppState>,
    conn: &mut Connection,
    hub: &Arc<Hub>,
    text: &str,
) -> bool {
    let event: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => {
            send_error(conn.sink.clone(), "invalid json");
            return true;
        }
    };

    let kind = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match kind {
        "ping" => {
            let _ = conn.sink.send(Message::text(
                serde_json::json!({"type":"pong","ts": chrono::Utc::now().timestamp_millis()})
                    .to_string(),
            ));
        }
        "subscribe" | "unsubscribe" => {
            let Some(room) = event
                .get("room")
                .and_then(|r| r.as_str())
                .map(str::to_string)
            else {
                send_error(conn.sink.clone(), "room required");
                return true;
            };
            if !authorize_room(state, conn.user_id, &room).await {
                send_error(conn.sink.clone(), "not allowed to join this room");
                return true;
            }
            if kind == "subscribe" {
                subscribe_room(conn, hub.clone(), room);
            } else {
                unsubscribe_room(conn, &room);
            }
        }
        "typing" => {
            let Some(room) = event.get("room").and_then(|r| r.as_str()) else {
                return true;
            };
            if authorize_room(state, conn.user_id, room).await {
                let payload = serde_json::json!({
                    "type": "typing",
                    "data": { "user_id": conn.user_id, "display_name": conn.display_name }
                });
                hub.publish(room, payload.to_string());
            }
        }
        "message.send" => {
            let Some(room) = event.get("room").and_then(|r| r.as_str()) else {
                send_error(conn.sink.clone(), "room required");
                return true;
            };
            let Some(body) = event.get("body").and_then(|b| b.as_str()) else {
                send_error(conn.sink.clone(), "body required");
                return true;
            };
            let Some(conversation_id) = room
                .strip_prefix("conv:")
                .and_then(|id| Uuid::parse_str(id).ok())
            else {
                send_error(conn.sink.clone(), "messages go to conv:<uuid> rooms");
                return true;
            };
            if !crate::chat::repo::is_member(&state.pool, conversation_id, conn.user_id)
                .await
                .unwrap_or(false)
            {
                send_error(conn.sink.clone(), "not a member");
                return true;
            }
            match crate::chat::repo::send_message(
                &state.pool,
                conversation_id,
                conn.user_id,
                &conn.display_name,
                body,
            )
            .await
            {
                Ok(message) => {
                    hub.publish(room, message.event().to_string());
                    let ack =
                        serde_json::json!({ "type": "message.sent", "data": { "id": message.id } });
                    let _ = conn.sink.send(Message::text(ack.to_string()));
                }
                Err(err) => send_error(conn.sink.clone(), &err.to_string()),
            }
        }
        "read" => {
            if let Some(room) = event
                .get("room")
                .and_then(|r| r.as_str())
                .and_then(|r| r.strip_prefix("conv:"))
            {
                if let Ok(cid) = Uuid::parse_str(room) {
                    let _ = crate::chat::repo::mark_read(&state.pool, cid, conn.user_id).await;
                }
            }
        }
        "reaction" => {
            let Some(room) = event.get("room").and_then(|r| r.as_str()) else {
                return true;
            };
            let Some(party_id) = room
                .strip_prefix("party:")
                .and_then(|id| Uuid::parse_str(id).ok())
            else {
                send_error(conn.sink.clone(), "reactions belong to party:<uuid> rooms");
                return true;
            };
            if !authorize_room(state, conn.user_id, room).await {
                return true;
            }
            let emoji = event
                .get("emoji")
                .and_then(|e| e.as_str())
                .filter(|e| !e.is_empty() && e.chars().count() <= 8)
                .unwrap_or("❤")
                .to_string();
            let offset = event
                .get("second_offset")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .clamp(0, 86_400 * 2) as i32;
            let _ = crate::parties::repo::add_reaction(
                &state.pool,
                party_id,
                conn.user_id,
                offset,
                &emoji,
            )
            .await;
            hub.publish(
                room,
                serde_json::json!({
                    "type": "reaction",
                    "data": { "user_id": conn.user_id, "emoji": emoji, "second_offset": offset }
                })
                .to_string(),
            );
        }
        "presence.update" => {
            // User presence (changeset 5): online | idle | watch_party | dnd | invisible
            let Some(status) = event.get("status").and_then(|s| s.as_str()) else {
                send_error(conn.sink.clone(), "status required");
                return true;
            };
            const STATUSES: &[&str] = &["online", "idle", "watch_party", "dnd", "invisible"];
            if !STATUSES.contains(&status) {
                send_error(conn.sink.clone(), "invalid status");
                return true;
            }
            let text = event
                .get("custom_text")
                .and_then(|t| t.as_str())
                .map(|t| t.chars().take(120).collect::<String>())
                .unwrap_or_default();
            let _ = sqlx::query(
                "INSERT INTO presence (user_id, status, custom_text) VALUES ($1, $2, $3) \
                 ON CONFLICT (user_id) DO UPDATE SET status = EXCLUDED.status, \
                    custom_text = EXCLUDED.custom_text, updated_at = now()",
            )
            .bind(conn.user_id)
            .bind(status)
            .bind(text)
            .execute(&state.pool)
            .await;
            // Invisible users get stored but never broadcast.
            if status != "invisible" {
                hub.publish(
                    GLOBAL_ROOM,
                    serde_json::json!({
                        "type": "presence.update",
                        "data": { "user_id": conn.user_id, "status": status }
                    })
                    .to_string(),
                );
            }
        }
        other => send_error(conn.sink.clone(), &format!("unknown event type '{other}'")),
    }
    true
}

fn send_error(sink: mpsc::UnboundedSender<Message>, message: &str) {
    let _ = sink.send(Message::text(
        serde_json::json!({ "type": "error", "data": { "message": message } }).to_string(),
    ));
}
