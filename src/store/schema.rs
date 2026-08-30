//! Schema definition and the pragmas that make RSS deterministic.

use rusqlite::{Connection, Result};

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS logs (
  id      INTEGER PRIMARY KEY,   -- supplied by the app, never AUTOINCREMENT
  ts      INTEGER NOT NULL,      -- unix millis
  name    TEXT    NOT NULL,
  level   INTEGER NOT NULL DEFAULT 2,
  message TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_logs_name_id ON logs(name, id DESC);
CREATE INDEX IF NOT EXISTS idx_logs_ts      ON logs(ts);

CREATE TABLE IF NOT EXISTS devices (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  name         TEXT    NOT NULL,
  platform     TEXT,
  token_hash   BLOB    NOT NULL,
  token_prefix TEXT    NOT NULL,
  created_at   INTEGER NOT NULL,
  last_seen    INTEGER,
  revoked_at   INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_devices_token_hash ON devices(token_hash);

CREATE TABLE IF NOT EXISTS alert_rules (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  name          TEXT    NOT NULL,
  enabled       INTEGER NOT NULL DEFAULT 1,

  -- What to match. NULL means "any".
  min_level     INTEGER NOT NULL DEFAULT 4,
  device_id     INTEGER,
  name_filter   TEXT,
  contains      TEXT,

  -- When to fire: `threshold` matches inside `window_secs`, then silent for
  -- `cooldown_secs` so a crash loop is one alert rather than a thousand.
  threshold     INTEGER NOT NULL DEFAULT 1,
  window_secs   INTEGER NOT NULL DEFAULT 300,
  cooldown_secs INTEGER NOT NULL DEFAULT 900,

  -- Where to send it.
  url           TEXT    NOT NULL,
  format        TEXT    NOT NULL DEFAULT 'generic',
  secret        TEXT,

  created_at    INTEGER NOT NULL,
  last_fired_at INTEGER,
  fire_count    INTEGER NOT NULL DEFAULT 0,
  last_error    TEXT
);
"#;

/// Full-text search over log text.
///
/// An external-content table: FTS5 keeps only the index and reads the columns
/// back from `logs`, so message text is not stored twice. Triggers keep it in
/// step, which matters because retention deletes rows behind our back.
const FTS_DDL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS logs_fts USING fts5(
  message,
  name,
  content='logs',
  content_rowid='id',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER IF NOT EXISTS logs_fts_ai AFTER INSERT ON logs BEGIN
  INSERT INTO logs_fts(rowid, message, name) VALUES (new.id, new.message, new.name);
END;

CREATE TRIGGER IF NOT EXISTS logs_fts_ad AFTER DELETE ON logs BEGIN
  INSERT INTO logs_fts(logs_fts, rowid, message, name)
  VALUES ('delete', old.id, old.message, old.name);
END;

CREATE TRIGGER IF NOT EXISTS logs_fts_au AFTER UPDATE ON logs BEGIN
  INSERT INTO logs_fts(logs_fts, rowid, message, name)
  VALUES ('delete', old.id, old.message, old.name);
  INSERT INTO logs_fts(rowid, message, name) VALUES (new.id, new.message, new.name);
END;
"#;

/// Columns added after the initial release. SQLite has no `ADD COLUMN IF NOT
/// EXISTS`, so each is applied only when absent.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    (
        "logs",
        "device_id",
        "ALTER TABLE logs ADD COLUMN device_id INTEGER",
    ),
    // Arbitrary caller-supplied JSON object: session id, app version, user id.
    // Stored as text; SQLite has no JSON type.
    (
        "logs",
        "context",
        "ALTER TABLE logs ADD COLUMN context TEXT",
    ),
];

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Brings an existing database up to the current schema.
fn migrate(conn: &Connection) -> Result<()> {
    for (table, column, ddl) in ADDED_COLUMNS {
        if !column_exists(conn, table, column)? {
            tracing::info!(table, column, "adding column");
            conn.execute_batch(ddl)?;
        }
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_logs_device_id ON logs(device_id, id DESC)",
    )?;
    build_fts(conn)?;
    Ok(())
}

/// Creates the search index, and backfills it if the database predates it.
fn build_fts(conn: &Connection) -> Result<()> {
    let existed: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='logs_fts'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);

    conn.execute_batch(FTS_DDL)?;

    if !existed {
        // The triggers only cover rows written from now on, so rebuild once
        // over whatever is already stored.
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))?;
        if rows > 0 {
            tracing::info!(rows, "backfilling the search index (one time)");
            conn.execute_batch("INSERT INTO logs_fts(logs_fts) VALUES ('rebuild')")?;
        }
    }
    Ok(())
}

