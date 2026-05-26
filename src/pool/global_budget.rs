use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use redis::{Commands, Connection};
use uuid::Uuid;

use crate::pool::{NodeId, RequestMeta};

#[derive(Debug, Clone)]
pub struct GlobalBudgetConfig {
    pub redis_url: String,
    pub instance_id: String,
    pub window_secs: u64,
    pub lease_ttl_secs: u64,
    pub max_calls_per_window: u64,
    pub max_tokens_per_window: u64,
    pub max_kb_per_window: u64,
    pub max_concurrent: u32,
    pub cooldown_secs: i64,
}

#[derive(Debug, Clone)]
pub struct GlobalLease {
    pub node_id: NodeId,
    pub lease_id: String,
}

#[derive(Clone)]
pub struct GlobalBudgetRegistry {
    client: redis::Client,
    config: GlobalBudgetConfig,
    connection: Arc<Mutex<Option<Connection>>>,
    local_leases: Arc<Mutex<HashMap<NodeId, Vec<String>>>>,
}

impl GlobalBudgetRegistry {
    pub fn new(config: GlobalBudgetConfig) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(config.redis_url.as_str())?;
        Ok(Self {
            client,
            config,
            connection: Arc::new(Mutex::new(None)),
            local_leases: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    fn with_connection<T>(
        &self,
        action: impl FnOnce(&mut Connection) -> redis::RedisResult<T>,
    ) -> redis::RedisResult<T> {
        let mut cached = self.connection.lock().unwrap();
        if cached.is_none() {
            *cached = Some(self.client.get_connection()?);
        }

        let result = action(cached.as_mut().unwrap());
        if result.is_err() {
            *cached = None;
        }
        result
    }

    pub fn try_acquire(&self, node_id: &str, meta: &RequestMeta) -> Result<GlobalLease, String> {
        let lease_id = format!("{}:{}", self.config.instance_id, Uuid::new_v4());
        let now = chrono::Utc::now();
        let now_secs = now.timestamp();
        let now_ms = now.timestamp_millis();
        let window_secs = self.config.window_secs.max(1);
        let bucket = now_secs / window_secs as i64;
        let budget_key = self.budget_key(node_id, bucket);
        let cooldown_key = self.cooldown_key(node_id);
        let leases_key = self.leases_key(node_id);
        let lease_key = self.lease_key(node_id, &lease_id);
        let lease_ttl_ms = self.config.lease_ttl_secs.saturating_mul(1000).max(1000);
        let lease_expires_at_ms = now_ms.saturating_add(lease_ttl_ms as i64);

        let script = redis::Script::new(
            r#"
local budget_key = KEYS[1]
local cooldown_key = KEYS[2]
local leases_key = KEYS[3]
local lease_key = KEYS[4]
local now = tonumber(ARGV[1])
local now_ms = tonumber(ARGV[2])
local lease_expires_at_ms = tonumber(ARGV[3])
local lease_id = ARGV[4]
local instance_id = ARGV[5]
local calls_add = tonumber(ARGV[6])
local tokens_add = tonumber(ARGV[7])
local kb_add = tonumber(ARGV[8])
local max_calls = tonumber(ARGV[9])
local max_tokens = tonumber(ARGV[10])
local max_kb = tonumber(ARGV[11])
local max_concurrent = tonumber(ARGV[12])
local cooldown_secs = tonumber(ARGV[13])
local window_secs = tonumber(ARGV[14])
local lease_ttl_ms = tonumber(ARGV[15])

redis.call('ZREMRANGEBYSCORE', leases_key, '-inf', now_ms)
local active = tonumber(redis.call('ZCARD', leases_key) or '0')

local cooldown_until = tonumber(redis.call('GET', cooldown_key) or '0')
if cooldown_until > now then
  return {'cooldown', cooldown_until}
end

if active + 1 > max_concurrent then
  return {'max_concurrent', active}
end

local calls = tonumber(redis.call('HGET', budget_key, 'calls') or '0')
local tokens = tonumber(redis.call('HGET', budget_key, 'tokens') or '0')
local kb = tonumber(redis.call('HGET', budget_key, 'kb') or '0')
if calls + calls_add > max_calls then
  redis.call('SET', cooldown_key, now + cooldown_secs, 'EX', cooldown_secs)
  redis.call('HSET', budget_key, 'budget_hit_reason', 'max_calls')
  redis.call('EXPIRE', budget_key, window_secs)
  return {'max_calls', calls}
end
if tokens + tokens_add > max_tokens then
  redis.call('SET', cooldown_key, now + cooldown_secs, 'EX', cooldown_secs)
  redis.call('HSET', budget_key, 'budget_hit_reason', 'max_tokens')
  redis.call('EXPIRE', budget_key, window_secs)
  return {'max_tokens', tokens}
end
if kb + kb_add > max_kb then
  redis.call('SET', cooldown_key, now + cooldown_secs, 'EX', cooldown_secs)
  redis.call('HSET', budget_key, 'budget_hit_reason', 'max_kb')
  redis.call('EXPIRE', budget_key, window_secs)
  return {'max_kb', kb}
end

redis.call('HINCRBY', budget_key, 'calls', calls_add)
redis.call('HINCRBY', budget_key, 'tokens', tokens_add)
redis.call('HINCRBY', budget_key, 'kb', kb_add)
redis.call('HSET', budget_key, 'last_instance_id', instance_id, 'budget_hit_reason', '')
redis.call('ZADD', leases_key, lease_expires_at_ms, lease_id)
redis.call('PEXPIRE', leases_key, lease_ttl_ms)
redis.call('SET', lease_key, instance_id, 'PX', lease_ttl_ms)
redis.call('EXPIRE', budget_key, window_secs)
return {'ok', active + 1}
"#,
        );

        let response: Vec<String> = self
            .with_connection(|conn| {
                script
                    .key(&budget_key)
                    .key(&cooldown_key)
                    .key(&leases_key)
                    .key(&lease_key)
                    .arg(now_secs)
                    .arg(now_ms)
                    .arg(lease_expires_at_ms)
                    .arg(&lease_id)
                    .arg(&self.config.instance_id)
                    .arg(1u64)
                    .arg(meta.estimated_input_tokens())
                    .arg(meta.request_kb())
                    .arg(self.config.max_calls_per_window)
                    .arg(self.config.max_tokens_per_window)
                    .arg(self.config.max_kb_per_window)
                    .arg(self.config.max_concurrent)
                    .arg(self.config.cooldown_secs)
                    .arg(window_secs)
                    .arg(lease_ttl_ms)
                    .invoke(conn)
            })
            .map_err(|err| err.to_string())?;

        if response.first().map(String::as_str) == Some("ok") {
            let lease = GlobalLease {
                node_id: node_id.to_string(),
                lease_id,
            };
            self.local_leases
                .lock()
                .unwrap()
                .entry(node_id.to_string())
                .or_default()
                .push(lease.lease_id.clone());
            Ok(lease)
        } else {
            Err(response
                .first()
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()))
        }
    }

    pub fn release_one(&self, node_id: &str) {
        let lease_id = {
            let mut leases = self.local_leases.lock().unwrap();
            leases.get_mut(node_id).and_then(|items| items.pop())
        };
        let Some(lease_id) = lease_id else {
            return;
        };
        let _ = self.release(node_id, &lease_id);
    }

    pub fn release(&self, node_id: &str, lease_id: &str) -> Result<(), String> {
        let leases_key = self.leases_key(node_id);
        let lease_key = self.lease_key(node_id, lease_id);
        let script = redis::Script::new(
            r#"
local exists = redis.call('EXISTS', KEYS[2])
if exists == 1 then
  redis.call('DEL', KEYS[2])
  redis.call('ZREM', KEYS[1], ARGV[1])
end
return exists
"#,
        );
        let _: i64 = self
            .with_connection(|conn| {
                script
                    .key(leases_key)
                    .key(lease_key)
                    .arg(lease_id)
                    .invoke(conn)
            })
            .map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn snapshot(&self, node_id: &str) -> HashMap<String, String> {
        let now = chrono::Utc::now();
        let window_secs = self.config.window_secs.max(1);
        let bucket = now.timestamp() / window_secs as i64;
        let leases_key = self.leases_key(node_id);
        let Ok(mut snapshot) = self.with_connection(|conn| {
            let mut snapshot: HashMap<String, String> =
                conn.hgetall(self.budget_key(node_id, bucket))?;
            let _ = redis::cmd("ZREMRANGEBYSCORE")
                .arg(&leases_key)
                .arg("-inf")
                .arg(now.timestamp_millis())
                .query::<i64>(conn);
            if let Ok(active) = redis::cmd("ZCARD").arg(&leases_key).query::<i64>(conn) {
                snapshot.insert("active".to_string(), active.to_string());
            }
            if let Ok(cooldown_until) = conn.get::<_, String>(self.cooldown_key(node_id)) {
                snapshot.insert("cooldown_until".to_string(), cooldown_until);
            }
            Ok(snapshot)
        }) else {
            return HashMap::new();
        };
        snapshot.insert("bucket".to_string(), bucket.to_string());
        snapshot
    }

    fn budget_key(&self, node_id: &str, bucket: i64) -> String {
        format!("zprs:budget:{node_id}:{bucket}")
    }

    fn cooldown_key(&self, node_id: &str) -> String {
        format!("zprs:cooldown:{node_id}")
    }

    fn leases_key(&self, node_id: &str) -> String {
        format!("zprs:leases:{node_id}")
    }

    fn lease_key(&self, node_id: &str, lease_id: &str) -> String {
        format!("zprs:lease:{node_id}:{lease_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redis_url() -> Option<String> {
        let url = std::env::var("ZEN_PROXY_TEST_REDIS_URL")
            .unwrap_or_else(|_| "redis://127.0.0.1:6379/".to_string());
        let client = redis::Client::open(url.as_str()).ok()?;
        let mut conn = client.get_connection().ok()?;
        redis::cmd("PING").query::<String>(&mut conn).ok()?;
        Some(url)
    }

    fn cleanup(url: &str, node_id: &str) {
        let Ok(client) = redis::Client::open(url) else {
            return;
        };
        let Ok(mut conn) = client.get_connection() else {
            return;
        };
        let keys = redis::cmd("KEYS")
            .arg(format!("zprs:*{node_id}*"))
            .query::<Vec<String>>(&mut conn)
            .unwrap_or_default();
        if !keys.is_empty() {
            let _ = redis::cmd("DEL").arg(keys).query::<i64>(&mut conn);
        }
    }

    fn registry(
        url: &str,
        instance_id: &str,
        max_calls: u64,
        lease_ttl_secs: u64,
    ) -> GlobalBudgetRegistry {
        GlobalBudgetRegistry::new(GlobalBudgetConfig {
            redis_url: url.to_string(),
            instance_id: instance_id.to_string(),
            window_secs: 60,
            lease_ttl_secs,
            max_calls_per_window: max_calls,
            max_tokens_per_window: 1_000_000,
            max_kb_per_window: 1_000_000,
            max_concurrent: 1,
            cooldown_secs: 1,
        })
        .unwrap()
    }

    fn meta() -> RequestMeta {
        RequestMeta {
            model: "deepseek-v4-flash".to_string(),
            stream: true,
            body_size: 128,
            affinity_key: String::new(),
        }
    }

    #[test]
    fn redis_global_budget_is_shared_across_instances() {
        let Some(url) = redis_url() else {
            eprintln!("skipping Redis global budget test: Redis is unavailable");
            return;
        };
        let node_id = format!("test-global-budget-{}", Uuid::new_v4());
        cleanup(&url, &node_id);

        let a = registry(&url, "test-a", 2, 30);
        let b = registry(&url, "test-b", 2, 30);

        let first = a.try_acquire(&node_id, &meta()).unwrap();
        a.release(&first.node_id, &first.lease_id).unwrap();
        let second = b.try_acquire(&node_id, &meta()).unwrap();
        b.release(&second.node_id, &second.lease_id).unwrap();
        let third = a.try_acquire(&node_id, &meta()).unwrap_err();

        assert_eq!(third, "max_calls");
        cleanup(&url, &node_id);
    }

    #[test]
    fn redis_global_budget_cleans_expired_leases_before_concurrency_check() {
        let Some(url) = redis_url() else {
            eprintln!("skipping Redis global budget test: Redis is unavailable");
            return;
        };
        let node_id = format!("test-global-lease-{}", Uuid::new_v4());
        cleanup(&url, &node_id);

        let a = registry(&url, "test-a", 10, 1);
        let b = registry(&url, "test-b", 10, 1);

        let _leaked = a.try_acquire(&node_id, &meta()).unwrap();
        assert_eq!(
            b.try_acquire(&node_id, &meta()).unwrap_err(),
            "max_concurrent"
        );
        std::thread::sleep(std::time::Duration::from_millis(1200));
        let recovered = b.try_acquire(&node_id, &meta()).unwrap();

        b.release(&recovered.node_id, &recovered.lease_id).unwrap();
        cleanup(&url, &node_id);
    }
}
