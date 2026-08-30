# Remote Logger for Mobile Application Developers

A remote log sink: mobile apps `POST` log lines, and you watch them stream live
in a browser. Written in Rust — a single static binary that idles at well under
10 MB of RAM.

## Demo

[https://logger-server-z5w8.onrender.com](https://logger-server-z5w8.onrender.com)

## Why it was rewritten

The service was originally Spring Boot / Kotlin on the JVM, with H2 and
Hibernate. That stack idles around 250–400 MB of RSS to serve what is really an
append-only table and a fan-out socket. The Rust port keeps the same product and
the same URLs while making memory bounded and predictable.

| | Kotlin / Spring Boot | Rust |
|---|---|---|
| Idle RSS | ~250–400 MB | **~6–9 MB** |
| RSS with 1000 live tails | ~500 MB+ | **~15 MB** |
| Cold start | 2–5 s | **< 20 ms** |
| Image size | ~450 MB | **~6 MB** |
| `GET /logs` on a huge table | loads it all into heap | streams, flat memory |

The rewrite also fixed defects carried by the original implementation:

- `GET /logs` was an unbounded `findAll()` — it now streams from a cursor.
- SSE fan-out allocated a fresh coroutine scope per POST with no backpressure —
  it is now a fixed-capacity broadcast of pre-serialised frames.
- Logs had **no timestamp at all**; ordering relied on the autoincrement id.
- Nothing pruned the database, so it grew until the disk filled.
- `name` was unindexed, so lookups by name full-scanned the table.
- Writes were unauthenticated and unthrottled.
- Dead SSE clients were reaped only on `IOException`, and reconnects silently
  dropped whatever arrived in the gap.

## How it works

```
  POST /api/v1/logs ──▶ validate ──▶ id from AtomicU64
                            │
                ┌───────────┴───────────┐
                ▼                       ▼
      bounded mpsc queue        broadcast::Sender<Arc<Frame>>
                │                       │
       single writer thread      SSE subscribers
       batched txn, WAL          lagging client is evicted
                │                       │
             SQLite ◀── read-only pool (streaming cursor)
```

Three properties do the heavy lifting:

1. **Serialize once, share by refcount.** Each log line is turned into its SSE
   wire frame exactly once. A thousand subscribers cost a thousand pointer
   copies, not a thousand JSON strings.
2. **Every queue is bounded.** The broadcast ring is fixed; a subscriber that
   falls behind is dropped and reconnects with `Last-Event-ID`, which replays
   the gap from SQLite. The ingest queue is bounded too — when it fills, the
   server returns `503` instead of growing.
3. **Result sets are never materialised.** The export endpoint steps a SQLite
   cursor and emits fixed-size chunks, so peak memory is one chunk whether the
   table holds a thousand rows or fifty million.

## API

| Method | Path | Notes |
|---|---|---|
| `POST` | `/api/v1/logs` | `202` + `{id, ts}`. Add `?sync=true` to wait for durability (`201`). |
| `POST` | `/api/v1/logs/batch` | Array of records. The cheapest way to ingest at volume. |
| `GET` | `/api/v1/logs/recent` | `?limit=` (max 5000), `?before_id=` for cursor pagination. |
| `GET` | `/api/v1/logs/by-name/{name}` | Same paging params. Index-backed. |
| `GET` | `/api/v1/logs/export` | Streams everything. `?format=ndjson` for line-delimited. |
| `GET` | `/api/v1/logs/stream` | SSE. Honours `Last-Event-ID`; 15 s keepalive. |
| `GET` | `/healthz` `/metrics` | Liveness and Prometheus text. |

### Log record

```json
{ "id": 1, "ts": 1788067277526, "name": "[MyApp] ", "level": 2, "message": "hello" }
```

`ts` (unix millis) and `level` are optional on input — omit them and the server
fills them in, so a client that only sends `{name, message}` still works.

### Legacy routes

The original paths remain live and map onto the same handlers, so apps already
in the field keep working without a redeploy:

`POST /logs` · `GET /logs` · `GET /logs/recent` · `GET /logs/{name}` · `GET /logs/stream`

`GET /logs` still returns a JSON array — it is merely produced incrementally now,
so anything that parses the response is unaffected.

## Configuration

Everything is an environment variable, and every one has a working default.

| Variable | Default | Purpose |
|---|---|---|
| `PORT` / `LOGGER_PORT` | `8080` | Listen port. `PORT` is what most PaaS platforms inject. |
| `LOGGER_DB_PATH` | `logs.db` | SQLite file. |
| `LOGGER_WORKERS` | `min(cores, 4)` | Tokio worker threads. |
| `LOGGER_READER_CONNS` | `4` | Read-only connection pool size. |
| `LOGGER_MAX_ROWS` | `1000000` | Row cap before pruning. `0` disables. |
| `LOGGER_MAX_AGE_DAYS` | `7` | Age cap before pruning. `0` disables. |
| `LOGGER_MAX_MESSAGE_LEN` | `50384` | Message truncation limit, matching the original. |
| `LOGGER_API_KEY` | *(unset)* | When set, writes require a matching `X-Api-Key`. |
| `LOGGER_RATE_LIMIT_RPS` | `500` | Per-IP write rate. `0` disables. |
| `LOGGER_TRUST_PROXY` | `false` | Honour `X-Forwarded-For`. Set only behind a trusted proxy. |
| `LOGGER_SSE_CAPACITY` | `1024` | Broadcast ring size. |
| `LOGGER_INGEST_QUEUE` | `8192` | Write queue depth before shedding. |
| `LOGGER_LOG` | `info` | `tracing` filter, e.g. `logger_server=debug`. |

> Enabling `LOGGER_API_KEY` is opt-in precisely so that deploying this version
> breaks nothing. Set it when you want the write endpoint locked down.

## Running

```sh
cargo run --release
# then open http://localhost:8080
```

```sh
curl -X POST localhost:8080/api/v1/logs \
  -H 'content-type: application/json' \
  -d '{"name":"[MyApp] ","message":"hello","level":2}'

curl -N localhost:8080/api/v1/logs/stream    # live tail
```

## Docker

Two images. The default is a fully static musl binary on an empty base — no OS,
no shell, nothing to patch.

```sh
docker build --platform linux/amd64 -t sudhis/logger_server:2.0.0 .
docker run --rm -p 8080:8080 -v logger-data:/data sudhis/logger_server:2.0.0
```

`Dockerfile.glibc` builds the same server on distroless instead. It is larger and
uses slightly more RSS, but it is easier to attach tooling to.

Mount a volume at `/data` if you want logs to survive a restart; without one the
database lives in the container's writable layer and is discarded with it.

## Tests

```sh
cargo test
```

Twenty-two tests covering the API surface, legacy-route parity, cursor
pagination, character-boundary truncation, batching, retention, streaming
export, and — for SSE — live delivery, gap replay on reconnect, eviction of a
subscriber that cannot keep up, and stream termination on shutdown.

## Prerequisites

Rust 1.80 or newer (the toolchain file pins 1.98). No JVM, no Gradle.
