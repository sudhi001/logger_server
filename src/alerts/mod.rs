//! Alerting: match log lines against rules, and deliver webhooks when one fires.

pub mod delivery;
pub mod engine;
pub mod guard;
