# Configuration

Everything is an environment variable, and every one has a working default. The
server starts correctly with nothing set — though you should always set
`LOGGER_ADMIN_TOKEN`.

## Server

| Variable | Default | What it does |
|---|---|---|
| `PORT` / `LOGGER_PORT` | `8080` | Listen port. `PORT` is what Render, Fly and Heroku inject, so it usually just works |
| `LOGGER_DB_PATH` | `logs.db` | SQLite file. `/data/logs.db` in the container |
| `LOGGER_WORKERS` | `min(cores, 4)` | Async worker threads. Raise only if you have measured a CPU bottleneck |
| `LOGGER_READER_CONNS` | `4` | Read-only database connections |
| `LOGGER_LOG` | `info` | Log filter, e.g. `logger_server=debug` |

## Security

| Variable | Default | What it does |
|---|---|---|
| `LOGGER_ADMIN_TOKEN` | *generated* | Gates the dashboard, all reads, and device management |
| `LOGGER_SESSION_TTL_HOURS` | `168` (7 days) | How long a dashboard login lasts |
| `LOGGER_COOKIE_SECURE` | follows `TRUST_PROXY` | Send the session cookie with `Secure`. Must be **off** for plain-HTTP local use or login silently fails |
| `LOGGER_TRUST_PROXY` | `false` | Honour `X-Forwarded-For`. Turn on **only** behind a proxy that overwrites it |
| `LOGGER_RATE_LIMIT_RPS` | `500` | Per-IP writes per second. `0` disables |
| `LOGGER_RATE_LIMIT_BURST` | `1000` | Burst allowance above the sustained rate |
| `LOGGER_MAX_BODY_BYTES` | `1048576` | Request body cap. Raise if you send very large batches |

> **`LOGGER_ADMIN_TOKEN`**: if you do not set it, the server generates one at
> boot and prints it to its log. That keeps a fresh container usable, but the
> value changes on every restart — which signs everyone out and means digging
> through logs to get back in. Set it explicitly anywhere real.

> **`LOGGER_TRUST_PROXY`**: behind a proxy, every request appears to come from
> the proxy, so per-IP rate limiting lumps all your clients together. Turning
> this on fixes that. Turning it on when you are *not* behind a trusted proxy
> makes the limiter trivially bypassable, since anyone can spoof the header.

## AI agents (MCP)

| Variable | Default | What it does |
|---|---|---|
| `LOGGER_MCP_ENABLED` | `true` | Serve the `/mcp` endpoint at all |
| `LOGGER_MCP_MODE` | `admin` | `read`, `write`, or `admin` — what an agent may do |

> `read` lets an agent search, read and aggregate but change nothing, and is the
> right choice unless you specifically want an agent creating and revoking
> device tokens. Agents read log text, which is influenced by whatever your app
> was given; see [agents.md](agents.md) for the trade-off in full.

## Alerting

| Variable | Default | What it does |
|---|---|---|
| `LOGGER_WEBHOOK_ALLOW_PRIVATE` | `false` | Let webhooks reach private, loopback and link-local addresses |
| `LOGGER_PUBLIC_URL` | `http://localhost:PORT` | How this server is reached, for links inside alerts |
| `LOGGER_ALERT_QUEUE` | `256` | Pending deliveries before new alerts are dropped |

> Leaving `LOGGER_WEBHOOK_ALLOW_PRIVATE` off is what stops a webhook URL being
> used to probe your own network or read a cloud metadata endpoint. Turn it on
> only for a deliberate internal relay. See [alerting.md](alerting.md).

## Retention

| Variable | Default | What it does |
|---|---|---|
| `LOGGER_MAX_ROWS` | `1000000` | Oldest rows are pruned beyond this. `0` disables |
| `LOGGER_MAX_AGE_DAYS` | `7` | Rows older than this are pruned. `0` disables |
| `LOGGER_MAX_MESSAGE_LEN` | `50384` | Message truncation limit |

Pruning runs on the writer thread every 10 seconds and checkpoints the
write-ahead log afterwards, so the WAL file does not grow without bound either.

Rough sizing: a log line averages ~200 bytes on disk, so the 1,000,000 default
is on the order of 200 MB. On a host with a small or ephemeral disk, lower it.

## Throughput and memory

| Variable | Default | What it does |
|---|---|---|
| `LOGGER_INGEST_QUEUE` | `8192` | Write queue depth. When full, writes are refused with `503` rather than buffered |
| `LOGGER_SSE_CAPACITY` | `1024` | Live-stream ring size. A client falling this far behind is disconnected |

Both are deliberately bounded. A larger `LOGGER_INGEST_QUEUE` absorbs bigger
bursts at the cost of memory and of losing more if the process dies; a larger
`LOGGER_SSE_CAPACITY` tolerates slower dashboard clients before dropping them.

If `logger_shed_total` is climbing in [`/metrics`](api.md#operations), the
queue is the thing to raise — but check first whether the disk is the real
constraint.

## Allocator

The container sets `MIMALLOC_ARENA_EAGER_COMMIT=0`. mimalloc otherwise commits
its arenas up front, costing about 7 MB of resident memory the server never
uses — measured at 12.6 MB versus 5.8 MB of anonymous RSS. Setting a
`MIMALLOC_PURGE_DELAY` was also tried and consistently made things worse, so it
is not used.

The glibc image sets `MALLOC_ARENA_MAX=2` for the same reason: to stop glibc
spawning a per-thread arena for every worker.

## Examples

Local development:

```sh
LOGGER_ADMIN_TOKEN=dev-token \
LOGGER_DB_PATH=./dev.db \
LOGGER_COOKIE_SECURE=false \
cargo run --release
```

Behind a proxy, in production:

```sh
LOGGER_ADMIN_TOKEN=<long random string>
LOGGER_TRUST_PROXY=true
LOGGER_COOKIE_SECURE=true
LOGGER_MAX_ROWS=500000
LOGGER_MAX_AGE_DAYS=14
```

A small or free-tier box:

```sh
LOGGER_WORKERS=1
LOGGER_READER_CONNS=2
LOGGER_MAX_ROWS=100000
LOGGER_SSE_CAPACITY=256
```
