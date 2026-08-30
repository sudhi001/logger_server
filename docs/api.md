# API reference

Base URL is wherever you deployed it. All request and response bodies are JSON.

## Authentication

![Device tokens write only; the admin token or a session reads and manages](images/auth.svg)

Two credentials, with different powers:

| Credential | Header | Can do |
|---|---|---|
| **Device token** (`lgrd_…`) | `Authorization: Bearer <token>` or `X-Device-Token: <token>` | Write logs, nothing else |
| **Admin token** (`lgra_…`) | `Authorization: Bearer <token>` or `X-Admin-Token: <token>` | Read logs, manage devices |
| **Session cookie** | Set by `POST /api/v1/auth/login` | Same as admin — this is what the dashboard uses |

A device token cannot read logs. An admin token is not meant to be shipped in an
app. Endpoints are marked **device**, **viewer** (admin token or session), or
**public** below.

---

## Write

### `POST /api/v1/logs` — device

Also reachable as `POST /logs`.

```json
{ "name": "[Network] ", "message": "GET /session -> 200", "level": 2, "ts": 1788067277526 }
```

| Field | Required | Notes |
|---|---|---|
| `message` | yes | Truncated to `LOGGER_MAX_MESSAGE_LEN` (50,384) characters |
| `name` | no | Free-text tag, truncated to 255 characters |
| `level` | no | `0` trace, `1` debug, `2` info (default), `3` warn, `4` error. Clamped to `0`–`4` |
| `ts` | no | Unix **milliseconds**. Defaults to server receipt time |
| `context` | no | A JSON **object** of structured fields (session id, app version, user id). Max 8 KB. Anything that is not an object is rejected with `400` |

`device_id` and `device` are set by the server from your token; sending them has
no effect.

**Query parameters**

| Name | Default | Effect |
|---|---|---|
| `sync` | `false` | Wait for the row to be committed before responding |

**Responses**

| Status | Meaning |
|---|---|
| `202 Accepted` | Queued and broadcast to live dashboards; on disk milliseconds later |
| `201 Created` | Only with `?sync=true` — the row is durable |
| `400` | Neither `name` nor `message` supplied |
| `401` | Missing, unknown, or revoked device token |
| `413` | Body over `LOGGER_MAX_BODY_BYTES` (1 MiB) |
| `429` | Per-IP rate limit exceeded |
| `503` | Write queue full — retry, `Retry-After: 1` |

```json
{ "id": 1, "ts": 1788067277526 }
```

### `POST /api/v1/logs/batch` — device

A JSON array of the same objects. This is how you should ingest at volume: one
request carrying hundreds of lines is dramatically cheaper than hundreds of
requests, for the client and the server both.

```json
[ { "name": "[a] ", "message": "one" }, { "name": "[a] ", "message": "two" } ]
```

```json
{ "accepted": 2, "dropped": 0, "first_id": 1, "last_id": 2 }
```

**`dropped` is the field to watch.** If the server's write queue fills partway
through your batch it takes what it can and tells you how many it refused. The
refused ones are the **last `dropped` entries** of what you sent. Resending them
is your responsibility.

`400` if the array is empty; `503` only if nothing at all could be accepted.

---

## Read

### `GET /api/v1/logs/recent` — viewer

Also `GET /logs/recent`. Newest first.

| Parameter | Default | Notes |
|---|---|---|
| `limit` | `1000` | Clamped to `5000` |
| `before_id` | — | Cursor: only rows with a smaller id. Page by passing the last id you saw |
| `min_level` | `0` | Only rows at this level or above |
| `device_id` | — | Restrict to one device |

```json
[ { "id": 2, "ts": 1788067277541, "name": "[svc] ", "level": 4,
    "message": "boom", "device_id": 1, "device": "Pixel 8 — QA" } ]
```

Paging backwards through everything:

```sh
curl "$URL/api/v1/logs/recent?limit=500" -H "x-admin-token: $ADMIN"
curl "$URL/api/v1/logs/recent?limit=500&before_id=<last id from above>" -H "x-admin-token: $ADMIN"
```

### `GET /api/v1/logs/search` — viewer

Full-text search across everything stored.

| Parameter | Notes |
|---|---|
| `q` | Free text. Words are ANDed; the last matches as a prefix. Omit to filter without text matching |
| `min_level` | Minimum severity, `0`–`4` |
| `device_id` | Restrict to one device |
| `name` | Exact tag match |
| `since` / `until` | Unix **milliseconds**, inclusive |
| `before_id` | Pagination cursor |
| `limit` | Default 1000, capped at 5000 |

Your text is escaped, never interpreted as query syntax, so quotes and operators
cannot produce a syntax error. Punctuation splits into tokens, so `txn_9f21ab`
is found inside a longer message.

```sh
curl "$URL/api/v1/logs/search?q=NullPointerException&min_level=4" -H "x-admin-token: $ADMIN"
```

### `GET /api/v1/logs/{id}/context` — viewer

The lines around one log, in the order they happened.

| Parameter | Default | Notes |
|---|---|---|
| `before` | 20 | Lines before, capped at 500 |
| `after` | 20 | Lines after, capped at 500 |

```json
{ "before": [ … ], "match": { … }, "after": [ … ] }
```

`400` if there is no log with that id.

### `GET /api/v1/logs/stats` — viewer

Aggregates over a window. `since` and `until` are Unix milliseconds; both
optional.

```json
{
  "total": 15234,
  "since": null, "until": null,
  "first_ts": 1788070506225, "last_ts": 1788075947997,
  "by_level":  [ { "level": 4, "label": "error", "count": 22 } ],
  "by_device": [ { "name": "Pixel 8 Pro — Priya (QA)", "count": 9012 } ],
  "by_name":   [ { "name": "[Net] ", "count": 4410 } ]
}
```