/// Pragmas applied to every connection, reader and writer alike.
///
/// `cache_size` is negative on purpose: SQLite reads a negative value as KiB,
/// so this is a hard 2 MiB page cache rather than 2000 *pages* (~8 MiB).
/// `mmap_size = 0` keeps the database out of the address space so that RSS
/// reflects real memory use and stays bounded on large tables.
fn apply_common_pragmas(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "cache_size", -2000)?;
    conn.pragma_update(None, "mmap_size", 0i64)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(())
}

/// Explains a failure to open the database in terms of what to actually do.
///
/// SQLite reports a directory it cannot write into as "attempt to write a
/// readonly database", which sends people looking for a read-only flag that
/// does not exist. The container runs as uid 65532, so the usual cause is a
/// volume still owned by root from an older version.
pub fn describe_open_failure(path: &str, err: &rusqlite::Error) -> String {
    let dir = std::path::Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));

    if dir_is_writable(dir) {
        return format!("cannot open the database at {path}: {err}");
    }

    format!(
        "cannot open the database at {path}: {err}\n\
         \n\
         The directory {} is not writable by this process (uid {}). SQLite needs \
         to create logs.db-wal and logs.db-shm alongside the database, not just \
         the database file.\n\
         \n\
         If this is a Docker volume created before version 3.4.0, it is still \
         owned by root while the container now runs as uid 65532. Fix it once:\n\
         \n\
         \x20 docker run --rm -v <volume>:/data alpine chown -R 65532:65532 /data",
        dir.display(),
        current_uid()
    )
}

/// Whether *this* process can create a file in `dir`.
///
/// `Permissions::readonly()` is the obvious-looking call and the wrong one: it
/// reports the mode bits, so a root-owned 0755 directory reads as writable even
/// though an unprivileged process cannot touch it — which is exactly the case
/// this message exists to explain. Actually creating a file is the only honest
/// test.
fn dir_is_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".logger_server_write_probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// `geteuid` without pulling in a dependency for one call.
fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        extern "C" {
            fn geteuid() -> u32;
        }
        unsafe { geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Opens the single read-write connection and initialises the schema.
pub fn open_writer(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // WAL lets readers run concurrently with the writer. It is a persistent
    // property of the database file, so setting it here covers readers too.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    apply_common_pragmas(&conn)?;
    conn.execute_batch(DDL)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Opens a read-only connection for the reader pool.
pub fn open_reader(path: &str) -> Result<Connection> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    apply_common_pragmas(&conn)?;
    Ok(conn)
}

/// Highest id currently stored, used to seed the ingest counter at boot.
pub fn max_id(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COALESCE(MAX(id), 0) FROM logs", [], |r| r.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writability_is_tested_by_writing_not_by_mode_bits() {
        let base = std::env::temp_dir().join(format!(
            "logger_probe_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        assert!(dir_is_writable(&base), "a fresh temp dir is writable");

        // A directory that does not exist is not writable, and must not panic.
        assert!(!dir_is_writable(&base.join("does-not-exist")));

        // The probe file must not be left behind.
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
        std::fs::remove_dir_all(&base).ok();
    }
}
