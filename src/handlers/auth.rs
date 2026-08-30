//! Dashboard login.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::{session, token};
use crate::error::AppError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub token: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub ok: bool,
}

fn cookie(state: &AppState, value: &str, max_age: i64) -> String {
    let mut c = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        session::COOKIE_NAME,
        value,
        max_age
    );
    if state.cfg.cookie_secure {
        c.push_str("; Secure");
    }
    c
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginRequest>,
) -> Result<Response, AppError> {
    if !token::constant_time_eq(
        body.token.trim().as_bytes(),
        state.cfg.admin_token.as_bytes(),
    ) {
        state.metrics.auth_failures.fetch_add(1, Ordering::Relaxed);
        // A uniform delay would be better still, but the token is high-entropy
        // and the comparison above is already constant-time.
        return Err(AppError::Unauthorized);
    }

    let id = state
        .sessions
        .create()
        .ok_or_else(|| AppError::Internal("cannot create session".into()))?;

    let ttl = state.cfg.session_ttl.as_secs() as i64;
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, cookie(&state, &id, ttl))],
        Json(LoginResponse { ok: true }),
    )
        .into_response())
}

pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(id) = session::from_cookies(&headers) {
        state.sessions.revoke(&id);
    }
    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie(&state, "", 0))],
        Json(LoginResponse { ok: true }),
    )
        .into_response()
}

/// Lets the dashboard decide whether to render or bounce to the login screen.
pub async fn whoami(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let authorised = session::from_cookies(&headers)
        .map(|id| state.sessions.is_valid(&id))
        .unwrap_or(false);

    if authorised {
        (StatusCode::OK, Json(LoginResponse { ok: true })).into_response()
    } else {
        (StatusCode::UNAUTHORIZED, Json(LoginResponse { ok: false })).into_response()
    }
}
