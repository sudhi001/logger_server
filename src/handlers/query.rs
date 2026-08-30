//! Read path: bounded queries plus the streaming export.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;

use crate::error::AppError;
use crate::model::{LogContext, LogRecord, LogStats};
use crate::state::AppState;
use crate::store::reader::SearchQuery;

const DEFAULT_LIMIT: i64 = 1000;
const MAX_LIMIT: i64 = 5000;

#[derive(Debug, Deserialize, Default)]
pub struct ListParams {
    pub limit: Option<i64>,
    /// Cursor: return rows with a strictly smaller id.
    pub before_id: Option<i64>,
    /// Minimum severity, so the dashboard can narrow without pulling everything.
    pub min_level: Option<u8>,
    pub device_id: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct SearchParams {
    /// Free text, matched against the search index.
    pub q: Option<String>,
    pub limit: Option<i64>,
    pub before_id: Option<i64>,
    pub min_level: Option<u8>,
    pub device_id: Option<i64>,
    /// Exact tag match, e.g. `[Net] `.
    pub name: Option<String>,
    /// Unix milliseconds, inclusive.
    pub since: Option<i64>,
    pub until: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ContextParams {
    pub before: Option<i64>,
    pub after: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct StatsParams {
    pub since: Option<i64>,
    pub until: Option<i64>,
}

impl ListParams {
    /// Clamped so a caller cannot ask for the whole table on a bounded endpoint.
    fn limit(&self) -> i64 {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }
}

pub async fn recent(
    State(state): State<Arc<AppState>>,
    Query(p): Query<ListParams>,
) -> Result<Json<Vec<LogRecord>>, AppError> {
    Ok(Json(
        state
            .store
            .reader
            .recent(p.limit(), p.before_id, p.min_level, p.device_id)
            .await?,
    ))
}

/// Full-text search across everything stored, not just what is loaded in the
/// dashboard.
pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(p): Query<SearchParams>,
) -> Result<Json<Vec<LogRecord>>, AppError> {
    let q = SearchQuery {
        text: p.q,
        min_level: p.min_level,
        device_id: p.device_id,
        name: p.name,
        since: p.since,
        until: p.until,
        before_id: p.before_id,
        limit: p.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
    };
    Ok(Json(state.store.reader.search(q).await?))
}

/// The lines around one log line — what you need to explain a crash.
pub async fn context(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Query(p): Query<ContextParams>,
) -> Result<Json<LogContext>, AppError> {
    let before = p.before.unwrap_or(20).clamp(0, 500);
    let after = p.after.unwrap_or(20).clamp(0, 500);
    state
        .store
        .reader
        .context(id, before, after)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::BadRequest(format!("no log with id {id}")))
}

pub async fn stats(
    State(state): State<Arc<AppState>>,
    Query(p): Query<StatsParams>,
) -> Result<Json<LogStats>, AppError> {
    Ok(Json(state.store.reader.stats(p.since, p.until).await?))
}

pub async fn by_name(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(p): Query<ListParams>,
) -> Result<Json<Vec<LogRecord>>, AppError> {
    Ok(Json(
        state
            .store
            .reader
            .by_name(name, p.limit(), p.before_id)
            .await?,
    ))
}

#[derive(Debug, Deserialize, Default)]
pub struct ExportParams {
    /// `ndjson` for newline-delimited; anything else yields a JSON array.
    pub format: Option<String>,
}

/// Streams the entire table.
///
/// Deliberately unbounded, but the body is produced incrementally from a SQLite
/// cursor, so server memory stays flat no matter how many rows exist. The
/// default output is a JSON array so that callers of the original `GET /logs`
/// keep parsing it unchanged.
pub async fn export(
    State(state): State<Arc<AppState>>,
    Query(p): Query<ExportParams>,
) -> Result<Response, AppError> {
    let ndjson = p.format.as_deref() == Some("ndjson");
    let rx = state.store.reader.export(ndjson).await?;

    let content_type = if ndjson {
        "application/x-ndjson"
    } else {
        "application/json"
    };

    Ok((
        [(header::CONTENT_TYPE, content_type)],
        Body::from_stream(ReceiverStream::new(rx)),
    )
        .into_response())
}
