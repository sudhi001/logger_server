//! Rule matching and firing.
//!
//! Evaluated on the ingest path, so it must stay cheap. Rules live in memory
//! and are reloaded when one changes; the level test comes first and rejects
//! almost every line before anything more expensive runs.
//!
//! Delivery never happens here — a fired rule is handed to a background task
//! through a bounded channel, so a slow webhook cannot slow down ingest.

use std::collections::VecDeque;
use std::sync::{Mutex, RwLock};

use tokio::sync::mpsc;

use crate::model::{AlertEvent, AlertRule, LogRecord};

/// Sliding-window state for one rule.
#[derive(Debug, Default)]
struct RuleState {
    /// Timestamps of recent matches, oldest first.
    hits: VecDeque<i64>,
    last_fired_at: i64,
}

#[derive(Debug)]
struct Compiled {
    rule: AlertRule,
    /// Pre-lowercased, so the hot path does not re-lowercase per log line.
    contains_lower: Option<String>,
    state: Mutex<RuleState>,
}

impl Compiled {
    /// Cheap tests first: level rejects the overwhelming majority of lines.
    fn matches(&self, log: &LogRecord) -> bool {
        if log.level < self.rule.min_level {
            return false;
        }
        if let Some(want) = self.rule.device_id {
            if log.device_id != Some(want) {
                return false;
            }
        }
        if let Some(tag) = &self.rule.name_filter {
            if log.name.trim() != tag.trim() {
                return false;
            }
        }
        if let Some(needle) = &self.contains_lower {
            if !log.message.to_lowercase().contains(needle) {
                return false;
            }
        }
        true
    }
}

pub struct AlertEngine {
    rules: RwLock<Vec<Compiled>>,
    tx: mpsc::Sender<AlertEvent>,
}

impl AlertEngine {
    pub fn new(tx: mpsc::Sender<AlertEvent>) -> Self {
        Self {
            rules: RwLock::new(Vec::new()),
            tx,
        }
    }

    /// Replaces the rule set. Window state is not carried across a reload:
    /// after an edit, the counts start again, which is the behaviour people
    /// expect from "I just changed the rule".
    pub fn reload(&self, rules: Vec<AlertRule>) {
        let compiled = rules
            .into_iter()
            .filter(|r| r.enabled)
            .map(|r| Compiled {
                contains_lower: r.contains.as_ref().map(|c| c.to_lowercase()),
                rule: r,
                state: Mutex::new(RuleState::default()),
            })
            .collect::<Vec<_>>();

        if let Ok(mut guard) = self.rules.write() {
            tracing::info!(active = compiled.len(), "alert rules loaded");
            *guard = compiled;
        }
    }

    /// A clone of the delivery channel, for the manual test endpoint.
    pub fn sender(&self) -> mpsc::Sender<AlertEvent> {
        self.tx.clone()
    }

    pub fn active_count(&self) -> usize {
        self.rules.read().map(|r| r.len()).unwrap_or(0)
    }

    /// Called for every ingested log. Returns immediately; delivery is someone
    /// else's job.
    pub fn observe(&self, log: &LogRecord) {
        let Ok(rules) = self.rules.read() else {
            return;
        };
        if rules.is_empty() {
            return;
        }

        for c in rules.iter() {
            if !c.matches(log) {
                continue;
            }
            if let Some(event) = self.tally(c, log) {
                // A full channel means the delivery task is behind. Dropping is
                // correct: alerts are a notification, and blocking ingest to
                // deliver one would be worse than missing it.
                if self.tx.try_send(event).is_err() {
                    tracing::warn!(rule = %c.rule.name, "alert dropped, delivery queue full");
                }
            }
        }
    }

    /// Records a match and decides whether it tips the rule over.
    fn tally(&self, c: &Compiled, log: &LogRecord) -> Option<AlertEvent> {
        let now = log.ts;
        let mut state = c.state.lock().ok()?;

        // Forget matches that have fallen out of the window.
        let cutoff = now - c.rule.window_secs * 1000;
        while state.hits.front().is_some_and(|t| *t < cutoff) {
            state.hits.pop_front();
        }
        state.hits.push_back(now);

        if (state.hits.len() as i64) < c.rule.threshold {
            return None;
        }
        if now - state.last_fired_at < c.rule.cooldown_secs * 1000 {
            return None;
        }

        state.last_fired_at = now;
        let count = state.hits.len() as i64;
        // Cleared so the next alert counts fresh matches rather than
        // re-firing on the same ones the moment the cooldown lapses.
        state.hits.clear();

        Some(AlertEvent {
            rule_id: c.rule.id,
            rule_name: c.rule.name.clone(),
            count,
            window_secs: c.rule.window_secs,
            fired_at: now,
            trigger: log.clone(),
        })
    }
}
