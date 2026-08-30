//! Live tail over Server-Sent Events.
//!
//! Three properties matter here, and all three were absent from the original
//! Kotlin implementation:
//!
//! 1. **Bounded memory.** Frames are serialised once and shared by reference
//!    count; a subscriber that cannot keep up is evicted rather than buffered
//!    for. Memory is therefore independent of client count and client speed.
//! 2. **No lost lines across reconnects.** Eviction and network drops both end
//!    the response, and the browser reconnects with `Last-Event-ID`, which is
//!    replayed from SQLite before the live feed resumes.
//! 3. **Terminates on shutdown.** Otherwise an open tail would hold graceful
//!    shutdown open forever and block every redeploy.

use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{Duration, MissedTickBehavior};

use crate::error::AppError;
use crate::hub::LogFrame;
use crate::state::AppState;

/// SSE comment frame. Keeps proxies from timing the connection out.
const KEEPALIVE: Bytes = Bytes::from_static(b": keepalive\n\n");
/// Tells the browser how soon to reconnect after we close a stream.
const RETRY_HINT: Bytes = Bytes::from_static(b"retry: 2000\n\n");
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// Upper bound on gap replay, so a client reconnecting after a long absence
/// cannot ask the server to read the whole table into memory.
const MAX_REPLAY: i64 = 5000;

struct StreamCtx {
    state: Arc<AppState>,
    rx: tokio::sync::broadcast::Receiver<Arc<LogFrame>>,
    keepalive: tokio::time::Interval,
    shutdown: tokio::sync::watch::Receiver<bool>,
    /// Ids at or below this were already delivered by the replay.
    high_water: i64,
}

pub async fn stream(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if state.started_shutdown() {
        return Err(AppError::Overloaded);
    }

    // Subscribe *before* querying the replay, so nothing published between the
    // two steps is missed. The overlap is removed by `high_water` below.
    let rx = state.hub.subscribe();

    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok());

    let mut replay: Vec<Bytes> = vec![RETRY_HINT];
    let mut high_water = last_event_id.unwrap_or(0);

    if let Some(after) = last_event_id {
        let missed = state.store.reader.since_id(after, MAX_REPLAY).await?;
        if !missed.is_empty() {
            tracing::debug!(count = missed.len(), after, "replaying gap for reconnect");
        }
        for rec in &missed {
            high_water = high_water.max(rec.id);
            replay.push(LogFrame::new(rec).bytes);
        }
    }

    let mut keepalive = tokio::time::interval(KEEPALIVE_INTERVAL);
    // Without this, ticks missed while a burst of logs is being written would
    // fire back to back afterwards.
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let ctx = StreamCtx {
        shutdown: state.shutdown.clone(),
        state: state.clone(),
        rx,
        keepalive,
        high_water,
    };

    state.metrics.sse_opened.fetch_add(1, Ordering::Relaxed);

    let live = futures_util::stream::unfold(ctx, |mut ctx| async move {
        loop {
            tokio::select! {
                biased;

                _ = ctx.shutdown.changed() => return None,

                received = ctx.rx.recv() => match received {
                    Ok(frame) => {
                        // Already delivered by the replay above.
                        if frame.id <= ctx.high_water {
                            continue;
                        }
                        // Cloning `Bytes` bumps a refcount; it does not copy
                        // the payload. This is the whole fan-out cost.
                        return Some((Ok::<Bytes, Infallible>(frame.bytes.clone()), ctx));
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        ctx.state.metrics.sse_evicted.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(skipped, "SSE subscriber fell behind; closing stream");
                        // Closing is the point: the client reconnects with
                        // Last-Event-ID and we replay the gap from SQLite,
                        // instead of growing a per-client buffer here.
                        return None;
                    }
                    Err(RecvError::Closed) => return None,
                },

                _ = ctx.keepalive.tick() => {
                    return Some((Ok(KEEPALIVE), ctx));
                }
            }
        }
    });

    // The first interval tick fires immediately, which conveniently flushes the
    // response headers to the client before any log arrives.
    let body =
        futures_util::stream::iter(replay.into_iter().map(Ok::<Bytes, Infallible>)).chain(live);

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            // Stops nginx-style proxies from buffering the stream.
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        Body::from_stream(body),
    )
        .into_response())
}
