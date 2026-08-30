//! Router-level tests. These drive the real router through `tower::ServiceExt`,
//! so no socket or port is involved.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use logger_server::config::Config;
use logger_server::{build_state, routes};
use tower::ServiceExt;

const ADMIN: &str = "lgra_test_admin_token";

struct Harness {
    app: axum::Router,
    writer: logger_server::store::WriterHandle,
    /// Token for a device registered during setup.
    device: String,
}

/// Builds an isolated app with its own temporary database and one device.
async fn app(mutate: impl FnOnce(&mut Config)) -> Harness {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let mut cfg = Config::from_env();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    cfg.db_path = std::env::temp_dir()
        .join(format!("logger_test_{unique}_{seq}.db"))
        .to_string_lossy()
        .into_owned();
    cfg.admin_token = ADMIN.to_string();
    cfg.rate_limit_rps = 0;
    mutate(&mut cfg);

    let (state, writer, shutdown) = build_state(cfg).expect("state");
    // Leaking the shutdown sender keeps the watch channel open for the test.
    std::mem::forget(shutdown);
    let app = routes::build(state);

    // Register a device through the real admin API, so the tests exercise the
    // same path an operator would.
    let resp = app
        .clone()
        .oneshot(admin(post_raw(
            "/api/v1/devices",
            r#"{"name":"test-device","platform":"test"}"#,
        )))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "device setup failed");
    let created: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let device = created["token"].as_str().unwrap().to_string();

    Harness {
        app,
        writer,
        device,
    }
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn post_raw(uri: &str, json: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(json.to_owned()))
        .unwrap()
}

