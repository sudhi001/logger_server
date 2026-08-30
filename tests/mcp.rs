//! MCP endpoint tests: protocol shape, tool behaviour, and the access boundary.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use logger_server::config::Config;
use logger_server::mcp::tools::Access;
use logger_server::{build_state, routes};
use serde_json::{json, Value};
use tower::ServiceExt;

const ADMIN: &str = "lgra_test_admin_token";

struct Harness {
    app: axum::Router,
    writer: logger_server::store::WriterHandle,
    device: String,
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
        .join(format!("logger_mcp_{unique}_{seq}.db"))
        .to_string_lossy()
        .into_owned();
    cfg.admin_token = ADMIN.to_string();
    cfg.rate_limit_rps = 0;
    cfg.mcp_access = Access::Admin;
    mutate(&mut cfg);

    let (state, writer, shutdown) = build_state(cfg).expect("state");
    std::mem::forget(shutdown);
    let app = routes::build(state);

    let resp = app
        .clone()
        .oneshot(admin(post("/api/v1/devices", r#"{"name":"mcp-device"}"#)))
        .await
        .unwrap();
    let created: Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let device = created["token"].as_str().unwrap().to_string();

    Harness {
        app,
        writer,
        device,
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

fn post(uri: &str, json_body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(json_body.to_owned()))
        .unwrap()
}

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
    /// Sends one JSON-RPC call and returns the parsed response.
    async fn rpc(&self, body: Value) -> Value {
        let resp = self
            .app
            .clone()
            .oneshot(admin(post("/mcp", &body.to_string())))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "rpc call should return 200");
        serde_json::from_str(&body_string(resp).await).unwrap()
    }

    /// Calls a tool and returns the text content the model would see.
    async fn call(&self, name: &str, args: Value) -> String {
        let v = self
            .rpc(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": args }
            }))
            .await;
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    async fn is_error(&self, name: &str, args: Value) -> bool {
        let v = self
            .rpc(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": args }
            }))
            .await;
        v["result"]["isError"].as_bool().unwrap_or(false)
    }

    async fn log(&self, body: &str) {
        self.app
            .clone()
            .oneshot(with_device(post("/api/v1/logs", body), &self.device))
            .await
            .unwrap();
    }
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
}

