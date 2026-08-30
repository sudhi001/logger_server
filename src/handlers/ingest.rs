//! Write path.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::error::AppError;
use crate::hub::LogFrame;
use crate::model::{now_millis, BatchAck, IngestAck, LogRecord, NewLog};
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct IngestParams {
    /// Wait for the row to be committed before responding.
    #[serde(default)]
    pub sync: bool,
}

/// Normalises an inbound log into a storable record.
///
/// Truncation is done on a character boundary: slicing a UTF-8 string at a
/// fixed byte offset would panic mid-codepoint.
fn to_record(state: &AppState, id: i64, input: NewLog) -> Result<LogRecord, AppError> {
    if input.name.is_empty() && input.message.is_empty() {
        return Err(AppError::BadRequest(
            "at least one of name or message is required".into(),
        ));
    }

    let limit = state.cfg.max_message_len;
    let message = if input.message.chars().count() > limit {
        input.message.chars().take(limit).collect()
    } else {
        input.message
    };

    // Bound the name too; it is indexed and otherwise attacker-controlled.
    let name = if input.name.chars().count() > 255 {
        input.name.chars().take(255).collect()
    } else {
        input.name
    };

    Ok(LogRecord {
        id,
        ts: input.ts.unwrap_or_else(now_millis),
        name,
        level: input.level.unwrap_or(2),
        message,
    })
}

/// Publishes to SSE subscribers and queues the durable write.
fn dispatch(state: &AppState, rec: LogRecord) -> Result<(), AppError> {
    state.hub.publish(Arc::new(LogFrame::new(&rec)));
    match state.store.enqueue(rec) {
        Ok(()) => {
            state.metrics.ingested.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        Err(e) => {
            state.metrics.shed.fetch_add(1, Ordering::Relaxed);
            Err(e)
        }
    }
}

pub async fn ingest_one(
    State(state): State<Arc<AppState>>,
    Query(params): Query<IngestParams>,
    Json(input): Json<NewLog>,
) -> Result<(StatusCode, Json<IngestAck>), AppError> {
    let id = state.store.next_id();
    let rec = to_record(&state, id, input)?;
    let ack = IngestAck {
        id: rec.id,
        ts: rec.ts,
    };

    if params.sync {
        state.hub.publish(Arc::new(LogFrame::new(&rec)));
        let wait = state.store.enqueue_sync(rec).inspect_err(|_| {
            state.metrics.shed.fetch_add(1, Ordering::Relaxed);
        })?;
        state.metrics.ingested.fetch_add(1, Ordering::Relaxed);
        match wait.await {
            Ok(Ok(())) => return Ok((StatusCode::CREATED, Json(ack))),
            Ok(Err(e)) => return Err(AppError::Internal(e)),
            Err(_) => return Err(AppError::Internal("writer dropped the ack".into())),
        }
    }

    dispatch(&state, rec)?;
    // 202: queued and already broadcast, durable a few milliseconds later.
    Ok((StatusCode::ACCEPTED, Json(ack)))
}

pub async fn ingest_batch(
    State(state): State<Arc<AppState>>,
    Json(inputs): Json<Vec<NewLog>>,
) -> Result<(StatusCode, Json<BatchAck>), AppError> {
    if inputs.is_empty() {
        return Err(AppError::BadRequest("empty batch".into()));
    }

    let total = inputs.len();
    let mut first_id = 0i64;
    let mut last_id = 0i64;
    let mut accepted = 0usize;
    let mut dropped = 0usize;

    for (idx, input) in inputs.into_iter().enumerate() {
        let id = state.store.next_id();
        let rec = to_record(&state, id, input)?;
        let rec_id = rec.id;

        // A full queue mid-batch reports what was accepted rather than
        // discarding the whole batch.
        if dispatch(&state, rec).is_err() {
            // Everything from here on is dropped, not just the record that
            // happened to hit the boundary. `dispatch` already counted that
            // one, so only the remainder is added here.
            dropped = total - idx;
            state
                .metrics
                .shed
                .fetch_add(dropped as u64 - 1, Ordering::Relaxed);
            break;
        }

        if accepted == 0 {
            first_id = rec_id;
        }
        last_id = rec_id;
        accepted += 1;
    }

    if accepted == 0 {
        return Err(AppError::Overloaded);
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(BatchAck {
            accepted,
            dropped,
            first_id,
            last_id,
        }),
    ))
}
