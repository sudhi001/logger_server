//! Read path: a small pool of read-only connections used from blocking tasks,
//! plus a streaming cursor for the unbounded export endpoint.

use std::sync::{Arc, Mutex};

use bytes::{BufMut, BytesMut};
use rusqlite::Connection;
use tokio::sync::{mpsc, Semaphore};

use crate::error::AppError;
use crate::model::LogRecord;
use crate::store::schema;

/// Rows accumulated per chunk pushed to the client during an export.
const EXPORT_CHUNK_ROWS: usize = 256;
/// Chunks buffered in flight. Small on purpose: a slow client backpressures
/// the SQLite cursor instead of accumulating rows in memory.
const EXPORT_CHUNK_BUFFER: usize = 2;
/// Concurrent exports. Capped separately from the pool so that a long-running
/// export cannot starve the fast-path queries.
const MAX_CONCURRENT_EXPORTS: usize = 2;

#[derive(Clone)]
pub struct Reader {
    path: Arc<str>,
    pool: Arc<Mutex<Vec<Connection>>>,
    permits: Arc<Semaphore>,
    exports: Arc<Semaphore>,
}

impl Reader {
    pub fn new(path: &str, size: usize) -> rusqlite::Result<Self> {
        let mut conns = Vec::with_capacity(size);
        for _ in 0..size {
            conns.push(schema::open_reader(path)?);
        }
        Ok(Self {
            path: Arc::from(path),
            pool: Arc::new(Mutex::new(conns)),
            permits: Arc::new(Semaphore::new(size)),
            exports: Arc::new(Semaphore::new(MAX_CONCURRENT_EXPORTS)),
        })
    }

    /// Runs `f` against a pooled connection on a blocking thread.
    async fn with_conn<F, T>(&self, f: F) -> Result<T, AppError>
    where
        F: FnOnce(&Connection) -> Result<T, AppError> + Send + 'static,
        T: Send + 'static,
    {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::Internal("reader pool closed".into()))?;

        let pool = self.pool.clone();
        let path = self.path.clone();

        tokio::task::spawn_blocking(move || {
            // A permit normally guarantees an available connection. If a prior
            // task panicked mid-use its connection was dropped, so open a
            // replacement rather than panicking again.
            let conn = match pool.lock().unwrap().pop() {
                Some(c) => c,
                None => schema::open_reader(&path)?,
            };
            let result = f(&conn);
            pool.lock().unwrap().push(conn);
            drop(permit);
            result
        })
        .await
        .map_err(|e| AppError::Internal(format!("reader task failed: {e}")))?
    }

    pub async fn recent(
        &self,
        limit: i64,
        before_id: Option<i64>,
        min_level: Option<u8>,
        device_id: Option<i64>,
    ) -> Result<Vec<LogRecord>, AppError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT l.id, l.ts, l.name, l.level, l.message, l.device_id, d.name
                 FROM logs l LEFT JOIN devices d ON d.id = l.device_id
                 WHERE l.id < ?1
                   AND l.level >= ?2
                   AND (?3 IS NULL OR l.device_id = ?3)
                 ORDER BY l.id DESC LIMIT ?4",
            )?;
            let cursor = before_id.unwrap_or(i64::MAX);
            let out = collect(stmt.query(rusqlite::params![
                cursor,
                min_level.unwrap_or(0),
                device_id,
                limit
            ])?);
            out
        })
        .await
    }

    pub async fn by_name(
        &self,
        name: String,
        limit: i64,
        before_id: Option<i64>,
    ) -> Result<Vec<LogRecord>, AppError> {
        self.with_conn(move |conn| {
            // Served by idx_logs_name_id; the original service full-scanned here.
            let mut stmt = conn.prepare_cached(
                "SELECT l.id, l.ts, l.name, l.level, l.message, l.device_id, d.name
                 FROM logs l LEFT JOIN devices d ON d.id = l.device_id
                 WHERE l.name = ?1 AND l.id < ?2
                 ORDER BY l.id DESC LIMIT ?3",
            )?;
            let cursor = before_id.unwrap_or(i64::MAX);
            let out = collect(stmt.query(rusqlite::params![name, cursor, limit])?);
            out
        })
        .await
    }

    /// Rows written after `after_id`, ascending. Used to replay the gap when an
    /// SSE client reconnects with `Last-Event-ID`.
    pub async fn since_id(&self, after_id: i64, limit: i64) -> Result<Vec<LogRecord>, AppError> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT l.id, l.ts, l.name, l.level, l.message, l.device_id, d.name
                 FROM logs l LEFT JOIN devices d ON d.id = l.device_id
                 WHERE l.id > ?1 ORDER BY l.id ASC LIMIT ?2",
            )?;
            let out = collect(stmt.query(rusqlite::params![after_id, limit])?);
            out
        })
        .await
    }

    pub async fn count(&self) -> Result<i64, AppError> {
        self.with_conn(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))?))
            .await
    }

    /// Streams the entire table without ever materialising it.
    ///
    /// A dedicated connection steps the cursor row by row on a blocking thread,
    /// pushing fixed-size chunks into a bounded channel. Peak memory is one
    /// chunk regardless of table size — this is what makes `GET /logs` safe.
    pub async fn export(
        &self,
        ndjson: bool,
    ) -> Result<mpsc::Receiver<Result<bytes::Bytes, std::io::Error>>, AppError> {
        let permit = self
            .exports
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AppError::Internal("export limiter closed".into()))?;

        // Opened fresh rather than taken from the pool: an export can run for
        // minutes and must not hold a pooled connection hostage.
        let path = self.path.clone();
        let (tx, rx) = mpsc::channel(EXPORT_CHUNK_BUFFER);

        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let conn = match schema::open_reader(&path) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.blocking_send(Err(std::io::Error::other(e.to_string())));
                    return;
                }
            };
            if let Err(e) = stream_rows(&conn, &tx, ndjson) {
                // The client hanging up is the normal way this ends.
                tracing::debug!(error = %e, "export ended early");
            }
        });

        Ok(rx)
    }
}

