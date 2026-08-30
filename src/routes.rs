//! Router assembly.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::cors::CorsLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::assets;
use crate::handlers::{health, ingest, query, stream};
use crate::middleware::{auth, ratelimit};
use crate::state::AppState;

async fn serve_asset(uri: axum::http::Uri) -> impl IntoResponse {
    match assets::lookup(uri.path()) {
        Some(asset) => asset.into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

pub fn build(state: Arc<AppState>) -> Router {
    // Write path carries auth and rate limiting; read paths do not.
    let writes = Router::new()
        .route("/api/v1/logs", post(ingest::ingest_one))
        .route("/api/v1/logs/batch", post(ingest::ingest_batch))
        // Legacy: the original POST /logs.
        .route("/logs", post(ingest::ingest_one))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            ratelimit::limit,
        ))
        .layer(DefaultBodyLimit::max(state.cfg.max_body_bytes));

    // Bounded reads. Compression is worthwhile here.
    let reads = Router::new()
        .route("/api/v1/logs/recent", get(query::recent))
        .route("/api/v1/logs/by-name/{name}", get(query::by_name))
        .route("/api/v1/logs/export", get(query::export))
        // Legacy aliases.
        .route("/logs/recent", get(query::recent))
        .route("/logs", get(query::export))
        .layer(CompressionLayer::new())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(300),
        ));

    // SSE is deliberately excluded from CompressionLayer and TimeoutLayer:
    // compression buffers (which would stall the live tail) and a long-lived
    // stream must not be cut off by a request timeout.
    let live = Router::new()
        .route("/api/v1/logs/stream", get(stream::stream))
        .route("/logs/stream", get(stream::stream));

    // Registered last so that the literal `/logs/recent` and `/logs/stream`
    // routes above win over this wildcard.
    let legacy_by_name = Router::new().route("/logs/{name}", get(query::by_name));

    let ops = Router::new()
        .route("/healthz", get(health::healthz))
        .route("/metrics", get(health::metrics));

    Router::new()
        .merge(writes)
        .merge(reads)
        .merge(live)
        .merge(legacy_by_name)
        .merge(ops)
        .fallback(serve_asset)
        .layer(CorsLayer::permissive())
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
