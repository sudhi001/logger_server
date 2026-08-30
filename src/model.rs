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
}

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
}

/// Acknowledgement returned by the ingest endpoints.
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

pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
