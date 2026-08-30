//! Shared application state.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tokio::sync::watch;

use crate::config::Config;
use crate::hub::Hub;
use crate::middleware::ratelimit::IpLimiter;
use crate::store::Store;

#[derive(Default)]
pub struct Metrics {
    pub ingested: AtomicU64,
    pub shed: AtomicU64,
    pub rate_limited: AtomicU64,
    pub sse_evicted: AtomicU64,
    pub sse_opened: AtomicU64,
}

pub struct AppState {
    pub cfg: Config,
    pub store: Store,
    pub hub: Hub,
    pub limiter: Option<Arc<IpLimiter>>,
    pub metrics: Metrics,
    /// Fires once on shutdown so in-flight SSE streams terminate instead of
    /// holding graceful shutdown open forever.
    pub shutdown: watch::Receiver<bool>,
}

impl AppState {
    pub fn started_shutdown(&self) -> bool {
        *self.shutdown.borrow()
    }
}
