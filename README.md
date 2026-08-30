# Remote Logger for Mobile Application Developers

A remote log sink: mobile apps `POST` log lines, and you watch them stream live
in a browser. Written in Rust — a single static binary in a 4.76 MB image
that idles at 11 MB of RAM.

## Demo

[https://logger-server-z5w8.onrender.com](https://logger-server-z5w8.onrender.com)

## Why it was rewritten

The service was originally Spring Boot / Kotlin on the JVM, with H2 and
Hibernate. That stack idles around 250–400 MB of RSS to serve what is really an
append-only table and a fan-out socket. The Rust port keeps the same product and
the same URLs while making memory bounded and predictable.

| | Kotlin / Spring Boot | Rust |
|---|---|---|
| Idle RSS | ~250–400 MB | **11.1 MB** |
| RSS, 400 live tails | — | **17.2 MB** |
| RSS, ~2000 live tails | — | **42.8 MB** (~16 KB/client) |
| Cold start | 2–5 s | **~25 ms** |
| Image size | 259 MB (compressed) | **4.92 MB** |
| Batched ingest | ~2–5 k/s | **~50 k rows/s** |
| `GET /logs` on a huge table | loads it all into heap | streams, flat memory |

Those Rust figures are measured, not estimated: `linux/amd64` image, `VmRSS`
read from `/proc`, under emulation on Apple Silicon. The idle number breaks down
as 7.3 MB anonymous plus 3.8 MB of file-backed binary pages.

The rewrite also fixed defects carried by the original implementation:

- `GET /logs` was an unbounded `findAll()` — it now streams from a cursor.
- SSE fan-out allocated a fresh coroutine scope per POST with no backpressure —
  it is now a fixed-capacity broadcast of pre-serialised frames.
- Logs had **no timestamp at all**; ordering relied on the autoincrement id.
- Nothing pruned the database, so it grew until the disk filled.
- `name` was unindexed, so lookups by name full-scanned the table.
- Writes were unauthenticated and unthrottled, and *every log was world-readable*
  to anyone with the URL.
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

What memory does scale with is *connection count*, at roughly 16 KB per open
SSE stream — that is hyper's per-connection read/write buffers plus a tokio
task, not per-client log buffering. A disconnected client is reaped within one
keepalive interval (15 s), since a closed socket is only detected on the next
write.

## Authentication

Nothing is open. There are two kinds of credential:

| | Credential | Grants |
|---|---|---|
| **Device** | A per-device token, `lgrd_…` | Writing logs, and nothing else |
| **Viewer** | The admin token `lgra_…`, or a session cookie from signing in | The dashboard, all read endpoints, and device management |

Every app that ships logs gets its **own** token, created from the Devices page
or over the API. Tokens are stored only as a SHA-256 hash, so the plaintext is
shown once at creation and is not recoverable — and a database leak does not
hand anyone a working credential. Revoking a device drops it from the in-memory
auth map, so its token stops working on the very next request.

A log's device attribution comes from the token that sent it, never from the
request body, so one device cannot write logs as another.

```sh
# Register a device (admin credential required).
curl -X POST localhost:8080/api/v1/devices \
  -H "x-admin-token: $LOGGER_ADMIN_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name":"Pixel 8 — QA","platform":"Android 15"}'

# Ship a log with the token it returned.
curl -X POST localhost:8080/api/v1/logs \
  -H "Authorization: Bearer $DEVICE_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name":"[MyApp] ","message":"hello","level":2}'
```

Set `LOGGER_ADMIN_TOKEN` to a value of your choosing. If you don't, the server
generates one at boot and prints it to the log — usable, but it changes on every
restart.

> **This is a breaking change.** Clients that previously posted to `/logs`
> without credentials now get `401`. Register a device and add the header.

## Dashboard

Reads are gated by a login, so the log stream is no longer world-readable.

- **Timestamps and colour-coded levels** — trace, debug, info, warn, error.
- **Live search** across message, tag, and device, plus level and device filters
  that apply to backfill and the live stream alike.
- **Pause / Follow / Clear / Copy** — freeze the view to read something (or hit
  space), toggle tail-following, or copy the visible lines as JSON. Scrolling up
  turns following off, the way a terminal does.
- **Expandable rows** — long messages collapse to one line; click to expand,
  which pretty-prints JSON payloads and preserves stack-trace newlines.
- A connection indicator, and a bounded 5,000-line buffer so a tab left open
  overnight does not grow until it dies.

## API

| Method | Path | Auth | Notes |
|---|---|---|---|
| `POST` | `/api/v1/logs` | device | `202` + `{id, ts}`. `?sync=true` waits for durability (`201`). |
| `POST` | `/api/v1/logs/batch` | device | Array ingest. Returns `accepted` and `dropped`. |
| `GET` | `/api/v1/logs/recent` | viewer | `?limit=` (max 5000), `?before_id=`, `?min_level=`, `?device_id=` |
| `GET` | `/api/v1/logs/by-name/{name}` | viewer | Index-backed. |
| `GET` | `/api/v1/logs/export` | viewer | Streams everything. `?format=ndjson` for line-delimited. |
| `GET` | `/api/v1/logs/stream` | viewer | SSE. Honours `Last-Event-ID`; 15 s keepalive. |
| `GET` `POST` | `/api/v1/devices` | viewer | List devices, or register one. |
| `DELETE` | `/api/v1/devices/{id}` | viewer | Revoke, effective immediately. |
| `POST` | `/api/v1/auth/login` `/logout` | — | Exchanges the admin token for a session cookie. |
| `GET` | `/metrics` | viewer | Prometheus text. |
| `GET` | `/healthz` | — | Public, so the platform can probe it. |

### Log record

```json
{ "id": 1, "ts": 1788067277526, "name": "[MyApp] ", "level": 2,
  "message": "hello", "device_id": 3, "device": "Pixel 8 — QA" }
```

`ts` (unix millis) and `level` are optional on input — omit them and the server
fills them in, so a client that only sends `{name, message}` still works. Levels
run 0–4 (trace, debug, info, warn, error) and are clamped to that range.
`device_id`/`device` are attached by the server from the authenticating token
and ignored if sent.

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
| `LOGGER_ADMIN_TOKEN` | *generated* | Gates the dashboard, reads, and device management. Generated and logged if unset. |
| `LOGGER_SESSION_TTL_HOURS` | `168` | How long a dashboard login lasts. |
| `LOGGER_COOKIE_SECURE` | follows `TRUST_PROXY` | Send the session cookie with `Secure`. Off for plain-HTTP local use. |
| `LOGGER_RATE_LIMIT_RPS` | `500` | Per-IP write rate. `0` disables. |
| `LOGGER_TRUST_PROXY` | `false` | Honour `X-Forwarded-For`. Set only behind a trusted proxy. |
| `LOGGER_SSE_CAPACITY` | `1024` | Broadcast ring size. |
| `LOGGER_INGEST_QUEUE` | `8192` | Write queue depth before shedding. |
| `LOGGER_LOG` | `info` | `tracing` filter, e.g. `logger_server=debug`. |

> Set `LOGGER_ADMIN_TOKEN` explicitly in any real deployment. The generated
> fallback keeps a fresh container usable but is lost on restart, which signs
> everyone out and changes the value you log in with.

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

Thirty tests covering the API surface, legacy-route parity, cursor
pagination, character-boundary truncation, batching, retention, streaming
export, and — for SSE — live delivery, gap replay on reconnect, eviction of a
subscriber that cannot keep up, and stream termination on shutdown.

The security-relevant ones are worth naming: writes and reads are both rejected
without a credential, a revoked token stops working on the next request, a
device cannot forge another device's attribution, tokens never appear in the
device listing, and the session cookie is `HttpOnly` and `SameSite=Strict`.

## Prerequisites

Rust 1.80 or newer (the toolchain file pins 1.98). No JVM, no Gradle.
