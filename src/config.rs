use std::collections::HashMap;
use std::env;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMode {
    Legacy,
    FreeModelKernel,
}

impl ProviderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::FreeModelKernel => "free_model_kernel",
        }
    }
}

impl fmt::Display for ProviderMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProviderMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "legacy" => Ok(Self::Legacy),
            "free_model_kernel" | "free-model-kernel" => Ok(Self::FreeModelKernel),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactorMode {
    Off,
    Observe,
    Enforce,
}

impl CompactorMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Enforce => "enforce",
        }
    }
}

impl fmt::Display for CompactorMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CompactorMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(Self::Off),
            "observe" | "observe_only" | "observe-only" => Ok(Self::Observe),
            "enforce" | "on" => Ok(Self::Enforce),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactCacheMode {
    Off,
    Metadata,
    Full,
}

impl ArtifactCacheMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Metadata => "metadata",
            Self::Full => "full",
        }
    }
}

impl fmt::Display for ArtifactCacheMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ArtifactCacheMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(Self::Off),
            "metadata" | "meta" => Ok(Self::Metadata),
            "full" | "on" => Ok(Self::Full),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolGuardMode {
    Off,
    Observe,
    Repair,
    Strict,
    RepairShadow,
}

impl ProtocolGuardMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Observe => "observe",
            Self::Repair => "repair",
            Self::Strict => "strict",
            Self::RepairShadow => "repair_shadow",
        }
    }
}

impl fmt::Display for ProtocolGuardMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProtocolGuardMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" => Ok(Self::Off),
            "observe" | "observe_only" | "observe-only" => Ok(Self::Observe),
            "repair" | "on" => Ok(Self::Repair),
            "strict" => Ok(Self::Strict),
            "repair_shadow" | "repair-shadow" | "shadow" => Ok(Self::RepairShadow),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolGuardOrphanPolicy {
    Downgrade,
    Reject,
}

impl ProtocolGuardOrphanPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Downgrade => "downgrade",
            Self::Reject => "reject",
        }
    }
}

impl fmt::Display for ProtocolGuardOrphanPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProtocolGuardOrphanPolicy {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "downgrade" | "recover" => Ok(Self::Downgrade),
            "reject" | "error" => Ok(Self::Reject),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalBudgetMode {
    Off,
    SyncRedis,
}

impl GlobalBudgetMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::SyncRedis => "sync_redis",
        }
    }
}

