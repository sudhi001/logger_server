//! Device management. Reachable only with a dashboard session or the admin token.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use crate::error::AppError;
use crate::model::{now_millis, Device, DeviceCreated, NewDevice};
use crate::state::AppState;
use crate::store::devices;

/// Registers a device and returns its token.
///
/// This is the only moment the plaintext token exists on the server; only its
/// digest is stored, so it cannot be shown again.
pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(input): Json<NewDevice>,
) -> Result<(StatusCode, Json<DeviceCreated>), AppError> {
    let created = state.store.with_admin(|conn, cache| {
        devices::create(
            conn,
            cache,
            &input.name,
            input.platform.as_deref(),
            now_millis(),
        )
    })?;
    tracing::info!(device = %created.device.name, id = created.device.id, "device registered");
    Ok((StatusCode::CREATED, Json(created)))
}

pub async fn list(State(state): State<Arc<AppState>>) -> Result<Json<Vec<Device>>, AppError> {
    let list = state.store.with_admin(|conn, _| devices::list(conn))?;
    Ok(Json(list))
}

/// Revokes a device. Its token stops working on the next request, not at some
/// later cache refresh.
pub async fn revoke(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    state
        .store
        .with_admin(|conn, cache| devices::revoke(conn, cache, id, now_millis()))?;
    tracing::info!(id, "device revoked");
    Ok(StatusCode::NO_CONTENT)
}