fn get_raw(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

/// Adds admin credentials, standing in for a dashboard session.
fn admin(mut req: Request<Body>) -> Request<Body> {
    req.headers_mut()
        .insert("x-admin-token", ADMIN.parse().unwrap());
    req
}

fn with_device(mut req: Request<Body>, token: &str) -> Request<Body> {
    req.headers_mut()
        .insert("x-device-token", token.parse().unwrap());
    req
}

impl Harness {
    fn post(&self, uri: &str, json: &str) -> Request<Body> {
        with_device(post_raw(uri, json), &self.device)
    }
    fn get(&self, uri: &str) -> Request<Body> {
        admin(get_raw(uri))
    }
}

/// Gives the writer thread time to commit a batch.
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

#[tokio::test]
async fn ingest_then_read_back() {
    let h = app(|_| {}).await;

    let resp = h
        .app
        .clone()
        .oneshot(h.post("/api/v1/logs", r#"{"name":"svc","message":"hello"}"#))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(body_string(resp).await.contains("\"id\":1"));

    settle().await;

    let resp = h
        .app
        .clone()
        .oneshot(h.get("/api/v1/logs/recent"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("hello"), "got {body}");
    // The device is resolved from the token and attached on read.
    assert!(
        body.contains("test-device"),
        "device name should be joined in: {body}"
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn writes_require_a_device_token() {
    let h = app(|_| {}).await;

    // No token at all.
    let resp = h
        .app
        .clone()
        .oneshot(post_raw("/api/v1/logs", r#"{"name":"n","message":"m"}"#))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // A well-formed but unknown token.
    let resp = h
        .app
        .clone()
        .oneshot(with_device(
            post_raw("/api/v1/logs", r#"{"name":"n","message":"m"}"#),
            "lgrd_notarealtokenatallxxxxxxxxxxxxxxx",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // The legacy path is authenticated too.
    let resp = h
        .app
        .clone()
        .oneshot(post_raw("/logs", r#"{"name":"n","message":"m"}"#))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // The registered device works, via Authorization: Bearer as well.
    let mut req = post_raw("/api/v1/logs", r#"{"name":"n","message":"m"}"#);
    req.headers_mut().insert(
        "authorization",
        format!("Bearer {}", h.device).parse().unwrap(),
    );
    assert_eq!(
        h.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn reads_require_a_viewer_credential() {
    let h = app(|_| {}).await;

    for uri in [
        "/api/v1/logs/recent",
        "/api/v1/logs/export",
        "/api/v1/logs/stream",
        "/api/v1/devices",
        "/logs",
        "/logs/recent",
        "/metrics",
    ] {
        let resp = h.app.clone().oneshot(get_raw(uri)).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} must not be readable without a credential"
        );
    }

    // Health stays public so the platform can probe it.
    let resp = h.app.clone().oneshot(get_raw("/healthz")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // The login page must render before a session exists.
    let resp = h.app.clone().oneshot(get_raw("/login.html")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    h.writer.shutdown();
}

#[tokio::test]
async fn login_issues_a_session_cookie_that_grants_reads() {
    let h = app(|_| {}).await;

    let bad = h
        .app
        .clone()
        .oneshot(post_raw("/api/v1/auth/login", r#"{"token":"wrong"}"#))
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

    let ok = h
        .app
        .clone()
        .oneshot(post_raw(
            "/api/v1/auth/login",
            &serde_json::json!({ "token": ADMIN }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);

    let cookie = ok
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .expect("session cookie")
        .to_string();
    assert!(
        cookie.contains("HttpOnly"),
        "cookie must be HttpOnly: {cookie}"
    );
    assert!(
        cookie.contains("SameSite=Strict"),
        "cookie must be SameSite=Strict"
    );

    let session = cookie.split(';').next().unwrap().to_string();
    let mut req = get_raw("/api/v1/logs/recent");
    req.headers_mut().insert("cookie", session.parse().unwrap());
    assert_eq!(
        h.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn a_revoked_device_stops_working_immediately() {
    let h = app(|_| {}).await;

    let list = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/devices"))
            .await
            .unwrap(),
    )
    .await;
    let devices: Vec<serde_json::Value> = serde_json::from_str(&list).unwrap();
    let id = devices[0]["id"].as_i64().unwrap();

    // Works before revocation.
    assert_eq!(
        h.app
            .clone()
            .oneshot(h.post("/api/v1/logs", r#"{"name":"n","message":"before"}"#))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );

    let resp = h
        .app
        .clone()
        .oneshot(admin(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/devices/{id}"))
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // The in-memory cache is updated synchronously, so the very next request fails.
    assert_eq!(
        h.app
            .clone()
            .oneshot(h.post("/api/v1/logs", r#"{"name":"n","message":"after"}"#))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn a_device_cannot_forge_another_devices_attribution() {
    let h = app(|_| {}).await;

    // device_id in the body must be ignored; attribution comes from the token.
    let resp = h
        .app
        .clone()
        .oneshot(h.post(
            "/api/v1/logs",
            r#"{"name":"n","message":"m","device_id":9999}"#,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success());
    settle().await;

    let body = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/logs/recent"))
            .await
            .unwrap(),
    )
    .await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(rows[0]["device_id"].as_i64().unwrap(), 1);
    assert_eq!(rows[0]["device"].as_str().unwrap(), "test-device");
    h.writer.shutdown();
}

#[tokio::test]
async fn tokens_are_never_stored_or_listed_in_plaintext() {
    let h = app(|_| {}).await;
    let list = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/devices"))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !list.contains(&h.device),
        "listing must not echo the token back"
    );
    // Only a short recognisable prefix is exposed.
    assert!(list.contains(&h.device[..12]));
    h.writer.shutdown();
}

#[tokio::test]
async fn sync_ingest_is_durable_before_responding() {
    let h = app(|_| {}).await;

    let resp = h
        .app
        .clone()
        .oneshot(h.post(
            "/api/v1/logs?sync=true",
            r#"{"name":"a","message":"durable"}"#,
        ))
        .await
        .unwrap();
    // 201, not 202: the row is committed by the time this returns.
    assert_eq!(resp.status(), StatusCode::CREATED);

    // No settle(): the read must already see it.
    let resp = h
        .app
        .clone()
        .oneshot(h.get("/api/v1/logs/recent"))
        .await
        .unwrap();
    assert!(body_string(resp).await.contains("durable"));
    h.writer.shutdown();
}

#[tokio::test]
async fn message_is_truncated_on_a_character_boundary() {
    // A multi-byte character straddling the limit would panic a byte-wise slice.
    let h = app(|c| c.max_message_len = 10).await;

    let msg = "é".repeat(50);
    let payload = serde_json::json!({ "name": "n", "message": msg }).to_string();
    let resp = h
        .app
        .clone()
        .oneshot(h.post("/api/v1/logs", &payload))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    settle().await;
    let body = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/logs/recent"))
            .await
            .unwrap(),
    )
    .await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed[0]["message"].as_str().unwrap().chars().count(), 10);
    h.writer.shutdown();
}

#[tokio::test]
async fn level_is_clamped_to_the_known_range() {
    let h = app(|_| {}).await;
    h.app
        .clone()
        .oneshot(h.post("/api/v1/logs", r#"{"name":"n","message":"m","level":200}"#))
        .await
        .unwrap();
    settle().await;

    let body = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/logs/recent"))
            .await
            .unwrap(),
    )
    .await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(rows[0]["level"].as_u64().unwrap(), 4, "clamped to error");
    h.writer.shutdown();
}

#[tokio::test]
async fn recent_filters_by_minimum_level() {
    let h = app(|_| {}).await;
    for level in 0..5 {
        let payload = format!(r#"{{"name":"n","message":"lvl{level}","level":{level}}}"#);
        h.app
            .clone()
            .oneshot(h.post("/api/v1/logs", &payload))
            .await
            .unwrap();
    }
    settle().await;

    let body = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/logs/recent?min_level=3"))
            .await
            .unwrap(),
    )
    .await;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(rows.len(), 2, "warn and error only");
    assert!(rows.iter().all(|r| r["level"].as_u64().unwrap() >= 3));
    h.writer.shutdown();
}

#[tokio::test]
async fn recent_limit_is_clamped() {
    let h = app(|_| {}).await;
    for i in 0..20 {
        let payload = format!(r#"{{"name":"n","message":"m{i}"}}"#);
        h.app
            .clone()
            .oneshot(h.post("/api/v1/logs", &payload))
            .await
            .unwrap();
    }
    settle().await;

    let body = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/logs/recent?limit=5"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Vec<serde_json::Value>>(&body)
            .unwrap()
            .len(),
        5
    );

    // Absurd limits clamp rather than being honoured.
    let body = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/logs/recent?limit=99999999"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Vec<serde_json::Value>>(&body)
            .unwrap()
            .len(),
        20
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn cursor_pagination_walks_backwards_without_gaps() {
    let h = app(|_| {}).await;
    for i in 0..10 {
        let payload = format!(r#"{{"name":"n","message":"m{i}"}}"#);
        h.app
            .clone()
            .oneshot(h.post("/api/v1/logs", &payload))
            .await
            .unwrap();
    }
    settle().await;

    let mut seen = Vec::new();
    let mut cursor: Option<i64> = None;
    loop {
        let uri = match cursor {
            Some(c) => format!("/api/v1/logs/recent?limit=3&before_id={c}"),
            None => "/api/v1/logs/recent?limit=3".to_string(),
        };
        let body = body_string(h.app.clone().oneshot(h.get(&uri)).await.unwrap()).await;
        let page: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
        if page.is_empty() {
            break;
        }
        for row in &page {
            seen.push(row["id"].as_i64().unwrap());
        }
        cursor = Some(*seen.last().unwrap());
    }

    assert_eq!(seen.len(), 10, "every row visited exactly once");
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 10, "no duplicates across pages");
    h.writer.shutdown();
}

#[tokio::test]
async fn legacy_routes_match_their_v1_equivalents() {
    let h = app(|_| {}).await;

    // The exact body the original Kotlin controller accepted.
    let resp = h
        .app
        .clone()
        .oneshot(h.post("/logs", r#"{"name":"[tag] ","message":"legacy"}"#))
        .await
        .unwrap();
    assert!(resp.status().is_success());
    settle().await;

    let legacy = body_string(h.app.clone().oneshot(h.get("/logs/recent")).await.unwrap()).await;
    let v1 = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/logs/recent"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(legacy, v1);

    // GET /logs still yields a JSON array, just streamed.
    let export = body_string(h.app.clone().oneshot(h.get("/logs")).await.unwrap()).await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&export).unwrap();
    assert_eq!(parsed.len(), 1);

    // The wildcard name route must not shadow /logs/recent.
    let by_name = body_string(
        h.app
            .clone()
            .oneshot(h.get("/logs/%5Btag%5D%20"))
            .await
            .unwrap(),
    )
    .await;
    assert!(by_name.contains("legacy"), "got {by_name}");
    h.writer.shutdown();
}

#[tokio::test]
async fn export_streams_ndjson_when_asked() {
    let h = app(|_| {}).await;
    for i in 0..5 {
        let payload = format!(r#"{{"name":"n","message":"m{i}"}}"#);
        h.app
            .clone()
            .oneshot(h.post("/api/v1/logs", &payload))
            .await
            .unwrap();
    }
    settle().await;

    let body = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/logs/export?format=ndjson"))
            .await
            .unwrap(),
    )
    .await;
    let lines: Vec<_> = body.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 5);
    for line in lines {
        serde_json::from_str::<serde_json::Value>(line).expect("each line is valid JSON");
    }
    h.writer.shutdown();
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    let h = app(|c| c.max_body_bytes = 1024).await;
    let huge = serde_json::json!({ "name": "n", "message": "x".repeat(64 * 1024) }).to_string();
    let resp = h
        .app
        .clone()
        .oneshot(h.post("/api/v1/logs", &huge))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    h.writer.shutdown();
}

#[tokio::test]
async fn batch_ingest_accepts_all_rows() {
    let h = app(|_| {}).await;
    let payload = serde_json::json!([
        {"name": "a", "message": "1"},
        {"name": "a", "message": "2"},
        {"name": "a", "message": "3"},
    ])
    .to_string();

    let resp = h
        .app
        .clone()
        .oneshot(h.post("/api/v1/logs/batch", &payload))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(body_string(resp).await.contains("\"accepted\":3"));

    settle().await;
    let body = body_string(
        h.app
            .clone()
            .oneshot(h.get("/api/v1/logs/recent"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Vec<serde_json::Value>>(&body)
            .unwrap()
            .len(),
        3
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn a_batch_that_overflows_the_queue_reports_every_dropped_row() {
    // Regression: the handler used to break out of the loop on the first
    // rejected record and count only that one, so `logger_shed_total` silently
    // under-reported by the whole remainder of the batch.
    let h = app(|c| c.ingest_queue = 64).await;

    let rows: Vec<_> = (0..4000)
        .map(|i| serde_json::json!({ "name": "n", "message": format!("m{i}") }))
        .collect();
    let payload = serde_json::Value::Array(rows).to_string();

    let resp = h
        .app
        .clone()
        .oneshot(h.post("/api/v1/logs/batch", &payload))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let ack: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let accepted = ack["accepted"].as_u64().unwrap();
    let dropped = ack["dropped"].as_u64().unwrap();

    assert!(dropped > 0, "a 64-slot queue cannot absorb 4000 rows");
    assert_eq!(
        accepted + dropped,
        4000,
        "every row is either accepted or accounted for as dropped"
    );

    // The metric must agree with what the client was told.
    let metrics = body_string(h.app.clone().oneshot(h.get("/metrics")).await.unwrap()).await;
    let shed: u64 = metrics
        .lines()
        .find(|l| l.starts_with("logger_shed_total "))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
        .unwrap();
    assert_eq!(
        shed, dropped,
        "logger_shed_total must match the reported drop count"
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn static_assets_are_served_from_the_binary() {
    let h = app(|_| {}).await;
    for (path, needle) in [
        ("/", "Logs"),
        ("/app.js", "EventSource"),
        ("/app.css", "--bg"),
        ("/devices.html", "Register a device"),
        ("/login.html", "LOGGER_ADMIN_TOKEN"),
    ] {
        let resp = h.app.clone().oneshot(get_raw(path)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        assert!(body_string(resp).await.contains(needle), "{path}");
    }
    h.writer.shutdown();
}
