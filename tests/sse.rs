//! End-to-end SSE tests over a real socket.
//!
//! These cover the properties the memory story depends on: a slow subscriber is
//! evicted rather than buffered for, and the gap it misses is replayed on
//! reconnect so no log line is actually lost.

use std::time::Duration;

use logger_server::config::Config;
use logger_server::{build_state, routes};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

struct Harness {
    port: u16,
    writer: Option<logger_server::store::WriterHandle>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

async fn start(mutate: impl FnOnce(&mut Config)) -> Harness {
    let mut cfg = Config::from_env();
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    cfg.db_path = std::env::temp_dir()
        .join(format!("logger_sse_{unique}_{seq}.db"))
        .to_string_lossy()
        .into_owned();
    cfg.api_key = None;
    cfg.rate_limit_rps = 0;
    cfg.port = 0;
    mutate(&mut cfg);

    let (state, writer, shutdown) = build_state(cfg).expect("state");
    let app = routes::build(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let shutdown_rx = shutdown.subscribe();

    tokio::spawn(async move {
        let mut rx = shutdown_rx;
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = rx.changed().await;
        })
        .await;
    });

    Harness {
        port,
        writer: Some(writer),
        shutdown,
    }
}

impl Harness {
    async fn post(&self, name: &str, message: &str) {
        let body = serde_json::json!({ "name": name, "message": message }).to_string();
        let mut sock = TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        let req = format!(
            "POST /api/v1/logs HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(req.as_bytes()).await.unwrap();
        let mut sink = Vec::new();
        let _ = sock.read_to_end(&mut sink).await;
    }

    /// Fetches the Prometheus metrics text.
    async fn metrics(&self) -> String {
        let mut sock = TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        sock.write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut out = Vec::new();
        let _ = sock.read_to_end(&mut out).await;
        String::from_utf8_lossy(&out)
            .lines()
            .filter(|l| l.starts_with("logger_"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Posts many records in one request. Used to flood a stalled subscriber
    /// quickly; one connection per record would dominate the test runtime.
    async fn post_batch(&self, count: usize, filler: usize) {
        let rows: Vec<_> = (0..count)
            .map(|i| serde_json::json!({ "name": "flood", "message": format!("{i}:{}", "x".repeat(filler)) }))
            .collect();
        let body = serde_json::Value::Array(rows).to_string();
        let mut sock = TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        let req = format!(
            "POST /api/v1/logs/batch HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(req.as_bytes()).await.unwrap();
        let mut sink = Vec::new();
        let _ = sock.read_to_end(&mut sink).await;
    }

    /// Opens an SSE stream and returns the socket, headers already consumed.
    async fn open_stream(&self, last_event_id: Option<i64>) -> BufReader<TcpStream> {
        let sock = TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        let mut sock = sock;
        let extra = match last_event_id {
            Some(id) => format!("Last-Event-ID: {id}\r\n"),
            None => String::new(),
        };
        // Connection: close makes the end of the response body observable as EOF;
        // with keep-alive the socket would stay open for reuse.
        let req =
            format!("GET /logs/stream HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{extra}\r\n");
        sock.write_all(req.as_bytes()).await.unwrap();

        let mut reader = BufReader::new(sock);
        // Consume the response headers.
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        reader
    }

    fn stop(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(w) = self.writer.take() {
            w.shutdown();
        }
    }
}

/// Reads SSE `data:` payloads until `want` are collected or the deadline passes.
async fn collect_data(
    reader: &mut BufReader<TcpStream>,
    want: usize,
    within: Duration,
) -> Vec<String> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + within;
    while out.len() < want {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut line = String::new();
        match tokio::time::timeout(remaining, reader.read_line(&mut line)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(_)) => {
                if let Some(rest) = line.strip_prefix("data: ") {
                    out.push(rest.trim_end().to_string());
                }
            }
            Ok(Err(_)) => break,
        }
    }
    out
}

#[tokio::test]
async fn live_logs_reach_a_connected_subscriber() {
    let h = start(|_| {}).await;
    let mut stream = h.open_stream(None).await;

    h.post("svc", "first").await;
    h.post("svc", "second").await;

    let data = collect_data(&mut stream, 2, Duration::from_secs(5)).await;
    assert_eq!(data.len(), 2, "got {data:?}");
    assert!(data[0].contains("first"));
    assert!(data[1].contains("second"));
    h.stop();
}

#[tokio::test]
async fn a_stalled_subscriber_is_evicted_rather_than_buffered() {
    // Tiny ring so the subscriber falls behind quickly.
    let h = start(|c| {
        c.sse_capacity = 16;
        c.ingest_queue = 65536;
        c.max_body_bytes = 32 * 1024 * 1024;
    })
    .await;
    let mut stream = h.open_stream(None).await;

    // Flood far more than any socket buffer can hold while the subscriber reads
    // nothing. Two things must hold: the server's memory does not grow with the
    // backlog (the ring is fixed at 16), and the client is eventually dropped.
    for _ in 0..8 {
        h.post_batch(512, 4096).await;
    }

    // Now start reading. Backpressure means the server only re-polls this
    // subscriber once the socket drains; at that point `recv` reports Lagged and
    // the stream is closed rather than being back-filled.
    // Now start reading. Backpressure means the server only re-polls this
    // subscriber once the socket drains; at that point `recv` reports Lagged and
    // the stream is closed rather than being back-filled for.
    assert!(
        drain_to_eof(&mut stream, Duration::from_secs(20)).await,
        "server must close a subscriber that cannot keep up"
    );

    // The counter is the direct evidence that eviction, not buffering, happened.
    let metrics = h.metrics().await;
    assert!(
        metrics.contains("logger_sse_evicted_total 1"),
        "expected exactly one eviction, got:\n{metrics}"
    );
    h.stop();
}

#[tokio::test]
async fn reconnect_replays_the_gap_via_last_event_id() {
    let h = start(|_| {}).await;

    h.post("svc", "before-1").await;
    h.post("svc", "before-2").await;
    h.post("svc", "before-3").await;
    // Let the writer commit; replay reads from SQLite.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Reconnect as if we had last seen id 1.
    let mut stream = h.open_stream(Some(1)).await;
    let replayed = collect_data(&mut stream, 2, Duration::from_secs(5)).await;

    assert_eq!(replayed.len(), 2, "ids 2 and 3 replay; got {replayed:?}");
    assert!(replayed[0].contains("before-2"));
    assert!(replayed[1].contains("before-3"));

    // The live feed continues from there without repeating the replayed rows.
    h.post("svc", "after-reconnect").await;
    let live = collect_data(&mut stream, 1, Duration::from_secs(5)).await;
    assert_eq!(live.len(), 1);
    assert!(live[0].contains("after-reconnect"), "got {live:?}");
    h.stop();
}

/// Reads until the server closes the connection. Returns whether EOF arrived
/// before the deadline.
async fn drain_to_eof(reader: &mut BufReader<TcpStream>, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let mut line = String::new();
        match tokio::time::timeout(remaining, reader.read_line(&mut line)).await {
            Ok(Ok(0)) => return true,
            Ok(Ok(_)) => continue,
            _ => return false,
        }
    }
}

#[tokio::test]
async fn shutdown_closes_open_streams() {
    // Without this an open tail would hold graceful shutdown open forever.
    let h = start(|_| {}).await;
    let mut stream = h.open_stream(None).await;
    h.post("svc", "one").await;
    let _ = collect_data(&mut stream, 1, Duration::from_secs(5)).await;

    let _ = h.shutdown.send(true);

    // Bytes of the in-flight frame may still be buffered; what matters is that
    // the stream terminates rather than staying open indefinitely.
    assert!(
        drain_to_eof(&mut stream, Duration::from_secs(10)).await,
        "stream must end on shutdown"
    );
}
