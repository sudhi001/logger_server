//! Device registry.
//!
//! Authentication happens on every write, so it must not touch SQLite: a
//! database lookup per log line would collapse the ingest path. The token
//! digest -> device map is therefore held in memory, loaded at boot and updated
//! whenever a device is created or revoked. SQLite remains the source of truth;
//! the map is a cache that is only ever written through.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

use rusqlite::Connection;

use crate::auth::token::{self, TokenHash};
use crate::error::AppError;
use crate::model::{Device, DeviceCreated};

/// What the write path needs to know about an authenticated caller.
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    pub id: i64,
    pub name: String,
}

#[derive(Default)]
pub struct DeviceCache {
    by_hash: RwLock<HashMap<TokenHash, DeviceIdentity>>,
    /// device id -> most recent activity, awaiting persistence.
    pending_seen: Mutex<HashMap<i64, i64>>,
}

impl DeviceCache {
    pub fn load(conn: &Connection) -> rusqlite::Result<Self> {
        let mut stmt =
            conn.prepare("SELECT id, name, token_hash FROM devices WHERE revoked_at IS NULL")?;
        let mut rows = stmt.query([])?;
        let mut map = HashMap::new();
        while let Some(row) = rows.next()? {
            let blob: Vec<u8> = row.get(2)?;
            if let Ok(hash) = TokenHash::try_from(blob.as_slice()) {
                map.insert(
                    hash,
                    DeviceIdentity {
                        id: row.get(0)?,
                        name: row.get(1)?,
                    },
                );
            }
        }
        tracing::info!(devices = map.len(), "device cache loaded");
        Ok(Self {
            by_hash: RwLock::new(map),
            pending_seen: Mutex::new(HashMap::new()),
        })
    }

    pub fn lookup(&self, presented: &str) -> Option<DeviceIdentity> {
        let hash = token::hash(presented);
        self.by_hash.read().ok()?.get(&hash).cloned()
    }

    fn insert(&self, hash: TokenHash, identity: DeviceIdentity) {
        if let Ok(mut map) = self.by_hash.write() {
            map.insert(hash, identity);
        }
    }

    fn remove(&self, hash: &TokenHash) {
        if let Ok(mut map) = self.by_hash.write() {
            map.remove(hash);
        }
    }

    pub fn len(&self) -> usize {
        self.by_hash.read().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Notes activity without touching the database. Bounded by device count.
    pub fn note_seen(&self, id: i64, ts: i64) {
        if let Ok(mut pending) = self.pending_seen.lock() {
            pending.insert(id, ts);
        }
    }

    /// Hands the accumulated activity to the caller for persistence.
    pub fn take_pending_seen(&self) -> Vec<(i64, i64)> {
        match self.pending_seen.lock() {
            Ok(mut pending) => pending.drain().collect(),
            Err(_) => Vec::new(),
        }
    }
}

fn row_to_device(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        id: row.get(0)?,
        name: row.get(1)?,
        platform: row.get(2)?,
        token_prefix: row.get(3)?,
        created_at: row.get(4)?,
        last_seen: row.get(5)?,
        revoked: row.get::<_, Option<i64>>(6)?.is_some(),
    })
}

/// Creates a device and returns the plaintext token — the only time it exists
/// outside the caller's hands. Everything stored is the digest.
pub fn create(
    conn: &Connection,
    cache: &DeviceCache,
    name: &str,
    platform: Option<&str>,
    now: i64,
) -> Result<DeviceCreated, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("device name is required".into()));
    }
    if name.chars().count() > 120 {
        return Err(AppError::BadRequest("device name is too long".into()));
    }

    let plaintext = token::generate(token::DEVICE_PREFIX);
    let hash = token::hash(&plaintext);
    let prefix = token::display_prefix(&plaintext);

    conn.execute(
        "INSERT INTO devices (name, platform, token_hash, token_prefix, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![name, platform, hash.as_slice(), prefix, now],
    )?;
    let id = conn.last_insert_rowid();

    cache.insert(
        hash,
        DeviceIdentity {
            id,
            name: name.to_string(),
        },
    );

    Ok(DeviceCreated {
        device: Device {
            id,
            name: name.to_string(),
            platform: platform.map(str::to_string),
            token_prefix: prefix,
            created_at: now,
            last_seen: None,
            revoked: false,
        },
        token: plaintext,
    })
}

pub fn list(conn: &Connection) -> Result<Vec<Device>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, platform, token_prefix, created_at, last_seen, revoked_at
         FROM devices ORDER BY id DESC",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_device(row)?);
    }
    Ok(out)
}

/// Revokes a device. The cache entry is dropped immediately, so the token stops
/// working on the very next request rather than at some later refresh.
pub fn revoke(conn: &Connection, cache: &DeviceCache, id: i64, now: i64) -> Result<(), AppError> {
    let hash: Option<Vec<u8>> = conn
        .query_row("SELECT token_hash FROM devices WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .ok();

    let changed = conn.execute(
        "UPDATE devices SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
        rusqlite::params![now, id],
    )?;
    if changed == 0 {
        return Err(AppError::BadRequest("no such active device".into()));
    }

    if let Some(blob) = hash {
        if let Ok(h) = TokenHash::try_from(blob.as_slice()) {
            cache.remove(&h);
        }
    }
    Ok(())
}

/// Records activity for devices seen since the last flush.
///
/// Called from the writer thread on the retention cadence rather than per
/// request: a `last_seen` write on every log line would add a database write
/// per ingest, which is exactly what the batching design avoids.
pub fn flush_last_seen(conn: &Connection, seen: &[(i64, i64)]) -> rusqlite::Result<()> {
    if seen.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare_cached("UPDATE devices SET last_seen = ?2 WHERE id = ?1")?;
    for (id, ts) in seen {
        stmt.execute([id, ts])?;
    }
    Ok(())
}
