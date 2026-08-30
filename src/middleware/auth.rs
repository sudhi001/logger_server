//! Optional shared-secret authentication on writes.
//!
//! Disabled entirely when `LOGGER_API_KEY` is unset, so enabling this port
//! breaks no existing client. Opt in by setting the variable.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::AppError;
use crate::state::AppState;
use std::sync::Arc;

/// Compares in time independent of how many leading bytes match, so the header
/// cannot be used as an oracle to recover the key byte by byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub async fn require_api_key(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let Some(expected) = state.cfg.api_key.as_deref() else {
        return Ok(next.run(req).await);
    };

    let presented = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        Ok(next.run(req).await)
    } else {
        Err(AppError::Unauthorized)
    }
}
