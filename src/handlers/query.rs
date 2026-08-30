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
use crate::model::LogRecord;
use crate::state::AppState;

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
