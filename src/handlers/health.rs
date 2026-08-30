//! Liveness and metrics.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;

use crate::state::AppState;

pub async fn healthz(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    if state.started_shutdown() {
        // Lets the load balancer drain this instance before it stops accepting.
        return (StatusCode::SERVICE_UNAVAILABLE, "draining");
    }
    (StatusCode::OK, "ok")
}

/// Prometheus text exposition, written by hand to avoid a metrics dependency.
pub async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let m = &state.metrics;
    let rows = state.store.reader.count().await.unwrap_or(-1);

    let body = format!(
        "# HELP logger_ingested_total Log records accepted.\n\
         # TYPE logger_ingested_total counter\n\
         logger_ingested_total {}\n\
         # HELP logger_shed_total Records rejected because the write queue was full.\n\
         # TYPE logger_shed_total counter\n\
         logger_shed_total {}\n\
         # HELP logger_rate_limited_total Requests rejected by the per-IP limiter.\n\
         # TYPE logger_rate_limited_total counter\n\
         logger_rate_limited_total {}\n\
         # HELP logger_sse_opened_total SSE streams opened.\n\
         # TYPE logger_sse_opened_total counter\n\
         logger_sse_opened_total {}\n\
         # HELP logger_sse_evicted_total SSE clients dropped for falling behind.\n\
         # TYPE logger_sse_evicted_total counter\n\
         logger_sse_evicted_total {}\n\
         # HELP logger_sse_clients Currently connected SSE clients.\n\
         # TYPE logger_sse_clients gauge\n\
         logger_sse_clients {}\n\
         # HELP logger_rows Rows currently stored.\n\
         # TYPE logger_rows gauge\n\
         logger_rows {}\n\
         # HELP logger_auth_failures_total Rejected dashboard logins.\n\
         # TYPE logger_auth_failures_total counter\n\
         logger_auth_failures_total {}\n\
         # HELP logger_devices_active Registered devices with a live token.\n\
         # TYPE logger_devices_active gauge\n\
         logger_devices_active {}\n\
         # HELP logger_sessions_active Dashboard sessions currently valid.\n\
         # TYPE logger_sessions_active gauge\n\
         logger_sessions_active {}\n\
         # HELP logger_alert_rules_active Enabled alert rules.\n\
         # TYPE logger_alert_rules_active gauge\n\
         logger_alert_rules_active {}\n",
        m.ingested.load(Ordering::Relaxed),
        m.shed.load(Ordering::Relaxed),
        m.rate_limited.load(Ordering::Relaxed),
        m.sse_opened.load(Ordering::Relaxed),
        m.sse_evicted.load(Ordering::Relaxed),
        state.hub.subscriber_count(),
        rows,
        m.auth_failures.load(Ordering::Relaxed),
        state.devices.len(),
        state.sessions.len(),
        state.alerts.active_count(),
    );

    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}