`by_device` and `by_name` are the top 50 by count.

### `GET /api/v1/logs/by-name/{name}` — viewer

Also `GET /logs/{name}`. Same `limit` and `before_id`. URL-encode the tag —
`[app] ` becomes `%5Bapp%5D%20`.

### `GET /api/v1/logs/export` — viewer

Also `GET /logs`. Streams **every** row, oldest first.

| Parameter | Effect |
|---|---|
| `format=ndjson` | One JSON object per line |
| *(omitted)* | A single JSON array |

The response is produced incrementally from a database cursor, so server memory
stays flat whether the table holds a thousand rows or fifty million. The array
form exists so callers of the original `GET /logs` keep working unchanged; use
NDJSON for anything new, since you can process it as it arrives.

```sh
curl "$URL/api/v1/logs/export?format=ndjson" -H "x-admin-token: $ADMIN" > backup.ndjson
```

### `GET /api/v1/logs/stream` — viewer

Also `GET /logs/stream`. Server-Sent Events.

```
retry: 2000

id: 42
data: {"id":42,"ts":1788067277526,"name":"[app] ","level":2,"message":"hello","device_id":1,"device":"Pixel 8"}
```

- A `: keepalive` comment every 15 seconds keeps proxies from timing you out.
- Send `Last-Event-ID: <id>` to have everything after that id replayed from the
  database before the live feed resumes. Browsers do this automatically on
  reconnect, so nothing is lost across a dropped connection.
- A client that cannot keep up is **disconnected** rather than buffered for.
  Reconnect and the gap is replayed. This is what keeps server memory bounded
  regardless of how many slow clients attach.

`EventSource` cannot set headers, so browsers authenticate with the session
cookie. From a script, use the admin token:

```sh
curl -N "$URL/api/v1/logs/stream" -H "x-admin-token: $ADMIN"
```

---

## Devices

### `GET /api/v1/devices` — viewer

```json
[ { "id": 1, "name": "Pixel 8 — QA", "platform": "Android 15",
    "token_prefix": "lgrd_nZQa6MF", "created_at": 1788070506225,
    "last_seen": 1788070525544, "revoked": false } ]
```

Only `token_prefix` is exposed. The token itself is stored as a SHA-256 hash and
cannot be retrieved.

### `POST /api/v1/devices` — viewer

```json
{ "name": "Pixel 8 — QA", "platform": "Android 15" }
```

`name` is required (≤ 120 characters); `platform` is free-text and optional.

`201` returns the device **plus the plaintext token, the only time it is ever
available**:

```json
{ "id": 1, "name": "Pixel 8 — QA", "platform": "Android 15",
  "token_prefix": "lgrd_nZQa6MF", "created_at": 1788070506225,
  "last_seen": null, "revoked": false,
  "token": "lgrd_nZQa6MFVrSzkWrAGQRmSqWnScurnWvgi" }
```

### `DELETE /api/v1/devices/{id}` — viewer

`204` on success, `400` if the device does not exist or is already revoked.

Revocation is immediate: the token is dropped from the in-memory auth map, so
the very next request using it gets `401`. Revoked devices stay in the listing
with `"revoked": true`, and the logs they already sent are untouched.

---

## Session

### `POST /api/v1/auth/login` — public

```json
{ "token": "lgra_your_admin_token" }
```

`200` sets a `logger_session` cookie (`HttpOnly`, `SameSite=Strict`, plus
`Secure` when `LOGGER_COOKIE_SECURE` is on). `401` if the token is wrong.

### `POST /api/v1/auth/logout` — public

Invalidates the session and clears the cookie.

### `GET /api/v1/auth/whoami` — public

`200` if the caller holds a valid session, `401` otherwise. The dashboard uses
this to decide whether to render or bounce to the login page.

---

## Operations

### `GET /healthz` — public

`200 ok` normally, `503 draining` once shutdown has begun so a load balancer can
route away before the process exits. Public, so a platform health check works
without credentials.

### `POST /mcp` — viewer

Model Context Protocol endpoint, JSON-RPC 2.0, for AI agents. `GET /mcp`
describes it. See [agents.md](agents.md).

### `GET /metrics` — viewer

Prometheus text format.

| Metric | Meaning |
|---|---|
| `logger_ingested_total` | Records accepted |
| `logger_shed_total` | Records refused because the write queue was full |
| `logger_rate_limited_total` | Requests rejected by the per-IP limiter |
| `logger_sse_opened_total` | Live streams opened |
| `logger_sse_evicted_total` | Clients dropped for falling behind |
| `logger_sse_clients` | Streams currently connected |
| `logger_rows` | Rows stored |
| `logger_auth_failures_total` | Rejected dashboard logins |
| `logger_devices_active` | Devices with a live token |
| `logger_sessions_active` | Valid dashboard sessions |

Watch `logger_shed_total` and `logger_sse_evicted_total`: both climbing means
the server is at its limits rather than quietly buffering.

---

## Errors

Every error is JSON:

```json
{ "error": "unauthorized" }
```

Internal failures are logged server-side and reported to the caller only as
`{"error":"internal error"}` — details are never echoed back.

---

## Legacy routes

The original API is still live, mapped onto the same handlers. It is now
authenticated like everything else.

| Old | Now |
|---|---|
| `POST /logs` | `POST /api/v1/logs` — **needs a device token** |
| `GET /logs` | `GET /api/v1/logs/export` — still a JSON array, now streamed |
| `GET /logs/recent` | `GET /api/v1/logs/recent` |
| `GET /logs/{name}` | `GET /api/v1/logs/by-name/{name}` |
| `GET /logs/stream` | `GET /api/v1/logs/stream` |

See [migration.md](migration.md).
