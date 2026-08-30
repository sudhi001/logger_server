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
"#;

/// Columns added after the initial release. SQLite has no `ADD COLUMN IF NOT
/// EXISTS`, so each is applied only when absent.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[(
    "logs",
    "device_id",
    "ALTER TABLE logs ADD COLUMN device_id INTEGER",
)];

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
