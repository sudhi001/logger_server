//! Alert rule management. Viewer credential required, like device management.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::alerts::guard;
use crate::error::AppError;
use crate::model::{now_millis, AlertEvent, AlertRule, LogRecord, NewAlertRule};
use crate::state::AppState;
use crate::store::alerts;

/// Rejects a URL before it is stored, so the mistake is visible at the moment
/// it is made rather than silently at 3am when the alert should have fired.
async fn validate_url(state: &AppState, url: &str) -> Result<(), AppError> {
    let url = url.trim().to_string();
    let allow = state.cfg.webhook_allow_private;
    // DNS resolution blocks. spawn_blocking rather than block_in_place, because
    // the latter requires a multi-threaded runtime and panics without one.
    tokio::task::spawn_blocking(move || guard::check(&url, allow))
        .await
        .map_err(|e| AppError::Internal(format!("url check failed: {e}")))?
        .map_err(|e| AppError::BadRequest(e.to_string()))
}

pub async fn list(State(state): State<Arc<AppState>>) -> Result<Json<Vec<AlertRule>>, AppError> {
    Ok(Json(state.store.with_admin(|conn, _| alerts::list(conn))?))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(input): Json<NewAlertRule>,
) -> Result<(StatusCode, Json<AlertRule>), AppError> {
    validate_url(&state, &input.url).await?;
    let rule = state
        .store
        .with_admin(|conn, _| alerts::create(conn, input, now_millis()))?;
    state.reload_alerts();
    tracing::info!(rule = %rule.name, id = rule.id, "alert rule created");
    Ok((StatusCode::CREATED, Json(rule)))
}

#[derive(Debug, Deserialize)]
pub struct EnabledBody {
    pub enabled: bool,
}

pub async fn set_enabled(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<EnabledBody>,
) -> Result<StatusCode, AppError> {
    state
        .store
        .with_admin(|conn, _| alerts::set_enabled(conn, id, body.enabled))?;
    state.reload_alerts();
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    state.store.with_admin(|conn, _| alerts::delete(conn, id))?;
    state.reload_alerts();
    Ok(StatusCode::NO_CONTENT)
}

/// Delivers a synthetic alert immediately, ignoring threshold and cooldown.
///
/// Worth having as its own endpoint: the alternative is configuring a webhook
/// and finding out whether it works during the incident you needed it for.
pub async fn test(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let rule = state
        .store
        .with_admin(|conn, _| alerts::get(conn, id))?
        .ok_or_else(|| AppError::BadRequest(format!("no alert rule with id {id}")))?;

    let event = AlertEvent {
        rule_id: rule.id,
        rule_name: rule.name.clone(),
        count: rule.threshold,
        window_secs: rule.window_secs,
        fired_at: now_millis(),
        trigger: LogRecord {
            id: 0,
            ts: now_millis(),
            name: "[test] ".into(),
            level: rule.min_level.max(4),
            message: format!(
                "Test alert for \"{}\". If you are reading this, the webhook works.",
                rule.name
            ),
            device_id: None,
            device: Some("logger_server".into()),
            context: Some(serde_json::json!({ "test": true })),
        },
    };

    let preview = crate::alerts::delivery::payload(&rule, &event, &state.cfg.public_url);

    match state.alerts_test_sender().try_send(event) {
        Ok(()) => Ok(Json(serde_json::json!({
            "queued": true,
            "message": "Test alert queued. Check the rule's last_error afterwards to see \
                        whether delivery succeeded.",
            "payload": preview,
        }))),
        Err(_) => Err(AppError::Overloaded),
    }
}