impl fmt::Display for GlobalBudgetMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for GlobalBudgetMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "off" | "local" | "none" => Ok(Self::Off),
            "sync_redis" | "redis" | "on" => Ok(Self::SyncRedis),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub bind_address: String,
    pub upstream_base: String,
    pub chat_target: String,
    pub model_target: String,
    pub admin_api_key: Option<String>,
    pub proxy_error_threshold: u32,
    pub proxy_cooldown_seconds: u64,
    pub proxy_recovery_interval: u64,
    pub pool_max_retries: u32,
    pub v4_empty_upstream_max_retries: u32,
    pub v4_retry_budget_ms: u64,
    pub pool_max_size: u32,
    pub connect_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub probe_timeout_secs: u64,
    pub probe_connect_timeout_secs: u64,
    pub pool_warm_interval_secs: u64,
    pub probe_batch_size: usize,
    pub dispatch_capacity: usize,
    pub active_capacity: usize,
    pub ratelimited_capacity: usize,
    pub dead_capacity: usize,
    pub model_override: Option<String>,
    pub model_mapping: HashMap<String, String>,
    pub allow_direct_fallback: bool,
    pub benchmark_mode: bool,
    pub log_level: String,
    pub sticky_ttl_secs: f64,
    pub proxy_api_key: Option<String>,
    pub upstream_api_key: String,
    pub opencode_headers_enabled: bool,
    pub opencode_user_agent_version: String,
    pub opencode_client_name: String,
    pub opencode_project_seed: String,
    pub opencode_session_ttl_secs: u64,
    pub pool_starvation_retry_after_secs: u64,
    pub global_backoff_cooldown_secs: u64,
    pub nodes_file: String,
    pub preferred_proxy_urls: Vec<String>,
    pub ledger_events_path: String,
    pub audit_log_enabled: bool,
    pub audit_log_dir: String,
    pub zen_provider_mode: ProviderMode,
    pub free_model_true_first_token_frt: bool,
    pub free_model_claude_code_stream_initial_fetch_timeout_secs: u64,
    pub free_model_claude_code_stream_slow_guard_min_input_tokens: u64,
    pub free_model_claude_code_stream_no_forwardable_retry_secs: u64,
    pub v4_model_registry_enabled: bool,
    pub dynamic_model_discovery_enabled: bool,
    pub dynamic_model_discovery_url: String,
    pub dynamic_model_discovery_interval_secs: u64,
    pub node_max_calls_per_window: u64,
    pub node_max_tokens_per_window: u64,
    pub node_max_kb_per_window: u64,
    pub node_budget_cooldown_secs: i64,
    pub node_budget_window_secs: u64,
    pub node_lease_ttl_secs: u64,
    pub global_budget_redis_url: Option<String>,
    pub instance_id: String,
    pub request_body_limit_mb: usize,
    pub v1_max_concurrent_requests: usize,
    pub context_warn_body_mb: usize,
    pub context_compact_body_mb: usize,
    pub context_target_body_mb: usize,
    pub context_upstream_body_limit_mb: usize,
    pub context_token_warn: u64,
    pub context_token_compact: u64,
    pub context_token_target: u64,
    pub context_large_chunk_bytes: usize,
    pub context_preserve_recent_messages: usize,
    pub zen_compactor_mode: CompactorMode,
    pub zen_artifact_cache_mode: ArtifactCacheMode,
    pub artifact_cache_dir: String,
    pub artifact_cache_max_mb: u64,
    pub artifact_cache_ttl_hours: u64,
    pub protocol_guard_mode: ProtocolGuardMode,
    pub protocol_guard_orphan_policy: ProtocolGuardOrphanPolicy,
    pub protocol_guard_synthetic_ids: bool,
    pub protocol_guard_log_sample_rate: f64,
    pub protocol_guard_max_ms: u64,
    pub protocol_guard_max_graph_messages: usize,
    pub protocol_guard_max_repair_actions: usize,
    pub v43_lanes_enabled: bool,
    pub v43_short_nonstream_concurrency: usize,
    pub v43_stream_concurrency: usize,
    pub v43_large_context_concurrency: usize,
    pub v43_huge_context_concurrency: usize,
    pub v43_large_context_body_mb: usize,
    pub v43_huge_context_body_mb: usize,
    pub v45_large_context_tokens: u64,
    pub v45_huge_context_tokens: u64,
    pub v45_ttft_slow_ms: u64,
    pub v45_ttft_bad_ms: u64,
    pub v46_long_nonstream_concurrency: usize,
    pub v46_long_output_concurrency: usize,
    pub v46_tool_heavy_concurrency: usize,
    pub v46_long_nonstream_tokens: u64,
    pub v46_long_output_tokens: u64,
    pub v43_lane_wait_timeout_ms: u64,
    pub v43_async_collector_enabled: bool,
    pub v43_collector_queue_capacity: usize,
    pub v43_dispatch_shards: usize,
    pub v43_node_min_concurrency: u32,
    pub v43_node_max_concurrency: u32,
    pub v43_aimd_success_step: u32,
    pub v43_aimd_failure_percent: u32,
    pub v43_aimd_slow_latency_ms: u64,
    pub v43_global_budget_mode: GlobalBudgetMode,
    pub v43_global_budget_fail_open: bool,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            port: load_env_var("PORT", 4000u16),
            bind_address: load_env_var("BIND_ADDRESS", "127.0.0.1".to_string()),
            upstream_base: load_env_var("UPSTREAM_BASE", "https://opencode.ai/zen".to_string()),
            chat_target: load_env_var("CHAT_TARGET", "/v1/chat/completions".to_string()),
            model_target: load_env_var("MODEL_TARGET", "/v1/models".to_string()),
            admin_api_key: match env::var("ADMIN_API_KEY") {
                Ok(v) if !v.is_empty() => Some(v),
                _ => None,
            },
            proxy_error_threshold: load_env_var("PROXY_ERROR_THRESHOLD", 5u32),
            proxy_cooldown_seconds: load_env_var("PROXY_COOLDOWN_SECONDS", 60u64),
            proxy_recovery_interval: load_env_var("PROXY_RECOVERY_INTERVAL", 30u64),
            pool_max_retries: load_env_var("POOL_MAX_RETRIES", 3u32),
            v4_empty_upstream_max_retries: load_env_var("V4_EMPTY_UPSTREAM_MAX_RETRIES", 12u32),
            v4_retry_budget_ms: load_env_var("V4_RETRY_BUDGET_MS", 45_000u64),
            pool_max_size: load_env_var("POOL_MAX_SIZE", 128u32),
            connect_timeout_secs: load_env_var("CONNECT_TIMEOUT_SECS", 5u64),
            request_timeout_secs: load_env_var("REQUEST_TIMEOUT_SECS", 120u64),
            probe_timeout_secs: load_env_var("PROBE_TIMEOUT_SECS", 30u64),
            probe_connect_timeout_secs: load_env_var("PROBE_CONNECT_TIMEOUT_SECS", 10u64),
            pool_warm_interval_secs: load_env_var("POOL_WARM_INTERVAL_SECS", 10u64),
            probe_batch_size: load_env_var("PROBE_BATCH_SIZE", 5usize),
            dispatch_capacity: load_env_var("DISPATCH_CAPACITY", 100usize),
            active_capacity: load_env_var("ACTIVE_CAPACITY", 100usize),
            ratelimited_capacity: load_env_var("RATELIMITED_CAPACITY", 100usize),
            dead_capacity: load_env_var("DEAD_CAPACITY", 100usize),
            model_override: match env::var("MODEL_OVERRIDE") {
                Ok(v) if !v.is_empty() => Some(v),
                _ => None,
            },
            model_mapping: Self::default_model_mapping(),
            allow_direct_fallback: load_env_var("ALLOW_DIRECT_FALLBACK", false),
            benchmark_mode: load_env_var("BENCHMARK_MODE", false),
            log_level: load_env_var("LOG_LEVEL", "info".to_string()),
            sticky_ttl_secs: load_env_var("STICKY_TTL_SECS", 180.0f64),
            nodes_file: env::var("NODES_FILE")
                .unwrap_or_else(|_| "/etc/zen-proxy/nodes.json".into()),
            preferred_proxy_urls: parse_proxy_list_env("PREFERRED_PROXY_URLS"),
            proxy_api_key: match env::var("PROXY_API_KEY") {
                Ok(v) if !v.is_empty() => Some(v),
                _ => None,
            },
            upstream_api_key: env::var("UPSTREAM_API_KEY").unwrap_or_else(|_| "public".into()),
            opencode_headers_enabled: load_env_var("OPENCODE_HEADERS_ENABLED", true),
            opencode_user_agent_version: load_env_var(
                "OPENCODE_USER_AGENT_VERSION",
                "0.0.0".to_string(),
            ),
            opencode_client_name: load_env_var("OPENCODE_CLIENT_NAME", "cli".to_string()),
            opencode_project_seed: load_env_var(
                "OPENCODE_PROJECT_SEED",
                "zen-proxy-rs".to_string(),
            ),
            opencode_session_ttl_secs: load_env_var("OPENCODE_SESSION_TTL_SECS", 1800u64),
            pool_starvation_retry_after_secs: load_env_var(
                "POOL_STARVATION_RETRY_AFTER_SECS",
                5u64,
            ),
            global_backoff_cooldown_secs: load_env_var("GLOBAL_BACKOFF_COOLDOWN_SECS", 30u64),
            ledger_events_path: env::var("LEDGER_EVENTS_PATH")
                .unwrap_or_else(|_| "/tmp/zen-proxy-ledger-events.jsonl".into()),
            audit_log_enabled: load_env_var("AUDIT_LOG_ENABLED", true),
            audit_log_dir: env::var("AUDIT_LOG_DIR")
                .unwrap_or_else(|_| "/tmp/zen-proxy-audit".into()),
            zen_provider_mode: load_env_var("ZEN_PROVIDER_MODE", ProviderMode::Legacy),
            free_model_true_first_token_frt: load_env_var("FREE_MODEL_TRUE_FIRST_TOKEN_FRT", true),
            free_model_claude_code_stream_initial_fetch_timeout_secs: load_env_var(
                "FREE_MODEL_CLAUDE_CODE_STREAM_INITIAL_FETCH_TIMEOUT_SECS",
                30u64,
            ),
            free_model_claude_code_stream_slow_guard_min_input_tokens: load_env_var(
                "FREE_MODEL_CLAUDE_CODE_STREAM_SLOW_GUARD_MIN_INPUT_TOKENS",
                150_000u64,
            ),
            free_model_claude_code_stream_no_forwardable_retry_secs: load_env_var(
                "FREE_MODEL_CLAUDE_CODE_STREAM_NO_FORWARDABLE_RETRY_SECS",
                45u64,
            )
            .max(1),
            v4_model_registry_enabled: load_env_var("V4_MODEL_REGISTRY_ENABLED", false),
            dynamic_model_discovery_enabled: load_env_var("DYNAMIC_MODEL_DISCOVERY_ENABLED", false),
            dynamic_model_discovery_url: env::var("DYNAMIC_MODEL_DISCOVERY_URL")
                .unwrap_or_else(|_| format!("{}{}", Self::default_upstream_base(), "/v1/models")),
            dynamic_model_discovery_interval_secs: load_env_var(
                "DYNAMIC_MODEL_DISCOVERY_INTERVAL_SECS",
                1800u64,
            )
            .max(60),
            node_max_calls_per_window: load_env_var("NODE_MAX_CALLS_PER_WINDOW", 100u64),
            node_max_tokens_per_window: load_env_var("NODE_MAX_TOKENS_PER_WINDOW", 10_000_000u64),
            node_max_kb_per_window: load_env_var("NODE_MAX_KB_PER_WINDOW", 64 * 1024u64),
            node_budget_cooldown_secs: load_env_var("NODE_BUDGET_COOLDOWN_SECS", 60i64),
            node_budget_window_secs: load_env_var("NODE_BUDGET_WINDOW_SECS", 3600u64),
            node_lease_ttl_secs: load_env_var("NODE_LEASE_TTL_SECS", 180u64),
            global_budget_redis_url: match env::var("GLOBAL_BUDGET_REDIS_URL") {
                Ok(v) if !v.is_empty() => Some(v),
                _ => None,
            },
            instance_id: env::var("INSTANCE_ID")
                .unwrap_or_else(|_| format!("zen-{}-{}", std::process::id(), uuid::Uuid::new_v4())),
            request_body_limit_mb: load_env_var("REQUEST_BODY_LIMIT_MB", 64usize),
            v1_max_concurrent_requests: load_env_var("V1_MAX_CONCURRENT_REQUESTS", 32usize),
            context_warn_body_mb: load_env_var("CONTEXT_WARN_BODY_MB", 24usize),
            context_compact_body_mb: load_env_var("CONTEXT_COMPACT_BODY_MB", 30usize),
            context_target_body_mb: load_env_var("CONTEXT_TARGET_BODY_MB", 26usize),
            context_upstream_body_limit_mb: load_env_var("CONTEXT_UPSTREAM_BODY_LIMIT_MB", 32usize),
            context_token_warn: load_env_var("CONTEXT_TOKEN_WARN", 600_000u64),
            context_token_compact: load_env_var("CONTEXT_TOKEN_COMPACT", 850_000u64),
            context_token_target: load_env_var("CONTEXT_TOKEN_TARGET", 750_000u64),
            context_large_chunk_bytes: load_env_var("CONTEXT_LARGE_CHUNK_BYTES", 256 * 1024usize),
            context_preserve_recent_messages: load_env_var(
                "CONTEXT_PRESERVE_RECENT_MESSAGES",
                8usize,
            ),
            zen_compactor_mode: load_env_var("ZEN_COMPACTOR_MODE", CompactorMode::Observe),
            zen_artifact_cache_mode: load_env_var(
                "ZEN_ARTIFACT_CACHE_MODE",
                ArtifactCacheMode::Metadata,
            ),
            artifact_cache_dir: env::var("ARTIFACT_CACHE_DIR")
                .unwrap_or_else(|_| "/tmp/zen-proxy-artifacts".into()),
            artifact_cache_max_mb: load_env_var("ARTIFACT_CACHE_MAX_MB", 2048u64),
            artifact_cache_ttl_hours: load_env_var("ARTIFACT_CACHE_TTL_HOURS", 24u64),
            protocol_guard_mode: load_env_var("PROTOCOL_GUARD_MODE", ProtocolGuardMode::Repair),
            protocol_guard_orphan_policy: load_env_var(
                "PROTOCOL_GUARD_ORPHAN_POLICY",
                ProtocolGuardOrphanPolicy::Downgrade,
            ),
            protocol_guard_synthetic_ids: load_env_var("PROTOCOL_GUARD_SYNTHETIC_IDS", true),
            protocol_guard_log_sample_rate: load_env_var("PROTOCOL_GUARD_LOG_SAMPLE_RATE", 1.0f64),
            protocol_guard_max_ms: load_env_var("PROTOCOL_GUARD_MAX_MS", 30u64),
            protocol_guard_max_graph_messages: load_env_var(
                "PROTOCOL_GUARD_MAX_GRAPH_MESSAGES",
                2000usize,
            ),
            protocol_guard_max_repair_actions: load_env_var(
                "PROTOCOL_GUARD_MAX_REPAIR_ACTIONS",
                200usize,
            ),
            v43_lanes_enabled: load_env_var("V43_LANES_ENABLED", false),
            v43_short_nonstream_concurrency: load_env_var(
                "V43_SHORT_NONSTREAM_CONCURRENCY",
                32usize,
            ),
            v43_stream_concurrency: load_env_var("V43_STREAM_CONCURRENCY", 96usize),
            v43_large_context_concurrency: load_env_var("V43_LARGE_CONTEXT_CONCURRENCY", 16usize),
            v43_huge_context_concurrency: load_env_var("V43_HUGE_CONTEXT_CONCURRENCY", 2usize),
            v43_large_context_body_mb: load_env_var("V43_LARGE_CONTEXT_BODY_MB", 8usize),
            v43_huge_context_body_mb: load_env_var("V43_HUGE_CONTEXT_BODY_MB", 32usize),
            v45_large_context_tokens: load_env_var("V45_LARGE_CONTEXT_TOKENS", 80_000u64),
            v45_huge_context_tokens: load_env_var("V45_HUGE_CONTEXT_TOKENS", 180_000u64),
            v45_ttft_slow_ms: load_env_var("V45_TTFT_SLOW_MS", 4_000u64),
            v45_ttft_bad_ms: load_env_var("V45_TTFT_BAD_MS", 8_000u64),
            v46_long_nonstream_concurrency: load_env_var("V46_LONG_NONSTREAM_CONCURRENCY", 4usize),
            v46_long_output_concurrency: load_env_var("V46_LONG_OUTPUT_CONCURRENCY", 4usize),
            v46_tool_heavy_concurrency: load_env_var("V46_TOOL_HEAVY_CONCURRENCY", 16usize),
            v46_long_nonstream_tokens: load_env_var("V46_LONG_NONSTREAM_TOKENS", 10_000u64),
            v46_long_output_tokens: load_env_var("V46_LONG_OUTPUT_TOKENS", 4_096u64),
            v43_lane_wait_timeout_ms: load_env_var("V43_LANE_WAIT_TIMEOUT_MS", 1_000u64),
            v43_async_collector_enabled: load_env_var("V43_ASYNC_COLLECTOR_ENABLED", false),
            v43_collector_queue_capacity: load_env_var("V43_COLLECTOR_QUEUE_CAPACITY", 8192usize),
            v43_dispatch_shards: load_env_var("V43_DISPATCH_SHARDS", 16usize),
            v43_node_min_concurrency: load_env_var("V43_NODE_MIN_CONCURRENCY", 1u32),
            v43_node_max_concurrency: load_env_var("V43_NODE_MAX_CONCURRENCY", 16u32),
            v43_aimd_success_step: load_env_var("V43_AIMD_SUCCESS_STEP", 1u32),
            v43_aimd_failure_percent: load_env_var("V43_AIMD_FAILURE_PERCENT", 50u32),
            v43_aimd_slow_latency_ms: load_env_var("V43_AIMD_SLOW_LATENCY_MS", 30_000u64),
            v43_global_budget_mode: load_env_var(
                "V43_GLOBAL_BUDGET_MODE",
                GlobalBudgetMode::SyncRedis,
            ),
            v43_global_budget_fail_open: load_env_var("V43_GLOBAL_BUDGET_FAIL_OPEN", true),
        }
    }

    pub fn reload(&mut self) {
        *self = Self::from_env();
    }

    fn default_model_mapping() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(
            "deepseek-v4-flash".to_string(),
            "deepseek-v4-flash-free".to_string(),
        );
        m.insert(
            "deepseek-v4-flash-lite".to_string(),
            "big-pickle".to_string(),
        );
        m
    }

    fn default_upstream_base() -> String {
        "https://opencode.ai/zen".to_string()
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.bind_address, self.port)
    }

    pub fn chat_url(&self) -> String {
        format!("{}{}", self.upstream_base, self.chat_target)
    }

    pub fn model_url(&self) -> String {
        format!("{}{}", self.upstream_base, self.model_target)
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_secs)
    }

    pub fn probe_timeout(&self) -> Duration {
        Duration::from_secs(self.probe_timeout_secs)
    }

    pub fn probe_connect_timeout(&self) -> Duration {
        Duration::from_secs(self.probe_connect_timeout_secs)
    }

    pub fn pool_warm_interval(&self) -> Duration {
        Duration::from_secs(self.pool_warm_interval_secs)
    }

    pub fn sticky_ttl(&self) -> Duration {
        Duration::from_secs_f64(self.sticky_ttl_secs)
    }

    pub fn load_nodes(&self) -> Vec<String> {
        let mut nodes = self.preferred_proxy_urls.clone();
        let file_nodes = match std::fs::read_to_string(&self.nodes_file) {
            Ok(contents) => match parse_nodes_file(&contents) {
                Ok(nodes) => {
                    tracing::info!(count = nodes.len(), file = %self.nodes_file, "loaded proxy nodes");
                    nodes
                }
                Err(e) => {
                    tracing::warn!(file = %self.nodes_file, error = %e, "failed to parse nodes file, using empty pool");
                    Vec::new()
                }
            },
            Err(_) => {
                tracing::warn!(file = %self.nodes_file, "nodes file not found, using empty pool");
                Vec::new()
            }
        };
        nodes.extend(file_nodes);
        dedupe_preserving_order(nodes)
    }
    pub fn proxy_auth_required(&self) -> bool {
        self.proxy_api_key.is_some()
    }

    pub fn v4_model_registry_active(&self) -> bool {
        self.v4_model_registry_enabled
            || matches!(self.zen_provider_mode, ProviderMode::FreeModelKernel)
    }
}

