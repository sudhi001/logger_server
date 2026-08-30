//! Alerting: rule matching, the threshold/window/cooldown machinery, and the
//! outbound guard as it is actually reached through the API.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use logger_server::config::Config;
use logger_server::model::AlertEvent;
use logger_server::{build_state, routes};
use serde_json::Value;
use tokio::sync::mpsc::Receiver;
use tower::ServiceExt;

const ADMIN: &str = "lgra_test_admin_token";

struct Harness {
    app: axum::Router,
    writer: logger_server::store::WriterHandle,
    device: String,
    /// Fired alerts land here instead of being delivered, so the tests assert
    /// on what *would* be sent without needing a webhook receiver.
    alerts: Receiver<AlertEvent>,
}

async fn app(mutate: impl FnOnce(&mut Config)) -> Harness {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let mut cfg = Config::from_env();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    cfg.db_path = std::env::temp_dir()
        .join(format!("logger_alerts_{unique}_{seq}.db"))
        .to_string_lossy()
        .into_owned();
    cfg.admin_token = ADMIN.to_string();
    cfg.rate_limit_rps = 0;
    cfg.webhook_allow_private = true;
    mutate(&mut cfg);

    let (state, writer, shutdown, alerts) = build_state(cfg).expect("state");
    std::mem::forget(shutdown);
    let app = routes::build(state);

    let resp = app
        .clone()
        .oneshot(admin(post("/api/v1/devices", r#"{"name":"alert-device"}"#)))
        .await
        .unwrap();
    let created: Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let device = created["token"].as_str().unwrap().to_string();

    Harness {
        app,
        writer,
        device,
        alerts,
    }
}

async fn body_string(resp: axum::response::Response) -> String {
    String::from_utf8(
        resp.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

fn post(uri: &str, json: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(json.to_owned()))
        .unwrap()
}

fn admin(mut req: Request<Body>) -> Request<Body> {
    req.headers_mut()
        .insert("x-admin-token", ADMIN.parse().unwrap());
    req
}

impl Harness {
    async fn create_rule(&self, json: &str) -> (StatusCode, String) {
        let resp = self
            .app
            .clone()
            .oneshot(admin(post("/api/v1/alerts", json)))
            .await
            .unwrap();
        let status = resp.status();
        (status, body_string(resp).await)
    }

    async fn log(&self, json: &str) {
        let mut req = post("/api/v1/logs", json);
        req.headers_mut()
            .insert("x-device-token", self.device.parse().unwrap());
        self.app.clone().oneshot(req).await.unwrap();
    }

    /// Fired alerts, without waiting on a timer.
    fn drain(&mut self) -> Vec<AlertEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.alerts.try_recv() {
            out.push(e);
        }
        out
    }
}

#[tokio::test]
async fn a_rule_fires_only_once_the_threshold_is_met() {
    let mut h = app(|_| {}).await;
    let (status, _) = h
        .create_rule(
            r#"{"name":"errors","url":"https://example.com/hook","min_level":4,
                "threshold":3,"window_secs":60,"cooldown_secs":0}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    h.log(r#"{"name":"[a] ","message":"boom 1","level":4}"#)
        .await;
    h.log(r#"{"name":"[a] ","message":"boom 2","level":4}"#)
        .await;
    assert!(
        h.drain().is_empty(),
        "two matches must not reach a threshold of three"
    );

    h.log(r#"{"name":"[a] ","message":"boom 3","level":4}"#)
        .await;
    let fired = h.drain();
    assert_eq!(fired.len(), 1, "the third match fires it");
    assert_eq!(fired[0].count, 3);
    assert_eq!(fired[0].trigger.message, "boom 3");
    h.writer.shutdown();
}

#[tokio::test]
async fn a_crash_loop_produces_one_alert_not_a_thousand() {
    // The whole reason cooldown exists.
    let mut h = app(|_| {}).await;
    h.create_rule(
        r#"{"name":"errors","url":"https://example.com/hook","min_level":4,
            "threshold":1,"window_secs":60,"cooldown_secs":3600}"#,
    )
    .await;

    for i in 0..200 {
        h.log(&format!(
            r#"{{"name":"[a] ","message":"crash {i}","level":4}}"#
        ))
        .await;
    }
    assert_eq!(
        h.drain().len(),
        1,
        "200 errors inside the cooldown is still one alert"
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn matching_narrows_by_level_tag_and_text() {
    let mut h = app(|_| {}).await;
    h.create_rule(
        r#"{"name":"net errors","url":"https://example.com/hook","min_level":4,
            "name_filter":"[Net]","contains":"timeout","threshold":1,"cooldown_secs":0}"#,
    )
    .await;

    // Each of these misses on exactly one criterion.
    h.log(r#"{"name":"[Net] ","message":"connection timeout","level":3}"#)
        .await; // level
    h.log(r#"{"name":"[Db] ","message":"connection timeout","level":4}"#)
        .await; // tag
    h.log(r#"{"name":"[Net] ","message":"connection refused","level":4}"#)
        .await; // text
    assert!(
        h.drain().is_empty(),
        "a rule must not fire on a partial match"
    );

    h.log(r#"{"name":"[Net] ","message":"connection TIMEOUT after 30s","level":4}"#)
        .await;
    let fired = h.drain();
    assert_eq!(fired.len(), 1, "text matching is case-insensitive");
    h.writer.shutdown();
}

#[tokio::test]
async fn a_disabled_rule_does_not_fire() {
    let mut h = app(|_| {}).await;
    let (_, body) = h
        .create_rule(
            r#"{"name":"errors","url":"https://example.com/hook","min_level":4,
                "threshold":1,"cooldown_secs":0}"#,
        )
        .await;
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();

    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/api/v1/alerts/{id}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"enabled":false}"#))
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(admin(req)).await.unwrap().status(),
        StatusCode::NO_CONTENT
    );

    h.log(r#"{"name":"[a] ","message":"boom","level":4}"#).await;
    assert!(h.drain().is_empty(), "a disabled rule is inert");
    h.writer.shutdown();
}

#[tokio::test]
async fn the_outbound_guard_is_enforced_at_the_api() {
    // The guard has unit tests; this asserts it is actually wired to rule creation.
    let h = app(|c| c.webhook_allow_private = false).await;

    for url in [
        "http://169.254.169.254/latest/meta-data/",
        "http://127.0.0.1:6379/",
        "http://10.0.0.5/",
        "file:///etc/passwd",
    ] {
        let (status, body) = h
            .create_rule(&format!(r#"{{"name":"probe","url":"{url}"}}"#))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url} must be refused");
        assert!(
            !body.contains("\"id\""),
            "no rule should be stored for {url}"
        );
    }

    let (status, _) = h
        .create_rule(r#"{"name":"ok","url":"https://hooks.slack.com/services/T/B/X"}"#)
        .await;
    assert_eq!(status, StatusCode::CREATED, "a public https URL is fine");
    h.writer.shutdown();
}

#[tokio::test]
async fn an_unknown_format_is_rejected_with_the_valid_options() {
    let h = app(|_| {}).await;
    let (status, body) = h
        .create_rule(r#"{"name":"x","url":"https://example.com/h","format":"telegram"}"#)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.contains("slack"),
        "the error should list what is valid: {body}"
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn the_signing_secret_is_never_returned() {
    let h = app(|_| {}).await;
    let (_, body) = h
        .create_rule(r#"{"name":"x","url":"https://example.com/h","secret":"super-secret-value"}"#)
        .await;
    assert!(
        !body.contains("super-secret-value"),
        "create must not echo the secret"
    );
    assert!(
        body.contains("\"signed\":true"),
        "but it should say one is set"
    );

    let resp = h
        .app
        .clone()
        .oneshot(admin(
            Request::builder()
                .uri("/api/v1/alerts")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    let listed = body_string(resp).await;
    assert!(
        !listed.contains("super-secret-value"),
        "nor must the listing"
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn the_test_endpoint_fires_regardless_of_threshold() {
    let mut h = app(|_| {}).await;
    let (_, body) = h
        .create_rule(
            r#"{"name":"rare","url":"https://example.com/hook","threshold":1000,
                "window_secs":60,"cooldown_secs":3600}"#,
        )
        .await;
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = h
        .app
        .clone()
        .oneshot(admin(post(&format!("/api/v1/alerts/{id}/test"), "")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let fired = h.drain();
    assert_eq!(fired.len(), 1, "a test bypasses a threshold of 1000");
    assert!(fired[0].trigger.message.contains("Test alert"));
    h.writer.shutdown();
}

#[tokio::test]
async fn rules_are_visible_to_an_agent_but_not_creatable_by_one() {
    // Creating a rule hands the server a URL it will POST log contents to, so
    // it is deliberately outside the MCP surface at every access level.
    let h = app(|_| {}).await;
    h.create_rule(r#"{"name":"visible","url":"https://example.com/hook"}"#)
        .await;

    let resp = h
        .app
        .clone()
        .oneshot(admin(post(
            "/mcp",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        )))
        .await
        .unwrap();
    let v: Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();

    assert!(names.contains(&"list_alerts"), "an agent can read them");
    assert!(
        !names.iter().any(|n| n.contains("create_alert")),
        "but not create them"
    );
    assert!(!names.iter().any(|n| n.contains("delete_alert")));
    h.writer.shutdown();
}
