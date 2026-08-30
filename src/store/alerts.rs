//! Persistence for alert rules.

use rusqlite::Connection;

use crate::error::AppError;
use crate::model::{AlertFormat, AlertRule, NewAlertRule};

const COLS: &str = "id, name, enabled, min_level, device_id, name_filter, contains, threshold, \
                    window_secs, cooldown_secs, url, format, secret, created_at, last_fired_at, \
                    fire_count, last_error";

fn row_to_rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<AlertRule> {
    let format: String = row.get(11)?;
    let secret: Option<String> = row.get(12)?;
    Ok(AlertRule {
        id: row.get(0)?,
        name: row.get(1)?,
        enabled: row.get::<_, i64>(2)? != 0,
        min_level: row.get(3)?,
        device_id: row.get(4)?,
        name_filter: row.get(5)?,
        contains: row.get(6)?,
        threshold: row.get(7)?,
        window_secs: row.get(8)?,
        cooldown_secs: row.get(9)?,
        url: row.get(10)?,
        format: AlertFormat::parse(&format).unwrap_or(AlertFormat::Generic),
        // The secret is never returned; only whether one is set.
        signed: secret.is_some_and(|s| !s.is_empty()),
        created_at: row.get(13)?,
        last_fired_at: row.get(14)?,
        fire_count: row.get(15)?,
        last_error: row.get(16)?,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<AlertRule>, AppError> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM alert_rules ORDER BY id DESC"))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_rule(row)?);
    }
    Ok(out)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<AlertRule>, AppError> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM alert_rules WHERE id = ?1"))?;
    let mut rows = stmt.query([id])?;
    Ok(match rows.next()? {
        Some(row) => Some(row_to_rule(row)?),
        None => None,
    })
}

/// The signing secret, fetched only at delivery time.
pub fn secret_for(conn: &Connection, id: i64) -> Option<String> {
    conn.query_row("SELECT secret FROM alert_rules WHERE id = ?1", [id], |r| {
        r.get::<_, Option<String>>(0)
    })
    .ok()
    .flatten()
    .filter(|s| !s.is_empty())
}

pub fn create(conn: &Connection, input: NewAlertRule, now: i64) -> Result<AlertRule, AppError> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("alert name is required".into()));
    }
    let format = match input.format.as_deref() {
        None => AlertFormat::Generic,
        Some(f) => AlertFormat::parse(f).ok_or_else(|| {
            AppError::BadRequest(format!(
                "unknown format {f:?}; use generic, slack, discord or pagerduty"
            ))
        })?,
    };

    // Clamped rather than rejected: a nonsensical window is a typo, not an
    // attack, and silently doing something sane beats a 400 mid-setup.
    let threshold = input.threshold.unwrap_or(1).clamp(1, 100_000);
    let window_secs = input.window_secs.unwrap_or(300).clamp(1, 86_400);
    let cooldown_secs = input.cooldown_secs.unwrap_or(900).clamp(0, 86_400);
    let min_level = input.min_level.unwrap_or(4).min(4);

    conn.execute(
        "INSERT INTO alert_rules
           (name, enabled, min_level, device_id, name_filter, contains, threshold,
            window_secs, cooldown_secs, url, format, secret, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            name,
            input.enabled.unwrap_or(true) as i64,
            min_level,
            input.device_id,
            input.name_filter.as_deref().filter(|s| !s.is_empty()),
            input.contains.as_deref().filter(|s| !s.is_empty()),
            threshold,
            window_secs,
            cooldown_secs,
            input.url.trim(),
            format.as_str(),
            input.secret.as_deref().filter(|s| !s.is_empty()),
            now,
        ],
    )?;
    let id = conn.last_insert_rowid();
    get(conn, id)?.ok_or_else(|| AppError::Internal("rule vanished after insert".into()))
}

pub fn set_enabled(conn: &Connection, id: i64, enabled: bool) -> Result<(), AppError> {
    let n = conn.execute(
        "UPDATE alert_rules SET enabled = ?2 WHERE id = ?1",
        rusqlite::params![id, enabled as i64],
    )?;
    if n == 0 {
        return Err(AppError::BadRequest(format!("no alert rule with id {id}")));
    }
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> Result<(), AppError> {
    let n = conn.execute("DELETE FROM alert_rules WHERE id = ?1", [id])?;
    if n == 0 {
        return Err(AppError::BadRequest(format!("no alert rule with id {id}")));
    }
    Ok(())
}

/// Records the outcome of a delivery attempt, so the UI can show whether a
/// webhook is actually working rather than only that it was configured.
pub fn record_result(conn: &Connection, id: i64, fired_at: i64, error: Option<&str>) {
    let _ = conn.execute(
        "UPDATE alert_rules
            SET last_fired_at = ?2, fire_count = fire_count + 1, last_error = ?3
          WHERE id = ?1",
        rusqlite::params![id, fired_at, error],
    );
}