fn collect(mut rows: rusqlite::Rows<'_>) -> Result<Vec<LogRecord>, AppError> {
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(LogRecord {
            id: row.get(0)?,
            ts: row.get(1)?,
            name: row.get(2)?,
            level: row.get(3)?,
            message: row.get(4)?,
            device_id: row.get(5)?,
            device: row.get(6)?,
        });
    }
    Ok(out)
}

/// Walks the table and emits either a JSON array (incrementally, so existing
/// clients that `JSON.parse` the body are unaffected) or NDJSON.
fn stream_rows(
    conn: &Connection,
    tx: &mpsc::Sender<Result<bytes::Bytes, std::io::Error>>,
    ndjson: bool,
) -> Result<(), String> {
    let mut stmt = conn
        .prepare(
            "SELECT l.id, l.ts, l.name, l.level, l.message, l.device_id, d.name
             FROM logs l LEFT JOIN devices d ON d.id = l.device_id
             ORDER BY l.id ASC",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;

    let mut buf = BytesMut::with_capacity(16 * 1024);
    let mut in_chunk = 0usize;
    let mut first = true;

    if !ndjson {
        buf.put_u8(b'[');
    }

    loop {
        let row = match rows.next() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => return Err(e.to_string()),
        };
        let rec = LogRecord {
            id: row.get(0).map_err(|e| e.to_string())?,
            ts: row.get(1).map_err(|e| e.to_string())?,
            name: row.get(2).map_err(|e| e.to_string())?,
            level: row.get(3).map_err(|e| e.to_string())?,
            message: row.get(4).map_err(|e| e.to_string())?,
            device_id: row.get(5).map_err(|e| e.to_string())?,
            device: row.get(6).map_err(|e| e.to_string())?,
        };

        if ndjson {
            serde_json::to_writer((&mut buf).writer(), &rec).map_err(|e| e.to_string())?;
            buf.put_u8(b'\n');
        } else {
            if !first {
                buf.put_u8(b',');
            }
            serde_json::to_writer((&mut buf).writer(), &rec).map_err(|e| e.to_string())?;
        }
        first = false;
        in_chunk += 1;

        if in_chunk >= EXPORT_CHUNK_ROWS {
            // Blocks when the client is slow: backpressure, not buffering.
            tx.blocking_send(Ok(buf.split().freeze()))
                .map_err(|_| "client disconnected".to_string())?;
            in_chunk = 0;
        }
    }

    if !ndjson {
        buf.put_u8(b']');
    }
    if !buf.is_empty() {
        tx.blocking_send(Ok(buf.freeze()))
            .map_err(|_| "client disconnected".to_string())?;
    }
    Ok(())
}
