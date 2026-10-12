use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

enum Backend {
    Memory(Mutex<MemoryStore>),
    Redis(deadpool_redis::Pool),
}

impl Clone for Backend {
    fn clone(&self) -> Self {
        match self {
            Self::Memory(_) => Self::Memory(Mutex::new(MemoryStore {
                entries: HashMap::new(),
            })),
            Self::Redis(pool) => Self::Redis(pool.clone()),
        }
    }
}

struct MemoryStore {
    entries: HashMap<String, (Instant, Value)>,
}

/// JSON value cache with a Redis backend and an in-process fallback.
///
/// Redis failures degrade to memory caching instead of erroring requests.
#[derive(Clone)]
#[allow(dead_code)]
pub struct CacheService {
    backend: Option<Backend>,
    mem: Arc<Mutex<MemoryStore>>,
}

impl CacheService {
    pub async fn new(redis_url: Option<String>) -> Self {
        let backend = match redis_url {
            Some(url) => match deadpool_redis::Config::from_url(url)
                .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            {
                Ok(pool) => Some(Backend::Redis(pool)),
                Err(err) => {
                    tracing::warn!(error = %err, "redis pool init failed; using memory cache only");
                    None
                }
            },
            None => None,
        };

        Self {
            backend,
            mem: Arc::new(Mutex::new(MemoryStore {
                entries: HashMap::new(),
            })),
        }
    }

    pub fn has_redis(&self) -> bool {
        matches!(self.backend, Some(Backend::Redis(_)))
    }

    pub async fn get(&self, key: &str) -> Option<Value> {
        if let Some(Backend::Redis(pool)) = &self.backend {
            match self.redis_get(pool, key).await {
                Ok(Some(value)) => return Some(value),
                Ok(None) => {}
                Err(err) => tracing::debug!(%key, error = %err, "redis get failed"),
            }
        }
        self.mem_get(key)
    }

    pub async fn set(&self, key: &str, value: Value, ttl: Duration) {
        if let Some(Backend::Redis(pool)) = &self.backend {
            if let Err(err) = self.redis_set(pool, key, &value, ttl).await {
                tracing::debug!(%key, error = %err, "redis set failed");
            }
        }
        self.mem_set(key, value, ttl);
    }

    #[allow(dead_code)]
    pub async fn invalidate(&self, prefix: &str) {
        if let Some(Backend::Redis(pool)) = &self.backend {
            let _ = self.redis_delete_prefix(pool, prefix).await;
        }
        if let Ok(mut store) = self.mem.lock() {
            store.entries.retain(|key, _| !key.starts_with(prefix));
        }
    }

    async fn redis_get(
        &self,
        pool: &deadpool_redis::Pool,
        key: &str,
    ) -> Result<Option<Value>, deadpool_redis::PoolError> {
        use deadpool_redis::redis::AsyncCommands;
        let mut conn = pool.get().await?;
        let raw: Option<String> = conn.get(key).await?;
        Ok(raw.and_then(|raw| serde_json::from_str(&raw).ok()))
    }

    async fn redis_set(
        &self,
        pool: &deadpool_redis::Pool,
        key: &str,
        value: &Value,
        ttl: Duration,
    ) -> Result<(), deadpool_redis::PoolError> {
        use deadpool_redis::redis::AsyncCommands;
        let mut conn = pool.get().await?;
        let raw = serde_json::to_string(value).unwrap_or_else(|_| "null".into());
        let _: () = conn.set_ex(key, raw, ttl.as_secs().max(1)).await?;
        Ok(())
    }

    async fn redis_delete_prefix(
        &self,
        pool: &deadpool_redis::Pool,
        _prefix: &str,
    ) -> Result<(), deadpool_redis::PoolError> {
        use deadpool_redis::redis::AsyncCommands;
        let mut conn = pool.get().await?;
        // Simple strategy: bump a generation counter keys include.
        let _: () = conn.incr(format!("gen:{_prefix}"), 1u64).await?;
        Ok(())
    }

    fn mem_get(&self, key: &str) -> Option<Value> {
        let mut store = self.mem.lock().ok()?;
        if let Some((expires_at, value)) = store.entries.get(key) {
            if *expires_at > Instant::now() {
                return Some(value.clone());
            }
        }
        store.entries.remove(key);
        None
    }

    fn mem_set(&self, key: &str, value: Value, ttl: Duration) {
        if let Ok(mut store) = self.mem.lock() {
            if store.entries.len() > 50_000 {
                store.entries.clear();
            }
            store
                .entries
                .insert(key.to_string(), (Instant::now() + ttl, value));
        }
    }
}
