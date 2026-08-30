//! Wire and storage types for a log record.

use serde::{Deserialize, Serialize};

/// A persisted log line.
///
/// `id` is assigned by the ingest path from an atomic counter (see
/// [`crate::store::Store`]) rather than by SQLite, which is what allows the
/// write to be acknowledged before it is durable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub id: i64,
    /// Unix epoch milliseconds.
    pub ts: i64,
    pub name: String,
    pub level: u8,
    pub message: String,
    /// Which registered device sent this. `None` only for rows written before
    /// device authentication existed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<i64>,
    /// Resolved device name, attached on read for display. Never stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Caller-supplied structured fields: session id, app version, user id, or
    /// anything else worth correlating on later.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

/// Level scale. `info` is 2, matching the default the original service implied.
pub const LEVEL_NAMES: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// Inbound body for `POST /api/v1/logs`.
///
/// `ts` and `level` are optional so that clients written against the original
/// Kotlin API — which sent only `{name, message}` — keep working unchanged.
#[derive(Debug, Deserialize)]
pub struct NewLog {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub message: String,
    pub ts: Option<i64>,
    pub level: Option<u8>,
    /// Optional JSON object of structured fields. Anything that is not an
    /// object is rejected, so the column stays queryable.
    pub context: Option<serde_json::Value>,
}

/// Acknowledgement returned by the ingest endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: i64,
    pub name: String,
    pub platform: Option<String>,
    /// Leading characters of the token, for recognition only.
    pub token_prefix: String,
    pub created_at: i64,
    pub last_seen: Option<i64>,
    pub revoked: bool,
}

/// Returned once, at creation. The plaintext token is never retrievable again.
#[derive(Debug, Serialize)]
pub struct DeviceCreated {
    #[serde(flatten)]
    pub device: Device,
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct NewDevice {
    pub name: String,
    #[serde(default)]
    pub platform: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IngestAck {
    pub id: i64,
    pub ts: i64,
}

#[derive(Debug, Serialize)]
pub struct BatchAck {
    pub accepted: usize,
    /// Records the server refused because its write queue was full. The client
    /// should resend these; the last `dropped` entries of the batch were lost.
    pub dropped: usize,
    pub first_id: i64,
    pub last_id: i64,
}

/// Aggregate view over a time window, so a caller can summarise instead of
/// reading thousands of lines.
#[derive(Debug, Serialize)]
pub struct LogStats {
    pub total: i64,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    pub by_level: Vec<LevelCount>,
    pub by_device: Vec<NamedCount>,
    pub by_name: Vec<NamedCount>,
}

#[derive(Debug, Serialize)]
pub struct LevelCount {
    pub level: u8,
    pub label: &'static str,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct NamedCount {
    pub name: String,
    pub count: i64,
}

/// A log line with the lines that surround it — the shape root-cause analysis
/// actually needs.
#[derive(Debug, Serialize)]
pub struct LogContext {
    pub before: Vec<LogRecord>,
    #[serde(rename = "match")]
    pub matched: LogRecord,
    pub after: Vec<LogRecord>,
}

pub fn level_label(level: u8) -> &'static str {
    LEVEL_NAMES.get(level as usize).copied().unwrap_or("info")
}

pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
