//! Authentication: device tokens for writing, sessions for reading.

pub mod session;
pub mod token;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::AppError;
use crate::state::AppState;
use crate::store::devices::DeviceIdentity;

/// Header a device may use instead of `Authorization: Bearer`.
const DEVICE_HEADER: &str = "x-device-token";
/// Header for scripted admin access, as an alternative to a browser session.
const ADMIN_HEADER: &str = "x-admin-token";

/// Authenticates a registered device and attaches its identity to the request.
///
/// The write path resolves the token against an in-memory map, so this costs a
/// SHA-256 and a hash lookup rather than a database round trip.
pub async fn require_device(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let presented = token::from_headers(req.headers(), DEVICE_HEADER)
        .ok_or(AppError::Unauthorized)?
        .to_string();

    let Some(identity) = state.devices.lookup(&presented) else {
        // Deliberately identical to the missing-token case: a distinct error
        // would confirm to a prober which tokens exist.
        return Err(AppError::Unauthorized);
    };

    state.note_device_seen(identity.id);
    req.extensions_mut().insert(identity);
    Ok(next.run(req).await)
}

/// Accepts either a valid dashboard session or the admin token.
///
/// Used for reads and for device management, so the UI works from a browser and
/// scripts work with a bearer token.
pub async fn require_viewer(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    if is_authorised(&state, &req) {
        Ok(next.run(req).await)
    } else {
        Err(AppError::Unauthorized)
    }
}

pub fn is_authorised(state: &AppState, req: &Request) -> bool {
    if let Some(id) = session::from_cookies(req.headers()) {
        if state.sessions.is_valid(&id) {
            return true;
        }
    }
    if let Some(presented) = token::from_headers(req.headers(), ADMIN_HEADER) {
        return token::constant_time_eq(presented.as_bytes(), state.cfg.admin_token.as_bytes());
    }
    false
}

/// Convenience for handlers that want the calling device.
pub fn device_of(req: &Request) -> Option<&DeviceIdentity> {
    req.extensions().get::<DeviceIdentity>()
}
