use std::sync::Arc;

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::chat::hub::{GLOBAL_ROOM, Hub};
use crate::error::{ApiError, ApiResult};
use crate::parties::models::Party;

/// Thin coordination layer on top of the database-backed party state.
///
/// The DB row is the single source of truth; the hub only fans events out to
/// websocket subscribers of the `party:{id}` room.
pub struct PartyManager {
    pool: PgPool,
    hub: Arc<Hub>,
    /// rolling latency samples per party: (sum_ms, count)
    latency: dashmap::DashMap<Uuid, (f64, u64)>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PartySyncState {
    pub party_id: Uuid,
    pub seq: i64,
    pub playing: bool,
    pub position_seconds: f64,
    pub server_time_ms: i64,
    pub server_position_seconds: f64,
}

impl PartyManager {
    pub fn new(pool: PgPool, hub: Arc<Hub>) -> Self {
        Self {
            pool,
            hub,
            latency: dashmap::DashMap::new(),
        }
    }

    /// Clients report rtt_ms + drift_seconds periodically; the manager keeps a
    /// rolling average surfaced through /state so hosts can see desync.
    pub async fn report_latency(&self, party_id: Uuid, rtt_ms: f64, drift_seconds: f64) {
        let mut entry = self.latency.entry(party_id).or_insert((0.0, 0));
        entry.0 += rtt_ms;
        entry.1 += 1;
        let avg_rtt = entry.0 / entry.1 as f64;
        let _ = drift_seconds; // drift is folded into client-side correction
        let _ = avg_rtt;
    }

    pub fn average_latency_ms(&self, party_id: Uuid) -> Option<f64> {
        self.latency.get(&party_id).map(|entry| {
            let (sum, n) = *entry;
            if n > 0 { sum / n as f64 } else { 0.0 }
        })
    }

    fn room(&self, party_id: Uuid) -> String {
        format!("party:{party_id}")
    }

    /// Host or cohost can control playback.
    pub async fn require_control(&self, party: &Party, user_id: Uuid) -> ApiResult<()> {
        if party.host_id == user_id {
            return Ok(());
        }
        let role = crate::parties::repo::member_role(&self.pool, party.id, user_id).await?;
        match role.as_deref() {
            Some("cohost") => Ok(()),
            _ => Err(ApiError::forbidden("only the host can control playback")),
        }
    }

    pub async fn apply_control(
        &self,
        actor: Uuid,
        action: &str,
        position: Option<f64>,
        party: Party,
    ) -> ApiResult<PartySyncState> {
        let (playing, position) = match action {
            "play" => {
                self.require_control(&party, actor).await?;
                (true, party.projected_position())
            }
            "pause" => {
                self.require_control(&party, actor).await?;
                (false, party.projected_position())
            }
            "seek" => {
                self.require_control(&party, actor).await?;
                let target = position
                    .ok_or_else(|| ApiError::bad_request("position_seconds required for seek"))?;
                (party.is_playing, target.max(0.0))
            }
            other => {
                return Err(ApiError::bad_request(format!(
                    "action must be play, pause or seek; got '{other}'"
                )));
            }
        };

        let updated =
            crate::parties::repo::apply_state_change(&self.pool, party.id, playing, position)
                .await?;

        crate::parties::repo::record_event(
            &self.pool,
            party.id,
            Some(actor),
            action,
            serde_json::json!({ "playing": playing, "position": position }),
        )
        .await?;

        let sync = self.sync_state(&updated);
        self.hub.publish_or_create(
            &self.room(party.id),
            serde_json::json!({
                "type": "state.sync",
                "data": sync
            })
            .to_string(),
        );
        Ok(sync)
    }

    pub async fn broadcast_member_event(
        &self,
        party_id: Uuid,
        event_kind: &str,
        payload: serde_json::Value,
    ) -> ApiResult<()> {
        crate::parties::repo::record_event(&self.pool, party_id, None, event_kind, payload.clone())
            .await?;
        self.hub.publish_or_create(
            &self.room(party_id),
            serde_json::json!({ "type": format!("{event_kind}.event"), "data": payload })
                .to_string(),
        );
        Ok(())
    }

    pub fn sync_state(&self, party: &Party) -> PartySyncState {
        PartySyncState {
            party_id: party.id,
            seq: party.last_event_seq,
            playing: party.is_playing,
            position_seconds: party.position_seconds,
            server_time_ms: chrono::Utc::now().timestamp_millis(),
            server_position_seconds: party.projected_position(),
        }
    }

    pub async fn current_state(&self, party_id: Uuid) -> ApiResult<PartySyncState> {
        let party = crate::parties::repo::get(&self.pool, party_id).await?;
        Ok(self.sync_state(&party))
    }

    #[allow(dead_code)]
    pub async fn announce_globally(&self, message: &str) {
        self.hub.publish_or_create(
            GLOBAL_ROOM,
            serde_json::json!({ "type": "announcement", "data": { "message": message } })
                .to_string(),
        );
    }
}
