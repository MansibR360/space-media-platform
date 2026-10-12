use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use tokio::sync::broadcast;
use uuid::Uuid;

pub const GLOBAL_ROOM: &str = "__global__";
const CHANNEL_CAPACITY: usize = 1024;

/// Canonical personal room name — auto-subscribed on every socket connect.
pub fn user_room(user_id: &Uuid) -> String {
    format!("user:{user_id}")
}

#[derive(Debug)]
struct Room {
    tx: broadcast::Sender<String>,
}

/// Central message bus for websocket connections.
///
/// Rooms are keyed by strings like `conv:{uuid}` or `party:{uuid}`;
/// every connected socket also receives the `__global__` room.
#[derive(Debug, Default)]
pub struct Hub {
    rooms: DashMap<String, Room>,
    presence: DashMap<Uuid, AtomicU64>,
    online_counter: AtomicU64,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    fn room(&self, key: &str) -> broadcast::Receiver<String> {
        if let Some(room) = self.rooms.get(key) {
            return room.tx.subscribe();
        }
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        let rx = tx.subscribe();
        self.rooms.insert(key.to_string(), Room { tx });
        rx
    }

    pub fn subscribe(&self, key: &str) -> broadcast::Receiver<String> {
        self.room(key)
    }

    pub fn publish(&self, key: &str, payload: String) {
        if let Some(room) = self.rooms.get(key) {
            let _ = room.tx.send(payload);
        }
    }

    /// Publish to a room, creating it if needed (used for presence + first event).
    pub fn publish_or_create(&self, key: &str, payload: String) {
        if self.publish_if_exists(key, payload.clone()) {
            return;
        }
        let _ = self.room(key);
        self.publish_if_exists(key, payload);
    }

    fn publish_if_exists(&self, key: &str, payload: String) -> bool {
        match self.rooms.get(key) {
            Some(room) => {
                let _ = room.tx.send(payload);
                true
            }
            None => false,
        }
    }

    pub fn connect(&self, user_id: Uuid) -> usize {
        let entry = self
            .presence
            .entry(user_id)
            .or_insert_with(|| AtomicU64::new(0));
        let count = entry.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            let total = self.online_counter.fetch_add(1, Ordering::SeqCst) + 1;
            self.broadcast_presence(user_id, true, total as usize);
        }
        count as usize
    }

    pub fn disconnect(&self, user_id: Uuid) -> usize {
        if let Some(entry) = self.presence.get(&user_id) {
            let current = entry.load(Ordering::SeqCst);
            if current > 0 {
                let remaining = entry.fetch_sub(1, Ordering::SeqCst) - 1;
                if remaining == 0 {
                    drop(entry);
                    self.presence.remove(&user_id);
                    let total = self
                        .online_counter
                        .fetch_sub(1, Ordering::SeqCst)
                        .saturating_sub(1);
                    self.broadcast_presence(user_id, false, total as usize);
                    return 0;
                }
                return remaining as usize;
            }
        }
        0
    }

    fn broadcast_presence(&self, user_id: Uuid, online: bool, online_count: usize) {
        let payload = serde_json::json!({
            "type": "presence",
            "room": GLOBAL_ROOM,
            "data": {
                "user_id": user_id,
                "online": online,
                "online_count": online_count,
            }
        });
        self.publish(GLOBAL_ROOM, payload.to_string());
    }

    pub fn online_users(&self) -> Vec<Uuid> {
        self.presence.iter().map(|item| *item.key()).collect()
    }

    #[allow(dead_code)]
    pub fn is_online(&self, user_id: Uuid) -> bool {
        self.presence
            .get(&user_id)
            .map(|c| c.load(Ordering::SeqCst) > 0)
            .unwrap_or(false)
    }

    #[allow(dead_code)]
    pub fn room_names(&self) -> Vec<String> {
        self.rooms.iter().map(|r| r.key().clone()).collect()
    }

    #[allow(dead_code)]
    pub fn stats(&self) -> HashMap<String, usize> {
        HashMap::from([
            ("rooms".into(), self.rooms.len()),
            ("online_users".into(), self.online_users().len()),
        ])
    }
}
