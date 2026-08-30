//! Storage facade: id allocation, the bounded write queue, and the read pool.

pub mod alerts;
pub mod devices;
pub mod reader;
pub mod retention;
pub mod schema;
pub mod writer;

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;

use rusqlite::Connection;
use tokio::sync::oneshot;

use crate::config::Config;
use crate::error::AppError;
use crate::model::LogRecord;

pub use devices::DeviceCache;
pub use reader::Reader;
pub use writer::WriteItem;

pub struct Store {
    next_id: AtomicI64,
    tx: SyncSender<WriteItem>,
    draining: Arc<AtomicBool>,
    pub reader: Reader,
    pub devices: Arc<DeviceCache>,
    /// A second read-write connection used only for device administration.
    ///
    /// The writer thread owns the connection on the hot path; device create and
    /// revoke are a handful of operations per day, so letting them take their
    /// own connection is far simpler than routing them through the write queue.
    /// WAL plus `busy_timeout` serialises the two safely.
    admin: Mutex<Connection>,
}

/// Returned to `main` so it can flush the writer on shutdown.
pub struct WriterHandle {
    draining: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WriterHandle {
    /// Signals the writer to drain its queue and exit, then waits for it.
    pub fn shutdown(mut self) {
        self.draining.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Store {
    pub fn open(cfg: &Config) -> Result<(Self, WriterHandle), AppError> {
        let (conn, max_id) = writer::open(cfg)?;
        let reader = Reader::new(&cfg.db_path, cfg.reader_conns)?;
        let device_cache = Arc::new(DeviceCache::load(&conn)?);
        let admin = schema::open_writer(&cfg.db_path)?;

        // Bounded: when full, ingest sheds load rather than growing memory.
        let (tx, rx) = sync_channel::<WriteItem>(cfg.ingest_queue);
        let draining = Arc::new(AtomicBool::new(false));

        let thread = {
            let cfg = cfg.clone();
            let draining = draining.clone();
            let devices = device_cache.clone();
            std::thread::Builder::new()
                .name("sqlite-writer".into())
                .spawn(move || writer::run(conn, rx, cfg, draining, devices))
                .map_err(|e| AppError::Internal(format!("cannot spawn writer: {e}")))?
        };

        tracing::info!(db = %cfg.db_path, max_id, "store opened");

        Ok((
            Self {
                next_id: AtomicI64::new(max_id),
                tx,
                draining: draining.clone(),
                reader,
                devices: device_cache,
                admin: Mutex::new(admin),
            },
            WriterHandle {
                draining,
                thread: Some(thread),
            },
        ))
    }

    /// Allocates the next monotonic id.
    ///
    /// Ids come from here rather than from SQLite `AUTOINCREMENT` so that the
    /// ingest endpoint can return an id before the row is durable, and so that
    /// ids stay monotonic even as retention deletes from the tail.
    pub fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Queues a record. Non-blocking; returns `Overloaded` when the queue is full.
    pub fn enqueue(&self, rec: LogRecord) -> Result<(), AppError> {
        self.send(WriteItem { rec, ack: None })
    }

    /// Queues a record and returns a receiver that fires once it is committed.
    pub fn enqueue_sync(
        &self,
        rec: LogRecord,
    ) -> Result<oneshot::Receiver<Result<(), String>>, AppError> {
        let (ack, rx) = oneshot::channel();
        self.send(WriteItem {
            rec,
            ack: Some(ack),
        })?;
        Ok(rx)
    }

    /// Runs a device-administration statement against the admin connection.
    pub fn with_admin<T>(
        &self,
        f: impl FnOnce(&Connection, &DeviceCache) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let conn = self
            .admin
            .lock()
            .map_err(|_| AppError::Internal("admin connection poisoned".into()))?;
        f(&conn, &self.devices)
    }

    fn send(&self, item: WriteItem) -> Result<(), AppError> {
        if self.draining.load(Ordering::Relaxed) {
            return Err(AppError::Overloaded);
        }
        match self.tx.try_send(item) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(AppError::Overloaded),
            Err(TrySendError::Disconnected(_)) => {
                Err(AppError::Internal("writer thread is gone".into()))
            }
        }
    }
}
