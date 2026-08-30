//! Runtime configuration, entirely from environment variables.
//!
//! Every value has a default that reproduces the behaviour of the original
//! Kotlin service, so the server starts correctly with no environment set.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub db_path: String,
    pub workers: usize,
    pub reader_conns: usize,
    /// Rows retained before the oldest are pruned.
    pub max_rows: i64,
    pub max_age: Option<Duration>,
    /// Matches the original Kotlin controller's truncation limit.
    pub max_message_len: usize,
    /// Gates the dashboard and every read endpoint. Generated at boot if unset.
    pub admin_token: String,
    /// How long a dashboard login stays valid.
    pub session_ttl: Duration,
    /// Send the session cookie with `Secure`. Must be off for plain-HTTP local use.
    pub cookie_secure: bool,
    /// What an AI agent reaching the MCP endpoint may do.
    pub mcp_access: crate::mcp::tools::Access,
    /// Serve the MCP endpoint at all.
    pub mcp_enabled: bool,
    /// Let webhooks reach private, loopback and link-local addresses. Off by
    /// default so a webhook URL cannot be used to probe your own network.
    pub webhook_allow_private: bool,
    /// How this server is reached from outside, for links inside alerts.
    pub public_url: String,
    /// Pending alert deliveries before new ones are dropped.
    pub alert_queue: usize,
    /// Per-IP writes per second. `0` disables rate limiting.
    pub rate_limit_rps: u32,
    pub rate_limit_burst: u32,
    /// Honour `X-Forwarded-For`. Only safe behind a proxy that overwrites it.
    pub trust_proxy: bool,
    /// Broadcast ring capacity; a subscriber that falls this far behind is evicted.
    pub sse_capacity: usize,
    /// Bounded ingest queue depth. When full, ingest sheds load with 503.
    pub ingest_queue: usize,
    pub max_body_bytes: usize,
}

/// The admin token, generated if the operator did not supply one.
///
/// Generating rather than refusing to start keeps a fresh deploy usable, but the
/// value is logged loudly because it does not survive a restart.
fn admin_token() -> String {
    match std::env::var("LOGGER_ADMIN_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            let generated = crate::auth::token::generate(crate::auth::token::ADMIN_PREFIX);
            tracing::warn!(
                token = %generated,
                "LOGGER_ADMIN_TOKEN is not set; generated a temporary one. \
                 It changes on every restart -- set the variable to keep it stable."
            );
            generated
        }
    }
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

impl Config {
    pub fn from_env() -> Self {
        let max_age_days: u64 = env_parse("LOGGER_MAX_AGE_DAYS", 7);
        let default_workers = std::thread::available_parallelism()
            .map(|n| n.get().min(4))
            .unwrap_or(2);

        Self {
            // PORT is what most PaaS platforms (Render, Fly, Heroku) inject.
            port: env_parse("LOGGER_PORT", env_parse("PORT", 8080)),
            db_path: std::env::var("LOGGER_DB_PATH").unwrap_or_else(|_| "logs.db".to_string()),
            workers: env_parse::<usize>("LOGGER_WORKERS", default_workers).max(1),
            reader_conns: env_parse::<usize>("LOGGER_READER_CONNS", 4).max(1),
            max_rows: env_parse("LOGGER_MAX_ROWS", 1_000_000),
            // 0 days means "never prune by age".
            max_age: (max_age_days > 0).then(|| Duration::from_secs(max_age_days * 86_400)),
            max_message_len: env_parse("LOGGER_MAX_MESSAGE_LEN", 50_384),
            admin_token: admin_token(),
            session_ttl: Duration::from_secs(
                env_parse::<u64>("LOGGER_SESSION_TTL_HOURS", 168).clamp(1, 8760) * 3600,
            ),
            // Behind a proxy the browser speaks HTTPS even though we serve
            // plain HTTP, so the cookie needs Secure there but not on localhost.
            cookie_secure: env_parse(
                "LOGGER_COOKIE_SECURE",
                env_parse("LOGGER_TRUST_PROXY", false),
            ),
            mcp_access: crate::mcp::tools::Access::from_env(
                std::env::var("LOGGER_MCP_MODE").ok().as_deref(),
            ),
            mcp_enabled: env_parse("LOGGER_MCP_ENABLED", true),
            webhook_allow_private: env_parse("LOGGER_WEBHOOK_ALLOW_PRIVATE", false),
            public_url: std::env::var("LOGGER_PUBLIC_URL")
                .ok()
                .filter(|u| !u.trim().is_empty())
                .unwrap_or_else(|| {
                    format!(
                        "http://localhost:{}",
                        env_parse::<u16>("LOGGER_PORT", env_parse("PORT", 8080))
                    )
                }),
            alert_queue: env_parse::<usize>("LOGGER_ALERT_QUEUE", 256).max(8),
            rate_limit_rps: env_parse("LOGGER_RATE_LIMIT_RPS", 500),
            rate_limit_burst: env_parse("LOGGER_RATE_LIMIT_BURST", 1_000),
            trust_proxy: env_parse("LOGGER_TRUST_PROXY", false),
            sse_capacity: env_parse::<usize>("LOGGER_SSE_CAPACITY", 1024).max(16),
            ingest_queue: env_parse::<usize>("LOGGER_INGEST_QUEUE", 8192).max(64),
            max_body_bytes: env_parse("LOGGER_MAX_BODY_BYTES", 1024 * 1024),
        }
    }
}
