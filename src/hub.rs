//! SSE fan-out.
//!
//! The wire frame is built **once** per log line and shared by `Bytes`
//! reference count, so N subscribers cost N pointer copies rather than N
//! serialisations. The channel is a fixed-capacity ring: a subscriber that
//! falls behind is evicted instead of being buffered for, which is what makes
//! the memory ceiling independent of client count and client speed.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::broadcast;

use crate::model::LogRecord;

/// A pre-serialised, ready-to-write SSE frame.
pub struct LogFrame {
    pub id: i64,
    /// Complete `id: N\ndata: {...}\n\n` payload.
    pub bytes: Bytes,
}

impl LogFrame {
    pub fn new(rec: &LogRecord) -> Self {
        // serde_json emits compact output with newlines escaped, so the JSON
        // always fits on a single `data:` line.
        let json = serde_json::to_string(rec).unwrap_or_else(|_| "{}".to_string());
        let mut buf = String::with_capacity(json.len() + 32);
        buf.push_str("id: ");
        buf.push_str(&rec.id.to_string());
        buf.push_str("\ndata: ");
        buf.push_str(&json);
        buf.push_str("\n\n");
        Self {
            id: rec.id,
            bytes: Bytes::from(buf),
        }
    }
}

pub struct Hub {
    tx: broadcast::Sender<Arc<LogFrame>>,
}

impl Hub {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publishes a frame. Returns without blocking even if nobody is listening.
    pub fn publish(&self, frame: Arc<LogFrame>) {
        // Err simply means no subscribers; that is not a failure.
        let _ = self.tx.send(frame);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<LogFrame>> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}
