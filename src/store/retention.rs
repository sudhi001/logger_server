//! Retention. Without this the database grows without bound, which is how the
//! original service would eventually fill its disk.

use rusqlite::Connection;

use crate::config::Config;
use crate::model::now_millis;

/// Enforces the row cap and the age cap, then truncates the WAL.
///
/// Called from the writer thread only, so it never contends for the write lock.
pub fn prune(conn: &Connection, cfg: &Config) {
    if let Some(max_age) = cfg.max_age {
        let cutoff = now_millis() - max_age.as_millis() as i64;
        if let Err(e) = conn.execute("DELETE FROM logs WHERE ts < ?1", [cutoff]) {
            tracing::warn!(error = %e, "age-based prune failed");
        }
    }

    if cfg.max_rows > 0 {
        // Delete by id rather than by COUNT/OFFSET: the primary key index makes
        // this a range delete instead of a scan.
        let sql = "DELETE FROM logs WHERE id <= (SELECT MAX(id) FROM logs) - ?1";
        if let Err(e) = conn.execute(sql, [cfg.max_rows]) {
            tracing::warn!(error = %e, "row-cap prune failed");
        }
    }

    // Deletes only grow the WAL until it is checkpointed.
    if let Err(e) = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE") {
        tracing::debug!(error = %e, "wal checkpoint skipped");
    }
}