#[tokio::test]
async fn initialize_negotiates_a_protocol_version() {
    let h = app(|_| {}).await;

    // A version we speak is echoed back.
    let v = h
        .rpc(json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                    "params":{"protocolVersion":"2024-11-05"}}))
        .await;
    assert_eq!(v["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(v["result"]["serverInfo"]["name"], "logger_server");
    assert!(v["result"]["capabilities"]["tools"].is_object());

    // One we do not falls back to ours rather than failing the handshake.
    let v = h
        .rpc(json!({"jsonrpc":"2.0","id":2,"method":"initialize",
                    "params":{"protocolVersion":"1999-01-01"}}))
        .await;
    assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
    h.writer.shutdown();
}

#[tokio::test]
async fn the_endpoint_requires_a_viewer_credential() {
    let h = app(|_| {}).await;
    let resp = h
        .app
        .clone()
        .oneshot(post(
            "/mcp",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    h.writer.shutdown();
}

#[tokio::test]
async fn notifications_get_no_response_body() {
    let h = app(|_| {}).await;
    // No id means a notification; JSON-RPC says do not reply.
    let resp = h
        .app
        .clone()
        .oneshot(admin(post(
            "/mcp",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(body_string(resp).await.is_empty());
    h.writer.shutdown();
}

#[tokio::test]
async fn search_finds_a_token_buried_in_a_message() {
    let h = app(|_| {}).await;
    h.log(r#"{"name":"[Net] ","message":"POST /checkout txn_9f21ab -> 500","level":4}"#)
        .await;
    h.log(r#"{"name":"[App] ","message":"unrelated chatter","level":1}"#)
        .await;
    settle().await;

    let out = h
        .call("search_logs", json!({ "query": "txn_9f21ab" }))
        .await;
    assert!(out.contains("txn_9f21ab"), "got {out}");
    assert!(
        !out.contains("unrelated chatter"),
        "search must not match everything"
    );

    // Prefix matching on the final token.
    let out = h.call("search_logs", json!({ "query": "check" })).await;
    assert!(
        out.contains("checkout"),
        "prefix search should match: {out}"
    );

    // An empty result is explained, not returned as a bare empty list.
    let out = h
        .call("search_logs", json!({ "query": "zzzznotpresent" }))
        .await;
    assert!(out.contains("No logs matched"), "got {out}");
    h.writer.shutdown();
}

#[tokio::test]
async fn search_syntax_from_a_user_cannot_break_the_query() {
    // FTS5 operators in user text would be a syntax error if passed through.
    let h = app(|_| {}).await;
    h.log(r#"{"name":"[App] ","message":"plain line","level":2}"#)
        .await;
    settle().await;

    for hostile in ["\" OR NOT AND(", "*", "NEAR(", "a\"\"b", "^", ")"] {
        assert!(
            !h.is_error("search_logs", json!({ "query": hostile })).await,
            "query {hostile:?} should be escaped, not error"
        );
    }
    h.writer.shutdown();
}

#[tokio::test]
async fn context_returns_the_lines_around_a_log_in_order() {
    let h = app(|_| {}).await;
    for i in 0..10 {
        h.log(&format!(
            r#"{{"name":"[App] ","message":"line {i}","level":2}}"#
        ))
        .await;
    }
    settle().await;

    let out = h
        .call(
            "get_log_context",
            json!({"log_id": 5, "before": 2, "after": 2}),
        )
        .await;
    let v: Value = serde_json::from_str(&out).unwrap();
    let before: Vec<i64> = v["before"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    let after: Vec<i64> = v["after"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();

    assert_eq!(v["match"]["id"], 5);
    assert_eq!(before, vec![3, 4], "before must read oldest-first");
    assert_eq!(after, vec![6, 7]);

    // A missing id is a readable message, not a crash.
    assert!(
        h.is_error("get_log_context", json!({"log_id": 99999}))
            .await
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn stats_aggregate_by_level_and_tag() {
    let h = app(|_| {}).await;
    for level in [1, 1, 1, 4, 4] {
        h.log(&format!(
            r#"{{"name":"[Net] ","message":"m","level":{level}}}"#
        ))
        .await;
    }
    settle().await;

    let out = h.call("get_log_stats", json!({})).await;
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["total"], 5);

    let by_level = v["by_level"].as_array().unwrap();
    let errors = by_level.iter().find(|l| l["level"] == 4).unwrap();
    assert_eq!(errors["count"], 2);
    assert_eq!(errors["label"], "error");
    h.writer.shutdown();
}

#[tokio::test]
async fn structured_context_survives_the_round_trip() {
    let h = app(|_| {}).await;
    h.log(
        r#"{"name":"[Net] ","message":"failed","level":4,
            "context":{"session":"s-42","appVersion":"3.1.0","retries":2}}"#,
    )
    .await;
    settle().await;

    let out = h.call("search_logs", json!({ "query": "failed" })).await;
    let start = out.find('[').unwrap();
    let rows: Value = serde_json::from_str(&out[start..]).unwrap();
    let ctx = &rows[0]["context"];
    assert_eq!(ctx["session"], "s-42");
    assert_eq!(ctx["appVersion"], "3.1.0");
    assert_eq!(ctx["retries"], 2, "numbers keep their type");
    h.writer.shutdown();
}

#[tokio::test]
async fn context_must_be_an_object() {
    let h = app(|_| {}).await;
    // A bare string would make the column unqueryable, so it is refused.
    let resp = h
        .app
        .clone()
        .oneshot(with_device(
            post(
                "/api/v1/logs",
                r#"{"name":"n","message":"m","context":"not-an-object"}"#,
            ),
            &h.device,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    h.writer.shutdown();
}

#[tokio::test]
async fn read_mode_hides_and_refuses_the_mutating_tools() {
    let h = app(|c| c.mcp_access = Access::Read).await;

    let v = h
        .rpc(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .await;
    let names: Vec<&str> = v["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"search_logs"));
    assert!(
        !names.contains(&"revoke_device"),
        "read mode must not advertise it"
    );
    assert!(!names.contains(&"create_device"));
    assert!(!names.contains(&"write_log"));

    // Not merely hidden — calling it anyway is refused.
    assert!(
        h.is_error("revoke_device", json!({ "device_id": 1 })).await,
        "a hidden tool must still be enforced when called directly"
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn admin_mode_can_create_and_revoke_a_device() {
    let h = app(|_| {}).await;

    let out = h
        .call(
            "create_device",
            json!({ "name": "agent-made", "platform": "test" }),
        )
        .await;
    assert!(out.contains("lgrd_"), "the token is returned once: {out}");

    let listed = h.call("list_devices", json!({})).await;
    assert!(listed.contains("agent-made"));

    let id = serde_json::from_str::<Value>(&listed).unwrap()[0]["id"]
        .as_i64()
        .unwrap();
    let out = h.call("revoke_device", json!({ "device_id": id })).await;
    assert!(out.contains("Revoked"), "got {out}");

    // Revoking the same device twice is an error the model can read.
    assert!(
        h.is_error("revoke_device", json!({ "device_id": id }))
            .await
    );
    h.writer.shutdown();
}

#[tokio::test]
async fn a_batch_of_calls_is_answered_as_a_batch() {
    let h = app(|_| {}).await;
    let v = h
        .rpc(json!([
            {"jsonrpc":"2.0","id":1,"method":"ping"},
            {"jsonrpc":"2.0","id":2,"method":"tools/list"}
        ]))
        .await;
    let arr = v.as_array().expect("batch in, batch out");
    assert_eq!(arr.len(), 2);
    h.writer.shutdown();
}

#[tokio::test]
async fn an_unknown_method_is_a_protocol_error_not_a_panic() {
    let h = app(|_| {}).await;
    let v = h
        .rpc(json!({"jsonrpc":"2.0","id":1,"method":"nope/nope"}))
        .await;
    assert_eq!(v["error"]["code"], -32601);
    h.writer.shutdown();
}
