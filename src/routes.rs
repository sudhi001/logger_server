//! Router assembly.
//!
//! Three trust zones:
//!   * **write** — a registered device token; ingest only.
//!   * **viewer** — a dashboard session or the admin token; reads, the live
//!     stream, and device management.
//!   * **public** — health, login, and the static assets needed to render the
//!     login screen.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::Router;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::assets;
use crate::auth;
use crate::handlers::{auth as auth_handlers, devices, health, ingest, query, stream};
use crate::middleware::ratelimit;
use crate::state::AppState;

async fn serve_asset(uri: axum::http::Uri) -> impl IntoResponse {
    match assets::lookup(uri.path()) {
        Some(asset) => asset.into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

pub fn build(state: Arc<AppState>) -> Router {
    // ---- write zone: device token required ----
    let writes = Router::new()
        .route("/api/v1/logs", post(ingest::ingest_one))
        .route("/api/v1/logs/batch", post(ingest::ingest_batch))
        // Legacy path, now authenticated like everything else.
        .route("/logs", post(ingest::ingest_one))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_device,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit,
        ))
        .layer(DefaultBodyLimit::max(state.cfg.max_body_bytes));

    // ---- viewer zone: bounded reads ----
    let reads = Router::new()
        .route("/api/v1/logs/recent", get(query::recent))
        .route("/api/v1/logs/by-name/{name}", get(query::by_name))
        .route("/api/v1/logs/export", get(query::export))
        .route("/api/v1/devices", get(devices::list).post(devices::create))
        .route("/api/v1/devices/{id}", delete(devices::revoke))
        .route("/metrics", get(health::metrics))
        // Legacy aliases.
        .route("/logs/recent", get(query::recent))
        .route("/logs", get(query::export))
        .layer(CompressionLayer::new())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(300),
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_viewer,
        ));

    // SSE is deliberately outside CompressionLayer and TimeoutLayer:
    // compression buffers (stalling the live tail) and a long-lived stream must
    // not be cut off by a request timeout.
    let live = Router::new()
        .route("/api/v1/logs/stream", get(stream::stream))
        .route("/logs/stream", get(stream::stream))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_viewer,
        ));

    // Registered after the literal routes so `/logs/recent` and `/logs/stream`
    // win over this wildcard.
    let legacy_by_name = Router::new()
        .route("/logs/{name}", get(query::by_name))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_viewer,
        ));

    // ---- public zone ----
    let public = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/api/v1/auth/login", post(auth_handlers::login))
        .route("/api/v1/auth/logout", post(auth_handlers::logout))
        .route("/api/v1/auth/whoami", get(auth_handlers::whoami));

    Router::new()
        .merge(writes)
        .merge(reads)
        .merge(live)
        .merge(legacy_by_name)
        .merge(public)
        // Assets are public: the login page has to render before there is a
        // session. They contain no data, only markup and script.
        .fallback(serve_asset)
        .layer(CorsLayer::permissive())
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
