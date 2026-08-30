//! The single writer thread.
//!
//! One dedicated OS thread owns the only read-write connection, so there is no
//! write-lock contention and `SQLITE_BUSY` cannot occur on the write path.
//!
//! Batching strategy: block for the first item, then drain whatever else has
//! already queued (up to `MAX_BATCH`) and commit it in one transaction. Under
//! low load this writes immediately with no added latency; under high load
//! batches form naturally and amortise the fsync across hundreds of rows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tokio::sync::oneshot;

use crate::config::Config;
use crate::model::LogRecord;
use crate::store::{retention, schema};

const MAX_BATCH: usize = 256;
const RETENTION_INTERVAL: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct WriteItem {
    pub rec: LogRecord,
    /// Present only for `?sync=true` ingests, which wait for durability.
    pub ack: Option<oneshot::Sender<Result<(), String>>>,
}

/// Runs until `draining` is set and the queue is empty, or all senders drop.
///
/// `recv_timeout` rather than `recv` so that the retention sweep still fires on
/// an otherwise idle server, and so shutdown is observed promptly.
pub fn run(mut conn: Connection, rx: Receiver<WriteItem>, cfg: Config, draining: Arc<AtomicBool>) {
    let mut batch: Vec<WriteItem> = Vec::with_capacity(MAX_BATCH);
    let mut last_retention = Instant::now();

    loop {
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(item) => {
                batch.push(item);
                // Opportunistically drain whatever already queued behind it.
                while batch.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(item) => batch.push(item),
                        Err(_) => break,
                    }
                }
                commit_batch(&mut conn, &mut batch);
            }
            Err(RecvTimeoutError::Timeout) => {
                // Queue is empty. If we are shutting down, this is the exit.
                if draining.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if last_retention.elapsed() >= RETENTION_INTERVAL {
            retention::prune(&conn, &cfg);
            last_retention = Instant::now();
        }
    }

    // Drain anything that raced the shutdown, so a redeploy loses nothing.
    loop {
        while batch.len() < MAX_BATCH {
            match rx.try_recv() {
                Ok(item) => batch.push(item),
                Err(_) => break,
            }
        }
        if batch.is_empty() {
            break;
        }
        commit_batch(&mut conn, &mut batch);
    }

    // Fold the WAL back into the main file so the next boot starts clean.
    let _ = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE");
    tracing::info!("writer thread stopped");
}

fn commit_batch(conn: &mut Connection, batch: &mut Vec<WriteItem>) {
    if batch.is_empty() {
        return;
    }
    let result = insert_all(conn, batch);
    let msg = result.as_ref().err().map(|e| e.to_string());
    if let Some(ref e) = msg {
        tracing::error!(error = %e, rows = batch.len(), "batch insert failed");
    }
    // Notify only the callers that asked for a durable ack.
    for item in batch.drain(..) {
        if let Some(ack) = item.ack {
            let _ = ack.send(match &msg {
                Some(e) => Err(e.clone()),
                None => Ok(()),
            });
        }
    }
}

fn insert_all(conn: &mut Connection, batch: &[WriteItem]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO logs (id, ts, name, level, message) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for item in batch {
            let r = &item.rec;
            stmt.execute(rusqlite::params![r.id, r.ts, r.name, r.level, r.message])?;
        }
    }
    tx.commit()
}

/// Opens the writer connection and returns it with the highest existing id.
pub fn open(cfg: &Config) -> rusqlite::Result<(Connection, i64)> {
    let conn = schema::open_writer(&cfg.db_path)?;
    let max = schema::max_id(&conn)?;
    Ok((conn, max))
}
