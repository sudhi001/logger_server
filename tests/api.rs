//! Router-level tests. These drive the real router through `tower::ServiceExt`,
//! so no socket or port is involved.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use logger_server::config::Config;
use logger_server::{build_state, routes};
use tower::ServiceExt;

/// Builds an isolated app with its own temporary database.
fn app(mutate: impl FnOnce(&mut Config)) -> (axum::Router, logger_server::store::WriterHandle) {
    let mut cfg = Config::from_env();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    cfg.db_path = std::env::temp_dir()
        .join(format!(
            "logger_test_{unique}_{:?}.db",
            std::thread::current().id()
        ))
        .to_string_lossy()
        .into_owned();
    cfg.api_key = None;
    cfg.rate_limit_rps = 0;
    mutate(&mut cfg);

    let (state, writer, _shutdown) = build_state(cfg).expect("state");
    // Leaking the shutdown sender keeps the watch channel open for the test.
    std::mem::forget(_shutdown);
    (routes::build(state), writer)
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn post(uri: &str, json: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(json.to_owned()))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

/// Gives the writer thread time to commit a batch.
async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
}

#[tokio::test]
async fn ingest_then_read_back() {
    let (app, writer) = app(|_| {});

    let resp = app
        .clone()
        .oneshot(post("/api/v1/logs", r#"{"name":"svc","message":"hello"}"#))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(body_string(resp).await.contains("\"id\":1"));

    settle().await;

    let resp = app.oneshot(get("/api/v1/logs/recent")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("hello"), "got {body}");
    writer.shutdown();
}

#[tokio::test]
async fn sync_ingest_is_durable_before_responding() {
    let (app, writer) = app(|_| {});

    let resp = app
        .clone()
        .oneshot(post(
            "/api/v1/logs?sync=true",
            r#"{"name":"a","message":"durable"}"#,
        ))
        .await
        .unwrap();
    // 201, not 202: the row is committed by the time this returns.
    assert_eq!(resp.status(), StatusCode::CREATED);

    // No settle(): the read must already see it.
    let resp = app.oneshot(get("/api/v1/logs/recent")).await.unwrap();
    assert!(body_string(resp).await.contains("durable"));
    writer.shutdown();
}

#[tokio::test]
async fn message_is_truncated_on_a_character_boundary() {
    // A multi-byte character straddling the limit would panic a byte-wise slice.
    let (app, writer) = app(|c| c.max_message_len = 10);

    let msg = "é".repeat(50);
    let payload = serde_json::json!({ "name": "n", "message": msg }).to_string();
    let resp = app
        .clone()
        .oneshot(post("/api/v1/logs", &payload))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    settle().await;
    let body = body_string(app.oneshot(get("/api/v1/logs/recent")).await.unwrap()).await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed[0]["message"].as_str().unwrap().chars().count(), 10);
    writer.shutdown();
}

#[tokio::test]
async fn recent_limit_is_clamped() {
    let (app, writer) = app(|_| {});
    for i in 0..20 {
        let payload = format!(r#"{{"name":"n","message":"m{i}"}}"#);
        app.clone()
            .oneshot(post("/api/v1/logs", &payload))
            .await
            .unwrap();
    }
    settle().await;

    let body = body_string(
        app.clone()
            .oneshot(get("/api/v1/logs/recent?limit=5"))
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
        app.oneshot(get("/api/v1/logs/recent?limit=99999999"))
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
    writer.shutdown();
}

#[tokio::test]
async fn cursor_pagination_walks_backwards_without_gaps() {
    let (app, writer) = app(|_| {});
    for i in 0..10 {
        let payload = format!(r#"{{"name":"n","message":"m{i}"}}"#);
        app.clone()
            .oneshot(post("/api/v1/logs", &payload))
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
        let body = body_string(app.clone().oneshot(get(&uri)).await.unwrap()).await;
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
    writer.shutdown();
}

#[tokio::test]
async fn legacy_routes_match_their_v1_equivalents() {
    let (app, writer) = app(|_| {});

    // The exact body the original Kotlin controller accepted.
    let resp = app
        .clone()
        .oneshot(post("/logs", r#"{"name":"[tag] ","message":"legacy"}"#))
        .await
        .unwrap();
    assert!(resp.status().is_success());
    settle().await;

    let legacy = body_string(app.clone().oneshot(get("/logs/recent")).await.unwrap()).await;
    let v1 = body_string(
        app.clone()
            .oneshot(get("/api/v1/logs/recent"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(legacy, v1);

    // GET /logs still yields a JSON array, just streamed.
    let export = body_string(app.clone().oneshot(get("/logs")).await.unwrap()).await;
    let parsed: Vec<serde_json::Value> = serde_json::from_str(&export).unwrap();
    assert_eq!(parsed.len(), 1);

    // The wildcard name route must not shadow /logs/recent.
    let by_name = body_string(app.oneshot(get("/logs/%5Btag%5D%20")).await.unwrap()).await;
    assert!(by_name.contains("legacy"), "got {by_name}");
    writer.shutdown();
}

#[tokio::test]
async fn export_streams_ndjson_when_asked() {
    let (app, writer) = app(|_| {});
    for i in 0..5 {
        let payload = format!(r#"{{"name":"n","message":"m{i}"}}"#);
        app.clone()
            .oneshot(post("/api/v1/logs", &payload))
            .await
            .unwrap();
    }
    settle().await;

    let body = body_string(
        app.oneshot(get("/api/v1/logs/export?format=ndjson"))
            .await
            .unwrap(),
    )
    .await;
    let lines: Vec<_> = body.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 5);
    for line in lines {
        serde_json::from_str::<serde_json::Value>(line).expect("each line is valid JSON");
    }
    writer.shutdown();
}

#[tokio::test]
async fn api_key_is_enforced_only_when_configured() {
    let (app, writer) = app(|c| c.api_key = Some("s3cret".into()));

    let resp = app
        .clone()
        .oneshot(post("/api/v1/logs", r#"{"name":"n","message":"m"}"#))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let mut req = post("/api/v1/logs", r#"{"name":"n","message":"m"}"#);
    req.headers_mut()
        .insert("x-api-key", "s3cret".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::ACCEPTED
    );

    // A wrong key of the same length must still fail.
    let mut req = post("/api/v1/logs", r#"{"name":"n","message":"m"}"#);
    req.headers_mut()
        .insert("x-api-key", "s3cre7".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    // Reads stay public.
    assert!(app
        .oneshot(get("/api/v1/logs/recent"))
        .await
        .unwrap()
        .status()
        .is_success());
    writer.shutdown();
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    let (app, writer) = app(|c| c.max_body_bytes = 1024);
    let huge = serde_json::json!({ "name": "n", "message": "x".repeat(64 * 1024) }).to_string();
    let resp = app.oneshot(post("/api/v1/logs", &huge)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    writer.shutdown();
}

#[tokio::test]
async fn batch_ingest_accepts_all_rows() {
    let (app, writer) = app(|_| {});
    let payload = serde_json::json!([
        {"name": "a", "message": "1"},
        {"name": "a", "message": "2"},
        {"name": "a", "message": "3"},
    ])
    .to_string();

    let resp = app
        .clone()
        .oneshot(post("/api/v1/logs/batch", &payload))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(body_string(resp).await.contains("\"accepted\":3"));

    settle().await;
    let body = body_string(app.oneshot(get("/api/v1/logs/recent")).await.unwrap()).await;
    assert_eq!(
        serde_json::from_str::<Vec<serde_json::Value>>(&body)
            .unwrap()
            .len(),
        3
    );
    writer.shutdown();
}

#[tokio::test]
async fn static_assets_are_served_from_the_binary() {
    let (app, writer) = app(|_| {});
    let resp = app.clone().oneshot(get("/")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(body_string(resp).await.contains("Logs Stream"));

    let resp = app.oneshot(get("/logs.js")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    writer.shutdown();
}
