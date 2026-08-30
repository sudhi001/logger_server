//! Storage-layer tests: batching, id monotonicity across pruning, and the
//! streaming export that keeps `GET /logs` from being an OOM cannon.

use std::time::Duration;

use logger_server::config::Config;
use logger_server::model::{now_millis, LogRecord};
use logger_server::store::Store;

fn cfg(mutate: impl FnOnce(&mut Config)) -> Config {
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let mut c = Config::from_env();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    c.db_path = std::env::temp_dir()
        .join(format!("logger_store_{unique}_{seq}.db"))
        .to_string_lossy()
        .into_owned();
    mutate(&mut c);
    c
}

fn record(store: &Store, name: &str, message: &str) -> LogRecord {
    LogRecord {
        id: store.next_id(),
        ts: now_millis(),
        name: name.to_string(),
        level: 2,
        message: message.to_string(),
        device_id: None,
        device: None,
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

#[tokio::test]
async fn writer_batches_a_burst_into_storage() {
    let c = cfg(|c| c.ingest_queue = 65536);
    let (store, writer) = Store::open(&c).unwrap();

    for i in 0..5000 {
        store
            .enqueue(record(&store, "burst", &format!("m{i}")))
            .unwrap();
    }
    settle().await;

    assert_eq!(store.reader.count().await.unwrap(), 5000);
    writer.shutdown();
}

#[tokio::test]
async fn ids_stay_monotonic_across_a_retention_prune() {
    // Small cap so pruning is guaranteed to fire.
    let c = cfg(|c| {
        c.max_rows = 100;
        c.max_age = None;
        c.ingest_queue = 65536;
    });
    let (store, writer) = Store::open(&c).unwrap();

    for i in 0..500 {
        store
            .enqueue(record(&store, "n", &format!("m{i}")))
            .unwrap();
    }
    // Retention runs on a 10s cadence on the writer thread; shutdown forces a
    // final flush, so re-open afterwards to observe the pruned state.
    settle().await;
    let before = store.reader.recent(1, None, None, None).await.unwrap();
    assert_eq!(
        before[0].id, 500,
        "ids come from the app counter, not rowid"
    );

    writer.shutdown();
    drop(store);

    // Re-opening must resume from the true maximum, never reuse an id.
    let (store2, writer2) = Store::open(&c).unwrap();
    let next = store2.next_id();
    assert_eq!(next, 501, "id counter resumes past the highest stored id");
    writer2.shutdown();
}

#[tokio::test]
async fn retention_enforces_the_row_cap() {
    let c = cfg(|c| {
        c.max_rows = 50;
        c.max_age = None;
        c.ingest_queue = 65536;
    });
    let (store, writer) = Store::open(&c).unwrap();

    for i in 0..300 {
        store
            .enqueue(record(&store, "n", &format!("m{i}")))
            .unwrap();
    }
    settle().await;

    // The prune sweep is time-based; drive it by keeping the writer busy past
    // the interval rather than sleeping the full 10s here.
    let deadline = std::time::Instant::now() + Duration::from_secs(14);
    while std::time::Instant::now() < deadline {
        store.enqueue(record(&store, "n", "tick")).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        if store.reader.count().await.unwrap() <= 60 {
            break;
        }
    }

    let count = store.reader.count().await.unwrap();
    assert!(count <= 60, "row cap must be enforced, got {count}");
    writer.shutdown();
}

#[tokio::test]
async fn queue_sheds_load_instead_of_growing() {
    // A queue this small fills immediately; the point is that it refuses work
    // rather than allocating without bound.
    let c = cfg(|c| c.ingest_queue = 64);
    let (store, writer) = Store::open(&c).unwrap();

    let mut shed = 0;
    for i in 0..20_000 {
        if store
            .enqueue(record(&store, "n", &format!("m{i}")))
            .is_err()
        {
            shed += 1;
        }
    }
    assert!(shed > 0, "a bounded queue must reject once full");
    writer.shutdown();
}

#[tokio::test]
async fn export_streams_every_row_in_order() {
    let c = cfg(|c| c.ingest_queue = 65536);
    let (store, writer) = Store::open(&c).unwrap();

    const N: usize = 10_000;
    for i in 0..N {
        store
            .enqueue(record(&store, "n", &format!("m{i}")))
            .unwrap();
    }
    settle().await;

    // Drain the export stream chunk by chunk, exactly as the HTTP body does.
    let mut rx = store.reader.export(true).await.unwrap();
    let mut buf = Vec::new();
    let mut chunks = 0;
    while let Some(chunk) = rx.recv().await {
        buf.extend_from_slice(&chunk.unwrap());
        chunks += 1;
    }

    let text = String::from_utf8(buf).unwrap();
    let lines: Vec<_> = text.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), N, "every row is emitted");
    assert!(
        chunks > 1,
        "output must arrive in multiple chunks, not one buffer; got {chunks}"
    );

    // Order and completeness.
    for (i, line) in lines.iter().enumerate() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["id"].as_i64().unwrap(), i as i64 + 1);
    }
    writer.shutdown();
}

#[tokio::test]
async fn by_name_uses_the_index_and_filters_correctly() {
    let c = cfg(|c| c.ingest_queue = 65536);
    let (store, writer) = Store::open(&c).unwrap();

    for i in 0..100 {
        let name = if i % 2 == 0 { "even" } else { "odd" };
        store
            .enqueue(record(&store, name, &format!("m{i}")))
            .unwrap();
    }
    settle().await;

    let rows = store
        .reader
        .by_name("even".into(), 1000, None)
        .await
        .unwrap();
    assert_eq!(rows.len(), 50);
    assert!(rows.iter().all(|r| r.name == "even"));
    // Descending by id.
    assert!(rows.windows(2).all(|w| w[0].id > w[1].id));
    writer.shutdown();
}

#[tokio::test]
async fn since_id_returns_only_newer_rows_ascending() {
    let c = cfg(|c| c.ingest_queue = 65536);
    let (store, writer) = Store::open(&c).unwrap();

    for i in 0..20 {
        store
            .enqueue(record(&store, "n", &format!("m{i}")))
            .unwrap();
    }
    settle().await;

    let rows = store.reader.since_id(15, 100).await.unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows.first().unwrap().id, 16);
    assert_eq!(rows.last().unwrap().id, 20);
    writer.shutdown();
}
