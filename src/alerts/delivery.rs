//! Webhook delivery.
//!
//! Runs as one background task draining a bounded channel, so nothing here can
//! slow ingest down. Each delivery is re-checked against the outbound guard
//! immediately before it is sent, because DNS can answer differently between
//! the rule being saved and the request being made.

use std::sync::Arc;
use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::sync::mpsc;

use crate::model::{level_label, AlertEvent, AlertFormat, AlertRule};
use crate::state::AppState;

const TIMEOUT: Duration = Duration::from_secs(10);
const ATTEMPTS: u32 = 3;

/// Builds the body for a target. Slack, Discord and PagerDuty each accept a
/// specific shape; anything else gets our own documented one.
pub fn payload(rule: &AlertRule, event: &AlertEvent, origin: &str) -> Value {
    let t = &event.trigger;
    let level = level_label(t.level);
    let summary = format!(
        "{} — {} {} in {}s",
        rule.name,
        event.count,
        if event.count == 1 { "match" } else { "matches" },
        event.window_secs
    );
    let device = t.device.clone().unwrap_or_else(|| "unknown device".into());
    let detail = format!("{}{}", t.name, t.message);

    match rule.format {
        AlertFormat::Slack => json!({
            "text": format!(":rotating_light: *{summary}*"),
            "blocks": [
                { "type": "section", "text": { "type": "mrkdwn",
                  "text": format!("*{summary}*\n`{level}` from *{device}*") } },
                { "type": "section", "text": { "type": "mrkdwn",
                  "text": format!("```{}```", truncate(&detail, 2800)) } },
                { "type": "context", "elements": [ { "type": "mrkdwn",
                  "text": format!("log #{} · <{origin}|open the dashboard>", t.id) } ] }
            ]
        }),

        AlertFormat::Discord => json!({
            "content": format!("**{summary}**"),
            "embeds": [{
                "title": format!("{level} · {device}"),
                "description": format!("```{}```", truncate(&detail, 3800)),
                "color": discord_colour(t.level),
                "footer": { "text": format!("log #{} · {origin}", t.id) }
            }]
        }),

        AlertFormat::Pagerduty => json!({
            // routing_key is the rule's secret for this format, which is how
            // PagerDuty's Events API identifies the integration.
            "event_action": "trigger",
            "dedup_key": format!("logger-rule-{}", rule.id),
            "payload": {
                "summary": truncate(&summary, 1024),
                "source": device,
                "severity": if t.level >= 4 { "error" } else { "warning" },
                "timestamp": rfc3339(t.ts),
                "custom_details": {
                    "message": truncate(&detail, 4000),
                    "log_id": t.id,
                    "matches": event.count,
                    "window_seconds": event.window_secs,
                    "context": t.context.clone(),
                }
            },
            "links": [ { "href": origin, "text": "Open the dashboard" } ]
        }),

        AlertFormat::Generic => json!({
            "rule": { "id": rule.id, "name": rule.name },
            "summary": summary,
            "count": event.count,
            "window_seconds": event.window_secs,
            "fired_at": event.fired_at,
            "origin": origin,
            "trigger": t,
        }),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn discord_colour(level: u8) -> u32 {
    match level {
        4 => 0xf8_51_49, // red
        3 => 0xe3_b3_41, // amber
        _ => 0x4c_9a_ff, // blue
    }
}

/// PagerDuty wants ISO 8601. Hand-rolled from epoch millis to avoid pulling in
/// a date library for one field.
fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's days-from-civil, inverted. Correct for any proleptic
/// Gregorian date, which is more than a log timestamp needs.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    format!("sha256={hex}")
}

/// Drains fired alerts and delivers them. Runs until the sender is dropped.
pub async fn run(state: Arc<AppState>, mut rx: mpsc::Receiver<AlertEvent>) {
    let client = match reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("logger_server/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "cannot build the webhook client; alerting is off");
            return;
        }
    };

    while let Some(event) = rx.recv().await {
        let Ok(Some(rule)) = state
            .store
            .with_admin(|conn, _| crate::store::alerts::get(conn, event.rule_id))
        else {
            continue;
        };
        let secret = state
            .store
            .with_admin(|conn, _| Ok(crate::store::alerts::secret_for(conn, rule.id)))
            .unwrap_or(None);

        let result = deliver(&client, &state, &rule, &event, secret.as_deref()).await;

        let err = result.err();
        if let Some(ref e) = err {
            tracing::warn!(rule = %rule.name, error = %e, "webhook delivery failed");
        } else {
            tracing::info!(rule = %rule.name, count = event.count, "alert delivered");
        }
        let _ = state.store.with_admin(|conn, _| {
            crate::store::alerts::record_result(conn, rule.id, event.fired_at, err.as_deref());
            Ok(())
        });
    }
}

