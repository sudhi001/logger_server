//! Browser sessions for the dashboard.
//!
//! A session is an opaque random id held in an HttpOnly cookie, mapped to an
//! expiry in memory. Nothing is signed or stored on disk: sessions are cheap to
//! mint and a restart simply logs everyone out, which for a dashboard is an
//! acceptable trade for having no key management at all.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use crate::auth::token;

pub const COOKIE_NAME: &str = "logger_session";
/// Refuses to grow without bound if something mints sessions in a loop.
const MAX_SESSIONS: usize = 10_000;

pub struct Sessions {
    inner: RwLock<HashMap<String, Instant>>,
    ttl: Duration,
}

impl Sessions {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    pub fn create(&self) -> Option<String> {
        let id = token::generate("lgrs_");
        let mut map = self.inner.write().ok()?;
        if map.len() >= MAX_SESSIONS {
            // Cheap defence: drop anything already expired before giving up.
            let now = Instant::now();
            map.retain(|_, exp| *exp > now);
            if map.len() >= MAX_SESSIONS {
                tracing::warn!("session table full; refusing to mint another");
                return None;
            }
        }
        map.insert(id.clone(), Instant::now() + self.ttl);
        Some(id)
    }

    pub fn is_valid(&self, id: &str) -> bool {
        let Ok(map) = self.inner.read() else {
            return false;
        };
        map.get(id).is_some_and(|exp| *exp > Instant::now())
    }

    pub fn revoke(&self, id: &str) {
        if let Ok(mut map) = self.inner.write() {
            map.remove(id);
        }
    }

    pub fn purge_expired(&self) {
        if let Ok(mut map) = self.inner.write() {
            let now = Instant::now();
            map.retain(|_, exp| *exp > now);
        }
    }

    pub fn len(&self) -> usize {
        self.inner.read().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Pulls the session id out of a `Cookie` header.
pub fn from_cookies(headers: &axum::http::HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(value) = part
            .strip_prefix(COOKIE_NAME)
            .and_then(|r| r.strip_prefix('='))
        {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}
