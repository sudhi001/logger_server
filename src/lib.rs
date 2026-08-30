//! Remote log sink: ingest, persist, and live-tail application logs.

pub mod alerts;
pub mod assets;
pub mod auth;
pub mod config;
pub mod error;
pub mod handlers;
pub mod hub;
pub mod mcp;
pub mod middleware;
pub mod model;
pub mod routes;
pub mod state;
pub mod store;

use std::sync::Arc;

use tokio::sync::watch;

use crate::config::Config;
use crate::error::AppError;
use crate::hub::Hub;
use crate::state::{AppState, Metrics};
use crate::store::{Store, WriterHandle};

/// Builds the application state and starts the writer thread.
#[allow(clippy::type_complexity)]
pub fn build_state(
    cfg: Config,
) -> Result<
    (
        Arc<AppState>,
        WriterHandle,
        watch::Sender<bool>,
        tokio::sync::mpsc::Receiver<crate::model::AlertEvent>,
    ),
    AppError,
> {
    let (store, writer) = Store::open(&cfg)?;
    let hub = Hub::new(cfg.sse_capacity);
    let limiter = middleware::ratelimit::build(cfg.rate_limit_rps, cfg.rate_limit_burst);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let devices = store.devices.clone();
    let sessions = crate::auth::session::Sessions::new(cfg.session_ttl);
    // Bounded: a slow webhook must never be able to slow ingest down.
    let (alert_tx, alert_rx) = tokio::sync::mpsc::channel(cfg.alert_queue);
    let alerts = crate::alerts::engine::AlertEngine::new(alert_tx);

    let state = Arc::new(AppState {
        hub,
        limiter,
        devices,
        alerts,
        sessions,
        store,
        metrics: Metrics::default(),
        shutdown: shutdown_rx,
        cfg,
    });

    state.reload_alerts();

    Ok((state, writer, shutdown_tx, alert_rx))
}