async fn deliver(
    client: &reqwest::Client,
    state: &Arc<AppState>,
    rule: &AlertRule,
    event: &AlertEvent,
    secret: Option<&str>,
) -> Result<(), String> {
    // Re-checked here, not only at save time: the host may resolve differently
    // now than it did then.
    guard_url(&rule.url, state.cfg.webhook_allow_private).await?;

    let body = payload(rule, event, &state.cfg.public_url);
    let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;

    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        let mut req = client
            .post(&rule.url)
            .header("content-type", "application/json")
            .header("x-logger-rule", rule.id.to_string());

        // PagerDuty carries its routing key in the body; everyone else gets an
        // HMAC of the body they can verify.
        if let Some(s) = secret {
            if rule.format == AlertFormat::Pagerduty {
                let mut with_key: Value = body.clone();
                with_key["routing_key"] = json!(s);
                let rekeyed = serde_json::to_vec(&with_key).map_err(|e| e.to_string())?;
                req = req.body(rekeyed);
            } else {
                req = req.header("x-logger-signature", sign(s, &bytes));
                req = req.body(bytes.clone());
            }
        } else {
            req = req.body(bytes.clone());
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) => {
                let status = resp.status();
                last = format!("HTTP {status}");
                // 4xx other than rate limiting will fail identically next time.
                if status.is_client_error() && status.as_u16() != 429 {
                    return Err(last);
                }
            }
            Err(e) => last = e.to_string(),
        }

        if attempt < ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(300 * 2u64.pow(attempt))).await;
        }
    }
    Err(format!("giving up after {ATTEMPTS} attempts: {last}"))
}

async fn guard_url(url: &str, allow_private: bool) -> Result<(), String> {
    // Resolution blocks, so it runs on the blocking pool rather than a worker.
    let (url, allow) = (url.to_string(), allow_private);
    tokio::task::spawn_blocking(move || super::guard::check(&url, allow))
        .await
        .map_err(|e| format!("url check failed: {e}"))?
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_matches_known_timestamps() {
        // Hand-rolled calendar maths is exactly the kind of thing that is wrong
        // at leap years and century boundaries, so pin real values.
        for (ms, want) in [
            (0_i64, "1970-01-01T00:00:00.000Z"),
            (1_000, "1970-01-01T00:00:01.000Z"),
            (951_782_400_000, "2000-02-29T00:00:00.000Z"), // leap day, century leap year
            (1_078_012_800_000, "2004-02-29T00:00:00.000Z"), // ordinary leap year
            (1_709_164_800_000, "2024-02-29T00:00:00.000Z"),
            (1_788_075_947_856, "2026-08-30T07:45:47.856Z"),
            (4_102_444_800_000, "2100-01-01T00:00:00.000Z"), // 2100 is NOT a leap year
            (1_735_689_599_999, "2024-12-31T23:59:59.999Z"),
        ] {
            assert_eq!(rfc3339(ms), want, "for {ms}");
        }
    }

    #[test]
    fn signing_is_stable_and_key_sensitive() {
        let body = br#"{"a":1}"#;
        assert_eq!(
            sign("k", body),
            sign("k", body),
            "same input, same signature"
        );
        assert_ne!(
            sign("k", body),
            sign("k2", body),
            "a different key must differ"
        );
        assert_ne!(
            sign("k", body),
            sign("k", br#"{"a":2}"#),
            "a different body must differ"
        );
        assert!(sign("k", body).starts_with("sha256="));
    }

    #[test]
    fn truncate_respects_character_boundaries() {
        // Slicing a multi-byte string by bytes would panic mid-codepoint.
        let s = "é".repeat(50);
        assert_eq!(truncate(&s, 10).chars().count(), 11); // 10 plus the ellipsis
        assert_eq!(truncate("short", 100), "short");
    }
}