pub fn load_env_var<T: FromStr>(key: &str, default: T) -> T {
    match env::var(key) {
        Ok(raw) if !raw.is_empty() => match raw.parse::<T>() {
            Ok(val) => val,
            Err(_) => {
                tracing::warn!(
                    "env var {} has unparseable value \"{}\", using default",
                    key,
                    raw
                );
                default
            }
        },
        Ok(_) | Err(_) => default,
    }
}

fn parse_nodes_file(contents: &str) -> Result<Vec<String>, String> {
    if let Ok(nodes) = serde_json::from_str::<Vec<String>>(contents) {
        return Ok(nodes);
    }

    let nodes = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(parse_proxy_line)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(nodes)
}

fn parse_proxy_line(line: &str) -> Result<String, String> {
    if line.contains("://") {
        return Ok(line.to_string());
    }

    let parts = line.split(':').collect::<Vec<_>>();
    match parts.as_slice() {
        [host, port, user, pass] if !host.is_empty() && !port.is_empty() => {
            Ok(format!("http://{user}:{pass}@{host}:{port}"))
        }
        [host, port] if !host.is_empty() && !port.is_empty() => Ok(format!("http://{host}:{port}")),
        _ => Err(format!("unsupported proxy line format: {line}")),
    }
}

fn parse_proxy_list_env(key: &str) -> Vec<String> {
    env::var(key)
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .filter_map(|item| match parse_proxy_line(item) {
                    Ok(url) => Some(url),
                    Err(err) => {
                        tracing::warn!(env = key, error = %err, "ignoring invalid preferred proxy");
                        None
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn dedupe_preserving_order(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            deduped.push(item);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn remove_env_vars(keys: &[&str]) {
        for key in keys {
            env::remove_var(key);
        }
    }

    #[test]
    fn from_env_uses_defaults_when_unset() {
        let _guard = env_lock();
        remove_env_vars(&[
            "PORT",
            "MODEL_OVERRIDE",
            "ADMIN_API_KEY",
            "LOG_LEVEL",
            "PREFERRED_PROXY_URLS",
            "PROBE_BATCH_SIZE",
            "OPENCODE_HEADERS_ENABLED",
            "OPENCODE_CLIENT_NAME",
            "OPENCODE_PROJECT_SEED",
            "OPENCODE_SESSION_TTL_SECS",
            "ZEN_PROVIDER_MODE",
            "FREE_MODEL_TRUE_FIRST_TOKEN_FRT",
            "V4_MODEL_REGISTRY_ENABLED",
            "DYNAMIC_MODEL_DISCOVERY_ENABLED",
            "DYNAMIC_MODEL_DISCOVERY_URL",
            "DYNAMIC_MODEL_DISCOVERY_INTERVAL_SECS",
            "V4_RETRY_BUDGET_MS",
            "CONNECT_TIMEOUT_SECS",
            "REQUEST_TIMEOUT_SECS",
            "AUDIT_LOG_ENABLED",
            "AUDIT_LOG_DIR",
            "NODE_MAX_CALLS_PER_WINDOW",
            "NODE_MAX_TOKENS_PER_WINDOW",
            "NODE_MAX_KB_PER_WINDOW",
            "NODE_BUDGET_COOLDOWN_SECS",
            "NODE_BUDGET_WINDOW_SECS",
            "NODE_LEASE_TTL_SECS",
            "GLOBAL_BUDGET_REDIS_URL",
            "INSTANCE_ID",
            "REQUEST_BODY_LIMIT_MB",
            "V1_MAX_CONCURRENT_REQUESTS",
            "CONTEXT_WARN_BODY_MB",
            "CONTEXT_COMPACT_BODY_MB",
            "CONTEXT_TARGET_BODY_MB",
            "CONTEXT_UPSTREAM_BODY_LIMIT_MB",
            "CONTEXT_TOKEN_WARN",
            "CONTEXT_TOKEN_COMPACT",
            "CONTEXT_TOKEN_TARGET",
            "CONTEXT_LARGE_CHUNK_BYTES",
            "CONTEXT_PRESERVE_RECENT_MESSAGES",
            "ZEN_COMPACTOR_MODE",
            "ZEN_ARTIFACT_CACHE_MODE",
            "ARTIFACT_CACHE_DIR",
            "ARTIFACT_CACHE_MAX_MB",
            "ARTIFACT_CACHE_TTL_HOURS",
            "PROTOCOL_GUARD_MODE",
            "PROTOCOL_GUARD_ORPHAN_POLICY",
            "PROTOCOL_GUARD_SYNTHETIC_IDS",
            "PROTOCOL_GUARD_LOG_SAMPLE_RATE",
            "PROTOCOL_GUARD_MAX_MS",
            "PROTOCOL_GUARD_MAX_GRAPH_MESSAGES",
            "PROTOCOL_GUARD_MAX_REPAIR_ACTIONS",
            "V43_LANES_ENABLED",
            "V43_SHORT_NONSTREAM_CONCURRENCY",
            "V43_STREAM_CONCURRENCY",
            "V43_LARGE_CONTEXT_CONCURRENCY",
            "V43_HUGE_CONTEXT_CONCURRENCY",
            "V43_LARGE_CONTEXT_BODY_MB",
            "V43_HUGE_CONTEXT_BODY_MB",
            "V45_LARGE_CONTEXT_TOKENS",
            "V45_HUGE_CONTEXT_TOKENS",
            "V45_TTFT_SLOW_MS",
            "V45_TTFT_BAD_MS",
            "V43_LANE_WAIT_TIMEOUT_MS",
            "V43_ASYNC_COLLECTOR_ENABLED",
            "V43_COLLECTOR_QUEUE_CAPACITY",
        ]);

        let cfg = Config::from_env();
        assert_eq!(cfg.port, 4000);
        assert!(cfg.admin_api_key.is_none());
        assert!(cfg.model_override.is_none());
        assert!(!cfg.allow_direct_fallback);
        assert!(!cfg.benchmark_mode);
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.probe_timeout_secs, 30);
        assert!(cfg.preferred_proxy_urls.is_empty());
        assert_eq!(cfg.probe_batch_size, 5);
        assert_eq!(cfg.dispatch_capacity, 100);
        assert_eq!(cfg.ledger_events_path, "/tmp/zen-proxy-ledger-events.jsonl");
        assert!(cfg.audit_log_enabled);
        assert_eq!(cfg.audit_log_dir, "/tmp/zen-proxy-audit");
        assert!(cfg.opencode_headers_enabled);
        assert_eq!(cfg.opencode_client_name, "cli");
        assert_eq!(cfg.opencode_project_seed, "zen-proxy-rs");
        assert_eq!(cfg.opencode_session_ttl_secs, 1800);
        assert_eq!(cfg.zen_provider_mode, ProviderMode::Legacy);
        assert!(cfg.free_model_true_first_token_frt);
        assert!(!cfg.v4_model_registry_enabled);
        assert!(!cfg.dynamic_model_discovery_enabled);
        assert_eq!(
            cfg.dynamic_model_discovery_url,
            "https://opencode.ai/zen/v1/models"
        );
        assert_eq!(cfg.dynamic_model_discovery_interval_secs, 1800);
        assert_eq!(cfg.v4_retry_budget_ms, 45_000);
        assert_eq!(cfg.connect_timeout_secs, 5);
        assert_eq!(cfg.request_timeout_secs, 120);
        assert!(!cfg.v4_model_registry_active());
        assert_eq!(cfg.node_max_calls_per_window, 100);
        assert_eq!(cfg.node_max_tokens_per_window, 10_000_000);
        assert_eq!(cfg.node_max_kb_per_window, 64 * 1024);
        assert_eq!(cfg.node_budget_cooldown_secs, 60);
        assert_eq!(cfg.node_budget_window_secs, 3600);
        assert_eq!(cfg.node_lease_ttl_secs, 180);
        assert!(cfg.global_budget_redis_url.is_none());
        assert!(cfg.instance_id.starts_with("zen-"));
        assert_eq!(cfg.request_body_limit_mb, 64);
        assert_eq!(cfg.v1_max_concurrent_requests, 32);
        assert_eq!(cfg.context_warn_body_mb, 24);
        assert_eq!(cfg.context_compact_body_mb, 30);
        assert_eq!(cfg.context_target_body_mb, 26);
        assert_eq!(cfg.context_upstream_body_limit_mb, 32);
        assert_eq!(cfg.context_token_warn, 600_000);
        assert_eq!(cfg.context_token_compact, 850_000);
        assert_eq!(cfg.context_token_target, 750_000);
        assert_eq!(cfg.context_large_chunk_bytes, 256 * 1024);
        assert_eq!(cfg.context_preserve_recent_messages, 8);
        assert_eq!(cfg.zen_compactor_mode, CompactorMode::Observe);
        assert_eq!(cfg.zen_artifact_cache_mode, ArtifactCacheMode::Metadata);
        assert_eq!(cfg.artifact_cache_dir, "/tmp/zen-proxy-artifacts");
        assert_eq!(cfg.artifact_cache_max_mb, 2048);
        assert_eq!(cfg.artifact_cache_ttl_hours, 24);
        assert_eq!(cfg.protocol_guard_mode, ProtocolGuardMode::Repair);
        assert_eq!(
            cfg.protocol_guard_orphan_policy,
            ProtocolGuardOrphanPolicy::Downgrade
        );
        assert!(cfg.protocol_guard_synthetic_ids);
        assert_eq!(cfg.protocol_guard_log_sample_rate, 1.0);
        assert_eq!(cfg.protocol_guard_max_ms, 30);
        assert_eq!(cfg.protocol_guard_max_graph_messages, 2000);
        assert_eq!(cfg.protocol_guard_max_repair_actions, 200);
        assert!(!cfg.v43_lanes_enabled);
        assert_eq!(cfg.v43_short_nonstream_concurrency, 32);
        assert_eq!(cfg.v43_stream_concurrency, 96);
        assert_eq!(cfg.v43_large_context_concurrency, 16);
        assert_eq!(cfg.v43_huge_context_concurrency, 2);
        assert_eq!(cfg.v43_large_context_body_mb, 8);
        assert_eq!(cfg.v43_huge_context_body_mb, 32);
        assert_eq!(cfg.v45_large_context_tokens, 80_000);
        assert_eq!(cfg.v45_huge_context_tokens, 180_000);
        assert_eq!(cfg.v45_ttft_slow_ms, 4_000);
        assert_eq!(cfg.v45_ttft_bad_ms, 8_000);
        assert_eq!(cfg.v46_long_nonstream_concurrency, 4);
        assert_eq!(cfg.v46_long_output_concurrency, 4);
        assert_eq!(cfg.v46_tool_heavy_concurrency, 16);
        assert_eq!(cfg.v46_long_nonstream_tokens, 10_000);
        assert_eq!(cfg.v46_long_output_tokens, 4_096);
        assert_eq!(cfg.v43_lane_wait_timeout_ms, 1_000);
        assert!(!cfg.v43_async_collector_enabled);
        assert_eq!(cfg.v43_collector_queue_capacity, 8192);
        assert_eq!(cfg.v43_dispatch_shards, 16);
        assert_eq!(cfg.v43_node_min_concurrency, 1);
        assert_eq!(cfg.v43_node_max_concurrency, 16);
        assert_eq!(cfg.v43_aimd_success_step, 1);
        assert_eq!(cfg.v43_aimd_failure_percent, 50);
        assert_eq!(cfg.v43_aimd_slow_latency_ms, 30_000);
        assert_eq!(cfg.v43_global_budget_mode, GlobalBudgetMode::SyncRedis);
        assert!(cfg.v43_global_budget_fail_open);
    }

    #[test]
    fn from_env_reads_env_overrides() {
        let _guard = env_lock();
        unsafe { env::set_var("PORT", "8080") };
        unsafe { env::set_var("LOG_LEVEL", "debug") };
        unsafe { env::set_var("PREFERRED_PROXY_URLS", "http://127.0.0.1:7897,1.2.3.4:8080") };
        unsafe { env::set_var("PROBE_BATCH_SIZE", "10") };
        unsafe { env::set_var("OPENCODE_HEADERS_ENABLED", "true") };
        unsafe { env::set_var("OPENCODE_CLIENT_NAME", "desktop-cli") };
        unsafe { env::set_var("ZEN_PROVIDER_MODE", "free_model_kernel") };
        unsafe { env::set_var("FREE_MODEL_TRUE_FIRST_TOKEN_FRT", "false") };
        unsafe { env::set_var("V4_MODEL_REGISTRY_ENABLED", "true") };
        unsafe { env::set_var("V4_RETRY_BUDGET_MS", "12345") };
        unsafe { env::set_var("CONNECT_TIMEOUT_SECS", "9") };
        unsafe { env::set_var("REQUEST_TIMEOUT_SECS", "600") };
        unsafe { env::set_var("AUDIT_LOG_ENABLED", "false") };
        unsafe { env::set_var("AUDIT_LOG_DIR", "/tmp/zen-audit-test") };
        unsafe { env::set_var("NODE_MAX_CALLS_PER_WINDOW", "7") };
        unsafe { env::set_var("NODE_MAX_TOKENS_PER_WINDOW", "777") };
        unsafe { env::set_var("NODE_MAX_KB_PER_WINDOW", "77") };
        unsafe { env::set_var("NODE_BUDGET_COOLDOWN_SECS", "17") };
        unsafe { env::set_var("NODE_BUDGET_WINDOW_SECS", "1700") };
        unsafe { env::set_var("NODE_LEASE_TTL_SECS", "270") };
        unsafe { env::set_var("GLOBAL_BUDGET_REDIS_URL", "redis://127.0.0.1:6379/") };
        unsafe { env::set_var("INSTANCE_ID", "test-instance") };
        unsafe { env::set_var("REQUEST_BODY_LIMIT_MB", "128") };
        unsafe { env::set_var("V1_MAX_CONCURRENT_REQUESTS", "12") };
        unsafe { env::set_var("CONTEXT_WARN_BODY_MB", "20") };
        unsafe { env::set_var("CONTEXT_COMPACT_BODY_MB", "29") };
        unsafe { env::set_var("CONTEXT_TARGET_BODY_MB", "25") };
        unsafe { env::set_var("CONTEXT_UPSTREAM_BODY_LIMIT_MB", "31") };
        unsafe { env::set_var("CONTEXT_TOKEN_WARN", "500000") };
        unsafe { env::set_var("CONTEXT_TOKEN_COMPACT", "900000") };
        unsafe { env::set_var("CONTEXT_TOKEN_TARGET", "700000") };
        unsafe { env::set_var("CONTEXT_LARGE_CHUNK_BYTES", "65536") };
        unsafe { env::set_var("CONTEXT_PRESERVE_RECENT_MESSAGES", "12") };
        unsafe { env::set_var("ZEN_COMPACTOR_MODE", "enforce") };
        unsafe { env::set_var("ZEN_ARTIFACT_CACHE_MODE", "full") };
        unsafe { env::set_var("ARTIFACT_CACHE_DIR", "/tmp/zen-test-artifacts") };
        unsafe { env::set_var("ARTIFACT_CACHE_MAX_MB", "64") };
        unsafe { env::set_var("ARTIFACT_CACHE_TTL_HOURS", "2") };
        unsafe { env::set_var("PROTOCOL_GUARD_MODE", "strict") };
        unsafe { env::set_var("PROTOCOL_GUARD_ORPHAN_POLICY", "reject") };
        unsafe { env::set_var("PROTOCOL_GUARD_SYNTHETIC_IDS", "false") };
        unsafe { env::set_var("PROTOCOL_GUARD_LOG_SAMPLE_RATE", "0.5") };
        unsafe { env::set_var("PROTOCOL_GUARD_MAX_MS", "11") };
        unsafe { env::set_var("PROTOCOL_GUARD_MAX_GRAPH_MESSAGES", "123") };
        unsafe { env::set_var("PROTOCOL_GUARD_MAX_REPAIR_ACTIONS", "9") };
        unsafe { env::set_var("V43_LANES_ENABLED", "true") };
        unsafe { env::set_var("V43_SHORT_NONSTREAM_CONCURRENCY", "33") };
        unsafe { env::set_var("V43_STREAM_CONCURRENCY", "99") };
        unsafe { env::set_var("V43_LARGE_CONTEXT_CONCURRENCY", "17") };
        unsafe { env::set_var("V43_HUGE_CONTEXT_CONCURRENCY", "3") };
        unsafe { env::set_var("V43_LARGE_CONTEXT_BODY_MB", "9") };
        unsafe { env::set_var("V43_HUGE_CONTEXT_BODY_MB", "33") };
        unsafe { env::set_var("V45_LARGE_CONTEXT_TOKENS", "210000") };
        unsafe { env::set_var("V45_HUGE_CONTEXT_TOKENS", "610000") };
        unsafe { env::set_var("V45_TTFT_SLOW_MS", "3456") };
        unsafe { env::set_var("V45_TTFT_BAD_MS", "9876") };
        unsafe { env::set_var("V46_LONG_NONSTREAM_CONCURRENCY", "5") };
        unsafe { env::set_var("V46_LONG_OUTPUT_CONCURRENCY", "6") };
        unsafe { env::set_var("V46_TOOL_HEAVY_CONCURRENCY", "7") };
        unsafe { env::set_var("V46_LONG_NONSTREAM_TOKENS", "11000") };
        unsafe { env::set_var("V46_LONG_OUTPUT_TOKENS", "5000") };
        unsafe { env::set_var("V43_LANE_WAIT_TIMEOUT_MS", "1500") };
        unsafe { env::set_var("V43_ASYNC_COLLECTOR_ENABLED", "true") };
        unsafe { env::set_var("V43_COLLECTOR_QUEUE_CAPACITY", "1234") };
        unsafe { env::set_var("V43_DISPATCH_SHARDS", "7") };
        unsafe { env::set_var("V43_NODE_MIN_CONCURRENCY", "2") };
        unsafe { env::set_var("V43_NODE_MAX_CONCURRENCY", "21") };
        unsafe { env::set_var("V43_AIMD_SUCCESS_STEP", "3") };
        unsafe { env::set_var("V43_AIMD_FAILURE_PERCENT", "40") };
        unsafe { env::set_var("V43_AIMD_SLOW_LATENCY_MS", "12345") };
        unsafe { env::set_var("V43_GLOBAL_BUDGET_MODE", "off") };
        unsafe { env::set_var("V43_GLOBAL_BUDGET_FAIL_OPEN", "false") };

        let cfg = Config::from_env();
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(
            cfg.preferred_proxy_urls,
            vec![
                "http://127.0.0.1:7897".to_string(),
                "http://1.2.3.4:8080".to_string()
            ]
        );
        assert_eq!(cfg.probe_batch_size, 10);
        assert!(cfg.opencode_headers_enabled);
        assert_eq!(cfg.opencode_client_name, "desktop-cli");
        assert_eq!(cfg.zen_provider_mode, ProviderMode::FreeModelKernel);
        assert!(!cfg.free_model_true_first_token_frt);
        assert!(cfg.v4_model_registry_enabled);
        assert_eq!(cfg.v4_retry_budget_ms, 12_345);
        assert_eq!(cfg.connect_timeout_secs, 9);
        assert_eq!(cfg.request_timeout_secs, 600);
        assert!(!cfg.audit_log_enabled);
        assert_eq!(cfg.audit_log_dir, "/tmp/zen-audit-test");
        assert!(cfg.v4_model_registry_active());
        assert_eq!(cfg.node_max_calls_per_window, 7);
        assert_eq!(cfg.node_max_tokens_per_window, 777);
        assert_eq!(cfg.node_max_kb_per_window, 77);
        assert_eq!(cfg.node_budget_cooldown_secs, 17);
        assert_eq!(cfg.node_budget_window_secs, 1700);
        assert_eq!(cfg.node_lease_ttl_secs, 270);
        assert_eq!(
            cfg.global_budget_redis_url.as_deref(),
            Some("redis://127.0.0.1:6379/")
        );
        assert_eq!(cfg.instance_id, "test-instance");
        assert_eq!(cfg.request_body_limit_mb, 128);
        assert_eq!(cfg.v1_max_concurrent_requests, 12);
        assert_eq!(cfg.context_warn_body_mb, 20);
        assert_eq!(cfg.context_compact_body_mb, 29);
        assert_eq!(cfg.context_target_body_mb, 25);
        assert_eq!(cfg.context_upstream_body_limit_mb, 31);
        assert_eq!(cfg.context_token_warn, 500_000);
        assert_eq!(cfg.context_token_compact, 900_000);
        assert_eq!(cfg.context_token_target, 700_000);
        assert_eq!(cfg.context_large_chunk_bytes, 65_536);
        assert_eq!(cfg.context_preserve_recent_messages, 12);
        assert_eq!(cfg.zen_compactor_mode, CompactorMode::Enforce);
        assert_eq!(cfg.zen_artifact_cache_mode, ArtifactCacheMode::Full);
        assert_eq!(cfg.artifact_cache_dir, "/tmp/zen-test-artifacts");
        assert_eq!(cfg.artifact_cache_max_mb, 64);
        assert_eq!(cfg.artifact_cache_ttl_hours, 2);
        assert_eq!(cfg.protocol_guard_mode, ProtocolGuardMode::Strict);
        assert_eq!(
            cfg.protocol_guard_orphan_policy,
            ProtocolGuardOrphanPolicy::Reject
        );
        assert!(!cfg.protocol_guard_synthetic_ids);
        assert_eq!(cfg.protocol_guard_log_sample_rate, 0.5);
        assert_eq!(cfg.protocol_guard_max_ms, 11);
        assert_eq!(cfg.protocol_guard_max_graph_messages, 123);
        assert_eq!(cfg.protocol_guard_max_repair_actions, 9);
        assert!(cfg.v43_lanes_enabled);
        assert_eq!(cfg.v43_short_nonstream_concurrency, 33);
        assert_eq!(cfg.v43_stream_concurrency, 99);
        assert_eq!(cfg.v43_large_context_concurrency, 17);
        assert_eq!(cfg.v43_huge_context_concurrency, 3);
        assert_eq!(cfg.v43_large_context_body_mb, 9);
        assert_eq!(cfg.v43_huge_context_body_mb, 33);
        assert_eq!(cfg.v45_large_context_tokens, 210_000);
        assert_eq!(cfg.v45_huge_context_tokens, 610_000);
        assert_eq!(cfg.v45_ttft_slow_ms, 3_456);
        assert_eq!(cfg.v45_ttft_bad_ms, 9_876);
        assert_eq!(cfg.v46_long_nonstream_concurrency, 5);
        assert_eq!(cfg.v46_long_output_concurrency, 6);
        assert_eq!(cfg.v46_tool_heavy_concurrency, 7);
        assert_eq!(cfg.v46_long_nonstream_tokens, 11_000);
        assert_eq!(cfg.v46_long_output_tokens, 5_000);
        assert_eq!(cfg.v43_lane_wait_timeout_ms, 1500);
        assert!(cfg.v43_async_collector_enabled);
        assert_eq!(cfg.v43_collector_queue_capacity, 1234);
        assert_eq!(cfg.v43_dispatch_shards, 7);
        assert_eq!(cfg.v43_node_min_concurrency, 2);
        assert_eq!(cfg.v43_node_max_concurrency, 21);
        assert_eq!(cfg.v43_aimd_success_step, 3);
        assert_eq!(cfg.v43_aimd_failure_percent, 40);
        assert_eq!(cfg.v43_aimd_slow_latency_ms, 12_345);
        assert_eq!(cfg.v43_global_budget_mode, GlobalBudgetMode::Off);
        assert!(!cfg.v43_global_budget_fail_open);

        remove_env_vars(&[
            "PORT",
            "LOG_LEVEL",
            "PREFERRED_PROXY_URLS",
            "PROBE_BATCH_SIZE",
            "OPENCODE_HEADERS_ENABLED",
            "OPENCODE_CLIENT_NAME",
            "ZEN_PROVIDER_MODE",
            "FREE_MODEL_TRUE_FIRST_TOKEN_FRT",
            "V4_MODEL_REGISTRY_ENABLED",
            "V4_RETRY_BUDGET_MS",
            "CONNECT_TIMEOUT_SECS",
            "REQUEST_TIMEOUT_SECS",
            "AUDIT_LOG_ENABLED",
            "AUDIT_LOG_DIR",
            "NODE_MAX_CALLS_PER_WINDOW",
            "NODE_MAX_TOKENS_PER_WINDOW",
            "NODE_MAX_KB_PER_WINDOW",
            "NODE_BUDGET_COOLDOWN_SECS",
            "NODE_BUDGET_WINDOW_SECS",
            "NODE_LEASE_TTL_SECS",
            "GLOBAL_BUDGET_REDIS_URL",
            "INSTANCE_ID",
            "REQUEST_BODY_LIMIT_MB",
            "V1_MAX_CONCURRENT_REQUESTS",
            "CONTEXT_WARN_BODY_MB",
            "CONTEXT_COMPACT_BODY_MB",
            "CONTEXT_TARGET_BODY_MB",
            "CONTEXT_UPSTREAM_BODY_LIMIT_MB",
            "CONTEXT_TOKEN_WARN",
            "CONTEXT_TOKEN_COMPACT",
            "CONTEXT_TOKEN_TARGET",
            "CONTEXT_LARGE_CHUNK_BYTES",
            "CONTEXT_PRESERVE_RECENT_MESSAGES",
            "ZEN_COMPACTOR_MODE",
            "ZEN_ARTIFACT_CACHE_MODE",
            "ARTIFACT_CACHE_DIR",
            "ARTIFACT_CACHE_MAX_MB",
            "ARTIFACT_CACHE_TTL_HOURS",
            "PROTOCOL_GUARD_MODE",
            "PROTOCOL_GUARD_ORPHAN_POLICY",
            "PROTOCOL_GUARD_SYNTHETIC_IDS",
            "PROTOCOL_GUARD_LOG_SAMPLE_RATE",
            "PROTOCOL_GUARD_MAX_MS",
            "PROTOCOL_GUARD_MAX_GRAPH_MESSAGES",
            "PROTOCOL_GUARD_MAX_REPAIR_ACTIONS",
            "V43_LANES_ENABLED",
            "V43_SHORT_NONSTREAM_CONCURRENCY",
            "V43_STREAM_CONCURRENCY",
            "V43_LARGE_CONTEXT_CONCURRENCY",
            "V43_HUGE_CONTEXT_CONCURRENCY",
            "V43_LARGE_CONTEXT_BODY_MB",
            "V43_HUGE_CONTEXT_BODY_MB",
            "V45_LARGE_CONTEXT_TOKENS",
            "V45_HUGE_CONTEXT_TOKENS",
            "V45_TTFT_SLOW_MS",
            "V45_TTFT_BAD_MS",
            "V46_LONG_NONSTREAM_CONCURRENCY",
            "V46_LONG_OUTPUT_CONCURRENCY",
            "V46_TOOL_HEAVY_CONCURRENCY",
            "V46_LONG_NONSTREAM_TOKENS",
            "V46_LONG_OUTPUT_TOKENS",
            "V43_LANE_WAIT_TIMEOUT_MS",
            "V43_ASYNC_COLLECTOR_ENABLED",
            "V43_COLLECTOR_QUEUE_CAPACITY",
            "V43_DISPATCH_SHARDS",
            "V43_NODE_MIN_CONCURRENCY",
            "V43_NODE_MAX_CONCURRENCY",
            "V43_AIMD_SUCCESS_STEP",
            "V43_AIMD_FAILURE_PERCENT",
            "V43_AIMD_SLOW_LATENCY_MS",
            "V43_GLOBAL_BUDGET_MODE",
            "V43_GLOBAL_BUDGET_FAIL_OPEN",
        ]);
    }

    #[test]
    fn from_env_graceful_on_bad_values() {
        let _guard = env_lock();
        unsafe { env::set_var("PORT", "not-a-number") };

        let cfg = Config::from_env();
        assert_eq!(cfg.port, 4000);

        env::remove_var("PORT");
    }

    #[test]
    fn model_mapping_is_pre_populated() {
        let _guard = env_lock();
        let cfg = Config::from_env();
        assert_eq!(
            cfg.model_mapping.get("deepseek-v4-flash").unwrap(),
            "deepseek-v4-flash-free"
        );
        assert_eq!(
            cfg.model_mapping.get("deepseek-v4-flash-lite").unwrap(),
            "big-pickle"
        );
        assert_eq!(cfg.model_mapping.len(), 2);
    }

    #[test]
    fn parse_nodes_file_accepts_json_array() {
        let nodes = parse_nodes_file(r#"["socks5://127.0.0.1:1080"]"#).unwrap();
        assert_eq!(nodes, vec!["socks5://127.0.0.1:1080"]);
    }

    #[test]
    fn parse_nodes_file_accepts_webshare_host_port_user_pass() {
        let nodes = parse_nodes_file("1.2.3.4:8080:user:pass\n").unwrap();
        assert_eq!(nodes, vec!["http://user:pass@1.2.3.4:8080"]);
    }

    #[test]
    fn load_nodes_prepends_preferred_proxies_and_dedupes() {
        let _guard = env_lock();
        let path =
            std::env::temp_dir().join(format!("zen-proxy-test-nodes-{}.txt", std::process::id()));
        std::fs::write(&path, "http://127.0.0.1:7897\n5.6.7.8:9000\n").unwrap();
        unsafe { env::set_var("NODES_FILE", &path) };
        unsafe { env::set_var("PREFERRED_PROXY_URLS", "http://127.0.0.1:7897") };

        let cfg = Config::from_env();
        assert_eq!(
            cfg.load_nodes(),
            vec![
                "http://127.0.0.1:7897".to_string(),
                "http://5.6.7.8:9000".to_string()
            ]
        );

        env::remove_var("NODES_FILE");
        env::remove_var("PREFERRED_PROXY_URLS");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reload_re_reads_env() {
        let _guard = env_lock();
        let mut cfg = Config::from_env();

        unsafe { env::set_var("PORT", "9999") };
        cfg.reload();
        assert_eq!(cfg.port, 9999);

        env::remove_var("PORT");
    }

    #[test]
    fn load_env_var_returns_default_on_empty_var() {
        let _guard = env_lock();
        unsafe { env::set_var("PORT", "") };
        let port: u16 = load_env_var("PORT", 4000u16);
        assert_eq!(port, 4000);
        env::remove_var("PORT");
    }

    #[test]
    fn convenience_accessors() {
        let _guard = env_lock();
        let cfg = Config::from_env();
        assert_eq!(cfg.bind_addr(), "127.0.0.1:4000");
        assert!(cfg.chat_url().ends_with("/v1/chat/completions"));
        assert!(cfg.model_url().ends_with("/v1/models"));
        assert_eq!(cfg.connect_timeout(), Duration::from_secs(5));
        assert_eq!(cfg.request_timeout(), Duration::from_secs(120));
        assert_eq!(cfg.probe_timeout(), Duration::from_secs(30));
        assert_eq!(cfg.probe_connect_timeout(), Duration::from_secs(10));
        assert_eq!(cfg.pool_warm_interval(), Duration::from_secs(10));
        assert_eq!(cfg.sticky_ttl(), Duration::from_secs_f64(180.0));
    }

    #[test]
    fn model_override_none_when_unset() {
        let _guard = env_lock();
        env::remove_var("MODEL_OVERRIDE");
        let cfg = Config::from_env();
        assert!(cfg.model_override.is_none());
    }

    #[test]
    fn model_override_some_when_set() {
        let _guard = env_lock();
        unsafe { env::set_var("MODEL_OVERRIDE", "custom-model") };
        let cfg = Config::from_env();
        assert_eq!(cfg.model_override.as_deref(), Some("custom-model"));
        env::remove_var("MODEL_OVERRIDE");
    }

    #[test]
    fn model_override_none_when_empty() {
        let _guard = env_lock();
        unsafe { env::set_var("MODEL_OVERRIDE", "") };
        let cfg = Config::from_env();
        assert!(cfg.model_override.is_none());
        env::remove_var("MODEL_OVERRIDE");
    }

    #[test]
    fn admin_api_key_none_when_unset() {
        let _guard = env_lock();
        env::remove_var("ADMIN_API_KEY");
        let cfg = Config::from_env();
        assert!(cfg.admin_api_key.is_none());
    }

    #[test]
    fn admin_api_key_some_when_set() {
        let _guard = env_lock();
        unsafe { env::set_var("ADMIN_API_KEY", "secret-key-123") };
        let cfg = Config::from_env();
        assert_eq!(cfg.admin_api_key.as_deref(), Some("secret-key-123"));
        env::remove_var("ADMIN_API_KEY");
    }

    #[test]
    fn nodes_file_default() {
        let _guard = env_lock();
        env::remove_var("NODES_FILE");
        let cfg = Config::from_env();
        assert_eq!(cfg.nodes_file, "/etc/zen-proxy/nodes.json");
    }
}
